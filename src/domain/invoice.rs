use super::customer::PaymentTerms;
use super::primitives::{Currency, FieldError, Hours, ProjectCode, is_zero_u16};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

// ---------------------------------------------------------------- invoices --

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvoiceStatus {
    Draft,
    Issued,
    /// Some recorded payments, balance still open (#114).
    #[serde(rename = "partly_paid")]
    PartlyPaid,
    Paid,
    /// Remaining balance forgiven: final, excluded from outstanding (#114).
    #[serde(rename = "written_off")]
    WrittenOff,
}

impl InvoiceStatus {
    /// Issued and not yet closed out: locks its entries (#18) and counts as
    /// receivable (#114).
    pub fn is_open(&self) -> bool {
        matches!(self, InvoiceStatus::Issued | InvoiceStatus::PartlyPaid)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Time,
    Expense,
    Fixed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoiceLine {
    pub kind: LineKind,
    pub date: NaiveDate,
    /// Time lines reference an entry; expense lines reference an expense.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expense_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_code: Option<ProjectCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_code: Option<ProjectCode>,
    /// Hours + rate apply to time lines only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hours: Option<Hours>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_minor: Option<u64>,
    /// Manual lines are re-priced by the server from quantity x unit price,
    /// so an edit payload may omit it (#143); it is always stored.
    #[serde(default)]
    pub amount_minor: u64,
    #[serde(default)]
    pub note: String,
    /// Manual lines (#143): quantity in HUNDREDTHS (150 = 1.50 units) and the
    /// snapshot unit price, so `amount_minor` is always recomputable with
    /// integer math. Absent on tracked lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_hundredths: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_price_minor: Option<u64>,
    /// Product vs Service on manual lines (#143/#147 catalog).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_kind: Option<LineItemKind>,
}

/// What a manual line sells (#143). Tracked lines need no value (time and
/// expenses are inherently services/costs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LineItemKind {
    Product,
    Service,
}

/// Integer money math for manual lines: hundredths x minor units rounded
/// half up to a minor unit — the same discipline as `Hours::amount_minor`.
#[must_use]
pub fn manual_amount_minor(quantity_hundredths: u32, unit_price_minor: u64) -> u64 {
    let numerator = u128::from(quantity_hundredths) * u128::from(unit_price_minor);
    u64::try_from((numerator + 50) / 100).unwrap_or(u64::MAX)
}

/// Percentages in hundredths of a percent applied to the integer subtotal.
#[must_use]
pub fn percent_of_minor(value_minor: u64, hundredths: u16) -> u64 {
    // hundredths-of-a-percent over 10_000, rounded half up to a minor unit.
    let numerator = u128::from(value_minor) * u128::from(hundredths);
    u64::try_from((numerator + 5_000) / 10_000).unwrap_or(u64::MAX)
}

/// Document money: discount off the subtotal, VAT on the net. Persisted
/// totals are always these values — never re-derived at render time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct InvoiceTotals {
    pub subtotal_minor: u64,
    pub discount_minor: u64,
    pub tax_minor: u64,
    pub total_minor: u64,
}

#[must_use]
pub fn invoice_totals(
    subtotal_minor: u64,
    tax_hundredths: u16,
    discount_hundredths: u16,
) -> InvoiceTotals {
    let discount_minor = percent_of_minor(subtotal_minor, discount_hundredths);
    let net = subtotal_minor.saturating_sub(discount_minor);
    let tax_minor = percent_of_minor(net, tax_hundredths);
    InvoiceTotals {
        subtotal_minor,
        discount_minor,
        tax_minor,
        total_minor: net.saturating_add(tax_minor), // #190: guard against a
                                                    // corrupt subtotal saturating into u64::MAX (#13/D); validated
                                                    // inputs never reach the ceiling.
    }
}

/// A manual line as submitted (#143); the server computes the amount.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualLineInput {
    pub description: String,
    pub item_kind: LineItemKind,
    /// Integer API: hundredths of a unit (150 = 1.50). No floats.
    pub quantity_hundredths: u32,
    pub unit_price_minor: u64,
    #[serde(default)]
    pub project_code: Option<ProjectCode>,
}

pub const MANUAL_LINE_MAX: usize = 100;

/// Shared manual-line bounds (#187): description, quantity and unit price
/// rules live in exactly one place. Field names are passed in because the
/// create payload calls the description `description` while a stored draft
/// line calls it `note`.
pub fn check_manual_line(
    description: &str,
    quantity_hundredths: u32,
    unit_price_minor: u64,
    desc_field: &str,
    qty_field: &str,
    price_field: &str,
    errors: &mut Vec<FieldError>,
) {
    let desc = description.trim();
    if desc.is_empty() || desc.chars().count() > 500 {
        errors.push(FieldError::new(
            desc_field,
            "required, at most 500 characters",
        ));
    }
    if quantity_hundredths == 0 || quantity_hundredths > 1_000_000 {
        errors.push(FieldError::new(
            qty_field,
            "must be between 0.01 and 10000.00 units",
        ));
    }
    if unit_price_minor > 100_000_000 {
        errors.push(FieldError::new(price_field, "at most 100000000"));
    }
}

/// Validate + convert manual line input; errors carry `lines[i].field` names.
///
/// `today` is threaded in rather than read off the wall clock: the domain never
/// calls `Utc::now()` itself (see `clock.rs`), so a manual-line draft stays
/// deterministic under `FixedClock`.
pub fn validate_manual_lines(
    lines: &[ManualLineInput],
    today: NaiveDate,
    errors: &mut Vec<FieldError>,
) -> Vec<InvoiceLine> {
    if lines.len() > MANUAL_LINE_MAX {
        errors.push(FieldError::new(
            "lines",
            format!("at most {MANUAL_LINE_MAX} lines"),
        ));
        return Vec::new();
    }
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            check_manual_line(
                &l.description,
                l.quantity_hundredths,
                l.unit_price_minor,
                &format!("lines[{i}].description"),
                &format!("lines[{i}].quantity_hundredths"),
                &format!("lines[{i}].unit_price_minor"),
                errors,
            );
            InvoiceLine {
                kind: LineKind::Fixed,
                date: today,
                entry_id: None,
                expense_id: None,
                project_code: l.project_code.clone(),
                task_code: None,
                hours: None,
                rate_minor: None,
                amount_minor: manual_amount_minor(l.quantity_hundredths, l.unit_price_minor),
                note: l.description.trim().to_string(),
                quantity_hundredths: Some(l.quantity_hundredths),
                unit_price_minor: Some(l.unit_price_minor),
                item_kind: Some(l.item_kind),
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<super::ImportedInvoice>,
    /// Set on a correction revision: the invoice this document supersedes. The
    /// original is left intact (its own document and PDF stay as evidence) and
    /// is only marked with [`Invoice::superseded_by`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Uuid>,
    /// Set on the superseded original: the active correction revision. A
    /// superseded invoice is history and is excluded from every active
    /// aggregate; its entries stay locked by the revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<Uuid>,
    #[serde(default, skip_serializing_if = "InvoiceDocument::is_empty")]
    pub document: InvoiceDocument,
    pub id: Uuid,
    pub number: String,
    pub customer_id: Uuid,
    pub currency: Currency,
    pub period_from: NaiveDate,
    pub period_to: NaiveDate,
    pub lines: Vec<InvoiceLine>,
    pub total_minor: u64,
    pub status: InvoiceStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<DateTime<Utc>>,
    /// Resolved due date at issue (#27, #116): payment terms come from the
    /// customer, else the org invoice template, else a net-14 fallback.
    /// None until issued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub payment_reference: String,
    /// VAT percent in hundredths (2100 = 21.00%) — manual/draft feature
    /// (#143); tracked invoices keep 0 and their sum-of-lines total.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub tax_hundredths: u16,
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub discount_hundredths: u16,
    /// Archived PDF document hint (#113): metadata about the immutable
    /// `invoices/<id>.pdf` written at issue time. `None` for drafts and for
    /// legacy issued invoices whose PDF has not been resolved yet (the first
    /// download renders it from the snapshot-locked invoice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf: Option<PdfHint>,
    /// Recorded payments ledger (#114). Legacy documents default to empty;
    /// a legacy `Paid` invoice without entries reports its full total as
    /// paid (see `paid_minor`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub payments: Vec<InvoicePayment>,
    /// Non-empty once written off: the reason for forgiving the balance (#114).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub write_off_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_off_at: Option<DateTime<Utc>>,
    /// The resolved invoice subject, frozen at issue. Empty on drafts and on
    /// legacy issued documents (which keep resolving live or fall back to the
    /// legacy `Invoice {n} from {org}` wording).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    /// The payment terms used to resolve [`Invoice::due_date`] at issue, so a
    /// document can print `(Net 30)` from its own snapshot instead of looking
    /// the customer up again (and can never disagree with `due_date`).
    /// `None` when no terms applied (the legacy net-14-from-period-to
    /// fallback) and on legacy documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms: Option<PaymentTerms>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceDocument {
    #[serde(default)]
    pub po_number: String,
    #[serde(default)]
    pub invoice_date: Option<NaiveDate>,
    #[serde(default)]
    pub due_date: Option<NaiveDate>,
    #[serde(default)]
    pub payment_terms: Option<crate::domain::PaymentTerms>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

impl InvoiceDocument {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// One retained entry of an invoice's activity timeline (Subtask A).
///
/// Persisted as a **dot-file sidecar** next to the invoice
/// (`invoices/.timeline.<id>.json`) and returned by
/// `GET /api/invoices/{id}/timeline`. It is a *display aid*, never a record of
/// authority: the invoice document and its archived PDF stay the source of
/// truth, so a missing or malformed sidecar simply reads as an empty timeline.
/// Recording an event must never touch invoice semantics — it cannot affect
/// [`InvoiceStatus::is_open`] or [`already_invoiced`], and it adds no field to
/// the closed [`Invoice`] schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceTimelineEvent {
    pub kind: InvoiceTimelineEventKind,
    /// Human-readable sentence for the GUI, e.g. `Invoice created.`
    pub description: String,
    /// Display name of the acting user. Never a password hash or any secret.
    pub actor_name: String,
    /// When it happened, from the injected `Clock` (RFC 3339 on the wire), the
    /// same serialisation as every other persisted timestamp.
    pub at: DateTime<Utc>,
    /// Optional per-kind structured extras (e.g. the recipient + From identity
    /// for `email_sent`). Omitted when absent so older events stay clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The stable, machine-readable timeline kinds. Subtask A emits `created`,
/// `issued`, `updated` and `email_sent`; Subtask B adds `sent`, `paid` and
/// `write_off`. `payment` stays reserved (a recorded payment is emitted as
/// `paid` today) so a later subtask can add it without a contract change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvoiceTimelineEventKind {
    Created,
    Issued,
    Updated,
    EmailSent,
    /// An explicit "marked as sent" transition (`POST /invoices/{id}/sent`).
    Sent,
    /// Money was received: a full or partial settlement of the invoice.
    Paid,
    /// The remaining balance was forgiven (#114).
    WriteOff,
    Payment,
}

/// The watermark stage a fresh render of an invoice document carries
/// (Subtask B). Derived state only: there is no persisted flag, and the
/// archived issue-time PDF stays write-once and byte-stable (#113), so the
/// watermark can only ever apply to an on-demand render.
///
/// Deterministic precedence, **Paid > Sent > Draft**:
///
/// 1. [`InvoiceWatermarkStage::Paid`] — the invoice has been paid. Both
///    [`InvoiceStatus::Paid`] **and** [`InvoiceStatus::PartlyPaid`] count: a
///    partial settlement means money has already been received, so the
///    document must not read as an unpaid one. `WrittenOff` does **not**
///    count: the balance was forgiven, never collected, so a written-off
///    invoice keeps whatever delivery stage it had.
/// 2. [`InvoiceWatermarkStage::Sent`] — a delivery has happened: **any**
///    timeline event with kind [`InvoiceTimelineEventKind::Sent`] or
///    [`InvoiceTimelineEventKind::EmailSent`]. An email send therefore marks
///    the invoice sent *automatically*, with no separate manual step.
/// 3. [`InvoiceWatermarkStage::Draft`] — anything else, an issued invoice that
///    has never been delivered included.
///
/// Pure, no I/O, no allocation: the timeline sidecar is the single source of
/// truth for "sent" and the invoice document for "paid".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceWatermarkStage {
    Draft,
    Sent,
    Paid,
}

impl InvoiceWatermarkStage {
    /// Stable lower-case name, for logs and tests (`paid`/`sent`/`draft`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            InvoiceWatermarkStage::Draft => "draft",
            InvoiceWatermarkStage::Sent => "sent",
            InvoiceWatermarkStage::Paid => "paid",
        }
    }
}

/// The watermark stage for `invoice` given its retained `events` (oldest or
/// newest first — the result does not depend on order). See
/// [`InvoiceWatermarkStage`] for the documented Paid > Sent > Draft precedence.
#[must_use]
pub fn invoice_watermark_stage(
    invoice: &Invoice,
    events: &[InvoiceTimelineEvent],
) -> InvoiceWatermarkStage {
    if matches!(
        invoice.status,
        InvoiceStatus::Paid | InvoiceStatus::PartlyPaid
    ) {
        return InvoiceWatermarkStage::Paid;
    }
    if invoice_sent_at(events).is_some() {
        return InvoiceWatermarkStage::Sent;
    }
    InvoiceWatermarkStage::Draft
}

/// The earliest delivery timestamp the timeline implies, if any (Subtask B).
///
/// A delivery is a [`InvoiceTimelineEventKind::Sent`] (the manual
/// `POST /invoices/{id}/sent` transition) or an
/// [`InvoiceTimelineEventKind::EmailSent`] (the `/email` route), so both count
/// and the derived `sent_at` needs no field on the closed [`Invoice`] schema.
/// The *earliest* such event wins and the result is independent of the order
/// the sidecar stores its events in — that is what makes the manual transition
/// idempotent: re-marking an already-sent invoice reports the original
/// timestamp rather than minting a second one.
#[must_use]
pub fn invoice_sent_at(events: &[InvoiceTimelineEvent]) -> Option<DateTime<Utc>> {
    events
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                InvoiceTimelineEventKind::Sent | InvoiceTimelineEventKind::EmailSent
            )
        })
        .map(|e| e.at)
        .min()
}

/// Filesystem-safe display name for generated PDFs (#189: `store` and
/// `api::invoicing` kept two sanitizers that disagreed on `.`, so
/// `PdfHint.filename` could differ from the `Content-Disposition` name).
/// Alphanumerics, `-` and `_` survive; everything else becomes `_`.
#[must_use]
pub fn safe_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// One recorded payment against an invoice (#114). Amounts are minor units;
/// `method` names how the settlement arrived — `manual` for a payment recorded
/// at the desk, or a free-text label (bank transfer, cheque) passed by the caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvoicePayment {
    pub id: Uuid,
    pub amount_minor: u64,
    pub received_at: DateTime<Utc>,
    pub reference: String,
    pub method: String,
}

/// Metadata for a PDF archived next to the invoice document (#113). Lets the
/// GUI say "document on file" without reading the file; `sha256` is the
/// download `ETag` and the archive's verification handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfHint {
    /// Sanitised download filename, e.g. `INV-0001.pdf` (derived from
    /// `Invoice::number`, never from user input).
    pub filename: String,
    /// Size of the archived file in bytes.
    pub bytes: u64,
    /// Lower-case hex SHA-256 of the archived bytes.
    pub sha256: String,
    /// When the archive was written (issue time, or lazy first-download for
    /// legacy invoices).
    pub archived_at: DateTime<Utc>,
}

impl Invoice {
    /// True once a correction revision replaced this document. It stays
    /// readable in history, but no aggregate counts it as an active invoice.
    #[must_use]
    pub fn is_superseded(&self) -> bool {
        self.superseded_by.is_some()
    }

    /// True on the active correction revision of another invoice.
    #[must_use]
    pub fn is_revision(&self) -> bool {
        self.supersedes.is_some()
    }
    /// Sum of the payment ledger (integer minor units). A legacy `Paid`
    /// document predating #114 carries no ledger: its total counts as paid
    /// so receivables are never overstated by the migration.
    #[must_use]
    pub fn paid_minor(&self) -> u64 {
        let ledger = self.payments.iter().fold(0_u64, |sum, payment| {
            sum.saturating_add(payment.amount_minor)
        });
        let ledger = ledger.saturating_add(
            self.imported
                .as_ref()
                .map_or(0, |imported| imported.opening_paid_minor),
        );
        if self.imported.is_none() && ledger == 0 && self.status == InvoiceStatus::Paid {
            self.total_minor
        } else {
            ledger
        }
    }

    /// What remains collectable (#114). A written-off balance is forgiven:
    /// it is not outstanding, so this returns 0 for that state.
    #[must_use]
    pub fn balance_minor(&self) -> u64 {
        if self.status == InvoiceStatus::WrittenOff {
            return 0;
        }
        self.total_minor.saturating_sub(self.paid_minor())
    }
}

/// Aggregate of invoice states for the dashboard (#27, #114).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct InvoiceSummary {
    pub draft: usize,
    pub issued: usize,
    pub overdue: usize,
    pub paid: usize,
    /// Issued with a partially settled balance (#114).
    pub partly_paid: usize,
    /// Forgiven invoices, excluded from outstanding (#114).
    pub written_off: usize,
    /// Outstanding balances (issued + partly paid) keyed by currency.
    pub outstanding: BTreeMap<String, u64>,
}

/// Roll up invoice states as of `today`; an issued invoice past its due date
/// counts as overdue and its total is outstanding.
pub fn summarise_invoices(invoices: &[Invoice], today: NaiveDate) -> InvoiceSummary {
    let mut s = InvoiceSummary {
        draft: 0,
        issued: 0,
        overdue: 0,
        paid: 0,
        partly_paid: 0,
        written_off: 0,
        outstanding: BTreeMap::new(),
    };
    // A superseded original and its correction revision share a number: only
    // the active revision counts, the original stays history.
    for inv in invoices.iter().filter(|invoice| !invoice.is_superseded()) {
        match inv.status {
            InvoiceStatus::Draft => s.draft += 1,
            InvoiceStatus::Paid => s.paid += 1,
            InvoiceStatus::WrittenOff => s.written_off += 1,
            InvoiceStatus::Issued => s.issued += 1,
            InvoiceStatus::PartlyPaid => s.partly_paid += 1,
        }
        // Overdue + outstanding follow the BALANCE (#114): a settled or
        // forgiven invoice is neither, and a partial payment pulls only the
        // remainder into receivables.
        if inv.status.is_open() && inv.balance_minor() > 0 {
            if inv.due_date.is_some_and(|d| d < today) {
                s.overdue += 1;
            }
            // #190: saturating — a corrupt document can carry u64::MAX
            // amounts (the saturation ceiling), and a plain `+=` would
            // panic in debug / wrap in release on the receivables total.
            let outstanding = s.outstanding.entry(inv.currency.0.clone()).or_insert(0);
            *outstanding = outstanding.saturating_add(inv.balance_minor());
        }
    }
    s
}

/// Why invoice generation was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum InvoiceError {
    /// The period's billable work spans more than one currency; an invoice is
    /// single-currency, so the operator must resolve it (per ADR-001).
    MixedCurrency { a: String, b: String },
    /// No billable, not-yet-invoiced work in the period.
    NothingToInvoice,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A legacy `invoices/<id>.json` (written before `subject`/`terms`
    /// existed) still loads, defaults both fields, and round-trips
    /// byte-identically — reading never rewrites a document.
    #[test]
    fn legacy_invoice_document_loads_and_round_trips_byte_identically() {
        // Exactly the canonical bytes the store writes today: empty optionals
        // are omitted by `skip_serializing_if`, so a legacy file never holds
        // them. The new fields must default without disturbing that shape.
        let legacy = r#"{"id":"11111111-1111-1111-1111-111111111111","number":"INV-0001","customer_id":"22222222-2222-2222-2222-222222222222","currency":"EUR","period_from":"2026-10-01","period_to":"2026-10-31","lines":[],"total_minor":1000,"status":"issued","created_at":"2026-10-05T09:00:00Z","issued_at":"2026-10-05T09:00:00Z","due_date":"2026-10-19","payment_reference":""}"#;
        let invoice: Invoice = serde_json::from_str(legacy).unwrap();
        assert_eq!(invoice.subject, "");
        assert_eq!(invoice.terms, None);
        assert_eq!(serde_json::to_string(&invoice).unwrap(), legacy);
    }

    /// A timeline event stamped at a fixed epoch second, so `min` is testable
    /// without a clock.
    fn tl(kind: InvoiceTimelineEventKind, epoch_secs: i64) -> InvoiceTimelineEvent {
        InvoiceTimelineEvent {
            kind,
            description: String::new(),
            actor_name: "Tester".into(),
            at: DateTime::from_timestamp(epoch_secs, 0).unwrap(),
            meta: None,
        }
    }

    #[test]
    fn watermark_stage_ignores_lifecycle_and_forgiven_states() {
        let invoice = inv(InvoiceStatus::Issued, 100, None);
        // No events at all: nothing has been delivered.
        assert_eq!(
            invoice_watermark_stage(&invoice, &[]),
            InvoiceWatermarkStage::Draft
        );
        // Created/issued/updated are lifecycle, not delivery: still draft.
        let lifecycle = [
            tl(InvoiceTimelineEventKind::Created, 10),
            tl(InvoiceTimelineEventKind::Issued, 20),
            tl(InvoiceTimelineEventKind::Updated, 30),
        ];
        assert_eq!(
            invoice_watermark_stage(&invoice, &lifecycle),
            InvoiceWatermarkStage::Draft
        );
        // A written-off balance is never "paid" (it was forgiven, not
        // collected), so the delivery stage decides on its own.
        assert_eq!(
            invoice_watermark_stage(&inv(InvoiceStatus::WrittenOff, 100, None), &[]),
            InvoiceWatermarkStage::Draft
        );
        assert_eq!(
            invoice_watermark_stage(&inv(InvoiceStatus::WrittenOff, 100, None), &lifecycle),
            InvoiceWatermarkStage::Draft
        );
    }

    #[test]
    fn watermark_stage_is_sent_for_a_manual_or_emailed_delivery() {
        let invoice = inv(InvoiceStatus::Issued, 100, None);
        // The manual transition.
        assert_eq!(
            invoice_watermark_stage(&invoice, &[tl(InvoiceTimelineEventKind::Sent, 20)]),
            InvoiceWatermarkStage::Sent
        );
        // An email send marks the invoice sent automatically.
        assert_eq!(
            invoice_watermark_stage(&invoice, &[tl(InvoiceTimelineEventKind::EmailSent, 20)]),
            InvoiceWatermarkStage::Sent
        );
        // Both delivery kinds at once: still exactly one stage.
        let both = [
            tl(InvoiceTimelineEventKind::EmailSent, 20),
            tl(InvoiceTimelineEventKind::Sent, 30),
        ];
        assert_eq!(
            invoice_watermark_stage(&invoice, &both),
            InvoiceWatermarkStage::Sent
        );
        // A written-off invoice that was delivered keeps `Sent`.
        assert_eq!(
            invoice_watermark_stage(&inv(InvoiceStatus::WrittenOff, 100, None), &both),
            InvoiceWatermarkStage::Sent
        );
    }

    #[test]
    fn watermark_stage_paid_beats_sent() {
        let delivered = [tl(InvoiceTimelineEventKind::EmailSent, 20)];
        // Paid with no timeline events at all.
        assert_eq!(
            invoice_watermark_stage(&inv(InvoiceStatus::Paid, 100, None), &[]),
            InvoiceWatermarkStage::Paid
        );
        // Paid beats a delivery.
        assert_eq!(
            invoice_watermark_stage(&inv(InvoiceStatus::Paid, 100, None), &delivered),
            InvoiceWatermarkStage::Paid
        );
        // Money has been received on a partly paid invoice too.
        assert_eq!(
            invoice_watermark_stage(&inv(InvoiceStatus::PartlyPaid, 100, None), &delivered),
            InvoiceWatermarkStage::Paid
        );
        // Stable names for logs and tests.
        assert_eq!(InvoiceWatermarkStage::Paid.as_str(), "paid");
        assert_eq!(InvoiceWatermarkStage::Sent.as_str(), "sent");
        assert_eq!(InvoiceWatermarkStage::Draft.as_str(), "draft");
    }

    #[test]
    fn invoice_sent_at_is_the_earliest_delivery_whatever_the_order() {
        assert_eq!(invoice_sent_at(&[]), None);
        // Lifecycle and settlement events are not deliveries.
        let none = [
            tl(InvoiceTimelineEventKind::Created, 10),
            tl(InvoiceTimelineEventKind::Issued, 20),
            tl(InvoiceTimelineEventKind::Paid, 30),
        ];
        assert_eq!(invoice_sent_at(&none), None);
        // The oldest delivery wins, even when a newer one is stored first.
        let out_of_order = [
            tl(InvoiceTimelineEventKind::Sent, 50),
            tl(InvoiceTimelineEventKind::EmailSent, 20),
            tl(InvoiceTimelineEventKind::Paid, 60),
        ];
        assert_eq!(
            invoice_sent_at(&out_of_order),
            Some(DateTime::from_timestamp(20, 0).unwrap())
        );
    }

    #[test]
    fn manual_money_math_is_exact_integers() {
        // 1.50 units x 99.00 = 148.50 -> 14850 minor.
        assert_eq!(manual_amount_minor(150, 9900), 14850);
        // half-up rounding on the last minor unit: 3 x 333 / 100 = 9.99 -> 999
        assert_eq!(manual_amount_minor(300, 333), 999);
        assert_eq!(manual_amount_minor(1, 1), 0); // 0.01 x 0.01 = 0.00005 -> 0
        assert_eq!(percent_of_minor(10_000, 2100), 2100); // 21% VAT
        let t = invoice_totals(15_000, 2100, 1000);
        assert_eq!(
            (
                t.subtotal_minor,
                t.discount_minor,
                t.tax_minor,
                t.total_minor
            ),
            (15_000, 1_500, 2_835, 16_335)
        );
        // Zero percents are exact pass-through: tracked totals stay stable.
        let z = invoice_totals(18_000, 0, 0);
        assert_eq!(z.total_minor, 18_000);
        assert_eq!(z.discount_minor + z.tax_minor, 0);
    }

    #[test]
    fn manual_line_validation_guards() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let mut errs = Vec::new();
        let ok = ManualLineInput {
            description: "Seat".into(),
            item_kind: LineItemKind::Product,
            quantity_hundredths: 400,
            unit_price_minor: 2500,
            project_code: None,
        };
        let lines = validate_manual_lines(std::slice::from_ref(&ok), today, &mut errs);
        assert!(errs.is_empty());
        assert_eq!(lines[0].amount_minor, 10_000);
        assert_eq!(lines[0].kind, LineKind::Fixed);
        assert_eq!(lines[0].date, today);
        let bad = ManualLineInput {
            quantity_hundredths: 0,
            ..ok.clone()
        };
        let mut e2 = Vec::new();
        validate_manual_lines(&[bad], today, &mut e2);
        assert_eq!(e2.len(), 1);
        // > MANUAL_LINE_MAX lines short-circuits.
        let many = vec![ok; MANUAL_LINE_MAX + 1];
        let mut e3 = Vec::new();
        assert!(validate_manual_lines(&many, today, &mut e3).is_empty());
        assert!(!e3.is_empty());
    }

    #[test]
    fn summary_uses_balance_and_new_lifecycle_states() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 10).unwrap();
        let mk = |status: InvoiceStatus, total: u64, paid: u64| {
            let mut i = inv(
                status,
                total,
                Some(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()),
            );
            if paid > 0 {
                i.payments.push(InvoicePayment {
                    id: Uuid::new_v4(),
                    amount_minor: paid,
                    received_at: i.created_at,
                    reference: "r".into(),
                    method: "manual".into(),
                });
            }
            i
        };
        let invoices = vec![
            mk(InvoiceStatus::Issued, 10_000, 0),
            mk(InvoiceStatus::PartlyPaid, 10_000, 4_000),
            mk(InvoiceStatus::Paid, 10_000, 10_000),
            mk(InvoiceStatus::WrittenOff, 10_000, 2_000),
        ];
        let s = summarise_invoices(&invoices, today);
        assert_eq!(s.issued, 1);
        assert_eq!(s.partly_paid, 1);
        assert_eq!(s.paid, 1);
        assert_eq!(s.written_off, 1);
        // Outstanding = balances of open states only: 10000 + 6000.
        assert_eq!(s.outstanding.get("EUR").copied(), Some(16_000));
        // Both open invoices are past due; paid/written_off never overdue.
        assert_eq!(s.overdue, 2);
    }

    #[test]
    fn invoice_summary_counts_and_outstanding() {
        let today = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        let invoices = vec![
            inv(InvoiceStatus::Draft, 100, None),
            inv(
                InvoiceStatus::Issued,
                500,
                Some(NaiveDate::from_ymd_opt(2026, 1, 20).unwrap()),
            ), // overdue
            inv(
                InvoiceStatus::Issued,
                300,
                Some(NaiveDate::from_ymd_opt(2026, 6, 1).unwrap()),
            ), // not yet due
            inv(InvoiceStatus::Paid, 999, None),
        ];
        let s = summarise_invoices(&invoices, today);
        assert_eq!((s.draft, s.issued, s.overdue, s.paid), (1, 2, 1, 1));
        assert_eq!(s.outstanding.get("EUR"), Some(&(800))); // both issued, unpaid
    }

    #[test]
    fn invoice_summaries_saturate_corrupt_money_totals() {
        let today = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        let mut first = inv(InvoiceStatus::Issued, u64::MAX, None);
        let mut second = inv(InvoiceStatus::Issued, 10, None);
        first.currency = Currency("EUR".into());
        second.currency = Currency("EUR".into());
        let summary = summarise_invoices(&[first, second], today);
        assert_eq!(summary.outstanding.get("EUR"), Some(&u64::MAX));
    }

    #[test]
    fn invoice_totals_saturate_corrupt_tax_addition() {
        let totals = invoice_totals(u64::MAX, 10_000, 0);
        assert_eq!(totals.total_minor, u64::MAX);
    }
}
