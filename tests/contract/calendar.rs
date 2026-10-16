use crate::support::*;

use axum::http::StatusCode;
use serde_json::json;
use tucano_time::api::AppState;
use tucano_time::auth::Session;
use tucano_time::clock::SystemClock;
use tucano_time::store::Store;

#[tokio::test]
async fn timer_start_stop_creates_entry() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    // Start.
    let (s, timer) = json_req(
        &app,
        "POST",
        "/api/timer",
        Some(json!({"customer_id":cid,"project_code":"P1","note":"focus"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{timer}");
    // Second start while running -> 409.
    assert_eq!(
        json_req(
            &app,
            "POST",
            "/api/timer",
            Some(json!({"customer_id":cid,"project_code":"P1"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // Get shows it running.
    let (sg, cur) = json_req(&app, "GET", "/api/timer", None).await;
    assert_eq!(sg, StatusCode::OK);
    assert_eq!(cur["timer"]["project_code"], "P1");
    // Stop -> creates a timer-sourced entry.
    let (ss, entry) = json_req(&app, "POST", "/api/timer/stop", None).await;
    assert_eq!(ss, StatusCode::OK, "{entry}");
    assert_eq!(entry["source"], "timer");
    assert_eq!(entry["project_code"], "P1");
    // Timer cleared.
    let (_, none) = json_req(&app, "GET", "/api/timer", None).await;
    assert!(none.is_null());
}

#[tokio::test]
async fn calendar_events_from_configured_feed() {
    use tucano_time::vault::SecretVault;
    let dir = tempfile::tempdir().unwrap();
    let ics = dir.path().join("cal.ics");
    std::fs::write(&ics, "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:e1\r\nDTSTART:20261002T090000Z\r\nDTEND:20261002T100000Z\r\nSUMMARY:Stand-up\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").unwrap();

    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let vault = std::sync::Arc::new(
        SecretVault::open(&root, Some("unit-test-vault-key-000000000000"))
            .unwrap()
            .unwrap(),
    );
    vault
        .put("calendar.ics_url", ics.to_str().unwrap())
        .unwrap();
    let session = std::sync::Arc::new(Session::new(
        b"test-session-secret-0000000000000032".to_vec(),
        3600,
        false,
    ));
    let state = AppState {
        store: std::sync::Arc::new(store),
        clock: std::sync::Arc::new(SystemClock),
        locks: std::sync::Arc::new(tucano_time::lock::NoLocks),
        session,
        rate: std::sync::Arc::new(tucano_time::ratelimit::RateLimiter::new(
            8,
            std::time::Duration::from_secs(300),
        )),
        audit: std::sync::Arc::new(tucano_time::audit::AuditLog::new(&root)),
        revocations: std::sync::Arc::new(tucano_time::revoke::Revocations::new(&root)),
        vault: Some(vault),
        email: std::sync::Arc::new(tucano_time::email::DisabledEmailSender),
        payments: std::sync::Arc::new(tucano_time::payments::PaymentRegistry::default()),
        accounting: std::sync::Arc::new(tucano_time::accounting::AccountingRegistry::default()),
        sso: std::sync::Arc::new(tucano_time::sso::SsoRegistry::default()),
        oauth_flows: std::sync::Arc::new(tucano_time::calendar_oauth::OAuthFlows::new()),
        cfg: std::sync::Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
            tucano_time::appconfig::AppConfig::empty(),
        ))),
    };
    let router = tucano_time::build_router(state);
    raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    let admin = login_cookie(&router, ADMIN_EMAIL, ADMIN_PW).await;

    let (s, body, _) = raw(
        &router,
        "GET",
        "/api/calendar/events?from=2026-10-01&to=2026-10-07",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["title"], "Stand-up");
}

#[tokio::test]
async fn calendar_oauth_start_and_callback_plumbing() {
    let (app, _d) = app().await;
    // No client id configured (no vault in this test app) -> 422.
    let (s1, b1) = json_req(
        &app,
        "GET",
        "/api/calendar/oauth/start?provider=google",
        None,
    )
    .await;
    assert_eq!(s1, StatusCode::UNPROCESSABLE_ENTITY, "{b1}");

    // Unknown provider -> 400.
    let (s2, _) = json_req(
        &app,
        "GET",
        "/api/calendar/oauth/start?provider=yahoo",
        None,
    )
    .await;
    assert_eq!(s2, StatusCode::BAD_REQUEST);

    // Callback with a state we never issued -> 401 (CSRF state is the defence).
    let (s3, b3) = json_req(
        &app,
        "GET",
        "/api/calendar/oauth/callback?code=abc&state=forged",
        None,
    )
    .await;
    assert_eq!(s3, StatusCode::UNAUTHORIZED, "{b3}");

    // Calendar events endpoint still reports ICS-not-configured (no provider).
    let (s4, _) = json_req(
        &app,
        "GET",
        "/api/calendar/events?from=2026-10-01&to=2026-10-07",
        None,
    )
    .await;
    assert_eq!(s4, StatusCode::NOT_FOUND);
}

/// #262 Tier 2: timer errors — an unknown project code is a 422 field error,
/// and stopping with no running timer is a 404.
#[tokio::test]
async fn timer_invalid_project_is_422_and_stop_without_timer_is_404() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();

    let (s_bad, body) = json_req(
        &app,
        "POST",
        "/api/timer",
        Some(json!({"customer_id":cid,"project_code":"NOPE"})),
    )
    .await;
    assert_eq!(s_bad, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");

    let (s_stop, _) = json_req(&app, "POST", "/api/timer/stop", None).await;
    assert_eq!(s_stop, StatusCode::NOT_FOUND);
}
