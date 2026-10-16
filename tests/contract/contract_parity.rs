use crate::support::*;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use axum::http::header;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use tower::ServiceExt;

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
        serde_json::from_str(include_str!("../../openapi.json")).expect("valid openapi.json");
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
        serde_json::from_str(include_str!("../../openapi.json")).expect("valid openapi.json");
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
        serde_json::from_str(include_str!("../../openapi.json")).expect("valid openapi.json");
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
        serde_json::from_str(include_str!("../../openapi.json")).expect("valid openapi.json");
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
        serde_json::from_str(include_str!("../../openapi.json")).expect("valid openapi.json");
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
        serde_json::from_str(include_str!("../../openapi.json")).expect("valid openapi.json");
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
    let text = include_str!("../../openapi.json");
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
        serde_json::from_str(include_str!("../../openapi.json")).expect("valid openapi.json");
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
        serde_json::from_str(include_str!("../../openapi.json")).expect("valid openapi.json");
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
