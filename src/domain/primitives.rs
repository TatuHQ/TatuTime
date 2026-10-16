use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Number;

/// Hours as hundredths — the one hours convention: `1` = 0.01 h, so
/// `Hours(150)` is 1.50 h and `Hours(2400)` is 24.00 h (the maximum). A
/// duration is never held in a float; `amount_minor` does the exact integer
/// billing.
///
/// ```rust
/// let half = Hours(150);                       // 1.50 h
/// assert_eq!(half.amount_minor(6000), 9000);   // 1.5 h at 60.00/h = 90.00
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hours(pub u32);

impl Hours {
    pub const MIN: u32 = 1;
    pub const MAX: u32 = 2400;

    /// Exact `hours * rate_minor / 100`, rounded half up to a minor unit.
    pub fn amount_minor(self, rate_minor: u64) -> u64 {
        let numerator = u128::from(self.0) * u128::from(rate_minor);
        u64::try_from((numerator + 50) / 100).unwrap_or(u64::MAX)
    }

    fn parse(s: &str) -> Option<u32> {
        let s = s.trim();
        let (int, frac) = match s.split_once('.') {
            Some((i, f)) => {
                if f.is_empty() || f.len() > 2 || !f.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                (
                    i,
                    if f.len() == 1 {
                        format!("{f}0")
                    } else {
                        f.to_owned()
                    },
                )
            }
            None => (s, "00".to_owned()),
        };
        if int.is_empty() || int.len() > 2 || !int.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let hundredths: u32 = int.parse::<u32>().ok()? * 100 + frac.parse::<u32>().ok()?;
        if (Self::MIN..=Self::MAX).contains(&hundredths) {
            Some(hundredths)
        } else {
            None
        }
    }
}

impl<'de> Deserialize<'de> for Hours {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let n = Number::deserialize(d)?;
        // Number::to_string() is the shortest round-trip decimal, so it is an
        // exact textual form of the JSON token; parsing that as a decimal
        // avoids any binary float involvement.
        Self::parse(&n.to_string())
            .map(Self)
            .ok_or_else(|| serde::de::Error::custom("hours must be 0.01 to 24.00, two decimals"))
    }
}

impl Serialize for Hours {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let whole = self.0 / 100;
        let frac = self.0 % 100;
        let text = if frac == 0 {
            format!("{whole}")
        } else if frac.is_multiple_of(10) {
            format!("{whole}.{}", frac / 10)
        } else {
            format!("{whole}.{frac:02}")
        };
        use std::str::FromStr;
        Number::from_str(&text)
            .map_err(serde::ser::Error::custom)
            .and_then(|n| n.serialize(s))
    }
}

/// ISO-4217 alpha code, normalised to upper case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Currency(pub String);

impl<'de> Deserialize<'de> for Currency {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::parse(&raw)
            .ok_or_else(|| serde::de::Error::custom("currency must be a 3-letter ISO-4217 code"))
    }
}

impl Currency {
    pub fn parse(raw: &str) -> Option<Self> {
        let up = raw.trim().to_uppercase();
        if up.len() == 3 && up.bytes().all(|b| b.is_ascii_alphabetic()) {
            Some(Self(up))
        } else {
            None
        }
    }
}

/// Project code: letters, digits, dash and underscore, normalised to upper
/// case, must start with a letter or digit, at most 24 characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ProjectCode(pub String);

impl<'de> Deserialize<'de> for ProjectCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::parse(&raw).ok_or_else(|| serde::de::Error::custom("invalid project code"))
    }
}

impl ProjectCode {
    pub fn parse(raw: &str) -> Option<Self> {
        let up = raw.trim().to_uppercase();
        let ok = !up.is_empty()
            && up.len() <= 24
            && up
                .bytes()
                .enumerate()
                .all(|(i, b)| b.is_ascii_alphanumeric() || (i > 0 && (b == b'-' || b == b'_')));
        if ok { Some(Self(up)) } else { None }
    }
}

pub(super) fn validate_name(raw: &str) -> Option<&str> {
    let t = raw.trim();
    if t.is_empty() || t.chars().count() > 120 {
        None
    } else {
        Some(t)
    }
}

pub(super) fn validate_note(raw: &str) -> Option<&str> {
    if raw.chars().count() <= 500 {
        Some(raw)
    } else {
        None
    }
}

pub(super) fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

pub(super) fn default_active() -> bool {
    true
}

pub(super) fn default_true() -> bool {
    true
}

/// Field-level problem; carried into the structured 422 error shape.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

impl FieldError {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

/// Validation for a rate: 0 is allowed (unbilled work), absurd values are not.
pub fn validate_rate_minor(rate: u64, field: &str, errors: &mut Vec<FieldError>) {
    if rate > 100_000_000 {
        errors.push(FieldError::new(
            field,
            "rate in minor units must be at most 100000000",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_hours_token(token: &str) -> Result<Hours, serde_json::Error> {
        serde_json::from_str::<Hours>(token)
    }

    #[test]
    fn hours_accepts_two_decimal_forms() {
        assert_eq!(parse_hours_token("7.5").unwrap().0, 750);
        assert_eq!(parse_hours_token("7.50").unwrap().0, 750);
        assert_eq!(parse_hours_token("7").unwrap().0, 700);
        assert_eq!(parse_hours_token("0.07").unwrap().0, 7);
        assert_eq!(parse_hours_token("24").unwrap().0, 2400);
        assert_eq!(parse_hours_token("0.01").unwrap().0, 1);
    }

    #[test]
    fn hours_rejects_out_of_range_and_precision() {
        assert!(parse_hours_token("0").is_err());
        assert!(parse_hours_token("0.001").is_err());
        assert!(parse_hours_token("24.01").is_err());
        assert!(parse_hours_token("100").is_err());
        assert!(parse_hours_token("-2").is_err());
    }

    #[test]
    fn hours_round_trips_exactly() {
        for token in ["7.5", "7.25", "8", "0.07", "24"] {
            let h = parse_hours_token(token).unwrap();
            let out = serde_json::to_string(&h).unwrap();
            let back = parse_hours_token(&out).unwrap();
            assert_eq!(h, back);
        }
    }

    #[test]
    fn amount_is_exact_minor_math() {
        // 7.50 h * 60.00 EUR/h = 450.00 EUR = 45000 cents.
        assert_eq!(Hours(750).amount_minor(6000), 45000);
        // 0.07 h * 33 cents/h = 2.31 -> 2 (half-up at the cent).
        assert_eq!(Hours(7).amount_minor(33), 2);
        assert_eq!(Hours(1).amount_minor(50), 1); // 0.5 -> 1
    }

    #[test]
    fn currency_and_code_normalise() {
        assert_eq!(Currency::parse(" eur").unwrap().0, "EUR");
        assert!(Currency::parse("EU").is_none());
        assert!(Currency::parse("EU1").is_none());
        assert_eq!(ProjectCode::parse(" acme-42 ").unwrap().0, "ACME-42");
        assert!(ProjectCode::parse("").is_none());
        assert!(ProjectCode::parse("-abc").is_none());
        assert!(ProjectCode::parse("a/b").is_none());
        assert!(ProjectCode::parse("../x").is_none());
    }
}
