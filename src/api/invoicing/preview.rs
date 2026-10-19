//! Sample-document fixtures shared by the template save gauntlet and the
//! unsaved-template PDF preview.
//!
//! The Settings editor shows a live preview of a candidate template before it
//! is saved, so the synthetic invoice/customer it renders against must be one
//! definition, used by both consumers:
//!
//! * `PUT /admin/invoice-template` keeps its extra gauntlet — a template that
//!   cannot render is rejected — and
//! * `POST /admin/invoice-template/preview` returns the rendered PDF of the
//!   **candidate** template, persisting nothing.
//!
//! The sample is deliberately representative (two time lines, one expense
//! line, a period, VAT, a due date, a subject, net-30 terms, a customer company
//! number and one EUR plus one USD payment rail) so every documented section
//! and `%variable%` has a value to show. Every value is synthetic. Nothing here
//! writes to the store.

use chrono::{NaiveDate, TimeZone, Utc};
use uuid::Uuid;

use crate::api::*;
use crate::domain::{
    Address, Contact, Currency, Invoice, InvoiceLine, InvoiceStatus, InvoiceTemplate, LineKind,
    PaymentDetails, PaymentRail, PaymentTerms, TermsKind,
};

/// A date that exists in every calendar (no leap-day edge) and keeps the
/// sample byte-stable across runs — the preview's ETag depends on it.
fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("sample date is valid")
}

/// The synthetic invoice every preview and save-gauntlet renders against.
///
/// Two time lines and one expense line, a period, a subject, net-30 terms, a
/// due date and totals with VAT: enough for `%line_count%`, `%total%`,
/// `%period_from%`, `%period_to%`, `%invoice_due_date%`, the subject/terms
/// rows and the label overrides to be visible.
#[must_use]
pub(crate) fn sample_invoice() -> Invoice {
    Invoice {
        id: Uuid::nil(),
        number: "INV-0000".into(),
        customer_id: Uuid::nil(),
        currency: Currency("EUR".into()),
        period_from: day(2026, 10, 1),
        period_to: day(2026, 10, 31),
        lines: vec![
            InvoiceLine {
                kind: LineKind::Time,
                date: day(2026, 10, 1),
                entry_id: None,
                expense_id: None,
                project_code: Some(crate::domain::ProjectCode("SAMPLE-1".into())),
                task_code: None,
                hours: Some(crate::domain::Hours(1200)),
                rate_minor: Some(9000),
                amount_minor: 108_000,
                note: "Consulting".into(),
                quantity_hundredths: None,
                unit_price_minor: None,
                item_kind: None,
            },
            InvoiceLine {
                kind: LineKind::Time,
                date: day(2026, 10, 8),
                entry_id: None,
                expense_id: None,
                project_code: Some(crate::domain::ProjectCode("SAMPLE-1".into())),
                task_code: None,
                hours: Some(crate::domain::Hours(450)),
                rate_minor: Some(9000),
                amount_minor: 40_500,
                note: "Support".into(),
                quantity_hundredths: None,
                unit_price_minor: None,
                item_kind: None,
            },
            InvoiceLine {
                kind: LineKind::Expense,
                date: day(2026, 10, 9),
                entry_id: None,
                expense_id: None,
                project_code: None,
                task_code: None,
                hours: None,
                rate_minor: None,
                amount_minor: 12_500,
                note: "Train tickets".into(),
                quantity_hundredths: None,
                unit_price_minor: None,
                item_kind: None,
            },
        ],
        // Subtotal 1,610.00 + 23% VAT 370.30 = 1,980.30 (integer minor units).
        total_minor: 198_030,
        status: InvoiceStatus::Draft,
        created_at: Utc
            .with_ymd_and_hms(2026, 10, 5, 9, 0, 0)
            .single()
            .expect("sample timestamp is valid"),
        issued_at: None,
        due_date: Some(day(2026, 11, 4)),
        paid_at: None,
        payment_reference: String::new(),
        pdf: None,
        payments: vec![],
        write_off_reason: String::new(),
        written_off_at: None,
        tax_hundredths: 2300,
        discount_hundredths: 0,
        // Frozen at issue in production; the sample shows both new document
        // sections (the subject row and the `(Net 30)` due-date suffix).
        subject: "Sample Customer invoice for 10 2026".into(),
        terms: Some(PaymentTerms {
            kind: TermsKind::Net30,
            days: None,
        }),
    }
}

/// The customer the sample invoice bills: a billing contact, a company
/// number and one EUR + one USD payment rail, so `%payment_details%`, the
/// `Company number` row and the document's multi-rail "Notes" section are
/// exercised.
#[must_use]
pub(crate) fn sample_customer() -> crate::domain::Customer {
    crate::domain::Customer {
        id: Uuid::nil(),
        name: "Sample Customer".into(),
        currency: Currency("EUR".into()),
        default_rate_minor: 9000,
        active: true,
        email: "billing@sample.test".into(),
        payment_terms: Some(PaymentTerms {
            kind: TermsKind::Net30,
            days: None,
        }),
        invoice_notes: String::new(),
        invoice_subject: String::new(),
        address: Some(Address {
            street: "12 Sample Street".into(),
            city: "Sampleville".into(),
            postal_code: "1000".into(),
            country: "BE".into(),
        }),
        contacts: vec![Contact {
            name: "Billing Contact".into(),
            role: "Accounts".into(),
            email: "billing@sample.test".into(),
            billing: true,
        }],
        tax_hundredths: 0,
        discount_hundredths: 0,
        payment_details: Some(PaymentDetails {
            bank_name: "Example Bank, 1 Sample Plaza, Testville".into(),
            account_name: "Sample Customer".into(),
            account_number: String::new(),
            iban: "EX000000000000000000".into(),
            bic: "EXAMPLEB".into(),
            reference: "Invoice number".into(),
            instructions: "Please quote the invoice number with your transfer.".into(),
            rails: vec![
                PaymentRail {
                    currency: "EUR".into(),
                    account_name: "Sample Customer".into(),
                    iban: Some("EX000000000000000000".into()),
                    bic: Some("EXAMPLEB".into()),
                    instructions: Some("For all Euro Payments; quote the invoice number.".into()),
                    ..Default::default()
                },
                PaymentRail {
                    currency: "USD".into(),
                    account_name: "Sample Customer".into(),
                    account_number: Some("000000000000".into()),
                    routing_number: Some("000000000".into()),
                    swift: Some("EXAMPLEB".into()),
                    instructions: Some("For all USD Payments; quote the invoice number.".into()),
                    ..Default::default()
                },
            ],
        }),
        legal_id: "0123456789".into(),
    }
}

/// The single validation pass behind both the save and the preview: unknown
/// `%tokens%`, oversized fields, control characters in the header-bound
/// subject and malformed payment terms. The error order (and so the `fields`
/// array) is unchanged from the original `PUT` handler.
#[must_use]
pub(crate) fn validate_template(template: &InvoiceTemplate) -> Vec<FieldError> {
    let mut errors = Vec::new();
    crate::template::validate_field(
        "subject",
        &template.subject,
        crate::template::SUBJECT_MAX,
        &mut errors,
    );
    // #S1: the subject is header-bound, so a CR/LF/NUL in it could fold a
    // header into the invoice email. Refused on write, never stripped. The body
    // and footer are document text (multi-line by design) and are untouched.
    if crate::email::has_control_characters(&template.subject) {
        errors.push(FieldError::new(
            "subject",
            "must be a single line without control characters",
        ));
    }
    crate::template::validate_field(
        "body",
        &template.body,
        crate::template::BODY_MAX,
        &mut errors,
    );
    crate::template::validate_field(
        "footer",
        &template.footer,
        crate::template::FOOTER_MAX,
        &mut errors,
    );
    crate::domain::validate_payment_terms(template.payment_terms.as_ref(), &mut errors);
    template.labels.validate(&mut errors);
    errors
}

/// Build the document of the sample invoice with `template`'s content applied.
///
/// Used by the `PUT` gauntlet (which only builds it, as the assertion that the
/// template renders) and by the preview endpoint (which renders the result).
#[must_use]
pub(crate) fn sample_document(
    template: &InvoiceTemplate,
    org: &crate::pdf::Org,
) -> crate::pdf::Doc {
    let invoice = sample_invoice();
    let customer = sample_customer();
    let mut doc = crate::pdf::doc_for_labeled(&invoice, &customer, org, &template.labels);
    let content = super::content_for(&invoice, &customer, template);
    crate::template::apply_doc_content(&mut doc, &content);
    doc
}

/// Render the candidate template against the sample document. Pure in
/// (template, org): the same pair always yields byte-identical output.
#[must_use]
pub(crate) fn render_sample_pdf(template: &InvoiceTemplate, org: &crate::pdf::Org) -> Vec<u8> {
    crate::pdf::render_invoice_pdf(&sample_document(template, org))
}

/// Bumped whenever the sample fixture or the renderer changes in a way that can
/// alter the bytes. Part of the input-keyed `ETag`, so a renderer upgrade can
/// never leave a stale tag looking current (SET-06).
const PREVIEW_ETAG_VERSION: u32 = 2;

/// The preview `ETag`, keyed on the **inputs** — canonical candidate-template
/// JSON, the resolved org profile and the renderer/sample version — so it can be
/// evaluated *before* rendering (SET-06). The sample invoice/customer are pure
/// constants covered by `PREVIEW_ETAG_VERSION`.
fn preview_etag(template: &InvoiceTemplate, org: &crate::pdf::Org) -> String {
    use sha2::{Digest, Sha256};
    let inputs = serde_json::json!({
        "version": PREVIEW_ETAG_VERSION,
        "template": template,
        "org": {
            "name": org.name,
            "legal_id": org.legal_id,
            "legal_id_label": org.legal_id_label,
            "address": org.address,
            "accent": org.accent,
        },
    });
    let canonical = serde_json::to_vec(&inputs).unwrap_or_default();
    crate::hex_lower(&Sha256::digest(canonical))
}

/// Whether an `If-None-Match` header matches `etag`. Handles the `*` form, a
/// comma-separated list and weak validators (`W/"…"`), exactly the shapes a
/// browser or a fetch client may send. A malformed header simply misses.
fn if_none_match_matches(headers: &axum::http::HeaderMap, etag: &str) -> bool {
    let Some(raw) = headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    raw.split(',').map(str::trim).any(|candidate| {
        let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
        candidate == "*" || candidate.trim_matches('"') == etag
    })
}

/// `POST /admin/invoice-template/preview` — render the **unsaved** candidate
/// template as a PDF, using the saved org profile (branding/accent/sender) and
/// the shared sample invoice.
///
/// Validates exactly like `PUT` (identical 422 `validation_failed` shapes) and
/// **persists nothing**: no template, invoice, archive or timeline. A command
/// on existing configuration, so **200** (C8), not 201. The `ETag` is a SHA-256
/// of the inputs (candidate template + resolved org + renderer/sample version),
/// so a conditional request can be answered **304 with no body** before any
/// rendering happens; the same `ETag` rides both responses (SET-06).
pub async fn invoice_template_preview(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    ValidJson(template): ValidJson<InvoiceTemplate>,
) -> ApiResult {
    let errors = validate_template(&template);
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let org = super::org_for(&app)?;
    let etag = preview_etag(&template, &org);
    let quoted = format!("\"{etag}\"");
    if if_none_match_matches(&headers, &etag) {
        tracing::debug!("invoice template preview not modified");
        return Ok((
            StatusCode::NOT_MODIFIED,
            [(axum::http::header::ETAG, quoted)],
        )
            .into_response());
    }
    let bytes = render_sample_pdf(&template, &org);
    tracing::debug!(
        bytes = bytes.len(),
        "invoice template pdf preview (candidate, nothing persisted)"
    );
    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/pdf".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "inline; filename=\"invoice-template-preview.pdf\"".to_string(),
            ),
            (axum::http::header::ETAG, quoted),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_is_representative_and_internally_consistent() {
        let invoice = sample_invoice();
        let customer = sample_customer();
        assert_eq!(invoice.lines.len(), 3, "two time lines + one expense");
        assert!(
            invoice.lines.iter().any(|l| l.kind == LineKind::Time),
            "time line present"
        );
        assert!(
            invoice.lines.iter().any(|l| l.kind == LineKind::Expense),
            "expense line present"
        );
        assert_eq!(
            invoice.lines.iter().map(|l| l.amount_minor).sum::<u64>(),
            161_000,
            "line amounts add up to the documented subtotal"
        );
        let totals = crate::domain::invoice_totals(161_000, invoice.tax_hundredths, 0);
        assert_eq!(totals.total_minor, invoice.total_minor);
        // The new document sections have representative data (phase 2).
        assert_eq!(invoice.subject, "Sample Customer invoice for 10 2026");
        assert_eq!(
            invoice.terms,
            Some(PaymentTerms {
                kind: TermsKind::Net30,
                days: None
            })
        );
        assert!(!customer.legal_id.is_empty(), "company number");
        let details = customer.payment_details.as_ref().expect("payment details");
        assert_eq!(details.rails.len(), 2, "one EUR and one USD rail");
        assert!(details.rails.iter().any(|r| r.currency == "EUR"));
        assert!(details.rails.iter().any(|r| r.currency == "USD"));
        assert!(
            customer
                .contacts
                .iter()
                .any(|c| c.billing && !c.email.is_empty()),
            "billing contact"
        );
    }

    /// The settings preview must show the new document sections too, not just
    /// the template content: subject row, company number, rail notes and the
    /// hours summary. The layout draws one literal per word, so match words.
    #[test]
    fn sample_document_shows_the_new_document_sections() {
        let doc = sample_document(
            &InvoiceTemplate::default(),
            &crate::pdf::Org::named("Sample Org"),
        );
        assert_eq!(
            doc.subject.as_deref(),
            Some("Sample Customer invoice for 10 2026")
        );
        assert_eq!(doc.total_hours.as_deref(), Some("16h 50min"));
        assert_eq!(doc.column_labels[0], "Item type");
        let meta_keys: Vec<&str> = doc
            .meta
            .iter()
            .filter_map(|b| match b {
                crate::pdf::Block::KeyVal { key, .. } => Some(key.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            meta_keys,
            vec!["Subject", "Invoice ID", "Issue date", "Due date", "Period"]
        );
        assert_eq!(
            doc.header
                .iter()
                .filter(|b| matches!(b, crate::pdf::Block::KeyVal { key, .. } if key == "Company number"))
                .count(),
            1
        );
        let texts: Vec<&str> = doc
            .footer
            .iter()
            .filter_map(|b| match b {
                crate::pdf::Block::Paragraph(spans) => Some(spans[0].text.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"For all EUR Payments;"), "{texts:?}");
        assert!(texts.contains(&"For all USD Payments;"), "{texts:?}");
        assert!(doc.footer.iter().any(|b| matches!(
            b,
            crate::pdf::Block::Heading { spans, .. } if spans[0].text == "Notes"
        )));
    }

    #[test]
    fn validation_is_the_same_for_save_and_preview() {
        let mut bad = InvoiceTemplate {
            body: "pay %bogus%".into(),
            ..Default::default()
        };
        let errors = validate_template(&bad);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "body");
        // Control characters are refused in the header-bound subject.
        bad = InvoiceTemplate {
            subject: "x\r\nBcc: evil@example.test".into(),
            ..Default::default()
        };
        let errors = validate_template(&bad);
        assert!(errors.iter().any(|e| e.field == "subject"));
        // The default template is valid and renders deterministically.
        let org = crate::pdf::Org::named("Sample Org");
        let a = render_sample_pdf(&InvoiceTemplate::default(), &org);
        let b = render_sample_pdf(&InvoiceTemplate::default(), &org);
        assert_eq!(a, b, "same template + org renders byte-identically");
        assert!(a.starts_with(b"%PDF-"));
    }
}
