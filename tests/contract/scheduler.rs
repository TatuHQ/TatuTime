use crate::support::*;

use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use tatu_time::store::Store;

#[tokio::test]
async fn notifications_endpoint_empty_then_read() {
    let (app, _d) = app().await;
    let (s, body) = json_req(&app, "GET", "/api/notifications", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["notifications"].as_array().unwrap().len(), 0);
    assert_eq!(body["unread"], 0);
    let (s2, _) = json_req(&app, "POST", "/api/notifications/read", None).await;
    assert_eq!(s2, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn notifications_hide_stale_and_resolved_no_time_reminders() {
    let (app, dir) = app().await;
    let store = Store::open(dir.path().join("data")).unwrap();
    let user = store
        .list_users()
        .unwrap()
        .into_iter()
        .find(|user| user.email == ADMIN_EMAIL)
        .unwrap();
    let now = chrono::Utc::now();
    for (kind, created_at) in [
        ("no_time_today", now - chrono::Duration::days(1)),
        ("no_time_today", now),
        ("submit_timesheet", now),
    ] {
        store
            .push_notification(
                user.id,
                &tatu_time::domain::Notification {
                    id: uuid::Uuid::new_v4(),
                    kind: kind.to_string(),
                    title: kind.to_string(),
                    body: String::new(),
                    created_at,
                    read: false,
                },
            )
            .unwrap();
    }
    let (status, before) = json_req(&app, "GET", "/api/notifications", None).await;
    assert_eq!(status, StatusCode::OK);
    // #16's Approvals view is hidden, so the stored submission nudge is not
    // listed; only today's unresolved no-time reminder remains.
    assert_eq!(before["notifications"].as_array().unwrap().len(), 1);
    assert_eq!(before["notifications"][0]["kind"], "no_time_today");
    assert_eq!(before["unread"], 1);
    let customer = new_customer(&app, "Reminder test", "EUR", 6000).await;
    let cid = customer["id"].as_str().unwrap();
    new_project(&app, cid, "REMINDER", json!({})).await;
    let (created, _) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({
            "date": now.date_naive().to_string(),
            "customer_id": cid,
            "project_code": "REMINDER",
            "hours": 1,
        })),
    )
    .await;
    assert_eq!(created, StatusCode::CREATED);
    let (_, after) = json_req(&app, "GET", "/api/notifications", None).await;
    let notifications = after["notifications"].as_array().unwrap();
    assert!(notifications.is_empty(), "{notifications:?}");
    assert_eq!(after["unread"], 0);
}

#[tokio::test]
async fn schedule_crud_admin_only() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (s, sched) = json_req(&app, "POST", "/api/schedules", Some(json!({"customer_id":cid,"cadence":"monthly","mode":"retainer","retainer_amount_minor":150000,"currency":"EUR"}))).await;
    assert_eq!(s, StatusCode::CREATED, "{sched}");
    let sid = sched["id"].as_str().unwrap().to_string();
    let (sl, list) = json_req(&app, "GET", "/api/schedules", None).await;
    assert_eq!(sl, StatusCode::OK);
    assert_eq!(list["schedules"].as_array().unwrap().len(), 1);
    // Retainer with 0 amount rejected.
    let (sz, _) = json_req(&app, "POST", "/api/schedules", Some(json!({"customer_id":cid,"cadence":"monthly","mode":"retainer","retainer_amount_minor":0,"currency":"EUR"}))).await;
    assert_eq!(sz, StatusCode::UNPROCESSABLE_ENTITY);
    // Member forbidden (admin tier).
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"N","email":"n@t.local","password":"memberpass1","role":"member"})),
    )
    .await;
    let member = login_cookie(&app.router, "n@t.local", "memberpass1").await;
    let (sm, _, _) = raw(&app.router, "GET", "/api/schedules", None, Some(&member)).await;
    assert_eq!(sm, StatusCode::FORBIDDEN);
    // Delete.
    assert_eq!(
        json_req(&app, "DELETE", &format!("/api/schedules/{sid}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}

// ------------------------------------------------------------------ #136 ---

#[tokio::test]
async fn recurring_schedule_pause_resume_keeps_the_cursor() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (s, scd) = json_req(
        &app,
        "POST",
        "/api/schedules",
        Some(json!({"customer_id":cid,"cadence":"monthly","mode":"time","currency":"EUR"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{scd}");
    let sid = scd["id"].as_str().unwrap().to_string();
    // Pause: active=false, identity preserved.
    let (sp, paused) = json_req(
        &app,
        "PUT",
        &format!("/api/schedules/{sid}"),
        Some(json!({"active": false})),
    )
    .await;
    assert_eq!(sp, StatusCode::OK, "{paused}");
    assert_eq!(paused["active"], false);
    assert_eq!(paused["customer_id"], cid);
    assert_eq!(paused["cadence"], "monthly");
    // Resume via re-read.
    let (_sr, list) = json_req(&app, "GET", "/api/schedules", None).await;
    assert_eq!(list["schedules"].as_array().unwrap().len(), 1);
    let (sa, act) = json_req(
        &app,
        "PUT",
        &format!("/api/schedules/{sid}"),
        Some(json!({"active": true})),
    )
    .await;
    assert_eq!(sa, StatusCode::OK, "{act}");
    assert_eq!(act["active"], true);
    // Unknown schedule: 404; unknown field: 422 without change.
    let (s4, _) = json_req(
        &app,
        "PUT",
        "/api/schedules/00000000-0000-0000-0000-000000000000",
        Some(json!({"active": false})),
    )
    .await;
    assert_eq!(s4, StatusCode::NOT_FOUND);
    let (s5, _) = json_req(
        &app,
        "PUT",
        &format!("/api/schedules/{sid}"),
        Some(json!({"active": false, "cadence": "weekly"})),
    )
    .await;
    assert_eq!(s5, StatusCode::UNPROCESSABLE_ENTITY);
    let (_s6, still) = json_req(&app, "GET", "/api/schedules", None).await;
    assert_eq!(
        still["schedules"][0]["active"], true,
        "rejection persisted nothing"
    );
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
    let (s7, _, _) = raw(
        &app.router,
        "PUT",
        &format!("/api/schedules/{sid}"),
        Some(json!({"active": false})),
        Some(&cookie),
    )
    .await;
    assert_eq!(s7, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn recurring_never_rebills_settled_or_written_off_entries() {
    // #248: the recurring scheduler must share the API's "already invoiced"
    // predicate. A settled (`paid` / `partly_paid`) or written-off invoice's
    // entries are released from the edit lock but must never be re-billed,
    // while a draft's lines stay billable.
    use tatu_time::recurring::RecurringJob;
    use tatu_time::scheduler::Job;

    // Run the daily recurring job once against the router's data dir. `now`
    // fixes the weekly period to 2026-10-01..2026-10-07 (the seeded week).
    let run_recurring = |dir: &std::path::Path| {
        let store = Store::open(dir.join("data")).expect("store");
        RecurringJob::new(Arc::new(store)).run("2026-10-07T12:00:00Z".parse().unwrap());
    };
    let invoice_count = |list: &Value| list["invoices"].as_array().unwrap().len();

    // Every way of closing an invoice must keep its lines out of a new bill.
    for (label, settle) in [
        ("paid", 0u64),
        ("partly_paid", 1000u64),
        ("written_off", 0u64),
    ] {
        let (app, dir) = app().await;
        let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
        let cid = c["id"].as_str().unwrap().to_string();
        new_project(&app, &cid, "P1", json!({"rate_minor": 6000})).await;
        json_req(
            &app,
            "POST",
            "/api/entries",
            Some(json!({"date":"2026-10-02","customer_id":cid.clone(),"project_code":"P1","hours":2})),
        )
        .await;
        // Bill week 1, then close the invoice the named way.
        let (_s, inv) = json_req(
            &app,
            "POST",
            "/api/invoices",
            Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-07"})),
        )
        .await;
        let iid = inv["id"].as_str().unwrap();
        json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
        if label == "written_off" {
            let (sw, wo) = json_req(
                &app,
                "POST",
                &format!("/api/invoices/{iid}/write-off"),
                Some(json!({"reason":"forgiven"})),
            )
            .await;
            assert_eq!(sw, StatusCode::OK, "{label}: {wo}");
        } else {
            let pay = if label == "paid" {
                json!({"reference":"x"})
            } else {
                json!({"reference":"x","amount_minor": settle})
            };
            let (sp, paid) =
                json_req(&app, "POST", &format!("/api/invoices/{iid}/pay"), Some(pay)).await;
            assert_eq!(sp, StatusCode::OK, "{label}: {paid}");
        }
        // A weekly time schedule whose next period covers the settled week.
        let (ss, sched) = json_req(
            &app,
            "POST",
            "/api/schedules",
            Some(json!({"customer_id":cid.clone(),"cadence":"weekly","mode":"time","currency":"EUR"})),
        )
        .await;
        assert_eq!(ss, StatusCode::CREATED, "{label}: {sched}");
        run_recurring(dir.path());
        let (_sl, list) = json_req(&app, "GET", "/api/invoices", None).await;
        assert_eq!(
            invoice_count(&list),
            1,
            "{label}: the recurring job must not re-bill a settled entry: {list}"
        );
    }

    // (c) A DRAFT's lines stay billable: the same recurring run still bills.
    let (app, dir) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({"rate_minor": 6000})).await;
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid.clone(),"project_code":"P1","hours":2})),
    )
    .await;
    // Draft only, never issued: its lines are not excluded.
    json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid.clone(),"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    json_req(
        &app,
        "POST",
        "/api/schedules",
        Some(json!({"customer_id":cid.clone(),"cadence":"weekly","mode":"time","currency":"EUR"})),
    )
    .await;
    run_recurring(dir.path());
    let (_sl, list) = json_req(&app, "GET", "/api/invoices", None).await;
    assert_eq!(
        invoice_count(&list),
        2,
        "a draft's lines stay billable, so the recurring run adds an invoice: {list}"
    );
}
