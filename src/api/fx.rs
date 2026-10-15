//! `GET /api/fx` (#…) — the exchange rate for one currency pair.
//!
//! Read-only and side-effect free: it never writes, and it never guesses. When
//! the reference table cannot be loaded it answers `503 fx_unavailable` so the
//! caller can fall back to a rate typed by hand.

use axum::Json;
use axum::extract::Query;
use axum::response::IntoResponse;

use super::ApiResult;
use crate::domain::FieldError;
use crate::error::ApiError;
use crate::fx::{self, FxError};

/// `?from=USD&to=EUR` — ISO-4217 codes, matched case-insensitively.
#[derive(Debug, serde::Deserialize)]
pub struct FxQuery {
    from: String,
    to: String,
}

/// Validates one currency code and normalises it to upper case.
fn code(field: &str, raw: &str) -> Result<String, ApiError> {
    let code = raw.trim().to_ascii_uppercase();
    if code.len() == 3 && code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Ok(code);
    }
    Err(ApiError::validation(vec![FieldError::new(
        field,
        "must be a three-letter ISO-4217 currency code",
    )]))
}

pub async fn fx_rate(Query(q): Query<FxQuery>) -> ApiResult {
    let from = code("from", &q.from)?;
    let to = code("to", &q.to)?;
    let rate = fx::convert(&from, &to).map_err(|err| match err {
        FxError::UnknownCurrency => ApiError::validation(vec![FieldError::new(
            "to",
            format!("no published reference rate for {from}/{to}"),
        )]),
        // Fail-closed: never invent a rate, and let the caller type one instead.
        FxError::Unavailable => ApiError::fx_unavailable(),
    })?;
    // `date` is null for a currency converted with itself: that identity needs
    // no published table, so it also answers offline.
    let date = rate.date.map(|d| d.to_string());
    Ok(Json(serde_json::json!({
        "from": from,
        "to": to,
        "rate": rate.rate,
        "date": date,
        "source": if date.is_some() { "ecb" } else { "identity" },
    }))
    .into_response())
}
