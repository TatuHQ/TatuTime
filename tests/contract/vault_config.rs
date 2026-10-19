use crate::support::*;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
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
    let boot = tucano_time::vault::VaultBoot::from_vault(
        vault,
        tucano_time::vault::KeySource::Env,
        None,
        true,
    );
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
        vault: tucano_time::vault::VaultSlot::new(boot),
        email: std::sync::Arc::new(tucano_time::email::DisabledEmailSender),
        accounting: std::sync::Arc::new(tucano_time::accounting::AccountingRegistry::default()),
        sso: std::sync::Arc::new(tucano_time::sso::SsoRegistry::default()),
        oauth_flows: std::sync::Arc::new(tucano_time::calendar_oauth::OAuthFlows::new()),
        cfg: std::sync::Arc::new(std::sync::Mutex::new(std::sync::Arc::new(
            tucano_time::appconfig::AppConfig::empty(),
        ))),
        env: std::sync::Arc::new(tucano_time::appconfig::process_env),
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
        "/api/admin/secrets/tucano.api_token",
        Some(json!({"value":"dummy-secret-value-1234"})),
        Some(&admin),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // list shows masked hint, never the value
    let (s2, list, _) = raw(&router, "GET", "/api/admin/secrets", None, Some(&admin)).await;
    assert_eq!(s2, StatusCode::OK);
    let text = serde_json::to_string(&list).unwrap();
    assert!(text.contains("tucano.api_token"), "{text}");
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
        "/api/admin/secrets/tucano.api_token",
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

/// SET-13: `Str`/`Csv` values are trimmed on write and a whitespace-only
/// `org_name` is a 422 naming the field (documents must never render an empty
/// issuer). `base_currency: null` still clears the key.
#[tokio::test]
async fn config_values_are_trimmed_and_blank_org_name_is_refused() {
    let (app, d) = app().await;
    let (s_blank, body_blank) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"org_name": "   "})),
    )
    .await;
    assert_eq!(s_blank, StatusCode::UNPROCESSABLE_ENTITY, "{body_blank}");
    assert_eq!(body_blank["error"]["code"], "validation_failed");
    assert_eq!(body_blank["error"]["fields"][0]["field"], "org_name");
    assert!(
        body_blank["error"]["fields"][0]["message"]
            .as_str()
            .unwrap()
            .contains("org_name"),
        "{body_blank}"
    );
    let cfg_path = d.path().join("data").join("config.json");
    assert!(
        !cfg_path.exists()
            || !std::fs::read_to_string(&cfg_path)
                .unwrap()
                .contains("org_name"),
        "a refused blank org_name persists nothing"
    );

    // Padded values are stored trimmed, for both `Str` and `Csv`.
    let (s_trim, body_trim) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({
            "org_name": "  Tucano Ltd  ",
            "sso_allowed_domains": " tucanotime.test , other.test "
        })),
    )
    .await;
    assert_eq!(s_trim, StatusCode::OK, "{body_trim}");
    let persisted = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(persisted.contains("\"Tucano Ltd\""), "{persisted}");
    assert!(!persisted.contains("  Tucano"), "{persisted}");
    let (_s_eff, eff) = json_req(&app, "GET", "/api/admin/config", None).await;
    let org = eff["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "org_name")
        .unwrap();
    assert_eq!(org["value"], "Tucano Ltd");
    let domains = eff["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "sso_allowed_domains")
        .unwrap();
    assert_eq!(domains["value"], "tucanotime.test , other.test");

    // `null` still clears an optional currency key.
    let (s_set, _) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"base_currency": "EUR"})),
    )
    .await;
    assert_eq!(s_set, StatusCode::OK);
    let (s_clear, clear_body) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"base_currency": null})),
    )
    .await;
    assert_eq!(s_clear, StatusCode::OK, "{clear_body}");
    let (_s_after, after) = json_req(&app, "GET", "/api/admin/config", None).await;
    let currency = after["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "base_currency")
        .unwrap();
    assert_eq!(currency["value"], serde_json::Value::Null);
}

/// Subtask D: the `base_currency` display preference lives in the appconfig
/// whitelist (config.json, not the vault). It is ISO 4217 shaped (exactly three
/// ASCII letters), normalised to canonical uppercase on write, unset by
/// default, and never masked like a secret.
#[tokio::test]
async fn base_currency_validation_normalisation_and_unset() {
    let (app, d) = app().await;

    // Absent by default: the key is present in the effective table but unset
    // (null / default source), so no base currency is configured and nothing
    // is fabricated for the Trends conversion.
    let (_s, eff) = json_req(&app, "GET", "/api/admin/config", None).await;
    let bc = eff["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "base_currency")
        .unwrap();
    assert_eq!(bc["value"], serde_json::Value::Null, "unset by default");
    assert_eq!(bc["source"], "default");

    // A valid code round-trips and is returned in plain text (never masked as
    // a secret: the whole value is visible, not a 2-char hint).
    let (s, body) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"base_currency": "EUR"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let cfg_file = d.path().join("data").join("config.json");
    let persisted = std::fs::read_to_string(&cfg_file).unwrap();
    assert!(persisted.contains("base_currency"), "{persisted}");
    assert!(persisted.contains("EUR"), "{persisted}");
    let (_s2, eff2) = json_req(&app, "GET", "/api/admin/config", None).await;
    let bc2 = eff2["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "base_currency")
        .unwrap();
    assert_eq!(bc2["value"], "EUR");
    assert_eq!(bc2["source"], "file");

    // Lowercase is normalised to canonical uppercase, not rejected: `eur` and
    // `EUR` cannot both exist in the store.
    let (s3, _) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"base_currency": "gbp"})),
    )
    .await;
    assert_eq!(s3, StatusCode::OK);
    let (_s4, eff3) = json_req(&app, "GET", "/api/admin/config", None).await;
    let bc3 = eff3["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "base_currency")
        .unwrap();
    assert_eq!(bc3["value"], "GBP", "lowercase normalised to uppercase");

    // Invalid shapes are 422 and persist nothing: the store still holds GBP.
    for bad in ["US", "USDD", "12A", ""] {
        let (s, body) = json_req(
            &app,
            "PUT",
            "/api/admin/config",
            Some(json!({ "base_currency": bad })),
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad:?}: {body}");
    }
    let (_s5, eff4) = json_req(&app, "GET", "/api/admin/config", None).await;
    let bc4 = eff4["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "base_currency")
        .unwrap();
    assert_eq!(bc4["value"], "GBP", "rejected writes persist nothing");

    // An unknown field in the same payload is still refused, and the valid
    // sibling key in that payload is not persisted either.
    let (s6, body6) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"base_currency": "USD", "not_a_knob": 1})),
    )
    .await;
    assert_eq!(s6, StatusCode::UNPROCESSABLE_ENTITY, "{body6}");
    let (_s7, eff5) = json_req(&app, "GET", "/api/admin/config", None).await;
    let bc5 = eff5["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "base_currency")
        .unwrap();
    assert_eq!(
        bc5["value"], "GBP",
        "unknown-field payload persists nothing"
    );
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
    let boot = tucano_time::vault::VaultBoot::from_vault(
        vault,
        tucano_time::vault::KeySource::Env,
        None,
        true,
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
        vault: tucano_time::vault::VaultSlot::new(boot),
        email: Arc::new(tucano_time::email::DisabledEmailSender),
        accounting: Arc::new(tucano_time::accounting::AccountingRegistry::default()),
        sso: Arc::new(tucano_time::sso::SsoRegistry::default()),
        oauth_flows: Arc::new(tucano_time::calendar_oauth::OAuthFlows::new()),
        cfg: Arc::new(std::sync::Mutex::new(Arc::new(
            tucano_time::appconfig::AppConfig::empty(),
        ))),
        env: Arc::new(tucano_time::appconfig::process_env),
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

// ------------------------------------------------ credential-vault lifecycle --

const FIXED_KEY: &str = "0123456789abcdef0123456789abcdef";

/// Neither the master key nor a host path may appear in any response.
fn assert_clean(body: &serde_json::Value, root: &std::path::Path, key: &str) {
    let text = body.to_string();
    if !key.is_empty() {
        assert!(!text.contains(key), "master key leaked: {text}");
    }
    assert!(
        !text.contains(root.to_str().unwrap()),
        "host path leaked: {text}"
    );
    assert!(
        !text.contains("secrets.bin"),
        "store file name leaked: {text}"
    );
}

#[tokio::test]
async fn vault_status_enabled_reports_key_file_without_leaking() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let boot = vault_boot_enabled(&root);
    let key = std::fs::read_to_string(root.join("vault.key")).unwrap();
    let client = app_with_vault(store, boot, tucano_time::appconfig::AppConfig::empty()).await;

    let (status, body) = json_req(&client, "GET", "/api/admin/vault", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["source"], "key_file");
    assert_eq!(body["disabled_reason"], serde_json::Value::Null);
    assert!(body["fingerprint"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(body["key_file_name"], "vault.key");
    assert_eq!(body["key_file_present"], true);
    assert_eq!(body["secrets_count"], 0);
    assert_eq!(body["external_override"], false);
    assert_eq!(body["can_enable"], false);
    assert_eq!(body["can_disable"], true);
    assert_eq!(body["can_regenerate"], true);
    assert_clean(&body, &root, &key);

    // A member is refused (admin tier).
    json_req(
        &client,
        "POST",
        "/api/users",
        Some(json!({"name":"M","email":"vault-member@t.local","password":"memberpass1","role":"member"})),
    )
    .await;
    let member = login_cookie(&client.router, "vault-member@t.local", "memberpass1").await;
    let (s, b, _) = raw(
        &client.router,
        "GET",
        "/api/admin/vault",
        None,
        Some(&member),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
}

#[tokio::test]
async fn vault_status_disabled_is_200_and_offers_enable() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let client = app_with_vault(
        store,
        vault_boot_disabled(),
        tucano_time::appconfig::AppConfig::empty(),
    )
    .await;

    let (status, body) = json_req(&client, "GET", "/api/admin/vault", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["source"], "none");
    assert_eq!(body["disabled_reason"], "config");
    assert_eq!(body["can_enable"], true);
    assert_eq!(body["can_disable"], false);
    assert_eq!(body["key_file_present"], false);
    assert_eq!(body["key_file_name"], serde_json::Value::Null);
    assert_clean(&body, dir.path(), "");
}

#[tokio::test]
async fn vault_enable_mints_a_key_then_refuses_a_second_call() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let client = app_with_vault(
        store,
        vault_boot_disabled(),
        tucano_time::appconfig::AppConfig::empty(),
    )
    .await;

    let (status, body) = json_req(&client, "POST", "/api/admin/vault/enable", None).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["created_key"], true);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["source"], "key_file");
    let key_path = root.join("vault.key");
    let bytes = std::fs::read(&key_path).unwrap();
    assert_eq!(bytes.len(), 32);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_clean(&body, &root, std::str::from_utf8(&bytes).unwrap());

    let (second, b2) = json_req(&client, "POST", "/api/admin/vault/enable", None).await;
    assert_eq!(second, StatusCode::CONFLICT, "{b2}");
    assert_eq!(b2["error"]["code"], "vault_already_enabled");
}

#[tokio::test]
async fn vault_enable_reuses_an_existing_valid_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let key_path = root.join("vault.key");
    std::fs::write(&key_path, FIXED_KEY.as_bytes()).unwrap();
    let before = std::fs::metadata(&key_path).unwrap();
    let before_mtime = before.modified().unwrap();

    let client = app_with_vault(
        store,
        vault_boot_disabled(),
        tucano_time::appconfig::AppConfig::empty(),
    )
    .await;
    let (status, body) = json_req(&client, "POST", "/api/admin/vault/enable", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created_key"], false);
    assert_eq!(body["enabled"], true);
    assert_eq!(std::fs::read(&key_path).unwrap(), FIXED_KEY.as_bytes());
    assert_eq!(
        std::fs::metadata(&key_path).unwrap().modified().unwrap(),
        before_mtime,
        "a reused key file must not be rewritten"
    );
    assert_clean(&body, &root, FIXED_KEY);
}

#[tokio::test]
async fn vault_disable_persists_the_flag_and_deletes_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let boot = vault_boot_enabled(&root);
    let key_path = root.join("vault.key");
    let key = std::fs::read_to_string(&key_path).unwrap();
    let client = app_with_vault(store, boot, tucano_time::appconfig::AppConfig::empty()).await;

    let (status, body) = json_req(
        &client,
        "POST",
        "/api/admin/vault/disable",
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["disabled_reason"], "config");
    assert_eq!(body["key_file_present"], false);
    assert_eq!(body["can_enable"], true);
    assert!(!key_path.exists(), "the generated key file must be deleted");
    assert_clean(&body, &root, &key);

    let persisted = std::fs::read_to_string(root.join("config.json")).unwrap();
    assert!(persisted.contains("vault_enabled"), "{persisted}");
    assert!(persisted.contains("false"), "{persisted}");

    let (_, now) = json_req(&client, "GET", "/api/admin/vault", None).await;
    assert_eq!(now["enabled"], false);
}

#[tokio::test]
async fn vault_disable_and_regenerate_refuse_while_a_secret_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let boot = vault_boot_enabled(&root);
    boot.vault
        .as_ref()
        .unwrap()
        .put("smtp.host", "relay.local")
        .unwrap();
    let key_path = root.join("vault.key");
    let store_path = root.join("secrets.bin");
    let key_bytes = std::fs::read(&key_path).unwrap();
    let secret_bytes = std::fs::read(&store_path).unwrap();
    let client = app_with_vault(store, boot, tucano_time::appconfig::AppConfig::empty()).await;

    let (s1, b1) = json_req(
        &client,
        "POST",
        "/api/admin/vault/disable",
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(s1, StatusCode::CONFLICT, "{b1}");
    assert_eq!(b1["error"]["code"], "vault_in_use");

    let (s2, b2) = json_req(
        &client,
        "POST",
        "/api/admin/vault/regenerate",
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT, "{b2}");
    assert_eq!(b2["error"]["code"], "vault_in_use");

    assert_eq!(std::fs::read(&key_path).unwrap(), key_bytes);
    assert_eq!(std::fs::read(&store_path).unwrap(), secret_bytes);
}

#[tokio::test]
async fn vault_regenerate_changes_the_fingerprint_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let boot = vault_boot_enabled(&root);
    let client = app_with_vault(store, boot, tucano_time::appconfig::AppConfig::empty()).await;

    let (_, before) = json_req(&client, "GET", "/api/admin/vault", None).await;
    let (status, body) = json_req(
        &client,
        "POST",
        "/api/admin/vault/regenerate",
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created_key"], true);
    assert_ne!(body["fingerprint"], before["fingerprint"]);
    assert_eq!(std::fs::read(root.join("vault.key")).unwrap().len(), 32);
}

#[tokio::test]
async fn vault_disable_and_regenerate_refuse_an_external_override() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let vault = Arc::new(
        tucano_time::vault::SecretVault::open_with(
            &root,
            FIXED_KEY,
            tucano_time::vault::KeySource::Env,
        )
        .unwrap(),
    );
    let boot = tucano_time::vault::VaultBoot::from_vault(
        vault,
        tucano_time::vault::KeySource::Env,
        None,
        true,
    );
    let client = app_with_vault(store, boot, tucano_time::appconfig::AppConfig::empty()).await;

    let (s1, b1) = json_req(
        &client,
        "POST",
        "/api/admin/vault/disable",
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(s1, StatusCode::CONFLICT, "{b1}");
    assert_eq!(b1["error"]["code"], "vault_managed_externally");

    let (s2, b2) = json_req(
        &client,
        "POST",
        "/api/admin/vault/regenerate",
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT, "{b2}");
    assert_eq!(b2["error"]["code"], "vault_managed_externally");
}

#[tokio::test]
async fn vault_lifecycle_rejects_invalid_confirmation_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let client = app_with_vault(
        store,
        vault_boot_enabled(&root),
        tucano_time::appconfig::AppConfig::empty(),
    )
    .await;

    let cases: Vec<Option<serde_json::Value>> = vec![
        None,
        Some(json!({})),
        Some(json!({"confirm": false})),
        Some(json!({"confirm": true, "x": 1})),
    ];
    for uri in ["/api/admin/vault/disable", "/api/admin/vault/regenerate"] {
        for body in &cases {
            let (status, response) = json_req(&client, "POST", uri, body.clone()).await;
            assert!(
                status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
                "{uri} {body:?} -> {status}: {response}"
            );
            assert_eq!(
                response["error"]["code"], "validation_failed",
                "{uri} {body:?}: {response}"
            );
        }
        // Malformed JSON: the same shared error shape, never a panic or a write.
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::COOKIE, &client.cookie)
            .header("x-csrf-protection", "1")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap();
        let res = client.router.clone().oneshot(req).await.unwrap();
        assert!(
            res.status() == StatusCode::UNPROCESSABLE_ENTITY
                || res.status() == StatusCode::BAD_REQUEST,
            "{uri}: malformed JSON must be a client error"
        );
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            response["error"]["code"], "validation_failed",
            "{uri}: {response}"
        );
    }

    // Nothing above changed the state.
    let (_, status) = json_req(&client, "GET", "/api/admin/vault", None).await;
    assert_eq!(status["enabled"], true);
    assert_eq!(status["secrets_count"], 0);
}

/// M1: a valid `TUCANO_VAULT_ENABLED=false` is an operator override. The admin
/// API must refuse to enable the vault instead of flipping the flag behind the
/// environment's back (which the next boot would simply reverse).
#[tokio::test]
async fn vault_enable_refuses_when_the_environment_disables_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let env: tucano_time::api::EnvLookup = Arc::new(|key: &str| {
        if key == "TUCANO_VAULT_ENABLED" {
            Some("false".to_string())
        } else {
            None
        }
    });
    let client = app_with_vault_env(
        store,
        vault_boot_disabled(),
        tucano_time::appconfig::AppConfig::empty(),
        env,
    )
    .await;

    let (status, body) = json_req(&client, "POST", "/api/admin/vault/enable", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "vault_managed_externally");
    assert!(
        !root.join("vault.key").exists(),
        "a refused enable must not mint a key"
    );
    let (_, now) = json_req(&client, "GET", "/api/admin/vault", None).await;
    assert_eq!(now["enabled"], false);
    assert_eq!(now["can_enable"], false);
}

/// M1, other direction: an env value that enables the vault makes the admin
/// `disable` a 409 rather than a flag the next boot would ignore.
#[tokio::test]
async fn vault_disable_refuses_when_the_environment_enables_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let boot = vault_boot_enabled(&root);
    let key_path = root.join("vault.key");
    let env: tucano_time::api::EnvLookup = Arc::new(|key: &str| {
        if key == "TUCANO_VAULT_ENABLED" {
            Some("true".to_string())
        } else {
            None
        }
    });
    let client =
        app_with_vault_env(store, boot, tucano_time::appconfig::AppConfig::empty(), env).await;

    let (status, body) = json_req(
        &client,
        "POST",
        "/api/admin/vault/disable",
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "vault_managed_externally");
    assert!(key_path.exists(), "a refused disable keeps the key");
    let (_, now) = json_req(&client, "GET", "/api/admin/vault", None).await;
    assert_eq!(now["enabled"], true);
    assert_eq!(now["can_disable"], false);
}

/// M3: `PUT /api/admin/config` must not write `vault_enabled` (a stored
/// `false` next to a `secrets.bin` hard-fails the next boot). The knob stays
/// readable and is flagged read-only.
#[tokio::test]
async fn config_put_cannot_write_the_vault_lifecycle_flag() {
    let (app, d) = app().await;
    let (_, before) = json_req(&app, "GET", "/api/admin/vault", None).await;
    let cfg_path = d.path().join("data").join("config.json");

    for flag in [false, true] {
        let (status, body) = json_req(
            &app,
            "PUT",
            "/api/admin/config",
            Some(json!({"vault_enabled": flag})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["code"], "validation_failed");
        assert_eq!(body["error"]["fields"][0]["field"], "vault_enabled");
        assert!(
            body["error"]["fields"][0]["message"]
                .as_str()
                .unwrap()
                .contains("/api/admin/vault"),
            "the refusal must point at the lifecycle endpoints: {body}"
        );
    }
    assert!(
        !cfg_path.exists(),
        "a refused config write must persist nothing"
    );
    let (_, after) = json_req(&app, "GET", "/api/admin/vault", None).await;
    assert_eq!(after, before, "the vault state is unchanged");

    // GET still lists the knob, flagged read-only for the GUI table.
    let (_, eff) = json_req(&app, "GET", "/api/admin/config", None).await;
    let row = eff["config"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "vault_enabled")
        .expect("vault_enabled stays visible");
    assert_eq!(row["writable"], false);
    assert_eq!(row["value"], before["enabled"]);
}

/// H2: when the flag cannot be persisted, `disable` must leave the live slot
/// and the key file untouched (500 `data_dir_not_writable`, still enabled).
#[tokio::test]
async fn vault_disable_persistence_failure_keeps_the_key_and_enabled_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = store.root().to_path_buf();
    let boot = vault_boot_enabled(&root);
    let key_path = root.join("vault.key");
    let client = app_with_vault(store, boot, tucano_time::appconfig::AppConfig::empty()).await;

    // A directory at the config write's temp path makes persistence fail
    // deterministically, regardless of the uid running the tests.
    std::fs::create_dir(root.join("config.json.tmp")).unwrap();

    let (status, body) = json_req(
        &client,
        "POST",
        "/api/admin/vault/disable",
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"]["code"], "data_dir_not_writable");
    assert!(
        key_path.exists(),
        "the key must not be deleted before the disabled flag is persisted"
    );
    assert!(!root.join("config.json").exists());
    let (_, now) = json_req(&client, "GET", "/api/admin/vault", None).await;
    assert_eq!(now["enabled"], true, "the live slot stays enabled");
}
