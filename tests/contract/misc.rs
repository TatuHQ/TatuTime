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
