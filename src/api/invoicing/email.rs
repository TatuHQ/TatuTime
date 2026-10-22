use super::{
    content_for, get_invoice_or_404, invoice_pdf_for_delivery, lifecycle::record_timeline,
    load_org, load_template, org_for,
};
use crate::api::*;
use crate::domain::InvoiceTimelineEventKind;

/// Formats minor units for an email body, e.g. `120000` + `EUR` -> `1,200.00 EUR`.
pub fn money_for_email(minor: u64, currency: &str) -> String {
    let whole = minor / 100;
    let frac = minor % 100;
    let grouped = whole
        .to_string()
        .chars()
        .rev()
        .enumerate()
        .fold(String::new(), |mut acc, (i, c)| {
            if i > 0 && i % 3 == 0 {
                acc.push(',');
            }
            acc.push(c);
            acc
        })
        .chars()
        .rev()
        .collect::<String>();
    format!("{grouped}.{:02} {currency}", frac)
}

/// Optional body for `POST /invoices/{id}/email`: an explicit recipient list.
/// Absent (or an empty list) keeps the historical single billing-address send.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceEmailInput {
    #[serde(default)]
    pub recipients: Vec<String>,
    /// Attach the stored receipt files of the invoice's expense lines (#new).
    /// Defaults to false so existing callers keep the invoice-PDF-only email.
    #[serde(default)]
    pub include_receipts: bool,
}

/// One receipt file attached to an invoice's expense lines, resolved from the
/// stored expense documents (never from the invoice line snapshot).
pub(crate) struct InvoiceReceipt {
    pub expense_id: Uuid,
    pub name: String,
    pub b64: String,
}

/// The receipts this invoice can send: one per distinct expense line that has a
/// stored receipt. Order follows the invoice lines. Files are not decoded here,
/// so the read-only summary endpoint never pays for base64 decoding.
pub(crate) fn invoice_receipts(
    app: &AppState,
    invoice: &crate::domain::Invoice,
) -> Result<Vec<InvoiceReceipt>, ApiError> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in &invoice.lines {
        let Some(id) = line.expense_id else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        let Some(expense) = app.store.get_expense(id)? else {
            continue;
        };
        let Some(b64) = expense
            .receipt_b64
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
        else {
            continue;
        };
        out.push(InvoiceReceipt {
            expense_id: id,
            name: receipt_filename(expense.receipt_name.as_deref(), expense.date),
            b64: b64.to_string(),
        });
    }
    Ok(out)
}

/// `GET /invoices/{id}/receipts` — names of the receipt files an email could
/// attach, for the recipient dialog's include-receipts option. Never returns
/// the bytes. Admin tier like the rest of the invoice surface.
pub async fn invoice_receipts_handler(
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    let receipts = invoice_receipts(&app, &invoice)?;
    Ok(Json(serde_json::json!({
        "receipts": receipts
            .iter()
            .map(|r| serde_json::json!({
                "expense_id": r.expense_id,
                "name": r.name,
            }))
            .collect::<Vec<_>>(),
    }))
    .into_response())
}

/// Sanitise a stored receipt name for `Content-Disposition`: keep the extension
/// and readable stem, drop a path (a receipt never has one) and any character
/// that could break the header. A nameless receipt gets a date-based name.
fn receipt_filename(name: Option<&str>, date: chrono::NaiveDate) -> String {
    let raw = name.unwrap_or("").trim();
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '"' | ';' | '\r' | '\n') {
                '_'
            } else {
                c
            }
        })
        .collect();
    cleaned = cleaned.trim().trim_matches('.').to_string();
    if cleaned.is_empty() {
        return format!("receipt-{date}");
    }
    // Keep header sizes sane while preserving the extension.
    const MAX: usize = 120;
    if cleaned.chars().count() > MAX {
        let (stem, ext) = cleaned
            .rsplit_once('.')
            .filter(|(_, ext)| ext.len() <= 12)
            .unwrap_or((cleaned.as_str(), ""));
        let ext_len = if ext.is_empty() { 0 } else { ext.len() + 1 };
        let take = MAX.saturating_sub(ext_len);
        let stem: String = stem.chars().take(take).collect();
        cleaned = if ext.is_empty() {
            stem
        } else {
            format!("{stem}.{ext}")
        };
    }
    cleaned
}

/// Decode the resolved receipts and enforce the per-email caps before anything
/// is sent. A malformed receipt is a hard error: silently dropping a file the
/// admin asked to include would misrepresent the delivery.
fn receipt_attachments(receipts: &[InvoiceReceipt]) -> Result<Vec<(String, Vec<u8>)>, ApiError> {
    const MAX_RECEIPTS: usize = 20;
    const MAX_TOTAL_BYTES: usize = 25_000_000;
    if receipts.len() > MAX_RECEIPTS {
        return Err(ApiError::validation(vec![FieldError::new(
            "include_receipts",
            format!("at most {MAX_RECEIPTS} expense receipts per email"),
        )]));
    }
    use base64::Engine;
    let mut out = Vec::with_capacity(receipts.len());
    let mut total = 0usize;
    for receipt in receipts {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(receipt.b64.as_bytes())
            .map_err(|_| ApiError::internal("an expense receipt is not valid base64".into()))?;
        total = total.saturating_add(bytes.len());
        out.push((receipt.name.clone(), bytes));
    }
    if total > MAX_TOTAL_BYTES {
        return Err(ApiError::validation(vec![FieldError::new(
            "include_receipts",
            "expense receipts exceed the 25 MB total email attachment limit",
        )]));
    }
    Ok(out)
}

/// Emails an issued invoice to the customer's billing address (#35), or to an
/// explicit recipient list when the request carries one. Admin only (invoices
/// are admin surface). Non-blocking: the transport runs on a blocking thread so
/// a slow relay never stalls the request. The archived PDF rides along as an
/// `application/pdf` attachment (#113).
///
/// All recipients are resolved and validated **before** anything is sent, so a
/// rejected list never produces a partial delivery.
pub async fn send_invoice_email(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
    OptionalValidJson(input): OptionalValidJson<InvoiceEmailInput>,
) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    if invoice.status == crate::domain::InvoiceStatus::Draft {
        return Err(ApiError::conflict("issue the invoice before emailing it"));
    }
    let customer = get_customer(&app.store, invoice.customer_id)?;
    // #139: no explicit list -> the flagged billing contact; the legacy
    // top-level email still works for customers without contacts. An explicit
    // list is used verbatim (order preserved, case-insensitive de-duplication)
    // and does not require the customer to have a billing address.
    let include_receipts = input.as_ref().is_some_and(|body| body.include_receipts);
    let requested = input.map(|body| body.recipients).unwrap_or_default();
    let recipients = crate::email::resolve_recipients(&requested, Some(customer.billing_email()))
        .map_err(|errors| {
        ApiError::validation(
            errors
                .into_iter()
                .map(|(field, message)| FieldError::new(field, message))
                .collect(),
        )
    })?;
    let (filename, pdf) = invoice_pdf_for_delivery(&app, &invoice)?;
    let receipts = if include_receipts {
        invoice_receipts(&app, &invoice)?
    } else {
        Vec::new()
    };
    let receipt_files = receipt_attachments(&receipts)?;
    let receipt_count = receipt_files.len();
    let mut attachments = vec![(filename, pdf)];
    let receipt_names: Vec<String> = receipt_files.iter().map(|(name, _)| name.clone()).collect();
    attachments.extend(receipt_files);
    let amount = money_for_email(invoice.total_minor, &invoice.currency.0);
    let due = invoice.due_date.map(|d| d.to_string());
    let org = org_for(&app)?;
    let template = load_template(&app)?;
    let content = content_for(&invoice, &customer, &template);
    // "How to pay": the customer's own configured payment details (#get-paid).
    let payment = customer
        .payment_details
        .as_ref()
        .and_then(|p| p.email_block());
    let mut text = crate::email::render_invoice_email(
        &customer.name,
        &invoice.number,
        &amount,
        due.as_deref(),
        &org.name,
        true,
        payment.as_deref(),
    );
    if receipt_count > 0 {
        text.push_str(&format!(
            "\n\n{} expense receipt{} attached for your reference.",
            receipt_count,
            if receipt_count == 1 { "" } else { "s" }
        ));
    }
    // The document's own snapshot is the first word on the subject (phase 2):
    // whatever `invoice.subject` reads on the PDF is what the email carries.
    // Otherwise the live precedence still applies (customer override → org
    // template → legacy), and the HTML letter is set only when there is
    // template content, keeping unset templates byte-identical.
    let subject = {
        let snapshot = invoice.subject.trim();
        if snapshot.is_empty() {
            content
                .subject
                .clone()
                .unwrap_or_else(|| crate::email::invoice_subject(&invoice.number, &org.name))
        } else {
            snapshot.to_string()
        }
    };
    let html = crate::template::email_html(&content);
    let profile = load_org(&app)?;
    let msg = crate::email::EmailMessage {
        to: recipients.clone(),
        subject,
        text,
        html,
        attachments,
        from_name: sender_field(&profile.from_name),
        reply_to: sender_field(&profile.reply_to),
    };
    // Compatibility: `sent_to` stays a string (comma-joined when several);
    // `recipients` carries the same list as an array.
    let sent_to = recipients.join(", ");
    let timeline_subject = msg.subject.clone();
    let sender = app.email.clone();
    let transport = app.email.transport();
    // A slow relay must not stall the async request thread (#35).
    blocking("email send", move || {
        sender.send(&msg).map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                "email_failed",
                e.to_string(),
            )
        })
    })
    .await?;
    // Timeline (Subtask A): record the delivery with the recipient, the From
    // identity used and the transport, never the email body. The From is the
    // org identity this send resolved (the SMTP envelope address is not
    // exposed on the transport port), as `Name <reply_to>` when a Reply-To is
    // configured. `/email-copy` is deliberately NOT recorded here — it is an
    // ad-hoc PDF copy to a third party, not the customer delivery, so it is not
    // an `email_sent` lifecycle event (it stays in the audit log).
    let from_name = {
        let n = profile.from_name.trim();
        if n.is_empty() {
            org.name.clone()
        } else {
            n.to_string()
        }
    };
    let from_email = profile.reply_to.trim().to_string();
    let from_display = if from_email.is_empty() {
        from_name.clone()
    } else {
        format!("{from_name} <{from_email}>")
    };
    let mut meta = serde_json::Map::new();
    meta.insert("to".into(), serde_json::Value::from(sent_to.clone()));
    meta.insert(
        "recipients".into(),
        serde_json::Value::from(recipients.clone()),
    );
    meta.insert("subject".into(), serde_json::Value::from(timeline_subject));
    meta.insert("from_name".into(), serde_json::Value::from(from_name));
    if !from_email.is_empty() {
        meta.insert("from_email".into(), serde_json::Value::from(from_email));
    }
    meta.insert("transport".into(), serde_json::Value::from(transport));
    if !receipt_names.is_empty() {
        meta.insert(
            "receipts".into(),
            serde_json::Value::from(receipt_names.clone()),
        );
        meta.insert(
            "receipt_count".into(),
            serde_json::Value::from(receipt_count),
        );
    }
    let receipt_note = if receipt_count == 0 {
        String::new()
    } else {
        format!(
            ", with {receipt_count} expense receipt{}",
            if receipt_count == 1 { "" } else { "s" }
        )
    };
    record_timeline(
        &app,
        invoice.id,
        InvoiceTimelineEventKind::EmailSent,
        format!("Sent invoice to {sent_to}, from {from_display}{receipt_note}."),
        actor.0.name.clone(),
        Some(meta),
    )
    .await;
    // Honest delivery state (#130): with no SMTP configured the transport
    // succeeds silently — clients must not read a 200 as "the customer got
    // an email". `transport` is "smtp" only when a relay accepted the send.
    Ok(Json(serde_json::json!({
        "sent_to": sent_to,
        "recipients": recipients,
        "transport": transport,
        "receipt_count": receipt_count,
    }))
    .into_response())
}

/// Body for `POST /invoices/{id}/email-copy` (#112).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailCopyInput {
    pub to: String,
    #[serde(default)]
    pub note: String,
}

/// Cap on the free-form note so an email body cannot be padded (#50).
const COPY_NOTE_MAX_CHARS: usize = 2000;

/// Sends a PDF copy of an (issued) invoice to an arbitrary recipient — the
/// company accountant, a partner, whoever (#112). Admin tier, drafts are not
/// shared, the recipient is validated before anything is sent or persisted,
/// and the send lands in the audit log (#52) as recipient + invoice id —
/// never the PDF bytes.
/// NOTE (Subtask A): unlike `send_invoice_email`, this route does **not** append
/// to the invoice timeline. It emails an ad-hoc PDF copy to an arbitrary third
/// party (the accountant, a partner), not the customer's billing contact, so it
/// is not a `email_sent` lifecycle event and has no timeline kind of its own.
/// Its record is the audit log entry written below (`invoice_email_copy`).
pub async fn send_invoice_email_copy(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<EmailCopyInput>,
) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    // Fail-loud validation before any side effect (no partial persistence):
    let to = input.to.trim().to_string();
    if !crate::email::valid_address(&to) {
        return Err(ApiError::validation(vec![FieldError::new(
            "to",
            "not a valid email address",
        )]));
    }
    if input.note.chars().count() > COPY_NOTE_MAX_CHARS {
        return Err(ApiError::validation(vec![FieldError::new(
            "note",
            "note too long (max 2000 characters)",
        )]));
    }
    if invoice.status == crate::domain::InvoiceStatus::Draft {
        return Err(ApiError::conflict(
            "issue the invoice before sharing a copy",
        ));
    }
    let customer = get_customer(&app.store, invoice.customer_id)?;
    let (filename, pdf) = invoice_pdf_for_delivery(&app, &invoice)?;
    let org = org_for(&app)?;
    let subject = format!("Copy of invoice {} for {}", invoice.number, customer.name);
    let text = crate::email::render_copy_email(
        &invoice.number,
        &customer.name,
        &actor.0.name,
        Some(&input.note),
        &org.name,
    );
    let profile = load_org(&app)?;
    let msg = crate::email::EmailMessage {
        to: vec![to.clone()],
        subject,
        text,
        html: None,
        attachments: vec![(filename, pdf)],
        // Copy email carries the same configured sender identity (#146).
        from_name: sender_field(&profile.from_name),
        reply_to: sender_field(&profile.reply_to),
    };
    let sender = app.email.clone();
    let result = blocking("email copy send", move || {
        sender.send(&msg).map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                "email_failed",
                e.to_string(),
            )
        })
    })
    .await;
    match result {
        Ok(()) => {
            // #52: recipient + invoice, never the document bytes.
            app.audit.record(
                "invoice_email_copy",
                &format!("{id}:{to}:{}", app.email.transport()),
                app.clock.now(),
            );
            Ok(
                Json(serde_json::json!({ "sent_to": to, "transport": app.email.transport() }))
                    .into_response(),
            )
        }
        Err(e) => Err(e),
    }
}

/// Empty string in the org profile means "omit" (#146).
fn sender_field(value: &str) -> Option<String> {
    let v = value.trim();
    (!v.is_empty()).then(|| v.to_string())
}
