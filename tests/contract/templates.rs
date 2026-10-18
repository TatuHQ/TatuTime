use crate::support::*;

use axum::http::{StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use serde_json::json;
use tower::ServiceExt;

// ------------------------------------------------------------------ #116 ---

async fn new_customer_ex(app: &Client, body: Value) -> (StatusCode, Value) {
    json_req(app, "POST", "/api/customers", Some(body)).await
}

/// Draft an invoice for a customer (billing email set), returning (customer,
/// draft invoice). `extra` merges into the customer creation body (#116
/// fields).
async fn seed_customer_invoice(app: &Client, name: &str, extra: Value) -> (Value, Value) {
    let mut body = json!({"name": name, "currency": "EUR", "default_rate_minor": 6000, "email": "billing@t116.test"});
    if let (Some(obj), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            obj.insert(k.clone(), v.clone());
        }
    }
    let (s, c) = new_customer_ex(app, body).await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let cid = c["id"].as_str().unwrap();
    new_project(app, cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    let (_s2, inv) = json_req(
        app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    (c, inv)
}

#[tokio::test]
async fn payment_terms_decide_due_date_at_issue() {
    let today = chrono::Utc::now().date_naive();
    let cases = [
        (json!({"kind":"upon_receipt"}), today),
        (json!({"kind":"net_15"}), today + chrono::Duration::days(15)),
        (json!({"kind":"net_30"}), today + chrono::Duration::days(30)),
        (json!({"kind":"net_45"}), today + chrono::Duration::days(45)),
        (
            json!({"kind":"custom","days":21}),
            today + chrono::Duration::days(21),
        ),
    ];
    for (i, (terms, expected)) in cases.iter().enumerate() {
        let (app, _d) = app().await;
        let (_c, inv) =
            seed_customer_invoice(&app, &format!("TERMS{i}"), json!({"payment_terms": terms}))
                .await;
        let iid = inv["id"].as_str().unwrap();
        let (s, issued) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
        assert_eq!(s, StatusCode::OK, "{issued}");
        assert_eq!(
            issued["due_date"].as_str().unwrap(),
            expected.to_string(),
            "terms {terms}"
        );
    }
}

#[tokio::test]
async fn legacy_net14_fallback_is_untouched_and_org_default_applies() {
    // No terms anywhere: the legacy net-14 from period_to (2026-10-07) stays.
    let (app, _d) = app().await;
    let (_c, inv) = seed_customer_invoice(&app, "LEGACY", json!({})).await;
    let iid = inv["id"].as_str().unwrap();
    let (_, issued) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(
        issued["due_date"], "2026-10-21",
        "legacy fallback unchanged"
    );

    // Org default terms now apply to a terms-less customer.
    let (s, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"","footer":"","payment_terms":{"kind":"net_20"}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_c2, inv2) = seed_customer_invoice(&app, "NEXT", json!({})).await;
    let iid2 = inv2["id"].as_str().unwrap();
    let (_, issued2) = json_req(&app, "POST", &format!("/api/invoices/{iid2}/issue"), None).await;
    let today = chrono::Utc::now().date_naive();
    assert_eq!(
        issued2["due_date"].as_str().unwrap(),
        (today + chrono::Duration::days(20)).to_string()
    );

    // Customer terms beat the org default.
    let (_c3, inv3) = seed_customer_invoice(
        &app,
        "PAYER",
        json!({"payment_terms": {"kind": "upon_receipt"}}),
    )
    .await;
    let iid3 = inv3["id"].as_str().unwrap();
    let (_, issued3) = json_req(&app, "POST", &format!("/api/invoices/{iid3}/issue"), None).await;
    assert_eq!(issued3["due_date"].as_str().unwrap(), today.to_string());
}

#[tokio::test]
async fn template_save_validates_and_rejects_without_persisting() {
    let (app, d) = app().await;
    // Unknown %token% in the body: 422 naming the field; nothing persisted.
    let (s, body) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"Inv %invoice_number%","body":"pay %bogus% now","footer":""})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["fields"][0]["field"], "body");
    assert!(
        body["error"]["fields"][0]["message"]
            .as_str()
            .unwrap()
            .contains("%bogus%")
    );
    assert!(
        !d.path().join("data").join("invoice_template.json").exists(),
        "rejection persists nothing"
    );

    // Case-sensitive: %Total% is unknown.
    let (s2, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"%Total%","body":"","footer":""})),
    )
    .await;
    assert_eq!(s2, StatusCode::UNPROCESSABLE_ENTITY);

    // Malformed payment terms.
    let (s3, b3) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"payment_terms": {"kind": "custom"}})),
    )
    .await;
    assert_eq!(s3, StatusCode::UNPROCESSABLE_ENTITY, "{b3}");
    let (s4, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"payment_terms": {"kind": "net_30", "days": 30}})),
    )
    .await;
    assert_eq!(s4, StatusCode::UNPROCESSABLE_ENTITY);
    let (s5, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"payment_terms": {"kind": "custom", "days": 0}})),
    )
    .await;
    assert_eq!(s5, StatusCode::UNPROCESSABLE_ENTITY);

    // Unknown field rejected pre-write (#deny_unknown_fields contract).
    let (s6, _) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject": "s", "evil": true})),
    )
    .await;
    assert_eq!(s6, StatusCode::UNPROCESSABLE_ENTITY);

    // A valid template round-trips and lands on disk atomically.
    let (s7, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"Invoice %invoice_number% for %customer_name%","body":"# Hello\n\nthanks","footer":"Bank: IBAN","payment_terms":{"kind":"net_30"}})),
    )
    .await;
    assert_eq!(s7, StatusCode::OK, "{saved}");
    assert_eq!(saved["body"], "# Hello\n\nthanks");

    // GET returns the template plus the variable cheatsheet.
    let (_s8, got) = json_req(&app, "GET", "/api/admin/invoice-template", None).await;
    assert_eq!(
        got["template"]["subject"],
        "Invoice %invoice_number% for %customer_name%"
    );
    let vars = got["variables"].as_array().unwrap();
    assert!(vars.iter().any(|v| v["name"] == "%invoice_issue_month%"));
    assert!(vars.iter().any(|v| v["name"] == "%line_count%"));

    // Member: admin tier.
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
    let (s9, _, _) = raw(
        &app.router,
        "GET",
        "/api/admin/invoice-template",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(s9, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn document_endpoint_renders_live_and_escapes() {
    let (app, _d) = app().await;
    let (_c, inv) = seed_customer_invoice(
        &app,
        "DOCS",
        json!({"invoice_notes": "Please pay by <script>alert(1)</script> transfer"}),
    )
    .await;
    json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"October: %invoice_number%","body":"Dear %customer_name%,\n\n- consulting\n- support","footer":"Wire to IBAN XX","payment_terms":null})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    let (s, doc) = json_req(&app, "GET", &format!("/api/invoices/{iid}/document"), None).await;
    assert_eq!(s, StatusCode::OK, "{doc}");
    assert_eq!(
        doc["subject"],
        format!("October: {}", inv["number"].as_str().unwrap())
    );
    let html = doc["html"].as_str().unwrap();
    assert!(html.contains("<li>consulting</li>"), "{html}");
    assert!(html.contains("Wire to IBAN XX"), "{html}");
    // Customer notes are appended after the org footer.
    assert!(html.contains("by &lt;script&gt;"), "{html}");
    assert!(!html.contains("<script>"), "escaping is non-negotiable");

    // Unknown id: 404.
    let (s404, _) = json_req(
        &app,
        "GET",
        "/api/invoices/00000000-0000-0000-0000-000000000000/document",
        None,
    )
    .await;
    assert_eq!(s404, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn email_uses_resolved_subject_and_html_body() {
    let (app, _d) = app().await;
    let (_c, inv) = seed_customer_invoice(
        &app,
        "MAILER",
        json!({"invoice_subject": "%invoice_number% for %customer_name% — %invoice_issue_month%"}),
    )
    .await;
    json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"WRONG %invoice_number%","body":"Org footer body","footer":"","payment_terms":null})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (s, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    // Customer subject override beats the org template, variables resolved.
    assert_eq!(
        msgs[0].subject,
        format!("{} for MAILER — October", inv["number"].as_str().unwrap())
    );
    assert!(
        msgs[0].text.contains("Please find invoice"),
        "legacy text part stays"
    );
    let html = msgs[0].html.as_ref().expect("template body -> html part");
    assert!(html.contains("Org footer body"), "{html}");
    // The PDF attachment still rides along.
    assert!(msgs[0].attachment.is_some());
}

#[tokio::test]
async fn pdf_archive_applies_template_on_both_issue_and_lazy_paths() {
    let (app, d) = app().await;
    json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"THANK-YOU NOTE text","footer":"","payment_terms":null})),
    )
    .await;
    let (_c, inv) = seed_customer_invoice(&app, "SEAM", json!({})).await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (_s, _h, at_issue) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;

    // Drop the archive: the lazy path re-renders from the *same* content
    // source and must produce identical bytes (#113 determinism x #116 seam).
    std::fs::remove_file(
        d.path()
            .join("data")
            .join("invoices")
            .join(format!("{iid}.pdf")),
    )
    .unwrap();
    let (_s2, _h2, at_lazy) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_eq!(
        at_issue, at_lazy,
        "issue and lazy paths share the content source"
    );

    // And template content actually flows into the document: a body-less
    // template renders different bytes for the same invoice state.
    json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"","footer":"","payment_terms":null})),
    )
    .await;
    std::fs::remove_file(
        d.path()
            .join("data")
            .join("invoices")
            .join(format!("{iid}.pdf")),
    )
    .unwrap();
    let (_s3, _h3, at_no_template) =
        raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;
    assert_ne!(
        at_issue, at_no_template,
        "the body text changed the document"
    );
}

#[tokio::test]
async fn customer_invoice_fields_round_trip_and_validate() {
    let (app, _d) = app().await;
    // Unknown token in the subject: rejected before persistence.
    let (s, _) = new_customer_ex(
        &app,
        json!({"name":"BAD","currency":"EUR","default_rate_minor":1,"invoice_subject":"%nope%"}),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (_s2, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert!(list["customers"].as_array().unwrap().is_empty());

    // Invalid terms shapes.
    for bad in [
        json!({"kind":"custom","days":400}),
        json!({"kind":"net_20","days":20}),
    ] {
        let (sb, _bb) = new_customer_ex(
            &app,
            json!({"name":"BAD","currency":"EUR","default_rate_minor":1,"payment_terms": bad}),
        )
        .await;
        assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }

    // Valid shape round-trips through create and update.
    let (sc, c) = new_customer_ex(
        &app,
        json!({"name":"FULL","currency":"EUR","default_rate_minor":1,
               "payment_terms":{"kind":"custom","days":21},
               "invoice_notes":"net 21, thanks","invoice_subject":"Inv %invoice_number%"}),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{c}");
    assert_eq!(c["payment_terms"]["kind"], "custom");
    assert_eq!(c["payment_terms"]["days"], 21);
    let cid = c["id"].as_str().unwrap();
    let (_su, u) = json_req(
        &app,
        "PUT",
        &format!("/api/customers/{cid}"),
        Some(
            json!({"name":"FULL","currency":"EUR","default_rate_minor":1,
                    "payment_terms":{"kind":"upon_receipt"},
                    "invoice_notes":"","invoice_subject":""}),
        ),
    )
    .await;
    assert_eq!(u["payment_terms"]["kind"], "upon_receipt");
    assert!(
        u["payment_terms"].get("days").is_none() || u["payment_terms"]["days"].is_null(),
        "skipped when absent: {}",
        u["payment_terms"]
    );
}

// ------------------------------------------- unsaved template PDF preview --
//
// `POST /api/admin/invoice-template/preview` renders a CANDIDATE template
// against the shared sample (two time lines, one expense line, VAT, a due date
// and full payment details) with no invoice in the store, so the Settings
// editor can show what it is about to save.

/// POST a JSON body and keep the raw response: the preview answers with PDF
/// bytes, which `json_req` would turn into a string.
async fn preview_req(
    app: &Client,
    body: Value,
    cookie: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    preview_req_conditional(app, body, cookie, None).await
}

/// The same request with an optional `If-None-Match`, for the 304 path
/// (SET-06).
async fn preview_req_conditional(
    app: &Client,
    body: Value,
    cookie: Option<&str>,
    if_none_match: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut b = axum::http::Request::builder()
        .method("POST")
        .uri("/api/admin/invoice-template/preview")
        .header("x-csrf-protection", "1")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    if let Some(tag) = if_none_match {
        b = b.header(header::IF_NONE_MATCH, tag);
    }
    let res = app
        .router
        .clone()
        .oneshot(b.body(axum::body::Body::from(body.to_string())).unwrap())
        .await
        .expect("oneshot");
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

/// Independent parse of the preview bytes, as the PDF suite already does.
/// The layout draws each word separately, so extracted text is normalised to
/// single spaces before assertions about prose.
fn pdf_text(bytes: &[u8]) -> String {
    let doc = lopdf::Document::load_mem(bytes).expect("preview re-parses as a PDF");
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    doc.extract_text(&pages)
        .expect("text extraction")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn pdf_pages(bytes: &[u8]) -> usize {
    lopdf::Document::load_mem(bytes)
        .expect("preview re-parses as a PDF")
        .get_pages()
        .len()
}

#[tokio::test]
async fn template_preview_renders_a_pdf_with_no_invoice_in_the_store() {
    let (app, _d) = app().await;
    // The whole point: a preview exists before any invoice does.
    let (s, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    assert!(
        list["invoices"].as_array().unwrap().is_empty(),
        "the no-invoice case is explicit: {list}"
    );

    let body = json!({
        "subject": "Invoice %invoice_number%",
        "body": "# Preview\n\nHello %customer_name%",
        "footer": "Thanks",
        "payment_terms": null
    });
    let (status, headers, bytes) = preview_req(&app, body.clone(), Some(&app.cookie)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(headers[header::CONTENT_TYPE], "application/pdf");
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "inline; filename=\"invoice-template-preview.pdf\""
    );
    let etag = headers[header::ETAG].to_str().unwrap();
    assert!(
        etag.starts_with('"') && etag.ends_with('"') && etag.len() > 10,
        "quoted ETag: {etag}"
    );
    assert!(bytes.starts_with(b"%PDF-"), "PDF magic");
    assert!(
        bytes.len() > 2_000,
        "sensible preview size: {} bytes",
        bytes.len()
    );
    assert_eq!(pdf_pages(&bytes), 1);
    assert!(
        pdf_text(&bytes).contains("INV-0000"),
        "the sample invoice number is on the document"
    );

    // Same candidate template + org => the same ETag, so the GUI can skip a
    // redundant render.
    let (_, again, _) = preview_req(&app, body, Some(&app.cookie)).await;
    assert_eq!(again[header::ETAG], headers[header::ETAG]);
}

/// SET-06: the preview ETag is a real conditional request. It is derived from
/// the inputs, so a matching `If-None-Match` (exact, `*` or a list) answers 304
/// with no body before any render; a changed candidate yields a new tag.
#[tokio::test]
async fn template_preview_honours_if_none_match_with_304() {
    let (app, _d) = app().await;
    let body = json!({
        "subject": "Conditional %invoice_number%",
        "body": "Body CONDITIONAL",
        "footer": "Footer CONDITIONAL",
        "payment_terms": null
    });
    let (s1, h1, bytes1) = preview_req(&app, body.clone(), Some(&app.cookie)).await;
    assert_eq!(s1, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes1));
    let etag = h1[header::ETAG].to_str().unwrap().to_string();
    assert!(!bytes1.is_empty());

    // Exact validator: 304, empty body, same ETag.
    let (s2, h2, bytes2) =
        preview_req_conditional(&app, body.clone(), Some(&app.cookie), Some(&etag)).await;
    assert_eq!(s2, StatusCode::NOT_MODIFIED);
    assert!(bytes2.is_empty(), "304 carries no body");
    assert_eq!(h2[header::ETAG], h1[header::ETAG]);

    // The `*` form matches any current representation.
    let (s_star, _, star_body) =
        preview_req_conditional(&app, body.clone(), Some(&app.cookie), Some("*")).await;
    assert_eq!(s_star, StatusCode::NOT_MODIFIED);
    assert!(star_body.is_empty());

    // A comma-separated list is honoured, including a weak validator.
    let list = format!("\"deadbeef\", W/{etag}");
    let (s_list, _, list_body) =
        preview_req_conditional(&app, body.clone(), Some(&app.cookie), Some(&list)).await;
    assert_eq!(s_list, StatusCode::NOT_MODIFIED);
    assert!(list_body.is_empty());

    // A different candidate is a different input, hence a different ETag and
    // a fresh 200 body.
    let changed = json!({
        "subject": "Conditional %invoice_number%",
        "body": "Body CHANGED",
        "footer": "Footer CONDITIONAL",
        "payment_terms": null
    });
    let (s3, h3, bytes3) = preview_req(&app, changed, Some(&app.cookie)).await;
    assert_eq!(s3, StatusCode::OK);
    assert_ne!(h3[header::ETAG], h1[header::ETAG], "inputs changed the tag");
    assert!(!bytes3.is_empty());
}

#[tokio::test]
async fn template_preview_reflects_the_candidate_bytes_and_text() {
    let (app, _d) = app().await;
    let candidate = |body: &str, footer: &str, quantity: &str| {
        json!({
            "subject": "Preview %invoice_number%",
            "body": body,
            "footer": footer,
            "payment_terms": null,
            "labels": {"quantity": quantity}
        })
    };
    let (s1, _, first) = preview_req(
        &app,
        candidate(
            "Body ALPHA preview\n\nPay to %payment_details%\n\nLines: %line_count%",
            "Footer ALPHA",
            "Hours",
        ),
        Some(&app.cookie),
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "{}", String::from_utf8_lossy(&first));
    let (s2, _, second) = preview_req(
        &app,
        candidate("Body BETA preview", "Footer BETA", "Units"),
        Some(&app.cookie),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "{}", String::from_utf8_lossy(&second));

    assert_ne!(first, second, "the candidate template changes the bytes");
    let alpha = pdf_text(&first);
    let beta = pdf_text(&second);
    assert!(alpha.contains("Body ALPHA preview"), "{alpha}");
    assert!(alpha.contains("Footer ALPHA"), "{alpha}");
    assert!(alpha.contains("Hours"), "quantity label override: {alpha}");
    assert!(
        alpha.contains("Sample Bank") && alpha.contains("IBAN"),
        "%payment_details% resolved from the sample customer: {alpha}"
    );
    assert!(
        alpha.contains("Lines: 3"),
        "%line_count% sees two time lines + one expense: {alpha}"
    );
    assert!(beta.contains("Body BETA preview"), "{beta}");
    assert!(beta.contains("Units"), "quantity label override: {beta}");
    assert!(
        !beta.contains("Body ALPHA preview"),
        "the render is the candidate, not the other one: {beta}"
    );
}

#[tokio::test]
async fn template_preview_rejects_invalid_candidates_and_persists_nothing() {
    let (app, d) = app().await;
    let saved = json!({
        "subject": "SAVED %invoice_number%",
        "body": "saved body",
        "footer": "",
        "payment_terms": null
    });
    let (s, _) = json_req(&app, "PUT", "/api/admin/invoice-template", Some(saved)).await;
    assert_eq!(s, StatusCode::OK);
    let (_, before) = json_req(&app, "GET", "/api/admin/invoice-template", None).await;

    // Unknown %token%: the same 422 shape as PUT.
    let (s1, _, b1) = preview_req(
        &app,
        json!({"subject":"ok","body":"pay %bogus% now","footer":""}),
        Some(&app.cookie),
    )
    .await;
    assert_eq!(s1, StatusCode::UNPROCESSABLE_ENTITY);
    let e1: Value = serde_json::from_slice(&b1).unwrap();
    assert_eq!(e1["error"]["code"], "validation_failed", "{e1}");
    assert_eq!(e1["error"]["fields"][0]["field"], "body", "{e1}");
    assert!(
        e1["error"]["fields"][0]["message"]
            .as_str()
            .unwrap()
            .contains("%bogus%"),
        "{e1}"
    );

    // Oversized subject: 121 > SUBJECT_MAX.
    let (s2, _, b2) = preview_req(
        &app,
        json!({"subject": "x".repeat(121), "body": "", "footer": ""}),
        Some(&app.cookie),
    )
    .await;
    assert_eq!(s2, StatusCode::UNPROCESSABLE_ENTITY);
    let e2: Value = serde_json::from_slice(&b2).unwrap();
    assert_eq!(e2["error"]["fields"][0]["field"], "subject", "{e2}");

    // Control characters in the header-bound subject.
    let (s3, _, _) = preview_req(
        &app,
        json!({"subject": "x\r\nBcc: evil@example.test", "body": "", "footer": ""}),
        Some(&app.cookie),
    )
    .await;
    assert_eq!(s3, StatusCode::UNPROCESSABLE_ENTITY);

    // Unknown top-level field: deny_unknown_fields, like PUT.
    let (s4, _, _) = preview_req(
        &app,
        json!({"subject": "ok", "body": "", "footer": "", "evil": true}),
        Some(&app.cookie),
    )
    .await;
    assert_eq!(s4, StatusCode::UNPROCESSABLE_ENTITY);

    // Nothing persisted: the saved template is byte-for-byte unchanged.
    let (_, after) = json_req(&app, "GET", "/api/admin/invoice-template", None).await;
    assert_eq!(after, before, "a rejected preview saves nothing");
    let disk = std::fs::read(d.path().join("data").join("invoice_template.json")).unwrap();
    let disk: Value = serde_json::from_slice(&disk).unwrap();
    assert_eq!(
        disk, after["template"],
        "the stored singleton is untouched on disk"
    );
    assert_eq!(disk["subject"], "SAVED %invoice_number%");
}

#[tokio::test]
async fn template_preview_creates_no_invoice_archive_or_timeline() {
    let (app, d) = app().await;
    let (s, _, bytes) = preview_req(
        &app,
        json!({"subject":"Preview %invoice_number%","body":"body","footer":""}),
        Some(&app.cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));

    let data = d.path().join("data");
    let invoices = data.join("invoices");
    let invoice_files: Vec<_> = std::fs::read_dir(&invoices)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    assert!(
        invoice_files.is_empty(),
        "no invoice JSON, PDF archive, number ledger or timeline sidecar: {invoice_files:?}"
    );
    assert!(
        !data.join("invoice_template.json").exists(),
        "the candidate is never saved"
    );
    assert!(
        !data.join("archive").exists(),
        "no cold archive tree is created"
    );
    let (_, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert!(
        list["invoices"].as_array().unwrap().is_empty(),
        "still no invoice: {list}"
    );
}

#[tokio::test]
async fn template_preview_is_admin_only() {
    let (app, _d) = app().await;
    let body = json!({"subject":"Preview","body":"body","footer":""});
    // Anonymous: 401 before the handler runs.
    let (anonymous, _, _) = preview_req(&app, body.clone(), None).await;
    assert_eq!(anonymous, StatusCode::UNAUTHORIZED);

    // Member: authenticated but not admin => 403.
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"Eve","email":"eve-preview@test.local","password":"evepass123","role":"member"})),
    )
    .await;
    let cookie = login_cookie(&app.router, "eve-preview@test.local", "evepass123").await;
    let (member, _, _) = preview_req(&app, body, Some(&cookie)).await;
    assert_eq!(member, StatusCode::FORBIDDEN);
}

// ------------------------------------------------------------------ #S1 ---

#[tokio::test]
async fn template_subject_refuses_control_characters() {
    // #S1: the org template subject is written into the email's Subject header,
    // so a CR/LF/NUL is refused on write (422) and the previous template stays
    // intact. The body keeps accepting newlines -- it is multi-line document
    // text by design and never reaches a header.
    let (app, _d) = app().await;
    let (ok, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"October: %invoice_number%",
                    "body":"Dear %customer_name%,\n\n- consulting\n- support",
                    "footer":"Wire to IBAN XX","payment_terms":null})),
    )
    .await;
    assert_eq!(ok, StatusCode::OK, "{saved}");
    let (s, b) = json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(
            json!({"subject":"Inv %invoice_number%\r\nBcc: attacker@evil.example",
                    "body":"x","footer":"","payment_terms":null}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert_eq!(b["error"]["code"], "validation_failed", "{b}");
    assert_eq!(b["error"]["fields"][0]["field"], "subject", "{b}");
    assert!(
        !b.to_string().contains("attacker@evil.example"),
        "the refusal echoes no hostile value: {b}"
    );
    let (_sg, got) = json_req(&app, "GET", "/api/admin/invoice-template", None).await;
    assert_eq!(
        got["template"]["subject"], "October: %invoice_number%",
        "the rejection persisted nothing: {got}"
    );
}
