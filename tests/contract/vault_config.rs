use crate::support::*;

use axum::Router;
use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;
use tucano_time::api::AppState;
use tucano_time::auth::Session;
use tucano_time::clock::SystemClock;
use tucano_time::store::Store;

#[tokio::test]
async fn secret_vault_endpoints_mask_and_authorize() {
    use tucano_time::vault::SecretVault;
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let vault = std::sync::Arc::new(
        SecretVault::open(&root, Some("0123456789abcdef0123456789abcdef"))
            .unwrap()
            .unwrap(),
    );
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
        accounting: std::sync::Arc::new(tucano_time::accounting::AccountingRegistry::default()),
        sso: std::sync::Arc::new(tucano_time::sso::SsoRegistry::default()),
        oauth_flows: std::sync::Arc::new(tucano_time::calendar_oauth::OAuthFlows::new()),
        cfg: std::sync::Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
            tucano_time::appconfig::AppConfig::empty(),
        ))),
    };
    let router = tucano_time::build_router(state);
    // bootstrap admin
    raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    let admin = login_cookie(&router, ADMIN_EMAIL, ADMIN_PW).await;
    // member
    raw(
        &router,
        "POST",
        "/api/users",
        Some(json!({"name":"M","email":"m@t.local","password":"memberpass1","role":"member"})),
        Some(&admin),
    )
    .await;
    let member = login_cookie(&router, "m@t.local", "memberpass1").await;

    // admin sets a secret
    let (s, _, _) = raw(
        &router,
        "PUT",
        "/api/admin/secrets/acme.api_token",
        Some(json!({"value":"dummy-secret-value-1234"})),
        Some(&admin),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // list shows masked hint, never the value
    let (s2, list, _) = raw(&router, "GET", "/api/admin/secrets", None, Some(&admin)).await;
    assert_eq!(s2, StatusCode::OK);
    let text = serde_json::to_string(&list).unwrap();
    assert!(text.contains("acme.api_token"), "{text}");
    assert!(
        !text.contains("dummy-secret-value-1234"),
        "value leaked in list: {text}"
    );
    assert!(text.contains("34"), "expected masked hint: {text}");
    assert!(
        !text.contains("1234"),
        "hint must show at most the final 2 chars (review A8)"
    );
    // member forbidden
    let (s3, _, _) = raw(
        &router,
        "PUT",
        "/api/admin/secrets/x",
        Some(json!({"value":"y"})),
        Some(&member),
    )
    .await;
    assert_eq!(s3, StatusCode::FORBIDDEN);
    // delete
    let (s4, _, _) = raw(
        &router,
        "DELETE",
        "/api/admin/secrets/acme.api_token",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(s4, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn admin_config_roundtrip_precedence_and_guards() {
    let (app, d) = app().await;
    // Effective view: everything starts at its default source.
    let (s1, body) = json_req(&app, "GET", "/api/admin/config", None).await;
    assert_eq!(s1, StatusCode::OK, "{body}");
    // #190: no on-disk path is serialised (AGENTS.md: responses never expose
    // paths). The old `path` key leaked `<data>/config.json`.
    assert_eq!(
        body.get("path"),
        None,
        "GET /admin/config must not expose the filesystem path: {body}"
    );
    let serialized = body.to_string();
    assert!(
        !serialized.contains("config.json") && !serialized.contains(d.path().to_str().unwrap()),
        "no filesystem paths in the body: {serialized}"
    );
    let days = body["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "reminder_days")
        .unwrap();
    assert_eq!(days["value"], 7);
    assert_eq!(days["source"], "default");
    assert!(days["description"].as_str().unwrap().contains("reminder"));

    // PUT persists to <data>/config.json and flips the source to "file".
    let (s3, body3) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"reminder_days": 15, "sso_admin_group": "tt-admins"})),
    )
    .await;
    assert_eq!(s3, StatusCode::OK);
    assert_eq!(body3["ok"], true);
    let cfg_file = d.path().join("data").join("config.json");
    let persisted = std::fs::read_to_string(&cfg_file).unwrap();
    assert!(persisted.contains("reminder_days"), "{persisted}");
    let (_s4, eff) = json_req(&app, "GET", "/api/admin/config", None).await;
    let days2 = eff["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "reminder_days")
        .unwrap();
    assert_eq!(days2["value"], 15);
    assert_eq!(days2["source"], "file");

    // Secret-shaped / unknown keys cannot be persisted through the API (#94 guard).
    let (s5, b5) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"smtp.password": "hunter2"})),
    )
    .await;
    assert_eq!(s5, StatusCode::UNPROCESSABLE_ENTITY, "{b5}");
    let (s6, _) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"max_docs": 2})),
    )
    .await;
    assert_eq!(s6, StatusCode::UNPROCESSABLE_ENTITY, "out of range refused");

    // Members cannot see or edit configuration (admin tier).
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"N","email":"cfgmember@t.local","password":"memberpass1","role":"member"})),
    )
    .await;
    let member = login_cookie(&app.router, "cfgmember@t.local", "memberpass1").await;
    let (sm, _, _) = raw(&app.router, "GET", "/api/admin/config", None, Some(&member)).await;
    assert_eq!(sm, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn config_file_changes_request_time_behaviour_without_restart() {
    // #94: request-time knobs (SSO admin group) read the live config.
    let (app, _d) = app().await;
    let (_s, eff) = json_req(&app, "GET", "/api/admin/config", None).await;
    let grp = eff["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "sso_admin_group")
        .unwrap();
    assert_eq!(grp["value"], "");
    json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"sso_admin_group": "finance-admins"})),
    )
    .await;
    let (_s2, eff2) = json_req(&app, "GET", "/api/admin/config", None).await;
    let grp2 = eff2["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "sso_admin_group")
        .unwrap();
    assert_eq!(grp2["value"], "finance-admins");
    assert_eq!(grp2["source"], "file");
}

#[tokio::test]
async fn demo_flags_cannot_be_persisted_via_config() {
    // Review A3: demo/bypass flags stay env-only; config.json rejects unknown keys.
    let (app, _d) = app().await;
    for key in ["demo_mode", "insecure_demo"] {
        let (s, body) = json_req(&app, "PUT", "/api/admin/config", Some(json!({key: true}))).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{key}: {body}");
    }
}

/// Build a router with a real (temp-dir) vault for the secret-endpoint tests.
async fn vault_router(dir: &std::path::Path) -> (Router, String) {
    use tucano_time::vault::SecretVault;
    let store = Store::open(dir.join("data")).unwrap();
    let root = store.root().to_path_buf();
    let vault = Arc::new(
        SecretVault::open(&root, Some("0123456789abcdef0123456789abcdef"))
            .unwrap()
            .unwrap(),
    );
    let state = AppState {
        store: Arc::new(store),
        clock: Arc::new(SystemClock),
        locks: Arc::new(tucano_time::lock::NoLocks),
        session: Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        )),
        rate: Arc::new(tucano_time::ratelimit::RateLimiter::new(
            8,
            std::time::Duration::from_secs(300),
        )),
        audit: Arc::new(tucano_time::audit::AuditLog::new(&root)),
        revocations: Arc::new(tucano_time::revoke::Revocations::new(&root)),
        vault: Some(vault),
        email: Arc::new(tucano_time::email::DisabledEmailSender),
        accounting: Arc::new(tucano_time::accounting::AccountingRegistry::default()),
        sso: Arc::new(tucano_time::sso::SsoRegistry::default()),
        oauth_flows: Arc::new(tucano_time::calendar_oauth::OAuthFlows::new()),
        cfg: Arc::new(std::sync::Mutex::new(Arc::new(
            tucano_time::appconfig::AppConfig::empty(),
        ))),
    };
    let router = tucano_time::build_router(state);
    let _ = raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    let admin = login_cookie(&router, ADMIN_EMAIL, ADMIN_PW).await;
    (router, admin)
}

/// #262 Tier 2: `PUT /admin/secrets/{key}` rejects an invalid key name with 422
/// (the value is never stored). A valid key still succeeds.
#[tokio::test]
async fn secret_put_invalid_key_name_is_422() {
    let dir = tempfile::tempdir().unwrap();
    let (router, admin) = vault_router(dir.path()).await;
    let long_key = "a".repeat(101); // > 100 chars -> invalid
    let (s, body, _) = raw(
        &router,
        "PUT",
        &format!("/api/admin/secrets/{long_key}"),
        Some(json!({"value":"x"})),
        Some(&admin),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    // The invalid key was not persisted.
    let (_, list, _) = raw(&router, "GET", "/api/admin/secrets", None, Some(&admin)).await;
    assert!(
        list["secrets"].as_array().unwrap().is_empty(),
        "an invalid key must not be stored"
    );
}
