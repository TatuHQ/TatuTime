use chrono::{DateTime, Utc};

use crate::domain::{Currency, Customer, Invoice, InvoiceLine, InvoiceWatermarkStage, LineKind};

// =============================================================== doc model ==

/// The issuing organisation. `doc_for` is the only place that reads it, and
/// the API layer resolves it from `org_profile.json` (#138) with the
/// `org_name` config key (#94) as the name fallback.
#[derive(Debug, Clone, PartialEq)]
pub struct Org {
    pub name: String,
    /// VAT / company id printed under the From block (#138).
    pub legal_id: String,
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
}

/// One row of the line-item table. Every string is pre-formatted; the
/// renderer never recomputes amounts.
#[derive(Debug, Clone, PartialEq)]
pub struct LineRow {
    pub date: String,
    pub description: String,
    /// `""` when the line has no hours (expense/fixed).
    pub hours: String,
    pub rate: String,
    pub amount: String,
}

/// The invoice as a document — the #113/#116 seam. `header`/`footer` frame
/// the page, `blocks` is free content (customer notes from #116), `lines` is
/// the table, `totals` the money summary (always present, printed from
/// `total_minor`).
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub title: String,
    /// Table column captions: [description, quantity, unit price] (#147);
    /// empty strings keep the built-in captions at layout time.
    pub column_labels: [String; 3],
    /// Content accent for the totals section (#146); None = no color ops.
    pub accent: Option<[f32; 3]>,
    /// On-demand watermark stage (Subtask C); `None` renders no stamp. Set by
    /// the caller — the archive path passes the generation-time stage, the
    /// preview the live one — never read from a global (see the module note).
    pub watermark: Option<InvoiceWatermarkStage>,
    pub header: Vec<Block>,
    pub blocks: Vec<Block>,
    pub lines: Vec<LineRow>,
    pub totals: Vec<Block>,
    pub footer: Vec<Block>,
}

/// The date a document was issued, for display: the issue timestamp when set,
/// otherwise the creation timestamp.
fn issue_date(created_at: DateTime<Utc>, issued_at: Option<DateTime<Utc>>) -> String {
    (issued_at.unwrap_or(created_at)).date_naive().to_string()
}

/// A plain-text (no styles) key/val block.
fn kv(key: impl Into<String>, value: impl Into<String>) -> Block {
    Block::KeyVal {
        key: key.into(),
        spans: vec![Span::text(value.into())],
    }
}

/// The one place invoice strings get formatted (#116 replaces *content*
/// here, never layout). Prints `due_date` **as stored** — the terms
/// resolution order belongs to #116, and an issued invoice is never
/// recomputed.
#[must_use]
pub fn doc_for(invoice: &Invoice, customer: &Customer, org: &Org) -> Doc {
    doc_for_labeled(
        invoice,
        customer,
        org,
        &crate::domain::LabelOverrides::default(),
    )
}

/// `doc_for` with the org's display-label overrides (#147). Default labels
/// keep the document byte-identical to the built-in wording.
#[must_use]
pub fn doc_for_labeled(
    invoice: &Invoice,
    customer: &Customer,
    org: &Org,
    labels: &crate::domain::LabelOverrides,
) -> Doc {
    let money = |minor: u64| {
        crate::api::money_for_email(minor, "")
            .trim_end()
            .to_string()
    };
    let mut header = vec![
        kv("From", org.name.clone()),
        kv("Bill to", customer.name.clone()),
        kv("Invoice number", invoice.number.clone()),
        kv(
            "Issue date",
            issue_date(invoice.created_at, invoice.issued_at),
        ),
        kv(
            "Period",
            format!("{} to {}", invoice.period_from, invoice.period_to),
        ),
    ];
    if let Some(due) = invoice.due_date {
        header.push(kv("Due date", due.to_string()));
    }
    // #138 company identity printed on the document header.
    if let Some(a) = &org.address {
        let lines: Vec<&str> = [
            a.street.trim(),
            a.postal_code.trim(),
            a.city.trim(),
            a.country.trim(),
        ]
        .into_iter()
        .filter(|v| !v.is_empty())
        .collect();
        if !lines.is_empty() {
            header.push(kv("From address", lines.join(", ")));
        }
    }
    if !org.legal_id.trim().is_empty() {
        header.push(kv("VAT / company id", org.legal_id.trim().to_string()));
    }
    let lines = invoice.lines.iter().map(|l| line_row(l, &money)).collect();
    let subtotal = invoice.lines.iter().map(|l| l.amount_minor).sum::<u64>();
    let mut totals = Vec::new();
    if subtotal != invoice.total_minor {
        totals.push(Block::KeyVal {
            key: labels.label(&labels.subtotal, "Subtotal").to_string(),
            spans: vec![Span::text(money_for_email_total(
                subtotal,
                &invoice.currency,
            ))],
        });
        if invoice.discount_hundredths > 0 {
            let disc = crate::domain::percent_of_minor(subtotal, invoice.discount_hundredths);
            totals.push(Block::KeyVal {
                key: labels.label(&labels.discount, "Discount").to_string(),
                spans: vec![Span::text(money_for_email_total(disc, &invoice.currency))],
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
                spans: vec![Span::text(money_for_email_total(tax, &invoice.currency))],
            });
        }
    }
    totals.push(Block::KeyVal {
        key: labels.label(&labels.total, "Total").to_string(),
        spans: vec![Span::bold(money_for_email_total(
            invoice.total_minor,
            &invoice.currency,
        ))],
    });
    Doc {
        title: format!("Invoice {}", invoice.number),
        column_labels: [
            labels.description.clone(),
            labels.quantity.clone(),
            labels.unit_price.clone(),
        ],
        accent: parse_accent(&org.accent),
        // `doc_for` never stamps: the watermark is an explicit caller input.
        watermark: None,
        header,
        blocks: vec![],
        lines,
        totals,
        // "How to pay": when the customer carries payment details, print
        // them under the totals. Absent (every legacy customer) keeps the
        // document byte-identical to before.
        footer: payment_footer(customer, &org.name),
    }
}

/// The footer blocks of the built-in document: an optional payment-details
/// section (rows + free-text instructions) and the generator credit.
fn payment_footer(customer: &Customer, org_name: &str) -> Vec<Block> {
    let mut footer: Vec<Block> = Vec::new();
    if let Some(p) = &customer.payment_details {
        let rows = p.rows();
        let instructions = p.instructions.trim();
        if !rows.is_empty() || !instructions.is_empty() {
            footer.push(Block::Heading {
                level: 2,
                spans: vec![Span::bold("Payment details".to_string())],
            });
            for (label, value) in rows {
                footer.push(Block::KeyVal {
                    key: label.to_string(),
                    spans: vec![Span::text(value)],
                });
            }
            for line in instructions.lines().filter(|l| !l.trim().is_empty()) {
                footer.push(Block::Paragraph(vec![Span::text(line.trim().to_string())]));
            }
        }
    }
    footer.push(Block::Paragraph(vec![Span::italic(format!(
        "Generated by {org_name} on TucanoTime."
    ))]));
    footer
}

/// Amount formatting shared with the email surface — minor units in, grouped
/// decimal out, no floats (`money_for_email` with the currency stripped back
/// off, used for table columns).
fn money_for_email_total(minor: u64, currency: &Currency) -> String {
    crate::api::money_for_email(minor, &currency.0)
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
    let (hours, rate) = match line.kind {
        LineKind::Time => (
            line.hours.map(|h| hours_text(h.0)).unwrap_or_default(),
            line.rate_minor.map(money).unwrap_or_else(|| "-".into()),
        ),
        _ => (String::new(), "-".into()),
    };
    LineRow {
        date: line.date.to_string(),
        description: desc.join(" - "),
        hours,
        rate,
        amount: money(line.amount_minor),
    }
}

/// Hundredths to a fixed two-decimal string ("8.50"), integer arithmetic.
fn hours_text(hundredths: u32) -> String {
    format!("{}.{:02}", hundredths / 100, hundredths % 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Currency, Customer, InvoiceLine, InvoiceStatus, LineKind};
    use crate::pdf::render_invoice_pdf;
    use chrono::TimeZone;

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
        }
    }

    fn customer() -> Customer {
        Customer {
            id: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
            name: "Capybara Solutions".into(),
            currency: Currency("EUR".into()),
            default_rate_minor: 9000,
            active: true,
            email: "billing@capybara.example".into(),
            payment_terms: None,
            invoice_notes: String::new(),
            invoice_subject: String::new(),
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,
            payment_details: None,
        }
    }

    fn org() -> Org {
        Org::named("Tucano")
    }

    #[test]
    fn money_never_floats_and_totals_are_printed_not_summed() {
        let doc = doc_for(&sample_invoice(), &customer(), &org());
        let total = match &doc.totals[0] {
            Block::KeyVal { spans, .. } => &spans[0].text,
            _ => panic!("totals are a KeyVal"),
        };
        assert_eq!(total, "845.00 EUR");
        // Line amounts format from minor units only.
        assert_eq!(doc.lines[0].amount, "720.00");
        assert_eq!(doc.lines[0].hours, "8.00");
        assert_eq!(doc.lines[0].rate, "90.00");
    }

    #[test]
    fn due_date_prints_as_stored() {
        let mut inv = sample_invoice();
        inv.due_date = None; // draft: no due date line at all
        let doc = doc_for(&inv, &customer(), &org());
        assert!(
            !doc.header
                .iter()
                .any(|b| matches!(b, Block::KeyVal { key, .. } if key == "Due date")),
        );
    }

    #[test]
    fn expense_and_fixed_rows_have_no_hours_or_rate() {
        let doc = doc_for(&sample_invoice(), &customer(), &org());
        assert_eq!(doc.lines[1].hours, "");
        assert_eq!(doc.lines[1].rate, "-");
        assert_eq!(doc.lines[1].description, "Flight tickets");
    }

    #[test]
    fn configured_payment_details_print_a_how_to_pay_footer() {
        let mut c = customer();
        c.payment_details = Some(crate::domain::PaymentDetails {
            iban: "DE89371314000000000000".into(),
            bic: "GENODED1".into(),
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
    fn no_payment_details_keeps_the_legacy_footer() {
        let doc = doc_for(&sample_invoice(), &customer(), &org());
        assert_eq!(doc.footer.len(), 1, "credit line only");
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
