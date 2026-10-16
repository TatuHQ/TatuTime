use super::primitives::Currency;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ------------------------------------------------------------- reimbursements --

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaimState {
    Draft,
    Submitted,
    Approved,
    Rejected,
}

/// A reimbursement claim over a set of expenses (#24). While submitted or
/// approved, its expenses are locked from deletion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpenseClaim {
    pub id: Uuid,
    pub user_id: Uuid,
    pub title: String,
    pub expense_ids: Vec<Uuid>,
    pub total_minor: u64,
    pub currency: Currency,
    pub state: ClaimState,
    pub comment: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimInput {
    pub title: String,
    pub expense_ids: Vec<Uuid>,
}
