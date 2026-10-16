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
    Category, Customer, Entry, Expense, ExpenseClaim, Invoice, InvoiceStatus, Notification,
    PdfHint, Project, RecurringSchedule, Submission, Task, Timer,
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
/// (open invoices #8/#114 and submitted/approved submissions #16) and every
/// locked expense id (open invoices and submitted/approved reimbursement
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

/// The store's collection directories in one place: one row per [`Entity`]
/// impl, plus the two collections that are not flat `Entity` dirs
/// ([`ENTRIES_DIR`], date-sharded; [`NOTIFICATIONS_DIR`], per-user). Every row
/// is the impl's own `DIR` constant — never a re-typed literal — so this is the
/// layout as the store derives it; the store's path builders below use the same
/// constants for the two non-`Entity` collections.
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

    /// Generic JSON document read at a root-relative path (auxiliary state
    /// files like the accounting sync status, #33). `Ok(None)` when absent.
    pub fn read_json_rel<T: serde::de::DeserializeOwned>(
        &self,
        rel: &str,
    ) -> Result<Option<T>, StoreError> {
        read_json(&self.root.join(rel))
    }

    /// One-locked read-modify-write of a root-relative JSON document:
    /// `update` sees the current bytes and returns the replacement. Used for
    /// small ledgers where list-then-put across two locks would race (review
    /// D3/B4 — accounting sync records).
    pub fn update_json_rel<T, F>(&self, rel: &str, update: F) -> Result<T, StoreError>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
        F: FnOnce(Option<T>) -> T,
    {
        let _guard = self.write_lock()?;
        let path = self.root.join(rel);
        let current = read_json::<T>(&path)?;
        let next = update(current);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_json(&path, &next)?;
        Ok(next)
    }

    /// Generic atomic JSON write at a root-relative path, under the write lock.
    pub fn write_json_rel<T: serde::Serialize + ?Sized>(
        &self,
        rel: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_json(&path, value)
    }

    /// Override the per-collection document cap (used by tests; production uses
    /// the default or `TUCANO_MAX_DOCS`).
    pub fn with_max_docs(mut self, max: usize) -> Self {
        self.max_docs = max;
        self
    }

    // ------------------------------------------------ collection primitives --

    fn doc_dir<T: Entity>(&self) -> PathBuf {
        self.root.join(T::DIR)
    }

    fn doc_path<T: Entity>(&self, id: &str) -> PathBuf {
        self.doc_dir::<T>().join(format!("{id}.json"))
    }

    /// Read every document in a collection directory. A missing directory is
    /// an empty collection (it is created on first write); dot files and
    /// stranded `.tmp` files are not documents (`dir_entries` skips them).
    /// The caller owns the ordering.
    pub(crate) fn list_docs<T: Entity>(&self) -> Result<Vec<T>, StoreError> {
        self.walk_docs(&self.doc_dir::<T>(), read_json::<T>)
    }

    /// The shared `dir_entries -> json filter -> decode -> collect` walk,
    /// with an injectable decoder: nested collections (projects) plug their
    /// own legacy-aware decode hook here (`domain::project_from_bytes`).
    pub(crate) fn walk_docs<T>(
        &self,
        dir: &Path,
        decode: impl Fn(&Path) -> Result<Option<T>, StoreError>,
    ) -> Result<Vec<T>, StoreError> {
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for path in dir_entries(dir, self.max_docs)? {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(v) = decode(&path)? {
                out.push(v);
            }
        }
        Ok(out)
    }

    pub(crate) fn get_doc<T: Entity>(&self, id: &str) -> Result<Option<T>, StoreError> {
        read_json(&self.doc_path::<T>(id))
    }

    /// Lock, ensure the directory, atomic write — the one put path for
    /// flat collections.
    pub(crate) fn put_doc<T: Entity>(&self, value: &T) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        std::fs::create_dir_all(self.doc_dir::<T>())?;
        write_json(&self.doc_path::<T>(&value.id()), value)
    }

    /// Lock, exists → `NotFound`, remove — the one delete path. Guards that
    /// need to inspect other collections take `write_lock` themselves and
    /// reuse `remove_doc_locked`.
    pub(crate) fn remove_doc<T: Entity>(&self, id: &str) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        self.remove_doc_locked::<T>(id)
    }

    pub(crate) fn remove_doc_locked<T: Entity>(&self, id: &str) -> Result<(), StoreError> {
        let path = self.doc_path::<T>(id);
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }

    // ---------------------------------------------------------- side indexes --
    //
    // Two full scans had crept onto hot paths (review D3): payment webhooks
    // resolve invoices by human number, and login/SSO resolve users by email.
    // Both are dot-file maps maintained under the same write lock as the
    // documents they index — crash-safe tmp+rename via `write_json`, skipped
    // by `dir_entries` so an index is never mistaken for a document. A
    // missing or stale index never loses data: reads verify the fast path
    // against the document itself, fall back to the old scan, and self-heal.

    fn invoice_index_path(&self) -> PathBuf {
        self.root.join("invoices").join(".idx.numbers.json")
    }

    fn email_index_path(&self) -> PathBuf {
        self.root.join("users").join(".idx.emails.json")
    }

    /// Map `number -> id` for one invoice. The caller holds the write lock.
    fn index_invoice_locked(&self, invoice: &Invoice) -> Result<(), StoreError> {
        let path = self.invoice_index_path();
        let mut idx: HashMap<String, String> = read_json(&path)?.unwrap_or_default();
        let id = invoice.id.to_string();
        idx.retain(|_, v| v != &id);
        idx.insert(invoice.number.clone(), id);
        write_json(&path, &idx)
    }

    /// Drop a deleted invoice from the index (its number is never recycled,
    /// review B3 — the entry simply disappears with the document).
    fn unindex_invoice_locked(&self, id: &str) -> Result<(), StoreError> {
        let path = self.invoice_index_path();
        if let Some(mut idx) = read_json::<HashMap<String, String>>(&path)? {
            idx.retain(|_, v| v != id);
            write_json(&path, &idx)?;
        }
        Ok(())
    }

    /// Rebuild the whole number index from the documents (self-heal after a
    /// pre-index data dir or detected drift). Duplicate numbers (a restored
    /// file) resolve to the earliest-created invoice, matching the scan.
    fn rebuild_invoice_index(&self) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let mut best: HashMap<String, (String, chrono::DateTime<chrono::Utc>)> = HashMap::new();
        for inv in self.list_invoices()? {
            match best.get(&inv.number) {
                Some((_, at)) if *at <= inv.created_at => {}
                _ => {
                    best.insert(inv.number.clone(), (inv.id.to_string(), inv.created_at));
                }
            }
        }
        let idx: HashMap<String, String> = best
            .into_iter()
            .map(|(number, (id, _))| (number, id))
            .collect();
        std::fs::create_dir_all(self.doc_dir::<Invoice>())?;
        write_json(&self.invoice_index_path(), &idx)
    }

    /// Map `email(lower) -> id` for one user. The caller holds the write lock.
    fn index_user_locked(&self, user: &User) -> Result<(), StoreError> {
        let path = self.email_index_path();
        let mut idx: HashMap<String, String> = read_json(&path)?.unwrap_or_default();
        let id = user.id.to_string();
        idx.retain(|_, v| v != &id);
        idx.insert(user.email.to_lowercase(), id);
        write_json(&path, &idx)
    }

    fn unindex_user_locked(&self, id: &str) -> Result<(), StoreError> {
        let path = self.email_index_path();
        if let Some(mut idx) = read_json::<HashMap<String, String>>(&path)? {
            idx.retain(|_, v| v != id);
            write_json(&path, &idx)?;
        }
        Ok(())
    }

    fn rebuild_email_index(&self) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let mut idx: HashMap<String, String> = HashMap::new();
        for u in self.list_users()? {
            // list_users is name-sorted; the scan's first match wins, so keep
            // the first insertion per address.
            idx.entry(u.email.to_lowercase())
                .or_insert_with(|| u.id.to_string());
        }
        std::fs::create_dir_all(self.doc_dir::<User>())?;
        write_json(&self.email_index_path(), &idx)
    }

    // ------------------------------------------- per-user read index (#335) --
    //
    // A member's day/week read used to decode *every* user's entry documents
    // for those days (and every expense) and filter afterwards through
    // `visible_to` — an O(all rows) cost for an O(own rows) answer (review
    // §2.2). The information needed to skip that work is already on the
    // documents (`user_id`, #51), so it is now indexed, in the shape of the two
    // dot-file side indexes above:
    //
    //   entries/.idx.users.json    { "<YYYY-MM-DD>": { "<user>": ["<id>", …] } }
    //   expenses/.idx.users.json   { "<user>": ["<id>", …] }
    //
    // `<user>` is the owner's `Uuid`, or `""` for a legacy/unattributed row
    // (`user_id: None`, which a member can never see). Both are dot files, so
    // `dir_entries` skips them: they are **not** documents — never counted
    // toward `MAX_DOCS`, never returned by a listing — and a crash-stranded
    // `*.tmp` is pruned at open. Each is written under the same `write_lock` as
    // the document it files.
    //
    // **Trust rule — the index is never the source of truth.** It is used only
    // while it is *provably* current: the id set it holds for a collection must
    // equal the id set of the `<id>.json` documents actually on disk (a
    // directory listing — names only, no decode). That equality is exact and
    // strictly stronger than the #255 mtime/count stamp: a document appended,
    // removed or *replaced* behind the index's back all break set equality and
    // force a scan, so a stale or truncated index can only ever be slower, never
    // wrong. A missing or unreadable (corrupt) index reads as "no index" and
    // takes the same path. On the fallback the caller scans the authoritative
    // collection and rewrites the index while it holds the lock, healing it.
    // The *mapping* (which id belongs to which user) rests, as for the existing
    // indexes, on the API being the only writer and on `user_id` being immutable
    // for a document; the store writes both under one lock, so a crash can only
    // strand a stale index — a state the set-equality check already refuses.

    fn entry_index_path(&self) -> PathBuf {
        self.root.join(ENTRIES_DIR).join(".idx.users.json")
    }

    fn expense_index_path(&self) -> PathBuf {
        self.root.join("expenses").join(".idx.users.json")
    }

    /// Lenient read: a missing or unreadable index is simply "no index" — the
    /// caller falls back to the authoritative scan and rebuilds it.
    fn read_entry_index(&self) -> Option<EntryUserIndex> {
        let bytes = std::fs::read(self.entry_index_path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn read_expense_index(&self) -> Option<ExpenseUserIndex> {
        let bytes = std::fs::read(self.expense_index_path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// The document ids a day folder holds, from its listing alone (no decode).
    fn entry_day_disk_ids(
        &self,
        date: NaiveDate,
    ) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let dir = self.day_dir(date);
        if !dir.exists() {
            return Ok(std::collections::BTreeSet::new());
        }
        Ok(json_stems(&dir_entries(&dir, self.max_docs)?))
    }

    fn expense_disk_ids(&self) -> Result<std::collections::BTreeSet<String>, StoreError> {
        let dir = self.doc_dir::<Expense>();
        if !dir.exists() {
            return Ok(std::collections::BTreeSet::new());
        }
        Ok(json_stems(&dir_entries(&dir, self.max_docs)?))
    }

    /// One entry document decode, counted for the #335 read-cost test.
    fn read_entry(&self, path: &Path) -> Result<Option<Entry>, StoreError> {
        #[cfg(test)]
        self.doc_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        read_json::<Entry>(path)
    }

    /// One expense document decode, counted for the #335 read-cost test.
    fn read_expense(&self, path: &Path) -> Result<Option<Expense>, StoreError> {
        #[cfg(test)]
        self.doc_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        read_json::<Expense>(path)
    }

    /// #335: entity documents decoded so far (test-only measurement seam).
    #[cfg(test)]
    fn doc_reads(&self) -> u64 {
        self.doc_reads.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The ids the entry index files under `<date>`, **iff** its id set equals
    /// the day folder's. `None` means "not provably current" (absent, corrupt
    /// or drifted) and the caller must scan.
    fn entry_index_day_ids(
        &self,
        idx: &EntryUserIndex,
        date: NaiveDate,
    ) -> Result<Option<HashMap<String, Vec<String>>>, StoreError> {
        let key = date.format("%Y-%m-%d").to_string();
        let day = idx.days.get(&key).cloned().unwrap_or_default();
        let indexed: std::collections::BTreeSet<String> = day.values().flatten().cloned().collect();
        if indexed == self.entry_day_disk_ids(date)? {
            Ok(Some(day))
        } else {
            Ok(None)
        }
    }

    /// A member's expense ids, **iff** the index's id set equals the collection's.
    fn expense_index_ids_if_current(&self, key: &str) -> Result<Option<Vec<String>>, StoreError> {
        let Some(idx) = self.read_expense_index() else {
            return Ok(None);
        };
        let indexed: std::collections::BTreeSet<String> =
            idx.users.values().flatten().cloned().collect();
        if indexed == self.expense_disk_ids()? {
            Ok(Some(idx.users.get(key).cloned().unwrap_or_default()))
        } else {
            Ok(None)
        }
    }

    /// Read exactly `ids` from the day folder, refusing (returning `None`) if
    /// any is missing or is not `user_id`'s — either means the index mis-filed
    /// a row, so the caller must not trust it for this read.
    fn read_entries_by_ids(
        &self,
        date: NaiveDate,
        ids: &[String],
        user_id: Uuid,
    ) -> Result<Option<Vec<Entry>>, StoreError> {
        let mut out = Vec::with_capacity(ids.len());
        for raw in ids {
            let Ok(id) = Uuid::parse_str(raw) else {
                return Ok(None);
            };
            match self.read_entry(&self.entry_path(date, id))? {
                Some(e) if e.user_id == Some(user_id) => out.push(e),
                _ => return Ok(None),
            }
        }
        Ok(Some(out))
    }

    fn read_expenses_by_ids(
        &self,
        ids: &[String],
        user_id: Uuid,
    ) -> Result<Option<Vec<Expense>>, StoreError> {
        let mut out = Vec::with_capacity(ids.len());
        for raw in ids {
            let Ok(id) = Uuid::parse_str(raw) else {
                return Ok(None);
            };
            match self.read_expense(&self.doc_path::<Expense>(&id.to_string()))? {
                Some(e) if e.user_id == Some(user_id) => out.push(e),
                _ => return Ok(None),
            }
        }
        Ok(Some(out))
    }

    /// File `entry` under its day and owner. The caller holds the write lock.
    fn index_entry_locked(&self, entry: &Entry) -> Result<(), StoreError> {
        let path = self.entry_index_path();
        let mut idx = self.read_entry_index().unwrap_or_default();
        let day = idx
            .days
            .entry(entry.date.format("%Y-%m-%d").to_string())
            .or_default();
        let id = entry.id.to_string();
        // Re-file: drop any prior owner for this id, then add the owner.
        for ids in day.values_mut() {
            ids.retain(|x| x != &id);
        }
        day.entry(index_user_key(entry.user_id))
            .or_default()
            .push(id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_json(&path, &idx)
    }

    /// Drop `id` from `date`'s slice (a delete, or the old side of a move). The
    /// caller holds the write lock.
    fn unindex_entry_locked(&self, date: NaiveDate, id: Uuid) -> Result<(), StoreError> {
        let path = self.entry_index_path();
        let Some(mut idx) = self.read_entry_index() else {
            return Ok(());
        };
        let key = date.format("%Y-%m-%d").to_string();
        let id = id.to_string();
        if let Some(day) = idx.days.get_mut(&key) {
            for ids in day.values_mut() {
                ids.retain(|x| x != &id);
            }
            day.retain(|_, ids| !ids.is_empty());
            if day.is_empty() {
                idx.days.remove(&key);
            }
        }
        write_json(&path, &idx)
    }

    /// Rewrite one day's slice from the scan that just ran (self-heal). The
    /// caller holds the write lock.
    fn write_entry_day_index_locked(
        &self,
        date: NaiveDate,
        docs: &[Entry],
    ) -> Result<(), StoreError> {
        let path = self.entry_index_path();
        let mut idx = self.read_entry_index().unwrap_or_default();
        let key = date.format("%Y-%m-%d").to_string();
        if docs.is_empty() {
            idx.days.remove(&key);
        } else {
            idx.days.insert(key, group_entries_by_user(docs));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_json(&path, &idx)
    }

    fn index_expense_locked(&self, expense: &Expense) -> Result<(), StoreError> {
        let path = self.expense_index_path();
        let mut idx = self.read_expense_index().unwrap_or_default();
        let id = expense.id.to_string();
        for ids in idx.users.values_mut() {
            ids.retain(|x| x != &id);
        }
        idx.users
            .entry(index_user_key(expense.user_id))
            .or_default()
            .push(id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_json(&path, &idx)
    }

    fn unindex_expense_locked(&self, id: &str) -> Result<(), StoreError> {
        let path = self.expense_index_path();
        let Some(mut idx) = self.read_expense_index() else {
            return Ok(());
        };
        for ids in idx.users.values_mut() {
            ids.retain(|x| x != id);
        }
        idx.users.retain(|_, ids| !ids.is_empty());
        write_json(&path, &idx)
    }

    fn write_expense_index_locked(&self, docs: &[Expense]) -> Result<(), StoreError> {
        let path = self.expense_index_path();
        let mut idx = ExpenseUserIndex::default();
        for e in docs {
            idx.users
                .entry(index_user_key(e.user_id))
                .or_default()
                .push(e.id.to_string());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_json(&path, &idx)
    }

    /// Scan a day and rewrite its index slice in one locked step (the fallback
    /// path). Takes the write lock, so it must run on the blocking pool.
    fn scan_and_index_entry_day(&self, date: NaiveDate) -> Result<Vec<Entry>, StoreError> {
        let _guard = self.write_lock()?;
        let docs = self.walk_docs(&self.day_dir(date), |p| self.read_entry(p))?;
        self.write_entry_day_index_locked(date, &docs)?;
        Ok(docs)
    }

    /// Scan the expense collection and rewrite its index in one locked step.
    fn scan_and_index_expenses(&self) -> Result<Vec<Expense>, StoreError> {
        let _guard = self.write_lock()?;
        let docs = self.walk_docs(&self.doc_dir::<Expense>(), |p| self.read_expense(p))?;
        self.write_expense_index_locked(&docs)?;
        Ok(docs)
    }

    /// Rebuild **both** per-user side indexes from the documents (#335).
    /// Idempotent: this is the one-time migration of a data dir written before
    /// the index existed (run by [`Store::open`] when the index is absent) and a
    /// maintenance primitive. Takes the write lock once for the whole pass.
    pub fn rebuild_read_indexes(&self) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let mut idx = EntryUserIndex::default();
        let entries_dir = self.root.join(ENTRIES_DIR);
        if entries_dir.exists() {
            for day in dir_entries(&entries_dir, self.max_docs)? {
                if !day.is_dir() {
                    continue;
                }
                let Some(name) = day.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let docs = self.walk_docs(&day, |p| self.read_entry(p))?;
                if !docs.is_empty() {
                    idx.days
                        .insert(name.to_string(), group_entries_by_user(&docs));
                }
            }
        }
        if let Some(parent) = self.entry_index_path().parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_json(&self.entry_index_path(), &idx)?;
        let expenses = self.walk_docs(&self.doc_dir::<Expense>(), |p| self.read_expense(p))?;
        self.write_expense_index_locked(&expenses)
    }

    /// A member's own entries for one day (#335): the indexed fast path decodes
    /// only their rows; anything untrustworthy falls back to the day scan (and
    /// heals the index). Order matches [`Store::list_by_date`] exactly.
    pub fn list_by_date_for_user(
        &self,
        date: NaiveDate,
        user_id: Uuid,
    ) -> Result<Vec<Entry>, StoreError> {
        let idx = self.read_entry_index();
        self.list_entry_day_for_user(date, user_id, idx.as_ref())
    }

    /// The range counterpart; the index is read once for the whole walk. Order
    /// matches [`Store::list_range`].
    pub fn list_range_for_user(
        &self,
        from: NaiveDate,
        to: NaiveDate,
        user_id: Uuid,
    ) -> Result<Vec<Entry>, StoreError> {
        if (to - from).num_days() > MAX_RANGE_DAYS {
            return Err(StoreError::RangeTooLarge);
        }
        let idx = self.read_entry_index();
        let mut out = Vec::new();
        let mut date = from;
        while date <= to {
            out.extend(self.list_entry_day_for_user(date, user_id, idx.as_ref())?);
            date = date
                .succ_opt()
                .ok_or_else(|| StoreError::Io("date range overflow".into()))?;
        }
        out.sort_by_key(|e| (e.date, e.created_at));
        Ok(out)
    }

    fn list_entry_day_for_user(
        &self,
        date: NaiveDate,
        user_id: Uuid,
        idx: Option<&EntryUserIndex>,
    ) -> Result<Vec<Entry>, StoreError> {
        if let Some(idx) = idx
            && let Some(day) = self.entry_index_day_ids(idx, date)?
        {
            let ids = day
                .get(&index_user_key(Some(user_id)))
                .cloned()
                .unwrap_or_default();
            if let Some(mut own) = self.read_entries_by_ids(date, &ids, user_id)? {
                sort_entries_by_created(&mut own);
                return Ok(own);
            }
        }
        // No trustworthy index: scan the day, heal the index under the lock,
        // filter. The scan is authoritative, so this answer is always correct.
        let all = self.scan_and_index_entry_day(date)?;
        let mut own: Vec<Entry> = all
            .into_iter()
            .filter(|e| e.user_id == Some(user_id))
            .collect();
        sort_entries_by_created(&mut own);
        Ok(own)
    }

    /// A member's own expenses (#335): the index answers when it provably
    /// covers the collection; otherwise the scan runs and heals it. Order
    /// matches [`Store::list_expenses`].
    pub fn list_expenses_for_user(&self, user_id: Uuid) -> Result<Vec<Expense>, StoreError> {
        let key = index_user_key(Some(user_id));
        if let Some(ids) = self.expense_index_ids_if_current(&key)?
            && let Some(mut own) = self.read_expenses_by_ids(&ids, user_id)?
        {
            own.sort_by_key(|e| std::cmp::Reverse((e.date, e.created_at)));
            return Ok(own);
        }
        let all = self.scan_and_index_expenses()?;
        let mut own: Vec<Expense> = all
            .into_iter()
            .filter(|e| e.user_id == Some(user_id))
            .collect();
        own.sort_by_key(|e| std::cmp::Reverse((e.date, e.created_at)));
        Ok(own)
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
            // Only *open* invoices freeze their records: settling (paid) or
            // writing off releases them (#8/#114).
            if !inv.status.is_open() {
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

    pub fn list_by_date(&self, date: NaiveDate) -> Result<Vec<Entry>, StoreError> {
        let mut out = self.walk_docs(&self.day_dir(date), |p| self.read_entry(p))?;
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

    /// Entries are addressable by id alone, but stored under their date, so
    /// this walks every day folder. NOTE (#190): there is no `MAX_RANGE_DAYS`
    /// bound here — the earlier comment claimed one but the body never
    /// implemented it. Cost is O(days) directory probes, not O(entries).
    pub fn get_entry(&self, id: Uuid) -> Result<Option<Entry>, StoreError> {
        let dir = self.root.join(ENTRIES_DIR);
        if !dir.exists() {
            return Ok(None);
        }
        for day in dir_entries(&dir, self.max_docs)? {
            let path = day.join(format!("{id}.json"));
            if let Some(e) = read_json::<Entry>(&path)? {
                return Ok(Some(e));
            }
        }
        Ok(None)
    }

    pub fn put_entry(&self, entry: &Entry) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        std::fs::create_dir_all(self.day_dir(entry.date))?;
        write_json(&self.entry_path(entry.date, entry.id), entry)?;
        self.index_entry_locked(entry)
    }

    pub fn delete_entry(&self, entry: &Entry) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.entry_path(entry.date, entry.id);
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        std::fs::remove_file(&path)?;
        self.unindex_entry_locked(entry.date, entry.id)
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
        let dir = self.root.join(ENTRIES_DIR);
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for day in dir_entries(&dir, self.max_docs)? {
            if day.is_dir() {
                for path in dir_entries(&day, self.max_docs)? {
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    if out.len() >= self.max_docs {
                        return Err(StoreError::TooManyItems);
                    }
                    if let Some(e) = read_json::<Entry>(&path)? {
                        out.push(e);
                    }
                }
            }
        }
        Ok(out)
    }
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

    /// Webhook-safe payment record (#185): the reference-uniqueness check
    /// runs **inside** the write lock, so two concurrent deliveries of the
    /// same provider event serialize and exactly one posts. `Ok(None)` means
    /// the reference was already recorded — callers treat that as success
    /// (idempotent replay), not an error.
    pub fn record_payment_once(
        &self,
        id: Uuid,
        amount: Option<u64>,
        reference: String,
        method: String,
        now: DateTime<Utc>,
    ) -> Result<Option<Invoice>, StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Invoice>(&id.to_string());
        let Some(invoice) = read_json::<Invoice>(&path)? else {
            return Err(StoreError::NotFound);
        };
        if invoice.payments.iter().any(|p| p.reference == reference) {
            return Ok(None);
        }
        self.record_payment_locked(id, amount, reference, method, now)
            .map(Some)
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
            let path = s.entry_path(entry.date, entry.id);
            if !path.exists() {
                return Err(StoreError::NotFound);
            }
            std::fs::remove_file(&path).map_err(StoreError::from)?;
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
    /// transaction (#18/#218): an expense referenced by an open invoice or a
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
        if let Some(old_date) = existing_date
            && old_date != updated.date
        {
            let old = self.entry_path(old_date, updated.id);
            if old.exists() {
                std::fs::remove_file(old)?;
            }
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

/// Directory modification time, or `None` when the directory is absent (a
/// missing collection is empty; creating it later changes the stamp).
fn dir_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

fn prune_tmp_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".tmp"))
            && let Ok(md) = p.metadata()
            && md.is_file()
        {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Read a directory and return its document paths, bounded by `max` (#50).
/// Sorting by name keeps listings stable across platforms. Dot files (state
/// such as `invoices/.seq.json`) and crashed-write `*.tmp` leftovers (review
/// D6) are **not** documents: they are skipped here and pruned at open.
fn dir_entries(dir: &Path, max: usize) -> Result<Vec<PathBuf>, StoreError> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            !(name.starts_with('.') || name.ends_with(".tmp"))
        })
        .collect();
    if paths.len() > max {
        return Err(StoreError::TooManyItems);
    }
    paths.sort();
    Ok(paths)
}

/// The `<id>` stems of every `.json` document among `paths` (a directory
/// listing), ignoring anything else the folder holds.
fn json_stems(paths: &[PathBuf]) -> std::collections::BTreeSet<String> {
    paths
        .iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .map(str::to_string)
        .collect()
}

/// The per-user entry index (#335): `date -> (owner -> ids)`, holding **every**
/// document in the day so a member read can prove its slice complete against
/// the day folder's own listing. A dot file, never a document.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct EntryUserIndex {
    #[serde(default)]
    days: HashMap<String, HashMap<String, Vec<String>>>,
}

/// The per-user expense index (#335): `owner -> ids` over the flat `expenses/`
/// collection (same completeness rule as [`Store::entry_day_disk_ids`]).
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct ExpenseUserIndex {
    #[serde(default)]
    users: HashMap<String, Vec<String>>,
}

/// The index key for an owner: the `Uuid`, or `""` for a legacy `None`.
fn index_user_key(owner: Option<Uuid>) -> String {
    owner.map(|u| u.to_string()).unwrap_or_default()
}

/// `owner -> ids` for one day's entries — the shape [`EntryUserIndex`] stores.
fn group_entries_by_user(docs: &[Entry]) -> HashMap<String, Vec<String>> {
    let mut day: HashMap<String, Vec<String>> = HashMap::new();
    for e in docs {
        day.entry(index_user_key(e.user_id))
            .or_default()
            .push(e.id.to_string());
    }
    day
}

/// The order [`Store::list_by_date`] documents, shared so the indexed member
/// path cannot drift from the scan.
fn sort_entries_by_created(out: &mut [Entry]) {
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
}

fn read_json<T>(path: &Path) -> Result<Option<T>, StoreError>
where
    T: serde::de::DeserializeOwned,
{
    match std::fs::read(path) {
        Ok(bytes) => {
            let value = serde_json::from_slice(&bytes)
                .map_err(|e| StoreError::Io(format!("corrupt document {}: {e}", path.display())))?;
            Ok(Some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_json<T>(path: &Path, value: &T) -> Result<(), StoreError>
where
    T: serde::Serialize + ?Sized,
{
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| StoreError::Io(format!("serialisation failed: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Binary sibling of `write_json` (#113): same atomic tmp + rename discipline
/// under the same writer lock; stranded `.tmp` files are pruned on open.
fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let tmp = path.with_extension("pdf.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

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
            .pay_invoice(inv.id, Utc::now(), "stripe:1".into())
            .unwrap();
        assert_eq!(paid.status, InvoiceStatus::Paid);
        assert_eq!(paid.payment_reference, "stripe:1");
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

    #[test]
    fn update_json_rel_is_locked_rmw() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let out: Vec<u32> = store
            .update_json_rel("counter.json", |cur: Option<Vec<u32>>| {
                let mut v = cur.unwrap_or_default();
                v.push(v.len() as u32 + 1);
                v
            })
            .unwrap();
        assert_eq!(out, vec![1]);
        let out = store
            .update_json_rel("counter.json", |cur: Option<Vec<u32>>| {
                let mut v = cur.unwrap_or_default();
                v.push(v.len() as u32 + 1);
                v
            })
            .unwrap();
        assert_eq!(out, vec![1, 2]);
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use crate::domain::{Invoice, InvoiceStatus};
    use chrono::TimeZone;

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

    fn draft() -> Invoice {
        Invoice {
            id: Uuid::new_v4(),
            number: String::new(),
            customer_id: Uuid::new_v4(),
            currency: crate::domain::Currency("EUR".into()),
            period_from: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            period_to: NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
            lines: vec![],
            total_minor: 0,
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

    #[test]
    fn email_index_serves_lookup_and_self_heals() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let u = user("someone@Example.ORG");
        store.put_user(&u).unwrap();
        // Fast path (also case-insensitive).
        assert_eq!(
            store
                .get_user_by_email("someone@example.org")
                .unwrap()
                .unwrap()
                .id,
            u.id
        );
        // Simulate a pre-index data dir: the scan still finds the user and
        // the index is rebuilt.
        std::fs::remove_file(store.root.join("users").join(".idx.emails.json")).unwrap();
        assert_eq!(
            store
                .get_user_by_email("someone@example.org")
                .unwrap()
                .unwrap()
                .id,
            u.id
        );
        assert!(store.root.join("users").join(".idx.emails.json").exists());
        // Delete unindexes; unknown email stays None without rebuilding churn.
        store.delete_user(u.id).unwrap();
        assert!(
            store
                .get_user_by_email("someone@example.org")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn invoice_number_index_find_create_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let a = store.create_invoice(draft()).unwrap();
        assert_eq!(a.number, "INV-0001");
        assert_eq!(
            store
                .find_invoice_by_number("INV-0001")
                .unwrap()
                .unwrap()
                .id,
            a.id
        );
        assert!(store.find_invoice_by_number("INV-0002").unwrap().is_none());
        // The index dot file is never mistaken for an invoice document.
        assert_eq!(store.list_invoices().unwrap().len(), 1);
        // Delete drops the mapping; a deleted number is not recycled anyway.
        store.delete_invoice(a.id).unwrap();
        assert!(store.find_invoice_by_number("INV-0001").unwrap().is_none());
        // Pre-index directory shape: rebuild happens on first hit.
        let b = store.create_invoice(draft()).unwrap();
        std::fs::remove_file(store.root.join("invoices").join(".idx.numbers.json")).unwrap();
        assert_eq!(
            store.find_invoice_by_number(&b.number).unwrap().unwrap().id,
            b.id
        );
        assert!(
            store
                .root
                .join("invoices")
                .join(".idx.numbers.json")
                .exists()
        );
    }

    #[test]
    fn bootstrap_indexes_the_first_admin() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        store.put_user_if_none(&user("admin@x.co")).unwrap();
        assert!(store.get_user_by_email("admin@x.co").unwrap().is_some());
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

#[cfg(test)]
mod per_user_index_tests {
    // #335: the per-user entry/expense index must never lose a row — a stale,
    // missing, corrupt or truncated index has to answer exactly as the scan
    // does, and heal. These drive each drift shape directly.
    use super::*;
    use crate::domain::{Entry, Hours, ProjectCode, Source};
    use chrono::TimeZone;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 2).unwrap()
    }

    fn other_day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 3).unwrap()
    }

    fn entry_at(user: Option<Uuid>, seq: u32, date: NaiveDate) -> Entry {
        let at = Utc.with_ymd_and_hms(2026, 10, 2, 9, 0, seq).unwrap();
        Entry {
            id: Uuid::new_v4(),
            date,
            customer_id: Uuid::new_v4(),
            user_id: user,
            project_code: ProjectCode("P1".into()),
            task_code: None,
            hours: Hours(100),
            note: String::new(),
            billable: true,
            source: Source::Manual,
            created_at: at,
            updated_at: at,
        }
    }

    fn expense(user: Option<Uuid>) -> Expense {
        let at = Utc.with_ymd_and_hms(2026, 10, 2, 9, 0, 0).unwrap();
        Expense {
            id: Uuid::new_v4(),
            date: day(),
            customer_id: Uuid::new_v4(),
            user_id: user,
            project_code: None,
            category_id: None,
            amount_minor: 100,
            currency: crate::domain::Currency("EUR".into()),
            billable: false,
            note: String::new(),
            original_amount_minor: None,
            original_currency: None,
            exchange_rate: None,
            exchange_rate_date: None,
            receipt_name: None,
            receipt_b64: None,
            created_at: at,
            updated_at: at,
        }
    }

    fn entry_index_path(store: &Store) -> PathBuf {
        store.root().join("entries").join(".idx.users.json")
    }

    fn expense_index_path(store: &Store) -> PathBuf {
        store.root().join("expenses").join(".idx.users.json")
    }

    fn ids(entries: Vec<Entry>) -> Vec<Uuid> {
        entries.into_iter().map(|e| e.id).collect()
    }

    #[test]
    fn entry_index_answers_the_member_and_heals_from_any_drift() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();

        let a1 = entry_at(Some(alice), 1, day());
        let a2 = entry_at(Some(alice), 2, day());
        store.put_entry(&a1).unwrap();
        store.put_entry(&a2).unwrap();
        store.put_entry(&entry_at(Some(bob), 3, day())).unwrap();
        // A legacy unattributed row: never visible to a member (#51).
        store.put_entry(&entry_at(None, 4, day())).unwrap();

        // Fast path: exactly Alice's rows, in the store's own order.
        assert_eq!(
            ids(store.list_by_date_for_user(day(), alice).unwrap()),
            vec![a1.id, a2.id]
        );

        // Missing index: the scan answers and the index is rebuilt.
        std::fs::remove_file(entry_index_path(&store)).unwrap();
        assert_eq!(
            ids(store.list_by_date_for_user(day(), alice).unwrap()),
            vec![a1.id, a2.id]
        );
        assert!(entry_index_path(&store).exists(), "index healed");

        // Corrupt index: same answer, healed to a parseable file.
        std::fs::write(entry_index_path(&store), b"{ not json").unwrap();
        assert_eq!(
            ids(store.list_by_date_for_user(day(), alice).unwrap()),
            vec![a1.id, a2.id]
        );
        assert!(store.read_entry_index().is_some(), "index healed");

        // A document appended *behind the index's back* — the file lands in the
        // day folder with no index update. The set-equality check must notice
        // and the read must still be complete.
        let late = entry_at(Some(alice), 5, day());
        std::fs::write(
            store.entry_path(day(), late.id),
            serde_json::to_vec(&late).unwrap(),
        )
        .unwrap();
        assert_eq!(
            ids(store.list_by_date_for_user(day(), alice).unwrap()),
            vec![a1.id, a2.id, late.id]
        );
        // Healed: the index now names the late row for Alice.
        let idx = store.read_entry_index().unwrap();
        let healed = store
            .entry_index_day_ids(&idx, day())
            .unwrap()
            .expect("index is current after the healing read");
        assert!(healed[&alice.to_string()].contains(&late.id.to_string()));
    }

    #[test]
    fn member_day_read_decodes_only_their_own_rows() {
        // The measured claim: with N users and K rows each, the shared scan
        // decodes N*K documents while the indexed member read decodes K.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        const USERS: usize = 5;
        const PER_USER: usize = 3;
        let users: Vec<Uuid> = (0..USERS).map(|_| Uuid::new_v4()).collect();
        for (u, user) in users.iter().enumerate() {
            for k in 0..PER_USER {
                store
                    .put_entry(&entry_at(Some(*user), (u * PER_USER + k) as u32 + 1, day()))
                    .unwrap();
            }
        }

        let before = store.doc_reads();
        let all = store.list_by_date(day()).unwrap();
        let scanned = store.doc_reads() - before;
        assert_eq!(all.len(), USERS * PER_USER);
        assert_eq!(scanned, (USERS * PER_USER) as u64, "scan decodes N*K");

        let before = store.doc_reads();
        let own = store.list_by_date_for_user(day(), users[0]).unwrap();
        let indexed = store.doc_reads() - before;
        assert_eq!(own.len(), PER_USER);
        assert_eq!(indexed, PER_USER as u64, "indexed read decodes K");
        assert_eq!(
            scanned / indexed,
            USERS as u64,
            "one decode per user avoided"
        );
    }

    #[test]
    fn member_read_never_decodes_an_unreadable_sibling_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let mine = entry_at(Some(alice), 1, day());
        store.put_entry(&mine).unwrap();
        let theirs = entry_at(Some(bob), 2, day());
        store.put_entry(&theirs).unwrap();

        // Bob's document is replaced with garbage — the listing still names it,
        // so the index stays current for the id set, yet decoding it would fail.
        // The old "decode the whole collection" member read would 500 here.
        std::fs::write(store.entry_path(day(), theirs.id), b"{ not json").unwrap();

        let own = store.list_by_date_for_user(day(), alice).unwrap();
        assert_eq!(ids(own), vec![mine.id]);
        // The scan path (the admin answer) does read it, and surfaces the fault.
        assert!(
            store.list_by_date(day()).is_err(),
            "the full scan decodes the sibling and fails"
        );
    }

    #[test]
    fn moving_an_entry_between_days_reindexes_both_and_delete_unindexes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let alice = Uuid::new_v4();
        let mut e = entry_at(Some(alice), 1, day());
        store.put_entry(&e).unwrap();

        e.date = other_day();
        store.save_entry(Some(day()), &e).unwrap();
        assert!(
            store
                .list_by_date_for_user(day(), alice)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .list_by_date_for_user(other_day(), alice)
                .unwrap()
                .len(),
            1
        );

        store.delete_entry(&e).unwrap();
        assert!(
            store
                .list_by_date_for_user(other_day(), alice)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.list_by_date(other_day()).unwrap().len(), 0);
    }

    #[test]
    fn expense_index_answers_the_member_and_heals() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let a = expense(Some(alice));
        store.put_expense(&a).unwrap();
        store.put_expense(&expense(Some(bob))).unwrap();

        assert_eq!(store.list_expenses_for_user(alice).unwrap().len(), 1);
        // Missing index: the scan answers and heals.
        std::fs::remove_file(expense_index_path(&store)).unwrap();
        assert_eq!(store.list_expenses_for_user(alice).unwrap()[0].id, a.id);
        assert!(expense_index_path(&store).exists(), "index healed");
        // Corrupt index: same answer, healed.
        std::fs::write(expense_index_path(&store), b"[").unwrap();
        assert_eq!(store.list_expenses_for_user(alice).unwrap()[0].id, a.id);
        // Delete unindexes the row.
        store.delete_expense(a.id).unwrap();
        assert!(store.list_expenses_for_user(alice).unwrap().is_empty());
    }

    #[test]
    fn open_migrates_a_pre_index_data_dir() {
        // A data dir written before #335 exists (documents but no index); the
        // one-time migration at `Store::open` must build both indexes so the
        // first member read is already served from them.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let alice = Uuid::new_v4();
        let e = entry_at(Some(alice), 1, day());
        let day_dir = root.join("entries").join("2026-10-02");
        std::fs::create_dir_all(&day_dir).unwrap();
        std::fs::write(
            day_dir.join(format!("{}.json", e.id)),
            serde_json::to_vec(&e).unwrap(),
        )
        .unwrap();
        let exp = expense(Some(alice));
        std::fs::create_dir_all(root.join("expenses")).unwrap();
        std::fs::write(
            root.join("expenses").join(format!("{}.json", exp.id)),
            serde_json::to_vec(&exp).unwrap(),
        )
        .unwrap();
        assert!(!root.join("entries").join(".idx.users.json").exists());

        let store = Store::open(&root).unwrap();
        assert!(
            entry_index_path(&store).exists(),
            "entries index migrated at open"
        );
        assert!(
            expense_index_path(&store).exists(),
            "expense index migrated at open"
        );
        assert_eq!(
            ids(store.list_by_date_for_user(day(), alice).unwrap()),
            vec![e.id]
        );
        assert_eq!(store.list_expenses_for_user(alice).unwrap()[0].id, exp.id);
    }
}
