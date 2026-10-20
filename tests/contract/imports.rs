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
