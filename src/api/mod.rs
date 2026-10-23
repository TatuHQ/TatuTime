//! The REST API: routing, extractors and handlers (#100).
//!
//! Handlers validate, then hand off to `store`; nothing touches the filesystem
//! except through it, and nothing returns internal detail to a client. This
//! module owns the shared surface — `AppState`, the request extractors, the
//! route guards and the cross-cutting helpers the handlers reuse. The endpoint
//! bodies live in the feature submodules below; `lib.rs` keeps the central
//! route table and the `pub use` re-exports keep `api::<handler>` paths stable.

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
    Project, ProjectCode, ProjectInput, Source, StartTimerInput, Task, TaskInput, Timer,
    elapsed_hundredths, generate_invoice, validate_category_input, validate_customer_input,
    validate_entry_input, validate_expense_input, validate_project_input, validate_task_input,
};
use crate::domain::{Submission, SubmissionState};
use crate::error::ApiError;
use crate::report;
use crate::store::{Store, StoreError};

pub mod admin_email;
pub mod auth;
pub mod backup;
pub mod entries_reports;
pub mod expenses;
pub mod fx;
pub mod imports;
pub mod invoicing;
pub mod pagination;
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
        // QA-06: the chunked read path reaches this extractor (a Content-Length
        // over the cap is refused by `RequestBodyLimitLayer` before routing and
        // is rewritten by the API's response-mapping layer). Preserve the
        // transport's 413 as `payload_too_large`; every other body-read failure
        // stays the generic `bad_request`. Neither branch echoes the limit.
        let body = Bytes::from_request(req, state).await.map_err(|rejection| {
            if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::payload_too_large()
            } else {
                ApiError::bad_request("failed to read the request body")
            }
        })?;
        let value = serde_json::from_slice::<T>(&body).map_err(|e| deser_error(e, &body))?;
        Ok(ValidJson(value))
    }
}

/// An **optional** JSON body: an absent `Content-Type` means "no body" and
/// yields `None`, while a present body is parsed with the same
/// `deny_unknown_fields`/422 mapping as [`ValidJson`].
///
/// axum 0.8's own `Option<Json<T>>` treats a deserialisation failure as a
/// `JsonRejection` whose plain-text 422 the API layer would rewrite into a
/// generic 400 — losing `validation_failed` and the field name. This adapter
/// keeps the app's error shape, which is why the endpoint uses it instead.
/// Like every body extractor it must be the handler's last argument.
pub struct OptionalValidJson<T>(pub Option<T>);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for OptionalValidJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Some(content_type) = req.headers().get(http::header::CONTENT_TYPE) else {
            return Ok(Self(None));
        };
        let json = content_type
            .to_str()
            .map(|ct| {
                let ct = ct.trim().to_ascii_lowercase();
                ct.starts_with("application/json") || ct.contains("+json")
            })
            .unwrap_or(false);
        if !json {
            return Err(ApiError::bad_request("expected application/json"));
        }
        let body = Bytes::from_request(req, state).await.map_err(|rejection| {
            if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::payload_too_large()
            } else {
                ApiError::bad_request("failed to read the request body")
            }
        })?;
        let value = serde_json::from_slice::<T>(&body).map_err(|e| deser_error(e, &body))?;
        Ok(Self(Some(value)))
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
/// serde quotes the *value* as readily as the key, so a field is only the token
/// after one of its markers; a *type* mismatch carries none, so [`key_at`]
/// recovers it from the raw body (#338). Otherwise the field stays empty and the
/// reason is serde's own sentence — `unknown field` keeps its exact wording, which
/// the GUI keys on to spot a server older than the page.
///
/// ```rust
/// {"name": 42}            // field `name`, recovered from the body (#338)
/// {"inner": {"name": 42}} // field `name`, the leaf
/// ```
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
    let offset = if matches!(body.get(offset + 1), Some(b'[' | b'{')) {
        offset + 1
    } else {
        offset
    };
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

/// The environment lookup injected into [`AppState`]: `TUCANO_*` reads at
/// request time use this seam instead of `std::env`, so tests can exercise
/// operator-managed settings without mutating process-wide state.
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub clock: Arc<dyn crate::clock::Clock>,
    pub locks: Arc<dyn crate::lock::EntryLock>,
    pub session: Arc<Session>,
    pub rate: Arc<crate::ratelimit::RateLimiter>,
    pub audit: Arc<crate::audit::AuditLog>,
    pub revocations: Arc<crate::revoke::Revocations>,
    /// Swappable credential vault (#77): the admin API can enable, disable or
    /// regenerate the key without a restart.
    pub vault: Arc<crate::vault::VaultSlot>,
    /// Email transport (#35). Disabled unless SMTP is configured; tests inject
    /// a recording sender.
    pub email: Arc<dyn crate::email::EmailSender>,
    /// Accounting sync providers (#33). Empty registry disables the endpoints.
    pub accounting: Arc<crate::accounting::AccountingRegistry>,
    /// Identity providers for SSO (#32). Empty registry disables SSO.
    pub sso: Arc<crate::sso::SsoRegistry>,
    /// Pending OAuth calendar consent flows (#36).
    pub oauth_flows: Arc<crate::calendar_oauth::OAuthFlows>,
    /// Externalised runtime configuration (#94): env > config.json > default.
    /// A `Mutex` swap so `PUT /admin/config` can hot-update request-time knobs.
    pub cfg: Arc<std::sync::Mutex<Arc<crate::appconfig::AppConfig>>>,
    /// Environment lookup used by request-time resolution (the vault lifecycle
    /// reads `TUCANO_VAULT_ENABLED` / `TUCANO_SECRET_KEY(_FILE)` the same way
    /// boot does). Defaults to the process environment; tests inject a map so
    /// operator-managed 409s are exercised without mutating process env.
    pub env: EnvLookup,
}

impl AppState {
    /// Convenience wiring with an ephemeral session key (dev/tests). Production
    /// uses `with_session` and a configured secret.
    #[doc(hidden)]
    pub fn new(store: Store) -> Self {
        Self::with_session(store, Arc::new(ephemeral_session()))
    }

    /// Lenient test/embedding convenience — **not** the production boot path.
    ///
    /// Unlike [`AppState::boot`], a vault that cannot be resolved degrades to
    /// a disabled slot (the error is logged, never propagated), and a corrupt
    /// `config.json` is replaced with defaults. That is deliberate for test
    /// harnesses and small embeddings, but it means a caller cannot tell
    /// "disabled by policy" from "disabled by a broken key". Production
    /// (`main`) must keep using the hard-checked [`AppState::boot`] path.
    #[doc(hidden)]
    pub fn with_session(store: Store, session: Arc<Session>) -> Self {
        // Lenient path for tests/dev: resolve config + vault from env, ignoring
        // errors (main() uses `boot`, which arrives with hard-checked values).
        let cfg = Arc::new(
            crate::appconfig::AppConfig::load(store.root()).unwrap_or_else(|e| {
                tracing::error!("config.json unusable ({e}); starting with defaults");
                crate::appconfig::AppConfig::empty()
            }),
        );
        let enabled = cfg.get_bool("vault_enabled", &crate::appconfig::process_env);
        let vault = crate::vault::open_for_boot(
            store.root(),
            std::env::var("TUCANO_SECRET_KEY").ok(),
            std::env::var("TUCANO_SECRET_KEY_FILE").ok(),
            enabled,
        )
        .unwrap_or_else(|e| {
            tracing::error!("vault bootstrap failed ({e}); starting with the vault disabled");
            crate::vault::VaultBoot::disabled(crate::vault::DisabledReason::Config)
        });
        Self::boot(store, session, vault, cfg)
    }

    /// The canonical constructor (#94): main() resolves config + vault with
    /// fail-fast policy and hands them over already validated.
    #[must_use]
    pub fn boot(
        store: Store,
        session: Arc<Session>,
        vault: crate::vault::VaultBoot,
        cfg: Arc<crate::appconfig::AppConfig>,
    ) -> Self {
        let store = Arc::new(store);
        let locks = Arc::new(crate::lock::CombinedLocks::new(vec![
            Box::new(crate::lock::ImportLock::new(store.clone())),
            Box::new(crate::lock::InvoiceLock::new(store.clone())),
            Box::new(crate::lock::SubmissionLock::new(store.clone())),
            // #255/#190: reimbursement claims (#24) lock their expenses through
            // the same seam instead of a direct scan in `api::expenses`.
            Box::new(crate::lock::ClaimLock::new(store.clone())),
        ]));
        let audit = Arc::new(crate::audit::AuditLog::new(store.root()));
        let revocations = Arc::new(crate::revoke::Revocations::new(store.root()));
        let vault = crate::vault::VaultSlot::new(vault);
        // Email adapter (#35): a *dynamic* transport that re-resolves the
        // vault/env SMTP settings on every send, so `PUT /api/admin/email` and
        // an admin-API key change apply to the next message without a restart.
        // Tests replace it wholesale through `with_email` and never touch SMTP.
        let email: Arc<dyn crate::email::EmailSender> =
            Arc::new(crate::email::DynamicEmailSender::new(vault.clone()));
        // Accounting and SSO provider registries stay boot-time snapshots: an
        // enable/regenerate applies to them on the next restart (only the email
        // transport re-resolves the vault live).
        let boot_vault = vault.get();
        // Accounting providers (#33): enabled when an OAuth token exists.
        let accounting = crate::accounting::registry_from_vault(boot_vault.as_deref(), &cfg);
        // SSO identity providers (#32): enabled when OIDC/SAML config exists.
        let sso = Arc::new(crate::sso::registry_from_vault(boot_vault.as_deref(), &cfg));
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
            accounting,
            sso,
            oauth_flows: Arc::new(crate::calendar_oauth::OAuthFlows::new()),
            cfg: Arc::new(std::sync::Mutex::new(cfg)),
            env: Arc::new(crate::appconfig::process_env),
            store,
        }
    }

    /// Resolve an environment variable exactly as boot does. Request handlers
    /// must use this seam (not `std::env` directly) so tests can inject a map.
    #[must_use]
    pub fn env_var(&self, key: &str) -> Option<String> {
        (self.env)(key)
    }

    /// Replace the environment lookup (tests inject operator flags without
    /// mutating process-wide environment, which is unsafe and racy).
    #[must_use]
    pub fn with_env(mut self, env: EnvLookup) -> Self {
        self.env = env;
        self
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
    /// Replaces the dynamic vault-resolving sender entirely: an injected
    /// transport is used verbatim for every send.
    #[must_use]
    pub fn with_email(mut self, sender: Arc<dyn crate::email::EmailSender>) -> Self {
        self.email = sender;
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

/// The accounting/sso provider lookup shape: a disabled or unknown provider
/// is a 400 naming the kind, byte-identical to the copies this replaces.
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

/// `?from=YYYY-MM-DD&to=YYYY-MM-DD` as a typed query (#100). Unknown params are
/// tolerated and missing ones keep the documented message.
#[derive(Debug, serde::Deserialize)]
pub struct RangeQuery {
    #[serde(default)]
    pub(crate) from: Option<String>,
    #[serde(default)]
    pub(crate) to: Option<String>,
}

impl RangeQuery {
    /// Resolve both bounds, with the documented errors.
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
    // Liveness plus the build identity (version + commit): the GUI footer reads
    // it instead of hardcoding a version, so a page can never claim a build the
    // server is not running (`buildinfo`).
    Json(crate::buildinfo::health())
}

// Re-exports keep the central route table in `lib.rs` and `crate::api::…`
// paths elsewhere in the crate stable (#100).
pub use self::admin_email::{
    EmailSettingsInput, EmailTestInput, admin_email_get, admin_email_put, admin_email_test,
};
pub use self::auth::{
    BootstrapInput, LoginInput, SsoAssertionInput, audit_log, auth_status, bootstrap, login,
    logout, me, sso_assertion, sso_providers,
};
pub use self::backup::{BackupSettingsInput, backup_settings, run_backup, update_backup_settings};
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
pub use self::imports::{
    CommitImportInput, CreateBatchInput, ImportPageQuery, RowsQuery, commit_import_batch,
    create_import_batch, delete_import_batch, download_import_source, get_import_batch,
    import_rows, imported_invoice_pdf, list_import_batches, preview_import_batch,
    review_import_batch, upload_import_source,
};
pub use self::invoicing::{
    InvoiceEditInput, InvoiceInput, ManualInvoiceInput, PayInput, SyncInput, create_invoice,
    create_item_type, create_manual_invoice, delete_invoice, delete_item_type, get_invoice_handler,
    invoice_document, invoice_export_csv, invoice_pdf, invoice_pdf_preview,
    invoice_receipts_handler, invoice_report_handler, invoice_summary, invoice_template_get,
    invoice_template_preview, invoice_template_put, invoice_timeline, issue_invoice, list_invoices,
    list_item_types, mark_invoice_sent, money_for_email, org_get, org_put, pay_invoice,
    preview_invoice, preview_invoice_pdf, revise_invoice, send_invoice_email,
    send_invoice_email_copy, sync_invoice, sync_status, update_invoice_draft, update_item_type,
    write_off_invoice,
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
    SecretInput, VaultConfirmInput, admin_config, delete_secret, list_secrets, set_secret,
    update_config, vault_disable, vault_enable, vault_regenerate, vault_status,
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
