// The REST API: routing, extractors and handlers. Handlers validate, then
// hand off to `store`; nothing touches the filesystem except through it, and
// nothing returns internal detail to a client.
//
// This module owns the shared surface — `AppState`, the request extractors,
// the route guards and the cross-cutting helpers the handlers reuse. The
// endpoint bodies live in the feature submodules below, split along the
// section banners this file carried until #100. `lib.rs` keeps the central
// route table; the `pub use` re-exports at the bottom keep `api::<handler>`
// paths stable.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::{self, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::{
    PublicUser, Role, Session, User, normalise_email, token_from_cookie_header, valid_password,
    verify_password,
};
use crate::domain::{
    Category, CategoryInput, ClaimInput, ClaimState, Currency, Customer, CustomerInput, Entry,
    EntryInput, Expense, ExpenseClaim, ExpenseInput, FieldError, Hours, Invoice, InvoiceError,
    InvoiceStatus, Project, ProjectCode, ProjectInput, Source, StartTimerInput, Task, TaskInput,
    Timer, elapsed_hundredths, generate_invoice, validate_category_input, validate_customer_input,
    validate_entry_input, validate_expense_input, validate_project_input, validate_task_input,
};
use crate::domain::{Submission, SubmissionState};
use crate::error::ApiError;
use crate::report;
use crate::store::{Store, StoreError};

pub mod auth;
pub mod entries_reports;
pub mod expenses;
pub mod fx;
pub mod invoicing;
pub mod people;
pub mod retainers;
pub mod timer_calendar;
pub mod vault_config;

pub type ApiResult = Result<Response, ApiError>;

/// JSON body that must arrive as exactly one unknown-field-rejecting object.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let body = Bytes::from_request(req, state)
            .await
            .map_err(|_| ApiError::bad_request("failed to read the request body"))?;
        let value = serde_json::from_slice::<T>(&body).map_err(|e| deser_error(e, &body))?;
        Ok(ValidJson(value))
    }
}

/// The authenticated user, resolved from the session cookie.
#[derive(Debug, Clone)]
pub struct AuthUser(pub User);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        authenticate(state, &parts.headers).await.map(AuthUser)
    }
}

/// Turn a serde failure into the shared 422 shape, naming the field it is about.
///
/// serde quotes the *value* in backticks as readily as the key — `invalid type:
/// integer \`42\`, expected a string` — so a field is only ever the token after one
/// of serde's field markers. Reading "the first backticked token" instead reported
/// the value (`"field": "42"`), i.e. a field nobody has, on the one path that
/// tells a caller what to fix. When no marker is present the field stays empty and
/// the *reason* becomes serde's own sentence rather than a generic line that hides
/// it; `unknown field` keeps its exact wording, which the GUI keys on to spot a
/// server older than the page.
///
/// A *type* mismatch (#338) carries no marker: serde names the value, not the
/// key. The field is recovered from the raw body instead — [`key_at`] maps the
/// error's position back to the object key whose value it was parsing, which is
/// exact for the flat and single-level-nested shapes these endpoints accept
/// (`{"name":42}` names `name`; `{"inner":{"name":42}}` names the leaf `name`).
/// The mapping is attempted only for serde's *data* errors, so a syntax error's
/// position (an unclosed brace, trailing characters) can never borrow a key from
/// an unrelated byte. Where the body holds no enclosing key — a bare scalar, or a
/// body that is not JSON — the field stays empty exactly as before and the reason
/// remains serde's sentence.
fn deser_error(e: serde_json::Error, body: &[u8]) -> ApiError {
    let msg = e.to_string();
    let after = |marker: &str| -> Option<String> {
        msg.split(marker)
            .nth(1)
            .and_then(|rest| rest.split('`').next())
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
    };
    if msg.contains("unknown field") {
        return ApiError {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "validation_failed",
            message: "payload failed validation".into(),
            fields: vec![FieldError::new(
                after("unknown field `").unwrap_or_default(),
                "unknown field",
            )],
        };
    }
    let field = after("for field `")
        .or_else(|| after("missing field `"))
        .or_else(|| after("duplicate field `"))
        .or_else(|| type_mismatch_key(&e, body))
        .unwrap_or_default();
    ApiError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code: "validation_failed",
        message: "payload failed validation".into(),
        fields: vec![FieldError::new(field, msg)],
    }
}

/// The key a context-free serde failure (a type mismatch, #338) was about, or
/// `None` when the body cannot supply one.
///
/// Only a *data* error is mapped: serde reports the position of a value it was
/// already parsing, so the key whose value's first token begins at or before that
/// position is the field. A syntax error's position can land anywhere in the text,
/// so it is left alone rather than misattributed to a neighbouring key.
fn type_mismatch_key(e: &serde_json::Error, body: &[u8]) -> Option<String> {
    if e.classify() != serde_json::error::Category::Data {
        return None;
    }
    key_at(body, e.line(), e.column())
}

/// The object key whose value's first token begins at or before `(line, column)`.
///
/// A byte-level scan rather than a parse: it reads the body left to right and,
/// whenever it meets a string that is a key — one whose next non-space byte is
/// `:` — remembers it with the byte where its value's first token starts. The
/// last such key that begins at or before the error wins. That is the innermost
/// key, because serde is depth-first and reports the position of the value it was
/// parsing. The limits are deliberate and documented so the mapping is trusted no
/// further than it goes:
///
/// - it names the *innermost key only*, never a dotted path, so a nested mismatch
///   reports the leaf (`{"inner":{"name":42}}` → `name`) — the key the GUI can
///   match a control against — rather than `inner.name`;
/// - an offending array element is reported under its key (`{"tags":[…,42]}` →
///   `tags`), since elements have no key of their own;
/// - a string *value* that merely looks like a key (`"a\": b"`) is skipped: only a
///   string followed by `:` is a key;
/// - a string it cannot decode (an unknown escape, an unterminated quote) ends the
///   scan with whatever key it had already found, never with a wrong one.
///
/// Column is 1-based and counts bytes — serde_json's own convention, the one a
/// multibyte body must be measured in — so the offset is the line's first byte
/// plus `column - 1`.
fn key_at(body: &[u8], line: usize, column: usize) -> Option<String> {
    let offset = byte_offset(body, line, column);
    let mut i = 0;
    let mut found: Option<(usize, String)> = None;
    while i < body.len() && i <= offset {
        if body[i] != b'"' {
            i += 1;
            continue;
        }
        let Some((key, end)) = read_string(body, i) else {
            break;
        };
        let mut j = end;
        while j < body.len() && body[j].is_ascii_whitespace() {
            j += 1;
        }
        if body.get(j) == Some(&b':') {
            let mut value_start = j + 1;
            while value_start < body.len() && body[value_start].is_ascii_whitespace() {
                value_start += 1;
            }
            if value_start <= offset {
                found = Some((value_start, key));
            }
        }
        i = end;
    }
    found.map(|(_, key)| key)
}

/// The byte offset the 1-based `line`/`column` point at, clamped to the body.
fn byte_offset(body: &[u8], line: usize, column: usize) -> usize {
    let mut start = 0;
    for _ in 1..line {
        match body[start..].iter().position(|&b| b == b'\n') {
            Some(nl) => start += nl + 1,
            None => return body.len(),
        }
    }
    let line_end = body[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(body.len(), |nl| start + nl);
    (start + column.saturating_sub(1)).min(line_end)
}

/// The unescaped text of the JSON string starting at `start` (its opening quote),
/// and the index just past its closing quote.
fn read_string(body: &[u8], start: usize) -> Option<(String, usize)> {
    let mut out: Vec<u8> = Vec::new();
    let mut i = start + 1;
    while i < body.len() {
        match body[i] {
            b'"' => return String::from_utf8(out).ok().map(|s| (s, i + 1)),
            b'\\' => {
                i += 1;
                match *body.get(i)? {
                    b'"' => out.push(b'"'),
                    b'\\' => out.push(b'\\'),
                    b'/' => out.push(b'/'),
                    b'b' => out.push(b'\x08'),
                    b'f' => out.push(b'\x0c'),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'u' => {
                        let hex = std::str::from_utf8(body.get(i + 1..i + 5)?).ok()?;
                        let cp = u32::from_str_radix(hex, 16).ok()?;
                        let ch = char::from_u32(cp)?;
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        i += 4;
                    }
                    _ => return None,
                }
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    None
}

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub clock: Arc<dyn crate::clock::Clock>,
    pub locks: Arc<dyn crate::lock::EntryLock>,
    pub session: Arc<Session>,
    pub rate: Arc<crate::ratelimit::RateLimiter>,
    pub audit: Arc<crate::audit::AuditLog>,
    pub revocations: Arc<crate::revoke::Revocations>,
    pub vault: Option<Arc<crate::vault::SecretVault>>,
    /// Email transport (#35). Disabled unless SMTP is configured; tests inject
    /// a recording sender.
    pub email: Arc<dyn crate::email::EmailSender>,
    /// Payment providers (#34). Empty registry disables the endpoints.
    pub payments: Arc<crate::payments::PaymentRegistry>,
    /// Accounting sync providers (#33). Empty registry disables the endpoints.
    pub accounting: Arc<crate::accounting::AccountingRegistry>,
    /// Identity providers for SSO (#32). Empty registry disables SSO.
    pub sso: Arc<crate::sso::SsoRegistry>,
    /// Pending OAuth calendar consent flows (#36).
    pub oauth_flows: Arc<crate::calendar_oauth::OAuthFlows>,
    /// Externalised runtime configuration (#94): env > config.json > default.
    /// A `Mutex` swap so `PUT /admin/config` can hot-update request-time knobs.
    pub cfg: Arc<std::sync::Mutex<Arc<crate::appconfig::AppConfig>>>,
}

impl AppState {
    /// Convenience wiring with an ephemeral session key (dev/tests). Production
    /// uses `with_session` and a configured secret.
    pub fn new(store: Store) -> Self {
        Self::with_session(store, Arc::new(ephemeral_session()))
    }

    pub fn with_session(store: Store, session: Arc<Session>) -> Self {
        // Lenient path for tests/dev: resolve config + vault from env, ignoring
        // errors (main() uses `boot`, which arrives with hard-checked values).
        let cfg = Arc::new(
            crate::appconfig::AppConfig::load(store.root()).unwrap_or_else(|e| {
                tracing::error!("config.json unusable ({e}); starting with defaults");
                crate::appconfig::AppConfig::empty()
            }),
        );
        let vault = crate::vault::open_for_boot(
            store.root(),
            std::env::var("TUCANO_SECRET_KEY").ok(),
            std::env::var("TUCANO_SECRET_KEY_FILE").ok(),
        )
        .ok()
        .flatten();
        Self::boot(store, session, vault, cfg)
    }

    /// The canonical constructor (#94): main() resolves config + vault with
    /// fail-fast policy and hands them over already validated.
    #[must_use]
    pub fn boot(
        store: Store,
        session: Arc<Session>,
        vault: Option<Arc<crate::vault::SecretVault>>,
        cfg: Arc<crate::appconfig::AppConfig>,
    ) -> Self {
        let store = Arc::new(store);
        let locks = Arc::new(crate::lock::CombinedLocks::new(vec![
            Box::new(crate::lock::InvoiceLock::new(store.clone())),
            Box::new(crate::lock::SubmissionLock::new(store.clone())),
            // #255/#190: reimbursement claims (#24) lock their expenses through
            // the same seam instead of a direct scan in `api::expenses`.
            Box::new(crate::lock::ClaimLock::new(store.clone())),
        ]));
        let audit = Arc::new(crate::audit::AuditLog::new(store.root()));
        let revocations = Arc::new(crate::revoke::Revocations::new(store.root()));
        // Email adapter (#35): SMTP if configured (vault or env), else disabled.
        let email: Arc<dyn crate::email::EmailSender> =
            match crate::email::SmtpConfig::from_sources(vault.as_deref()) {
                Some(cfg) => Arc::new(crate::email::SmtpEmailSender::new(cfg)),
                None => Arc::new(crate::email::DisabledEmailSender),
            };
        // Payment providers (#34): enabled when their webhook secrets exist.
        let payments = crate::payments::registry_from_vault(vault.as_deref());
        // Accounting providers (#33): enabled when an OAuth token exists.
        let accounting = crate::accounting::registry_from_vault(vault.as_deref(), &cfg);
        // SSO identity providers (#32): enabled when OIDC/SAML config exists.
        let sso = Arc::new(crate::sso::registry_from_vault(vault.as_deref(), &cfg));
        Self {
            clock: Arc::new(crate::clock::SystemClock),
            locks,
            session,
            // Lock a login identity after 8 failures within 5 minutes (#47).
            rate: Arc::new(crate::ratelimit::RateLimiter::new(
                8,
                std::time::Duration::from_secs(300),
            )),
            audit,
            revocations,
            vault,
            email,
            payments,
            accounting,
            sso,
            oauth_flows: Arc::new(crate::calendar_oauth::OAuthFlows::new()),
            cfg: Arc::new(std::sync::Mutex::new(cfg)),
            store,
        }
    }

    /// Snapshot of the live configuration (#94).
    #[must_use]
    pub fn cfg(&self) -> Arc<crate::appconfig::AppConfig> {
        match self.cfg.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    /// Hot-swaps the configuration after a `PUT /admin/config` (#94).
    pub fn reload_config(&self, cfg: Arc<crate::appconfig::AppConfig>) {
        match self.cfg.lock() {
            Ok(mut g) => *g = cfg,
            Err(p) => *p.into_inner() = cfg,
        }
    }

    /// Swaps the email transport (tests inject a `RecordingEmailSender`).
    #[must_use]
    pub fn with_email(mut self, sender: Arc<dyn crate::email::EmailSender>) -> Self {
        self.email = sender;
        self
    }

    /// Swaps the payment registry (tests inject providers with known secrets).
    #[must_use]
    pub fn with_payments(mut self, registry: Arc<crate::payments::PaymentRegistry>) -> Self {
        self.payments = registry;
        self
    }

    /// Swaps the accounting registry (tests inject stub transports).
    #[must_use]
    pub fn with_accounting(mut self, registry: Arc<crate::accounting::AccountingRegistry>) -> Self {
        self.accounting = registry;
        self
    }

    /// Swaps the SSO registry (tests inject stub identity providers).
    #[must_use]
    pub fn with_sso(mut self, registry: Arc<crate::sso::SsoRegistry>) -> Self {
        self.sso = registry;
        self
    }
}

/// A random per-process session key; sessions do not survive a restart. A
/// hardening ticket covers requiring a configured secret in production.
fn ephemeral_session() -> Session {
    use rand::TryRng;
    let mut key = [0u8; 32];
    rand::rngs::SysRng
        .try_fill_bytes(&mut key)
        .expect("OS CSPRNG");
    Session::new(key.to_vec(), 60 * 60 * 24, false)
}

/// Resolve the current user from the request's session cookie.
///
/// #254: the two per-request disk reads (`revoked.json` and the user document,
/// both re-parsed on every call) run on the blocking pool, never on an async
/// worker. Ordering, freshness (the revocation file is still read on every
/// request — review B6) and error mapping are unchanged.
async fn authenticate(state: &AppState, headers: &http::HeaderMap) -> Result<User, ApiError> {
    let unauth = || {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "authentication required",
        )
    };
    let header = headers
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(unauth)?;
    let token = token_from_cookie_header(header).ok_or_else(unauth)?;
    let claims = state
        .session
        .verify(token, state.clock.now())
        .ok_or_else(unauth)?;
    let store = state.store.clone();
    let revocations = state.revocations.clone();
    let now = state.clock.now();
    let jti = claims.jti.clone();
    let uid = claims.uid;
    let session_version = claims.session_version;
    let user = blocking("auth user load", move || {
        if revocations.is_revoked(&jti, now) {
            return Err(unauth()); // logged out (#45)
        }
        let user = store.get_user(uid)?.ok_or_else(unauth)?;
        if session_version != user.session_version {
            return Err(unauth()); // credential or role changed since session issuance
        }
        Ok(user)
    })
    .await?;
    if !user.active {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "disabled",
            "account is disabled",
        ));
    }
    Ok(user)
}

/// Private-per-user (#51): an admin sees/edits everything; a member only their
/// own records. `owner` is the record's `user_id` (None = legacy/unattributed).
fn visible_to(actor: &User, owner: Option<Uuid>) -> bool {
    actor.role == Role::Admin || owner == Some(actor.id)
}

/// Admin-only action (#51): approvals and invoicing.
fn ensure_admin(actor: &User) -> Result<(), ApiError> {
    if actor.role == Role::Admin {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "administrator role required",
        ))
    }
}

// ----------------------------------------------------------- shared helpers --

/// Non-identity attributes for creating a user.
pub(crate) struct NewUser {
    pub(crate) role: Role,
    pub(crate) active: bool,
    pub(crate) default_rate_minor: u64,
    pub(crate) cost_rate_minor: u64,
}

pub(crate) async fn new_user(
    name: &str,
    email: &str,
    password: &str,
    p: NewUser,
    app: &AppState,
) -> Result<User, ApiError> {
    // #254: argon2id is deliberately ~50–100 ms of CPU — hash on the blocking
    // pool so a burst of account creations cannot saturate the async workers.
    let password = password.to_owned();
    let password_hash = blocking("password hashing", move || {
        crate::auth::hash_password(&password)
            .map_err(|_| ApiError::internal("password hashing failed".into()))
    })
    .await?;
    let user = User {
        id: Uuid::new_v4(),
        name: name.to_owned(),
        email: email.to_owned(),
        role: p.role,
        active: p.active,
        default_rate_minor: p.default_rate_minor,
        cost_rate_minor: p.cost_rate_minor,
        password_hash,
        created_at: app.clock.now(),
        session_version: 1,
    };
    let user_for_write = user.clone();
    store_blocking("user write", &app.store, move |s| {
        s.put_user(&user_for_write)
    })
    .await?;
    Ok(user)
}

/// Shared `#[serde(default = ...)]` target: an optional boolean that defaults
/// to active/true.
pub(crate) fn default_active_true() -> bool {
    true
}

pub(crate) fn get_customer(store: &Store, id: Uuid) -> Result<Customer, ApiError> {
    store
        .get_customer(id)?
        .ok_or_else(|| ApiError::not_found("customer"))
}

/// Run blocking work on the blocking pool and join it, in one place (the
/// `spawn_blocking` + `JoinError` boilerplate lived in four handlers). The
/// closure maps its own domain error into the endpoint's `ApiError`; a
/// panicking task arrives here as the standard internal 500 — logged with
/// context, never leaked to the client.
pub(crate) async fn blocking<T: Send + 'static>(
    ctx: &str,
    work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| ApiError::internal(format!("{ctx} panicked")))?
}

/// Offload a synchronous `Store` operation to the blocking pool (#254). The
/// store is entirely `std::fs`, and on contention its writer lock even polls
/// with `std::thread::sleep` up to 5 s — running any of that on an async worker
/// stalls unrelated requests. `T` must be `Send`; a `StoreError` maps through
/// `From` into the usual API error shape, so the response contract is unchanged.
pub(crate) async fn store_blocking<T, F>(
    ctx: &str,
    store: &Arc<Store>,
    work: F,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&Store) -> Result<T, StoreError> + Send + 'static,
{
    let store = Arc::clone(store);
    blocking(ctx, move || Ok(work(&store)?)).await
}

/// Append one audit line on the blocking pool (#254): `record` opens and writes
/// `audit.log` under a mutex, which must not happen on an async worker. The
/// write is best-effort by design, so a join failure is ignored, exactly as the
/// synchronous call ignored an unwritable log.
pub(crate) async fn record_audit(app: &AppState, event: &str, subject: &str) {
    let audit = app.audit.clone();
    let event = event.to_owned();
    let subject = subject.to_owned();
    let now = app.clock.now();
    let _ = blocking("audit append", move || {
        audit.record(&event, &subject, now);
        Ok(())
    })
    .await;
}

/// The payments/accounting/sso provider lookup shape: a disabled or unknown
/// provider is a 400 naming the kind, byte-identical to the three copies this
/// replaces.
pub(crate) fn provider_or_400<T>(found: Option<T>, kind: &str) -> Result<T, ApiError> {
    found.ok_or_else(|| ApiError::bad_request(format!("unknown or disabled {kind} provider")))
}

pub(crate) fn parse_date(s: &str) -> Result<chrono::NaiveDate, ApiError> {
    chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|_| ApiError::bad_request(format!("'{s}' is not a date in YYYY-MM-DD form")))
}

/// Resolve an optional `from`/`to` query pair, shared by the typed range
/// queries below. Same messages as the old map-based `parse_range`.
pub(crate) fn parse_range(
    from: Option<&String>,
    to: Option<&String>,
) -> Result<(chrono::NaiveDate, chrono::NaiveDate), ApiError> {
    let missing = || ApiError::bad_request("query needs 'from' and 'to'");
    let (from, to) = (from.ok_or_else(missing)?, to.ok_or_else(missing)?);
    let (from, to) = (parse_date(from)?, parse_date(to)?);
    if from > to {
        return Err(ApiError::bad_request("'from' must not be after 'to'"));
    }
    Ok((from, to))
}

/// `?from=YYYY-MM-DD&to=YYYY-MM-DD` as a typed query (#100), replacing the
/// hand-rolled `Query<HashMap>` + manual lookups. Unknown params stay
/// tolerated and missing ones keep the documented message.
#[derive(Debug, serde::Deserialize)]
pub struct RangeQuery {
    #[serde(default)]
    pub(crate) from: Option<String>,
    #[serde(default)]
    pub(crate) to: Option<String>,
}

impl RangeQuery {
    /// Resolve both bounds; same errors as the map-based original.
    pub(crate) fn dates(&self) -> Result<(chrono::NaiveDate, chrono::NaiveDate), ApiError> {
        parse_range(self.from.as_ref(), self.to.as_ref())
    }
}

/// Route guard: reject with 401 unless a valid session cookie is present.
pub async fn require_auth(
    State(app): State<AppState>,
    mut req: Request,
    next: axum::middleware::Next,
) -> Result<Response, ApiError> {
    let user = authenticate(&app, req.headers()).await?;
    req.extensions_mut().insert(user);
    Ok(next.run(req).await)
}

/// Route guard: like `require_auth`, plus an admin-role check (403 otherwise).
pub async fn require_admin(
    State(app): State<AppState>,
    mut req: Request,
    next: axum::middleware::Next,
) -> Result<Response, ApiError> {
    let user = authenticate(&app, req.headers()).await?;
    if user.role != Role::Admin {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "administrator role required",
        ));
    }
    req.extensions_mut().insert(user);
    Ok(next.run(req).await)
}

pub async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

// Re-exports keep the central route table in `lib.rs` (and `crate::api::…`
// paths elsewhere in the crate) working unchanged after the #100 split.
pub use self::auth::{
    BootstrapInput, LoginInput, SsoAssertionInput, audit_log, auth_status, bootstrap, login,
    logout, me, sso_assertion, sso_providers,
};
pub use self::entries_reports::{
    budget_report, create_entry, delete_entry, export_csv, get_entry, list_entries, profitability,
    summary, update_entry,
};
pub use self::expenses::{
    ClaimDecisionInput, DecisionInput, SubmitInput, create_category, create_claim, create_expense,
    create_submission, decide_claim, decide_submission, delete_category, delete_expense,
    get_expense_handler, list_categories, list_claims, list_expenses, list_submissions,
    submit_claim, update_category, update_expense,
};
pub use self::fx::{FxQuery, fx_rate};
pub use self::invoicing::{
    CheckoutInput, InvoiceEditInput, InvoiceInput, ManualInvoiceInput, PayInput, SyncInput,
    create_checkout, create_invoice, create_item_type, create_manual_invoice, delete_invoice,
    delete_item_type, get_invoice_handler, invoice_document, invoice_export_csv, invoice_pdf,
    invoice_report_handler, invoice_summary, invoice_template_get, invoice_template_put,
    issue_invoice, list_invoices, list_item_types, money_for_email, org_get, org_put, pay_invoice,
    payment_webhook, preview_invoice, send_invoice_email, send_invoice_email_copy, sync_invoice,
    sync_status, update_invoice_draft, update_item_type, write_off_invoice,
};
pub use self::people::{
    UserInput, UserPasswordInput, UserUpdateInput, change_user_password, create_customer,
    create_project, create_task, create_user, delete_customer, delete_project, delete_task,
    delete_user, get_customer_handler, get_project_handler, get_task_handler, list_customers,
    list_projects, list_tasks, list_users, update_customer, update_project, update_task,
    update_user,
};
pub use self::retainers::{
    RetainerCreate, RetainerTxInput, close_retainer, create_retainer, credit_retainer,
    draw_retainer, get_retainer, list_retainers,
};
pub use self::timer_calendar::{
    ScheduleInput, SchedulePatch, calendar_events, calendar_oauth_callback, calendar_oauth_start,
    create_schedule, delete_schedule, discard_timer, get_timer, list_notifications, list_schedules,
    mark_notifications_read, start_timer, stop_timer, update_schedule,
};
pub use self::vault_config::{
    SecretInput, admin_config, delete_secret, list_secrets, set_secret, update_config,
};

#[cfg(test)]
mod deser_error_tests {
    use super::*;
    use serde::de::DeserializeOwned;

    /// A payload shape small enough to provoke each serde failure by hand.
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Probe {
        #[allow(dead_code)]
        name: String,
    }

    /// One level of nesting, for the innermost-key shape.
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Nested {
        #[allow(dead_code)]
        inner: Probe,
    }

    /// A sequence field, for the array-element shape.
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Tags {
        #[allow(dead_code)]
        tags: Vec<String>,
    }

    /// Two keys, to check *which* key is named and that a key-lookalike inside a
    /// string value is not one.
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TwoKeys {
        #[allow(dead_code)]
        note: String,
        #[allow(dead_code)]
        name: String,
    }

    fn failure<T: DeserializeOwned + std::fmt::Debug>(json: &str) -> (String, String) {
        let err = serde_json::from_str::<T>(json).expect_err("the probe must be refused");
        let mapped = deser_error(err, json.as_bytes());
        assert_eq!(mapped.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(mapped.code, "validation_failed");
        let field = &mapped.fields[0];
        (field.field.clone(), field.message.clone())
    }

    #[test]
    fn a_wrong_typed_field_is_named_by_its_key_not_its_value() {
        let (field, reason) = failure::<Probe>("{\"name\":42}");
        // #338: serde_json names no key for a struct type mismatch — it quotes the
        // *value* (`42`) and gives its position. The mapper ties that position back
        // to the key, so the field is the real key and never the quoted value.
        assert_eq!(
            field, "name",
            "the key, not the value serde quotes: {reason}"
        );
        assert_ne!(field, "42");
        assert!(reason.contains("invalid type"), "the reason: {reason}");
        assert!(
            reason.contains("column 10"),
            "serde's own position survives: {reason}"
        );
    }

    #[test]
    fn whitespace_between_key_and_value_does_not_move_the_field() {
        let (field, _) = failure::<Probe>("{\"name\": 42 }");
        assert_eq!(field, "name");
    }

    #[test]
    fn the_innermost_key_is_named_for_a_nested_mismatch() {
        let (field, reason) = failure::<Nested>("{\"inner\":{\"name\":42}}");
        assert_eq!(field, "name", "the leaf key, not a dotted path: {reason}");
    }

    #[test]
    fn an_offending_array_element_is_reported_under_its_key() {
        let (field, reason) = failure::<Tags>("{\"tags\":[\"a\",42]}");
        assert_eq!(field, "tags", "elements have no key of their own: {reason}");
    }

    #[test]
    fn the_last_key_before_the_error_wins() {
        let (field, reason) = failure::<TwoKeys>("{\"note\":\"x\",\"name\":42}");
        assert_eq!(field, "name", "the second key, not the first: {reason}");
    }

    #[test]
    fn a_key_lookalike_inside_a_string_value_is_not_a_key() {
        // The `": ` here is inside a string *value*; only `name` is a key.
        let (field, _) = failure::<TwoKeys>("{\"note\":\"a\\\": b\",\"name\":42}");
        assert_eq!(field, "name");
    }

    #[test]
    fn a_position_on_a_later_line_is_still_named() {
        let (field, _) = failure::<Probe>("{\n  \"name\": 42\n}");
        assert_eq!(field, "name", "the column is measured within its line");
    }

    #[test]
    fn no_enclosing_key_leaves_the_field_empty() {
        // A bare scalar names no key; the reason is still serde's sentence.
        let (field, reason) = failure::<Probe>("42");
        assert_eq!(field, "");
        assert!(reason.contains("invalid type"), "{reason}");
    }

    #[test]
    fn a_syntax_error_never_borrows_a_key() {
        // `trailing characters` is a syntax error; its position must not name the
        // `name` key the parsed prefix happened to contain.
        let (field, reason) = failure::<Probe>("{\"name\":\"x\"} nope");
        assert_eq!(field, "", "a syntax error names no field: {reason}");
    }

    #[test]
    fn an_unknown_field_keeps_its_exact_wording() {
        let (field, reason) = failure::<Probe>("{\"evil\":1}");
        assert_eq!(field, "evil");
        assert_eq!(
            reason, "unknown field",
            "the GUI keys its stale-server hint on this wording"
        );
    }

    #[test]
    fn a_missing_field_is_named_and_a_broken_body_still_says_why() {
        let (field, reason) = failure::<Probe>("{}");
        assert_eq!(field, "name");
        assert!(reason.contains("missing field"), "{reason}");

        let err = serde_json::from_str::<Probe>("{\"name\":").expect_err("not JSON");
        let mapped = deser_error(err, b"{\"name\":");
        assert_eq!(
            mapped.fields[0].field, "",
            "a body that is not JSON names no field"
        );
        assert!(
            !mapped.fields[0].message.is_empty(),
            "and still says what was wrong"
        );
    }
}
