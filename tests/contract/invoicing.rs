use crate::support::*;

use axum::http::StatusCode;
use axum::http::header;
use serde_json::Value;
use serde_json::json;

// --------------------------------------------------------------- invoices --

#[tokio::test]
async fn invoice_sums_billable_and_ignores_nonbillable() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    // 3h billable @60 = 180.00
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    // 5h NON-billable -> excluded
    json_req(&app, "POST", "/api/entries", Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":5,"billable":false}))).await;

    let (s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    assert_eq!(inv["status"], "draft");
    assert_eq!(inv["currency"], "EUR");
    assert_eq!(inv["lines"].as_array().unwrap().len(), 1);
    assert_eq!(inv["total_minor"], 18000);
}

#[tokio::test]
async fn issuing_invoice_locks_its_entries() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    let (_, e) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let eid = e["id"].as_str().unwrap();

    let (_, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();

    // Draft does NOT lock.
    let (s_before, _) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":4})),
    )
    .await;
    assert_eq!(
        s_before,
        StatusCode::OK,
        "draft invoice must not lock entries"
    );

    // QA-02: the edit left the draft stale, so regenerate it before issuing.
    let (s_drop, b_drop) = json_req(&app, "DELETE", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(s_drop, StatusCode::NO_CONTENT, "{b_drop}");
    let (s_fresh, fresh) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s_fresh, StatusCode::CREATED, "{fresh}");
    let iid = fresh["id"].as_str().unwrap();

    // Issue -> now locked.
    let (si, b_issue) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(si, StatusCode::OK, "{b_issue}");
    let (s_edit, body) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":4})),
    )
    .await;
    assert_eq!(s_edit, StatusCode::CONFLICT, "{body}");
    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(s_del, StatusCode::CONFLICT);
}

#[tokio::test]
async fn regeneration_excludes_invoiced_entries() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{}/issue", inv["id"].as_str().unwrap()),
        None,
    )
    .await;
    // Second invoice over the same period has nothing left to bill.
    let (s2, body) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn invoice_rejects_mixed_currencies() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"currency":"EUR","rate_minor":6000})).await;
    new_project(&app, cid, "P2", json!({"currency":"USD","rate_minor":3000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":1})),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P2","hours":1})),
    )
    .await;
    let (s, body) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("currencies")
    );
}

#[tokio::test]
async fn invoice_includes_billable_expenses_and_locks_them() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    // 2h billable time @60 = 12000
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
    )
    .await;
    // billable expense 50.00 = 5000
    let (_, exp) = json_req(&app, "POST", "/api/expenses", Some(json!({"date":"2026-10-03","customer_id":cid,"project_code":"P1","amount_minor":5000,"currency":"EUR"}))).await;
    let xid = exp["id"].as_str().unwrap().to_string();

    // Invoice includes both -> 12000 + 5000 = 17000, two lines of different kinds.
    let (_, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(inv["total_minor"], 17000);
    let kinds: Vec<&str> = inv["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"time") && kinds.contains(&"expense"),
        "{kinds:?}"
    );

    // Issue, then the expense is locked from deletion.
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{}/issue", inv["id"].as_str().unwrap()),
        None,
    )
    .await;
    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None).await;
    assert_eq!(
        s_del,
        StatusCode::CONFLICT,
        "issued invoice locks its expense"
    );
}

#[tokio::test]
async fn invoice_can_exclude_expenses() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-03","customer_id":cid,"amount_minor":5000,"currency":"EUR"})),
    )
    .await;
    let (_, inv) = json_req(&app, "POST", "/api/invoices", Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07","include_expenses":false}))).await;
    assert_eq!(inv["total_minor"], 12000, "expenses excluded");
    assert!(
        inv["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| l["kind"] == "time")
    );
}

// ------------------------------------------------------------------ #113 ---

#[tokio::test]
async fn invoice_pdf_archived_at_issue_and_downloadable() {
    let (app, d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;

    // The invoice JSON carries the hint.
    let hint = &issued["pdf"];
    assert_eq!(
        hint["filename"],
        format!("{}.pdf", issued["number"].as_str().unwrap())
    );
    assert_eq!(hint["sha256"].as_str().unwrap().len(), 64);
    assert!(hint["bytes"].as_u64().unwrap() > 100);
    assert!(hint["archived_at"].is_string());

    // The file is on disk next to the document, atomically written at issue.
    let pdf_path = d
        .path()
        .join("data")
        .join("invoices")
        .join(format!("{iid}.pdf"));
    assert!(pdf_path.exists(), "archive written at issue");
    let on_disk = std::fs::read(&pdf_path).unwrap();

    // Download contract: admin 200 + binary headers, ETag from the sha256.
    let (status, headers, body) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/pdf");
    let disp = headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        disp.contains(format!("filename=\"{}.pdf\"", issued["number"].as_str().unwrap()).as_str()),
        "{disp}"
    );
    let etag = headers[header::ETAG].to_str().unwrap().to_string();
    assert_eq!(etag, format!("\"{}\"", hint["sha256"].as_str().unwrap()));
    assert!(body.starts_with(b"%PDF-1.4"), "bytes start with %PDF");
    assert_eq!(body, on_disk, "download is the archived bytes");

    // Independent sha: body hashes to the advertised ETag.
    use sha2::{Digest, Sha256};
    let sha = Sha256::digest(&body);
    assert_eq!(
        sha.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        hint["sha256"].as_str().unwrap()
    );

    // Re-download: same bytes (immutable archive, stable ETag).
    let (s2, h2, body2) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(h2[header::ETAG], headers[header::ETAG]);
    assert_eq!(body, body2);
}

#[tokio::test]
async fn invoice_pdf_legacy_invoice_resolves_byte_stably() {
    let (app, d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    let (_s, headers, first) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert!(headers[header::ETAG].to_str().unwrap().starts_with('"'));

    // Simulate a legacy archive: the file vanishes while the (immutable)
    // invoice document stays. The first read must re-render **identical**
    // bytes and re-persist the archive.
    std::fs::remove_file(
        d.path()
            .join("data")
            .join("invoices")
            .join(format!("{iid}.pdf")),
    )
    .unwrap();
    let (_s2, _h2, second) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(first, second, "legacy re-render is byte-stable");
    let path = d
        .path()
        .join("data")
        .join("invoices")
        .join(format!("{iid}.pdf"));
    assert!(path.exists(), "archive re-persisted on first read");
    assert_eq!(std::fs::read(&path).unwrap(), second);
    // The hint's sha still matches (recomputed, but from identical bytes).
    let (_s3, fresh) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    use sha2::{Digest, Sha256};
    assert_eq!(
        fresh["pdf"]["sha256"].as_str().unwrap(),
        Sha256::digest(&second)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
}

#[tokio::test]
async fn invoice_pdf_rejects_draft_and_unknown() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "NOEMAIL", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();

    // Draft: 409, nothing persisted.
    let (status, _h, body) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(String::from_utf8_lossy(&body).contains("issue the invoice"));

    // Unknown id: 404, never a PDF-shaped answer.
    let (s404, _, _) = raw_req(
        &app,
        "GET",
        "/api/invoices/00000000-0000-0000-0000-000000000000/pdf",
    )
    .await;
    assert_eq!(s404, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn member_cannot_download_invoice_pdf() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Eve","email":"eve@test.local","password":"evepass123","role":"member"}),
        ),
    )
    .await;
    let cookie = login_cookie(&app.router, "eve@test.local", "evepass123").await;
    let (s, _, _) = raw(
        &app.router,
        "GET",
        &format!("/api/invoices/{iid}/pdf"),
        None,
        Some(&cookie),
    )
    .await;
    // Invoices are the admin tier (#51): the whole surface refuses a member.
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn invoice_email_carries_the_archived_pdf_attachment() {
    let (app, _d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;
    let (_s, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;

    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    let (name, bytes) = msgs[0]
        .attachment
        .as_ref()
        .expect("the invoice email carries the PDF (#113)");
    assert_eq!(*name, format!("{}.pdf", issued["number"].as_str().unwrap()));
    assert_eq!(*bytes, pdf, "the attachment is the archived document");
    assert!(
        msgs[0].text.contains("PDF document is attached"),
        "{}",
        msgs[0].text
    );

    // The MIME builder labels the part as a PDF. `build_mime` now returns a
    // Result -- it refuses a header-bound control character at the sink (#S1)
    // -- so the same two assertions run on the unwrapped body.
    let mime = tucano_time::email::build_mime(&msgs[0]).expect("the message builds");
    assert!(mime.contains("Content-Type: application/pdf"));
    assert!(mime.contains("base64"));
}

// ------------------------------------------------------------------ #138 ---

#[tokio::test]
async fn org_profile_round_trips_and_prints_on_the_document() {
    let (app, _d) = app().await;
    // Default read: everything empty.
    let (_s0, empty) = json_req(&app, "GET", "/api/admin/org", None).await;
    assert_eq!(empty["name"], "");
    assert!(empty.get("address").is_none() || empty["address"].is_null());

    // Validation before persistence.
    let (sb, bb) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"X","legal_id":"","address":{"street":"a","country":""}})),
    )
    .await;
    assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY, "{bb}");
    let (_sv, before) = json_req(&app, "GET", "/api/admin/org", None).await;
    assert_eq!(before["name"], "", "rejection persisted nothing");

    // Valid save round-trips.
    let (sp, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"Tucano BV","legal_id":"BE0123456789",
          "address":{"street":"Markt 1","city":"Brugge","postal_code":"8000","country":"BE"}})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{saved}");
    assert_eq!(saved["name"], "Tucano BV");

    // The identity prints on the issued document (uncompressed streams, so
    // the bytes are greppable — the determinism contract of #113 in use).
    let (issued, iid) = seed_issued_invoice(&app).await;
    let (_s2, _h2, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let text = String::from_utf8_lossy(&pdf).into_owned();
    // The layout emits one text-showing op per WORD (positions carry the
    // spacing), so decode the literals back into running text.
    let joined: String = text
        .split('(')
        .skip(1)
        .map(|s| s.split(')').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(joined.contains("BE0123456789"), "legal id on the PDF");
    assert!(
        joined.contains("Tucano BV"),
        "profile name replaces the config default"
    );
    assert!(joined.contains("Brugge"), "address on the PDF");
    assert_eq!(issued["status"], "issued");

    // Member: admin tier on both verbs.
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Eve","email":"eve@test.local","password":"evepass123","role":"member"}),
        ),
    )
    .await;
    let cookie = login_cookie(&app.router, "eve@test.local", "evepass123").await;
    let (sm, _, _) = raw(&app.router, "GET", "/api/admin/org", None, Some(&cookie)).await;
    assert_eq!(sm, StatusCode::FORBIDDEN);
    let (spm, _, _) = raw(
        &app.router,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"Evil"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(spm, StatusCode::FORBIDDEN);
}

// ------------------------------------------------------------------ #143 ---

#[tokio::test]
async fn manual_invoice_creation_computes_exact_totals() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    let (s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,"tax_hundredths":2100,"discount_hundredths":1000,
          "lines":[{"description":"Office seat","item_kind":"product","quantity_hundredths":250,"unit_price_minor":4000,"project_code":"P1"},
                   {"description":"Setup service","item_kind":"service","quantity_hundredths":100,"unit_price_minor":5000}]})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    assert_eq!(inv["status"], "draft");
    assert!(inv["number"].as_str().unwrap().starts_with("INV-"));
    assert_eq!(inv["currency"], "EUR");
    let lines = inv["lines"].as_array().unwrap();
    assert_eq!(lines[0]["amount_minor"], 10000); // 2.50 x 40.00
    assert_eq!(lines[0]["kind"], "fixed");
    assert_eq!(lines[0]["item_kind"], "product");
    assert_eq!(lines[0]["project_code"], "P1");
    assert_eq!(lines[1]["amount_minor"], 5000);
    // subtotal 15000, discount 10% = 1500, VAT 21% of 13500 = 2835, total 16335.
    assert_eq!(inv["total_minor"], 16335);
    assert_eq!(inv["tax_hundredths"], 2100);
    assert_eq!(inv["discount_hundredths"], 1000);
    // Draft locks nothing and sends nothing.
    let eid = json_req(&app, "GET", "/api/entries", None).await;
    let _ = eid;
    let pdf = raw_req(
        &app,
        "GET",
        &format!("/api/invoices/{}/pdf", inv["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(
        pdf.0,
        StatusCode::CONFLICT,
        "draft has no archived document"
    );
}

#[tokio::test]
async fn manual_invoice_validation_matrix_persists_nothing() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    let base = |lines: Value, extra: Value| -> Value {
        let mut o = json!({"customer_id": cid, "lines": lines});
        if let (Some(o1), Some(o2)) = (o.as_object_mut(), extra.as_object()) {
            for (k, v) in o2 {
                o1.insert(k.clone(), v.clone());
            }
        }
        o
    };
    let good = json!([{"description":"X","item_kind":"product","quantity_hundredths":100,"unit_price_minor":100}]);
    for (label, body) in [
        ("empty lines", base(json!([]), json!({}))),
        (
            "zero qty",
            base(
                json!([{"description":"X","item_kind":"product","quantity_hundredths":0,"unit_price_minor":100}]),
                json!({}),
            ),
        ),
        (
            "bad percent",
            base(good.clone(), json!({"tax_hundredths": 10001})),
        ),
        (
            "unknown project",
            base(
                json!([{"description":"X","item_kind":"product","quantity_hundredths":100,"unit_price_minor":100,"project_code":"NOPE"}]),
                json!({}),
            ),
        ),
        ("unknown field", base(good.clone(), json!({"evil": true}))),
        (
            "float quantity",
            json!({"customer_id": cid, "lines": [{"description":"X","item_kind":"product","quantity_hundredths":1.5,"unit_price_minor":100}]}),
        ),
    ] {
        let (st, body) = json_req(&app, "POST", "/api/invoices/manual", Some(body)).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{label}: {body}");
    }
    let (_s, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert!(
        list["invoices"].as_array().unwrap().is_empty(),
        "nothing persisted"
    );
}

#[tokio::test]
async fn draft_editing_replaces_lines_and_issued_stays_immutable() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_s, gent) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = gent["id"].as_str().unwrap().to_string();
    let tracked = &gent["lines"][0];
    assert_eq!(tracked["amount_minor"], 18000);

    // Draft edit: keep the tracked line verbatim + add a manual line, tax 10%.
    let edit = json!({"lines": [tracked,
        {"kind":"fixed","date":"2026-10-02","note":"Extra hosting","item_kind":"service",
         "quantity_hundredths":100,"unit_price_minor":900}],
        "tax_hundredths": 1000, "discount_hundredths": 0});
    let (se, edited) = json_req(&app, "PUT", &format!("/api/invoices/{iid}"), Some(edit)).await;
    assert_eq!(se, StatusCode::OK, "{edited}");
    assert_eq!(edited["lines"].as_array().unwrap().len(), 2);
    // 18000 + 900 = 18900 subtotal; 10% = 1890; total 20790.
    assert_eq!(edited["total_minor"], 20790);
    // The tracked line survived untouched (snapshot #8).
    assert_eq!(edited["lines"][0]["amount_minor"], 18000);
    assert_eq!(edited["lines"][0]["entry_id"], tracked["entry_id"]);

    // Tampering with a tracked line's amount is refused; nothing persisted.
    let mut tampered = tracked.clone();
    tampered["amount_minor"] = json!(1);
    let (st, bt) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(json!({"lines":[tampered],"tax_hundredths":0,"discount_hundredths":0})),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{bt}");
    let (_sr, again) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(again["total_minor"], 20790, "rejection persisted nothing");

    // Issue: snapshot locks the document; PUT now conflicts.
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (si, _) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(json!({"lines":[],"tax_hundredths":0})),
    )
    .await;
    assert_eq!(si, StatusCode::CONFLICT);
    // The archived PDF shows the EDITED lines (the document follows the
    // draft at issue time, not the earlier generation).
    let (_sp, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let joined: String = String::from_utf8_lossy(&pdf)
        .split('(')
        .skip(1)
        .map(|x| x.split(')').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        joined.contains("Extra hosting"),
        "manual line reached the document"
    );
    assert!(joined.contains("207.90"), "total with VAT printed");
}

// ------------------------------------------------- #187: validation gaps --

#[tokio::test]
async fn draft_line_input_rejects_unknown_fields_before_persisting() {
    // #187: the stored InvoiceLine was used directly as the PUT body
    // element without deny_unknown_fields — a typo'd or stale client field
    // was silently dropped. The split input DTO is strict.
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    let (_s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,
          "lines":[{"description":"Seat","item_kind":"product","quantity_hundredths":100,"unit_price_minor":4000}]})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    let good = json!({"kind":"fixed","date":"2026-10-02","note":"Seat",
                      "item_kind":"product","quantity_hundredths":100,"unit_price_minor":4000});
    // Outer envelope was always strict; prove the LINE element now is too.
    let mut stale = good.clone();
    stale["amountTtc"] = json!(1); // legacy/typo key
    let (s, b) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(json!({"lines":[stale],"tax_hundredths":0,"discount_hundredths":0})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    // Sanity: the same edit without the unknown field succeeds.
    let (ok, _) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(json!({"lines":[good],"tax_hundredths":0,"discount_hundredths":0})),
    )
    .await;
    assert_eq!(ok, StatusCode::OK);
}

#[tokio::test]
async fn manual_line_bounds_reject_identically_on_both_endpoints() {
    // #187: create (domain::validate_manual_lines) and edit previously kept
    // two copies of the bounds. One shared checker now guards both.
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    let (s1, b1) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,
          "lines":[{"description":"Seat","item_kind":"product","quantity_hundredths":1_000_001,"unit_price_minor":4000}]})),
    )
    .await;
    assert_eq!(s1, StatusCode::UNPROCESSABLE_ENTITY, "{b1}");
    let (_s, draft) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,
          "lines":[{"description":"Seat","item_kind":"product","quantity_hundredths":100,"unit_price_minor":4000}]})),
    )
    .await;
    let iid = draft["id"].as_str().unwrap().to_string();
    let (s2, b2) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(
            json!({"lines":[{"kind":"fixed","date":"2026-10-02","note":"Seat",
            "item_kind":"product","quantity_hundredths":1_000_001,"unit_price_minor":4000}],
            "tax_hundredths":0,"discount_hundredths":0}),
        ),
    )
    .await;
    assert_eq!(s2, StatusCode::UNPROCESSABLE_ENTITY, "{b2}");
    let msg = b2["error"]["fields"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        msg == b1["error"]["fields"][0]["message"]
            .as_str()
            .unwrap_or_default(),
        "both endpoints must report the same bound: {msg} vs {b1}"
    );
}

// ------------------------------------------------------------------ #134 ---

#[tokio::test]
async fn invoice_preview_projects_and_paid_exclusion() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "PA", json!({"rate_minor": 5000})).await;
    new_project(&app, &cid, "PB", json!({"rate_minor": 7000})).await;
    for (proj, day) in [("PA", "2026-10-02"), ("PB", "2026-10-03")] {
        json_req(
            &app,
            "POST",
            "/api/entries",
            Some(json!({"date":day,"customer_id":cid.clone(),"project_code":proj,"hours":2})),
        )
        .await;
    }
    // Preview only PA: exactly its 2h x 50 = 10000, nothing persisted.
    let (sp, preview) = json_req(
        &app,
        "POST",
        "/api/invoices/preview",
        Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-31","project_codes":["PA"]})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{preview}");
    assert_eq!(preview["number"], "", "preview is unsaved");
    let lines = preview["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["project_code"], "PA");
    assert_eq!(preview["total_minor"], 10000);
    let (_s0, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert!(
        list["invoices"].as_array().unwrap().is_empty(),
        "preview persists nothing"
    );

    // Unknown project for the customer: 422.
    let (su, _) = json_req(
        &app,
        "POST",
        "/api/invoices/preview",
        Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-31","project_codes":["NOPE"]})),
    )
    .await;
    assert_eq!(su, StatusCode::UNPROCESSABLE_ENTITY);

    // Save PA for real, pay it fully, then generate again: PA must be
    // excluded now (any non-draft invoice's lines are billed work).
    let (_s1, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-31","project_codes":["PA"]})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference":"x"})),
    )
    .await;
    let (s2, again) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-31"})),
    )
    .await;
    assert_eq!(s2, StatusCode::CREATED, "{again}");
    let l2 = again["lines"].as_array().unwrap();
    assert_eq!(l2.len(), 1, "only PB remains billable");
    assert_eq!(l2[0]["project_code"], "PB");
    assert_eq!(again["total_minor"], 14000);
    // Issue that one too; with every line on a non-draft invoice the period
    // is exhausted -> NothingToInvoice 409.
    let iid2 = again["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid2}/issue"), None).await;
    // And the fully-invoiced period repeats -> NothingToInvoice 409.
    let (s3, _b3) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-31"})),
    )
    .await;
    assert_eq!(s3, StatusCode::CONFLICT);
}

// ------------------------------------------------------------------ #147 ---

#[tokio::test]
async fn item_type_catalog_crud_and_editor_prefill_contract() {
    let (app, _d) = app().await;
    let (s, item) = json_req(
        &app,
        "POST",
        "/api/admin/item-types",
        Some(
            json!({"name":"Support hour","kind":"service","description":"Extended support block",
                    "default_price_minor":9500,"currency":"EUR"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{item}");
    let tid = item["id"].as_str().unwrap().to_string();
    assert_eq!(item["active"], true);
    for bad in [
        json!({"name":"","kind":"service"}),
        json!({"name":"X","kind":"product","default_price_minor":100_000_001}),
    ] {
        let (sb, _) = json_req(&app, "POST", "/api/admin/item-types", Some(bad)).await;
        assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let (_sl, list) = json_req(&app, "GET", "/api/admin/item-types", None).await;
    assert_eq!(list["item_types"].as_array().unwrap().len(), 1);
    let (sa, archived) = json_req(
        &app,
        "PUT",
        &format!("/api/admin/item-types/{tid}"),
        Some(json!({"name":"Support hour","kind":"service","default_price_minor":9500,"currency":"EUR","active":false})),
    )
    .await;
    assert_eq!(sa, StatusCode::OK, "{archived}");
    assert_eq!(archived["active"], false);
    assert_eq!(archived["id"], tid);
    let (s4, _) = json_req(
        &app,
        "PUT",
        "/api/admin/item-types/00000000-0000-0000-0000-000000000000",
        Some(json!({"name":"X","kind":"service"})),
    )
    .await;
    assert_eq!(s4, StatusCode::NOT_FOUND);
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Eve","email":"eve@test.local","password":"evepass123","role":"member"}),
        ),
    )
    .await;
    let cookie = login_cookie(&app.router, "eve@test.local", "evepass123").await;
    let (sm, _, _) = raw(
        &app.router,
        "GET",
        "/api/admin/item-types",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(sm, StatusCode::FORBIDDEN);
    let (sd, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/admin/item-types/{tid}"),
        None,
    )
    .await;
    assert_eq!(sd, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn document_labels_reach_the_pdf_and_validation_binds() {
    let (app, _d) = app().await;
    let (sb, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"","footer":"","labels":{"total":"a very long label that goes over the forty character bound"}})),
    )
    .await;
    assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"","footer":"","labels":{"description":"Service delivered","total":"Amount due"}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    assert_eq!(saved["labels"]["total"], "Amount due");
    let (issued, iid) = seed_issued_invoice(&app).await;
    assert_eq!(issued["status"], "issued");
    let (_sp, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let joined: String = String::from_utf8_lossy(&pdf)
        .split('(')
        .skip(1)
        .map(|x| x.split(')').next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        joined.contains("Amount due"),
        "custom total label on the PDF"
    );
    assert!(
        joined.contains("Service delivered"),
        "custom description column label"
    );
    let (_sg, got) = json_req(&app, "GET", "/api/admin/invoice-template", None).await;
    assert_eq!(
        got["template"]["labels"]["description"],
        "Service delivered"
    );
}

// ------------------------------------------------------------------ #146 ---

#[tokio::test]
async fn appearance_and_messages_are_honored_not_inert() {
    let (app, _d) = app().await;
    // Invalid shapes are refused before persistence.
    let (sb, _) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"X","reply_to":"nope"})),
    )
    .await;
    assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY);
    let (sb2, _) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"X","accent":"e95420"})),
    )
    .await;
    assert_eq!(sb2, StatusCode::UNPROCESSABLE_ENTITY);
    let (_sv, before) = json_req(&app, "GET", "/api/admin/org", None).await;
    assert_eq!(before["name"], "", "rejections persisted nothing");

    // Valid save round-trips.
    let (s, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"Tucano BV","from_name":"Tucano Invoicing",
                    "reply_to":"billing@tucano.test","accent":"#E95420"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    assert_eq!(saved["from_name"], "Tucano Invoicing");

    // The mail adapter honors the sender identity (#146): the recorded
    // message carries it and build_mime would render From/Reply-To.
    let (issued, iid) = seed_issued_invoice(&app).await;
    let _ = issued;
    let (se, _b) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK);
    let msg = &app.email.messages()[0];
    assert_eq!(msg.from_name.as_deref(), Some("Tucano Invoicing"));
    assert_eq!(msg.reply_to.as_deref(), Some("billing@tucano.test"));

    // The accent paints the newly issued document: color ops exist, and
    // re-rendering is byte-identical (the #113 determinism contract).
    let (_s2, _h2, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert!(pdf.windows(3).any(|w| w == b" rg"), "accent emitted");
    let (_s3, _h3, pdf2) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(pdf, pdf2, "accented bytes deterministic");

    // Clearing the accent restores the ink-only document (no color ops).
    json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"Tucano BV","from_name":"","reply_to":"","accent":""})),
    )
    .await;
    let c = new_customer(&app, "PLAIN", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "PZ", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-11-02","customer_id":cid,"project_code":"PZ","hours":1})),
    )
    .await;
    let (_s4, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-11-01","to":"2026-11-07"})),
    )
    .await;
    let iid2 = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid2}/issue"), None).await;
    let (_s5, _h5, plain) = raw_req(&app, "GET", &format!("/api/invoices/{iid2}/pdf")).await;
    assert!(
        !plain.windows(3).any(|w| w == b" rg"),
        "cleared accent = no color"
    );
}

/// #262 Tier 2: issuing — an empty period is a 409 and persists nothing; a
/// double issue is refused (409) and never re-locks or duplicates.
#[tokio::test]
async fn invoice_issue_empty_period_conflicts_and_double_issue_is_refused() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;

    // No billable, not-yet-invoiced work in the period -> 409, nothing stored.
    let (s_empty, body) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-01-01","to":"2026-01-07"})),
    )
    .await;
    assert_eq!(s_empty, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "conflict");
    let (_, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert!(
        list["invoices"].as_array().unwrap().is_empty(),
        "an empty period must persist no invoice"
    );

    // Issue once, then a second issue is refused.
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(3), "2026-02-02")),
    )
    .await;
    let (sc, draft) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-02-01","to":"2026-02-07"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{draft}");
    let iid = draft["id"].as_str().unwrap();
    assert_eq!(
        json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None)
            .await
            .0,
        StatusCode::OK
    );
    let (s_again, again_body) =
        json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(s_again, StatusCode::CONFLICT, "{again_body}");
}

// ------------------------------------------- invoice timeline (Subtask A) --

/// Read `GET /api/invoices/{id}/timeline` and return its `events` array.
async fn timeline(app: &Client, iid: &str) -> Value {
    let (s, body) = json_req(app, "GET", &format!("/api/invoices/{iid}/timeline"), None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    body["events"].clone()
}

#[tokio::test]
async fn invoice_timeline_records_create_issue_and_email_newest_first() {
    let (app, _d) = app().await;
    // seed_issued_invoice generates a tracked draft, then issues it.
    let (issued, iid) = seed_issued_invoice(&app).await;

    let events = timeline(&app, &iid).await;
    assert_eq!(events.as_array().unwrap().len(), 2, "{events}");
    // Newest first: issued precedes created.
    assert_eq!(events[0]["kind"], "issued");
    assert_eq!(events[0]["description"], "Invoice issued.");
    assert_eq!(events[0]["actor_name"], "Admin");
    assert_eq!(events[1]["kind"], "created");
    assert_eq!(events[1]["description"], "Invoice created.");
    assert_eq!(events[1]["actor_name"], "Admin");
    // A stable, unambiguous timestamp for the GUI to format later.
    assert!(events[0]["at"].as_str().unwrap().contains('T'));
    // `created`/`issued` carry no meta, so the field is omitted.
    assert!(events[0].get("meta").is_none(), "{events}");

    // The email route appends `email_sent` with recipient + From + transport.
    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    let events = timeline(&app, &iid).await;
    assert_eq!(events.as_array().unwrap().len(), 3, "{events}");
    assert_eq!(events[0]["kind"], "email_sent");
    assert_eq!(events[0]["actor_name"], "Admin");
    assert_eq!(events[0]["meta"]["to"], "billing@acme.test");
    assert_eq!(events[0]["meta"]["transport"], "recording");
    assert!(events[0]["meta"]["subject"].is_string());
    assert!(events[0]["meta"]["from_name"].is_string());
    assert_eq!(events[1]["kind"], "issued");
    assert_eq!(events[2]["kind"], "created");

    // A timeline event never changes invoice semantics.
    let (_, inv) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(inv["status"], "issued");
    assert_eq!(inv["number"], issued["number"]);
}

#[tokio::test]
async fn invoice_timeline_records_manual_create_and_draft_edit() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,
          "lines":[{"description":"Seat","item_kind":"product","quantity_hundredths":100,"unit_price_minor":4000}]})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    let iid = inv["id"].as_str().unwrap();

    let events = timeline(&app, iid).await;
    assert_eq!(events.as_array().unwrap().len(), 1, "{events}");
    assert_eq!(events[0]["kind"], "created");
    assert_eq!(events[0]["actor_name"], "Admin");

    // A draft edit appends `updated`; still newest first.
    let (se, edited) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(
            json!({"lines":[{"kind":"fixed","date":"2026-10-02","note":"Seat",
            "item_kind":"product","quantity_hundredths":200,"unit_price_minor":4000}],
            "tax_hundredths":0,"discount_hundredths":0}),
        ),
    )
    .await;
    assert_eq!(se, StatusCode::OK, "{edited}");
    let events = timeline(&app, iid).await;
    assert_eq!(events.as_array().unwrap().len(), 2, "{events}");
    assert_eq!(events[0]["kind"], "updated");
    assert!(
        events[0]["description"].as_str().unwrap().contains("lines"),
        "{events}"
    );
    assert_eq!(events[0]["actor_name"], "Admin");
    assert_eq!(events[1]["kind"], "created");

    // An invalid shape is rejected and appends nothing (rejection without
    // persistence is part of the contract).
    let (sbad, _) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{iid}"),
        Some(
            json!({"lines":[{"kind":"fixed","date":"2026-10-02","note":"X",
            "item_kind":"product","quantity_hundredths":100,"unit_price_minor":100,"evil":1}],
            "tax_hundredths":0,"discount_hundredths":0}),
        ),
    )
    .await;
    assert_eq!(sbad, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        timeline(&app, iid).await.as_array().unwrap().len(),
        2,
        "a rejected edit appends no event"
    );
}

#[tokio::test]
async fn invoice_timeline_unknown_id_and_access_control() {
    let (app, _d) = app().await;
    let unknown = uuid::Uuid::new_v4();
    let (s, body) = json_req(
        &app,
        "GET",
        &format!("/api/invoices/{unknown}/timeline"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{body}");

    let (_issued, iid) = seed_issued_invoice(&app).await;

    // Unauthenticated: rejected like the rest of the invoice surface.
    let (sa, _) = anon_req(&app, "GET", &format!("/api/invoices/{iid}/timeline"), None).await;
    assert_eq!(sa, StatusCode::UNAUTHORIZED);

    // A member is forbidden (invoices are the admin tier, #51).
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Eve","email":"eve@test.local","password":"evepass123","role":"member"}),
        ),
    )
    .await;
    let cookie = login_cookie(&app.router, "eve@test.local", "evepass123").await;
    let (sm, _, _) = raw(
        &app.router,
        "GET",
        &format!("/api/invoices/{iid}/timeline"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(sm, StatusCode::FORBIDDEN);

    // Read-only: another verb is a 405 and appends no event.
    let before = timeline(&app, &iid).await.as_array().unwrap().len();
    let (sp, _) = json_req(&app, "POST", &format!("/api/invoices/{iid}/timeline"), None).await;
    assert_eq!(sp, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        timeline(&app, &iid).await.as_array().unwrap().len(),
        before,
        "a rejected verb appends no event"
    );
}

#[tokio::test]
async fn invoice_timeline_sidecar_is_a_lenient_dot_file() {
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();

    // The sidecar exists on disk but is a dot file: never an invoice document.
    let sidecar = d
        .path()
        .join("data")
        .join("invoices")
        .join(format!(".timeline.{iid}.json"));
    assert!(sidecar.exists(), "creation wrote a timeline sidecar");
    let (_, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert_eq!(
        list["invoices"].as_array().unwrap().len(),
        1,
        "the .timeline sidecar must not appear as an invoice (dot-file rule)"
    );

    // A malformed sidecar never breaks the invoice read paths.
    std::fs::write(&sidecar, b"{not json").unwrap();
    let (s_read, _) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(s_read, StatusCode::OK);
    let (s_list, list2) = json_req(&app, "GET", "/api/invoices", None).await;
    assert_eq!(s_list, StatusCode::OK);
    assert_eq!(list2["invoices"].as_array().unwrap().len(), 1);
    assert!(
        timeline(&app, &iid).await.as_array().unwrap().is_empty(),
        "a malformed sidecar reads as an empty timeline, never an error"
    );

    // Recording again self-heals the malformed file.
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let events = timeline(&app, &iid).await;
    assert_eq!(events.as_array().unwrap().len(), 1, "{events}");
    assert_eq!(events[0]["kind"], "issued");
}

// ------------------------------- invoice sent / paid / write-off (Subtask B) --

#[tokio::test]
async fn invoice_sent_marks_the_transition_and_is_idempotent() {
    let (app, _d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;
    assert_eq!(issued["status"], "issued");

    let (s, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/sent"), None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let first = body["sent_at"].as_str().expect("sent_at").to_string();
    assert!(first.contains('T'), "an RFC 3339 timestamp: {first}");

    let events = timeline(&app, &iid).await;
    assert_eq!(events.as_array().unwrap().len(), 3, "{events}");
    assert_eq!(events[0]["kind"], "sent");
    assert_eq!(events[0]["description"], "Invoice marked as sent.");
    assert_eq!(events[0]["actor_name"], "Admin");
    // The reported `sent_at` IS the recorded event's timestamp.
    assert_eq!(events[0]["at"], first.as_str(), "{events}");
    // A manual mark-as-sent knows no recipient, so none is fabricated.
    assert!(events[0].get("meta").is_none(), "{events}");

    // Idempotent: the same earliest `sent_at`, no duplicate event appended.
    let (s2, body2) = json_req(&app, "POST", &format!("/api/invoices/{iid}/sent"), None).await;
    assert_eq!(s2, StatusCode::OK, "{body2}");
    assert_eq!(body2["sent_at"], first.as_str());
    assert_eq!(
        timeline(&app, &iid).await.as_array().unwrap().len(),
        3,
        "re-marking appends no second event"
    );

    // `sent_at` stays derived: the closed Invoice schema gains no field and the
    // invoice's own status semantics are unchanged.
    let (_, inv) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(inv["status"], "issued");
    assert!(inv.get("sent_at").is_none(), "{inv}");
}

#[tokio::test]
async fn invoice_sent_on_a_draft_is_conflict_and_records_nothing() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "SHOP", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (sc, inv) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,
          "lines":[{"description":"Seat","item_kind":"product","quantity_hundredths":100,"unit_price_minor":4000}]})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{inv}");
    let iid = inv["id"].as_str().unwrap();

    // A draft has no archived PDF and nothing has been delivered: 409.
    let (s, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/sent"), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    let events = timeline(&app, iid).await;
    assert_eq!(events.as_array().unwrap().len(), 1, "{events}");
    assert_eq!(events[0]["kind"], "created", "the refusal appended nothing");
}

#[tokio::test]
async fn invoice_sent_unknown_id_and_access_control() {
    let (app, _d) = app().await;
    let unknown = uuid::Uuid::new_v4();
    let (s, body) = json_req(&app, "POST", &format!("/api/invoices/{unknown}/sent"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{body}");

    let (_issued, iid) = seed_issued_invoice(&app).await;

    // Unauthenticated: rejected like the rest of the invoice surface.
    let (sa, _) = anon_req(&app, "POST", &format!("/api/invoices/{iid}/sent"), None).await;
    assert_eq!(sa, StatusCode::UNAUTHORIZED);

    // A member is forbidden (invoices are the admin tier, #51).
    let cookie = member_cookie(&app, "eve@test.local", "evepass123").await;
    let (sm, _, _) = raw(
        &app.router,
        "POST",
        &format!("/api/invoices/{iid}/sent"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(sm, StatusCode::FORBIDDEN);

    // POST-only: another verb is a 405 and records nothing.
    let before = timeline(&app, &iid).await.as_array().unwrap().len();
    let (sg, _) = json_req(&app, "GET", &format!("/api/invoices/{iid}/sent"), None).await;
    assert_eq!(sg, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        timeline(&app, &iid).await.as_array().unwrap().len(),
        before,
        "a rejected verb appends no event"
    );
}

#[tokio::test]
async fn invoice_email_implies_sent_with_a_single_event() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;

    // `/email` records `email_sent`, which already implies "sent".
    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    let events = timeline(&app, &iid).await;
    assert_eq!(events[0]["kind"], "email_sent", "{events}");
    let emailed_at = events[0]["at"].as_str().unwrap().to_string();

    // A manual mark afterwards is a no-op: the earliest delivery wins and no
    // second send event is recorded.
    let (ss, sent) = json_req(&app, "POST", &format!("/api/invoices/{iid}/sent"), None).await;
    assert_eq!(ss, StatusCode::OK, "{sent}");
    assert_eq!(sent["sent_at"], emailed_at.as_str());
    let events = timeline(&app, &iid).await;
    let sends = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| matches!(e["kind"].as_str(), Some("sent" | "email_sent")))
        .count();
    assert_eq!(sends, 1, "one send event, not an email_sent + sent pair");
}

#[tokio::test]
async fn invoice_pay_and_write_off_retain_the_status_change() {
    let (app, _d) = app().await;

    // Fully paid: the settlement is retained, the invoice keeps its meaning.
    let (issued, paid_id) = seed_issued_invoice(&app).await;
    let total = issued["total_minor"].as_u64().unwrap();
    let (sp, paid) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{paid_id}/pay"),
        Some(json!({"reference":"BANK-77"})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{paid}");
    assert_eq!(paid["status"], "paid");
    let events = timeline(&app, &paid_id).await;
    assert_eq!(events[0]["kind"], "paid");
    assert_eq!(events[0]["actor_name"], "Admin");
    assert_eq!(events[0]["meta"]["amount_minor"], total);
    assert_eq!(events[0]["meta"]["method"], "manual");
    assert_eq!(events[0]["meta"]["reference"], "BANK-77");
    assert_eq!(events[0]["meta"]["status"], "paid");

    // Partly paid: money received, the remainder still open.
    let c = new_customer(&app, "PARTIAL", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(4), "2026-11-02")),
    )
    .await;
    let (_, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-11-01","to":"2026-11-07"})),
    )
    .await;
    let pid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{pid}/issue"), None).await;
    let (spp, partial) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{pid}/pay"),
        Some(json!({"amount_minor":10000})),
    )
    .await;
    assert_eq!(spp, StatusCode::OK, "{partial}");
    assert_eq!(partial["status"], "partly_paid");
    let events = timeline(&app, pid).await;
    assert_eq!(events[0]["kind"], "paid");
    assert_eq!(events[0]["meta"]["amount_minor"], 10000);
    assert_eq!(events[0]["meta"]["status"], "partly_paid");

    // Written off: the forgiven balance and its reason are retained.
    let c2 = new_customer(&app, "GOODWILL", "EUR", 6000).await;
    let cid2 = c2["id"].as_str().unwrap();
    new_project(&app, cid2, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid2, "P1", json!(2), "2026-12-02")),
    )
    .await;
    let (_, inv2) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid2,"from":"2026-12-01","to":"2026-12-07"})),
    )
    .await;
    let wid = inv2["id"].as_str().unwrap();
    let (_si, iss2) = json_req(&app, "POST", &format!("/api/invoices/{wid}/issue"), None).await;
    let wtotal = iss2["total_minor"].as_u64().unwrap();
    let (sw, wo) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{wid}/write-off"),
        Some(json!({"reason":"goodwill"})),
    )
    .await;
    assert_eq!(sw, StatusCode::OK, "{wo}");
    assert_eq!(wo["status"], "written_off");
    let events = timeline(&app, wid).await;
    assert_eq!(events[0]["kind"], "write_off");
    assert_eq!(events[0]["meta"]["reason"], "goodwill");
    assert_eq!(events[0]["meta"]["balance_minor"], wtotal);

    // Recording history changes no invoice semantics, and the sidecar stays
    // invisible to the listing: one document per invoice, none gaining a
    // timeline-derived field.
    let (sl, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert_eq!(sl, StatusCode::OK);
    let invoices = list["invoices"].as_array().unwrap();
    assert_eq!(
        invoices.len(),
        3,
        "one document per invoice, no sidecar rows"
    );
    assert!(
        invoices.iter().all(|i| i.get("sent_at").is_none()),
        "sent_at stays derived, never persisted on the invoice: {list}"
    );
    let (_, stored) = json_req(&app, "GET", &format!("/api/invoices/{wid}"), None).await;
    assert_eq!(stored["status"], "written_off");
    assert_eq!(stored["write_off_reason"], "goodwill");
    let (_, stored_paid) = json_req(&app, "GET", &format!("/api/invoices/{paid_id}"), None).await;
    assert_eq!(stored_paid["status"], "paid");
}

// ---------------------------------------- invoice PDF preview (Subtask C) --

/// Whether the (uncompressed) PDF bytes carry the `(STAMP)` literal — the same
/// greppable-literal trick the org-profile test uses, since the layout emits
/// one text-showing op per word and never compresses the content stream.
fn has_stamp(bytes: &[u8], stamp: &str) -> bool {
    let needle = format!("({stamp})");
    bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[tokio::test]
async fn invoice_pdf_preview_renders_a_draft_and_never_archives() {
    let (app, d) = app().await;
    // A DRAFT with billable work: `/pdf` refuses it, the preview must not.
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    assert_eq!(inv["status"], "draft");

    let (status, headers, body) =
        raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf/preview")).await;
    assert_eq!(status, StatusCode::OK, "a draft previews");
    assert_eq!(headers[header::CONTENT_TYPE], "application/pdf");
    assert_eq!(headers[header::CONTENT_DISPOSITION], "inline");
    assert!(body.starts_with(b"%PDF"), "a real PDF");
    assert!(has_stamp(&body, "DRAFT"), "the draft stamp is present");

    // Computed, never archived: no `invoices/<id>.pdf`, no pdf hint on the doc.
    let pdf_path = d
        .path()
        .join("data")
        .join("invoices")
        .join(format!("{iid}.pdf"));
    assert!(!pdf_path.exists(), "the preview must not write an archive");
    let (_si, inv2) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert!(inv2["pdf"].is_null(), "no pdf hint attached");
    assert_eq!(inv2["status"], "draft", "the preview changes no state");

    // The deliberate asymmetry: /pdf still refuses the same draft.
    let (sd, _hd, _bd) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(sd, StatusCode::CONFLICT, "/pdf still 409s a draft");
}

#[tokio::test]
async fn invoice_pdf_preview_tracks_the_watermark_stage() {
    let (app, _d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;
    assert_eq!(issued["status"], "issued");
    let url = format!("/api/invoices/{iid}/pdf/preview");

    // The archived bytes, for the "unchanged" assertion at the end.
    let (_sa, _ha, archived_before) =
        raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;

    // Issued but never delivered: the live stage is Draft.
    let (s1, h1, p1) = raw_req(&app, "GET", &url).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(h1[header::CONTENT_TYPE], "application/pdf");
    assert!(p1.starts_with(b"%PDF"));
    assert!(has_stamp(&p1, "DRAFT"), "issued-never-sent reads DRAFT");
    assert!(!has_stamp(&p1, "SENT"));
    let etag_draft = h1[header::ETAG].to_str().unwrap().to_string();
    // Deterministic render: a second call is byte-identical with the same ETag.
    let (_s1b, h1b, p1b) = raw_req(&app, "GET", &url).await;
    assert_eq!(p1, p1b, "the preview render is deterministic");
    assert_eq!(h1b[header::ETAG], h1[header::ETAG]);

    // Mark as sent -> SENT (the stage, and so the ETag, moves).
    let (ss, sent) = json_req(&app, "POST", &format!("/api/invoices/{iid}/sent"), None).await;
    assert_eq!(ss, StatusCode::OK, "{sent}");
    let (_s2, h2, p2) = raw_req(&app, "GET", &url).await;
    assert!(has_stamp(&p2, "SENT"), "the sent stamp is present");
    assert!(!has_stamp(&p2, "DRAFT"));
    assert_ne!(
        etag_draft,
        h2[header::ETAG].to_str().unwrap(),
        "a stage change rotates the ETag"
    );

    // Paid -> PAID beats SENT.
    let (sp, paid) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference":"BANK-77"})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{paid}");
    assert_eq!(paid["status"], "paid");
    let (_s3, _h3, p3) = raw_req(&app, "GET", &url).await;
    assert!(has_stamp(&p3, "PAID"), "the paid stamp is present");

    // The archive never changed (write-once, byte-stable) and keeps its
    // issue-time Draft stamp; the listing is one document, unaffected.
    let (_s4, _h4, archived_after) =
        raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(
        archived_before, archived_after,
        "the archive stays byte-stable"
    );
    assert!(
        has_stamp(&archived_before, "DRAFT"),
        "the archive keeps its issue-time stamp"
    );
    let (_sl, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert_eq!(list["invoices"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn invoice_pdf_preview_unknown_id_and_access_control() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    let url = format!("/api/invoices/{iid}/pdf/preview");

    // Unknown id: 404.
    let (s404, _, _) = raw_req(
        &app,
        "GET",
        "/api/invoices/00000000-0000-0000-0000-000000000000/pdf/preview",
    )
    .await;
    assert_eq!(s404, StatusCode::NOT_FOUND);

    // Anonymous: 401.
    let (sa, _) = anon_req(&app, "GET", &url, None).await;
    assert_eq!(sa, StatusCode::UNAUTHORIZED);

    // Member: 403 (the whole invoice surface is admin tier, #51).
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Eve","email":"eve@test.local","password":"evepass123","role":"member"}),
        ),
    )
    .await;
    let cookie = login_cookie(&app.router, "eve@test.local", "evepass123").await;
    let (sm, _, _) = raw(&app.router, "GET", &url, None, Some(&cookie)).await;
    assert_eq!(sm, StatusCode::FORBIDDEN);

    // GET-only: another verb is a 405.
    let (st, _) = json_req(&app, "POST", &url, None).await;
    assert_eq!(st, StatusCode::METHOD_NOT_ALLOWED);
}

/// #350: invoicing discovery reads through the same merge, so an archived but
/// unbilled period still previews and still issues with its lines — and once
/// issued, the same period previews empty rather than re-billing the cold day.
#[tokio::test]
async fn an_archived_unbilled_period_still_previews_and_issues() {
    use tucano_time::lock::NoLocks;
    use tucano_time::store::Store;

    let day = "2026-08-03";
    let cutoff = chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();

    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({"rate_minor": 6000})).await;
    let (_, entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(&cid, "P1", json!(3), day)),
    )
    .await;
    let eid = entry["id"].as_str().unwrap().to_string();

    // The period is unbilled, so #349 archives it: the cold copy is what the
    // builder below has to discover.
    let store = Store::open(d.path().join("data")).expect("store");
    let report = store
        .archive_entries_before(cutoff, false, &NoLocks)
        .expect("the pass runs");
    assert_eq!(report.days.len(), 1, "{:?}", report.days);

    // Preview: the archived day is one line of the draft, priced at the resolved
    // rate — the relocation changed neither the discovery nor the number.
    let (s_draft, draft) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id": cid, "from": day, "to": day})),
    )
    .await;
    assert_eq!(s_draft, StatusCode::CREATED, "{draft}");
    let lines = draft["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 1, "the archived day bills once: {draft}");
    assert_eq!(lines[0]["entry_id"], eid.as_str(), "{draft}");
    assert_eq!(draft["total_minor"], 18000, "3h at 60.00: {draft}");

    // Issue: the archived entry is the invoice's line, and the invoice is now
    // evidence for an issued number (the permanent #248 exclusion).
    let iid = draft["id"].as_str().unwrap();
    let (s_issue, issued) =
        json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(s_issue, StatusCode::OK, "{issued}");
    assert_eq!(issued["status"], "issued");

    // No duplicate re-preview: the same period now has nothing billable, in either
    // tree, and the empty draft is refused instead of being persisted.
    let (s_again, again) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id": cid, "from": day, "to": day})),
    )
    .await;
    assert_eq!(s_again, StatusCode::CONFLICT, "{again}");
    assert_eq!(again["error"]["code"], "conflict");

    // Negative: a period that was never billed is refused the same way, so the
    // refusal above is about the billing state and not about the archive.
    let (s_empty, body) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id": cid, "from": "2026-08-04", "to": "2026-08-04"})),
    )
    .await;
    assert_eq!(s_empty, StatusCode::CONFLICT, "{body}");
}

// ------------------------------------------------- QA-02: issue vs timesheet --

/// QA-02: a draft built from 7.5 h whose entry was then edited to 7 h (drafts
/// lock nothing) must not issue. The refusal is 409 `draft_out_of_date`, no
/// invoice is issued and no entry is locked; regenerating the draft bills the
/// current 7 h.
#[tokio::test]
async fn issuing_refuses_a_draft_the_timesheet_moved_past() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    let (s_e, e) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-01","customer_id":cid,"project_code":"P1","hours":7.5})),
    )
    .await;
    assert_eq!(s_e, StatusCode::CREATED, "{e}");
    let eid = e["id"].as_str().unwrap();

    let (s, draft) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{draft}");
    let iid = draft["id"].as_str().unwrap();
    assert_eq!(draft["lines"][0]["hours"], 7.5);
    assert_eq!(draft["lines"][0]["amount_minor"], 45000);
    assert_eq!(draft["total_minor"], 45000);

    // A draft locks nothing: the entry shrinks to 7 h.
    let (s_edit, b_edit) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-01","customer_id":cid,"project_code":"P1","hours":7})),
    )
    .await;
    assert_eq!(s_edit, StatusCode::OK, "{b_edit}");

    // Issue is refused with the specific code, and persists nothing.
    let (s_issue, b_issue) =
        json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(s_issue, StatusCode::CONFLICT, "{b_issue}");
    assert_eq!(b_issue["error"]["code"], "draft_out_of_date", "{b_issue}");
    assert!(
        b_issue["error"]["message"]
            .as_str()
            .unwrap()
            .contains("regenerate"),
        "{b_issue}"
    );
    let (_, still) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(still["status"], "draft", "nothing was issued: {still}");
    assert!(still["issued_at"].is_null(), "{still}");
    assert!(still["pdf"].is_null(), "no PDF was archived: {still}");

    // The entry was never locked by the refused issue.
    let (s_unlocked, b_unlocked) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(json!({"date":"2026-10-01","customer_id":cid,"project_code":"P1","hours":7,"note":"still editable"})),
    )
    .await;
    assert_eq!(s_unlocked, StatusCode::OK, "{b_unlocked}");

    // Regenerate (delete + rebuild) and issue: the line bills the current 7 h.
    let (s_drop, _) = json_req(&app, "DELETE", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(s_drop, StatusCode::NO_CONTENT);
    let (s_new, fresh) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s_new, StatusCode::CREATED, "{fresh}");
    let fresh_id = fresh["id"].as_str().unwrap();
    let (s_final, issued) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{fresh_id}/issue"),
        None,
    )
    .await;
    assert_eq!(s_final, StatusCode::OK, "{issued}");
    assert_eq!(issued["status"], "issued");
    assert_eq!(issued["lines"][0]["hours"], 7.0);
    assert_eq!(issued["lines"][0]["amount_minor"], 42000, "7 h x 60.00");
    assert_eq!(issued["total_minor"], 42000);
}

/// QA-02 (no regression): an unchanged draft still issues with the same
/// totals, and a second invoice over the same period is still refused.
#[tokio::test]
async fn an_unchanged_draft_still_issues_and_a_second_invoice_is_refused() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (s, draft) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{draft}");
    let iid = draft["id"].as_str().unwrap();
    let (s_issue, issued) =
        json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(s_issue, StatusCode::OK, "{issued}");
    assert_eq!(issued["status"], "issued");
    assert_eq!(issued["total_minor"], draft["total_minor"]);
    assert_eq!(issued["total_minor"], 18000);
    assert_eq!(issued["lines"], draft["lines"], "the snapshot is untouched");
    // Nothing is left to bill for that period.
    let (s_second, body) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(s_second, StatusCode::CONFLICT, "{body}");
}
