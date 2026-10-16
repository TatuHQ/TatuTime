use crate::support::*;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn second_retainer_close_is_409_not_500() {
    // #187: the store transaction erased RetainerError into Io(String) and
    // close_retainer never applied the substring rescue — closing a closed
    // retainer answered 500. Typed errors make every mutation 409.
    let (app, _d) = app().await;
    let rid = seed_retainer(&app, 10_000).await;
    let (s1, b1) = json_req(&app, "POST", &format!("/api/retainers/{rid}"), None).await;
    assert_eq!(s1, StatusCode::OK, "{b1}");
    let (s2, b2) = json_req(&app, "POST", &format!("/api/retainers/{rid}"), None).await;
    assert_eq!(s2, StatusCode::CONFLICT, "second close: {b2}");
    // A draw on the closed retainer is the same 409 class, not a 500.
    let (s3, b3) = json_req(
        &app,
        "POST",
        &format!("/api/retainers/{rid}/draw"),
        Some(json!({"amount_minor": 100, "reason": "late draw"})),
    )
    .await;
    assert_eq!(s3, StatusCode::CONFLICT, "draw after close: {b3}");
}

// ------------------------------------------------------------------ #144 ---

async fn seed_retainer(app: &Client, opening: u64) -> String {
    let c = new_customer(app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(app, &cid, "WEB", json!({"rate_minor": 6000})).await;
    let (s, r) = json_req(
        app,
        "POST",
        "/api/retainers",
        Some(json!({"customer_id": cid, "project_code": "WEB", "opening_amount_minor": opening})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    r["retainer"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn retainer_ledger_reconciles_and_guarded_ops_persist_nothing() {
    let (app, dir) = app().await;
    let rid = seed_retainer(&app, 50_000).await;
    let (_s, d) = json_req(&app, "GET", &format!("/api/retainers/{rid}"), None).await;
    assert_eq!(d["balance_minor"], 50_000);
    assert_eq!(d["retainer"]["transactions"].as_array().unwrap().len(), 1);

    // credit then draw: balance reconciles from the ledger every read.
    let (sc, cred) = json_req(
        &app,
        "POST",
        &format!("/api/retainers/{rid}/credit"),
        Some(json!({"amount_minor": 10_000, "reason": "top up", "idempotency_key": "k1"})),
    )
    .await;
    assert_eq!(sc, StatusCode::OK, "{cred}");
    assert_eq!(cred["balance_minor"], 60_000);
    let (sd, drawn) = json_req(
        &app,
        "POST",
        &format!("/api/retainers/{rid}/draw"),
        Some(json!({"amount_minor": 25_000, "reason": "March work", "idempotency_key": "k2"})),
    )
    .await;
    assert_eq!(sd, StatusCode::OK, "{drawn}");
    assert_eq!(drawn["balance_minor"], 35_000);
    // balance is NEVER stored: the doc has no balance field, only txs.
    let doc_raw = std::fs::read_to_string(
        dir.path()
            .join("data")
            .join("retainers")
            .join(format!("{rid}.json")),
    )
    .unwrap();
    assert!(!doc_raw.contains("balance_minor"), "ledger only");

    // Replay of the same idempotency key: 409, no second tx.
    let (sr, br) = json_req(
        &app,
        "POST",
        &format!("/api/retainers/{rid}/credit"),
        Some(json!({"amount_minor": 10_000, "reason": "double click", "idempotency_key": "k1"})),
    )
    .await;
    assert_eq!(sr, StatusCode::CONFLICT, "{br}");
    let (_s3, d3) = json_req(&app, "GET", &format!("/api/retainers/{rid}"), None).await;
    assert_eq!(d3["balance_minor"], 35_000, "replay did not move money");

    // Guards: zero amount 422; draw without reason 422; overdraw 409; wrong currency 422.
    let (s0, _) = json_req(
        &app,
        "POST",
        &format!("/api/retainers/{rid}/credit"),
        Some(json!({"amount_minor": 0})),
    )
    .await;
    assert_eq!(s0, StatusCode::UNPROCESSABLE_ENTITY);
    let (srn, _) = json_req(
        &app,
        "POST",
        &format!("/api/retainers/{rid}/draw"),
        Some(json!({"amount_minor": 100})),
    )
    .await;
    assert_eq!(srn, StatusCode::UNPROCESSABLE_ENTITY);
    let (sov, _) = json_req(
        &app,
        "POST",
        &format!("/api/retainers/{rid}/draw"),
        Some(json!({"amount_minor": 35_001, "reason": "greed"})),
    )
    .await;
    assert_eq!(sov, StatusCode::CONFLICT);
    let (scur, _) = json_req(
        &app,
        "POST",
        &format!("/api/retainers/{rid}/credit"),
        Some(json!({"amount_minor": 100, "currency": "USD"})),
    )
    .await;
    assert_eq!(scur, StatusCode::UNPROCESSABLE_ENTITY);
    let (_sv2, dv2) = json_req(&app, "GET", &format!("/api/retainers/{rid}"), None).await;
    assert_eq!(dv2["balance_minor"], 35_000, "rejected ops changed nothing");

    // Close is final: further mutations 409; history stays readable.
    let (scl, _) = json_req(&app, "POST", &format!("/api/retainers/{rid}"), None).await;
    assert_eq!(scl, StatusCode::OK);
    let (scl2, _) = json_req(
        &app,
        "POST",
        &format!("/api/retainers/{rid}/credit"),
        Some(json!({"amount_minor": 100})),
    )
    .await;
    assert_eq!(scl2, StatusCode::CONFLICT);
    let (_sl, dl) = json_req(&app, "GET", &format!("/api/retainers/{rid}"), None).await;
    assert_eq!(dl["retainer"]["status"], "closed");
    assert_eq!(dl["retainer"]["transactions"].as_array().unwrap().len(), 3);

    // Member: 403 across the surface; unknown id: 404.
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Eve","email":"eve@test.local","password":"evepass123","role":"member"}),
        ),
    )
    .await;
    let cookie = login_cookie(&app.router, "eve@test.local", "evepass123").await;
    let (sm, _, _) = raw(&app.router, "GET", "/api/retainers", None, Some(&cookie)).await;
    assert_eq!(sm, StatusCode::FORBIDDEN);
    let (s404, _) = json_req(
        &app,
        "POST",
        "/api/retainers/00000000-0000-0000-0000-000000000000/credit",
        Some(json!({"amount_minor": 1})),
    )
    .await;
    assert_eq!(s404, StatusCode::NOT_FOUND);
}
