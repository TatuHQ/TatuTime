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
//! line, a period, VAT, a due date and payment details) so every documented
//! `%variable%` has a real value to show. Nothing here writes to the store.

use chrono::{NaiveDate, TimeZone, Utc};
use uuid::Uuid;

use crate::api::*;
use crate::domain::{
    Address, Contact, Currency, Invoice, InvoiceLine, InvoiceStatus, InvoiceTemplate, LineKind,
    PaymentDetails, PaymentTerms, TermsKind,
};

/// A date that exists in every calendar (no leap-day edge) and keeps the
/// sample byte-stable across runs — the preview's ETag depends on it.
fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("sample date is valid")
}

/// The synthetic invoice every preview and save-gauntlet renders against.
///
/// Two time lines and one expense line, a period, a due date and totals with
/// VAT: enough for `%line_count%`, `%total%`, `%period_from%`,
/// `%period_to%`, `%invoice_due_date%` and the label overrides to be visible.
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
    }
}

/// The customer the sample invoice bills: a billing contact and full payment
/// details, so `%payment_details%` and the document's "How to pay" section
/// are exercised.
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
            bank_name: "Sample Bank".into(),
            account_name: "Sample Customer".into(),
            account_number: String::new(),
            iban: "DE89 3704 0044 0532 0130 00".into(),
            bic: "COBADEFFXXX".into(),
            reference: "Invoice number".into(),
            instructions: "Please quote the invoice number with your transfer.".into(),
        }),
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

/// `POST /admin/invoice-template/preview` — render the **unsaved** candidate
/// template as a PDF, using the saved org profile (branding/accent/sender) and
/// the shared sample invoice.
///
/// Validates exactly like `PUT` (identical 422 `validation_failed` shapes) and
/// **persists nothing**: no template, invoice, archive or timeline. A command
/// on existing configuration, so **200** (C8), not 201. The `ETag` is the
/// SHA-256 of the rendered bytes, and therefore a fingerprint of
/// template + org + sample: the GUI can skip a redundant render when it is
/// unchanged.
pub async fn invoice_template_preview(
    State(app): State<AppState>,
    ValidJson(template): ValidJson<InvoiceTemplate>,
) -> ApiResult {
    let errors = validate_template(&template);
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let org = super::org_for(&app)?;
    let bytes = render_sample_pdf(&template, &org);
    use sha2::{Digest, Sha256};
    let etag = crate::hex_lower(&Sha256::digest(&bytes));
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
            (axum::http::header::ETAG, format!("\"{etag}\"")),
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
        assert!(customer.payment_details.is_some(), "payment details");
        assert!(
            customer
                .contacts
                .iter()
                .any(|c| c.billing && !c.email.is_empty()),
            "billing contact"
        );
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
