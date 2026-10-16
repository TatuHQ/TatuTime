//! Entries and reports: the CRUD surface for time entries (#51 rules) and
//! the read-side reports built over them.

use super::pagination::{self, Order};
use super::*;

/// Entry list body: serialised exactly once by `Json`, with no intermediate
/// `serde_json::Value` per row (#256).
///
/// `next_cursor` is always a key, with no `skip_serializing_if` (#334): `null`
/// is the answer to "was that all of it?", and dropping the key would leave the
/// caller guessing — the silent cut this endpoint must never be.
#[derive(serde::Serialize)]
struct EntryList<'a> {
    entries: Vec<&'a Entry>,
    next_cursor: Option<String>,
}

/// Entry cross-references: the customer must exist and the project must
/// belong to it. Violations are field errors (422), not 404s, because the
/// payload names them.
fn validate_entry_refs(
    store: &Store,
    draft: &crate::domain::EntryDraft,
) -> Result<Vec<FieldError>, crate::store::StoreError> {
    // #187: a store failure (corrupt customer doc, IO fault, oversized
    // collection) must never answer 422 "customer does not exist" — that would
    // be an infrastructure outage masquerading as client input. Only a provable
    // miss is a field error; a genuine store error propagates.
    let mut errors = Vec::new();
    let Some(customer) = store.get_customer(draft.customer_id)? else {
        // The whole reference tree is unreachable without its customer:
        // report every dangling reference at once (the documented 422 shape).
        errors.push(FieldError::new("customer_id", "customer does not exist"));
        errors.push(FieldError::new(
            "project_code",
            "project does not exist for this customer",
        ));
        if draft.task_code.is_some() {
            errors.push(FieldError::new(
                "task_code",
                "task does not exist for this project",
            ));
        }
        return Ok(errors);
    };
    if store
        .get_project(customer.id, &draft.project_code)?
        .is_none()
    {
        errors.push(FieldError::new(
            "project_code",
            "project does not exist for this customer",
        ));
    }
    // A task, when present, must belong to the entry's project.
    if let Some(task_code) = &draft.task_code
        && store
            .get_task(customer.id, &draft.project_code, task_code)?
            .is_none()
    {
        errors.push(FieldError::new(
            "task_code",
            "task does not exist for this project",
        ));
    }
    Ok(errors)
}

/// Map `validate_entry_refs`' two failure kinds: a store fault is the API
/// error it is; collected field errors are the documented 422.
/// #185 semantics + #218 placement: consulted from INSIDE the write-lock
/// transaction of the guarded store calls, so no issue can race in between
/// checking and writing. Unverifiable lock state fails closed (503).
fn ensure_unlocked(app: &AppState, id: Uuid, action: &str) -> Result<(), ApiError> {
    match app.locks.entry_lock(id) {
        Ok(Some(reason)) => Err(ApiError::conflict(reason.message())),
        Ok(None) => Ok(()),
        Err(e) => {
            tracing::warn!(error = ?e.0, entry = %id, "lock state unavailable; refusing entry {action}");
            Err(ApiError::lock_unavailable())
        }
    }
}

fn ensure_entry_refs(store: &Store, draft: &crate::domain::EntryDraft) -> Result<(), ApiError> {
    let errors = validate_entry_refs(store, draft)?;
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ApiError::validation(errors))
    }
}

/// `GET /entries`: either `?date=` for one day or `?from=&to=` for a range,
/// with `?limit=`/`?cursor=` for the page (#334).
#[derive(Debug, serde::Deserialize)]
pub struct EntryQuery {
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    limit: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
}

pub async fn list_entries(
    State(app): State<AppState>,
    actor: AuthUser,
    Query(q): Query<EntryQuery>,
) -> ApiResult {
    // #334: the page is part of the query, so a malformed `limit`/`cursor` is
    // the caller's error — resolved (and refused) before the scan runs.
    let page = pagination::page_request(q.limit.as_ref(), q.cursor.as_ref())?;
    // #335: an admin sees every user's rows, so the scan is already the answer;
    // a member reads through the per-user side index, which decodes only their
    // own documents. Both yield the same set, and the `visible_to` filter below
    // stays the one authority on visibility for either actor.
    let admin = actor.0.role == crate::auth::Role::Admin;
    let viewer = actor.0.id;
    // #254: the range/day scans read the whole entry tree from disk; run them
    // on the blocking pool so a large scan never stalls an async worker.
    let entries = match (&q.date, &q.from, &q.to) {
        (Some(date), None, None) => {
            let date = parse_date(date)?;
            if admin {
                store_blocking("entry day list", &app.store, move |s| s.list_by_date(date)).await?
            } else {
                store_blocking("entry day list", &app.store, move |s| {
                    s.list_by_date_for_user(date, viewer)
                })
                .await?
            }
        }
        (None, Some(_), Some(_)) => {
            let (from, to) = parse_range(q.from.as_ref(), q.to.as_ref())?;
            if admin {
                store_blocking("entry range list", &app.store, move |s| {
                    s.list_range(from, to)
                })
                .await?
            } else {
                store_blocking("entry range list", &app.store, move |s| {
                    s.list_range_for_user(from, to, viewer)
                })
                .await?
            }
        }
        _ => {
            return Err(ApiError::bad_request(
                "query needs 'date' or 'from' and 'to'",
            ));
        }
    };
    // Private-per-user (#51): a member sees only their own entries — and only
    // afterwards is the page cut, so a page for a member holds `limit` of
    // *their* rows, not whatever survived someone else's filtering.
    let mut items: Vec<&Entry> = entries
        .iter()
        .filter(|e| visible_to(&actor.0, e.user_id))
        .collect();
    // #334: the documented total order — `(date, id)` ascending — applied here
    // rather than relied on from the store, because a keyset cursor is only
    // sound over an order the contract fixes. `id` is unique and immutable, so
    // no two rows compare equal and a write cannot move one across the walk.
    items.sort_by_key(|e| (e.date, e.id));
    let (items, next_cursor) =
        pagination::paginate(items, |e| (e.date, e.id), &page, Order::Ascending);
    Ok(Json(EntryList {
        entries: items,
        next_cursor,
    })
    .into_response())
}

pub async fn create_entry(
    State(app): State<AppState>,
    actor: AuthUser,
    ValidJson(input): ValidJson<EntryInput>,
) -> ApiResult {
    let draft = validate_entry_input(&input).map_err(ApiError::validation)?;
    ensure_entry_refs(&app.store, &draft)?;
    let now = app.clock.now();
    let entry = Entry {
        id: Uuid::new_v4(),
        date: draft.date,
        customer_id: draft.customer_id,
        user_id: Some(actor.0.id),
        project_code: ProjectCode(draft.project_code),
        task_code: draft.task_code.map(ProjectCode),
        hours: draft.hours,
        note: draft.note,
        billable: draft.billable,
        source: crate::domain::Source::Manual,
        created_at: now,
        updated_at: now,
    };
    app.store.put_entry(&entry)?;
    Ok((StatusCode::CREATED, Json(&entry)).into_response())
}

pub async fn get_entry(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let entry = app
        .store
        .get_entry(id)?
        .filter(|e| visible_to(&actor.0, e.user_id))
        .ok_or_else(|| ApiError::not_found("entry"))?;
    Ok(Json(&entry).into_response())
}

pub async fn update_entry(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<EntryInput>,
) -> ApiResult {
    let existing = app
        .store
        .get_entry(id)?
        .filter(|e| visible_to(&actor.0, e.user_id))
        .ok_or_else(|| ApiError::not_found("entry"))?;
    let draft = validate_entry_input(&input).map_err(ApiError::validation)?;
    ensure_entry_refs(&app.store, &draft)?;
    let updated = Entry {
        id,
        date: draft.date,
        customer_id: draft.customer_id,
        user_id: existing.user_id,
        project_code: ProjectCode(draft.project_code),
        task_code: draft.task_code.map(ProjectCode),
        hours: draft.hours,
        note: draft.note,
        billable: draft.billable,
        source: existing.source,
        created_at: existing.created_at,
        updated_at: app.clock.now(),
    };
    // Relocation across day folders is one store transaction (review B4):
    // half-applying it would double-count the entry in every report. #218: the
    // lock check runs INSIDE that transaction, so an issue_invoice landing
    // between check and save cannot let the edit through.
    app.store
        .save_entry_guarded(Some(existing.date), &updated, || {
            ensure_unlocked(&app, id, "edit")
        })?;
    Ok(Json(&updated).into_response())
}

pub async fn delete_entry(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult {
    let entry = app
        .store
        .get_entry(id)?
        .filter(|e| visible_to(&actor.0, e.user_id))
        .ok_or_else(|| ApiError::not_found("entry"))?;
    // #218: verified inside the delete transaction (see update_entry).
    app.store
        .delete_entry_guarded(&entry, || ensure_unlocked(&app, id, "delete"))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// --------------------------------------------------------------- reports --

/// `GET /reports/summary`: a range plus optional grouping and billable filter.
#[derive(Debug, serde::Deserialize)]
pub struct SummaryQuery {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    billable: Option<String>,
}

pub async fn summary(
    State(app): State<AppState>,
    actor: AuthUser,
    Query(q): Query<SummaryQuery>,
) -> ApiResult {
    let (from, to) = parse_range(q.from.as_ref(), q.to.as_ref())?;
    let group = q.group.as_deref().unwrap_or("customer");
    let kind = match group {
        "customer" => report::Group::Customer,
        "project" => report::Group::Project,
        "person" => report::Group::Person,
        "week" => report::Group::Week,
        other => {
            return Err(ApiError::bad_request(format!(
                "unknown group '{other}' (use customer, project, person or week)"
            )));
        }
    };
    let billable_filter = match q.billable.as_deref() {
        None => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "'billable' must be true or false, got '{other}'"
            )));
        }
    };
    let mut entries = store_blocking("summary entries", &app.store, move |s| {
        s.list_range(from, to)
    })
    .await?;
    // Private-per-user (#51): members report only on their own time.
    entries.retain(|e| visible_to(&actor.0, e.user_id));
    let customers: Vec<Customer> =
        store_blocking("summary customers", &app.store, |s| s.list_customers()).await?;
    let (projects, users) = gather_hierarchy(&app, &customers).await?;
    let rows = report::summarise(
        &entries,
        &customers,
        &projects,
        &users,
        kind,
        billable_filter,
    );
    Ok(Json(rows).into_response())
}

/// Every project and user for a set of customers, loaded once for reports.
/// #189: the task tier was collected here for years after #177 removed the
/// last billing use of it — every report rate now resolves person → project →
/// customer, so the per-project `list_tasks` fan-out was pure cost.
type Hierarchy = (Vec<(Uuid, Project)>, Vec<User>);

async fn gather_hierarchy(app: &AppState, customers: &[Customer]) -> Result<Hierarchy, ApiError> {
    // #254: the per-customer project fan-out is a burst of small disk reads —
    // one blocking-pool job instead of one read per customer on the worker.
    let customers = customers.to_vec();
    store_blocking("report hierarchy", &app.store, move |s| {
        let mut projects: Vec<(Uuid, Project)> = Vec::new();
        for c in &customers {
            for p in s.list_projects(c.id)? {
                projects.push((c.id, p));
            }
        }
        let users = s.list_users()?;
        Ok((projects, users))
    })
    .await
}

/// `GET /reports/export.csv`: a range plus optional customer filter.
#[derive(Debug, serde::Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    customer_id: Option<String>,
}

pub async fn export_csv(
    State(app): State<AppState>,
    actor: AuthUser,
    Query(q): Query<ExportQuery>,
) -> ApiResult {
    let (from, to) = parse_range(q.from.as_ref(), q.to.as_ref())?;
    let filter = match &q.customer_id {
        Some(s) => Some(
            Uuid::parse_str(s)
                .map_err(|_| ApiError::bad_request("'customer_id' must be a UUID"))?,
        ),
        None => None,
    };
    let mut entries = store_blocking("export entries", &app.store, move |s| {
        s.list_range(from, to)
    })
    .await?;
    entries.retain(|e| visible_to(&actor.0, e.user_id));
    let customers = store_blocking("export customers", &app.store, |s| s.list_customers()).await?;
    let (projects, users) = gather_hierarchy(&app, &customers).await?;
    let csv = report::export_csv(&entries, &customers, &projects, &users, filter);
    Ok((
        [(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")],
        csv,
    )
        .into_response())
}

/// Profitability per customer AND currency: revenue (non-draft invoices) vs
/// cost (billable expenses + labour at each person's cost rate) (#29, #186).
pub async fn profitability(State(app): State<AppState>, Query(q): Query<RangeQuery>) -> ApiResult {
    let (from, to) = q.dates()?;
    let invoices = store_blocking("profit invoices", &app.store, |s| s.list_invoices()).await?;
    let expenses = store_blocking("profit expenses", &app.store, |s| s.list_expenses()).await?;
    let entries = store_blocking("profit entries", &app.store, move |s| {
        s.list_range(from, to)
    })
    .await?;
    let customers = store_blocking("profit customers", &app.store, |s| s.list_customers()).await?;
    let (projects, users) = gather_hierarchy(&app, &customers).await?;
    let result = report::summarise_profit(
        &invoices,
        &expenses,
        &entries,
        report::ProfitContext {
            customers: &customers,
            projects: &projects,
            users: &users,
        },
        from,
        to,
    );
    Ok(Json(result).into_response())
}

/// Budget burn vs budget for budgeted projects (#30).
pub async fn budget_report(State(app): State<AppState>) -> ApiResult {
    let customers = store_blocking("budget customers", &app.store, |s| s.list_customers()).await?;
    // Reuse the shared hierarchy walk (review D3) and the lifetime entry scan:
    // budgets are lifetime totals, so a date-windowed query would undercount.
    let (projects, users) = gather_hierarchy(&app, &customers).await?;
    let entries = store_blocking("budget entries", &app.store, |s| s.list_all_entries()).await?;
    let rows = crate::budgets::burn_report(&projects, &entries, &customers, &users);
    Ok(Json(serde_json::json!({ "rows": rows })).into_response())
}
