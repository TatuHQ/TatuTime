//! Admin Harvest import API (#402).
//!
//! Every route is admin-tier and lives below `/api/admin/imports/harvest`; the
//! ordinary entry/expense/invoice handlers gain no import escape hatch. Uploads
//! are raw bodies (CSV/ZIP), never base64 JSON, and are structurally validated
//! before they replace a previously staged source. Preview and rows recompute
//! the #400 plan from the stored batch; commit revalidates it under the store
//! writer lock and returns the journaled outcome.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::domain::{FieldError, ImportChoices};
use crate::store::{
    ImportBatch, ImportBatchState, ImportFiles, ImportKind, MAX_IMPORT_CSV_BYTES,
    MAX_IMPORT_ZIP_BYTES,
};

fn kind(raw: &str) -> Option<ImportKind> {
    match raw {
        "time" => Some(ImportKind::Time),
        "expenses" => Some(ImportKind::Expenses),
        "invoices" => Some(ImportKind::Invoices),
        _ => None,
    }
}

fn parse_kind(raw: &str) -> Result<ImportKind, ApiError> {
    kind(raw).ok_or_else(|| ApiError::not_found("import source kind"))
}

async fn batch(app: &AppState, id: Uuid) -> Result<ImportBatch, ApiError> {
    store_blocking("import batch read", &app.store, move |s| {
        s.get_import_batch(id)
    })
    .await?
    .ok_or_else(|| ApiError::not_found("import batch"))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBatchInput {
    #[serde(default)]
    pub namespace: Option<String>,
}

/// `POST /api/admin/imports/harvest` — create a draft batch.
pub async fn create_import_batch(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<CreateBatchInput>,
) -> ApiResult {
    let namespace = input
        .namespace
        .unwrap_or_else(|| "harvest".into())
        .trim()
        .to_ascii_lowercase();
    let valid = !namespace.is_empty()
        && namespace.len() <= 64
        && namespace
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !valid {
        return Err(ApiError::validation(vec![FieldError::new(
            "namespace",
            "1-64 lowercase letters, digits, '-' or '_'",
        )]));
    }
    let now = app.clock.now();
    let batch = ImportBatch {
        id: Uuid::new_v4(),
        namespace,
        state: ImportBatchState::Draft,
        created_at: now,
        updated_at: now,
        files: ImportFiles::default(),
        choices: ImportChoices::default(),
        preview_revision: None,
        commit: None,
        error: None,
    };
    let created = batch.clone();
    store_blocking("import batch create", &app.store, move |s| {
        s.create_import_batch(batch)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(created)).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPageQuery {
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn page_bounds(q: &ImportPageQuery) -> (usize, usize) {
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    (offset, limit)
}

/// `GET /api/admin/imports/harvest` — paginated history, newest first.
pub async fn list_import_batches(
    State(app): State<AppState>,
    Query(q): Query<ImportPageQuery>,
) -> ApiResult {
    let (offset, limit) = page_bounds(&q);
    let batches = store_blocking("import history", &app.store, |s| s.list_import_batches()).await?;
    let total = batches.len();
    let items: Vec<_> = batches.into_iter().skip(offset).take(limit).collect();
    Ok(Json(json!({
        "total": total,
        "offset": offset,
        "limit": limit,
        "items": items,
    }))
    .into_response())
}

/// `GET /api/admin/imports/harvest/{id}` — one batch and its outcome.
pub async fn get_import_batch(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    Ok(Json(batch(&app, id).await?).into_response())
}

/// `DELETE /api/admin/imports/harvest/{id}` — discard a batch that never
/// committed, including every staged source. A committed batch is financial
/// history and answers 409; delete its records through their own lifecycle.
pub async fn delete_import_batch(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    store_blocking("import batch discard", &app.store, move |s| {
        s.delete_import_batch(id)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `PUT /api/admin/imports/harvest/{id}/files/{kind}` — stage/replace one
/// source. The structural parser runs before the old file is replaced.
pub async fn upload_import_source(
    State(app): State<AppState>,
    Path((id, raw_kind)): Path<(Uuid, String)>,
    body: Bytes,
) -> ApiResult {
    let kind = parse_kind(&raw_kind)?;
    let limit = match kind {
        ImportKind::Invoices => MAX_IMPORT_ZIP_BYTES,
        ImportKind::Time | ImportKind::Expenses => MAX_IMPORT_CSV_BYTES,
    };
    if body.len() > limit {
        return Err(ApiError::payload_too_large());
    }
    let current = batch(&app, id).await?;
    if current.state == ImportBatchState::Committed {
        return Err(ApiError::conflict("a committed import batch is immutable"));
    }
    match kind {
        ImportKind::Time => {
            crate::harvest::parse(crate::harvest::ReportKind::Time, &body).map_err(|issues| {
                ApiError::validation(vec![FieldError::new(
                    "file",
                    format!("time CSV cannot be parsed: {} issue(s)", issues.len()),
                )])
            })?;
        }
        ImportKind::Expenses => {
            crate::harvest::parse(crate::harvest::ReportKind::Expense, &body).map_err(
                |issues| {
                    ApiError::validation(vec![FieldError::new(
                        "file",
                        format!("expense CSV cannot be parsed: {} issue(s)", issues.len()),
                    )])
                },
            )?;
        }
        ImportKind::Invoices => {
            crate::harvest_invoices::inspect_invoice_zip(&body).map_err(|error| {
                ApiError::validation(vec![FieldError::new("file", error.message)])
            })?;
        }
    }
    let first = !current.files.contains(kind);
    let name = match kind {
        ImportKind::Time => "time.csv",
        ImportKind::Expenses => "expenses.csv",
        ImportKind::Invoices => "invoices.zip",
    };
    let now = app.clock.now();
    let updated = store_blocking("import source stage", &app.store, move |s| {
        s.stage_import_source(id, kind, name, &body, now)
    })
    .await?;
    let status = if first {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(updated)).into_response())
}

/// `GET /api/admin/imports/harvest/{id}/files/{kind}` — admin-only source
/// download for review. Only allowlisted kinds are addressable.
pub async fn download_import_source(
    State(app): State<AppState>,
    Path((id, raw_kind)): Path<(Uuid, String)>,
) -> ApiResult {
    let kind = parse_kind(&raw_kind)?;
    let bytes = store_blocking("import source read", &app.store, move |s| {
        s.read_import_source(id, kind)
    })
    .await?
    .ok_or_else(|| ApiError::not_found("import source"))?;
    let content_type = match kind {
        ImportKind::Invoices => "application/zip",
        ImportKind::Time | ImportKind::Expenses => "text/csv; charset=utf-8",
    };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"import-source\"".to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// `PUT /api/admin/imports/harvest/{id}/review` — replace reviewed mappings.
pub async fn review_import_batch(
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
    ValidJson(choices): ValidJson<ImportChoices>,
) -> ApiResult {
    let now = app.clock.now();
    let updated = store_blocking("import review", &app.store, move |s| {
        s.update_import_choices(id, choices, now)
    })
    .await?;
    Ok(Json(updated).into_response())
}

/// `POST /api/admin/imports/harvest/{id}/preview` — recompute the plan.
pub async fn preview_import_batch(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    let preview =
        store_blocking("import preview", &app.store, move |s| s.import_preview(id)).await?;
    if preview.ready {
        let revision = preview.revision.clone();
        let now = app.clock.now();
        let _ = store_blocking("import review mark", &app.store, move |s| {
            s.mark_import_reviewed(id, &revision, now)
        })
        .await;
    }
    Ok(Json(preview).into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowsQuery {
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub source: Option<String>,
}

/// `GET /api/admin/imports/harvest/{id}/rows` — paginated preview rows.
pub async fn import_rows(
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<RowsQuery>,
) -> ApiResult {
    let (offset, limit) = page_bounds(&ImportPageQuery {
        offset: q.offset,
        limit: q.limit,
    });
    let preview =
        store_blocking("import preview", &app.store, move |s| s.import_preview(id)).await?;
    let mut rows: Vec<serde_json::Value> = Vec::new();
    if q.source.as_deref().is_none_or(|source| source == "time") {
        rows.extend(
            preview
                .time
                .iter()
                .map(|row| json!({"source":"time","row":row})),
        );
    }
    if q.source.as_deref().is_none_or(|source| source == "expense") {
        rows.extend(
            preview
                .expenses
                .iter()
                .map(|row| json!({"source":"expense","row":row})),
        );
    }
    if q.source.as_deref().is_none_or(|source| source == "invoice") {
        rows.extend(
            preview
                .invoices
                .iter()
                .map(|row| json!({"source":"invoice","row":row})),
        );
    }
    let total = rows.len();
    let items: Vec<_> = rows.into_iter().skip(offset).take(limit).collect();
    Ok(Json(json!({
        "revision": preview.revision,
        "ready": preview.ready,
        "blocking": preview.blocking,
        "totals": preview.totals,
        "issues": preview.issues,
        "total": total,
        "offset": offset,
        "limit": limit,
        "items": items,
    }))
    .into_response())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitImportInput {
    pub revision: String,
}

/// `POST /api/admin/imports/harvest/{id}/commit` — confirm the exact preview.
pub async fn commit_import_batch(
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<CommitImportInput>,
) -> ApiResult {
    let now = app.clock.now();
    let outcome = store_blocking("import commit", &app.store, move |s| {
        s.commit_import(id, &input.revision, now)
    })
    .await?;
    Ok(Json(outcome).into_response())
}

/// `GET /api/admin/imports/harvest/{id}/invoices/{sha}/pdf` — the original
/// archived member, never a regenerated document.
pub async fn imported_invoice_pdf(
    State(app): State<AppState>,
    Path((id, sha256)): Path<(Uuid, String)>,
) -> ApiResult {
    let bytes = store_blocking("import invoice pdf", &app.store, move |s| {
        s.import_invoice_pdf(id, &sha256)
    })
    .await?
    .ok_or_else(|| ApiError::not_found("imported invoice PDF"))?;
    let response: Response = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/pdf".to_string()),
            (
                header::CONTENT_DISPOSITION,
                "inline; filename=\"imported-invoice.pdf\"".to_string(),
            ),
        ],
        bytes,
    )
        .into_response();
    Ok(response)
}
