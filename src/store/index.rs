use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use uuid::Uuid;

use super::io::{dir_entries, json_stems, read_json, write_json};
use super::{ARCHIVE_DIR, ColdIdIndex, ENTRIES_DIR, MAX_RANGE_DAYS, Store, StoreError};
use crate::auth::User;
use crate::domain::{Entry, Expense, Invoice};

impl Store {
    // ---------------------------------------------------------- side indexes --
    //
    // Two full scans had crept onto hot paths (review D3): number lookups
    // resolve invoices by human number, and login/SSO resolve users by email.
    // Both are dot-file maps maintained under the same write lock as the
    // documents they index — crash-safe tmp+rename via `write_json`, skipped
    // by `dir_entries` so an index is never mistaken for a document. A
    // missing or stale index never loses data: reads verify the fast path
    // against the document itself, fall back to the old scan, and self-heal.

    pub(super) fn invoice_index_path(&self) -> PathBuf {
        self.root.join("invoices").join(".idx.numbers.json")
    }

    pub(super) fn email_index_path(&self) -> PathBuf {
        self.root.join("users").join(".idx.emails.json")
    }

    /// Map `number -> id` for one invoice. The caller holds the write lock.
    pub(super) fn index_invoice_locked(&self, invoice: &Invoice) -> Result<(), StoreError> {
        let path = self.invoice_index_path();
        let mut idx: HashMap<String, String> = read_json(&path)?.unwrap_or_default();
        let id = invoice.id.to_string();
        idx.retain(|_, v| v != &id);
        idx.insert(invoice.number.clone(), id);
        write_json(&path, &idx)
    }

    /// Drop a deleted invoice from the index (its number is never recycled,
    /// review B3 — the entry simply disappears with the document).
    pub(super) fn unindex_invoice_locked(&self, id: &str) -> Result<(), StoreError> {
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
    pub(super) fn rebuild_invoice_index(&self) -> Result<(), StoreError> {
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
    pub(super) fn index_user_locked(&self, user: &User) -> Result<(), StoreError> {
        let path = self.email_index_path();
        let mut idx: HashMap<String, String> = read_json(&path)?.unwrap_or_default();
        let id = user.id.to_string();
        idx.retain(|_, v| v != &id);
        idx.insert(user.email.to_lowercase(), id);
        write_json(&path, &idx)
    }

    pub(super) fn unindex_user_locked(&self, id: &str) -> Result<(), StoreError> {
        let path = self.email_index_path();
        if let Some(mut idx) = read_json::<HashMap<String, String>>(&path)? {
            idx.retain(|_, v| v != id);
            write_json(&path, &idx)?;
        }
        Ok(())
    }

    pub(super) fn rebuild_email_index(&self) -> Result<(), StoreError> {
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

    pub(super) fn entry_index_path(&self) -> PathBuf {
        self.root.join(ENTRIES_DIR).join(".idx.users.json")
    }

    /// The cold id index (#350): `archive/entries/.idx.ids.json`, holding
    /// `date -> ids` for the published cold days. Written **only** by
    /// [`Store::archive_entries_before`] — never by a read — and read only as a
    /// hint: a slice is believed when its id set equals the cold day folder's own
    /// listing, and a missing, truncated or drifted index falls back to the
    /// authoritative walk. Correctness never depends on it.
    pub(super) fn cold_index_path(&self) -> PathBuf {
        self.root
            .join(ARCHIVE_DIR)
            .join(ENTRIES_DIR)
            .join(".idx.ids.json")
    }

    /// Lenient read: a missing, truncated or corrupt cold index is simply "no
    /// index", and the caller walks the cold tree instead.
    pub(super) fn read_cold_index(&self) -> Option<ColdIdIndex> {
        let bytes = std::fs::read(self.cold_index_path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn expense_index_path(&self) -> PathBuf {
        self.root.join("expenses").join(".idx.users.json")
    }

    /// Lenient read: a missing or unreadable index is simply "no index" — the
    /// caller falls back to the authoritative scan and rebuilds it.
    pub(super) fn read_entry_index(&self) -> Option<EntryUserIndex> {
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
    pub(super) fn read_entry(&self, path: &Path) -> Result<Option<Entry>, StoreError> {
        #[cfg(test)]
        self.doc_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        read_json::<Entry>(path)
    }

    /// One expense document decode, counted for the #335 read-cost test.
    pub(super) fn read_expense(&self, path: &Path) -> Result<Option<Expense>, StoreError> {
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
    /// the *merged* day's — hot ∪ cold (#350). `None` means "not provably
    /// current" (absent, corrupt or drifted) and the caller must scan.
    ///
    /// The merged comparison is load-bearing, not a refinement. #349 prunes the
    /// relocated day's key, so on a cold day the index's id set (empty) *equals*
    /// the hot folder's id set (empty): comparing against the hot folder alone
    /// would "validate" an empty read and show a member their own archived week
    /// as zero hours, with no error.
    fn entry_index_day_ids(
        &self,
        idx: &EntryUserIndex,
        date: NaiveDate,
    ) -> Result<Option<HashMap<String, Vec<String>>>, StoreError> {
        let key = date.format("%Y-%m-%d").to_string();
        let day = idx.days.get(&key).cloned().unwrap_or_default();
        let indexed: std::collections::BTreeSet<String> = day.values().flatten().cloned().collect();
        let mut merged = self.entry_day_disk_ids(date)?;
        merged.extend(self.cold_day_ids(date)?);
        if indexed == merged {
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

    /// Read exactly `ids` for `date` — the hot copy first, the cold copy on a
    /// miss (#350) — refusing (returning `None`) if any is missing from both
    /// trees or is not `user_id`'s: either means the index mis-filed a row, so
    /// the caller must not trust it for this read.
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
            let doc = match self.read_entry(&self.entry_path(date, id))? {
                Some(e) => Some(e),
                None => self.read_entry(&self.cold_entry_path(date, id))?,
            };
            match doc {
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
    pub(super) fn index_entry_locked(&self, entry: &Entry) -> Result<(), StoreError> {
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
    pub(super) fn unindex_entry_locked(&self, date: NaiveDate, id: Uuid) -> Result<(), StoreError> {
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

    pub(super) fn index_expense_locked(&self, expense: &Expense) -> Result<(), StoreError> {
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

    pub(super) fn unindex_expense_locked(&self, id: &str) -> Result<(), StoreError> {
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
    ///
    /// The answer is the **merged** day (hot ∪ cold, #350) — a member read of an
    /// archived day must still see their rows — but the heal files the **hot**
    /// documents only. The #335 index may only name days that are on disk, so
    /// handing it the cold rows would re-add a key naming the relocated day and
    /// regress #349's invariant on the first member read of an archived week.
    fn scan_and_index_entry_day(&self, date: NaiveDate) -> Result<Vec<Entry>, StoreError> {
        let _guard = self.write_lock()?;
        let hot = self.walk_docs(&self.day_dir(date), |p| self.read_entry(p))?;
        let mut merged = hot.clone();
        merge_cold_docs(&mut merged, self.cold_day_docs(date)?);
        self.write_entry_day_index_locked(date, &hot)?;
        Ok(merged)
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
}

/// The per-user entry index (#335): `date -> (owner -> ids)`, holding **every**
/// document in the day so a member read can prove its slice complete against
/// the day folder's own listing. A dot file, never a document.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct EntryUserIndex {
    #[serde(default)]
    pub(super) days: HashMap<String, HashMap<String, Vec<String>>>,
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

/// Merge a day's cold documents into its hot ones, by entry id, **hot wins**
/// (#350). The #349 pass removes the hot day only after its cold copy is
/// complete, so a day a crash left in both trees still resolves to the document
/// the pass had not finished with.
pub(super) fn merge_cold_docs(hot: &mut Vec<Entry>, cold: Vec<Entry>) {
    let mut seen: std::collections::HashSet<Uuid> = hot.iter().map(|e| e.id).collect();
    for doc in cold {
        if seen.insert(doc.id) {
            hot.push(doc);
        }
    }
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
pub(super) fn sort_entries_by_created(out: &mut [Entry]) {
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use crate::domain::{Invoice, InvoiceStatus};
    use chrono::{TimeZone, Utc};

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
mod per_user_index_tests {
    // #335: the per-user entry/expense index must never lose a row — a stale,
    // missing, corrupt or truncated index has to answer exactly as the scan
    // does, and heal. These drive each drift shape directly.
    use super::*;
    use crate::domain::{Entry, Hours, ProjectCode, Source};
    use chrono::{TimeZone, Utc};

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
