use crate::support::*;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use axum::http::header;
use http_body_util::BodyExt;
use tower::ServiceExt;

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
