//! Unified Harvest import planner (#400).
//!
//! One deterministic preview for time, expenses and invoice PDFs. The planner
//! performs **no business writes**: it resolves reviewed mappings against the
//! current store, classifies every row (new / duplicate / conflict / correction
//! / excluded / unresolved), and returns a revision hash over the sources and
//! mappings. Commit revalidates that revision and the store under one writer
//! lock (#401).
//!
//! Two rules shape the design:
//!
//! - Source identity is canonical and stable across overlapping exports,
//!   independent of the batch: `provider:namespace:<canonical hash>:<occurrence>`.
//!   Legitimate identical rows keep their own occurrence, and re-importing the
//!   same export is a duplicate, never a second record.
//! - Nothing is guessed. Missing mappings, ambiguous invoice metadata and
//!   unresolved duplicates are blocking issues; the only inference is an exact,
//!   reviewed mapping supplied by the admin.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::expense::Category;
use super::primitives::{Currency, Hours, ProjectCode};
use super::{Customer, Entry, ImportProvider, Invoice, InvoiceStatus, Project, Task};
use crate::harvest::{Report, ReportKind, Row};
use crate::harvest_invoices::{PdfInspection, PdfInspectionStatus};

/// A stable namespace for ids this planner proposes. Preview and commit use the
/// same id for the same source reference, so a retried commit cannot create a
/// second customer/project/task/category or a second imported record.
const IMPORT_ID_NAMESPACE: Uuid = Uuid::from_u128(0x7c5a91e2_3b404d1f_9a6e2f0d_8b1c4e77);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportChoices {
    /// Source person (`First Last` + employee id) -> existing user.
    #[serde(default)]
    pub users: Vec<PersonChoice>,
    /// Harvest client -> existing customer or a reviewed new customer.
    #[serde(default)]
    pub customers: Vec<CustomerChoice>,
    /// Harvest project -> existing/new project under the chosen customer.
    #[serde(default)]
    pub projects: Vec<ProjectChoice>,
    /// Harvest task -> existing/new task on the chosen project.
    #[serde(default)]
    pub tasks: Vec<TaskChoice>,
    /// Harvest expense category -> existing/new category.
    #[serde(default)]
    pub categories: Vec<CategoryChoice>,
    /// Reviewed metadata for each invoice PDF (matched by exact SHA-256).
    #[serde(default)]
    pub invoices: Vec<InvoiceChoice>,
    /// Source keys the admin explicitly excluded.
    #[serde(default)]
    pub excluded: Vec<String>,
    /// Explicit, reviewed row -> invoice associations. Never inferred from
    /// client/date/amount resemblance.
    #[serde(default)]
    pub invoice_links: Vec<RowInvoiceLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonChoice {
    pub person: String,
    #[serde(default)]
    pub employee_id: String,
    /// `None` with `skip = false` is unresolved.
    #[serde(default)]
    pub user_id: Option<Uuid>,
    #[serde(default)]
    pub skip: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CustomerChoice {
    Existing {
        source_client: String,
        customer_id: Uuid,
    },
    Create {
        source_client: String,
        name: String,
        currency: Currency,
        default_rate_minor: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ProjectChoice {
    Existing {
        source_client: String,
        source_project: String,
        #[serde(default)]
        source_code: String,
        customer_id: Uuid,
        code: ProjectCode,
    },
    Create {
        source_client: String,
        source_project: String,
        #[serde(default)]
        source_code: String,
        /// Omitted by the GUI when the customer is created in the same pass;
        /// the planner then inherits the reviewed customer mapping's id.
        #[serde(default)]
        customer_id: Option<Uuid>,
        /// Raw reviewed code; normalised into a collision-safe `ProjectCode`.
        #[serde(default)]
        code: String,
        name: String,
        currency: Currency,
        #[serde(default)]
        rate_minor: u64,
    },
    /// Explicitly drop the project (expense rows only; time needs a project).
    Skip {
        source_client: String,
        source_project: String,
        #[serde(default)]
        source_code: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum TaskChoice {
    Existing {
        source_task: String,
        code: ProjectCode,
    },
    Create {
        source_task: String,
        /// Raw reviewed code; normalised into a collision-safe `ProjectCode`.
        #[serde(default)]
        code: String,
        name: String,
    },
    Skip {
        source_task: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CategoryChoice {
    Existing {
        source_category: String,
        category_id: Uuid,
    },
    Create {
        source_category: String,
        name: String,
        #[serde(default)]
        billable: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceChoice {
    pub sha256: String,
    /// Reviewed customer; `None` keeps the invoice unresolved.
    #[serde(default)]
    pub customer_id: Option<Uuid>,
    /// The legal number exactly as printed; never renumbered.
    #[serde(default)]
    pub original_number: String,
    #[serde(default)]
    pub currency: Option<Currency>,
    /// Date-only provenance from the original document.
    #[serde(default)]
    pub invoice_date: Option<NaiveDate>,
    /// Native invoice period, explicitly reviewed. Never invented from the
    /// invoice date.
    #[serde(default)]
    pub period_from: Option<NaiveDate>,
    #[serde(default)]
    pub period_to: Option<NaiveDate>,
    #[serde(default)]
    pub total_minor: Option<u64>,
    /// Reviewed historical settlement. Never derived from a filename or a PDF
    /// "PAID" stamp.
    #[serde(default)]
    pub opening_paid_minor: u64,
    /// Reviewed native status; `None` keeps the invoice unresolved.
    #[serde(default)]
    pub status: Option<InvoiceStatus>,
    #[serde(default)]
    pub excluded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowInvoiceLink {
    pub source_key: String,
    pub invoice_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportDecision {
    New,
    Duplicate,
    Conflict,
    Correction,
    Excluded,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanIssue {
    /// `time`, `expense` or `invoice`.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// ZIP member / CSV file name for a multi-source batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    pub field: String,
    pub message: String,
    /// Blocking issues prevent `ready` and therefore commit.
    pub blocking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTime {
    pub source_key: String,
    pub record: usize,
    pub line: u64,
    pub decision: ImportDecision,
    pub source_client: String,
    pub source_project: String,
    pub source_code: String,
    pub source_person: String,
    pub source_employee_id: String,
    pub source_task: String,
    pub date: Option<NaiveDate>,
    pub customer_id: Option<Uuid>,
    pub project_code: Option<String>,
    pub task_code: Option<String>,
    pub user_id: Option<Uuid>,
    pub hours_hundredths: Option<u32>,
    pub billable: bool,
    pub invoiced: bool,
    pub currency: Option<String>,
    pub rate_minor: Option<u64>,
    pub cost_rate_minor: Option<u64>,
    pub amount_minor: Option<u64>,
    pub cost_amount_minor: Option<u64>,
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoice_sha256: Option<String>,
    pub issues: Vec<PlanIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedExpense {
    pub source_key: String,
    pub record: usize,
    pub line: u64,
    pub decision: ImportDecision,
    pub source_client: String,
    pub source_project: String,
    pub source_code: String,
    pub source_person: String,
    pub source_employee_id: String,
    pub source_category: String,
    pub date: Option<NaiveDate>,
    pub customer_id: Option<Uuid>,
    pub project_code: Option<String>,
    pub category_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub amount_minor: Option<u64>,
    pub currency: Option<String>,
    pub billable: bool,
    pub invoiced: bool,
    pub note: String,
    pub issues: Vec<PlanIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedInvoice {
    pub source_key: String,
    pub original_name: String,
    pub pdf_sha256: String,
    pub decision: ImportDecision,
    pub customer_id: Option<Uuid>,
    pub original_number: String,
    pub currency: Option<String>,
    pub invoice_date: Option<NaiveDate>,
    pub period_from: Option<NaiveDate>,
    pub period_to: Option<NaiveDate>,
    pub total_minor: Option<u64>,
    pub opening_paid_minor: u64,
    pub status: Option<InvoiceStatus>,
    /// Parser suggestions are shown next to the reviewed values, never used as
    /// committed metadata on their own.
    pub suggestions: PdfInspection,
    pub issues: Vec<PlanIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationRow {
    pub source: String,
    pub currency: String,
    pub accepted_count: u64,
    pub accepted_minor: u64,
    pub cost_minor: u64,
    pub skipped_count: u64,
    pub skipped_minor: u64,
    pub opening_paid_minor: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportPreview {
    /// Hash over the exact source bytes/parsed reports and mapping choices.
    /// Commit must supply this value and fails with a stale-preview conflict
    /// when the mapping or sources changed.
    pub revision: String,
    pub ready: bool,
    pub blocking: usize,
    pub time: Vec<PlannedTime>,
    pub expenses: Vec<PlannedExpense>,
    pub invoices: Vec<PlannedInvoice>,
    pub totals: Vec<ReconciliationRow>,
    pub issues: Vec<PlanIssue>,
}

/// The authoritative store view the planner checks against. The API builds it
/// under the read path; preview never writes.
pub struct StoreView<'a> {
    pub customers: &'a [Customer],
    pub projects: &'a [Project],
    pub tasks: &'a [Task],
    pub categories: &'a [Category],
    /// `(id, display name)` for every user; the planner never creates users.
    pub users: &'a [(Uuid, String)],
    pub entries: &'a [Entry],
    pub expenses: &'a [super::Expense],
    pub invoices: &'a [Invoice],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceCandidate {
    pub original_name: String,
    pub sha256: String,
    pub inspection: PdfInspection,
}

fn digest(bytes: &[u8]) -> String {
    crate::hex_lower(&Sha256::digest(bytes))
}

fn canonical_row(row: &Row) -> String {
    // The retained raw fields make the identity independent of column order and
    // include every source value; parsed fields are added for clarity.
    serde_json::json!({
        "fields": &row.fields,
        "date": row.date,
        "client": row.client,
        "project": row.project,
        "project_code": row.project_code,
        "person": row.person,
        "employee_id": row.employee_id,
        "task": row.task,
        "category": row.category,
        "note": row.note,
        "currency": row.currency,
        "billable": row.billable,
        "invoiced": row.invoiced,
        "approved": row.approved,
        "hours": row.hours_hundredths,
        "amount_minor": row.amount_minor,
        "billable_rate_minor": row.billable_rate_minor,
        "billable_amount_minor": row.billable_amount_minor,
        "cost_rate_minor": row.cost_rate_minor,
        "cost_amount_minor": row.cost_amount_minor,
    })
    .to_string()
}

fn report_keys(kind: ReportKind, report: &Report) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    report
        .rows
        .iter()
        .map(|row| {
            let canonical = canonical_row(row);
            let hash = digest(canonical.as_bytes());
            let occurrence = counts.entry(hash.clone()).or_default();
            *occurrence += 1;
            let prefix = match kind {
                ReportKind::Time => "time",
                ReportKind::Expense => "expense",
            };
            format!("{prefix}:{hash}:{occurrence}")
        })
        .collect()
}

fn person_key(person: &str, employee_id: &str) -> String {
    format!(
        "{}|{}",
        person.trim().to_lowercase(),
        employee_id.trim().to_lowercase()
    )
}

fn customer_key(source_client: &str) -> String {
    source_client.trim().to_lowercase()
}

fn project_key(source_client: &str, source_project: &str, source_code: &str) -> String {
    format!(
        "{}|{}|{}",
        customer_key(source_client),
        source_project.trim().to_lowercase(),
        source_code.trim().to_uppercase()
    )
}

fn task_key(source_task: &str) -> String {
    source_task.trim().to_lowercase()
}

fn category_key(source_category: &str) -> String {
    source_category.trim().to_lowercase()
}

fn issue(
    source: &str,
    record: Option<usize>,
    line: Option<u64>,
    member: Option<&str>,
    field: &str,
    message: impl Into<String>,
    blocking: bool,
) -> PlanIssue {
    PlanIssue {
        source: source.into(),
        record,
        line,
        member: member.map(str::to_string),
        field: field.into(),
        message: message.into(),
        blocking,
    }
}

/// Deterministic, collision-safe code suggestion from a source name. The GUI
/// proposes this for a new reference; the admin still reviews and can edit it.
#[must_use]
pub fn suggest_code(name: &str, fallback: &str, taken: &BTreeSet<String>) -> String {
    let mut base: String = name
        .trim()
        .to_uppercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    base = base.trim_matches('_').to_string();
    if base.is_empty() {
        base = fallback
            .trim()
            .to_uppercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
    }
    if !base
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        base = format!("IMPORT_{base}");
    }
    base.truncate(24);
    if base.is_empty() {
        base = "IMPORTED".into();
    }
    let mut candidate = base.clone();
    let mut suffix = 2_u32;
    while taken.contains(&candidate) {
        let suffix_text = format!("_{suffix}");
        let keep = 24_usize.saturating_sub(suffix_text.len());
        candidate = format!("{}{suffix_text}", &base[..base.len().min(keep)]);
        suffix += 1;
    }
    candidate
}

fn plan_id(kind: &str, key: &str) -> Uuid {
    // UUIDv5-shaped id derived from SHA-256; the `uuid` crate is built without
    // its v5 feature, and this keeps the id stable without a new dependency
    // feature.
    let digest = Sha256::digest(format!("{IMPORT_ID_NAMESPACE}:{kind}:{key}"));
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn reviewed_customer(
    choice: Option<&CustomerChoice>,
    customers: &[Customer],
    issues: &mut Vec<PlanIssue>,
    source: &str,
    record: Option<usize>,
) -> Option<Uuid> {
    match choice {
        Some(CustomerChoice::Existing { customer_id, .. }) => {
            if customers.iter().any(|c| c.id == *customer_id) {
                Some(*customer_id)
            } else {
                issues.push(issue(
                    source,
                    record,
                    None,
                    None,
                    "customer_id",
                    "the mapped customer no longer exists",
                    true,
                ));
                None
            }
        }
        Some(CustomerChoice::Create {
            source_client,
            name,
            ..
        }) => {
            if name.trim().is_empty() || name.chars().count() > 120 {
                issues.push(issue(
                    source,
                    record,
                    None,
                    None,
                    "customer",
                    "a new customer needs a reviewed 1-120 character name",
                    true,
                ));
                None
            } else {
                // Stable id: the same reviewed source client always resolves to
                // the same new customer, even across a retried commit.
                Some(plan_id("customer", &customer_key(source_client)))
            }
        }
        None => {
            issues.push(issue(
                source,
                record,
                None,
                None,
                "customer",
                "no customer mapping was reviewed",
                true,
            ));
            None
        }
    }
}

fn choice_source_client(choice: &CustomerChoice) -> &str {
    match choice {
        CustomerChoice::Existing { source_client, .. } => source_client,
        CustomerChoice::Create { source_client, .. } => source_client,
    }
}

#[allow(clippy::too_many_arguments)]
fn reviewed_project(
    choice: Option<&ProjectChoice>,
    projects: &[Project],
    customer_id: Uuid,
    issues: &mut Vec<PlanIssue>,
    source: &str,
    record: Option<usize>,
    required: bool,
    taken: &mut BTreeMap<Uuid, BTreeSet<String>>,
) -> Option<String> {
    match choice {
        Some(ProjectChoice::Existing {
            customer_id: mapped,
            code,
            ..
        }) => {
            if *mapped != customer_id {
                issues.push(issue(
                    source,
                    record,
                    None,
                    None,
                    "project",
                    "the mapped project belongs to a different customer",
                    true,
                ));
                None
            } else if projects
                .iter()
                .any(|p| p.customer_id == customer_id && p.code == *code)
            {
                Some(code.0.clone())
            } else {
                issues.push(issue(
                    source,
                    record,
                    None,
                    None,
                    "project",
                    "the mapped project no longer exists",
                    true,
                ));
                None
            }
        }
        Some(ProjectChoice::Create {
            customer_id: mapped,
            code,
            name,
            ..
        }) => {
            if mapped.is_some_and(|mapped| mapped != customer_id) {
                issues.push(issue(
                    source,
                    record,
                    None,
                    None,
                    "project",
                    "the new project must belong to the mapped customer",
                    true,
                ));
                return None;
            }
            // The GUI sends the source code (or an empty string) plus the
            // reviewed name; prefer the source code and fall back to the name,
            // deriving a safe code instead of rejecting raw text. Project codes
            // are scoped to their customer, so only that customer's codes are
            // taken.
            let taken = taken.entry(customer_id).or_default();
            let code = ProjectCode(suggest_code(code, name, taken));
            if projects
                .iter()
                .any(|p| p.customer_id == customer_id && p.code == code)
            {
                issues.push(issue(
                    source,
                    record,
                    None,
                    None,
                    "project",
                    "a project with this code already exists",
                    true,
                ));
                None
            } else {
                taken.insert(code.0.clone());
                Some(code.0)
            }
        }
        Some(ProjectChoice::Skip { .. }) if !required => None,
        Some(ProjectChoice::Skip { .. }) => {
            issues.push(issue(
                source,
                record,
                None,
                None,
                "project",
                "time rows require a project",
                true,
            ));
            None
        }
        None if !required => None,
        None => {
            issues.push(issue(
                source,
                record,
                None,
                None,
                "project",
                "no project mapping was reviewed",
                true,
            ));
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn reviewed_task(
    choice: Option<&TaskChoice>,
    tasks: &[Task],
    customer_id: Option<Uuid>,
    project_code: Option<&str>,
    source_task: &str,
    issues: &mut Vec<PlanIssue>,
    source: &str,
    record: Option<usize>,
) -> Option<String> {
    if source_task.trim().is_empty() {
        return None;
    }
    match choice {
        Some(TaskChoice::Existing { code, .. }) => {
            if let (Some(customer_id), Some(project_code)) = (customer_id, project_code) {
                let exists = tasks.iter().any(|task| {
                    task.customer_id == customer_id
                        && task.project_code.0 == project_code
                        && task.code == *code
                });
                if !exists {
                    issues.push(issue(
                        source,
                        record,
                        None,
                        None,
                        "task",
                        "the mapped task does not exist on this project",
                        true,
                    ));
                    return None;
                }
            }
            Some(code.0.clone())
        }
        Some(TaskChoice::Create { code, name, .. }) => {
            // Derive a safe code from the reviewed name, avoiding the tasks
            // that already exist on this project. Raw GUI text never has to be
            // a valid ProjectCode before the request reaches the planner.
            let existing: BTreeSet<String> = match (customer_id, project_code) {
                (Some(customer_id), Some(project_code)) => tasks
                    .iter()
                    .filter(|task| {
                        task.customer_id == customer_id && task.project_code.0 == project_code
                    })
                    .map(|task| task.code.0.clone())
                    .collect(),
                _ => BTreeSet::new(),
            };
            Some(suggest_code(name, code, &existing))
        }
        Some(TaskChoice::Skip { .. }) => None,
        None => {
            issues.push(issue(
                source,
                record,
                None,
                None,
                "task",
                "the source row names a task but no task mapping was reviewed",
                true,
            ));
            None
        }
    }
}

fn reviewed_category(
    choice: Option<&CategoryChoice>,
    categories: &[Category],
    issues: &mut Vec<PlanIssue>,
    source: &str,
    record: Option<usize>,
) -> Option<Uuid> {
    match choice {
        Some(CategoryChoice::Existing { category_id, .. }) => {
            if categories.iter().any(|c| c.id == *category_id) {
                Some(*category_id)
            } else {
                issues.push(issue(
                    source,
                    record,
                    None,
                    None,
                    "category",
                    "the mapped category no longer exists",
                    true,
                ));
                None
            }
        }
        Some(CategoryChoice::Create { name, .. }) => {
            if name.trim().is_empty() || name.chars().count() > 120 {
                issues.push(issue(
                    source,
                    record,
                    None,
                    None,
                    "category",
                    "a new category needs a reviewed 1-120 character name",
                    true,
                ));
                None
            } else {
                Some(plan_id("category", &category_key(name)))
            }
        }
        None => {
            issues.push(issue(
                source,
                record,
                None,
                None,
                "category",
                "no category mapping was reviewed",
                true,
            ));
            None
        }
    }
}

fn parser_issues(report: &Report, source: &str, member: Option<&str>) -> Vec<PlanIssue> {
    report
        .issues
        .iter()
        .map(|issue| PlanIssue {
            source: source.into(),
            record: Some(issue.record),
            line: Some(issue.line),
            member: member.map(str::to_string),
            field: issue.field.clone(),
            message: issue.message.clone(),
            blocking: true,
        })
        .collect()
}

fn decision_for(base: ImportDecision, issues: &[PlanIssue], blocked: bool) -> ImportDecision {
    if issues.iter().any(|issue| issue.blocking) || blocked {
        ImportDecision::Unresolved
    } else {
        base
    }
}

/// Build the unified preview.
#[must_use]
pub fn plan(
    namespace: &str,
    time: Option<&Report>,
    expenses: Option<&Report>,
    invoice_candidates: &[InvoiceCandidate],
    choices: &ImportChoices,
    view: &StoreView<'_>,
) -> ImportPreview {
    let mut global_issues = Vec::new();
    let excluded: BTreeSet<&str> = choices.excluded.iter().map(String::as_str).collect();
    let user_choices = choices
        .users
        .iter()
        .map(|choice| (person_key(&choice.person, &choice.employee_id), choice))
        .collect::<HashMap<_, _>>();
    let customer_choices = choices
        .customers
        .iter()
        .map(|choice| (customer_key(choice_source_client(choice)), choice))
        .collect::<HashMap<_, _>>();
    let project_choices = choices
        .projects
        .iter()
        .map(|choice| {
            let (source_client, source_project, source_code) = choice_parts(choice);
            (
                project_key(source_client, source_project, source_code),
                choice,
            )
        })
        .collect::<HashMap<_, _>>();
    let task_choices = choices
        .tasks
        .iter()
        .map(|choice| {
            let source_task = match choice {
                TaskChoice::Existing { source_task, .. }
                | TaskChoice::Create { source_task, .. }
                | TaskChoice::Skip { source_task } => source_task,
            };
            (task_key(source_task), choice)
        })
        .collect::<HashMap<_, _>>();
    let category_choices = choices
        .categories
        .iter()
        .map(|choice| {
            let source = match choice {
                CategoryChoice::Existing {
                    source_category, ..
                }
                | CategoryChoice::Create {
                    source_category, ..
                } => source_category,
            };
            (category_key(source), choice)
        })
        .collect::<HashMap<_, _>>();
    let invoice_choices = choices
        .invoices
        .iter()
        .map(|choice| (choice.sha256.as_str(), choice))
        .collect::<HashMap<_, _>>();
    let links = choices
        .invoice_links
        .iter()
        .map(|link| (link.source_key.as_str(), link.invoice_sha256.as_str()))
        .collect::<HashMap<_, _>>();

    let mut taken_codes: BTreeMap<Uuid, BTreeSet<String>> = BTreeMap::new();
    for project in view.projects {
        taken_codes
            .entry(project.customer_id)
            .or_default()
            .insert(project.code.0.clone());
    }

    // Existing provenance, keyed by provider + namespace + source key. Batch id
    // is deliberately excluded: overlapping exports must collide.
    let imported_time: BTreeSet<(String, String)> = view
        .entries
        .iter()
        .filter_map(|entry| {
            entry.imported.as_ref().map(|imported| {
                (
                    imported.origin.namespace.clone(),
                    imported.origin.source_key.clone(),
                )
            })
        })
        .collect();
    let imported_expenses: BTreeSet<(String, String)> = view
        .expenses
        .iter()
        .filter_map(|expense| {
            expense
                .imported
                .as_ref()
                .map(|imported| (imported.namespace.clone(), imported.source_key.clone()))
        })
        .collect();
    let ordinary_entries: BTreeSet<(NaiveDate, Uuid, String, u32, String)> = view
        .entries
        .iter()
        .filter(|entry| entry.imported.is_none())
        .map(|entry| {
            (
                entry.date,
                entry.customer_id,
                entry.project_code.0.clone(),
                entry.hours.0,
                entry.note.clone(),
            )
        })
        .collect();
    let ordinary_expenses: BTreeSet<(NaiveDate, Uuid, u64, String, String)> = view
        .expenses
        .iter()
        .filter(|expense| expense.imported.is_none())
        .map(|expense| {
            (
                expense.date,
                expense.customer_id,
                expense.amount_minor,
                expense.currency.0.clone(),
                expense.note.clone(),
            )
        })
        .collect();

    let mut time_rows = Vec::new();
    if let Some(report) = time {
        let parsed = parser_issues(report, "time", Some("HARVEST_TIME_CSV"));
        global_issues.extend(
            parsed
                .iter()
                .filter(|issue| issue.record.is_none_or(|record| record == 0))
                .cloned(),
        );
        let mut row_issues: HashMap<usize, Vec<PlanIssue>> = HashMap::new();
        for parsed_issue in parsed {
            if let Some(record) = parsed_issue.record
                && record != 0
            {
                row_issues.entry(record).or_default().push(parsed_issue);
            }
        }
        let correction_records: BTreeSet<usize> = report
            .correction_pairs
            .iter()
            .flat_map(|pair| [pair.positive_record, pair.negative_record])
            .collect();
        let keys = report_keys(ReportKind::Time, report);
        for (row, source_key) in report.rows.iter().zip(keys) {
            let mut issues = row_issues.remove(&row.record).unwrap_or_default();
            if excluded.contains(source_key.as_str()) {
                time_rows.push(PlannedTime {
                    source_key,
                    record: row.record,
                    line: row.line,
                    decision: ImportDecision::Excluded,
                    source_client: row.client.clone(),
                    source_project: row.project.clone(),
                    source_code: row.project_code.clone(),
                    source_person: row.person.clone(),
                    source_employee_id: row.employee_id.clone(),
                    source_task: row.task.clone(),
                    date: row.date,
                    customer_id: None,
                    project_code: None,
                    task_code: None,
                    user_id: None,
                    hours_hundredths: None,
                    billable: row.billable.unwrap_or(false),
                    invoiced: row.invoiced.unwrap_or(false),
                    currency: row.currency.as_ref().map(|c| c.0.clone()),
                    rate_minor: row.billable_rate_minor,
                    cost_rate_minor: row.cost_rate_minor,
                    amount_minor: None,
                    cost_amount_minor: None,
                    note: row.note.clone(),
                    invoice_sha256: None,
                    issues,
                });
                continue;
            }
            if correction_records.contains(&row.record) {
                time_rows.push(PlannedTime {
                    source_key,
                    record: row.record,
                    line: row.line,
                    decision: ImportDecision::Correction,
                    source_client: row.client.clone(),
                    source_project: row.project.clone(),
                    source_code: row.project_code.clone(),
                    source_person: row.person.clone(),
                    source_employee_id: row.employee_id.clone(),
                    source_task: row.task.clone(),
                    date: row.date,
                    customer_id: None,
                    project_code: None,
                    task_code: None,
                    user_id: None,
                    hours_hundredths: None,
                    billable: row.billable.unwrap_or(false),
                    invoiced: row.invoiced.unwrap_or(false),
                    currency: row.currency.as_ref().map(|c| c.0.clone()),
                    rate_minor: row.billable_rate_minor,
                    cost_rate_minor: row.cost_rate_minor,
                    amount_minor: None,
                    cost_amount_minor: None,
                    note: row.note.clone(),
                    invoice_sha256: None,
                    issues,
                });
                continue;
            }
            let person = user_choices.get(&person_key(&row.person, &row.employee_id));
            let user_id = match person {
                Some(choice) if choice.skip => None,
                Some(choice) => choice.user_id,
                None => None,
            };
            if person.is_none() {
                issues.push(issue(
                    "time",
                    Some(row.record),
                    Some(row.line),
                    None,
                    "person",
                    "no user mapping was reviewed for this person",
                    true,
                ));
            } else if let Some(choice) = person
                && !choice.skip
            {
                match choice.user_id {
                    None => issues.push(issue(
                        "time",
                        Some(row.record),
                        Some(row.line),
                        None,
                        "person",
                        "the person mapping is unresolved",
                        true,
                    )),
                    Some(user_id) if !view.users.iter().any(|(known, _)| *known == user_id) => {
                        issues.push(issue(
                            "time",
                            Some(row.record),
                            Some(row.line),
                            None,
                            "person",
                            "the mapped user no longer exists",
                            true,
                        ));
                    }
                    Some(_) => {}
                }
            }
            let customer_id = reviewed_customer(
                customer_choices.get(&customer_key(&row.client)).copied(),
                view.customers,
                &mut issues,
                "time",
                Some(row.record),
            );
            let project_code = customer_id.and_then(|customer_id| {
                reviewed_project(
                    project_choices
                        .get(&project_key(&row.client, &row.project, &row.project_code))
                        .copied(),
                    view.projects,
                    customer_id,
                    &mut issues,
                    "time",
                    Some(row.record),
                    true,
                    &mut taken_codes,
                )
            });
            let task_code = reviewed_task(
                task_choices.get(&task_key(&row.task)).copied(),
                view.tasks,
                customer_id,
                project_code.as_deref(),
                &row.task,
                &mut issues,
                "time",
                Some(row.record),
            );
            let hours = row
                .hours_hundredths
                .filter(|hours| *hours > 0)
                .and_then(|h| u32::try_from(h).ok().filter(|h| *h <= Hours::MAX));
            if hours.is_none() {
                issues.push(issue(
                    "time",
                    Some(row.record),
                    Some(row.line),
                    None,
                    "hours",
                    "hours must be a positive hundredth value",
                    true,
                ));
            }
            if row.currency.is_none() {
                issues.push(issue(
                    "time",
                    Some(row.record),
                    Some(row.line),
                    None,
                    "currency",
                    "currency is required",
                    true,
                ));
            }
            let rate = row.billable_rate_minor.unwrap_or(0);
            let amount = row
                .billable_amount_minor
                .and_then(|value| u64::try_from(value).ok());
            let cost_amount = row
                .cost_amount_minor
                .and_then(|value| u64::try_from(value).ok());
            let key = (namespace.to_string(), source_key.clone());
            let duplicate = imported_time.contains(&key);
            if ordinary_entries.contains(&(
                row.date.unwrap_or_default(),
                customer_id.unwrap_or_default(),
                project_code.clone().unwrap_or_default(),
                hours.unwrap_or(0),
                row.note.clone(),
            )) {
                issues.push(issue(
                    "time",
                    Some(row.record),
                    Some(row.line),
                    None,
                    "record",
                    "a similar ordinary entry already exists; it will never be overwritten",
                    false,
                ));
            }
            let blocked = hours.is_none() || customer_id.is_none() || project_code.is_none();
            let decision = decision_for(
                if duplicate {
                    ImportDecision::Duplicate
                } else {
                    ImportDecision::New
                },
                &issues,
                blocked,
            );
            let linked_invoice = links.get(source_key.as_str()).map(|s| (*s).to_string());
            time_rows.push(PlannedTime {
                source_key,
                record: row.record,
                line: row.line,
                decision,
                source_client: row.client.clone(),
                source_project: row.project.clone(),
                source_code: row.project_code.clone(),
                source_person: row.person.clone(),
                source_employee_id: row.employee_id.clone(),
                source_task: row.task.clone(),
                date: row.date,
                customer_id,
                project_code,
                task_code,
                user_id,
                hours_hundredths: hours,
                billable: row.billable.unwrap_or(false),
                invoiced: row.invoiced.unwrap_or(false),
                currency: row.currency.as_ref().map(|c| c.0.clone()),
                rate_minor: Some(rate),
                cost_rate_minor: row.cost_rate_minor,
                amount_minor: amount.or_else(|| hours.map(|hours| Hours(hours).amount_minor(rate))),
                cost_amount_minor: cost_amount,
                note: row.note.clone(),
                invoice_sha256: linked_invoice,
                issues,
            });
        }
    }

    let mut expense_rows = Vec::new();
    if let Some(report) = expenses {
        let parsed = parser_issues(report, "expense", Some("HARVEST_EXPENSE_CSV"));
        global_issues.extend(
            parsed
                .iter()
                .filter(|issue| issue.record.is_none_or(|record| record == 0))
                .cloned(),
        );
        let mut row_issues: HashMap<usize, Vec<PlanIssue>> = HashMap::new();
        for parsed_issue in parsed {
            if let Some(record) = parsed_issue.record
                && record != 0
            {
                row_issues.entry(record).or_default().push(parsed_issue);
            }
        }
        let keys = report_keys(ReportKind::Expense, report);
        for (row, source_key) in report.rows.iter().zip(keys) {
            let mut issues = row_issues.remove(&row.record).unwrap_or_default();
            if excluded.contains(source_key.as_str()) {
                expense_rows.push(PlannedExpense {
                    source_key,
                    record: row.record,
                    line: row.line,
                    decision: ImportDecision::Excluded,
                    source_client: row.client.clone(),
                    source_project: row.project.clone(),
                    source_code: row.project_code.clone(),
                    source_person: row.person.clone(),
                    source_employee_id: row.employee_id.clone(),
                    source_category: row.category.clone(),
                    date: row.date,
                    customer_id: None,
                    project_code: None,
                    category_id: None,
                    user_id: None,
                    amount_minor: row.amount_minor,
                    currency: row.currency.as_ref().map(|c| c.0.clone()),
                    billable: row.billable.unwrap_or(false),
                    invoiced: row.invoiced.unwrap_or(false),
                    note: row.note.clone(),
                    issues,
                });
                continue;
            }
            let person = user_choices.get(&person_key(&row.person, &row.employee_id));
            let user_id = match person {
                Some(choice) if choice.skip => None,
                Some(choice) => choice.user_id,
                None => None,
            };
            if person.is_none() {
                issues.push(issue(
                    "expense",
                    Some(row.record),
                    Some(row.line),
                    None,
                    "person",
                    "no user mapping was reviewed for this person",
                    true,
                ));
            } else if let Some(choice) = person
                && !choice.skip
            {
                match choice.user_id {
                    None => issues.push(issue(
                        "expense",
                        Some(row.record),
                        Some(row.line),
                        None,
                        "person",
                        "the person mapping is unresolved",
                        true,
                    )),
                    Some(user_id) if !view.users.iter().any(|(known, _)| *known == user_id) => {
                        issues.push(issue(
                            "expense",
                            Some(row.record),
                            Some(row.line),
                            None,
                            "person",
                            "the mapped user no longer exists",
                            true,
                        ));
                    }
                    Some(_) => {}
                }
            }
            let customer_id = reviewed_customer(
                customer_choices.get(&customer_key(&row.client)).copied(),
                view.customers,
                &mut issues,
                "expense",
                Some(row.record),
            );
            let project_code = customer_id.and_then(|customer_id| {
                reviewed_project(
                    project_choices
                        .get(&project_key(&row.client, &row.project, &row.project_code))
                        .copied(),
                    view.projects,
                    customer_id,
                    &mut issues,
                    "expense",
                    Some(row.record),
                    false,
                    &mut taken_codes,
                )
            });
            let category_id = reviewed_category(
                category_choices.get(&category_key(&row.category)).copied(),
                view.categories,
                &mut issues,
                "expense",
                Some(row.record),
            );
            let currency = row
                .currency
                .clone()
                .unwrap_or_else(|| Currency(String::new()));
            if row.currency.is_none() {
                issues.push(issue(
                    "expense",
                    Some(row.record),
                    Some(row.line),
                    None,
                    "currency",
                    "currency is required",
                    true,
                ));
            }
            let amount = row.amount_minor;
            if amount.is_none() {
                issues.push(issue(
                    "expense",
                    Some(row.record),
                    Some(row.line),
                    None,
                    "amount",
                    "amount is required",
                    true,
                ));
            }
            let key = (namespace.to_string(), source_key.clone());
            let duplicate = imported_expenses.contains(&key);
            if ordinary_expenses.contains(&(
                row.date.unwrap_or_default(),
                customer_id.unwrap_or_default(),
                amount.unwrap_or(0),
                currency.0.clone(),
                row.note.clone(),
            )) {
                issues.push(issue(
                    "expense",
                    Some(row.record),
                    Some(row.line),
                    None,
                    "record",
                    "a similar ordinary expense already exists; it will never be overwritten",
                    false,
                ));
            }
            let blocked = amount.is_none() || customer_id.is_none() || category_id.is_none();
            let decision = decision_for(
                if duplicate {
                    ImportDecision::Duplicate
                } else {
                    ImportDecision::New
                },
                &issues,
                blocked,
            );
            expense_rows.push(PlannedExpense {
                source_key,
                record: row.record,
                line: row.line,
                decision,
                source_client: row.client.clone(),
                source_project: row.project.clone(),
                source_code: row.project_code.clone(),
                source_person: row.person.clone(),
                source_employee_id: row.employee_id.clone(),
                source_category: row.category.clone(),
                date: row.date,
                customer_id,
                project_code,
                category_id,
                user_id,
                amount_minor: amount,
                currency: row.currency.as_ref().map(|c| c.0.clone()),
                billable: row.billable.unwrap_or(false),
                invoiced: row.invoiced.unwrap_or(false),
                note: row.note.clone(),
                issues,
            });
        }
    }

    let existing_invoice_numbers: BTreeMap<&str, &Invoice> = view
        .invoices
        .iter()
        .map(|inv| (inv.number.as_str(), inv))
        .collect();
    let existing_imported: BTreeMap<&str, &Invoice> = view
        .invoices
        .iter()
        .filter_map(|invoice| {
            invoice
                .imported
                .as_ref()
                .map(|imported| (imported.origin.source_key.as_str(), invoice))
        })
        .collect();
    let existing_hashes: BTreeSet<&str> = view
        .invoices
        .iter()
        .filter_map(|invoice| invoice.imported.as_ref().map(|i| i.pdf_sha256.as_str()))
        .collect();

    let mut invoice_rows = Vec::new();
    for candidate in invoice_candidates {
        let source_key = format!("invoice:{}", candidate.sha256);
        let choice = invoice_choices.get(candidate.sha256.as_str()).copied();
        let mut issues = Vec::new();
        if choice.is_some_and(|choice| choice.excluded) || excluded.contains(source_key.as_str()) {
            invoice_rows.push(PlannedInvoice {
                source_key,
                original_name: candidate.original_name.clone(),
                pdf_sha256: candidate.sha256.clone(),
                decision: ImportDecision::Excluded,
                customer_id: None,
                original_number: choice.map_or_else(String::new, |c| c.original_number.clone()),
                currency: None,
                invoice_date: None,
                period_from: None,
                period_to: None,
                total_minor: None,
                opening_paid_minor: 0,
                status: None,
                suggestions: candidate.inspection.clone(),
                issues,
            });
            continue;
        }
        let Some(choice) = choice else {
            issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "review",
                "invoice metadata has not been reviewed",
                true,
            ));
            invoice_rows.push(PlannedInvoice {
                source_key,
                original_name: candidate.original_name.clone(),
                pdf_sha256: candidate.sha256.clone(),
                decision: ImportDecision::Unresolved,
                customer_id: None,
                original_number: candidate.original_name.clone(),
                currency: None,
                invoice_date: None,
                period_from: None,
                period_to: None,
                total_minor: None,
                opening_paid_minor: 0,
                status: None,
                suggestions: candidate.inspection.clone(),
                issues,
            });
            continue;
        };
        let customer_ok = choice
            .customer_id
            .is_some_and(|id| view.customers.iter().any(|c| c.id == id));
        if !customer_ok {
            issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "customer_id",
                "a reviewed, existing customer is required",
                true,
            ));
        }
        let number = choice.original_number.trim();
        if number.is_empty() || number.chars().count() > 100 {
            issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "original_number",
                "the original legal number is required (1-100 characters)",
                true,
            ));
        }
        if choice.currency.is_none() {
            issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "currency",
                "the invoice currency is required",
                true,
            ));
        }
        if choice.invoice_date.is_none() {
            issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "invoice_date",
                "the original invoice date is required",
                true,
            ));
        }
        let period_ok = matches!(
            (choice.period_from, choice.period_to),
            (Some(from), Some(to)) if from <= to
        );
        if !period_ok {
            issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "period",
                "a reviewed period_from/period_to is required; it is never inferred from the invoice date",
                true,
            ));
        }
        let total_ok = choice.total_minor.is_some();
        if !total_ok {
            issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "total_minor",
                "the original invoice total is required",
                true,
            ));
        }
        if let Some(total) = choice.total_minor
            && choice.opening_paid_minor > total
        {
            issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "opening_paid_minor",
                "the opening paid amount cannot exceed the invoice total",
                true,
            ));
        }
        match (choice.status, choice.total_minor, choice.opening_paid_minor) {
            (None, _, _) => issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "status",
                "a reviewed settlement status is required",
                true,
            )),
            (Some(InvoiceStatus::Draft), _, _) => issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "status",
                "an imported original invoice cannot be a draft",
                true,
            )),
            (Some(InvoiceStatus::Paid), Some(total), opening) if opening != total => {
                issues.push(issue(
                    "invoice",
                    None,
                    None,
                    Some(&candidate.original_name),
                    "opening_paid_minor",
                    "a paid invoice needs a reviewed opening paid amount equal to its total",
                    true,
                ));
            }
            (Some(InvoiceStatus::Issued | InvoiceStatus::WrittenOff), _, opening)
                if opening != 0 =>
            {
                issues.push(issue(
                    "invoice",
                    None,
                    None,
                    Some(&candidate.original_name),
                    "opening_paid_minor",
                    "an issued or written-off invoice cannot carry an opening paid amount",
                    true,
                ));
            }
            _ => {}
        }
        let duplicate = existing_imported.contains_key(source_key.as_str())
            || existing_hashes.contains(candidate.sha256.as_str());
        if let Some(existing) = existing_invoice_numbers.get(number)
            && existing
                .imported
                .as_ref()
                .is_none_or(|imported| imported.pdf_sha256 != candidate.sha256)
        {
            issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "original_number",
                "an invoice with this number already exists and is not this PDF",
                true,
            ));
        }
        if candidate.inspection.status != PdfInspectionStatus::Suggestions {
            issues.push(issue(
                "invoice",
                None,
                None,
                Some(&candidate.original_name),
                "pdf",
                "the original PDF needs manual review; its text was not sufficient for suggestions",
                false,
            ));
        }
        let decision = decision_for(
            if duplicate {
                ImportDecision::Duplicate
            } else {
                ImportDecision::New
            },
            &issues,
            false,
        );
        invoice_rows.push(PlannedInvoice {
            source_key,
            original_name: candidate.original_name.clone(),
            pdf_sha256: candidate.sha256.clone(),
            decision,
            customer_id: choice.customer_id,
            original_number: number.to_string(),
            currency: choice.currency.as_ref().map(|c| c.0.clone()),
            invoice_date: choice.invoice_date,
            period_from: choice.period_from,
            period_to: choice.period_to,
            total_minor: choice.total_minor,
            opening_paid_minor: choice.opening_paid_minor,
            status: choice.status,
            suggestions: candidate.inspection.clone(),
            issues,
        });
    }

    // A reviewed link must point at a real, included invoice in this batch and
    // an externally billed row. The row's currency and customer must agree with
    // the invoice and the linked rows may not exceed its total. Link checks are
    // reconciliation only: they never make a row billable again, so a linked
    // row stays in the skipped (external billed) side of the totals.
    let mut linked_totals: BTreeMap<&str, u64> = BTreeMap::new();
    let mut over_total_reported: BTreeSet<&str> = BTreeSet::new();
    for link in &choices.invoice_links {
        let Some(invoice) = invoice_rows
            .iter()
            .find(|row| row.pdf_sha256 == link.invoice_sha256)
        else {
            global_issues.push(issue(
                "invoice",
                None,
                None,
                None,
                "invoice_links",
                "a reviewed row link points at an invoice outside this batch",
                true,
            ));
            continue;
        };
        if invoice.decision == ImportDecision::Excluded {
            global_issues.push(issue(
                "invoice",
                None,
                None,
                Some(&invoice.original_name),
                "invoice_links",
                "a reviewed row link points at an excluded invoice",
                true,
            ));
        }
        let source = time_rows
            .iter()
            .find(|row| row.source_key == link.source_key)
            .map(|row| {
                (
                    row.record,
                    row.line,
                    row.invoiced,
                    row.currency.as_deref(),
                    row.customer_id,
                    row.amount_minor,
                )
            })
            .or_else(|| {
                expense_rows
                    .iter()
                    .find(|row| row.source_key == link.source_key)
                    .map(|row| {
                        (
                            row.record,
                            row.line,
                            row.invoiced,
                            row.currency.as_deref(),
                            row.customer_id,
                            row.amount_minor,
                        )
                    })
            });
        let Some((record, line, invoiced, currency, customer_id, amount)) = source else {
            global_issues.push(issue(
                "invoice",
                None,
                None,
                None,
                "invoice_links",
                "a reviewed row link points at a row outside this batch",
                true,
            ));
            continue;
        };
        if !invoiced {
            global_issues.push(issue(
                "invoice",
                Some(record),
                Some(line),
                None,
                "invoice_links",
                "only externally billed rows may be linked to an imported invoice",
                true,
            ));
            continue;
        }
        if let (Some(row_currency), Some(invoice_currency)) =
            (currency, invoice.currency.as_deref())
            && row_currency != invoice_currency
        {
            global_issues.push(issue(
                "invoice",
                Some(record),
                Some(line),
                None,
                "invoice_links",
                "the linked row currency differs from the invoice currency",
                true,
            ));
        }
        if let (Some(row_customer), Some(invoice_customer)) = (customer_id, invoice.customer_id)
            && row_customer != invoice_customer
        {
            global_issues.push(issue(
                "invoice",
                Some(record),
                Some(line),
                None,
                "invoice_links",
                "the linked row customer differs from the invoice customer",
                true,
            ));
        }
        if let (Some(amount), Some(total)) = (amount, invoice.total_minor) {
            let sum = linked_totals
                .entry(link.invoice_sha256.as_str())
                .or_default();
            *sum = sum.saturating_add(amount);
            if *sum > total && over_total_reported.insert(link.invoice_sha256.as_str()) {
                global_issues.push(issue(
                    "invoice",
                    None,
                    None,
                    Some(&invoice.original_name),
                    "invoice_links",
                    "the reviewed row links exceed the invoice total",
                    true,
                ));
            }
        }
    }

    let totals = reconcile(&time_rows, &expense_rows, &invoice_rows);
    let blocking = time_rows
        .iter()
        .flat_map(|row| &row.issues)
        .chain(expense_rows.iter().flat_map(|row| &row.issues))
        .chain(invoice_rows.iter().flat_map(|row| &row.issues))
        .chain(global_issues.iter())
        .filter(|issue| issue.blocking)
        .count();
    let accepted = time_rows
        .iter()
        .filter(|row| row.decision == ImportDecision::New)
        .count()
        + expense_rows
            .iter()
            .filter(|row| row.decision == ImportDecision::New)
            .count()
        + invoice_rows
            .iter()
            .filter(|row| row.decision == ImportDecision::New)
            .count();
    let ready = blocking == 0 && accepted > 0;

    let revision = digest(
        &serde_json::to_vec(&serde_json::json!({
            "namespace": namespace,
            "time": time,
            "expenses": expenses,
            "invoices": invoice_candidates.iter().map(|candidate| serde_json::json!({
                "sha256": candidate.sha256,
                "name": candidate.original_name,
            })).collect::<Vec<_>>(),
            "choices": choices,
        }))
        .unwrap_or_default(),
    );

    ImportPreview {
        revision,
        ready,
        blocking,
        time: time_rows,
        expenses: expense_rows,
        invoices: invoice_rows,
        totals,
        issues: global_issues,
    }
}

fn choice_parts(choice: &ProjectChoice) -> (&str, &str, &str) {
    match choice {
        ProjectChoice::Existing {
            source_client,
            source_project,
            source_code,
            ..
        }
        | ProjectChoice::Create {
            source_client,
            source_project,
            source_code,
            ..
        }
        | ProjectChoice::Skip {
            source_client,
            source_project,
            source_code,
        } => (source_client, source_project, source_code),
    }
}

fn reconcile(
    time: &[PlannedTime],
    expenses: &[PlannedExpense],
    invoices: &[PlannedInvoice],
) -> Vec<ReconciliationRow> {
    #[derive(Default)]
    struct Totals {
        accepted_count: u64,
        accepted_minor: u64,
        cost_minor: u64,
        skipped_count: u64,
        skipped_minor: u64,
        opening_paid_minor: u64,
    }
    let mut map: BTreeMap<(String, String), Totals> = BTreeMap::new();
    let mut add =
        |source: &str, currency: &str, decision: &ImportDecision, amount: u64, cost: u64| {
            let entry = map
                .entry((source.to_string(), currency.to_string()))
                .or_default();
            match decision {
                ImportDecision::New => {
                    entry.accepted_count += 1;
                    entry.accepted_minor = entry.accepted_minor.saturating_add(amount);
                    entry.cost_minor = entry.cost_minor.saturating_add(cost);
                }
                ImportDecision::Correction => {
                    entry.accepted_count += 1;
                    // Equal-and-opposite rows cancel; the raw rows stay in history.
                }
                _ => {
                    entry.skipped_count += 1;
                    entry.skipped_minor = entry.skipped_minor.saturating_add(amount);
                }
            }
        };
    for row in time {
        add(
            "time",
            row.currency.as_deref().unwrap_or(""),
            &row.decision,
            row.amount_minor.unwrap_or(0),
            row.cost_amount_minor.unwrap_or(0),
        );
    }
    for row in expenses {
        add(
            "expense",
            row.currency.as_deref().unwrap_or(""),
            &row.decision,
            row.amount_minor.unwrap_or(0),
            0,
        );
    }
    for row in invoices {
        let currency = row.currency.as_deref().unwrap_or("");
        let entry = map.entry(("invoice".into(), currency.into())).or_default();
        match row.decision {
            ImportDecision::New => {
                entry.accepted_count += 1;
                entry.accepted_minor = entry
                    .accepted_minor
                    .saturating_add(row.total_minor.unwrap_or(0));
                entry.opening_paid_minor = entry
                    .opening_paid_minor
                    .saturating_add(row.opening_paid_minor);
            }
            _ => {
                entry.skipped_count += 1;
                entry.skipped_minor = entry
                    .skipped_minor
                    .saturating_add(row.total_minor.unwrap_or(0));
            }
        }
    }
    map.into_iter()
        .map(|((source, currency), totals)| ReconciliationRow {
            source,
            currency,
            accepted_count: totals.accepted_count,
            accepted_minor: totals.accepted_minor,
            cost_minor: totals.cost_minor,
            skipped_count: totals.skipped_count,
            skipped_minor: totals.skipped_minor,
            opening_paid_minor: totals.opening_paid_minor,
        })
        .collect()
}

/// Ids the commit will use for reviewed new references and records. Deterministic
/// and stable across retries, so a journaled batch can be replayed safely.
#[must_use]
pub fn planned_time_id(namespace: &str, source_key: &str) -> Uuid {
    plan_id("entry", &format!("{namespace}:{source_key}"))
}

#[must_use]
pub fn planned_expense_id(namespace: &str, source_key: &str) -> Uuid {
    plan_id("expense", &format!("{namespace}:{source_key}"))
}

#[must_use]
pub fn planned_invoice_id(namespace: &str, source_key: &str) -> Uuid {
    plan_id("invoice", &format!("{namespace}:{source_key}"))
}

#[must_use]
pub fn planned_customer_id(source_client: &str) -> Uuid {
    plan_id("customer", &customer_key(source_client))
}

#[must_use]
pub fn planned_category_id(source_category: &str) -> Uuid {
    plan_id("category", &category_key(source_category))
}

#[must_use]
pub fn planned_entry_source_key(
    provider: ImportProvider,
    namespace: &str,
    source_key: &str,
) -> String {
    format!(
        "{}:{namespace}:{source_key}",
        match provider {
            ImportProvider::Harvest => "harvest",
        }
    )
}

/// Whether the chosen code is safe to create (validated through the same
/// `ProjectCode` rules as the rest of the domain).
#[must_use]
pub fn valid_code(code: &str) -> bool {
    ProjectCode::parse(code).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ImportOrigin, ImportedInvoice, ImportedTime};
    use crate::harvest::{ReportKind, parse};
    use crate::harvest_invoices::{PdfInspection, PdfInspectionStatus};
    use serde_json::json;

    const TIME_HEADER: &str = "Date,Client,Project,Project Code,Task,Notes,Hours,Billable?,Invoiced?,First Name,Last Name,Employee Id,Billable Rate,Billable Amount,Cost Rate,Cost Amount,Currency\n";
    const EXPENSE_HEADER: &str = "Date,Client,Project,Project Code,Category,Notes,Amount,Units,Billable,Invoiced?,Approved?,First name,Last name,Employee Id,Currency\n";

    struct Fixture {
        cid: Uuid,
        uid: Uuid,
        cat: Uuid,
        customers: Vec<Customer>,
        projects: Vec<Project>,
        tasks: Vec<Task>,
        categories: Vec<Category>,
        users: Vec<(Uuid, String)>,
    }

    fn fixture() -> Fixture {
        let cid = Uuid::new_v4();
        let uid = Uuid::new_v4();
        let cat = Uuid::new_v4();
        Fixture {
            cid,
            uid,
            cat,
            customers: vec![
                serde_json::from_value(json!({
                    "id": cid, "name": "Acme", "currency": "EUR",
                    "default_rate_minor": 6000, "active": true, "email": ""
                }))
                .unwrap(),
            ],
            projects: vec![Project {
                customer_id: cid,
                code: ProjectCode("WEB".into()),
                name: "Website".into(),
                currency: Currency("EUR".into()),
                rate_minor: 6500,
                active: true,
                budget_hours: None,
                budget_amount_minor: None,
            }],
            tasks: vec![
                serde_json::from_value(json!({
                    "customer_id": cid, "project_code": "WEB", "code": "DESIGN",
                    "name": "Design", "active": true
                }))
                .unwrap(),
            ],
            categories: vec![
                serde_json::from_value(json!({
                    "id": cat, "name": "Travel", "default_billable": true, "active": true
                }))
                .unwrap(),
            ],
            users: vec![(uid, "Ada Lovelace".into())],
        }
    }

    fn time_report(rows: &[&str]) -> Report {
        let body = rows.join("\n");
        parse(
            ReportKind::Time,
            format!("{TIME_HEADER}{body}\n").as_bytes(),
        )
        .unwrap()
    }

    fn expense_report(rows: &[&str]) -> Report {
        let body = rows.join("\n");
        parse(
            ReportKind::Expense,
            format!("{EXPENSE_HEADER}{body}\n").as_bytes(),
        )
        .unwrap()
    }

    fn choices(f: &Fixture) -> ImportChoices {
        ImportChoices {
            users: vec![PersonChoice {
                person: "Ada Lovelace".into(),
                employee_id: "E1".into(),
                user_id: Some(f.uid),
                skip: false,
            }],
            customers: vec![CustomerChoice::Existing {
                source_client: "Acme".into(),
                customer_id: f.cid,
            }],
            projects: vec![ProjectChoice::Existing {
                source_client: "Acme".into(),
                source_project: "Website".into(),
                source_code: "WEB".into(),
                customer_id: f.cid,
                code: ProjectCode("WEB".into()),
            }],
            tasks: vec![TaskChoice::Existing {
                source_task: "Design".into(),
                code: ProjectCode("DESIGN".into()),
            }],
            categories: vec![CategoryChoice::Existing {
                source_category: "Travel".into(),
                category_id: f.cat,
            }],
            invoices: vec![],
            excluded: vec![],
            invoice_links: vec![],
        }
    }

    fn invoice_choice(f: &Fixture, sha: &str) -> InvoiceChoice {
        InvoiceChoice {
            sha256: sha.into(),
            customer_id: Some(f.cid),
            original_number: "H-1".into(),
            currency: Some(Currency("EUR".into())),
            invoice_date: NaiveDate::from_ymd_opt(2026, 1, 5),
            period_from: NaiveDate::from_ymd_opt(2026, 1, 1),
            period_to: NaiveDate::from_ymd_opt(2026, 1, 31),
            total_minor: Some(10_000),
            opening_paid_minor: 0,
            status: Some(InvoiceStatus::Issued),
            excluded: false,
        }
    }

    fn candidate() -> InvoiceCandidate {
        InvoiceCandidate {
            original_name: "H-1.pdf".into(),
            sha256: "a".repeat(64),
            inspection: PdfInspection {
                pages: 1,
                text_bytes: 32,
                text_truncated: false,
                status: PdfInspectionStatus::Suggestions,
                reason: None,
                suggestions: vec![],
            },
        }
    }

    fn view<'a>(f: &'a Fixture, entries: &'a [Entry], invoices: &'a [Invoice]) -> StoreView<'a> {
        StoreView {
            customers: &f.customers,
            projects: &f.projects,
            tasks: &f.tasks,
            categories: &f.categories,
            users: &f.users,
            entries,
            expenses: &[],
            invoices,
        }
    }

    fn plan_with(
        f: &Fixture,
        time: Option<&Report>,
        expenses: Option<&Report>,
        invoices: &[InvoiceCandidate],
        choices: &ImportChoices,
        entries: &[Entry],
        stored_invoices: &[Invoice],
    ) -> ImportPreview {
        let view = view(f, entries, stored_invoices);
        plan("harvest-account", time, expenses, invoices, choices, &view)
    }

    #[test]
    fn combined_sources_produce_reviewable_preview_and_reconciliation() {
        let f = fixture();
        let time = time_report(&[
            "2026-01-05,Acme,Website,WEB,Design,Work,2.00,Yes,No,Ada,Lovelace,E1,65.00,130.00,50.00,100.00,EUR",
        ]);
        let expenses = expense_report(&[
            "2026-01-06,Acme,Website,WEB,Travel,Train,12.00,,Yes,No,Yes,Ada,Lovelace,E1,EUR",
        ]);
        let candidates = [candidate()];
        let choices = ImportChoices {
            invoices: vec![invoice_choice(&f, &"a".repeat(64))],
            ..choices(&f)
        };
        let preview = plan_with(
            &f,
            Some(&time),
            Some(&expenses),
            &candidates,
            &choices,
            &[],
            &[],
        );
        assert!(preview.ready, "{:?}", preview.issues);
        assert_eq!(preview.blocking, 0);
        assert_eq!(preview.time.len(), 1);
        assert_eq!(preview.time[0].decision, ImportDecision::New);
        assert_eq!(preview.time[0].customer_id, Some(f.cid));
        assert_eq!(preview.time[0].user_id, Some(f.uid));
        assert_eq!(preview.time[0].hours_hundredths, Some(200));
        assert_eq!(preview.time[0].amount_minor, Some(13_000));
        assert_eq!(preview.expenses[0].decision, ImportDecision::New);
        assert_eq!(preview.expenses[0].category_id, Some(f.cat));
        assert_eq!(preview.invoices[0].decision, ImportDecision::New);
        let time_totals = preview
            .totals
            .iter()
            .find(|row| row.source == "time" && row.currency == "EUR")
            .unwrap();
        assert_eq!(time_totals.accepted_minor, 13_000);
        assert_eq!(time_totals.cost_minor, 10_000);
        let invoice_totals = preview
            .totals
            .iter()
            .find(|row| row.source == "invoice")
            .unwrap();
        assert_eq!(invoice_totals.accepted_minor, 10_000);
    }

    #[test]
    fn identical_rows_keep_distinct_occurrences_and_overlaps_are_duplicates() {
        let f = fixture();
        let row = "2026-01-05,Acme,Website,WEB,Design,Work,2.00,Yes,No,Ada,Lovelace,E1,65.00,130.00,50.00,100.00,EUR";
        let time = time_report(&[row, row]);
        let choices = choices(&f);
        let first = plan_with(&f, Some(&time), None, &[], &choices, &[], &[]);
        assert_eq!(first.time.len(), 2);
        assert_ne!(first.time[0].source_key, first.time[1].source_key);
        assert_eq!(first.time[0].decision, ImportDecision::New);
        assert_eq!(first.time[1].decision, ImportDecision::New);

        let mut entry: Entry = serde_json::from_value(json!({
            "id": Uuid::new_v4(), "date": "2026-01-05",
            "customer_id": f.cid, "project_code": "WEB", "hours": 2,
            "note": "Work", "billable": true,
            "created_at": "2026-01-05T00:00:00Z",
            "updated_at": "2026-01-05T00:00:00Z"
        }))
        .unwrap();
        entry.imported = Some(ImportedTime {
            origin: ImportOrigin {
                provider: ImportProvider::Harvest,
                namespace: "harvest-account".into(),
                batch_id: Uuid::new_v4(),
                source_key: first.time[0].source_key.clone(),
                invoiced: false,
            },
            currency: Currency("EUR".into()),
            rate_minor: 6500,
            cost_rate_minor: 5000,
            source_hours: Hours(200),
            amount_minor: 13_000,
            cost_amount_minor: 10_000,
        });
        let second = plan_with(&f, Some(&time), None, &[], &choices, &[entry], &[]);
        assert_eq!(second.time[0].decision, ImportDecision::Duplicate);
        assert_eq!(second.time[1].decision, ImportDecision::New);
        assert!(second.ready);
    }

    #[test]
    fn correction_pairs_cancel_without_becoming_new_records() {
        let f = fixture();
        let time = time_report(&[
            "2026-01-05,Acme,Website,WEB,Design,Work,2.00,Yes,No,Ada,Lovelace,E1,65.00,130.00,50.00,100.00,EUR",
            "2026-01-05,Acme,Website,WEB,Design,Work,-2.00,Yes,No,Ada,Lovelace,E1,65.00,-130.00,50.00,-100.00,EUR",
        ]);
        let preview = plan_with(&f, Some(&time), None, &[], &choices(&f), &[], &[]);
        assert_eq!(
            preview
                .time
                .iter()
                .filter(|row| row.decision == ImportDecision::Correction)
                .count(),
            2
        );
        assert!(
            !preview.ready,
            "a batch of only corrections has nothing to commit"
        );
    }

    #[test]
    fn unresolved_and_conflicting_invoices_block_until_reviewed() {
        let f = fixture();
        let candidates = [candidate()];
        let preview = plan_with(&f, None, None, &candidates, &choices(&f), &[], &[]);
        assert_eq!(preview.invoices[0].decision, ImportDecision::Unresolved);
        assert!(!preview.ready);

        let mut existing: Invoice = serde_json::from_value(json!({
            "id": Uuid::new_v4(), "number": "H-1", "customer_id": f.cid,
            "currency": "EUR", "period_from": "2026-01-01", "period_to": "2026-01-31",
            "lines": [], "total_minor": 10_000, "status": "issued",
            "created_at": "2026-01-05T00:00:00Z"
        }))
        .unwrap();
        existing.imported = Some(ImportedInvoice {
            origin: ImportOrigin {
                provider: ImportProvider::Harvest,
                namespace: "harvest-account".into(),
                batch_id: Uuid::new_v4(),
                source_key: "invoice:other".into(),
                invoiced: true,
            },
            original_number: "H-1".into(),
            pdf_sha256: "b".repeat(64),
            opening_paid_minor: 0,
        });
        let choices = ImportChoices {
            invoices: vec![invoice_choice(&f, &"a".repeat(64))],
            ..choices(&f)
        };
        let preview = plan_with(&f, None, None, &candidates, &choices, &[], &[existing]);
        assert_eq!(preview.invoices[0].decision, ImportDecision::Unresolved);
        assert!(
            preview.invoices[0]
                .issues
                .iter()
                .any(|issue| issue.field == "original_number" && issue.blocking)
        );
    }

    #[test]
    fn revision_tracks_reviewed_mapping_changes() {
        let f = fixture();
        let time = time_report(&[
            "2026-01-05,Acme,Website,WEB,Design,Work,2.00,Yes,No,Ada,Lovelace,E1,65.00,130.00,50.00,100.00,EUR",
        ]);
        let first = plan_with(&f, Some(&time), None, &[], &choices(&f), &[], &[]);
        let mut changed = choices(&f);
        changed.excluded.push(first.time[0].source_key.clone());
        let second = plan_with(&f, Some(&time), None, &[], &changed, &[], &[]);
        assert_ne!(first.revision, second.revision);
        assert_eq!(second.time[0].decision, ImportDecision::Excluded);
    }

    #[test]
    fn mapped_user_and_task_must_exist_under_the_project() {
        let f = fixture();
        let time = time_report(&[
            "2026-01-05,Acme,Website,WEB,Design,Work,2.00,Yes,No,Ada,Lovelace,E1,65.00,130.00,50.00,100.00,EUR",
        ]);
        let mut invalid_user = choices(&f);
        invalid_user.users[0].user_id = Some(Uuid::new_v4());
        let preview = plan_with(&f, Some(&time), None, &[], &invalid_user, &[], &[]);
        assert!(!preview.ready);
        assert!(
            preview.time[0]
                .issues
                .iter()
                .any(|issue| issue.field == "person" && issue.blocking)
        );

        let mut wrong_task = choices(&f);
        wrong_task.tasks = vec![TaskChoice::Existing {
            source_task: "Design".into(),
            code: ProjectCode("OTHER".into()),
        }];
        let preview = plan_with(&f, Some(&time), None, &[], &wrong_task, &[], &[]);
        assert!(!preview.ready);
        assert!(
            preview.time[0]
                .issues
                .iter()
                .any(|issue| issue.field == "task" && issue.blocking)
        );
    }

    fn invoiced_time_row(currency: &str) -> Report {
        time_report(&[&format!(
            "2026-01-05,Acme,Website,WEB,Design,Work,2.00,Yes,Yes,Ada,Lovelace,E1,65.00,130.00,50.00,100.00,{currency}"
        )])
    }

    fn link_choices(f: &Fixture, sha: &str, source_key: &str) -> ImportChoices {
        let mut choices = ImportChoices {
            invoices: vec![invoice_choice(f, sha)],
            ..choices(f)
        };
        choices.invoice_links = vec![RowInvoiceLink {
            source_key: source_key.into(),
            invoice_sha256: sha.into(),
        }];
        choices
    }

    #[test]
    fn gui_create_choices_inherit_the_planned_customer_and_derive_safe_codes() {
        let f = fixture();
        let time = time_report(&[
            "2026-01-05,Acme,Website,WEB,Design,Work,2.00,Yes,No,Ada,Lovelace,E1,65.00,130.00,50.00,100.00,EUR",
        ]);
        // Exactly the shape the Settings workspace sends: the customer is
        // created in the same pass, so the project create carries a null
        // customer_id, and the task code is the raw source task name.
        let choices: ImportChoices = serde_json::from_value(json!({
            "users": [{"person":"Ada Lovelace","employee_id":"E1","user_id":f.uid,"skip":false}],
            "customers": [{
                "action":"create","source_client":"Acme","name":"Acme",
                "currency":"EUR","default_rate_minor":0
            }],
            "projects": [{
                "action":"create","source_client":"Acme","source_project":"Website",
                "source_code":"WEB","customer_id":null,"code":"WEB",
                "name":"Website","currency":"EUR","rate_minor":0
            }],
            "tasks": [{"action":"create","source_task":"Design","code":"Design","name":"Design"}],
            "categories": [], "invoices": [], "excluded": [], "invoice_links": []
        }))
        .unwrap();
        let preview = plan_with(&f, Some(&time), None, &[], &choices, &[], &[]);
        assert!(preview.ready, "{:?}", preview.issues);
        assert_eq!(
            preview.time[0].customer_id,
            Some(planned_customer_id("Acme")),
            "the project inherits the planned customer"
        );
        assert_eq!(preview.time[0].project_code.as_deref(), Some("WEB"));
        assert_eq!(
            preview.time[0].task_code.as_deref(),
            Some("DESIGN"),
            "the raw task name becomes a safe code"
        );
    }

    #[test]
    fn explicit_links_reconcile_the_invoice_total_without_rebilling_the_row() {
        let f = fixture();
        let time = invoiced_time_row("EUR");
        let baseline = plan_with(&f, Some(&time), None, &[], &choices(&f), &[], &[]);
        let source_key = baseline.time[0].source_key.clone();
        let sha = "a".repeat(64);
        let mut mapping = link_choices(&f, &sha, &source_key);

        // The reviewed invoice total (100.00) is below the linked row (130.00):
        // the link is a reconciliation conflict, not an acceptance.
        let preview = plan_with(&f, Some(&time), None, &[candidate()], &mapping, &[], &[]);
        assert!(!preview.ready);
        assert!(
            preview.issues.iter().any(|issue| {
                issue.field == "invoice_links"
                    && issue.blocking
                    && issue.message.contains("exceed the invoice total")
            }),
            "{:?}",
            preview.issues
        );
        let totals = preview
            .totals
            .iter()
            .find(|row| row.source == "time" && row.currency == "EUR")
            .unwrap();
        // The external row is imported exactly once; the association is not a
        // second acceptance and the row keeps its permanent external billing
        // flag, so invoice generation can never bill it again.
        assert_eq!(totals.accepted_count, 1);
        assert_eq!(totals.accepted_minor, 13_000);
        assert!(preview.time[0].invoiced);
        assert_eq!(
            preview.time[0].invoice_sha256.as_deref(),
            Some(sha.as_str()),
            "the reviewed association stays on the preview row"
        );

        // A total that covers the linked row reconciles and unblocks the commit.
        mapping.invoices[0].total_minor = Some(20_000);
        let preview = plan_with(&f, Some(&time), None, &[candidate()], &mapping, &[], &[]);
        assert!(preview.ready, "{:?}", preview.issues);
        let totals = preview
            .totals
            .iter()
            .find(|row| row.source == "time" && row.currency == "EUR")
            .unwrap();
        assert_eq!(totals.accepted_count, 1);
        assert_eq!(totals.accepted_minor, 13_000);
        assert!(preview.time[0].invoiced);
    }

    #[test]
    fn explicit_links_reject_currency_and_customer_mismatches() {
        let sha = "a".repeat(64);

        let f = fixture();
        let dollars = invoiced_time_row("USD");
        let baseline = plan_with(&f, Some(&dollars), None, &[], &choices(&f), &[], &[]);
        let mut mapping = link_choices(&f, &sha, &baseline.time[0].source_key);
        mapping.invoices[0].total_minor = Some(20_000);
        let preview = plan_with(&f, Some(&dollars), None, &[candidate()], &mapping, &[], &[]);
        assert!(!preview.ready);
        assert!(
            preview.issues.iter().any(|issue| {
                issue.field == "invoice_links" && issue.message.contains("currency differs")
            }),
            "{:?}",
            preview.issues
        );

        let mut f = fixture();
        let other = Uuid::new_v4();
        f.customers.push(
            serde_json::from_value(json!({
                "id": other, "name": "Other", "currency": "EUR",
                "default_rate_minor": 6000, "active": true, "email": ""
            }))
            .unwrap(),
        );
        let euros = invoiced_time_row("EUR");
        let baseline = plan_with(&f, Some(&euros), None, &[], &choices(&f), &[], &[]);
        let mut mapping = link_choices(&f, &sha, &baseline.time[0].source_key);
        mapping.invoices[0].customer_id = Some(other);
        mapping.invoices[0].total_minor = Some(20_000);
        let preview = plan_with(&f, Some(&euros), None, &[candidate()], &mapping, &[], &[]);
        assert!(!preview.ready);
        assert!(
            preview.issues.iter().any(|issue| {
                issue.field == "invoice_links" && issue.message.contains("customer differs")
            }),
            "{:?}",
            preview.issues
        );
    }

    #[test]
    fn code_suggestions_are_deterministic_and_collision_safe() {
        let mut taken = BTreeSet::from(["WEBSITE".to_string()]);
        assert_eq!(suggest_code("Website", "WEB", &taken), "WEBSITE_2");
        taken.insert("WEBSITE_2".into());
        assert_eq!(suggest_code("Website", "WEB", &taken), "WEBSITE_3");
        assert_eq!(suggest_code("  ", "WEB", &BTreeSet::new()), "WEB");
        assert_eq!(
            suggest_code("1st client!", "WEB", &BTreeSet::new()),
            "1ST_CLIENT"
        );
    }
}
