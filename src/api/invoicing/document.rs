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

/// The singleton invoice template (#116) as stored. An install that has never
/// saved one resolves to the seeded defaults (`net_30` + the period subject);
/// an explicitly saved template is used exactly as stored, so the code fallback
/// chain in [`resolve_due_date`] stays intact.
pub(crate) fn load_template(app: &AppState) -> Result<crate::domain::InvoiceTemplate, ApiError> {
    Ok(app
        .store
        .read_json_rel::<crate::domain::InvoiceTemplate>("invoice_template.json")?
        .unwrap_or_else(crate::domain::InvoiceTemplate::seeded))
}

/// Resolved variables + rendered content for one invoice/customer/template.
pub(crate) fn content_for(
    invoice: &Invoice,
    customer: &crate::domain::Customer,
    template: &crate::domain::InvoiceTemplate,
) -> crate::template::DocContent {
    let issue_day = invoice.document.invoice_date.or_else(|| {
        invoice
            .issued_at
            .or(Some(invoice.created_at))
            .map(|dt| dt.date_naive())
    });
    let vars = crate::template::vars_for(invoice, customer, issue_day);
    crate::template::doc_content(invoice, template, customer, &vars)
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
    let content = content_for(invoice, &customer, &template);
    // The stored snapshot wins for an issued invoice; a draft — and a legacy
    // issued document that predates the field — resolves the subject live,
    // exactly as `/document` and the email do. Rendering from the stored value
    // alone left the preview's PDF without the Subject row its own JSON
    // reported. The title follows the same resolved value.
    let mut view = invoice.clone();
    view.subject = content.subject.clone().unwrap_or_default();
    if view.status == crate::domain::InvoiceStatus::Draft {
        view.due_date = Some(resolve_due_date(
            invoice,
            &customer,
            &template,
            app.clock.now(),
        ));
        view.terms = if invoice.document.due_date.is_some() {
            None
        } else {
            invoice
                .document
                .payment_terms
                .or_else(|| resolve_terms(&customer, &template))
        };
    }
    let mut doc = crate::pdf::doc_for_labeled(&view, &customer, &org_for(app)?, &template.labels);
    crate::template::apply_doc_content(&mut doc, &content);
    doc.watermark = Some(stage);
    Ok(crate::pdf::render_invoice_pdf(&doc))
}

/// The payment terms an issue will use: customer first, else the org template.
/// `None` means the legacy net-14-from-`period_to` fallback applies. This is
/// the ONE resolver, so the frozen `Invoice::terms` snapshot and the computed
/// `due_date` can never disagree.
#[must_use]
pub(crate) fn resolve_terms(
    customer: &crate::domain::Customer,
    template: &crate::domain::InvoiceTemplate,
) -> Option<crate::domain::PaymentTerms> {
    customer
        .payment_terms
        .as_ref()
        .or(template.payment_terms.as_ref())
        .copied()
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
    if let Some(due) = invoice.document.due_date {
        return due;
    }
    let terms = invoice
        .document
        .payment_terms
        .or_else(|| resolve_terms(customer, template));
    if let Some(days) = terms.as_ref().and_then(|t| t.fixed_days()) {
        let from = invoice
            .document
            .invoice_date
            .unwrap_or_else(|| now.date_naive());
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
/// oversized fields, malformed payment terms), render the pieces against the
/// shared sample invoice as an extra gauntlet, then persist atomically. A
/// rejection persists nothing.
///
/// Validation and the sample live in [`super::preview`], so the preview
/// endpoint and this save path agree by construction on what a valid template
/// is and what it renders against.
pub async fn invoice_template_put(
    State(app): State<AppState>,
    ValidJson(template): ValidJson<crate::domain::InvoiceTemplate>,
) -> ApiResult {
    let errors = super::preview::validate_template(&template);
    if errors.is_empty() {
        // Build the sample document: anything that cannot render here would
        // fail on a real invoice later. Never persists a broken template.
        let _ = super::preview::sample_document(&template, &crate::pdf::Org::named("Sample Org"));
    }
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    app.store
        .write_json_rel("invoice_template.json", &template)?;
    Ok(Json(template).into_response())
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
