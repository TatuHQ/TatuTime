use crate::support::*;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use http_body_util::BodyExt;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use tucano_time::store::Store;

#[tokio::test]
async fn concurrent_identical_webhook_records_exactly_one_payment() {
    // #185: the webhook idempotency check ran on an unlocked snapshot, so two
    // concurrent deliveries of the same event could both post a partial
    // payment. The dedup now runs inside record_payment_once's write lock.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let stripe = tucano_time::payments::StripeProvider::with_secret("whsec_dup");
    let app = Client::with_payments(
        store,
        tucano_time::payments::PaymentRegistry::new(vec![Arc::new(stripe)]),
    )
    .await;

    let c = new_customer(&app, "ACME", "EUR", 6000).await;
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
    let iid = inv["id"].as_str().unwrap().to_string();
    let number = inv["number"].as_str().unwrap().to_string();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;

    // A PARTIAL (6000 of 18000) with a fixed provider event id, delivered twice
    // concurrently: a full settle would end at "already_paid" before the
    // reference check, so only a partial exercises the locked dedup.
    let payload = format!(
        "{{\"id\":\"evt_dup_1\",\"type\":\"checkout.session.completed\",\"payment_status\":\"paid\",\"amount_minor\":6000,\"currency\":\"EUR\",\"client_reference_id\":\"pay_ref_dup\",\"metadata\":{{\"invoice_number\":\"{number}\"}}}}"
    );
    let sig = format!(
        "sha256={}",
        tucano_time::payments::sign("whsec_dup", &payload)
    );
    let ((s1, b1), (s2, b2)) = tokio::join!(
        webhook_post(&app.router, "stripe", &payload, Some(&sig)),
        webhook_post(&app.router, "stripe", &payload, Some(&sig)),
    );
    assert_eq!(s1, StatusCode::OK, "{b1}");
    assert_eq!(s2, StatusCode::OK, "{b2}");
    // One settles as a new payment, the other as an idempotent replay — never
    // two ledger entries for the same event id.
    let statuses: Vec<&str> = vec![
        b1["status"].as_str().unwrap(),
        b2["status"].as_str().unwrap(),
    ];
    assert!(
        statuses.contains(&"already_processed"),
        "second delivery must be reported as a replay: {statuses:?}"
    );

    let (_s, got) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(got["status"], "partly_paid");
    let payments = got["payments"].as_array().unwrap();
    assert_eq!(
        payments.len(),
        1,
        "duplicate event must record exactly one payment"
    );
    assert_eq!(payments[0]["amount_minor"], 6000);

    let make_event = |event_id: &str| {
        format!(
            "{{\"id\":\"{event_id}\",\"type\":\"checkout.session.completed\",\"payment_status\":\"paid\",\"amount_minor\":10000,\"currency\":\"EUR\",\"client_reference_id\":\"pay_ref_dup\",\"metadata\":{{\"invoice_number\":\"{number}\"}}}}"
        )
    };
    let event_a = make_event("evt_distinct_a");
    let event_b = make_event("evt_distinct_b");
    let sig_a = format!(
        "sha256={}",
        tucano_time::payments::sign("whsec_dup", &event_a)
    );
    let sig_b = format!(
        "sha256={}",
        tucano_time::payments::sign("whsec_dup", &event_b)
    );
    let ((status_a, body_a), (status_b, body_b)) = tokio::join!(
        webhook_post(&app.router, "stripe", &event_a, Some(&sig_a)),
        webhook_post(&app.router, "stripe", &event_b, Some(&sig_b)),
    );
    assert_ne!(
        status_a, status_b,
        "only one distinct payment fits the remaining balance"
    );
    assert!(
        [status_a, status_b].contains(&StatusCode::CONFLICT),
        "the unrecorded competing event must remain retryable: {body_a} {body_b}"
    );
    let (_s, after_race) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    let race_payments = after_race["payments"].as_array().unwrap();
    assert_eq!(
        race_payments.len(),
        2,
        "only one competing event may be recorded"
    );
    assert_eq!(
        race_payments
            .iter()
            .map(|payment| payment["amount_minor"].as_u64().unwrap())
            .sum::<u64>(),
        16000,
        "recorded total must not exceed the invoice balance"
    );
}

#[tokio::test]
async fn invoice_pay_flow_and_summary() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
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

/// Posts a raw string body to the webhook endpoint WITHOUT the CSRF header,
/// proving the provider webhook is exempt and authenticated only by signature.
async fn webhook_post(
    router: &Router,
    provider: &str,
    body: &str,
    signature: Option<&str>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(format!("/api/payments/webhook/{provider}"))
        .header("content-type", "application/json");
    if let Some(s) = signature {
        b = b.header("x-webhook-signature", s);
    }
    let res = router
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .expect("oneshot");
    let status = res.status();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn checkout_then_signed_webhook_marks_invoice_paid() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let stripe = tucano_time::payments::StripeProvider::with_secret("whsec_topsecret");
    let registry = tucano_time::payments::PaymentRegistry::new(vec![Arc::new(stripe)]);
    let app = Client::with_payments(store, registry).await;

    let c = new_customer(&app, "ACME", "EUR", 6000).await;
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
    let iid = inv["id"].as_str().unwrap().to_string();
    let number = inv["number"].as_str().unwrap().to_string();

    // Draft -> checkout rejected.
    let (sd, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/checkout"),
        Some(json!({"provider":"stripe"})),
    )
    .await;
    assert_eq!(sd, StatusCode::CONFLICT);

    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;

    // Unknown provider rejected.
    let (su, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/checkout"),
        Some(json!({"provider":"bitcoin"})),
    )
    .await;
    assert_eq!(su, StatusCode::BAD_REQUEST);

    // Checkout link for the issued invoice.
    let (sc, session) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/checkout"),
        Some(json!({"provider":"stripe"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{session}");
    assert!(session["url"].as_str().unwrap().contains(&number));
    let reference = session["reference"].as_str().unwrap().to_string();

    // Unsigned / badly signed webhook rejected (and CSRF-exempt, since no
    // x-csrf header was sent at all).
    // Amount/currency ride the normalized event and must match the invoice
    // (3h @ 60.00 = 18000 EUR) — review A11.
    let payload = format!(
        "{{\"type\":\"checkout.session.completed\",\"payment_status\":\"paid\",\"amount_minor\":18000,\"currency\":\"EUR\",\"client_reference_id\":\"{reference}\",\"metadata\":{{\"invoice_number\":\"{number}\"}}}}"
    );
    let (sbad, _) = webhook_post(&app.router, "stripe", &payload, Some("sha256=deadbeef")).await;
    assert_eq!(sbad, StatusCode::UNAUTHORIZED);

    // Correct HMAC signature pays the invoice.
    let sig = tucano_time::payments::sign("whsec_topsecret", &payload);
    let (sgood, body) = webhook_post(&app.router, "stripe", &payload, Some(&sig)).await;
    assert_eq!(sgood, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "paid");
    let (_s2, got) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(got["status"], "paid");
    assert!(
        got["payment_reference"]
            .as_str()
            .unwrap()
            .contains("stripe:")
    );

    // Replay is idempotent.
    let (sre, re) = webhook_post(&app.router, "stripe", &payload, Some(&sig)).await;
    assert_eq!(sre, StatusCode::OK, "{re}");
    assert_eq!(re["status"], "already_paid");
}

#[tokio::test]
async fn webhook_partial_settles_partly_and_mismatch_stays_refused() {
    // Review A11 refined by #114: a signed event records a LEDGER PAYMENT of
    // its real amount — a partial parks at partly_paid (never a full settle),
    // a wrong-currency event is refused, over-collection is refused, and a
    // replay of the same provider event is idempotent.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let stripe: Arc<dyn tucano_time::payments::PaymentProvider> = Arc::new(
        tucano_time::payments::StripeProvider::with_secret("whsec_mismatch"),
    );
    let app = Client::with_payments(
        store,
        tucano_time::payments::PaymentRegistry::new(vec![stripe]),
    )
    .await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
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
    let iid = inv["id"].as_str().unwrap().to_string();
    let number = inv["number"].as_str().unwrap().to_string();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (_s2, session) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/checkout"),
        Some(json!({"provider":"stripe"})),
    )
    .await;
    let reference = session["reference"].as_str().unwrap().to_string();

    let make = |amount: u64, currency: &str, ev: &str| {
        format!(
            "{{\"type\":\"checkout.session.completed\",\"payment_status\":\"paid\",\"amount_minor\":{amount},\"currency\":\"{currency}\",\"client_reference_id\":\"{reference}\",\"metadata\":{{\"invoice_number\":\"{number}\"}},\"id\":\"{ev}\"}}"
        )
    };
    // Wrong currency: refused, no ledger entry.
    let wrongc = make(18000, "USD", "evt_usd");
    let sig = tucano_time::payments::sign("whsec_mismatch", &wrongc);
    let (sw, _) = webhook_post(&app.router, "stripe", &wrongc, Some(&sig)).await;
    assert_eq!(
        sw,
        StatusCode::CONFLICT,
        "currency mismatch must not settle"
    );

    // Over-collection (beyond the balance): refused (#114 keeps A11's "never
    // silently overpay" while allowing genuine partials).
    let over = make(19000, "EUR", "evt_over");
    let sig = tucano_time::payments::sign("whsec_mismatch", &over);
    let (so, _) = webhook_post(&app.router, "stripe", &over, Some(&sig)).await;
    assert_eq!(so, StatusCode::CONFLICT, "over-balance must be refused");

    // Partial: records a ledger payment, parks at partly_paid.
    let partial = make(5000, "EUR", "evt_part");
    let sig = tucano_time::payments::sign("whsec_mismatch", &partial);
    let (sp, bp) = webhook_post(&app.router, "stripe", &partial, Some(&sig)).await;
    assert_eq!(sp, StatusCode::OK, "{bp}");
    assert_eq!(bp["status"], "partly_paid");
    let (_s3, mid) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(
        mid["status"], "partly_paid",
        "partial does not fully settle"
    );
    assert_eq!(mid["payments"].as_array().unwrap().len(), 1);
    assert_eq!(mid["payments"][0]["amount_minor"], 5000);
    assert_eq!(mid["payments"][0]["method"], "stripe");

    // Replay of the SAME provider event: idempotent, ledger unchanged.
    let (sr, br) = webhook_post(&app.router, "stripe", &partial, Some(&sig)).await;
    assert_eq!(sr, StatusCode::OK, "{br}");
    assert_eq!(br["status"], "already_processed");
    let (_s4, mid2) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(
        mid2["payments"].as_array().unwrap().len(),
        1,
        "replay added no entry"
    );

    // A second distinct event covering the balance clears it.
    let rest = make(13000, "EUR", "evt_rest");
    let sig2 = tucano_time::payments::sign("whsec_mismatch", &rest);
    let (sp2, bp2) = webhook_post(&app.router, "stripe", &rest, Some(&sig2)).await;
    assert_eq!(sp2, StatusCode::OK, "{bp2}");
    assert_eq!(bp2["status"], "paid");

    // A distinct event cannot be acknowledged after another event has settled
    // the invoice unless its own reference is present in the ledger.
    let stale = make(13000, "EUR", "evt_stale");
    let stale_sig = tucano_time::payments::sign("whsec_mismatch", &stale);
    let (ss, _) = webhook_post(&app.router, "stripe", &stale, Some(&stale_sig)).await;
    assert_eq!(
        ss,
        StatusCode::CONFLICT,
        "unrecorded distinct event is not processed"
    );
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
