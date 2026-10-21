//! Admin Harvest import staging, journal and commit (#401).
//!
//! Batch metadata lives in `imports/<id>.json`; uploaded source bytes live in
//! `imports/<id>/<time.csv|expenses.csv|invoices.zip>`. Nothing here is a
//! business document: staging is admin-only and never read by member routes.
//!
//! A commit re-parses the staged sources, rebuilds the #400 preview **inside the
//! writer lock**, verifies the caller's preview revision and only then writes.
//! Every planned file is journaled in `imports/.journal.<id>.json` before it is
//! created; any failure removes exactly those files, and a boot interrupted
//! mid-commit rolls the journal back before serving traffic. Ids are
//! deterministic ([`crate::domain::planned_time_id`] and friends), so an
//! identical retry returns the same records instead of creating a second set.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::io::{read_json, write_bytes_atomic, write_json};
use super::*;
use crate::domain::{
    Category, Currency, Customer, Entry, Expense, Hours, ImportChoices, ImportDecision,
    ImportOrigin, ImportProvider, ImportedInvoice, ImportedTime, Invoice, InvoiceLine,
    InvoiceStatus, LineKind, Project, ProjectCode, Source, Task,
};
use crate::harvest::{ReportKind, parse};
use crate::harvest_invoices::{inspect_invoice_text, inspect_invoice_zip};

pub const IMPORTS_DIR: &str = "imports";
pub const MAX_IMPORT_CSV_BYTES: usize = 1024 * 1024;
pub const MAX_IMPORT_ZIP_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportKind {
    Time,
    Expenses,
    Invoices,
}

impl ImportKind {
    fn file_name(self) -> &'static str {
        match self {
            Self::Time => "time.csv",
            Self::Expenses => "expenses.csv",
            Self::Invoices => "invoices.zip",
        }
    }

    fn limit(self) -> usize {
        match self {
            Self::Invoices => MAX_IMPORT_ZIP_BYTES,
            Self::Time | Self::Expenses => MAX_IMPORT_CSV_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedImportFile {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportFiles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<StagedImportFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expenses: Option<StagedImportFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoices: Option<StagedImportFile>,
}

impl ImportFiles {
    fn get(&self, kind: ImportKind) -> &Option<StagedImportFile> {
        match kind {
            ImportKind::Time => &self.time,
            ImportKind::Expenses => &self.expenses,
            ImportKind::Invoices => &self.invoices,
        }
    }

    #[must_use]
    pub fn contains(&self, kind: ImportKind) -> bool {
        match kind {
            ImportKind::Time => self.time.is_some(),
            ImportKind::Expenses => self.expenses.is_some(),
            ImportKind::Invoices => self.invoices.is_some(),
        }
    }

    fn set(&mut self, kind: ImportKind, file: StagedImportFile) {
        match kind {
            ImportKind::Time => self.time = Some(file),
            ImportKind::Expenses => self.expenses = Some(file),
            ImportKind::Invoices => self.invoices = Some(file),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportBatchState {
    Draft,
    Reviewed,
    Committed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportCommitOutcome {
    pub committed_at: DateTime<Utc>,
    pub revision: String,
    pub entries: Vec<Uuid>,
    pub expenses: Vec<Uuid>,
    pub invoices: Vec<String>,
    pub customers: Vec<Uuid>,
    pub projects: Vec<String>,
    pub tasks: Vec<String>,
    pub categories: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportBatch {
    pub id: Uuid,
    pub namespace: String,
    pub state: ImportBatchState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub files: ImportFiles,
    pub choices: ImportChoices,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<ImportCommitOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Prepared,
    Committed,
    RolledBack,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalFile {
    path: String,
    /// Previous bytes of a sidecar; `None` means it did not exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImportJournal {
    batch_id: Uuid,
    state: JournalState,
    created: Vec<String>,
    /// Sidecars the commit mutates (invoice/entry/expense indexes and number
    /// ledgers), captured before the first write and restored on rollback.
    #[serde(default)]
    sidecars: Vec<JournalFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<ImportCommitOutcome>,
}

fn hex(bytes: &[u8]) -> String {
    crate::hex_lower(&Sha256::digest(bytes))
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Remove a staged path if it exists; a missing path is already the desired
/// state, so discarding a partially-staged or twice-discarded batch is safe.
fn remove_staged_path(path: &Path, directory: bool) -> Result<(), StoreError> {
    let outcome = if directory {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match outcome {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

impl Store {
    fn imports_dir(&self) -> PathBuf {
        self.root.join(IMPORTS_DIR)
    }

    fn import_batch_path(&self, id: Uuid) -> PathBuf {
        self.imports_dir().join(format!("{id}.json"))
    }

    fn import_source_path(&self, id: Uuid, kind: ImportKind) -> PathBuf {
        self.imports_dir()
            .join(id.to_string())
            .join(kind.file_name())
    }

    fn import_journal_path(&self, id: Uuid) -> PathBuf {
        self.imports_dir().join(format!(".journal.{id}.json"))
    }

    pub fn list_import_batches(&self) -> Result<Vec<ImportBatch>, StoreError> {
        let mut out = self.walk_docs(&self.imports_dir(), read_json::<ImportBatch>)?;
        out.sort_by_key(|batch| std::cmp::Reverse(batch.created_at));
        Ok(out)
    }

    pub fn get_import_batch(&self, id: Uuid) -> Result<Option<ImportBatch>, StoreError> {
        read_json(&self.import_batch_path(id))
    }

    pub fn create_import_batch(&self, batch: ImportBatch) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        if self.import_batch_path(batch.id).exists() {
            return Err(StoreError::AlreadyExists("import batch".into()));
        }
        std::fs::create_dir_all(self.imports_dir())?;
        write_json(&self.import_batch_path(batch.id), &batch)
    }

    /// Discard a batch that never committed: its staged sources, its document
    /// and any journal. A committed batch is financial history and refuses;
    /// delete completed imports only through the ordinary invoice/entry/expense
    /// lifecycle, never through the import workspace.
    pub fn delete_import_batch(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let batch = self.get_import_batch(id)?.ok_or(StoreError::NotFound)?;
        if batch.state == ImportBatchState::Committed {
            return Err(StoreError::Conflict(
                "a committed import batch is financial history and cannot be discarded".into(),
            ));
        }
        if let Some(journal) = read_json::<ImportJournal>(&self.import_journal_path(id))? {
            match journal.state {
                // The documents exist even if the batch write never happened:
                // this is committed history, not a draft.
                JournalState::Committed => {
                    return Err(StoreError::Conflict(
                        "a committed import batch is financial history and cannot be discarded"
                            .into(),
                    ));
                }
                // Retry a rollback whose first attempt failed. If it fails
                // again the journal stays Prepared and nothing is deleted, so
                // boot recovery can still finish it.
                JournalState::Prepared => self.rollback_import_journal(&journal)?,
                JournalState::RolledBack => {}
            }
        }
        remove_staged_path(&self.imports_dir().join(id.to_string()), true)?;
        remove_staged_path(&self.import_batch_path(id), false)?;
        remove_staged_path(&self.import_journal_path(id), false)?;
        Ok(())
    }

    /// Replace the reviewed mapping. Any source change or mapping change
    /// invalidates the previous preview revision.
    pub fn update_import_choices(
        &self,
        id: Uuid,
        choices: ImportChoices,
        now: DateTime<Utc>,
    ) -> Result<ImportBatch, StoreError> {
        let _guard = self.write_lock()?;
        let mut batch = self.get_import_batch(id)?.ok_or(StoreError::NotFound)?;
        if batch.state == ImportBatchState::Committed {
            return Err(StoreError::Conflict(
                "a committed import batch is immutable".into(),
            ));
        }
        batch.choices = choices;
        batch.state = ImportBatchState::Draft;
        batch.preview_revision = None;
        batch.commit = None;
        batch.error = None;
        batch.updated_at = now;
        write_json(&self.import_batch_path(id), &batch)?;
        Ok(batch)
    }

    /// Stage or replace exactly one source. A structurally invalid upload is
    /// rejected by the parser/inspector before this method is called by the
    /// API; a size rejection here never touches the previously staged file.
    pub fn stage_import_source(
        &self,
        id: Uuid,
        kind: ImportKind,
        name: &str,
        bytes: &[u8],
        now: DateTime<Utc>,
    ) -> Result<ImportBatch, StoreError> {
        if bytes.len() > kind.limit() {
            return Err(StoreError::Conflict(format!(
                "{} exceeds the {} byte limit",
                kind.file_name(),
                kind.limit()
            )));
        }
        let _guard = self.write_lock()?;
        let mut batch = self.get_import_batch(id)?.ok_or(StoreError::NotFound)?;
        if batch.state == ImportBatchState::Committed {
            return Err(StoreError::Conflict(
                "a committed import batch is immutable".into(),
            ));
        }
        let path = self.import_source_path(id, kind);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_bytes_atomic(&path, bytes)?;
        batch.files.set(
            kind,
            StagedImportFile {
                name: name.to_string(),
                sha256: hex(bytes),
                bytes: bytes.len() as u64,
                uploaded_at: now,
            },
        );
        batch.state = ImportBatchState::Draft;
        batch.preview_revision = None;
        batch.commit = None;
        batch.error = None;
        batch.updated_at = now;
        write_json(&self.import_batch_path(id), &batch)?;
        Ok(batch)
    }

    pub fn read_import_source(
        &self,
        id: Uuid,
        kind: ImportKind,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let batch = self.get_import_batch(id)?.ok_or(StoreError::NotFound)?;
        let expected = batch.files.get(kind);
        let path = self.import_source_path(id, kind);
        match std::fs::read(&path) {
            Ok(bytes) if bytes.len() > kind.limit() => Err(StoreError::Conflict(
                "staged import source exceeds its limit".into(),
            )),
            Ok(bytes) => {
                let Some(metadata) = expected else {
                    return Err(StoreError::Conflict(
                        "a staged source exists without batch metadata".into(),
                    ));
                };
                if bytes.len() as u64 != metadata.bytes || hex(&bytes) != metadata.sha256 {
                    return Err(StoreError::Conflict(
                        "the staged source no longer matches its recorded hash".into(),
                    ));
                }
                Ok(Some(bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if expected.is_some() {
                    Err(StoreError::Conflict(
                        "the staged source is missing from the batch".into(),
                    ))
                } else {
                    Ok(None)
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Record the current preview revision so the batch shows as reviewed.
    pub fn mark_import_reviewed(
        &self,
        id: Uuid,
        revision: &str,
        now: DateTime<Utc>,
    ) -> Result<ImportBatch, StoreError> {
        let _guard = self.write_lock()?;
        let mut batch = self.get_import_batch(id)?.ok_or(StoreError::NotFound)?;
        if batch.state == ImportBatchState::Committed {
            return Err(StoreError::Conflict(
                "a committed import batch is immutable".into(),
            ));
        }
        batch.state = ImportBatchState::Reviewed;
        batch.preview_revision = Some(revision.to_string());
        batch.error = None;
        batch.updated_at = now;
        write_json(&self.import_batch_path(id), &batch)?;
        Ok(batch)
    }

    /// Commit a reviewed batch. The caller's revision must match a fresh
    /// in-lock preview; an identical retry returns the original outcome.
    pub fn commit_import(
        &self,
        id: Uuid,
        expected_revision: &str,
        now: DateTime<Utc>,
    ) -> Result<ImportCommitOutcome, StoreError> {
        let _guard = self.write_lock()?;
        let mut batch = self.get_import_batch(id)?.ok_or(StoreError::NotFound)?;
        if let Some(outcome) = &batch.commit {
            if outcome.revision == expected_revision {
                return Ok(outcome.clone());
            }
            return Err(StoreError::Conflict(
                "the batch was already committed with a different revision".into(),
            ));
        }
        if batch.files.time.is_none()
            && batch.files.expenses.is_none()
            && batch.files.invoices.is_none()
        {
            return Err(StoreError::Conflict("the batch has no sources".into()));
        }

        let time_bytes = self.read_import_source(id, ImportKind::Time)?;
        let expense_bytes = self.read_import_source(id, ImportKind::Expenses)?;
        let invoice_bytes = self.read_import_source(id, ImportKind::Invoices)?;
        let time = match &time_bytes {
            Some(bytes) => Some(parse(ReportKind::Time, bytes).map_err(|issues| {
                StoreError::Conflict(format!(
                    "time CSV has {} blocking issue(s); preview it again",
                    issues.len()
                ))
            })?),
            None => None,
        };
        let expenses = match &expense_bytes {
            Some(bytes) => Some(parse(ReportKind::Expense, bytes).map_err(|issues| {
                StoreError::Conflict(format!(
                    "expense CSV has {} blocking issue(s); preview it again",
                    issues.len()
                ))
            })?),
            None => None,
        };
        let invoice_sources = match &invoice_bytes {
            Some(bytes) => inspect_invoice_zip(bytes).map_err(|error| {
                StoreError::Conflict(format!("invoice ZIP rejected: {}", error.message))
            })?,
            None => Vec::new(),
        };
        let candidates = invoice_sources
            .iter()
            .map(|source| crate::domain::InvoiceCandidate {
                original_name: source.original_name.clone(),
                sha256: source.sha256.clone(),
                inspection: inspect_invoice_text(source),
            })
            .collect::<Vec<_>>();

        let customers = self.list_customers()?;
        let mut projects = Vec::new();
        let mut tasks = Vec::new();
        for customer in &customers {
            let list = self.list_projects(customer.id)?;
            for project in &list {
                tasks.extend(self.list_tasks(customer.id, &project.code.0)?);
            }
            projects.extend(list);
        }
        let categories = self.list_categories()?;
        let users = self
            .list_users()?
            .into_iter()
            .map(|user| (user.id, user.name))
            .collect::<Vec<_>>();
        let entries = self.list_all_entries()?;
        let stored_expenses = self.list_expenses()?;
        let invoices = self.list_invoices()?;
        let view = crate::domain::StoreView {
            customers: &customers,
            projects: &projects,
            tasks: &tasks,
            categories: &categories,
            users: &users,
            entries: &entries,
            expenses: &stored_expenses,
            invoices: &invoices,
        };
        let preview = crate::domain::plan(
            &batch.namespace,
            time.as_ref(),
            expenses.as_ref(),
            &candidates,
            &batch.choices,
            &view,
        );
        if preview.revision != expected_revision || !preview.ready {
            return Err(StoreError::Conflict(
                "the preview is stale or not ready; recompute it before committing".into(),
            ));
        }

        // Precompute every path and value this commit may create, then journal
        // all paths before the first write.
        let mut created: Vec<String> = Vec::new();
        let mut pending_refs: Vec<(PathBuf, serde_json::Value)> = Vec::new();
        let mut customer_ids = Vec::new();
        let mut project_keys = Vec::new();
        let mut task_keys = Vec::new();
        let mut category_ids = Vec::new();

        for choice in &batch.choices.customers {
            if let crate::domain::CustomerChoice::Create {
                source_client,
                name,
                currency,
                default_rate_minor,
            } = choice
            {
                let cid = crate::domain::planned_customer_id(source_client);
                let path = self.doc_path::<Customer>(&cid.to_string());
                if path.exists() {
                    continue;
                }
                self.ensure_capacity::<Customer>()?;
                let customer: Customer = serde_json::from_value(serde_json::json!({
                    "id": cid,
                    "name": name,
                    "currency": currency,
                    "default_rate_minor": default_rate_minor,
                    "active": true,
                }))
                .map_err(|error| StoreError::Io(format!("invalid new customer: {error}")))?;
                created.push(relative(&self.root, &path));
                pending_refs.push((
                    path,
                    serde_json::to_value(&customer)
                        .map_err(|error| StoreError::Io(error.to_string()))?,
                ));
                customer_ids.push(cid);
            }
        }
        for choice in &batch.choices.projects {
            if let crate::domain::ProjectChoice::Create {
                source_client,
                source_project,
                source_code,
                customer_id,
                name,
                currency,
                rate_minor,
                ..
            } = choice
            {
                // The planner derives the safe code and, when the customer is
                // created in the same batch, the planned customer id. Reuse
                // those planned values so the commit writes exactly what the
                // reviewed preview showed.
                let planned_code = preview
                    .time
                    .iter()
                    .find(|row| {
                        row.source_client == *source_client
                            && row.source_project == *source_project
                            && row.source_code == *source_code
                    })
                    .and_then(|row| row.project_code.clone())
                    .or_else(|| {
                        preview
                            .expenses
                            .iter()
                            .find(|row| {
                                row.source_client == *source_client
                                    && row.source_project == *source_project
                                    && row.source_code == *source_code
                            })
                            .and_then(|row| row.project_code.clone())
                    });
                let Some(planned_code) = planned_code else {
                    continue;
                };
                let customer_id = customer_id
                    .unwrap_or_else(|| crate::domain::planned_customer_id(source_client));
                let code = ProjectCode(planned_code);
                let path = self.project_path(customer_id, &code.0);
                if path.exists() {
                    continue;
                }
                let project = Project {
                    customer_id,
                    code: code.clone(),
                    name: name.clone(),
                    currency: currency.clone(),
                    rate_minor: *rate_minor,
                    active: true,
                    budget_hours: None,
                    budget_amount_minor: None,
                };
                created.push(relative(&self.root, &path));
                pending_refs.push((
                    path,
                    serde_json::to_value(&project)
                        .map_err(|error| StoreError::Io(error.to_string()))?,
                ));
                project_keys.push(format!("{customer_id}/{}", code.0));
            }
        }
        // A task choice is created under the first planned time row that uses
        // it; a task with no included row creates nothing.
        let mut task_key_set: HashSet<String> = HashSet::new();
        for choice in &batch.choices.tasks {
            if let crate::domain::TaskChoice::Create {
                source_task, name, ..
            } = choice
                && let Some(row) = preview.time.iter().find(|row| {
                    row.decision == ImportDecision::New
                        && row.source_task == *source_task
                        && row.task_code.is_some()
                })
                && let (Some(customer_id), Some(project_code), Some(task_code)) = (
                    row.customer_id,
                    row.project_code.as_deref(),
                    row.task_code.as_deref(),
                )
            {
                let code = ProjectCode(task_code.to_string());
                let path = self.task_path(customer_id, project_code, &code.0);
                if path.exists() {
                    continue;
                }
                let key = format!("{customer_id}/{project_code}/{}", code.0);
                if !task_key_set.insert(key.clone()) {
                    return Err(StoreError::Conflict(
                        "two new source tasks resolve to the same code; review the task mappings"
                            .into(),
                    ));
                }
                let task = Task {
                    customer_id,
                    project_code: ProjectCode(project_code.to_string()),
                    code,
                    name: name.clone(),
                    active: true,
                };
                created.push(relative(&self.root, &path));
                pending_refs.push((
                    path,
                    serde_json::to_value(&task)
                        .map_err(|error| StoreError::Io(error.to_string()))?,
                ));
                task_keys.push(key);
            }
        }
        for choice in &batch.choices.categories {
            if let crate::domain::CategoryChoice::Create {
                source_category,
                name,
                billable,
            } = choice
            {
                let cid = crate::domain::planned_category_id(source_category);
                let path = self.doc_path::<Category>(&cid.to_string());
                if path.exists() {
                    continue;
                }
                self.ensure_capacity::<Category>()?;
                let category = Category {
                    id: cid,
                    name: name.clone(),
                    default_billable: *billable,
                    active: true,
                };
                created.push(relative(&self.root, &path));
                pending_refs.push((
                    path,
                    serde_json::to_value(&category)
                        .map_err(|error| StoreError::Io(error.to_string()))?,
                ));
                category_ids.push(cid);
            }
        }

        let mut entry_ids = Vec::new();
        let mut new_entries = Vec::new();
        for row in preview
            .time
            .iter()
            .filter(|row| row.decision == ImportDecision::New)
        {
            let id = crate::domain::planned_time_id(&batch.namespace, &row.source_key);
            let Some(date) = row.date else {
                return Err(StoreError::Conflict("a planned entry has no date".into()));
            };
            if self.get_entry(id)?.is_some() {
                return Err(StoreError::Conflict(
                    "a planned entry already exists; preview is stale".into(),
                ));
            }
            let Some(customer_id) = row.customer_id else {
                return Err(StoreError::Conflict(
                    "a planned entry has no customer".into(),
                ));
            };
            let Some(project_code) = row.project_code.as_deref() else {
                return Err(StoreError::Conflict(
                    "a planned entry has no project".into(),
                ));
            };
            let Some(hours) = row.hours_hundredths else {
                return Err(StoreError::Conflict("a planned entry has no hours".into()));
            };
            let Some(currency) = row.currency.as_deref().and_then(Currency::parse) else {
                return Err(StoreError::Conflict(
                    "a planned entry has no currency".into(),
                ));
            };
            let entry = Entry {
                imported: Some(ImportedTime {
                    origin: ImportOrigin {
                        provider: ImportProvider::Harvest,
                        namespace: batch.namespace.clone(),
                        batch_id: batch.id,
                        source_key: row.source_key.clone(),
                        invoiced: row.invoiced,
                    },
                    currency,
                    rate_minor: row.rate_minor.unwrap_or(0),
                    cost_rate_minor: row.cost_rate_minor.unwrap_or(0),
                    source_hours: Hours(hours),
                    amount_minor: row.amount_minor.unwrap_or(0),
                    cost_amount_minor: row.cost_amount_minor.unwrap_or(0),
                }),
                id,
                date,
                customer_id,
                user_id: row.user_id,
                project_code: ProjectCode(project_code.to_string()),
                task_code: row.task_code.clone().map(ProjectCode),
                hours: Hours(hours),
                note: row.note.clone(),
                billable: row.billable,
                source: Source::HarvestImport,
                created_at: now,
                updated_at: now,
            };
            created.push(relative(&self.root, &self.entry_path(date, id)));
            entry_ids.push(id);
            new_entries.push(entry);
        }

        let mut expense_ids = Vec::new();
        let mut new_expenses = Vec::new();
        for row in preview
            .expenses
            .iter()
            .filter(|row| row.decision == ImportDecision::New)
        {
            let id = crate::domain::planned_expense_id(&batch.namespace, &row.source_key);
            let Some(date) = row.date else {
                return Err(StoreError::Conflict("a planned expense has no date".into()));
            };
            if self.get_expense(id)?.is_some() {
                return Err(StoreError::Conflict(
                    "a planned expense already exists; preview is stale".into(),
                ));
            }
            let Some(customer_id) = row.customer_id else {
                return Err(StoreError::Conflict(
                    "a planned expense has no customer".into(),
                ));
            };
            let Some(amount_minor) = row.amount_minor else {
                return Err(StoreError::Conflict(
                    "a planned expense has no amount".into(),
                ));
            };
            let Some(currency) = row.currency.as_deref().and_then(Currency::parse) else {
                return Err(StoreError::Conflict(
                    "a planned expense has no currency".into(),
                ));
            };
            let expense = Expense {
                imported: Some(ImportOrigin {
                    provider: ImportProvider::Harvest,
                    namespace: batch.namespace.clone(),
                    batch_id: batch.id,
                    source_key: row.source_key.clone(),
                    invoiced: row.invoiced,
                }),
                id,
                date,
                customer_id,
                user_id: row.user_id,
                project_code: row.project_code.clone().map(ProjectCode),
                category_id: row.category_id,
                amount_minor,
                currency,
                billable: row.billable,
                note: row.note.clone(),
                original_amount_minor: None,
                original_currency: None,
                exchange_rate: None,
                exchange_rate_date: None,
                receipt_name: None,
                receipt_b64: None,
                created_at: now,
                updated_at: now,
            };
            created.push(relative(
                &self.root,
                &self.doc_path::<Expense>(&id.to_string()),
            ));
            expense_ids.push(id);
            new_expenses.push(expense);
        }

        let mut invoice_numbers = Vec::new();
        let mut new_invoices = Vec::new();
        let mut invoice_pdfs = Vec::new();
        let mut taken: HashSet<String> = invoices.iter().map(|i| i.number.clone()).collect();
        for row in preview
            .invoices
            .iter()
            .filter(|row| row.decision == ImportDecision::New)
        {
            let id = crate::domain::planned_invoice_id(&batch.namespace, &row.source_key);
            if self.get_invoice(id)?.is_some() {
                return Err(StoreError::Conflict(
                    "a planned invoice already exists; preview is stale".into(),
                ));
            }
            if !taken.insert(row.original_number.clone()) {
                return Err(StoreError::Conflict(
                    "an invoice number is already taken; preview is stale".into(),
                ));
            }
            let Some(customer_id) = row.customer_id else {
                return Err(StoreError::Conflict(
                    "a planned invoice has no customer".into(),
                ));
            };
            let Some(currency) = row.currency.as_deref().and_then(Currency::parse) else {
                return Err(StoreError::Conflict(
                    "a planned invoice has no currency".into(),
                ));
            };
            let Some(period_from) = row.period_from else {
                return Err(StoreError::Conflict(
                    "a planned invoice has no reviewed period_from".into(),
                ));
            };
            let Some(period_to) = row.period_to else {
                return Err(StoreError::Conflict(
                    "a planned invoice has no reviewed period_to".into(),
                ));
            };
            let Some(total_minor) = row.total_minor else {
                return Err(StoreError::Conflict(
                    "a planned invoice has no total".into(),
                ));
            };
            let Some(status) = row.status else {
                return Err(StoreError::Conflict(
                    "a planned invoice has no reviewed status".into(),
                ));
            };
            let source = invoice_sources
                .iter()
                .find(|source| source.sha256 == row.pdf_sha256)
                .ok_or_else(|| StoreError::Conflict("a planned invoice PDF is missing".into()))?;
            let pdf_path = self.invoice_pdf_path(&id.to_string());
            created.push(relative(
                &self.root,
                &self.doc_path::<Invoice>(&id.to_string()),
            ));
            created.push(relative(&self.root, &pdf_path));
            invoice_numbers.push(row.original_number.clone());
            invoice_pdfs.push((pdf_path, source.pdf.clone()));
            new_invoices.push(Invoice {
                supersedes: None,
                superseded_by: None,
                imported: Some(ImportedInvoice {
                    origin: ImportOrigin {
                        provider: ImportProvider::Harvest,
                        namespace: batch.namespace.clone(),
                        batch_id: batch.id,
                        source_key: row.source_key.clone(),
                        invoiced: true,
                    },
                    original_number: row.original_number.clone(),
                    pdf_sha256: row.pdf_sha256.clone(),
                    opening_paid_minor: row.opening_paid_minor,
                }),
                document: crate::domain::InvoiceDocument {
                    invoice_date: row.invoice_date,
                    ..Default::default()
                },
                id,
                number: row.original_number.clone(),
                customer_id,
                currency,
                period_from,
                period_to,
                lines: vec![InvoiceLine {
                    kind: LineKind::Fixed,
                    date: row.invoice_date.unwrap_or(period_from),
                    entry_id: None,
                    expense_id: None,
                    project_code: None,
                    task_code: None,
                    hours: None,
                    rate_minor: None,
                    amount_minor: total_minor,
                    note: format!("Imported original invoice {}", row.original_number),
                    quantity_hundredths: None,
                    unit_price_minor: None,
                    item_kind: None,
                }],
                total_minor,
                status,
                created_at: now,
                issued_at: None,
                due_date: None,
                paid_at: None,
                payment_reference: String::new(),
                tax_hundredths: 0,
                discount_hundredths: 0,
                pdf: None,
                payments: Vec::new(),
                write_off_reason: if status == InvoiceStatus::WrittenOff {
                    "Imported as written off in the source system".into()
                } else {
                    String::new()
                },
                written_off_at: None,
                subject: String::new(),
                terms: None,
            });
        }

        if !new_invoices.is_empty() {
            self.ensure_capacity::<Invoice>()?;
        }
        if !new_expenses.is_empty() {
            self.ensure_capacity::<Expense>()?;
        }

        let sidecars = self.capture_import_sidecars()?;
        let journal = ImportJournal {
            batch_id: batch.id,
            state: JournalState::Prepared,
            created: created.clone(),
            sidecars,
            outcome: None,
        };
        std::fs::create_dir_all(self.imports_dir())?;
        write_json(&self.import_journal_path(batch.id), &journal)?;

        let result = (|| -> Result<ImportCommitOutcome, StoreError> {
            for (path, value) in &pending_refs {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                write_json(path, value)?;
            }
            for entry in &new_entries {
                std::fs::create_dir_all(self.day_dir(entry.date))?;
                write_json(&self.entry_path(entry.date, entry.id), entry)?;
                self.rehydrate_entry_locked(entry.date, entry.id)?;
                self.index_entry_locked(entry)?;
            }
            for expense in &new_expenses {
                std::fs::create_dir_all(self.doc_dir::<Expense>())?;
                write_json(&self.doc_path::<Expense>(&expense.id.to_string()), expense)?;
                self.index_expense_locked(expense)?;
            }
            for (path, bytes) in &invoice_pdfs {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                write_bytes_atomic(path, bytes)?;
            }
            for invoice in &mut new_invoices {
                invoice.pdf = Some(crate::domain::PdfHint {
                    filename: format!("{}.pdf", crate::domain::safe_filename(&invoice.number)),
                    bytes: invoice_pdfs
                        .iter()
                        .find(|(path, _)| *path == self.invoice_pdf_path(&invoice.id.to_string()))
                        .map_or(0, |(_, bytes)| bytes.len() as u64),
                    sha256: invoice
                        .imported
                        .as_ref()
                        .map_or_else(String::new, |imported| imported.pdf_sha256.clone()),
                    archived_at: now,
                });
                write_json(&self.doc_path::<Invoice>(&invoice.id.to_string()), invoice)?;
                self.index_invoice_locked(invoice)?;
                self.advance_invoice_high_water(&invoice.number)?;
            }
            Ok(ImportCommitOutcome {
                committed_at: now,
                revision: preview.revision.clone(),
                entries: entry_ids.clone(),
                expenses: expense_ids.clone(),
                invoices: invoice_numbers.clone(),
                customers: customer_ids.clone(),
                projects: project_keys.clone(),
                tasks: task_keys.clone(),
                categories: category_ids.clone(),
            })
        })();

        match result {
            Ok(outcome) => {
                let mut committed = journal;
                committed.state = JournalState::Committed;
                committed.outcome = Some(outcome.clone());
                write_json(&self.import_journal_path(batch.id), &committed)?;
                batch.state = ImportBatchState::Committed;
                batch.preview_revision = Some(preview.revision.clone());
                batch.commit = Some(outcome.clone());
                batch.updated_at = now;
                write_json(&self.import_batch_path(batch.id), &batch)?;
                Ok(outcome)
            }
            Err(error) => {
                let cleanup = self.rollback_import_journal(&journal);
                batch.state = ImportBatchState::Failed;
                batch.error = Some(match &cleanup {
                    Ok(()) => error.to_string(),
                    Err(cleanup) => format!("{error}; rollback incomplete: {cleanup}"),
                });
                batch.updated_at = now;
                let _ = write_json(&self.import_batch_path(batch.id), &batch);
                match cleanup {
                    Ok(()) => Err(error),
                    // The journal stays Prepared so boot recovery retries it.
                    Err(cleanup) => Err(StoreError::Io(format!(
                        "{error}; rollback incomplete: {cleanup}"
                    ))),
                }
            }
        }
    }

    /// Sidecars the commit mutates. Their previous bytes are journaled and
    /// restored on rollback, so an index cannot point at a removed record and
    /// a retry cannot observe an advanced number ledger.
    fn import_sidecar_paths(&self) -> Vec<PathBuf> {
        [
            "invoices/.seq.json",
            "invoices/.seq.prefixes.json",
            "invoices/.idx.numbers.json",
            "entries/.idx.users.json",
            "expenses/.idx.users.json",
        ]
        .into_iter()
        .map(|relative| self.root.join(relative))
        .collect()
    }

    fn capture_import_sidecars(&self) -> Result<Vec<JournalFile>, StoreError> {
        self.import_sidecar_paths()
            .into_iter()
            .map(|path| {
                let bytes = match std::fs::read(&path) {
                    Ok(bytes) => Some(bytes),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                };
                Ok(JournalFile {
                    path: relative(&self.root, &path),
                    bytes,
                })
            })
            .collect()
    }

    fn restore_import_sidecars(&self, sidecars: &[JournalFile]) -> Result<(), StoreError> {
        for sidecar in sidecars {
            let path = self.root.join(&sidecar.path);
            match &sidecar.bytes {
                Some(bytes) => write_bytes_atomic(&path, bytes)?,
                None => match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                },
            }
        }
        Ok(())
    }

    /// Remove every journaled document, restore the sidecars, then mark the
    /// journal rolled back. On any cleanup failure the journal stays
    /// `Prepared`, so boot recovery can retry it.
    fn rollback_import_journal(&self, journal: &ImportJournal) -> Result<(), StoreError> {
        for created in journal.created.iter().rev() {
            match std::fs::remove_file(self.root.join(created)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.restore_import_sidecars(&journal.sidecars)?;
        let mut rolled_back = journal.clone();
        rolled_back.state = JournalState::RolledBack;
        write_json(&self.import_journal_path(journal.batch_id), &rolled_back)
    }

    /// Advance the local ledger when an imported legal number uses local
    /// syntax. Never lowers a sequence, never rewrites the imported number,
    /// and never creates an unrelated prefix sequence.
    fn advance_invoice_high_water(&self, number: &str) -> Result<(), StoreError> {
        if let Some(rest) = number.strip_prefix("INV-")
            && let Ok(n) = rest.parse::<u64>()
        {
            let path = self.root.join("invoices").join(".seq.json");
            let stored: u64 = read_json(&path)?.unwrap_or(0);
            if n > stored {
                write_json(&path, &n)?;
            }
        }
        if let Some((prefix, rest)) = number.rsplit_once('-')
            && let Ok(n) = rest.parse::<u64>()
            && let Some(normalized) = crate::domain::normalize_invoice_prefix(prefix)
        {
            let path = self.root.join("invoices").join(".seq.prefixes.json");
            let mut sequences: BTreeMap<String, u64> = read_json(&path)?.unwrap_or_default();
            if sequences.get(&normalized).is_some_and(|value| *value < n) {
                sequences.insert(normalized, n);
                write_json(&path, &sequences)?;
            }
        }
        Ok(())
    }

    /// Roll back journals a previous boot left in `Prepared`. Called during
    /// `Store::open`, before the server serves traffic.
    pub fn recover_import_journals(&self) -> Result<usize, StoreError> {
        let dir = self.imports_dir();
        if !dir.exists() {
            return Ok(0);
        }
        let _guard = self.write_lock()?;
        let mut recovered = 0;
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !name.starts_with(".journal.") || !name.ends_with(".json") {
                continue;
            }
            let Some(mut journal) = read_json::<ImportJournal>(&path)? else {
                continue;
            };
            match journal.state {
                JournalState::Prepared => {
                    // A cleanup failure propagates: boot must fail closed rather
                    // than serve a partially imported store.
                    self.rollback_import_journal(&journal)?;
                    if let Some(mut batch) = self.get_import_batch(journal.batch_id)?
                        && batch.state != ImportBatchState::Committed
                    {
                        batch.state = ImportBatchState::Failed;
                        batch.error =
                            Some("an interrupted commit was rolled back; retry the import".into());
                        write_json(&self.import_batch_path(batch.id), &batch)?;
                    }
                    recovered += 1;
                }
                JournalState::Committed => {
                    // Crash between the journal commit and the batch update:
                    // finalize the batch from the journaled outcome so a retry
                    // returns the same ids instead of seeing them as stale.
                    let Some(outcome) = &journal.outcome else {
                        return Err(StoreError::Conflict(
                            "a committed import journal has no outcome".into(),
                        ));
                    };
                    if let Some(mut batch) = self.get_import_batch(journal.batch_id)?
                        && batch.state != ImportBatchState::Committed
                    {
                        batch.state = ImportBatchState::Committed;
                        batch.preview_revision = Some(outcome.revision.clone());
                        batch.commit = Some(outcome.clone());
                        batch.updated_at = outcome.committed_at;
                        batch.error = None;
                        write_json(&self.import_batch_path(batch.id), &batch)?;
                        recovered += 1;
                    }
                    let _ = &mut journal;
                }
                JournalState::RolledBack => {}
            }
        }
        Ok(recovered)
    }
}

impl Store {
    /// Compute the #400 preview from the staged sources without writing.
    pub fn import_preview(&self, id: Uuid) -> Result<crate::domain::ImportPreview, StoreError> {
        let batch = self.get_import_batch(id)?.ok_or(StoreError::NotFound)?;
        let time = match self.read_import_source(id, ImportKind::Time)? {
            Some(bytes) => Some(parse(ReportKind::Time, &bytes).map_err(|issues| {
                StoreError::Conflict(format!("time CSV has {} issue(s)", issues.len()))
            })?),
            None => None,
        };
        let expenses = match self.read_import_source(id, ImportKind::Expenses)? {
            Some(bytes) => Some(parse(ReportKind::Expense, &bytes).map_err(|issues| {
                StoreError::Conflict(format!("expense CSV has {} issue(s)", issues.len()))
            })?),
            None => None,
        };
        let candidates = match self.read_import_source(id, ImportKind::Invoices)? {
            Some(bytes) => inspect_invoice_zip(&bytes)
                .map_err(|error| {
                    StoreError::Conflict(format!("invoice ZIP rejected: {}", error.message))
                })?
                .iter()
                .map(|source| crate::domain::InvoiceCandidate {
                    original_name: source.original_name.clone(),
                    sha256: source.sha256.clone(),
                    inspection: inspect_invoice_text(source),
                })
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };
        let customers = self.list_customers()?;
        let mut projects = Vec::new();
        let mut tasks = Vec::new();
        for customer in &customers {
            let list = self.list_projects(customer.id)?;
            for project in &list {
                tasks.extend(self.list_tasks(customer.id, &project.code.0)?);
            }
            projects.extend(list);
        }
        let categories = self.list_categories()?;
        let users = self
            .list_users()?
            .into_iter()
            .map(|user| (user.id, user.name))
            .collect::<Vec<_>>();
        let entries = self.list_all_entries()?;
        let expenses_stored = self.list_expenses()?;
        let invoices = self.list_invoices()?;
        let view = crate::domain::StoreView {
            customers: &customers,
            projects: &projects,
            tasks: &tasks,
            categories: &categories,
            users: &users,
            entries: &entries,
            expenses: &expenses_stored,
            invoices: &invoices,
        };
        Ok(crate::domain::plan(
            &batch.namespace,
            time.as_ref(),
            expenses.as_ref(),
            &candidates,
            &batch.choices,
            &view,
        ))
    }

    /// Read one original PDF from the staged ZIP by exact SHA-256. The path is
    /// never taken from the caller; only a validated member hash is accepted.
    pub fn import_invoice_pdf(
        &self,
        id: Uuid,
        sha256: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(bytes) = self.read_import_source(id, ImportKind::Invoices)? else {
            return Ok(None);
        };
        let sources = inspect_invoice_zip(&bytes).map_err(|error| {
            StoreError::Conflict(format!("invoice ZIP rejected: {}", error.message))
        })?;
        Ok(sources
            .into_iter()
            .find(|source| source.sha256 == sha256)
            .map(|source| source.pdf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, User};
    use crate::domain::{
        CategoryChoice, CustomerChoice, ImportChoices, InvoiceChoice, PersonChoice, ProjectChoice,
        ProjectCode, TaskChoice,
    };
    use serde_json::json;

    fn invoice_zip(member: &str, pdf: &[u8]) -> Vec<u8> {
        use std::io::{Cursor, Write as _};
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                member,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(pdf).unwrap();
        writer.finish().unwrap().into_inner()
    }

    /// Make a directory unwritable and report whether the OS enforces it.
    /// A root test run (or a filesystem that ignores modes) skips the fault
    /// test instead of pretending the boundary was exercised.
    #[cfg(unix)]
    fn deny_writes(dir: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let probe = dir.join(".probe");
        let blocked = std::fs::write(&probe, b"x").is_err();
        let _ = std::fs::remove_file(&probe);
        blocked
    }

    #[cfg(unix)]
    fn allow_writes(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    const TIME_HEADER: &str = "Date,Client,Project,Project Code,Task,Notes,Hours,Billable?,Invoiced?,First Name,Last Name,Employee Id,Billable Rate,Billable Amount,Cost Rate,Cost Amount,Currency\n";

    fn fixture() -> (tempfile::TempDir, Store, Uuid, Uuid, Uuid) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let cid = Uuid::new_v4();
        let uid = Uuid::new_v4();
        let cat = Uuid::new_v4();
        let customer: Customer = serde_json::from_value(json!({
            "id": cid, "name": "Acme", "currency": "EUR",
            "default_rate_minor": 6000, "active": true, "email": ""
        }))
        .unwrap();
        store.put_customer(&customer).unwrap();
        store
            .put_project(&Project {
                customer_id: cid,
                code: ProjectCode("WEB".into()),
                name: "Website".into(),
                currency: Currency("EUR".into()),
                rate_minor: 6500,
                active: true,
                budget_hours: None,
                budget_amount_minor: None,
            })
            .unwrap();
        store
            .put_task(&Task {
                customer_id: cid,
                project_code: ProjectCode("WEB".into()),
                code: ProjectCode("DESIGN".into()),
                name: "Design".into(),
                active: true,
            })
            .unwrap();
        store
            .put_category(&Category {
                id: cat,
                name: "Travel".into(),
                default_billable: true,
                active: true,
            })
            .unwrap();
        store
            .put_user(&User {
                id: uid,
                name: "Ada Lovelace".into(),
                email: "ada@example.test".into(),
                role: Role::Member,
                active: true,
                default_rate_minor: 0,
                cost_rate_minor: 0,
                password_hash: String::new(),
                created_at: Utc::now(),
                session_version: 1,
            })
            .unwrap();
        (dir, store, cid, uid, cat)
    }

    fn choices(cid: Uuid, uid: Uuid, cat: Uuid) -> ImportChoices {
        ImportChoices {
            users: vec![PersonChoice {
                person: "Ada Lovelace".into(),
                employee_id: "E1".into(),
                user_id: Some(uid),
                skip: false,
            }],
            customers: vec![CustomerChoice::Existing {
                source_client: "Acme".into(),
                customer_id: cid,
            }],
            projects: vec![ProjectChoice::Existing {
                source_client: "Acme".into(),
                source_project: "Website".into(),
                source_code: "WEB".into(),
                customer_id: cid,
                code: ProjectCode("WEB".into()),
            }],
            tasks: vec![TaskChoice::Existing {
                source_task: "Design".into(),
                code: ProjectCode("DESIGN".into()),
            }],
            categories: vec![CategoryChoice::Existing {
                source_category: "Travel".into(),
                category_id: cat,
            }],
            invoices: Vec::new(),
            excluded: Vec::new(),
            invoice_links: Vec::new(),
        }
    }

    fn batch(id: Uuid, choices: ImportChoices) -> ImportBatch {
        ImportBatch {
            id,
            namespace: "harvest-account".into(),
            state: ImportBatchState::Draft,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            files: ImportFiles::default(),
            choices,
            preview_revision: None,
            commit: None,
            error: None,
        }
    }

    fn csv() -> String {
        format!(
            "{TIME_HEADER}2026-01-05,Acme,Website,WEB,Design,Work,2.00,Yes,No,Ada,Lovelace,E1,65.00,130.00,50.00,100.00,EUR\n"
        )
    }

    #[test]
    fn commit_is_journaled_idempotent_and_duplicate_safe() {
        let (_dir, store, cid, uid, cat) = fixture();
        let choices = choices(cid, uid, cat);
        let id = Uuid::new_v4();
        store
            .create_import_batch(batch(id, choices.clone()))
            .unwrap();
        store
            .stage_import_source(
                id,
                ImportKind::Time,
                "time.csv",
                csv().as_bytes(),
                Utc::now(),
            )
            .unwrap();
        let preview = store.import_preview(id).unwrap();
        assert!(preview.ready, "{:?}", preview.issues);
        let outcome = store
            .commit_import(id, &preview.revision, Utc::now())
            .unwrap();
        assert_eq!(outcome.entries.len(), 1);
        let entry = store.get_entry(outcome.entries[0]).unwrap().unwrap();
        assert!(!entry.externally_invoiced());
        assert_eq!(entry.source, Source::HarvestImport);
        assert_eq!(
            entry.imported.as_ref().unwrap().origin.source_key,
            preview.time[0].source_key
        );
        assert_eq!(entry.hours, Hours(200));

        // The identical retry returns the same outcome, not a second record.
        let retry = store
            .commit_import(id, &preview.revision, Utc::now())
            .unwrap();
        assert_eq!(retry.entries, outcome.entries);
        assert_eq!(store.list_all_entries().unwrap().len(), 1);

        // A second batch with the same source sees a duplicate and cannot commit.
        let second_id = Uuid::new_v4();
        store
            .create_import_batch(batch(second_id, choices))
            .unwrap();
        store
            .stage_import_source(
                second_id,
                ImportKind::Time,
                "time.csv",
                csv().as_bytes(),
                Utc::now(),
            )
            .unwrap();
        let duplicate = store.import_preview(second_id).unwrap();
        assert_eq!(duplicate.time[0].decision, ImportDecision::Duplicate);
        assert!(!duplicate.ready);
        assert!(
            store
                .commit_import(second_id, &duplicate.revision, Utc::now())
                .is_err()
        );
    }

    #[test]
    fn startup_rolls_back_a_prepared_journal() {
        let (dir, store, _cid, _uid, _cat) = fixture();
        let id = Uuid::new_v4();
        let orphan = dir.path().join("data/customers/orphan.json");
        std::fs::write(&orphan, b"{}").unwrap();
        std::fs::create_dir_all(store.imports_dir()).unwrap();
        write_json(
            &store.import_journal_path(id),
            &ImportJournal {
                batch_id: id,
                state: JournalState::Prepared,
                created: vec!["customers/orphan.json".into()],
                sidecars: Vec::new(),
                outcome: None,
            },
        )
        .unwrap();
        drop(store);
        let reopened = Store::open(dir.path().join("data")).unwrap();
        assert!(
            !orphan.exists(),
            "a prepared journal is rolled back on boot"
        );
        let journal: ImportJournal = read_json(&reopened.import_journal_path(id))
            .unwrap()
            .unwrap();
        assert_eq!(journal.state, JournalState::RolledBack);
    }

    #[test]
    fn staged_source_tampering_fails_closed() {
        let (_dir, store, cid, uid, cat) = fixture();
        let id = Uuid::new_v4();
        store
            .create_import_batch(batch(id, choices(cid, uid, cat)))
            .unwrap();
        store
            .stage_import_source(
                id,
                ImportKind::Time,
                "time.csv",
                csv().as_bytes(),
                Utc::now(),
            )
            .unwrap();
        std::fs::write(
            store.import_source_path(id, ImportKind::Time),
            b"Date,Client\n2026-01-05,Changed\n",
        )
        .unwrap();
        assert!(store.import_preview(id).is_err());
        assert!(
            store
                .commit_import(id, &"0".repeat(64), Utc::now())
                .is_err()
        );
    }

    #[test]
    fn boot_finalizes_a_committed_journal_after_a_crash_before_the_batch_write() {
        let (dir, store, cid, uid, cat) = fixture();
        let id = Uuid::new_v4();
        store
            .create_import_batch(batch(id, choices(cid, uid, cat)))
            .unwrap();
        store
            .stage_import_source(
                id,
                ImportKind::Time,
                "time.csv",
                csv().as_bytes(),
                Utc::now(),
            )
            .unwrap();
        let preview = store.import_preview(id).unwrap();
        let outcome = store
            .commit_import(id, &preview.revision, Utc::now())
            .unwrap();
        // Simulate the crash window: the committed journal is durable but the
        // batch still looks like a draft.
        let mut batch = store.get_import_batch(id).unwrap().unwrap();
        batch.state = ImportBatchState::Draft;
        batch.commit = None;
        batch.preview_revision = None;
        write_json(&store.import_batch_path(id), &batch).unwrap();
        drop(store);
        let reopened = Store::open(dir.path().join("data")).unwrap();
        let batch = reopened.get_import_batch(id).unwrap().unwrap();
        assert_eq!(batch.state, ImportBatchState::Committed);
        assert_eq!(
            batch.commit.as_ref().unwrap().entries,
            outcome.entries,
            "recovery must preserve the journaled ids"
        );
    }

    #[test]
    fn discarding_a_batch_rolls_back_a_prepared_journal_and_removes_its_sources() {
        let (dir, store, cid, uid, cat) = fixture();
        let id = Uuid::new_v4();
        store
            .create_import_batch(batch(id, choices(cid, uid, cat)))
            .unwrap();
        store
            .stage_import_source(
                id,
                ImportKind::Time,
                "time.csv",
                csv().as_bytes(),
                Utc::now(),
            )
            .unwrap();
        let orphan = dir.path().join("data/customers/orphan.json");
        std::fs::write(&orphan, b"{}").unwrap();
        write_json(
            &store.import_journal_path(id),
            &ImportJournal {
                batch_id: id,
                state: JournalState::Prepared,
                created: vec!["customers/orphan.json".into()],
                sidecars: Vec::new(),
                outcome: None,
            },
        )
        .unwrap();

        store.delete_import_batch(id).unwrap();
        assert!(
            !orphan.exists(),
            "a discard rolls back an unfinished commit before deleting"
        );
        assert!(store.get_import_batch(id).unwrap().is_none());
        assert!(
            !store.imports_dir().join(id.to_string()).exists(),
            "staged sources are removed with the batch"
        );
        assert!(!store.import_journal_path(id).exists());
    }

    #[test]
    fn rollback_restores_the_number_ledger_sidecar() {
        let (dir, store, _cid, _uid, _cat) = fixture();
        let id = Uuid::new_v4();
        let seq = dir.path().join("data/invoices/.seq.json");
        std::fs::create_dir_all(seq.parent().unwrap()).unwrap();
        write_json(&seq, &7_u64).unwrap();
        let sidecars = store.capture_import_sidecars().unwrap();
        write_json(&seq, &99_u64).unwrap();
        std::fs::create_dir_all(store.imports_dir()).unwrap();
        write_json(
            &store.import_journal_path(id),
            &ImportJournal {
                batch_id: id,
                state: JournalState::Prepared,
                created: Vec::new(),
                sidecars,
                outcome: None,
            },
        )
        .unwrap();
        drop(store);
        let reopened = Store::open(dir.path().join("data")).unwrap();
        let restored: u64 = read_json(&seq).unwrap().unwrap();
        assert_eq!(restored, 7, "rollback restores the previous ledger value");
        let journal: ImportJournal = read_json(&reopened.import_journal_path(id))
            .unwrap()
            .unwrap();
        assert_eq!(journal.state, JournalState::RolledBack);
    }

    #[cfg(unix)]
    #[test]
    fn a_failure_at_the_pdf_boundary_rolls_back_the_documents_and_retries_the_same_number() {
        let (_dir, store, cid, uid, cat) = fixture();
        let id = Uuid::new_v4();
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
        let sha = hex(pdf);
        let mut mapping = choices(cid, uid, cat);
        mapping.invoices = vec![InvoiceChoice {
            sha256: sha,
            customer_id: Some(cid),
            original_number: "INV-0050".into(),
            currency: Some(Currency("EUR".into())),
            invoice_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 31),
            period_from: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            period_to: chrono::NaiveDate::from_ymd_opt(2026, 1, 31),
            total_minor: Some(20_000),
            opening_paid_minor: 0,
            status: Some(InvoiceStatus::Issued),
            excluded: false,
        }];
        store.create_import_batch(batch(id, mapping)).unwrap();
        store
            .stage_import_source(
                id,
                ImportKind::Time,
                "time.csv",
                csv().as_bytes(),
                Utc::now(),
            )
            .unwrap();
        store
            .stage_import_source(
                id,
                ImportKind::Invoices,
                "invoices.zip",
                &invoice_zip("invoices/INV-0050.pdf", pdf),
                Utc::now(),
            )
            .unwrap();
        let preview = store.import_preview(id).unwrap();
        assert!(preview.ready, "{:?}", preview.issues);

        if !deny_writes(&store.root.join("invoices")) {
            eprintln!("skip: this filesystem/user does not enforce directory write modes");
            allow_writes(&store.root.join("invoices"));
            return;
        }
        let failed = store.commit_import(id, &preview.revision, Utc::now());
        allow_writes(&store.root.join("invoices"));
        assert!(failed.is_err(), "the PDF write boundary fails the commit");
        assert_eq!(
            store.get_import_batch(id).unwrap().unwrap().state,
            ImportBatchState::Failed
        );
        let entry_id =
            crate::domain::planned_time_id("harvest-account", &preview.time[0].source_key);
        assert!(
            store.get_entry(entry_id).unwrap().is_none(),
            "the earlier entry write was rolled back"
        );
        assert!(store.find_invoice_by_number("INV-0050").unwrap().is_none());
        let seq: Option<u64> = read_json(&store.root.join("invoices/.seq.json")).unwrap();
        assert!(
            seq.is_none_or(|value| value < 50),
            "a failed import must not consume a number: {seq:?}"
        );

        // The same reviewed revision commits cleanly once the boundary is gone.
        let outcome = store
            .commit_import(id, &preview.revision, Utc::now())
            .unwrap();
        assert_eq!(outcome.invoices, vec!["INV-0050".to_string()]);
        assert!(store.get_entry(entry_id).unwrap().is_some());
        assert_eq!(store.list_invoices().unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_failure_at_the_document_boundary_keeps_pre_existing_data_and_retries() {
        let (_dir, store, cid, uid, cat) = fixture();
        let id = Uuid::new_v4();
        store
            .create_import_batch(batch(id, choices(cid, uid, cat)))
            .unwrap();
        store
            .stage_import_source(
                id,
                ImportKind::Time,
                "time.csv",
                csv().as_bytes(),
                Utc::now(),
            )
            .unwrap();
        let preview = store.import_preview(id).unwrap();
        assert!(preview.ready, "{:?}", preview.issues);

        let day = store.day_dir(chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
        if !deny_writes(&day) {
            eprintln!("skip: this filesystem/user does not enforce directory write modes");
            allow_writes(&day);
            return;
        }
        let failed = store.commit_import(id, &preview.revision, Utc::now());
        allow_writes(&day);
        assert!(failed.is_err());
        assert_eq!(
            store.get_import_batch(id).unwrap().unwrap().state,
            ImportBatchState::Failed
        );
        assert_eq!(store.list_customers().unwrap().len(), 1);
        assert_eq!(store.list_projects(cid).unwrap().len(), 1);
        assert_eq!(store.list_categories().unwrap().len(), 1);
        assert!(store.list_all_entries().unwrap().is_empty());

        let outcome = store
            .commit_import(id, &preview.revision, Utc::now())
            .unwrap();
        assert_eq!(outcome.entries.len(), 1);
        assert_eq!(store.list_all_entries().unwrap().len(), 1);
    }
}
