//! Exchange rates for the expense currency conversion.
//!
//! Rates come from the European Central Bank's free daily reference feed: one
//! small XML document, EUR-based, no key, published once a working day. The table
//! is fetched lazily and cached in-process **with its reference date**, and never
//! invented — with no table the caller gets [`FxError::Unavailable`] and the API
//! answers `503 fx_unavailable` (fail-closed, like the vault and SSO allow-list).
//! `TATU_FX_RATES_FILE` points the loader at a local copy of the *same* document
//! instead of the network — the seam the contract tests use offline, and an
//! air-gapped deployment story; a configured file is read on every call and never
//! populates the network cache, so the two sources cannot contaminate each other.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{NaiveDate, Utc};

/// The ECB's daily reference document (EUR-based, no key, no quota).
pub const ECB_URL: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml";

/// Settings/env key for the offline rates file.
pub const RATES_FILE_ENV: &str = "TATU_FX_RATES_FILE";

/// How long a fetched table is served before a refresh is attempted.
const CACHE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// The published document is a few kilobytes; anything larger is not it.
const MAX_DOC_BYTES: usize = 512 * 1024;

/// One conversion, as handed to a client.
#[derive(Debug, Clone, PartialEq)]
pub struct Rate {
    /// Units of `to` per one unit of `from`.
    pub rate: f64,
    /// The reference date the rate was published for. `None` for the identity
    /// conversion of a currency with itself, which needs no table at all (and
    /// therefore works offline).
    pub date: Option<NaiveDate>,
}

/// The ECB's published table: units of each currency per 1 EUR.
#[derive(Debug, Clone, PartialEq)]
pub struct Rates {
    pub date: NaiveDate,
    per_eur: HashMap<String, f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FxError {
    /// No rate table could be loaded (offline, unreadable or malformed
    /// document) — the rate is never guessed.
    Unavailable,
    /// The currency is not in the reference table.
    UnknownCurrency,
}

impl Rates {
    /// `from → to`, derived from the EUR-based table. EUR is implied at 1.
    pub fn rate(&self, from: &str, to: &str) -> Result<Rate, FxError> {
        let per_eur = |code: &str| match code {
            "EUR" => Ok(1.0),
            other => self
                .per_eur
                .get(other)
                .copied()
                .ok_or(FxError::UnknownCurrency),
        };
        let from_rate = per_eur(from)?;
        let to_rate = per_eur(to)?;
        Ok(Rate {
            rate: to_rate / from_rate,
            date: Some(self.date),
        })
    }
}

/// Parse the ECB document.
///
/// Deliberately a tiny scanner rather than an XML dependency: the shape is
/// fixed — `<Cube time='YYYY-MM-DD'>` once, then `<Cube currency='XXX'
/// rate='N'/>` per currency — and the project keeps its dependency surface
/// small.
pub fn parse_ecb(xml: &str) -> Option<Rates> {
    let date = NaiveDate::parse_from_str(attr(xml, "time='")?, "%Y-%m-%d").ok()?;
    let mut per_eur = HashMap::new();
    for chunk in xml.split("currency='").skip(1) {
        let Some((code, rest)) = chunk.split_once('\'') else {
            continue;
        };
        let Some(rate) = attr(rest, "rate='").and_then(|raw| raw.parse::<f64>().ok()) else {
            continue;
        };
        if code.len() == 3 && code.chars().all(|c| c.is_ascii_alphabetic()) && rate > 0.0 {
            per_eur.insert(code.to_ascii_uppercase(), rate);
        }
    }
    if per_eur.is_empty() {
        return None;
    }
    Some(Rates { date, per_eur })
}

/// The value of the `key` attribute (e.g. `time='`), up to its closing quote.
fn attr<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let start = text.find(key)? + key.len();
    let rest = &text[start..];
    Some(&rest[..rest.find('\'')?])
}

/// The rates document this process should serve, in precedence order: an
/// explicit override (see [`set_rates_document`]), the configured offline file,
/// then nothing — the network cache takes over.
fn document() -> Option<String> {
    if let Ok(guard) = override_doc().lock()
        && let Some(doc) = guard.as_ref()
    {
        return Some(doc.clone());
    }
    rates_file()
}

fn override_doc() -> &'static Mutex<Option<String>> {
    static OVERRIDE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

/// Pins the rates document for this process, bypassing both the configured file
/// and the network. The contract tests use it — they must run offline, and an
/// env-var seam would be both unsafe to mutate and shared between parallel
/// tests. An embedder can use it the same way. `None` restores the normal path.
pub fn set_rates_document(doc: Option<String>) {
    if let Ok(mut guard) = override_doc().lock() {
        *guard = doc;
    }
}

/// The configured offline file, when it holds something.
fn rates_file() -> Option<String> {
    let path = std::env::var(RATES_FILE_ENV).ok()?;
    if path.trim().is_empty() {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn fetch() -> Option<String> {
    let mut response = ureq::get(ECB_URL).call().ok()?;
    let text = response.body_mut().read_to_string().ok()?;
    (text.len() <= MAX_DOC_BYTES).then_some(text)
}

type Cache = Mutex<Option<(Instant, Rates)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// The process-wide table, refreshed once the TTL has passed. A failed refresh
/// keeps serving the last known table: its date travels with every rate, so a
/// caller can always tell how fresh it is. With no table at all this fails.
fn table() -> Result<Rates, FxError> {
    let mut guard = cache().lock().map_err(|_| FxError::Unavailable)?;
    if let Some((fetched_at, rates)) = guard.as_ref()
        && fetched_at.elapsed() < CACHE_TTL
    {
        return Ok(rates.clone());
    }
    let text = fetch().ok_or(FxError::Unavailable)?;
    let fresh = parse_ecb(&text).ok_or(FxError::Unavailable)?;
    *guard = Some((Instant::now(), fresh.clone()));
    Ok(fresh)
}

/// The conversion `from → to`.
///
/// Currencies are case-insensitive ISO-4217 codes. Converting a currency with
/// itself is the identity and needs no table, so it also works offline.
pub fn convert(from: &str, to: &str) -> Result<Rate, FxError> {
    let (from, to) = (from.to_ascii_uppercase(), to.to_ascii_uppercase());
    if from == to {
        return Ok(Rate {
            rate: 1.0,
            date: None,
        });
    }
    // An explicit document or a configured offline file is authoritative, and
    // read fresh every call (replacing the file takes effect without a restart).
    if let Some(text) = document() {
        return parse_ecb(&text)
            .ok_or(FxError::Unavailable)?
            .rate(&from, &to);
    }
    table()?.rate(&from, &to)
}

/// Today, for the identity conversion's audit trail.
pub fn today() -> NaiveDate {
    Utc::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gesmes:Envelope xmlns:gesmes="http://www.gesmes.org/xml/2002-08-01">
  <Cube>
    <Cube time='2026-10-14'>
      <Cube currency='USD' rate='1.0831'/>
      <Cube currency='GBP' rate='0.85210'/>
      <Cube currency='JPY' rate='163.42'/>
    </Cube>
  </Cube>
</gesmes:Envelope>"#;

    #[test]
    fn parses_the_reference_date_and_the_table() {
        let rates = parse_ecb(DOC).expect("the ECB document parses");
        assert_eq!(rates.date, NaiveDate::from_ymd_opt(2026, 10, 14).unwrap());
        assert_eq!(rates.per_eur.get("USD"), Some(&1.0831));
        assert_eq!(rates.per_eur.get("JPY"), Some(&163.42));
    }

    #[test]
    fn converts_through_the_eur_base_in_both_directions() {
        let rates = parse_ecb(DOC).unwrap();
        let usd_to_eur = rates.rate("USD", "EUR").unwrap();
        assert!((usd_to_eur.rate - 1.0 / 1.0831).abs() < 1e-9);
        assert_eq!(usd_to_eur.date, Some(rates.date));
        // A cross rate is derived from the two EUR legs.
        let gbp_to_usd = rates.rate("GBP", "USD").unwrap();
        assert!((gbp_to_usd.rate - 1.0831 / 0.85210).abs() < 1e-9);
        // EUR is implied at 1 without being listed.
        assert!((rates.rate("EUR", "EUR").unwrap().rate - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn an_unknown_currency_is_refused_rather_than_guessed() {
        let rates = parse_ecb(DOC).unwrap();
        assert_eq!(rates.rate("USD", "XXX"), Err(FxError::UnknownCurrency));
    }

    #[test]
    fn a_document_without_rates_is_not_a_table() {
        assert!(parse_ecb("<Cube time='2026-10-14'></Cube>").is_none());
        assert!(parse_ecb("").is_none());
        assert!(
            parse_ecb("<Cube currency='USD' rate='1.08'/>").is_none(),
            "no date"
        );
    }

    #[test]
    fn the_same_currency_needs_no_table_so_it_works_offline() {
        // No rates file and no network in this test: the identity still resolves.
        let rate = convert("eur", "EUR").expect("identity conversion");
        assert!((rate.rate - 1.0).abs() < f64::EPSILON);
        assert_eq!(rate.date, None);
    }
}
