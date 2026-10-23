//! In-app ZIP backups (Settings → Backup).
//!
//! The operator CLI still archives to `.tar.gz` with a sha256 manifest
//! (`--backup`); this surface is the scheduled/manual counterpart the GUI
//! drives. A backup is a plain ZIP of everything below the data directory,
//! with the files at the archive root so restoring is "extract into the data
//! directory". Backups are **not** encrypted yet — they contain `secrets.bin`
//! and `vault.key`, exactly as the data directory does.

use super::*;

use chrono::{Local, Offset};

use crate::backup::{self, Cadence};

fn cadence_from(
    cfg: &crate::appconfig::AppConfig,
    env: &dyn Fn(&str) -> Option<String>,
) -> Cadence {
    Cadence::parse(&cfg.get_str("backup_cadence", env)).unwrap_or(Cadence::Daily)
}

fn time_from(
    cfg: &crate::appconfig::AppConfig,
    env: &dyn Fn(&str) -> Option<String>,
) -> (u32, u32) {
    crate::appconfig::parse_time_of_day(&cfg.get_str("backup_time", env)).unwrap_or((2, 0))
}

/// The running app's local time zone, as a small, client-safe object. The
/// schedule is evaluated in this zone, so Settings shows exactly what the time
/// means. `label` is fixed-width (`UTC+02:00`) and an abbreviation such as
/// `CEST` rides along when the platform provides one.
fn timezone_json() -> serde_json::Value {
    let now = Local::now();
    let offset = now.offset().fix().local_minus_utc();
    let label = if offset == 0 {
        "UTC".to_string()
    } else {
        let sign = if offset < 0 { '-' } else { '+' };
        let seconds = offset.unsigned_abs();
        format!(
            "UTC{sign}{:02}:{:02}",
            seconds / 3600,
            (seconds % 3600) / 60
        )
    };
    let abbreviation = now.format("%Z").to_string();
    let abbreviation = if abbreviation.is_empty() || abbreviation == label {
        None
    } else {
        Some(abbreviation)
    };
    serde_json::json!({
        "offset_minutes": offset / 60,
        "label": label,
        "abbreviation": abbreviation,
    })
}

fn file_json(file: &backup::BackupFile) -> serde_json::Value {
    serde_json::json!({
        "file_name": file.file_name,
        "size_bytes": file.size_bytes,
        "created_at": file.created_at.to_rfc3339(),
    })
}

/// The complete Settings payload: the three stored knobs plus the live
/// schedule facts (time zone, next run, last backup and the stored files).
fn status_json(app: &AppState) -> serde_json::Value {
    let cfg = app.cfg();
    let env = |key: &str| app.env_var(key);
    let enabled = cfg.get_bool("backup_enabled", &env);
    let cadence = cadence_from(&cfg, &env);
    let time = time_from(&cfg, &env);
    let now = app.clock.now();
    let destination = backup::configured_directory(app.store.root(), &env);
    let backups = backup::list_backups_in(&destination).unwrap_or_else(|error| {
        tracing::warn!(error = %error, "backup directory could not be listed");
        Vec::new()
    });
    let next_run_at = enabled
        .then(|| backup::next_run(now, cadence, time))
        .flatten();
    let next_run = next_run_at.map(|at| at.to_rfc3339());
    let next_run_local = next_run_at.map(|at| {
        at.with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    });
    let last_backup = backups.first().map(file_json);
    serde_json::json!({
        "enabled": enabled,
        "cadence": cadence.as_str(),
        "time": format!("{:02}:{:02}", time.0, time.1),
        "timezone": timezone_json(),
        "next_run": next_run,
        "next_run_local": next_run_local,
        "last_backup": last_backup,
        "backups": backups.iter().map(file_json).collect::<Vec<_>>(),
    })
}

/// `GET /api/admin/backup` — settings, time zone, next run and stored files.
pub async fn backup_settings(State(app): State<AppState>) -> ApiResult {
    Ok(Json(status_json(&app)).into_response())
}

/// The three editable knobs. Everything else in the status payload is derived.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupSettingsInput {
    pub enabled: bool,
    pub cadence: String,
    pub time: String,
}

/// `PUT /api/admin/backup` — persist the three backup knobs into config.json.
/// The generic configuration endpoint could also write them; this one validates
/// them together and returns the refreshed schedule in one round-trip.
pub async fn update_backup_settings(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<BackupSettingsInput>,
) -> ApiResult {
    let mut cfg =
        crate::appconfig::AppConfig::load(app.store.root()).map_err(ApiError::corrupt_document)?;
    let mut patch = serde_json::Map::new();
    patch.insert(
        "backup_enabled".to_string(),
        serde_json::Value::Bool(input.enabled),
    );
    patch.insert(
        "backup_cadence".to_string(),
        serde_json::Value::String(input.cadence),
    );
    patch.insert(
        "backup_time".to_string(),
        serde_json::Value::String(input.time),
    );
    cfg.update(patch).map_err(|error| {
        ApiError::validation(vec![FieldError::new(
            error.key.unwrap_or_else(|| "backup".to_string()),
            error.message,
        )])
    })?;
    app.reload_config(Arc::new(cfg));
    record_audit(&app, "backup_settings_updated", "backup").await;
    Ok(Json(status_json(&app)).into_response())
}

/// `POST /api/admin/backup/run` — write a backup now. The response describes
/// the created file; the archive itself stays in the backup directory.
pub async fn run_backup(State(app): State<AppState>) -> ApiResult {
    let org = super::invoicing::org_for(&app)?;
    let now = app.clock.now();
    let env = |key: &str| app.env_var(key);
    let destination = backup::configured_directory(app.store.root(), &env);
    let created =
        backup::run_in(&destination, app.store.root(), &org.name, now).map_err(|error| {
            tracing::error!(error = %error, "manual backup failed");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "backup_failed",
                "the backup could not be written; check the server log",
            )
        })?;
    record_audit(&app, "backup_created", &created.file_name).await;
    Ok(Json(serde_json::json!({
        "file_name": created.file_name,
        "size_bytes": created.size_bytes,
        "created_at": created.created_at.to_rfc3339(),
    }))
    .into_response())
}
