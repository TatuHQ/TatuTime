use super::io::dir_mtime;
use super::*;

impl Store {
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
}
