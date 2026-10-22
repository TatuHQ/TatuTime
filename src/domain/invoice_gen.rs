use super::customer::Customer;
use super::entry::Entry;
use super::expense::Expense;
use super::invoice::{Invoice, InvoiceError, InvoiceLine, InvoiceStatus, LineKind};
use super::primitives::Currency;
use super::project::Project;
use crate::auth::User;
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

/// Entry and expense ids already committed to a **non-draft** invoice, in the
/// `excluded_entries` / `excluded_expenses` shape [`generate_invoice`] consumes
/// (#8/#248): the single "already invoiced" predicate, so a line on an issued,
/// settled (`partly_paid`/`paid`) or written-off document is never billed twice.
/// Settling or writing off releases the edit lock (#114) but not the billing
/// exclusion; drafts stay billable until issued.
///
/// ```rust
/// let (entries, expenses) = already_invoiced(&invoices);
/// // entries = every id on a non-draft invoice; expenses = their expense ids
/// ```
pub fn already_invoiced(invoices: &[Invoice]) -> (Vec<Uuid>, Vec<Uuid>) {
    let billed: Vec<&Invoice> = invoices
        .iter()
        .filter(|i| i.status != InvoiceStatus::Draft)
        .collect();
    let entries = billed
        .iter()
        .flat_map(|i| i.lines.iter())
        .filter_map(|l| l.entry_id)
        .collect();
    let expenses = billed
        .iter()
        .flat_map(|i| i.lines.iter())
        .filter_map(|l| l.expense_id)
        .collect();
    (entries, expenses)
}

/// Read-only context for invoice generation, bundled to keep the signature small.
pub struct InvoiceSources<'a> {
    pub projects: &'a [Project],
    pub users: &'a [User],
    pub entries: &'a [Entry],
    pub expenses: &'a [Expense],
    /// Entry ids already on a non-draft invoice (excluded); see
    /// [`already_invoiced`].
    pub excluded_entries: &'a [Uuid],
    /// Expense ids already on a non-draft invoice (excluded); see
    /// [`already_invoiced`].
    pub excluded_expenses: &'a [Uuid],
    pub include_expenses: bool,
    /// #134: when true the `projects` list is the billable SCOPE — entries
    /// whose project is absent were deliberately deselected by the staged
    /// wizard and must not bill via the customer-rate fallback.
    pub restrict_projects: bool,
}

/// Build a draft invoice from the billable entries (and, if enabled, billable
/// expenses) in `[from, to]` for one customer. Each line snapshots the rate
/// resolved per entry (person > project > customer), so a later rate change never
/// rewrites an issued invoice; `excluded_*` ids (see [`already_invoiced`]) are skipped.
///
/// ```rust
/// // a month's billable time becomes one draft, the rate frozen on every line
/// let draft = generate_invoice(number, &customer, &sources, from, to, now)?;
/// ```
pub fn generate_invoice(
    number: String,
    customer: &Customer,
    src: &InvoiceSources<'_>,
    from: NaiveDate,
    to: NaiveDate,
    now: DateTime<Utc>,
) -> Result<Invoice, InvoiceError> {
    let InvoiceSources {
        projects,
        users,
        entries,
        expenses,
        excluded_entries,
        excluded_expenses,
        include_expenses,
        restrict_projects,
    } = src;
    // Review D4: index the lookups done per entry — linear finds made large
    // billing runs O(entries × records).
    let excluded_entry: std::collections::HashSet<Uuid> =
        excluded_entries.iter().copied().collect();
    let excluded_expense: std::collections::HashSet<Uuid> =
        excluded_expenses.iter().copied().collect();
    let project_by_code: std::collections::HashMap<&str, &Project> =
        projects.iter().map(|p| (p.code.0.as_str(), p)).collect();
    let user_by_id: std::collections::HashMap<Uuid, &User> =
        users.iter().map(|u| (u.id, u)).collect();
    let user_rate = |e: &Entry| -> Option<u64> {
        e.user_id
            .and_then(|uid| user_by_id.get(&uid).map(|u| u.default_rate_minor))
    };
    let mut lines: Vec<InvoiceLine> = Vec::new();
    let mut currency: Option<Currency> = None;
    let mut set_currency = |cur: &Currency| -> Result<(), InvoiceError> {
        match &currency {
            None => {
                currency = Some(cur.clone());
                Ok(())
            }
            Some(existing) if existing == cur => Ok(()),
            Some(existing) => Err(InvoiceError::MixedCurrency {
                a: existing.0.clone(),
                b: cur.0.clone(),
            }),
        }
    };
    for e in entries.iter() {
        if e.customer_id != customer.id
            || !e.billable
            || e.externally_invoiced()
            || e.date < from
            || e.date > to
        {
            continue;
        }
        if excluded_entry.contains(&e.id) {
            continue; // already billed on a non-draft invoice
        }
        let project = project_by_code.get(e.project_code.0.as_str()).copied();
        if *restrict_projects && project.is_none() {
            continue; // deselected project (#134)
        }
        let (cur, rate) = e.billing_rates(customer, project, user_rate(e));
        set_currency(&cur)?;
        lines.push(InvoiceLine {
            kind: LineKind::Time,
            date: e.date,
            entry_id: Some(e.id),
            expense_id: None,
            project_code: Some(e.project_code.clone()),
            task_code: e.task_code.clone(),
            hours: Some(e.hours),
            rate_minor: Some(rate),
            amount_minor: e.billing_amount_minor(rate),
            note: e.note.clone(),
            quantity_hundredths: None,
            unit_price_minor: None,
            item_kind: None,
        });
    }
    if *include_expenses {
        for x in expenses.iter() {
            if x.customer_id != customer.id
                || !x.billable
                || x.externally_invoiced()
                || x.date < from
                || x.date > to
            {
                continue;
            }
            if excluded_expense.contains(&x.id) {
                continue;
            }
            if *restrict_projects
                && x.project_code
                    .as_ref()
                    .is_none_or(|pc| !project_by_code.contains_key(pc.0.as_str()))
            {
                continue; // deselected project (#134)
            }
            set_currency(&x.currency)?;
            lines.push(InvoiceLine {
                kind: LineKind::Expense,
                date: x.date,
                entry_id: None,
                expense_id: Some(x.id),
                project_code: x.project_code.clone(),
                task_code: None,
                hours: None,
                rate_minor: None,
                amount_minor: x.amount_minor,
                note: x.note.clone(),
                quantity_hundredths: None,
                unit_price_minor: None,
                item_kind: None,
            });
        }
    }
    if lines.is_empty() {
        return Err(InvoiceError::NothingToInvoice);
    }
    let total_minor = lines
        .iter()
        .fold(0u64, |acc, l| acc.saturating_add(l.amount_minor)); // #190
    Ok(Invoice {
        supersedes: None,
        superseded_by: None,
        imported: None,
        id: Uuid::new_v4(),
        number,
        customer_id: customer.id,
        // #220: unreachable while `lines` is non-empty (every pushed line
        // calls set_currency first, and empty lines errored above) — expressed
        // as a typed error instead of an in-place panic.
        currency: currency.ok_or(InvoiceError::NothingToInvoice)?,
        period_from: from,
        period_to: to,
        lines,
        total_minor,
        status: InvoiceStatus::Draft,
        created_at: now,
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
        subject: String::new(),
        terms: None,
        document: Default::default(),
    })
}

/// QA-02: whether `draft` has drifted from the timesheet it was generated from.
///
/// The issue transaction calls this **before persisting anything**, so an
/// issued invoice can never bill hours the timesheet no longer holds —
/// AGENTS.md's rule that the archived invoice is the record of what was billed
/// and "the timesheet must never drift from it".
///
/// Re-deriving the whole line set would need the create-time narrowing
/// (`include_expenses`, `project_codes`) that a draft does not carry, and
/// manual lines have no source document at all. So the check is exact where it
/// can be: **every tracked line** (time/expense) is re-resolved from its source
/// record at issue time — existence, customer, period, billability, date,
/// project/task, and the rate [`effective_rates`] resolves *now* — and must
/// still reproduce the stored snapshot (`rate_minor`, `amount_minor`). A line
/// whose source is gone, changed, or already billed on another non-draft
/// invoice makes the draft stale; a manual (fixed) line never does.
#[allow(clippy::too_many_arguments)]
pub fn draft_is_stale(
    draft: &Invoice,
    customer: &Customer,
    projects: &[Project],
    users: &[User],
    entries: &[Entry],
    expenses: &[Expense],
    excluded_entries: &[Uuid],
    excluded_expenses: &[Uuid],
) -> bool {
    let project_by_code: std::collections::HashMap<&str, &Project> =
        projects.iter().map(|p| (p.code.0.as_str(), p)).collect();
    let user_by_id: std::collections::HashMap<Uuid, &User> =
        users.iter().map(|u| (u.id, u)).collect();
    let entry_by_id: std::collections::HashMap<Uuid, &Entry> =
        entries.iter().map(|e| (e.id, e)).collect();
    let expense_by_id: std::collections::HashMap<Uuid, &Expense> =
        expenses.iter().map(|x| (x.id, x)).collect();
    let excluded_entry: std::collections::HashSet<Uuid> =
        excluded_entries.iter().copied().collect();
    let excluded_expense: std::collections::HashSet<Uuid> =
        excluded_expenses.iter().copied().collect();
    let user_rate = |e: &Entry| -> Option<u64> {
        e.user_id
            .and_then(|uid| user_by_id.get(&uid).map(|u| u.default_rate_minor))
    };
    for line in &draft.lines {
        match line.kind {
            LineKind::Time => {
                let Some(id) = line.entry_id else {
                    // A time line with no source entry is not something the
                    // timesheet can vouch for: never issue it.
                    return true;
                };
                if excluded_entry.contains(&id) {
                    return true; // already billed on a non-draft invoice (#248)
                }
                let Some(entry) = entry_by_id.get(&id).copied() else {
                    return true; // the entry was deleted after the draft was built
                };
                if entry.customer_id != customer.id
                    || !entry.billable
                    || entry.externally_invoiced()
                    || entry.date < draft.period_from
                    || entry.date > draft.period_to
                    || entry.date != line.date
                    || line.hours != Some(entry.hours)
                    || line.project_code.as_ref() != Some(&entry.project_code)
                    || line.task_code != entry.task_code
                {
                    return true;
                }
                let project = project_by_code.get(entry.project_code.0.as_str()).copied();
                let (currency, rate) = entry.billing_rates(customer, project, user_rate(entry));
                if currency != draft.currency
                    || line.rate_minor != Some(rate)
                    || line.amount_minor != entry.billing_amount_minor(rate)
                {
                    return true; // the period resolution no longer yields the snapshot
                }
            }
            LineKind::Expense => {
                let Some(id) = line.expense_id else {
                    return true;
                };
                if excluded_expense.contains(&id) {
                    return true;
                }
                let Some(expense) = expense_by_id.get(&id).copied() else {
                    return true;
                };
                if expense.customer_id != customer.id
                    || !expense.billable
                    || expense.externally_invoiced()
                    || expense.currency != draft.currency
                    || expense.date < draft.period_from
                    || expense.date > draft.period_to
                    || expense.date != line.date
                    || expense.amount_minor != line.amount_minor
                {
                    return true;
                }
            }
            // Manual lines are free-form: no timesheet record to drift from.
            LineKind::Fixed => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Hours, ProjectCode};

    fn inv(status: InvoiceStatus, total: u64, due: Option<NaiveDate>) -> Invoice {
        Invoice {
            supersedes: None,
            superseded_by: None,
            imported: None,
            id: Uuid::new_v4(),
            number: "INV-1".into(),
            customer_id: Uuid::new_v4(),
            currency: Currency("EUR".into()),
            period_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            period_to: NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(),
            lines: vec![],
            total_minor: total,
            status,
            created_at: Utc::now(),
            issued_at: None,
            due_date: due,
            paid_at: None,
            payment_reference: String::new(),
            pdf: None,
            payments: vec![],
            write_off_reason: String::new(),
            written_off_at: None,
            tax_hundredths: 0,
            discount_hundredths: 0,
            subject: String::new(),
            terms: None,
            document: Default::default(),
        }
    }

    #[test]
    fn already_invoiced_excludes_every_non_draft_status() {
        let entry = Uuid::new_v4();
        let expense = Uuid::new_v4();
        let line = InvoiceLine {
            kind: LineKind::Time,
            date: NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
            entry_id: Some(entry),
            expense_id: Some(expense),
            project_code: Some(ProjectCode("P1".into())),
            task_code: None,
            hours: Some(Hours(100)),
            rate_minor: Some(6000),
            amount_minor: 6000,
            note: String::new(),
            quantity_hundredths: None,
            unit_price_minor: None,
            item_kind: None,
        };
        let with_line = |status: InvoiceStatus| {
            let mut i = inv(status, 6000, None);
            i.lines = vec![line.clone()];
            i
        };
        // A draft's lines stay billable (#248): the predicate must NOT exclude
        // them, or the recurring job could never bill a fresh period.
        let (draft_entries, draft_expenses) = already_invoiced(&[with_line(InvoiceStatus::Draft)]);
        assert!(draft_entries.is_empty() && draft_expenses.is_empty());
        // Every settled or closed state is already billed: issued, partly
        // paid, paid and written-off all keep their lines out of new drafts.
        for status in [
            InvoiceStatus::Issued,
            InvoiceStatus::PartlyPaid,
            InvoiceStatus::Paid,
            InvoiceStatus::WrittenOff,
        ] {
            let (entries, expenses) = already_invoiced(&[with_line(status)]);
            assert_eq!(entries, vec![entry], "entries for {status:?}");
            assert_eq!(expenses, vec![expense], "expenses for {status:?}");
        }
    }
}
