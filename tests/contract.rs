// Contract tests: exercise the HTTP surface in-process against a temporary
// data dir, and check payloads against the checked-in openapi.json. This is
// the guard the shared rules demand: valid and invalid shapes, rejection
// before persistence, documented error shape, and no server-side leakage.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

use tucano_time::api::AppState;
use tucano_time::auth::Session;
use tucano_time::clock::SystemClock;
use tucano_time::lock::{EntryLock, LockReason};
use tucano_time::store::Store;

const ADMIN_EMAIL: &str = "admin@test.local";
const ADMIN_PW: &str = "supersecret1";

/// An in-process client holding a router and an authenticated session cookie.
struct Client {
    router: Router,
    cookie: String,
    /// The recording email transport installed on the router (#35).
    email: Arc<tucano_time::email::RecordingEmailSender>,
}

async fn raw(
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

async fn login_cookie(router: &Router, email: &str, pw: &str) -> String {
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
    async fn new(store: Store, locks: Option<Arc<dyn EntryLock>>) -> Self {
        Self::build(
            store,
            locks,
            tucano_time::payments::PaymentRegistry::default(),
            tucano_time::accounting::AccountingRegistry::default(),
        )
        .await
    }

    /// Client with accounting providers injected (#33 tests).
    async fn with_accounting(
        store: Store,
        accounting: tucano_time::accounting::AccountingRegistry,
    ) -> Self {
        Self::build(
            store,
            None,
            tucano_time::payments::PaymentRegistry::default(),
            accounting,
        )
        .await
    }

    async fn with_payments(store: Store, payments: tucano_time::payments::PaymentRegistry) -> Self {
        Self::build(
            store,
            None,
            payments,
            tucano_time::accounting::AccountingRegistry::default(),
        )
        .await
    }

    async fn build(
        store: Store,
        locks: Option<Arc<dyn EntryLock>>,
        payments: tucano_time::payments::PaymentRegistry,
        accounting: tucano_time::accounting::AccountingRegistry,
    ) -> Self {
        let session = Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        ));
        let recorder = Arc::new(tucano_time::email::RecordingEmailSender::default());
        let payments = Arc::new(payments);
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
                    vault: None,
                    email: recorder.clone(),
                    payments,
                    accounting,
                    sso: Arc::new(tucano_time::sso::SsoRegistry::default()),
                    oauth_flows: Arc::new(tucano_time::calendar_oauth::OAuthFlows::new()),
                    cfg: Arc::new(std::sync::Mutex::new(Arc::new(
                        tucano_time::appconfig::AppConfig::empty(),
                    ))),
                }
            }
            None => AppState::with_session(store, session)
                .with_email(recorder.clone())
                .with_payments(payments)
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
struct LockOne(Option<uuid::Uuid>);
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

async fn app() -> (Client, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("data")).expect("store");
    (Client::new(store, None).await, dir)
}

async fn app_locked(dir_path: &std::path::Path, locks: Arc<dyn EntryLock>) -> Client {
    let store = Store::open(dir_path.join("data")).expect("store");
    Client::new(store, Some(locks)).await
}

/// Authenticated request.
async fn json_req(
    client: &Client,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (s, v, _) = raw(&client.router, method, uri, body, Some(&client.cookie)).await;
    (s, v)
}

/// Authenticated request that keeps the **raw response bytes and headers** —
/// needed for the binary PDF download (#113), where the body is not JSON.
async fn raw_req(
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

/// Seed a customer (with billing email), one project and an entry, then
/// generate + issue an invoice. Returns the issued invoice JSON and the
/// invoice id. Shared by the #113 PDF tests.
async fn seed_issued_invoice(client: &Client) -> (Value, String) {
    let (sc, cust) = json_req(
        client,
        "POST",
        "/api/customers",
        Some(json!({"name":"ACME","currency":"EUR","default_rate_minor":6000,"email":"billing@acme.test"})),
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
async fn anon_req(
    client: &Client,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (s, v, _) = raw(&client.router, method, uri, body, None).await;
    (s, v)
}

/// Create a customer and return its object.
async fn new_customer(app: &Client, name: &str, currency: &str, rate: u64) -> Value {
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

async fn new_project(app: &Client, cid: &str, code: &str, extra: Value) -> Value {
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

fn entry_body(cid: &str, code: &str, hours: Value, date: &str) -> Value {
    json!({"date": date, "customer_id": cid, "project_code": code, "hours": hours})
}

// -------------------------------------------------------------------- tests --

#[tokio::test]
async fn healthz_is_ok() {
    let (app, _d) = app().await;
    let (status, body) = json_req(&app, "GET", "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

// ------------------------- mechanical router↔openapi.json drift check (#253) --
//
// AGENTS.md makes `openapi.json` the source of truth and requires route changes
// and contract changes to land in the same commit, but nothing verified it
// mechanically. Two sources are cross-checked here:
//
//   * `API_ROUTES` (src/lib.rs) — the canonical `(method, path, tier)` registry
//     the router is authored against;
//   * the live router from `build_router`, whose mounted path set is read back
//     from axum's `Debug` (axum exposes no route introspection).
//
// The tests diff the registry against the contract in both directions and probe
// the live router so the registry can never over-claim a route — and so a route
// added to or removed from `build_router` fails even when the registry and the
// contract were not touched. The `Error.code` vocabulary is already compared to
// `ApiError::codes()` by `contract_error_code_enum_matches_rust_vocabulary`
// (#249).

/// Replace axum `{param}` segments with a concrete value so a registry path can
/// be dispatched. The value is irrelevant — a probe never reaches a handler.
fn concretise(path: &str) -> String {
    path.split('/')
        .map(|seg| if seg.starts_with('{') { "probe" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

/// Dispatch `TRACE` — a method no route registers — and return the status and
/// the `Allow` header. Against a mounted path this yields 405 with the methods
/// it serves; an unmounted path falls through to the JSON 404 fallback; a
/// guarded path answers 401 before method routing when no session is supplied.
async fn trace_probe(
    router: &Router,
    path: &str,
    cookie: Option<&str>,
) -> (StatusCode, Option<String>) {
    let mut b = Request::builder()
        .method("TRACE")
        .uri(concretise(path))
        .header("x-csrf-protection", "1");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    let res = router
        .clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .expect("oneshot");
    let allow = res
        .headers()
        .get(header::ALLOW)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (res.status(), allow)
}

/// The path set the mounted router actually serves, read back from axum's
/// `Debug` of the route node (the only route introspection axum offers). The
/// always-present fallback routes (`/`, `/{*…}`) and the dev-only `/_dev/*`
/// endpoints — the live-reload SSE stream and its `/_dev/reload.js` client,
/// both 404 outside dev mode — are not JSON-API operations and are dropped.
/// If axum changes its `Debug` shape this fails loudly rather than silently
/// under-checking, which is the right failure mode for a test.
fn router_paths(router: &Router) -> std::collections::BTreeSet<String> {
    format!("{router:?}")
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|t| {
            t.starts_with('/') && !t.contains("{*") && *t != "/" && !t.starts_with("/_dev/")
        })
        .map(str::to_string)
        .collect()
}

/// The `(METHOD, path)` set the contract declares.
fn contract_operations(contract: &Value) -> std::collections::BTreeSet<(String, String)> {
    let mut ops = std::collections::BTreeSet::new();
    for (path, item) in contract["paths"].as_object().expect("paths object") {
        for method in item.as_object().expect("path item").keys() {
            if matches!(method.as_str(), "get" | "post" | "put" | "delete" | "patch") {
                ops.insert((method.to_ascii_uppercase(), path.clone()));
            }
        }
    }
    ops
}

/// Whether `METHOD path` declares the response `code`.
fn declares_code(
    paths: &serde_json::Map<String, Value>,
    method: &str,
    path: &str,
    code: &str,
) -> bool {
    paths
        .get(path)
        .and_then(|item| item.get(method.to_ascii_lowercase()))
        .and_then(|op| op.get("responses"))
        .and_then(|r| r.get(code))
        .is_some()
}

/// Whether `METHOD path` publishes an explicit empty `security: []` — i.e. the
/// contract marks the operation reachable without a session cookie.
fn declares_public_security(
    paths: &serde_json::Map<String, Value>,
    method: &str,
    path: &str,
) -> bool {
    paths
        .get(path)
        .and_then(|item| item.get(method.to_ascii_lowercase()))
        .and_then(|op| op.get("security"))
        .and_then(|s| s.as_array())
        .is_some_and(|s| s.is_empty())
}

/// Router mounts the contract lacks and vice versa, as `(METHOD, path, ticket)`.
/// Empty today — the two `(method, path)` sets already agree — but present so
/// future drift is allow-listed explicitly (with a ticket) rather than skipped.
const ROUTE_ALLOWLIST: &[(&str, &str, &str)] = &[];

/// Lock-guarded mutations and the response codes their handlers can actually
/// produce (#18/#185, `InvoiceLock`/`SubmissionLock`) that the contract must
/// declare. The broader 400/422/429 matrices were completed in #251; this keeps
/// the lock lifecycle pinned.
const LOCK_LIFECYCLE_CODES: &[(&str, &str, &[&str])] = &[
    ("PUT", "/api/entries/{id}", &["409", "503"]),
    ("DELETE", "/api/entries/{id}", &["409", "503"]),
    // #255: the expense lock now flows through the CombinedLocks seam too
    // (invoice + claim providers), so an unverifiable state is a 503 like the
    // entry paths — this was a raw 500 before the #190 fold.
    ("DELETE", "/api/expenses/{id}", &["409", "503"]),
    ("POST", "/api/submissions", &["503"]),
];

/// Operations that run before a session exists and must publish `security: []`.
const PUBLIC_OPERATIONS: &[(&str, &str)] = &[
    ("GET", "/healthz"),
    ("GET", "/openapi.json"),
    ("GET", "/docs"),
    ("POST", "/api/auth/bootstrap"),
    ("POST", "/api/auth/login"),
    ("POST", "/api/auth/logout"),
    ("GET", "/api/auth/status"),
    ("GET", "/api/auth/sso/providers"),
    ("POST", "/api/auth/sso/assertion"),
    ("POST", "/api/payments/webhook/{provider}"),
];

/// `(METHOD, path, kind, ticket)` gaps the contract is known to have, where
/// `kind` is a declared response code (e.g. `403`) or the literal
/// `security[]`. EVERY entry MUST name the ticket that will close it; the tests
/// fail if an entry is no longer a real gap, so this list only ever shrinks
/// (#253).
///
/// Empty today: the #251 response-code gaps (lock 409/503) are closed and
/// declared in `openapi.json`, so nothing remains to allow-list.
const ALLOWED_CONTRACT_GAPS: &[(&str, &str, &str, &str)] = &[];

#[tokio::test]
async fn router_routes_match_openapi_bidirectionally() {
    // #253 (a): the mounted router's (method, path) set must equal the contract
    // in both directions, accounting for the `/api` nest and the root
    // `/healthz|/openapi.json|/docs`.
    let contract: Value =
        serde_json::from_str(include_str!("../openapi.json")).expect("valid openapi.json");
    let contract_ops = contract_operations(&contract);
    let registry: std::collections::BTreeSet<(String, String)> = tucano_time::API_ROUTES
        .iter()
        .map(|(m, p, _)| ((*m).to_string(), (*p).to_string()))
        .collect();
    let allowed: std::collections::BTreeSet<(&str, &str)> =
        ROUTE_ALLOWLIST.iter().map(|(m, p, _)| (*m, *p)).collect();

    let mut drift = Vec::new();
    for (method, path) in &registry {
        if !contract_ops.contains(&(method.clone(), path.clone()))
            && !allowed.contains(&(method.as_str(), path.as_str()))
        {
            drift.push(format!("router mounts {method} {path}; contract does not"));
        }
    }
    for (method, path) in &contract_ops {
        if !registry.contains(&(method.clone(), path.clone()))
            && !allowed.contains(&(method.as_str(), path.as_str()))
        {
            drift.push(format!(
                "contract declares {method} {path}; router does not"
            ));
        }
    }
    assert!(
        drift.is_empty(),
        "router/contract route drift:\n  {}",
        drift.join("\n  ")
    );

    // #253 (a, live, paths): read the mounted path set straight off the router
    // so a route added/removed in `build_router` fails even when neither the
    // registry nor the contract changed.
    let (client, _dir) = app().await;
    let served = router_paths(&client.router);
    let registered: std::collections::BTreeSet<String> = tucano_time::API_ROUTES
        .iter()
        .map(|(_, p, _)| (*p).to_string())
        .collect();
    assert_eq!(
        served, registered,
        "the mounted router's paths drifted from API_ROUTES — update src/lib.rs"
    );

    // #253 (a, live, methods + tier): probe each path once. `TRACE` is
    // registered nowhere, so with an admin session a mounted path answers 405
    // and advertises its methods in `Allow`; without a session a guarded path
    // answers 401 while the open tier still answers 405.
    let mut expected: std::collections::BTreeMap<
        &str,
        (tucano_time::Tier, std::collections::BTreeSet<&'static str>),
    > = std::collections::BTreeMap::new();
    for (method, path, tier) in tucano_time::API_ROUTES {
        expected
            .entry(*path)
            .or_insert((*tier, std::collections::BTreeSet::new()))
            .1
            .insert(*method);
    }
    for (path, (tier, methods)) in &expected {
        let (status, allow) = trace_probe(&client.router, path, Some(&client.cookie)).await;
        assert_eq!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{path} is in API_ROUTES but the router did not mount it (probe {status})"
        );
        // GET implies HEAD, which `Allow` advertises; nothing else is implicit,
        // so the advertised set (minus HEAD) must be exactly the registry's.
        let advertised: std::collections::BTreeSet<String> = allow
            .unwrap_or_default()
            .split(',')
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty() && m != "HEAD")
            .collect();
        let want: std::collections::BTreeSet<String> =
            methods.iter().map(|m| (*m).to_string()).collect();
        assert_eq!(
            advertised, want,
            "{path}: the router serves {advertised:?} but API_ROUTES declares {want:?}"
        );

        let (anon, _) = trace_probe(&client.router, path, None).await;
        match tier {
            tucano_time::Tier::Open => assert_eq!(
                anon,
                StatusCode::METHOD_NOT_ALLOWED,
                "{path} is open but an unauthenticated probe returned {anon}"
            ),
            tucano_time::Tier::Member | tucano_time::Tier::Admin => assert_eq!(
                anon,
                StatusCode::UNAUTHORIZED,
                "{path} is guarded but an unauthenticated probe returned {anon}"
            ),
        }
    }
}

#[test]
fn contract_declares_producible_status_and_security() {
    // #253 (b): every code a handler can actually produce must be declared, and
    // the auth tiers the router enforces must be discoverable from the document.
    // Admin ops must declare 403; lock-guarded mutations must declare their
    // 409/503 lifecycle; pre-session ops must publish `security: []`. Known gaps
    // are allow-listed per ticket and must be removed once closed.
    let contract: Value =
        serde_json::from_str(include_str!("../openapi.json")).expect("valid openapi.json");
    assert_eq!(contract["openapi"], "3.1.0");
    let paths = contract["paths"].as_object().expect("paths object");

    let mut gaps: std::collections::BTreeSet<(String, String, String)> =
        std::collections::BTreeSet::new();
    for (method, path, tier) in tucano_time::API_ROUTES {
        if *tier == tucano_time::Tier::Admin && !declares_code(paths, method, path, "403") {
            gaps.insert(((*method).to_string(), (*path).to_string(), "403".into()));
        }
    }
    for (method, path, codes) in LOCK_LIFECYCLE_CODES {
        for code in *codes {
            if !declares_code(paths, method, path, code) {
                gaps.insert((
                    (*method).to_string(),
                    (*path).to_string(),
                    (*code).to_string(),
                ));
            }
        }
    }
    for (method, path) in PUBLIC_OPERATIONS {
        if !declares_public_security(paths, method, path) {
            gaps.insert((
                (*method).to_string(),
                (*path).to_string(),
                "security[]".into(),
            ));
        }
    }

    let mut allowed = std::collections::BTreeSet::new();
    for (m, p, k, _ticket) in ALLOWED_CONTRACT_GAPS {
        allowed.insert(((*m).to_string(), (*p).to_string(), (*k).to_string()));
    }

    let unexpected: Vec<String> = gaps
        .iter()
        .filter(|gap| !allowed.contains(*gap))
        .map(|(m, p, k)| format!("  {m} {p} must declare {k}"))
        .collect();
    assert!(
        unexpected.is_empty(),
        "contract omits producible status/security (fix the contract or allow-list it with a ticket):\n{}",
        unexpected.join("\n")
    );

    // Shrinking-only allow-list: an entry whose gap is already closed is stale.
    let mut stale = Vec::new();
    for (m, p, k, ticket) in ALLOWED_CONTRACT_GAPS {
        if !gaps.contains(&((*m).to_string(), (*p).to_string(), (*k).to_string())) {
            stale.push(format!("{m} {p} {k} ({ticket})"));
        }
    }
    assert!(
        stale.is_empty(),
        "stale allow-list entries — the gap is closed, remove them from ALLOWED_CONTRACT_GAPS: {}",
        stale.join(", ")
    );
}

#[test]
fn contract_error_code_enum_matches_rust_vocabulary() {
    // #249: the Error.code enum in the contract must be exactly the Rust
    // vocabulary (ApiError::codes()) — no stale codes, and none missing.
    let contract: Value =
        serde_json::from_str(include_str!("../openapi.json")).expect("valid openapi.json");
    let enum_codes: Vec<String> = contract["components"]["schemas"]["Error"]["properties"]["error"]
        ["properties"]["code"]["enum"]
        .as_array()
        .expect("Error.code enum")
        .iter()
        .map(|c| c.as_str().expect("string code").to_string())
        .collect();
    let rust_codes: Vec<String> = tucano_time::error::ApiError::codes()
        .iter()
        .map(|c| (*c).to_string())
        .collect();
    assert_eq!(
        enum_codes, rust_codes,
        "openapi.json Error.code enum drifted from ApiError::codes()"
    );
}

#[test]
fn contract_documents_csrf_on_mutating_operations() {
    // #249: every mutating operation (except the documented pre-auth/signature
    // exemptions) must document the X-CSRF-Protection header and a 403.
    let contract: Value =
        serde_json::from_str(include_str!("../openapi.json")).expect("valid openapi.json");
    // Mirrors the `exempt` set in csrf_guard (src/lib.rs) plus the payment
    // webhook prefix check.
    let exempt = [
        "/api/auth/login",
        "/api/auth/bootstrap",
        "/api/auth/sso/assertion",
    ];
    let paths = contract["paths"].as_object().expect("paths object");
    for (path, item) in paths {
        for method in ["post", "put", "delete", "patch"] {
            let Some(op) = item.get(method) else { continue };
            if exempt.contains(&path.as_str()) || path.starts_with("/api/payments/webhook/") {
                continue;
            }
            let empty = Vec::new();
            let params = op["parameters"].as_array().unwrap_or(&empty);
            assert!(
                params
                    .iter()
                    .any(|p| p["$ref"] == "#/components/parameters/CsrfProtection"),
                "{method} {path} must document the CSRF header"
            );
            assert!(
                op["responses"].get("403").is_some(),
                "{method} {path} must document a 403 (csrf) response"
            );
        }
    }
}

// --------------------------- #252 contract document fidelity ----------------
//
// #252 replaced the document's `nullable` (3.0-only, silently ignored by 3.1
// validators), corrected parameter requiredness/placement and the timer/webhook/
// draft-line shapes, and declared every operation tag. These tests read the
// checked-in document itself, so a regression in any of those fixes fails here.

/// The `name`/`$ref` tail of every parameter declared on `METHOD path`, at
/// operation and path-item level alike (the levels are merged on purpose: a
/// parameter parked at path level applies to the operation too, which is exactly
/// the leak #252 (5) fixed).
fn declared_params(contract: &Value, method: &str, path: &str) -> Vec<String> {
    let item = &contract["paths"][path];
    let mut out = Vec::new();
    for level in [item, &item[method.to_ascii_lowercase()]] {
        for param in level["parameters"].as_array().into_iter().flatten() {
            out.push(match param["$ref"].as_str() {
                Some(reference) => reference
                    .rsplit('/')
                    .next()
                    .unwrap_or(reference)
                    .to_string(),
                None => param["name"].as_str().unwrap_or_default().to_string(),
            });
        }
    }
    out
}

/// Whether `schema` is a JSON Schema union that admits `null` (#252 (7)):
/// `{"type": [..., "null"]}` or `oneOf: [..., {"type": "null"}]`.
fn admits_null(schema: &Value) -> bool {
    let type_union = schema["type"]
        .as_array()
        .is_some_and(|types| types.iter().any(|t| t == "null"));
    let one_of = schema["oneOf"]
        .as_array()
        .is_some_and(|arms| arms.iter().any(|arm| arm["type"] == "null"));
    type_union || one_of
}

#[test]
fn contract_openapi_has_no_nullable_keyword() {
    // #252 (7): `nullable` is an OpenAPI 3.0 keyword. This document is 3.1, so a
    // strict validator ignores it and `null` stops matching; the only valid
    // spellings are a `"type": [..., "null"]` union or a `oneOf` with
    // `{"type": "null"}`, which is what every nullable site here uses.
    fn walk(node: &Value, path: &str, hits: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                if map.contains_key("nullable") {
                    hits.push(path.to_string());
                }
                for (key, value) in map {
                    walk(value, &format!("{path}/{key}"), hits);
                }
            }
            Value::Array(items) => {
                for (index, value) in items.iter().enumerate() {
                    walk(value, &format!("{path}/{index}"), hits);
                }
            }
            _ => {}
        }
    }
    let contract: Value =
        serde_json::from_str(include_str!("../openapi.json")).expect("valid openapi.json");
    let mut hits = Vec::new();
    walk(&contract, "", &mut hits);
    assert!(
        hits.is_empty(),
        "`nullable` is 3.0-only and ignored by 3.1 validators; replace it with a null union: {}",
        hits.join(", ")
    );
}

#[test]
fn contract_declares_all_operation_tags() {
    // #252 (9): Swagger UI renders a tag that is missing from the root `tags`
    // list as an extra, unsorted group. Every tag an operation uses must be
    // declared, and a declared-but-unused tag is dead weight — both drift.
    let contract: Value =
        serde_json::from_str(include_str!("../openapi.json")).expect("valid openapi.json");
    let declared: std::collections::BTreeSet<String> = contract["tags"]
        .as_array()
        .expect("root tags array")
        .iter()
        .map(|tag| tag["name"].as_str().expect("tag name").to_string())
        .collect();
    let mut used: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (path, item) in contract["paths"].as_object().expect("paths object") {
        for method in ["get", "post", "put", "delete", "patch"] {
            let Some(op) = item.get(method) else { continue };
            for tag in op["tags"].as_array().expect("operation tags") {
                used.entry(tag.as_str().expect("tag name").to_string())
                    .or_default()
                    .push(format!("{} {path}", method.to_ascii_uppercase()));
            }
        }
    }
    let missing: Vec<String> = used
        .iter()
        .filter(|(tag, _)| !declared.contains(*tag))
        .map(|(tag, ops)| format!("{tag} (used by {})", ops.join(", ")))
        .collect();
    assert!(
        missing.is_empty(),
        "root `tags` does not declare: {}",
        missing.join("; ")
    );
    let unused: Vec<&str> = declared
        .iter()
        .filter(|tag| !used.contains_key(*tag))
        .map(String::as_str)
        .collect();
    assert!(
        unused.is_empty(),
        "root `tags` declares unused tags: {}",
        unused.join(", ")
    );
}

/// A walker that fails on a duplicate key inside the same JSON object (#252 (8)).
/// `serde_json::Value` keeps the last duplicate silently, so the document needs
/// its own strict pass: that is how `ProjectInput.budget_amount_minor` ended up
/// carrying two `minimum` keys, one of which no validator could see.
struct NoDuplicateKeys;

impl<'de> serde::Deserialize<'de> for NoDuplicateKeys {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateKeysVisitor)
    }
}

struct NoDuplicateKeysVisitor;

impl<'de> serde::de::Visitor<'de> for NoDuplicateKeysVisitor {
    type Value = NoDuplicateKeys;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut seen: Vec<String> = Vec::new();
        while let Some(key) = map.next_key::<String>()? {
            if seen.contains(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON key `{key}`"
                )));
            }
            seen.push(key);
            map.next_value::<NoDuplicateKeys>()?;
        }
        Ok(NoDuplicateKeys)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        while seq.next_element::<NoDuplicateKeys>()?.is_some() {}
        Ok(NoDuplicateKeys)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateKeysVisitor)
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateKeysVisitor)
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }
}

#[test]
fn contract_openapi_has_no_duplicate_keys() {
    let text = include_str!("../openapi.json");
    let mut deserializer = serde_json::Deserializer::from_str(text);
    NoDuplicateKeys::deserialize(&mut deserializer)
        .expect("openapi.json must not contain a duplicate key in one object");
    deserializer
        .end()
        .expect("openapi.json must be a single JSON document");
}

#[test]
fn contract_range_and_oauth_parameter_requiredness_matches_handlers() {
    // #252 (1, 2, 5): the range-only handlers (`parse_range`) hard-fail 400 when
    // from or to is absent, so their components are required; the flexible list
    // endpoints keep the pair optional (and must not inherit it from path level,
    // which is how POST /api/entries used to advertise three params it ignores);
    // the OAuth callback reads code/state with serde defaults, so neither is
    // required.
    let contract: Value =
        serde_json::from_str(include_str!("../openapi.json")).expect("valid openapi.json");
    let params = &contract["components"]["parameters"];
    for name in ["FromParam", "ToParam"] {
        assert_eq!(
            params[name]["required"],
            json!(true),
            "{name} is required: the range handlers answer 400 without it"
        );
    }
    for name in ["OptionalFromParam", "OptionalToParam"] {
        assert_eq!(
            params[name]["required"],
            json!(false),
            "{name} is optional: the list endpoints filter only when the pair is given"
        );
    }
    for path in [
        "/api/reports/summary",
        "/api/reports/export.csv",
        "/api/reports/profitability",
        "/api/calendar/events",
        "/api/invoices/report",
    ] {
        let declared = declared_params(&contract, "GET", path);
        assert!(
            declared.iter().any(|p| p == "FromParam") && declared.iter().any(|p| p == "ToParam"),
            "GET {path} needs the required range pair, found {declared:?}"
        );
    }
    let entries = declared_params(&contract, "GET", "/api/entries");
    assert!(
        entries.iter().any(|p| p == "DateParam")
            && entries.iter().any(|p| p == "OptionalFromParam")
            && entries.iter().any(|p| p == "OptionalToParam"),
        "GET /api/entries takes a day or an optional range, found {entries:?}"
    );
    let create = declared_params(&contract, "POST", "/api/entries");
    assert_eq!(
        create,
        vec!["CsrfProtection"],
        "POST /api/entries ignores the list filters and must not inherit them"
    );
    for name in ["code", "state"] {
        let param = contract["paths"]["/api/calendar/oauth/callback"]["get"]["parameters"]
            .as_array()
            .expect("callback parameters")
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("{name} must be declared"));
        assert_eq!(
            param["required"],
            json!(false),
            "the handler defaults oauth callback {name}, so it is not required"
        );
    }
}

#[test]
fn contract_declares_problem_operation_shapes() {
    // #252 (3, 4, 6, 7): the timer start returns the running Timer; the webhook
    // documents the signature headers it reads; PUT /api/invoices/{id} declares
    // its path `id` once; the draft edit references the input line shape (only
    // kind+date mandatory) rather than the stored one; and GET /api/timer can be
    // null through a 3.1 union.
    let contract: Value =
        serde_json::from_str(include_str!("../openapi.json")).expect("valid openapi.json");
    let started = &contract["paths"]["/api/timer"]["post"]["responses"]["201"];
    assert_eq!(
        started["content"]["application/json"]["schema"]["$ref"],
        json!("#/components/schemas/Timer"),
        "POST /api/timer 201 returns the serialized timer"
    );
    assert!(
        contract["components"]["schemas"]["Timer"].is_object(),
        "the finished timer start needs a Timer schema to reference"
    );
    let listed = &contract["paths"]["/api/timer"]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];
    assert!(
        admits_null(listed),
        "GET /api/timer answers JSON null when no timer runs: {listed}"
    );

    let webhook = declared_params(&contract, "POST", "/api/payments/webhook/{provider}");
    for header in ["x-webhook-signature", "stripe-signature"] {
        assert!(
            webhook.iter().any(|p| p == header),
            "the webhook reads {header}; declared params: {webhook:?}"
        );
    }

    let edit = declared_params(&contract, "PUT", "/api/invoices/{id}");
    assert_eq!(
        edit.iter().filter(|p| *p == "id").count(),
        1,
        "the path `id` must be declared once, not at both path and operation level: {edit:?}"
    );

    let lines =
        &contract["components"]["schemas"]["InvoiceEditInput"]["properties"]["lines"]["items"];
    assert_eq!(
        lines["$ref"],
        json!("#/components/schemas/InvoiceLineInput"),
        "the edit body takes the input line shape, not the stored one"
    );
    assert_eq!(
        contract["components"]["schemas"]["InvoiceLineInput"]["required"],
        json!(["kind", "date"]),
        "only kind and date are mandatory on a submitted line"
    );
}

#[tokio::test]
async fn unknown_field_rejected_before_persist() {
    let (app, d) = app().await;
    let (status, body) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"ACME","currency":"EUR","default_rate_minor":1,"evil":true})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_failed");
    assert_eq!(body["error"]["fields"][0]["field"], "evil");
    assert_eq!(body["error"]["fields"][0]["message"], "unknown field");
    // Nothing was written: the customers collection stays empty.
    let (s2, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert_eq!(s2, StatusCode::OK);
    assert!(list["customers"].as_array().unwrap().is_empty());
    let _ = d;
}

#[tokio::test]
async fn wrong_type_is_validation_failure() {
    let (app, _d) = app().await;
    // name given as a number
    let (status, body) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":42,"currency":"EUR","default_rate_minor":1})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_failed");
}

#[tokio::test]
async fn currency_normalised_and_rejected() {
    let (app, _d) = app().await;
    let created = new_customer(&app, "ACME", "eur", 6000).await;
    assert_eq!(
        created["currency"], "EUR",
        "currency is normalised to upper case"
    );

    let (status, _) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"X","currency":"Euros","default_rate_minor":1})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn full_customer_project_entry_flow() {
    let (app, _d) = app().await;
    let cust = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = cust["id"].as_str().unwrap();

    let proj = new_project(&app, cid, "p-1", json!({"name":"Portal"})).await;
    assert_eq!(proj["code"], "P-1", "code is normalised to upper case");

    let (status, entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "p-1", json!(8), "2026-10-02")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let eid = entry["id"].as_str().unwrap();
    assert_eq!(entry["hours"], 8);
    assert!(entry["note"].is_string(), "note defaults to empty string");

    // Read back by day and by id.
    let (s, list) = json_req(&app, "GET", "/api/entries?date=2026-10-02", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["entries"].as_array().unwrap().len(), 1);
    let (s2, got) = json_req(&app, "GET", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(got["id"], eid);
}

#[tokio::test]
async fn entry_requires_existing_customer_and_project() {
    let (app, _d) = app().await;
    let (status, body) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(
            "00000000-0000-0000-0000-000000000000",
            "P1",
            json!(1),
            "2026-10-02",
        )),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let fields = body["error"]["fields"].as_array().unwrap();
    assert!(fields.iter().any(|f| f["field"] == "customer_id"));
    assert!(fields.iter().any(|f| f["field"] == "project_code"));
}

#[tokio::test]
async fn project_must_belong_to_customer() {
    let (app, _d) = app().await;
    let a = new_customer(&app, "A", "EUR", 100).await;
    let b = new_customer(&app, "B", "EUR", 100).await;
    new_project(&app, a["id"].as_str().unwrap(), "P1", json!({})).await;
    // Use A's project code under B.
    let (status, body) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(
            b["id"].as_str().unwrap(),
            "P1",
            json!(1),
            "2026-10-02",
        )),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["field"] == "project_code")
    );
}

#[tokio::test]
async fn hours_boundaries() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    // 24.00 accepted.
    let (ok, _) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(24.00), "2026-10-02")),
    )
    .await;
    assert_eq!(ok, StatusCode::CREATED);

    // 24.01 rejected.
    let (bad, _) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(24.01), "2026-10-02")),
    )
    .await;
    assert_eq!(bad, StatusCode::UNPROCESSABLE_ENTITY);

    // three decimals rejected.
    let (bad2, _) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1.234), "2026-10-02")),
    )
    .await;
    assert_eq!(bad2, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn invalid_date_rejected_with_field_detail() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (status, body) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-13-40")),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["fields"][0]["field"], "date");
}

#[tokio::test]
async fn duplicate_project_conflicts() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (status, _) = json_req(
        &app,
        "POST",
        &format!("/api/customers/{cid}/projects"),
        Some(json!({"code": "P1", "currency": "EUR", "rate_minor": 6000})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn customer_delete_blocked_while_referenced() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    // Has projects -> cannot delete.
    let (status, _) = json_req(&app, "DELETE", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    // Delete the project, then it's removable.
    let (s, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/customers/{cid}/projects/P1"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s2, _) = json_req(&app, "DELETE", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(s2, StatusCode::NO_CONTENT);
    let (s3, _) = json_req(&app, "GET", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(s3, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn range_and_date_queries_are_exclusive_forms() {
    let (app, _d) = app().await;
    let (status, _) = json_req(&app, "GET", "/api/entries", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status2, _) = json_req(
        &app,
        "GET",
        "/api/entries?date=2026-10-02&from=2026-10-01",
        None,
    )
    .await;
    assert_eq!(status2, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn report_groups_by_project_with_effective_rate() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await; // 60.00/h default
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "STD", json!({})).await;
    new_project(&app, cid, "PREM", json!({"rate_minor": 12000})).await; // 120.00/h override

    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "STD", json!(2), "2026-10-02")),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "PREM", json!(2), "2026-10-02")),
    )
    .await;

    let (status, summary) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=project",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = summary["rows"].as_array().unwrap();
    let prem = rows
        .iter()
        .find(|r| r["label"].as_str().unwrap().ends_with("PREM"))
        .unwrap();
    let std = rows
        .iter()
        .find(|r| r["label"].as_str().unwrap().ends_with("STD"))
        .unwrap();
    assert_eq!(prem["amount_minor"], 24000, "2h * 120.00 = 240.00");
    assert_eq!(std["amount_minor"], 12000, "2h * 60.00 = 120.00");
}

#[tokio::test]
async fn csv_export_quotes_and_escapes_note() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME, Inc.", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let body = json!({
        "date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":1.5,
        "note":"=SUM(A1) injected"
    });
    json_req(&app, "POST", "/api/entries", Some(body)).await;
    let (status, csv) = json_req(
        &app,
        "GET",
        "/api/reports/export.csv?from=2026-10-01&to=2026-10-07",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let text = csv.as_str().unwrap();
    assert!(text.starts_with("date,customer,"), "header present");
    // Formula injection is defused with a leading apostrophe and the comma in
    // the customer name is quoted.
    assert!(
        text.contains("\"ACME, Inc.\""),
        "comma field quoted; got: {text}"
    );
    assert!(text.contains("'=SUM(A1)"), "formula escaped; got: {text}");
}

#[tokio::test]
async fn error_shape_never_leaks_internals() {
    let (app, _d) = app().await;
    let (status, body) = json_req(
        &app,
        "GET",
        "/api/entries/00000000-0000-0000-0000-000000000000",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
    let text = body.to_string();
    for leak in ["data/", "temp", "stack", "No such file", "store.rs"] {
        assert!(!text.contains(leak), "error body leaked {leak:?}");
    }
}

#[tokio::test]
async fn update_entry_can_move_day() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (_, entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    let eid = entry["id"].as_str().unwrap();

    let (status, updated) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(entry_body(cid, "P1", json!(1), "2026-10-03")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["date"], "2026-10-03");
    // Old day now empty, new day has it.
    let (_, old) = json_req(&app, "GET", "/api/entries?date=2026-10-02", None).await;
    assert!(old["entries"].as_array().unwrap().is_empty());
    let (_, new) = json_req(&app, "GET", "/api/entries?date=2026-10-03", None).await;
    assert_eq!(new["entries"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn oversized_note_rejected() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let body = json!({
        "date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":1,
        "note": "x".repeat(501)
    });
    let (status, _) = json_req(&app, "POST", "/api/entries", Some(body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn project_requires_currency_and_rate() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    // Missing rate_minor.
    let (s1, b1) = json_req(
        &app,
        "POST",
        &format!("/api/customers/{cid}/projects"),
        Some(json!({"code": "P1", "currency": "EUR"})),
    )
    .await;
    assert_eq!(s1, StatusCode::UNPROCESSABLE_ENTITY, "{b1}");
    // Missing currency.
    let (s2, _) = json_req(
        &app,
        "POST",
        &format!("/api/customers/{cid}/projects"),
        Some(json!({"code": "P1", "rate_minor": 6000})),
    )
    .await;
    assert_eq!(s2, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn legacy_project_document_resolves_from_customer() {
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "USD", 4500).await;
    let cid = c["id"].as_str().unwrap();
    // Write a pre-#11 project document that omits currency and rate_minor.
    let dir = d
        .path()
        .join("data")
        .join("customers")
        .join(cid)
        .join("projects");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("OLD-1.json"),
        format!(r#"{{"customer_id":"{cid}","code":"OLD-1","name":"Old","active":true}}"#),
    )
    .unwrap();
    // Reading it back resolves currency/rate from the customer (USD / 4500).
    let (status, project) = json_req(
        &app,
        "GET",
        &format!("/api/customers/{cid}/projects/OLD-1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(project["currency"], "USD");
    assert_eq!(project["rate_minor"], 4500);
    // And it appears in the list with the same resolved values.
    let (ls, list) = json_req(&app, "GET", &format!("/api/customers/{cid}/projects"), None).await;
    assert_eq!(ls, StatusCode::OK);
    assert_eq!(list["projects"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn entry_billable_defaults_true_and_persists_false() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    // Omitted -> billable true.
    let (_, def) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    assert_eq!(def["billable"], true);

    // Explicit false persists.
    let body = json!({
        "date": "2026-10-03", "customer_id": cid, "project_code": "P1",
        "hours": 2, "note": "internal", "billable": false
    });
    let (_, nb) = json_req(&app, "POST", "/api/entries", Some(body)).await;
    assert_eq!(nb["billable"], false);
    let eid = nb["id"].as_str().unwrap();
    let (_, got) = json_req(&app, "GET", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(got["billable"], false);
}

#[tokio::test]
async fn legacy_entry_document_without_billable_reads_true() {
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    // Craft a pre-#20 entry document (no `billable` field) directly on disk.
    let id = uuid::Uuid::new_v4();
    let dir = d.path().join("data").join("entries").join("2026-10-04");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.json")),
        format!(
            r#"{{"id":"{id}","date":"2026-10-04","customer_id":"{cid}","project_code":"P1","hours":3,"note":"","created_at":"2026-10-04T09:00:00Z","updated_at":"2026-10-04T09:00:00Z"}}"#
        ),
    )
    .unwrap();
    let (status, list) = json_req(&app, "GET", "/api/entries?date=2026-10-04", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["entries"][0]["billable"], true);
}

#[tokio::test]
async fn entry_source_defaults_manual() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (_, e) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    assert_eq!(e["source"], "manual");
}

#[tokio::test]
async fn legacy_entry_document_without_source_reads_manual() {
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let id = uuid::Uuid::new_v4();
    let dir = d.path().join("data").join("entries").join("2026-10-05");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.json")),
        format!(
            r#"{{"id":"{id}","date":"2026-10-05","customer_id":"{cid}","project_code":"P1","hours":2,"note":"","billable":true,"created_at":"2026-10-05T09:00:00Z","updated_at":"2026-10-05T09:00:00Z"}}"#
        ),
    )
    .unwrap();
    let (status, e) = json_req(&app, "GET", &format!("/api/entries/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(e["source"], "manual");
}

#[tokio::test]
async fn locked_entry_rejects_edit_and_delete_but_not_read() {
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (_, e) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    let eid: uuid::Uuid = e["id"].as_str().unwrap().parse().unwrap();
    drop(app);

    // Reopen the same data dir with that entry locked (as an invoice would).
    let app2 = app_locked(d.path(), Arc::new(LockOne(Some(eid)))).await;
    let body = json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2,"billable":true});
    let (s_edit, _) = json_req(&app2, "PUT", &format!("/api/entries/{eid}"), Some(body)).await;
    assert_eq!(s_edit, StatusCode::CONFLICT);
    let (s_del, _) = json_req(&app2, "DELETE", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(s_del, StatusCode::CONFLICT);
    let (s_get, _) = json_req(&app2, "GET", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(s_get, StatusCode::OK, "locked entries are still readable");
}

// ------------------------------------------------- fail-closed locking (185) --

#[tokio::test]
async fn unverifiable_lock_state_refuses_writes_but_never_reads() {
    // #185: lock providers used to map a store error to "not locked". With an
    // unreadable invoice document the locks must fail CLOSED: entry edit and
    // delete get 503 (Retry-After), submission is refused, reads still work.
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    let (_, e) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(&cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    let eid = e["id"].as_str().unwrap().to_string();

    // Corrupt the invoices collection so InvoiceLock cannot verify its state.
    let inv_dir = d.path().join("data").join("invoices");
    std::fs::create_dir_all(&inv_dir).unwrap();
    std::fs::write(inv_dir.join("corrupt.json"), b"{ this is not json").unwrap();

    let body = json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2,"billable":true});
    let (s_edit, _) = json_req(&app, "PUT", &format!("/api/entries/{eid}"), Some(body)).await;
    assert_eq!(
        s_edit,
        StatusCode::SERVICE_UNAVAILABLE,
        "edit must fail closed"
    );
    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(
        s_del,
        StatusCode::SERVICE_UNAVAILABLE,
        "delete must fail closed"
    );
    let (s_sub, _) = json_req(
        &app,
        "POST",
        "/api/submissions",
        Some(json!({"week_start": "2026-09-28"})),
    )
    .await;
    assert_eq!(
        s_sub,
        StatusCode::SERVICE_UNAVAILABLE,
        "submit must fail closed"
    );
    let (s_get, _) = json_req(&app, "GET", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(s_get, StatusCode::OK, "reads are never blocked");
}

#[tokio::test]
async fn unverifiable_claim_state_refuses_expense_delete_but_never_reads() {
    // #255 (+ the #190 follow-up): the expense lock now flows through the same
    // CombinedLocks seam the entries use, so a claim provider that cannot
    // verify its state fails the delete CLOSED with 503 — before the fold this
    // path surfaced a raw store error as a 500. Reads stay untouched.
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    let (_, x) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"amount_minor":5000,"currency":"EUR"})),
    )
    .await;
    let xid = x["id"].as_str().unwrap().to_string();

    // Corrupt the claims collection so the claim provider cannot verify state.
    let claims_dir = d.path().join("data").join("claims");
    std::fs::create_dir_all(&claims_dir).unwrap();
    std::fs::write(claims_dir.join("corrupt.json"), b"{ not json").unwrap();

    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None).await;
    assert_eq!(
        s_del,
        StatusCode::SERVICE_UNAVAILABLE,
        "expense delete must fail closed when the lock state is unverifiable"
    );
    let (s_get, _) = json_req(&app, "GET", &format!("/api/expenses/{xid}"), None).await;
    assert_eq!(s_get, StatusCode::OK, "reads are never blocked");
}

#[tokio::test]
async fn delete_customer_refused_when_project_docs_are_unreadable() {
    // #185: `delete_customer` swallowed a failed project listing as "empty"
    // and then remove_dir_all'd the customer tree INCLUDING the unreadable
    // project docs. An unlistable collection must refuse the deletion and
    // leave every file intact.
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    let project_path = d
        .path()
        .join(format!("data/customers/{cid}/projects/P1.json"));
    std::fs::write(&project_path, b"{ this is not json").unwrap();

    let (s, b) = json_req(&app, "DELETE", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{b}");
    assert!(
        d.path().join(format!("data/customers/{cid}.json")).exists(),
        "customer document must survive a refused delete"
    );
    assert!(
        project_path.exists(),
        "project documents must not be deleted when they could not be listed"
    );
}

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

// ------------------------------------------------------------------ tasks --

async fn new_task(app: &Client, cid: &str, pcode: &str, code: &str, extra: Value) -> Value {
    let mut body = json!({"code": code});
    if let Value::Object(map) = extra {
        for (k, v) in map {
            body[k] = v;
        }
    }
    let (status, created) = json_req(
        app,
        "POST",
        &format!("/api/customers/{cid}/projects/{pcode}/tasks"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create task: {created}");
    created
}

#[tokio::test]
async fn task_crud_and_entry_reference() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    let t = new_task(&app, cid, "P1", "t-9", json!({"name": "Design"})).await;
    assert_eq!(t["code"], "T-9", "task code normalised");

    // Entry referencing the task.
    let body = json!({
        "date": "2026-10-02", "customer_id": cid, "project_code": "P1",
        "task_code": "T-9", "hours": 2
    });
    let (s, e) = json_req(&app, "POST", "/api/entries", Some(body)).await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    assert_eq!(e["task_code"], "T-9");

    // Task delete blocked while an entry references it.
    let (s_del, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/customers/{cid}/projects/P1/tasks/T-9"),
        None,
    )
    .await;
    assert_eq!(s_del, StatusCode::CONFLICT);

    // Project delete blocked while a task exists.
    let (s_pdel, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/customers/{cid}/projects/P1"),
        None,
    )
    .await;
    assert_eq!(s_pdel, StatusCode::CONFLICT);
}

#[tokio::test]
async fn entry_task_must_belong_to_project() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    new_project(&app, cid, "P2", json!({})).await;
    new_task(&app, cid, "P2", "T1", json!({})).await;
    // Reference P2's task under P1.
    let body = json!({
        "date": "2026-10-02", "customer_id": cid, "project_code": "P1",
        "task_code": "T1", "hours": 1
    });
    let (s, b) = json_req(&app, "POST", "/api/entries", Some(body)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        b["error"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["field"] == "task_code")
    );
}

#[tokio::test]
async fn reports_and_invoices_use_project_rates_for_legacy_tasks() {
    let (app, dir) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await; // 60/h default
    let cid = c["id"].as_str().unwrap();
    new_project(
        &app,
        cid,
        "P1",
        json!({"currency": "USD", "rate_minor": 7500}),
    )
    .await;
    let mut task = new_task(&app, cid, "P1", "PREM", json!({})).await;
    task["currency"] = json!("GBP");
    task["rate_minor"] = json!(9500);
    std::fs::write(
        dir.path()
            .join(format!("data/customers/{cid}/projects/P1/tasks/PREM.json")),
        serde_json::to_vec(&task).unwrap(),
    )
    .unwrap();

    let body = json!({
        "date": "2026-10-02", "customer_id": cid, "project_code": "P1",
        "task_code": "PREM", "hours": 2
    });
    json_req(&app, "POST", "/api/entries", Some(body)).await;
    // Same project, no task -> project rate.
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":1})),
    )
    .await;

    let (s, summary) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=customer",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(summary["rows"][0]["amount_minor"], 22500);
    assert_eq!(summary["rows"][0]["currency"], "USD");
    let (status, invoice) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({
            "customer_id": cid, "from": "2026-10-01", "to": "2026-10-07"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{invoice}");
    assert_eq!(invoice["total_minor"], 22500);
    assert_eq!(invoice["currency"], "USD");
    assert!(
        invoice["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|line| line["rate_minor"] == 7500)
    );
}

#[tokio::test]
async fn tasks_reject_billing_overrides_without_persistence() {
    let (app, _dir) = app().await;
    let customer = new_customer(&app, "Task rates", "EUR", 6000).await;
    let cid = customer["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let task = new_task(&app, cid, "P1", "T1", json!({"name": "Original"})).await;
    let base = format!("/api/customers/{cid}/projects/P1/tasks");
    for field in ["rate_minor", "currency"] {
        for method in ["POST", "PUT"] {
            let mut payload =
                json!({"code": if method == "POST" { "NEW" } else { "T1" }, "name": "Changed"});
            payload[field] = if field == "rate_minor" {
                json!(9500)
            } else {
                json!("GBP")
            };
            let path = if method == "POST" {
                base.clone()
            } else {
                format!("{base}/T1")
            };
            let (status, _) = json_req(&app, method, &path, Some(payload)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            let (_, saved) = json_req(&app, "GET", &format!("{base}/T1"), None).await;
            assert_eq!(saved, task);
            let (missing, _) = json_req(&app, "GET", &format!("{base}/NEW"), None).await;
            assert_eq!(missing, StatusCode::NOT_FOUND);
        }
    }
}

// ------------------------------------------------------------------- auth --

#[tokio::test]
async fn protected_routes_require_auth() {
    let (app, _d) = app().await;
    // Unauthenticated -> 401.
    let (s, _) = anon_req(&app, "GET", "/api/customers", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Authenticated -> 200.
    let (s2, _) = json_req(&app, "GET", "/api/customers", None).await;
    assert_eq!(s2, StatusCode::OK);
}

#[tokio::test]
async fn bootstrap_only_works_once() {
    let (app, _d) = app().await;
    // A second bootstrap must be refused (an admin already exists).
    let (s, _) = anon_req(
        &app,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"X","email":"x@y.co","password":"whatever12"})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn login_rejects_bad_password_without_leaking() {
    let (app, _d) = app().await;
    let (s, body) = anon_req(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({"email": ADMIN_EMAIL, "password": "wrong-password"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "invalid_credentials");
    // Same generic code for an unknown email (no user enumeration).
    let (s2, b2) = anon_req(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({"email": "nobody@test.local", "password": "whatever12"})),
    )
    .await;
    assert_eq!(s2, StatusCode::UNAUTHORIZED);
    assert_eq!(b2["error"]["code"], "invalid_credentials");
}

#[tokio::test]
async fn me_returns_current_user() {
    let (app, _d) = app().await;
    let (s, me) = json_req(&app, "GET", "/api/auth/me", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["email"], ADMIN_EMAIL);
    assert_eq!(me["role"], "admin");
    assert!(
        me.get("password_hash").is_none(),
        "the hash must never be returned"
    );
}

/// A wrong verb on a real path answers the app's one error shape and says which
/// verbs it does take — axum's default is a bare 405 with an empty body, which
/// tells a client nothing (and hides a client talking to a stale server that
/// does not serve a route yet).
#[tokio::test]
async fn a_wrong_method_answers_the_error_shape_and_the_allow_header() {
    let (app, _d) = app().await;
    // The path exists (GET, POST); PATCH is not mounted on it.
    let (status, headers, body) = raw_req(&app, "PATCH", "/api/expenses").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{body:?}");
    let json: Value = serde_json::from_slice(&body).expect("the app's error shape");
    assert_eq!(json["error"]["code"], "method_not_allowed", "{json}");
    let message = json["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("PATCH") && message.contains("GET") && message.contains("POST"),
        "the message names the verb and the accepted ones: {json}"
    );
    let allow = headers
        .get(header::ALLOW)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        allow.contains("GET") && allow.contains("POST"),
        "Allow lists what the path takes: {allow:?}"
    );

    // A templated path resolves to its own methods.
    let id = uuid::Uuid::new_v4();
    let (status, _, body) = raw_req(&app, "POST", &format!("/api/expenses/{id}")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{body:?}");
    let json: Value = serde_json::from_slice(&body).expect("the app's error shape");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("PUT"),
        "an edit path offers PUT: {json}"
    );

    // An unknown path stays a 404 in the API namespace: the fallback answers
    // method mismatches only, never invents paths.
    let (status, _, _) = raw_req(&app, "PATCH", "/api/not-a-route").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn member_cannot_manage_users() {
    let (app, _d) = app().await;
    // Admin creates a member.
    let (s, _) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Pam","email":"pam@test.local","password":"memberpass1","role":"member"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    // Member logs in and is forbidden from /users, but can use /customers.
    let cookie = login_cookie(&app.router, "pam@test.local", "memberpass1").await;
    let (s_users, _, _) = raw(&app.router, "GET", "/api/users", None, Some(&cookie)).await;
    assert_eq!(s_users, StatusCode::FORBIDDEN);
    let (s_cust, _, _) = raw(&app.router, "GET", "/api/customers", None, Some(&cookie)).await;
    assert_eq!(s_cust, StatusCode::OK);
}

#[tokio::test]
async fn admin_can_update_user_and_duplicate_email_is_rejected_without_persistence() {
    let (app, _d) = app().await;
    let (s, bob) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({
            "name":"Bob","email":"bob@test.local","password":"bobpass123","role":"member",
            "active":true,"default_rate_minor":4500,"cost_rate_minor":2200
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let bob_id = bob["id"].as_str().unwrap();

    let (s, updated) = json_req(
        &app,
        "PUT",
        &format!("/api/users/{bob_id}"),
        Some(json!({
            "name":"Bob Q","email":"bob@test.local","role":"admin","active":false,
            "default_rate_minor":5000,"cost_rate_minor":2400
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(updated["name"], "Bob Q");
    assert_eq!(updated["role"], "admin");
    assert_eq!(updated["active"], false);
    assert_eq!(updated["default_rate_minor"], 5000);
    assert_eq!(updated["cost_rate_minor"], 2400);

    let (s, _) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({
            "name":"Other","email":"BOB@test.local","password":"otherpass1","role":"member"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn member_cannot_update_user() {
    let (app, _d) = app().await;
    let (s, user) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Pam","email":"pam@test.local","password":"memberpass1","role":"member"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let cookie = login_cookie(&app.router, "pam@test.local", "memberpass1").await;
    let (s, _, _) = raw(
        &app.router,
        "PUT",
        &format!("/api/users/{}", user["id"].as_str().unwrap()),
        Some(json!({"name":"Changed"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn password_change_revokes_existing_sessions_and_accepts_new_password() {
    let (app, _d) = app().await;
    let (s, user) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({
            "name":"Pam",
            "email":"pam@test.local",
            "password":"memberpass1",
            "role":"member"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let old_cookie = login_cookie(&app.router, "pam@test.local", "memberpass1").await;
    let id = user["id"].as_str().unwrap();
    let (s, _) = json_req(
        &app,
        "PUT",
        &format!("/api/users/{id}/password"),
        Some(json!({ "password": "newmemberpass1" })),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (s_old, _, _) = raw(&app.router, "GET", "/api/auth/me", None, Some(&old_cookie)).await;
    assert_eq!(s_old, StatusCode::UNAUTHORIZED);

    let new_cookie = login_cookie(&app.router, "pam@test.local", "newmemberpass1").await;
    let (s_new, _, _) = raw(&app.router, "GET", "/api/auth/me", None, Some(&new_cookie)).await;
    assert_eq!(s_new, StatusCode::OK);
}

#[tokio::test]
async fn user_management_validation_and_authorization_persist_nothing() {
    let (app, dir) = app().await;
    let (_, user) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"QA","email":"qa@test.local","password":"qa-password1"})),
    )
    .await;
    let id = user["id"].as_str().unwrap();
    let path = format!("/api/users/{id}");
    let store = Store::open(dir.path().join("data")).unwrap();
    let uid = uuid::Uuid::parse_str(id).unwrap();
    let before = serde_json::to_value(store.get_user(uid).unwrap().unwrap()).unwrap();
    for invalid in [
        json!({"name":""}),
        json!({"email":"invalid"}),
        json!({"role":"owner"}),
        json!({"default_rate_minor":100000001}),
        json!({"cost_rate_minor":-1}),
        json!({"cost_rate_minor":1.5}),
        json!({"unexpected":true}),
    ] {
        let (status, body) = json_req(&app, "PUT", &path, Some(invalid.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body["error"].is_object());
        let mut create =
            json!({"name":"New","email":"new@test.local","password":"initial-password1"});
        create
            .as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        let (status, _) = json_req(&app, "POST", "/api/users", Some(create)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(store.list_users().unwrap().len(), 2);
        assert_eq!(
            serde_json::to_value(store.get_user(uid).unwrap().unwrap()).unwrap(),
            before
        );
    }
    for method in ["POST", "PUT"] {
        let path = if method == "POST" {
            "/api/users".to_string()
        } else {
            format!("/api/users/{id}/password")
        };
        let payload = if method == "POST" {
            json!({"name":"New","email":"new@test.local","password":"short"})
        } else {
            json!({"password":"short"})
        };
        let (status, _) = json_req(&app, method, &path, Some(payload)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let cookie = login_cookie(&app.router, "qa@test.local", "qa-password1").await;
    for (method, path, payload) in [
        ("GET", "/api/users".to_string(), None),
        (
            "POST",
            "/api/users".to_string(),
            Some(json!({"name":"New","email":"new@test.local","password":"initial-password1"})),
        ),
        (
            "PUT",
            format!("/api/users/{id}"),
            Some(json!({"name":"Changed"})),
        ),
        (
            "PUT",
            format!("/api/users/{id}/password"),
            Some(json!({"password":"changed-password1"})),
        ),
        ("DELETE", format!("/api/users/{id}"), None),
    ] {
        let (status, _, _) = raw(&app.router, method, &path, payload, Some(&cookie)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        serde_json::to_value(store.get_user(uid).unwrap().unwrap()).unwrap(),
        before
    );
}

#[tokio::test]
async fn user_management_preserves_admin_history_and_revalidates_sessions() {
    let (app, dir) = app().await;
    let (_, me) = json_req(&app, "GET", "/api/auth/me", None).await;
    let admin_id = me["id"].as_str().unwrap();
    for body in [json!({"role":"member"}), json!({"active":false})] {
        let (status, _) =
            json_req(&app, "PUT", &format!("/api/users/{admin_id}"), Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
    let (status, _) = json_req(&app, "DELETE", &format!("/api/users/{admin_id}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, user) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"QA","email":"qa@test.local","password":"qa-password1"})),
    )
    .await;
    let id = user["id"].as_str().unwrap();
    let path = format!("/api/users/{id}");
    let cookie = login_cookie(&app.router, "qa@test.local", "qa-password1").await;
    let (status, _) = json_req(
        &app,
        "PUT",
        &path,
        Some(json!({"email":"renamed@test.local","role":"member","active":true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        raw(&app.router, "GET", "/api/auth/me", None, Some(&cookie))
            .await
            .0,
        StatusCode::OK
    );
    assert!(
        login_cookie(&app.router, "qa@test.local", "qa-password1")
            .await
            .is_empty()
    );
    let renamed_cookie = login_cookie(&app.router, "renamed@test.local", "qa-password1").await;
    assert!(!renamed_cookie.is_empty());
    let (status, _) = json_req(&app, "PUT", &path, Some(json!({"email":ADMIN_EMAIL}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    json_req(&app, "PUT", &path, Some(json!({"role":"admin"}))).await;
    assert_eq!(
        raw(
            &app.router,
            "GET",
            "/api/auth/me",
            None,
            Some(&renamed_cookie)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let admin_cookie = login_cookie(&app.router, "renamed@test.local", "qa-password1").await;
    json_req(&app, "PUT", &path, Some(json!({"active":false}))).await;
    assert_eq!(
        raw(
            &app.router,
            "GET",
            "/api/auth/me",
            None,
            Some(&admin_cookie)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    json_req(
        &app,
        "PUT",
        &path,
        Some(json!({"active":true,"role":"member"})),
    )
    .await;
    let cookie = login_cookie(&app.router, "renamed@test.local", "qa-password1").await;
    let customer = new_customer(&app, "History", "EUR", 6000).await;
    let cid = customer["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (timer_status, _, _) = raw(
        &app.router,
        "POST",
        "/api/timer",
        Some(json!({"customer_id":cid,"project_code":"P1"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(timer_status, StatusCode::CREATED);
    assert_eq!(
        json_req(&app, "DELETE", &path, None).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        raw(&app.router, "DELETE", "/api/timer", None, Some(&cookie))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (status, _, _) = raw(
        &app.router,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-06","customer_id":cid,"project_code":"P1","hours":1})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        json_req(&app, "DELETE", &path, None).await.0,
        StatusCode::CONFLICT
    );
    json_req(&app, "PUT", &path, Some(json!({"active":false}))).await;
    assert_eq!(
        json_req(&app, "DELETE", &path, None).await.0,
        StatusCode::CONFLICT
    );
    let (_, list) = json_req(&app, "GET", "/api/users", None).await;
    assert!(!list.to_string().contains("password"));
    let (_, disposable) = json_req(&app, "POST", "/api/users", Some(json!({"name":"Disposable","email":"disposable@test.local","password":"disposable-password1"}))).await;
    assert_eq!(
        json_req(
            &app,
            "DELETE",
            &format!("/api/users/{}", disposable["id"].as_str().unwrap()),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let store = Store::open(dir.path().join("data")).unwrap();
    let mut last_admin = store
        .get_user(uuid::Uuid::parse_str(admin_id).unwrap())
        .unwrap()
        .unwrap();
    let expected = last_admin.clone();
    last_admin.active = false;
    assert!(store.put_user_if_unchanged(&last_admin, &expected).is_err());
    assert!(store.delete_user(expected.id).is_err());
    let stale = store
        .get_user(uuid::Uuid::parse_str(id).unwrap())
        .unwrap()
        .unwrap();
    let mut updated = stale.clone();
    updated.name = "Concurrent edit".into();
    store.put_user_if_unchanged(&updated, &stale).unwrap();
    assert!(store.put_user_if_unchanged(&stale, &stale).is_err());
    let audit = std::fs::read_to_string(dir.path().join("data/audit.log")).unwrap();
    assert!(!audit.contains("qa-password1"));
}

#[tokio::test]
async fn report_applies_person_rate_tier_and_attributes_entry() {
    let (app, _d) = app().await;
    // Customer default 60/h; project rate 30/h.
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 3000})).await;

    // Admin creates a member with a personal default of 45/h.
    let (_s, bob) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"Bob","email":"bob@test.local","password":"bobpass123","role":"member","default_rate_minor":4500})),
    )
    .await;
    let bob_id = bob["id"].as_str().unwrap().to_string();

    // Bob logs 2h on P1.
    let bob_cookie = login_cookie(&app.router, "bob@test.local", "bobpass123").await;
    let (_s2, entry, _) = raw(
        &app.router,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
        Some(&bob_cookie),
    )
    .await;
    assert_eq!(
        entry["user_id"], bob_id,
        "entry attributed to the logging user"
    );

    // Report: person rate (45) overrides project rate (30) -> 2h * 45 = 90.00 = 9000.
    let (_s3, summary) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=customer",
        None,
    )
    .await;
    assert_eq!(summary["rows"][0]["amount_minor"], 9000);
}

// --------------------------------------------------------------- invoices --

#[tokio::test]
async fn invoice_sums_billable_and_ignores_nonbillable() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    // 3h billable @60 = 180.00
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    // 5h NON-billable -> excluded
    json_req(&app, "POST", "/api/entries", Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":5,"billable":false}))).await;

    let (s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    assert_eq!(inv["status"], "draft");
    assert_eq!(inv["currency"], "EUR");
    assert_eq!(inv["lines"].as_array().unwrap().len(), 1);
    assert_eq!(inv["total_minor"], 18000);
}

#[tokio::test]
async fn issuing_invoice_locks_its_entries() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    let (_, e) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let eid = e["id"].as_str().unwrap();

    let (_, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();

    // Draft does NOT lock.
    let (s_before, _) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":4})),
    )
    .await;
    assert_eq!(
        s_before,
        StatusCode::OK,
        "draft invoice must not lock entries"
    );

    // Issue -> now locked.
    let (si, _) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(si, StatusCode::OK);
    let (s_edit, body) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":4})),
    )
    .await;
    assert_eq!(s_edit, StatusCode::CONFLICT, "{body}");
    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(s_del, StatusCode::CONFLICT);
}

#[tokio::test]
async fn regeneration_excludes_invoiced_entries() {
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
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{}/issue", inv["id"].as_str().unwrap()),
        None,
    )
    .await;
    // Second invoice over the same period has nothing left to bill.
    let (s2, body) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn invoice_rejects_mixed_currencies() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"currency":"EUR","rate_minor":6000})).await;
    new_project(&app, cid, "P2", json!({"currency":"USD","rate_minor":3000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":1})),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P2","hours":1})),
    )
    .await;
    let (s, body) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("currencies")
    );
}

// ---------------------------------------------------------------- expenses --

#[tokio::test]
async fn category_and_expense_crud_with_references() {
    let (app, _d) = app().await;
    let (s0, defaults) = json_req(&app, "GET", "/api/categories", None).await;
    assert_eq!(s0, StatusCode::OK);
    let default_names = defaults["categories"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|category| category["name"].as_str())
        .collect::<Vec<_>>();
    for name in ["Food", "Travel", "Lodging", "Supplies", "Software"] {
        assert!(
            default_names.contains(&name),
            "missing default category {name}"
        );
    }
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    let (sc, cat) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Travel"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED);
    let cat_id = cat["id"].as_str().unwrap();

    let (su, updated) = json_req(
        &app,
        "PUT",
        &format!("/api/categories/{cat_id}"),
        Some(json!({"name":"Work travel","default_billable":true,"active":true})),
    )
    .await;
    assert_eq!(su, StatusCode::OK);
    assert_eq!(updated["name"], "Work travel");

    let (se, exp) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"project_code":"P1","category_id":cat_id,
            "amount_minor":12500,"currency":"EUR","note":"flight"
        })),
    )
    .await;
    assert_eq!(se, StatusCode::CREATED, "{exp}");
    assert_eq!(exp["billable"], true); // default

    // Category can't be deleted while an expense references it.
    let (sd, _) = json_req(&app, "DELETE", &format!("/api/categories/{cat_id}"), None).await;
    assert_eq!(sd, StatusCode::CONFLICT);

    // Filter expenses by customer.
    let (sf, list) = json_req(
        &app,
        "GET",
        &format!("/api/expenses?customer_id={cid}"),
        None,
    )
    .await;
    assert_eq!(sf, StatusCode::OK);
    assert_eq!(list["expenses"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn expense_rejects_unknown_project_and_category() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (s, body) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"project_code":"NOPE",
            "category_id":"00000000-0000-0000-0000-000000000000","amount_minor":100,"currency":"EUR"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let fields: Vec<&str> = body["error"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["field"].as_str().unwrap())
        .collect();
    assert!(fields.contains(&"project_code"));
    assert!(fields.contains(&"category_id"));
}

#[tokio::test]
async fn expense_receipt_roundtrips() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (_, e) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":500,"currency":"EUR",
            "receipt_name":"hotel.png","receipt_b64":"aGVsbG8="
        })),
    )
    .await;
    let eid = e["id"].as_str().unwrap();
    let (_, got) = json_req(&app, "GET", &format!("/api/expenses/{eid}"), None).await;
    assert_eq!(got["receipt_name"], "hotel.png");
    assert_eq!(got["receipt_b64"], "aGVsbG8=");
}

/// A receipt rides inline (#23), so the transport cap has to clear the field
/// cap: a scanned page is megabytes of base64, and while the body limit sat
/// below the field rule it answered `413 length limit exceeded` before any
/// handler saw the payload — the receipt could never be stored at all. This
/// pins both rules: what the dialog promises (up to ~3 MB of file) is accepted
/// whole, and what the API refuses comes back as the app's own 422 naming
/// `receipt_b64`, never as a bare 413 from the transport.
#[tokio::test]
async fn a_megabyte_receipt_is_stored_and_an_over_long_one_is_the_apps_own_422() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    // ~1 MB of base64: the order of magnitude of a scanned receipt.
    let stored = "A".repeat(1_000_000);
    let (s, e) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":500,"currency":"EUR",
            "receipt_name":"scan.pdf","receipt_b64":stored
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    let eid = e["id"].as_str().unwrap();
    let (_, got) = json_req(&app, "GET", &format!("/api/expenses/{eid}"), None).await;
    assert_eq!(
        got["receipt_b64"].as_str().map(str::len),
        Some(1_000_000),
        "the bytes travel whole"
    );

    // Past the field rule the answer is the app's error shape, and nothing is
    // written: the caller learns which field was wrong, not that a body limit
    // tripped somewhere in front of the router.
    let too_big = "A".repeat(4_000_001);
    let (s, body) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":500,"currency":"EUR",
            "receipt_name":"huge.pdf","receipt_b64":too_big
        })),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["fields"][0]["field"], "receipt_b64");
    assert_eq!(body["error"]["fields"][0]["message"], "receipt too large");
}

/// A converted expense keeps the amount as entered, its currency and the rate
/// that produced the billed pair, and the billed pair stays authoritative.
#[tokio::test]
async fn expense_records_the_conversion_it_was_entered_with() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    // USD 100.00 spent, billed as EUR 92.06 at the published 2026-10-01 rate.
    // The rate is padded and carries trailing zeros on purpose: what is stored
    // must be the digits the caller wrote, normalised — never a float's
    // rendering of them (#11).
    let (s, e) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":9206,"currency":"EUR",
            "original_amount_minor":10000,"original_currency":"USD",
            "exchange_rate":" 0.920600 ","exchange_rate_date":"2026-10-01",
            "note":"conference fee"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    assert_eq!(
        e["amount_minor"], 9206,
        "the billed amount is authoritative"
    );
    assert_eq!(e["currency"], "EUR");
    assert_eq!(e["original_amount_minor"], 10000);
    assert_eq!(e["original_currency"], "USD");
    assert_eq!(e["exchange_rate"], "0.9206");
    assert_eq!(e["exchange_rate_date"], "2026-10-01");

    // The provenance survives the document round-trip, on both read paths.
    let eid = e["id"].as_str().unwrap();
    let (_, got) = json_req(&app, "GET", &format!("/api/expenses/{eid}"), None).await;
    assert_eq!(got["exchange_rate"], "0.9206");
    assert_eq!(got["exchange_rate_date"], "2026-10-01");
    let (_, list) = json_req(&app, "GET", "/api/expenses", None).await;
    assert_eq!(list["expenses"][0]["original_currency"], "USD");

    // An expense that was never converted carries none of the four keys, so a
    // reader can tell "billed as entered" from "converted" without guessing.
    let (sp, plain) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-03","customer_id":cid,"amount_minor":500,"currency":"EUR"})),
    )
    .await;
    assert_eq!(sp, StatusCode::CREATED, "{plain}");
    for key in [
        "original_amount_minor",
        "original_currency",
        "exchange_rate",
        "exchange_rate_date",
    ] {
        assert!(
            plain.get(key).is_none(),
            "an unconverted expense must not carry {key}: {plain}"
        );
    }
}

/// The conversion group is all-or-nothing, a rate is a positive six-decimal
/// number (never a float), and a rejected payload persists nothing.
#[tokio::test]
async fn expense_conversion_is_all_or_nothing_and_never_a_float() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    // The billed pair is EUR; the amount was entered in USD.
    let full = || {
        json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":9206,"currency":"EUR",
            "original_amount_minor":10000,"original_currency":"USD",
            "exchange_rate":"0.9206","exchange_rate_date":"2026-10-01"
        })
    };
    let without = |keys: &[&str]| {
        let mut body = full();
        for key in keys {
            body.as_object_mut().unwrap().remove(*key);
        }
        body
    };
    let (s, ok) = json_req(&app, "POST", "/api/expenses", Some(full())).await;
    assert_eq!(s, StatusCode::CREATED, "{ok}");

    let mut cases: Vec<(Value, &str, &str)> = vec![
        (
            without(&["original_amount_minor"]),
            "original_amount_minor",
            "the amount as entered is missing",
        ),
        (
            without(&["original_currency"]),
            "original_currency",
            "the entered currency is missing",
        ),
        (
            without(&["exchange_rate"]),
            "exchange_rate",
            "the applied rate is missing",
        ),
    ];
    // No sign, no exponent, no thousands separator, no seventh decimal, no
    // magnitude beyond the documented ceiling, and no zero.
    for bad in [
        "0",
        "0.0",
        "-1",
        "+1",
        "1e3",
        "1.2345678",
        "1000001",
        "abc",
        "1,5",
    ] {
        let mut body = full();
        body["exchange_rate"] = json!(bad);
        cases.push((
            body,
            "exchange_rate",
            "a rate that is not positive and 6-decimal",
        ));
    }
    let mut same_currency = full();
    same_currency["original_currency"] = json!("EUR");
    cases.push((
        same_currency,
        "original_currency",
        "the billed currency converted with itself",
    ));
    let mut bad_date = full();
    bad_date["exchange_rate_date"] = json!("yesterday");
    cases.push((
        bad_date,
        "exchange_rate_date",
        "a rate date that is not a date",
    ));
    let mut huge = full();
    huge["original_amount_minor"] = json!(100_000_001);
    cases.push((
        huge,
        "original_amount_minor",
        "an original amount above the billed ceiling",
    ));

    for (body, field, why) in cases {
        let (status, err) = json_req(&app, "POST", "/api/expenses", Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}: {err}");
        let fields: Vec<&str> = err["error"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["field"].as_str().unwrap())
            .collect();
        assert!(
            fields.contains(&field),
            "{why}: expected {field} in {fields:?}"
        );
        // Rejection without persistence is part of the contract.
        let (_, list) = json_req(&app, "GET", "/api/expenses", None).await;
        assert_eq!(
            list["expenses"].as_array().unwrap().len(),
            1,
            "{why}: the refused payload must not be persisted"
        );
    }
}

/// An edit replaces the record: new values land, the identity and history stay,
/// and a field the payload omits is cleared — the same PUT semantics the entries
/// endpoint documents.
#[tokio::test]
async fn expense_update_replaces_the_record_and_keeps_its_identity() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    new_project(&app, &cid, "P2", json!({})).await;
    let (s, created) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"project_code":"P1","amount_minor":9206,
            "currency":"EUR","original_amount_minor":10000,"original_currency":"USD",
            "exchange_rate":"0.9206","exchange_rate_date":"2026-10-01","note":"conference fee"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    let (su, updated) = json_req(
        &app,
        "PUT",
        &format!("/api/expenses/{id}"),
        Some(json!({
            "date":"2026-10-09","customer_id":cid,"project_code":"P2","amount_minor":4200,
            "currency":"EUR","billable":false,"note":"taxi"
        })),
    )
    .await;
    assert_eq!(su, StatusCode::OK, "{updated}");
    assert_eq!(updated["id"], created["id"], "the id survives the edit");
    assert_eq!(
        updated["created_at"], created["created_at"],
        "creation history is kept"
    );
    assert_eq!(
        updated["user_id"], created["user_id"],
        "an edit never rewrites the author (#51)"
    );
    assert_eq!(updated["date"], "2026-10-09");
    assert_eq!(updated["project_code"], "P2");
    assert_eq!(updated["amount_minor"], 4200);
    assert_eq!(updated["billable"], false);
    assert_eq!(updated["note"], "taxi");
    assert!(
        updated["original_currency"].is_null() && updated["exchange_rate"].is_null(),
        "a conversion the payload omits is cleared by a full replacement: {updated}"
    );

    // The stored document agrees, on both read paths.
    let (_, got) = json_req(&app, "GET", &format!("/api/expenses/{id}"), None).await;
    assert_eq!(got["note"], "taxi");
    assert_eq!(got["project_code"], "P2");
    assert!(got["original_amount_minor"].is_null());
    let (_, list) = json_req(&app, "GET", "/api/expenses", None).await;
    assert_eq!(list["expenses"][0]["note"], "taxi");

    // The receipt is part of the record too: an edit replaces or clears it like
    // any other field (#23).
    let (sr, with_receipt) = json_req(
        &app,
        "PUT",
        &format!("/api/expenses/{id}"),
        Some(json!({
            "date":"2026-10-09","customer_id":cid,"amount_minor":4200,"currency":"EUR",
            "receipt_name":"taxi.png","receipt_b64":"aGVsbG8="
        })),
    )
    .await;
    assert_eq!(sr, StatusCode::OK, "{with_receipt}");
    assert_eq!(with_receipt["receipt_name"], "taxi.png");
    assert_eq!(with_receipt["receipt_b64"], "aGVsbG8=");
    let (sc, cleared) = json_req(
        &app,
        "PUT",
        &format!("/api/expenses/{id}"),
        Some(json!({
            "date":"2026-10-09","customer_id":cid,"amount_minor":4200,"currency":"EUR"
        })),
    )
    .await;
    assert_eq!(sc, StatusCode::OK, "{cleared}");
    assert!(
        cleared.get("receipt_name").is_none() && cleared.get("receipt_b64").is_none(),
        "an omitted receipt is cleared by a full replacement: {cleared}"
    );

    // A later edit can put a conversion back, normalised exactly as on create.
    let (s2, reconverted) = json_req(
        &app,
        "PUT",
        &format!("/api/expenses/{id}"),
        Some(json!({
            "date":"2026-10-09","customer_id":cid,"amount_minor":4200,"currency":"EUR",
            "original_amount_minor":4500,"original_currency":"USD","exchange_rate":"1.080000"
        })),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "{reconverted}");
    assert_eq!(reconverted["original_amount_minor"], 4500);
    assert_eq!(reconverted["exchange_rate"], "1.08");
    assert!(
        reconverted["exchange_rate_date"].is_null(),
        "the rate's date is optional: {reconverted}"
    );
}

/// The update path answers like the create and the delete: an unowned record is
/// a 404 (never a 403 — that would leak its existence, #51), a bad shape is a
/// 422 that changes nothing, and an expense frozen by an open invoice is a 409.
#[tokio::test]
async fn expense_update_refuses_unowned_bad_and_locked_expenses() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    let (_, x) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"project_code":"P1","amount_minor":5000,
            "currency":"EUR","note":"parts"
        })),
    )
    .await;
    let xid = x["id"].as_str().unwrap().to_string();
    let path = format!("/api/expenses/{xid}");
    let body = json!({
        "date":"2026-10-02","customer_id":cid,"project_code":"P1","amount_minor":5000,
        "currency":"EUR","note":"tampered"
    });

    // No session at all.
    let (s_anon, _, _) = raw(&app.router, "PUT", &path, Some(body.clone()), None).await;
    assert_eq!(s_anon, StatusCode::UNAUTHORIZED, "member-tier route");

    // A member editing someone else's expense: 404, and the note is untouched.
    let member = member_cookie(&app, "eve@test.local", "evepass123").await;
    let (s_other, _, _) = raw(&app.router, "PUT", &path, Some(body.clone()), Some(&member)).await;
    assert_eq!(
        s_other,
        StatusCode::NOT_FOUND,
        "an unowned expense must not leak its existence"
    );
    let (_, still) = json_req(&app, "GET", &path, None).await;
    assert_eq!(still["note"], "parts");

    // A shape the create would refuse: unknown project → 422, nothing persisted.
    let (s_bad, bad) = json_req(
        &app,
        "PUT",
        &path,
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"project_code":"NOPE","amount_minor":5000,
            "currency":"EUR","note":"tampered"
        })),
    )
    .await;
    assert_eq!(s_bad, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    let (_, unchanged) = json_req(&app, "GET", &path, None).await;
    assert_eq!(unchanged["note"], "parts");
    assert_eq!(unchanged["project_code"], "P1");

    // Issued invoice → the expense is frozen against edits, exactly as it is
    // against deletes (#18/#218), and settling it releases the record again.
    let (_, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (s_locked, locked) = json_req(&app, "PUT", &path, Some(body.clone())).await;
    assert_eq!(s_locked, StatusCode::CONFLICT, "{locked}");
    assert!(
        locked["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("open invoice"),
        "{locked}"
    );
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"amount_minor": 5000, "reference": "wire-1"})),
    )
    .await;
    let (s_free, freed) = json_req(&app, "PUT", &path, Some(body)).await;
    assert_eq!(
        s_free,
        StatusCode::OK,
        "a settled invoice releases its expense for correction: {freed}"
    );
    assert_eq!(freed["note"], "tampered");
}

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

#[tokio::test]
async fn report_splits_billable_and_groups_by_person() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    // 3h billable + 2h non-billable, both by the admin.
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    json_req(&app, "POST", "/api/entries", Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2,"billable":false}))).await;

    let (_, s) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=customer",
        None,
    )
    .await;
    assert_eq!(s["billable_hours"], 3.0);
    assert_eq!(s["nonbillable_hours"], 2.0);
    assert_eq!(s["total_hours"], 5.0);

    // billable=true scopes rows to billable only (3h).
    let (_, sb) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=customer&billable=true",
        None,
    )
    .await;
    assert_eq!(sb["rows"][0]["hours"], 3.0);

    // group=person yields one row for the admin.
    let (_, sp) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=person",
        None,
    )
    .await;
    assert_eq!(sp["group"], "person");
    assert_eq!(sp["rows"][0]["label"], "Admin");
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

// --------------------------------------------------- RBAC / private-per-user --

#[tokio::test]
async fn member_sees_only_own_entries() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    // Admin logs one entry.
    let (_, admin_entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
    )
    .await;
    let admin_eid = admin_entry["id"].as_str().unwrap().to_string();

    // Create member Dana and log one entry as Dana.
    json_req(&app, "POST", "/api/users", Some(json!({"name":"Dana","email":"dana@test.local","password":"danapass123","role":"member"}))).await;
    let dana = login_cookie(&app.router, "dana@test.local", "danapass123").await;
    let (_, dana_entry, _) = raw(
        &app.router,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
        Some(&dana),
    )
    .await;
    let dana_eid = dana_entry["id"].as_str().unwrap().to_string();

    // Dana lists entries -> only her own.
    let (s, list, _) = raw(
        &app.router,
        "GET",
        "/api/entries?date=2026-10-02",
        None,
        Some(&dana),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let ids: Vec<&str> = list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![dana_eid.as_str()], "member sees only own entries");

    // Dana cannot read the admin's entry (404, not 403 — no existence leak).
    let (s404, _, _) = raw(
        &app.router,
        "GET",
        &format!("/api/entries/{admin_eid}"),
        None,
        Some(&dana),
    )
    .await;
    assert_eq!(s404, StatusCode::NOT_FOUND);

    // Admin sees both.
    let (_, admin_list) = json_req(&app, "GET", "/api/entries?date=2026-10-02", None).await;
    assert_eq!(admin_list["entries"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn member_cannot_invoice_or_approve() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Eve","email":"eve@test.local","password":"evepass123","role":"member"}),
        ),
    )
    .await;
    let eve = login_cookie(&app.router, "eve@test.local", "evepass123").await;

    let (s_inv, _, _) = raw(
        &app.router,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
        Some(&eve),
    )
    .await;
    assert_eq!(
        s_inv,
        StatusCode::FORBIDDEN,
        "member cannot create invoices"
    );
    let (s_users, _, _) = raw(&app.router, "GET", "/api/users", None, Some(&eve)).await;
    assert_eq!(s_users, StatusCode::FORBIDDEN, "member cannot list users");
    let (s_dec, _, _) = raw(
        &app.router,
        "POST",
        "/api/submissions/00000000-0000-0000-0000-000000000000/decision",
        Some(json!({"decision":"approve"})),
        Some(&eve),
    )
    .await;
    assert_eq!(s_dec, StatusCode::FORBIDDEN, "member cannot approve");
}

#[tokio::test]
async fn ubuntu_font_is_embedded_and_served_with_font_mime_type() {
    let (app, _dir) = app().await;
    let request = Request::builder()
        .uri("/vendor/ubuntu/f1ea362b-Ubuntu-wdth-wght--latin-v0.896a.woff2")
        .body(Body::empty())
        .unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "font/woff2");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..4], b"wOF2");
}

#[tokio::test]
async fn security_headers_present() {
    let (client, _d) = app().await;
    let res = client
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let h = res.headers();
    assert_eq!(
        h.get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert_eq!(
        h.get("x-frame-options").and_then(|v| v.to_str().ok()),
        Some("DENY")
    );
    assert_eq!(
        h.get("referrer-policy").and_then(|v| v.to_str().ok()),
        Some("no-referrer")
    );
    let csp = h
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(csp.contains("default-src 'self'"), "CSP present: {csp}");
    assert!(csp.contains("frame-ancestors 'none'"));
    // The receipt preview (#23) renders a PDF from a blob URL this origin minted
    // itself, so framing is opened to 'self' and blob: and nothing else — script,
    // style and connect stay strictly 'self' (review A9).
    assert!(
        csp.contains("frame-src 'self' blob:"),
        "the receipt preview needs its own frame source: {csp}"
    );
}

#[tokio::test]
async fn mutation_without_csrf_header_is_forbidden() {
    let (client, _d) = app().await;
    // POST without the X-CSRF-Protection header -> 403 (login is exempt, so
    // use an authenticated data mutation).
    let res = client
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/customers")
                .header(header::COOKIE, &client.cookie)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"name":"X","currency":"EUR","default_rate_minor":1}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn page_routes_serve_the_shell_and_never_shadow_the_api() {
    let (app, _d) = app().await;
    let get = |uri: &str| {
        let router = app.router.clone();
        let uri = uri.to_owned();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response")
        }
    };
    // Page routes serve the single-page shell (case-insensitive, trailing
    // slash ignored) so every view can be bookmarked or referenced.
    for uri in [
        "/",
        "/timesheets/week",
        "/Timesheets/Calendar/",
        "/customers",
        "/settings/users",
        "/approvals",
    ] {
        let res = get(uri).await;
        assert_eq!(res.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            res.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8",
            "{uri}"
        );
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        assert!(
            String::from_utf8_lossy(&bytes).starts_with("<!doctype html>"),
            "{uri}: expected the GUI shell"
        );
    }
    // The API lives under /api and keeps its session gate; page paths are
    // allowlisted (not a blanket catch-all), and meta stays at the root.
    let res = get("/api/customers").await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = get("/healthz").await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = get("/definitely/not/a/page").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("json error shape");
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test]
async fn login_rate_limited_after_repeated_failures() {
    let (client, _d) = app().await;
    let body = json!({"email": "attacker@test.local", "password": "wrong"});
    let mut last = StatusCode::UNAUTHORIZED;
    // 8 failures allowed (each 401); the 9th is locked out (429).
    for _ in 0..8 {
        let (s, _) = anon_req(&client, "POST", "/api/auth/login", Some(body.clone())).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        last = s;
    }
    let (s9, _) = anon_req(&client, "POST", "/api/auth/login", Some(body)).await;
    assert_eq!(
        s9,
        StatusCode::TOO_MANY_REQUESTS,
        "locked after threshold (last was {last:?})"
    );
}

#[test]
fn concurrent_writer_times_out_with_lock_busy() {
    use fs2::FileExt;
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    // A store that gives up quickly when the writer lock is held.
    let store = tucano_time::store::Store::with_lock_timeout(
        dir.path().join("data"),
        Duration::from_millis(50),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    // Simulate another process holding the advisory writer lock.
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.path().join("data").join(".tucanotime.lock"))
        .unwrap();
    held.lock_exclusive().unwrap();

    let cust = tucano_time::domain::Customer {
        id: uuid::Uuid::new_v4(),
        name: "X".into(),
        currency: tucano_time::domain::Currency("EUR".into()),
        default_rate_minor: 1,
        active: true,
        email: String::new(),
        payment_terms: None,
        invoice_notes: String::new(),
        invoice_subject: String::new(),
        address: None,
        contacts: vec![],
        tax_hundredths: 0,
        discount_hundredths: 0,
    };
    let err = store.put_customer(&cust).expect_err("should time out");
    assert!(
        matches!(err, tucano_time::store::StoreError::LockTimeout),
        "{err:?}"
    );

    // Release -> the same write now succeeds.
    held.unlock().unwrap();
    drop(held);
    store
        .put_customer(&cust)
        .expect("write succeeds once lock is free");
}

#[tokio::test]
async fn audit_log_records_login_failure() {
    let (client, _d) = app().await;
    // A failed login (as an anonymous caller).
    let _ = anon_req(
        &client,
        "POST",
        "/api/auth/login",
        Some(json!({"email":"ghost@test.local","password":"wrong"})),
    )
    .await;
    // Admin reads the audit log and sees the event.
    let (s, body) = json_req(&client, "GET", "/api/audit", None).await;
    assert_eq!(s, StatusCode::OK);
    let events = body["events"].as_array().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "login_failed" && e["subject"] == "ghost@test.local"),
        "audit missing login_failed: {events:?}"
    );
    // No password material is ever recorded.
    let raw = serde_json::to_string(&events).unwrap();
    assert!(!raw.contains("wrong"), "audit leaked a password");
}

#[tokio::test]
async fn logout_revokes_the_session() {
    let (client, _d) = app().await;
    // Session works.
    let (s1, _) = json_req(&client, "GET", "/api/auth/me", None).await;
    assert_eq!(s1, StatusCode::OK);
    // Log out (CSRF header is sent by raw()).
    let (s2, _, _) = raw(
        &client.router,
        "POST",
        "/api/auth/logout",
        None,
        Some(&client.cookie),
    )
    .await;
    assert_eq!(s2, StatusCode::NO_CONTENT);
    // The same cookie is now revoked.
    let (s3, _) = json_req(&client, "GET", "/api/auth/me", None).await;
    assert_eq!(
        s3,
        StatusCode::UNAUTHORIZED,
        "revoked session must not work"
    );
}

#[tokio::test]
async fn revocation_is_visible_immediately_across_instances() {
    // #254 (review B6 freshness): authenticate must re-read `revoked.json` on
    // every request — the read now runs on the blocking pool, but it must not
    // become a stale per-request cache. A logout performed by a *separate*
    // instance sharing the data dir has to take effect on the very next request.
    let (a, dir) = app().await;
    let (s1, _) = json_req(&a, "GET", "/api/auth/me", None).await;
    assert_eq!(s1, StatusCode::OK, "session works before any revocation");

    // Second instance (its own in-memory revocations) sharing the data dir.
    let b = app_locked(dir.path(), Arc::new(tucano_time::lock::NoLocks)).await;
    let (s2, _, _) = raw(&b.router, "POST", "/api/auth/logout", None, Some(&a.cookie)).await;
    assert_eq!(s2, StatusCode::NO_CONTENT);

    // Instance A sees the revocation next request, through the offloaded read.
    let (s3, _) = json_req(&a, "GET", "/api/auth/me", None).await;
    assert_eq!(
        s3,
        StatusCode::UNAUTHORIZED,
        "a revocation written elsewhere must be honoured immediately"
    );
}

#[tokio::test]
async fn invoice_includes_billable_expenses_and_locks_them() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    // 2h billable time @60 = 12000
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
    )
    .await;
    // billable expense 50.00 = 5000
    let (_, exp) = json_req(&app, "POST", "/api/expenses", Some(json!({"date":"2026-10-03","customer_id":cid,"project_code":"P1","amount_minor":5000,"currency":"EUR"}))).await;
    let xid = exp["id"].as_str().unwrap().to_string();

    // Invoice includes both -> 12000 + 5000 = 17000, two lines of different kinds.
    let (_, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(inv["total_minor"], 17000);
    let kinds: Vec<&str> = inv["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"time") && kinds.contains(&"expense"),
        "{kinds:?}"
    );

    // Issue, then the expense is locked from deletion.
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{}/issue", inv["id"].as_str().unwrap()),
        None,
    )
    .await;
    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None).await;
    assert_eq!(
        s_del,
        StatusCode::CONFLICT,
        "issued invoice locks its expense"
    );
}

#[tokio::test]
async fn invoice_can_exclude_expenses() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-03","customer_id":cid,"amount_minor":5000,"currency":"EUR"})),
    )
    .await;
    let (_, inv) = json_req(&app, "POST", "/api/invoices", Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07","include_expenses":false}))).await;
    assert_eq!(inv["total_minor"], 12000, "expenses excluded");
    assert!(
        inv["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["kind"] == "time")
    );
}

#[tokio::test]
async fn profitability_revenue_vs_cost() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    // Bob: cost rate 20/h.
    json_req(&app, "POST", "/api/users", Some(json!({"name":"Bob","email":"bob@t.local","password":"bobpass123","role":"member","cost_rate_minor":2000}))).await;
    let bob = login_cookie(&app.router, "bob@t.local", "bobpass123").await;
    // Bob logs 2h billable (revenue 2x60=12000, labour cost 2x20=4000).
    raw(
        &app.router,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
        Some(&bob),
    )
    .await;
    // billable expense 5000.
    json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-03","customer_id":cid,"amount_minor":5000,"currency":"EUR"})),
    )
    .await;
    // invoice (12000 + 5000 = 17000) and issue.
    let (_, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{}/issue", inv["id"].as_str().unwrap()),
        None,
    )
    .await;

    let (s, prof) = json_req(
        &app,
        "GET",
        "/api/reports/profitability?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let row = prof["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == "ACME")
        .expect("ACME row");
    assert_eq!(row["revenue_minor"], 17000);
    assert_eq!(row["cost_minor"], 9000); // 5000 expense + 4000 labour
    assert_eq!(row["margin_minor"], 8000);
}

#[tokio::test]
async fn invoice_report_and_export() {
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
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{}/issue", inv["id"].as_str().unwrap()),
        None,
    )
    .await;

    let (s, rep) = json_req(
        &app,
        "GET",
        "/api/invoices/report?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(rep["issued"], 1);
    assert_eq!(rep["revenue_by_currency"]["EUR"], 18000); // 3h * 60
    let row = &rep["rows"][0];
    assert_eq!(row["label"], "ACME");
    assert_eq!(row["revenue_minor"], 18000);

    let (s2, csv) = json_req(&app, "GET", "/api/invoices/export.csv", None).await;
    assert_eq!(s2, StatusCode::OK);
    let text = csv.as_str().unwrap();
    assert!(text.starts_with("number,customer,"), "csv header: {text}");
    assert!(text.contains("INV-0001") && text.contains("ACME"));
}

#[tokio::test]
async fn reports_partition_by_currency_and_never_sum_cents_across_it() {
    // #186: a customer may hold projects in different currencies (#11), so
    // every grouping level must produce per-currency rows — previously the
    // EUR and USD cents were added into a single number.
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P-EUR", json!({"rate_minor": 6000})).await;
    new_project(
        &app,
        &cid,
        "P-USD",
        json!({"rate_minor": 5000, "currency": "USD"}),
    )
    .await;
    for (proj, hours) in [("P-EUR", 2), ("P-USD", 3)] {
        let (s, e) = json_req(
            &app,
            "POST",
            "/api/entries",
            Some(entry_body(&cid, proj, json!(hours), "2026-10-02")),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{e}");
    }

    for group in ["customer", "project", "person", "week"] {
        let (s, rep) = json_req(
            &app,
            "GET",
            &format!("/api/reports/summary?from=2026-10-01&to=2026-10-07&group={group}"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let rows = rep["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{group}: one row per currency, got {rows:?}");
        let eur = rows
            .iter()
            .find(|r| r["currency"] == "EUR")
            .expect("EUR row");
        let usd = rows
            .iter()
            .find(|r| r["currency"] == "USD")
            .expect("USD row");
        assert_eq!(eur["amount_minor"], 12000, "{group}"); // 2h * 60.00
        assert_eq!(usd["amount_minor"], 15000, "{group}"); // 3h * 50.00
        // Week rows no longer smuggle the currency into the key.
        assert!(
            !rows
                .iter()
                .any(|r| r["key"].as_str().unwrap().contains('|')),
            "{group}: keys are labels, currency rides its own field"
        );
    }

    // Invoice report + profitability partition the same way.
    for (proj, amount) in [("P-EUR", 12000u64), ("P-USD", 15000)] {
        let (_, inv) = json_req(
            &app,
            "POST",
            "/api/invoices",
            Some(json!({
                "customer_id": cid,
                "from": "2026-10-01",
                "to": "2026-10-07",
                "project_codes": [proj],
                "include_expenses": false,
            })),
        )
        .await;
        assert_eq!(inv["total_minor"], amount);
        json_req(
            &app,
            "POST",
            &format!("/api/invoices/{}/issue", inv["id"].as_str().unwrap()),
            None,
        )
        .await;
    }
    let (_s, rep) = json_req(
        &app,
        "GET",
        "/api/invoices/report?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    let rows = rep["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "one row per (customer, currency): {rows:?}");
    assert_eq!(rep["revenue_by_currency"]["EUR"], 12000);
    assert_eq!(rep["revenue_by_currency"]["USD"], 15000);

    let (_s, prof) = json_req(
        &app,
        "GET",
        "/api/reports/profitability?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    let rows = prof["rows"].as_array().unwrap();
    let acme: Vec<&Value> = rows.iter().filter(|r| r["label"] == "ACME").collect();
    assert_eq!(acme.len(), 2, "profitability splits by currency: {rows:?}");
    let eur = acme.iter().find(|r| r["currency"] == "EUR").unwrap();
    let usd = acme.iter().find(|r| r["currency"] == "USD").unwrap();
    assert_eq!(eur["revenue_minor"], 12000);
    assert_eq!(usd["revenue_minor"], 15000);
}

#[tokio::test]
async fn reimbursement_claim_lifecycle_locks_expenses() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (_, e1) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"amount_minor":5000,"currency":"EUR"})),
    )
    .await;
    let (_, e2) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-03","customer_id":cid,"amount_minor":3000,"currency":"EUR"})),
    )
    .await;
    let ids = json!([e1["id"], e2["id"]]);

    // Create draft claim.
    let (s, claim) = json_req(
        &app,
        "POST",
        "/api/claims",
        Some(json!({"title":"Oct costs","expense_ids":ids})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{claim}");
    assert_eq!(claim["state"], "draft");
    assert_eq!(claim["total_minor"], 8000);
    let claim_id = claim["id"].as_str().unwrap().to_string();
    let xid = e1["id"].as_str().unwrap().to_string();

    // Draft does not lock; submit then it locks.
    assert_eq!(
        json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    // (that deleted e1; recreate for the lock test)
    let (_, e1b) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"amount_minor":5000,"currency":"EUR"})),
    )
    .await;
    let xid = e1b["id"].as_str().unwrap().to_string();
    let (_, claim2) = json_req(
        &app,
        "POST",
        "/api/claims",
        Some(json!({"title":"Oct costs","expense_ids":[e1b["id"], e2["id"]]})),
    )
    .await;
    let cid2 = claim2["id"].as_str().unwrap().to_string();
    json_req(&app, "POST", &format!("/api/claims/{cid2}/submit"), None).await;
    assert_eq!(
        json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None)
            .await
            .0,
        StatusCode::CONFLICT,
        "submitted claim locks expense"
    );

    // Admin approves.
    let (sd, decided) = json_req(
        &app,
        "POST",
        &format!("/api/claims/{cid2}/decision"),
        Some(json!({"decision":"approve"})),
    )
    .await;
    assert_eq!(sd, StatusCode::OK, "{decided}");
    assert_eq!(decided["state"], "approved");
    let _ = claim_id;
}

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
        payments: std::sync::Arc::new(tucano_time::payments::PaymentRegistry::default()),
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
        "/api/admin/secrets/stripe.secret_key",
        Some(json!({"value":"dummy-secret-value-1234"})),
        Some(&admin),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // list shows masked hint, never the value
    let (s2, list, _) = raw(&router, "GET", "/api/admin/secrets", None, Some(&admin)).await;
    assert_eq!(s2, StatusCode::OK);
    let text = serde_json::to_string(&list).unwrap();
    assert!(text.contains("stripe.secret_key"), "{text}");
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
        "/api/admin/secrets/stripe.secret_key",
        None,
        Some(&admin),
    )
    .await;
    assert_eq!(s4, StatusCode::NO_CONTENT);
}

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
async fn notifications_endpoint_empty_then_read() {
    let (app, _d) = app().await;
    let (s, body) = json_req(&app, "GET", "/api/notifications", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["notifications"].as_array().unwrap().len(), 0);
    assert_eq!(body["unread"], 0);
    let (s2, _) = json_req(&app, "POST", "/api/notifications/read", None).await;
    assert_eq!(s2, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn notifications_hide_stale_and_resolved_no_time_reminders() {
    let (app, dir) = app().await;
    let store = Store::open(dir.path().join("data")).unwrap();
    let user = store
        .list_users()
        .unwrap()
        .into_iter()
        .find(|user| user.email == ADMIN_EMAIL)
        .unwrap();
    let now = chrono::Utc::now();
    for (kind, created_at) in [
        ("no_time_today", now - chrono::Duration::days(1)),
        ("no_time_today", now),
        ("submit_timesheet", now),
    ] {
        store
            .push_notification(
                user.id,
                &tucano_time::domain::Notification {
                    id: uuid::Uuid::new_v4(),
                    kind: kind.to_string(),
                    title: kind.to_string(),
                    body: String::new(),
                    created_at,
                    read: false,
                },
            )
            .unwrap();
    }
    let (status, before) = json_req(&app, "GET", "/api/notifications", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(before["notifications"].as_array().unwrap().len(), 2);
    assert_eq!(before["unread"], 2);
    let customer = new_customer(&app, "Reminder test", "EUR", 6000).await;
    let cid = customer["id"].as_str().unwrap();
    new_project(&app, cid, "REMINDER", json!({})).await;
    let (created, _) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({
            "date": now.date_naive().to_string(),
            "customer_id": cid,
            "project_code": "REMINDER",
            "hours": 1,
        })),
    )
    .await;
    assert_eq!(created, StatusCode::CREATED);
    let (_, after) = json_req(&app, "GET", "/api/notifications", None).await;
    let notifications = after["notifications"].as_array().unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0]["kind"], "submit_timesheet");
    assert_eq!(after["unread"], 1);
}

#[tokio::test]
async fn schedule_crud_admin_only() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (s, sched) = json_req(&app, "POST", "/api/schedules", Some(json!({"customer_id":cid,"cadence":"monthly","mode":"retainer","retainer_amount_minor":150000,"currency":"EUR"}))).await;
    assert_eq!(s, StatusCode::CREATED, "{sched}");
    let sid = sched["id"].as_str().unwrap().to_string();
    let (sl, list) = json_req(&app, "GET", "/api/schedules", None).await;
    assert_eq!(sl, StatusCode::OK);
    assert_eq!(list["schedules"].as_array().unwrap().len(), 1);
    // Retainer with 0 amount rejected.
    let (sz, _) = json_req(&app, "POST", "/api/schedules", Some(json!({"customer_id":cid,"cadence":"monthly","mode":"retainer","retainer_amount_minor":0,"currency":"EUR"}))).await;
    assert_eq!(sz, StatusCode::UNPROCESSABLE_ENTITY);
    // Member forbidden (admin tier).
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"N","email":"n@t.local","password":"memberpass1","role":"member"})),
    )
    .await;
    let member = login_cookie(&app.router, "n@t.local", "memberpass1").await;
    let (sm, _, _) = raw(&app.router, "GET", "/api/schedules", None, Some(&member)).await;
    assert_eq!(sm, StatusCode::FORBIDDEN);
    // Delete.
    assert_eq!(
        json_req(&app, "DELETE", &format!("/api/schedules/{sid}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn budget_report_shows_burn() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(
        &app,
        cid,
        "P1",
        json!({"budget_hours": 1000, "budget_amount_minor": 60000}),
    )
    .await;
    let (s, _) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(6), "2026-10-02")),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (sb, body) = json_req(&app, "GET", "/api/reports/budgets", None).await;
    assert_eq!(sb, StatusCode::OK);
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["project"], "P1");
    assert_eq!(rows[0]["burn_hours"], 6.0);
    assert_eq!(rows[0]["hours_pct"], 60.0);
    assert_eq!(rows[0]["over"], false);
    assert_eq!(rows[0]["burn_amount_minor"], 36000);
    assert_eq!(rows[0]["amount_pct"], 60.0);
}

#[tokio::test]
async fn invoice_email_sends_to_customer() {
    let (app, _d) = app().await;
    // Customer with a billing email (#35).
    let (sc, cust) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"ACME","currency":"EUR","default_rate_minor":6000,"email":"billing@acme.test"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cust}");
    assert_eq!(cust["email"], "billing@acme.test");
    let cid = cust["id"].as_str().unwrap();
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
    let iid = inv["id"].as_str().unwrap();

    // Draft invoices must be issued before emailing.
    let (sd, _) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(sd, StatusCode::CONFLICT);

    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    assert_eq!(body["sent_to"], "billing@acme.test");
    // #130: the result names the transport; the test recorder never claims smtp.
    assert_eq!(body["transport"], "recording");
    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].text.contains("3h") || msgs[0].text.contains("180.00"),
        "{}",
        msgs[0].text
    );
    assert!(msgs[0].subject.contains(inv["number"].as_str().unwrap()));
}

#[tokio::test]
async fn invoice_email_without_customer_email_is_422() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "NOEMAIL", "EUR", 6000).await; // no email field
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
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(app.email.messages().len(), 0);
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

/// Stub transport for accounting tests: returns deterministic ids, optionally
/// fails the first N posts to exercise the recorded-failure path.
struct StubTransport {
    posts: std::sync::Mutex<Vec<(String, Value)>>,
    fail_first: usize,
}

impl tucano_time::accounting::Transport for StubTransport {
    fn post(&self, path: &str, body: Value) -> Result<String, tucano_time::accounting::SyncError> {
        let mut posts = self.posts.lock().unwrap();
        posts.push((path.to_string(), body));
        if posts.len() <= self.fail_first {
            return Err(tucano_time::accounting::SyncError::Transport(
                "stub offline".into(),
            ));
        }
        Ok(format!("remote-{path}-{}", posts.len()))
    }
}

#[tokio::test]
async fn accounting_sync_idempotent_with_visible_status() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let t = Arc::new(StubTransport {
        posts: std::sync::Mutex::new(vec![]),
        fail_first: 0,
    });
    let qbo: Arc<dyn tucano_time::accounting::AccountingSync> =
        Arc::new(tucano_time::accounting::QboProvider::new(t.clone()));
    let registry = tucano_time::accounting::AccountingRegistry::new(vec![qbo]);
    let app = Client::with_accounting(store, registry).await;

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
    let iid = inv["id"].as_str().unwrap();

    // Draft -> 409.
    let (sd, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(sd, StatusCode::CONFLICT);

    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;

    // Unknown provider -> 400.
    let (su, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"sage"})),
    )
    .await;
    assert_eq!(su, StatusCode::BAD_REQUEST);

    // Sync -> synced record with a remote id; the doc went to "invoice".
    let (ss, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(ss, StatusCode::OK, "{body}");
    assert_eq!(body["record"]["status"], "synced");
    assert!(
        body["record"]["remote_id"]
            .as_str()
            .unwrap()
            .starts_with("remote-invoice")
    );

    // Idempotent: a second sync is a no-op (no duplicate post).
    let posts_before = t.posts.lock().unwrap().len();
    let (s2, b2) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["noop"], true);
    assert_eq!(t.posts.lock().unwrap().len(), posts_before);

    // Visible status lists the record + enabled providers.
    let (sv, status) = json_req(&app, "GET", "/api/sync/accounting", None).await;
    assert_eq!(sv, StatusCode::OK);
    assert_eq!(status["providers"].as_array().unwrap(), &vec![json!("qbo")]);
    assert_eq!(status["records"].as_array().unwrap().len(), 1);

    // Paying then syncing pushes invoice AND payment documents.
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference":"bank-transfer"})),
    )
    .await;
    let (_s3, b3) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    // Paid after sync -> the next sync pushes the PAYMENT against the remote
    // invoice (not a no-op), then later syncs are no-ops again.
    assert_eq!(b3["record"]["kind"], "payment", "{b3}");
    assert_eq!(b3["record"]["status"], "synced", "{b3}");
    {
        let posts = t.posts.lock().unwrap();
        assert_eq!(posts[posts.len() - 1].0, "payment");
    }
    let (_s4, b4) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(b4["noop"], true, "{b4}");
}

#[tokio::test]
async fn accounting_sync_failure_is_recorded_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let t = Arc::new(StubTransport {
        posts: std::sync::Mutex::new(vec![]),
        fail_first: 1, // first post fails, retry succeeds
    });
    let qbo: Arc<dyn tucano_time::accounting::AccountingSync> =
        Arc::new(tucano_time::accounting::QboProvider::new(t.clone()));
    let app = Client::with_accounting(
        store,
        tucano_time::accounting::AccountingRegistry::new(vec![qbo]),
    )
    .await;

    let c = new_customer(&app, "GLOBEX", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
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
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;

    // First sync fails at the transport -> 200 with a recorded failure.
    let (sf, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(sf, StatusCode::OK, "{body}");
    assert_eq!(body["failed"], true);
    assert_eq!(body["record"]["status"], "failed");
    // #190: the persisted/serialised error is a stable marker; the raw
    // transport text ("stub offline") stays in the server log only.
    assert_eq!(body["record"]["error"], "sync failed (see server log)");
    let (_sv, st) = json_req(&app, "GET", "/api/sync/accounting", None).await;
    assert!(
        !st.to_string().contains("stub offline"),
        "sync_status must not echo provider detail: {st}"
    );

    // Retry (second post succeeds) -> synced. The failed record is not
    // short-circuited (only synced records are).
    let (sr, body2) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(sr, StatusCode::OK);
    assert_eq!(body2["record"]["status"], "synced", "{body2}");
    assert!(
        body2["record"]["remote_id"]
            .as_str()
            .unwrap()
            .starts_with("remote-invoice")
    );
}

#[tokio::test]
async fn sso_jit_provisions_and_logs_in() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let idp =
        tucano_time::sso::SignedTokenIdp::new("okta-test", "stub-secret", vec!["acme.test".into()]);
    let registry = Arc::new(tucano_time::sso::SsoRegistry::new(vec![Arc::new(idp)]));
    let state = AppState::with_session(
        store,
        Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        )),
    )
    .with_sso(registry);
    let router = tucano_time::build_router(state);

    // Local admin exists FIRST — SSO runs alongside local login (#32 DoD).
    let (sa, _, _) = raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    assert_eq!(sa, StatusCode::CREATED);

    // Providers are advertised pre-session.
    let (s, body, _) = raw(&router, "GET", "/api/auth/sso/providers", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        body["providers"].as_array().unwrap(),
        &vec![json!("okta-test")]
    );

    let idp =
        tucano_time::sso::SignedTokenIdp::new("okta-test", "stub-secret", vec!["acme.test".into()]);

    // A valid signed assertion JIT-creates the user (member) and logs in.
    let claims = tucano_time::sso::TokenClaims {
        sub: "u1".into(),
        email: "Newbie@acme.test".into(),
        name: "New Bie".into(),
        groups: vec![],
        exp: chrono::Utc::now().timestamp() + 300,
    };
    let token = idp.issue(&claims);
    let (s1, user, _) = raw(
        &router,
        "POST",
        "/api/auth/sso/assertion",
        Some(json!({"provider":"okta-test","payload":token,"signature":""})),
        None,
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "{user}");
    assert_eq!(user["email"], "newbie@acme.test");
    assert_eq!(user["role"], "member");

    // The session cookie works like a local one.
    let cookie = {
        let (_s, _b, c) = raw(
            &router,
            "POST",
            "/api/auth/sso/assertion",
            Some(json!({"provider":"okta-test","payload":token,"signature":""})),
            None,
        )
        .await;
        c.unwrap_or_default()
    };
    let (sm, me, _) = raw(&router, "GET", "/api/auth/me", None, Some(&cookie)).await;
    assert_eq!(sm, StatusCode::OK, "{me}");
    assert_eq!(me["name"], "New Bie");

    // Bad signature -> 401, and no user created for it.
    let (sb, _, _) = raw(
        &router,
        "POST",
        "/api/auth/sso/assertion",
        Some(json!({"provider":"okta-test","payload":"aGVhZGVy.cGF5bG9hZC5mb3JnZWQ.bad","signature":""})),
        None,
    )
    .await;
    assert_eq!(sb, StatusCode::UNAUTHORIZED);

    // Local login still works alongside SSO.
    let (sl, _, _) = raw(
        &router,
        "POST",
        "/api/auth/login",
        Some(json!({"email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    assert_eq!(sl, StatusCode::OK);
}

#[tokio::test]
async fn sso_empty_allowed_domains_denies_and_provisions_nothing() {
    // #260: SSO with an empty allow-list must fail closed — the assertion is
    // rejected and no account is JIT-provisioned (the old allow-all is gone).
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let idp = tucano_time::sso::SignedTokenIdp::new("okta-test", "stub-secret", vec![]);
    let registry = Arc::new(tucano_time::sso::SsoRegistry::new(vec![Arc::new(idp)]));
    let state = AppState::with_session(
        store,
        Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        )),
    )
    .with_sso(registry);
    let router = tucano_time::build_router(state);

    let (sa, _, _) = raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    assert_eq!(sa, StatusCode::CREATED);
    let admin_cookie = login_cookie(&router, ADMIN_EMAIL, ADMIN_PW).await;

    // A correctly-signed assertion but an empty allow-list: rejected, never
    // provisioned.
    let idp = tucano_time::sso::SignedTokenIdp::new("okta-test", "stub-secret", vec![]);
    let claims = tucano_time::sso::TokenClaims {
        sub: "u1".into(),
        email: "Intruder@acme.test".into(),
        name: "Intruder".into(),
        groups: vec![],
        exp: chrono::Utc::now().timestamp() + 300,
    };
    let token = idp.issue(&claims);
    let (s1, body, _) = raw(
        &router,
        "POST",
        "/api/auth/sso/assertion",
        Some(json!({"provider":"okta-test","payload":token,"signature":""})),
        None,
    )
    .await;
    assert_eq!(s1, StatusCode::BAD_REQUEST, "{body}");

    // Only the bootstrap admin exists: nothing was provisioned for the domain.
    let (su, users, _) = raw(&router, "GET", "/api/users", None, Some(&admin_cookie)).await;
    assert_eq!(su, StatusCode::OK, "{users}");
    assert_eq!(users["users"].as_array().map(Vec::len), Some(1), "{users}");
}

#[tokio::test]
async fn sso_unknown_provider_is_400() {
    let (app, _d) = app().await;
    let (s, _) = json_req(
        &app,
        "POST",
        "/api/auth/sso/assertion",
        Some(json!({"provider":"nope","payload":"x","signature":""})),
    )
    .await;
    // app() client is authenticated; endpoint is public but provider unknown.
    assert_eq!(s, StatusCode::BAD_REQUEST);
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
async fn management_reports_are_admin_only() {
    // Review A4: cost rates / budgets must not be visible to members.
    let (app, _d) = app().await;
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"M","email":"repmember@t.local","password":"memberpass1","role":"member"})),
    )
    .await;
    let member = login_cookie(&app.router, "repmember@t.local", "memberpass1").await;
    for path in ["/api/reports/profitability", "/api/reports/budgets"] {
        let (s, _, _) = raw(&app.router, "GET", path, None, Some(&member)).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path} must be admin-only");
    }
    // Members keep their own summary/export.
    let (s, _, _) = raw(
        &app.router,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07",
        None,
        Some(&member),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn sso_assertion_is_csrf_exempt_pre_session() {
    // Review A6: a real IdP POST-binding cannot send X-CSRF-Protection; the
    // route must reach the handler (400 unknown provider), not 403 csrf.
    let (app, _d) = app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/auth/sso/assertion")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"provider":"nope","payload":"x"}).to_string(),
        ))
        .unwrap();
    let res = app.router.clone().oneshot(req).await.expect("oneshot");
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn demo_flags_cannot_be_persisted_via_config() {
    // Review A3: webhook-bypass flags are env-only, never config.json.
    let (app, _d) = app().await;
    for key in ["stripe_demo", "paypal_demo"] {
        let (s, body) = json_req(&app, "PUT", "/api/admin/config", Some(json!({key: true}))).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{key}: {body}");
    }
}

// ------------------------------------------------------------------ #113 ---

#[tokio::test]
async fn invoice_pdf_archived_at_issue_and_downloadable() {
    let (app, d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;

    // The invoice JSON carries the hint.
    let hint = &issued["pdf"];
    assert_eq!(
        hint["filename"],
        format!("{}.pdf", issued["number"].as_str().unwrap())
    );
    assert_eq!(hint["sha256"].as_str().unwrap().len(), 64);
    assert!(hint["bytes"].as_u64().unwrap() > 100);
    assert!(hint["archived_at"].is_string());

    // The file is on disk next to the document, atomically written at issue.
    let pdf_path = d
        .path()
        .join("data")
        .join("invoices")
        .join(format!("{iid}.pdf"));
    assert!(pdf_path.exists(), "archive written at issue");
    let on_disk = std::fs::read(&pdf_path).unwrap();

    // Download contract: admin 200 + binary headers, ETag from the sha256.
    let (status, headers, body) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/pdf");
    let disp = headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        disp.contains(format!("filename=\"{}.pdf\"", issued["number"].as_str().unwrap()).as_str()),
        "{disp}"
    );
    let etag = headers[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(etag, format!("\"{}\"", hint["sha256"].as_str().unwrap()));
    assert!(body.starts_with(b"%PDF-1.4"), "bytes start with %PDF");
    assert_eq!(body, on_disk, "download is the archived bytes");

    // Independent sha: body hashes to the advertised ETag.
    use sha2::{Digest, Sha256};
    let sha = Sha256::digest(&body);
    assert_eq!(
        sha.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        hint["sha256"].as_str().unwrap()
    );

    // Re-download: same bytes (immutable archive, stable ETag).
    let (s2, h2, body2) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(h2[header::ETAG], headers[header::ETAG]);
    assert_eq!(body, body2);
}

#[tokio::test]
async fn invoice_pdf_legacy_invoice_resolves_byte_stably() {
    let (app, d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    let (_s, headers, first) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert!(headers[header::ETAG].to_str().unwrap().starts_with('"'));

    // Simulate a legacy archive: the file vanishes while the (immutable)
    // invoice document stays. The first read must re-render **identical**
    // bytes and re-persist the archive.
    std::fs::remove_file(
        d.path()
            .join("data")
            .join("invoices")
            .join(format!("{iid}.pdf")),
    )
    .unwrap();
    let (_s2, _h2, second) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(first, second, "legacy re-render is byte-stable");
    let path = d
        .path()
        .join("data")
        .join("invoices")
        .join(format!("{iid}.pdf"));
    assert!(path.exists(), "archive re-persisted on first read");
    assert_eq!(std::fs::read(&path).unwrap(), second);
    // The hint's sha still matches (recomputed, but from identical bytes).
    let (_s3, fresh) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    use sha2::{Digest, Sha256};
    assert_eq!(
        fresh["pdf"]["sha256"].as_str().unwrap(),
        Sha256::digest(&second)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
}

#[tokio::test]
async fn invoice_pdf_rejects_draft_and_unknown() {
    let (app, _d) = app().await;
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
    let iid = inv["id"].as_str().unwrap();

    // Draft: 409, nothing persisted.
    let (status, _h, body) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(String::from_utf8_lossy(&body).contains("issue the invoice"));

    // Unknown id: 404, never a PDF-shaped answer.
    let (s404, _, _) = raw_req(
        &app,
        "GET",
        "/api/invoices/00000000-0000-0000-0000-000000000000/pdf",
    )
    .await;
    assert_eq!(s404, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn member_cannot_download_invoice_pdf() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
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
    let (s, _, _) = raw(
        &app.router,
        "GET",
        &format!("/api/invoices/{iid}/pdf"),
        None,
        Some(&cookie),
    )
    .await;
    // Invoices are the admin tier (#51): the whole surface refuses a member.
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn invoice_email_carries_the_archived_pdf_attachment() {
    let (app, _d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;
    let (_s, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;

    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    let (name, bytes) = msgs[0]
        .attachment
        .as_ref()
        .expect("the invoice email carries the PDF (#113)");
    assert_eq!(*name, format!("{}.pdf", issued["number"].as_str().unwrap()));
    assert_eq!(*bytes, pdf, "the attachment is the archived document");
    assert!(
        msgs[0].text.contains("PDF document is attached"),
        "{}",
        msgs[0].text
    );

    // The MIME builder labels the part as a PDF.
    let mime = tucano_time::email::build_mime(&msgs[0]);
    assert!(mime.contains("Content-Type: application/pdf"));
    assert!(mime.contains("base64"));
}

// ------------------------------------------------------------------ #112 ---

#[tokio::test]
async fn email_copy_sends_pdf_to_third_party_with_audit_trail() {
    let (app, _d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;
    let (_s, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;

    let (status, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "accountant@firm.co", "note": "for the October books"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sent_to"], "accountant@firm.co");
    assert_eq!(body["transport"], "recording");

    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    assert_eq!(m.to, "accountant@firm.co");
    assert!(
        m.subject.contains(issued["number"].as_str().unwrap()),
        "{}",
        m.subject
    );
    // The cover names the requester (session user "Admin") and the note.
    assert!(m.text.contains("Admin"), "{}", m.text);
    assert!(m.text.contains("for the October books"), "{}", m.text);
    let (name, bytes) = m.attachment.as_ref().expect("PDF copy attached");
    assert_eq!(*name, format!("{}.pdf", issued["number"].as_str().unwrap()));
    assert_eq!(*bytes, pdf, "the attached copy is the archived document");

    // Audit records recipient + invoice, never the bytes (#52).
    let (_s, audit) = json_req(&app, "GET", "/api/audit", None).await;
    let hit = audit["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "invoice_email_copy");
    assert!(hit.is_some(), "audit event recorded: {audit}");
    let subject = hit.unwrap()["subject"].as_str().unwrap();
    assert!(
        subject.contains(iid.as_str()) && subject.contains("accountant@firm.co"),
        "{subject}"
    );
    assert!(!subject.contains("%PDF"));
}

#[tokio::test]
async fn email_copy_invalid_recipient_is_422_without_sending() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    let (status, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "not-an-email"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["fields"][0]["field"], "to");
    // Nothing sent, nothing persisted beyond the earlier issue.
    assert_eq!(app.email.messages().len(), 0);
    let (_s, audit) = json_req(&app, "GET", "/api/audit", None).await;
    assert!(
        !audit["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "invoice_email_copy"),
        "rejection without side effects"
    );
}

#[tokio::test]
async fn email_copy_draft_is_409_and_shapes_are_rejected() {
    let (app, _d) = app().await;
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
    let iid = inv["id"].as_str().unwrap();

    // Draft: 409, no send.
    let (status, _b) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "x@y.co"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(app.email.messages().len(), 0);

    // Unknown field (deny_unknown_fields) and oversized note: 422.
    let (s1, _b1) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "x@y.co", "bcc": "sneaky@evil.co"})),
    )
    .await;
    assert_eq!(s1, StatusCode::UNPROCESSABLE_ENTITY);
    let (s2, _b2) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "x@y.co", "note": "n".repeat(2001)})),
    )
    .await;
    assert_eq!(s2, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(app.email.messages().len(), 0);

    // Unknown invoice: 404.
    let (s3, _) = json_req(
        &app,
        "POST",
        "/api/invoices/00000000-0000-0000-0000-000000000000/email-copy",
        Some(json!({"to": "x@y.co"})),
    )
    .await;
    assert_eq!(s3, StatusCode::NOT_FOUND);

    // Member: admin tier, 403.
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
    let (s4, _, _) = raw(
        &app.router,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "x@y.co"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(s4, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn email_copy_accepts_paid_invoices() {
    // Draft is the only refused state: a settled invoice still goes to the
    // accountant (matches the #113 download rule).
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "bank:42"})),
    )
    .await;
    let (status, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "books@firm.co"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].attachment.is_some());
}

// ------------------------------------------------------------------ #116 ---

async fn new_customer_ex(app: &Client, body: Value) -> (StatusCode, Value) {
    json_req(app, "POST", "/api/customers", Some(body)).await
}

/// Draft an invoice for a customer (billing email set), returning (customer,
/// draft invoice). `extra` merges into the customer creation body (#116
/// fields).
async fn seed_customer_invoice(app: &Client, name: &str, extra: Value) -> (Value, Value) {
    let mut body = json!({"name": name, "currency": "EUR", "default_rate_minor": 6000, "email": "billing@t116.test"});
    if let (Some(obj), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            obj.insert(k.clone(), v.clone());
        }
    }
    let (s, c) = new_customer_ex(app, body).await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let cid = c["id"].as_str().unwrap();
    new_project(app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_s2, inv) = json_req(
        app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    (c, inv)
}

#[tokio::test]
async fn payment_terms_decide_due_date_at_issue() {
    let today = chrono::Utc::now().date_naive();
    let cases = [
        (json!({"kind":"upon_receipt"}), today),
        (json!({"kind":"net_15"}), today + chrono::Duration::days(15)),
        (json!({"kind":"net_30"}), today + chrono::Duration::days(30)),
        (json!({"kind":"net_45"}), today + chrono::Duration::days(45)),
        (
            json!({"kind":"custom","days":21}),
            today + chrono::Duration::days(21),
        ),
    ];
    for (i, (terms, expected)) in cases.iter().enumerate() {
        let (app, _d) = app().await;
        let (_c, inv) =
            seed_customer_invoice(&app, &format!("TERMS{i}"), json!({"payment_terms": terms}))
                .await;
        let iid = inv["id"].as_str().unwrap();
        let (s, issued) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
        assert_eq!(s, StatusCode::OK, "{issued}");
        assert_eq!(
            issued["due_date"].as_str().unwrap(),
            expected.to_string(),
            "terms {terms}"
        );
    }
}

#[tokio::test]
async fn legacy_net14_fallback_is_untouched_and_org_default_applies() {
    // No terms anywhere: the legacy net-14 from period_to (2026-10-07) stays.
    let (app, _d) = app().await;
    let (_c, inv) = seed_customer_invoice(&app, "LEGACY", json!({})).await;
    let iid = inv["id"].as_str().unwrap();
    let (_, issued) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(
        issued["due_date"], "2026-10-21",
        "legacy fallback unchanged"
    );

    // Org default terms now apply to a terms-less customer.
    let (s, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"","footer":"","payment_terms":{"kind":"net_20"}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_c2, inv2) = seed_customer_invoice(&app, "NEXT", json!({})).await;
    let iid2 = inv2["id"].as_str().unwrap();
    let (_, issued2) = json_req(&app, "POST", &format!("/api/invoices/{iid2}/issue"), None).await;
    let today = chrono::Utc::now().date_naive();
    assert_eq!(
        issued2["due_date"].as_str().unwrap(),
        (today + chrono::Duration::days(20)).to_string()
    );

    // Customer terms beat the org default.
    let (_c3, inv3) = seed_customer_invoice(
        &app,
        "PAYER",
        json!({"payment_terms": {"kind": "upon_receipt"}}),
    )
    .await;
    let iid3 = inv3["id"].as_str().unwrap();
    let (_, issued3) = json_req(&app, "POST", &format!("/api/invoices/{iid3}/issue"), None).await;
    assert_eq!(issued3["due_date"].as_str().unwrap(), today.to_string());
}

#[tokio::test]
async fn template_save_validates_and_rejects_without_persisting() {
    let (app, d) = app().await;
    // Unknown %token% in the body: 422 naming the field; nothing persisted.
    let (s, body) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"Inv %invoice_number%","body":"pay %bogus% now","footer":""})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["fields"][0]["field"], "body");
    assert!(
        body["error"]["fields"][0]["message"]
            .as_str()
            .unwrap()
            .contains("%bogus%")
    );
    assert!(
        !d.path().join("data").join("invoice_template.json").exists(),
        "rejection persists nothing"
    );

    // Case-sensitive: %Total% is unknown.
    let (s2, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"%Total%","body":"","footer":""})),
    )
    .await;
    assert_eq!(s2, StatusCode::UNPROCESSABLE_ENTITY);

    // Malformed payment terms.
    let (s3, b3) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"payment_terms": {"kind": "custom"}})),
    )
    .await;
    assert_eq!(s3, StatusCode::UNPROCESSABLE_ENTITY, "{b3}");
    let (s4, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"payment_terms": {"kind": "net_30", "days": 30}})),
    )
    .await;
    assert_eq!(s4, StatusCode::UNPROCESSABLE_ENTITY);
    let (s5, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"payment_terms": {"kind": "custom", "days": 0}})),
    )
    .await;
    assert_eq!(s5, StatusCode::UNPROCESSABLE_ENTITY);

    // Unknown field rejected pre-write (#deny_unknown_fields contract).
    let (s6, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject": "s", "evil": true})),
    )
    .await;
    assert_eq!(s6, StatusCode::UNPROCESSABLE_ENTITY);

    // A valid template round-trips and lands on disk atomically.
    let (s7, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"Invoice %invoice_number% for %customer_name%","body":"# Hello\n\nthanks","footer":"Bank: IBAN","payment_terms":{"kind":"net_30"}})),
    )
    .await;
    assert_eq!(s7, StatusCode::OK, "{saved}");
    assert_eq!(saved["body"], "# Hello\n\nthanks");

    // GET returns the template plus the variable cheatsheet.
    let (_s8, got) = json_req(&app, "GET", "/api/admin/invoice-template", None).await;
    assert_eq!(
        got["template"]["subject"],
        "Invoice %invoice_number% for %customer_name%"
    );
    let vars = got["variables"].as_array().unwrap();
    assert!(vars.iter().any(|v| v["name"] == "%invoice_issue_month%"));
    assert!(vars.iter().any(|v| v["name"] == "%line_count%"));

    // Member: admin tier.
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
    let (s9, _, _) = raw(
        &app.router,
        "GET",
        "/api/admin/invoice-template",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(s9, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn document_endpoint_renders_live_and_escapes() {
    let (app, _d) = app().await;
    let (_c, inv) = seed_customer_invoice(
        &app,
        "DOCS",
        json!({"invoice_notes": "Please pay by <script>alert(1)</script> transfer"}),
    )
    .await;
    json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"October: %invoice_number%","body":"Dear %customer_name%,\n\n- consulting\n- support","footer":"Wire to IBAN XX","payment_terms":null})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    let (s, doc) = json_req(&app, "GET", &format!("/api/invoices/{iid}/document"), None).await;
    assert_eq!(s, StatusCode::OK, "{doc}");
    assert_eq!(
        doc["subject"],
        format!("October: {}", inv["number"].as_str().unwrap())
    );
    let html = doc["html"].as_str().unwrap();
    assert!(html.contains("<li>consulting</li>"), "{html}");
    assert!(html.contains("Wire to IBAN XX"), "{html}");
    // Customer notes are appended after the org footer.
    assert!(html.contains("by &lt;script&gt;"), "{html}");
    assert!(!html.contains("<script>"), "escaping is non-negotiable");

    // Unknown id: 404.
    let (s404, _) = json_req(
        &app,
        "GET",
        "/api/invoices/00000000-0000-0000-0000-000000000000/document",
        None,
    )
    .await;
    assert_eq!(s404, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn email_uses_resolved_subject_and_html_body() {
    let (app, _d) = app().await;
    let (_c, inv) = seed_customer_invoice(
        &app,
        "MAILER",
        json!({"invoice_subject": "%invoice_number% for %customer_name% — %invoice_issue_month%"}),
    )
    .await;
    json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"WRONG %invoice_number%","body":"Org footer body","footer":"","payment_terms":null})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (s, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    // Customer subject override beats the org template, variables resolved.
    assert_eq!(
        msgs[0].subject,
        format!("{} for MAILER — October", inv["number"].as_str().unwrap())
    );
    assert!(
        msgs[0].text.contains("Please find invoice"),
        "legacy text part stays"
    );
    let html = msgs[0].html.as_ref().expect("template body -> html part");
    assert!(html.contains("Org footer body"), "{html}");
    // The PDF attachment still rides along.
    assert!(msgs[0].attachment.is_some());
}

#[tokio::test]
async fn pdf_archive_applies_template_on_both_issue_and_lazy_paths() {
    let (app, d) = app().await;
    json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"THANK-YOU NOTE text","footer":"","payment_terms":null})),
    )
    .await;
    let (_c, inv) = seed_customer_invoice(&app, "SEAM", json!({})).await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (_s, _h, at_issue) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;

    // Drop the archive: the lazy path re-renders from the *same* content
    // source and must produce identical bytes (#113 determinism x #116 seam).
    std::fs::remove_file(
        d.path()
            .join("data")
            .join("invoices")
            .join(format!("{iid}.pdf")),
    )
    .unwrap();
    let (_s2, _h2, at_lazy) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(
        at_issue, at_lazy,
        "issue and lazy paths share the content source"
    );

    // And template content actually flows into the document: a body-less
    // template renders different bytes for the same invoice state.
    json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"","footer":"","payment_terms":null})),
    )
    .await;
    std::fs::remove_file(
        d.path()
            .join("data")
            .join("invoices")
            .join(format!("{iid}.pdf")),
    )
    .unwrap();
    let (_s3, _h3, at_no_template) =
        raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_ne!(
        at_issue, at_no_template,
        "the body text changed the document"
    );
}

#[tokio::test]
async fn customer_invoice_fields_round_trip_and_validate() {
    let (app, _d) = app().await;
    // Unknown token in the subject: rejected before persistence.
    let (s, _) = new_customer_ex(
        &app,
        json!({"name":"BAD","currency":"EUR","default_rate_minor":1,"invoice_subject":"%nope%"}),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (_s2, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert!(list["customers"].as_array().unwrap().is_empty());

    // Invalid terms shapes.
    for bad in [
        json!({"kind":"custom","days":400}),
        json!({"kind":"net_20","days":20}),
    ] {
        let (sb, _bb) = new_customer_ex(
            &app,
            json!({"name":"BAD","currency":"EUR","default_rate_minor":1,"payment_terms": bad}),
        )
        .await;
        assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }

    // Valid shape round-trips through create and update.
    let (sc, c) = new_customer_ex(
        &app,
        json!({"name":"FULL","currency":"EUR","default_rate_minor":1,
               "payment_terms":{"kind":"custom","days":21},
               "invoice_notes":"net 21, thanks","invoice_subject":"Inv %invoice_number%"}),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{c}");
    assert_eq!(c["payment_terms"]["kind"], "custom");
    assert_eq!(c["payment_terms"]["days"], 21);
    let cid = c["id"].as_str().unwrap();
    let (_su, u) = json_req(
        &app,
        "PUT",
        &format!("/api/customers/{cid}"),
        Some(
            json!({"name":"FULL","currency":"EUR","default_rate_minor":1,
                    "payment_terms":{"kind":"upon_receipt"},
                    "invoice_notes":"","invoice_subject":""}),
        ),
    )
    .await;
    assert_eq!(u["payment_terms"]["kind"], "upon_receipt");
    assert!(
        u["payment_terms"].get("days").is_none() || u["payment_terms"]["days"].is_null(),
        "skipped when absent: {}",
        u["payment_terms"]
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
async fn invoice_report_gains_paid_and_balance_columns() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "half", "amount_minor": 8000})),
    )
    .await;
    let (s, rep) = json_req(
        &app,
        "GET",
        "/api/invoices/report?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rep}");
    assert_eq!(rep["partly_paid"], 1);
    let row = rep["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["revenue_minor"] == 18000)
        .expect("ACME row");
    assert_eq!(row["paid_minor"], 8000);
    assert_eq!(row["balance_minor"], 10000);
    let (s2, csv) = json_req(&app, "GET", "/api/invoices/export.csv", None).await;
    assert_eq!(s2, StatusCode::OK);
    let csv = csv.as_str().unwrap();
    assert!(
        csv.contains("paid_minor,balance_minor,write_off_reason"),
        "{csv}"
    );
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

#[tokio::test]
async fn email_result_reports_disabled_transport_honestly() {
    // #130: the DEFAULT production wiring (no SMTP env, no vault) boots
    // DisabledEmailSender — sends succeed as logged no-ops. The response must
    // disclose transport "disabled" so no client can read a 200 as delivery.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).expect("store");
    let router = tucano_time::build_router(AppState::new(store));
    raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    let cookie = login_cookie(&router, ADMIN_EMAIL, ADMIN_PW).await;
    let call = |m: String, u: String, b: Option<Value>| {
        let r = router.clone();
        let c = cookie.clone();
        async move {
            let (s, v, _) = raw(&r, &m, &u, b, Some(&c)).await;
            (s, v)
        }
    };
    let (sc, cust) = call(
        "POST".to_string(),
        "/api/customers".into(),
        Some(json!({"name":"ACME","currency":"EUR","default_rate_minor":6000,"email":"billing@acme.test"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cust}");
    let cid = cust["id"].as_str().unwrap().to_string();
    call(
        "POST".to_string(),
        format!("/api/customers/{cid}/projects"),
        Some(json!({"code":"P1","currency":"EUR","rate_minor":6000})),
    )
    .await;
    call(
        "POST".to_string(),
        "/api/entries".into(),
        Some(json!({"date":"2026-10-02","customer_id":cid.clone(),"project_code":"P1","hours":3})),
    )
    .await;
    let (_s, inv) = call(
        "POST".to_string(),
        "/api/invoices".into(),
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    call("POST".into(), format!("/api/invoices/{iid}/issue"), None).await;
    let (se, body) = call("POST".into(), format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    assert_eq!(body["sent_to"], "billing@acme.test");
    assert_eq!(body["transport"], "disabled", "no SMTP => honest label");
    let (sc2, copy) = call(
        "POST".to_string(),
        format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "books@firm.co"})),
    )
    .await;
    assert_eq!(sc2, StatusCode::OK, "{copy}");
    assert_eq!(copy["transport"], "disabled");
    // The audit line records the transport alongside the recipient (#52).
    let (_sa, audit) = call("GET".into(), "/api/audit".into(), None).await;
    let hit = audit["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "invoice_email_copy");
    assert!(hit.is_some());
    assert!(
        hit.unwrap()["subject"]
            .as_str()
            .unwrap()
            .ends_with(":disabled")
    );
}

// ------------------------------------------------------------------ #136 ---

#[tokio::test]
async fn recurring_schedule_pause_resume_keeps_the_cursor() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (s, scd) = json_req(
        &app,
        "POST",
        "/api/schedules",
        Some(json!({"customer_id":cid,"cadence":"monthly","mode":"time","currency":"EUR"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{scd}");
    let sid = scd["id"].as_str().unwrap().to_string();
    // Pause: active=false, identity preserved.
    let (sp, paused) = json_req(
        &app,
        "PUT",
        &format!("/api/schedules/{sid}"),
        Some(json!({"active": false})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{paused}");
    assert_eq!(paused["active"], false);
    assert_eq!(paused["customer_id"], cid);
    assert_eq!(paused["cadence"], "monthly");
    // Resume via re-read.
    let (_sr, list) = json_req(&app, "GET", "/api/schedules", None).await;
    assert_eq!(list["schedules"].as_array().unwrap().len(), 1);
    let (sa, act) = json_req(
        &app,
        "PUT",
        &format!("/api/schedules/{sid}"),
        Some(json!({"active": true})),
    )
    .await;
    assert_eq!(sa, StatusCode::OK, "{act}");
    assert_eq!(act["active"], true);
    // Unknown schedule: 404; unknown field: 422 without change.
    let (s4, _) = json_req(
        &app,
        "PUT",
        "/api/schedules/00000000-0000-0000-0000-000000000000",
        Some(json!({"active": false})),
    )
    .await;
    assert_eq!(s4, StatusCode::NOT_FOUND);
    let (s5, _) = json_req(
        &app,
        "PUT",
        &format!("/api/schedules/{sid}"),
        Some(json!({"active": false, "cadence": "weekly"})),
    )
    .await;
    assert_eq!(s5, StatusCode::UNPROCESSABLE_ENTITY);
    let (_s6, still) = json_req(&app, "GET", "/api/schedules", None).await;
    assert_eq!(
        still["schedules"][0]["active"], true,
        "rejection persisted nothing"
    );
    // Member: admin tier.
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
    let (s7, _, _) = raw(
        &app.router,
        "PUT",
        &format!("/api/schedules/{sid}"),
        Some(json!({"active": false})),
        Some(&cookie),
    )
    .await;
    assert_eq!(s7, StatusCode::FORBIDDEN);
}

// ------------------------------------------------------------------ #139 ---

#[tokio::test]
async fn customer_billing_details_round_trip_and_validate() {
    let (app, _d) = app().await;
    let (s, c) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(
            json!({"name":"GLOBAL","currency":"USD","default_rate_minor":7000,
          "address":{"street":"Mainstross 1","city":"Berlin","postal_code":"10115","country":"DE"},
          "contacts":[{"name":"Ada","role":"Finance","email":"ada@global.test","billing":true},
                      {"name":"Bob","role":"Ops","email":"bob@global.test"}],
          "tax_hundredths":1900,"discount_hundredths":250}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    assert_eq!(c["address"]["country"], "DE");
    assert_eq!(c["contacts"].as_array().unwrap().len(), 2);
    assert_eq!(c["tax_hundredths"], 1900);
    assert_eq!(c["discount_hundredths"], 250);
    // No top-level email at all: the billing contact receives the invoice.
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(
        &app,
        &cid,
        "P1",
        json!({"currency":"USD","rate_minor":7000}),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid.clone(),"project_code":"P1","hours":2})),
    )
    .await;
    let (_s2, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    assert_eq!(
        body["sent_to"], "ada@global.test",
        "billing contact wins (#139)"
    );
    assert_eq!(app.email.messages()[0].to, "ada@global.test");

    // Validation matrix: bad contact email, two billing flags, tax too big,
    // country too short — all 422, nothing persisted by the last shape.
    let (s3, b3) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"BAD","currency":"EUR","default_rate_minor":1,
          "contacts":[{"name":"N","email":"nope"}]})),
    )
    .await;
    assert_eq!(s3, StatusCode::UNPROCESSABLE_ENTITY, "{b3}");
    let (s4, _) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"BAD","currency":"EUR","default_rate_minor":1,
          "contacts":[{"name":"A","billing":true},{"name":"B","billing":true}]})),
    )
    .await;
    assert_eq!(s4, StatusCode::UNPROCESSABLE_ENTITY);
    let (s5, _) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"BAD","currency":"EUR","default_rate_minor":1,"tax_hundredths":10001})),
    )
    .await;
    assert_eq!(s5, StatusCode::UNPROCESSABLE_ENTITY);
    let (s6, b6) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"BAD","currency":"EUR","default_rate_minor":1,
          "address":{"street":"x","country":"D"}})),
    )
    .await;
    assert_eq!(s6, StatusCode::UNPROCESSABLE_ENTITY, "{b6}");
    let (_s7, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert!(
        list["customers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["name"] != "BAD"),
        "rejections persisted nothing"
    );
}

#[tokio::test]
async fn legacy_customer_keeps_working_and_new_fields_are_omitted_when_empty() {
    let (app, d) = app().await;
    let c = new_customer(&app, "OLDSTYLE", "EUR", 5000).await; // only the pre-#139 shape
    assert_eq!(c["email"], "");
    assert!(c.get("contacts").is_none(), "empty vec is skipped: {c}");
    assert!(c.get("address").is_none());
    assert!(c.get("tax_hundredths").is_none() || c["tax_hundredths"] == 0);
    // The stored JSON lacks the new keys entirely; reads must work anyway.
    let path = d
        .path()
        .join("data")
        .join("customers")
        .join(format!("{}.json", c["id"].as_str().unwrap()));
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("contacts"));
    let (_s2, got) = json_req(
        &app,
        "GET",
        &format!("/api/customers/{}", c["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(got["name"], "OLDSTYLE");
}

// ------------------------------------------------------------------ #138 ---

#[tokio::test]
async fn org_profile_round_trips_and_prints_on_the_document() {
    let (app, _d) = app().await;
    // Default read: everything empty.
    let (_s0, empty) = json_req(&app, "GET", "/api/admin/org", None).await;
    assert_eq!(empty["name"], "");
    assert!(empty.get("address").is_none() || empty["address"].is_null());

    // Validation before persistence.
    let (sb, bb) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"X","legal_id":"","address":{"street":"a","country":""}})),
    )
    .await;
    assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY, "{bb}");
    let (_sv, before) = json_req(&app, "GET", "/api/admin/org", None).await;
    assert_eq!(before["name"], "", "rejection persisted nothing");

    // Valid save round-trips.
    let (sp, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"Tucano BV","legal_id":"BE0123456789",
          "address":{"street":"Markt 1","city":"Brugge","postal_code":"8000","country":"BE"}})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{saved}");
    assert_eq!(saved["name"], "Tucano BV");

    // The identity prints on the issued document (uncompressed streams, so
    // the bytes are greppable — the determinism contract of #113 in use).
    let (issued, iid) = seed_issued_invoice(&app).await;
    let (_s2, _h2, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let text = String::from_utf8_lossy(&pdf).into_owned();
    // The layout emits one text-showing op per WORD (positions carry the
    // spacing), so decode the literals back into running text.
    let joined: String = text
        .split('(')
        .skip(1)
        .map(|s| s.split(')').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(joined.contains("BE0123456789"), "legal id on the PDF");
    assert!(
        joined.contains("Tucano BV"),
        "profile name replaces the config default"
    );
    assert!(joined.contains("Brugge"), "address on the PDF");
    assert_eq!(issued["status"], "issued");

    // Member: admin tier on both verbs.
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
    let (sm, _, _) = raw(&app.router, "GET", "/api/admin/org", None, Some(&cookie)).await;
    assert_eq!(sm, StatusCode::FORBIDDEN);
    let (spm, _, _) = raw(
        &app.router,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"Evil"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(spm, StatusCode::FORBIDDEN);
}

// ------------------------------------------------------------------ #143 ---

#[tokio::test]
async fn manual_invoice_creation_computes_exact_totals() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    let (s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,"tax_hundredths":2100,"discount_hundredths":1000,
          "lines":[{"description":"Office seat","item_kind":"product","quantity_hundredths":250,"unit_price_minor":4000,"project_code":"P1"},
                   {"description":"Setup service","item_kind":"service","quantity_hundredths":100,"unit_price_minor":5000}]})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    assert_eq!(inv["status"], "draft");
    assert!(inv["number"].as_str().unwrap().starts_with("INV-"));
    assert_eq!(inv["currency"], "EUR");
    let lines = inv["lines"].as_array().unwrap();
    assert_eq!(lines[0]["amount_minor"], 10000); // 2.50 x 40.00
    assert_eq!(lines[0]["kind"], "fixed");
    assert_eq!(lines[0]["item_kind"], "product");
    assert_eq!(lines[0]["project_code"], "P1");
    assert_eq!(lines[1]["amount_minor"], 5000);
    // subtotal 15000, discount 10% = 1500, VAT 21% of 13500 = 2835, total 16335.
    assert_eq!(inv["total_minor"], 16335);
    assert_eq!(inv["tax_hundredths"], 2100);
    assert_eq!(inv["discount_hundredths"], 1000);
    // Draft locks nothing and sends nothing.
    let eid = json_req(&app, "GET", "/api/entries", None).await;
    let _ = eid;
    let pdf = raw_req(
        &app,
        "GET",
        &format!("/api/invoices/{}/pdf", inv["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(
        pdf.0,
        StatusCode::CONFLICT,
        "draft has no archived document"
    );
}

#[tokio::test]
async fn manual_invoice_validation_matrix_persists_nothing() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    let base = |lines: Value, extra: Value| -> Value {
        let mut o = json!({"customer_id": cid, "lines": lines});
        if let (Some(o1), Some(o2)) = (o.as_object_mut(), extra.as_object()) {
            for (k, v) in o2 {
                o1.insert(k.clone(), v.clone());
            }
        }
        o
    };
    let good = json!([{"description":"X","item_kind":"product","quantity_hundredths":100,"unit_price_minor":100}]);
    for (label, body) in [
        ("empty lines", base(json!([]), json!({}))),
        (
            "zero qty",
            base(
                json!([{"description":"X","item_kind":"product","quantity_hundredths":0,"unit_price_minor":100}]),
                json!({}),
            ),
        ),
        (
            "bad percent",
            base(good.clone(), json!({"tax_hundredths": 10001})),
        ),
        (
            "unknown project",
            base(
                json!([{"description":"X","item_kind":"product","quantity_hundredths":100,"unit_price_minor":100,"project_code":"NOPE"}]),
                json!({}),
            ),
        ),
        ("unknown field", base(good.clone(), json!({"evil": true}))),
        (
            "float quantity",
            json!({"customer_id": cid, "lines": [{"description":"X","item_kind":"product","quantity_hundredths":1.5,"unit_price_minor":100}]}),
        ),
    ] {
        let (st, body) = json_req(&app, "POST", "/api/invoices/manual", Some(body)).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{label}: {body}");
    }
    let (_s, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert!(
        list["invoices"].as_array().unwrap().is_empty(),
        "nothing persisted"
    );
}

#[tokio::test]
async fn draft_editing_replaces_lines_and_issued_stays_immutable() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_s, gent) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = gent["id"].as_str().unwrap().to_string();
    let tracked = &gent["lines"][0];
    assert_eq!(tracked["amount_minor"], 18000);

    // Draft edit: keep the tracked line verbatim + add a manual line, tax 10%.
    let edit = json!({"lines": [tracked,
        {"kind":"fixed","date":"2026-10-02","note":"Extra hosting","item_kind":"service",
         "quantity_hundredths":100,"unit_price_minor":900}],
        "tax_hundredths": 1000, "discount_hundredths": 0});
    let (se, edited) = json_req(&app, "PUT", &format!("/api/invoices/{iid}"), Some(edit)).await;
    assert_eq!(se, StatusCode::OK, "{edited}");
    assert_eq!(edited["lines"].as_array().unwrap().len(), 2);
    // 18000 + 900 = 18900 subtotal; 10% = 1890; total 20790.
    assert_eq!(edited["total_minor"], 20790);
    // The tracked line survived untouched (snapshot #8).
    assert_eq!(edited["lines"][0]["amount_minor"], 18000);
    assert_eq!(edited["lines"][0]["entry_id"], tracked["entry_id"]);

    // Tampering with a tracked line's amount is refused; nothing persisted.
    let mut tampered = tracked.clone();
    tampered["amount_minor"] = json!(1);
    let (st, bt) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(json!({"lines":[tampered],"tax_hundredths":0,"discount_hundredths":0})),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{bt}");
    let (_sr, again) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(again["total_minor"], 20790, "rejection persisted nothing");

    // Issue: snapshot locks the document; PUT now conflicts.
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (si, _) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(json!({"lines":[],"tax_hundredths":0})),
    )
    .await;
    assert_eq!(si, StatusCode::CONFLICT);
    // The archived PDF shows the EDITED lines (the document follows the
    // draft at issue time, not the earlier generation).
    let (_sp, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let joined: String = String::from_utf8_lossy(&pdf)
        .split('(')
        .skip(1)
        .map(|x| x.split(')').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        joined.contains("Extra hosting"),
        "manual line reached the document"
    );
    assert!(joined.contains("207.90"), "total with VAT printed");
}

// ------------------------------------------------- #187: validation gaps --

#[tokio::test]
async fn draft_line_input_rejects_unknown_fields_before_persisting() {
    // #187: the stored InvoiceLine was used directly as the PUT body
    // element without deny_unknown_fields — a typo'd or stale client field
    // was silently dropped. The split input DTO is strict.
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    let (_s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,
          "lines":[{"description":"Seat","item_kind":"product","quantity_hundredths":100,"unit_price_minor":4000}]})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    let good = json!({"kind":"fixed","date":"2026-10-02","note":"Seat",
                      "item_kind":"product","quantity_hundredths":100,"unit_price_minor":4000});
    // Outer envelope was always strict; prove the LINE element now is too.
    let mut stale = good.clone();
    stale["amountTtc"] = json!(1); // legacy/typo key
    let (s, b) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(json!({"lines":[stale],"tax_hundredths":0,"discount_hundredths":0})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    // Sanity: the same edit without the unknown field succeeds.
    let (ok, _) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(json!({"lines":[good],"tax_hundredths":0,"discount_hundredths":0})),
    )
    .await;
    assert_eq!(ok, StatusCode::OK);
}

#[tokio::test]
async fn manual_line_bounds_reject_identically_on_both_endpoints() {
    // #187: create (domain::validate_manual_lines) and edit previously kept
    // two copies of the bounds. One shared checker now guards both.
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    let (s1, b1) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,
          "lines":[{"description":"Seat","item_kind":"product","quantity_hundredths":1_000_001,"unit_price_minor":4000}]})),
    )
    .await;
    assert_eq!(s1, StatusCode::UNPROCESSABLE_ENTITY, "{b1}");
    let (_s, draft) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,
          "lines":[{"description":"Seat","item_kind":"product","quantity_hundredths":100,"unit_price_minor":4000}]})),
    )
    .await;
    let iid = draft["id"].as_str().unwrap().to_string();
    let (s2, b2) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(
            json!({"lines":[{"kind":"fixed","date":"2026-10-02","note":"Seat",
            "item_kind":"product","quantity_hundredths":1_000_001,"unit_price_minor":4000}],
            "tax_hundredths":0,"discount_hundredths":0}),
        ),
    )
    .await;
    assert_eq!(s2, StatusCode::UNPROCESSABLE_ENTITY, "{b2}");
    let msg = b2["error"]["fields"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        msg == b1["error"]["fields"][0]["message"]
            .as_str()
            .unwrap_or_default(),
        "both endpoints must report the same bound: {msg} vs {b1}"
    );
}

#[tokio::test]
async fn claim_rejects_duplicate_expense_ids() {
    // #187: [id, id] summed the expense amount twice into the persisted
    // claim total while referencing one expense.
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    let (_s, x) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(
            json!({"date":"2026-10-02","customer_id":cid,"amount_minor":2500,
                    "currency":"EUR","note":"taxi"}),
        ),
    )
    .await;
    let (s, b) = json_req(
        &app,
        "POST",
        "/api/claims",
        Some(json!({"title":"dup","expense_ids":[x["id"], x["id"]]})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
}

#[tokio::test]
async fn oversized_project_budgets_rejected_without_persisting() {
    // #187: budget fields were the only money/hours inputs persisted with no
    // bound (u64::MAX survived, breaking the burn-rate math downstream).
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let p = new_project(&app, cid, "P1", json!({"budget_hours": 5000})).await;
    assert_eq!(p["budget_hours"], 5000);
    for (field, value) in [
        ("budget_amount_minor", json!(u64::MAX)),
        ("budget_hours", json!(100_000_001u64)),
    ] {
        let mut body = json!({"code":"P1","currency":"EUR","rate_minor":6000,
                              "budget_hours": 5000});
        body[field] = value;
        let (s, b) = json_req(
            &app,
            "PUT",
            &format!("/api/customers/{cid}/projects/P1"),
            Some(body),
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{field}: {b}");
        let (_sr, got) = json_req(
            &app,
            "GET",
            &format!("/api/customers/{cid}/projects/P1"),
            None,
        )
        .await;
        assert_eq!(
            got["budget_hours"], 5000,
            "{field} rejection persisted nothing"
        );
    }
}

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

#[tokio::test]
async fn expense_on_partly_paid_invoice_stays_locked() {
    // #218: the expense-delete scan said '== Issued' while the invoice locks
    // (and the dashboard treats as open) issued | partly_paid — a partial
    // payment silently unlocked the expense rows sitting on that invoice.
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({"rate_minor": 6000})).await;
    let (_s, x) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(
            json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1",
                    "amount_minor":5000,"currency":"EUR","billable":true,"note":"parts"}),
        ),
    )
    .await;
    let xid = x["id"].as_str().unwrap().to_string();
    let (_s2, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    assert_eq!(inv["total_minor"], 5000, "expense-only invoice: {inv}");
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    // Partial payment: issued -> partly_paid.
    let (_sp, paid) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"amount_minor": 2000, "reference": "wire-1"})),
    )
    .await;
    assert_eq!(paid["status"], "partly_paid", "{paid}");
    let (s_del, b_del) = json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None).await;
    assert_eq!(
        s_del,
        StatusCode::CONFLICT,
        "partly_paid must lock its expenses: {b_del}"
    );
    assert!(
        b_del["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("open invoice"),
        "{b_del}"
    );
    // Settle it: only then may the (no longer part of any receivable) expense
    // be removed — matching the entry-lock lifecycle documented in #216.
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"amount_minor": 3000, "reference": "wire-2"})),
    )
    .await;
    let (s_after, b_after) = json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None).await;
    assert_eq!(s_after, StatusCode::NO_CONTENT, "paid releases: {b_after}");
}

#[tokio::test]
async fn store_fault_during_entry_validation_is_500_not_422() {
    // #187: validate_entry_refs' `.ok().flatten()` turned any store failure
    // into a bogus "customer does not exist" 422. A corrupt customer doc
    // must surface as a 500 (generic, no internals leaked).
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    std::fs::write(
        d.path().join(format!("data/customers/{cid}.json")),
        b"{ not json",
    )
    .unwrap();
    let (s, b) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(&cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{b}");
    assert_eq!(b["error"]["code"], "internal");
    let text = b.to_string();
    assert!(
        !text.contains("json") && !text.contains("data/"),
        "no internals: {text}"
    );
}

// ------------------------------------------------------------------ #134 ---

#[tokio::test]
async fn invoice_preview_projects_and_paid_exclusion() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "PA", json!({"rate_minor": 5000})).await;
    new_project(&app, &cid, "PB", json!({"rate_minor": 7000})).await;
    for (proj, day) in [("PA", "2026-10-02"), ("PB", "2026-10-03")] {
        json_req(
            &app,
            "POST",
            "/api/entries",
            Some(json!({"date":day,"customer_id":cid.clone(),"project_code":proj,"hours":2})),
        )
        .await;
    }
    // Preview only PA: exactly its 2h x 50 = 10000, nothing persisted.
    let (sp, preview) = json_req(
        &app,
        "POST",
        "/api/invoices/preview",
        Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-31","project_codes":["PA"]})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{preview}");
    assert_eq!(preview["number"], "", "preview is unsaved");
    let lines = preview["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["project_code"], "PA");
    assert_eq!(preview["total_minor"], 10000);
    let (_s0, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert!(
        list["invoices"].as_array().unwrap().is_empty(),
        "preview persists nothing"
    );

    // Unknown project for the customer: 422.
    let (su, _) = json_req(
        &app,
        "POST",
        "/api/invoices/preview",
        Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-31","project_codes":["NOPE"]})),
    )
    .await;
    assert_eq!(su, StatusCode::UNPROCESSABLE_ENTITY);

    // Save PA for real, pay it fully, then generate again: PA must be
    // excluded now (any non-draft invoice's lines are billed work).
    let (_s1, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-31","project_codes":["PA"]})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference":"x"})),
    )
    .await;
    let (s2, again) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-31"})),
    )
    .await;
    assert_eq!(s2, StatusCode::CREATED, "{again}");
    let l2 = again["lines"].as_array().unwrap();
    assert_eq!(l2.len(), 1, "only PB remains billable");
    assert_eq!(l2[0]["project_code"], "PB");
    assert_eq!(again["total_minor"], 14000);
    // Issue that one too; with every line on a non-draft invoice the period
    // is exhausted -> NothingToInvoice 409.
    let iid2 = again["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid2}/issue"), None).await;
    // And the fully-invoiced period repeats -> NothingToInvoice 409.
    let (s3, _b3) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-31"})),
    )
    .await;
    assert_eq!(s3, StatusCode::CONFLICT);
}

#[tokio::test]
async fn recurring_never_rebills_settled_or_written_off_entries() {
    // #248: the recurring scheduler must share the API's "already invoiced"
    // predicate. A settled (`paid` / `partly_paid`) or written-off invoice's
    // entries are released from the edit lock but must never be re-billed,
    // while a draft's lines stay billable.
    use tucano_time::recurring::RecurringJob;
    use tucano_time::scheduler::Job;

    // Run the daily recurring job once against the router's data dir. `now`
    // fixes the weekly period to 2026-10-01..2026-10-07 (the seeded week).
    let run_recurring = |dir: &std::path::Path| {
        let store = Store::open(dir.join("data")).expect("store");
        RecurringJob::new(Arc::new(store)).run("2026-10-07T12:00:00Z".parse().unwrap());
    };
    let invoice_count = |list: &Value| list["invoices"].as_array().unwrap().len();

    // Every way of closing an invoice must keep its lines out of a new bill.
    for (label, settle) in [
        ("paid", 0u64),
        ("partly_paid", 1000u64),
        ("written_off", 0u64),
    ] {
        let (app, dir) = app().await;
        let c = new_customer(&app, "ACME", "EUR", 6000).await;
        let cid = c["id"].as_str().unwrap().to_string();
        new_project(&app, &cid, "P1", json!({"rate_minor": 6000})).await;
        json_req(
            &app,
            "POST",
            "/api/entries",
            Some(json!({"date":"2026-10-02","customer_id":cid.clone(),"project_code":"P1","hours":2})),
        )
        .await;
        // Bill week 1, then close the invoice the named way.
        let (_s, inv) = json_req(
            &app,
            "POST",
            "/api/invoices",
            Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-07"})),
        )
        .await;
        let iid = inv["id"].as_str().unwrap();
        json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
        if label == "written_off" {
            let (sw, wo) = json_req(
                &app,
                "POST",
                &format!("/api/invoices/{iid}/write-off"),
                Some(json!({"reason":"forgiven"})),
            )
            .await;
            assert_eq!(sw, StatusCode::OK, "{label}: {wo}");
        } else {
            let pay = if label == "paid" {
                json!({"reference":"x"})
            } else {
                json!({"reference":"x","amount_minor": settle})
            };
            let (sp, paid) =
                json_req(&app, "POST", &format!("/api/invoices/{iid}/pay"), Some(pay)).await;
            assert_eq!(sp, StatusCode::OK, "{label}: {paid}");
        }
        // A weekly time schedule whose next period covers the settled week.
        let (ss, sched) = json_req(
            &app,
            "POST",
            "/api/schedules",
            Some(json!({"customer_id":cid.clone(),"cadence":"weekly","mode":"time","currency":"EUR"})),
        )
        .await;
        assert_eq!(ss, StatusCode::CREATED, "{label}: {sched}");
        run_recurring(dir.path());
        let (_sl, list) = json_req(&app, "GET", "/api/invoices", None).await;
        assert_eq!(
            invoice_count(&list),
            1,
            "{label}: the recurring job must not re-bill a settled entry: {list}"
        );
    }

    // (c) A DRAFT's lines stay billable: the same recurring run still bills.
    let (app, dir) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid.clone(),"project_code":"P1","hours":2})),
    )
    .await;
    // Draft only, never issued: its lines are not excluded.
    json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/schedules",
        Some(json!({"customer_id":cid.clone(),"cadence":"weekly","mode":"time","currency":"EUR"})),
    )
    .await;
    run_recurring(dir.path());
    let (_sl, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert_eq!(
        invoice_count(&list),
        2,
        "a draft's lines stay billable, so the recurring run adds an invoice: {list}"
    );
}

// ------------------------------------------------------------------ #147 ---

#[tokio::test]
async fn item_type_catalog_crud_and_editor_prefill_contract() {
    let (app, _d) = app().await;
    let (s, item) = json_req(
        &app,
        "POST",
        "/api/admin/item-types",
        Some(
            json!({"name":"Support hour","kind":"service","description":"Extended support block",
                    "default_price_minor":9500,"currency":"EUR"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{item}");
    let tid = item["id"].as_str().unwrap().to_string();
    assert_eq!(item["active"], true);
    for bad in [
        json!({"name":"","kind":"service"}),
        json!({"name":"X","kind":"product","default_price_minor":100_000_001}),
    ] {
        let (sb, _) = json_req(&app, "POST", "/api/admin/item-types", Some(bad)).await;
        assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let (_sl, list) = json_req(&app, "GET", "/api/admin/item-types", None).await;
    assert_eq!(list["item_types"].as_array().unwrap().len(), 1);
    let (sa, archived) = json_req(
        &app,
        "PUT",
        &format!("/api/admin/item-types/{tid}"),
        Some(json!({"name":"Support hour","kind":"service","default_price_minor":9500,"currency":"EUR","active":false})),
    )
    .await;
    assert_eq!(sa, StatusCode::OK, "{archived}");
    assert_eq!(archived["active"], false);
    assert_eq!(archived["id"], tid);
    let (s4, _) = json_req(
        &app,
        "PUT",
        "/api/admin/item-types/00000000-0000-0000-0000-000000000000",
        Some(json!({"name":"X","kind":"service"})),
    )
    .await;
    assert_eq!(s4, StatusCode::NOT_FOUND);
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
    let (sm, _, _) = raw(
        &app.router,
        "GET",
        "/api/admin/item-types",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(sm, StatusCode::FORBIDDEN);
    let (sd, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/admin/item-types/{tid}"),
        None,
    )
    .await;
    assert_eq!(sd, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn document_labels_reach_the_pdf_and_validation_binds() {
    let (app, _d) = app().await;
    let (sb, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"","footer":"","labels":{"total":"a very long label that goes over the forty character bound"}})),
    )
    .await;
    assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"","footer":"","labels":{"description":"Service delivered","total":"Amount due"}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    assert_eq!(saved["labels"]["total"], "Amount due");
    let (issued, iid) = seed_issued_invoice(&app).await;
    assert_eq!(issued["status"], "issued");
    let (_sp, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let joined: String = String::from_utf8_lossy(&pdf)
        .split('(')
        .skip(1)
        .map(|x| x.split(')').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        joined.contains("Amount due"),
        "custom total label on the PDF"
    );
    assert!(
        joined.contains("Service delivered"),
        "custom description column label"
    );
    let (_sg, got) = json_req(&app, "GET", "/api/admin/invoice-template", None).await;
    assert_eq!(
        got["template"]["labels"]["description"],
        "Service delivered"
    );
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

// ------------------------------------------------------------------ #146 ---

#[tokio::test]
async fn appearance_and_messages_are_honored_not_inert() {
    let (app, _d) = app().await;
    // Invalid shapes are refused before persistence.
    let (sb, _) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"X","reply_to":"nope"})),
    )
    .await;
    assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY);
    let (sb2, _) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"X","accent":"e95420"})),
    )
    .await;
    assert_eq!(sb2, StatusCode::UNPROCESSABLE_ENTITY);
    let (_sv, before) = json_req(&app, "GET", "/api/admin/org", None).await;
    assert_eq!(before["name"], "", "rejections persisted nothing");

    // Valid save round-trips.
    let (s, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"Tucano BV","from_name":"Tucano Invoicing",
                    "reply_to":"billing@tucano.test","accent":"#E95420"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    assert_eq!(saved["from_name"], "Tucano Invoicing");

    // The mail adapter honors the sender identity (#146): the recorded
    // message carries it and build_mime would render From/Reply-To.
    let (issued, iid) = seed_issued_invoice(&app).await;
    let _ = issued;
    let (se, _b) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK);
    let msg = &app.email.messages()[0];
    assert_eq!(msg.from_name.as_deref(), Some("Tucano Invoicing"));
    assert_eq!(msg.reply_to.as_deref(), Some("billing@tucano.test"));

    // The accent paints the newly issued document: color ops exist, and
    // re-rendering is byte-identical (the #113 determinism contract).
    let (_s2, _h2, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert!(pdf.windows(3).any(|w| w == b" rg"), "accent emitted");
    let (_s3, _h3, pdf2) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(pdf, pdf2, "accented bytes deterministic");

    // Clearing the accent restores the ink-only document (no color ops).
    json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"Tucano BV","from_name":"","reply_to":"","accent":""})),
    )
    .await;
    let c = new_customer(&app, "PLAIN", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "PZ", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-11-02","customer_id":cid,"project_code":"PZ","hours":1})),
    )
    .await;
    let (_s4, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-11-01","to":"2026-11-07"})),
    )
    .await;
    let iid2 = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid2}/issue"), None).await;
    let (_s5, _h5, plain) = raw_req(&app, "GET", &format!("/api/invoices/{iid2}/pdf")).await;
    assert!(
        !plain.windows(3).any(|w| w == b" rg"),
        "cleared accent = no color"
    );
}

#[tokio::test]
async fn customer_without_ever_having_projects_deletes_cleanly() {
    // #141 regression: the projects directory only exists after a project was
    // added; remove_dir_all's ENOENT used to surface as a 500.
    let (app, _d) = app().await;
    let c = new_customer(&app, "BARE", "EUR", 5000).await;
    let cid = c["id"].as_str().unwrap();
    let (s, _, _) = raw_req(&app, "DELETE", &format!("/api/customers/{cid}")).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_sg, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert!(
        list["customers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["id"] != c["id"])
    );
}

// ===========================================================================
// #262 — contract/unit gaps from the full-repo review.
//
// Tier 1: the dangerous, previously-dark paths (DELETE over HTTP, RBAC on
// non-owned *mutations*, the single-instance guard, the body-size cap, the
// range cap, and the README-visible operations no test ever hit). Tier 2:
// invalid-shape (422) coverage for endpoints that only had happy-path tests.
// Everything asserts rejection *without persistence* and the documented
// status/error shape — the bar AGENTS.md's test policy sets.
// ===========================================================================

/// Create a member and return their session cookie (shared by the RBAC tests).
async fn member_cookie(app: &Client, email: &str, pw: &str) -> String {
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

// ---------------------------------------------------- Tier 1: dangerous paths --

/// #262(1): `DELETE /api/invoices/{id}` over HTTP — the path that was only
/// covered at the store level. A draft deletes (204 → gone); an issued invoice
/// can never be deleted (409); a member cannot reach the admin-tier surface.
#[tokio::test]
async fn delete_invoice_over_http_is_admin_only_and_draft_only() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(3), "2026-10-02")),
    )
    .await;

    // Draft: deletable.
    let (sc, draft) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{draft}");
    let did = draft["id"].as_str().unwrap().to_string();

    // A member never reaches the admin tier -> 403, and the draft survives.
    let member = member_cookie(&app, "dana@test.local", "danapass123").await;
    let (s_forbidden, _, _) = raw(
        &app.router,
        "DELETE",
        &format!("/api/invoices/{did}"),
        None,
        Some(&member),
    )
    .await;
    assert_eq!(s_forbidden, StatusCode::FORBIDDEN);
    assert_eq!(
        json_req(&app, "GET", &format!("/api/invoices/{did}"), None)
            .await
            .0,
        StatusCode::OK,
        "refused delete must not remove the draft"
    );

    // Admin deletes the draft -> 204, then it is gone (404).
    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/invoices/{did}"), None).await;
    assert_eq!(s_del, StatusCode::NO_CONTENT);
    assert_eq!(
        json_req(&app, "GET", &format!("/api/invoices/{did}"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // Issued: never deletable -> 409, and the invoice survives.
    let (sc2, draft2) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(sc2, StatusCode::CREATED, "{draft2}");
    let iid = draft2["id"].as_str().unwrap().to_string();
    let (si, issued) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(si, StatusCode::OK, "{issued}");
    let (s_conflict, body) = json_req(&app, "DELETE", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(s_conflict, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        json_req(&app, "GET", &format!("/api/invoices/{iid}"), None)
            .await
            .0,
        StatusCode::OK,
        "an issued invoice must survive the refused delete"
    );
}

/// #262(2): the happy-path `DELETE /api/entries/{id}` (204) — the timesheet's
/// primary correction path, previously proven only by its 409/503 refusals.
#[tokio::test]
async fn delete_entry_over_http_returns_204_and_removes_it() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (sc, entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(2), "2026-10-02")),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{entry}");
    let eid = entry["id"].as_str().unwrap().to_string();

    let (s, _, _) = raw_req(&app, "DELETE", &format!("/api/entries/{eid}")).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        json_req(&app, "GET", &format!("/api/entries/{eid}"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, list) = json_req(&app, "GET", "/api/entries?date=2026-10-02", None).await;
    assert!(
        list["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["id"] != entry["id"]),
        "deleted entry must leave the day list"
    );
}

/// #262(3): RBAC on non-owned **mutations** — a member's PUT/DELETE of another
/// user's entry or expense must be 404, never 403 (no existence leak, #51), and
/// the refused mutations must leave storage untouched.
#[tokio::test]
async fn non_owner_member_mutations_are_404_not_403() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    // The admin owns this entry and this expense.
    let (_, admin_entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(2), "2026-10-02")),
    )
    .await;
    let admin_eid = admin_entry["id"].as_str().unwrap().to_string();
    let (sc, cat) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Travel"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cat}");
    let (se, expense) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"category_id":cat["id"],"amount_minor":1000,"currency":"EUR"})),
    )
    .await;
    assert_eq!(se, StatusCode::CREATED, "{expense}");
    let exp_id = expense["id"].as_str().unwrap().to_string();

    let member = member_cookie(&app, "dana@test.local", "danapass123").await;

    let (s_put, _, _) = raw(
        &app.router,
        "PUT",
        &format!("/api/entries/{admin_eid}"),
        Some(entry_body(cid, "P1", json!(5), "2026-10-02")),
        Some(&member),
    )
    .await;
    assert_eq!(
        s_put,
        StatusCode::NOT_FOUND,
        "non-owner edit is 404, not 403"
    );
    let (s_del, _, _) = raw(
        &app.router,
        "DELETE",
        &format!("/api/entries/{admin_eid}"),
        None,
        Some(&member),
    )
    .await;
    assert_eq!(
        s_del,
        StatusCode::NOT_FOUND,
        "non-owner delete is 404, not 403"
    );
    let (s_exp, _, _) = raw(
        &app.router,
        "DELETE",
        &format!("/api/expenses/{exp_id}"),
        None,
        Some(&member),
    )
    .await;
    assert_eq!(
        s_exp,
        StatusCode::NOT_FOUND,
        "non-owner expense delete is 404"
    );

    // Nothing was mutated by the refused writes.
    let (_, still) = json_req(&app, "GET", &format!("/api/entries/{admin_eid}"), None).await;
    assert_eq!(still["hours"], 2);
    let (_, exp_still) = json_req(&app, "GET", &format!("/api/expenses/{exp_id}"), None).await;
    assert_eq!(exp_still["id"], expense["id"]);
}

/// #262(4): the `.server.lock` single-instance guard (the data-corruption
/// guard it exists for). A second instance against the same data dir must be
/// refused; releasing the first lets a new one start.
#[test]
fn server_lock_refuses_a_second_instance() {
    let dir = tempfile::tempdir().unwrap();
    let first = tucano_time::acquire_server_lock(dir.path()).expect("first lock");
    let second = tucano_time::acquire_server_lock(dir.path());
    assert!(second.is_err(), "a second instance must be refused");
    drop(first);
    let third = tucano_time::acquire_server_lock(dir.path());
    assert!(third.is_ok(), "the lock must be reacquirable after release");
}

/// #262(5): the request-body cap (`MAX_REQUEST_BODY_BYTES`, src/lib.rs). Two
/// rules bound a write, and they answer differently on purpose:
///
/// * a body *under* the transport cap but over a **field** rule is validated like
///   any other payload — 422 in the app's own shape, naming the field; the cap
///   was raised to 6 MiB so an inline receipt (~3 MB of file) is never refused by
///   the transport before a handler can say what was wrong (the bug that made a
///   scanned receipt unstorable);
/// * a body *over* the cap is bounded before any handler, and still answers the
///   documented `bad_request` 400 rather than 413 — the #262(5) gap the ignored
///   test below tracks (the `ValidJson` extractor collapses the length-limit read
///   error; `openapi.json` declares no 413).
///
/// Neither path persists a byte.
#[tokio::test]
async fn oversized_body_is_rejected_without_persistence() {
    let (app, _d) = app().await;

    // Past the field rule, well inside the cap: the handler answers, in the app's
    // shape, with the field that was wrong.
    let long = "x".repeat(300 * 1024);
    let (status, body) =
        json_req(&app, "POST", "/api/categories", Some(json!({"name": long}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    assert_eq!(body["error"]["fields"][0]["field"], "name");

    // Past the cap itself: refused by the transport, whatever status the gap
    // above gives it.
    let over = "x".repeat(7 * 1024 * 1024);
    let (status, body) =
        json_req(&app, "POST", "/api/categories", Some(json!({"name": over}))).await;
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::PAYLOAD_TOO_LARGE,
        "an over-cap body must be refused: {status} {body}"
    );

    // Neither one persisted anything.
    let (_, list) = json_req(&app, "GET", "/api/categories", None).await;
    assert!(
        list["categories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["name"].as_str().unwrap_or("").len() < 1000)
    );
}

/// #262(5) INTENT — kept ignored because the gap is in the behaviour, not the
/// test: an oversized body *should* answer `413 Payload Too Large` (the status
/// the issue names and the convention `RequestBodyLimitLayer` exists to signal).
/// Today the `ValidJson` extractor collapses the length-limit read failure into
/// its generic 400, so this cannot pass without a production change —
/// deliberately out of scope for this test-only PR. Follow-up: give the body
/// limit a 413 code in the router/extractor and declare it in `openapi.json`.
/// The sample is the cap itself, which the inline receipt moved to 6 MiB.
#[ignore = "defect: over-cap bodies surface as 400, not 413 (needs a production follow-up)"]
#[tokio::test]
async fn oversized_body_should_surface_as_413() {
    let (app, _d) = app().await;
    let huge = "x".repeat(7 * 1024 * 1024);
    let (status, _) = json_req(&app, "POST", "/api/categories", Some(json!({"name": huge}))).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

/// #262(6a): `GET /api/auth/status` — public, drives the first-run setup UI.
#[tokio::test]
async fn auth_status_is_public_and_reports_initialisation() {
    let (app, _d) = app().await;
    let (status, body) = anon_req(&app, "GET", "/api/auth/status", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["initialised"], true);
}

/// #262(6b): `GET /openapi.json` + `GET /docs` — the served contract and its UI.
#[tokio::test]
async fn openapi_and_docs_are_served_at_root() {
    let (app, _d) = app().await;
    let (s_doc, _h, bytes) = raw_req(&app, "GET", "/openapi.json").await;
    assert_eq!(s_doc, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&bytes).expect("openapi.json is JSON");
    assert!(parsed["paths"].is_object());
    assert_eq!(parsed["openapi"], "3.1.0");
    let (s_docs, _h2, html) = raw_req(&app, "GET", "/docs").await;
    assert_eq!(s_docs, StatusCode::OK);
    assert!(
        String::from_utf8_lossy(&html)
            .to_ascii_lowercase()
            .contains("swagger"),
        "docs page must serve the Swagger UI shell"
    );
}

/// #262(6c): `GET /api/submissions` and `GET /api/claims` — the owner-scoped
/// `visible_to()` list filtering, untested until now.
#[tokio::test]
async fn submissions_and_claims_lists_filter_by_owner() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    // The admin submits a week (needs an own, unlocked entry).
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

    // A member with none of their own sees an empty list.
    let member = member_cookie(&app, "dana@test.local", "danapass123").await;
    let (sm, member_list, _) =
        raw(&app.router, "GET", "/api/submissions", None, Some(&member)).await;
    assert_eq!(sm, StatusCode::OK);
    assert!(
        member_list["submissions"].as_array().unwrap().is_empty(),
        "a member must not see another user's submission"
    );

    // The admin (owner) sees exactly their own.
    let (sa, admin_list) = json_req(&app, "GET", "/api/submissions", None).await;
    assert_eq!(sa, StatusCode::OK);
    assert_eq!(admin_list["submissions"].as_array().unwrap().len(), 1);
    assert_eq!(admin_list["submissions"][0]["id"], sub["id"]);

    // Claims: an empty, owner-filtered read is a valid 200.
    let (sc, claims) = json_req(&app, "GET", "/api/claims", None).await;
    assert_eq!(sc, StatusCode::OK);
    assert!(claims["claims"].as_array().unwrap().is_empty());
}

/// #262(6d): `GET …/tasks` (list) — project-scoped task listing.
/// A minimal ECB-shaped reference document for the offline FX tests. The
/// endpoint's real source is the ECB's published daily feed; this seam keeps
/// the maths, the reference date and the fail-closed path testable with no
/// network at all.
const ECB_SAMPLE: &str = r#"<Cube time='2026-10-14'>
      <Cube currency='USD' rate='1.0831'/>
      <Cube currency='GBP' rate='0.85210'/>
    </Cube>"#;

#[tokio::test]
async fn fx_requires_a_session_and_validates_both_codes() {
    let (app, _d) = app().await;
    let (s, _, _) = raw(&app.router, "GET", "/api/fx?from=USD&to=EUR", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "member-tier route");

    let (s, body) = json_req(&app, "GET", "/api/fx?from=US&to=EUR", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (s, body) = json_req(&app, "GET", "/api/fx?from=EURO&to=EUR", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    let (s, body) = json_req(&app, "GET", "/api/fx?from=USD", None).await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "a missing parameter is a bad request: {body}"
    );
}

#[tokio::test]
async fn fx_currency_with_itself_is_the_identity_and_needs_no_table() {
    let (app, _d) = app().await;
    let (s, body) = json_req(&app, "GET", "/api/fx?from=eur&to=EUR", None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["from"], "EUR", "codes are normalised: {body}");
    assert_eq!(body["rate"], 1.0);
    assert_eq!(body["source"], "identity");
    assert!(
        body["date"].is_null(),
        "no published table was consulted: {body}"
    );
}

#[tokio::test]
async fn fx_serves_the_reference_table_and_fails_closed_without_one() {
    let (app, _d) = app().await;
    tucano_time::fx::set_rates_document(Some(ECB_SAMPLE.to_string()));
    let (s, body) = json_req(&app, "GET", "/api/fx?from=USD&to=EUR", None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(
        body["date"], "2026-10-14",
        "the rate travels with its date: {body}"
    );
    assert_eq!(body["source"], "ecb");
    let usd_eur = body["rate"].as_f64().expect("a numeric rate");
    assert!((usd_eur - 1.0 / 1.0831).abs() < 1e-9, "{body}");

    // A cross rate is derived from the two EUR legs.
    let (s, body) = json_req(&app, "GET", "/api/fx?from=GBP&to=USD", None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let gbp_usd = body["rate"].as_f64().expect("a numeric rate");
    assert!((gbp_usd - 1.0831 / 0.85210).abs() < 1e-9, "{body}");

    // A currency the table does not list is refused, never guessed.
    let (s, body) = json_req(&app, "GET", "/api/fx?from=USD&to=XXX", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // An unusable document fails closed: a 503, not a made-up rate.
    tucano_time::fx::set_rates_document(Some("not a rates document".to_string()));
    let (s, body) = json_req(&app, "GET", "/api/fx?from=USD&to=EUR", None).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "fx_unavailable");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("enter a rate manually"),
        "{body}"
    );

    // Leave the process as it was found for every other test.
    tucano_time::fx::set_rates_document(None);
}

#[tokio::test]
async fn tasks_list_returns_the_projects_tasks() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    new_task(&app, cid, "P1", "T-1", json!({"name":"Design"})).await;

    let (s, list) = json_req(
        &app,
        "GET",
        &format!("/api/customers/{cid}/projects/P1/tasks"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let codes: Vec<&str> = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["T-1"]);
}

/// #262(7): the task-tier update-happy-path (a rename) and the un-referencing
/// delete after the last entry is removed.
#[tokio::test]
async fn task_rename_then_delete_after_unreferencing() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    new_task(&app, cid, "P1", "T-1", json!({"name":"Design"})).await;

    // Rename: body code must match the path code; the name changes.
    let (s_up, updated) = json_req(
        &app,
        "PUT",
        &format!("/api/customers/{cid}/projects/P1/tasks/T-1"),
        Some(json!({"code":"T-1","name":"Design System"})),
    )
    .await;
    assert_eq!(s_up, StatusCode::OK, "{updated}");
    assert_eq!(updated["name"], "Design System");
    let (_, got) = json_req(
        &app,
        "GET",
        &format!("/api/customers/{cid}/projects/P1/tasks/T-1"),
        None,
    )
    .await;
    assert_eq!(got["name"], "Design System", "rename must persist");

    // Referenced by an entry -> delete is refused (409).
    let (_, entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","task_code":"T-1","hours":2})),
    )
    .await;
    let eid = entry["id"].as_str().unwrap().to_string();
    assert_eq!(
        json_req(
            &app,
            "DELETE",
            &format!("/api/customers/{cid}/projects/P1/tasks/T-1"),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    // Un-reference it, then the delete succeeds (204) and the task is gone.
    assert_eq!(
        json_req(&app, "DELETE", &format!("/api/entries/{eid}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (s_del, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/customers/{cid}/projects/P1/tasks/T-1"),
        None,
    )
    .await;
    assert_eq!(s_del, StatusCode::NO_CONTENT);
    assert_eq!(
        json_req(
            &app,
            "GET",
            &format!("/api/customers/{cid}/projects/P1/tasks/T-1"),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

/// #262(8): the HTTP-level 400-day range cap (`MAX_RANGE_DAYS`, enforced at the
/// store level). A wider query rejects with 400; the ceiling itself passes.
#[tokio::test]
async fn range_query_over_max_days_rejects_with_400() {
    let (app, _d) = app().await;
    let (status, body) = json_req(
        &app,
        "GET",
        "/api/entries?from=2020-01-01&to=2026-06-01",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "bad_request");
    // A window inside the cap is accepted.
    let (ok, _) = json_req(
        &app,
        "GET",
        "/api/entries?from=2026-01-01&to=2026-06-01",
        None,
    )
    .await;
    assert_eq!(ok, StatusCode::OK);
}

// ----------------------------------------------------- Tier 2: missing 422s --

/// #262 Tier 2: categories POST and PUT reject invalid shapes (missing/blank
/// name, unknown fields) with 422 and persist nothing.
#[tokio::test]
async fn category_invalid_shapes_are_422_without_persistence() {
    let (app, _d) = app().await;
    let (s_missing, body) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"default_billable": true})),
    )
    .await;
    assert_eq!(s_missing, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    for bad in [
        json!({"name": "   "}),
        json!({"name": "Travel", "bogus": 1}),
        json!({"name": "T", "default_billable": "yes"}),
    ] {
        let label = bad.to_string();
        let (s, _) = json_req(&app, "POST", "/api/categories", Some(bad)).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "payload: {label}");
    }

    // A valid create, then an invalid update leaves the stored value intact.
    let (sc, cat) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Travel"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cat}");
    let cat_id = cat["id"].as_str().unwrap();
    let (s_up, _) = json_req(
        &app,
        "PUT",
        &format!("/api/categories/{cat_id}"),
        Some(json!({"name": ""})),
    )
    .await;
    assert_eq!(s_up, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, list) = json_req(&app, "GET", "/api/categories", None).await;
    assert!(
        list["categories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == cat["id"] && c["name"] == "Travel"),
        "refused update must not change the stored category"
    );
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

/// #262 Tier 2: the claim workflow's error shapes — submit-twice conflicts, a
/// malformed decision is 422, and an unknown claim is 404.
#[tokio::test]
async fn claim_error_shapes_are_honest() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (sc, cat) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Travel"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cat}");
    let (_, exp) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"category_id":cat["id"],"amount_minor":1000,"currency":"EUR"})),
    )
    .await;
    let (scc, claim) = json_req(
        &app,
        "POST",
        "/api/claims",
        Some(json!({"title":"Oct costs","expense_ids":[exp["id"]]})),
    )
    .await;
    assert_eq!(scc, StatusCode::CREATED, "{claim}");
    let claim_id = claim["id"].as_str().unwrap();

    // Submit a draft succeeds; a second submit is a 409 (no longer a draft).
    assert_eq!(
        json_req(
            &app,
            "POST",
            &format!("/api/claims/{claim_id}/submit"),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        json_req(
            &app,
            "POST",
            &format!("/api/claims/{claim_id}/submit"),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // A malformed decision is 422 and leaves the claim submitted.
    let (s_dec, body) = json_req(
        &app,
        "POST",
        &format!("/api/claims/{claim_id}/decision"),
        Some(json!({"decision":"nope"})),
    )
    .await;
    assert_eq!(s_dec, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    // An unknown claim id is a 404, never a phantom state change.
    assert_eq!(
        json_req(
            &app,
            "POST",
            "/api/claims/00000000-0000-0000-0000-000000000000/submit",
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // Submitting an unknown claim is 404; the known one is untouched.
    let (_, listed) = json_req(&app, "GET", "/api/claims", None).await;
    assert_eq!(listed["claims"][0]["state"], "submitted");
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

/// #262 Tier 2: issuing — an empty period is a 409 and persists nothing; a
/// double issue is refused (409) and never re-locks or duplicates.
#[tokio::test]
async fn invoice_issue_empty_period_conflicts_and_double_issue_is_refused() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;

    // No billable, not-yet-invoiced work in the period -> 409, nothing stored.
    let (s_empty, body) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-01-01","to":"2026-01-07"})),
    )
    .await;
    assert_eq!(s_empty, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "conflict");
    let (_, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert!(
        list["invoices"].as_array().unwrap().is_empty(),
        "an empty period must persist no invoice"
    );

    // Issue once, then a second issue is refused.
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(3), "2026-02-02")),
    )
    .await;
    let (sc, draft) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-02-01","to":"2026-02-07"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{draft}");
    let iid = draft["id"].as_str().unwrap();
    assert_eq!(
        json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None)
            .await
            .0,
        StatusCode::OK
    );
    let (s_again, again_body) =
        json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(s_again, StatusCode::CONFLICT, "{again_body}");
}

/// #262 Tier 2: the bootstrap endpoint enforces the weak-password rule through
/// HTTP (previously unit-tested only), on a store with no users yet.
#[tokio::test]
async fn bootstrap_weak_password_is_422_through_the_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let router = tucano_time::build_router(AppState::with_session(
        store,
        Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        )),
    ));
    let (s, body, _) = raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":"a@b.co","password":"short"})),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    // The refusal created no user: the store still reports uninitialised.
    let (_s2, st, _) = raw(&router, "GET", "/api/auth/status", None, None).await;
    assert_eq!(st["initialised"], false);
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
        payments: Arc::new(tucano_time::payments::PaymentRegistry::default()),
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

// --------------------------------------------------------------- GUI assets --
//
// #264 follow-up. The GUI is an ES-module graph served exactly as written: the
// shell loads `/app.js`, which imports the modules under `/js/`. Browsers are
// MIME-strict for module scripts, and a request for a path this server does not
// serve is answered with the JSON 404 body (`application/json`) — so a single
// specifier that resolves to the wrong URL makes the browser refuse that module
// and, because a module graph is all-or-nothing, nothing in the GUI executes:
// buttons dead, navigation dead, no data rendered, while the HTML and CSS still
// paint. jsdom cannot see any of it (it never resolves a module URL and never
// enforces MIME), which is how the GUI suites stayed green on a dead app.
//
// This guard walks the graph the way the browser does — from the entry point the
// served shell declares, following every specifier resolved against the URL of
// the module that declares it — and pins the served content type of each kind of
// asset the GUI needs.

/// An unauthenticated GET that keeps the status, the content type and the body.
async fn asset_get(router: &Router, path: &str) -> (StatusCode, String, String) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.expect("oneshot");
    let status = res.status();
    let ctype = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    (status, ctype, String::from_utf8_lossy(&bytes).into_owned())
}

/// The module specifiers a source declares, in order. Every import/export in the
/// GUI is a single-line statement, so scanning for the first quoted literal on a
/// line that starts with `import`/`export` is exact — and because non-module
/// specifiers (a `'Sunday'` in an array literal, say) are dropped, a parse that
/// missed one would show up as a shorter crawl, which the module count catches.
fn module_specifiers(src: &str) -> Vec<String> {
    src.lines()
        .filter_map(|line| {
            let line = line.trim();
            if !(line.starts_with("import") || line.starts_with("export")) {
                return None;
            }
            let open = line.find('\'')?;
            let close = line[open + 1..].find('\'')?;
            let spec = &line[open + 1..open + 1 + close];
            // Only specifiers that name another file of the graph: a leading
            // `./` or `../`. Anything else on an import/export line is a plain
            // string (e.g. `export const API_BASE = '/api'`), not a dependency.
            if spec.starts_with("./") || spec.starts_with("../") {
                Some(spec.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Resolve `spec` the way the browser does: absolute stays absolute, relative is
/// resolved against the URL of the module that declares it.
fn resolve_specifier(from_url: &str, spec: &str) -> String {
    let joined = if spec.starts_with('/') {
        spec.to_string()
    } else {
        let dir = match from_url.rfind('/') {
            Some(i) => &from_url[..=i],
            None => "/",
        };
        format!("{dir}{spec}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for segment in joined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

/// #264: the served GUI must be a loadable ES-module graph. Every module URL
/// reachable from the entry point answers 200 with a JavaScript MIME type, the
/// shell stays HTML, both stylesheets stay CSS, and a missing asset stays the
/// documented JSON 404 instead of something the module loader would accept.
#[tokio::test]
async fn gui_module_graph_is_served_with_js_mime() {
    let (client, _dir) = app().await;

    let (status, ctype, html) = asset_get(&client.router, "/").await;
    assert_eq!(status, StatusCode::OK, "the GUI shell must be served at /");
    assert!(
        ctype.starts_with("text/html"),
        "/ must be HTML, got {ctype}"
    );

    for css in ["/styles.css", "/vendor/ubuntu/components.css"] {
        let (status, ctype, _) = asset_get(&client.router, css).await;
        assert_eq!(status, StatusCode::OK, "{css} must be served");
        assert!(
            ctype.starts_with("text/css"),
            "{css} must be CSS, got {ctype}"
        );
    }

    // The entry point the browser is told to load (#264 loads it as a module).
    let entry = html
        .split_once("type=\"module\"")
        .and_then(|(_, rest)| rest.split_once("src=\""))
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(src, _)| src.to_string())
        .expect("the shell must load a <script type=\"module\"> entry point");
    assert!(
        entry.starts_with('/'),
        "module entry points are root-relative, got {entry}"
    );

    // Crawl the graph exactly as the browser does, one specifier at a time.
    let mut pending = vec![entry.clone()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(url) = pending.pop() {
        if !seen.insert(url.clone()) {
            continue;
        }
        let (status, ctype, body) = asset_get(&client.router, &url).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{url} is part of the module graph but is not served — the browser then refuses \
             the whole graph and the GUI stays inert"
        );
        assert!(
            ctype.starts_with("text/javascript") || ctype.starts_with("application/javascript"),
            "{url} must carry a JavaScript MIME type, got {ctype} — a module script with any \
             other type is refused by the browser"
        );
        for spec in module_specifiers(&body) {
            pending.push(resolve_specifier(&url, &spec));
        }
    }
    assert!(
        seen.len() >= 18,
        "expected the entry point plus its 17 modules (#264), walked {}: {:?}",
        seen.len(),
        seen
    );

    // A missing asset must stay the documented JSON 404: no filesystem path and,
    // above all, nothing the module loader would accept as a script.
    let (status, ctype, body) = asset_get(&client.router, "/js/does-not-exist.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        ctype.starts_with("application/json"),
        "a missing asset must keep the JSON 404 shape, got {ctype}"
    );
    assert!(
        body.contains("\"code\":\"not_found\"") && !body.contains("web/"),
        "the 404 body must not leak a path: {body}"
    );
}
