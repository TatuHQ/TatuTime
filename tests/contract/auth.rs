use crate::support::*;

use axum::http::StatusCode;
use axum::http::header;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use tucano_time::api::AppState;
use tucano_time::auth::Session;
use tucano_time::store::Store;

// ------------------------------------------------------------------- auth --

#[tokio::test]
async fn protected_routes_require_auth() {
    let (app, _d) = app().await;
    // Unauthenticated -> 401.
    let (s, _) = anon_req(&app, "GET", "/api/customers", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Authenticated -> 200.
    let (s2, _) = json_req(&app, "GET", "/api/customers", None).await;
    assert_eq!(s2, StatusCode::OK);
}

#[tokio::test]
async fn bootstrap_only_works_once() {
    let (app, _d) = app().await;
    // A second bootstrap must be refused (an admin already exists).
    let (s, _) = anon_req(
        &app,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"X","email":"x@y.co","password":"whatever12"})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn login_rejects_bad_password_without_leaking() {
    let (app, _d) = app().await;
    let (s, body) = anon_req(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({"email": ADMIN_EMAIL, "password": "wrong-password"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "invalid_credentials");
    // Same generic code for an unknown email (no user enumeration).
    let (s2, b2) = anon_req(
        &app,
        "POST",
        "/api/auth/login",
        Some(json!({"email": "nobody@test.local", "password": "whatever12"})),
    )
    .await;
    assert_eq!(s2, StatusCode::UNAUTHORIZED);
    assert_eq!(b2["error"]["code"], "invalid_credentials");
}

#[tokio::test]
async fn me_returns_current_user() {
    let (app, _d) = app().await;
    let (s, me) = json_req(&app, "GET", "/api/auth/me", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["email"], ADMIN_EMAIL);
    assert_eq!(me["role"], "admin");
    assert!(
        me.get("password_hash").is_none(),
        "the hash must never be returned"
    );
}

/// A wrong verb on a real path answers the app's one error shape and says which
/// verbs it does take — axum's default is a bare 405 with an empty body, which
/// tells a client nothing (and hides a client talking to a stale server that
/// does not serve a route yet).
#[tokio::test]
async fn a_wrong_method_answers_the_error_shape_and_the_allow_header() {
    let (app, _d) = app().await;
    // The path exists (GET, POST); PATCH is not mounted on it.
    let (status, headers, body) = raw_req(&app, "PATCH", "/api/expenses").await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{body:?}");
    let json: Value = serde_json::from_slice(&body).expect("the app's error shape");
    assert_eq!(json["error"]["code"], "method_not_allowed", "{json}");
    let message = json["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("PATCH") && message.contains("GET") && message.contains("POST"),
        "the message names the verb and the accepted ones: {json}"
    );
    let allow = headers
        .get(header::ALLOW)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        allow.contains("GET") && allow.contains("POST"),
        "Allow lists what the path takes: {allow:?}"
    );

    // A templated path resolves to its own methods.
    let id = uuid::Uuid::new_v4();
    let (status, _, body) = raw_req(&app, "POST", &format!("/api/expenses/{id}")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{body:?}");
    let json: Value = serde_json::from_slice(&body).expect("the app's error shape");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("PUT"),
        "an edit path offers PUT: {json}"
    );

    // An unknown path stays a 404 in the API namespace: the fallback answers
    // method mismatches only, never invents paths.
    let (status, _, _) = raw_req(&app, "PATCH", "/api/not-a-route").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn member_cannot_manage_users() {
    let (app, _d) = app().await;
    // Admin creates a member.
    let (s, _) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Pam","email":"pam@test.local","password":"memberpass1","role":"member"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    // Member logs in and is forbidden from /users, but can use /customers.
    let cookie = login_cookie(&app.router, "pam@test.local", "memberpass1").await;
    let (s_users, _, _) = raw(&app.router, "GET", "/api/users", None, Some(&cookie)).await;
    assert_eq!(s_users, StatusCode::FORBIDDEN);
    let (s_cust, _, _) = raw(&app.router, "GET", "/api/customers", None, Some(&cookie)).await;
    assert_eq!(s_cust, StatusCode::OK);
}

#[tokio::test]
async fn admin_can_update_user_and_duplicate_email_is_rejected_without_persistence() {
    let (app, _d) = app().await;
    let (s, bob) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({
            "name":"Bob","email":"bob@test.local","password":"bobpass123","role":"member",
            "active":true,"default_rate_minor":4500,"cost_rate_minor":2200
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let bob_id = bob["id"].as_str().unwrap();

    let (s, updated) = json_req(
        &app,
        "PUT",
        &format!("/api/users/{bob_id}"),
        Some(json!({
            "name":"Bob Q","email":"bob@test.local","role":"admin","active":false,
            "default_rate_minor":5000,"cost_rate_minor":2400
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(updated["name"], "Bob Q");
    assert_eq!(updated["role"], "admin");
    assert_eq!(updated["active"], false);
    assert_eq!(updated["default_rate_minor"], 5000);
    assert_eq!(updated["cost_rate_minor"], 2400);

    let (s, _) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({
            "name":"Other","email":"BOB@test.local","password":"otherpass1","role":"member"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn member_cannot_update_user() {
    let (app, _d) = app().await;
    let (s, user) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Pam","email":"pam@test.local","password":"memberpass1","role":"member"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let cookie = login_cookie(&app.router, "pam@test.local", "memberpass1").await;
    let (s, _, _) = raw(
        &app.router,
        "PUT",
        &format!("/api/users/{}", user["id"].as_str().unwrap()),
        Some(json!({"name":"Changed"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn password_change_revokes_existing_sessions_and_accepts_new_password() {
    let (app, _d) = app().await;
    let (s, user) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({
            "name":"Pam",
            "email":"pam@test.local",
            "password":"memberpass1",
            "role":"member"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let old_cookie = login_cookie(&app.router, "pam@test.local", "memberpass1").await;
    let id = user["id"].as_str().unwrap();
    let (s, _) = json_req(
        &app,
        "PUT",
        &format!("/api/users/{id}/password"),
        Some(json!({ "password": "newmemberpass1" })),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (s_old, _, _) = raw(&app.router, "GET", "/api/auth/me", None, Some(&old_cookie)).await;
    assert_eq!(s_old, StatusCode::UNAUTHORIZED);

    let new_cookie = login_cookie(&app.router, "pam@test.local", "newmemberpass1").await;
    let (s_new, _, _) = raw(&app.router, "GET", "/api/auth/me", None, Some(&new_cookie)).await;
    assert_eq!(s_new, StatusCode::OK);
}

#[tokio::test]
async fn user_management_validation_and_authorization_persist_nothing() {
    let (app, dir) = app().await;
    let (_, user) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"QA","email":"qa@test.local","password":"qa-password1"})),
    )
    .await;
    let id = user["id"].as_str().unwrap();
    let path = format!("/api/users/{id}");
    let store = Store::open(dir.path().join("data")).unwrap();
    let uid = uuid::Uuid::parse_str(id).unwrap();
    let before = serde_json::to_value(store.get_user(uid).unwrap().unwrap()).unwrap();
    for invalid in [
        json!({"name":""}),
        json!({"email":"invalid"}),
        json!({"role":"owner"}),
        json!({"default_rate_minor":100000001}),
        json!({"cost_rate_minor":-1}),
        json!({"cost_rate_minor":1.5}),
        json!({"unexpected":true}),
    ] {
        let (status, body) = json_req(&app, "PUT", &path, Some(invalid.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body["error"].is_object());
        let mut create =
            json!({"name":"New","email":"new@test.local","password":"initial-password1"});
        create
            .as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        let (status, _) = json_req(&app, "POST", "/api/users", Some(create)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(store.list_users().unwrap().len(), 2);
        assert_eq!(
            serde_json::to_value(store.get_user(uid).unwrap().unwrap()).unwrap(),
            before
        );
    }
    for method in ["POST", "PUT"] {
        let path = if method == "POST" {
            "/api/users".to_string()
        } else {
            format!("/api/users/{id}/password")
        };
        let payload = if method == "POST" {
            json!({"name":"New","email":"new@test.local","password":"short"})
        } else {
            json!({"password":"short"})
        };
        let (status, _) = json_req(&app, method, &path, Some(payload)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    let cookie = login_cookie(&app.router, "qa@test.local", "qa-password1").await;
    for (method, path, payload) in [
        ("GET", "/api/users".to_string(), None),
        (
            "POST",
            "/api/users".to_string(),
            Some(json!({"name":"New","email":"new@test.local","password":"initial-password1"})),
        ),
        (
            "PUT",
            format!("/api/users/{id}"),
            Some(json!({"name":"Changed"})),
        ),
        (
            "PUT",
            format!("/api/users/{id}/password"),
            Some(json!({"password":"changed-password1"})),
        ),
        ("DELETE", format!("/api/users/{id}"), None),
    ] {
        let (status, _, _) = raw(&app.router, method, &path, payload, Some(&cookie)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        serde_json::to_value(store.get_user(uid).unwrap().unwrap()).unwrap(),
        before
    );
}

#[tokio::test]
async fn user_management_preserves_admin_history_and_revalidates_sessions() {
    let (app, dir) = app().await;
    let (_, me) = json_req(&app, "GET", "/api/auth/me", None).await;
    let admin_id = me["id"].as_str().unwrap();
    for body in [json!({"role":"member"}), json!({"active":false})] {
        let (status, _) =
            json_req(&app, "PUT", &format!("/api/users/{admin_id}"), Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
    let (status, _) = json_req(&app, "DELETE", &format!("/api/users/{admin_id}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, user) = json_req(
        &app,
        "POST",
        "/api/users",
        Some(json!({"name":"QA","email":"qa@test.local","password":"qa-password1"})),
    )
    .await;
    let id = user["id"].as_str().unwrap();
    let path = format!("/api/users/{id}");
    let cookie = login_cookie(&app.router, "qa@test.local", "qa-password1").await;
    let (status, _) = json_req(
        &app,
        "PUT",
        &path,
        Some(json!({"email":"renamed@test.local","role":"member","active":true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        raw(&app.router, "GET", "/api/auth/me", None, Some(&cookie))
            .await
            .0,
        StatusCode::OK
    );
    assert!(
        login_cookie(&app.router, "qa@test.local", "qa-password1")
            .await
            .is_empty()
    );
    let renamed_cookie = login_cookie(&app.router, "renamed@test.local", "qa-password1").await;
    assert!(!renamed_cookie.is_empty());
    let (status, _) = json_req(&app, "PUT", &path, Some(json!({"email":ADMIN_EMAIL}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    json_req(&app, "PUT", &path, Some(json!({"role":"admin"}))).await;
    assert_eq!(
        raw(
            &app.router,
            "GET",
            "/api/auth/me",
            None,
            Some(&renamed_cookie)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let admin_cookie = login_cookie(&app.router, "renamed@test.local", "qa-password1").await;
    json_req(&app, "PUT", &path, Some(json!({"active":false}))).await;
    assert_eq!(
        raw(
            &app.router,
            "GET",
            "/api/auth/me",
            None,
            Some(&admin_cookie)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    json_req(
        &app,
        "PUT",
        &path,
        Some(json!({"active":true,"role":"member"})),
    )
    .await;
    let cookie = login_cookie(&app.router, "renamed@test.local", "qa-password1").await;
    let customer = new_customer(&app, "History", "EUR", 6000).await;
    let cid = customer["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (timer_status, _, _) = raw(
        &app.router,
        "POST",
        "/api/timer",
        Some(json!({"customer_id":cid,"project_code":"P1"})),
        Some(&cookie),
    )
    .await;
    assert_eq!(timer_status, StatusCode::CREATED);
    assert_eq!(
        json_req(&app, "DELETE", &path, None).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        raw(&app.router, "DELETE", "/api/timer", None, Some(&cookie))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (status, _, _) = raw(
        &app.router,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-06","customer_id":cid,"project_code":"P1","hours":1})),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        json_req(&app, "DELETE", &path, None).await.0,
        StatusCode::CONFLICT
    );
    json_req(&app, "PUT", &path, Some(json!({"active":false}))).await;
    assert_eq!(
        json_req(&app, "DELETE", &path, None).await.0,
        StatusCode::CONFLICT
    );
    let (_, list) = json_req(&app, "GET", "/api/users", None).await;
    assert!(!list.to_string().contains("password"));
    let (_, disposable) = json_req(&app, "POST", "/api/users", Some(json!({"name":"Disposable","email":"disposable@test.local","password":"disposable-password1"}))).await;
    assert_eq!(
        json_req(
            &app,
            "DELETE",
            &format!("/api/users/{}", disposable["id"].as_str().unwrap()),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let store = Store::open(dir.path().join("data")).unwrap();
    let mut last_admin = store
        .get_user(uuid::Uuid::parse_str(admin_id).unwrap())
        .unwrap()
        .unwrap();
    let expected = last_admin.clone();
    last_admin.active = false;
    assert!(store.put_user_if_unchanged(&last_admin, &expected).is_err());
    assert!(store.delete_user(expected.id).is_err());
    let stale = store
        .get_user(uuid::Uuid::parse_str(id).unwrap())
        .unwrap()
        .unwrap();
    let mut updated = stale.clone();
    updated.name = "Concurrent edit".into();
    store.put_user_if_unchanged(&updated, &stale).unwrap();
    assert!(store.put_user_if_unchanged(&stale, &stale).is_err());
    let audit = std::fs::read_to_string(dir.path().join("data/audit.log")).unwrap();
    assert!(!audit.contains("qa-password1"));
}

#[tokio::test]
async fn login_rate_limited_after_repeated_failures() {
    let (client, _d) = app().await;
    let body = json!({"email": "attacker@test.local", "password": "wrong"});
    let mut last = StatusCode::UNAUTHORIZED;
    // 8 failures allowed (each 401); the 9th is locked out (429).
    for _ in 0..8 {
        let (s, _) = anon_req(&client, "POST", "/api/auth/login", Some(body.clone())).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        last = s;
    }
    let (s9, _) = anon_req(&client, "POST", "/api/auth/login", Some(body)).await;
    assert_eq!(
        s9,
        StatusCode::TOO_MANY_REQUESTS,
        "locked after threshold (last was {last:?})"
    );
}

#[tokio::test]
async fn audit_log_records_login_failure() {
    let (client, _d) = app().await;
    // A failed login (as an anonymous caller).
    let _ = anon_req(
        &client,
        "POST",
        "/api/auth/login",
        Some(json!({"email":"ghost@test.local","password":"wrong"})),
    )
    .await;
    // Admin reads the audit log and sees the event.
    let (s, body) = json_req(&client, "GET", "/api/audit", None).await;
    assert_eq!(s, StatusCode::OK);
    let events = body["events"].as_array().unwrap();
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "login_failed" && e["subject"] == "ghost@test.local"),
        "audit missing login_failed: {events:?}"
    );
    // No password material is ever recorded.
    let raw = serde_json::to_string(&events).unwrap();
    assert!(!raw.contains("wrong"), "audit leaked a password");
}

#[tokio::test]
async fn logout_revokes_the_session() {
    let (client, _d) = app().await;
    // Session works.
    let (s1, _) = json_req(&client, "GET", "/api/auth/me", None).await;
    assert_eq!(s1, StatusCode::OK);
    // Log out (CSRF header is sent by raw()).
    let (s2, _, _) = raw(
        &client.router,
        "POST",
        "/api/auth/logout",
        None,
        Some(&client.cookie),
    )
    .await;
    assert_eq!(s2, StatusCode::NO_CONTENT);
    // The same cookie is now revoked.
    let (s3, _) = json_req(&client, "GET", "/api/auth/me", None).await;
    assert_eq!(
        s3,
        StatusCode::UNAUTHORIZED,
        "revoked session must not work"
    );
}

#[tokio::test]
async fn revocation_is_visible_immediately_across_instances() {
    // #254 (review B6 freshness): authenticate must re-read `revoked.json` on
    // every request — the read now runs on the blocking pool, but it must not
    // become a stale per-request cache. A logout performed by a *separate*
    // instance sharing the data dir has to take effect on the very next request.
    let (a, dir) = app().await;
    let (s1, _) = json_req(&a, "GET", "/api/auth/me", None).await;
    assert_eq!(s1, StatusCode::OK, "session works before any revocation");

    // Second instance (its own in-memory revocations) sharing the data dir.
    let b = app_locked(dir.path(), Arc::new(tucano_time::lock::NoLocks)).await;
    let (s2, _, _) = raw(&b.router, "POST", "/api/auth/logout", None, Some(&a.cookie)).await;
    assert_eq!(s2, StatusCode::NO_CONTENT);

    // Instance A sees the revocation next request, through the offloaded read.
    let (s3, _) = json_req(&a, "GET", "/api/auth/me", None).await;
    assert_eq!(
        s3,
        StatusCode::UNAUTHORIZED,
        "a revocation written elsewhere must be honoured immediately"
    );
}

/// #262(6a): `GET /api/auth/status` — public, drives the first-run setup UI.
#[tokio::test]
async fn auth_status_is_public_and_reports_initialisation() {
    let (app, _d) = app().await;
    let (status, body) = anon_req(&app, "GET", "/api/auth/status", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["initialised"], true);
}

/// #262 Tier 2: the bootstrap endpoint enforces the weak-password rule through
/// HTTP (previously unit-tested only), on a store with no users yet.
#[tokio::test]
async fn bootstrap_weak_password_is_422_through_the_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let router = tucano_time::build_router(AppState::with_session(
        store,
        Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        )),
    ));
    let (s, body, _) = raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":"a@b.co","password":"short"})),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    // The refusal created no user: the store still reports uninitialised.
    let (_s2, st, _) = raw(&router, "GET", "/api/auth/status", None, None).await;
    assert_eq!(st["initialised"], false);
}

/// A file under `users/` that does not decode to a `User` must not make the
/// instance look initialised. It used to: `has_users` counted any file, so a
/// single corrupt document refused bootstrap as `initialised` while every
/// login scan failed with an opaque 500 — no way in at all.
#[tokio::test]
async fn an_undecodable_user_document_does_not_lock_the_instance_out() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    std::fs::create_dir_all(root.join("users")).unwrap();
    std::fs::write(root.join("users").join("broken.json"), b"not json").unwrap();
    let store = Store::open(&root).unwrap();
    let router = tucano_time::build_router(AppState::with_session(
        store,
        Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        )),
    ));

    // The corrupt document is skipped: the instance is uninitialised (200,
    // never a 500), so the GUI offers first-run setup instead of a login form
    // that cannot work.
    let (s, body, _) = raw(&router, "GET", "/api/auth/status", None, None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["initialised"], false, "{body}");

    // Bootstrap is therefore open and creates the first admin...
    let (s, body, cookie) = raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":"admin@example.com","password":"supersecret1"})),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    assert!(
        cookie.unwrap_or_default().contains("tt_session="),
        "bootstrap signs the new admin in"
    );

    // ...who can sign in while the corrupt file is still on disk.
    let (s, body, _) = raw(
        &router,
        "POST",
        "/api/auth/login",
        Some(json!({"email":"admin@example.com","password":"supersecret1"})),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["email"], "admin@example.com");

    // With a real admin present, status reports initialised and bootstrap is
    // refused again — the create-once guarantee still holds with the tolerant
    // predicate.
    let (s, body, _) = raw(&router, "GET", "/api/auth/status", None, None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["initialised"], true, "{body}");
    let (s, body, _) = raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"X","email":"x@y.co","password":"whatever12"})),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "initialised", "{body}");

    // The refusal persisted nothing: `broken.json` plus exactly one admin
    // document (dot files are side indexes, not accounts).
    let docs = std::fs::read_dir(root.join("users"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.ends_with(".json") && !name.starts_with('.')
        })
        .count();
    assert_eq!(
        docs, 2,
        "one broken file and exactly one admin were written"
    );
}
