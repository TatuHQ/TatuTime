use crate::support::*;

use axum::http::StatusCode;
use serde_json::json;

// ------------------------------------------------- fail-closed locking (185) --

#[tokio::test]
async fn unverifiable_lock_state_refuses_writes_but_never_reads() {
    // #185: lock providers used to map a store error to "not locked". With an
    // unreadable invoice document the locks must fail CLOSED: entry edit and
    // delete get 503 (Retry-After), submission is refused, reads still work.
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    let (_, e) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(&cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    let eid = e["id"].as_str().unwrap().to_string();

    // Corrupt the invoices collection so InvoiceLock cannot verify its state.
    let inv_dir = d.path().join("data").join("invoices");
    std::fs::create_dir_all(&inv_dir).unwrap();
    std::fs::write(inv_dir.join("corrupt.json"), b"{ this is not json").unwrap();

    let body = json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2,"billable":true});
    let (s_edit, _) = json_req(&app, "PUT", &format!("/api/entries/{eid}"), Some(body)).await;
    assert_eq!(
        s_edit,
        StatusCode::SERVICE_UNAVAILABLE,
        "edit must fail closed"
    );
    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(
        s_del,
        StatusCode::SERVICE_UNAVAILABLE,
        "delete must fail closed"
    );
    let (s_sub, _) = json_req(
        &app,
        "POST",
        "/api/submissions",
        Some(json!({"week_start": "2026-09-28"})),
    )
    .await;
    assert_eq!(
        s_sub,
        StatusCode::SERVICE_UNAVAILABLE,
        "submit must fail closed"
    );
    let (s_get, _) = json_req(&app, "GET", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(s_get, StatusCode::OK, "reads are never blocked");
}

#[tokio::test]
async fn unverifiable_claim_state_refuses_expense_delete_but_never_reads() {
    // #255 (+ the #190 follow-up): the expense lock now flows through the same
    // CombinedLocks seam the entries use, so a claim provider that cannot
    // verify its state fails the delete CLOSED with 503 — before the fold this
    // path surfaced a raw store error as a 500. Reads stay untouched.
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    let (_, x) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"amount_minor":5000,"currency":"EUR"})),
    )
    .await;
    let xid = x["id"].as_str().unwrap().to_string();

    // Corrupt the claims collection so the claim provider cannot verify state.
    let claims_dir = d.path().join("data").join("claims");
    std::fs::create_dir_all(&claims_dir).unwrap();
    std::fs::write(claims_dir.join("corrupt.json"), b"{ not json").unwrap();

    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None).await;
    assert_eq!(
        s_del,
        StatusCode::SERVICE_UNAVAILABLE,
        "expense delete must fail closed when the lock state is unverifiable"
    );
    let (s_get, _) = json_req(&app, "GET", &format!("/api/expenses/{xid}"), None).await;
    assert_eq!(s_get, StatusCode::OK, "reads are never blocked");
}

#[tokio::test]
async fn expense_on_partly_paid_invoice_stays_locked() {
    // #218: the expense-delete scan said '== Issued' while the invoice locks
    // (and the dashboard treats as open) issued | partly_paid — a partial
    // payment silently unlocked the expense rows sitting on that invoice.
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({"rate_minor": 6000})).await;
    let (_s, x) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(
            json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1",
                    "amount_minor":5000,"currency":"EUR","billable":true,"note":"parts"}),
        ),
    )
    .await;
    let xid = x["id"].as_str().unwrap().to_string();
    let (_s2, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    assert_eq!(inv["total_minor"], 5000, "expense-only invoice: {inv}");
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    // Partial payment: issued -> partly_paid.
    let (_sp, paid) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"amount_minor": 2000, "reference": "wire-1"})),
    )
    .await;
    assert_eq!(paid["status"], "partly_paid", "{paid}");
    let (s_del, b_del) = json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None).await;
    assert_eq!(
        s_del,
        StatusCode::CONFLICT,
        "partly_paid must lock its expenses: {b_del}"
    );
    assert!(
        b_del["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("issued invoice"),
        "{b_del}"
    );
    // Invoiced records are PERMANENT: settling the invoice no longer
    // releases its expense — the document is the record of what was billed.
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"amount_minor": 3000, "reference": "wire-2"})),
    )
    .await;
    let (s_after, b_after) = json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None).await;
    assert_eq!(
        s_after,
        StatusCode::CONFLICT,
        "paid keeps the lock: {b_after}"
    );
}

#[tokio::test]
async fn invoiced_time_is_permanently_locked() {
    // The lifecycle rule: once an entry sits on an ISSUED invoice it can
    // never be edited or deleted again — not while open, not after the
    // invoice is settled. Only drafts leave lines free.
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({"rate_minor": 6000})).await;
    let (_s0, e) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid.clone(),"project_code":"P1","hours":3})),
    )
    .await;
    let eid = e["id"].as_str().unwrap().to_string();
    let (_s2, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    // A draft locks nothing.
    let (s_draft, _) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":4})),
    )
    .await;
    assert_eq!(s_draft, StatusCode::OK, "draft invoices must not lock");
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (s_open, b_open) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":5})),
    )
    .await;
    assert_eq!(s_open, StatusCode::CONFLICT, "issued locks: {b_open}");
    assert!(
        b_open["error"]["message"]
            .as_str()
            .unwrap()
            .contains("permanent")
    );
    // Settle it in full: the lock SURVIVES settlement (this is the change —
    // previously paying released the entry for editing).
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"amount_minor": 18000, "reference": "wire-full"})),
    )
    .await;
    let (s_paid, _) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":6})),
    )
    .await;
    assert_eq!(s_paid, StatusCode::CONFLICT, "paid keeps the entry locked");
    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(
        s_del,
        StatusCode::CONFLICT,
        "paid keeps the entry undeletable"
    );
    // And the stored hours are exactly what was invoiced (3h at PUT-to-4h
    // was allowed pre-issue; the last write before issue won).
    let (_s3, got) = json_req(&app, "GET", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(got["hours"], 4.0);
}
