//! Admin SMTP settings surface: read the effective transport, store new
//! settings in the vault, and send a test message (#35 follow-up).
//!
//! The password is write-only: it goes into the vault and is never serialised
//! back — `GET` reports only `has_password`. Settings are resolved from the
//! vault/env on every send (`DynamicEmailSender`), so a `PUT` here takes effect
//! on the next message without a restart.

use super::vault_config::require_vault;
use super::*;

/// The same bound the Settings form applies, enforced server-side too: a host
/// or username is a single identifier, never a document (SET-09).
const MAX_SMTP_FIELD_CHARS: usize = 255;
/// The audit log records an identifier, not a payload; a hostile or accidental
/// 5 000-char host must not land there verbatim (SET-09).
const AUDIT_SUBJECT_MAX_CHARS: usize = 64;

/// Shorten an audit subject to [`AUDIT_SUBJECT_MAX_CHARS`], appending an
/// ellipsis when truncated. Credentials never reach this function.
fn audit_subject(value: &str) -> String {
    if value.chars().count() <= AUDIT_SUBJECT_MAX_CHARS {
        return value.to_string();
    }
    let mut short: String = value.chars().take(AUDIT_SUBJECT_MAX_CHARS).collect();
    short.push('…');
    short
}

/// The `GET`/`PUT` payload. `password` is only ever an input.
fn settings_view(app: &AppState) -> serde_json::Value {
    let vault = app.vault.get();
    let resolved = crate::email::SmtpConfig::from_sources(vault.as_deref());
    let configured = resolved.is_some();
    let host = resolved
        .as_ref()
        .map(|c| c.host.clone())
        .unwrap_or_default();
    let port = resolved.as_ref().map(|c| c.port).unwrap_or(25);
    let username = resolved
        .as_ref()
        .and_then(|c| c.username.clone())
        .unwrap_or_default();
    let from = resolved
        .as_ref()
        .map(|c| c.from.clone())
        .unwrap_or_default();
    // Only whether a password exists — never the value itself.
    let has_password = resolved
        .as_ref()
        .is_some_and(|c| c.password.as_ref().is_some_and(|p| !p.is_empty()));
    serde_json::json!({
        "vault_enabled": app.vault.is_enabled(),
        "configured": configured,
        "transport": if configured { "smtp" } else { "disabled" },
        "host": host,
        "port": port,
        "username": username,
        "from": from,
        "has_password": has_password,
    })
}

/// `GET /api/admin/email` — the effective SMTP settings, never the password.
pub async fn admin_email_get(State(app): State<AppState>) -> ApiResult {
    Ok(Json(settings_view(&app)).into_response())
}

/// Body for `PUT /api/admin/email`. `port` is required; `username`, `password`
/// and `from` are omitted when they should keep their stored value.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailSettingsInput {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
}

/// `PUT /api/admin/email` — validate, then persist the SMTP settings as one
/// encrypted vault write. Vault disabled is a fail-closed 503 and mutates
/// nothing; an invalid field is a 422 and mutates nothing.
pub async fn admin_email_put(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<EmailSettingsInput>,
) -> ApiResult {
    // H1: every secret write path takes the gate *before* resolving the vault,
    // so a concurrent disable/regenerate cannot delete or rotate the key under
    // this `put_many`.
    let gate = app.vault.gate();
    let vault = require_vault(&app)?;

    let mut errors: Vec<FieldError> = Vec::new();
    let host = input.host.trim();
    if host.is_empty() {
        errors.push(FieldError::new("host", "SMTP host is required"));
    } else if host.chars().count() > MAX_SMTP_FIELD_CHARS {
        errors.push(FieldError::new(
            "host",
            format!("at most {MAX_SMTP_FIELD_CHARS} characters"),
        ));
    } else if host.chars().any(char::is_whitespace) || crate::email::has_control_characters(host) {
        errors.push(FieldError::new(
            "host",
            "SMTP host must not contain spaces or control characters",
        ));
    }
    // `u16` already caps at 65535 on the wire; zero is the remaining bad value.
    if input.port == 0 {
        errors.push(FieldError::new("port", "port must be between 1 and 65535"));
    }
    let username = input.username.as_deref().map(str::trim);
    if let Some(username) = username {
        if username.chars().count() > MAX_SMTP_FIELD_CHARS {
            errors.push(FieldError::new(
                "username",
                format!("at most {MAX_SMTP_FIELD_CHARS} characters"),
            ));
        } else if crate::email::has_control_characters(username) {
            errors.push(FieldError::new(
                "username",
                "username must not contain control characters",
            ));
        }
    }
    let from = input.from.as_deref().map(str::trim);
    if let Some(from) = from
        && !crate::email::valid_address(from)
    {
        errors.push(FieldError::new("from", "not a valid email address"));
    }
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }

    let mut entries: Vec<(String, String)> = vec![
        ("smtp.host".into(), host.to_string()),
        ("smtp.port".into(), input.port.to_string()),
    ];
    // Omitted optional fields keep whatever the vault already holds, so a
    // settings edit without a password never wipes the stored one.
    if let Some(username) = username {
        entries.push(("smtp.username".into(), username.to_string()));
    }
    if let Some(password) = input.password.as_deref() {
        entries.push(("smtp.password".into(), password.to_string()));
    }
    if let Some(from) = from {
        entries.push(("smtp.from".into(), from.to_string()));
    }
    vault
        .put_many(&entries)
        .map_err(|e| ApiError::internal(format!("vault write failed: {e}")))?;
    // No await while the gate is held; release it before the audit write.
    drop(gate);
    // Audit the host only — credentials never reach the audit log, and the
    // subject is bounded so an oversized host cannot flood it (SET-09).
    record_audit(&app, "email_settings_updated", &audit_subject(host)).await;
    Ok(Json(settings_view(&app)).into_response())
}

/// Body for `POST /api/admin/email/test`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailTestInput {
    pub to: String,
}

/// `POST /api/admin/email/test` — send one short message through the transport
/// that is live *now*. `422 email_not_configured` when no host resolves,
/// `502 email_failed` when the relay refuses.
pub async fn admin_email_test(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<EmailTestInput>,
) -> ApiResult {
    if let Some(problem) = crate::email::recipient_problem(&input.to) {
        return Err(ApiError::validation(vec![FieldError::new("to", problem)]));
    }
    let vault = app.vault.get();
    if crate::email::SmtpConfig::from_sources(vault.as_deref()).is_none() {
        return Err(ApiError::email_not_configured());
    }
    let to = input.to.trim().to_string();
    let msg = crate::email::EmailMessage {
        to: vec![to.clone()],
        subject: "TucanoTime test email".into(),
        text: "This is a test email from TucanoTime. If you can read it, SMTP delivery works.\n"
            .into(),
        html: None,
        attachment: None,
        from_name: None,
        reply_to: None,
    };
    let sender = app.email.clone();
    let transport = sender.transport();
    blocking("email test send", move || {
        sender.send(&msg).map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                "email_failed",
                e.to_string(),
            )
        })
    })
    .await?;
    record_audit(&app, "email_test_sent", &to).await;
    Ok(Json(serde_json::json!({ "sent_to": to, "transport": transport })).into_response())
}
