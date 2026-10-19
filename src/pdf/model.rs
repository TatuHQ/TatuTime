use chrono::{DateTime, Datelike, NaiveDate, Utc};

use crate::domain::{
    Customer, Invoice, InvoiceLine, InvoiceWatermarkStage, LabelOverrides, LineItemKind, LineKind,
    PaymentRail, PaymentTerms, TermsKind,
};

// =============================================================== doc model ==

/// The issuing organisation. `doc_for` is the only place that reads it, and
/// the API layer resolves it from `org_profile.json` (#138) with the
/// `org_name` config key (#94) as the name fallback.
#[derive(Debug, Clone, PartialEq)]
pub struct Org {
    pub name: String,
    /// Company/registration id printed in the issuer block (#138).
    pub legal_id: String,
    /// Display prefix for [`Org::legal_id`] (`Tax ID`, `VAT`, `Company number`).
    /// Empty keeps the historical `VAT / company id` wording.
    pub legal_id_label: String,
    pub address: Option<crate::domain::Address>,
    /// '#rrggbb' document accent (#146); empty keeps the ink-only default.
    pub accent: String,
}

impl Org {
    /// Identity-only org (tests, config fallback): no legal id or address.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            legal_id: String::new(),
            legal_id_label: String::new(),
            address: None,
            accent: String::new(),
        }
    }

    /// Compose the document identity from the stored org profile, profile-first:
    /// a non-blank `profile.name` wins, otherwise `fallback_name` (the
    /// `org_name` config key). This is the ONE resolver behind invoice documents,
    /// invoice emails and the overdue-reminder job (SET-03): a Company-name
    /// change reaches every surface without duplicating the precedence rule.
    #[must_use]
    pub fn resolve(profile: &crate::domain::OrgProfile, fallback_name: &str) -> Self {
        let name = if profile.name.trim().is_empty() {
            fallback_name.to_string()
        } else {
            profile.name.trim().to_string()
        };
        Self {
            name,
            legal_id: profile.legal_id.clone(),
            legal_id_label: profile.legal_id_label.clone(),
            address: profile.address.clone(),
            accent: profile.accent.clone(),
        }
    }
}

/// Parse a '#rrggbb' accent into PDF floats (case-insensitive, no alpha).
/// Anything else is `None` — an unparseable accent must never corrupt bytes.
#[must_use]
pub fn parse_accent(hex: &str) -> Option<[f32; 3]> {
    let h = hex.trim().strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = |i: usize| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok();
    let (r, g, b) = (v(0)?, v(1)?, v(2)?);
    Some([
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    ])
}

/// Inline text with style flags. #116's markdown `**`/`*` map 1:1 onto these
/// via the base-14 `Helvetica-Bold` / `Helvetica-Oblique` faces.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
}

impl Span {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: false,
        }
    }
    pub fn bold(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: true,
            italic: false,
        }
    }
    pub fn italic(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: true,
        }
    }
}

/// A block of flowing content. Deliberately the same vocabulary #116's
/// markdown subset renders to; tables/links/images are out of scope.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
    Bullet(Vec<Span>),
    Numbered {
        number: u32,
        spans: Vec<Span>,
    },
    /// A `label: value` line (header fields, totals).
    KeyVal {
        key: String,
        spans: Vec<Span>,
    },
    /// A horizontal separator. `dashed` draws it as a broken rule — the
    /// payment-rail separator in the "Notes" section.
    Rule {
        dashed: bool,
    },
}

/// One row of the line-item table. Every string is pre-formatted; the
/// renderer never recomputes amounts.
#[derive(Debug, Clone, PartialEq)]
pub struct LineRow {
    /// What the line sells: `Service` (time and service items), `Product`
    /// (manual product items) or `Expense`. Derived, never stored.
    pub item_type: String,
    pub description: String,
    /// Quantity column text (`""` when the line has no quantity).
    pub quantity: String,
    /// Unit-price column text (`-` when the line has no unit price).
    pub unit_price: String,
    pub amount: String,
}

/// The invoice as a document — the #113/#116 seam. `header` is the left
/// identity column (`From` then `Invoice for`), `meta` the right metadata
/// column (subject, invoice id, dates, period), `blocks` free content
/// (customer notes from #116), `lines` the table, `totals` the money summary
/// (always present, printed from persisted minor units).
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub title: String,
    /// The invoice subject row; `None` on drafts and legacy documents, which
    /// render no row at all.
    pub subject: Option<String>,
    /// Left column: issuer (`From`) and customer (`Invoice for`) blocks.
    pub header: Vec<Block>,
    /// Right column metadata rows, printed beside the title/issuer block.
    pub meta: Vec<Block>,
    /// Table column captions: [item type, description, quantity, unit price]
    /// (#147/#subject); empty strings keep the built-in captions at layout
    /// time.
    pub column_labels: [String; 4],
    /// `100h 00min` over the time lines, when the invoice has any.
    pub total_hours: Option<String>,
    /// Content accent for the totals section (#146); None = no color ops.
    pub accent: Option<[f32; 3]>,
    /// On-demand watermark stage (Subtask C); `None` renders no stamp. Set by
    /// the caller — the archive path passes the generation-time stage, the
    /// preview the live one — never read from a global (see the module note).
    pub watermark: Option<InvoiceWatermarkStage>,
    pub blocks: Vec<Block>,
    pub lines: Vec<LineRow>,
    pub totals: Vec<Block>,
    pub footer: Vec<Block>,
}

/// The day a document was issued, for display: the issue timestamp when set,
/// otherwise the creation timestamp.
fn issue_day(created_at: DateTime<Utc>, issued_at: Option<DateTime<Utc>>) -> NaiveDate {
    (issued_at.unwrap_or(created_at)).date_naive()
}

/// The one date format every document surface uses: `dd/mm/yyyy`. Integer
/// fields only, no locale, no clock.
#[must_use]
pub fn format_date(date: NaiveDate) -> String {
    format!("{:02}/{:02}/{}", date.day(), date.month(), date.year())
}

/// A plain-text (no styles) key/val block.
fn kv(key: impl Into<String>, value: impl Into<String>) -> Block {
    Block::KeyVal {
        key: key.into(),
        spans: vec![Span::text(value.into())],
    }
}

/// Group the whole part of an amount with `,` thousands separators. Pure
/// string arithmetic on `u64` — no float ever touches a persisted value.
fn group_thousands(whole: u64) -> String {
    let digits = whole.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The document's symbol for a currency: `EUR → €`, `USD → $`, `GBP → £`,
/// anything else falls back to the code itself. Unknown currencies therefore
/// print their code as the symbol (`CHF CHF1,234.00`), never a guess.
fn currency_symbol(code: &str) -> &str {
    match code.to_ascii_uppercase().as_str() {
        "EUR" => "\u{20ac}",
        "USD" => "$",
        "GBP" => "\u{a3}",
        _ => code,
    }
}

/// Document money: the currency code, a space, then the symbol and the
/// grouped amount — `EUR €1,234.56`, or `EUR -€1,300.00` when `negative`
/// (sign before the symbol). Integer minor units in, string out: the same
/// discipline as [`crate::api::money_for_email`], which stays the EMAIL
/// format (`1,200.00 EUR`) so the two surfaces never silently converge.
#[must_use]
pub fn money_doc(minor: u64, currency: &str, negative: bool) -> String {
    let code = currency.trim();
    let symbol = if code.is_empty() {
        ""
    } else {
        currency_symbol(code)
    };
    let sign = if negative { "-" } else { "" };
    let body = format!(
        "{sign}{symbol}{}.{:02}",
        group_thousands(minor / 100),
        minor % 100
    );
    if code.is_empty() {
        body
    } else {
        format!("{code} {body}")
    }
}

/// Hundredths to a fixed two-decimal string ("8.50"), integer arithmetic.
fn two_decimals(hundredths: u32) -> String {
    format!("{}.{:02}", hundredths / 100, hundredths % 100)
}

/// Hundredths of an hour as `100h 00min`. Integer arithmetic only.
///
/// `hundredths` counts 1/100 h, so the whole-minute value is
/// `hundredths * 60 / 100`; the remainder of *that* division is the minute
/// part. Printing `hundredths % 100` directly (the pre-fix bug) labelled
/// hundredths as minutes: 21.75 h read `21h 75min` instead of `21h 45min`.
fn hours_minutes_text(hundredths: u64) -> String {
    let total_minutes = hundredths.saturating_mul(60) / 100;
    format!("{}h {:02}min", total_minutes / 60, total_minutes % 60)
}

/// The `(Net 30)` / `(Due on receipt)` suffix of the due-date row, taken from
/// the invoice's OWN terms snapshot — never re-read from the customer, so an
/// issued document can never disagree with its frozen `due_date`.
fn terms_suffix(terms: Option<&PaymentTerms>) -> Option<String> {
    let terms = terms?;
    match terms.kind {
        TermsKind::UponReceipt => Some("(Due on receipt)".to_string()),
        _ => terms.fixed_days().map(|days| format!("(Net {days})")),
    }
}

/// The one place invoice strings get formatted (#116 replaces *content*
/// here, never layout). Prints `due_date` **as stored** — the terms
/// resolution order belongs to #116, and an issued invoice is never
/// recomputed.
#[must_use]
pub fn doc_for(invoice: &Invoice, customer: &Customer, org: &Org) -> Doc {
    doc_for_labeled(invoice, customer, org, &LabelOverrides::default())
}

/// `doc_for` with the org's display-label overrides (#147). Default labels
/// keep the document's built-in wording.
#[must_use]
pub fn doc_for_labeled(
    invoice: &Invoice,
    customer: &Customer,
    org: &Org,
    labels: &LabelOverrides,
) -> Doc {
    let currency = invoice.currency.0.trim();
    let money = |minor: u64| money_doc(minor, currency, false);

    let from_label = labels.label(&labels.from, "From");
    let invoice_for_label = labels.label(&labels.invoice_for, "Invoice for");
    let subject_label = labels.label(&labels.subject, "Subject");
    let invoice_id_label = labels.label(&labels.invoice_id, "Invoice ID");
    let issue_label = labels.label(&labels.issue_date, "Issue date");
    let due_label = labels.label(&labels.due_date, "Due date");
    let item_type_label = labels.label(&labels.item_type, "Item type");
    let payments_label = labels.label(&labels.payments, "Payments");
    let amount_due_label = labels.label(&labels.amount_due, "Amount due");
    let payment_heading = labels.label(&labels.payment_heading, "Notes");

    let subject = {
        let s = invoice.subject.trim();
        (!s.is_empty()).then(|| s.to_string())
    };

    // Left column: who issues the document, then who it is for.
    let mut header = vec![Block::Heading {
        level: 2,
        spans: vec![Span::bold(from_label.to_string())],
    }];
    let org_name = org.name.trim();
    if !org_name.is_empty() {
        header.push(Block::Paragraph(vec![Span::text(org_name)]));
    }
    header.extend(address_blocks(org.address.as_ref(), AddressOrder::Issuer));
    if !org.legal_id.trim().is_empty() {
        let label = {
            let v = org.legal_id_label.trim();
            if v.is_empty() { "VAT / company id" } else { v }
        };
        header.push(kv(label, org.legal_id.trim()));
    }
    header.push(Block::Heading {
        level: 2,
        spans: vec![Span::bold(invoice_for_label.to_string())],
    });
    let customer_name = customer.name.trim();
    if !customer_name.is_empty() {
        header.push(Block::Paragraph(vec![Span::text(customer_name)]));
    }
    header.extend(address_blocks(
        customer.address.as_ref(),
        AddressOrder::Customer,
    ));
    if !customer.legal_id.trim().is_empty() {
        header.push(kv("Company number", customer.legal_id.trim()));
    }

    // Right column: the one-line identity of the invoice itself.
    let mut meta = Vec::new();
    if let Some(subject) = &subject {
        meta.push(kv(subject_label, subject.clone()));
    }
    meta.push(kv(invoice_id_label, invoice.number.clone()));
    meta.push(kv(
        issue_label,
        format_date(issue_day(invoice.created_at, invoice.issued_at)),
    ));
    if let Some(due) = invoice.due_date {
        // The suffix comes from the invoice's own snapshot: a legacy document
        // has no terms and prints the bare date.
        let value = match terms_suffix(invoice.terms.as_ref()) {
            Some(suffix) => format!("{} {}", format_date(due), suffix),
            None => format_date(due),
        };
        meta.push(kv(due_label, value));
    }
    meta.push(kv(
        "Period",
        format!(
            "{} to {}",
            format_date(invoice.period_from),
            format_date(invoice.period_to)
        ),
    ));

    // The number now lives in the metadata block, so the title is the plain
    // document name — but an install that never touched the new labels keeps
    // the historical `Invoice INV-0001` title.
    let labels_in_use = [
        &labels.from,
        &labels.invoice_for,
        &labels.invoice_id,
        &labels.issue_date,
        &labels.due_date,
        &labels.subject,
        &labels.payment_heading,
        &labels.item_type,
        &labels.payments,
        &labels.amount_due,
    ]
    .iter()
    .any(|v| !v.trim().is_empty());
    let title = if subject.is_some() || labels_in_use {
        "INVOICE".to_string()
    } else {
        format!("Invoice {}", invoice.number)
    };

    let total_hours = {
        let sum = invoice
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Time)
            .filter_map(|l| l.hours.map(|h| u64::from(h.0)))
            .sum::<u64>();
        invoice
            .lines
            .iter()
            .any(|l| l.kind == LineKind::Time && l.hours.is_some())
            .then(|| hours_minutes_text(sum))
    };

    let lines = invoice.lines.iter().map(|l| line_row(l, &money)).collect();
    let subtotal = invoice.lines.iter().map(|l| l.amount_minor).sum::<u64>();
    let mut totals = vec![Block::KeyVal {
        key: labels.label(&labels.subtotal, "Subtotal").to_string(),
        spans: vec![Span::text(money(subtotal))],
    }];
    if invoice.discount_hundredths > 0 {
        let disc = crate::domain::percent_of_minor(subtotal, invoice.discount_hundredths);
        totals.push(Block::KeyVal {
            key: labels.label(&labels.discount, "Discount").to_string(),
            spans: vec![Span::text(money(disc))],
        });
    }
    if invoice.tax_hundredths > 0 {
        let net = subtotal.saturating_sub(crate::domain::percent_of_minor(
            subtotal,
            invoice.discount_hundredths,
        ));
        let tax = crate::domain::percent_of_minor(net, invoice.tax_hundredths);
        totals.push(Block::KeyVal {
            key: labels.label(&labels.tax, "VAT").to_string(),
            spans: vec![Span::text(money(tax))],
        });
    }
    let paid = invoice.paid_minor();
    if paid > 0 {
        // Money received: show the settlement (negative) and what is left.
        totals.push(Block::KeyVal {
            key: payments_label.to_string(),
            spans: vec![Span::text(money_doc(paid, currency, true))],
        });
        totals.push(Block::KeyVal {
            key: amount_due_label.to_string(),
            spans: vec![Span::bold(money(invoice.total_minor.saturating_sub(paid)))],
        });
    } else {
        totals.push(Block::KeyVal {
            key: labels.label(&labels.total, "Total").to_string(),
            spans: vec![Span::bold(money(invoice.total_minor))],
        });
    }

    Doc {
        title,
        subject,
        header,
        meta,
        column_labels: [
            item_type_label.to_string(),
            labels.description.clone(),
            labels.quantity.clone(),
            labels.unit_price.clone(),
        ],
        total_hours,
        accent: parse_accent(&org.accent),
        // `doc_for` never stamps: the watermark is an explicit caller input.
        watermark: None,
        blocks: vec![],
        lines,
        totals,
        // "How to pay": the customer's structured rails when configured, else
        // the historical flat payment block. Absent (every legacy customer)
        // keeps the document at the credit line only. A configured
        // `payment_heading` labels both shapes; their built-in defaults stay
        // `Notes` (rails) and `Payment details` (the legacy flat block).
        footer: payment_footer(
            customer,
            &org.name,
            payment_heading,
            labels.label(&labels.payment_heading, "Payment details"),
        ),
    }
}

/// Which order the four address parts print in. The issuer block keeps its
/// historical street/postal/city/country order; the customer block follows
/// the postal convention of the reference document
/// (`street, city, postal code, country`).
#[derive(Clone, Copy)]
enum AddressOrder {
    Issuer,
    Customer,
}

/// One `Block::Paragraph` per non-empty address line, terminated by a comma
/// except on the last line. `wrap_spans` treats `\n` as a space, so each
/// line must be its own paragraph or a multi-line address collapses.
fn address_blocks(address: Option<&crate::domain::Address>, order: AddressOrder) -> Vec<Block> {
    let Some(a) = address else {
        return Vec::new();
    };
    let fields: [&str; 4] = match order {
        AddressOrder::Issuer => [&a.street, &a.postal_code, &a.city, &a.country],
        AddressOrder::Customer => [&a.street, &a.city, &a.postal_code, &a.country],
    };
    let lines: Vec<String> = fields
        .into_iter()
        .flat_map(|field| field.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    let last = lines.len().saturating_sub(1);
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let text = if i < last { format!("{line},") } else { line };
            Block::Paragraph(vec![Span::text(text)])
        })
        .collect()
}

/// The footer blocks of the built-in document: the payment section and the
/// generator credit. `heading` labels the structured multi-currency rails,
/// `flat_heading` the legacy single-account block; both follow the
/// `payment_heading` override when one is configured.
fn payment_footer(
    customer: &Customer,
    org_name: &str,
    heading: &str,
    flat_heading: &str,
) -> Vec<Block> {
    let mut footer: Vec<Block> = Vec::new();
    if let Some(p) = &customer.payment_details {
        let rails: Vec<&PaymentRail> = p
            .rails
            .iter()
            .filter(|r| !r.currency.trim().is_empty())
            .collect();
        if !rails.is_empty() {
            // Multi-currency: one block per rail, separated by a dashed rule.
            footer.push(Block::Heading {
                level: 2,
                spans: vec![Span::bold(heading.to_string())],
            });
            for (i, rail) in rails.into_iter().enumerate() {
                if i > 0 {
                    footer.push(Block::Rule { dashed: true });
                }
                footer.extend(rail_blocks(rail));
            }
        } else {
            // Single-rail fallback: the historical flat block, unchanged.
            let rows = p.rows();
            let instructions = p.instructions.trim();
            if !rows.is_empty() || !instructions.is_empty() {
                footer.push(Block::Heading {
                    level: 2,
                    spans: vec![Span::bold(flat_heading.to_string())],
                });
                for (label, value) in rows {
                    footer.push(kv(label, value));
                }
                for line in instructions.lines().filter(|l| !l.trim().is_empty()) {
                    footer.push(Block::Paragraph(vec![Span::text(line.trim().to_string())]));
                }
            }
        }
    }
    footer.push(Block::Paragraph(vec![Span::italic(format!(
        "Generated by {org_name} on TucanoTime."
    ))]));
    footer
}

/// One currency rail: `For all EUR Payments;`, the account identity lines,
/// then the labelled bank rows and the free-text instructions.
fn rail_blocks(rail: &PaymentRail) -> Vec<Block> {
    let non_empty = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let mut out = vec![Block::Paragraph(vec![Span::bold(format!(
        "For all {} Payments;",
        rail.currency.trim()
    ))])];
    for line in rail
        .account_name
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        out.push(Block::Paragraph(vec![Span::text(line.to_string())]));
    }
    if let Some(iban) = non_empty(&rail.iban) {
        out.push(kv("IBAN", iban));
    }
    let swift = match (non_empty(&rail.bic), non_empty(&rail.swift)) {
        (Some(bic), Some(swift)) => Some(format!("{bic} / {swift}")),
        (bic, swift) => bic.or(swift),
    };
    if let Some(swift) = swift {
        out.push(kv("Swift/BIC", swift));
    }
    let bank = match (non_empty(&rail.bank_name), non_empty(&rail.bank_address)) {
        (Some(name), Some(address)) => Some(format!("{name}, {address}")),
        (name, address) => name.or(address),
    };
    if let Some(bank) = bank {
        out.push(kv("Bank name and address", bank));
    }
    if let Some(value) = non_empty(&rail.account_number) {
        out.push(kv("Account number", value));
    }
    if let Some(value) = non_empty(&rail.account_type) {
        out.push(kv("Account type", value));
    }
    if let Some(value) = non_empty(&rail.routing_number) {
        out.push(kv("Routing number", value));
    }
    if let Some(instructions) = non_empty(&rail.instructions) {
        for line in instructions
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            out.push(Block::Paragraph(vec![Span::text(line.to_string())]));
        }
    }
    out
}

fn line_row(line: &InvoiceLine, money: &impl Fn(u64) -> String) -> LineRow {
    let mut desc: Vec<String> = Vec::new();
    if let Some(code) = &line.project_code {
        desc.push(code.0.clone());
    }
    if let Some(task) = &line.task_code {
        desc.push(task.0.clone());
    }
    if !line.note.trim().is_empty() {
        desc.push(line.note.trim().to_string());
    }
    if desc.is_empty() {
        desc.push(
            match line.kind {
                LineKind::Time => "Time",
                LineKind::Expense => "Expense",
                LineKind::Fixed => "Fixed charge",
            }
            .to_string(),
        );
    }
    let item_type = match line.kind {
        LineKind::Time => "Service",
        LineKind::Expense => "Expense",
        LineKind::Fixed => match line.item_kind {
            Some(LineItemKind::Product) => "Product",
            Some(LineItemKind::Service) => "Service",
            None => "Fixed",
        },
    };
    let (quantity, unit_price) = match line.kind {
        LineKind::Time => (
            line.hours.map(|h| two_decimals(h.0)).unwrap_or_default(),
            line.rate_minor.map(money).unwrap_or_else(|| "-".into()),
        ),
        LineKind::Fixed => match (line.quantity_hundredths, line.unit_price_minor) {
            (Some(quantity), Some(price)) => (two_decimals(quantity), money(price)),
            _ => (String::new(), "-".into()),
        },
        LineKind::Expense => (String::new(), "-".into()),
    };
    LineRow {
        item_type: item_type.to_string(),
        description: desc.join(" - "),
        quantity,
        unit_price,
        amount: money(line.amount_minor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Address, Currency, Customer, InvoiceLine, InvoicePayment, InvoiceStatus, LineKind,
        PaymentDetails, PaymentTerms, TermsKind,
    };
    use crate::pdf::render_invoice_pdf;
    use chrono::TimeZone;
    use uuid::Uuid;

    fn sample_invoice() -> Invoice {
        Invoice {
            id: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
            number: "INV-0001".into(),
            customer_id: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
            currency: Currency("EUR".into()),
            period_from: chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
            period_to: chrono::NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            lines: vec![
                InvoiceLine {
                    kind: LineKind::Time,
                    date: chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
                    entry_id: None,
                    expense_id: None,
                    project_code: Some(crate::domain::ProjectCode("WEB-1".into())),
                    task_code: None,
                    hours: Some(crate::domain::Hours(800)),
                    rate_minor: Some(9000),
                    amount_minor: 72000,
                    note: String::new(),
                    quantity_hundredths: None,
                    unit_price_minor: None,
                    item_kind: None,
                },
                InvoiceLine {
                    kind: LineKind::Expense,
                    date: chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
                    entry_id: None,
                    expense_id: None,
                    project_code: None,
                    task_code: None,
                    hours: None,
                    rate_minor: None,
                    amount_minor: 12500,
                    note: "Flight tickets".into(),
                    quantity_hundredths: None,
                    unit_price_minor: None,
                    item_kind: None,
                },
            ],
            total_minor: 84500,
            status: InvoiceStatus::Issued,
            created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            issued_at: Some(Utc.timestamp_opt(1_760_000_000, 0).unwrap()),
            due_date: Some(chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap()),
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
        }
    }

    fn customer() -> Customer {
        Customer {
            id: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
            name: "Acme Widgets GmbH".into(),
            currency: Currency("EUR".into()),
            default_rate_minor: 9000,
            active: true,
            email: "billing@acme.example".into(),
            payment_terms: None,
            invoice_notes: String::new(),
            invoice_subject: String::new(),
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,
            payment_details: None,
            legal_id: String::new(),
        }
    }

    fn org() -> Org {
        Org::named("Tucano")
    }

    /// The `(key, value)` pairs of a metadata column, in order.
    fn meta_rows(doc: &Doc) -> Vec<(&str, &str)> {
        doc.meta
            .iter()
            .filter_map(|b| match b {
                Block::KeyVal { key, spans } => Some((key.as_str(), spans[0].text.as_str())),
                _ => None,
            })
            .collect()
    }

    /// The header as a flat list of text lines (headings and paragraphs).
    fn header_texts(doc: &Doc) -> Vec<String> {
        let mut out = Vec::new();
        for block in &doc.header {
            match block {
                Block::Heading { spans, .. } | Block::Paragraph(spans) => {
                    for span in spans {
                        out.extend(span.text.lines().map(str::to_string));
                    }
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn money_document_is_symbol_first_grouped_and_integer_only() {
        assert_eq!(money_doc(123_456, "EUR", false), "EUR \u{20ac}1,234.56");
        assert_eq!(money_doc(130_000, "EUR", true), "EUR -\u{20ac}1,300.00");
        assert_eq!(money_doc(5, "USD", false), "USD $0.05");
        assert_eq!(money_doc(123_456, "GBP", false), "GBP \u{a3}1,234.56");
        // Unknown currencies fall back to their code as the symbol.
        assert_eq!(money_doc(100, "CHF", false), "CHF CHF1.00");
        // No currency: just the amount.
        assert_eq!(money_doc(100, "", false), "1.00");
        assert_eq!(money_doc(0, "EUR", false), "EUR \u{20ac}0.00");
    }

    #[test]
    fn dates_and_hours_format_without_floats() {
        assert_eq!(
            format_date(chrono::NaiveDate::from_ymd_opt(2026, 8, 3).unwrap()),
            "03/08/2026"
        );
        assert_eq!(
            format_date(chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()),
            "31/12/2026"
        );
        assert_eq!(hours_minutes_text(10_000), "100h 00min");
        assert_eq!(hours_minutes_text(800), "8h 00min");
        assert_eq!(hours_minutes_text(0), "0h 00min");
        // Fractional hours are minutes, not hundredths (#new): 21.75 h is
        // 21h 45min, never the invalid `21h 75min`.
        assert_eq!(hours_minutes_text(2175), "21h 45min");
        assert_eq!(hours_minutes_text(1650), "16h 30min");
        assert_eq!(hours_minutes_text(125), "1h 15min");
        assert_eq!(hours_minutes_text(1), "0h 00min");
        assert_eq!(two_decimals(10_000), "100.00");
    }

    #[test]
    fn terms_suffix_follows_the_snapshot() {
        let suffix = |terms: PaymentTerms| terms_suffix(Some(&terms));
        assert_eq!(
            suffix(PaymentTerms {
                kind: TermsKind::UponReceipt,
                days: None
            })
            .as_deref(),
            Some("(Due on receipt)")
        );
        assert_eq!(
            suffix(PaymentTerms {
                kind: TermsKind::Net15,
                days: None
            })
            .as_deref(),
            Some("(Net 15)")
        );
        assert_eq!(
            suffix(PaymentTerms {
                kind: TermsKind::Net30,
                days: None
            })
            .as_deref(),
            Some("(Net 30)")
        );
        assert_eq!(
            suffix(PaymentTerms {
                kind: TermsKind::Net45,
                days: None
            })
            .as_deref(),
            Some("(Net 45)")
        );
        assert_eq!(
            suffix(PaymentTerms {
                kind: TermsKind::Custom,
                days: Some(21)
            })
            .as_deref(),
            Some("(Net 21)")
        );
        assert_eq!(terms_suffix(None), None);
    }

    #[test]
    fn legacy_document_keeps_the_legacy_title_and_prints_no_subject() {
        let inv = sample_invoice();
        let doc = doc_for(&inv, &customer(), &org());
        assert_eq!(doc.title, "Invoice INV-0001");
        assert_eq!(doc.subject, None);
        // No subject row; the metadata is id, issue date, due date, period.
        let issue = format_date(issue_day(inv.created_at, inv.issued_at));
        assert_eq!(
            meta_rows(&doc),
            vec![
                ("Invoice ID", "INV-0001"),
                ("Issue date", issue.as_str()),
                ("Due date", "14/11/2026"),
                ("Period", "01/10/2026 to 31/10/2026"),
            ]
        );
        // A legacy invoice carries no terms snapshot: no suffix, no re-read.
        assert!(!meta_rows(&doc)[2].1.contains('('));
        // No address on either side, so the only customer text is its name.
        let texts = header_texts(&doc);
        assert_eq!(
            texts,
            vec!["From", "Tucano", "Invoice for", "Acme Widgets GmbH"]
        );
    }

    #[test]
    fn subject_and_terms_print_on_the_metadata_block() {
        let mut inv = sample_invoice();
        inv.subject = "Acme Widgets GmbH invoice for 08 2026".into();
        inv.terms = Some(PaymentTerms {
            kind: TermsKind::Net30,
            days: None,
        });
        inv.due_date = chrono::NaiveDate::from_ymd_opt(2026, 10, 15);
        let doc = doc_for(&inv, &customer(), &org());
        assert_eq!(doc.title, "INVOICE");
        assert_eq!(
            doc.subject.as_deref(),
            Some("Acme Widgets GmbH invoice for 08 2026")
        );
        let rows = meta_rows(&doc);
        assert_eq!(
            rows[0],
            ("Subject", "Acme Widgets GmbH invoice for 08 2026")
        );
        assert_eq!(rows[3], ("Due date", "15/10/2026 (Net 30)"));
    }

    #[test]
    fn invoice_for_block_prints_the_address_and_company_number() {
        let mut c = customer();
        c.name = "Acme Widgets GmbH".into();
        c.address = Some(Address {
            street: "9 Placeholder Road".into(),
            city: "Testville".into(),
            postal_code: "AA1 1AA".into(),
            country: "Freedonia".into(),
        });
        c.legal_id = "99999999".into();
        let mut o = org();
        o.legal_id = "00.000.000/0000-00".into();
        o.legal_id_label = "Tax ID".into();
        o.address = Some(Address {
            street: "12 Example Street".into(),
            city: "Sample City".into(),
            postal_code: "00000-000".into(),
            country: "Elsewhere".into(),
        });
        let doc = doc_for(&sample_invoice(), &c, &o);
        let texts = header_texts(&doc);
        assert_eq!(texts[0], "From");
        assert_eq!(texts[1], "Tucano");
        assert_eq!(texts[2], "12 Example Street,");
        assert_eq!(texts[3], "00000-000,");
        assert_eq!(texts[4], "Sample City,");
        assert_eq!(texts[5], "Elsewhere");
        // The issuer label comes from the org profile, the customer's is fixed.
        assert!(doc.header.iter().any(|b| matches!(
            b,
            Block::KeyVal { key, spans }
                if key == "Tax ID" && spans[0].text == "00.000.000/0000-00"
        )));
        assert_eq!(texts[6], "Invoice for");
        assert_eq!(texts[7], "Acme Widgets GmbH");
        assert_eq!(texts[8], "9 Placeholder Road,");
        assert_eq!(texts[9], "Testville,");
        assert_eq!(texts[10], "AA1 1AA,");
        assert_eq!(texts[11], "Freedonia");
        assert!(doc.header.iter().any(|b| matches!(
            b,
            Block::KeyVal { key, spans } if key == "Company number" && spans[0].text == "99999999"
        )));
    }

    #[test]
    fn money_never_floats_and_totals_are_printed_not_summed() {
        let doc = doc_for(&sample_invoice(), &customer(), &org());
        let totals: Vec<(&str, &str)> = doc
            .totals
            .iter()
            .filter_map(|b| match b {
                Block::KeyVal { key, spans } => Some((key.as_str(), spans[0].text.as_str())),
                _ => None,
            })
            .collect();
        // No payment recorded: Subtotal, then the bold Total (no Amount due).
        assert_eq!(
            totals,
            vec![
                ("Subtotal", "EUR \u{20ac}845.00"),
                ("Total", "EUR \u{20ac}845.00"),
            ]
        );
        // Line amounts format from minor units only.
        assert_eq!(doc.lines[0].amount, "EUR \u{20ac}720.00");
        assert_eq!(doc.lines[0].quantity, "8.00");
        assert_eq!(doc.lines[0].unit_price, "EUR \u{20ac}90.00");
        assert_eq!(doc.total_hours.as_deref(), Some("8h 00min"));
    }

    #[test]
    fn partly_paid_invoice_prints_payments_and_amount_due() {
        let mut inv = sample_invoice();
        inv.status = InvoiceStatus::PartlyPaid;
        inv.payments = vec![InvoicePayment {
            id: Uuid::nil(),
            amount_minor: 10_000,
            received_at: inv.created_at,
            reference: "BANK-1".into(),
            method: "manual".into(),
        }];
        let doc = doc_for(&inv, &customer(), &org());
        let totals: Vec<(&str, &str)> = doc
            .totals
            .iter()
            .filter_map(|b| match b {
                Block::KeyVal { key, spans } => Some((key.as_str(), spans[0].text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            totals,
            vec![
                ("Subtotal", "EUR \u{20ac}845.00"),
                ("Payments", "EUR -\u{20ac}100.00"),
                ("Amount due", "EUR \u{20ac}745.00"),
            ]
        );
    }

    #[test]
    fn due_date_prints_as_stored() {
        let mut inv = sample_invoice();
        inv.due_date = None; // draft: no due date line at all
        let doc = doc_for(&inv, &customer(), &org());
        assert!(
            !doc.meta
                .iter()
                .any(|b| matches!(b, Block::KeyVal { key, .. } if key == "Due date")),
        );
    }

    #[test]
    fn expense_and_fixed_rows_have_no_hours_or_rate() {
        let doc = doc_for(&sample_invoice(), &customer(), &org());
        assert_eq!(doc.lines[0].item_type, "Service");
        assert_eq!(doc.lines[1].item_type, "Expense");
        assert_eq!(doc.lines[1].quantity, "");
        assert_eq!(doc.lines[1].unit_price, "-");
        assert_eq!(doc.lines[1].description, "Flight tickets");
    }

    #[test]
    fn manual_lines_print_their_stored_quantity_and_price() {
        let mut inv = sample_invoice();
        inv.lines = vec![InvoiceLine {
            kind: LineKind::Fixed,
            date: chrono::NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
            entry_id: None,
            expense_id: None,
            project_code: None,
            task_code: None,
            hours: None,
            rate_minor: None,
            amount_minor: 10_000,
            note: "Office seat".into(),
            quantity_hundredths: Some(250),
            unit_price_minor: Some(4000),
            item_kind: Some(LineItemKind::Product),
        }];
        inv.total_minor = 10_000;
        let doc = doc_for(&inv, &customer(), &org());
        assert_eq!(doc.lines[0].item_type, "Product");
        assert_eq!(doc.lines[0].quantity, "2.50");
        assert_eq!(doc.lines[0].unit_price, "EUR \u{20ac}40.00");
        assert_eq!(doc.total_hours, None);
    }

    #[test]
    fn configured_payment_details_print_a_how_to_pay_footer() {
        let mut c = customer();
        c.payment_details = Some(PaymentDetails {
            iban: "EX000000000000000000".into(),
            bic: "EXAMPLEB".into(),
            reference: "Invoice number".into(),
            instructions: "Bank transfer in EUR only.".into(),
            ..Default::default()
        });
        let doc = doc_for(&sample_invoice(), &c, &org());
        let keys: Vec<&str> = doc
            .footer
            .iter()
            .filter_map(|b| match b {
                Block::KeyVal { key, .. } => Some(key.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            keys.contains(&"IBAN") && keys.contains(&"BIC/SWIFT"),
            "{keys:?}"
        );
        let rendered = render_invoice_pdf(&doc);
        assert!(rendered.starts_with(b"%PDF"));
    }

    #[test]
    fn payment_heading_override_labels_the_flat_block_too() {
        let mut c = customer();
        c.payment_details = Some(PaymentDetails {
            iban: "EX000000000000000000".into(),
            ..Default::default()
        });
        // Default: the legacy single block keeps its historical heading.
        let default_doc = doc_for(&sample_invoice(), &c, &org());
        assert!(matches!(
            &default_doc.footer[0],
            Block::Heading { spans, .. } if spans[0].text == "Payment details"
        ));
        // A configured override labels every payment section, flat or rails.
        let labels = LabelOverrides {
            payment_heading: "How to pay".into(),
            ..Default::default()
        };
        let doc = doc_for_labeled(&sample_invoice(), &c, &org(), &labels);
        assert!(matches!(
            &doc.footer[0],
            Block::Heading { spans, .. } if spans[0].text == "How to pay"
        ));
    }

    #[test]
    fn payment_rails_print_one_block_per_currency_separated_by_a_rule() {
        let mut c = customer();
        c.payment_details = Some(PaymentDetails {
            rails: vec![
                PaymentRail {
                    currency: "EUR".into(),
                    account_name: "Acme Widgets GmbH".into(),
                    iban: Some("EX000000000000000000".into()),
                    bic: Some("EXAMPLEB".into()),
                    ..Default::default()
                },
                PaymentRail {
                    currency: "USD".into(),
                    account_name: "Acme Widgets GmbH".into(),
                    account_number: Some("000000000000".into()),
                    routing_number: Some("000000000".into()),
                    swift: Some("EXAMPLEB".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        let doc = doc_for(&sample_invoice(), &c, &org());
        // Heading is the new "Notes" wording, rails only (no flat rows).
        assert!(matches!(
            &doc.footer[0],
            Block::Heading { spans, .. } if spans[0].text == "Notes"
        ));
        let paragraphs: Vec<&str> = doc
            .footer
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(spans) => Some(spans[0].text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            paragraphs.contains(&"For all EUR Payments;")
                && paragraphs.contains(&"For all USD Payments;"),
            "{paragraphs:?}"
        );
        let keys: Vec<(&str, &str)> = doc
            .footer
            .iter()
            .filter_map(|b| match b {
                Block::KeyVal { key, spans } => Some((key.as_str(), spans[0].text.as_str())),
                _ => None,
            })
            .collect();
        assert!(keys.contains(&("IBAN", "EX000000000000000000")), "{keys:?}");
        assert!(keys.contains(&("Swift/BIC", "EXAMPLEB")), "{keys:?}");
        assert!(keys.contains(&("Routing number", "000000000")), "{keys:?}");
        assert!(keys.contains(&("Swift/BIC", "EXAMPLEB")), "{keys:?}");
        // Exactly one separator, between the two rails.
        let rules = doc
            .footer
            .iter()
            .filter(|b| matches!(b, Block::Rule { dashed: true }))
            .count();
        assert_eq!(rules, 1);
    }

    #[test]
    fn no_payment_details_keeps_the_legacy_footer() {
        let doc = doc_for(&sample_invoice(), &customer(), &org());
        assert_eq!(doc.footer.len(), 1, "credit line only");
    }

    #[test]
    fn label_overrides_reach_the_new_rows() {
        let labels = LabelOverrides {
            from: "Issuer".into(),
            invoice_for: "Billed to".into(),
            invoice_id: "Number".into(),
            issue_date: "Issued".into(),
            due_date: "Pay by".into(),
            subject: "Regarding".into(),
            payment_heading: "How to pay".into(),
            item_type: "Kind".into(),
            payments: "Received".into(),
            amount_due: "Balance".into(),
            ..Default::default()
        };
        let mut inv = sample_invoice();
        inv.subject = "August work".into();
        let mut c = customer();
        c.payment_details = Some(PaymentDetails {
            rails: vec![PaymentRail {
                currency: "EUR".into(),
                account_name: "Holder".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let doc = doc_for_labeled(&inv, &c, &org(), &labels);
        assert_eq!(doc.title, "INVOICE");
        assert_eq!(doc.column_labels[0], "Kind");
        let rows = meta_rows(&doc);
        assert_eq!(rows[0].0, "Regarding");
        assert_eq!(rows[1].0, "Number");
        assert_eq!(rows[2].0, "Issued");
        assert_eq!(rows[3].0, "Pay by");
        assert!(matches!(
            &doc.footer[0],
            Block::Heading { spans, .. } if spans[0].text == "How to pay"
        ));
    }

    #[test]
    fn accent_parsing_and_color_ops_are_deterministic() {
        assert_eq!(
            parse_accent("#e95420"),
            Some([0.9137255, 0.32941177, 0.1254902])
        );
        assert!(parse_accent("e95420").is_none(), "missing # is refused");
        assert!(parse_accent("#12345").is_none());
        assert!(parse_accent("#gggggg").is_none());
        let o = Org {
            accent: "#e95420".into(),
            ..org()
        };
        let inv = sample_invoice();
        let a = render_invoice_pdf(&doc_for(&inv, &customer(), &o));
        let b = render_invoice_pdf(&doc_for(&inv, &customer(), &o));
        assert_eq!(a, b, "accented render stays deterministic");
        assert!(a.windows(3).any(|w| w == b" rg"), "fill color op emitted");
        // Default (no accent) has no color ops at all.
        let plain = render_invoice_pdf(&doc_for(&inv, &customer(), &org()));
        assert!(!plain.windows(3).any(|w| w == b" rg"));
        assert!(!plain.windows(3).any(|w| w == b" RG"));
    }
}
