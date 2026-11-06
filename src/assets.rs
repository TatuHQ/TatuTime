use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::{Assets, OPENAPI, spa_page};

pub(crate) async fn openapi() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        OPENAPI,
    )
}

pub(crate) async fn docs() -> Response {
    match Assets::get("swagger.html") {
        Some(file) => (
            [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
            String::from_utf8_lossy(file.data.as_ref()).into_owned(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Serves the GUI.
///
/// **Production** (no `TATU_WEB_DIR`): files are read from the embedded
/// bundle compiled in at build time — zero runtime deps, single binary.
///
/// **Dev mode** (`TATU_WEB_DIR=/path/to/web`): files are read from disk on
/// every request, so editing HTML/CSS/JS takes effect on the next browser
/// refresh with no Rust rebuild. Every HTML response gets a
/// `<script src="/_dev/reload.js">` tag — an external asset, never an inline
/// script, so `script-src 'self'` permits it — which opens an SSE connection
/// to `/_dev/reload`; the server sends a `reload` event whenever it detects a
/// file change and the script calls `location.reload()`.
///
/// Unknown non-API paths 404 with the JSON error shape so no path or asset
/// enumeration leaks internals (production) or filesystem paths (dev).
pub(crate) async fn static_assets(uri: axum::http::Uri) -> Response {
    // GUI page routes (e.g. /timesheets/week, /settings/users) all serve the
    // single-page shell so a screen can be bookmarked or referenced directly.
    // Every other path is treated as a real asset and 404s if absent, so this
    // stays an allowlist rather than a blanket catch-all.
    let rel = if spa_page(uri.path()) {
        "index.html"
    } else {
        uri.path().trim_start_matches('/')
    };

    // Dev mode: serve directly from the web dir on disk.
    if let Ok(web_dir) = std::env::var("TATU_WEB_DIR") {
        // Prevent directory traversal: only allow simple relative paths with
        // no `..` segments and no leading `/`.
        if rel.contains("..") || rel.starts_with('/') {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": { "code": "not_found", "message": "resource does not exist" }
                })),
            )
                .into_response();
        }
        let full_path = std::path::Path::new(&web_dir).join(rel);
        return match std::fs::read(&full_path) {
            Ok(bytes) => {
                let ct = content_type(rel);
                // Point every HTML page at the live-reload asset so the browser
                // auto-refreshes when the watcher fires. A `<script src>` (not
                // an inline script) is what makes it legal under the app's own
                // `script-src 'self'` CSP.
                let body: axum::body::Body = if ct.starts_with("text/html") {
                    const SNIPPET: &[u8] = br#"<script src="/_dev/reload.js"></script>"#;
                    let mut out = bytes;
                    out.extend_from_slice(SNIPPET);
                    out.into()
                } else {
                    bytes.into()
                };
                (
                    [
                        (axum::http::header::CONTENT_TYPE, ct),
                        (axum::http::header::CACHE_CONTROL, "no-store"),
                    ],
                    body,
                )
                    .into_response()
            }
            Err(_) => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": { "code": "not_found", "message": "resource does not exist" }
                })),
            )
                .into_response(),
        };
    }

    // Production: serve from the embedded bundle.
    match Assets::get(rel) {
        Some(file) => (
            [
                (axum::http::header::CONTENT_TYPE, content_type(rel)),
                // Stable file names mean a cached copy can survive a deploy.
                // `no-cache` alone is not enough behind a CDN: Cloudflare's
                // Browser Cache TTL rewrites it to `max-age=14400`, so a
                // browser can keep an old module while loading the new shell
                // (new form inputs appear, but their save code is stale).
                // `no-store` is the header both Cloudflare and browsers honour,
                // so an update is visible on the next load. Images and fonts
                // keep the cheaper revalidation policy.
                (
                    axum::http::header::CACHE_CONTROL,
                    cache_control(content_type(rel)),
                ),
            ],
            file.data,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "not_found", "message": "resource does not exist" }
            })),
        )
            .into_response(),
    }
}

/// The dev live-reload client, served as a real asset (`/_dev/reload.js`).
///
/// This is the client half of [`dev_reload`]: it opens the `EventSource` and
/// reloads the page when the server announces a change. It must be an external
/// script because the app's CSP is `script-src 'self'` — an inline snippet would
/// be refused and log a CSP violation.
///
/// Dev-only, exactly like the SSE endpoint: without `TATU_WEB_DIR` it 404s
/// with the standard error shape, and no copy exists in the embedded bundle.
pub(crate) async fn dev_reload_script() -> Response {
    if std::env::var("TATU_WEB_DIR").is_err() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "not_found", "message": "resource does not exist" }
            })),
        )
            .into_response();
    }
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )],
        DEV_RELOAD_JS,
    )
        .into_response()
}

/// Body of `/_dev/reload.js`. Kept byte-identical in behaviour to the snippet
/// it replaces: on a `reload` event, refresh; if the stream drops, retry by
/// reloading, which re-establishes the connection.
const DEV_RELOAD_JS: &str = "\
(function () {
  var es = new EventSource('/_dev/reload');
  es.addEventListener('reload', function () { location.reload(); });
  es.onerror = function () { setTimeout(function () { location.reload(); }, 1000); };
})();
";

/// SSE endpoint used in dev mode only (`TATU_WEB_DIR` set).
///
/// Clients subscribe at `/_dev/reload`; the server polls the web dir every
/// 300 ms and sends a `reload` event when any file's mtime advances.  The
/// endpoint returns 404 in production so it is invisible outside dev mode.
pub(crate) async fn dev_reload(
    tx: Option<std::sync::Arc<tokio::sync::watch::Sender<u64>>>,
) -> Response {
    let Some(tx) = tx else {
        // Not in dev mode — treat as unknown route.
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "code": "not_found", "message": "resource does not exist" }
            })),
        )
            .into_response();
    };

    let mut rx = tx.subscribe();
    // Skip the current value; we only want future changes.
    rx.mark_unchanged();

    // Build a Stream from the watch receiver using `unfold` — no extra crate
    // needed since futures-util is already a transitive dep of axum/tokio.
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.changed().await.ok()?;
        let event = axum::response::sse::Event::default()
            .event("reload")
            .data("1");
        Some((Ok::<_, std::convert::Infallible>(event), rx))
    });

    axum::response::Sse::new(stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new().interval(std::time::Duration::from_secs(15)),
        )
        .into_response()
}

/// Spawn a background task that polls the web dir every 300 ms and sends a
/// tick on `tx` whenever any file's mtime advances.  Returns `None` when
/// `TATU_WEB_DIR` is not set (production — nothing to watch).
pub fn spawn_web_watcher() -> Option<std::sync::Arc<tokio::sync::watch::Sender<u64>>> {
    let web_dir = std::env::var("TATU_WEB_DIR").ok()?;
    let (tx, _rx) = tokio::sync::watch::channel(0u64);
    let tx = std::sync::Arc::new(tx);
    let tx2 = tx.clone();
    tokio::spawn(async move {
        let mut last: u64 = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let stamp = dir_stamp(&web_dir);
            if stamp != last {
                last = stamp;
                // Ignore send errors — all receivers may have dropped.
                let _ = tx2.send(stamp);
            }
        }
    });
    Some(tx)
}

/// Cheap directory fingerprint: sum of all mtimes (seconds) under `dir`.
/// Not cryptographically meaningful — just needs to change when any file does.
fn dir_stamp(dir: &str) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter_map(|m| m.modified().ok())
        .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .fold(0u64, u64::wrapping_add)
}

/// Content type by extension. The web set is fixed and owned, so a small map
/// beats pulling a MIME-guessing dependency into the tree.
fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "json" => "application/json",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Code and markup must never be served from a cache across a deploy: their
/// URLs are stable, so a stale module can be paired with a newer shell. Images
/// and fonts are safe to revalidate (`no-cache`) instead.
fn cache_control(content_type: &str) -> &'static str {
    if content_type.starts_with("text/") || content_type == "application/json" {
        "no-store"
    } else {
        "no-cache"
    }
}
