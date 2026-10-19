use crate::support::*;

use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use tucano_time::store::Store;

/// Stub transport for accounting tests: returns deterministic ids, optionally
/// fails the first N posts to exercise the recorded-failure path.
struct StubTransport {
    posts: std::sync::Mutex<Vec<(String, Value)>>,
    fail_first: usize,
}

impl tucano_time::accounting::Transport for StubTransport {
    fn post(&self, path: &str, body: Value) -> Result<String, tucano_time::accounting::SyncError> {
        let mut posts = self.posts.lock().unwrap();
        posts.push((path.to_string(), body));
        if posts.len() <= self.fail_first {
            return Err(tucano_time::accounting::SyncError::Transport(
                "stub offline".into(),
            ));
        }
        Ok(format!("remote-{path}-{}", posts.len()))
    }
}

#[tokio::test]
async fn accounting_sync_idempotent_with_visible_status() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let t = Arc::new(StubTransport {
        posts: std::sync::Mutex::new(vec![]),
        fail_first: 0,
    });
    let qbo: Arc<dyn tucano_time::accounting::AccountingSync> =
        Arc::new(tucano_time::accounting::QboProvider::new(t.clone()));
    let registry = tucano_time::accounting::AccountingRegistry::new(vec![qbo]);
    let app = Client::with_accounting(store, registry).await;

    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
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

    // Draft -> 409.
    let (sd, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(sd, StatusCode::CONFLICT);

    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;

    // Unknown provider -> 400.
    let (su, _) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"sage"})),
    )
    .await;
    assert_eq!(su, StatusCode::BAD_REQUEST);

    // Sync -> synced record with a remote id; the doc went to "invoice".
    let (ss, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(ss, StatusCode::OK, "{body}");
    assert_eq!(body["record"]["status"], "synced");
    assert!(
        body["record"]["remote_id"]
            .as_str()
            .unwrap()
            .starts_with("remote-invoice")
    );

    // Idempotent: a second sync is a no-op (no duplicate post).
    let posts_before = t.posts.lock().unwrap().len();
    let (s2, b2) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["noop"], true);
    assert_eq!(t.posts.lock().unwrap().len(), posts_before);

    // Visible status lists the record + enabled providers.
    let (sv, status) = json_req(&app, "GET", "/api/sync/accounting", None).await;
    assert_eq!(sv, StatusCode::OK);
    assert_eq!(status["providers"].as_array().unwrap(), &vec![json!("qbo")]);
    assert_eq!(status["records"].as_array().unwrap().len(), 1);

    // Paying then syncing pushes invoice AND payment documents.
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"reference":"bank-transfer"})),
    )
    .await;
    let (_s3, b3) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    // Paid after sync -> the next sync pushes the PAYMENT against the remote
    // invoice (not a no-op), then later syncs are no-ops again.
    assert_eq!(b3["record"]["kind"], "payment", "{b3}");
    assert_eq!(b3["record"]["status"], "synced", "{b3}");
    {
        let posts = t.posts.lock().unwrap();
        assert_eq!(posts[posts.len() - 1].0, "payment");
    }
    let (_s4, b4) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(b4["noop"], true, "{b4}");
}

#[tokio::test]
async fn accounting_sync_failure_is_recorded_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let t = Arc::new(StubTransport {
        posts: std::sync::Mutex::new(vec![]),
        fail_first: 1, // first post fails, retry succeeds
    });
    let qbo: Arc<dyn tucano_time::accounting::AccountingSync> =
        Arc::new(tucano_time::accounting::QboProvider::new(t.clone()));
    let app = Client::with_accounting(
        store,
        tucano_time::accounting::AccountingRegistry::new(vec![qbo]),
    )
    .await;

    let c = new_customer(&app, "GLOBEX", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
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
    let iid = inv["id"].as_str().unwrap().to_string();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;

    // First sync fails at the transport -> 200 with a recorded failure.
    let (sf, body) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(sf, StatusCode::OK, "{body}");
    assert_eq!(body["failed"], true);
    assert_eq!(body["record"]["status"], "failed");
    // #190: the persisted/serialised error is a stable marker; the raw
    // transport text ("stub offline") stays in the server log only.
    assert_eq!(body["record"]["error"], "sync failed (see server log)");
    let (_sv, st) = json_req(&app, "GET", "/api/sync/accounting", None).await;
    assert!(
        !st.to_string().contains("stub offline"),
        "sync_status must not echo provider detail: {st}"
    );

    // Retry (second post succeeds) -> synced. The failed record is not
    // short-circuited (only synced records are).
    let (sr, body2) = json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/sync"),
        Some(json!({"provider":"qbo"})),
    )
    .await;
    assert_eq!(sr, StatusCode::OK);
    assert_eq!(body2["record"]["status"], "synced", "{body2}");
    assert!(
        body2["record"]["remote_id"]
            .as_str()
            .unwrap()
            .starts_with("remote-invoice")
    );
}
