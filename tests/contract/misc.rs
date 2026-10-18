use crate::support::*;

use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use axum::http::header;
use http_body_util::BodyExt;
use serde_json::Value;
use serde_json::json;
use tower::ServiceExt;

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

/// #262(5) / QA-06: the request-body cap (`MAX_REQUEST_BODY_BYTES`, src/lib.rs).
/// Two rules bound a write, and they answer differently on purpose:
///
/// * a body *under* the transport cap but over a **field** rule is validated like
///   any other payload — 422 in the app's own shape, naming the field; the cap
///   was raised to 6 MiB so an inline receipt (~3 MB of file) is never refused by
///   the transport before a handler can say what was wrong (the bug that made a
///   scanned receipt unstorable);
/// * a body *over* the cap now answers the documented **413
///   `payload_too_large`** on *both* read paths: the `Content-Length` pre-check
///   (refused before routing) and the chunked read (discovered by the
///   extractor). The message never echoes the limit.
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

    // Past the cap itself, Content-Length path: refused by the transport before
    // any handler, in the documented shape.
    let over = "x".repeat(7 * 1024 * 1024);
    let (status, body) =
        sized_json_req(&app, "POST", "/api/categories", json!({"name": over})).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["error"]["code"], "payload_too_large", "{body}");
    assert!(
        !body
            .to_string()
            .to_ascii_lowercase()
            .contains("length limit"),
        "the transport message must not be echoed: {body}"
    );

    // Past the cap itself, chunked path (no Content-Length): the extractor
    // discovers the limit while reading and answers the same shape.
    let over = "x".repeat(7 * 1024 * 1024);
    let (status, body) =
        json_req(&app, "POST", "/api/categories", Some(json!({"name": over}))).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["error"]["code"], "payload_too_large", "{body}");

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

/// #262(5) / QA-06: an oversized body answers `413 Payload Too Large` in the
/// app's single JSON error shape — not `text/plain` on the Content-Length path
/// and not the extractor's generic 400 on the chunked one. Both paths are
/// asserted, which is why this test is no longer ignored.
#[tokio::test]
async fn oversized_body_should_surface_as_413() {
    let (app, _d) = app().await;
    let huge = "x".repeat(7 * 1024 * 1024);

    let (sized, sized_body) =
        sized_json_req(&app, "POST", "/api/categories", json!({"name": huge})).await;
    assert_eq!(sized, StatusCode::PAYLOAD_TOO_LARGE, "{sized_body}");
    assert_eq!(sized_body["error"]["code"], "payload_too_large");

    let huge = "x".repeat(7 * 1024 * 1024);
    let (chunked, chunked_body) =
        json_req(&app, "POST", "/api/categories", Some(json!({"name": huge}))).await;
    assert_eq!(chunked, StatusCode::PAYLOAD_TOO_LARGE, "{chunked_body}");
    assert_eq!(chunked_body["error"]["code"], "payload_too_large");
}

/// QA-07: a malformed path parameter (including a traversal-shaped one) and a
/// malformed query answer the app's one error shape with `bad_request`, never
/// axum's plain-text rejection quoting the extractor's internals.
#[tokio::test]
async fn malformed_path_and_query_parameters_answer_the_json_error_shape() {
    let (app, _d) = app().await;

    let (status, body) = json_req(&app, "GET", "/api/entries/..%2f..%2fcustomers", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "bad_request", "{body}");
    let text = body.to_string();
    for leaked in ["UUID", "Invalid URL", "Cannot parse", "deserialize"] {
        assert!(!text.contains(leaked), "extractor internals leaked: {text}");
    }

    // The same mapping covers a Query rejection (a repeated single-valued param
    // is a serde error inside the extractor).
    let (status, body) = json_req(
        &app,
        "GET",
        "/api/entries?date=2026-10-01&date=2026-10-02",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "bad_request", "{body}");
    let text = body.to_string();
    assert!(
        !text.contains("deserialize") && !text.contains("query string"),
        "extractor internals leaked: {text}"
    );
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

/// QA-11: a collection directory replaced by a regular file is a **broken data
/// tree**, not an empty collection. It used to answer `200 {"entries":[]}`
/// while every write failed with an opaque 500.
#[tokio::test]
async fn a_collection_replaced_by_a_file_is_not_an_empty_collection() {
    let (app, d) = app().await;
    let entries = d.path().join("data/entries");
    std::fs::remove_dir_all(&entries).expect("remove the entries collection");
    std::fs::write(&entries, b"x").expect("replace it with a file");

    let (status, body) = json_req(&app, "GET", "/api/entries?date=2026-10-02", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"]["code"], "data_dir_invalid", "{body}");
    let text = body.to_string();
    assert!(
        !text.contains("entries") && !text.contains("data/"),
        "the path must not be echoed: {text}"
    );

    // A range read and a single-document read refuse the same way.
    let (status, body) = json_req(
        &app,
        "GET",
        "/api/entries?from=2026-10-01&to=2026-10-02",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"]["code"], "data_dir_invalid", "{body}");
}

/// QA-12: a corrupt `config.json` is a *stored document* failure, distinguishable
/// from the opaque `internal`, with no path in the response.
#[tokio::test]
async fn a_corrupt_config_is_a_corrupt_document_not_internal() {
    let (app, d) = app().await;
    std::fs::write(d.path().join("data/config.json"), b"{ not json").expect("write config");

    let (status, body) = json_req(
        &app,
        "PUT",
        "/api/admin/config",
        Some(json!({"org_name": "X"})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"]["code"], "corrupt_document", "{body}");
    let text = body.to_string();
    assert!(
        !text.contains("config.json") && !text.contains("data/"),
        "the path must not be echoed: {text}"
    );
}

/// QA-12: a data directory the process cannot write answers
/// `data_dir_not_writable` (actionable), never the opaque `internal`.
#[tokio::test]
async fn a_read_only_collection_dir_answers_data_dir_not_writable() {
    use std::os::unix::fs::PermissionsExt;

    let (app, d) = app().await;
    let customers = d.path().join("data/customers");
    let original = std::fs::metadata(&customers)
        .expect("customers dir")
        .permissions();
    let mut read_only = original.clone();
    read_only.set_mode(0o555);
    std::fs::set_permissions(&customers, read_only).expect("chmod 555");

    // Running as root bypasses directory permissions: skip rather than assert a
    // lie (say so in the report).
    if std::fs::write(customers.join(".probe"), b"x").is_ok() {
        let _ = std::fs::remove_file(customers.join(".probe"));
        std::fs::set_permissions(&customers, original).expect("restore mode");
        return;
    }

    let (status, body) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"Blocked","currency":"EUR","default_rate_minor":1000})),
    )
    .await;
    std::fs::set_permissions(&customers, original).expect("restore mode");

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"]["code"], "data_dir_not_writable", "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not writable"),
        "the message must be actionable: {body}"
    );
}
