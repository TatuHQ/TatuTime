use crate::api::*;

/// Body for `POST /invoices/{id}/sync`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncInput {
    pub provider: String,
}

/// Record one sync attempt from a provider push outcome, translating the
/// Ok/Err into Synced/Failed with the shared `SyncAttempt` shape (#189).
///
/// #254: `record_sync` read-modify-writes the sync ledger under the store
/// write lock (which polls with `std::thread::sleep` on contention); it runs on
/// the blocking pool like every other store access here.
async fn record_sync_from(
    app: &AppState,
    provider: &str,
    kind: &str,
    invoice: &Invoice,
    detail: String,
    outcome: Result<String, crate::accounting::SyncError>,
) -> Result<crate::accounting::SyncRecord, ApiError> {
    let (remote_id, status, error) = match outcome {
        Ok(remote) => (remote, crate::accounting::SyncStatus::Synced, String::new()),
        Err(e) => (
            String::new(),
            crate::accounting::SyncStatus::Failed,
            e.to_string(),
        ),
    };
    let provider = provider.to_owned();
    let kind = kind.to_owned();
    let invoice = invoice.clone();
    let now = app.clock.now();
    store_blocking("accounting sync record", &app.store, move |s| {
        crate::accounting::record_sync(
            s,
            crate::accounting::SyncAttempt {
                provider: &provider,
                kind: &kind,
                invoice: &invoice,
                remote_id,
                status,
                error,
                now,
                detail,
            },
        )
    })
    .await
}

/// Copies an issued invoice to the accounting provider (#33). Idempotent: an
/// already-synced invoice is a no-op, and failures are recorded rather than
/// thrown (sync never blocks the invoice flow).
pub async fn sync_invoice(
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<SyncInput>,
) -> ApiResult {
    let invoice = store_blocking("sync invoice load", &app.store, move |s| s.get_invoice(id))
        .await?
        .ok_or_else(|| ApiError::not_found("invoice"))?;
    if invoice.status == crate::domain::InvoiceStatus::Draft {
        return Err(ApiError::conflict("issue the invoice before syncing it"));
    }
    let provider = provider_or_400(app.accounting.get(&input.provider), "accounting")?;
    let customer = store_blocking("sync customer", &app.store, move |s| {
        s.get_customer(invoice.customer_id)
    })
    .await?
    .ok_or_else(|| ApiError::not_found("customer"))?;
    let records = store_blocking("sync records", &app.store, |s| {
        crate::accounting::list_records(s)
    })
    .await?;
    // A legacy `Paid` invoice predating the ledger (#114) is represented as
    // one payment of the total under the empty detail.
    let detail_of = |p: &crate::domain::InvoicePayment| p.id.to_string();
    let pending: Vec<(String, u64, String, chrono::DateTime<chrono::Utc>)> =
        if invoice.payments.is_empty() {
            if invoice.paid_minor() > 0 {
                vec![(
                    String::new(),
                    invoice.paid_minor(),
                    invoice.payment_reference.clone(),
                    invoice.paid_at.unwrap_or_else(|| app.clock.now()),
                )]
            } else {
                Vec::new()
            }
        } else {
            invoice
                .payments
                .iter()
                .filter(|p| {
                    !records.iter().any(|r| {
                        r.provider == provider.name()
                            && r.kind == "payment"
                            && r.invoice_id == invoice.id
                            && r.detail == detail_of(p)
                            && r.status == crate::accounting::SyncStatus::Synced
                    })
                })
                .map(|p| {
                    (
                        detail_of(p),
                        p.amount_minor,
                        p.reference.clone(),
                        p.received_at,
                    )
                })
                .collect()
        };
    if let Some(prev) = records
        .iter()
        .find(|r| {
            r.provider == provider.name() && r.kind == "invoice" && r.invoice_id == invoice.id
        })
        .cloned()
        .filter(|r| r.status == crate::accounting::SyncStatus::Synced)
    {
        // Invoice already synced: push every collected-but-unsynced payment
        // against the remote invoice (#33, #114), else a plain no-op.
        if pending.is_empty() {
            return Ok(Json(serde_json::json!({ "record": prev, "noop": true })).into_response());
        }
        let mut last: Option<crate::accounting::SyncRecord> = None;
        for pending in &pending {
            // #254: the provider push is a blocking ureq HTTP call; each one
            // runs on the blocking pool, off the async worker.
            let push = provider.clone();
            let remote = prev.remote_id.clone();
            let currency = invoice.currency.0.clone();
            let reference = pending.2.clone();
            let (amount, paid_at, detail) = (pending.1, pending.3, pending.0.clone());
            let outcome = blocking("accounting push_payment", move || {
                Ok(push.push_payment(&remote, amount, &currency, &reference, paid_at))
            })
            .await?;
            let rec = record_sync_from(&app, provider.name(), "payment", &invoice, detail, outcome)
                .await?;
            if rec.status == crate::accounting::SyncStatus::Synced {
                record_audit(&app, "accounting_payment_sync", &invoice.number).await;
            }
            last = Some(rec);
        }
        return Ok(Json(
            serde_json::json!({ "record": last, "invoice_synced": prev.remote_id, "payments": pending.len() }),
        )
        .into_response());
    }
    let key = crate::accounting::invoice_key(provider.name(), &invoice.number);
    // #254: `push_invoice` is a blocking ureq HTTP call — offload it.
    let push = provider.clone();
    let doc_invoice = invoice.clone();
    let doc_customer = customer.clone();
    let pushed = blocking("accounting push_invoice", move || {
        let doc = crate::accounting::InvoiceDoc {
            invoice: &doc_invoice,
            customer: &doc_customer,
        };
        Ok(push.push_invoice(&doc, &key))
    })
    .await?;
    match pushed {
        Ok(remote) => {
            // Synced invoices also carry their ledger, in real amounts.
            for pending in &pending {
                let push = provider.clone();
                let remote_id = remote.clone();
                let currency = invoice.currency.0.clone();
                let reference = pending.2.clone();
                let (amount, paid_at, detail) = (pending.1, pending.3, pending.0.clone());
                let outcome = blocking("accounting push_payment", move || {
                    Ok(push.push_payment(&remote_id, amount, &currency, &reference, paid_at))
                })
                .await?;
                if let Err(e) = &outcome {
                    tracing::warn!(error = %e, "payment sync failed (invoice synced anyway)");
                }
                record_sync_from(&app, provider.name(), "payment", &invoice, detail, outcome)
                    .await?;
            }
            let rec = record_sync_from(
                &app,
                provider.name(),
                "invoice",
                &invoice,
                String::new(),
                Ok(remote),
            )
            .await?;
            record_audit(&app, "accounting_sync", &invoice.number).await;
            Ok(Json(serde_json::json!({ "record": rec })).into_response())
        }
        Err(e) => {
            let rec = record_sync_from(
                &app,
                provider.name(),
                "invoice",
                &invoice,
                String::new(),
                Err(e),
            )
            .await?;
            Ok(Json(serde_json::json!({ "record": rec, "failed": true })).into_response())
        }
    }
}

/// Visible sync status for the dashboard (#33): every provider record.
pub async fn sync_status(State(app): State<AppState>) -> ApiResult {
    let records = crate::accounting::list_records(&app.store)?;
    Ok(Json(serde_json::json!({
        "records": records,
        "providers": app.accounting.names(),
    }))
    .into_response())
}
