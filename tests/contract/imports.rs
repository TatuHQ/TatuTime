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

#[tokio::test]
async fn import_api_discards_uncommitted_batches_and_keeps_committed_history() {
    let (app, _d) = app().await;
    let base = "/api/admin/imports/harvest";
    let id = new_import_batch(&app).await;
    let files = format!("{base}/{id}/files/time");
    let time = format!("{IMPORT_TIME_HEADER}{IMPORT_ROW}").into_bytes();
    let (status, _) = body_req(&app, "PUT", &files, "text/csv", time).await;
    assert_eq!(status, StatusCode::CREATED);

    // A discard removes the batch, its staged source and its history entry.
    let (status, _) = json_req(&app, "DELETE", &format!("{base}/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = json_req(&app, "GET", &format!("{base}/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = raw_req(&app, "GET", &files).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, history) = json_req(&app, "GET", base, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history["total"], 0, "{history}");
    // Discarding a missing batch is a plain 404, not a second success.
    let (status, _) = json_req(&app, "DELETE", &format!("{base}/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A committed batch is financial history: 409, and every record survives.
    let choices = import_fixture(&app).await;
    let committed = new_import_batch(&app).await;
    body_req(
        &app,
        "PUT",
        &format!("{base}/{committed}/files/time"),
        "text/csv",
        format!("{IMPORT_TIME_HEADER}{IMPORT_ROW}").into_bytes(),
    )
    .await;
    json_req(
        &app,
        "PUT",
        &format!("{base}/{committed}/review"),
        Some(choices),
    )
    .await;
    let (status, preview) =
        json_req(&app, "POST", &format!("{base}/{committed}/preview"), None).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let revision = preview["revision"].as_str().unwrap();
    let (status, _) = json_req(
        &app,
        "POST",
        &format!("{base}/{committed}/commit"),
        Some(json!({"revision":revision})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = json_req(&app, "DELETE", &format!("{base}/{committed}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, batch) = json_req(&app, "GET", &format!("{base}/{committed}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(batch["state"], "committed", "{batch}");
    let (status, entries) = json_req(
        &app,
        "GET",
        "/api/entries?from=2026-01-05&to=2026-01-05",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(entries["entries"].as_array().unwrap().len(), 1, "{entries}");
}

#[tokio::test]
async fn competing_commits_return_one_outcome_and_never_alter_history() {
    let (app, _d) = app().await;
    let base = "/api/admin/imports/harvest";
    let choices = import_fixture(&app).await;
    let id = new_import_batch(&app).await;
    body_req(
        &app,
        "PUT",
        &format!("{base}/{id}/files/time"),
        "text/csv",
        format!("{IMPORT_TIME_HEADER}{IMPORT_ROW}").into_bytes(),
    )
    .await;
    json_req(&app, "PUT", &format!("{base}/{id}/review"), Some(choices)).await;
    let (status, preview) = json_req(&app, "POST", &format!("{base}/{id}/preview"), None).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let revision = preview["revision"].as_str().unwrap().to_string();
    let commit = format!("{base}/{id}/commit");
    let body = json!({"revision": revision});

    // Two confirmations race on the same revision: the store serializes them
    // and the second returns the first outcome instead of a second record.
    let (first, second) = tokio::join!(
        json_req(&app, "POST", &commit, Some(body.clone())),
        json_req(&app, "POST", &commit, Some(body.clone())),
    );
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert_eq!(second.0, StatusCode::OK, "{}", second.1);
    assert_eq!(first.1["entries"], second.1["entries"]);
    assert_eq!(first.1["invoices"], second.1["invoices"]);
    let (status, entries) = json_req(
        &app,
        "GET",
        "/api/entries?from=2026-01-05&to=2026-01-05",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(entries["entries"].as_array().unwrap().len(), 1, "{entries}");

    // A competitor holding a different revision is refused and leaves the
    // committed financial history untouched.
    let (status, _) = json_req(
        &app,
        "POST",
        &commit,
        Some(json!({"revision": format!("{revision}-competing")})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, entries) = json_req(
        &app,
        "GET",
        "/api/entries?from=2026-01-05&to=2026-01-05",
        None,
    )
    .await;
    assert_eq!(entries["entries"].as_array().unwrap().len(), 1, "{entries}");
}

// ------------------------------------------------ three-source import (#404) --

const EXPENSE_SOURCE_HEADER: &str = "Date,Client,Project,Project Code,Category,Notes,Amount,Units,Billable,Invoiced?,Approved?,First name,Last name,Employee Id,Currency\n";

fn invoice_zip_named(member: &str) -> Vec<u8> {
    use std::io::{Cursor, Write as _};
    let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            member,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(pdf).unwrap();
    writer.finish().unwrap().into_inner()
}

fn invoice_zip() -> Vec<u8> {
    invoice_zip_named("invoices/H-1.pdf")
}

async fn manual_draft(client: &Client, cid: &serde_json::Value) -> serde_json::Value {
    let (status, invoice) = json_req(
        client,
        "POST",
        "/api/invoices/manual",
        Some(json!({
            "customer_id": cid,
            "lines": [{
                "description": "Work",
                "quantity_hundredths": 100,
                "unit_price_minor": 10000,
                "item_kind": "service"
            }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{invoice}");
    invoice
}

#[tokio::test]
async fn time_expenses_and_original_invoice_pdf_commit_together() {
    use sha2::{Digest, Sha256};
    let (app, dir) = app().await;
    let base = "/api/admin/imports/harvest";
    let mut choices = import_fixture(&app).await;
    let cid = choices["customers"][0]["customer_id"].clone();
    let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
    let sha = Sha256::digest(pdf)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    choices["invoices"] = json!([{
        "sha256": sha,
        "customer_id": cid,
        "original_number": "H-1",
        "currency": "EUR",
        "invoice_date": "2026-01-31",
        "period_from": "2026-01-01",
        "period_to": "2026-01-31",
        "total_minor": 10000,
        "opening_paid_minor": 0,
        "status": "issued",
        "excluded": false
    }]);

    let id = new_import_batch(&app).await;
    let time = format!("{IMPORT_TIME_HEADER}{IMPORT_ROW}");
    let expense = format!(
        "{EXPENSE_SOURCE_HEADER}2026-01-06,Acme,Website,WEB,Travel,Train,12.00,,Yes,No,Yes,Ada,Lovelace,E1,EUR\n"
    );
    for (kind, content_type, bytes) in [
        ("time", "text/csv", time.into_bytes()),
        ("expenses", "text/csv", expense.into_bytes()),
        ("invoices", "application/zip", invoice_zip()),
    ] {
        let (status, _) = body_req(
            &app,
            "PUT",
            &format!("{base}/{id}/files/{kind}"),
            content_type,
            bytes,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{kind}");
    }
    let (status, batch) =
        json_req(&app, "PUT", &format!("{base}/{id}/review"), Some(choices)).await;
    assert_eq!(status, StatusCode::OK, "{batch}");
    let (status, preview) = json_req(&app, "POST", &format!("{base}/{id}/preview"), None).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["ready"], true, "{preview}");
    let (status, outcome) = json_req(
        &app,
        "POST",
        &format!("{base}/{id}/commit"),
        Some(json!({"revision": preview["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["entries"].as_array().unwrap().len(), 1);
    assert_eq!(outcome["expenses"].as_array().unwrap().len(), 1);
    assert_eq!(outcome["invoices"], json!(["H-1"]));

    let (status, invoices) = json_req(&app, "GET", "/api/invoices", None).await;
    assert_eq!(status, StatusCode::OK);
    let invoice = invoices["invoices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|invoice| invoice["number"] == "H-1")
        .expect("imported invoice");
    assert_eq!(invoice["imported"]["original_number"], "H-1");
    let (status, _, archived) = raw_req(
        &app,
        "GET",
        &format!("/api/invoices/{}/pdf", invoice["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(archived, pdf, "the original PDF is preserved byte-for-byte");

    // #404: the original bytes and provenance survive backup/restore.
    let backup = dir.path().join("import-backup.tar.gz");
    tucano_time::backup::create(&dir.path().join("data"), &backup).unwrap();
    let restored_dir = tempfile::tempdir().unwrap();
    let restored_root = restored_dir.path().join("data");
    tucano_time::backup::restore(&backup, &restored_root, false).unwrap();
    let restored = Store::open(&restored_root).unwrap();
    let restored_invoice = restored
        .find_invoice_by_number("H-1")
        .unwrap()
        .expect("restored imported invoice");
    assert_eq!(
        restored
            .invoice_pdf_bytes(restored_invoice.id)
            .unwrap()
            .unwrap(),
        pdf,
        "backup/restore preserves the original PDF bytes"
    );
}

#[tokio::test]
async fn imported_legal_numbers_raise_the_local_high_water_across_restart_and_deleted_drafts() {
    use sha2::{Digest, Sha256};
    let (app, dir) = app().await;
    let base = "/api/admin/imports/harvest";
    let mut choices = import_fixture(&app).await;
    let cid = choices["customers"][0]["customer_id"].clone();
    let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
    let sha = Sha256::digest(pdf)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    choices["invoices"] = json!([{
        "sha256": sha,
        "customer_id": cid,
        "original_number": "INV-0042",
        "currency": "EUR",
        "invoice_date": "2026-01-31",
        "period_from": "2026-01-01",
        "period_to": "2026-01-31",
        "total_minor": 10000,
        "opening_paid_minor": 0,
        "status": "issued",
        "excluded": false
    }]);
    let id = new_import_batch(&app).await;
    let (status, _) = body_req(
        &app,
        "PUT",
        &format!("{base}/{id}/files/invoices"),
        "application/zip",
        invoice_zip_named("invoices/INV-0042.pdf"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    json_req(&app, "PUT", &format!("{base}/{id}/review"), Some(choices)).await;
    let (status, preview) = json_req(&app, "POST", &format!("{base}/{id}/preview"), None).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let (status, outcome) = json_req(
        &app,
        "POST",
        &format!("{base}/{id}/commit"),
        Some(json!({"revision": preview["revision"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["invoices"], json!(["INV-0042"]));

    // The imported legal number is preserved, and the next local draft continues
    // after it rather than reusing 0043 or restarting at 0001.
    let first = manual_draft(&app, &cid).await;
    assert_eq!(first["number"], "INV-0043", "{first}");
    let (status, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/invoices/{}", first["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let second = manual_draft(&app, &cid).await;
    assert_eq!(
        second["number"], "INV-0044",
        "a deleted draft never frees its number"
    );

    // A restart reads the durable ledger back, so the high-water mark survives.
    let restarted = Client::new(Store::open(dir.path().join("data")).unwrap(), None).await;
    let third = manual_draft(&restarted, &cid).await;
    assert_eq!(third["number"], "INV-0045", "{third}");
}

/// #404: the real supplied exports are private and never live in the repo or
/// CI. An operator exercises them locally by naming their folder; the test
/// retains only counts and SHA-256 hashes in its output, never a row, an
/// amount or PDF text.
#[test]
fn supplied_private_exports_parse_only_when_explicitly_enabled() {
    use sha2::{Digest, Sha256};
    let Ok(folder) = std::env::var("TUCANOTIME_HARVEST_SAMPLES") else {
        eprintln!(
            "skip: set TUCANOTIME_HARVEST_SAMPLES to the folder holding the private Harvest exports"
        );
        return;
    };
    let dir = std::path::PathBuf::from(folder);
    let read = |name: &str| {
        std::fs::read(dir.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"))
    };
    let time = read("harvest_time_report.csv");
    let expenses = read("harvest_expense_report.csv");
    let invoices = read("harvest_invoice_pack.zip");
    let time_report = tucano_time::harvest::parse(tucano_time::harvest::ReportKind::Time, &time)
        .unwrap_or_else(|issues| panic!("time export has {} issue(s)", issues.len()));
    let expense_report =
        tucano_time::harvest::parse(tucano_time::harvest::ReportKind::Expense, &expenses)
            .unwrap_or_else(|issues| panic!("expense export has {} issue(s)", issues.len()));
    let sources = tucano_time::harvest_invoices::inspect_invoice_zip(&invoices)
        .unwrap_or_else(|error| panic!("invoice pack: {}", error.message));
    assert!(!time_report.rows.is_empty(), "the time export has rows");
    assert!(
        !expense_report.rows.is_empty(),
        "the expense export has rows"
    );
    assert!(!sources.is_empty(), "the invoice pack has PDFs");
    for (name, bytes) in [
        ("time", &time),
        ("expenses", &expenses),
        ("invoices", &invoices),
    ] {
        let digest = Sha256::digest(bytes);
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        eprintln!("{name}: {} bytes, sha256 {hex}", bytes.len());
    }
    eprintln!(
        "time rows: {}, expense rows: {}, invoice PDFs: {}",
        time_report.rows.len(),
        expense_report.rows.len(),
        sources.len()
    );
}
