use crate::support::*;
use axum::http::StatusCode;
use serde_json::json;
use tucano_time::domain::{Currency, Hours, ImportOrigin, ImportProvider, ImportedTime};
use tucano_time::store::Store;
use uuid::Uuid;

#[tokio::test]
async fn externally_invoiced_entries_are_locked_and_unbilled_imports_keep_provenance() {
    let (app, dir) = app().await;
    let customer = new_customer(&app, "Example", "EUR", 6000).await;
    let cid = customer["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"name":"Work"})).await;
    let input = entry_body(cid, "P1", json!(2), "2026-01-01");
    let (status, created) = json_req(&app, "POST", "/api/entries", Some(input.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let mut entry = store.get_entry(id).unwrap().unwrap();
    entry.imported = Some(ImportedTime {
        origin: ImportOrigin {
            provider: ImportProvider::Harvest,
            namespace: "test".into(),
            batch_id: Uuid::new_v4(),
            source_key: "row:1".into(),
            invoiced: true,
        },
        currency: Currency("USD".into()),
        rate_minor: 6500,
        cost_rate_minor: 6000,
        source_hours: Hours(200),
        amount_minor: 13000,
        cost_amount_minor: 12000,
    });
    store.put_entry(&entry).unwrap();
    let path = format!("/api/entries/{id}");
    for method in ["PUT", "DELETE"] {
        let (status, _) = json_req(
            &app,
            method,
            &path,
            if method == "PUT" {
                Some(input.clone())
            } else {
                None
            },
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(store.get_entry(id).unwrap().unwrap(), entry);
    }
    entry.imported.as_mut().unwrap().origin.invoiced = false;
    store.put_entry(&entry).unwrap();
    let mut edited = input.clone();
    edited["note"] = json!("Updated note");
    edited["hours"] = json!(3);
    let (status, saved) = json_req(&app, "PUT", &path, Some(edited)).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["imported"],
        serde_json::to_value(&entry.imported).unwrap()
    );
    let mut forged = input;
    forged["imported"] = saved["imported"].clone();
    let (status, _) = json_req(&app, "POST", "/api/entries", Some(forged)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn imported_invoice_preview_and_download_preserve_original_bytes_and_fail_closed() {
    use sha2::{Digest, Sha256};
    use tucano_time::domain::{ImportedInvoice, Invoice, InvoiceStatus};
    let (app, dir) = app().await;
    let customer = new_customer(&app, "Example", "EUR", 6000).await;
    let (status, created) = json_req(&app, "POST", "/api/invoices/manual", Some(json!({"customer_id":customer["id"],"lines":[{"description":"Work","quantity_hundredths":100,"unit_price_minor":10000,"item_kind":"service"}]}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let mut invoice: Invoice = serde_json::from_value(created).unwrap();
    let path = format!("/api/invoices/{}/pdf", invoice.id);
    let (status, _, original) = raw_req(&app, "GET", &format!("{path}/preview")).await;
    assert_eq!(status, StatusCode::OK);
    let checksum = Sha256::digest(&original)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    invoice.status = InvoiceStatus::Issued;
    invoice.imported = Some(ImportedInvoice {
        origin: ImportOrigin {
            provider: ImportProvider::Harvest,
            namespace: "test".into(),
            batch_id: Uuid::new_v4(),
            source_key: "invoice:1".into(),
            invoiced: true,
        },
        original_number: "H-1".into(),
        pdf_sha256: checksum,
        opening_paid_minor: 0,
    });
    let store = Store::open(dir.path().join("data")).unwrap();
    store.put_invoice(&invoice).unwrap();
    let archive = dir
        .path()
        .join("data/invoices")
        .join(format!("{}.pdf", invoice.id));
    std::fs::write(&archive, &original).unwrap();
    for url in [path.clone(), format!("{path}/preview")] {
        let (status, _, bytes) = raw_req(&app, "GET", &url).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(bytes, original);
    }
    std::fs::write(&archive, b"changed").unwrap();
    let (status, _, _) = raw_req(&app, "GET", &path).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    std::fs::remove_file(&archive).unwrap();
    let (status, _, _) = raw_req(&app, "GET", &format!("{path}/preview")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        !archive.exists(),
        "a missing original must not be regenerated"
    );
}

/// #398/#218: a submitted week omits externally invoiced history, answered from
/// the week's own records rather than a scan of every entry in the deployment.
#[tokio::test]
async fn submission_skips_externally_invoiced_entries() {
    let (app, dir) = app().await;
    let customer = new_customer(&app, "Example", "EUR", 6000).await;
    let cid = customer["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"name":"Work"})).await;
    let body = entry_body(cid, "P1", json!(2), "2026-01-05");
    let (status, first) = json_req(&app, "POST", "/api/entries", Some(body.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, second) = json_req(&app, "POST", "/api/entries", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    let first_id = Uuid::parse_str(first["id"].as_str().unwrap()).unwrap();
    let second_id = Uuid::parse_str(second["id"].as_str().unwrap()).unwrap();

    let store = Store::open(dir.path().join("data")).unwrap();
    let mut entry = store.get_entry(first_id).unwrap().unwrap();
    entry.imported = Some(ImportedTime {
        origin: ImportOrigin {
            provider: ImportProvider::Harvest,
            namespace: "test".into(),
            batch_id: Uuid::new_v4(),
            source_key: "row:1".into(),
            invoiced: true,
        },
        currency: Currency("EUR".into()),
        rate_minor: 6000,
        cost_rate_minor: 0,
        source_hours: Hours(200),
        amount_minor: 12000,
        cost_amount_minor: 0,
    });
    store.put_entry(&entry).unwrap();

    let (status, submission) = json_req(
        &app,
        "POST",
        "/api/submissions",
        Some(json!({"week_start":"2026-01-05"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{submission}");
    let submitted: Vec<Uuid> = submission["entry_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| Uuid::parse_str(id.as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(submitted, vec![second_id], "{submission}");
    assert!(!submitted.contains(&first_id));
}

// ---------------------------------------------------------- admin import API --

const IMPORT_TIME_HEADER: &str = "Date,Client,Project,Project Code,Task,Notes,Hours,Billable?,Invoiced?,First Name,Last Name,Employee Id,Billable Rate,Billable Amount,Cost Rate,Cost Amount,Currency\n";
const IMPORT_ROW: &str = "2026-01-05,Acme,Website,WEB,Design,Work,2.00,Yes,No,Ada,Lovelace,E1,65.00,130.00,50.00,100.00,EUR\n";

async fn import_fixture(app: &Client) -> serde_json::Value {
    let customer = new_customer(app, "Acme", "EUR", 6000).await;
    let cid = customer["id"].as_str().unwrap();
    new_project(app, cid, "WEB", json!({"name":"Website","rate_minor":6500})).await;
    let (status, task) = json_req(
        app,
        "POST",
        &format!("/api/customers/{cid}/projects/WEB/tasks"),
        Some(json!({"code":"DESIGN","name":"Design"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{task}");
    let (status, category) = json_req(
        app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Travel","default_billable":true})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{category}");
    let (status, me) = json_req(app, "GET", "/api/auth/me", None).await;
    assert_eq!(status, StatusCode::OK);
    json!({
        "users": [{"person":"Ada Lovelace","employee_id":"E1","user_id":me["id"],"skip":false}],
        "customers": [{"action":"existing","source_client":"Acme","customer_id":cid}],
        "projects": [{"action":"existing","source_client":"Acme","source_project":"Website","source_code":"WEB","customer_id":cid,"code":"WEB"}],
        "tasks": [{"action":"existing","source_task":"Design","code":"DESIGN"}],
        "categories": [{"action":"existing","source_category":"Travel","category_id":category["id"]}],
        "invoices": [],
        "excluded": [],
        "invoice_links": []
    })
}

async fn new_import_batch(app: &Client) -> String {
    let (status, batch) = json_req(
        app,
        "POST",
        "/api/admin/imports/harvest",
        Some(json!({"namespace":"harvest-account"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{batch}");
    batch["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn import_api_is_admin_only_and_validates_uploads() {
    let (app, _d) = app().await;
    let base = "/api/admin/imports/harvest";
    let (anon, _, _) = raw(&app.router, "GET", base, None, None).await;
    assert_eq!(anon, StatusCode::UNAUTHORIZED);
    let member = member_cookie(&app, "import-member@test.local", "memberpass123").await;
    let (forbidden, _, _) = raw(&app.router, "GET", base, None, Some(&member)).await;
    assert_eq!(forbidden, StatusCode::FORBIDDEN);

    let id = new_import_batch(&app).await;
    let files = format!("{base}/{id}/files/time");
    let invalid = b"not,a,harvest,report\n".to_vec();
    let (status, _) = body_req(&app, "PUT", &files, "text/csv", invalid.clone()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let valid = format!("{IMPORT_TIME_HEADER}{IMPORT_ROW}").into_bytes();
    let (status, _) = body_req(&app, "PUT", &files, "text/csv", valid.clone()).await;
    assert_eq!(status, StatusCode::CREATED);

    // A rejected replacement leaves the previous valid source untouched.
    let (status, _) = body_req(&app, "PUT", &files, "text/csv", invalid).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _, stored) = raw_req(&app, "GET", &files).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored, valid);

    let oversized = vec![b'x'; 1024 * 1024 + 1];
    let (status, _) = body_req(&app, "PUT", &files, "text/csv", oversized).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn import_api_previews_commits_retries_and_rejects_stale_revision() {
    let (app, _d) = app().await;
    let base = "/api/admin/imports/harvest";
    let choices = import_fixture(&app).await;
    let id = new_import_batch(&app).await;
    let csv = format!("{IMPORT_TIME_HEADER}{IMPORT_ROW}").into_bytes();
    let (status, _) = body_req(
        &app,
        "PUT",
        &format!("{base}/{id}/files/time"),
        "text/csv",
        csv,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, batch) =
        json_req(&app, "PUT", &format!("{base}/{id}/review"), Some(choices)).await;
    assert_eq!(status, StatusCode::OK, "{batch}");
    let (status, preview) = json_req(&app, "POST", &format!("{base}/{id}/preview"), None).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["ready"], true, "{preview}");
    let revision = preview["revision"].as_str().unwrap().to_string();

    let (status, outcome) = json_req(
        &app,
        "POST",
        &format!("{base}/{id}/commit"),
        Some(json!({"revision":revision})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["entries"].as_array().unwrap().len(), 1);

    // Identical retry returns the same ids, not a second record.
    let (status, retry) = json_req(
        &app,
        "POST",
        &format!("{base}/{id}/commit"),
        Some(json!({"revision":revision})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(retry["entries"], outcome["entries"]);
    let (status, history) = json_req(&app, "GET", base, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history["total"], 1);

    // A mapping change after preview invalidates the revision.
    let stale_id = new_import_batch(&app).await;
    body_req(
        &app,
        "PUT",
        &format!("{base}/{stale_id}/files/time"),
        "text/csv",
        format!("{IMPORT_TIME_HEADER}{IMPORT_ROW}").into_bytes(),
    )
    .await;
    let (_, stale_batch) = json_req(
        &app,
        "PUT",
        &format!("{base}/{stale_id}/review"),
        Some(json!({
            "users":[], "customers":[], "projects":[], "tasks":[], "categories":[],
            "invoices":[], "excluded":[], "invoice_links":[]
        })),
    )
    .await;
    assert_eq!(stale_batch["state"], "draft");
    let (status, rows) = json_req(&app, "GET", &format!("{base}/{stale_id}/rows"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rows["ready"], false, "{rows}");
    assert!(rows["blocking"].as_u64().unwrap() > 0);
}
