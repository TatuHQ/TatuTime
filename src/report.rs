// Read-only aggregation and CSV export. Pure functions over already-loaded
// documents so the totals can be unit tested without touching disk.
//
// Amounts use each entry's effective rate (project override, else customer
// default) at read time. A future invoice snapshots these numbers; nothing in
// reports mutates stored data.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use uuid::Uuid;

use crate::auth::User;
use crate::domain::{Customer, Entry};

fn margin_minor(revenue: u64, cost: u64) -> i64 {
    if revenue >= cost {
        i64::try_from(revenue - cost).unwrap_or(i64::MAX)
    } else {
        let difference = cost - revenue;
        if difference > i64::MAX as u64 {
            i64::MIN
        } else {
            -(difference as i64)
        }
    }
}

#[cfg(test)]
mod margin_tests {
    use super::margin_minor;

    #[test]
    fn margin_conversion_handles_signed_boundaries_without_wrapping() {
        assert_eq!(margin_minor(i64::MAX as u64, 0), i64::MAX);
        assert_eq!(margin_minor((i64::MAX as u64) + 1, 0), i64::MAX);
        assert_eq!(margin_minor(0, i64::MAX as u64), -i64::MAX);
        assert_eq!(margin_minor(0, (i64::MAX as u64) + 1), i64::MIN);
        assert_eq!(margin_minor(0, u64::MAX), i64::MIN);
        assert_eq!(margin_minor(10, 9), 1);
        assert_eq!(margin_minor(9, 10), -1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Customer,
    Project,
    Week,
    Person,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SummaryRow {
    pub key: String,
    pub label: String,
    pub currency: String,
    pub hours: f64,
    pub amount_minor: u64,
    pub entries: usize,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Summary {
    pub group: String,
    pub rows: Vec<SummaryRow>,
    pub total_hours: f64,
    pub billable_hours: f64,
    pub nonbillable_hours: f64,
}

fn group_key(
    entry: &Entry,
    kind: Group,
    customer_name: &str,
    code: &str,
    user_name: &str,
) -> (String, String) {
    match kind {
        Group::Customer => (entry.customer_id.to_string(), customer_name.to_owned()),
        Group::Project => (
            format!("{}:{code}", entry.customer_id),
            format!("{customer_name} / {code}"),
        ),
        Group::Person => (
            entry.user_id.map(|u| u.to_string()).unwrap_or_default(),
            user_name.to_owned(),
        ),
        Group::Week => {
            let iso = entry.date.iso_week();
            (
                format!("{}-W{:02}", iso.year(), iso.week()),
                format!("{}-W{:02}", iso.year(), iso.week()),
            )
        }
    }
}

/// `billable_filter` scopes the rows to billable (`Some(true)`), non-billable
/// (`Some(false)`) or all (`None`); the billable/non-billable split in the
/// totals always reflects the full entry set.
pub fn summarise(
    entries: &[Entry],
    customers: &[Customer],
    projects: &[(Uuid, crate::domain::Project)],
    users: &[User],
    kind: Group,
    billable_filter: Option<bool>,
) -> Summary {
    let customer_by_id: BTreeMap<Uuid, &Customer> = customers.iter().map(|c| (c.id, c)).collect();
    // #256: index the per-entry lookups once (the Review D4 pattern already
    // used by `domain::generate_invoice`), instead of an `iter().find` scan
    // per entry per row.
    let user_by_id: std::collections::HashMap<Uuid, &User> =
        users.iter().map(|u| (u.id, u)).collect();
    let project_by_key: std::collections::HashMap<(Uuid, &str), &crate::domain::Project> = projects
        .iter()
        .map(|(cid, p)| ((*cid, p.code.0.as_str()), p))
        .collect();
    let user_name = |e: &Entry| -> String {
        e.user_id
            .and_then(|uid| user_by_id.get(&uid))
            .map(|u| u.name.clone())
            .unwrap_or_else(|| "(unattributed)".to_string())
    };
    let billable_hours: f64 = entries
        .iter()
        .filter(|e| e.billable)
        .map(|e| e.hours.0 as f64 / 100.0)
        .sum();
    let nonbillable_hours: f64 = entries
        .iter()
        .filter(|e| !e.billable)
        .map(|e| e.hours.0 as f64 / 100.0)
        .sum();
    let project_for = |e: &Entry| -> Option<&crate::domain::Project> {
        project_by_key
            .get(&(e.customer_id, e.project_code.0.as_str()))
            .copied()
    };
    let user_rate = |e: &Entry| -> Option<u64> {
        e.user_id
            .and_then(|uid| user_by_id.get(&uid))
            .map(|u| u.default_rate_minor)
    };

    // Rows keyed by (group, currency): effective_rates resolves the PROJECT
    // currency (#11), so ANY group can straddle currencies — a customer with
    // EUR and USD projects must never see cents added across currencies
    // (#186). The former comment claiming customer/project groups were
    // single-currency was false; weeks now follow the same rule as everything
    // else instead of a "|CURR" suffix hack on the visible key.
    let mut acc: BTreeMap<(String, String, String), AccRow> = BTreeMap::new();
    for e in entries {
        if let Some(want) = billable_filter
            && e.billable != want
        {
            continue;
        }
        let Some(customer) = customer_by_id.get(&e.customer_id) else {
            continue; // dangling reference: excluded from totals, never fabricated.
        };
        let project = project_for(e);
        let (currency, rate) = e.billing_rates(customer, project, user_rate(e));
        let label_customer = &customer.name;
        let (key, label) = group_key(e, kind, label_customer, &e.project_code.0, &user_name(e));
        let row = acc
            .entry((key, currency.0.clone(), label))
            .or_insert_with(|| AccRow {
                hours: 0.0,
                amount: 0,
                entries: 0,
            });
        row.hours += e.hours.0 as f64 / 100.0;
        row.amount = row.amount.saturating_add(e.billing_amount_minor(rate));
        row.entries += 1;
    }

    let mut rows: Vec<SummaryRow> = acc
        .into_iter()
        .map(|((key, cur, label), r)| SummaryRow {
            key,
            label,
            currency: cur,
            hours: round2(r.hours),
            amount_minor: r.amount,
            entries: r.entries,
        })
        .collect();
    rows.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.currency.cmp(&b.currency)));

    Summary {
        group: group_name(kind).to_owned(),
        rows,
        total_hours: round2(entries.iter().map(|e| e.hours.0 as f64 / 100.0).sum()),
        billable_hours: round2(billable_hours),
        nonbillable_hours: round2(nonbillable_hours),
    }
}

#[derive(Debug)]
struct AccRow {
    hours: f64,
    amount: u64,
    entries: usize,
}

// ----------------------------------------------------- monthly time report --

/// The hierarchy level a monthly time report groups by. Unlike `Group` (the
/// money-oriented summary) this is time only, so no currency or rate is
/// resolved and every total is a plain hour count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonthlyGroup {
    Customer,
    Project,
    Task,
}

impl MonthlyGroup {
    pub fn name(self) -> &'static str {
        match self {
            MonthlyGroup::Customer => "customer",
            MonthlyGroup::Project => "project",
            MonthlyGroup::Task => "task",
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MonthCell {
    pub month: String,
    pub hours: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MonthRow {
    pub key: String,
    pub label: String,
    pub hours: f64,
    pub entries: usize,
    /// One cell per entry in `MonthlyReport::months`, in the same order.
    pub months: Vec<MonthCell>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MonthlyReport {
    pub group: String,
    pub from: String,
    pub to: String,
    pub months: Vec<String>,
    pub rows: Vec<MonthRow>,
    pub total_hours: f64,
    pub billable_hours: f64,
    pub nonbillable_hours: f64,
}

/// Reference data needed to label a monthly report. Projects are keyed by
/// `(customer, code)`; tasks additionally carry their project code. A missing
/// reference never hides time — the row falls back to the code the entry
/// already stores.
pub struct MonthlyContext<'a> {
    pub customers: &'a [Customer],
    pub projects: &'a [(Uuid, crate::domain::Project)],
    pub tasks: &'a [(Uuid, String, crate::domain::Task)],
}

#[derive(Debug)]
struct MonthAcc {
    label: String,
    hundredths: u64,
    months: Vec<u64>,
    entries: usize,
}

fn month_key(date: NaiveDate) -> String {
    format!("{:04}-{:02}", date.year(), date.month())
}

/// Every month touched by an inclusive `from`/`to` range, oldest first. The
/// store caps a range at 400 days, so this is at most fourteen months and a
/// simple walk needs no calendar arithmetic beyond the December rollover.
fn months_in_range(from: NaiveDate, to: NaiveDate) -> Vec<String> {
    let mut out = Vec::new();
    let (mut year, mut month) = (from.year(), from.month());
    while (year, month) <= (to.year(), to.month()) {
        out.push(format!("{year:04}-{month:02}"));
        if month == 12 {
            year += 1;
            month = 1;
        } else {
            month += 1;
        }
    }
    out
}

fn task_label(task: Option<&crate::domain::Task>, code: &str) -> String {
    match task {
        Some(task) if !task.name.trim().is_empty() && task.name != code => {
            format!("{code} ({})", task.name)
        }
        _ => code.to_owned(),
    }
}

/// Hours per month, grouped by customer, project or task. The totals are
/// accumulated as integer hundredths, so no float error can creep into a cell
/// before it is formatted for JSON.
///
/// Rows are ordered by total hours (largest first), which is also the order the
/// GUI's stacked monthly chart draws them in. A task-grouped report includes a
/// `(no task)` row for untasked entries, so the row totals always reconcile
/// with the report total.
pub fn summarise_months(
    entries: &[Entry],
    ctx: MonthlyContext<'_>,
    kind: MonthlyGroup,
    billable_filter: Option<bool>,
    from: NaiveDate,
    to: NaiveDate,
) -> MonthlyReport {
    let months = months_in_range(from, to);
    let month_index: std::collections::HashMap<&str, usize> = months
        .iter()
        .enumerate()
        .map(|(i, month)| (month.as_str(), i))
        .collect();
    let customer_by_id: std::collections::HashMap<Uuid, &Customer> =
        ctx.customers.iter().map(|c| (c.id, c)).collect();
    let project_by_key: std::collections::HashMap<(Uuid, &str), &crate::domain::Project> = ctx
        .projects
        .iter()
        .map(|(cid, p)| ((*cid, p.code.0.as_str()), p))
        .collect();
    let task_by_key: std::collections::HashMap<(Uuid, &str, &str), &crate::domain::Task> = ctx
        .tasks
        .iter()
        .map(|(cid, pcode, task)| ((*cid, pcode.as_str(), task.code.0.as_str()), task))
        .collect();

    let total_hundredths: u64 = entries.iter().map(|e| u64::from(e.hours.0)).sum::<u64>();
    let billable_hundredths: u64 = entries
        .iter()
        .filter(|e| e.billable)
        .map(|e| u64::from(e.hours.0))
        .sum();

    let mut acc: BTreeMap<String, MonthAcc> = BTreeMap::new();
    for entry in entries {
        if let Some(want) = billable_filter
            && entry.billable != want
        {
            continue;
        }
        let Some(customer) = customer_by_id.get(&entry.customer_id) else {
            continue; // dangling reference: never fabricate a row for it.
        };
        let project_code = entry.project_code.0.as_str();
        let project = project_by_key
            .get(&(entry.customer_id, project_code))
            .copied();
        let (key, label) = match kind {
            MonthlyGroup::Customer => (customer.id.to_string(), customer.name.clone()),
            MonthlyGroup::Project => {
                let base = format!("{} / {project_code}", customer.name);
                let label = match project {
                    Some(project)
                        if !project.name.trim().is_empty() && project.name != project_code =>
                    {
                        format!("{base} ({})", project.name)
                    }
                    _ => base,
                };
                (format!("{}:{project_code}", customer.id), label)
            }
            MonthlyGroup::Task => match &entry.task_code {
                Some(task_code) => {
                    let code = task_code.0.as_str();
                    let task = task_by_key
                        .get(&(entry.customer_id, project_code, code))
                        .copied();
                    (
                        format!("{}:{project_code}:{code}", customer.id),
                        format!(
                            "{} / {project_code} / {}",
                            customer.name,
                            task_label(task, code)
                        ),
                    )
                }
                None => (
                    format!("{}:{project_code}:(no task)", customer.id),
                    format!("{} / {project_code} / (no task)", customer.name),
                ),
            },
        };
        let row = acc.entry(key).or_insert_with(|| MonthAcc {
            label,
            hundredths: 0,
            months: vec![0; months.len()],
            entries: 0,
        });
        row.hundredths = row.hundredths.saturating_add(u64::from(entry.hours.0));
        row.entries += 1;
        if let Some(&index) = month_index.get(month_key(entry.date).as_str()) {
            row.months[index] = row.months[index].saturating_add(u64::from(entry.hours.0));
        }
    }

    let mut rows: Vec<MonthRow> = acc
        .into_iter()
        .map(|(key, row)| MonthRow {
            key,
            label: row.label,
            hours: round2(row.hundredths as f64 / 100.0),
            entries: row.entries,
            months: months
                .iter()
                .enumerate()
                .map(|(i, month)| MonthCell {
                    month: month.clone(),
                    hours: round2(row.months[i] as f64 / 100.0),
                })
                .collect(),
        })
        .collect();
    rows.sort_by(|a, b| {
        b.hours
            .partial_cmp(&a.hours)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.label.cmp(&b.label))
    });

    MonthlyReport {
        group: kind.name().to_owned(),
        from: from.to_string(),
        to: to.to_string(),
        months,
        rows,
        total_hours: round2(total_hundredths as f64 / 100.0),
        billable_hours: round2(billable_hundredths as f64 / 100.0),
        nonbillable_hours: round2((total_hundredths - billable_hundredths) as f64 / 100.0),
    }
}

#[cfg(test)]
mod monthly_tests {
    use super::months_in_range;

    #[test]
    fn the_month_axis_crosses_a_year_boundary_in_order() {
        let from = chrono::NaiveDate::from_ymd_opt(2025, 11, 15).unwrap();
        let to = chrono::NaiveDate::from_ymd_opt(2026, 2, 2).unwrap();
        assert_eq!(
            months_in_range(from, to),
            vec!["2025-11", "2025-12", "2026-01", "2026-02"]
        );
        // A range inside one month yields exactly that month, and the first and
        // last month are included even when the bounds are mid-month.
        let from = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let to = chrono::NaiveDate::from_ymd_opt(2026, 10, 21).unwrap();
        assert_eq!(months_in_range(from, to), vec!["2026-10"]);
    }
}

// ----------------------------------------------------------- profitability --

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProfitRow {
    pub customer_id: String,
    pub label: String,
    pub currency: String,
    pub revenue_minor: u64,
    pub cost_minor: u64,
    pub margin_minor: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Profitability {
    pub rows: Vec<ProfitRow>,
}

/// Revenue (from non-draft invoices in the period) vs cost (billable-expense
/// amounts + labour: billable hours × each person's cost rate), per customer
/// AND currency (#186). Money is per-currency; no FX is applied (#29): an
/// invoice's currency, an expense's currency and a time entry's resolved
/// project currency (`#11`) each partition the ledger, so a customer working
/// in EUR and USD gets two honest rows, never a merged cents figure.
/// Reference data for profitability rows: who, in what currency, at what
/// cost. Grouped because `summarise_profit` needs the full hierarchy to
/// resolve entry currencies (#186).
pub struct ProfitContext<'a> {
    pub customers: &'a [Customer],
    pub projects: &'a [(Uuid, crate::domain::Project)],
    pub users: &'a [User],
}

pub fn summarise_profit(
    invoices: &[crate::domain::Invoice],
    expenses: &[crate::domain::Expense],
    entries: &[Entry],
    ctx: ProfitContext<'_>,
    from: NaiveDate,
    to: NaiveDate,
) -> Profitability {
    let customers = ctx.customers;
    let projects = ctx.projects;
    let users = ctx.users;
    use crate::domain::InvoiceStatus;
    // #256: build the per-entry indexes ONCE, outside the per-customer loop,
    // and group entries by customer, so the pass is O(records + entries) rather
    // than O(customers × entries).
    let user_by_id: std::collections::HashMap<Uuid, &User> =
        users.iter().map(|u| (u.id, u)).collect();
    let project_by_key: std::collections::HashMap<(Uuid, &str), &crate::domain::Project> = projects
        .iter()
        .map(|(cid, p)| ((*cid, p.code.0.as_str()), p))
        .collect();
    let mut entries_by_customer: std::collections::HashMap<Uuid, Vec<&Entry>> =
        std::collections::HashMap::new();
    for en in entries {
        entries_by_customer
            .entry(en.customer_id)
            .or_default()
            .push(en);
    }
    let mut rows: Vec<ProfitRow> = Vec::new();
    for c in customers {
        let entry_currency = |e: &Entry| -> String {
            if let Some(imported) = &e.imported {
                return imported.currency.0.clone();
            }
            project_by_key
                .get(&(e.customer_id, e.project_code.0.as_str()))
                .map(|p| p.currency.0.clone())
                .unwrap_or_else(|| c.currency.0.clone())
        };
        // (revenue, cost) accumulators keyed by currency.
        let mut ledger: std::collections::BTreeMap<String, (u64, u64)> =
            std::collections::BTreeMap::new();
        for i in invoices
            .iter()
            .filter(|i| !i.is_superseded())
            .filter(|i| i.customer_id == c.id && i.status != InvoiceStatus::Draft)
            .filter(|i| i.period_to >= from && i.period_from <= to)
        {
            let e = ledger.entry(i.currency.0.clone()).or_default();
            e.0 = e.0.saturating_add(i.total_minor);
        }
        for x in expenses
            .iter()
            .filter(|x| x.customer_id == c.id && x.date >= from && x.date <= to)
        {
            let e = ledger.entry(x.currency.0.clone()).or_default();
            e.1 = e.1.saturating_add(x.amount_minor);
        }
        if let Some(list) = entries_by_customer.get(&c.id) {
            for &en in list {
                if !((en.billable || en.imported.is_some()) && en.date >= from && en.date <= to) {
                    continue;
                }
                let rate = en
                    .user_id
                    .and_then(|uid| user_by_id.get(&uid).map(|u| u.cost_rate_minor))
                    .unwrap_or(0);
                let e = ledger.entry(entry_currency(en)).or_default();
                e.1 = e.1.saturating_add(en.imported_cost_minor(rate));
            }
        }
        for (currency, (revenue, cost)) in ledger {
            if revenue == 0 && cost == 0 {
                continue;
            }
            rows.push(ProfitRow {
                customer_id: c.id.to_string(),
                label: c.name.clone(),
                currency,
                revenue_minor: revenue,
                cost_minor: cost,
                margin_minor: margin_minor(revenue, cost),
            });
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.margin_minor));
    Profitability { rows }
}

// --------------------------------------------------------- invoice reports --

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct InvoiceReportRow {
    pub customer_id: String,
    pub label: String,
    pub currency: String,
    pub revenue_minor: u64,
    pub invoices: usize,
    /// Collected ledger total across the window (#114).
    pub paid_minor: u64,
    /// Still-collectable balance; written-off amounts are forgiven, not
    /// outstanding (#114).
    pub balance_minor: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct InvoiceReport {
    pub rows: Vec<InvoiceReportRow>,
    pub draft: usize,
    pub issued: usize,
    pub paid: usize,
    pub overdue: usize,
    /// Revenue by currency over the full non-draft set (#186): a single
    /// `total_revenue_minor` summed cents across currencies, which is not a
    /// meaningful number — consumers must use this map.
    pub revenue_by_currency: std::collections::BTreeMap<String, u64>,
    /// #114 additions: the two open/late lifecycle states.
    pub partly_paid: usize,
    pub written_off: usize,
}

/// Revenue by customer from non-draft invoices overlapping `[from, to]`, plus
/// overall status counts (#31).
pub fn invoice_report(
    invoices: &[crate::domain::Invoice],
    customers: &[Customer],
    from: NaiveDate,
    to: NaiveDate,
) -> InvoiceReport {
    use crate::domain::InvoiceStatus;
    let mut rows: Vec<InvoiceReportRow> = Vec::new();
    for c in customers {
        let mine: Vec<&crate::domain::Invoice> = invoices
            .iter()
            .filter(|i| !i.is_superseded())
            .filter(|i| i.customer_id == c.id && i.status != InvoiceStatus::Draft)
            .filter(|i| i.period_to >= from && i.period_from <= to)
            .collect();
        if mine.is_empty() {
            continue;
        }
        // #186: partition by INVOICE currency (projects carry independent
        // currencies since #11, so one customer can hold EUR and USD
        // invoices). Summing their cents into one row labeled with the
        // customer's default currency produced silently wrong money.
        let mut by_cur: std::collections::BTreeMap<String, Vec<&crate::domain::Invoice>> =
            std::collections::BTreeMap::new();
        for i in &mine {
            by_cur.entry(i.currency.0.clone()).or_default().push(i);
        }
        for (cur, invs) in by_cur {
            rows.push(InvoiceReportRow {
                customer_id: c.id.to_string(),
                label: c.name.clone(),
                currency: cur,
                revenue_minor: invs.iter().map(|i| i.total_minor).sum(),
                invoices: invs.len(),
                paid_minor: invs.iter().map(|i| i.paid_minor()).sum(),
                balance_minor: invs.iter().map(|i| i.balance_minor()).sum(),
            });
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.revenue_minor));
    InvoiceReport {
        rows,
        draft: invoices
            .iter()
            .filter(|i| i.status == InvoiceStatus::Draft)
            .count(),
        issued: invoices
            .iter()
            .filter(|i| i.status == InvoiceStatus::Issued)
            .count(),
        paid: invoices
            .iter()
            .filter(|i| i.status == InvoiceStatus::Paid)
            .count(),
        overdue: invoices
            .iter()
            .filter(|i| {
                i.status.is_open() && i.balance_minor() > 0 && i.due_date.is_some_and(|d| d < to)
            })
            .count(),
        revenue_by_currency: {
            let mut m: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
            for i in invoices.iter().filter(|i| i.status != InvoiceStatus::Draft) {
                let e = m.entry(i.currency.0.clone()).or_insert(0);
                *e = e.saturating_add(i.total_minor);
            }
            m
        },
        partly_paid: invoices
            .iter()
            .filter(|i| i.status == InvoiceStatus::PartlyPaid)
            .count(),
        written_off: invoices
            .iter()
            .filter(|i| i.status == InvoiceStatus::WrittenOff)
            .count(),
    }
}

/// CSV export of invoices for accountants / other tools (#31).
pub fn invoice_csv(invoices: &[crate::domain::Invoice], customers: &[Customer]) -> String {
    let name = |id: Uuid| {
        customers
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    };
    // #114: paid/balance columns appended (existing positions stable for
    // consumers that parse by index).
    let mut out = String::from(
        "number,customer,period_from,period_to,currency,total_minor,status,issued_at,due_date,paid_at,payment_reference,paid_minor,balance_minor,write_off_reason\n",
    );
    for i in invoices.iter().filter(|invoice| !invoice.is_superseded()) {
        let fields = [
            i.number.clone(),
            name(i.customer_id),
            i.period_from.format("%Y-%m-%d").to_string(),
            i.period_to.format("%Y-%m-%d").to_string(),
            i.currency.0.clone(),
            i.total_minor.to_string(),
            format!("{:?}", i.status).to_lowercase(),
            i.issued_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
            i.due_date
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            i.paid_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
            i.payment_reference.clone(),
            i.paid_minor().to_string(),
            i.balance_minor().to_string(),
            i.write_off_reason.clone(),
        ];
        let joined = fields
            .iter()
            .map(|f| csv_escape(f))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&joined);
        out.push('\n');
    }
    out
}

fn group_name(kind: Group) -> &'static str {
    match kind {
        Group::Customer => "customer",
        Group::Project => "project",
        Group::Person => "person",
        Group::Week => "week",
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// CSV export: one row per entry with the rate and amount that a report shows.
pub fn export_csv(
    entries: &[Entry],
    customers: &[Customer],
    projects: &[(Uuid, crate::domain::Project)],
    users: &[User],
    customer_filter: Option<Uuid>,
) -> String {
    let customer_by_id: BTreeMap<Uuid, &Customer> = customers.iter().map(|c| (c.id, c)).collect();
    // #256: index the per-entry lookups once, as in `summarise`.
    let user_by_id: std::collections::HashMap<Uuid, &User> =
        users.iter().map(|u| (u.id, u)).collect();
    let project_by_key: std::collections::HashMap<(Uuid, &str), &crate::domain::Project> = projects
        .iter()
        .map(|(cid, p)| ((*cid, p.code.0.as_str()), p))
        .collect();
    let project_for = |e: &Entry| -> Option<&crate::domain::Project> {
        project_by_key
            .get(&(e.customer_id, e.project_code.0.as_str()))
            .copied()
    };
    let user_rate = |e: &Entry| -> Option<u64> {
        e.user_id
            .and_then(|uid| user_by_id.get(&uid))
            .map(|u| u.default_rate_minor)
    };

    let mut out = String::new();
    out.push_str(
        "date,customer,project_code,task_code,hours,billable,currency,hourly_rate,amount,note\n",
    );
    for e in entries {
        if let Some(fid) = customer_filter
            && e.customer_id != fid
        {
            continue;
        }
        let Some(customer) = customer_by_id.get(&e.customer_id) else {
            continue;
        };
        let project = project_for(e);
        let (currency, rate) = e.billing_rates(customer, project, user_rate(e));
        let hours = e.hours.0 as f64 / 100.0;
        let amount = e.billing_amount_minor(rate);
        let fields = [
            e.date.format("%Y-%m-%d").to_string(),
            customer.name.clone(),
            e.project_code.0.clone(),
            e.task_code
                .as_ref()
                .map(|c| c.0.clone())
                .unwrap_or_default(),
            format!("{:.2}", hours),
            if e.billable {
                "yes".to_string()
            } else {
                "no".to_string()
            },
            currency.0.clone(),
            format!("{:.2}", rate as f64 / 100.0),
            format!("{:.2}", amount as f64 / 100.0),
            e.note.clone(),
        ];
        let joined = fields
            .iter()
            .map(|f| csv_escape(f))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&joined);
        out.push('\n');
    }
    out
}

/// Quote a CSV field and defuse spreadsheet formula injection (a leading
/// `=`, `+`, `-`, `@`, tab or CR becomes a text prefix).
fn csv_escape(field: &str) -> String {
    let mut s = field.to_owned();
    if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        s = format!("'{s}");
    }
    if s.contains(['"', ',', '\n', '\r']) {
        s = format!("\"{}\"", s.replace('"', "\"\""));
    }
    s
}
