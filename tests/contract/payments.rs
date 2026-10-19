use crate::support::*;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn invoice_pay_flow_and_summary() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();

    // Issue sets due_date (net-14).
    let (_, issued) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(issued["status"], "issued");
    assert!(issued["due_date"].is_string(), "issue sets a due date");

    // Pay it.
    let (_, paid) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference":"bank-123"})),
    )
    .await;
    assert_eq!(paid["status"], "paid");
    assert_eq!(paid["payment_reference"], "bank-123");

    // Summary reflects one paid invoice, nothing outstanding.
    let (s, sum) = json_req(&app, "GET", "/api/invoices/summary", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(sum["paid"], 1);
    assert_eq!(sum["issued"], 0);
    assert!(sum["outstanding"].as_object().unwrap().is_empty());
}

// ------------------------------------------------------------------ #114 ---

#[tokio::test]
async fn record_payment_partial_then_settles_with_balance_semantics() {
    let (app, _d) = app().await;
    // 3h @ 60.00 = 18000 minor.
    let (_issued, iid) = seed_issued_invoice(&app).await;

    // A partial payment lands on partly_paid with a ledger entry.
    let (sp, part) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "bank:1", "amount_minor": 5000})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{part}");
    assert_eq!(part["status"], "partly_paid");
    let pays = part["payments"].as_array().unwrap();
    assert_eq!(pays.len(), 1);
    assert_eq!(pays[0]["amount_minor"], 5000);
    assert_eq!(pays[0]["method"], "manual");
    assert!(pays[0]["id"].is_string());
    assert!(pays[0]["received_at"].is_string());

    // Summary uses the BALANCE now: 13000 outstanding, not 18000.
    let (_s1, sum) = json_req(&app, "GET", "/api/invoices/summary", None).await;
    assert_eq!(sum["partly_paid"], 1);
    assert_eq!(sum["issued"], 0);
    assert_eq!(sum["outstanding"]["EUR"], 13000);

    // Zero amount: 422, ledger untouched.
    let (s0, _b0) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "x", "amount_minor": 0})),
    )
    .await;
    assert_eq!(s0, StatusCode::UNPROCESSABLE_ENTITY);

    // Over-balance: 409, still no persistence.
    let (so, bo) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "x", "amount_minor": 13001})),
    )
    .await;
    assert_eq!(so, StatusCode::CONFLICT, "{bo}");
    let (_s2, mid) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(
        mid["payments"].as_array().unwrap().len(),
        1,
        "rejections persisted nothing"
    );

    // The exact balance settles it; legacy mirror fields stay truthful.
    let (sf, paid) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "bank:2", "amount_minor": 13000})),
    )
    .await;
    assert_eq!(sf, StatusCode::OK, "{paid}");
    assert_eq!(paid["status"], "paid");
    assert!(paid["paid_at"].is_string());
    assert_eq!(paid["payment_reference"], "bank:2");
    assert_eq!(paid["payments"].as_array().unwrap().len(), 2);

    // No balance left: further payments conflict.
    let (sx, _bx) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "x", "amount_minor": 1})),
    )
    .await;
    assert_eq!(sx, StatusCode::CONFLICT);

    // Audit trail (#52): two invoice_payment events.
    let (_sa, audit) = json_req(&app, "GET", "/api/audit", None).await;
    let n = audit["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"] == "invoice_payment")
        .count();
    assert_eq!(n, 2, "{audit}");
}

#[tokio::test]
async fn pay_without_amount_still_pays_in_full() {
    // The default (no amount_minor) keeps #27's all-or-nothing behaviour.
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    let (s, paid) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "cheque:9"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{paid}");
    assert_eq!(paid["status"], "paid");
    assert_eq!(paid["payments"][0]["amount_minor"], 18000);
}

#[tokio::test]
async fn write_off_requires_reason_and_excludes_the_balance() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;

    // Missing/blank reason: 422 without persistence.
    let (sr, _br) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/write-off"),
        Some(json!({"reason": "   "})),
    )
    .await;
    assert_eq!(sr, StatusCode::UNPROCESSABLE_ENTITY);

    // Valid write-off of the open invoice.
    let (sw, wo) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/write-off"),
        Some(json!({"reason": "customer insolvent, debt forgiven 2026-10"})),
    )
    .await;
    assert_eq!(sw, StatusCode::OK, "{wo}");
    assert_eq!(wo["status"], "written_off");
    assert_eq!(
        wo["write_off_reason"],
        "customer insolvent, debt forgiven 2026-10"
    );
    assert!(wo["written_off_at"].is_string());

    // Outstanding drops to zero but the invoice is counted separately.
    let (_s1, sum) = json_req(&app, "GET", "/api/invoices/summary", None).await;
    assert_eq!(sum["written_off"], 1);
    assert_eq!(sum["issued"], 0);
    assert!(
        sum["outstanding"].as_object().unwrap().is_empty()
            || sum["outstanding"]["EUR"] == serde_json::json!(0),
        "{sum}"
    );

    // Final: re-writing-off and paying both conflict.
    let (s2, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/write-off"),
        Some(json!({"reason": "again"})),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT);
    let (s3, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "x", "amount_minor": 100})),
    )
    .await;
    assert_eq!(s3, StatusCode::CONFLICT);

    // Audit: invoice_write_off recorded with id + actor.
    let (_sa, audit) = json_req(&app, "GET", "/api/audit", None).await;
    assert!(
        audit["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "invoice_write_off")
    );
}

#[tokio::test]
async fn write_off_rejects_draft_and_paid_states() {
    let (app, _d) = app().await;
    // Draft: refused.
    let c = new_customer(&app, "NOEMAIL", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let did = inv["id"].as_str().unwrap().to_string();
    let (sd, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{did}/write-off"),
        Some(json!({"reason": "early forgiveness"})),
    )
    .await;
    assert_eq!(sd, StatusCode::CONFLICT, "drafts cannot be written off");

    // Paid: refused too (nothing left to forgive).
    let (_issued, iid) = seed_issued_invoice(&app).await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "full"})),
    )
    .await;
    let (sp, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/write-off"),
        Some(json!({"reason": "too late"})),
    )
    .await;
    assert_eq!(sp, StatusCode::CONFLICT);
}

#[tokio::test]
async fn partly_paid_invoice_still_locks_its_entries() {
    let (app, _d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "half", "amount_minor": 9000})),
    )
    .await;
    let eid = issued["lines"][0]["entry_id"].as_str().unwrap();
    let (s, _) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-02","customer_id":issued["customer_id"],"project_code":"P1","hours":4})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "partly paid keeps the #18 lock");
}

#[tokio::test]
async fn legacy_paid_docs_read_with_synthesised_ledger() {
    // A document written before #114: status=paid, no payments array. The
    // API must read it, treat the total as paid and show zero balance — the
    // open invoices dashboard must not overstate receivables.
    let (app, d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "old:1"})),
    )
    .await;
    let path = d
        .path()
        .join("data")
        .join("invoices")
        .join(format!("{iid}.json"));
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(raw["status"], "paid");
    // Rewrite it as a legacy doc (strip the ledger).
    let mut legacy = raw.as_object().unwrap().clone();
    legacy.remove("payments");
    std::fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let (_s, sum) = json_req(&app, "GET", "/api/invoices/summary", None).await;
    assert_eq!(sum["paid"], 1);
    assert!(
        sum["outstanding"].as_object().unwrap().is_empty()
            || sum["outstanding"]["EUR"] == serde_json::json!(0),
        "legacy paid counts as settled: {sum}"
    );
    let (sr, rep) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(sr, StatusCode::OK, "{rep}");
    assert_eq!(rep["payments"].as_array().map(Vec::len).unwrap_or(0), 0);
    // Reports see it fully paid (synthesised).
    let (_s2, rep2) = json_req(
        &app,
        "GET",
        "/api/invoices/report?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    let row = rep2["rows"].as_array().unwrap().first().unwrap();
    assert_eq!(row["paid_minor"], 18000);
    assert_eq!(row["balance_minor"], 0);
}
