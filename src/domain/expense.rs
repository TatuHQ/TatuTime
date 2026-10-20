use super::primitives::{
    Currency, FieldError, ProjectCode, default_active, default_true, validate_name, validate_note,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------- expenses --

/// A cost category (org-wide), e.g. Travel, Software.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Category {
    pub id: Uuid,
    pub name: String,
    pub default_billable: bool,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Expense {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported: Option<super::ImportOrigin>,
    pub id: Uuid,
    pub date: NaiveDate,
    pub customer_id: Uuid,
    /// The user who recorded the expense (#51 ownership; None for legacy docs).
    #[serde(default)]
    pub user_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_code: Option<ProjectCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_id: Option<Uuid>,
    pub amount_minor: u64,
    pub currency: Currency,
    pub billable: bool,
    #[serde(default)]
    pub note: String,
    /// The amount as it was entered, in its own currency, when that differs from
    /// the billed `amount_minor`/`currency` pair — the provenance of a converted
    /// expense. All four conversion fields are set together or not at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_amount_minor: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_currency: Option<Currency>,
    /// The applied `original_currency` -> `currency` rate, kept as the decimal
    /// string the caller supplied: money never rides on a float (#11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange_rate: Option<String>,
    /// The reference date the rate was published for (for example the day the
    /// ECB published its table), never the expense date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange_rate_date: Option<NaiveDate>,
    /// Optional receipt stored inline (base64) with its filename (#23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_b64: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryInput {
    pub name: String,
    #[serde(default = "default_true")]
    pub default_billable: bool,
    #[serde(default = "default_active")]
    pub active: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpenseInput {
    pub date: String,
    pub customer_id: Uuid,
    #[serde(default)]
    pub project_code: Option<ProjectCode>,
    #[serde(default)]
    pub category_id: Option<Uuid>,
    pub amount_minor: u64,
    pub currency: Currency,
    #[serde(default = "default_true")]
    pub billable: bool,
    #[serde(default)]
    pub note: String,
    /// Converted expense: the amount as entered, the currency it was entered in
    /// and the rate that produced the billed `amount_minor`/`currency`. The three
    /// are an all-or-nothing group (see [`validate_expense_input`]) and
    /// `exchange_rate_date` carries the rate's own reference date.
    #[serde(default)]
    pub original_amount_minor: Option<u64>,
    #[serde(default)]
    pub original_currency: Option<Currency>,
    #[serde(default)]
    pub exchange_rate: Option<String>,
    #[serde(default)]
    pub exchange_rate_date: Option<String>,
    #[serde(default)]
    pub receipt_name: Option<String>,
    #[serde(default)]
    pub receipt_b64: Option<String>,
}

pub fn validate_category_input(input: &CategoryInput) -> Result<CategoryDraft, Vec<FieldError>> {
    let mut errors = Vec::new();
    let name = validate_name(&input.name).map(str::to_owned);
    if name.is_none() {
        errors.push(FieldError::new("name", "required, at most 120 characters"));
    }
    if errors.is_empty() {
        Ok(CategoryDraft {
            name: name.unwrap(),
            default_billable: input.default_billable,
            active: input.active,
        })
    } else {
        Err(errors)
    }
}

/// A conversion rate as a decimal string, normalised for storage: digits only
/// (no sign, no exponent, no thousands separator), an integer part that is
/// present, at most six decimals and a value greater than zero and at most
/// `1_000_000`. Trailing fraction zeros are dropped, so the stored string is the
/// rate the caller wrote and not a float's rendering of it (#11). Magnitude is
/// compared digit by digit for the same reason.
fn validate_rate(raw: &str) -> Option<String> {
    let (int, frac) = match raw.split_once('.') {
        Some((int, frac)) => (int, Some(frac)),
        None => (raw, None),
    };
    if int.is_empty() || !int.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if let Some(frac) = frac
        && (frac.is_empty() || frac.len() > 6 || !frac.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let int_digits = int.trim_start_matches('0');
    if int_digits.len() > 7 || (int_digits.len() == 7 && int_digits > "1000000") {
        return None;
    }
    let frac_digits = frac.unwrap_or("").trim_end_matches('0');
    if int_digits.is_empty() && frac_digits.is_empty() {
        return None; // zero, or nothing at all
    }
    let int_norm = if int_digits.is_empty() {
        "0"
    } else {
        int_digits
    };
    Some(match frac_digits.is_empty() {
        true => int_norm.to_string(),
        false => format!("{int_norm}.{frac_digits}"),
    })
}

pub fn validate_expense_input(input: &ExpenseInput) -> Result<ExpenseDraft, Vec<FieldError>> {
    let mut errors = Vec::new();
    let date = NaiveDate::parse_from_str(input.date.trim(), "%Y-%m-%d")
        .map_err(|_| {
            errors.push(FieldError::new(
                "date",
                "must be a calendar date as YYYY-MM-DD",
            ))
        })
        .ok();
    if input.amount_minor > 100_000_000 {
        errors.push(FieldError::new("amount_minor", "at most 100000000"));
    }
    if validate_note(&input.note).is_none() {
        errors.push(FieldError::new("note", "at most 500 characters"));
    }
    if input
        .receipt_b64
        .as_deref()
        .is_some_and(|b| b.len() > 4_000_000)
    {
        errors.push(FieldError::new("receipt_b64", "receipt too large"));
    }
    // A converted expense records the amount as entered, the currency it was
    // entered in and the rate that produced the billed amount. The three are an
    // all-or-nothing group: the rate alone cannot reproduce what was paid, and
    // the amount alone cannot be billed. The billed `amount_minor`/`currency`
    // stay authoritative — the invoice snapshots them, not the original pair.
    let original_currency = input.original_currency.as_ref().map(|c| c.0.clone());
    let rate = match input.exchange_rate.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => match validate_rate(raw) {
            Some(rate) => Some(rate),
            None => {
                errors.push(FieldError::new(
                    "exchange_rate",
                    "must be a positive decimal number with at most 6 decimals, at most 1000000",
                ));
                None
            }
        },
        _ => None,
    };
    let group = [
        input.original_amount_minor.is_some(),
        original_currency.is_some(),
        rate.is_some(),
    ];
    if group.iter().any(|given| *given) && !group.iter().all(|given| *given) {
        if input.original_amount_minor.is_none() {
            errors.push(FieldError::new(
                "original_amount_minor",
                "required when the expense records a conversion",
            ));
        }
        if original_currency.is_none() {
            errors.push(FieldError::new(
                "original_currency",
                "required when the expense records a conversion",
            ));
        }
        // Only when the rate itself was absent — a malformed rate is already
        // reported against the same field and must not be reported twice.
        if rate.is_none() && !errors.iter().any(|f| f.field == "exchange_rate") {
            errors.push(FieldError::new(
                "exchange_rate",
                "required when the expense records a conversion",
            ));
        }
    }
    if input
        .original_amount_minor
        .is_some_and(|amount| amount > 100_000_000)
    {
        errors.push(FieldError::new(
            "original_amount_minor",
            "at most 100000000",
        ));
    }
    if original_currency.as_deref() == Some(input.currency.0.as_str()) {
        errors.push(FieldError::new(
            "original_currency",
            "must differ from 'currency'; omit the conversion when the amount is already billed in its own currency",
        ));
    }
    let rate_date = match input.exchange_rate_date.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .map_err(|_| {
                errors.push(FieldError::new(
                    "exchange_rate_date",
                    "must be a calendar date as YYYY-MM-DD",
                ))
            })
            .ok(),
        _ => None,
    };
    match (date, errors.is_empty()) {
        (Some(date), true) => Ok(ExpenseDraft {
            date,
            customer_id: input.customer_id,
            project_code: input.project_code.as_ref().map(|c| c.0.clone()),
            category_id: input.category_id,
            amount_minor: input.amount_minor,
            currency: input.currency.0.clone(),
            original_amount_minor: input.original_amount_minor,
            original_currency,
            exchange_rate: rate,
            exchange_rate_date: rate_date,
            billable: input.billable,
            note: input.note.clone(),
            receipt_name: input.receipt_name.clone(),
            receipt_b64: input.receipt_b64.clone(),
        }),
        _ => Err(errors),
    }
}

#[derive(Debug)]
pub struct CategoryDraft {
    pub name: String,
    pub default_billable: bool,
    pub active: bool,
}

#[derive(Debug)]
pub struct ExpenseDraft {
    pub date: NaiveDate,
    pub customer_id: Uuid,
    pub project_code: Option<String>,
    pub category_id: Option<Uuid>,
    pub amount_minor: u64,
    pub currency: String,
    /// Provenance of a converted expense: the amount's own currency, the applied
    /// rate (normalised) and the rate's reference date. All `None` when nothing
    /// was converted, so an ordinary expense carries no conversion baggage.
    pub original_amount_minor: Option<u64>,
    pub original_currency: Option<String>,
    pub exchange_rate: Option<String>,
    pub exchange_rate_date: Option<NaiveDate>,
    pub billable: bool,
    pub note: String,
    pub receipt_name: Option<String>,
    pub receipt_b64: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_category_input_requires_a_name() {
        let ok = validate_category_input(&CategoryInput {
            name: "  Travel  ".into(),
            default_billable: true,
            active: true,
        })
        .unwrap();
        assert_eq!(ok.name, "Travel", "name is trimmed");

        for bad in ["   ", &"x".repeat(121)] {
            let err = validate_category_input(&CategoryInput {
                name: bad.to_string(),
                default_billable: true,
                active: true,
            })
            .unwrap_err();
            assert!(err.iter().any(|f| f.field == "name"), "name {bad:?}");
        }
    }

    #[test]
    fn validate_expense_input_bounds_amount_date_note_and_receipt() {
        let base = |amount: u64, date: &str, receipt: Option<String>| ExpenseInput {
            date: date.into(),
            customer_id: Uuid::nil(),
            project_code: None,
            category_id: None,
            amount_minor: amount,
            currency: Currency("EUR".into()),
            billable: true,
            note: String::new(),
            original_amount_minor: None,
            original_currency: None,
            exchange_rate: None,
            exchange_rate_date: None,
            receipt_name: None,
            receipt_b64: receipt,
        };

        let ok = validate_expense_input(&base(1000, "2026-10-02", None)).unwrap();
        assert_eq!(ok.amount_minor, 1000);

        // Amount ceiling.
        let err = validate_expense_input(&base(100_000_001, "2026-10-02", None)).unwrap_err();
        assert!(err.iter().any(|f| f.field == "amount_minor"));
        // Malformed date.
        let err2 = validate_expense_input(&base(1000, "nope", None)).unwrap_err();
        assert!(err2.iter().any(|f| f.field == "date"));
        // Oversized receipt.
        let err3 = validate_expense_input(&base(1000, "2026-10-02", Some("a".repeat(4_000_001))))
            .unwrap_err();
        assert!(err3.iter().any(|f| f.field == "receipt_b64"));
        // Overlong note.
        let mut long_note = base(1000, "2026-10-02", None);
        long_note.note = "x".repeat(501);
        let err4 = validate_expense_input(&long_note).unwrap_err();
        assert!(err4.iter().any(|f| f.field == "note"));
    }

    #[test]
    fn validate_expense_input_keeps_conversion_provenance_or_rejects_it() {
        // Billed to the customer in EUR, recorded as USD 100.00 at the published
        // rate for 2026-10-01. The rate is padded and carries trailing zeros on
        // purpose: it must be trimmed and normalised, never round-tripped
        // through a float (#11).
        let base = || ExpenseInput {
            date: "2026-10-02".into(),
            customer_id: Uuid::nil(),
            project_code: None,
            category_id: None,
            amount_minor: 9206,
            currency: Currency("EUR".into()),
            billable: true,
            note: String::new(),
            original_amount_minor: Some(10_000),
            original_currency: Some(Currency("USD".into())),
            exchange_rate: Some(" 0.920600 ".into()),
            exchange_rate_date: Some("2026-10-01".into()),
            receipt_name: None,
            receipt_b64: None,
        };

        let ok = validate_expense_input(&base()).unwrap();
        assert_eq!(
            ok.amount_minor, 9206,
            "the billed amount stays authoritative"
        );
        assert_eq!(ok.currency, "EUR");
        assert_eq!(ok.original_amount_minor, Some(10_000));
        assert_eq!(ok.original_currency.as_deref(), Some("USD"));
        assert_eq!(ok.exchange_rate.as_deref(), Some("0.9206"));
        assert_eq!(
            ok.exchange_rate_date,
            Some(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap())
        );

        // An ordinary expense carries no conversion at all.
        let plain = validate_expense_input(&ExpenseInput {
            original_amount_minor: None,
            original_currency: None,
            exchange_rate: None,
            exchange_rate_date: None,
            ..base()
        })
        .unwrap();
        assert!(plain.original_amount_minor.is_none());
        assert!(plain.original_currency.is_none());
        assert!(plain.exchange_rate.is_none());
        assert!(plain.exchange_rate_date.is_none());

        // A partial group is refused against the field that is missing.
        let missing_amount = validate_expense_input(&ExpenseInput {
            original_amount_minor: None,
            ..base()
        })
        .unwrap_err();
        assert!(
            missing_amount
                .iter()
                .any(|f| f.field == "original_amount_minor")
        );
        let missing_currency = validate_expense_input(&ExpenseInput {
            original_currency: None,
            ..base()
        })
        .unwrap_err();
        assert!(
            missing_currency
                .iter()
                .any(|f| f.field == "original_currency")
        );
        let missing_rate = validate_expense_input(&ExpenseInput {
            exchange_rate: None,
            ..base()
        })
        .unwrap_err();
        assert!(missing_rate.iter().any(|f| f.field == "exchange_rate"));

        // A rate that is not a positive, six-decimal number is refused once.
        for bad in [
            "0",
            "0.0",
            "-1",
            "+1",
            "1e3",
            "1.2345678",
            "1000001",
            "abc",
            "1,5",
            ".",
        ] {
            let err = validate_expense_input(&ExpenseInput {
                exchange_rate: Some(bad.into()),
                ..base()
            })
            .unwrap_err();
            let hits = err.iter().filter(|f| f.field == "exchange_rate").count();
            assert_eq!(hits, 1, "rate {bad:?} must be refused exactly once");
        }

        // Converting the billed currency with itself is not a conversion, and a
        // rate date that is not a date is a typo.
        let same_currency = validate_expense_input(&ExpenseInput {
            original_currency: Some(Currency("EUR".into())),
            ..base()
        })
        .unwrap_err();
        assert!(
            same_currency.iter().any(|f| f.field == "original_currency"),
            "a conversion of the billed currency with itself is refused"
        );
        let bad_date = validate_expense_input(&ExpenseInput {
            exchange_rate_date: Some("yesterday".into()),
            ..base()
        })
        .unwrap_err();
        assert!(bad_date.iter().any(|f| f.field == "exchange_rate_date"));
        // The original amount keeps the same ceiling as the billed one.
        let huge = validate_expense_input(&ExpenseInput {
            original_amount_minor: Some(100_000_001),
            ..base()
        })
        .unwrap_err();
        assert!(huge.iter().any(|f| f.field == "original_amount_minor"));
    }
}
