//! Authentication and SSO: session issue/verify endpoints, bootstrap of the
//! first admin, and the #32 assertion consumer.

use super::*;
use crate::auth;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginInput {
    email: String,
    password: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapInput {
    name: String,
    email: String,
    password: String,
}

fn set_cookie(state: &AppState, user: &User) -> (http::HeaderMap, PublicUser) {
    let token = state.session.issue(user, state.clock.now());
    let mut headers = http::HeaderMap::new();
    if let Ok(v) = state.session.cookie(&token, 60 * 60 * 24).parse() {
        headers.insert(http::header::SET_COOKIE, v);
    }
    (headers, PublicUser::from(user))
}

/// Create the first administrator. Open only while no users exist.
pub async fn bootstrap(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<BootstrapInput>,
) -> ApiResult {
    if app.rate.is_locked("bootstrap") {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many attempts; try again later",
        ));
    }
    if store_blocking("bootstrap check", &app.store, |s| s.has_users()).await? {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "initialised",
            "an administrator already exists",
        ));
    }
    let email = normalise_email(&input.email)
        .ok_or_else(|| ApiError::validation(vec![FieldError::new("email", "invalid email")]))?;
    if !valid_password(&input.password) {
        return Err(ApiError::validation(vec![FieldError::new(
            "password",
            "must be at least 8 characters",
        )]));
    }
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(ApiError::validation(vec![FieldError::new(
            "name",
            "required, at most 120 characters",
        )]));
    }
    // Hash outside the lock; the check-then-insert is atomic in the store
    // (review A12/B4: two racing bootstraps must not mint two admins). The
    // argon2id work runs on the blocking pool (#254).
    let password = input.password.clone();
    let password_hash = blocking("password hashing", move || {
        auth::hash_password(&password)
            .map_err(|_| ApiError::internal("password hashing failed".into()))
    })
    .await?;
    let user = User {
        id: Uuid::new_v4(),
        name: name.to_owned(),
        email: email.clone(),
        role: Role::Admin,
        active: true,
        default_rate_minor: 0,
        cost_rate_minor: 0,
        password_hash,
        created_at: app.clock.now(),
        session_version: 1,
    };
    let user_for_write = user.clone();
    let store = app.store.clone();
    let inserted = blocking("bootstrap user write", move || {
        Ok(store.put_user_if_none(&user_for_write))
    })
    .await?;
    match inserted {
        Ok(()) => {}
        Err(crate::store::StoreError::AlreadyExists(_)) => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "initialised",
                "an administrator already exists",
            ));
        }
        Err(other) => return Err(other.into()),
    }
    app.rate.reset("bootstrap");
    record_audit(&app, "bootstrap", &email).await;
    let (headers, pubuser) = set_cookie(&app, &user);
    Ok((StatusCode::CREATED, headers, Json(pubuser)).into_response())
}

pub async fn login(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<LoginInput>,
) -> ApiResult {
    let email = normalise_email(&input.email).unwrap_or_default();
    let key = format!("login:{email}");
    if app.rate.is_locked(&key) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many failed attempts; try again later",
        ));
    }
    // #254: the user lookup (disk read) and the argon2id verify (deliberately
    // ~50–100 ms of CPU) run together on the blocking pool, so a login flood
    // cannot pin an async worker. The user-enumeration-safe shape is unchanged.
    let store = app.store.clone();
    let email_lookup = email.clone();
    let password = input.password.clone();
    let candidate = blocking("login verify", move || {
        let user = store.get_user_by_email(&email_lookup)?;
        Ok(match user {
            Some(u) if u.active && verify_password(&password, &u.password_hash) => Some(u),
            // Same response for unknown email and bad password (no enumeration).
            _ => None,
        })
    })
    .await?;
    let user = match candidate {
        Some(u) => u,
        None => {
            app.rate.record_failure(&key);
            record_audit(&app, "login_failed", &email).await;
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "invalid email or password",
            ));
        }
    };
    app.rate.reset(&key);
    record_audit(&app, "login_ok", &email).await;
    let (headers, pubuser) = set_cookie(&app, &user);
    Ok((headers, Json(pubuser)).into_response())
}

pub async fn logout(State(app): State<AppState>, req: axum::extract::Request) -> ApiResult {
    // Revoke the current session so the cookie stops working before expiry (#45).
    if let Some(header) = req
        .headers()
        .get(http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        && let Some(token) = token_from_cookie_header(header)
        && let Some(claims) = app.session.verify(token, app.clock.now())
    {
        // #254: the revocation write (fs + mutex) and the audit append run on
        // the blocking pool, off the async worker.
        let revocations = app.revocations.clone();
        let jti = claims.jti.clone();
        let exp = claims.exp;
        let now = app.clock.now();
        blocking("session revoke", move || {
            revocations.revoke(&jti, exp, now);
            Ok(())
        })
        .await?;
        record_audit(&app, "logout", &claims.uid.to_string()).await;
    }
    let mut headers = http::HeaderMap::new();
    let cookie = format!(
        "{}=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0",
        auth::SESSION_COOKIE
    );
    if app.session.secure() {
        // Rebuild with Secure when configured.
        let cookie = format!("{cookie}; Secure");
        if let Ok(v) = cookie.parse() {
            headers.insert(http::header::SET_COOKIE, v);
        }
    } else if let Ok(v) = cookie.parse() {
        headers.insert(http::header::SET_COOKIE, v);
    }
    Ok((headers, StatusCode::NO_CONTENT).into_response())
}

pub async fn me(State(app): State<AppState>, req: axum::extract::Request) -> ApiResult {
    let user = authenticate(&app, req.headers()).await?;
    Ok(Json(PublicUser::from(&user)).into_response())
}

/// Public: whether an administrator exists yet (drives first-run setup UI).
pub async fn auth_status(State(app): State<AppState>) -> ApiResult {
    Ok(Json(serde_json::json!({ "initialised": app.store.has_users()? })).into_response())
}

// --------------------------------------------------------------------- sso --

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SsoAssertionInput {
    pub provider: String,
    /// Raw signed assertion (compact token or base64 SAML statement).
    pub payload: String,
    #[serde(default)]
    pub signature: String,
}

/// Public: which SSO providers are configured (drives the login screen).
pub async fn sso_providers(State(app): State<AppState>) -> ApiResult {
    Ok(Json(serde_json::json!({ "providers": app.sso.names() })).into_response())
}

/// SSO assertion consumer (#32). Verifies the provider signature, maps groups
/// to the #19 roles, and just-in-time provisions the user, then issues the
/// same session cookie as local login. Local login keeps working alongside.
pub async fn sso_assertion(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<SsoAssertionInput>,
) -> ApiResult {
    let idp = provider_or_400(app.sso.get(&input.provider), "SSO")?;
    // Review A12: assertion verification gets the same lockout treatment as
    // login, so repeated forged/failed assertions cannot be brute-forced.
    let rate_key = format!("sso:{provider}", provider = input.provider);
    if app.rate.is_locked(&rate_key) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many failed attempts; try again later",
        ));
    }
    let identity = idp.verify(&input.payload, &input.signature).map_err(|e| {
        app.rate.record_failure(&rate_key);
        match e {
            crate::sso::SsoError::BadSignature | crate::sso::SsoError::Expired => ApiError::new(
                StatusCode::UNAUTHORIZED,
                "sso_invalid",
                "assertion failed verification",
            ),
            other => {
                tracing::warn!(error = %other, "SSO assertion rejected");
                app.rate.reset(&rate_key); // well-formed-but-invalid is not an attack signal
                ApiError::bad_request("assertion rejected")
            }
        }
    })?;
    app.rate.reset(&rate_key);

    let lookup_email = identity.email.clone();
    let existing = store_blocking("sso user lookup", &app.store, move |s| {
        s.get_user_by_email(&lookup_email)
    })
    .await?;
    let user = match existing {
        Some(u) => {
            if !u.active {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "inactive",
                    "this account is deactivated",
                ));
            }
            // Resolve this the same way as JIT provisioning so injected env
            // settings and config-file values govern every SSO login.
            let admin_group = app.cfg().get_str("sso_admin_group", app.env.as_ref());
            let role = crate::sso::map_role(&identity.groups, &admin_group);
            if role != u.role && !admin_group.is_empty() {
                let mut updated = u.clone();
                updated.role = role;
                updated.session_version += 1;
                let write = updated.clone();
                let expected = u.clone();
                store_blocking("sso role refresh", &app.store, move |s| {
                    s.put_user_if_unchanged(&write, &expected)
                })
                .await?;
                updated
            } else {
                u
            }
        }
        None => {
            // Just-in-time provisioning (#19): the IdP is the password store,
            // so local login gets an unsusable random password.
            let admin_group = app.cfg().get_str("sso_admin_group", app.env.as_ref());
            let role = crate::sso::map_role(&identity.groups, &admin_group);
            let temp_password = Uuid::new_v4().to_string();
            new_user(
                if identity.name.is_empty() {
                    identity.email.as_str()
                } else {
                    identity.name.as_str()
                },
                &identity.email,
                &temp_password,
                NewUser {
                    role,
                    active: true,
                    default_rate_minor: 0,
                    cost_rate_minor: 0,
                },
                &app,
            )
            .await?
        }
    };
    record_audit(&app, &format!("sso_login:{}", idp.name()), &identity.email).await;
    let (headers, pubuser) = set_cookie(&app, &user);
    Ok((headers, Json(pubuser)).into_response())
}

/// Recent security audit events (admin only, #52).
pub async fn audit_log(State(app): State<AppState>) -> ApiResult {
    let events = app.audit.recent(200);
    Ok(Json(serde_json::json!({ "events": events })).into_response())
}
