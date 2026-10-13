// Structured error responses. Every failure leaves as
//
//   { "error": { "code": "...", "message": "...", "fields": [...] } }
//
// with no paths, stack traces or raw filesystem errors in the body. The
// machine-readable contract in openapi.json fixes this shape.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::domain::FieldError;
use crate::store::StoreError;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub fields: Vec<FieldError>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            fields: Vec::new(),
        }
    }

    pub fn validation(fields: Vec<FieldError>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "validation_failed",
            message: "payload failed validation".into(),
            fields,
        }
    }

    pub fn not_found(what: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("{what} does not exist"),
        )
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// The entry-lock state could not be verified (#185). Fail closed: the
    /// write is refused with a retry hint, never allowed because the check
    /// was inconclusive.
    pub fn lock_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "lock_unavailable",
            "could not verify entry lock state; retry shortly",
        )
    }

    /// A corrupt or unreadable stored document: server-side 500, logged with
    /// detail, generic message out.
    pub fn internal(context: String) -> Self {
        tracing::error!(%context, "internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "the request could not be completed",
        )
    }
}

/// The complete, emittable `Error.code` vocabulary (#249): every string any
/// code path can place in `error.code`. This is the single source of truth and
/// is kept in lockstep with the `Error.code` enum in `openapi.json` by the
/// `contract_error_code_vocabulary` test, which fails if the two drift apart.
/// `invalid_json` is deliberately absent — a malformed body is surfaced as
/// `validation_failed`/`bad_request`, never as `invalid_json` (#249).
pub const CODE_VOCABULARY: &[&str] = &[
    "validation_failed",
    "not_found",
    "conflict",
    "bad_request",
    "internal",
    "write_lock_busy",
    "lock_unavailable",
    "csrf",
    "unauthenticated",
    "forbidden",
    "rate_limited",
    "initialised",
    "invalid_credentials",
    "disabled",
    "inactive",
    "sso_invalid",
    "bad_signature",
    "bad_state",
    "no_vault",
    "vault_disabled",
    "calendar_not_configured",
    "calendar_fetch",
    "email_failed",
    "oauth_exchange",
];

impl ApiError {
    /// The machine-readable error-code vocabulary (#249), exactly as published
    /// in the `Error.code` enum of `openapi.json`.
    pub fn codes() -> &'static [&'static str] {
        CODE_VOCABULARY
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound => ApiError::not_found("resource"),
            StoreError::AlreadyExists(m) => ApiError::conflict(m),
            StoreError::Conflict(m) => ApiError::conflict(m),
            StoreError::RangeTooLarge => ApiError::bad_request(e.to_string()),
            StoreError::LockTimeout => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "write_lock_busy",
                "another write is in progress; retry shortly",
            ),
            StoreError::TooManyItems => {
                // A collection exceeded MAX_DOCS — a server-side limit, not a
                // client error; report generically without leaking the cap.
                ApiError::internal("collection exceeds the configured size limit".into())
            }
            StoreError::Io(detail) => ApiError::internal(detail),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "error": { "code": self.code, "message": self.message }
        });
        if !self.fields.is_empty() {
            body["error"]["fields"] = serde_json::to_value(&self.fields).unwrap_or_default();
        }
        let mut resp = (self.status, axum::Json(body)).into_response();
        // A busy write lock or an unverifiable lock state is transient; tell
        // the client when to retry (#62, #185).
        if matches!(self.code, "write_lock_busy" | "lock_unavailable")
            && let Ok(v) = "1".parse()
        {
            resp.headers_mut()
                .insert(axum::http::header::RETRY_AFTER, v);
        }
        resp
    }
}

// Rejected JSON payloads arrive through `ValidJson`; the shared 422 shape
// above is produced there, so no axum-rejection glue lives in this module.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_vocabulary_has_no_duplicates() {
        let codes = ApiError::codes();
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "CODE_VOCABULARY has duplicates");
    }

    #[test]
    fn constructors_emit_documented_codes() {
        // Every code a constructor can produce must be in the vocabulary, so
        // the enum can never silently omit a real, emittable code (#249).
        let emitted = [
            ApiError::validation(Vec::new()).code,
            ApiError::not_found("x").code,
            ApiError::conflict("x").code,
            ApiError::bad_request("x").code,
            ApiError::lock_unavailable().code,
            ApiError::internal("boom".into()).code,
        ];
        for code in emitted {
            assert!(
                ApiError::codes().contains(&code),
                "constructor code {code:?} is missing from CODE_VOCABULARY"
            );
        }
    }
}
