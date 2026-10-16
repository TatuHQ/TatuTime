// File-based persistence. No database: every entity is one pretty-printed
// JSON document on disk, mirroring the conceptual organisation of the data
// (a customer is a folder that contains its projects; entries live in a
// per-day folder). The API is the only actor that writes below the data dir.
//
// Path components are never taken raw from a request: customer files are
// named by `Uuid`, project files by a validated `ProjectCode`, entry
// directories by a validated `YYYY-MM-DD` date and entry files by `Uuid`.
// Traversal input therefore cannot reach the filesystem.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};
use fs2::FileExt;
use uuid::Uuid;

use crate::auth::User;
use crate::domain::{
    Category, Customer, Entry, Expense, ExpenseClaim, Invoice, InvoiceStatus, InvoiceTimelineEvent,
    Notification, PdfHint, Project, RecurringSchedule, Submission, Task, Timer,
};

mod index;
mod io;

use index::{merge_cold_docs, sort_entries_by_created};
use io::{
    dir_entries, dir_mtime, json_stems, prune_tmp_files, read_json, write_bytes_atomic, write_json,
};

/// Hard cap on a range scan so a malformed or adversarial query cannot spin
/// over the whole tree. 400 days covers a year plus buffer.
pub const MAX_RANGE_DAYS: i64 = 400;

/// Hard bound on a PDF archive read (#50, #113): invoices render far below
/// this; anything larger is corruption or a hostile data dir, and the read is
/// refused rather than streamed into memory.
pub const MAX_PDF_BYTES: u64 = 8 * 1024 * 1024;

/// Hard cap on documents in any single collection directory (#50). A personal
/// timesheet is far below this; it bounds a pathological/corrupted tree so no
/// request does an unbounded directory walk.
pub const MAX_DOCS: usize = 100_000;

/// Cap on retained per-invoice timeline events. The sidecar is a display aid,
/// not a record of authority, so once this many entries exist the oldest are
/// dropped and only the newest [`MAX_TIMELINE_EVENTS`] are kept.
pub const MAX_TIMELINE_EVENTS: usize = 200;

/// Default cross-process writer-lock acquisition deadline (#62).
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_millis(5000);
/// Poll interval between `try_lock_exclusive` attempts.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("state conflict: {0}")]
    Conflict(String),
    #[error("range too large (max {MAX_RANGE_DAYS} days)")]
    RangeTooLarge,
    #[error("write lock busy")]
    LockTimeout,
    #[error("collection too large (max {MAX_DOCS})")]
    TooManyItems,
    #[error("io failure: {0}")]
    Io(String),
}

/// Outcome of a generic store transaction (#187): either a store failure or
/// the caller's own typed domain error, so handlers map each arm correctly
/// instead of re-parsing message strings.
#[derive(Debug)]
pub enum UpdateError<E> {
    Store(StoreError),
    Domain(E),
}

impl<E> From<StoreError> for UpdateError<E> {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

// `Io` carries the operation context only; the underlying error is logged
// server-side, never serialised to a client (see security rules).
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// The frozen-reference snapshot the #18 lock seam answers from (#255).
///
/// One pass over the lock-source collections yields every locked entry id
/// (issued invoices — any non-draft status — #8/#114 and submitted/approved
/// submissions #16) and every
/// locked expense id (issued invoices and submitted/approved reimbursement
/// claims #24). The maps carry the human handle used in the 409 message
/// (invoice number / submission week). The snapshot is memoized on a cheap
/// directory-mtime stamp — see [`Store::lock_snapshot`].
#[derive(Debug, Default, Clone)]
pub struct LockSnapshot {
    /// Entry id -> invoice number (frozen by an *open* invoice).
    pub invoiced_entries: std::collections::HashMap<Uuid, String>,
    /// Expense id -> invoice number (frozen by an *open* invoice).
    pub invoiced_expenses: std::collections::HashMap<Uuid, String>,
    /// Entry id -> week start `YYYY-MM-DD` (frozen by a submitted/approved week).
    pub submitted_entries: std::collections::HashMap<Uuid, String>,
    /// Expense id -> claim id (frozen by a submitted/approved claim).
    pub claimed_expenses: std::collections::HashMap<Uuid, String>,
}

/// Cheap freshness token for the memoized lock snapshot (#255): the
/// modification time of each lock-source collection directory. Every writer
/// uses atomic tmp+rename (an architecture invariant), so any add, remove or
/// replacement bumps its directory's mtime — a changed stamp forces a rebuild.
/// A missing directory reads as `None`; its later creation changes the stamp.
#[derive(Clone, PartialEq, Eq)]
struct LockStamp {
    invoices: Option<std::time::SystemTime>,
    submissions: Option<std::time::SystemTime>,
    claims: Option<std::time::SystemTime>,
}

pub struct Store {
    root: PathBuf,
    /// Intra-process serialiser: one thread mutates at a time.
    write_guard: Mutex<()>,
    /// Cross-process writer-lock deadline (#62).
    lock_timeout: Duration,
    /// Per-collection document cap (#50); env-overridable for tests.
    max_docs: usize,
    /// Memoized #255 lock snapshot paired with the collection stamps it was
    /// built from. Always the innermost lock (acquired briefly, never while
    /// another store mutex is held), so it cannot deadlock the write guard.
    lock_cache: Mutex<Option<(LockStamp, Arc<LockSnapshot>)>>,
    /// #335 measurement seam: counts the entity documents decoded through
    /// [`Store::read_entry`]/[`Store::read_expense`], so a test can *prove* the
    /// per-user index reads only the member's rows instead of asserting it in
    /// prose. Compiled only for tests — production carries no counter.
    #[cfg(test)]
    doc_reads: std::sync::atomic::AtomicU64,
}

/// Held for the duration of a mutating operation: the process-local mutex plus
/// the advisory file lock. Dropping it releases both (the flock on fd close).
struct WriteGuard<'a> {
    _mutex: MutexGuard<'a, ()>,
    _lock: File,
}

/// One flat JSON document collection under the data root: every entity of
/// type `T` lives at `<root>/<DIR>/<id>.json` (#101). The twelve
/// `dir_entries -> read_json -> sort` / `write_lock -> create_dir_all ->
/// write_json` / `exists -> NotFound -> remove` blocks this file carried now
/// live once, in the primitives below, so the lock + atomicity discipline
/// (and the #97 transaction seam) has a single home.
pub trait Entity: serde::Serialize + serde::de::DeserializeOwned + Sized {
    /// Collection directory below the data root, e.g. `"invoices"`.
    const DIR: &'static str;
    /// Document stem: the file is `<id>.json`. Usually the entity `Uuid`;
    /// `Timer` keys by its owner instead.
    fn id(&self) -> String;
}

impl Entity for User {
    const DIR: &'static str = "users";
    fn id(&self) -> String {
        self.id.to_string()
    }
}

impl Entity for Invoice {
    const DIR: &'static str = "invoices";
    fn id(&self) -> String {
        self.id.to_string()
    }
}

impl Entity for crate::domain::ItemType {
    const DIR: &'static str = "items";
    fn id(&self) -> String {
        self.id.to_string()
    }
}

impl Entity for crate::retainer::Retainer {
    const DIR: &'static str = "retainers";
    fn id(&self) -> String {
        self.id.to_string()
    }
}

impl Entity for Category {
    const DIR: &'static str = "categories";
    fn id(&self) -> String {
        self.id.to_string()
    }
}

impl Entity for Expense {
    const DIR: &'static str = "expenses";
    fn id(&self) -> String {
        self.id.to_string()
    }
}

impl Entity for Submission {
    const DIR: &'static str = "submissions";
    fn id(&self) -> String {
        self.id.to_string()
    }
}

impl Entity for ExpenseClaim {
    const DIR: &'static str = "claims";
    fn id(&self) -> String {
        self.id.to_string()
    }
}

impl Entity for RecurringSchedule {
    const DIR: &'static str = "schedules";
    fn id(&self) -> String {
        self.id.to_string()
    }
}

impl Entity for Customer {
    const DIR: &'static str = "customers";
    fn id(&self) -> String {
        self.id.to_string()
    }
}

impl Entity for Timer {
    const DIR: &'static str = "timers";
    fn id(&self) -> String {
        self.user_id.to_string()
    }
}

/// Date-sharded entry collection: `entries/<YYYY-MM-DD>/<id>.json`, one folder
/// per day rather than one flat `Entity` directory (#38).
pub const ENTRIES_DIR: &str = "entries";

/// Per-user notifications, `notifications/<user_id>.json` — keyed by owner like
/// `timers/`, not by document id.
pub const NOTIFICATIONS_DIR: &str = "notifications";

/// Cold-tree root below the data root (#349): [`Store::archive_entries_before`]
/// relocates whole day folders to `archive/entries/<YYYY-MM>/<YYYY-MM-DD>/`, the
/// same `<id>.json` bytes under the same tmp+rename discipline. Every reader
/// merges it back in (#350) — `list_by_date`, `list_range`, `get_entry` and the
/// lifetime `scan_all_entries` all answer from hot ∪ cold — so a relocation
/// changes no number a caller or a user can see (ADR-003).
pub const ARCHIVE_DIR: &str = "archive";

/// The store's collection directories in one place: one row per [`Entity`]
/// impl, plus the collections that are not flat `Entity` dirs — [`ENTRIES_DIR`],
/// date-sharded; [`NOTIFICATIONS_DIR`], per-user; and the [`ARCHIVE_DIR`] cold
/// tree the #349 retention primitive relocates whole day folders into (the
/// operator-run pass writes it, every reader merges it back in, #350). Every row is the
/// impl's own `DIR` constant or one of those named constants — never a re-typed
/// literal — so this is the layout as the store derives it; the store's path
/// builders below use the same constants for the non-`Entity` collections.
///
/// The storage-layout guard
/// (`storage_layout::collections_match_agents_md`, tests/contract.rs) holds this
/// list and `AGENTS.md`'s "Architecture invariants" storage bullet together, in
/// both directions (#336): a collection added here with no sentence there, or a
/// documented directory with no row here, fails the build named for the drift.
/// Adding a collection therefore means editing this list and the docs in the
/// same change.
///
/// Deliberately *not* here: the flat side-car files (`audit.log`,
/// `revoked.json`, `scheduler.json`, `secrets.bin`, `config.json`,
/// `session.key`, `.server.lock`, `sync/accounting.json`, `invoices/.seq.json`,
/// the `*.idx.*` indexes). This list is about collection directories; those
/// files belong to the module that owns them.
pub const DOC_COLLECTIONS: &[&str] = &[
    User::DIR,
    Customer::DIR,
    Invoice::DIR,
    Category::DIR,
    Expense::DIR,
    Submission::DIR,
    ExpenseClaim::DIR,
    RecurringSchedule::DIR,
    crate::retainer::Retainer::DIR,
    crate::domain::ItemType::DIR,
    Timer::DIR,
    ENTRIES_DIR,
    NOTIFICATIONS_DIR,
    // #349/ADR-003 precondition 3: the cold tree
    // `archive/entries/<YYYY-MM>/<YYYY-MM-DD>/` is part of the documented
    // layout, so the `AGENTS.md` bullet and this list move together (#336).
    ARCHIVE_DIR,
];

impl Store {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::with_lock_timeout(root, DEFAULT_LOCK_TIMEOUT)
    }

    pub fn with_lock_timeout(
        root: impl AsRef<Path>,
        lock_timeout: Duration,
    ) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        // Best-effort hygiene (review D6): a crash between tmp-write and
        // rename can strand `*.tmp` files; they are not documents.
        for dir in [
            "customers",
            "users",
            "invoices",
            "categories",
            "expenses",
            "submissions",
            "claims",
        ] {
            prune_tmp_files(&root.join(dir));
        }
        prune_tmp_files(&root.join(ENTRIES_DIR));
        if let Ok(days) = std::fs::read_dir(root.join(ENTRIES_DIR)) {
            for day in days.filter_map(|e| e.ok()) {
                if day.path().is_dir() {
                    prune_tmp_files(&day.path());
                }
            }
        }
        std::fs::create_dir_all(root.join("customers"))?;
        std::fs::create_dir_all(root.join(ENTRIES_DIR))?;
        std::fs::create_dir_all(root.join("users"))?;
        std::fs::create_dir_all(root.join("categories"))?;
        std::fs::create_dir_all(root.join("expenses"))?;
        let max_docs = std::env::var("TUCANO_MAX_DOCS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(MAX_DOCS);
        let store = Self {
            root,
            write_guard: Mutex::new(()),
            lock_timeout,
            max_docs,
            lock_cache: Mutex::new(None),
            #[cfg(test)]
            doc_reads: std::sync::atomic::AtomicU64::new(0),
        };
        // #335 migration: a data dir written before the per-user index existed
        // has none, so build it once here — otherwise every member read would
        // rebuild each touched day (or expenses) lazily. Best-effort: the index
        // is only an optimisation, so a failure is harmless and reads still
        // self-heal, while a boot left unable to serve would not be.
        if !store.entry_index_path().exists()
            && let Err(e) = store.rebuild_read_indexes()
        {
            tracing::warn!(error = %e, "per-user index migration skipped; reads will rebuild lazily");
        }
        Ok(store)
    }

    /// Acquire the writer lock for a mutating operation: the process-local
    /// mutex (cheap, serialises threads) then the cross-process advisory flock
    /// on `.tucanotime.lock`, retried until the deadline. Returns
    /// `LockTimeout` (→ 503) if another process holds it too long.
    ///
    /// The retry loop parks the calling thread (`std::thread::sleep`), so this
    /// must never be driven from a tokio worker: async callers reach every
    /// mutating method through `api::store_blocking`, which runs the whole
    /// operation — and thus any wait here — on the blocking pool (#254).
    fn write_lock(&self) -> Result<WriteGuard<'_>, StoreError> {
        let mutex = self
            .write_guard
            .lock()
            .map_err(|_| StoreError::Io("store lock poisoned".into()))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(".tucanotime.lock"))?;
        let deadline = Instant::now() + self.lock_timeout;
        loop {
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                }
                Err(_) => return Err(StoreError::LockTimeout),
            }
        }
        Ok(WriteGuard {
            _mutex: mutex,
            _lock: lock,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Override the per-collection document cap (used by tests; production uses
    /// the default or `TUCANO_MAX_DOCS`).
    pub fn with_max_docs(mut self, max: usize) -> Self {
        self.max_docs = max;
        self
    }

    // ---------------------------------------------------------------- users --

    pub fn list_users(&self) -> Result<Vec<User>, StoreError> {
        let mut out = self.list_docs::<User>()?;
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Whether any user document exists, without decoding the collection: a
    /// `read_dir` + the same "is a document" filter as `dir_entries` (dot
    /// files such as the email side index and `*.tmp` leftovers are not
    /// documents). The public `GET /api/auth/status` probe no longer
    /// deserialises every user to answer "is the dir empty" (#256).
    pub fn has_users(&self) -> Result<bool, StoreError> {
        let dir = self.doc_dir::<User>();
        if !dir.exists() {
            return Ok(false);
        }
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !name.starts_with('.')
                && !name.ends_with(".tmp")
                && path.extension().and_then(|e| e.to_str()) == Some("json")
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn get_user(&self, id: Uuid) -> Result<Option<User>, StoreError> {
        self.get_doc::<User>(&id.to_string())
    }

    pub fn get_user_by_email(&self, email: &str) -> Result<Option<User>, StoreError> {
        // Fast path: the email side index, verified against the document.
        if let Some(id) =
            read_json::<HashMap<String, String>>(&self.email_index_path())?.and_then(|idx| {
                idx.get(&email.to_lowercase())
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            && let Some(user) = self.get_user(id)?
            && user.email.eq_ignore_ascii_case(email)
        {
            return Ok(Some(user));
        }
        // The scan stays authoritative (pre-index dirs, restored docs, drift).
        let found = self
            .list_users()?
            .into_iter()
            .find(|u| u.email.eq_ignore_ascii_case(email));
        // Reaching a hit through the scan means the fast path missed while a
        // matching document exists: the index is absent or stale — heal it.
        if found.is_some() {
            self.rebuild_email_index()?;
        }
        Ok(found)
    }

    pub fn put_user(&self, user: &User) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        self.put_user_locked(user)
    }

    pub fn put_user_if_unchanged(&self, user: &User, expected: &User) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let current = self.get_user(user.id)?.ok_or(StoreError::NotFound)?;
        // #256: `User` is `PartialEq`; compare the documents directly rather
        // than round-tripping both through serde (which serialised two argon2
        // hash strings) purely as an equality check.
        if current != *expected {
            return Err(StoreError::Conflict(
                "user changed; reload and try again".into(),
            ));
        }
        self.put_user_locked(user)
    }

    fn put_user_locked(&self, user: &User) -> Result<(), StoreError> {
        let users = self.list_users()?;
        if users
            .iter()
            .any(|other| other.id != user.id && other.email.eq_ignore_ascii_case(&user.email))
        {
            return Err(StoreError::Conflict("a user with that email exists".into()));
        }
        if users.iter().any(|other| other.id == user.id)
            && !(user.active && user.role == crate::auth::Role::Admin)
            && !users.iter().any(|other| {
                other.id != user.id && other.active && other.role == crate::auth::Role::Admin
            })
        {
            return Err(StoreError::Conflict(
                "at least one active administrator is required".into(),
            ));
        }
        std::fs::create_dir_all(self.doc_dir::<User>())?;
        write_json(&self.doc_path::<User>(&user.id.to_string()), user)?;
        self.index_user_locked(user)
    }

    pub fn delete_user(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let target = self.get_user(id)?.ok_or(StoreError::NotFound)?;
        if target.active
            && target.role == crate::auth::Role::Admin
            && !self
                .list_users()?
                .iter()
                .any(|user| user.id != id && user.active && user.role == crate::auth::Role::Admin)
        {
            return Err(StoreError::Conflict(
                "at least one active administrator is required".into(),
            ));
        }
        if self
            .list_all_entries()?
            .iter()
            .any(|entry| entry.user_id == Some(id))
            || self
                .list_expenses()?
                .iter()
                .any(|expense| expense.user_id == Some(id))
            || self
                .list_submissions()?
                .iter()
                .any(|submission| submission.user_id == id)
            || self.list_claims()?.iter().any(|claim| claim.user_id == id)
            || self.get_timer(id)?.is_some()
        {
            return Err(StoreError::Conflict(
                "user has recorded history or a running timer; deactivate instead".into(),
            ));
        }
        self.remove_doc_locked::<User>(&id.to_string())?;
        self.unindex_user_locked(&id.to_string())
    }

    // -------------------------------------------------------- item types --

    pub fn list_item_types(&self) -> Result<Vec<crate::domain::ItemType>, StoreError> {
        let mut out = self.list_docs::<crate::domain::ItemType>()?;
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get_item_type(&self, id: Uuid) -> Result<Option<crate::domain::ItemType>, StoreError> {
        self.get_doc::<crate::domain::ItemType>(&id.to_string())
    }

    pub fn put_item_type(&self, item: &crate::domain::ItemType) -> Result<(), StoreError> {
        self.put_doc(item)
    }

    pub fn delete_item_type(&self, id: Uuid) -> Result<(), StoreError> {
        self.remove_doc::<crate::domain::ItemType>(&id.to_string())
    }

    // ---------------------------------------------------------- retainers --

    pub fn list_retainers(&self) -> Result<Vec<crate::retainer::Retainer>, StoreError> {
        let mut out = self.list_docs::<crate::retainer::Retainer>()?;
        out.sort_by_key(|r| r.created_at);
        Ok(out)
    }

    pub fn get_retainer(&self, id: Uuid) -> Result<Option<crate::retainer::Retainer>, StoreError> {
        self.get_doc::<crate::retainer::Retainer>(&id.to_string())
    }

    pub fn put_retainer(&self, r: &crate::retainer::Retainer) -> Result<(), StoreError> {
        self.put_doc(r)
    }

    /// Read-modify-write a retainer's ledger under the store lock (#144):
    /// two concurrent operations can never interleave a half-applied tx.
    /// The closure's own error type survives intact (#187) — erasing it into
    /// `StoreError::Io(String)` forced callers to re-parse messages by
    /// substring, and any path that forgot to was answering 500 to a 409.
    pub fn update_retainer<F, E>(
        &self,
        id: Uuid,
        f: F,
    ) -> Result<crate::retainer::Retainer, UpdateError<E>>
    where
        F: FnOnce(&mut crate::retainer::Retainer) -> Result<(), E>,
    {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<crate::retainer::Retainer>(&id.to_string());
        let Some(mut r) = read_json::<crate::retainer::Retainer>(&path)? else {
            return Err(StoreError::NotFound.into());
        };
        f(&mut r).map_err(UpdateError::Domain)?;
        write_json(&path, &r)?;
        Ok(r)
    }

    // ------------------------------------------------------------- invoices --

    pub fn list_invoices(&self) -> Result<Vec<Invoice>, StoreError> {
        let mut out = self.list_docs::<Invoice>()?;
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.number.cmp(&b.number))
        });
        Ok(out)
    }

    pub fn get_invoice(&self, id: Uuid) -> Result<Option<Invoice>, StoreError> {
        self.get_doc::<Invoice>(&id.to_string())
    }

    /// First invoice with this number (numbers are sequential but a restored
    /// file could duplicate one; the earliest wins). Used by payment webhooks
    /// (#34), which know the invoice by its human number: the number index
    /// turns that scan into one lookup (#102).
    pub fn find_invoice_by_number(&self, number: &str) -> Result<Option<Invoice>, StoreError> {
        let idx = read_json::<HashMap<String, String>>(&self.invoice_index_path())?;
        if let Some(id) = idx
            .as_ref()
            .and_then(|m| m.get(number))
            .and_then(|s| Uuid::parse_str(s).ok())
            && let Some(invoice) = self.get_invoice(id)?
            && invoice.number == number
        {
            return Ok(Some(invoice));
        }
        // Authoritative scan (also covers an absent or drifted index).
        let mut found: Option<Invoice> = None;
        for inv in self.list_invoices()? {
            if inv.number == number && found.as_ref().is_none_or(|f| inv.created_at < f.created_at)
            {
                found = Some(inv);
            }
        }
        // A hit through the scan means the fast path missed: rebuild.
        if found.is_some() {
            self.rebuild_invoice_index()?;
        }
        Ok(found)
    }

    pub fn put_invoice(&self, invoice: &Invoice) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        std::fs::create_dir_all(self.doc_dir::<Invoice>())?;
        write_json(&self.doc_path::<Invoice>(&invoice.id.to_string()), invoice)?;
        self.index_invoice_locked(invoice)
    }

    pub fn delete_invoice(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        self.remove_doc_locked::<Invoice>(&id.to_string())?;
        // #113: the invoice JSON is the record of authority — when it goes,
        // its archived PDF goes in the same transactional write (#52 logs it).
        let pdf = self.invoice_pdf_path(&id.to_string());
        if pdf.exists() {
            std::fs::remove_file(&pdf)?;
        }
        self.unindex_invoice_locked(&id.to_string())
    }

    /// Replace a DRAFT document wholesale (#143 draft editing): identity
    /// (`id`, `number`, `created_at`) and the Draft status are enforced by
    /// the store, never trusted from the payload; anything else the caller
    /// assembled lands atomically. Issued/paid/written-off documents Conflict
    /// — the snapshot rules of #8/#113/#114 stay unbreakable.
    pub fn replace_invoice_draft(
        &self,
        id: Uuid,
        mut updated: Invoice,
    ) -> Result<Invoice, StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Invoice>(&id.to_string());
        let Some(existing) = read_json::<Invoice>(&path)? else {
            return Err(StoreError::NotFound);
        };
        if existing.status != InvoiceStatus::Draft {
            return Err(StoreError::Conflict(
                "only a draft invoice can be edited".into(),
            ));
        }
        // #256: the caller hands the assembled candidate over by value, so the
        // store stamps identity in place instead of cloning the whole draft.
        updated.id = existing.id;
        updated.number = existing.number;
        updated.created_at = existing.created_at;
        updated.status = InvoiceStatus::Draft;
        updated.issued_at = None;
        write_json(&path, &updated)?;
        self.index_invoice_locked(&updated)?;
        Ok(updated)
    }

    // ----------------------------------------------------- invoice timeline --
    //
    // A per-invoice activity timeline (Subtask A) kept in a dot-file sidecar
    // beside the invoice document, exactly like the other side artefacts:
    // `write_json` (tmp + rename), skipped by `dir_entries` so it is never a
    // document and never counts toward `MAX_DOCS`. It is a display aid only —
    // never the source of truth — so reads are lenient (a missing or malformed
    // sidecar is an empty timeline) and writes are best-effort, with the caller
    // logging rather than failing the invoice operation that triggered them.

    /// `invoices/.timeline.<id>.json` — a dot file, so the collection reader
    /// skips it.
    fn invoice_timeline_path(&self, id: Uuid) -> PathBuf {
        self.doc_dir::<Invoice>()
            .join(format!(".timeline.{id}.json"))
    }

    /// Read one invoice's timeline, oldest first. A missing or malformed
    /// sidecar is an **empty** timeline, never an error: the timeline is a
    /// display aid and a corrupt file must not break an invoice read.
    pub fn invoice_timeline(&self, id: Uuid) -> Vec<InvoiceTimelineEvent> {
        read_json::<Vec<InvoiceTimelineEvent>>(&self.invoice_timeline_path(id))
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Append one event to an invoice's timeline, retaining at most
    /// [`MAX_TIMELINE_EVENTS`] (oldest dropped). Lenient on read so a malformed
    /// sidecar is replaced rather than wedging every later append.
    pub fn append_invoice_timeline(
        &self,
        id: Uuid,
        event: InvoiceTimelineEvent,
    ) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.invoice_timeline_path(id);
        let mut events: Vec<InvoiceTimelineEvent> =
            read_json(&path).ok().flatten().unwrap_or_default();
        events.push(event);
        if events.len() > MAX_TIMELINE_EVENTS {
            let excess = events.len() - MAX_TIMELINE_EVENTS;
            events.drain(0..excess);
        }
        std::fs::create_dir_all(self.doc_dir::<Invoice>())?;
        write_json(&path, &events)
    }

    /// Atomically assign the next invoice number and persist under one writer
    /// lock, so two processes never mint the same number (#62) and a deleted
    /// invoice never frees its number for reuse (review B3): the sequence is
    /// kept in `invoices/.seq.json`, never derived from the current count.
    pub fn create_invoice(&self, invoice: Invoice) -> Result<Invoice, StoreError> {
        let _guard = self.write_lock()?;
        self.create_invoice_inner(invoice)
    }

    // ----------------------------------------------------------- categories --

    pub fn list_categories(&self) -> Result<Vec<Category>, StoreError> {
        let mut out = self.list_docs::<Category>()?;
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Seed common expense categories once for a fresh installation. The
    /// marker prevents deleted defaults from reappearing on a later restart.
    pub fn ensure_default_categories(&self) -> Result<(), StoreError> {
        let marker = self.doc_dir::<Category>().join(".defaults-seeded");
        if marker.exists() {
            return Ok(());
        }
        let _guard = self.write_lock()?;
        if marker.exists() {
            return Ok(());
        }

        let existing = self.list_docs::<Category>()?;
        let dir = self.doc_dir::<Category>();
        std::fs::create_dir_all(&dir)?;
        if existing.is_empty() {
            for name in ["Food", "Travel", "Lodging", "Supplies", "Software"] {
                let category = Category {
                    id: Uuid::new_v4(),
                    name: name.to_string(),
                    default_billable: true,
                    active: true,
                };
                write_json(
                    &self.doc_path::<Category>(&category.id.to_string()),
                    &category,
                )?;
            }
        }

        let marker_tmp = dir.join(".defaults-seeded.tmp");
        std::fs::write(&marker_tmp, b"expense-category-defaults-v1\n")?;
        std::fs::rename(marker_tmp, marker)?;
        Ok(())
    }

    pub fn get_category(&self, id: Uuid) -> Result<Option<Category>, StoreError> {
        self.get_doc::<Category>(&id.to_string())
    }

    pub fn put_category(&self, category: &Category) -> Result<(), StoreError> {
        self.put_doc(category)
    }

    pub fn update_category(&self, category: &Category) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Category>(&category.id.to_string());
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        write_json(&path, category)
    }

    pub fn delete_category(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        if !self.doc_path::<Category>(&id.to_string()).exists() {
            return Err(StoreError::NotFound);
        }
        if self
            .list_expenses()?
            .iter()
            .any(|e| e.category_id == Some(id))
        {
            // A record still referenced by others is a *state conflict*, not an
            // id collision, so `Conflict` is the honest variant at this seam.
            // The full typed-error-family consolidation is deferred to #187.
            return Err(StoreError::Conflict(
                "category still used by expenses".into(),
            ));
        }
        self.remove_doc_locked::<Category>(&id.to_string())
    }

    // ------------------------------------------------------------- expenses --

    pub fn list_expenses(&self) -> Result<Vec<Expense>, StoreError> {
        let mut out = self.walk_docs(&self.doc_dir::<Expense>(), |p| self.read_expense(p))?;
        out.sort_by_key(|e| std::cmp::Reverse((e.date, e.created_at)));
        Ok(out)
    }

    pub fn get_expense(&self, id: Uuid) -> Result<Option<Expense>, StoreError> {
        self.get_doc::<Expense>(&id.to_string())
    }

    /// Lock, ensure the directory, atomic write — and file the row in the
    /// per-user index (#335) under the same lock, so a crash can only strand a
    /// stale index (safe: reads verify it) and never a phantom row.
    pub fn put_expense(&self, expense: &Expense) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        std::fs::create_dir_all(self.doc_dir::<Expense>())?;
        write_json(&self.doc_path::<Expense>(&expense.id.to_string()), expense)?;
        self.index_expense_locked(expense)
    }

    pub fn delete_expense(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        self.remove_doc_locked::<Expense>(&id.to_string())?;
        self.unindex_expense_locked(&id.to_string())
    }

    // ---------------------------------------------------------- submissions --

    pub fn list_submissions(&self) -> Result<Vec<Submission>, StoreError> {
        let mut out = self.list_docs::<Submission>()?;
        out.sort_by_key(|s| std::cmp::Reverse(s.week_start));
        Ok(out)
    }

    pub fn get_submission(&self, id: Uuid) -> Result<Option<Submission>, StoreError> {
        self.get_doc::<Submission>(&id.to_string())
    }

    pub fn put_submission(&self, submission: &Submission) -> Result<(), StoreError> {
        self.put_doc(submission)
    }

    // --------------------------------------------------------------- claims --

    pub fn list_claims(&self) -> Result<Vec<ExpenseClaim>, StoreError> {
        let mut out = self.list_docs::<ExpenseClaim>()?;
        out.sort_by_key(|c| std::cmp::Reverse(c.created_at));
        Ok(out)
    }

    pub fn get_claim(&self, id: Uuid) -> Result<Option<ExpenseClaim>, StoreError> {
        self.get_doc::<ExpenseClaim>(&id.to_string())
    }

    pub fn put_claim(&self, claim: &ExpenseClaim) -> Result<(), StoreError> {
        self.put_doc(claim)
    }

    // ------------------------------------------------------- lock snapshot (#255)

    /// The #18 lock snapshot: every frozen entry and expense id, memoized on
    /// the source collections' directory stamps. In steady state (no writes
    /// since the last call) this parses **zero** documents; any write that
    /// changes a source collection bumps its directory mtime and forces exactly
    /// one re-scan. A scan failure (unreadable or corrupt document) propagates
    /// and is **never cached**, so callers stay fail-closed (#185).
    ///
    /// Correctness rests on the atomic tmp+rename write discipline: because a
    /// rename into a directory bumps that directory's mtime, the memoized set
    /// cannot go stale behind an out-of-band edit — a changed stamp always
    /// rebuilds. This is the "recompute cheaply rather than risk staleness"
    /// choice the fail-closed contract demands.
    pub fn lock_snapshot(&self) -> Result<Arc<LockSnapshot>, StoreError> {
        let stamp = LockStamp {
            invoices: dir_mtime(&self.doc_dir::<Invoice>()),
            submissions: dir_mtime(&self.doc_dir::<Submission>()),
            claims: dir_mtime(&self.doc_dir::<ExpenseClaim>()),
        };
        {
            let cache = self
                .lock_cache
                .lock()
                .map_err(|_| StoreError::Io("lock cache poisoned".into()))?;
            if let Some((cached, snapshot)) = cache.as_ref()
                && *cached == stamp
            {
                return Ok(Arc::clone(snapshot));
            }
        }
        // Rebuild outside the cache lock; a poisoned cache still fails closed
        // on the *next* call rather than wedging here.
        let snapshot = Arc::new(self.scan_lock_state()?);
        if let Ok(mut cache) = self.lock_cache.lock() {
            *cache = Some((stamp, Arc::clone(&snapshot)));
        }
        Ok(snapshot)
    }

    /// One full pass over invoices, submissions and claims (the parse cost the
    /// snapshot caches). Any unreadable document aborts the scan so the caller
    /// refuses the write fail-closed (#185).
    fn scan_lock_state(&self) -> Result<LockSnapshot, StoreError> {
        let mut snap = LockSnapshot::default();
        for inv in self.list_invoices()? {
            // Any ISSUED invoice freezes its records for good: the invoice
            // document is the record of what was billed, so the underlying
            // lines must not drift from it. Only a draft (nothing billed
            // yet) leaves its lines editable.
            if inv.status == crate::domain::InvoiceStatus::Draft {
                continue;
            }
            for line in &inv.lines {
                if let Some(entry) = line.entry_id {
                    snap.invoiced_entries
                        .entry(entry)
                        .or_insert_with(|| inv.number.clone());
                }
                if let Some(expense) = line.expense_id {
                    snap.invoiced_expenses
                        .entry(expense)
                        .or_insert_with(|| inv.number.clone());
                }
            }
        }
        for sub in self.list_submissions()? {
            if !matches!(
                sub.state,
                crate::domain::SubmissionState::Submitted
                    | crate::domain::SubmissionState::Approved
            ) {
                continue;
            }
            let week = sub.week_start.format("%Y-%m-%d").to_string();
            for entry in &sub.entry_ids {
                snap.submitted_entries
                    .entry(*entry)
                    .or_insert_with(|| week.clone());
            }
        }
        for claim in self.list_claims()? {
            if !matches!(
                claim.state,
                crate::domain::ClaimState::Submitted | crate::domain::ClaimState::Approved
            ) {
                continue;
            }
            for expense in &claim.expense_ids {
                snap.claimed_expenses
                    .entry(*expense)
                    .or_insert_with(|| claim.id.to_string());
            }
        }
        Ok(snap)
    }

    // ---------------------------------------------------------------- timers --

    pub fn get_timer(&self, user_id: Uuid) -> Result<Option<Timer>, StoreError> {
        self.get_doc::<Timer>(&user_id.to_string())
    }

    pub fn put_timer(&self, timer: &Timer) -> Result<(), StoreError> {
        self.put_doc(timer)
    }

    pub fn delete_timer(&self, user_id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Timer>(&user_id.to_string());
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    // -------------------------------------------------------- notifications --

    fn notifications_path(&self, user_id: Uuid) -> PathBuf {
        self.root
            .join(NOTIFICATIONS_DIR)
            .join(format!("{user_id}.json"))
    }

    pub fn list_notifications(&self, user_id: Uuid) -> Result<Vec<Notification>, StoreError> {
        Ok(read_json::<Vec<Notification>>(&self.notifications_path(user_id))?.unwrap_or_default())
    }

    pub fn push_notification(&self, user_id: Uuid, n: &Notification) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let mut list: Vec<Notification> =
            read_json(&self.notifications_path(user_id))?.unwrap_or_default();
        list.push(n.clone());
        // Bound the stored list (newest kept).
        if list.len() > 200 {
            let start = list.len() - 200;
            list = list[start..].to_vec();
        }
        std::fs::create_dir_all(self.root.join(NOTIFICATIONS_DIR))?;
        write_json(&self.notifications_path(user_id), &list)
    }

    pub fn mark_notifications_read(&self, user_id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let mut list: Vec<Notification> =
            read_json(&self.notifications_path(user_id))?.unwrap_or_default();
        for n in list.iter_mut() {
            n.read = true;
        }
        std::fs::create_dir_all(self.root.join(NOTIFICATIONS_DIR))?;
        write_json(&self.notifications_path(user_id), &list)
    }

    // ------------------------------------------------------------- schedules --

    pub fn list_schedules(&self) -> Result<Vec<RecurringSchedule>, StoreError> {
        let mut out = self.list_docs::<RecurringSchedule>()?;
        out.sort_by_key(|s| s.created_at);
        Ok(out)
    }

    pub fn get_schedule(&self, id: Uuid) -> Result<Option<RecurringSchedule>, StoreError> {
        self.get_doc::<RecurringSchedule>(&id.to_string())
    }

    pub fn put_schedule(&self, s: &RecurringSchedule) -> Result<(), StoreError> {
        self.put_doc(s)
    }

    pub fn delete_schedule(&self, id: Uuid) -> Result<(), StoreError> {
        self.remove_doc::<RecurringSchedule>(&id.to_string())
    }

    // ------------------------------------------------------------ customers --

    pub fn list_customers(&self) -> Result<Vec<Customer>, StoreError> {
        let mut out = self.list_docs::<Customer>()?;
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get_customer(&self, id: Uuid) -> Result<Option<Customer>, StoreError> {
        self.get_doc::<Customer>(&id.to_string())
    }

    pub fn put_customer(&self, customer: &Customer) -> Result<(), StoreError> {
        self.put_doc(customer)
    }

    pub fn delete_customer(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        // A customer owns its project folder; removing it would orphan those
        // documents, so the caller must empty it first. An unreadable project
        // set must NOT read as "empty" (#185): propagating here keeps the
        // remove_dir_all below from deleting documents we failed to list.
        let projects = self.list_projects(id)?;
        if !projects.is_empty() {
            return Err(StoreError::Conflict(
                "customer still has projects; delete them first".into(),
            ));
        }
        if self.has_entries_for_customer(id)? {
            return Err(StoreError::Conflict(
                "customer still has time entries; delete or re-point them first".into(),
            ));
        }
        let doc = self.doc_path::<Customer>(&id.to_string());
        if !doc.exists() {
            return Err(StoreError::NotFound);
        }
        // The projects live in a sibling DIRECTORY (customers/<id>/projects),
        // which only exists once a project was ever added; ENOENT here must
        // not fail the deletion of a project-less customer (found by the
        // #141 workflow teardown — previously a 500).
        let dir = self.root.join("customers").join(id.to_string());
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(StoreError::Io(e.to_string())),
        }
        std::fs::remove_file(&doc)?;
        Ok(())
    }

    // ------------------------------------------------------------- projects --

    fn projects_dir(&self, customer_id: Uuid) -> PathBuf {
        self.root
            .join("customers")
            .join(customer_id.to_string())
            .join("projects")
    }

    fn project_path(&self, customer_id: Uuid, code: &str) -> PathBuf {
        self.projects_dir(customer_id).join(format!("{code}.json"))
    }

    pub fn list_projects(&self, customer_id: Uuid) -> Result<Vec<Project>, StoreError> {
        let customer = self
            .get_customer(customer_id)?
            .ok_or(StoreError::NotFound)?;
        // The nested-collection case (#101): parent key + injected decode hook
        // (`read_project` resolves pre-#11 documents from the customer).
        let mut out = self.walk_docs(&self.projects_dir(customer_id), |path| {
            self.read_project(path, &customer)
        })?;
        out.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(out)
    }

    /// Read one project document, resolving any pre-#11 missing currency/rate
    /// from the owning customer.
    fn read_project(
        &self,
        path: &Path,
        customer: &Customer,
    ) -> Result<Option<Project>, StoreError> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let project = crate::domain::project_from_bytes(&bytes, customer)
                    .map_err(|e| StoreError::Io(format!("corrupt project document: {e}")))?;
                Ok(Some(project))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_project(
        &self,
        customer_id: Uuid,
        code: &str,
    ) -> Result<Option<Project>, StoreError> {
        let customer = self
            .get_customer(customer_id)?
            .ok_or(StoreError::NotFound)?;
        self.read_project(&self.project_path(customer_id, code), &customer)
    }

    pub fn put_project(&self, project: &Project) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        std::fs::create_dir_all(self.projects_dir(project.customer_id))?;
        write_json(
            &self.project_path(project.customer_id, &project.code.0),
            project,
        )
    }

    pub fn delete_project(&self, customer_id: Uuid, code: &str) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.project_path(customer_id, code);
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        if !self.list_tasks(customer_id, code)?.is_empty() {
            return Err(StoreError::Conflict(
                "project still has tasks; delete them first".into(),
            ));
        }
        if self.has_entries_for_project(customer_id, code)? {
            return Err(StoreError::Conflict(
                "project still has time entries; delete or re-point them first".into(),
            ));
        }
        std::fs::remove_file(&path)?;
        // Remove the project's task folder (tasks live inside it).
        let _ = std::fs::remove_dir_all(self.tasks_dir(customer_id, code));
        Ok(())
    }

    // ---------------------------------------------------------------- tasks --

    fn tasks_dir(&self, customer_id: Uuid, project_code: &str) -> PathBuf {
        self.projects_dir(customer_id)
            .join(project_code)
            .join("tasks")
    }

    fn task_path(&self, customer_id: Uuid, project_code: &str, code: &str) -> PathBuf {
        self.tasks_dir(customer_id, project_code)
            .join(format!("{code}.json"))
    }

    pub fn list_tasks(
        &self,
        customer_id: Uuid,
        project_code: &str,
    ) -> Result<Vec<Task>, StoreError> {
        let mut out = self.walk_docs(
            &self.tasks_dir(customer_id, project_code),
            read_json::<Task>,
        )?;
        out.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(out)
    }

    pub fn get_task(
        &self,
        customer_id: Uuid,
        project_code: &str,
        code: &str,
    ) -> Result<Option<Task>, StoreError> {
        read_json(&self.task_path(customer_id, project_code, code))
    }

    pub fn put_task(&self, task: &Task) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        std::fs::create_dir_all(self.tasks_dir(task.customer_id, &task.project_code.0))?;
        write_json(
            &self.task_path(task.customer_id, &task.project_code.0, &task.code.0),
            task,
        )
    }

    pub fn delete_task(
        &self,
        customer_id: Uuid,
        project_code: &str,
        code: &str,
    ) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.task_path(customer_id, project_code, code);
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        if self.has_entries_for_task(customer_id, project_code, code)? {
            return Err(StoreError::Conflict(
                "task still has time entries; delete or re-point them first".into(),
            ));
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }

    // -------------------------------------------------------------- entries --

    fn day_dir(&self, date: NaiveDate) -> PathBuf {
        self.root
            .join(ENTRIES_DIR)
            .join(date.format("%Y-%m-%d").to_string())
    }

    fn entry_path(&self, date: NaiveDate, id: Uuid) -> PathBuf {
        self.day_dir(date).join(format!("{id}.json"))
    }

    // -------------------------------------------------- cold-tree reads (#350) --
    //
    // ADR-003 option B keeps the cold tree as the *same documents in a different
    // folder*, so the read story is a merge rather than a second store: every
    // reader answers from hot ∪ cold, and no number a caller or a user can see
    // changes when a day is relocated. The merge is by entry id with the **hot**
    // copy winning — the #349 pass removes the hot day only after its cold copy
    // is complete, so a crash can leave a day in both trees, and the hot copy is
    // then the one the pass had not finished with.
    //
    // Enumeration always goes through `dir_entries`, never a raw `read_dir`, so
    // the run manifests, the cold id index and a pass's `.<day>.staging` crash
    // leftovers can never be mistaken for a day folder and never count toward
    // `MAX_DOCS`.

    /// The cold day folder for `date`: `archive/entries/<YYYY-MM>/<YYYY-MM-DD>/`
    /// — [`Store::day_dir`]'s mirror on the cold side, and the one place the
    /// cold layout is spelled (the #349 pass builds its paths through it too).
    pub(crate) fn cold_day_dir(&self, date: NaiveDate) -> PathBuf {
        self.root
            .join(ARCHIVE_DIR)
            .join(ENTRIES_DIR)
            .join(date.format("%Y-%m").to_string())
            .join(date.format("%Y-%m-%d").to_string())
    }

    /// The cold document path for one entry: the mirror of
    /// [`Store::entry_path`].
    fn cold_entry_path(&self, date: NaiveDate, id: Uuid) -> PathBuf {
        self.cold_day_dir(date).join(format!("{id}.json"))
    }

    /// The cold month folders: `archive/entries/<YYYY-MM>/`.
    fn cold_month_dirs(&self) -> Result<Vec<PathBuf>, StoreError> {
        let root = self.root.join(ARCHIVE_DIR).join(ENTRIES_DIR);
        if !root.exists() {
            return Ok(Vec::new());
        }
        Ok(dir_entries(&root, self.max_docs)?
            .into_iter()
            .filter(|p| p.is_dir())
            .collect())
    }

    /// Every published cold day folder as a date, ascending: the cold mirror of
    /// the hot `<YYYY-MM-DD>` folders, reached month-first. A child whose name
    /// is not a well-formed day is not a day folder.
    fn cold_day_folders(&self) -> Result<Vec<NaiveDate>, StoreError> {
        let mut out = Vec::new();
        for month in self.cold_month_dirs()? {
            for day in dir_entries(&month, self.max_docs)? {
                if !day.is_dir() {
                    continue;
                }
                let Some(name) = day.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if let Ok(date) = NaiveDate::parse_from_str(name, "%Y-%m-%d") {
                    out.push(date);
                }
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// The ids a cold day folder holds, from its listing alone (no decode) —
    /// [`Store::entry_day_disk_ids`]'s cold mirror, and the authority the merged
    /// trust rule compares against (#335/#350). An absent cold day is empty.
    fn cold_day_ids(
        &self,
        date: NaiveDate,
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let dir = self.cold_day_dir(date);
        if !dir.exists() {
            return Ok(std::collections::BTreeSet::new());
        }
        Ok(json_stems(&dir_entries(&dir, self.max_docs)?))
    }

    /// The cold copies of one day's documents, in `dir_entries` order.
    fn cold_day_docs(&self, date: NaiveDate) -> Result<Vec<Entry>, StoreError> {
        let dir = self.cold_day_dir(date);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for path in dir_entries(&dir, self.max_docs)? {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(e) = self.read_entry(&path)? {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// One day as a reader sees it: the hot documents plus the cold copies they
    /// do not already hold, deduplicated by entry id with the hot copy winning
    /// (#350). The union — not either tree — is what a day *is* after a
    /// relocation. The `MAX_DOCS` cap is one budget over the union.
    fn merged_day_docs(&self, date: NaiveDate) -> Result<Vec<Entry>, StoreError> {
        let mut out = self.walk_docs(&self.day_dir(date), |p| self.read_entry(p))?;
        merge_cold_docs(&mut out, self.cold_day_docs(date)?);
        if out.len() > self.max_docs {
            return Err(StoreError::TooManyItems);
        }
        Ok(out)
    }

    /// Whether the cold id index's slice for `date` still describes the cold day
    /// folder: the same exact set-equality rule as #335. A stale, truncated or
    /// hand-edited index can only ever be *slower* (the caller walks), never
    /// wrong.
    fn cold_index_day_is_current(
        &self,
        idx: &ColdIdIndex,
        date: NaiveDate,
    ) -> Result<bool, StoreError> {
        let key = date.format("%Y-%m-%d").to_string();
        let listed: std::collections::BTreeSet<String> = idx
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        Ok(listed == self.cold_day_ids(date)?)
    }

    /// Whether the entry document exists in **either** tree (#350).
    fn entry_exists(&self, date: NaiveDate, id: Uuid) -> bool {
        self.entry_path(date, id).is_file() || self.cold_entry_path(date, id).is_file()
    }

    /// Where `id`'s document lives: the hot tree first, then the cold tree
    /// (#350). Hot wins because it is the copy the #349 pass has not finished
    /// with, so a day a crash left in both trees resolves to the same document it
    /// did before the pass.
    ///
    /// The hot probe is the pre-#350 lookup verbatim, minus the decode: it asks
    /// whether the document is there, and the caller decodes the path it gets.
    /// The cold side is reached through the id index while that index still
    /// describes the cold day, and by walking the cold tree otherwise — the index
    /// is a hint, never the source of truth.
    fn find_entry_path(&self, id: Uuid) -> Result<Option<PathBuf>, StoreError> {
        let dir = self.root.join(ENTRIES_DIR);
        if dir.exists() {
            for day in dir_entries(&dir, self.max_docs)? {
                let path = day.join(format!("{id}.json"));
                if path.is_file() {
                    return Ok(Some(path));
                }
            }
        }
        let wanted = id.to_string();
        if let Some(idx) = self.read_cold_index() {
            let mut hints: Vec<NaiveDate> = idx
                .iter()
                .filter(|(_, ids)| ids.iter().any(|listed| listed == &wanted))
                .filter_map(|(date, _)| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
                .collect();
            hints.sort();
            for date in hints {
                if self.cold_index_day_is_current(&idx, date)? {
                    let path = self.cold_entry_path(date, id);
                    if path.is_file() {
                        return Ok(Some(path));
                    }
                }
            }
        }
        for date in self.cold_day_folders()? {
            let path = self.cold_entry_path(date, id);
            if path.is_file() {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    pub fn list_by_date(&self, date: NaiveDate) -> Result<Vec<Entry>, StoreError> {
        let mut out = self.merged_day_docs(date)?;
        sort_entries_by_created(&mut out);
        Ok(out)
    }

    pub fn list_range(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<Entry>, StoreError> {
        if (to - from).num_days() > MAX_RANGE_DAYS {
            return Err(StoreError::RangeTooLarge);
        }
        let mut out = Vec::new();
        let mut date = from;
        while date <= to {
            out.extend(self.list_by_date(date)?);
            date = date
                .succ_opt()
                .ok_or_else(|| StoreError::Io("date range overflow".into()))?;
        }
        out.sort_by_key(|e| (e.date, e.created_at));
        Ok(out)
    }

    /// Entries are addressable by id alone, but stored under their date, so this
    /// walks the hot day folders and then the cold ones (#350). NOTE (#190):
    /// there is no `MAX_RANGE_DAYS` bound here — the earlier comment claimed one
    /// but the body never implemented it. Cost is O(days) directory probes, not
    /// O(entries).
    pub fn get_entry(&self, id: Uuid) -> Result<Option<Entry>, StoreError> {
        match self.find_entry_path(id)? {
            Some(path) => read_json::<Entry>(&path),
            None => Ok(None),
        }
    }

    pub fn put_entry(&self, entry: &Entry) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        std::fs::create_dir_all(self.day_dir(entry.date))?;
        write_json(&self.entry_path(entry.date, entry.id), entry)?;
        // #350: a write leaves exactly one document. A cold entry is writable by
        // construction (#349 refuses locked and invoiced days), so writing it
        // back is a *rehydration* — the cold copy goes.
        self.rehydrate_entry_locked(entry.date, entry.id)?;
        self.index_entry_locked(entry)
    }

    pub fn delete_entry(&self, entry: &Entry) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        // #350: the document may be in either tree — a cold entry is deletable
        // and leaves through the same door — and the check spans both, so a
        // successful `get_entry` can never be a 404 here.
        if !self.entry_exists(entry.date, entry.id) {
            return Err(StoreError::NotFound);
        }
        self.remove_entry_copies_locked(entry.date, entry.id)?;
        self.unindex_entry_locked(entry.date, entry.id)
    }

    /// Remove every copy of one entry document — hot and cold — and the cold day
    /// folder along with it when it empties (#350). The caller holds the write
    /// lock.
    fn remove_entry_copies_locked(&self, date: NaiveDate, id: Uuid) -> Result<(), StoreError> {
        for path in [self.entry_path(date, id), self.cold_entry_path(date, id)] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        self.prune_empty_cold_day(date);
        Ok(())
    }

    /// Drop the cold copy of one entry, if there is one (#350). The caller holds
    /// the write lock and has already written the hot document, so afterwards
    /// exactly one document for `id` exists on disk.
    fn rehydrate_entry_locked(&self, date: NaiveDate, id: Uuid) -> Result<(), StoreError> {
        match std::fs::remove_file(self.cold_entry_path(date, id)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.prune_empty_cold_day(date);
        Ok(())
    }

    /// Best-effort prune of an emptied cold day folder: a folder holding no
    /// document is not a cold day any more, and leaving it behind would make a
    /// later pass refuse the day (`ArchiveRefusal::ColdDayExists`) for a folder
    /// with nothing in it. A failure here must never fail the write that emptied
    /// it, so the result is dropped.
    fn prune_empty_cold_day(&self, date: NaiveDate) {
        let day = self.cold_day_dir(date);
        if !day.is_dir() {
            return;
        }
        if dir_entries(&day, self.max_docs).is_ok_and(|left| left.is_empty()) {
            let _ = std::fs::remove_dir(&day);
        }
    }

    fn has_entries_for_customer(&self, customer_id: Uuid) -> Result<bool, StoreError> {
        Ok(self
            .scan_all_entries()?
            .iter()
            .any(|e| e.customer_id == customer_id))
    }

    fn has_entries_for_project(&self, customer_id: Uuid, code: &str) -> Result<bool, StoreError> {
        Ok(self
            .scan_all_entries()?
            .iter()
            .any(|e| e.customer_id == customer_id && e.project_code.0 == code))
    }

    fn has_entries_for_task(
        &self,
        customer_id: Uuid,
        project_code: &str,
        task_code: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.scan_all_entries()?.iter().any(|e| {
            e.customer_id == customer_id
                && e.project_code.0 == project_code
                && e.task_code.as_ref().is_some_and(|t| t.0 == task_code)
        }))
    }

    /// Every entry document, across all day folders, without the
    /// `MAX_RANGE_DAYS` window. Load-bearing scans (budget burn, review B1)
    /// and the admin `budget_report` HTTP handler (#190 corrected the earlier
    /// "not for HTTP handlers" claim — an admin request already calls this;
    /// the full-tree scan is intended because budgets are lifetime totals).
    pub fn list_all_entries(&self) -> Result<Vec<Entry>, StoreError> {
        self.scan_all_entries()
    }

    fn scan_all_entries(&self) -> Result<Vec<Entry>, StoreError> {
        // #350: the lifetime scan is hot ∪ cold. That is the whole point of the
        // merge — a budget total, a deletion guard or any other lifetime count
        // must not change when a day is relocated. An id present in both trees (a
        // day a crash left half-published) counts **once**, and the `MAX_DOCS` cap
        // is one budget over the union rather than one per tree.
        let mut out = Vec::new();
        let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        let dir = self.root.join(ENTRIES_DIR);
        if dir.exists() {
            for day in dir_entries(&dir, self.max_docs)? {
                if !day.is_dir() {
                    continue;
                }
                for path in dir_entries(&day, self.max_docs)? {
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if out.len() >= self.max_docs {
                        return Err(StoreError::TooManyItems);
                    }
                    if let Some(e) = read_json::<Entry>(&path)?
                        && seen.insert(e.id)
                    {
                        out.push(e);
                    }
                }
            }
        }
        for date in self.cold_day_folders()? {
            for path in dir_entries(&self.cold_day_dir(date), self.max_docs)? {
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if out.len() >= self.max_docs {
                    return Err(StoreError::TooManyItems);
                }
                if let Some(e) = read_json::<Entry>(&path)?
                    && seen.insert(e.id)
                {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }

    // ------------------------------------------------- entries archival (#349) --
    //
    // ADR-003 (`docs/architecture/adr-003-entry-retention-and-archival.md`)
    // defers retention to a measurable trigger and names this primitive its
    // precondition 1: an operator-run pass that relocates whole day folders out
    // of the hot tree (`entries/<YYYY-MM-DD>/`) into the cold tree
    // (`archive/entries/<YYYY-MM>/<YYYY-MM-DD>/`), writes a per-run sha256
    // manifest, drops the relocated days from the #335 per-user index, and
    // refreshes the cold id index (#350). Read-side it changes nothing: every
    // reader merges the cold tree back in (the `cold-tree reads` block above), so
    // a relocated day stays visible to `get_entry`/`list_range`/`list_all_entries`
    // exactly as it was.
    //
    // What may move is not this module's decision (ADR-003: the lock seam and
    // `domain::already_invoiced` remain the sole authority). The `locks`
    // parameter — `CombinedLocks` in production — answers "is this entry
    // frozen", fail-closed on an unverifiable state (#185), and
    // `domain::already_invoiced` answers "has a *non-draft* invoice already
    // billed it" — the permanent exclusion that keeps a settled invoice's
    // rate/hours evidence in the live tree. A day holding either is refused
    // whole and reported: never touched, never half-moved.

    /// Relocate every day folder strictly before `cutoff` whose entries are all
    /// provably unlocked and unbilled (ADR-003 precondition 1, #349). Returns
    /// what the pass did — or, with `dry_run`, what it *would* do while writing
    /// absolutely nothing (no cold tree, no manifest, no index rewrite, no
    /// temporary file left behind).
    ///
    /// The whole pass runs under the one write lock, so a move can never race a
    /// concurrent `put_entry`; reads never take that lock, so serving continues.
    ///
    /// `locks` is the #18 seam, not a predicate this module re-derives. A day is
    /// refused — skipped, reported, left byte-identical — when it holds an entry
    /// frozen by that seam ([`ArchiveRefusal::Locked`]), an entry on a non-draft
    /// invoice ([`ArchiveRefusal::AlreadyInvoiced`]), or when the lock state
    /// cannot be verified at all ([`ArchiveRefusal::LockUnavailable`], fail
    /// closed). A day whose cold copy already exists is refused
    /// ([`ArchiveRefusal::ColdDayExists`]) rather than overwritten, and anything
    /// under `entries/` that is not a well-formed day folder of `<uuid>.json`
    /// documents is refused ([`ArchiveRefusal::Unrecognised`]).
    ///
    /// A relocation is all-or-nothing per day: the cold copy is assembled in a
    /// hidden staging folder and published with one `rename`, and the hot day is
    /// removed last, so a crash can leave a day in both trees but never in
    /// neither and never half-archived. The per-run manifest of sha256s and the
    /// one index rewrite land once the last day has moved; a pass that relocates
    /// nothing writes neither. An interrupted pass therefore leaves the days it
    /// had already published cold but unmanifested and still named by the index —
    /// nothing is lost (the hot day goes only after its cold copy is complete),
    /// and the #335 trust rule heals the index on the next member read.
    ///
    /// A day folder holding no document has nothing that could be frozen or
    /// billed, so it is relocated like any other clean day; both counts are zero
    /// in that case.
    pub fn archive_entries_before(
        &self,
        cutoff: NaiveDate,
        dry_run: bool,
        locks: &dyn crate::lock::EntryLock,
    ) -> Result<ArchiveReport, StoreError> {
        let _guard = self.write_lock()?;
        let mut report = ArchiveReport {
            dry_run,
            cutoff: cutoff.format("%Y-%m-%d").to_string(),
            ..ArchiveReport::default()
        };

        // The two refusal sets, resolved once for the whole pass: the #18 seam
        // (fail closed — an unverifiable state refuses every candidate day
        // rather than archiving behind its back) and every entry a *non-draft*
        // invoice already billed, through the shared predicate (#248).
        let locked = locks.locked_entries();
        if let Err(unavailable) = &locked {
            tracing::warn!(
                error = %unavailable.0,
                "archive_entries_before: lock state unverifiable; refusing every candidate day"
            );
        }
        let billed: std::collections::HashSet<Uuid> =
            crate::domain::already_invoiced(&self.list_invoices()?)
                .0
                .into_iter()
                .collect();

        // Candidates: the immediate children of `entries/` that are day folders
        // older than the cutoff. Anything else the folder holds is reported as
        // unrecognised — a relocation has to prove it understood the day before
        // it touches it.
        let entries_dir = self.root.join(ENTRIES_DIR);
        let mut candidates: Vec<NaiveDate> = Vec::new();
        if entries_dir.exists() {
            for child in dir_entries(&entries_dir, self.max_docs)? {
                let name = child
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                match NaiveDate::parse_from_str(&name, "%Y-%m-%d") {
                    // Out of scope (on or after the cutoff): neither a candidate
                    // nor a refusal — the pass leaves it exactly as it found it.
                    Ok(date) if child.is_dir() && date >= cutoff => {}
                    Ok(date) if child.is_dir() => candidates.push(date),
                    // A stray document, a directory that is not a day name, or a
                    // name that is not a date at all: reported, never touched.
                    _ => report.skipped.push(SkippedDay {
                        day: name,
                        entries: 0,
                        reason: ArchiveRefusal::Unrecognised,
                    }),
                }
            }
        }
        candidates.sort();

        let mut manifest: Vec<crate::backup::FileEntry> = Vec::new();
        let mut relocated: Vec<NaiveDate> = Vec::new();
        for date in candidates {
            let day = self.day_dir(date);
            // The day's documents by file name alone: an entry's id *is* its
            // document stem, so neither the refusal check nor the manifest ever
            // decodes the money/hours a relocation must not touch.
            let docs = dir_entries(&day, self.max_docs)?;
            let mut ids: Vec<Uuid> = Vec::with_capacity(docs.len());
            let mut unrecognised = false;
            for doc in &docs {
                match doc
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                {
                    Some(id) if doc.is_file() => ids.push(id),
                    _ => unrecognised = true,
                }
            }
            let refusal = if unrecognised {
                Some(ArchiveRefusal::Unrecognised)
            } else {
                match &locked {
                    // A day is never archived behind an inconclusive check.
                    Err(_) => Some(ArchiveRefusal::LockUnavailable),
                    Ok(frozen) if ids.iter().any(|id| frozen.contains(id)) => {
                        Some(ArchiveRefusal::Locked)
                    }
                    Ok(_) if ids.iter().any(|id| billed.contains(id)) => {
                        Some(ArchiveRefusal::AlreadyInvoiced)
                    }
                    // Already cold: fail closed rather than merge or overwrite.
                    Ok(_) if self.cold_day_dir(date).exists() => {
                        Some(ArchiveRefusal::ColdDayExists)
                    }
                    Ok(_) => None,
                }
            };
            if let Some(reason) = refusal {
                report.skipped_entries += docs.len() as u64;
                report.skipped.push(SkippedDay {
                    day: date.format("%Y-%m-%d").to_string(),
                    entries: docs.len() as u64,
                    reason,
                });
                continue;
            }
            let mut bytes = 0u64;
            for doc in &docs {
                bytes += std::fs::metadata(doc)?.len();
            }
            let row = ArchivedDay {
                day: date.format("%Y-%m-%d").to_string(),
                documents: docs.len() as u64,
                bytes,
            };
            report.documents += row.documents;
            report.bytes += row.bytes;
            if !dry_run {
                manifest.extend(self.relocate_day_locked(date, &day, &docs)?);
                relocated.push(date);
            }
            report.days.push(row);
        }

        if !relocated.is_empty() {
            // The index may only name days that are on disk (#335): drop the
            // relocated days' keys in the same pass, or the index keeps a slice
            // for a folder that is no longer there.
            self.drop_archived_day_keys_locked(&relocated)?;
            // #350: the cold id index is written here and nowhere else — no read
            // ever writes it — and it is rebuilt from the whole cold tree, so it
            // also describes the days earlier passes published. Read-side it is a
            // hint: a read that finds it stale or missing walks the cold tree and
            // is still correct.
            self.write_cold_index_locked()?;
            let rel = self.archive_manifest_rel();
            self.write_archive_manifest_locked(&rel, cutoff, &manifest)?;
            report.manifest = Some(rel);
        }
        report.skipped.sort_by_key(|row| row.day.clone());
        Ok(report)
    }

    /// Rebuild `archive/entries/.idx.ids.json` — `{ "<YYYY-MM-DD>": ["<id>", …] }`
    /// — from the published cold tree (#350). The pass calls it once, in the same
    /// end-of-run block that prunes the per-user index and writes the manifest; no
    /// read ever writes it, and no read trusts a slice without checking it against
    /// the cold day folder it describes.
    ///
    /// The index is derived from the tree rather than appended to, so it always
    /// describes the days *every* pass has published. A rehydrated entry (#350)
    /// leaves its row stale until the next pass, which the per-day check turns
    /// into a walk rather than a wrong answer.
    fn write_cold_index_locked(&self) -> Result<(), StoreError> {
        let mut idx = ColdIdIndex::new();
        for date in self.cold_day_folders()? {
            let ids = self.cold_day_ids(date)?;
            if !ids.is_empty() {
                idx.insert(
                    date.format("%Y-%m-%d").to_string(),
                    ids.into_iter().collect(),
                );
            }
        }
        let path = self.cold_index_path();
        let Some(parent) = path.parent() else {
            return Err(StoreError::Io("cold index path has no parent".into()));
        };
        std::fs::create_dir_all(parent)?;
        write_json(&path, &idx)
    }

    /// Publish one day into the cold tree byte for byte and return its manifest
    /// rows. The caller holds the write lock and has already refused the day if
    /// it is frozen, billed, already cold or not recognisably a day.
    ///
    /// The cold copy is assembled in a hidden `.staging` folder that is `rename`d
    /// into place as its last step, so the archive never shows a partial day, and
    /// the hot day is removed only once its cold copy is complete — a crash can
    /// duplicate a day, never lose one. Each document is copied with the store's
    /// tmp+rename discipline; nothing is decoded, so the money/hours invariants
    /// (minor units, hundredths) cannot be touched by a relocation.
    fn relocate_day_locked(
        &self,
        date: NaiveDate,
        day: &Path,
        docs: &[PathBuf],
    ) -> Result<Vec<crate::backup::FileEntry>, StoreError> {
        let cold = self.cold_day_dir(date);
        let month = cold
            .parent()
            .ok_or_else(|| StoreError::Io("archive day path has no parent".into()))?;
        std::fs::create_dir_all(month)?;
        let name = date.format("%Y-%m-%d").to_string();
        let staging = month.join(format!(".{name}.staging"));
        // A staging folder can only be a crash leftover of a pass whose hot day is
        // still here (the hot day goes last), so its content is a duplicate of
        // bytes about to be copied again.
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        std::fs::create_dir_all(&staging)?;
        let mut rows = Vec::with_capacity(docs.len());
        for doc in docs {
            let file = doc
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| StoreError::Io("entry document has no file name".into()))?;
            let (size, sha256) = copy_document_atomic(doc, &staging.join(file))?;
            rows.push(crate::backup::FileEntry {
                path: format!(
                    "{ARCHIVE_DIR}/{ENTRIES_DIR}/{}/{name}/{file}",
                    date.format("%Y-%m")
                ),
                size,
                sha256,
            });
        }
        std::fs::rename(&staging, &cold)?;
        std::fs::remove_dir_all(day)?;
        Ok(rows)
    }

    /// Drop the relocated days from the #335 per-user index, so it never names a
    /// day that is no longer on disk (the index's own trust rule compares exactly
    /// that, #335). The caller holds the write lock. A missing or unreadable index
    /// is "no index": nothing names the day, so nothing is rewritten.
    fn drop_archived_day_keys_locked(&self, days: &[NaiveDate]) -> Result<(), StoreError> {
        let Some(mut idx) = self.read_entry_index() else {
            return Ok(());
        };
        let keys: Vec<String> = days
            .iter()
            .map(|d| d.format("%Y-%m-%d").to_string())
            .collect();
        let before = idx.days.len();
        for key in &keys {
            idx.days.remove(key);
        }
        if idx.days.len() == before {
            return Ok(());
        }
        write_json(&self.entry_index_path(), &idx)
    }

    /// The data-root-relative path of this run's manifest:
    /// `archive/entries/.manifest.<UTC run stamp>-<random>.json`. One file per
    /// run — the ADR keeps the manifest with the operator's backup — and unique,
    /// so two runs can never overwrite each other's record.
    fn archive_manifest_rel(&self) -> String {
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
        let unique: String = Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(8)
            .collect();
        format!("{ARCHIVE_DIR}/{ENTRIES_DIR}/.manifest.{stamp}-{unique}.json")
    }

    /// Write the run's manifest: one [`crate::backup::FileEntry`] row (path, size,
    /// sha256) per document relocated, in the shape `--backup` and the #113 PDF
    /// archive already prove a file with, so a restore or an operator can verify
    /// the cold tree byte for byte. Atomic like every other write; the caller
    /// holds the lock.
    fn write_archive_manifest_locked(
        &self,
        rel: &str,
        cutoff: NaiveDate,
        files: &[crate::backup::FileEntry],
    ) -> Result<(), StoreError> {
        let cutoff = cutoff.format("%Y-%m-%d").to_string();
        write_json(
            &self.root.join(rel),
            &ArchiveManifest {
                app: "tucano-time",
                version: env!("CARGO_PKG_VERSION"),
                created_at: Utc::now().to_rfc3339(),
                cutoff,
                files,
            },
        )
    }
}

// -------------------------- entries archival report and manifest types (#349) --

/// What one [`Store::archive_entries_before`] pass did, or — for a dry run — what
/// it would have done without writing anything (#349). The operator-facing report
/// the ADR's trigger asks for: days, documents, bytes and the entries skipped, so
/// a `--dry-run` can be read before the real pass and the manifest kept with the
/// backup afterwards.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ArchiveReport {
    /// Whether this was a dry run (nothing was written).
    pub dry_run: bool,
    /// The cutoff the pass was given (`YYYY-MM-DD`): days strictly before it are
    /// candidates, a day on or after it is never touched and never reported.
    pub cutoff: String,
    /// Day folders relocated (a dry run: that would be relocated), ascending.
    pub days: Vec<ArchivedDay>,
    /// Day folders refused, with the reason and how many documents they hold.
    pub skipped: Vec<SkippedDay>,
    /// Documents relocated in total (a dry run: that would be).
    pub documents: u64,
    /// Bytes relocated in total (a dry run: that would be).
    pub bytes: u64,
    /// Documents left in the hot tree because their day was refused.
    pub skipped_entries: u64,
    /// Data-root-relative path of the sha256 manifest a real pass wrote; `None` on
    /// every dry run and whenever the pass relocated nothing.
    pub manifest: Option<String>,
}

/// One day folder a pass relocated — or, in a dry run, would relocate.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ArchivedDay {
    /// The day, `YYYY-MM-DD`.
    pub day: String,
    /// Documents relocated with it.
    pub documents: u64,
    /// Their total size in bytes.
    pub bytes: u64,
}

/// One day folder a pass refused to relocate.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SkippedDay {
    /// The day folder's name: `YYYY-MM-DD`, or whatever the directory is called
    /// when it is not a day folder at all.
    pub day: String,
    /// Documents on it.
    pub entries: u64,
    /// Why it stayed in the hot tree.
    pub reason: ArchiveRefusal,
}

/// Why a day folder stayed in the hot tree (#349).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveRefusal {
    /// An entry on the day is frozen by the #18 seam (`CombinedLocks`).
    Locked,
    /// An entry on the day is on a non-draft invoice (`domain::already_invoiced`):
    /// the permanent refusal, because a settled or written-off invoice's line is
    /// still the evidence for a number that was issued (ADR-003).
    AlreadyInvoiced,
    /// The lock state could not be verified, so the day is refused fail-closed
    /// (#185) rather than archived behind the seam's back.
    LockUnavailable,
    /// The cold day folder is already there: the archive is never overwritten, so
    /// an interrupted pass is resolved by the operator, not by a merge.
    ColdDayExists,
    /// The folder is not a well-formed day folder of `<uuid>.json` documents.
    Unrecognised,
}

/// The per-run relocation manifest (#349): `backup::Manifest`'s header plus the
/// cutoff the run was given, over the [`crate::backup::FileEntry`] rows (path,
/// size, sha256) that shape already proves an archived file with (#94/#113).
#[derive(Debug, serde::Serialize)]
struct ArchiveManifest<'a> {
    app: &'a str,
    version: &'a str,
    created_at: String,
    cutoff: String,
    files: &'a [crate::backup::FileEntry],
}

/// Copy one document byte for byte into `dest` with the store's tmp+rename
/// discipline, returning its size and sha256 — the values the manifest records.
/// The bytes are streamed through a 64 KiB buffer and never decoded, so a
/// relocation can neither re-encode money/hours nor blow a memory budget, and the
/// hash covers exactly the bytes the archive received.
fn copy_document_atomic(src: &Path, dest: &Path) -> Result<(u64, String), StoreError> {
    use sha2::Digest;
    use std::io::Read;
    let tmp = dest.with_extension("json.tmp");
    std::fs::copy(src, &tmp)?;
    let mut file = File::open(&tmp)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let size = file.metadata()?.len();
    drop(file);
    std::fs::rename(&tmp, dest)?;
    Ok((size, crate::hex_lower(&hasher.finalize())))
}

// -------------------------------------------- multi-document transactions --
//
// The fs2 write lock is non-reentrant, so handlers must NOT compose
// read-check-write across several public methods: two locked steps can
// interleave with another writer and leave half the pair applied (review
// B4). Each of these takes the lock once and applies the whole unit.

impl Store {
    /// Issue an invoice atomically: refuse unless the stored invoice is a
    /// draft, then persist the transition **and** the archived PDF in one
    /// transaction (#113) — no issue-vs-pay race window, and the document is
    /// on file exactly when the invoice is. The PDF is written only if absent
    /// (the archive is never overwritten for an issued invoice); the invoice
    /// JSON is the record of authority, so the write fails as a whole if the
    /// archive cannot be persisted (no partial persistence).
    pub fn issue_invoice(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
        due: NaiveDate,
        pdf: &[u8],
    ) -> Result<Invoice, StoreError> {
        let _guard = self.write_lock()?;
        let Some(mut invoice) = read_json::<Invoice>(&self.doc_path::<Invoice>(&id.to_string()))?
        else {
            return Err(StoreError::NotFound);
        };
        if invoice.status != InvoiceStatus::Draft {
            return Err(StoreError::Conflict(
                "only a draft invoice can be issued".into(),
            ));
        }
        let pdf_path = self.invoice_pdf_path(&id.to_string());
        let hint = if pdf_path.exists() {
            // Archive already on file (e.g. a retry after the invoice write
            // failed): the historical bytes stand, so hash what is stored.
            let on_disk = std::fs::read(&pdf_path)?;
            pdf_hint(&invoice.number, &on_disk, now)
        } else {
            std::fs::create_dir_all(self.doc_dir::<Invoice>())?;
            write_bytes_atomic(&pdf_path, pdf)?;
            // #255: the file just written *is* `pdf`, so hash the in-memory
            // bytes instead of re-reading up to 8 MB under the write lock.
            pdf_hint(&invoice.number, pdf, now)
        };
        invoice.status = InvoiceStatus::Issued;
        invoice.issued_at = Some(now);
        invoice.due_date = Some(due);
        invoice.pdf = Some(hint);
        write_json(&self.doc_path::<Invoice>(&id.to_string()), &invoice)?;
        Ok(invoice)
    }

    /// Archive an already-rendered PDF for an issued invoice that has no
    /// archive yet (the legacy-invoice path of #113: bytes are stable because
    /// the invoice is snapshot-locked). Never overwrites an existing file;
    /// (re-)writes only the hint, from the bytes actually on file.
    pub fn attach_invoice_pdf(
        &self,
        id: Uuid,
        pdf: &[u8],
        now: DateTime<Utc>,
    ) -> Result<Invoice, StoreError> {
        let _guard = self.write_lock()?;
        let Some(mut invoice) = read_json::<Invoice>(&self.doc_path::<Invoice>(&id.to_string()))?
        else {
            return Err(StoreError::NotFound);
        };
        if invoice.status == InvoiceStatus::Draft {
            return Err(StoreError::Conflict("drafts have no PDF archive".into()));
        }
        let path = self.invoice_pdf_path(&id.to_string());
        if invoice.pdf.is_some() && path.exists() {
            // Already archived and hinted: the historical record stands.
            return Ok(invoice);
        }
        if !path.exists() {
            std::fs::create_dir_all(self.doc_dir::<Invoice>())?;
            write_bytes_atomic(&path, pdf)?;
        }
        let archived = std::fs::read(&path)?;
        invoice.pdf = Some(pdf_hint(&invoice.number, &archived, now));
        write_json(&self.doc_path::<Invoice>(&id.to_string()), &invoice)?;
        Ok(invoice)
    }

    fn invoice_pdf_path(&self, id: &str) -> PathBuf {
        self.root.join("invoices").join(format!("{id}.pdf"))
    }

    /// Read the archived PDF (#50: size-capped; the path is built only from a
    /// `Uuid`, never from request text). `Ok(None)` when nothing is archived.
    pub fn invoice_pdf_bytes(&self, id: Uuid) -> Result<Option<Vec<u8>>, StoreError> {
        match std::fs::read(self.invoice_pdf_path(&id.to_string())) {
            Ok(bytes) if bytes.len() as u64 <= MAX_PDF_BYTES => Ok(Some(bytes)),
            Ok(_) => Err(StoreError::Io("archived PDF exceeds the size cap".into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Mark paid atomically with the issued-state check: records the full
    /// remaining balance (#114 default; keeps the pre-ledger behaviour for
    /// callers that do not pass an amount).
    pub fn pay_invoice(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
        reference: String,
    ) -> Result<Invoice, StoreError> {
        self.record_payment(id, None, reference, "manual".into(), now)
    }

    /// Record a payment on the invoice ledger (#114), state check + write
    /// under one lock. `amount` defaults to the full balance; rejection is
    /// all-or-nothing (zero, over-balance, closed states) with no partial
    /// persistence. Settling the last cent flips to `Paid`; anything less
    /// parks at `PartlyPaid`.
    pub fn record_payment(
        &self,
        id: Uuid,
        amount: Option<u64>,
        reference: String,
        method: String,
        now: DateTime<Utc>,
    ) -> Result<Invoice, StoreError> {
        let _guard = self.write_lock()?;
        self.record_payment_locked(id, amount, reference, method, now)
    }

    fn record_payment_locked(
        &self,
        id: Uuid,
        amount: Option<u64>,
        reference: String,
        method: String,
        now: DateTime<Utc>,
    ) -> Result<Invoice, StoreError> {
        let path = self.doc_path::<Invoice>(&id.to_string());
        let Some(mut invoice) = read_json::<Invoice>(&path)? else {
            return Err(StoreError::NotFound);
        };
        match invoice.status {
            InvoiceStatus::Issued | InvoiceStatus::PartlyPaid => {}
            InvoiceStatus::Draft => {
                return Err(StoreError::Conflict(
                    "only an issued invoice can be paid".into(),
                ));
            }
            InvoiceStatus::Paid => {
                return Err(StoreError::Conflict("this invoice is already paid".into()));
            }
            InvoiceStatus::WrittenOff => {
                return Err(StoreError::Conflict("this invoice is written off".into()));
            }
        }
        let balance = invoice.balance_minor();
        if balance == 0 {
            return Err(StoreError::Conflict(
                "this invoice has no balance to collect".into(),
            ));
        }
        let amt = amount.unwrap_or(balance);
        if amt == 0 {
            return Err(StoreError::Conflict(
                "payment amount must be positive".into(),
            ));
        }
        if amt > balance {
            return Err(StoreError::Conflict(format!(
                "payment of {amt} exceeds the balance of {balance}"
            )));
        }
        invoice.payments.push(crate::domain::InvoicePayment {
            id: uuid::Uuid::new_v4(),
            amount_minor: amt,
            received_at: now,
            reference: reference.clone(),
            method,
        });
        // The legacy mirror fields stay truthful for pre-#114 readers.
        invoice.payment_reference = reference;
        if amt == balance {
            invoice.status = InvoiceStatus::Paid;
            invoice.paid_at = Some(now);
        } else {
            invoice.status = InvoiceStatus::PartlyPaid;
        }
        write_json(&path, &invoice)?;
        Ok(invoice)
    }

    /// Forgive the remaining balance (#114): final for the open states;
    /// `paid_minor` and the ledger stay untouched.
    pub fn write_off_invoice(
        &self,
        id: Uuid,
        reason: String,
        now: DateTime<Utc>,
    ) -> Result<Invoice, StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Invoice>(&id.to_string());
        let Some(mut invoice) = read_json::<Invoice>(&path)? else {
            return Err(StoreError::NotFound);
        };
        if !invoice.status.is_open() {
            return Err(StoreError::Conflict(
                "only an issued or partly-paid invoice can be written off".into(),
            ));
        }
        if reason.trim().is_empty() {
            return Err(StoreError::Conflict(
                "a write-off reason is required".into(),
            ));
        }
        invoice.status = InvoiceStatus::WrittenOff;
        invoice.write_off_reason = reason.trim().to_string();
        invoice.written_off_at = Some(now);
        write_json(&path, &invoice)?;
        Ok(invoice)
    }

    /// Create the numbered invoice **and** advance the schedule under one
    /// lock (review B2): either both happen or neither, so a failed write
    /// can never double-bill a period or silently skip it.
    pub fn create_invoice_and_advance(
        &self,
        invoice: Invoice,
        schedule: &RecurringSchedule,
    ) -> Result<Invoice, StoreError> {
        let _guard = self.write_lock()?;
        let invoice = self.create_invoice_inner(invoice)?;
        std::fs::create_dir_all(self.root.join("schedules"))?;
        write_json(
            &self.doc_path::<RecurringSchedule>(&schedule.id.to_string()),
            schedule,
        )?;
        Ok(invoice)
    }

    /// Write an entry in one locked step; when the date moved, the old
    /// day-folder document is removed in the same transaction (review B4:
    /// never an entry living in two folders).
    pub fn save_entry(
        &self,
        existing_date: Option<NaiveDate>,
        updated: &Entry,
    ) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        self.save_entry_inner(existing_date, updated)
    }

    /// #218: run a handler-side guard (typically the `EntryLock` check)
    /// INSIDE this store's write lock, before anything is written. Without
    /// this shape the check and the write were two separate lock
    /// acquisitions, so an `issue_invoice` landing between them let an edit
    /// through on now-locked entries (round-2 finding R2-5 — the residual
    /// B4-pattern race). `verify` returns the handler's own error type;
    /// store errors fold in through `From`. `write` must use `*_locked`
    /// internals — the process lock is not reentrant.
    pub fn guarded_write<F, G, E>(&self, verify: F, write: G) -> Result<(), E>
    where
        F: FnOnce() -> Result<(), E>,
        G: FnOnce(&Self) -> Result<(), StoreError>,
        E: From<StoreError>,
    {
        let _guard = self.write_lock().map_err(E::from)?;
        verify()?;
        write(self).map_err(E::from)
    }

    /// `save_entry` with the lock check executed in the same transaction.
    pub fn save_entry_guarded<F, E>(
        &self,
        existing_date: Option<NaiveDate>,
        updated: &Entry,
        verify_unlocked: F,
    ) -> Result<(), E>
    where
        F: FnOnce() -> Result<(), E>,
        E: From<StoreError>,
    {
        self.guarded_write(verify_unlocked, |s| {
            s.save_entry_inner(existing_date, updated)
        })
    }

    /// `delete_entry` with the lock check executed in the same transaction.
    pub fn delete_entry_guarded<F, E>(&self, entry: &Entry, verify_unlocked: F) -> Result<(), E>
    where
        F: FnOnce() -> Result<(), E>,
        E: From<StoreError>,
    {
        self.guarded_write(verify_unlocked, |s| {
            // #350: a cold entry is an ordinary entry to every caller — the
            // handler has already resolved it through `get_entry`, so the delete
            // may not 404 because the document happens to be in the archive. The
            // existence check and the removal span both trees.
            if !s.entry_exists(entry.date, entry.id) {
                return Err(StoreError::NotFound);
            }
            s.remove_entry_copies_locked(entry.date, entry.id)?;
            s.unindex_entry_locked(entry.date, entry.id)
        })
    }

    /// `delete_expense` with the invoice/claim lock scans executed in the
    /// same transaction (#218).
    pub fn delete_expense_guarded<F, E>(&self, id: Uuid, verify_unlocked: F) -> Result<(), E>
    where
        F: FnOnce() -> Result<(), E>,
        E: From<StoreError>,
    {
        self.guarded_write(verify_unlocked, |s| {
            s.remove_doc_locked::<Expense>(&id.to_string())?;
            s.unindex_expense_locked(&id.to_string())
        })
    }

    /// `put_expense` with the invoice/claim lock check executed in the same
    /// transaction (#18/#218): an expense referenced by an issued invoice or a
    /// submitted/approved claim is frozen against edits exactly as it is against
    /// deletes, and an unverifiable lock state refuses the write (#185).
    pub fn update_expense_guarded<F, E>(
        &self,
        expense: &Expense,
        verify_unlocked: F,
    ) -> Result<(), E>
    where
        F: FnOnce() -> Result<(), E>,
        E: From<StoreError>,
    {
        self.guarded_write(verify_unlocked, |s| {
            let path = s.doc_path::<Expense>(&expense.id.to_string());
            if !path.exists() {
                return Err(StoreError::NotFound);
            }
            std::fs::create_dir_all(s.doc_dir::<Expense>())?;
            write_json(&path, expense)?;
            s.index_expense_locked(expense)
        })
    }

    fn save_entry_inner(
        &self,
        existing_date: Option<NaiveDate>,
        updated: &Entry,
    ) -> Result<(), StoreError> {
        std::fs::create_dir_all(self.day_dir(updated.date))?;
        write_json(&self.entry_path(updated.date, updated.id), updated)?;
        // #350: the write is authoritative and leaves one document, hot. The cold
        // copy under the new date goes (rehydration), and so does one left under
        // the old date — moving out of, or between, cold days strands nothing.
        self.rehydrate_entry_locked(updated.date, updated.id)?;
        if let Some(old_date) = existing_date
            && old_date != updated.date
        {
            let old = self.entry_path(old_date, updated.id);
            if old.exists() {
                std::fs::remove_file(old)?;
            }
            self.rehydrate_entry_locked(old_date, updated.id)?;
            self.unindex_entry_locked(old_date, updated.id)?;
        }
        self.index_entry_locked(updated)
    }

    /// Log the timer's entry and clear the timer under one lock: a failure
    /// leaves the timer intact rather than double-logging on retry.
    ///
    /// #214: the timer's existence is verified INSIDE the same lock, before
    /// the entry is written. Two concurrent `POST /api/timer/stop` calls both read
    /// the timer earlier (that read is unlocked); the loser used to still write
    /// its own entry and merely skip the timer delete, double-logging the
    /// time. Now the loser finds no timer under the lock and 404s instead.
    pub fn finish_timer(&self, user_id: Uuid, entry: &Entry) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Timer>(&user_id.to_string());
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        std::fs::create_dir_all(self.day_dir(entry.date))?;
        write_json(&self.entry_path(entry.date, entry.id), entry)?;
        std::fs::remove_file(&path)?;
        self.index_entry_locked(entry)
    }

    /// First-user (bootstrap) creation: the has-users check and the write
    /// share one lock, closing the double-admin TOCTOU (review A12/B4).
    pub fn put_user_if_none(&self, user: &User) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        // The "no users yet" guard keeps `AlreadyExists` on purpose: `api::auth`
        // matches it to raise the `initialised` code. Folding it into the
        // typed-error cleanup is tracked by #187.
        if !self.list_users()?.is_empty() {
            return Err(StoreError::AlreadyExists("initialised".into()));
        }
        write_json(&self.doc_path::<User>(&user.id.to_string()), user)?;
        self.index_user_locked(user)
    }

    /// Number + persist an invoice with the lock **already** held.
    fn create_invoice_inner(&self, mut invoice: Invoice) -> Result<Invoice, StoreError> {
        std::fs::create_dir_all(self.root.join("invoices"))?;
        let seq_path = self.root.join("invoices").join(".seq.json");
        let stored: u64 = read_json(&seq_path)?.unwrap_or(0);
        // #255: the count-derived guard only exists to bootstrap a pre-ledger
        // data dir (one written before B3 introduced `.seq.json`). Once the
        // ledger exists it is authoritative — the earlier unconditional
        // `list_invoices()` re-listed every invoice under the write lock on
        // every creation, which the sequence file already made unnecessary.
        let n = if seq_path.exists() {
            stored + 1
        } else {
            stored.max(self.list_invoices()?.len() as u64) + 1
        };
        write_json(&seq_path, &n)?;
        invoice.number = format!("INV-{n:04}");
        write_json(&self.doc_path::<Invoice>(&invoice.id.to_string()), &invoice)?;
        self.index_invoice_locked(&invoice)?;
        Ok(invoice)
    }
}

/// The cold id index (#350): `date -> ids` over the published cold days, in
/// `archive/entries/.idx.ids.json`. A dot file like the indexes below, so it is
/// never a document and never counts toward `MAX_DOCS`; written only by the
/// archival pass, and validated slice by slice against the cold day folder it
/// describes before any read believes it.
type ColdIdIndex = HashMap<String, Vec<String>>;

/// Filename sanitiser for derived download names: invoice numbers are
/// minted by `create_invoice` (`INV-%04d`), but the hint never carries
/// anything that could steer a path (#50).
/// The `pdf` hint persisted with the invoice (#113).
fn pdf_hint(number: &str, archived: &[u8], now: DateTime<Utc>) -> PdfHint {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(archived);
    PdfHint {
        filename: format!("{}.pdf", crate::domain::safe_filename(number)),
        bytes: archived.len() as u64,
        sha256: crate::hex_lower(&h.finalize()),
        archived_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cust() -> Customer {
        Customer {
            id: Uuid::new_v4(),
            name: "X".into(),
            currency: crate::domain::Currency("EUR".into()),
            default_rate_minor: 1,
            active: true,
            email: String::new(),
            payment_terms: None,
            invoice_notes: String::new(),
            invoice_subject: String::new(),
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,
            payment_details: None,
        }
    }

    #[test]
    fn collection_cap_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data"))
            .unwrap()
            .with_max_docs(2);
        store.put_customer(&cust()).unwrap();
        store.put_customer(&cust()).unwrap();
        store.put_customer(&cust()).unwrap(); // 3rd file
        let err = store.list_customers().expect_err("3 > cap of 2");
        assert!(matches!(err, StoreError::TooManyItems), "{err:?}");
    }

    #[test]
    fn has_users_ignores_side_indexes_and_tmp_files() {
        // #256: the public status probe answers from the directory, so only a
        // real user *document* may flip it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let store = Store::open(&root).unwrap();
        // No users dir at all.
        assert!(!store.has_users().unwrap());
        // The email side index and a stranded `.tmp` write live in the users
        // dir but are not documents.
        let users_dir = root.join("users");
        std::fs::create_dir_all(&users_dir).unwrap();
        std::fs::write(users_dir.join(".idx.emails.json"), b"{}").unwrap();
        std::fs::write(users_dir.join("abc.json.tmp"), b"{}").unwrap();
        assert!(!store.has_users().unwrap());
        // A real document flips it.
        let user = User {
            id: Uuid::new_v4(),
            name: "A".into(),
            email: "a@example.com".into(),
            role: crate::auth::Role::Member,
            active: true,
            default_rate_minor: 0,
            cost_rate_minor: 0,
            password_hash: "x".into(),
            created_at: chrono::Utc::now(),
            session_version: 0,
        };
        store.put_user(&user).unwrap();
        assert!(store.has_users().unwrap());
    }
}

#[cfg(test)]
mod invoice_seq_tests {
    use super::*;
    use crate::domain::{Currency, Invoice, InvoiceStatus};

    fn draft(number: &str) -> Invoice {
        Invoice {
            id: Uuid::new_v4(),
            number: number.into(),
            customer_id: Uuid::new_v4(),
            currency: Currency("EUR".into()),
            period_from: chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            period_to: chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
            lines: vec![],
            total_minor: 0,
            status: InvoiceStatus::Draft,
            created_at: chrono::Utc::now(),
            issued_at: None,
            due_date: None,
            paid_at: None,
            payment_reference: String::new(),
            pdf: None,
            payments: vec![],
            write_off_reason: String::new(),
            written_off_at: None,
            tax_hundredths: 0,
            discount_hundredths: 0,
        }
    }

    #[test]
    fn deleted_invoice_never_frees_its_number() {
        // Review B3: number must be monotonic, not count-derived.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let a = store.create_invoice(draft("_")).unwrap();
        let b = store.create_invoice(draft("_")).unwrap();
        assert_eq!(
            (a.number.as_str(), b.number.as_str()),
            ("INV-0001", "INV-0002")
        );
        store.delete_invoice(a.id).unwrap();
        let c = store.create_invoice(draft("_")).unwrap();
        assert_eq!(c.number, "INV-0003", "numbers must not be recycled");
        // The counter file is not mistaken for an invoice document.
        assert_eq!(store.list_invoices().unwrap().len(), 2);
    }
}

#[cfg(test)]
mod txn_tests {
    use super::*;
    use crate::domain::{Currency, InvoiceLine, InvoiceStatus, LineKind};
    use chrono::TimeZone;

    fn invoice() -> Invoice {
        Invoice {
            id: Uuid::new_v4(),
            number: "_".into(),
            customer_id: Uuid::new_v4(),
            currency: Currency("EUR".into()),
            period_from: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            period_to: NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
            lines: vec![InvoiceLine {
                kind: LineKind::Fixed,
                date: NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
                entry_id: None,
                expense_id: None,
                project_code: None,
                task_code: None,
                hours: None,
                rate_minor: None,
                amount_minor: 100,
                note: String::new(),
                quantity_hundredths: None,
                unit_price_minor: None,
                item_kind: None,
            }],
            total_minor: 100,
            status: InvoiceStatus::Draft,
            created_at: Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
            issued_at: None,
            due_date: None,
            paid_at: None,
            payment_reference: String::new(),
            pdf: None,
            payments: vec![],
            write_off_reason: String::new(),
            written_off_at: None,
            tax_hundredths: 0,
            discount_hundredths: 0,
        }
    }

    fn user(email: &str) -> User {
        User {
            id: Uuid::new_v4(),
            name: "U".into(),
            email: email.into(),
            role: crate::auth::Role::Member,
            active: true,
            default_rate_minor: 0,
            cost_rate_minor: 0,
            password_hash: String::new(),
            created_at: Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
            session_version: 1,
        }
    }

    #[test]
    fn put_user_if_none_is_atomic_first_user() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        store.put_user_if_none(&user("a@b.co")).unwrap();
        let err = store.put_user_if_none(&user("c@d.co")).unwrap_err();
        assert!(matches!(err, StoreError::AlreadyExists(_)));
        assert_eq!(store.list_users().unwrap().len(), 1);
    }

    #[test]
    fn issue_then_pay_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let inv = store.create_invoice(invoice()).unwrap();
        // Cannot pay a draft.
        let e = store
            .pay_invoice(inv.id, Utc::now(), "ref".into())
            .unwrap_err();
        assert!(matches!(e, StoreError::Conflict(_)));
        store
            .issue_invoice(
                inv.id,
                Utc::now(),
                NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
                b"%PDF-1.4 test",
            )
            .unwrap();
        let paid = store
            .pay_invoice(inv.id, Utc::now(), "wire:1".into())
            .unwrap();
        assert_eq!(paid.status, InvoiceStatus::Paid);
        assert_eq!(paid.payment_reference, "wire:1");
        // Double issue now conflicts.
        assert!(matches!(
            store.issue_invoice(
                inv.id,
                Utc::now(),
                NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
                b"%PDF-1.4 test"
            ),
            Err(StoreError::Conflict(_))
        ));
    }

    #[test]
    fn pdf_archive_is_written_once_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        // A draft has no archive.
        let inv = store.create_invoice(invoice()).unwrap();
        assert!(store.invoice_pdf_bytes(inv.id).unwrap().is_none());
        store
            .issue_invoice(
                inv.id,
                Utc::now(),
                NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
                b"%PDF-1.4 first",
            )
            .unwrap();
        assert_eq!(
            store.invoice_pdf_bytes(inv.id).unwrap().unwrap(),
            b"%PDF-1.4 first"
        );
        // A later attach with different bytes must NOT rewrite history.
        let again = store
            .attach_invoice_pdf(inv.id, b"%PDF-1.4 second", Utc::now())
            .unwrap();
        assert_eq!(
            store.invoice_pdf_bytes(inv.id).unwrap().unwrap(),
            b"%PDF-1.4 first",
            "the archive is immutable once issued"
        );
        assert_eq!(again.pdf.as_ref().unwrap().bytes, 14);
        // Hint carries a sha256 of the archived bytes.
        assert_eq!(again.pdf.as_ref().unwrap().sha256.len(), 64);
    }

    #[test]
    fn attach_refuses_drafts_and_deletes_with_the_invoice() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let inv = store.create_invoice(invoice()).unwrap();
        assert!(matches!(
            store.attach_invoice_pdf(inv.id, b"x", Utc::now()),
            Err(StoreError::Conflict(_))
        ));
        store
            .issue_invoice(
                inv.id,
                Utc::now(),
                NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
                b"%PDF-1.4 doc",
            )
            .unwrap();
        // Drafts-only delete guard lives in the API; the store removes the
        // PDF together with the document (same transaction).
        store.delete_invoice(inv.id).unwrap();
        assert!(store.invoice_pdf_bytes(inv.id).unwrap().is_none());
    }

    #[test]
    fn partial_payments_walk_the_ledger_to_paid() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let mut inv = invoice();
        inv.total_minor = 30_000;
        let inv = store.create_invoice(inv).unwrap();
        store
            .issue_invoice(
                inv.id,
                Utc::now(),
                NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
                b"%PDF x",
            )
            .unwrap();
        let p1 = store
            .record_payment(
                inv.id,
                Some(10_000),
                "a:1".into(),
                "manual".into(),
                Utc::now(),
            )
            .unwrap();
        assert_eq!(p1.status, InvoiceStatus::PartlyPaid);
        assert_eq!(p1.balance_minor(), 20_000);
        // Zero is refused without persisting; over-balance too.
        assert!(
            store
                .record_payment(inv.id, Some(0), "x".into(), "manual".into(), Utc::now())
                .is_err()
        );
        assert!(
            store
                .record_payment(
                    inv.id,
                    Some(20_001),
                    "x".into(),
                    "manual".into(),
                    Utc::now()
                )
                .is_err()
        );
        assert_eq!(
            store.get_invoice(inv.id).unwrap().unwrap().payments.len(),
            1
        );
        let p2 = store
            .record_payment(inv.id, None, "a:2".into(), "bank".into(), Utc::now())
            .unwrap();
        assert_eq!(p2.status, InvoiceStatus::Paid);
        assert_eq!(p2.balance_minor(), 0);
        assert_eq!(p2.payments.len(), 2);
        assert!(p2.paid_at.is_some(), "final settlement stamps paid_at");
    }

    #[test]
    fn write_off_is_final_and_keeps_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let mut inv = invoice();
        inv.total_minor = 5_000;
        let inv = store.create_invoice(inv).unwrap();
        store
            .issue_invoice(
                inv.id,
                Utc::now(),
                NaiveDate::from_ymd_opt(2026, 10, 21).unwrap(),
                b"%PDF x",
            )
            .unwrap();
        store
            .record_payment(
                inv.id,
                Some(2_000),
                "a:1".into(),
                "manual".into(),
                Utc::now(),
            )
            .unwrap();
        let wo = store
            .write_off_invoice(inv.id, "forgiven".into(), Utc::now())
            .unwrap();
        assert_eq!(wo.status, InvoiceStatus::WrittenOff);
        assert_eq!(wo.write_off_reason, "forgiven");
        assert_eq!(wo.payments.len(), 2 - 1, "ledger untouched by write-off");
        assert_eq!(wo.paid_minor(), 2_000);
        assert_eq!(wo.balance_minor(), 0, "forgiven balance is not outstanding");
        // Final: pay + re-write-off conflict.
        assert!(
            store
                .record_payment(inv.id, Some(100), "x".into(), "manual".into(), Utc::now())
                .is_err()
        );
        assert!(
            store
                .write_off_invoice(inv.id, "again".into(), Utc::now())
                .is_err()
        );
    }
}

#[cfg(test)]
mod timer_finish_tests {
    // #214: two concurrent POST /api/timer/stop calls must not both log time.
    use super::*;
    use crate::domain::{Entry, Hours, ProjectCode, Source};
    use chrono::{TimeZone, Utc};

    fn entry(id: Uuid, user: Uuid) -> Entry {
        Entry {
            id,
            date: NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
            customer_id: Uuid::new_v4(),
            user_id: Some(user),
            project_code: ProjectCode("P1".into()),
            task_code: None,
            hours: Hours(200),
            note: String::new(),
            billable: true,
            source: Source::Timer,
            created_at: Utc.with_ymd_and_hms(2026, 10, 2, 9, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 10, 2, 11, 0, 0).unwrap(),
        }
    }

    #[test]
    fn second_finish_timer_under_lock_is_refused_and_logs_one_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let user = Uuid::new_v4();
        let timer = Timer {
            user_id: user,
            customer_id: Uuid::new_v4(),
            project_code: ProjectCode("P1".into()),
            task_code: None,
            note: String::new(),
            started_at: Utc.with_ymd_and_hms(2026, 10, 2, 9, 0, 0).unwrap(),
        };
        store.put_timer(&timer).unwrap();

        // Two racing stops: both read the timer, both attempt to finish.
        let first = store.finish_timer(user, &entry(Uuid::new_v4(), user));
        let second = store.finish_timer(user, &entry(Uuid::new_v4(), user));
        assert!(first.is_ok(), "first stop logs the entry: {first:?}");
        assert!(
            matches!(second, Err(StoreError::NotFound)),
            "loser must be refused, never write a second entry: {second:?}"
        );
        assert_eq!(store.list_all_entries().unwrap().len(), 1);
        assert!(store.get_timer(user).unwrap().is_none());
    }
}
