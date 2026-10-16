use super::{get_invoice_or_404, org_for};
use crate::api::*;
use crate::domain::InvoiceWatermarkStage;

/// Download the archived invoice PDF (#113). Admin tier only (the whole
/// `/invoices` surface is, #51). A legacy issued invoice with no archive yet
/// gets it rendered lazily from the snapshot-locked document — byte-stable by
/// the renderer's determinism contract — and persisted on first read.
pub async fn invoice_pdf(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    if invoice.status == crate::domain::InvoiceStatus::Draft {
        return Err(ApiError::conflict(
            "issue the invoice before downloading its PDF",
        ));
    }
    let (bytes, filename) = match app.store.invoice_pdf_bytes(id)? {
        Some(b) => (
            b,
            format!("{}.pdf", crate::domain::safe_filename(&invoice.number)),
        ),
        None => {
            let b = render_pdf_now(&app, &invoice)?;
            // Persist the archive + hint; a failure here is a plain 500 and
            // costs nothing (the invoice JSON remains the record of truth).
            let attached = app.store.attach_invoice_pdf(id, &b, app.clock.now())?;
            let name = attached.pdf.map(|h| h.filename).unwrap_or_else(|| {
                format!("{}.pdf", crate::domain::safe_filename(&invoice.number))
            });
            (b, name)
        }
    };
    use sha2::{Digest, Sha256};
    let sha = Sha256::digest(&bytes);
    let etag = crate::hex_lower(&sha);
    tracing::debug!(invoice = %invoice.number, bytes = bytes.len(), "invoice pdf download");
    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/pdf".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
            (axum::http::header::ETAG, format!("\"{etag}\"")),
        ],
        bytes,
    )
        .into_response())
}

/// `GET /invoices/{id}/pdf/preview` — an on-demand, stage-stamped PDF render
/// (Subtask C). **Works for a draft**, the deliberate asymmetry with
/// `GET /invoices/{id}/pdf`, which answers **409** for a draft: no *archived*
/// document exists to serve. The document is stamped with the invoice's *current*
/// [`crate::domain::invoice_watermark_stage`] (`DRAFT`/`SENT`/`PAID`); the stamp
/// is text-only and date-free, so the render is deterministic and carries an
/// `ETag` like `/pdf`. Purely computed — **never archived**, never written — so
/// it cannot disturb the write-once `invoices/<id>.pdf`. **404** for an unknown
/// id; admin tier (`x-admin-only`), so a member is **403** and an anonymous
/// caller **401**.
pub async fn invoice_pdf_preview(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    let events = store_blocking("invoice timeline read", &app.store, move |s| {
        Ok(s.invoice_timeline(id))
    })
    .await?;
    let stage = crate::domain::invoice_watermark_stage(&invoice, &events);
    let bytes = render_pdf_with_stage(&app, &invoice, stage)?;
    use sha2::{Digest, Sha256};
    let etag = crate::hex_lower(&Sha256::digest(&bytes));
    tracing::debug!(invoice = %invoice.number, stage = stage.as_str(), bytes = bytes.len(), "invoice pdf preview");
    Ok((
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/pdf".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "inline".to_string(),
            ),
            (axum::http::header::ETAG, format!("\"{etag}\"")),
        ],
        bytes,
    )
        .into_response())
}

/// Resolve the invoice PDF for delivery: the archived bytes when present,
/// otherwise rendered on demand from the snapshot-locked invoice and archived
/// (#113: "fall back to rendering it on demand if the archive is missing").
pub(crate) fn invoice_pdf_for_delivery(
    app: &AppState,
    invoice: &Invoice,
) -> Result<(String, Vec<u8>), ApiError> {
    if let Some(bytes) = app.store.invoice_pdf_bytes(invoice.id)? {
        let name = invoice
            .pdf
            .as_ref()
            .map(|h| h.filename.clone())
            .unwrap_or_else(|| format!("{}.pdf", crate::domain::safe_filename(&invoice.number)));
        return Ok((name, bytes));
    }
    let bytes = render_pdf_now(app, invoice)?;
    let attached = app
        .store
        .attach_invoice_pdf(invoice.id, &bytes, app.clock.now())?;
    let name = attached
        .pdf
        .map(|h| h.filename)
        .unwrap_or_else(|| format!("{}.pdf", crate::domain::safe_filename(&invoice.number)));
    Ok((name, bytes))
}

// ------------------------------------------------- templates & document --

/// The singleton invoice template (#116) as stored; absent means defaults.
pub(crate) fn load_template(app: &AppState) -> Result<crate::domain::InvoiceTemplate, ApiError> {
    Ok(app
        .store
        .read_json_rel::<crate::domain::InvoiceTemplate>("invoice_template.json")?
        .unwrap_or_default())
}

/// Resolved variables + rendered content for one invoice/customer/template.
pub(crate) fn content_for(
    invoice: &Invoice,
    customer: &crate::domain::Customer,
    template: &crate::domain::InvoiceTemplate,
) -> crate::template::DocContent {
    let issue_day = invoice
        .issued_at
        .or(Some(invoice.created_at))
        .map(|dt| dt.date_naive());
    let vars = crate::template::vars_for(invoice, customer, issue_day);
    crate::template::doc_content(template, customer, &vars)
}

/// Render + archive the PDF from the invoice as stored today, using the current
/// template content (the legacy path of #113/#116). Deterministic in
/// (invoice, customer, template, org): a re-render while nothing was edited
/// yields identical bytes.
///
/// The archive/delivery path stamps the **generation-time stage**, `Draft`: an
/// invoice cannot be sent before it is issued, so an archived PDF is only ever
/// produced while undelivered. It is captured here, not resolved live, because
/// the archive is write-once (#113).
pub(crate) fn render_pdf_now(app: &AppState, invoice: &Invoice) -> Result<Vec<u8>, ApiError> {
    render_pdf_with_stage(app, invoice, InvoiceWatermarkStage::Draft)
}

/// Render the invoice with an explicit watermark stage (Subtask C). The
/// archive/delivery callers pass `Draft`; the on-demand preview passes the
/// live [`crate::domain::invoice_watermark_stage`]. The stage is threaded
/// through the `Doc`, never a global, so each render is self-contained.
pub(crate) fn render_pdf_with_stage(
    app: &AppState,
    invoice: &Invoice,
    stage: InvoiceWatermarkStage,
) -> Result<Vec<u8>, ApiError> {
    let customer = get_customer(&app.store, invoice.customer_id)?;
    let template = load_template(app)?;
    let mut doc = crate::pdf::doc_for_labeled(invoice, &customer, &org_for(app)?, &template.labels);
    let content = content_for(invoice, &customer, &template);
    crate::template::apply_doc_content(&mut doc, &content);
    doc.watermark = Some(stage);
    Ok(crate::pdf::render_invoice_pdf(&doc))
}

/// `due_date` at issue (#116): customer terms → org default terms → the
/// legacy net-14-from-`period_to`. Terms count from the issue day; the legacy
/// fallback keeps its exact original arithmetic so nothing shifts.
pub(crate) fn resolve_due_date(
    invoice: &Invoice,
    customer: &crate::domain::Customer,
    template: &crate::domain::InvoiceTemplate,
    now: chrono::DateTime<chrono::Utc>,
) -> chrono::NaiveDate {
    let terms = customer
        .payment_terms
        .as_ref()
        .or(template.payment_terms.as_ref());
    if let Some(days) = terms.and_then(|t| t.fixed_days()) {
        let from = now.date_naive();
        return from
            .checked_add_days(chrono::Days::new(u64::from(days)))
            .unwrap_or(from);
    }
    invoice
        .period_to
        .checked_add_days(chrono::Days::new(14))
        .unwrap_or(invoice.period_to)
}

/// `GET /admin/invoice-template` — the stored template plus the variable
/// cheatsheet, so the GUI renders the same table the validator enforces.
pub async fn invoice_template_get(State(app): State<AppState>) -> ApiResult {
    let template = load_template(&app)?;
    Ok(Json(serde_json::json!({
        "template": template,
        "variables": crate::template::VAR_HELP
            .iter()
            .map(|(name, example)| serde_json::json!({"name": name, "example": example}))
            .collect::<Vec<_>>(),
    }))
    .into_response())
}

/// `PUT /admin/invoice-template` — validate fail-loud (unknown `%tokens%`,
/// oversized fields, malformed payment terms), render the pieces against a
/// synthetic invoice as an extra gauntlet, then persist atomically. A
/// rejection persists nothing.
pub async fn invoice_template_put(
    State(app): State<AppState>,
    ValidJson(template): ValidJson<crate::domain::InvoiceTemplate>,
) -> ApiResult {
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
    if errors.is_empty() {
        // Render against a synthetic sample: anything that fails here would
        // fail on a real invoice later. Never persists a broken template.
        let sample = Invoice {
            id: Uuid::nil(),
            number: "INV-0000".into(),
            customer_id: Uuid::nil(),
            currency: crate::domain::Currency("EUR".into()),
            period_from: chrono::NaiveDate::MIN + chrono::Duration::days(1),
            period_to: chrono::NaiveDate::MIN + chrono::Duration::days(2),
            lines: vec![],
            total_minor: 0,
            status: crate::domain::InvoiceStatus::Draft,
            created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
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
        };
        let sample_customer = synthetic_customer();
        let vars = crate::template::vars_for(&sample, &sample_customer, None);
        // interpolate/parse cannot fail by design; the call is the assertion
        // that no panic escapes for this template.
        let _ = crate::template::doc_content(&template, &sample_customer, &vars);
    }
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    app.store
        .write_json_rel("invoice_template.json", &template)?;
    Ok(Json(template).into_response())
}

fn synthetic_customer() -> crate::domain::Customer {
    crate::domain::Customer {
        id: Uuid::nil(),
        name: "Sample Customer".into(),
        currency: crate::domain::Currency("EUR".into()),
        default_rate_minor: 0,
        active: true,
        email: String::new(),
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

/// `GET /invoices/{id}/document` — the live `{subject, html}` preview:
/// exactly the content the PDF (#113) and the email (#35) render from,
/// resolved from the *current* template + customer, not a baked snapshot.
pub async fn invoice_document(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    let customer = get_customer(&app.store, invoice.customer_id)?;
    let template = load_template(&app)?;
    let content = content_for(&invoice, &customer, &template);
    let org = org_for(&app)?;
    let subject = content
        .subject
        .clone()
        .or_else(|| Some(crate::email::invoice_subject(&invoice.number, &org.name)))
        .unwrap_or_default();
    let html = crate::template::email_html(&content).unwrap_or_default();
    Ok(Json(serde_json::json!({ "subject": subject, "html": html })).into_response())
}
