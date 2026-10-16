//! Keyset pagination for the list endpoints (#334).
//!
//! `GET /api/entries` and `GET /api/expenses` used to answer a whole request in
//! one body: the range is capped at `MAX_RANGE_DAYS` (400 days) and a collection
//! at `MAX_DOCS` (100 000), so a single response could serialise 100 000
//! documents — the first thing multi-user growth breaks, and a silent cut would
//! be worse, because a caller cannot tell "this is everything" from "there is
//! more". A page is bounded by `limit`, and the response says which of the two
//! it is: `next_cursor` is the opaque token for the following page, or `null`
//! when the page ended the set.
//!
//! **Order.** A page walk is only meaningful over a *total* order, and this is
//! the order the two endpoints walk:
//!
//! - `/api/entries`: `(date, id)` **ascending** — a timesheet reads forwards;
//! - `/api/expenses`: `(date, id)` **descending** — the register reads newest
//!   first.
//!
//! `id` breaks every tie, and it is exactly what makes the order total: it is
//! unique and immutable, so no two rows can compare equal and no write can move
//! a row to a position the walk has already passed. The direction is part of the
//! contract (see `openapi.json`), not an implementation detail.
//!
//! **Keyset, not offset.** The cursor carries the last `(date, id)` the endpoint
//! emitted, so the next page continues *after that record* — a write landing
//! between two page requests can neither skip a row (as an offset would, when a
//! row before the window is deleted) nor repeat one (when a row is inserted
//! before it).
//!
//! **Opacity.** The cursor is `base64url_nopad("YYYY-MM-DD|<uuid>")`, and that
//! encoding is genuinely opaque: a client sends a `next_cursor` back verbatim
//! and never parses it, which leaves the shape free to change without a break.
//! A cursor this service did not issue (or a hand-written one) is a 422 naming
//! `cursor`, never a silent restart from the beginning of the set — answering a
//! paging request with page one is how a caller loses rows without noticing.
//!
//! **No silent clamping.** A `limit` above [`MAX_LIMIT`] — or one that is not a
//! positive integer — is a 422 naming `limit`. Clamping would answer a request
//! for 5 000 rows with 1 000 and no signal that the other 4 000 were dropped.

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
