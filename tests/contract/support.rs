use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

use tucano_time::api::AppState;
use tucano_time::auth::Session;
use tucano_time::clock::SystemClock;
use tucano_time::lock::{EntryLock, LockReason};
use tucano_time::store::Store;

pub(crate) const ADMIN_EMAIL: &str = "admin@test.local";
pub(crate) const ADMIN_PW: &str = "supersecret1";

/// An in-process client holding a router and an authenticated session cookie.
pub(crate) struct Client {
    pub(crate) router: Router,
    pub(crate) cookie: String,
    /// The recording email transport installed on the router (#35).
    pub(crate) email: Arc<tucano_time::email::RecordingEmailSender>,
}

pub(crate) async fn raw(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value, Option<String>) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    // The CSRF guard requires this on mutations; send it on all test requests.
    b = b.header("x-csrf-protection", "1");
    let req = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            b.body(Body::from(v.to_string())).unwrap()
        }
        None => b.body(Body::empty()).unwrap(),
    };
    let res = router.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let set_cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").to_string());
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, value, set_cookie)
}

pub(crate) async fn login_cookie(router: &Router, email: &str, pw: &str) -> String {
    let (_s, _b, c) = raw(
        router,
        "POST",
        "/api/auth/login",
        Some(json!({"email": email, "password": pw})),
        None,
    )
    .await;
    c.unwrap_or_default()
}

impl Client {
    pub(crate) async fn new(store: Store, locks: Option<Arc<dyn EntryLock>>) -> Self {
        Self::build(
            store,
            locks,
            tucano_time::accounting::AccountingRegistry::default(),
        )
        .await
    }

    /// Client with accounting providers injected (#33 tests).
    pub(crate) async fn with_accounting(
        store: Store,
        accounting: tucano_time::accounting::AccountingRegistry,
    ) -> Self {
        Self::build(store, None, accounting).await
    }

    pub(crate) async fn build(
        store: Store,
        locks: Option<Arc<dyn EntryLock>>,
        accounting: tucano_time::accounting::AccountingRegistry,
    ) -> Self {
        let session = Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        ));
        let recorder = Arc::new(tucano_time::email::RecordingEmailSender::default());
        let accounting = Arc::new(accounting);
        let state = match locks {
            Some(l) => {
                let store = Arc::new(store);
                let audit = Arc::new(tucano_time::audit::AuditLog::new(store.root()));
                let revocations = Arc::new(tucano_time::revoke::Revocations::new(store.root()));
                AppState {
                    store,
                    clock: Arc::new(SystemClock),
                    locks: l,
                    session,
                    rate: Arc::new(tucano_time::ratelimit::RateLimiter::new(
                        8,
                        std::time::Duration::from_secs(300),
                    )),
                    audit,
                    revocations,
                    vault: tucano_time::vault::VaultSlot::disabled(
                        tucano_time::vault::DisabledReason::Config,
                    ),
                    email: recorder.clone(),
                    accounting,
                    sso: Arc::new(tucano_time::sso::SsoRegistry::default()),
                    oauth_flows: Arc::new(tucano_time::calendar_oauth::OAuthFlows::new()),
                    cfg: Arc::new(std::sync::Mutex::new(Arc::new(
                        tucano_time::appconfig::AppConfig::empty(),
                    ))),
                    env: Arc::new(tucano_time::appconfig::process_env),
                }
            }
            None => AppState::with_session(store, session)
                .with_email(recorder.clone())
                .with_accounting(accounting),
        };
        let router = tucano_time::build_router(state);
        // Bootstrap the first admin (403 if users already exist — fine).
        let _ = raw(
            &router,
            "POST",
            "/api/auth/bootstrap",
            Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
            None,
        )
        .await;
        let cookie = login_cookie(&router, ADMIN_EMAIL, ADMIN_PW).await;
        Self {
            router,
            cookie,
            email: recorder,
        }
    }
}

/// A test lock provider that freezes a single entry id.
pub(crate) struct LockOne(pub(crate) Option<uuid::Uuid>);
impl EntryLock for LockOne {
    fn entry_lock(
        &self,
        entry_id: uuid::Uuid,
    ) -> Result<Option<LockReason>, tucano_time::lock::LockUnavailable> {
        if self.0 == Some(entry_id) {
            Ok(Some(LockReason::Invoiced { id: "INV-1".into() }))
        } else {
            Ok(None)
        }
    }
    fn locked_entries(
        &self,
    ) -> Result<std::collections::HashSet<uuid::Uuid>, tucano_time::lock::LockUnavailable> {
        Ok(self.0.into_iter().collect())
    }
}

pub(crate) async fn app() -> (Client, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("data")).expect("store");
    (Client::new(store, None).await, dir)
}

/// Wrap a **pre-built** `AppState` in a router plus an authenticated admin
/// session. Vault-enabled tests build their own state through
/// `AppState::boot(...)` and hand it here; `recorder` is the transport they
/// injected with `with_email` (or a detached one when they injected none).
/// A boot state whose key was generated under `root` (`key_file` source).
pub(crate) fn vault_boot_enabled(root: &std::path::Path) -> tucano_time::vault::VaultBoot {
    tucano_time::vault::open_for_boot(root, None, None, true).expect("vault boot")
}

/// A boot state disabled by configuration (no key, no store).
pub(crate) fn vault_boot_disabled() -> tucano_time::vault::VaultBoot {
    tucano_time::vault::VaultBoot::disabled(tucano_time::vault::DisabledReason::Config)
}

/// Build a client from an explicit store + vault boot + config (vault
/// lifecycle tests construct the slot directly and never mutate process env).
pub(crate) async fn app_with_vault(
    store: Store,
    boot: tucano_time::vault::VaultBoot,
    cfg: tucano_time::appconfig::AppConfig,
) -> Client {
    let session = Arc::new(Session::new(
        b"test-session-secret-0000000000000032".to_vec(),
        3600,
        false,
    ));
    let recorder = Arc::new(tucano_time::email::RecordingEmailSender::default());
    let state = AppState::boot(store, session, boot, Arc::new(cfg)).with_email(recorder.clone());
    client_for_state(state, recorder).await
}

/// Same, with an injected environment map. This is how the operator-managed
/// (`TUCANO_VAULT_ENABLED`) 409s are tested without mutating process env.
pub(crate) async fn app_with_vault_env(
    store: Store,
    boot: tucano_time::vault::VaultBoot,
    cfg: tucano_time::appconfig::AppConfig,
    env: tucano_time::api::EnvLookup,
) -> Client {
    let session = Arc::new(Session::new(
        b"test-session-secret-0000000000000032".to_vec(),
        3600,
        false,
    ));
    let recorder = Arc::new(tucano_time::email::RecordingEmailSender::default());
    let state = AppState::boot(store, session, boot, Arc::new(cfg))
        .with_email(recorder.clone())
        .with_env(env);
    client_for_state(state, recorder).await
}

pub(crate) async fn client_for_state(
    state: AppState,
    recorder: Arc<tucano_time::email::RecordingEmailSender>,
) -> Client {
    let router = tucano_time::build_router(state);
    let _ = raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    let cookie = login_cookie(&router, ADMIN_EMAIL, ADMIN_PW).await;
    Client {
        router,
        cookie,
        email: recorder,
    }
}

pub(crate) async fn app_locked(dir_path: &std::path::Path, locks: Arc<dyn EntryLock>) -> Client {
    let store = Store::open(dir_path.join("data")).expect("store");
    Client::new(store, Some(locks)).await
}

/// Authenticated request.
pub(crate) async fn json_req(
    client: &Client,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (s, v, _) = raw(&client.router, method, uri, body, Some(&client.cookie)).await;
    (s, v)
}

/// Authenticated request with an explicit `Content-Length` header (QA-06).
///
/// `json_req` sends no length, which is the **chunked** read path: the cap is
/// only discovered while the body is consumed. This helper declares the size,
/// which is the path `RequestBodyLimitLayer` refuses before routing.
pub(crate) async fn sized_json_req(
    client: &Client,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let payload = body.to_string();
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, &client.cookie)
        .header("x-csrf-protection", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_LENGTH, payload.len())
        .body(Body::from(payload))
        .unwrap();
    let res = client.router.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, value)
}

/// Authenticated request that keeps the **raw response bytes and headers** —
/// needed for the binary PDF download (#113), where the body is not JSON.
/// Authenticated raw-body request (import CSV/ZIP uploads). Returns the status
/// and the JSON body when the response is JSON.
pub(crate) async fn body_req(
    client: &Client,
    method: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, &client.cookie)
        .header("x-csrf-protection", "1")
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap();
    let res = client.router.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

pub(crate) async fn raw_req(
    client: &Client,
    method: &str,
    uri: &str,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, &client.cookie)
        .header("x-csrf-protection", "1")
        .body(Body::empty())
        .unwrap();
    let res = client.router.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

/// Authenticated JSON request that keeps the raw response bytes — used for
/// endpoints whose success body is a binary document (the stateless invoice PDF
/// preview) but whose failures are the app's JSON error shape.
pub(crate) async fn raw_json_req(
    client: &Client,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, &client.cookie)
        .header("x-csrf-protection", "1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = client.router.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

/// Seed a customer (with billing email), one project and an entry, then
/// generate + issue an invoice. Returns the issued invoice JSON and the
/// invoice id. Shared by the #113 PDF tests.
pub(crate) async fn seed_issued_invoice(client: &Client) -> (Value, String) {
    let (sc, cust) = json_req(
        client,
        "POST",
        "/api/customers",
        Some(json!({"name":"Tucano Time","currency":"EUR","default_rate_minor":6000,"email":"billing@tucanotime.test"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cust}");
    let cid = cust["id"].as_str().unwrap();
    new_project(client, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        client,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_s, inv) = json_req(
        client,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    let (si, issued) = json_req(client, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(si, StatusCode::OK, "{issued}");
    (issued, iid)
}

/// Unauthenticated request (for the auth tests).
pub(crate) async fn anon_req(
    client: &Client,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (s, v, _) = raw(&client.router, method, uri, body, None).await;
    (s, v)
}

/// Create a customer and return its object.
pub(crate) async fn new_customer(app: &Client, name: &str, currency: &str, rate: u64) -> Value {
    let (status, body) = json_req(
        app,
        "POST",
        "/api/customers",
        Some(json!({"name": name, "currency": currency, "default_rate_minor": rate})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create customer: {body}");
    body
}

pub(crate) async fn new_project(app: &Client, cid: &str, code: &str, extra: Value) -> Value {
    // currency + rate_minor are required since #11; default to the customer's
    // values and let `extra` override (e.g. a project-specific rate).
    let mut body = json!({"code": code, "currency": "EUR", "rate_minor": 6000});
    if let Value::Object(map) = extra {
        for (k, v) in map {
            body[k] = v;
        }
    }
    let (status, created) = json_req(
        app,
        "POST",
        &format!("/api/customers/{cid}/projects"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create project: {created}");
    created
}

pub(crate) fn entry_body(cid: &str, code: &str, hours: Value, date: &str) -> Value {
    json!({"date": date, "customer_id": cid, "project_code": code, "hours": hours})
}

/// Create a member and return their session cookie (shared by the RBAC tests).
pub(crate) async fn member_cookie(app: &Client, email: &str, pw: &str) -> String {
    let (s, body) = json_req(
        app,
        "POST",
        "/api/users",
        Some(json!({"name":"Member","email":email,"password":pw,"role":"member"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "create member: {body}");
    login_cookie(&app.router, email, pw).await
}
