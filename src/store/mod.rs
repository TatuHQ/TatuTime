//! File-based persistence. No database: every entity is one pretty-printed
//! JSON document on disk, mirroring the conceptual organisation of the data
//! (a customer is a folder that contains its projects; entries live in a
//! per-day folder). The API is the only actor that writes below the data dir.
//!
//! Path components are never taken raw from a request: customer files are
//! named by `Uuid`, project files by a validated `ProjectCode`, entry
//! directories by a validated `YYYY-MM-DD` date and entry files by `Uuid`.
//! Traversal input therefore cannot reach the filesystem.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use fs2::FileExt;
use uuid::Uuid;

use crate::auth::User;
use crate::domain::{
    Category, Customer, Expense, ExpenseClaim, Invoice, RecurringSchedule, Submission, Timer,
};

mod cold;
mod customers;
mod entries;
mod expenses;
mod index;
mod invoices;
mod io;
mod lock;
mod timer;
mod txn;
mod users;

use io::prune_tmp_files;

pub use cold::{ArchiveRefusal, ArchiveReport, ArchivedDay, SkippedDay};

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

/// The store's collection directories: one row per [`Entity`] impl plus the
/// non-`Entity` dirs [`ENTRIES_DIR`] (date-sharded), [`NOTIFICATIONS_DIR`] and
/// [`ARCHIVE_DIR`] (#349/#350), each row its own `DIR` constant.
///
/// The guard (`tests/contract/store.rs`, `storage_layout`) holds this list and
/// `AGENTS.md`'s storage bullet together both ways (#336); a collection lands
/// in both in one change. Not here: the flat side-car files (`audit.log`,
/// `revoked.json`, `scheduler.json`, `secrets.bin`, `config.json`,
/// `session.key`, `.server.lock`, `sync/accounting.json`, `invoices/.seq.json`
/// and the `*.idx.*` indexes).
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
}
