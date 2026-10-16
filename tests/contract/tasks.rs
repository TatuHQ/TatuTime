use crate::support::*;

use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;

// ------------------------------------------------------------------ tasks --

async fn new_task(app: &Client, cid: &str, pcode: &str, code: &str, extra: Value) -> Value {
    let mut body = json!({"code": code});
    if let Value::Object(map) = extra {
        for (k, v) in map {
            body[k] = v;
        }
    }
    let (status, created) = json_req(
        app,
        "POST",
        &format!("/api/customers/{cid}/projects/{pcode}/tasks"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create task: {created}");
    created
}

#[tokio::test]
async fn task_crud_and_entry_reference() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    let t = new_task(&app, cid, "P1", "t-9", json!({"name": "Design"})).await;
    assert_eq!(t["code"], "T-9", "task code normalised");

    // Entry referencing the task.
    let body = json!({
        "date": "2026-10-02", "customer_id": cid, "project_code": "P1",
        "task_code": "T-9", "hours": 2
    });
    let (s, e) = json_req(&app, "POST", "/api/entries", Some(body)).await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    assert_eq!(e["task_code"], "T-9");

    // Task delete blocked while an entry references it.
    let (s_del, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/customers/{cid}/projects/P1/tasks/T-9"),
        None,
    )
    .await;
    assert_eq!(s_del, StatusCode::CONFLICT);

    // Project delete blocked while a task exists.
    let (s_pdel, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/customers/{cid}/projects/P1"),
        None,
    )
    .await;
    assert_eq!(s_pdel, StatusCode::CONFLICT);
}

#[tokio::test]
async fn entry_task_must_belong_to_project() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    new_project(&app, cid, "P2", json!({})).await;
    new_task(&app, cid, "P2", "T1", json!({})).await;
    // Reference P2's task under P1.
    let body = json!({
        "date": "2026-10-02", "customer_id": cid, "project_code": "P1",
        "task_code": "T1", "hours": 1
    });
    let (s, b) = json_req(&app, "POST", "/api/entries", Some(body)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        b["error"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["field"] == "task_code")
    );
}

#[tokio::test]
async fn reports_and_invoices_use_project_rates_for_legacy_tasks() {
    let (app, dir) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await; // 60/h default
    let cid = c["id"].as_str().unwrap();
    new_project(
        &app,
        cid,
        "P1",
        json!({"currency": "USD", "rate_minor": 7500}),
    )
    .await;
    let mut task = new_task(&app, cid, "P1", "PREM", json!({})).await;
    task["currency"] = json!("GBP");
    task["rate_minor"] = json!(9500);
    std::fs::write(
        dir.path()
            .join(format!("data/customers/{cid}/projects/P1/tasks/PREM.json")),
        serde_json::to_vec(&task).unwrap(),
    )
    .unwrap();

    let body = json!({
        "date": "2026-10-02", "customer_id": cid, "project_code": "P1",
        "task_code": "PREM", "hours": 2
    });
    json_req(&app, "POST", "/api/entries", Some(body)).await;
    // Same project, no task -> project rate.
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":1})),
    )
    .await;

    let (s, summary) = json_req(
        &app,
        "GET",
        "/api/reports/summary?from=2026-10-01&to=2026-10-07&group=customer",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(summary["rows"][0]["amount_minor"], 22500);
    assert_eq!(summary["rows"][0]["currency"], "USD");
    let (status, invoice) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({
            "customer_id": cid, "from": "2026-10-01", "to": "2026-10-07"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{invoice}");
    assert_eq!(invoice["total_minor"], 22500);
    assert_eq!(invoice["currency"], "USD");
    assert!(
        invoice["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|line| line["rate_minor"] == 7500)
    );
}

#[tokio::test]
async fn tasks_reject_billing_overrides_without_persistence() {
    let (app, _dir) = app().await;
    let customer = new_customer(&app, "Task rates", "EUR", 6000).await;
    let cid = customer["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let task = new_task(&app, cid, "P1", "T1", json!({"name": "Original"})).await;
    let base = format!("/api/customers/{cid}/projects/P1/tasks");
    for field in ["rate_minor", "currency"] {
        for method in ["POST", "PUT"] {
            let mut payload =
                json!({"code": if method == "POST" { "NEW" } else { "T1" }, "name": "Changed"});
            payload[field] = if field == "rate_minor" {
                json!(9500)
            } else {
                json!("GBP")
            };
            let path = if method == "POST" {
                base.clone()
            } else {
                format!("{base}/T1")
            };
            let (status, _) = json_req(&app, method, &path, Some(payload)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            let (_, saved) = json_req(&app, "GET", &format!("{base}/T1"), None).await;
            assert_eq!(saved, task);
            let (missing, _) = json_req(&app, "GET", &format!("{base}/NEW"), None).await;
            assert_eq!(missing, StatusCode::NOT_FOUND);
        }
    }
}

#[tokio::test]
async fn tasks_list_returns_the_projects_tasks() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    new_task(&app, cid, "P1", "T-1", json!({"name":"Design"})).await;

    let (s, list) = json_req(
        &app,
        "GET",
        &format!("/api/customers/{cid}/projects/P1/tasks"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let codes: Vec<&str> = list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["T-1"]);
}

/// #262(7): the task-tier update-happy-path (a rename) and the un-referencing
/// delete after the last entry is removed.
#[tokio::test]
async fn task_rename_then_delete_after_unreferencing() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "ACME", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    new_task(&app, cid, "P1", "T-1", json!({"name":"Design"})).await;

    // Rename: body code must match the path code; the name changes.
    let (s_up, updated) = json_req(
        &app,
        "PUT",
        &format!("/api/customers/{cid}/projects/P1/tasks/T-1"),
        Some(json!({"code":"T-1","name":"Design System"})),
    )
    .await;
    assert_eq!(s_up, StatusCode::OK, "{updated}");
    assert_eq!(updated["name"], "Design System");
    let (_, got) = json_req(
        &app,
        "GET",
        &format!("/api/customers/{cid}/projects/P1/tasks/T-1"),
        None,
    )
    .await;
    assert_eq!(got["name"], "Design System", "rename must persist");

    // Referenced by an entry -> delete is refused (409).
    let (_, entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","task_code":"T-1","hours":2})),
    )
    .await;
    let eid = entry["id"].as_str().unwrap().to_string();
    assert_eq!(
        json_req(
            &app,
            "DELETE",
            &format!("/api/customers/{cid}/projects/P1/tasks/T-1"),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );

    // Un-reference it, then the delete succeeds (204) and the task is gone.
    assert_eq!(
        json_req(&app, "DELETE", &format!("/api/entries/{eid}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (s_del, _) = json_req(
        &app,
        "DELETE",
        &format!("/api/customers/{cid}/projects/P1/tasks/T-1"),
        None,
    )
    .await;
    assert_eq!(s_del, StatusCode::NO_CONTENT);
    assert_eq!(
        json_req(
            &app,
            "GET",
            &format!("/api/customers/{cid}/projects/P1/tasks/T-1"),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
