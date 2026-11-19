use super::io::dir_mtime;
use super::*;

impl Store {
    // ------------------------------------------------------- lock snapshot (#255)

    pub(super) fn invalidate_lock_snapshot(&self) {
        self.lock_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        match self.lock_cache.lock() {
            Ok(mut cache) => *cache = None,
            Err(poisoned) => *poisoned.into_inner() = None,
        }
    }

    /// The #18 lock snapshot: every frozen entry and expense id, memoized on
    /// the source collections' directory stamps and this Store's write
    /// generation. In steady state (no writes since the last call) this parses
    /// **zero** documents; local writes invalidate it even when the filesystem
    /// does not advance the directory mtime. A scan failure (unreadable or
    /// corrupt document) propagates and is **never cached**, so callers stay
    /// fail-closed (#185).
    ///
    /// Correctness rests on the atomic tmp+rename write discipline: because a
    /// rename into a directory bumps that directory's mtime, the memoized set
    /// cannot go stale behind an out-of-band edit — a changed stamp always
    /// rebuilds. This is the "recompute cheaply rather than risk staleness"
    /// choice the fail-closed contract demands.
    pub fn lock_snapshot(&self) -> Result<Arc<LockSnapshot>, StoreError> {
        loop {
            let generation = self
                .lock_generation
                .load(std::sync::atomic::Ordering::Acquire);
            let stamp = self.lock_stamp();
            {
                let cache = self
                    .lock_cache
                    .lock()
                    .map_err(|_| StoreError::Io("lock cache poisoned".into()))?;
                if let Some((cached, cached_generation, snapshot)) = cache.as_ref()
                    && *cached == stamp
                    && *cached_generation == generation
                {
                    return Ok(Arc::clone(snapshot));
                }
            }

            let snapshot = Arc::new(self.scan_lock_state()?);
            if self
                .lock_generation
                .load(std::sync::atomic::Ordering::Acquire)
                != generation
                || self.lock_stamp() != stamp
            {
                continue;
            }
            if let Ok(mut cache) = self.lock_cache.lock() {
                *cache = Some((stamp, generation, Arc::clone(&snapshot)));
            }
            return Ok(snapshot);
        }
    }

    fn lock_stamp(&self) -> LockStamp {
        LockStamp {
            invoices: dir_mtime(&self.doc_dir::<Invoice>()),
            submissions: dir_mtime(&self.doc_dir::<Submission>()),
            claims: dir_mtime(&self.doc_dir::<ExpenseClaim>()),
        }
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
            // QA-09: carry the week's *state* with it — a submitted week can be
            // rejected to unlock, an approved one cannot, and the 409 message
            // must not offer an action the decision endpoint refuses.
            let frozen = FrozenWeek {
                week_start: sub.week_start.strftime("%Y-%m-%d").to_string(),
                approved: sub.state == crate::domain::SubmissionState::Approved,
            };
            for entry in &sub.entry_ids {
                snap.submitted_entries
                    .entry(*entry)
                    .or_insert_with(|| frozen.clone());
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
