use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use tower_http::limit::RequestBodyLimitLayer;

use crate::api::{self, AppState};
use crate::error::ApiError;
use crate::{
    API_ROUTES, dev_reload, dev_reload_script, docs, openapi, spawn_web_watcher, static_assets,
};

/// Assemble the router: server meta and the GUI shell stay at the root, every
/// JSON endpoint lives under `/api`, so the top-level paths are free for GUI page
/// routes.
///
/// ```rust
/// let app = build_router(state);
/// // root: /healthz, /openapi.json, /docs, the GUI
/// // /api/auth/login is public; /api/customers needs a session; /api/users an admin
/// ```
pub fn build_router(state: AppState) -> Router {
    // Server meta and the GUI shell stay at the root; every JSON endpoint lives
    // under `/api` so the top-level paths are free for GUI page routes (e.g.
    // `/timesheets/week`, `/settings/users`). The GUI assets (fallback) are
    // public so the login screen can load.
    let meta = Router::new()
        .route("/healthz", get(api::healthz))
        .route("/openapi.json", get(openapi))
        .route("/docs", get(docs));

    let public = Router::new()
        .route("/auth/bootstrap", axum::routing::post(api::bootstrap))
        .route("/auth/login", axum::routing::post(api::login))
        .route("/auth/logout", axum::routing::post(api::logout))
        .route("/auth/status", get(api::auth_status))
        .route("/auth/me", get(api::me))
        // SSO (#32): pre-session; the assertion authenticates itself.
        .route("/auth/sso/providers", get(api::sso_providers))
        .route(
            "/auth/sso/assertion",
            axum::routing::post(api::sso_assertion),
        );

    // Authenticated: all timesheet data.
    let protected = Router::new()
        .route(
            "/customers",
            get(api::list_customers).post(api::create_customer),
        )
        .route(
            "/customers/{id}",
            get(api::get_customer_handler)
                .put(api::update_customer)
                .delete(api::delete_customer),
        )
        .route(
            "/customers/{cid}/projects",
            get(api::list_projects).post(api::create_project),
        )
        .route(
            "/customers/{cid}/projects/{code}",
            get(api::get_project_handler)
                .put(api::update_project)
                .delete(api::delete_project),
        )
        .route(
            "/customers/{cid}/projects/{pcode}/tasks",
            get(api::list_tasks).post(api::create_task),
        )
        .route(
            "/customers/{cid}/projects/{pcode}/tasks/{code}",
            get(api::get_task_handler)
                .put(api::update_task)
                .delete(api::delete_task),
        )
        .route("/entries", get(api::list_entries).post(api::create_entry))
        .route(
            "/entries/{id}",
            get(api::get_entry)
                .put(api::update_entry)
                .delete(api::delete_entry),
        )
        .route("/reports/summary", get(api::summary))
        .route("/reports/export.csv", get(api::export_csv))
        .route(
            "/categories",
            get(api::list_categories).post(api::create_category),
        )
        .route(
            "/categories/{id}",
            axum::routing::put(api::update_category).delete(api::delete_category),
        )
        .route(
            "/expenses",
            get(api::list_expenses).post(api::create_expense),
        )
        .route(
            "/expenses/{id}",
            get(api::get_expense_handler)
                .put(api::update_expense)
                .delete(api::delete_expense),
        )
        // Exchange rates for the expense currency conversion (#…): read-only,
        // fail-closed when no reference table can be loaded.
        .route("/fx", get(api::fx_rate))
        .route(
            "/submissions",
            get(api::list_submissions).post(api::create_submission),
        )
        .route(
            "/submissions/{id}/decision",
            axum::routing::post(api::decide_submission),
        )
        .route("/claims", get(api::list_claims).post(api::create_claim))
        .route(
            "/claims/{id}/submit",
            axum::routing::post(api::submit_claim),
        )
        .route(
            "/timer",
            get(api::get_timer)
                .post(api::start_timer)
                .delete(api::discard_timer),
        )
        .route("/timer/stop", axum::routing::post(api::stop_timer))
        .route("/calendar/events", get(api::calendar_events))
        .route("/notifications", get(api::list_notifications))
        .route(
            "/notifications/read",
            axum::routing::post(api::mark_notifications_read),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api::require_auth,
        ));

    // Admin-only: user management and invoicing (#51).
    let admin = Router::new()
        .route("/users", get(api::list_users).post(api::create_user))
        .route(
            "/users/{id}",
            axum::routing::put(api::update_user).delete(api::delete_user),
        )
        .route(
            "/users/{id}/password",
            axum::routing::put(api::change_user_password),
        )
        .route("/audit", get(api::audit_log))
        // Retainer balances (#144): admin tier like invoices.
        .route(
            "/retainers",
            get(api::list_retainers).post(api::create_retainer),
        )
        .route(
            "/retainers/{id}",
            get(api::get_retainer).post(api::close_retainer),
        )
        .route(
            "/retainers/{id}/credit",
            axum::routing::post(api::credit_retainer),
        )
        .route(
            "/retainers/{id}/draw",
            axum::routing::post(api::draw_retainer),
        )
        // Management reporting (cost rates, budgets) is admin-only (review A4):
        // members see their own work via /reports/summary, not margins.
        .route("/reports/profitability", get(api::profitability))
        .route("/reports/budgets", get(api::budget_report))
        .route(
            "/invoices",
            get(api::list_invoices).post(api::create_invoice),
        )
        .route(
            "/invoices/manual",
            axum::routing::post(api::create_manual_invoice),
        )
        .route(
            "/invoices/preview",
            axum::routing::post(api::preview_invoice),
        )
        .route(
            "/invoices/{id}",
            get(api::get_invoice_handler)
                .delete(api::delete_invoice)
                .put(api::update_invoice_draft),
        )
        .route(
            "/invoices/{id}/issue",
            axum::routing::post(api::issue_invoice),
        )
        .route("/invoices/{id}/pdf", get(api::invoice_pdf))
        .route("/invoices/{id}/pdf/preview", get(api::invoice_pdf_preview))
        .route("/invoices/{id}/pay", axum::routing::post(api::pay_invoice))
        .route(
            "/invoices/{id}/write-off",
            axum::routing::post(api::write_off_invoice),
        )
        .route(
            "/invoices/{id}/email",
            axum::routing::post(api::send_invoice_email),
        )
        .route(
            "/invoices/{id}/email-copy",
            axum::routing::post(api::send_invoice_email_copy),
        )
        .route(
            "/invoices/{id}/sync",
            axum::routing::post(api::sync_invoice),
        )
        .route("/sync/accounting", get(api::sync_status))
        // Invoice document templates (#116): org singleton + live preview.
        .route(
            "/admin/invoice-template",
            get(api::invoice_template_get).put(api::invoice_template_put),
        )
        // Unsaved-template PDF preview: the Settings editor renders a candidate
        // before saving; computed, never persisted.
        .route(
            "/admin/invoice-template/preview",
            axum::routing::post(api::invoice_template_preview),
        )
        // Company identity (#138).
        .route("/admin/org", get(api::org_get).put(api::org_put))
        // Product/Service catalog (#147).
        .route(
            "/admin/item-types",
            get(api::list_item_types).post(api::create_item_type),
        )
        .route(
            "/admin/item-types/{id}",
            axum::routing::put(api::update_item_type).delete(api::delete_item_type),
        )
        .route("/invoices/{id}/document", get(api::invoice_document))
        .route("/invoices/{id}/timeline", get(api::invoice_timeline))
        .route(
            "/invoices/{id}/sent",
            axum::routing::post(api::mark_invoice_sent),
        )
        .route("/invoices/summary", get(api::invoice_summary))
        .route("/invoices/report", get(api::invoice_report_handler))
        .route("/invoices/export.csv", get(api::invoice_export_csv))
        .route(
            "/claims/{id}/decision",
            axum::routing::post(api::decide_claim),
        )
        .route(
            "/admin/config",
            get(api::admin_config).put(api::update_config),
        )
        // SMTP delivery settings (#35 follow-up): admin-read config, a test
        // send, and a vault-backed write that the dynamic transport picks up
        // without a restart.
        .route(
            "/admin/email",
            get(api::admin_email_get).put(api::admin_email_put),
        )
        .route(
            "/admin/email/test",
            axum::routing::post(api::admin_email_test),
        )
        // Credential-vault lifecycle (#77 defaults): inspect, enable, disable and
        // regenerate the managed key without a restart.
        .route("/admin/vault", get(api::vault_status))
        .route(
            "/admin/vault/enable",
            axum::routing::post(api::vault_enable),
        )
        .route(
            "/admin/vault/disable",
            axum::routing::post(api::vault_disable),
        )
        .route(
            "/admin/vault/regenerate",
            axum::routing::post(api::vault_regenerate),
        )
        .route("/admin/secrets", get(api::list_secrets))
        .route(
            "/admin/secrets/{key}",
            axum::routing::put(api::set_secret).delete(api::delete_secret),
        )
        .route(
            "/schedules",
            get(api::list_schedules).post(api::create_schedule),
        )
        .route(
            "/schedules/{id}",
            axum::routing::delete(api::delete_schedule).put(api::update_schedule),
        )
        .route("/calendar/oauth/start", get(api::calendar_oauth_start))
        .route(
            "/calendar/oauth/callback",
            get(api::calendar_oauth_callback),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api::require_admin,
        ));

    // The API nest answers a wrong verb with the app's own error shape (plus the
    // `Allow` header) instead of axum's bare, empty 405. The layer sits OUTSIDE
    // the per-route layers, so a guarded path still answers 401 first: the verb
    // is only rewritten for a caller the route would have accepted.
    let api_router = public
        .merge(protected)
        .merge(admin)
        .layer(axum::middleware::from_fn(json_method_not_allowed));

    meta.nest("/api", api_router)
        // Dev-mode live reload (SSE): active only when TUCANO_WEB_DIR is set.
        // In production the watcher is None and the endpoint returns 404.
        .route(
            "/_dev/reload",
            get({
                let tx = spawn_web_watcher();
                move || dev_reload(tx.clone())
            }),
        )
        // The client half of the same feature, also dev-only. It is served as a
        // real asset rather than inlined into the HTML because the app's own
        // `script-src 'self'` refuses inline scripts: as an inline snippet the
        // browser never ran it, so an edit under web/ did nothing until a
        // manual refresh (and every dev page reported a CSP violation).
        .route("/_dev/reload.js", get(dev_reload_script))
        .fallback(static_assets)
        .layer(axum::middleware::from_fn(csrf_guard))
        .layer(axum::middleware::from_fn(security_headers))
        // Two caps, one number: axum's own body limit is what `Bytes` (and so
        // `ValidJson`) extracts under — its 2 MB default has to be lifted too, or
        // the extractor answers `400 failed to read the request body` before the
        // handler can name the field — and the tower layer caps the raw stream in
        // front of the router.
        .layer(axum::extract::DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(RequestBodyLimitLayer::new(MAX_REQUEST_BODY_BYTES))
        // Outermost on purpose: the transport limit above short-circuits before
        // the route (and so before any layer inside the `/api` nest) runs, so
        // only a layer outside it can give that 413 the app's JSON shape.
        .layer(axum::middleware::from_fn(json_api_errors))
        .with_state(state)
}

/// The largest request body the API reads. It has to sit above the *field*
/// rule, not beside it: an expense carries its receipt inline as base64 (#23)
/// and `validate_expense_input` accepts up to 4 MB of it, so a smaller
/// transport cap answers first — before any handler, as a bare
/// `413 length limit exceeded` outside the app's error shape, which is how a
/// scanned receipt could never be stored. 4 MB of base64 is ~3 MB of file, so
/// 6 MiB holds the whole body with room for JSON quoting; the field-level rule
/// keeps refusing over-long receipts in the app's own 422 shape.
const MAX_REQUEST_BODY_BYTES: usize = 6 * 1024 * 1024;

/// The methods `API_ROUTES` mounts on `path` (the full path, `/api/...`), in
/// registry order. An entry whose template is the path itself always wins — a
/// literal segment beats a `{param}`, exactly as the router prefers it — and only
/// then does a templated entry match segment by segment.
fn allowed_methods(path: &str) -> Vec<&'static str> {
    let collect = |pick: &dyn Fn(&str) -> bool| {
        API_ROUTES
            .iter()
            .filter(|(_, template, _)| pick(template))
            .map(|(method, _, _)| *method)
            .collect::<Vec<_>>()
    };
    let exact = collect(&|template| template == path);
    if !exact.is_empty() {
        return exact;
    }
    let wanted: Vec<&str> = path.trim_matches('/').split('/').collect();
    let matches_template = |template: &str| {
        let parts: Vec<&str> = template.trim_matches('/').split('/').collect();
        parts.len() == wanted.len()
            && parts.iter().zip(&wanted).all(|(part, segment)| {
                part.strip_prefix('{')
                    .is_some_and(|_| part.ends_with('}') && !segment.is_empty())
                    || part == segment
            })
    };
    collect(&matches_template)
}

/// Give every **non-JSON** 4xx/5xx produced under `/api` the app's one error
/// shape (QA-06, QA-07).
///
/// Two producers cannot speak JSON themselves:
///
/// * the tower body limit, which answers `413 text/plain`,
///   `length limit exceeded` before routing — hence this layer sits outside it;
/// * axum's own `Path`/`Query` extractors, whose rejections are plain text and
///   quote extractor internals (`Invalid URL: Cannot parse `id` …`).
///
/// Responses that are already JSON are passed through untouched, so the app's
/// own errors (`ApiError`, the CSRF guard, the 405 rewrite) keep their status
/// and message. The GUI and the static assets are outside `/api` and never
/// enter this mapping.
async fn json_api_errors(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING};
    let path = req.uri().path().to_string();
    let is_api = path == "/api" || path.starts_with("/api/");
    let response = next.run(req).await;
    if !is_api {
        return response;
    }
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let already_json = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim()
                .to_ascii_lowercase()
                .starts_with("application/json")
        });
    if already_json {
        return response;
    }

    // The original body is a transport or extractor message, never a client
    // message, so it is dropped. The status is the one the contract documents
    // for the mapped code (413 stays 413, 400 stays 400, an unexpected 5xx
    // becomes the documented generic 500).
    let replacement = match status {
        StatusCode::PAYLOAD_TOO_LARGE => ApiError::payload_too_large(),
        StatusCode::NOT_FOUND => ApiError::not_found("resource"),
        status if status.is_server_error() => {
            ApiError::internal(format!("non-JSON {status} response under /api"))
        }
        _ => ApiError::bad_request("the request path or query is invalid"),
    };
    let mut mapped = replacement.into_response();
    // Keep every header an inner layer added (the security headers, `Allow`,
    // `Retry-After`, …); only the representation headers of the discarded body
    // are replaced.
    let headers = mapped.headers_mut();
    for (name, value) in response.headers() {
        if name != CONTENT_TYPE && name != CONTENT_LENGTH && name != TRANSFER_ENCODING {
            headers.insert(name.clone(), value.clone());
        }
    }
    mapped
}

/// Give axum's bare 405 the app's one error shape, naming the verbs the path
/// does accept. The layer sits OUTSIDE the per-route layers, so a guarded path
/// still answers 401 first — the verb is only rewritten for a caller the route
/// would have accepted — and it reads the registry rather than the response's
/// `Allow` header, which the outer router only adds afterwards.
async fn json_method_not_allowed(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use axum::http::header::{ALLOW, HeaderValue};
    let method = req.method().clone();
    // Inside the nest a request carries the stripped path (`/expenses`); the
    // registry is keyed by the full one.
    let path = req
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|original| original.0.path().to_string())
        .unwrap_or_else(|| req.uri().path().to_string());
    let mut response = next.run(req).await;
    if response.status() != StatusCode::METHOD_NOT_ALLOWED {
        return response;
    }
    let allowed = allowed_methods(&path);
    let mut replacement = ApiError::method_not_allowed(method.as_str(), &allowed).into_response();
    if !allowed.is_empty()
        && let Ok(value) = HeaderValue::from_str(&allowed.join(", "))
    {
        replacement.headers_mut().insert(ALLOW, value);
    }
    response = replacement;
    response
}

/// Baseline security response headers (#48). Swagger UI is vendored under
/// web/ (review A9), so no third-party origin can execute in the admin GUI's
/// cookie scope — the CSP is strictly 'self'.
async fn security_headers(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::header::{
        CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
    };
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(X_CONTENT_TYPE_OPTIONS, "nosniff".parse().expect("static"));
    h.insert(REFERRER_POLICY, "no-referrer".parse().expect("static"));
    h.insert(X_FRAME_OPTIONS, "DENY".parse().expect("static"));
    h.insert(
        CONTENT_SECURITY_POLICY,
        "default-src 'self'; script-src 'self'; \
         style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
         font-src 'self'; connect-src 'self'; frame-ancestors 'none'; \
         base-uri 'self'; form-action 'self'; \
         frame-src 'self' blob:"
            .parse()
            .expect("static"),
    );
    res
}

/// CSRF guard (#46): mutating requests must carry the `X-CSRF-Protection: 1`
/// header. Combined with the SameSite=Lax session cookie, a cross-site form
/// post cannot pass. The pre-auth login/bootstrap endpoints are exempt (no
/// session to abuse yet).
async fn csrf_guard(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let mutating = matches!(
        method,
        axum::http::Method::POST | axum::http::Method::PUT | axum::http::Method::DELETE
    );
    // Login/bootstrap are pre-session; the assertion authenticates itself.
    let exempt = path == "/api/auth/login"
        || path == "/api/auth/bootstrap"
        || path == "/api/auth/sso/assertion"; // pre-session; the assertion is its own proof (review A6)
    if mutating && !exempt {
        let ok = req
            .headers()
            .get("x-csrf-protection")
            .and_then(|v| v.to_str().ok())
            == Some("1");
        if !ok {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": { "code": "csrf", "message": "missing X-CSRF-Protection header" }
                })),
            )
                .into_response();
        }
    }
    next.run(req).await
}
