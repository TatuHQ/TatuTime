use super::primitives::Currency;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ------------------------------------------------------------- recurring --

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cadence {
    Weekly,
    Monthly,
    Quarterly,
}

impl Cadence {
    pub fn interval_days(self) -> i64 {
        match self {
            Cadence::Weekly => 7,
            Cadence::Monthly => 30,
            Cadence::Quarterly => 91,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecurMode {
    /// Bill tracked time + expenses for the period.
    Time,
    /// Bill a fixed retainer amount each cycle.
    Retainer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecurringSchedule {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub cadence: Cadence,
    pub mode: RecurMode,
    /// Retainer amount in minor units (used when mode = Retainer).
    pub retainer_amount_minor: u64,
    pub currency: Currency,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_period_end: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
}

/// If a schedule is due as of `today`, return the period `(from, to)` to bill.
pub fn due_period(
    cadence: Cadence,
    last_period_end: Option<NaiveDate>,
    today: NaiveDate,
) -> Option<(NaiveDate, NaiveDate)> {
    let interval = cadence.interval_days();
    match last_period_end {
        None => {
            let from = today - chrono::Duration::days(interval - 1);
            Some((from, today))
        }
        Some(last) => {
            if (today - last).num_days() >= interval {
                Some((last + chrono::Duration::days(1), today))
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_period_quarterly_boundary() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
        // Never run -> a full 91-day quarter ending today.
        assert_eq!(
            due_period(Cadence::Quarterly, None, today),
            Some((today - chrono::Duration::days(90), today))
        );
        // Ran 90 days ago -> not yet due; 91 days ago -> due, from the next day.
        let last = today - chrono::Duration::days(90);
        assert!(due_period(Cadence::Quarterly, Some(last), today).is_none());
        let last91 = today - chrono::Duration::days(91);
        assert_eq!(
            due_period(Cadence::Quarterly, Some(last91), today),
            Some((last91 + chrono::Duration::days(1), today))
        );
    }
}
