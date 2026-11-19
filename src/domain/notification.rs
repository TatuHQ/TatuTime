use crate::time::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// -------------------------------------------------------------- notifications --

/// A user-facing notification produced by reminders (#22) or alerts (#30).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub created_at: Timestamp,
    pub read: bool,
}

// -------------------------------------------------------------- submissions --

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubmissionState {
    Draft,
    Submitted,
    Approved,
    Rejected,
}

/// A weekly timesheet submission for one user. While `submitted` or `approved`
/// its entries are locked (via the shared lock seam, #18); `rejected`/`draft`
/// release them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Submission {
    pub id: Uuid,
    pub user_id: Uuid,
    pub week_start: CivilDate,
    pub week_end: CivilDate,
    pub state: SubmissionState,
    pub entry_ids: Vec<Uuid>,
    pub comment: String,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<Timestamp>,
}
