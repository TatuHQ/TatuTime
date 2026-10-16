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

    // Issue -> now locked.
    let (si, _) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(si, StatusCode::OK);
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

    // The MIME builder labels the part as a PDF.
    let mime = tucano_time::email::build_mime(&msgs[0]);
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
