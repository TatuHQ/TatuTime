use super::customer::Customer;
use super::primitives::{Currency, FieldError, Hours, ProjectCode, default_true, validate_note};
use super::project::{Project, Source};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ------------------------------------------------------------------- timer --

/// A running timer for one user (#14). Persisted so it survives reloads and is
/// visible from any device (server-side state).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timer {
    pub user_id: Uuid,
    pub customer_id: Uuid,
    pub project_code: ProjectCode,
    #[serde(default)]
    pub task_code: Option<ProjectCode>,
    #[serde(default)]
    pub note: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartTimerInput {
    pub customer_id: Uuid,
    pub project_code: ProjectCode,
    #[serde(default)]
    pub task_code: Option<ProjectCode>,
    #[serde(default)]
    pub note: String,
}

/// Elapsed hundredths-of-an-hour for a running timer, rounded half-up and
/// clamped to the valid entry range (0.01–24.00 h).
pub fn elapsed_hundredths(started_at: DateTime<Utc>, now: DateTime<Utc>) -> u32 {
    let secs = (now - started_at).num_seconds().max(0) as u64;
    let hundredths = (secs * 100 + 1800) / 3600; // round half up
    hundredths.clamp(1, 2400) as u32
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: Uuid,
    pub date: NaiveDate,
    pub customer_id: Uuid,
    /// The user who logged the entry (#21 attribution; None for legacy docs).
    #[serde(default)]
    pub user_id: Option<Uuid>,
    pub project_code: ProjectCode,
    /// Optional task within the project (#38). Must belong to `project_code`.
    #[serde(default)]
    pub task_code: Option<ProjectCode>,
    pub hours: Hours,
    pub note: String,
    /// Billable vs non-billable. Always serialised; defaults to true on read so
    /// pre-#20 documents load as billable (self-heals on next write).
    #[serde(default = "default_true")]
    pub billable: bool,
    /// Provenance. Defaults to `manual` so pre-#18 documents load unchanged.
    #[serde(default)]
    pub source: Source,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Effective (currency, rate) of an entry. Rate precedence (#21): **person →
/// project → customer default**; currency precedence: project → customer. The
/// customer supplies only the *default*, so a project's own rate always wins.
/// `user_rate` is the logging person's default (Some only when > 0), kept as a
/// plain number so `domain` stays independent of `auth`; tasks carry no billing
/// overrides (#177/#189).
///
/// ```rust
/// // project rate 6000 wins over the customer default 5000
/// let (currency, rate) = effective_rates(&customer, Some(&project), None);
/// // with no project, and no user rate, rate == customer.default_rate_minor
/// ```
pub fn effective_rates(
    customer: &Customer,
    project: Option<&Project>,
    user_rate: Option<u64>,
) -> (Currency, u64) {
    let currency = project
        .map(|p| p.currency.clone())
        .unwrap_or_else(|| customer.currency.clone());
    let rate = user_rate
        .filter(|r| *r > 0)
        .or_else(|| project.map(|p| p.rate_minor))
        .unwrap_or(customer.default_rate_minor);
    (currency, rate)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryInput {
    pub date: String,
    pub customer_id: Uuid,
    pub project_code: ProjectCode,
    #[serde(default)]
    pub task_code: Option<ProjectCode>,
    pub hours: Hours,
    #[serde(default)]
    pub note: String,
    #[serde(default = "default_true")]
    pub billable: bool,
}

pub fn validate_entry_input(input: &EntryInput) -> Result<EntryDraft, Vec<FieldError>> {
    let mut errors = Vec::new();
    let date = NaiveDate::parse_from_str(input.date.trim(), "%Y-%m-%d")
        .map_err(|_| {
            errors.push(FieldError::new(
                "date",
                "must be a calendar date as YYYY-MM-DD",
            ));
        })
        .ok();
    let note_ok = validate_note(&input.note).is_some();
    if !note_ok {
        errors.push(FieldError::new("note", "at most 500 characters"));
    }
    match (date, note_ok) {
        (Some(date), true) => Ok(EntryDraft {
            date,
            customer_id: input.customer_id,
            project_code: input.project_code.0.clone(),
            task_code: input.task_code.as_ref().map(|c| c.0.clone()),
            hours: input.hours,
            note: input.note.clone(),
            billable: input.billable,
        }),
        _ => Err(errors),
    }
}

#[derive(Debug)]
pub struct EntryDraft {
    pub date: NaiveDate,
    pub customer_id: Uuid,
    pub project_code: String,
    pub task_code: Option<String>,
    pub hours: Hours,
    pub note: String,
    pub billable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Task;

    #[test]
    fn entry_validation_reports_every_problem() {
        let input = EntryInput {
            date: "2026-13-01".into(),
            customer_id: Uuid::new_v4(),
            project_code: ProjectCode::parse("P1").unwrap(),
            task_code: None,
            hours: Hours(100),
            note: "x".repeat(501),
            billable: true,
        };
        let errs = validate_entry_input(&input).unwrap_err();
        assert!(errs.iter().any(|e| e.field == "date"));
        assert!(errs.iter().any(|e| e.field == "note"));
    }

    #[test]
    fn effective_rate_precedence_person_project_customer_ignores_legacy_tasks() {
        let entry = Entry {
            id: Uuid::new_v4(),
            date: chrono::NaiveDate::from_ymd_opt(2026, 10, 2).unwrap(),
            customer_id: Uuid::new_v4(),
            user_id: None,
            project_code: ProjectCode::parse("P1").unwrap(),
            task_code: None,
            hours: Hours(100),
            note: String::new(),
            billable: true,
            source: Source::Manual,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let customer = Customer {
            id: entry.customer_id,
            name: "Tucano Time".into(),
            currency: Currency("EUR".into()),
            default_rate_minor: 6000,
            active: true,
            email: String::new(),
            payment_terms: None,
            invoice_notes: String::new(),
            invoice_subject: String::new(),
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,

            payment_details: None,
            legal_id: String::new(),
        };
        let project = Project {
            customer_id: customer.id,
            code: entry.project_code.clone(),
            name: "P1".into(),
            currency: Currency("USD".into()),
            rate_minor: 3000,
            active: true,
            budget_hours: None,
            budget_amount_minor: None,
        };
        // Project value wins over customer default.
        assert_eq!(
            effective_rates(&customer, Some(&project), None),
            (Currency("USD".into()), 3000)
        );
        // Missing project falls to customer default.
        assert_eq!(
            effective_rates(&customer, None, None),
            (Currency("EUR".into()), 6000)
        );
        // Person rate beats the project rate, keeps the project currency.
        assert_eq!(
            effective_rates(&customer, Some(&project), Some(4500)),
            (Currency("USD".into()), 4500)
        );
        let task: Task = serde_json::from_value(serde_json::json!({
            "customer_id": customer.id,
            "project_code": entry.project_code,
            "code": "T1",
            "name": "T1",
            "currency": "GBP",
            "rate_minor": 5000,
            "active": true,
        }))
        .unwrap();
        assert_eq!(
            effective_rates(&customer, Some(&project), Some(4500)),
            (Currency("USD".into()), 4500)
        );
        assert_eq!(
            effective_rates(&customer, Some(&project), None),
            (Currency("USD".into()), 3000)
        );
        let serialized = serde_json::to_value(task).unwrap();
        assert!(serialized.get("currency").is_none());
        assert!(serialized.get("rate_minor").is_none());
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn elapsed_rounds_and_clamps() {
        let s = at(0);
        assert_eq!(elapsed_hundredths(s, at(3600)), 100); // 1.00h
        assert_eq!(elapsed_hundredths(s, at(1800)), 50); // 0.50h
        assert_eq!(elapsed_hundredths(s, at(18)), 1); // ~0.005 -> 0.01 (min)
        assert_eq!(elapsed_hundredths(s, at(0)), 1); // zero -> min 0.01
        assert_eq!(elapsed_hundredths(s, at(25 * 3600)), 2400); // clamp 24h
    }
}
