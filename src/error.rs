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

    /// No SMTP host is resolvable, so a test message cannot be sent (`POST
    /// /api/admin/email/test`). Distinct from `vault_disabled`: the vault may
    /// be fine and simply hold no `smtp.host` yet.
    pub fn email_not_configured() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "email_not_configured",
            "email is not configured; set an SMTP host first",
        )
    }

    /// The exchange-rate reference table could not be loaded (offline, an
    /// unreadable file, a malformed document). Fail-closed like the vault and
    /// the SSO allow-list: the rate is never guessed, so the caller can type
    /// one instead.
    pub fn fx_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "fx_unavailable",
            "exchange rates are unavailable; enter a rate manually",
        )
    }

    /// A request body over the transport cap (QA-06). Both the Content-Length
    /// pre-check and the chunked stream failure answer this shape; the limit
    /// itself is never echoed.
    pub fn payload_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "request body is too large; reduce its size and retry",
        )
    }

    /// A path that must be a directory exists as something else (QA-11). The
    /// operator has to fix the data tree; the path stays in the server log.
    pub fn data_dir_invalid() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "data_dir_invalid",
            "a data directory is not usable; stop the server and check TUCANO_DATA_DIR",
        )
    }

    /// The filesystem refused a write: a read-only mount or a permissions
    /// problem (QA-12). The underlying OS error stays in the server log.
    pub fn data_dir_not_writable() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "data_dir_not_writable",
            "the data directory is not writable; check its ownership and permissions",
        )
    }

    /// A stored document did not decode (QA-04/QA-12). The file name and the
    /// parse error are logged; the client only learns that the server log has
    /// the detail.
    pub fn corrupt_document(detail: impl Into<String>) -> Self {
        tracing::error!(detail = %detail.into(), "stored document could not be decoded");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "corrupt_document",
            "a stored document could not be read; see the server log",
        )
    }

    /// The path exists but not for this verb. Axum's own 405 is a bare status
    /// with an empty body, which tells a client nothing — including when it is
    /// talking to a server that does not serve the route it thinks it does.
    pub fn method_not_allowed(method: &str, allowed: &[&str]) -> Self {
        let message = match allowed.is_empty() {
            true => format!("{method} is not allowed here"),
            false => format!(
                "{method} is not allowed here; this endpoint accepts {}",
                allowed.join(", ")
            ),
        };
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            message,
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
    "method_not_allowed",
    "bad_request",
    "internal",
    "write_lock_busy",
    "lock_unavailable",
    "fx_unavailable",
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
    // An admin test send with no SMTP host configured.
    "email_not_configured",
    "oauth_exchange",
    // QA-05: a write refused because its collection is at MAX_DOCS.
    "collection_full",
    // QA-02: issue refused because the draft no longer matches the timesheet.
    "draft_out_of_date",
    // QA-06: a request body over the transport cap, on either read path.
    "payload_too_large",
    // QA-11: a data directory that should be a directory is something else.
    "data_dir_invalid",
    // QA-12: the filesystem refused a write (permissions, read-only mount).
    "data_dir_not_writable",
    // QA-04/QA-12: a stored document did not decode.
    "corrupt_document",
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
            // QA-05: the cap is now a *write-time* refusal and a client error:
            // the collection is still readable, so the caller can delete records
            // and retry. The configured cap itself is never echoed.
            StoreError::TooManyItems => ApiError::new(
                StatusCode::CONFLICT,
                "collection_full",
                "this collection has reached its size limit; delete unused records and retry",
            ),
            // QA-02: the timesheet moved after the draft was generated. Nothing
            // was issued (no status change, no archive, no entry lock).
            StoreError::DraftOutOfDate => ApiError::new(
                StatusCode::CONFLICT,
                "draft_out_of_date",
                "the timesheet changed after this draft was generated; regenerate the draft and issue it again",
            ),
            // QA-11: a collection path is a file (or something else that is not
            // a directory), so the collection cannot be read as empty.
            StoreError::DataDirInvalid => ApiError::data_dir_invalid(),
            // QA-12: permission denied / read-only filesystem.
            StoreError::NotWritable => ApiError::data_dir_not_writable(),
            // QA-04/QA-12: a document that does not decode. The detail (file and
            // parse error) is logged, never serialised.
            StoreError::CorruptDocument(detail) => ApiError::corrupt_document(detail),
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
            ApiError::payload_too_large().code,
            ApiError::data_dir_invalid().code,
            ApiError::data_dir_not_writable().code,
            ApiError::corrupt_document("x").code,
        ];
        for code in emitted {
            assert!(
                ApiError::codes().contains(&code),
                "constructor code {code:?} is missing from CODE_VOCABULARY"
            );
        }
    }
}
