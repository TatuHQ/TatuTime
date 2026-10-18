//! Entry/expense-lock port: one seam answers "is this record frozen, and why?".
//!
//! Invoices (#8), timesheet submissions (#16) and reimbursement claims (#24) are
//! lock providers; the entry edit/delete and expense delete handlers consult this
//! seam, so a record cannot be changed through any route once locked.
//! `CombinedLocks` composes the providers (first lock wins) and all three answer
//! from one memoized `store::LockSnapshot` (#255), so a per-record check never
//! re-reads every document. `NoLocks` locks nothing.

use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum LockReason {
    Invoiced {
        id: String,
    },
    /// An entry on a submitted or approved timesheet. `approved` distinguishes
    /// the two (QA-09): a submitted week can be rejected to unlock, an approved
    /// one is permanent, and the decision endpoint refuses to reject it.
    Submitted {
        id: String,
        approved: bool,
    },
    /// An expense referenced by an *open* invoice (#8/#114) — the expense
    /// counterpart of `Invoiced`.
    ExpenseInvoiced,
    /// An expense on a submitted/approved reimbursement claim (#24).
    ExpenseClaimed,
}

impl LockReason {
    pub fn message(&self) -> String {
        match self {
            LockReason::Invoiced { id } => {
                format!("entry is invoiced ({id}); invoiced time is permanent and cannot be changed")
            }
            // QA-09: the approved message must not offer an impossible action.
            // `POST /submissions/{id}/decision` only decides a *submitted* week,
            // so "reject it first" on an approved week sent the user into a 409.
            LockReason::Submitted {
                id,
                approved: true,
            } => format!(
                "entry is in an approved timesheet ({id}); approved time is permanent and cannot be changed"
            ),
            LockReason::Submitted {
                id,
                approved: false,
            } => {
                format!("entry is in a submitted timesheet ({id}); reject/withdraw it first")
            }
            LockReason::ExpenseInvoiced => {
                "expense is on an issued invoice; invoiced expenses are permanent and cannot be deleted".into()
            }
            LockReason::ExpenseClaimed => {
                "expense is on a submitted/approved claim; reject it first".into()
            }
        }
    }
}

/// The backing collection could not be read, so the lock state of a record
/// cannot be verified (#185). Handlers must treat this as **fail closed**:
/// refuse the write (503), never allow it because verification failed.
#[derive(Debug)]
pub struct LockUnavailable(pub String);

/// One entry's frozen state, plus one pass over every frozen entry. The providers
/// (invoices #8, submissions #16, claims #24) answer from one memoized snapshot
/// (#255); reads are never blocked — only a write to a frozen record is refused.
///
/// ```rust
/// match lock.entry_lock(id) {
///     Ok(Some(_reason)) => /* 409: invoiced, submitted or claimed */,
///     Ok(None)          => /* editable */,
///     Err(_)            => /* fail closed: 503 lock_unavailable (#185) */,
/// }
/// ```
pub trait EntryLock: Send + Sync {
    /// `Ok(Some(reason))` if the entry is locked and must not be edited or
    /// deleted; `Ok(None)` if provably unlocked; `Err` if the state cannot be
    /// verified — callers must fail closed (#185).
    fn entry_lock(&self, entry_id: Uuid) -> Result<Option<LockReason>, LockUnavailable>;

    /// Every locked entry id in **one** pass (#218). Callers that ask about
    /// many entries per request (submission creation) must not drive
    /// `entry_lock` per id — the providers answer from one snapshot (#255).
    /// Implementations must agree with `entry_lock` (covered by a unit test).
    fn locked_entries(&self) -> Result<std::collections::HashSet<Uuid>, LockUnavailable>;

    /// The expense counterpart (#255): `Ok(Some(reason))` freezes an expense
    /// delete the way `entry_lock` freezes an entry, `Ok(None)` is provably
    /// unlocked and `Err` is unverifiable (fail closed). Defaults to "nothing
    /// locked" so entry-only providers — and test doubles — need not implement
    /// it.
    fn expense_lock(&self, _expense_id: Uuid) -> Result<Option<LockReason>, LockUnavailable> {
        Ok(None)
    }
    fn locked_expenses(&self) -> Result<std::collections::HashSet<Uuid>, LockUnavailable> {
        Ok(std::collections::HashSet::new())
    }
}

/// Default: nothing is locked.
#[derive(Debug, Default)]
pub struct NoLocks;

impl EntryLock for NoLocks {
    fn entry_lock(&self, _entry_id: Uuid) -> Result<Option<LockReason>, LockUnavailable> {
        Ok(None)
    }
    fn locked_entries(&self) -> Result<std::collections::HashSet<Uuid>, LockUnavailable> {
        Ok(std::collections::HashSet::new())
    }
}

/// Any provider that locks the record wins; a provider that cannot verify
/// fails the whole check closed (#185).
pub struct CombinedLocks {
    providers: Vec<Box<dyn EntryLock>>,
}

impl CombinedLocks {
    pub fn new(providers: Vec<Box<dyn EntryLock>>) -> Self {
        Self { providers }
    }
}

impl EntryLock for CombinedLocks {
    fn entry_lock(&self, entry_id: Uuid) -> Result<Option<LockReason>, LockUnavailable> {
        for provider in &self.providers {
            if let Some(reason) = provider.entry_lock(entry_id)? {
                return Ok(Some(reason));
            }
        }
        Ok(None)
    }
    fn locked_entries(&self) -> Result<std::collections::HashSet<Uuid>, LockUnavailable> {
        // Union, fail-closed: one provider that cannot answer locks
        // everything out (#185 semantics carry over to bulk checks).
        let mut all = std::collections::HashSet::new();
        for provider in &self.providers {
            all.extend(provider.locked_entries()?);
        }
        Ok(all)
    }
    fn expense_lock(&self, expense_id: Uuid) -> Result<Option<LockReason>, LockUnavailable> {
        for provider in &self.providers {
            if let Some(reason) = provider.expense_lock(expense_id)? {
                return Ok(Some(reason));
            }
        }
        Ok(None)
    }
    fn locked_expenses(&self) -> Result<std::collections::HashSet<Uuid>, LockUnavailable> {
        let mut all = std::collections::HashSet::new();
        for provider in &self.providers {
            all.extend(provider.locked_expenses()?);
        }
        Ok(all)
    }
}

/// Read the memoized #255 snapshot, mapping a store failure to the fail-closed
/// `LockUnavailable` every provider shares (#185).
fn snapshot(
    store: &crate::store::Store,
) -> Result<std::sync::Arc<crate::store::LockSnapshot>, LockUnavailable> {
    store
        .lock_snapshot()
        .map_err(|e| LockUnavailable(e.to_string()))
}

/// Lock provider backed by **issued (non-draft) invoices**: an entry or
/// expense referenced by an `issued | partly_paid | paid | written_off`
/// invoice is frozen **permanently**. Settling or writing off an invoice no
/// longer releases its records — the archived document can never change
/// (rates/hours are snapshotted at issue), so the underlying timesheet lines
/// must not drift from it either. The only way back to editable is before
/// issue: a draft locks nothing and can be deleted.
/// Reads are unaffected; only edit/delete consult this.
pub struct InvoiceLock {
    store: std::sync::Arc<crate::store::Store>,
}

impl InvoiceLock {
    pub fn new(store: std::sync::Arc<crate::store::Store>) -> Self {
        Self { store }
    }
}

impl EntryLock for InvoiceLock {
    fn entry_lock(&self, entry_id: Uuid) -> Result<Option<LockReason>, LockUnavailable> {
        Ok(snapshot(&self.store)?
            .invoiced_entries
            .get(&entry_id)
            .map(|number| LockReason::Invoiced { id: number.clone() }))
    }

    fn locked_entries(&self) -> Result<std::collections::HashSet<Uuid>, LockUnavailable> {
        Ok(snapshot(&self.store)?
            .invoiced_entries
            .keys()
            .copied()
            .collect())
    }

    fn expense_lock(&self, expense_id: Uuid) -> Result<Option<LockReason>, LockUnavailable> {
        Ok(snapshot(&self.store)?
            .invoiced_expenses
            .contains_key(&expense_id)
            .then_some(LockReason::ExpenseInvoiced))
    }

    fn locked_expenses(&self) -> Result<std::collections::HashSet<Uuid>, LockUnavailable> {
        Ok(snapshot(&self.store)?
            .invoiced_expenses
            .keys()
            .copied()
            .collect())
    }
}

/// Lock provider backed by submitted/approved timesheets (#16): an entry in a
/// submitted or approved submission is frozen until it is rejected/withdrawn.
pub struct SubmissionLock {
    store: std::sync::Arc<crate::store::Store>,
}

impl SubmissionLock {
    pub fn new(store: std::sync::Arc<crate::store::Store>) -> Self {
        Self { store }
    }
}

impl EntryLock for SubmissionLock {
    fn entry_lock(&self, entry_id: Uuid) -> Result<Option<LockReason>, LockUnavailable> {
        Ok(snapshot(&self.store)?
            .submitted_entries
            .get(&entry_id)
            .map(|week| LockReason::Submitted {
                id: week.week_start.clone(),
                approved: week.approved,
            }))
    }

    fn locked_entries(&self) -> Result<std::collections::HashSet<Uuid>, LockUnavailable> {
        Ok(snapshot(&self.store)?
            .submitted_entries
            .keys()
            .copied()
            .collect())
    }
}

/// Lock provider backed by submitted/approved reimbursement claims (#24): an
/// expense referenced by one is frozen until the claim is rejected (or the
/// expense leaves it). It is the third provider behind the same seam the entry
/// locks use (#190/#255).
pub struct ClaimLock {
    store: std::sync::Arc<crate::store::Store>,
}

impl ClaimLock {
    pub fn new(store: std::sync::Arc<crate::store::Store>) -> Self {
        Self { store }
    }
}

impl EntryLock for ClaimLock {
    fn entry_lock(&self, _entry_id: Uuid) -> Result<Option<LockReason>, LockUnavailable> {
        Ok(None)
    }

    fn locked_entries(&self) -> Result<std::collections::HashSet<Uuid>, LockUnavailable> {
        Ok(std::collections::HashSet::new())
    }

    fn expense_lock(&self, expense_id: Uuid) -> Result<Option<LockReason>, LockUnavailable> {
        Ok(snapshot(&self.store)?
            .claimed_expenses
            .contains_key(&expense_id)
            .then_some(LockReason::ExpenseClaimed))
    }

    fn locked_expenses(&self) -> Result<std::collections::HashSet<Uuid>, LockUnavailable> {
        Ok(snapshot(&self.store)?
            .claimed_expenses
            .keys()
            .copied()
            .collect())
    }
}

#[cfg(test)]
mod bulk_tests {
    // #218/#255: locked_entries()/locked_expenses() must agree with the
    // per-record entry_lock()/expense_lock() for the real providers — the
    // submission path uses the bulk call and the invoice/claim providers now
    // answer from the memoized snapshot, so drift here would be silent. The
    // test also pins cache invalidation: a write that releases records after
    // the snapshot was built must be reflected on the next call.
    use super::*;
    use crate::domain::{
        ClaimState, Currency, Entry, ExpenseClaim, Hours, Invoice, InvoiceLine, InvoiceStatus,
        LineKind, ProjectCode, Source, Submission, SubmissionState,
    };
    use chrono::{NaiveDate, TimeZone, Utc};

    fn entry_ref(id: Uuid) -> Entry {
        Entry {
            id,
            date: NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
            customer_id: Uuid::new_v4(),
            user_id: None,
            project_code: ProjectCode("P1".into()),
            task_code: None,
            hours: Hours(100),
            note: String::new(),
            billable: true,
            source: Source::Manual,
            created_at: Utc.with_ymd_and_hms(2026, 10, 2, 0, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 10, 2, 0, 0, 0).unwrap(),
        }
    }

    /// A time line (when `entry_id` is set) or an expense line (otherwise).
    fn invoice_line(entry_id: Option<Uuid>, expense_id: Option<Uuid>) -> InvoiceLine {
        InvoiceLine {
            kind: if entry_id.is_some() {
                LineKind::Time
            } else {
                LineKind::Expense
            },
            date: NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
            entry_id,
            expense_id,
            project_code: Some(ProjectCode("P1".into())),
            task_code: None,
            hours: entry_id.map(|_| Hours(100)),
            rate_minor: entry_id.map(|_| 6000),
            amount_minor: 600,
            note: String::new(),
            quantity_hundredths: None,
            unit_price_minor: None,
            item_kind: None,
        }
    }

    fn invoice(number: &str, status: InvoiceStatus, lines: Vec<InvoiceLine>) -> Invoice {
        Invoice {
            id: Uuid::new_v4(),
            number: number.into(),
            customer_id: Uuid::new_v4(),
            currency: Currency("EUR".into()),
            period_from: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            period_to: NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
            lines,
            total_minor: 600,
            status,
            created_at: Utc.with_ymd_and_hms(2026, 10, 7, 0, 0, 0).unwrap(),
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

    fn claim(state: ClaimState, expense_ids: Vec<Uuid>) -> ExpenseClaim {
        ExpenseClaim {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            title: "October".into(),
            expense_ids,
            total_minor: 600,
            currency: Currency("EUR".into()),
            state,
            comment: String::new(),
            created_at: Utc.with_ymd_and_hms(2026, 10, 7, 0, 0, 0).unwrap(),
            submitted_at: None,
            decided_at: None,
        }
    }

    #[test]
    fn bulk_locked_entries_match_per_entry_checks() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            std::sync::Arc::new(crate::store::Store::open(dir.path().join("data")).unwrap());
        let (e_issued, e_partly, e_paid, e_sub, e_free) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let (x_issued, x_claim, x_free) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for entry in [e_issued, e_partly, e_paid] {
            store.put_entry(&entry_ref(entry)).unwrap();
        }
        // An issued invoice freezes both its entry and its expense line.
        let inv_issued = invoice(
            "INV-1",
            InvoiceStatus::Issued,
            vec![invoice_line(Some(e_issued), Some(x_issued))],
        );
        let inv_issued_id = inv_issued.id;
        store.put_invoice(&inv_issued).unwrap();
        store
            .put_invoice(&invoice(
                "INV-2",
                InvoiceStatus::PartlyPaid,
                vec![invoice_line(Some(e_partly), None)],
            ))
            .unwrap();
        store
            .put_invoice(&invoice(
                "INV-3",
                InvoiceStatus::Paid,
                vec![invoice_line(Some(e_paid), None)],
            ))
            .unwrap();
        store
            .put_submission(&Submission {
                id: Uuid::new_v4(),
                user_id: Uuid::new_v4(),
                week_start: NaiveDate::from_ymd_opt(2026, 9, 28).unwrap(),
                week_end: NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
                state: SubmissionState::Submitted,
                entry_ids: vec![e_sub],
                comment: String::new(),
                created_at: Utc::now(),
                submitted_at: None,
                decided_at: None,
            })
            .unwrap();
        // A *draft* claim does not lock; a submitted one does (#24).
        store
            .put_claim(&claim(ClaimState::Draft, vec![x_free]))
            .unwrap();
        store
            .put_claim(&claim(ClaimState::Submitted, vec![x_claim]))
            .unwrap();

        let combined = CombinedLocks::new(vec![
            Box::new(InvoiceLock::new(store.clone())),
            Box::new(SubmissionLock::new(store.clone())),
            Box::new(ClaimLock::new(store.clone())),
        ]);

        let ids = [e_issued, e_partly, e_paid, e_sub, e_free];
        let bulk = combined.locked_entries().unwrap();
        for id in ids {
            let per_entry = combined.entry_lock(id).unwrap().is_some();
            assert_eq!(
                bulk.contains(&id),
                per_entry,
                "bulk vs per-entry disagree for {id} (every issued invoice locks, paid included; free never does)"
            );
        }
        assert!(
            bulk.contains(&e_issued)
                && bulk.contains(&e_partly)
                && bulk.contains(&e_paid)
                && bulk.contains(&e_sub),
            "invoiced time is permanent: issued, partly-paid AND paid invoices lock"
        );
        assert!(!bulk.contains(&e_free));

        // Expense side, including the folded claim provider (#190 follow-up).
        let x_ids = [x_issued, x_claim, x_free];
        let bulk_x = combined.locked_expenses().unwrap();
        for id in x_ids {
            let per_expense = combined.expense_lock(id).unwrap().is_some();
            assert_eq!(
                bulk_x.contains(&id),
                per_expense,
                "bulk vs per-expense disagree for {id}"
            );
        }
        assert!(bulk_x.contains(&x_issued) && bulk_x.contains(&x_claim));
        assert!(!bulk_x.contains(&x_free), "a draft claim does not lock");

        // #255 cache invalidation: the snapshot built above must not survive a
        // write that changes its records. Deleting the invoice drops both
        // its entry and its expense on the very next call — the changed
        // directory stamp forces one re-scan, never a stale answer.
        store.delete_invoice(inv_issued_id).unwrap();
        let bulk2 = combined.locked_entries().unwrap();
        let bulk_x2 = combined.locked_expenses().unwrap();
        assert!(
            !bulk2.contains(&e_issued),
            "a released entry must not stay cached"
        );
        assert!(
            !bulk_x2.contains(&x_issued),
            "a released expense must not stay cached"
        );
        assert!(bulk2.contains(&e_partly) && bulk2.contains(&e_sub));
        assert!(bulk_x2.contains(&x_claim));
    }
}
