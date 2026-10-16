use std::path::PathBuf;

use chrono::NaiveDate;
use uuid::Uuid;

use super::cold::ColdIdIndex;
use super::index::{merge_cold_docs, sort_entries_by_created};
use super::io::{dir_entries, json_stems, read_json, write_json};
use super::*;
use crate::domain::Entry;

impl Store {
    // -------------------------------------------------------------- entries --

    pub(super) fn day_dir(&self, date: NaiveDate) -> PathBuf {
        self.root
            .join(ENTRIES_DIR)
            .join(date.format("%Y-%m-%d").to_string())
    }

    pub(super) fn entry_path(&self, date: NaiveDate, id: Uuid) -> PathBuf {
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
    pub(super) fn cold_entry_path(&self, date: NaiveDate, id: Uuid) -> PathBuf {
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
    pub(super) fn cold_day_folders(&self) -> Result<Vec<NaiveDate>, StoreError> {
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
    pub(super) fn cold_day_ids(
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
    pub(super) fn cold_day_docs(&self, date: NaiveDate) -> Result<Vec<Entry>, StoreError> {
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
    pub(super) fn entry_exists(&self, date: NaiveDate, id: Uuid) -> bool {
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
    pub(super) fn remove_entry_copies_locked(
        &self,
        date: NaiveDate,
        id: Uuid,
    ) -> Result<(), StoreError> {
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
    pub(super) fn rehydrate_entry_locked(
        &self,
        date: NaiveDate,
        id: Uuid,
    ) -> Result<(), StoreError> {
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

    pub(super) fn has_entries_for_customer(&self, customer_id: Uuid) -> Result<bool, StoreError> {
        Ok(self
            .scan_all_entries()?
            .iter()
            .any(|e| e.customer_id == customer_id))
    }

    pub(super) fn has_entries_for_project(
        &self,
        customer_id: Uuid,
        code: &str,
    ) -> Result<bool, StoreError> {
        Ok(self
            .scan_all_entries()?
            .iter()
            .any(|e| e.customer_id == customer_id && e.project_code.0 == code))
    }

    pub(super) fn has_entries_for_task(
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
}
