use crate::support::*;

use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;
use tucano_time::api::AppState;
use tucano_time::store::Store;

#[tokio::test]
async fn invoice_email_sends_to_customer() {
    let (app, _d) = app().await;
    // Customer with a billing email (#35).
    let (sc, cust) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"ACME","currency":"EUR","default_rate_minor":6000,"email":"billing@acme.test"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cust}");
    assert_eq!(cust["email"], "billing@acme.test");
    let cid = cust["id"].as_str().unwrap();
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

    // Draft invoices must be issued before emailing.
    let (sd, _) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(sd, StatusCode::CONFLICT);

    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    assert_eq!(body["sent_to"], "billing@acme.test");
    // #130: the result names the transport; the test recorder never claims smtp.
    assert_eq!(body["transport"], "recording");
    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].text.contains("3h") || msgs[0].text.contains("180.00"),
        "{}",
        msgs[0].text
    );
    assert!(msgs[0].subject.contains(inv["number"].as_str().unwrap()));
}

#[tokio::test]
async fn invoice_email_without_customer_email_is_422() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "NOEMAIL", "EUR", 6000).await; // no email field
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
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(app.email.messages().len(), 0);
}

// ------------------------------------------------------------------ #112 ---

#[tokio::test]
async fn email_copy_sends_pdf_to_third_party_with_audit_trail() {
    let (app, _d) = app().await;
    let (issued, iid) = seed_issued_invoice(&app).await;
    let (_s, _h, pdf) = raw_req(&app, "GET", &format!("/api/invoices/{iid}/pdf")).await;

    let (status, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "accountant@firm.co", "note": "for the October books"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sent_to"], "accountant@firm.co");
    assert_eq!(body["transport"], "recording");

    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    assert_eq!(m.to, "accountant@firm.co");
    assert!(
        m.subject.contains(issued["number"].as_str().unwrap()),
        "{}",
        m.subject
    );
    // The cover names the requester (session user "Admin") and the note.
    assert!(m.text.contains("Admin"), "{}", m.text);
    assert!(m.text.contains("for the October books"), "{}", m.text);
    let (name, bytes) = m.attachment.as_ref().expect("PDF copy attached");
    assert_eq!(*name, format!("{}.pdf", issued["number"].as_str().unwrap()));
    assert_eq!(*bytes, pdf, "the attached copy is the archived document");

    // Audit records recipient + invoice, never the bytes (#52).
    let (_s, audit) = json_req(&app, "GET", "/api/audit", None).await;
    let hit = audit["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "invoice_email_copy");
    assert!(hit.is_some(), "audit event recorded: {audit}");
    let subject = hit.unwrap()["subject"].as_str().unwrap();
    assert!(
        subject.contains(iid.as_str()) && subject.contains("accountant@firm.co"),
        "{subject}"
    );
    assert!(!subject.contains("%PDF"));
}

#[tokio::test]
async fn email_copy_invalid_recipient_is_422_without_sending() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    let (status, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "not-an-email"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["fields"][0]["field"], "to");
    // Nothing sent, nothing persisted beyond the earlier issue.
    assert_eq!(app.email.messages().len(), 0);
    let (_s, audit) = json_req(&app, "GET", "/api/audit", None).await;
    assert!(
        !audit["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "invoice_email_copy"),
        "rejection without side effects"
    );
}

#[tokio::test]
async fn email_copy_draft_is_409_and_shapes_are_rejected() {
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

    // Draft: 409, no send.
    let (status, _b) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "x@y.co"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(app.email.messages().len(), 0);

    // Unknown field (deny_unknown_fields) and oversized note: 422.
    let (s1, _b1) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "x@y.co", "bcc": "sneaky@evil.co"})),
    )
    .await;
    assert_eq!(s1, StatusCode::UNPROCESSABLE_ENTITY);
    let (s2, _b2) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "x@y.co", "note": "n".repeat(2001)})),
    )
    .await;
    assert_eq!(s2, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(app.email.messages().len(), 0);

    // Unknown invoice: 404.
    let (s3, _) = json_req(
        &app,
        "POST",
        "/api/invoices/00000000-0000-0000-0000-000000000000/email-copy",
        Some(json!({"to": "x@y.co"})),
    )
    .await;
    assert_eq!(s3, StatusCode::NOT_FOUND);

    // Member: admin tier, 403.
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
    let (s4, _, _) = raw(
        &app.router,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "x@y.co"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(s4, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn email_copy_accepts_paid_invoices() {
    // Draft is the only refused state: a settled invoice still goes to the
    // accountant (matches the #113 download rule).
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "bank:42"})),
    )
    .await;
    let (status, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "books@firm.co"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].attachment.is_some());
}

#[tokio::test]
async fn email_result_reports_disabled_transport_honestly() {
    // #130: the DEFAULT production wiring (no SMTP env, no vault) boots
    // DisabledEmailSender — sends succeed as logged no-ops. The response must
    // disclose transport "disabled" so no client can read a 200 as delivery.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).expect("store");
    let router = tucano_time::build_router(AppState::new(store));
    raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    let cookie = login_cookie(&router, ADMIN_EMAIL, ADMIN_PW).await;
    let call = |m: String, u: String, b: Option<Value>| {
        let r = router.clone();
        let c = cookie.clone();
        async move {
            let (s, v, _) = raw(&r, &m, &u, b, Some(&c)).await;
            (s, v)
        }
    };
    let (sc, cust) = call(
        "POST".to_string(),
        "/api/customers".into(),
        Some(json!({"name":"ACME","currency":"EUR","default_rate_minor":6000,"email":"billing@acme.test"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cust}");
    let cid = cust["id"].as_str().unwrap().to_string();
    call(
        "POST".to_string(),
        format!("/api/customers/{cid}/projects"),
        Some(json!({"code":"P1","currency":"EUR","rate_minor":6000})),
    )
    .await;
    call(
        "POST".to_string(),
        "/api/entries".into(),
        Some(json!({"date":"2026-10-02","customer_id":cid.clone(),"project_code":"P1","hours":3})),
    )
    .await;
    let (_s, inv) = call(
        "POST".to_string(),
        "/api/invoices".into(),
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    call("POST".into(), format!("/api/invoices/{iid}/issue"), None).await;
    let (se, body) = call("POST".into(), format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    assert_eq!(body["sent_to"], "billing@acme.test");
    assert_eq!(body["transport"], "disabled", "no SMTP => honest label");
    let (sc2, copy) = call(
        "POST".to_string(),
        format!("/api/invoices/{iid}/email-copy"),
        Some(json!({"to": "books@firm.co"})),
    )
    .await;
    assert_eq!(sc2, StatusCode::OK, "{copy}");
    assert_eq!(copy["transport"], "disabled");
    // The audit line records the transport alongside the recipient (#52).
    let (_sa, audit) = call("GET".into(), "/api/audit".into(), None).await;
    let hit = audit["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"] == "invoice_email_copy");
    assert!(hit.is_some());
    assert!(
        hit.unwrap()["subject"]
            .as_str()
            .unwrap()
            .ends_with(":disabled")
    );
}

#[tokio::test]
async fn invoice_email_refuses_control_characters_at_the_sink() {
    // #S1: a header-bound value that passes every write-path check -- the org
    // "email sender display name" is only length-validated -- must still be
    // refused when the mail is assembled, so no customer-planted CRLF can fold
    // a header into a message the app sends in the organisation's name, nor
    // inject a line into the SMTP dialogue. Nothing is sent, nothing is
    // recorded, and the refusal never echoes the hostile value.
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    let (s, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/org",
        Some(json!({"name":"Tucano BV","from_name":"Acme\r\nBcc: attacker@evil.example"})),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::OK,
        "the write path is unchanged here: {saved}"
    );

    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"]["code"], "email_failed", "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("control character"),
        "the refusal names the reason: {body}"
    );
    assert!(
        !body.to_string().contains("attacker@evil.example"),
        "the refused value is never echoed: {body}"
    );
    // Refused before it was emitted: the recording transport holds nothing.
    assert_eq!(app.email.messages().len(), 0, "refused, never emitted");
    // And no delivery was recorded on the invoice timeline.
    let (_st, tl) = json_req(&app, "GET", &format!("/api/invoices/{iid}/timeline"), None).await;
    assert!(
        tl["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["kind"] != "email_sent"),
        "a refused send records no delivery: {tl}"
    );
}
