use crate::support::*;

use axum::http::StatusCode;
use axum::http::header;
use serde_json::Value;
use serde_json::json;

/// Decode the WinAnsi bytes of a PDF string back to text (the euro sign and
/// dashes are single high bytes, not UTF-8).
fn winansi_text(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| match b {
            0x80 => '\u{20ac}',
            0x96 => '\u{2013}',
            0x97 => '\u{2014}',
            b if b < 0x20 => ' ',
            b => b as char,
        })
        .collect()
}

/// Decode the PDF's text-showing strings back into running text. pdf-writer
/// emits a literal `(...)` string while the bytes are ASCII and a hex `<...>`
/// string once they are not (the euro sign), so both forms are decoded. The
/// layout positions every word itself, so chunks are joined with spaces —
/// assertions target single words or the joined form.
///
/// The scan is limited to **page content streams**: the invoice now embeds the
/// Ubuntu TrueType fonts, and a raw pass over the file would decode glyph data
/// as text.
fn pdf_joined(pdf: &[u8]) -> String {
    let document = lopdf::Document::load_mem(pdf).expect("invoice PDF re-parses");
    let mut chunks = Vec::new();
    for page in document.get_pages().into_values() {
        let content = document.get_page_content(page);
        decode_pdf_strings(&content, &mut chunks);
    }
    chunks.join(" ")
}

fn decode_pdf_strings(bytes: &[u8], chunks: &mut Vec<String>) {
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => {
                let mut text = Vec::new();
                i += 1;
                while i < bytes.len() && bytes[i] != b')' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 1;
                        text.push(match bytes[i] {
                            b'n' => b'\n',
                            b'r' => b'\r',
                            b't' => b'\t',
                            other => other,
                        });
                    } else {
                        text.push(bytes[i]);
                    }
                    i += 1;
                }
                chunks.push(winansi_text(&text));
                i += 1;
            }
            // `<<` opens a dictionary, not a hex string: skip it untouched.
            b'<' if bytes.get(i + 1) != Some(&b'<') => {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end] != b'>' {
                    end += 1;
                }
                let digits: Vec<u8> = bytes[start..end]
                    .iter()
                    .copied()
                    .filter(u8::is_ascii_hexdigit)
                    .collect();
                let text: Vec<u8> = digits
                    .chunks(2)
                    .filter(|pair| pair.len() == 2)
                    .map(|pair| {
                        let text = std::str::from_utf8(pair).unwrap_or("00");
                        u8::from_str_radix(text, 16).unwrap_or(b'?')
                    })
                    .collect();
                chunks.push(winansi_text(&text));
                i = end + 1;
            }
            _ => i += 1,
        }
    }
}

// --------------------------------------------------------------- invoices --

#[tokio::test]
async fn invoice_sums_billable_and_ignores_nonbillable() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
        .attachments
        .first()
        .expect("the invoice email carries the PDF (#113)");
    assert_eq!(*name, format!("{}.pdf", issued["number"].as_str().unwrap()));
    assert_eq!(*bytes, pdf, "the attachment is the archived document");
    assert!(
        !has_stamp(bytes, "DRAFT"),
        "customer delivery must not be stamped draft"
    );
    assert!(
        msgs[0].text.contains("PDF document is attached"),
        "{}",
        msgs[0].text
    );

    // The MIME builder labels the part as a PDF. `build_mime` now returns a
    // Result -- it refuses a header-bound control character at the sink (#S1)
    // -- so the same two assertions run on the unwrapped body.
    let mime = tatu_time::email::build_mime(&msgs[0]).expect("the message builds");
    assert!(mime.contains("Content-Type: application/pdf"));
    assert!(mime.contains("base64"));
}

#[tokio::test]
async fn invoice_email_can_attach_expense_receipts_and_records_them_in_history() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Receipt Co", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (se, expense) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":500,"currency":"EUR",
            "receipt_name":"hotel.png","receipt_b64":"aGVsbG8="
        })),
    )
    .await;
    assert_eq!(se, StatusCode::CREATED, "{expense}");
    let (si, invoice) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-31"})),
    )
    .await;
    assert_eq!(si, StatusCode::CREATED, "{invoice}");
    let iid = invoice["id"].as_str().unwrap().to_string();
    let (s_issue, issued) =
        json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(s_issue, StatusCode::OK, "{issued}");

    // The dialog's summary endpoint names the receipt, never its bytes.
    let (sr, receipts) =
        json_req(&app, "GET", &format!("/api/invoices/{iid}/receipts"), None).await;
    assert_eq!(sr, StatusCode::OK, "{receipts}");
    assert_eq!(receipts["receipts"].as_array().unwrap().len(), 1);
    assert_eq!(receipts["receipts"][0]["name"], "hotel.png");
    assert!(
        receipts["receipts"][0].get("receipt_b64").is_none(),
        "the summary must not ship the file"
    );

    // Opting in attaches both the invoice PDF and the receipt.
    let (semail, sent) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email"),
        Some(json!({"recipients":["billing@receipt.test"],"include_receipts":true})),
    )
    .await;
    assert_eq!(semail, StatusCode::OK, "{sent}");
    assert_eq!(sent["receipt_count"], 1);
    let messages = app.email.messages();
    let msg = messages.last().expect("the email was recorded");
    assert_eq!(msg.attachments.len(), 2, "invoice PDF + one receipt");
    let (_, receipt_bytes) = msg
        .attachments
        .iter()
        .find(|(name, _)| name == "hotel.png")
        .expect("the receipt rides along");
    assert_eq!(receipt_bytes, b"hello");
    assert!(
        msg.text.contains("1 expense receipt attached"),
        "{}",
        msg.text
    );
    let mime = tatu_time::email::build_mime(msg).expect("multi-attachment MIME builds");
    assert!(mime.contains("Content-Type: image/png"), "{mime}");
    assert!(mime.contains("filename=\"hotel.png\""), "{mime}");
    assert!(mime.contains("Content-Type: application/pdf"), "{mime}");

    // The delivery is history: the timeline names the receipt, so the admin can
    // prove later what left the building.
    let (st, timeline) =
        json_req(&app, "GET", &format!("/api/invoices/{iid}/timeline"), None).await;
    assert_eq!(st, StatusCode::OK, "{timeline}");
    let event = timeline["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "email_sent")
        .expect("email_sent history entry");
    assert!(
        event["description"]
            .as_str()
            .unwrap_or_default()
            .contains("with 1 expense receipt"),
        "{event}"
    );
    assert_eq!(event["meta"]["receipts"][0], "hotel.png");
    assert_eq!(event["meta"]["receipt_count"], 1);

    // Without the option the historical PDF-only email is unchanged.
    let (splain, plain) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email"),
        Some(json!({"recipients":["billing@receipt.test"]})),
    )
    .await;
    assert_eq!(splain, StatusCode::OK, "{plain}");
    assert_eq!(plain["receipt_count"], 0);
    assert_eq!(
        app.email.messages().last().unwrap().attachments.len(),
        1,
        "receipts are opt-in"
    );
}

#[tokio::test]
async fn legacy_draft_stamp_is_removed_only_from_the_delivery_copy() {
    let (app, data) = app().await;
    let (_, iid) = seed_issued_invoice(&app).await;
    let (_, _, original) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let mut legacy = lopdf::Document::load_mem(&original).unwrap();
    for page in legacy.get_pages().into_values() {
        let mut stamp = lopdf::content::Content::decode(b"0.75 g\nBT\n/F2 60 Tf\n0.70710677 0.70710677 -0.70710677 0.70710677 100 300 Tm\n(DRAFT) Tj\nET\n0 g\n").unwrap();
        stamp.operations.extend(
            lopdf::content::Content::decode(&legacy.get_page_content(page))
                .unwrap()
                .operations,
        );
        legacy
            .change_page_content(page, stamp.encode().unwrap())
            .unwrap();
    }
    let mut archived = Vec::new();
    legacy.save_to(&mut archived).unwrap();
    let path = data
        .path()
        .join("data")
        .join("invoices")
        .join(format!("{iid}.pdf"));
    std::fs::write(&path, &archived).unwrap();
    assert!(has_stamp(&archived, "DRAFT"));
    let (status, response) =
        json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let messages = app.email.messages();
    let (_, attachment) = messages[0].attachments.first().unwrap();
    assert!(
        !has_stamp(attachment, "DRAFT"),
        "legacy DRAFT stamp must not reach the customer"
    );
    assert_eq!(
        std::fs::read(path).unwrap(),
        archived,
        "issued archives must never be rewritten"
    );
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

    // The accountant address is validated like an email-copy recipient.
    let (sa, ab) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"X","accountant_email":"not-an-address"})),
    )
    .await;
    assert_eq!(sa, StatusCode::UNPROCESSABLE_ENTITY, "{ab}");
    assert_eq!(ab["error"]["fields"][0]["field"], "accountant_email");
    let (_sa2, before2) = json_req(&app, "GET", "/api/admin/org", None).await;
    assert_eq!(
        before2["name"], "",
        "an invalid accountant address persists nothing"
    );

    // Valid save round-trips.
    let (sp, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"Tatu BV","legal_id":"BE0123456789",
          "accountant_email":"books@tatu.test",
          "address":{"street":"Markt 1","city":"Brugge","postal_code":"8000","country":"BE"}})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{saved}");
    assert_eq!(saved["name"], "Tatu BV");
    assert_eq!(saved["accountant_email"], "books@tatu.test");

    // The identity prints on the issued document (uncompressed streams, so
    // the bytes are greppable — the determinism contract of #113 in use).
    let (issued, iid) = seed_issued_invoice(&app).await;
    let (_s2, _h2, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let joined = pdf_joined(&pdf);
    // Phase 2: the identity lives in the issuer block — one paragraph per
    // address line and the legal id under its resolved label.
    assert!(
        joined.contains("Tatu BV"),
        "profile name replaces the config default"
    );
    assert!(
        joined.contains("Tax ID:"),
        "empty legal_id_label prints the default Tax ID wording"
    );
    assert!(joined.contains("BE0123456789"), "legal id on the PDF");
    assert!(joined.contains("Markt 1,"), "address line on the PDF");
    assert!(joined.contains("8000,"), "one paragraph per address line");
    assert!(joined.contains("Brugge,"), "address line on the PDF");
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

/// A correction revision keeps the original document and PDF as history: the
/// revision gets its own id and PDF, the original is marked `superseded_by`,
/// and active aggregates count only the revision.
#[tokio::test]
async fn correction_revisions_keep_the_original_and_supersede_it() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Corrections", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (s, original) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,
          "lines":[{"description":"Work","item_kind":"service","quantity_hundredths":100,"unit_price_minor":10000}]})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{original}");
    let original_id = original["id"].as_str().unwrap().to_string();
    let number = original["number"].as_str().unwrap().to_string();
    let (s, issued) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{original_id}/issue"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{issued}");

    let revision_body = json!({
        "lines":[{"kind":"fixed","date":"2026-01-01","note":"Corrected work",
                  "item_kind":"service","quantity_hundredths":100,
                  "unit_price_minor":12000,"amount_minor":12000}],
        "status":"issued"
    });
    let (s, revision) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{original_id}/revise"),
        Some(revision_body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{revision}");
    let revision_id = revision["id"].as_str().unwrap().to_string();
    assert_ne!(revision_id, original_id, "the revision has its own id");
    assert_eq!(revision["number"], number.as_str());
    assert_eq!(revision["supersedes"], original_id.as_str());
    assert_eq!(revision["status"], "issued");
    assert_eq!(revision["total_minor"], 12000);
    // The frozen subject is interpolated exactly as issuing resolves it: a raw
    // template placeholder must never be baked onto a revision (or its PDF).
    let subject = revision["subject"].as_str().unwrap_or_default();
    assert!(
        !subject.contains('%'),
        "the revision subject is resolved, got {subject:?}"
    );
    assert!(subject.contains("Corrections"), "{subject:?}");

    let (s, stored_original) =
        json_req(&app, "GET", &format!("/api/invoices/{original_id}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(stored_original["superseded_by"], revision_id.as_str());
    assert_eq!(stored_original["total_minor"], 10000, "original untouched");
    // Both documents remain downloadable.
    for id in [&original_id, &revision_id] {
        let (s, _, _) = raw_req(&app, "GET", &format!("/api/invoices/{id}/pdf")).await;
        assert_eq!(s, StatusCode::OK, "PDF for {id}");
    }
    // The original cannot be revised twice.
    let (s, again) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{original_id}/revise"),
        Some(revision_body),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{again}");

    // Active aggregates count only the revision.
    let (s, summary) = json_req(&app, "GET", "/api/invoices/summary", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(summary["outstanding"]["EUR"], 12000);
    assert_eq!(summary["issued"], 1);
    let (_, all) = json_req(&app, "GET", "/api/invoices", None).await;
    assert_eq!(
        all["invoices"].as_array().unwrap().len(),
        2,
        "history keeps both"
    );

    // Every invoice is deletable: removing the superseded original leaves the
    // revision active and drops its now-dangling `supersedes` link.
    let (s, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/invoices/{original_id}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, revision_after) =
        json_req(&app, "GET", &format!("/api/invoices/{revision_id}"), None).await;
    assert!(
        revision_after.get("supersedes").is_none(),
        "{revision_after}"
    );
}

/// A revision can be returned to draft, edited in place, and issued again.
#[tokio::test]
async fn a_sent_invoice_can_return_to_draft_through_a_revision() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Reopen", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (_, original) = json_req(
        &app,
        "POST",
        "/api/invoices/manual",
        Some(json!({"customer_id":cid,
          "lines":[{"description":"Work","item_kind":"service","quantity_hundredths":100,"unit_price_minor":5000}]})),
    )
    .await;
    let original_id = original["id"].as_str().unwrap().to_string();
    let (s, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{original_id}/issue"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, draft) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{original_id}/revise"),
        Some(json!({
            "lines":[{"kind":"fixed","date":"2026-01-01","note":"To correct",
                      "item_kind":"service","quantity_hundredths":100,
                      "unit_price_minor":5000,"amount_minor":5000}],
            "status":"draft"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{draft}");
    assert_eq!(draft["status"], "draft");
    let revision_id = draft["id"].as_str().unwrap();
    let (s, _, _) = raw_req(&app, "GET", &format!("/api/invoices/{revision_id}/pdf")).await;
    assert_eq!(s, StatusCode::CONFLICT, "a draft has no archived PDF");
    // The draft revision can be edited and issued again.
    let (s, edited) = json_req(
        &app,
        "PUT",
        &format!("/api/invoices/{revision_id}"),
        Some(json!({
            "lines":[{"kind":"fixed","date":"2026-01-01","note":"Corrected",
                      "item_kind":"service","quantity_hundredths":100,
                      "unit_price_minor":6000,"amount_minor":6000}],
            "status":"draft"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{edited}");
    assert_eq!(edited["total_minor"], 6000);
    let (s, reissued) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{revision_id}/issue"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{reissued}");
    assert_eq!(reissued["status"], "issued");
    assert_eq!(reissued["number"], original["number"]);
}

#[tokio::test]
async fn invoice_document_overrides_round_trip_and_reject_without_persistence() {
    let (app, _d) = app().await;
    let customer = new_customer(&app, "Document overrides", "EUR", 6000).await;
    let input = json!({"customer_id":customer["id"],
        "document":{"po_number":"PO-42","invoice_date":"2026-09-30","due_date":"2026-10-30",
                    "subject":"Consulting September","notes":"Payment instructions","payment_terms":{"kind":"net_30"}},
        "lines":[{"description":"Consulting","item_kind":"service","quantity_hundredths":250,"unit_price_minor":6000}]});
    let (status, invoice) = json_req(&app, "POST", "/api/invoices/manual", Some(input)).await;
    assert_eq!(status, StatusCode::CREATED, "{invoice}");
    assert_eq!(invoice["document"]["po_number"], "PO-42");
    let path = format!("/api/invoices/{}", invoice["id"].as_str().unwrap());
    for document in [
        json!({"unknown":true}),
        json!({"notes":"x".repeat(4001)}),
        json!({"invoice_date":"2026-10-30","due_date":"2026-09-30"}),
        json!({"invoice_date":"not-a-date"}),
        json!({"subject":42}),
        json!({"subject":"%not_a_document_variable%"}),
        json!({"subject":"%payment_details%"}),
        json!({"subject":"first\nsecond"}),
        json!({"payment_terms":{"kind":"custom","days":9999}}),
    ] {
        let (status, _) = json_req(
            &app,
            "PUT",
            &path,
            Some(json!({"lines":invoice["lines"],"document":document})),
        )
        .await;
        assert!(status.is_client_error());
        let (_, stored) = json_req(&app, "GET", &path, None).await;
        assert_eq!(stored, invoice, "invalid metadata persisted");
    }
    let (status, old_client) =
        json_req(&app, "PUT", &path, Some(json!({"lines":invoice["lines"]}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        old_client["document"], invoice["document"],
        "legacy payload clears overrides"
    );
    let (status, issued) = json_req(&app, "POST", &format!("{path}/issue"), None).await;
    assert_eq!(status, StatusCode::OK, "{issued}");
    assert_eq!(issued["subject"], "Consulting September");
    assert_eq!(issued["due_date"], "2026-10-30");
    assert!(
        issued["terms"].is_null(),
        "explicit date must not claim conflicting net terms"
    );
    let (status, _) = json_req(
        &app,
        "PUT",
        &path,
        Some(json!({"lines":invoice["lines"],"document":{"notes":"mutated"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
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
    let joined = pdf_joined(&pdf);
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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

// ------------------------------------------------------------------ #new ---

/// The creation wizard's grouping choice is a document-presentation setting:
/// the stateless PDF preview and the saved draft both honour it, while the
/// stored lines keep one entry-referencing row per tracked record.
#[tokio::test]
async fn preview_pdf_and_creation_apply_line_grouping() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Grouping Co", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "PA", json!({"rate_minor": 5000})).await;
    new_project(&app, &cid, "PB", json!({"rate_minor": 7000})).await;
    for (proj, day, hours) in [("PA", "2026-10-02", 2), ("PB", "2026-10-03", 1)] {
        json_req(
            &app,
            "POST",
            "/api/entries",
            Some(json!({"date":day,"customer_id":cid.clone(),"project_code":proj,"hours":hours})),
        )
        .await;
    }
    let body = |grouping: &str| {
        json!({
            "customer_id": cid.clone(),
            "from": "2026-10-01",
            "to": "2026-10-31",
            "line_grouping": grouping
        })
    };

    // Single line: one "Time entries" row, no project codes in the table.
    let (single_status, single_headers, single_pdf) =
        raw_json_req(&app, "POST", "/api/invoices/preview/pdf", body("single")).await;
    assert_eq!(single_status, StatusCode::OK);
    assert_eq!(
        single_headers[header::CONTENT_TYPE],
        "application/pdf",
        "the wizard preview is a PDF body"
    );
    assert!(single_pdf.starts_with(b"%PDF-"));
    let single_text = pdf_joined(&single_pdf);
    assert!(single_text.contains("Time entries"), "{single_text}");
    assert!(
        !single_text.contains("PA") && !single_text.contains("PB"),
        "a single row must not name the projects: {single_text}"
    );

    // By project: both project codes appear as their own rows.
    let (project_status, _, project_pdf) =
        raw_json_req(&app, "POST", "/api/invoices/preview/pdf", body("project")).await;
    assert_eq!(project_status, StatusCode::OK);
    let project_text = pdf_joined(&project_pdf);
    assert!(project_text.contains("PA"), "{project_text}");
    assert!(project_text.contains("PB"), "{project_text}");
    // A grouped row keeps the project rate as its unit price.
    assert!(
        project_text.contains("EUR \u{20ac}50.00"),
        "PA's 50.00 unit price: {project_text}"
    );
    assert!(
        project_text.contains("EUR \u{20ac}70.00"),
        "PB's 70.00 unit price: {project_text}"
    );

    // Nothing was persisted by either render.
    let (_ls, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert!(list["invoices"].as_array().unwrap().is_empty());

    // Saving keeps the per-entry lines (rate snapshot/lock semantics) but
    // freezes the chosen presentation on the document.
    let (created_status, created) =
        json_req(&app, "POST", "/api/invoices", Some(body("single"))).await;
    assert_eq!(created_status, StatusCode::CREATED, "{created}");
    assert_eq!(created["document"]["line_grouping"], "single");
    assert_eq!(created["lines"].as_array().unwrap().len(), 2);
    let iid = created["id"].as_str().unwrap();
    let (preview_status, _, preview_pdf) =
        raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf/preview")).await;
    assert_eq!(preview_status, StatusCode::OK);
    assert!(pdf_joined(&preview_pdf).contains("Time entries"));

    // A period with no billable work answers the same 409 as the real call,
    // and still persists nothing.
    let (empty_status, _, empty_body) = raw_json_req(
        &app,
        "POST",
        "/api/invoices/preview/pdf",
        json!({"customer_id":cid.clone(),"from":"2030-01-01","to":"2030-01-31"}),
    )
    .await;
    assert_eq!(empty_status, StatusCode::CONFLICT);
    let empty: Value = serde_json::from_slice(&empty_body).expect("error shape");
    assert_eq!(empty["error"]["code"], "conflict");
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
    let joined = pdf_joined(&pdf);
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
        Some(json!({"name":"Tatu BV","from_name":"Tatu Invoicing",
                    "reply_to":"billing@tatu.test","accent":"#E95420"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    assert_eq!(saved["from_name"], "Tatu Invoicing");

    // The mail adapter honors the sender identity (#146): the recorded
    // message carries it and build_mime would render From/Reply-To.
    let (issued, iid) = seed_issued_invoice(&app).await;
    let _ = issued;
    let (se, _b) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK);
    let msg = &app.email.messages()[0];
    assert_eq!(msg.from_name.as_deref(), Some("Tatu Invoicing"));
    assert_eq!(msg.reply_to.as_deref(), Some("billing@tatu.test"));

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
        Some(json!({"name":"Tatu BV","from_name":"","reply_to":"","accent":""})),
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
    assert_eq!(events[0]["meta"]["to"], "billing@tatutime.test");
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
    let Ok(document) = lopdf::Document::load_mem(bytes) else {
        return false;
    };
    document.get_pages().into_values().any(|page| {
        lopdf::content::Content::decode(&document.get_page_content(page)).is_ok_and(|content| {
            content.operations.iter().any(|operation| {
                operation.operator == "Tj"
                    && operation.operands.first().is_some_and(|operand| {
                        operand.as_str().is_ok_and(|text| text == stamp.as_bytes())
                    })
            })
        })
    })
}

#[tokio::test]
async fn invoice_pdf_preview_renders_a_draft_and_never_archives() {
    let (app, d) = app().await;
    // A DRAFT with billable work: `/pdf` refuses it, the preview must not.
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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

    // The unstamped archive never changed; preview stages are display-only.
    let (_s4, _h4, archived_after) =
        raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(
        archived_before, archived_after,
        "the archive stays byte-stable"
    );
    assert!(
        !has_stamp(&archived_before, "DRAFT"),
        "an issued archive must not be stamped DRAFT"
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
    use tatu_time::lock::NoLocks;
    use tatu_time::store::Store;

    let day = "2026-08-03";
    let cutoff = chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();

    let (app, d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
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

// ------------------------------------------- document defaults (phase 1) ---

#[tokio::test]
async fn customer_legal_id_round_trips_and_oversize_is_refused() {
    let (app, _d) = app().await;
    let (s, created) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({
            "name": "TatuTime",
            "currency": "EUR",
            "default_rate_minor": 6000,
            "legal_id": "  99999999  "
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    assert_eq!(created["legal_id"], "99999999");
    let cid = created["id"].as_str().unwrap();
    let (sg, got) = json_req(&app, "GET", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(sg, StatusCode::OK);
    assert_eq!(got["legal_id"], "99999999");

    // Over-long: 422 naming the field, nothing persisted.
    let (sb, body) = json_req(
        &app,
        "PUT",
        &format!("/api/customers/{cid}"),
        Some(json!({
            "name": "TatuTime",
            "currency": "EUR",
            "default_rate_minor": 6000,
            "legal_id": "1".repeat(61)
        })),
    )
    .await;
    assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["fields"][0]["field"], "legal_id");
    let (_, after) = json_req(&app, "GET", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(after["legal_id"], "99999999", "rejection persisted nothing");
}

#[tokio::test]
async fn org_profile_round_trips_legal_id_label() {
    let (app, _d) = app().await;
    let (s, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({
            "name": "TatuTime",
            "legal_id": "00.000.000/0000-00",
            "legal_id_label": "Tax ID"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    assert_eq!(saved["legal_id_label"], "Tax ID");
    let (_, got) = json_req(&app, "GET", "/api/admin/org", None).await;
    assert_eq!(got["legal_id_label"], "Tax ID");
    assert_eq!(got["legal_id"], "00.000.000/0000-00");

    // Over-long label: 422 and the stored profile is untouched.
    let (sb, body) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({
            "name": "TatuTime",
            "legal_id": "00.000.000/0000-00",
            "legal_id_label": "x".repeat(41)
        })),
    )
    .await;
    assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["fields"][0]["field"], "legal_id_label");
    let (_, after) = json_req(&app, "GET", "/api/admin/org", None).await;
    assert_eq!(after["legal_id_label"], "Tax ID");
}

#[tokio::test]
async fn payment_rails_round_trip_through_the_customer_surface() {
    let (app, _d) = app().await;
    let rails = json!([
        {
            "currency": "EUR",
            "account_name": "TatuTime",
            "iban": "EX000000000000000000",
            "bic": "EXAMPLEB"
        },
        {
            "currency": "USD",
            "account_name": "TatuTime",
            "routing_number": "000000000",
            "swift": "EXAMPLEB"
        }
    ]);
    let (s, created) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({
            "name": "Multi Rail",
            "currency": "EUR",
            "default_rate_minor": 6000,
            "payment_details": { "rails": rails }
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let cid = created["id"].as_str().unwrap();
    let stored = &created["payment_details"]["rails"];
    assert_eq!(stored.as_array().unwrap().len(), 2, "{created}");
    assert_eq!(stored[0]["currency"], "EUR");
    assert_eq!(stored[0]["iban"], "EX000000000000000000");
    assert_eq!(stored[1]["currency"], "USD");
    assert_eq!(stored[1]["routing_number"], "000000000");

    let (_, got) = json_req(&app, "GET", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(got["payment_details"]["rails"], *stored);

    // An empty currency is refused, and nothing is persisted.
    let (sb, body) = json_req(
        &app,
        "PUT",
        &format!("/api/customers/{cid}"),
        Some(json!({
            "name": "Multi Rail",
            "currency": "EUR",
            "default_rate_minor": 6000,
            "payment_details": { "rails": [ { "currency": "", "account_name": "X" } ] }
        })),
    )
    .await;
    assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["field"] == "payment_details.rails[0].currency"),
        "{body}"
    );
    let (_, after) = json_req(&app, "GET", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(after["payment_details"]["rails"], *stored);

    // `account_name` is required on a rail (the key is in the schema's
    // `required` list, and a rail without an account holder cannot be paid
    // into): a blank one is a 422 with the API's field path, and nothing is
    // persisted.
    let (sn, body) = json_req(
        &app,
        "PUT",
        &format!("/api/customers/{cid}"),
        Some(json!({
            "name": "Multi Rail",
            "currency": "EUR",
            "default_rate_minor": 6000,
            "payment_details": {
                "rails": [ { "currency": "EUR", "account_name": "   " } ]
            }
        })),
    )
    .await;
    assert_eq!(sn, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    assert!(
        body["error"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["field"] == "payment_details.rails[0].account_name"),
        "{body}"
    );
    let (_, after) = json_req(&app, "GET", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(
        after["payment_details"]["rails"], *stored,
        "a refused rail writes nothing"
    );
}

#[tokio::test]
async fn invoice_template_get_returns_the_seeded_defaults() {
    let (app, _d) = app().await;
    let (s, body) = json_req(&app, "GET", "/api/admin/invoice-template", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["template"]["payment_terms"]["kind"], "net_30");
    assert_eq!(
        body["template"]["subject"],
        "%customer_name% invoice for %period_month_padded% %period_year%"
    );
    let vars = body["variables"].as_array().unwrap();
    assert!(vars.iter().any(|v| v["name"] == "%period_month%"));
    assert!(vars.iter().any(|v| v["name"] == "%period_month_number%"));
    assert!(vars.iter().any(|v| v["name"] == "%period_month_padded%"));
    assert!(vars.iter().any(|v| v["name"] == "%period_year%"));
}

#[tokio::test]
async fn issuing_freezes_subject_and_terms_in_the_invoice_document() {
    let (app, _d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;
    assert_eq!(issued["subject"], "TatuTime invoice for 10 2026");
    assert_eq!(issued["terms"]["kind"], "net_30");
    // A later template edit cannot rewrite the issued snapshot, while the live
    // document endpoint reads the snapshot instead of the new template.
    let (st, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({
            "subject": "CHANGED %customer_name%",
            "body": "",
            "footer": "",
            "payment_terms": {"kind": "net_45"}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, after) = json_req(&app, "GET", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(after["subject"], "TatuTime invoice for 10 2026");
    assert_eq!(after["terms"]["kind"], "net_30");
    let (sd, doc) = json_req(&app, "GET", &format!("/api/invoices/{iid}/document"), None).await;
    assert_eq!(sd, StatusCode::OK);
    assert_eq!(doc["subject"], "TatuTime invoice for 10 2026");
}

// ------------------------------------- document rendering (phase 2) ---

/// The reference document: title + subject, the issuer and customer blocks,
/// `dd/mm/yyyy` dates with the frozen `(Net 30)` suffix, the derived total
/// hours, the item-type column and symbol-first money.
#[tokio::test]
async fn invoice_pdf_prints_the_document_sections_and_terms_suffix() {
    let (app, _d) = app().await;
    let (sc, c) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({
            "name": "TatuTime",
            "currency": "EUR",
            "default_rate_minor": 6000,
            "email": "billing@tatutime.example",
            "address": {
                "street": "9 Placeholder Road",
                "city": "Testville",
                "postal_code": "AA1 1AA",
                "country": "Freedonia"
            },
            "legal_id": "99999999"
        })),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{c}");
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-08-03","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-08-01","to":"2026-08-31"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    let (si, issued) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(si, StatusCode::OK, "{issued}");
    assert_eq!(issued["terms"]["kind"], "net_30");
    assert_eq!(issued["subject"], "TatuTime invoice for 08 2026");

    let (_sp, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let joined = pdf_joined(&pdf);

    // Subject row (the document name is the plain title).
    assert!(joined.contains("Subject:"), "subject label: {joined}");
    assert!(
        joined.contains("TatuTime invoice for 08 2026"),
        "subject text: {joined}"
    );
    // Invoice-for block: heading, name, one line per address part, company id.
    assert!(joined.contains("Invoice for"), "{joined}");
    assert!(joined.contains("9 Placeholder Road,"), "{joined}");
    assert!(joined.contains("Testville,"), "{joined}");
    assert!(joined.contains("AA1 1AA,"), "{joined}");
    assert!(joined.contains("Freedonia"), "{joined}");
    assert!(
        joined.contains("Company number: 99999999"),
        "customer legal id: {joined}"
    );
    // Dates: dd/mm/yyyy, and the due date carries the frozen terms suffix.
    let due = issued["due_date"].as_str().unwrap();
    let due_display = format!("{}/{}/{}", &due[8..10], &due[5..7], &due[0..4]);
    assert!(
        joined.contains(&format!("{due_display} (Net 30)")),
        "due date + terms: {joined}"
    );
    let issued_at = issued["issued_at"].as_str().unwrap();
    let issue_display = format!(
        "{}/{}/{}",
        &issued_at[8..10],
        &issued_at[5..7],
        &issued_at[0..4]
    );
    assert!(joined.contains(&issue_display), "issue date: {joined}");
    // Derived totals and the item-type table header.
    assert!(joined.contains("Total hours: 3h 00min"), "{joined}");
    assert!(joined.contains("Item type"), "{joined}");
    assert!(joined.contains("Service"), "{joined}");
    // Symbol-first money: `EUR €180.00`, never the email `180.00 EUR` form.
    assert!(joined.contains("EUR \u{20ac}180.00"), "{joined}");
    assert!(!joined.contains("180.00 EUR"), "{joined}");
    // Every page is numbered.
    assert!(joined.contains("Page 1 of 1"), "{joined}");
}

/// The on-demand preview works for a draft, so it must render the same
/// resolved subject `/document` and the email report — not only the frozen
/// snapshot an issued invoice carries.
#[tokio::test]
async fn draft_pdf_preview_prints_the_live_subject() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-08-03","customer_id":cid,"project_code":"P1","hours":2})),
    )
    .await;
    let (_s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-08-01","to":"2026-08-31"})),
    )
    .await;
    assert_eq!(inv["status"], "draft");
    let iid = inv["id"].as_str().unwrap();

    let (sd, doc) = json_req(&app, "GET", &format!("/api/invoices/{iid}/document"), None).await;
    assert_eq!(sd, StatusCode::OK, "{doc}");
    let subject = doc["subject"].as_str().unwrap();
    assert_eq!(subject, "TatuTime invoice for 08 2026");

    let (_sp, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf/preview")).await;
    let joined = pdf_joined(&pdf);
    assert!(joined.contains("Subject:"), "subject row: {joined}");
    assert!(joined.contains(subject), "resolved subject: {joined}");
    assert!(joined.contains("INVOICE"), "resolved title: {joined}");
}

/// A partly paid invoice renders the settlement rows on the live preview: the
/// archive keeps its issue-time bytes, the preview follows the money.
#[tokio::test]
async fn partly_paid_preview_prints_payments_and_amount_due() {
    let (app, _d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;
    assert_eq!(issued["total_minor"], 18000);
    let (sp, paid) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"amount_minor": 10000})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{paid}");
    assert_eq!(paid["status"], "partly_paid");

    let (_s, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf/preview")).await;
    let joined = pdf_joined(&pdf);
    assert!(joined.contains("Subtotal"), "{joined}");
    assert!(joined.contains("Payments"), "{joined}");
    assert!(
        joined.contains("EUR -\u{20ac}100.00"),
        "negative payment, sign before the symbol: {joined}"
    );
    assert!(joined.contains("Amount due"), "{joined}");
    assert!(joined.contains("EUR \u{20ac}80.00"), "{joined}");

    // The write-once archive is the issue-time (unpaid) document and still
    // shows the bold total rather than a settlement block.
    let (_sa, _ha, archived) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let archived_text = pdf_joined(&archived);
    assert!(archived_text.contains("Subtotal"), "{archived_text}");
    assert!(!archived_text.contains("Amount due"), "{archived_text}");
    assert!(
        !archived_text.contains("EUR -\u{20ac}100.00"),
        "{archived_text}"
    );
}

/// A customer with structured payment rails prints the canonical block:
/// a `Payment details` heading and one currency-labelled rail per currency,
/// the same rows the email and `%payment_details%` carry.
#[tokio::test]
async fn invoice_pdf_prints_one_block_per_payment_rail() {
    let (app, _d) = app().await;
    let (sc, c) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({
            "name": "TatuTime",
            "currency": "EUR",
            "default_rate_minor": 6000,
            "email": "rails@tatu.test",
            "payment_details": {
                "rails": [
                    {
                        "currency": "EUR",
                        "account_name": "TatuTime",
                        "iban": "EX000000000000000000",
                        "bic": "EXAMPLEB"
                    },
                    {
                        "currency": "USD",
                        "account_name": "TatuTime",
                        "account_number": "000000000000",
                        "routing_number": "000000000",
                        "swift": "EXAMPLEB"
                    }
                ]
            }
        })),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{c}");
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-08-03","customer_id":cid,"project_code":"P1","hours":1})),
    )
    .await;
    let (_s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-08-01","to":"2026-08-31"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;

    let (_sp, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let joined = pdf_joined(&pdf);
    assert!(
        joined.contains("Payment details"),
        "payment heading: {joined}"
    );
    assert!(
        !joined.contains("For all EUR Payments;"),
        "the old prose is replaced by labelled rows: {joined}"
    );
    assert_eq!(
        joined.matches("Currency").count(),
        2,
        "one currency-labelled rail each: {joined}"
    );
    assert!(joined.contains("EUR"), "{joined}");
    assert!(joined.contains("USD"), "{joined}");
    assert!(joined.contains("EX000000000000000000"), "{joined}");
    assert!(joined.contains("000000000"), "{joined}");
    // The credit line is the last footer line.
    assert!(
        joined.contains("Generated by TatuTime on TatuTime. Page 1 of 1"),
        "{joined}"
    );

    // The email carries the same currency-labelled block.
    let (se, sent) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{sent}");
    let mail = &app.email.messages()[0].text;
    assert!(mail.contains("How to pay:"), "{mail}");
    assert_eq!(
        mail.matches("Currency: ").count(),
        2,
        "one currency-labelled rail each: {mail}"
    );
    assert!(mail.contains("Currency: EUR"), "{mail}");
    assert!(mail.contains("Currency: USD"), "{mail}");
    assert!(mail.contains("EX000000000000000000"), "{mail}");
}

/// The three flat bank fields a customer configures for payment are printed on
/// the invoice itself: the archived PDF created at issue and the live preview
/// both carry the labelled values, not just the customer document (#438).
#[tokio::test]
async fn invoice_pdf_prints_bank_address_account_type_and_routing_number() {
    let (app, _d) = app().await;
    let (sc, c) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({
            "name": "Bank Fields Co",
            "currency": "EUR",
            "default_rate_minor": 6000,
            "email": "billing@bankfields.test",
            "payment_details": {
                "bank_name": "Example Bank",
                "bank_address": "1 Bank Street",
                "account_name": "Bank Fields Co",
                "account_number": "000000000000",
                "account_type": "Checking",
                "routing_number": "000000000",
                "iban": "EX000000000000000000",
                "bic": "EXAMPLEB",
                "reference": "Invoice number"
            }
        })),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{c}");
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
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
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;

    let expected = [
        "Bank address: 1 Bank Street",
        "Account type: Checking",
        "Routing number: 000000000",
    ];
    // The archived document written at issue.
    let (_sa, _ha, archived) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let archived = pdf_joined(&archived);
    for fragment in expected {
        assert!(
            archived.contains(fragment),
            "archived invoice PDF missing {fragment}: {archived}"
        );
    }
    // The on-demand preview rendered for the invoice panel.
    let (_sp, _hp, preview) =
        raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf/preview")).await;
    let preview = pdf_joined(&preview);
    for fragment in expected {
        assert!(
            preview.contains(fragment),
            "invoice PDF preview missing {fragment}: {preview}"
        );
    }
}

/// The same canonical block reaches the PDF, the `%payment_details%` document
/// variable and the invoice email when a customer has flat fields AND rails:
/// flat rows + instructions first, then every currency rail.
#[tokio::test]
async fn payment_block_is_canonical_across_pdf_variable_and_email() {
    let (app, _d) = app().await;
    let (sc, c) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({
            "name": "Canonical Co",
            "currency": "EUR",
            "default_rate_minor": 6000,
            "email": "pay@canonical.test",
            "payment_details": {
                "bank_name": "Example Bank",
                "bank_address": "1 Bank Street",
                "account_name": "Canonical Co",
                "account_type": "Checking",
                "routing_number": "000000000",
                "iban": "EX000000000000000000",
                "instructions": "Flat instruction.",
                "rails": [
                    {
                        "currency": "EUR",
                        "account_name": "Canonical Co",
                        "iban": "EX111111111111111111",
                        "instructions": "Rail EUR instruction."
                    },
                    {
                        "currency": "USD",
                        "account_name": "Canonical Co",
                        "account_number": "000000000000"
                    }
                ]
            }
        })),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{c}");
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-09-02","customer_id":cid,"project_code":"P1","hours":2})),
    )
    .await;
    let (_s, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-09-01","to":"2026-09-30"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    // The variable renders in the document (and thus in the email HTML too).
    json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({
            "subject": "Statement %invoice_number%",
            "body": "",
            "footer": "%payment_details%",
            "payment_terms": null
        })),
    )
    .await;

    let (_sp, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    let joined = pdf_joined(&pdf);
    let (_sd, doc) = json_req(&app, "GET", &format!("/api/invoices/{iid}/document"), None).await;
    let html = doc["html"].as_str().unwrap().to_string();
    let (se, sent) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{sent}");
    let mail = app.email.messages()[0].text.clone();

    // Every surface carries every line of the canonical block.
    for fragment in [
        "Bank: Example Bank",
        "Bank address: 1 Bank Street",
        "Account name: Canonical Co",
        "Account type: Checking",
        "Routing number: 000000000",
        "IBAN: EX000000000000000000",
        "Flat instruction.",
        "Currency: EUR",
        "IBAN: EX111111111111111111",
        "Rail EUR instruction.",
        "Currency: USD",
        "Account number: 000000000000",
    ] {
        assert!(
            joined.contains(fragment),
            "PDF missing {fragment}: {joined}"
        );
        assert!(
            html.contains(fragment),
            "document HTML missing {fragment}: {html}"
        );
        assert!(mail.contains(fragment), "email missing {fragment}: {mail}");
    }
    // And in the same order, flat block before the rails.
    let at = |needle: &str, haystack: &str| {
        haystack
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} missing from {haystack}"))
    };
    for haystack in [&joined, &html, &mail] {
        assert!(at("Flat instruction.", haystack) < at("Currency: EUR", haystack));
        assert!(at("Currency: EUR", haystack) < at("Currency: USD", haystack));
    }
}
