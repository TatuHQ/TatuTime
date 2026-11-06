use crate::support::*;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn customer_invoice_prefix_validation_and_legacy_updates() {
    let (app, _dir) = app().await;
    let base = json!({"name":"Prefix customer", "currency":"EUR", "default_rate_minor":1});
    let mut input = base.clone();
    input["invoice_prefix"] = json!(" cust1- ");
    let (status, customer) = json_req(&app, "POST", "/api/customers", Some(input)).await;
    assert_eq!(status, StatusCode::CREATED, "{customer}");
    assert_eq!(customer["invoice_prefix"], "CUST1");
    let path = format!("/api/customers/{}", customer["id"].as_str().unwrap());
    let (status, saved) = json_req(&app, "PUT", &path, Some(base.clone())).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved, customer);
    for invalid in [
        json!("INV"),
        json!("---"),
        json!("-ACME"),
        json!("A/B"),
        json!("A_B"),
        json!("É"),
        json!("ACME\n"),
        json!("A".repeat(21)),
        json!("A".repeat(65)),
        json!(42),
        json!(true),
        json!([]),
        json!({}),
    ] {
        for (method, target) in [("POST", "/api/customers"), ("PUT", path.as_str())] {
            let mut input = base.clone();
            input["invoice_prefix"] = invalid.clone();
            let (status, error) = json_req(&app, method, target, Some(input)).await;
            assert_eq!(
                status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{invalid}: {error}"
            );
            assert_eq!(error["error"]["code"], "validation_failed");
            assert!(
                error["error"]["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|field| field["field"] == "invoice_prefix"),
                "{error}"
            );
            let (_, unchanged) = json_req(&app, "GET", &path, None).await;
            assert_eq!(unchanged, customer);
        }
    }
    let (_, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert_eq!(list["customers"].as_array().unwrap().len(), 1);
    let mut input = base;
    input["invoice_prefix"] = json!("A".repeat(20));
    let (status, customer) = json_req(&app, "PUT", &path, Some(input.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(customer["invoice_prefix"], "A".repeat(20));
    input["invoice_prefix"] = json!("");
    let (status, customer) = json_req(&app, "PUT", &path, Some(input)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(customer.get("invoice_prefix").is_none());
}

#[tokio::test]
async fn customer_invoice_prefix_changes_only_future_drafts() {
    let (app, _dir) = app().await;
    let mut customer = json!({"name":"Prefix customer", "currency":"EUR", "default_rate_minor":1, "invoice_prefix":"CUST1"});
    let (status, created) = json_req(&app, "POST", "/api/customers", Some(customer.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap();
    let input = json!({"customer_id":id,"lines":[{"description":"Work", "quantity_hundredths":100,"unit_price_minor":100,"item_kind":"service"}]});
    let mut saved = Vec::new();
    for (prefix, expected) in [
        ("CUST1", "CUST1-001"),
        ("MONKEY", "MONKEY-001"),
        ("CUST1", "CUST1-002"),
        ("", "INV-0001"),
    ] {
        customer["invoice_prefix"] = json!(prefix);
        let (status, result) = json_req(
            &app,
            "PUT",
            &format!("/api/customers/{id}"),
            Some(customer.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let (status, invoice) =
            json_req(&app, "POST", "/api/invoices/manual", Some(input.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{invoice}");
        assert_eq!(invoice["number"], expected);
        let invoice_id = invoice["id"].as_str().unwrap().to_string();
        if saved.is_empty() {
            let (status, issued) = json_req(
                &app,
                "POST",
                &format!("/api/invoices/{invoice_id}/issue"),
                Some(json!({})),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{issued}");
            assert_eq!(issued["number"], expected);
        }
        saved.push((invoice_id, expected));
    }
    for (id, expected) in saved {
        let (status, invoice) = json_req(&app, "GET", &format!("/api/invoices/{id}"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(invoice["number"], expected);
    }
}

#[tokio::test]
async fn unknown_field_rejected_before_persist() {
    let (app, d) = app().await;
    let (status, body) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"TatuTime","currency":"EUR","default_rate_minor":1,"evil":true})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_failed");
    assert_eq!(body["error"]["fields"][0]["field"], "evil");
    assert_eq!(body["error"]["fields"][0]["message"], "unknown field");
    // Nothing was written: the customers collection stays empty.
    let (s2, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert_eq!(s2, StatusCode::OK);
    assert!(list["customers"].as_array().unwrap().is_empty());
    let _ = d;
}

#[tokio::test]
async fn wrong_type_is_validation_failure() {
    let (app, _d) = app().await;
    // name given as a number
    let (status, body) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":42,"currency":"EUR","default_rate_minor":1})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "validation_failed");
    // #338: the field is the *key* the value belongs to, never the value serde
    // quotes in backticks — that token used to be reported as the field
    // (`"field": "42"`), naming a key nobody has. serde_json names no key for a
    // struct type mismatch, so api::deser_error maps the error's position back to
    // the key in the raw body; the reason stays serde's own sentence.
    assert_eq!(body["error"]["fields"][0]["field"], "name");
    let reason = body["error"]["fields"][0]["message"].as_str().unwrap();
    assert!(reason.contains("invalid type"), "reason: {reason}");
    assert!(reason.contains("column"), "the position survives: {reason}");
}

#[tokio::test]
async fn currency_normalised_and_rejected() {
    let (app, _d) = app().await;
    let created = new_customer(&app, "TatuTime", "eur", 6000).await;
    assert_eq!(
        created["currency"], "EUR",
        "currency is normalised to upper case"
    );

    let (status, _) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"X","currency":"Euros","default_rate_minor":1})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn full_customer_project_entry_flow() {
    let (app, _d) = app().await;
    let cust = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = cust["id"].as_str().unwrap();

    let proj = new_project(&app, cid, "p-1", json!({"name":"Portal"})).await;
    assert_eq!(proj["code"], "P-1", "code is normalised to upper case");

    let (status, entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "p-1", json!(8), "2026-10-02")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let eid = entry["id"].as_str().unwrap();
    assert_eq!(entry["hours"], 8);
    assert!(entry["note"].is_string(), "note defaults to empty string");

    // Read back by day and by id.
    let (s, list) = json_req(&app, "GET", "/api/entries?date=2026-10-02", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["entries"].as_array().unwrap().len(), 1);
    let (s2, got) = json_req(&app, "GET", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(got["id"], eid);
}

#[tokio::test]
async fn entry_requires_existing_customer_and_project() {
    let (app, _d) = app().await;
    let (status, body) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(
            "00000000-0000-0000-0000-000000000000",
            "P1",
            json!(1),
            "2026-10-02",
        )),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let fields = body["error"]["fields"].as_array().unwrap();
    assert!(fields.iter().any(|f| f["field"] == "customer_id"));
    assert!(fields.iter().any(|f| f["field"] == "project_code"));
}

#[tokio::test]
async fn project_must_belong_to_customer() {
    let (app, _d) = app().await;
    let a = new_customer(&app, "A", "EUR", 100).await;
    let b = new_customer(&app, "B", "EUR", 100).await;
    new_project(&app, a["id"].as_str().unwrap(), "P1", json!({})).await;
    // Use A's project code under B.
    let (status, body) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(
            b["id"].as_str().unwrap(),
            "P1",
            json!(1),
            "2026-10-02",
        )),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["field"] == "project_code")
    );
}

#[tokio::test]
async fn hours_boundaries() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    // 24.00 accepted.
    let (ok, _) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(24.00), "2026-10-02")),
    )
    .await;
    assert_eq!(ok, StatusCode::CREATED);

    // 24.01 rejected.
    let (bad, _) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(24.01), "2026-10-02")),
    )
    .await;
    assert_eq!(bad, StatusCode::UNPROCESSABLE_ENTITY);

    // three decimals rejected.
    let (bad2, _) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1.234), "2026-10-02")),
    )
    .await;
    assert_eq!(bad2, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn invalid_date_rejected_with_field_detail() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (status, body) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-13-40")),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["fields"][0]["field"], "date");
}

#[tokio::test]
async fn duplicate_project_conflicts() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (status, _) = json_req(
        &app,
        "POST",
        &format!("/api/customers/{cid}/projects"),
        Some(json!({"code": "P1", "currency": "EUR", "rate_minor": 6000})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn customer_delete_blocked_while_referenced() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    // Has projects -> cannot delete.
    let (status, _) = json_req(&app, "DELETE", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    // Delete the project, then it's removable.
    let (s, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/customers/{cid}/projects/P1"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s2, _) = json_req(&app, "DELETE", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(s2, StatusCode::NO_CONTENT);
    let (s3, _) = json_req(&app, "GET", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(s3, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn range_and_date_queries_are_exclusive_forms() {
    let (app, _d) = app().await;
    let (status, _) = json_req(&app, "GET", "/api/entries", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status2, _) = json_req(
        &app,
        "GET",
        "/api/entries?date=2026-10-02&from=2026-10-01",
        None,
    )
    .await;
    assert_eq!(status2, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn project_requires_currency_and_rate() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    // Missing rate_minor.
    let (s1, b1) = json_req(
        &app,
        "POST",
        &format!("/api/customers/{cid}/projects"),
        Some(json!({"code": "P1", "currency": "EUR"})),
    )
    .await;
    assert_eq!(s1, StatusCode::UNPROCESSABLE_ENTITY, "{b1}");
    // Missing currency.
    let (s2, _) = json_req(
        &app,
        "POST",
        &format!("/api/customers/{cid}/projects"),
        Some(json!({"code": "P1", "rate_minor": 6000})),
    )
    .await;
    assert_eq!(s2, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn legacy_project_document_resolves_from_customer() {
    let (app, d) = app().await;
    let c = new_customer(&app, "TatuTime", "USD", 4500).await;
    let cid = c["id"].as_str().unwrap();
    // Write a pre-#11 project document that omits currency and rate_minor.
    let dir = d
        .path()
        .join("data")
        .join("customers")
        .join(cid)
        .join("projects");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("OLD-1.json"),
        format!(r#"{{"customer_id":"{cid}","code":"OLD-1","name":"Old","active":true}}"#),
    )
    .unwrap();
    // Reading it back resolves currency/rate from the customer (USD / 4500).
    let (status, project) = json_req(
        &app,
        "GET",
        &format!("/api/customers/{cid}/projects/OLD-1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(project["currency"], "USD");
    assert_eq!(project["rate_minor"], 4500);
    // And it appears in the list with the same resolved values.
    let (ls, list) = json_req(&app, "GET", &format!("/api/customers/{cid}/projects"), None).await;
    assert_eq!(ls, StatusCode::OK);
    assert_eq!(list["projects"].as_array().unwrap().len(), 1);
}

// ------------------------------------------------------------------ #139 ---

#[tokio::test]
async fn customer_billing_details_round_trip_and_validate() {
    let (app, _d) = app().await;
    let (s, c) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(
            json!({"name":"GLOBAL","currency":"USD","default_rate_minor":7000,
          "address":{"street":"Mainstross 1","city":"Berlin","postal_code":"10115","country":"DE"},
          "contacts":[{"name":"Ada","role":"Finance","email":"ada@global.test","billing":true},
                      {"name":"Bob","role":"Ops","email":"bob@global.test"}],
          "tax_hundredths":1900,"discount_hundredths":250}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    assert_eq!(c["address"]["country"], "DE");
    assert_eq!(c["contacts"].as_array().unwrap().len(), 2);
    assert_eq!(c["tax_hundredths"], 1900);
    assert_eq!(c["discount_hundredths"], 250);
    // No top-level email at all: the billing contact receives the invoice.
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(
        &app,
        &cid,
        "P1",
        json!({"currency":"USD","rate_minor":7000}),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid.clone(),"project_code":"P1","hours":2})),
    )
    .await;
    let (_s2, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    assert_eq!(
        body["sent_to"], "ada@global.test",
        "billing contact wins (#139)"
    );
    assert_eq!(
        app.email.messages()[0].to,
        vec!["ada@global.test".to_string()]
    );

    // Validation matrix: bad contact email, two billing flags, tax too big,
    // country too short — all 422, nothing persisted by the last shape.
    let (s3, b3) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"BAD","currency":"EUR","default_rate_minor":1,
          "contacts":[{"name":"N","email":"nope"}]})),
    )
    .await;
    assert_eq!(s3, StatusCode::UNPROCESSABLE_ENTITY, "{b3}");
    let (s4, _) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"BAD","currency":"EUR","default_rate_minor":1,
          "contacts":[{"name":"A","billing":true},{"name":"B","billing":true}]})),
    )
    .await;
    assert_eq!(s4, StatusCode::UNPROCESSABLE_ENTITY);
    let (s5, _) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"BAD","currency":"EUR","default_rate_minor":1,"tax_hundredths":10001})),
    )
    .await;
    assert_eq!(s5, StatusCode::UNPROCESSABLE_ENTITY);
    let (s6, b6) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(json!({"name":"BAD","currency":"EUR","default_rate_minor":1,
          "address":{"street":"x","country":"D"}})),
    )
    .await;
    assert_eq!(s6, StatusCode::UNPROCESSABLE_ENTITY, "{b6}");
    let (_s7, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert!(
        list["customers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["name"] != "BAD"),
        "rejections persisted nothing"
    );
}

#[tokio::test]
async fn payment_details_round_trip_validate_and_email() {
    let (app, _d) = app().await;
    // Round trip through POST, with the "Get paid" block a customer carries.
    let (s, c) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(
            json!({"name":"PAYME","currency":"EUR","default_rate_minor":6000,
            "email":"pay@payme.test",
            "payment_details":{"bank_name":"Example Bank","bank_address":"1 Bank Street",
                "account_type":"Checking","routing_number":"000000000",
                "iban":"EX000000000000000000",
                "bic":"EXAMPLEB","reference":"Invoice number",
                "instructions":"Please pay by bank transfer in EUR."}}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    assert_eq!(c["payment_details"]["iban"], "EX000000000000000000");
    assert_eq!(c["payment_details"]["bank_name"], "Example Bank");
    assert_eq!(c["payment_details"]["bank_address"], "1 Bank Street");
    assert_eq!(c["payment_details"]["account_type"], "Checking");
    assert_eq!(c["payment_details"]["routing_number"], "000000000");
    let cid = c["id"].as_str().unwrap().to_string();

    // An all-blank block normalises to absent on update (never persisted).
    let (s2, c2) = json_req(
        &app,
        "PUT",
        &format!("/api/customers/{cid}"),
        Some(
            json!({"name":"PAYME","currency":"EUR","default_rate_minor":6000,
            "email":"pay@payme.test",
            "payment_details":{"bank_name":"  ","instructions":"   "}}),
        ),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "{c2}");
    assert!(
        c2.get("payment_details").is_none(),
        "blank block must not persist: {c2}"
    );

    // Restore, then the rejection matrix: every bad shape is a 422.
    let pay = json!({"bank_name":"Example Bank","bank_address":"1 Bank Street",
        "account_type":"Checking","routing_number":"000000000",
        "iban":"EX000000000000000000",
        "bic":"EXAMPLEB","reference":"Invoice number",
        "instructions":"Please pay by bank transfer in EUR."});
    json_req(
        &app,
        "PUT",
        &format!("/api/customers/{cid}"),
        Some(
            json!({"name":"PAYME","currency":"EUR","default_rate_minor":6000,
            "email":"pay@payme.test","payment_details":pay}),
        ),
    )
    .await;
    for bad in [
        json!({"iban":"EX00 0000 0000 0000 0000 00!"}), // not letters/digits
        json!({"bic":"SHORT"}),                         // 5 chars — wrong length
        json!({"iban":"AB"}),                           // below the 5-char floor
        json!({"routing":"99"}),                        // unknown field
        json!({"bank_address":"x".repeat(201)}),        // over the 200 cap
        json!({"account_type":"x".repeat(41)}),         // over the 40 cap
        json!({"routing_number":"x".repeat(41)}),       // over the 40 cap
        json!({"instructions":"x".repeat(501)}),        // over the 500 cap
    ] {
        let (sb, bb) = json_req(
            &app,
            "PUT",
            &format!("/api/customers/{cid}"),
            Some(
                json!({"name":"PAYME","currency":"EUR","default_rate_minor":6000,
                "email":"pay@payme.test","payment_details":bad}),
            ),
        )
        .await;
        assert_eq!(sb, StatusCode::UNPROCESSABLE_ENTITY, "{bb}");
    }
    let (_sg, got) = json_req(&app, "GET", &format!("/api/customers/{cid}"), None).await;
    assert_eq!(
        got["payment_details"]["bank_name"], "Example Bank",
        "rejections must not touch the stored block"
    );

    // The invoice email carries a "How to pay" section, rows + instructions.
    new_project(
        &app,
        &cid,
        "P1",
        json!({"currency":"EUR","rate_minor":6000}),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid.clone(),"project_code":"P1","hours":2})),
    )
    .await;
    let (_s3, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (se, body) = json_req(&app, "POST", &format!("/api/invoices/{iid}/email"), None).await;
    assert_eq!(se, StatusCode::OK, "{body}");
    let mail = &app.email.messages()[0].text;
    for fragment in [
        "How to pay:",
        "Bank: Example Bank",
        "Bank address: 1 Bank Street",
        "Account type: Checking",
        "Routing number: 000000000",
        "IBAN: EX000000000000000000",
        "BIC/SWIFT: EXAMPLEB",
        "Reference: Invoice number",
        "Please pay by bank transfer in EUR.",
    ] {
        assert!(mail.contains(fragment), "email missing {fragment}: {mail}");
    }

    // The %payment_details% document variable resolves through the template.
    json_req(
        &app,
        "PUT",
        "/api/admin/invoice-template",
        Some(json!({"subject":"","body":"","footer":"Pay us: %payment_details%"})),
    )
    .await;
    let (sd, doc) = json_req(&app, "GET", &format!("/api/invoices/{iid}/document"), None).await;
    assert_eq!(sd, StatusCode::OK, "{doc}");
    let html = doc["html"].as_str().unwrap();
    assert!(html.contains("Pay us:"), "{html}");
    assert!(html.contains("EX000000000000000000"), "{html}");
    assert!(html.contains("Routing number: 000000000"), "{html}");
    assert!(html.contains("Account type: Checking"), "{html}");
}

#[tokio::test]
async fn legacy_customer_keeps_working_and_new_fields_are_omitted_when_empty() {
    let (app, d) = app().await;
    let c = new_customer(&app, "OLDSTYLE", "EUR", 5000).await; // only the pre-#139 shape
    assert_eq!(c["email"], "");
    assert!(c.get("contacts").is_none(), "empty vec is skipped: {c}");
    assert!(c.get("address").is_none());
    assert!(c.get("tax_hundredths").is_none() || c["tax_hundredths"] == 0);
    // The stored JSON lacks the new keys entirely; reads must work anyway.
    let path = d
        .path()
        .join("data")
        .join("customers")
        .join(format!("{}.json", c["id"].as_str().unwrap()));
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("contacts"));
    let (_s2, got) = json_req(
        &app,
        "GET",
        &format!("/api/customers/{}", c["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(got["name"], "OLDSTYLE");
}

#[tokio::test]
async fn oversized_project_budgets_rejected_without_persisting() {
    // #187: budget fields were the only money/hours inputs persisted with no
    // bound (u64::MAX survived, breaking the burn-rate math downstream).
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let p = new_project(&app, cid, "P1", json!({"budget_hours": 5000})).await;
    assert_eq!(p["budget_hours"], 5000);
    for (field, value) in [
        ("budget_amount_minor", json!(u64::MAX)),
        ("budget_hours", json!(100_000_001u64)),
    ] {
        let mut body = json!({"code":"P1","currency":"EUR","rate_minor":6000,
                              "budget_hours": 5000});
        body[field] = value;
        let (s, b) = json_req(
            &app,
            "PUT",
            &format!("/api/customers/{cid}/projects/P1"),
            Some(body),
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{field}: {b}");
        let (_sr, got) = json_req(
            &app,
            "GET",
            &format!("/api/customers/{cid}/projects/P1"),
            None,
        )
        .await;
        assert_eq!(
            got["budget_hours"], 5000,
            "{field} rejection persisted nothing"
        );
    }
}

#[tokio::test]
async fn customer_without_ever_having_projects_deletes_cleanly() {
    // #141 regression: the projects directory only exists after a project was
    // added; remove_dir_all's ENOENT used to surface as a 500.
    let (app, _d) = app().await;
    let c = new_customer(&app, "BARE", "EUR", 5000).await;
    let cid = c["id"].as_str().unwrap();
    let (s, _, _) = raw_req(&app, "DELETE", &format!("/api/customers/{cid}")).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_sg, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert!(
        list["customers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["id"] != c["id"])
    );
}

#[tokio::test]
async fn control_characters_are_refused_on_write_without_persisting() {
    // #S1: the customer email, the billing contact's address and the
    // invoice-subject override all end up in a header of an email the app
    // sends. Refused at the write seam -- the standard 422 shape, persisting
    // nothing -- never left to the sink alone.
    let (app, _d) = app().await;
    for (field, value) in [
        (
            "email",
            "billing@tatutime.test\r\nBcc: attacker@evil.example",
        ),
        (
            "invoice_subject",
            "Inv %invoice_number%\nBcc: attacker@evil.example",
        ),
        ("email", "billing@tatutime.test\0"),
    ] {
        let mut body = json!({"name":"EVIL","currency":"EUR","default_rate_minor":6000});
        body[field] = json!(value);
        let (s, b) = json_req(&app, "POST", "/api/customers", Some(body)).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{field}: {b}");
        assert_eq!(b["error"]["code"], "validation_failed", "{field}: {b}");
        assert_eq!(b["error"]["fields"][0]["field"], field, "{field}: {b}");
        assert!(
            !b.to_string().contains("attacker@evil.example"),
            "{field} rejection echoes no hostile value: {b}"
        );
    }
    // The flagged billing contact is the same seam (#139).
    let (sc, cb) = json_req(
        &app,
        "POST",
        "/api/customers",
        Some(
            json!({"name":"EVIL2","currency":"EUR","default_rate_minor":6000,
                    "contacts":[{"name":"Billing","billing":true,
                                 "email":"billing@tatutime.test\r\nBcc: attacker@evil.example"}]}),
        ),
    )
    .await;
    assert_eq!(sc, StatusCode::UNPROCESSABLE_ENTITY, "{cb}");
    assert_eq!(
        cb["error"]["fields"][0]["field"], "contacts[0].email",
        "{cb}"
    );

    let (_s, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert!(
        list["customers"].as_array().unwrap().is_empty(),
        "nothing persisted: {list}"
    );
}

/// The active/inactive switch is an ordinary persisted field: retired records
/// stay in their collections (history keeps resolving them) and can be
/// reactivated, which is what the GUI's pickers rely on.
#[tokio::test]
async fn active_status_round_trips_for_customers_and_projects() {
    let (app, _dir) = app().await;
    let customer = json!({
        "name": "Retired Co", "currency": "EUR", "default_rate_minor": 6000, "active": false
    });
    let (status, retired) = json_req(&app, "POST", "/api/customers", Some(customer.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{retired}");
    assert_eq!(retired["active"], false);
    let cid = retired["id"].as_str().unwrap();

    // The register lists retired records: deactivation is not a delete.
    let (_, list) = json_req(&app, "GET", "/api/customers", None).await;
    assert_eq!(list["customers"][0]["active"], false, "{list}");

    let mut active = customer;
    active["active"] = json!(true);
    let (status, customer) =
        json_req(&app, "PUT", &format!("/api/customers/{cid}"), Some(active)).await;
    assert_eq!(status, StatusCode::OK, "{customer}");
    assert_eq!(customer["active"], true);

    let project = new_project(&app, cid, "P1", json!({"active": false})).await;
    assert_eq!(project["active"], false);
    let (status, project) = json_req(
        &app,
        "PUT",
        &format!("/api/customers/{cid}/projects/P1"),
        Some(json!({
            "code": "P1", "name": "P1", "currency": "EUR",
            "rate_minor": 6000, "active": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{project}");
    assert_eq!(project["active"], true);
}
