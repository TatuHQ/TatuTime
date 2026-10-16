//! Keyset pagination for the list endpoints (#334).
//!
//! A page is bounded by `limit`; `next_cursor` is the opaque token for the next
//! page, or `null` when the set ended — so a caller can always tell "this is
//! everything" from "there is more". The walk is a *total* order, `(date, id)`:
//! ascending for `/api/entries`, descending for `/api/expenses` (the register
//! reads newest first). A keyset cursor continues *after* the last record, so a
//! write landing between two page requests can neither skip nor repeat a row. A
//! `cursor` or `limit` this service did not issue — or a `limit` above
//! [`MAX_LIMIT`] — is a 422 naming the field, never a silent restart from page
//! one or a silent clamp.

use base64::Engine as _;
use chrono::NaiveDate;
use uuid::Uuid;

use crate::domain::FieldError;
use crate::error::ApiError;

/// Page size when the caller does not ask for one: a week for a whole team, or
/// a day for a heavy user, fits many times over — and a caller that needs more
/// follows `next_cursor`, so the default is a bound, never a loss.
pub const DEFAULT_LIMIT: usize = 200;

/// The largest page this service will build. A response is bounded to
/// `MAX_LIMIT` documents — two orders of magnitude below `MAX_DOCS` — and a
/// larger `limit` is refused rather than clamped.
pub const MAX_LIMIT: usize = 1000;

/// The direction the endpoint walks its `(date, id)` order.
#[derive(Debug, Clone, Copy)]
pub enum Order {
    Ascending,
    Descending,
}

/// A resolved `limit`/`cursor` pair.
pub struct PageRequest {
    pub limit: usize,
    /// The last key the previous page emitted; the next page starts after it.
    pub cursor: Option<(NaiveDate, Uuid)>,
}

/// Resolve `limit`/`cursor` from the raw query strings.
///
/// Both arrive as strings so that a malformed value is *this* 422 — naming the
/// field, in the app's one error shape — rather than a query-extractor
/// rejection from the framework, which would answer in a shape of its own.
pub fn page_request(
    limit: Option<&String>,
    cursor: Option<&String>,
) -> Result<PageRequest, ApiError> {
    let limit = match limit {
        None => DEFAULT_LIMIT,
        Some(raw) => raw
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or_else(|| {
                ApiError::validation(vec![FieldError::new(
                    "limit",
                    format!("'limit' must be an integer between 1 and {MAX_LIMIT}"),
                )])
            })?,
    };
    let cursor = cursor.map(|raw| decode_cursor(raw)).transpose()?;
    Ok(PageRequest { limit, cursor })
}

/// Emit one page of `items` plus the cursor for the next one, or `None` when
/// this page ended the set.
///
/// `items` must already be in the endpoint's documented order and already
/// filtered to what the caller may see. Because the caller passes the *whole*
/// remainder of the matching set, "is there more" is answered exactly: a
/// non-null cursor means precisely that rows were left behind, and the cursor is
/// the key of the last row emitted — so following it can neither skip nor repeat
/// a row.
pub fn paginate<T, K>(
    items: Vec<T>,
    key: K,
    req: &PageRequest,
    order: Order,
) -> (Vec<T>, Option<String>)
where
    K: Fn(&T) -> (NaiveDate, Uuid),
{
    let mut rest: Vec<T> = match req.cursor {
        None => items,
        Some(stop) => items
            .into_iter()
            .filter(|item| match order {
                Order::Ascending => key(item) > stop,
                Order::Descending => key(item) < stop,
            })
            .collect(),
    };
    let more = rest.len() > req.limit;
    rest.truncate(req.limit);
    let next_cursor = match more {
        true => rest.last().map(|item| encode_cursor(key(item))),
        false => None,
    };
    (rest, next_cursor)
}

/// The last emitted key, encoded. See the module docs: opaque to clients.
fn encode_cursor((date, id): (NaiveDate, Uuid)) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{date}|{id}"))
}

fn decode_cursor(raw: &str) -> Result<(NaiveDate, Uuid), ApiError> {
    let invalid = || {
        ApiError::validation(vec![FieldError::new(
            "cursor",
            "'cursor' is not a cursor this endpoint issued",
        )])
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.trim())
        .map_err(|_| invalid())?;
    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
    let (date, id) = text.split_once('|').ok_or_else(invalid)?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| invalid())?;
    let id = Uuid::parse_str(id).map_err(|_| invalid())?;
    Ok((date, id))
}
