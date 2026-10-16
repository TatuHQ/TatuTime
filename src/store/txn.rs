use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

use super::invoices::pdf_hint;
use super::io::{read_json, write_bytes_atomic, write_json};
use super::*;
use crate::domain::{Entry, InvoiceStatus};

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
    /// #214: the timer's existence is verified INSIDE the same lock, before the
    /// entry is written. Two concurrent `POST /api/timer/stop` calls may both
    /// read the unlocked timer, but only the winner sees it under the lock; the
    /// loser finds none and 404s, so the time is never double-logged.
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
