//! Settings → Backup (#new): the three schedule knobs, the reported time zone
//! and the manual ZIP writer. The archive layout is the restore contract:
//! data files at the ZIP root, so extracting into the data directory restores
//! the installation.

use crate::support::*;

use axum::http::StatusCode;
use serde_json::json;
use std::io::Read;

fn zip_names(path: &std::path::Path) -> Vec<String> {
    let mut archive =
        zip::ZipArchive::new(std::fs::File::open(path).expect("open zip")).expect("zip");
    (0..archive.len())
        .map(|index| {
            archive
                .by_index(index)
                .expect("zip entry")
                .name()
                .expect("entry name")
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn backup_settings_report_defaults_and_the_server_timezone() {
    let (client, _dir) = app().await;
    let (status, body) = json_req(&client, "GET", "/api/admin/backup", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], false);
    assert_eq!(body["cadence"], "daily");
    assert_eq!(body["time"], "02:00");
    assert_eq!(body["next_run"], serde_json::Value::Null);
    assert!(body["last_backup"].is_null());
    assert_eq!(body["backups"].as_array().unwrap().len(), 0);
    // The server always names the zone it schedules in.
    let zone = body["timezone"]["label"].as_str().expect("timezone label");
    assert!(
        zone == "UTC" || zone.starts_with("UTC+") || zone.starts_with("UTC-"),
        "unexpected time zone label: {zone}"
    );
    assert!(body["timezone"]["offset_minutes"].is_i64());
}

#[tokio::test]
async fn backup_settings_reject_invalid_values_without_persisting() {
    let (client, _dir) = app().await;
    for (payload, field) in [
        (
            json!({"enabled": true, "cadence": "hourly", "time": "02:00"}),
            "backup_cadence",
        ),
        (
            json!({"enabled": true, "cadence": "daily", "time": "25:00"}),
            "backup_time",
        ),
        (
            json!({"enabled": true, "cadence": "daily", "time": "2pm"}),
            "backup_time",
        ),
    ] {
        let (status, body) = json_req(&client, "PUT", "/api/admin/backup", Some(payload)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"]["fields"][0]["field"], field, "{body}");
    }
    // Nothing was persisted: the defaults still describe the schedule.
    let (_, body) = json_req(&client, "GET", "/api/admin/backup", None).await;
    assert_eq!(body["enabled"], false);
    assert_eq!(body["cadence"], "daily");
    assert_eq!(body["time"], "02:00");

    let (status, body) = json_req(
        &client,
        "PUT",
        "/api/admin/backup",
        Some(json!({"enabled": true, "cadence": "weekly", "time": "3:30"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["cadence"], "weekly");
    assert_eq!(body["time"], "03:30", "the stored time is zero-padded");
    assert!(
        body["next_run"].is_string(),
        "enabled backups name the next run"
    );
    assert!(
        body["next_run_local"]
            .as_str()
            .is_some_and(|label| label.len() == 16 && label.contains(' ')),
        "the next run is also labelled in the server zone"
    );
}

#[tokio::test]
async fn manual_backup_writes_a_timestamped_company_zip() {
    let (client, dir) = app().await;
    let (status, body) = json_req(
        &client,
        "PUT",
        "/api/admin/org",
        Some(json!({"name": "Capyabara Solutions Ltda"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    new_customer(&client, "Acme", "EUR", 6000).await;

    let (status, created) = json_req(&client, "POST", "/api/admin/backup/run", None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let name = created["file_name"].as_str().expect("file name");
    assert!(
        name.contains("_tatutime-backup_capyabarasolutionsltda.zip"),
        "company name and marker in {name}"
    );
    assert!(name.ends_with(".zip"));
    assert!(created["size_bytes"].as_u64().unwrap() > 0);

    let path = dir.path().join("Backup").join(name);
    assert!(path.is_file(), "backup lives in the sibling Backup folder");
    let names = zip_names(&path);
    assert!(
        names.iter().any(|entry| entry.starts_with("users/")),
        "the user store is in the archive: {names:?}"
    );
    assert!(
        names.iter().any(|entry| entry.starts_with("customers/")),
        "customer documents are in the archive: {names:?}"
    );
    assert!(
        names.iter().any(|entry| entry == "org_profile.json"),
        "singleton documents are at the archive root: {names:?}"
    );
    assert!(
        !names.iter().any(|entry| entry.starts_with("Data/")),
        "files sit at the root so restoring means extracting into the data dir: {names:?}"
    );
    assert!(
        !names
            .iter()
            .any(|entry| entry.ends_with(".tmp") || entry.ends_with(".lock")),
        "transient artefacts are never archived: {names:?}"
    );

    // The plaintext archive is restorable by hand, so it really is a ZIP.
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut config = String::new();
    archive
        .by_name("org_profile.json")
        .unwrap()
        .read_to_string(&mut config)
        .unwrap();
    assert!(config.contains("Capyabara Solutions Ltda"));

    // The status now reports the stored file and a last-backup timestamp.
    let (status, body) = json_req(&client, "GET", "/api/admin/backup", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["backups"].as_array().unwrap().len(), 1);
    assert_eq!(body["last_backup"]["file_name"], name);
}

#[tokio::test]
async fn backup_is_admin_only() {
    let (client, _dir) = app().await;
    let cookie = member_cookie(&client, "member@test.local", "supersecret1").await;
    for (method, path) in [
        ("GET", "/api/admin/backup"),
        ("PUT", "/api/admin/backup"),
        ("POST", "/api/admin/backup/run"),
    ] {
        let body = (method == "PUT")
            .then(|| json!({"enabled": false, "cadence": "daily", "time": "02:00"}));
        let (status, _body, _) = raw(&client.router, method, path, body, Some(&cookie)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
    }
}
