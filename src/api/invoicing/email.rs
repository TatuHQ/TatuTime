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

/// Emails an issued invoice to the customer's billing address (#35). Admin
/// only (invoices are admin surface). Non-blocking: the transport runs on a
/// blocking thread so a slow relay never stalls the request. The archived
/// PDF rides along as an `application/pdf` attachment (#113).
pub async fn send_invoice_email(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    if invoice.status == crate::domain::InvoiceStatus::Draft {
        return Err(ApiError::conflict("issue the invoice before emailing it"));
    }
    let customer = get_customer(&app.store, invoice.customer_id)?;
    // #139: the flagged billing contact receives it; the legacy top-level
    // email still works for customers without contacts.
    let billing_email = customer.billing_email().to_string();
    if billing_email.is_empty() {
        return Err(ApiError::validation(vec![FieldError::new(
            "email",
            "customer has no billing email on file",
        )]));
    }
    let (filename, pdf) = invoice_pdf_for_delivery(&app, &invoice)?;
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
    let text = crate::email::render_invoice_email(
        &customer.name,
        &invoice.number,
        &amount,
        due.as_deref(),
        &org.name,
        true,
        payment.as_deref(),
    );
    // #116: the template subject wins when configured (customer override →
    // org template → legacy); the HTML letter is set only when there is
    // template content, keeping unset templates byte-identical.
    let subject = content
        .subject
        .clone()
        .unwrap_or_else(|| crate::email::invoice_subject(&invoice.number, &org.name));
    let html = crate::template::email_html(&content);
    let profile = load_org(&app)?;
    let msg = crate::email::EmailMessage {
        to: billing_email,
        subject,
        text,
        html,
        attachment: Some((filename, pdf)),
        from_name: sender_field(&profile.from_name),
        reply_to: sender_field(&profile.reply_to),
    };
    let sent_to = msg.to.clone();
    let timeline_subject = msg.subject.clone();
    let sender = app.email.clone();
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
    meta.insert("subject".into(), serde_json::Value::from(timeline_subject));
    meta.insert("from_name".into(), serde_json::Value::from(from_name));
    if !from_email.is_empty() {
        meta.insert("from_email".into(), serde_json::Value::from(from_email));
    }
    meta.insert(
        "transport".into(),
        serde_json::Value::from(app.email.transport()),
    );
    record_timeline(
        &app,
        invoice.id,
        InvoiceTimelineEventKind::EmailSent,
        format!("Sent invoice to {sent_to}, from {from_display}."),
        actor.0.name.clone(),
        Some(meta),
    )
    .await;
    // Honest delivery state (#130): with no SMTP configured the transport
    // succeeds silently — clients must not read a 200 as "the customer got
    // an email". `transport` is "smtp" only when a relay accepted the send.
    Ok(
        Json(serde_json::json!({ "sent_to": sent_to, "transport": app.email.transport() }))
            .into_response(),
    )
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
        to: to.clone(),
        subject,
        text,
        html: None,
        attachment: Some((filename, pdf)),
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
