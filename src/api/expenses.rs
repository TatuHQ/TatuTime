//! Expenses: categories, expense records (#24) and the submission/claim
//! approval workflows built on them.

use super::*;
use chrono::{DateTime, Utc};

use crate::domain::ExpenseDraft;

pub async fn list_categories(State(app): State<AppState>) -> ApiResult {
    app.store.ensure_default_categories()?;
    let categories = app.store.list_categories()?;
    Ok(Json(serde_json::json!({ "categories": categories })).into_response())
}

pub async fn create_category(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<CategoryInput>,
) -> ApiResult {
    let draft = validate_category_input(&input).map_err(ApiError::validation)?;
    let category = Category {
        id: Uuid::new_v4(),
        name: draft.name,
        default_billable: draft.default_billable,
        active: draft.active,
    };
    app.store.put_category(&category)?;
    Ok((StatusCode::CREATED, Json(category)).into_response())
}

pub async fn update_category(
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<CategoryInput>,
) -> ApiResult {
    let draft = validate_category_input(&input).map_err(ApiError::validation)?;
    let mut category = app
        .store
        .get_category(id)?
        .ok_or_else(|| ApiError::not_found("category"))?;
    category.name = draft.name;
    category.default_billable = draft.default_billable;
    category.active = draft.active;
    app.store.update_category(&category)?;
    Ok(Json(category).into_response())
}

pub async fn delete_category(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    app.store.delete_category(id)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `GET /expenses` filters: all optional, unknown params tolerated (#100).
#[derive(Debug, serde::Deserialize)]
pub struct ExpenseQuery {
    #[serde(default)]
    customer_id: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

pub async fn list_expenses(
    State(app): State<AppState>,
    actor: AuthUser,
    Query(q): Query<ExpenseQuery>,
) -> ApiResult {
    let mut expenses = store_blocking("expense list", &app.store, |s| s.list_expenses()).await?;
    if let Some(cid) = &q.customer_id {
        let cid = Uuid::parse_str(cid)
            .map_err(|_| ApiError::bad_request("'customer_id' must be a UUID"))?;
        expenses.retain(|e| e.customer_id == cid);
    }
    if let (Some(from), Some(to)) = (&q.from, &q.to) {
        let (from, to) = (parse_date(from)?, parse_date(to)?);
        expenses.retain(|e| e.date >= from && e.date <= to);
    }
    expenses.retain(|e| visible_to(&actor.0, e.user_id));
    Ok(Json(serde_json::json!({ "expenses": expenses })).into_response())
}

/// The customer/project/category an expense points at must exist. Shared by the
/// create and update paths so an edit rejects exactly the shapes a create does.
fn reference_errors(app: &AppState, draft: &ExpenseDraft) -> Result<Vec<FieldError>, ApiError> {
    let mut errors = Vec::new();
    if app.store.get_customer(draft.customer_id)?.is_none() {
        errors.push(FieldError::new("customer_id", "customer does not exist"));
    }
    if let Some(code) = &draft.project_code
        && app.store.get_project(draft.customer_id, code)?.is_none()
    {
        errors.push(FieldError::new(
            "project_code",
            "project does not exist for this customer",
        ));
    }
    if let Some(cat) = draft.category_id
        && app.store.get_category(cat)?.is_none()
    {
        errors.push(FieldError::new("category_id", "category does not exist"));
    }
    Ok(errors)
}

/// The stored document a validated payload describes, keeping the identity and
/// history a re-payload must never rewrite: `id`, `user_id` (#51) and
/// `created_at` come from the record being written, `updated_at` from the clock.
fn expense_from_draft(
    existing: Option<&Expense>,
    draft: ExpenseDraft,
    id: Uuid,
    now: DateTime<Utc>,
) -> Expense {
    Expense {
        id,
        date: draft.date,
        customer_id: draft.customer_id,
        user_id: existing.and_then(|e| e.user_id),
        project_code: draft.project_code.map(ProjectCode),
        category_id: draft.category_id,
        amount_minor: draft.amount_minor,
        currency: Currency(draft.currency),
        original_amount_minor: draft.original_amount_minor,
        original_currency: draft.original_currency.map(Currency),
        exchange_rate: draft.exchange_rate,
        exchange_rate_date: draft.exchange_rate_date,
        billable: draft.billable,
        note: draft.note,
        receipt_name: draft.receipt_name,
        receipt_b64: draft.receipt_b64,
        created_at: existing.map_or(now, |e| e.created_at),
        updated_at: now,
    }
}

pub async fn create_expense(
    State(app): State<AppState>,
    actor: AuthUser,
    ValidJson(input): ValidJson<ExpenseInput>,
) -> ApiResult {
    let draft = validate_expense_input(&input).map_err(ApiError::validation)?;
    let errors = reference_errors(&app, &draft)?;
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let now = app.clock.now();
    let mut expense = expense_from_draft(None, draft, Uuid::new_v4(), now);
    expense.user_id = Some(actor.0.id); // the author is the acting user (#51)
    app.store.put_expense(&expense)?;
    Ok((StatusCode::CREATED, Json(expense)).into_response())
}

/// Full replacement of an expense (#…): the same validation as a create, the
/// same ownership rule as a read (someone else's record is a 404, #51) and the
/// same lock seam as a delete, so an expense frozen by an open invoice or a
/// submitted claim cannot be edited — and an unverifiable lock state refuses the
/// write instead of allowing it (#185).
pub async fn update_expense(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<ExpenseInput>,
) -> ApiResult {
    let draft = validate_expense_input(&input).map_err(ApiError::validation)?;
    let existing = app
        .store
        .get_expense(id)?
        .filter(|e| visible_to(&actor.0, e.user_id))
        .ok_or_else(|| ApiError::not_found("expense"))?;
    let errors = reference_errors(&app, &draft)?;
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let verify = || -> Result<(), ApiError> {
        match app.locks.expense_lock(id) {
            Ok(Some(reason)) => Err(ApiError::conflict(reason.message())),
            Ok(None) => Ok(()),
            Err(e) => {
                tracing::warn!(error = ?e.0, expense = %id, "lock state unavailable; refusing expense update");
                Err(ApiError::lock_unavailable())
            }
        }
    };
    let updated = expense_from_draft(Some(&existing), draft, existing.id, app.clock.now());
    app.store.update_expense_guarded(&updated, verify)?;
    Ok(Json(updated).into_response())
}

pub async fn get_expense_handler(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let expense = app
        .store
        .get_expense(id)?
        .filter(|e| visible_to(&actor.0, e.user_id))
        .ok_or_else(|| ApiError::not_found("expense"))?;
    Ok(Json(expense).into_response())
}

pub async fn delete_expense(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult {
    // Ownership check before delete (404 if not the member's own record).
    let owned = app
        .store
        .get_expense(id)?
        .is_some_and(|e| visible_to(&actor.0, e.user_id));
    if !owned {
        return Err(ApiError::not_found("expense"));
    }
    // #218/#255: the invoice/claim lock check runs INSIDE the delete
    // transaction through the same #18 seam the entry paths consult — this was
    // the direct scan the #190 follow-up called out. Both providers answer from
    // one memoized snapshot, so an *open* invoice (issued | partly_paid, #114)
    // and a submitted/approved claim freeze the expense with the same 409, and
    // an unverifiable lock state is refused fail-closed with 503 (#185),
    // never allowed because the check was inconclusive.
    let verify = || -> Result<(), ApiError> {
        match app.locks.expense_lock(id) {
            Ok(Some(reason)) => Err(ApiError::conflict(reason.message())),
            Ok(None) => Ok(()),
            Err(e) => {
                tracing::warn!(error = ?e.0, expense = %id, "lock state unavailable; refusing expense delete");
                Err(ApiError::lock_unavailable())
            }
        }
    };
    app.store.delete_expense_guarded(id, verify)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ----------------------------------------------------------- submissions --

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitInput {
    pub week_start: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionInput {
    pub decision: String,
    #[serde(default)]
    pub comment: String,
}

pub async fn list_submissions(State(app): State<AppState>, actor: AuthUser) -> ApiResult {
    let submissions: Vec<Submission> = app
        .store
        .list_submissions()?
        .into_iter()
        .filter(|s| visible_to(&actor.0, Some(s.user_id)))
        .collect();
    Ok(Json(serde_json::json!({ "submissions": submissions })).into_response())
}

/// Submit the acting user's timesheet for a week; locks its entries.
pub async fn create_submission(
    State(app): State<AppState>,
    actor: AuthUser,
    ValidJson(input): ValidJson<SubmitInput>,
) -> ApiResult {
    let week_start = parse_date(&input.week_start)?;
    let week_end = week_start
        .checked_add_days(chrono::Days::new(6))
        .ok_or_else(|| ApiError::bad_request("invalid week"))?;
    let all = app.store.list_range(week_start, week_end)?;
    // #218: one bulk snapshot instead of a full invoice+submission re-scan
    // per entry (the per-entry entry_lock calls made a week submit
    // O(entries x documents)); fail-closed semantics (#185) unchanged.
    let locked = app.locks.locked_entries().map_err(|e| {
        tracing::warn!(error = ?e.0, "lock state unavailable; refusing submit");
        ApiError::lock_unavailable()
    })?;
    let entry_ids: Vec<Uuid> = all
        .iter()
        .filter(|e| e.user_id == Some(actor.0.id) && !locked.contains(&e.id))
        .map(|e| e.id)
        .collect();
    if entry_ids.is_empty() {
        return Err(ApiError::conflict(
            "no unlocked entries to submit this week",
        ));
    }
    let now = app.clock.now();
    let submission = Submission {
        id: Uuid::new_v4(),
        user_id: actor.0.id,
        week_start,
        week_end,
        state: SubmissionState::Submitted,
        entry_ids,
        comment: String::new(),
        created_at: now,
        submitted_at: Some(now),
        decided_at: None,
    };
    app.store.put_submission(&submission)?;
    Ok((StatusCode::CREATED, Json(submission)).into_response())
}

/// Approve or reject a submitted timesheet (admin only, #51). Rejecting releases its locks.
pub async fn decide_submission(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<DecisionInput>,
) -> ApiResult {
    // Route is admin-tier (require_admin); the inline ensure_admin is
    // deliberate defense-in-depth for mutation endpoints (#220).
    ensure_admin(&actor.0)?;
    let mut submission = app
        .store
        .get_submission(id)?
        .ok_or_else(|| ApiError::not_found("submission"))?;
    if submission.state != SubmissionState::Submitted {
        return Err(ApiError::conflict(
            "only a submitted timesheet can be decided",
        ));
    }
    submission.state = match input.decision.as_str() {
        "approve" => SubmissionState::Approved,
        "reject" => SubmissionState::Rejected,
        other => {
            return Err(ApiError::validation(vec![FieldError::new(
                "decision",
                format!("'{other}' is not approve or reject"),
            )]));
        }
    };
    submission.comment = input.comment;
    submission.decided_at = Some(app.clock.now());
    app.store.put_submission(&submission)?;
    Ok(Json(submission).into_response())
}

// ------------------------------------------------------------------ claims --

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimDecisionInput {
    pub decision: String,
    #[serde(default)]
    pub comment: String,
}

pub async fn list_claims(State(app): State<AppState>, actor: AuthUser) -> ApiResult {
    let claims: Vec<ExpenseClaim> = app
        .store
        .list_claims()?
        .into_iter()
        .filter(|c| visible_to(&actor.0, Some(c.user_id)))
        .collect();
    Ok(Json(serde_json::json!({ "claims": claims })).into_response())
}

/// Create a draft reimbursement claim over a set of the caller's expenses.
pub async fn create_claim(
    State(app): State<AppState>,
    actor: AuthUser,
    ValidJson(input): ValidJson<ClaimInput>,
) -> ApiResult {
    let title = input.title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(ApiError::validation(vec![FieldError::new(
            "title",
            "required, at most 120 characters",
        )]));
    }
    if input.expense_ids.is_empty() {
        return Err(ApiError::validation(vec![FieldError::new(
            "expense_ids",
            "at least one expense is required",
        )]));
    }
    let mut errors = Vec::new();
    let mut total: u64 = 0;
    let mut currency: Option<Currency> = None;
    // #187: a repeated expense id used to sum its amount twice into the
    // persisted claim total shown to approvers (while referencing one
    // expense). Reject duplicates before any work.
    let mut seen = std::collections::HashSet::new();
    for id in &input.expense_ids {
        if !seen.insert(id) {
            return Err(ApiError::validation(vec![FieldError::new(
                "expense_ids",
                format!("expense {id} is listed more than once"),
            )]));
        }
    }
    for id in &input.expense_ids {
        let Some(x) = app.store.get_expense(*id)? else {
            errors.push(FieldError::new(
                "expense_ids",
                format!("unknown expense {id}"),
            ));
            continue;
        };
        if !visible_to(&actor.0, x.user_id) {
            errors.push(FieldError::new(
                "expense_ids",
                "expense belongs to another user",
            ));
            continue;
        }
        total = total.saturating_add(x.amount_minor); // #190
        match &currency {
            None => currency = Some(x.currency.clone()),
            Some(c) if *c == x.currency => {}
            Some(c) => {
                errors.push(FieldError::new(
                    "expense_ids",
                    format!("mixed currencies ({} and {})", c.0, x.currency.0),
                ));
            }
        }
    }
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let claim = ExpenseClaim {
        id: Uuid::new_v4(),
        user_id: actor.0.id,
        title: title.to_owned(),
        expense_ids: input.expense_ids.clone(),
        total_minor: total,
        // #220: typed fallback — empty/duplicate ids were rejected above,
        // so currency is Some here; keep it an error, not a panic.
        currency: currency.ok_or_else(|| {
            ApiError::internal("claim currency unset despite non-empty ids".into())
        })?,
        state: ClaimState::Draft,
        comment: String::new(),
        created_at: app.clock.now(),
        submitted_at: None,
        decided_at: None,
    };
    app.store.put_claim(&claim)?;
    Ok((StatusCode::CREATED, Json(claim)).into_response())
}

pub async fn submit_claim(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let mut claim = app
        .store
        .get_claim(id)?
        .filter(|c| visible_to(&actor.0, Some(c.user_id)))
        .ok_or_else(|| ApiError::not_found("claim"))?;
    if claim.state != ClaimState::Draft {
        return Err(ApiError::conflict("only a draft claim can be submitted"));
    }
    claim.state = ClaimState::Submitted;
    claim.submitted_at = Some(app.clock.now());
    app.store.put_claim(&claim)?;
    Ok(Json(claim).into_response())
}

/// Approve or reject a submitted claim (admin only, #24).
pub async fn decide_claim(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<ClaimDecisionInput>,
) -> ApiResult {
    // Route is admin-tier (require_admin); the inline ensure_admin is
    // deliberate defense-in-depth for mutation endpoints (#220).
    ensure_admin(&actor.0)?;
    let mut claim = app
        .store
        .get_claim(id)?
        .ok_or_else(|| ApiError::not_found("claim"))?;
    if claim.state != ClaimState::Submitted {
        return Err(ApiError::conflict("only a submitted claim can be decided"));
    }
    claim.state = match input.decision.as_str() {
        "approve" => ClaimState::Approved,
        "reject" => ClaimState::Rejected,
        other => {
            return Err(ApiError::validation(vec![FieldError::new(
                "decision",
                format!("'{other}' is not approve or reject"),
            )]));
        }
    };
    claim.comment = input.comment;
    claim.decided_at = Some(app.clock.now());
    app.store.put_claim(&claim)?;
    Ok(Json(claim).into_response())
}
