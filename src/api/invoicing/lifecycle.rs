use super::{
    build_draft, content_for, load_template, money_for_email, org_for, resolve_due_date,
    resolve_terms,
};
use crate::api::*;
use crate::domain::{InvoiceTimelineEvent, InvoiceTimelineEventKind, InvoiceWatermarkStage};

/// `POST /invoices/preview` (#134): run the SAME generation as
/// `POST /invoices` but without persisting — the staged wizard reviews exact
/// lines, then saves the reviewed selection. A draft is created only by the
/// real POST; nothing here locks, numbers or stores.
pub async fn preview_invoice(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<InvoiceInput>,
) -> ApiResult {
    let customer = get_customer(&app.store, input.customer_id)?;
    let from = parse_date(&input.from)?;
    let to = parse_date(&input.to)?;
    if from > to {
        return Err(ApiError::bad_request("'from' must not be after 'to'"));
    }
    let draft = build_draft(
        &app,
        &customer,
        from,
        to,
        input.include_expenses,
        &input.project_codes,
    )?;
    Ok(Json(draft).into_response())
}

pub async fn list_invoices(State(app): State<AppState>) -> ApiResult {
    let invoices = store_blocking("invoice list", &app.store, |s| s.list_invoices()).await?;
    Ok(Json(serde_json::json!({ "invoices": invoices })).into_response())
}

/// Generate a draft invoice from the billable, not-yet-invoiced work in a period.
pub async fn create_invoice(
    State(app): State<AppState>,
    actor: AuthUser,
    ValidJson(input): ValidJson<InvoiceInput>,
) -> ApiResult {
    let customer = get_customer(&app.store, input.customer_id)?;
    let from = parse_date(&input.from)?;
    let to = parse_date(&input.to)?;
    if from > to {
        return Err(ApiError::bad_request("'from' must not be after 'to'"));
    }
    let draft = build_draft(
        &app,
        &customer,
        from,
        to,
        input.include_expenses,
        &input.project_codes,
    )?;
    let invoice = app.store.create_invoice(draft)?;
    record_timeline(
        &app,
        invoice.id,
        InvoiceTimelineEventKind::Created,
        "Invoice created.".into(),
        actor.0.name.clone(),
        None,
    )
    .await;
    Ok((StatusCode::CREATED, Json(invoice)).into_response())
}

/// The load-or-404 every invoice route starts with (#100). Reads only; state
/// checks stay with each endpoint.
pub(crate) fn get_invoice_or_404(app: &AppState, id: Uuid) -> Result<Invoice, ApiError> {
    app.store
        .get_invoice(id)?
        .ok_or_else(|| ApiError::not_found("invoice"))
}

pub async fn get_invoice_handler(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    Ok(Json(get_invoice_or_404(&app, id)?).into_response())
}

/// Append one entry to an invoice's activity timeline (Subtask A).
///
/// Best-effort by contract: the timeline is a display aid, never a record of
/// authority, so a failure is logged and the invoice operation that triggered
/// it still succeeds. Recording history can never change invoice semantics
/// (`is_open`/`already_invoiced` are untouched).
///
/// Returns the timestamp it stamped (one `Clock` read), so a caller that hands
/// the event back to the client — the `POST /invoices/{id}/sent` transition —
/// reports the exact value it recorded instead of taking a second, slightly
/// later reading. A failed append still returns it: the timeline never fails
/// the invoice operation, and there is nothing better for the caller to report.
pub(super) async fn record_timeline(
    app: &AppState,
    id: Uuid,
    kind: InvoiceTimelineEventKind,
    description: String,
    actor_name: String,
    meta: Option<serde_json::Map<String, serde_json::Value>>,
) -> chrono::DateTime<chrono::Utc> {
    let at = app.clock.now();
    let event = InvoiceTimelineEvent {
        kind,
        description,
        actor_name,
        at,
        meta,
    };
    let store = app.store.clone();
    if let Err(e) = blocking("invoice timeline append", move || {
        store
            .append_invoice_timeline(id, event)
            .map_err(ApiError::from)
    })
    .await
    {
        tracing::warn!(invoice = %id, error = %e.message, "could not append invoice timeline event");
    }
    at
}

/// `GET /invoices/{id}/timeline` — the retained activity timeline for one
/// invoice (Subtask A), newest first. Admin tier like the rest of the invoice
/// surface. Only the invoice's existence gates the read (unknown ids are 404);
/// a missing or malformed sidecar is simply an empty list.
pub async fn invoice_timeline(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    get_invoice_or_404(&app, id)?;
    let events = store_blocking("invoice timeline read", &app.store, move |s| {
        Ok(s.invoice_timeline(id))
    })
    .await?;
    let mut events = events;
    // Stored oldest-first (append order); the client reads newest-first.
    events.reverse();
    Ok(Json(serde_json::json!({ "events": events })).into_response())
}

/// `POST /invoices/{id}/sent` — the manual "mark as sent" transition
/// (Subtask B). Per C8 this is a *transition on an existing resource*, so it
/// answers **200** with that transition's new state, `{ "sent_at": "<RFC
/// 3339>" }`.
///
/// `sent_at` is **derived from the timeline**, deliberately never a field on
/// the closed `Invoice` schema: the `sent` event's `at` *is* the `sent_at`, and
/// an earlier `email_sent` (the `/email` route) counts just as much. Nothing
/// but a timeline event is ever persisted, so recording history cannot change
/// invoice status, `is_open()` or billing.
///
/// * **409** for a draft — nothing has been delivered and a draft has no
///   archived PDF, the same reasoning `/pdf` pins at 409.
/// * **Idempotent**: an invoice that already has a delivery (a manual `sent`
///   or an `email_sent`) is a **200** carrying the *earliest* delivery
///   timestamp, and **no** duplicate event is appended.
/// * **404** for an unknown id; the admin tier and the CSRF guard supply the
///   usual 401/403.
pub async fn mark_invoice_sent(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    if invoice.status == crate::domain::InvoiceStatus::Draft {
        return Err(ApiError::conflict(
            "issue the invoice before marking it as sent",
        ));
    }
    let events = store_blocking("invoice timeline read", &app.store, move |s| {
        Ok(s.invoice_timeline(id))
    })
    .await?;
    if let Some(sent_at) = crate::domain::invoice_sent_at(&events) {
        // Already delivered, by hand or by email: report the original
        // timestamp and append nothing, so a double click is harmless.
        return Ok(Json(serde_json::json!({ "sent_at": sent_at })).into_response());
    }
    // Honest wording: a manual transition knows no recipient, so the event
    // must not invent one — `/email` records the recipient it actually used.
    let sent_at = record_timeline(
        &app,
        id,
        InvoiceTimelineEventKind::Sent,
        "Invoice marked as sent.".into(),
        actor.0.name.clone(),
        None,
    )
    .await;
    Ok(Json(serde_json::json!({ "sent_at": sent_at })).into_response())
}

/// Issue a draft invoice: this locks its entries from edits/deletes (#18 seam)
/// and archives its PDF for the first time (#113). The document is rendered
/// from the exact state the store is about to persist (same clock, same due
/// date), so issuing and archiving commit or fail as one transaction.
pub async fn issue_invoice(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult {
    // Draft check + transition happen inside one store lock (review B4):
    // an issue racing a pay (or another issue) cannot double-transition.
    let invoice = get_invoice_or_404(&app, id)?;
    let customer = get_customer(&app.store, invoice.customer_id)?;
    let template = load_template(&app)?;
    // Payment terms (#116): customer -> org template -> legacy net-14 from
    // period_to (the fallback keeps its exact original arithmetic).
    // #190: capture the clock ONCE so the snapshot's issued_at and the value
    // the store persists are identical (three separate now() calls could
    // straddle a tick — visible at midnight boundaries and it breaks the
    // "re-render yields identical bytes" determinism claim).
    let now = app.clock.now();
    // ONE terms resolution feeds both the due date and the frozen snapshot, so
    // the printed `(Net N)` can never disagree with `due_date`.
    let terms = if invoice.document.due_date.is_some() {
        None
    } else {
        invoice
            .document
            .payment_terms
            .or_else(|| resolve_terms(&customer, &template))
    };
    let due = resolve_due_date(&invoice, &customer, &template, now);
    // Render against the to-be-issued snapshot: the PDF is the issue-time
    // document, and bytes are pure in this state (legacy re-renders match).
    let mut snapshot = invoice;
    snapshot.status = crate::domain::InvoiceStatus::Issued;
    snapshot.issued_at = Some(now);
    snapshot.due_date = Some(due);
    snapshot.terms = terms;
    let content = content_for(&snapshot, &customer, &template);
    // Freeze the resolved subject in the same transition. From here on the
    // document reads its own snapshot, not the live template/customer — and
    // the subject must be set BEFORE the document is built, or the archived
    // bytes would lack the Subject row a later lazy re-render prints.
    let subject = content.subject.clone().unwrap_or_default();
    snapshot.subject = subject.clone();
    let mut doc =
        crate::pdf::doc_for_labeled(&snapshot, &customer, &org_for(&app)?, &template.labels);
    crate::template::apply_doc_content(&mut doc, &content);
    // Subtask C: the archived PDF is generated at issue time, when the invoice
    // has not yet been delivered, so its frozen watermark stage is `Draft`.
    // The archive is write-once, so this stamp is never recomputed later.
    doc.watermark = Some(InvoiceWatermarkStage::Draft);
    let pdf = crate::pdf::render_invoice_pdf(&doc);
    let issued = app
        .store
        .issue_invoice(id, now, due, subject, terms, &pdf)?;
    record_timeline(
        &app,
        issued.id,
        InvoiceTimelineEventKind::Issued,
        "Invoice issued.".into(),
        actor.0.name.clone(),
        None,
    )
    .await;
    Ok(Json(issued).into_response())
}

/// Download names come from `Invoice::number` (minted server-side), sanitised
/// defensively — never from user input (#50).

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayInput {
    #[serde(default)]
    pub reference: String,
    /// Minor units; absent = the full remaining balance (today's behaviour,
    /// #114). Zero and over-balance are rejected without persisting.
    #[serde(default)]
    pub amount_minor: Option<u64>,
}

/// Body for `POST /invoices/{id}/write-off` (#114).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteOffInput {
    pub reason: String,
}

/// Record a payment on an issued invoice (#27, #114): full balance by
/// default, or a partial amount. A zero amount is a 422 (bad shape), an
/// over-payment a 409 (bad state arithmetic) — both without persistence.
pub async fn pay_invoice(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<PayInput>,
) -> ApiResult {
    // State check + transition under one lock (review B4).
    get_invoice_or_404(&app, id)?;
    if input.amount_minor == Some(0) {
        return Err(ApiError::validation(vec![FieldError::new(
            "amount_minor",
            "must be greater than zero",
        )]));
    }
    let invoice = match app.store.record_payment(
        id,
        input.amount_minor,
        input.reference,
        "manual".into(),
        app.clock.now(),
    ) {
        Ok(inv) => inv,
        // #220: the old 'payment of ' string-prefix arm was dead weight —
        // From<StoreError> already maps every Conflict to 409 with the same
        // (message-safe) body. Match on structure, not prose.
        Err(e) => return Err(e.into()),
    };
    let paid = invoice
        .payments
        .last()
        .ok_or_else(|| ApiError::internal("payment ledger empty after record".into()))?;
    app.audit.record(
        "invoice_payment",
        &format!(
            "{}:{}:{}:{}",
            id, paid.amount_minor, actor.0.id, paid.method
        ),
        app.clock.now(),
    );
    // Timeline (Subtask B): retain the status change alongside the audit entry,
    // which stays the security log. `meta` holds the non-secret settlement
    // detail the GUI shows: the amount in minor units, the method, the
    // resulting status, and the operator's reference when one was given.
    let mut meta = serde_json::Map::new();
    meta.insert("amount_minor".into(), serde_json::json!(paid.amount_minor));
    meta.insert("method".into(), serde_json::json!(&paid.method));
    meta.insert("status".into(), serde_json::json!(invoice.status));
    if !paid.reference.trim().is_empty() {
        meta.insert("reference".into(), serde_json::json!(&paid.reference));
    }
    record_timeline(
        &app,
        id,
        InvoiceTimelineEventKind::Paid,
        format!(
            "Payment of {} recorded.",
            money_for_email(paid.amount_minor, &invoice.currency.0)
        ),
        actor.0.name.clone(),
        Some(meta),
    )
    .await;
    Ok(Json(invoice).into_response())
}

/// Forgive the remaining balance of an open invoice (#114). Requires a
/// non-empty reason; 409 on draft/paid/written-off states.
pub async fn write_off_invoice(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<WriteOffInput>,
) -> ApiResult {
    get_invoice_or_404(&app, id)?;
    if input.reason.trim().is_empty() {
        return Err(ApiError::validation(vec![FieldError::new(
            "reason",
            "a write-off reason is required",
        )]));
    }
    if input.reason.chars().count() > 500 {
        return Err(ApiError::validation(vec![FieldError::new(
            "reason",
            "reason too long (max 500 characters)",
        )]));
    }
    let invoice = app
        .store
        .write_off_invoice(id, input.reason, app.clock.now())?;
    app.audit.record(
        "invoice_write_off",
        &format!("{}:{}", id, actor.0.id),
        app.clock.now(),
    );
    // Timeline (Subtask B): the forgiving transition. The forgiven balance is
    // derived from the returned document, which is race-free — the payment
    // ledger is final once the write-off is committed and `balance_minor()` is
    // 0 in this state. The reason is already on the invoice document, so it is
    // non-secret; it goes in `meta` rather than being echoed into a sentence.
    let forgiven_minor = invoice.total_minor.saturating_sub(invoice.paid_minor());
    let mut meta = serde_json::Map::new();
    meta.insert(
        "reason".into(),
        serde_json::json!(&invoice.write_off_reason),
    );
    meta.insert("balance_minor".into(), serde_json::json!(forgiven_minor));
    record_timeline(
        &app,
        id,
        InvoiceTimelineEventKind::WriteOff,
        "Remaining balance written off.".into(),
        actor.0.name.clone(),
        Some(meta),
    )
    .await;
    Ok(Json(invoice).into_response())
}

pub async fn invoice_summary(State(app): State<AppState>) -> ApiResult {
    let invoices = app.store.list_invoices()?;
    let summary = crate::domain::summarise_invoices(&invoices, app.clock.today());
    Ok(Json(summary).into_response())
}

pub async fn invoice_report_handler(
    State(app): State<AppState>,
    Query(q): Query<RangeQuery>,
) -> ApiResult {
    let (from, to) = q.dates()?;
    let invoices = app.store.list_invoices()?;
    let customers = app.store.list_customers()?;
    let report = report::invoice_report(&invoices, &customers, from, to);
    Ok(Json(report).into_response())
}

pub async fn invoice_export_csv(State(app): State<AppState>) -> ApiResult {
    let invoices = app.store.list_invoices()?;
    let customers = app.store.list_customers()?;
    let csv = report::invoice_csv(&invoices, &customers);
    Ok((
        [(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")],
        csv,
    )
        .into_response())
}

pub async fn delete_invoice(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    if invoice.status != InvoiceStatus::Draft {
        return Err(ApiError::conflict("only a draft invoice can be deleted"));
    }
    app.store.delete_invoice(id)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
