use crate::support::*;

use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn report_groups_by_project_with_effective_rate() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await; // 60.00/h default
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "STD", json!({})).await;
    new_project(&app, cid, "PREM", json!({"rate_minor": 12000})).await; // 120.00/h override

    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "STD", json!(2), "2026-10-02")),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "PREM", json!(2), "2026-10-02")),
    )
    .await;

    let (status, summary) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=project",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let rows = summary["rows"].as_array().unwrap();
    let prem = rows
        .iter()
        .find(|r| r["label"].as_str().unwrap().ends_with("PREM"))
        .unwrap();
    let std = rows
        .iter()
        .find(|r| r["label"].as_str().unwrap().ends_with("STD"))
        .unwrap();
    assert_eq!(prem["amount_minor"], 24000, "2h * 120.00 = 240.00");
    assert_eq!(std["amount_minor"], 12000, "2h * 60.00 = 120.00");
}

#[tokio::test]
async fn csv_export_quotes_and_escapes_note() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME, Inc.", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let body = json!({
        "date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":1.5,
        "note":"=SUM(A1) injected"
    });
    json_req(&app, "POST", "/api/entries", Some(body)).await;
    let (status, csv) = json_req(
        &app,
        "GET",
        "/api/reports/export.csv?from=2026-10-01&to=2026-10-07",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let text = csv.as_str().unwrap();
    assert!(text.starts_with("date,customer,"), "header present");
    // Formula injection is defused with a leading apostrophe and the comma in
    // the customer name is quoted.
    assert!(
        text.contains("\"ACME, Inc.\""),
        "comma field quoted; got: {text}"
    );
    assert!(text.contains("'=SUM(A1)"), "formula escaped; got: {text}");
}

#[tokio::test]
async fn error_shape_never_leaks_internals() {
    let (app, _d) = app().await;
    let (status, body) = json_req(
        &app,
        "GET",
        "/api/entries/00000000-0000-0000-0000-000000000000",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
    let text = body.to_string();
    for leak in ["data/", "temp", "stack", "No such file", "store.rs"] {
        assert!(!text.contains(leak), "error body leaked {leak:?}");
    }
}

#[tokio::test]
async fn update_entry_can_move_day() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (_, entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    let eid = entry["id"].as_str().unwrap();

    let (status, updated) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(entry_body(cid, "P1", json!(1), "2026-10-03")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["date"], "2026-10-03");
    // Old day now empty, new day has it.
    let (_, old) = json_req(&app, "GET", "/api/entries?date=2026-10-02", None).await;
    assert!(old["entries"].as_array().unwrap().is_empty());
    let (_, new) = json_req(&app, "GET", "/api/entries?date=2026-10-03", None).await;
    assert_eq!(new["entries"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn oversized_note_rejected() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let body = json!({
        "date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":1,
        "note": "x".repeat(501)
    });
    let (status, _) = json_req(&app, "POST", "/api/entries", Some(body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn entry_billable_defaults_true_and_persists_false() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    // Omitted -> billable true.
    let (_, def) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    assert_eq!(def["billable"], true);

    // Explicit false persists.
    let body = json!({
        "date": "2026-10-03", "customer_id": cid, "project_code": "P1",
        "hours": 2, "note": "internal", "billable": false
    });
    let (_, nb) = json_req(&app, "POST", "/api/entries", Some(body)).await;
    assert_eq!(nb["billable"], false);
    let eid = nb["id"].as_str().unwrap();
    let (_, got) = json_req(&app, "GET", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(got["billable"], false);
}

#[tokio::test]
async fn legacy_entry_document_without_billable_reads_true() {
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    // Craft a pre-#20 entry document (no `billable` field) directly on disk.
    let id = uuid::Uuid::new_v4();
    let dir = d.path().join("data").join("entries").join("2026-10-04");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.json")),
        format!(
            r#"{{"id":"{id}","date":"2026-10-04","customer_id":"{cid}","project_code":"P1","hours":3,"note":"","created_at":"2026-10-04T09:00:00Z","updated_at":"2026-10-04T09:00:00Z"}}"#
        ),
    )
    .unwrap();
    let (status, list) = json_req(&app, "GET", "/api/entries?date=2026-10-04", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["entries"][0]["billable"], true);
}

#[tokio::test]
async fn entry_source_defaults_manual() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (_, e) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    assert_eq!(e["source"], "manual");
}

#[tokio::test]
async fn legacy_entry_document_without_source_reads_manual() {
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let id = uuid::Uuid::new_v4();
    let dir = d.path().join("data").join("entries").join("2026-10-05");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.json")),
        format!(
            r#"{{"id":"{id}","date":"2026-10-05","customer_id":"{cid}","project_code":"P1","hours":2,"note":"","billable":true,"created_at":"2026-10-05T09:00:00Z","updated_at":"2026-10-05T09:00:00Z"}}"#
        ),
    )
    .unwrap();
    let (status, e) = json_req(&app, "GET", &format!("/api/entries/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(e["source"], "manual");
}

#[tokio::test]
async fn locked_entry_rejects_edit_and_delete_but_not_read() {
    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (_, e) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-10-02")),
    )
    .await;
    let eid: uuid::Uuid = e["id"].as_str().unwrap().parse().unwrap();
    drop(app);

    // Reopen the same data dir with that entry locked (as an invoice would).
    let app2 = app_locked(d.path(), Arc::new(LockOne(Some(eid)))).await;
    let body = json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2,"billable":true});
    let (s_edit, _) = json_req(&app2, "PUT", &format!("/api/entries/{eid}"), Some(body)).await;
    assert_eq!(s_edit, StatusCode::CONFLICT);
    let (s_del, _) = json_req(&app2, "DELETE", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(s_del, StatusCode::CONFLICT);
    let (s_get, _) = json_req(&app2, "GET", &format!("/api/entries/{eid}"), None).await;
    assert_eq!(s_get, StatusCode::OK, "locked entries are still readable");
}

#[tokio::test]
async fn report_applies_person_rate_tier_and_attributes_entry() {
    let (app, _d) = app().await;
    // Customer default 60/h; project rate 30/h.
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 3000})).await;

    // Admin creates a member with a personal default of 45/h.
    let (_s, bob) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"Bob","email":"bob@test.local","password":"bobpass123","role":"member","default_rate_minor":4500})),
    )
    .await;
    let bob_id = bob["id"].as_str().unwrap().to_string();

    // Bob logs 2h on P1.
    let bob_cookie = login_cookie(&app.router, "bob@test.local", "bobpass123").await;
    let (_s2, entry, _) = raw(
        &app.router,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
        Some(&bob_cookie),
    )
    .await;
    assert_eq!(
        entry["user_id"], bob_id,
        "entry attributed to the logging user"
    );

    // Report: person rate (45) overrides project rate (30) -> 2h * 45 = 90.00 = 9000.
    let (_s3, summary) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=customer",
        None,
    )
    .await;
    assert_eq!(summary["rows"][0]["amount_minor"], 9000);
}

#[tokio::test]
async fn report_splits_billable_and_groups_by_person() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    // 3h billable + 2h non-billable, both by the admin.
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
    )
    .await;
    json_req(&app, "POST", "/api/entries", Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2,"billable":false}))).await;

    let (_, s) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=customer",
        None,
    )
    .await;
    assert_eq!(s["billable_hours"], 3.0);
    assert_eq!(s["nonbillable_hours"], 2.0);
    assert_eq!(s["total_hours"], 5.0);

    // billable=true scopes rows to billable only (3h).
    let (_, sb) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=customer&billable=true",
        None,
    )
    .await;
    assert_eq!(sb["rows"][0]["hours"], 3.0);

    // group=person yields one row for the admin.
    let (_, sp) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=person",
        None,
    )
    .await;
    assert_eq!(sp["group"], "person");
    assert_eq!(sp["rows"][0]["label"], "Admin");
}

#[tokio::test]
async fn profitability_revenue_vs_cost() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    // Bob: cost rate 20/h.
    json_req(&app, "POST", "/api/users", Some(json!({"name":"Bob","email":"bob@t.local","password":"bobpass123","role":"member","cost_rate_minor":2000}))).await;
    let bob = login_cookie(&app.router, "bob@t.local", "bobpass123").await;
    // Bob logs 2h billable (revenue 2x60=12000, labour cost 2x20=4000).
    raw(
        &app.router,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
        Some(&bob),
    )
    .await;
    // billable expense 5000.
    json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-03","customer_id":cid,"amount_minor":5000,"currency":"EUR"})),
    )
    .await;
    // invoice (12000 + 5000 = 17000) and issue.
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

    let (s, prof) = json_req(
        &app,
        "GET",
        "/api/reports/profitability?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let row = prof["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == "ACME")
        .expect("ACME row");
    assert_eq!(row["revenue_minor"], 17000);
    assert_eq!(row["cost_minor"], 9000); // 5000 expense + 4000 labour
    assert_eq!(row["margin_minor"], 8000);
}

#[tokio::test]
async fn invoice_report_and_export() {
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

    let (s, rep) = json_req(
        &app,
        "GET",
        "/api/invoices/report?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(rep["issued"], 1);
    assert_eq!(rep["revenue_by_currency"]["EUR"], 18000); // 3h * 60
    let row = &rep["rows"][0];
    assert_eq!(row["label"], "ACME");
    assert_eq!(row["revenue_minor"], 18000);

    let (s2, csv) = json_req(&app, "GET", "/api/invoices/export.csv", None).await;
    assert_eq!(s2, StatusCode::OK);
    let text = csv.as_str().unwrap();
    assert!(text.starts_with("number,customer,"), "csv header: {text}");
    assert!(text.contains("INV-0001") && text.contains("ACME"));
}

#[tokio::test]
async fn reports_partition_by_currency_and_never_sum_cents_across_it() {
    // #186: a customer may hold projects in different currencies (#11), so
    // every grouping level must produce per-currency rows — previously the
    // EUR and USD cents were added into a single number.
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P-EUR", json!({"rate_minor": 6000})).await;
    new_project(
        &app,
        &cid,
        "P-USD",
        json!({"rate_minor": 5000, "currency": "USD"}),
    )
    .await;
    for (proj, hours) in [("P-EUR", 2), ("P-USD", 3)] {
        let (s, e) = json_req(
            &app,
            "POST",
            "/api/entries",
            Some(entry_body(&cid, proj, json!(hours), "2026-10-02")),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{e}");
    }

    for group in ["customer", "project", "person", "week"] {
        let (s, rep) = json_req(
            &app,
            "GET",
            &format!("/api/reports/summary?from=2026-10-01&to=2026-10-07&group={group}"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let rows = rep["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{group}: one row per currency, got {rows:?}");
        let eur = rows
            .iter()
            .find(|r| r["currency"] == "EUR")
            .expect("EUR row");
        let usd = rows
            .iter()
            .find(|r| r["currency"] == "USD")
            .expect("USD row");
        assert_eq!(eur["amount_minor"], 12000, "{group}"); // 2h * 60.00
        assert_eq!(usd["amount_minor"], 15000, "{group}"); // 3h * 50.00
        // Week rows no longer smuggle the currency into the key.
        assert!(
            !rows
                .iter()
                .any(|r| r["key"].as_str().unwrap().contains('|')),
            "{group}: keys are labels, currency rides its own field"
        );
    }

    // Invoice report + profitability partition the same way.
    for (proj, amount) in [("P-EUR", 12000u64), ("P-USD", 15000)] {
        let (_, inv) = json_req(
            &app,
            "POST",
            "/api/invoices",
            Some(json!({
                "customer_id": cid,
                "from": "2026-10-01",
                "to": "2026-10-07",
                "project_codes": [proj],
                "include_expenses": false,
            })),
        )
        .await;
        assert_eq!(inv["total_minor"], amount);
        json_req(
            &app,
            "POST",
            &format!("/api/invoices/{}/issue", inv["id"].as_str().unwrap()),
            None,
        )
        .await;
    }
    let (_s, rep) = json_req(
        &app,
        "GET",
        "/api/invoices/report?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    let rows = rep["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "one row per (customer, currency): {rows:?}");
    assert_eq!(rep["revenue_by_currency"]["EUR"], 12000);
    assert_eq!(rep["revenue_by_currency"]["USD"], 15000);

    let (_s, prof) = json_req(
        &app,
        "GET",
        "/api/reports/profitability?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    let rows = prof["rows"].as_array().unwrap();
    let acme: Vec<&Value> = rows.iter().filter(|r| r["label"] == "ACME").collect();
    assert_eq!(acme.len(), 2, "profitability splits by currency: {rows:?}");
    let eur = acme.iter().find(|r| r["currency"] == "EUR").unwrap();
    let usd = acme.iter().find(|r| r["currency"] == "USD").unwrap();
    assert_eq!(eur["revenue_minor"], 12000);
    assert_eq!(usd["revenue_minor"], 15000);
}

#[tokio::test]
async fn budget_report_shows_burn() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(
        &app,
        cid,
        "P1",
        json!({"budget_hours": 1000, "budget_amount_minor": 60000}),
    )
    .await;
    let (s, _) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(6), "2026-10-02")),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (sb, body) = json_req(&app, "GET", "/api/reports/budgets", None).await;
    assert_eq!(sb, StatusCode::OK);
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["project"], "P1");
    assert_eq!(rows[0]["burn_hours"], 6.0);
    assert_eq!(rows[0]["hours_pct"], 60.0);
    assert_eq!(rows[0]["over"], false);
    assert_eq!(rows[0]["burn_amount_minor"], 36000);
    assert_eq!(rows[0]["amount_pct"], 60.0);
}

#[tokio::test]
async fn management_reports_are_admin_only() {
    // Review A4: cost rates / budgets must not be visible to members.
    let (app, _d) = app().await;
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"M","email":"repmember@t.local","password":"memberpass1","role":"member"})),
    )
    .await;
    let member = login_cookie(&app.router, "repmember@t.local", "memberpass1").await;
    for path in ["/api/reports/profitability", "/api/reports/budgets"] {
        let (s, _, _) = raw(&app.router, "GET", path, None, Some(&member)).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path} must be admin-only");
    }
    // Members keep their own summary/export.
    let (s, _, _) = raw(
        &app.router,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07",
        None,
        Some(&member),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn invoice_report_gains_paid_and_balance_columns() {
    let (app, _d) = app().await;
    let (_issued, iid) = seed_issued_invoice(&app).await;
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference": "half", "amount_minor": 8000})),
    )
    .await;
    let (s, rep) = json_req(
        &app,
        "GET",
        "/api/invoices/report?from=2026-10-01&to=2026-10-31",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rep}");
    assert_eq!(rep["partly_paid"], 1);
    let row = rep["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["revenue_minor"] == 18000)
        .expect("ACME row");
    assert_eq!(row["paid_minor"], 8000);
    assert_eq!(row["balance_minor"], 10000);
    let (s2, csv) = json_req(&app, "GET", "/api/invoices/export.csv", None).await;
    assert_eq!(s2, StatusCode::OK);
    let csv = csv.as_str().unwrap();
    assert!(
        csv.contains("paid_minor,balance_minor,write_off_reason"),
        "{csv}"
    );
}

/// #262(8): the HTTP-level 400-day range cap (`MAX_RANGE_DAYS`, enforced at the
/// store level). A wider query rejects with 400; the ceiling itself passes.
#[tokio::test]
async fn range_query_over_max_days_rejects_with_400() {
    let (app, _d) = app().await;
    let (status, body) = json_req(
        &app,
        "GET",
        "/api/entries?from=2020-01-01&to=2026-06-01",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "bad_request");
    // A window inside the cap is accepted.
    let (ok, _) = json_req(
        &app,
        "GET",
        "/api/entries?from=2026-01-01&to=2026-06-01",
        None,
    )
    .await;
    assert_eq!(ok, StatusCode::OK);
}

// ------------------------------------------------------- pagination (#334) --

/// #334: a page that does not end the matching set says so (`next_cursor`), and
/// following the cursor reaches every row exactly once, in the documented
/// `(date, id)` order. The silent alternative — a short page with no signal —
/// is the failure this contract exists to prevent.
#[tokio::test]
async fn entry_pages_report_more_and_the_cursor_walks_every_row_once() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    // Five entries over three days, two days carrying two of them: the id
    // breaks that tie, and the tie-break is what makes the order total.
    for date in [
        "2026-10-01",
        "2026-10-01",
        "2026-10-02",
        "2026-10-02",
        "2026-10-03",
    ] {
        let (s, body) = json_req(
            &app,
            "POST",
            "/api/entries",
            Some(entry_body(cid, "P1", json!(1), date)),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
    }

    // The whole set in one page: this page ends it, and says so with null.
    let (s, all) = json_req(
        &app,
        "GET",
        "/api/entries?from=2026-10-01&to=2026-10-03&limit=100",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{all}");
    assert_eq!(
        all["next_cursor"],
        Value::Null,
        "a page that ends the set reports no more"
    );
    let rows = all["entries"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    let every: Vec<String> = rows
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    let dates: Vec<&str> = rows.iter().map(|e| e["date"].as_str().unwrap()).collect();
    assert!(
        dates.windows(2).all(|w| w[0] <= w[1]),
        "pages walk (date, id) ascending; got {dates:?}"
    );

    // Two rows per page: every page but the last must say there is more.
    let mut walked: Vec<String> = Vec::new();
    let mut url = String::from("/api/entries?from=2026-10-01&to=2026-10-03&limit=2");
    let mut pages = 0;
    loop {
        let (s, body) = json_req(&app, "GET", &url, None).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let page = body["entries"].as_array().unwrap();
        assert!(page.len() <= 2, "a page never exceeds 'limit'");
        walked.extend(page.iter().map(|e| e["id"].as_str().unwrap().to_string()));
        pages += 1;
        match body["next_cursor"].as_str() {
            Some(cursor) => {
                assert!(
                    walked.len() < every.len(),
                    "only a truncated page may name a next page"
                );
                let last = walked.last().unwrap();
                assert_ne!(cursor, last, "the cursor is a token, not the row id");
                assert!(
                    !cursor.contains('|'),
                    "the cursor is opaque; the raw (date, id) pair is never exposed"
                );
                url = format!("/api/entries?from=2026-10-01&to=2026-10-03&limit=2&cursor={cursor}");
            }
            None => {
                assert_eq!(walked.len(), every.len(), "the last page ended the set");
                break;
            }
        }
    }
    assert_eq!(
        walked, every,
        "the walk reaches every row exactly once, in the documented order"
    );
    assert_eq!(pages, 3, "five rows at two per page is three pages");
}

/// #334: a `limit` outside the ceiling and a cursor this service never issued
/// are 422s naming the field — deliberately not clamped or ignored, because a
/// quietly shortened page reads as "there was nothing more".
#[tokio::test]
async fn an_out_of_range_limit_and_a_malformed_cursor_are_field_named_422s() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(1), "2026-10-01")),
    )
    .await;

    for bad in ["1001", "0", "abc", "-1", ""] {
        let (s, body) = json_req(
            &app,
            "GET",
            &format!("/api/entries?date=2026-10-01&limit={bad}"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "limit={bad}: {body}");
        assert_eq!(body["error"]["code"], "validation_failed");
        assert_eq!(body["error"]["fields"][0]["field"], "limit", "limit={bad}");
    }

    for bad in ["nonsense", "MTIz"] {
        // "MTIz" is valid base64 that is still not a cursor this service issues:
        // it must not be read as "start over from the first page".
        let (s, body) = json_req(
            &app,
            "GET",
            &format!("/api/entries?date=2026-10-01&cursor={bad}"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "cursor={bad}: {body}");
        assert_eq!(body["error"]["code"], "validation_failed");
        assert_eq!(
            body["error"]["fields"][0]["field"], "cursor",
            "cursor={bad}"
        );
    }

    // The ceiling itself is accepted and still reports the set honestly.
    let (ok, body) = json_req(&app, "GET", "/api/entries?date=2026-10-01&limit=1000", None).await;
    assert_eq!(ok, StatusCode::OK, "{body}");
    assert_eq!(body["next_cursor"], Value::Null);

    // An unknown parameter stays tolerated (#100): paging did not make the
    // query strict.
    assert_eq!(
        json_req(
            &app,
            "GET",
            "/api/entries?date=2026-10-01&unrelated=1",
            None
        )
        .await
        .0,
        StatusCode::OK
    );

    // Paging does not relax the 400-day range cap (#262(8)).
    let (wide, body) = json_req(
        &app,
        "GET",
        "/api/entries?from=2020-01-01&to=2026-06-01&limit=10",
        None,
    )
    .await;
    assert_eq!(wide, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "bad_request");
}

/// #350: the entries API reads an archived (cold) day — `GET /api/entries?date=`
/// and `GET /api/entries/{id}` go through the same hot ∪ cold merge the store's
/// other readers use, and #51 visibility is unchanged by a relocation: a member's
/// own archived row is theirs, a sibling's archived row is not.
#[tokio::test]
async fn archived_days_read_through_the_entries_api() {
    use tucano_time::lock::NoLocks;
    use tucano_time::store::Store;

    let day = "2026-08-03";
    let cutoff = chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();

    let (app, d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;

    // The member's own row and the admin's, on the same day.
    let dana = member_cookie(&app, "dana@test.local", "danapass123").await;
    let (s_mine, mine, _) = raw(
        &app.router,
        "POST",
        "/api/entries",
        Some(entry_body(&cid, "P1", json!(4), day)),
        Some(&dana),
    )
    .await;
    assert_eq!(s_mine, StatusCode::CREATED, "{mine}");
    let mine_id = mine["id"].as_str().unwrap().to_string();
    let (s_theirs, theirs) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(&cid, "P1", json!(3), day)),
    )
    .await;
    assert_eq!(s_theirs, StatusCode::CREATED, "{theirs}");
    let theirs_id = theirs["id"].as_str().unwrap().to_string();

    // #349 relocates the whole day; everything below reads the cold copy.
    let store = Store::open(d.path().join("data")).expect("store");
    let report = store
        .archive_entries_before(cutoff, false, &NoLocks)
        .expect("the pass runs");
    assert_eq!(report.days.len(), 1, "{:?}", report.days);

    // By date: the admin sees both archived rows.
    let (s_day, day_list) = json_req(&app, "GET", &format!("/api/entries?date={day}"), None).await;
    assert_eq!(s_day, StatusCode::OK, "{day_list}");
    assert_eq!(
        day_list["entries"].as_array().unwrap().len(),
        2,
        "{day_list}"
    );

    // By id: the archived document is the entry, hours and date included.
    let (s_one, one) = json_req(&app, "GET", &format!("/api/entries/{theirs_id}"), None).await;
    assert_eq!(s_one, StatusCode::OK, "{one}");
    assert_eq!(one["hours"], json!(3));
    assert_eq!(one["date"], day);

    // #51 survives the relocation: the member's own archived row is theirs…
    let (s_own_day, own_day, _) = raw(
        &app.router,
        "GET",
        &format!("/api/entries?date={day}"),
        None,
        Some(&dana),
    )
    .await;
    assert_eq!(s_own_day, StatusCode::OK, "{own_day}");
    assert_eq!(own_day["entries"].as_array().unwrap().len(), 1, "{own_day}");
    assert_eq!(own_day["entries"][0]["id"], mine_id.as_str());
    // …and a sibling's is not, at either granularity — 404, not 403, so existence
    // is not leaked from the cold tree either.
    let (s_sibling, _, _) = raw(
        &app.router,
        "GET",
        &format!("/api/entries/{theirs_id}"),
        None,
        Some(&dana),
    )
    .await;
    assert_eq!(s_sibling, StatusCode::NOT_FOUND);

    // Negative: an id in neither tree is a 404, and a malformed date is refused
    // before any tree is read.
    let (s_missing, _) = json_req(
        &app,
        "GET",
        &format!("/api/entries/{}", uuid::Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(s_missing, StatusCode::NOT_FOUND);
    let (s_bad, body) = json_req(&app, "GET", "/api/entries?date=not-a-date", None).await;
    assert_eq!(s_bad, StatusCode::BAD_REQUEST, "{body}");
}
