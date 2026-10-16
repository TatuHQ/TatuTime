use crate::support::*;

use axum::http::StatusCode;
use serde_json::json;

// ------------------------------------------------------------ submissions --

async fn seed_week(app: &Client, cid: &str) -> String {
    let (_, e) = json_req(
        app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    e["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn submit_locks_and_reject_unlocks() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let eid = seed_week(&app, cid).await;

    let (s, sub) = json_req(
        &app,
        "POST",
        "/api/submissions",
        Some(json!({"week_start":"2026-09-28"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{sub}");
    let sid = sub["id"].as_str().unwrap().to_string();
    let edit_body = json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":4});

    // Submitted -> entry locked.
    let (s1, _) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(edit_body.clone()),
    )
    .await;
    assert_eq!(s1, StatusCode::CONFLICT);

    // Reject -> unlocked.
    let (sd, _) = json_req(
        &app,
        "POST",
        &format!("/api/submissions/{sid}/decision"),
        Some(json!({"decision":"reject","comment":"redo"})),
    )
    .await;
    assert_eq!(sd, StatusCode::OK);
    let (s2, _) = json_req(&app, "PUT", &format!("/api/entries/{eid}"), Some(edit_body)).await;
    assert_eq!(s2, StatusCode::OK);
}

#[tokio::test]
async fn approve_keeps_locked_and_double_submit_refused() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let eid = seed_week(&app, cid).await;

    let (_, sub) = json_req(
        &app,
        "POST",
        "/api/submissions",
        Some(json!({"week_start":"2026-09-28"})),
    )
    .await;
    let sid = sub["id"].as_str().unwrap();
    json_req(
        &app,
        "POST",
        &format!("/api/submissions/{sid}/decision"),
        Some(json!({"decision":"approve"})),
    )
    .await;

    // Approved -> still locked.
    let (s_edit, _) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":4})),
    )
    .await;
    assert_eq!(s_edit, StatusCode::CONFLICT);

    // Resubmitting the same week: the only entry is locked, so nothing to submit.
    let (s2, _) = json_req(
        &app,
        "POST",
        "/api/submissions",
        Some(json!({"week_start":"2026-09-28"})),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT);
}

/// #262 Tier 2: submissions POST rejects an absent/malformed `week_start`, and
/// the decision endpoint rejects a malformed `decision` without changing state.
#[tokio::test]
async fn submission_invalid_shapes_are_422() {
    let (app, _d) = app().await;
    // Absent required field -> deserialisation 422.
    let (s_absent, body) = json_req(&app, "POST", "/api/submissions", Some(json!({}))).await;
    assert_eq!(s_absent, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    // Malformed date -> the shared parse_date 400.
    let (s_bad, _) = json_req(
        &app,
        "POST",
        "/api/submissions",
        Some(json!({"week_start":"not-a-date"})),
    )
    .await;
    assert_eq!(s_bad, StatusCode::BAD_REQUEST);

    // A real submission, then a malformed decision must 422 and not transition.
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(2), "2026-09-28")),
    )
    .await;
    let (ss, sub) = json_req(
        &app,
        "POST",
        "/api/submissions",
        Some(json!({"week_start":"2026-09-28"})),
    )
    .await;
    assert_eq!(ss, StatusCode::CREATED, "{sub}");
    let sid = sub["id"].as_str().unwrap();
    let (s_dec, dec_body) = json_req(
        &app,
        "POST",
        &format!("/api/submissions/{sid}/decision"),
        Some(json!({"decision":"maybe"})),
    )
    .await;
    assert_eq!(s_dec, StatusCode::UNPROCESSABLE_ENTITY, "{dec_body}");
    assert_eq!(dec_body["error"]["code"], "validation_failed");
    let (_, listed) = json_req(&app, "GET", "/api/submissions", None).await;
    assert_eq!(
        listed["submissions"][0]["state"], "submitted",
        "a refused decision must not transition the submission"
    );
}
