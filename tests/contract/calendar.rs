use crate::support::*;

use axum::http::StatusCode;
use serde_json::json;
use tatu_time::api::AppState;
use tatu_time::auth::Session;
use tatu_time::clock::SystemClock;
use tatu_time::store::Store;

#[tokio::test]
async fn timer_start_stop_creates_entry() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
    use tatu_time::vault::SecretVault;
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
    let boot = tatu_time::vault::VaultBoot::from_vault(
        vault,
        tatu_time::vault::KeySource::Env,
        None,
        true,
    );
    let state = AppState {
        store: std::sync::Arc::new(store),
        clock: std::sync::Arc::new(SystemClock),
        locks: std::sync::Arc::new(tatu_time::lock::NoLocks),
        session,
        rate: std::sync::Arc::new(tatu_time::ratelimit::RateLimiter::new(
            8,
            std::time::Duration::from_secs(300),
        )),
        audit: std::sync::Arc::new(tatu_time::audit::AuditLog::new(&root)),
        revocations: std::sync::Arc::new(tatu_time::revoke::Revocations::new(&root)),
        vault: tatu_time::vault::VaultSlot::new(boot),
        email: std::sync::Arc::new(tatu_time::email::DisabledEmailSender),
        accounting: std::sync::Arc::new(tatu_time::accounting::AccountingRegistry::default()),
        sso: std::sync::Arc::new(tatu_time::sso::SsoRegistry::default()),
        oauth_flows: std::sync::Arc::new(tatu_time::calendar_oauth::OAuthFlows::new()),
        cfg: std::sync::Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
            tatu_time::appconfig::AppConfig::empty(),
        ))),
        env: std::sync::Arc::new(tatu_time::appconfig::process_env),
    };
    let router = tatu_time::build_router(state);
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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

/// QA-03: four parallel `POST /api/timer` calls produce exactly one 201 and
/// three 409 conflicts, and exactly one timer document lands on disk. The
/// unlocked pre-check is only the friendly path; `put_timer_if_none` is the
/// guard, re-checking existence inside the writer lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_timer_starts_yield_exactly_one_winner() {
    let (app, d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;

    let body = json!({"customer_id": cid, "project_code": "P1"});
    let (a, b, cc, dd) = tokio::join!(
        raw(
            &app.router,
            "POST",
            "/api/timer",
            Some(body.clone()),
            Some(&app.cookie)
        ),
        raw(
            &app.router,
            "POST",
            "/api/timer",
            Some(body.clone()),
            Some(&app.cookie)
        ),
        raw(
            &app.router,
            "POST",
            "/api/timer",
            Some(body.clone()),
            Some(&app.cookie)
        ),
        raw(
            &app.router,
            "POST",
            "/api/timer",
            Some(body.clone()),
            Some(&app.cookie)
        ),
    );
    let responses = [a, b, cc, dd];
    let statuses: Vec<StatusCode> = responses.iter().map(|(s, _, _)| *s).collect();
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CREATED)
            .count(),
        1,
        "exactly one start wins: {statuses:?}"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        3,
        "every loser is told the timer is already running: {statuses:?}"
    );
    for (status, body, _) in &responses {
        if *status == StatusCode::CONFLICT {
            assert_eq!(body["error"]["code"], "conflict", "{body}");
            assert!(
                body["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("already running"),
                "{body}"
            );
        }
    }

    // Exactly one timer document on disk, and it is the one the 201 reported.
    let timers = d.path().join("data/timers");
    let docs: Vec<std::path::PathBuf> = std::fs::read_dir(&timers)
        .expect("timers directory")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    assert_eq!(docs.len(), 1, "exactly one timer on disk: {docs:?}");
    let (_, running) = json_req(&app, "GET", "/api/timer", None).await;
    assert_eq!(running["timer"]["customer_id"], cid);
}
