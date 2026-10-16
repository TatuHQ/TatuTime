use super::customer::Customer;
use super::primitives::{Currency, FieldError, ProjectCode, default_active, validate_rate_minor};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Reads go exclusively through `project_from_bytes`/`ProjectDoc` (the single
/// legacy-aware disk path, #11); the redundant `Deserialize` derive that
/// let a second, drift-prone disk shape exist was removed (#183 → #220 /
/// old review item C3). The struct is still serialized on write.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Project {
    pub customer_id: Uuid,
    pub code: ProjectCode,
    pub name: String,
    /// The project's own currency (required). Billing reads this directly.
    pub currency: Currency,
    /// The project's own hourly rate in minor units (required).
    pub rate_minor: u64,
    pub active: bool,
    /// Optional total budget in hours (hundredths) (#30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_hours: Option<u32>,
    /// Optional total budget in minor units (#30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_amount_minor: Option<u64>,
}

/// On-disk shape used only for reading. Pre-#11 project documents may omit
/// `currency`/`rate_minor`; `resolve` fills them from the owning customer so a
/// legacy document is never rejected (self-heals on the next write).
#[derive(Debug, Clone, Deserialize)]
struct ProjectDoc {
    customer_id: Uuid,
    code: ProjectCode,
    #[serde(default)]
    name: String,
    #[serde(default)]
    currency: Option<Currency>,
    #[serde(default)]
    rate_minor: Option<u64>,
    #[serde(default = "default_active")]
    active: bool,
    #[serde(default)]
    budget_hours: Option<u32>,
    #[serde(default)]
    budget_amount_minor: Option<u64>,
}

impl ProjectDoc {
    fn resolve(self, customer: &Customer) -> Project {
        let name = if self.name.trim().is_empty() {
            self.code.0.clone()
        } else {
            self.name
        };
        Project {
            customer_id: self.customer_id,
            code: self.code,
            name,
            currency: self.currency.unwrap_or_else(|| customer.currency.clone()),
            rate_minor: self.rate_minor.unwrap_or(customer.default_rate_minor),
            active: self.active,
            budget_hours: self.budget_hours,
            budget_amount_minor: self.budget_amount_minor,
        }
    }
}

/// Parse a stored project document, filling any pre-#11 missing currency/rate
/// from the owning customer.
pub fn project_from_bytes(bytes: &[u8], customer: &Customer) -> Result<Project, serde_json::Error> {
    let doc: ProjectDoc = serde_json::from_slice(bytes)?;
    Ok(doc.resolve(customer))
}

/// An optional work level under a project (#38), without billing overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub customer_id: Uuid,
    pub project_code: ProjectCode,
    pub code: ProjectCode,
    pub name: String,
    pub active: bool,
}

/// Where a time entry came from. Server-set; clients create `manual` entries.
/// Timer (#14) and calendar import (#15/#36) write their own value so imported
/// time is always traceable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    #[default]
    Manual,
    Timer,
    CalendarImport,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectInput {
    pub code: ProjectCode,
    #[serde(default)]
    pub name: String,
    /// Required: the project's own currency (prefilled from the customer in the GUI).
    pub currency: Currency,
    /// Required: the project's own hourly rate in minor units.
    pub rate_minor: u64,
    #[serde(default = "default_active")]
    pub active: bool,
    #[serde(default)]
    pub budget_hours: Option<u32>,
    #[serde(default)]
    pub budget_amount_minor: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInput {
    pub code: ProjectCode,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_active")]
    pub active: bool,
}

pub fn validate_project_input(input: &ProjectInput) -> Result<ProjectDraft, Vec<FieldError>> {
    let mut errors = Vec::new();
    let name = if input.name.trim().is_empty() {
        input.code.0.clone()
    } else if input.name.chars().count() <= 120 {
        input.name.trim().to_owned()
    } else {
        errors.push(FieldError::new("name", "at most 120 characters"));
        String::new()
    };
    validate_rate_minor(input.rate_minor, "rate_minor", &mut errors);
    // #187: budget fields were the only money/hours inputs persisted with no
    // bound. Same ceilings as rates: 100_000_000 minor units; 1_000_000.00 h.
    if input.budget_amount_minor.unwrap_or_default() > 100_000_000 {
        errors.push(FieldError::new("budget_amount_minor", "at most 100000000"));
    }
    if input.budget_hours.unwrap_or_default() > 100_000_000 {
        errors.push(FieldError::new("budget_hours", "at most 1000000.00 hours"));
    }
    if errors.is_empty() {
        Ok(ProjectDraft {
            code: input.code.0.clone(),
            name,
            currency: input.currency.0.clone(),
            rate_minor: input.rate_minor,
            active: input.active,
            budget_hours: input.budget_hours,
            budget_amount_minor: input.budget_amount_minor,
        })
    } else {
        Err(errors)
    }
}

#[derive(Debug)]
pub struct ProjectDraft {
    pub code: String,
    pub name: String,
    pub currency: String,
    pub rate_minor: u64,
    pub active: bool,
    pub budget_hours: Option<u32>,
    pub budget_amount_minor: Option<u64>,
}

#[derive(Debug)]
pub struct TaskDraft {
    pub code: String,
    pub name: String,
    pub active: bool,
}

pub fn validate_task_input(input: &TaskInput) -> Result<TaskDraft, Vec<FieldError>> {
    let mut errors = Vec::new();
    let name = if input.name.trim().is_empty() {
        input.code.0.clone()
    } else if input.name.chars().count() <= 120 {
        input.name.trim().to_owned()
    } else {
        errors.push(FieldError::new("name", "at most 120 characters"));
        String::new()
    };
    if errors.is_empty() {
        Ok(TaskDraft {
            code: input.code.0.clone(),
            name,
            active: input.active,
        })
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- #262 Tier 3: direct validator coverage (previously HTTP-only) ----

    #[test]
    fn validate_project_input_defaults_and_bounds() {
        // A blank name falls back to the (normalised) project code.
        let ok = validate_project_input(&ProjectInput {
            code: ProjectCode::parse("p-1").unwrap(),
            name: "   ".into(),
            currency: Currency("EUR".into()),
            rate_minor: 6000,
            active: true,
            budget_hours: None,
            budget_amount_minor: None,
        })
        .unwrap();
        assert_eq!(ok.code, "P-1");
        assert_eq!(ok.name, "P-1", "blank name falls back to the code");

        // The #187 budget ceilings are enforced on both money and hours lanes.
        let err = validate_project_input(&ProjectInput {
            code: ProjectCode::parse("P1").unwrap(),
            name: "P1".into(),
            currency: Currency("EUR".into()),
            rate_minor: 6000,
            active: true,
            budget_hours: Some(100_000_001),
            budget_amount_minor: Some(100_000_001),
        })
        .unwrap_err();
        let fields: Vec<&str> = err.iter().map(|f| f.field.as_str()).collect();
        assert!(fields.contains(&"budget_hours"), "{fields:?}");
        assert!(fields.contains(&"budget_amount_minor"), "{fields:?}");

        // A name over the 120-char bound is a field error.
        let err2 = validate_project_input(&ProjectInput {
            code: ProjectCode::parse("P1").unwrap(),
            name: "x".repeat(121),
            currency: Currency("EUR".into()),
            rate_minor: 6000,
            active: true,
            budget_hours: None,
            budget_amount_minor: None,
        })
        .unwrap_err();
        assert!(err2.iter().any(|f| f.field == "name"));
    }

    #[test]
    fn validate_task_input_trims_and_bounds_name() {
        let ok = validate_task_input(&TaskInput {
            code: ProjectCode::parse("t-9").unwrap(),
            name: "  Design  ".into(),
            active: true,
        })
        .unwrap();
        assert_eq!(ok.code, "T-9");
        assert_eq!(ok.name, "Design", "name is trimmed");

        // A blank name falls back to the code; a too-long one is rejected.
        let blank = validate_task_input(&TaskInput {
            code: ProjectCode::parse("t-9").unwrap(),
            name: String::new(),
            active: true,
        })
        .unwrap();
        assert_eq!(blank.name, "T-9");
        let err = validate_task_input(&TaskInput {
            code: ProjectCode::parse("T-9").unwrap(),
            name: "x".repeat(121),
            active: true,
        })
        .unwrap_err();
        assert!(err.iter().any(|f| f.field == "name"));
    }
}
