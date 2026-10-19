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
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
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
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
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

/// QA-09: an approved week's 409 must not offer an action the API refuses —
/// rejecting an approved submission is itself a 409. A submitted week keeps the
/// actionable "reject it first".
#[tokio::test]
async fn approved_week_lock_message_does_not_offer_reject() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    let eid = seed_week(&app, &cid).await;
    let (_, sub) = json_req(
        &app,
        "POST",
        "/api/submissions",
        Some(json!({"week_start":"2026-09-28"})),
    )
    .await;
    let sid = sub["id"].as_str().unwrap().to_string();
    let edit = json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":4});

    // Submitted: the rejection is possible, so the message may suggest it.
    let (s_sub, b_sub) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(edit.clone()),
    )
    .await;
    assert_eq!(s_sub, StatusCode::CONFLICT, "{b_sub}");
    assert!(
        b_sub["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("reject"),
        "a submitted week can be rejected: {b_sub}"
    );

    // Approve, then the same edit must say permanent, never "reject".
    json_req(
        &app,
        "POST",
        &format!("/api/submissions/{sid}/decision"),
        Some(json!({"decision":"approve"})),
    )
    .await;
    let (s_app, b_app) = json_req(&app, "PUT", &format!("/api/entries/{eid}"), Some(edit)).await;
    assert_eq!(s_app, StatusCode::CONFLICT, "{b_app}");
    assert_eq!(b_app["error"]["code"], "conflict", "{b_app}");
    let message = b_app["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("approved"), "{b_app}");
    assert!(
        !message.contains("reject") && !message.contains("withdraw"),
        "an approved week cannot be rejected, so the message must not offer it: {b_app}"
    );

    // The action the old message offered is indeed refused.
    let (s_rej, _) = json_req(
        &app,
        "POST",
        &format!("/api/submissions/{sid}/decision"),
        Some(json!({"decision":"reject"})),
    )
    .await;
    assert_eq!(s_rej, StatusCode::CONFLICT);
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
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
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
