use crate::support::*;

use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use tucano_time::api::AppState;
use tucano_time::appconfig::AppConfig;
use tucano_time::auth::Session;
use tucano_time::email::{EmailError, EmailMessage, EmailSender, RecordingEmailSender};
use tucano_time::store::Store;
use tucano_time::vault::SecretVault;

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
    // The no-body path keeps the historical single recipient, now also exposed
    // as an array so the GUI can render the list.
    assert_eq!(body["recipients"], json!(["billing@acme.test"]));
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
    assert_eq!(m.to, vec!["accountant@firm.co".to_string()]);
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

// ------------------------------------------------ multi-recipient sending ---

#[tokio::test]
async fn invoice_email_explicit_recipients_are_ordered_deduped_and_recorded() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    let (s, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email"),
        Some(json!({
            "recipients": [
                "second@x.test",
                "FIRST@x.test",
                "second@X.TEST",
                "First@x.test"
            ]
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    // Given order, first spelling wins on a case-insensitive duplicate.
    assert_eq!(body["sent_to"], "second@x.test, FIRST@x.test");
    assert_eq!(body["recipients"], json!(["second@x.test", "FIRST@x.test"]));
    assert_eq!(body["transport"], "recording");

    let msgs = app.email.messages();
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0].to,
        vec!["second@x.test".to_string(), "FIRST@x.test".to_string()]
    );

    // The timeline records the same list (and keeps `to` for compatibility).
    let (_st, tl) = json_req(&app, "GET", &format!("/api/invoices/{iid}/timeline"), None).await;
    let event = tl["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "email_sent")
        .unwrap_or_else(|| panic!("email_sent recorded: {tl}"));
    assert_eq!(
        event["meta"]["recipients"],
        json!(["second@x.test", "FIRST@x.test"])
    );
    assert_eq!(event["meta"]["to"], "second@x.test, FIRST@x.test");
}

#[tokio::test]
async fn invoice_email_rejects_invalid_recipients_without_sending() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    let eleven: Vec<String> = (0..11).map(|i| format!("u{i}@x.test")).collect();
    let cases = [
        // A valid address does not carry a hostile one along.
        json!({"recipients": ["ok@x.test", "bad@x.test\r\nBcc: evil@y.test"]}),
        json!({"recipients": ["two@@x.test"]}),
        json!({"recipients": [""]}),
        json!({"recipients": ["   "]}),
        json!({"recipients": eleven}),
        // Unknown fields are still refused (deny_unknown_fields).
        json!({"recipients": ["ok@x.test"], "bcc": "sneak@y.test"}),
    ];
    for payload in cases {
        let (s, body) = json_req(
            &app,
            "POST",
            &format!("/api/invoices/{iid}/email"),
            Some(payload.clone()),
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{payload} -> {body}");
        assert_eq!(body["error"]["code"], "validation_failed", "{body}");
        assert!(
            !body["error"]["fields"]
                .as_array()
                .unwrap_or(&Vec::new())
                .is_empty(),
            "per-field messages: {body}"
        );
        assert_eq!(
            app.email.messages().len(),
            0,
            "nothing is sent for {payload}"
        );
    }
}

// ------------------------------------------------- admin SMTP settings ---

const TEST_VAULT_KEY: &str = "unit-test-vault-key-000000000000";

/// A transport that always fails, to exercise the 502 path.
struct FailingSender;

impl EmailSender for FailingSender {
    fn send(&self, _msg: &EmailMessage) -> Result<(), EmailError> {
        Err(EmailError::Transport("relay unavailable".into()))
    }
    fn transport(&self) -> &'static str {
        "smtp"
    }
}

/// An `AppState` with an enabled vault but **no** injected transport, so the
/// real per-send resolution (`DynamicEmailSender`) is exercised.
async fn vault_state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).expect("store");
    let vault = Arc::new(
        SecretVault::open(store.root(), Some(TEST_VAULT_KEY))
            .expect("vault")
            .expect("enabled"),
    );
    let session = Arc::new(Session::new(
        b"test-session-secret-0000000000000032".to_vec(),
        3600,
        false,
    ));
    (
        AppState::boot(store, session, Some(vault), Arc::new(AppConfig::empty())),
        dir,
    )
}

async fn vault_client_with(
    sender: Arc<dyn EmailSender>,
) -> (Client, tempfile::TempDir, Arc<RecordingEmailSender>) {
    let (state, dir) = vault_state().await;
    let recorder = Arc::new(RecordingEmailSender::default());
    let client = client_for_state(state.with_email(sender), recorder.clone()).await;
    (client, dir, recorder)
}

async fn vault_client_dynamic() -> (Client, tempfile::TempDir) {
    let (state, dir) = vault_state().await;
    let recorder = Arc::new(RecordingEmailSender::default());
    let client = client_for_state(state, recorder).await;
    (client, dir)
}

#[tokio::test]
async fn admin_email_settings_never_leak_the_password() {
    let (app, _d) = vault_client_dynamic().await;
    let (sg, before) = json_req(&app, "GET", "/api/admin/email", None).await;
    assert_eq!(sg, StatusCode::OK, "{before}");
    assert_eq!(before["vault_enabled"], true);
    assert_eq!(before["configured"], false);
    assert_eq!(before["transport"], "disabled");
    assert_eq!(before["has_password"], false);

    let secret = "smtp-password-that-must-not-leak";
    let (sp, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/email",
        Some(json!({
            "host": "relay.tucano.test",
            "port": 2525,
            "username": "mailer",
            "password": secret,
            "from": "billing@tucano.test"
        })),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{saved}");
    assert_eq!(saved["configured"], true);
    assert_eq!(saved["transport"], "smtp");
    assert_eq!(saved["host"], "relay.tucano.test");
    assert_eq!(saved["port"], 2525);
    assert_eq!(saved["username"], "mailer");
    assert_eq!(saved["from"], "billing@tucano.test");
    assert_eq!(saved["has_password"], true);
    assert!(saved.get("password").is_none(), "password never returned");
    assert!(!saved.to_string().contains(secret), "{saved}");

    // The read path re-resolves the vault and still never returns it.
    let (sg2, after) = json_req(&app, "GET", "/api/admin/email", None).await;
    assert_eq!(sg2, StatusCode::OK, "{after}");
    assert_eq!(after["configured"], true);
    assert_eq!(after["has_password"], true);
    assert!(!after.to_string().contains(secret), "{after}");
    assert!(after.get("password").is_none());

    // Omitting the password on a later edit keeps the stored one.
    let (sp2, kept) = json_req(
        &app,
        "PUT",
        "/api/admin/email",
        Some(json!({"host": "relay2.tucano.test", "port": 25})),
    )
    .await;
    assert_eq!(sp2, StatusCode::OK, "{kept}");
    assert_eq!(kept["has_password"], true, "omitted password is not wiped");
    assert_eq!(kept["host"], "relay2.tucano.test");
}

#[tokio::test]
async fn admin_email_put_validates_and_requires_the_vault() {
    let (vaulted, _d) = vault_client_dynamic().await;
    let invalid = [
        json!({"host": "", "port": 25}),
        json!({"host": "relay with space", "port": 25}),
        json!({"host": "relay.test", "port": 0}),
        json!({"host": "relay.test", "port": 25, "from": "not-an-address"}),
        json!({"host": "relay.test", "port": 25, "username": "bad\nuser"}),
        json!({"host": "relay.test", "port": 25, "extra": true}),
    ];
    for payload in invalid {
        let (s, body) = json_req(&vaulted, "PUT", "/api/admin/email", Some(payload.clone())).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{payload} -> {body}");
        assert_eq!(body["error"]["code"], "validation_failed", "{body}");
    }
    // Nothing was persisted by any rejected request.
    let (_sg, before) = json_req(&vaulted, "GET", "/api/admin/email", None).await;
    assert_eq!(before["configured"], false, "{before}");

    let (sp, saved) = json_req(
        &vaulted,
        "PUT",
        "/api/admin/email",
        Some(json!({"host": "relay.test", "port": 587})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{saved}");
    assert_eq!(saved["configured"], true);

    // Without a vault the write is a fail-closed 503 and mutates nothing.
    let (plain, _p) = app().await;
    let (s503, body) = json_req(
        &plain,
        "PUT",
        "/api/admin/email",
        Some(json!({"host": "relay.test", "port": 25})),
    )
    .await;
    assert_eq!(s503, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "vault_disabled", "{body}");
    let (_sg, after) = json_req(&plain, "GET", "/api/admin/email", None).await;
    assert_eq!(after["vault_enabled"], false);
    assert_eq!(after["configured"], false);
}

#[tokio::test]
async fn admin_email_test_unconfigured_configured_and_failure() {
    // Unconfigured: 422 email_not_configured and no send.
    let recorder = Arc::new(RecordingEmailSender::default());
    let (plain, _p, _detached) = vault_client_with(recorder.clone()).await;
    let (s1, body1) = json_req(
        &plain,
        "POST",
        "/api/admin/email/test",
        Some(json!({"to": "ops@x.test"})),
    )
    .await;
    assert_eq!(s1, StatusCode::UNPROCESSABLE_ENTITY, "{body1}");
    assert_eq!(body1["error"]["code"], "email_not_configured");
    assert_eq!(recorder.messages().len(), 0);

    // Configured + recording transport: the live transport is used.
    let recorder = Arc::new(RecordingEmailSender::default());
    let (app, _d, _r) = vault_client_with(recorder.clone()).await;
    json_req(
        &app,
        "PUT",
        "/api/admin/email",
        Some(json!({"host": "relay.test", "port": 25})),
    )
    .await;
    let (s2, body2) = json_req(
        &app,
        "POST",
        "/api/admin/email/test",
        Some(json!({"to": "ops@x.test"})),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "{body2}");
    assert_eq!(body2["sent_to"], "ops@x.test");
    assert_eq!(body2["transport"], "recording");
    let sent = recorder.messages();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, vec!["ops@x.test".to_string()]);

    // A failing transport is a 502 email_failed.
    let (failing, _f, _r) = vault_client_with(Arc::new(FailingSender)).await;
    json_req(
        &failing,
        "PUT",
        "/api/admin/email",
        Some(json!({"host": "relay.test", "port": 25})),
    )
    .await;
    let (s3, body3) = json_req(
        &failing,
        "POST",
        "/api/admin/email/test",
        Some(json!({"to": "ops@x.test"})),
    )
    .await;
    assert_eq!(s3, StatusCode::BAD_GATEWAY, "{body3}");
    assert_eq!(body3["error"]["code"], "email_failed");
}

/// A minimal SMTP sink: records the command lines and accepts one message per
/// connection. Enough to prove the real `SmtpEmailSender` path end to end.
struct FakeSmtp {
    addr: std::net::SocketAddr,
    lines: Arc<std::sync::Mutex<Vec<String>>>,
}

impl FakeSmtp {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = lines.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let Ok(mut stream) = stream else { continue };
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut reply = |line: &str| {
                    use std::io::Write;
                    let _ = stream.write_all(format!("{line}\r\n").as_bytes());
                    let _ = stream.flush();
                };
                reply("220 fake ESMTP");
                loop {
                    let mut line = String::new();
                    if std::io::BufRead::read_line(&mut reader, &mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let command = line.trim_end_matches(['\r', '\n']).to_string();
                    if let Ok(mut sink) = sink.lock() {
                        sink.push(command.clone());
                    }
                    let upper = command.to_ascii_uppercase();
                    if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                        reply("250 fake");
                    } else if upper.starts_with("MAIL FROM") || upper.starts_with("RCPT TO") {
                        reply("250 ok");
                    } else if upper == "DATA" {
                        reply("354 go");
                        loop {
                            let mut data = String::new();
                            if std::io::BufRead::read_line(&mut reader, &mut data).unwrap_or(0) == 0
                                || data.trim_end_matches(['\r', '\n']) == "."
                            {
                                break;
                            }
                        }
                        reply("250 queued");
                    } else if upper == "QUIT" {
                        reply("221 bye");
                        break;
                    } else {
                        reply("250 ok");
                    }
                }
            }
        });
        Self { addr, lines }
    }

    fn rcpt_lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.to_ascii_uppercase().starts_with("RCPT TO"))
            .cloned()
            .collect()
    }
}

#[tokio::test]
async fn smtp_settings_apply_on_the_next_send_without_restart() {
    // The state is built *before* any SMTP setting exists: boot resolves to
    // Disabled. A PUT into the vault must still reach the relay below, which is
    // only possible because the transport resolves per send.
    let (app, _d) = vault_client_dynamic().await;
    let sink = FakeSmtp::start();
    let (_sg, before) = json_req(&app, "GET", "/api/admin/email", None).await;
    assert_eq!(before["configured"], false);

    let (sp, saved) = json_req(
        &app,
        "PUT",
        "/api/admin/email",
        Some(json!({"host": "127.0.0.1", "port": sink.addr.port()})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{saved}");
    assert_eq!(saved["transport"], "smtp", "{saved}");

    // Admin test send.
    let (st, test_body) = json_req(
        &app,
        "POST",
        "/api/admin/email/test",
        Some(json!({"to": "ops@x.test"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{test_body}");
    assert_eq!(test_body["transport"], "smtp");
    assert!(
        sink.rcpt_lines()
            .iter()
            .any(|line| line == "RCPT TO:<ops@x.test>"),
        "{:?}",
        sink.rcpt_lines()
    );

    // Invoice email with two recipients: one RCPT per address, in order.
    let (_issued, iid) = seed_issued_invoice(&app).await;
    let (si, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/email"),
        Some(json!({"recipients": ["one@x.test", "two@x.test"]})),
    )
    .await;
    assert_eq!(si, StatusCode::OK, "{body}");
    assert_eq!(body["transport"], "smtp");
    let rcpts = sink.rcpt_lines();
    let one = rcpts
        .iter()
        .position(|line| line == "RCPT TO:<one@x.test>")
        .expect("first recipient");
    let two = rcpts
        .iter()
        .position(|line| line == "RCPT TO:<two@x.test>")
        .expect("second recipient");
    assert!(one < two, "recipients keep their order: {rcpts:?}");
}
