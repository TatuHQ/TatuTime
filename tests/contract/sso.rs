use crate::support::*;

use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;
use tatu_time::api::AppState;
use tatu_time::auth::Session;
use tatu_time::store::Store;
use tower::ServiceExt;

#[tokio::test]
async fn sso_jit_provisions_and_logs_in() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let idp = tatu_time::sso::SignedTokenIdp::new(
        "okta-test",
        "stub-secret",
        vec!["tatutime.test".into()],
    );
    let registry = Arc::new(tatu_time::sso::SsoRegistry::new(vec![Arc::new(idp)]));
    let state = AppState::with_session(
        store,
        Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        )),
    )
    .with_sso(registry);
    let router = tatu_time::build_router(state);

    // Local admin exists FIRST — SSO runs alongside local login (#32 DoD).
    let (sa, _, _) = raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    assert_eq!(sa, StatusCode::CREATED);

    // Providers are advertised pre-session.
    let (s, body, _) = raw(&router, "GET", "/api/auth/sso/providers", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        body["providers"].as_array().unwrap(),
        &vec![json!("okta-test")]
    );

    let idp = tatu_time::sso::SignedTokenIdp::new(
        "okta-test",
        "stub-secret",
        vec!["tatutime.test".into()],
    );

    // A valid signed assertion JIT-creates the user (member) and logs in.
    let claims = tatu_time::sso::TokenClaims {
        sub: "u1".into(),
        email: "Newbie@tatutime.test".into(),
        name: "New Bie".into(),
        groups: vec![],
        exp: chrono::Utc::now().timestamp() + 300,
    };
    let token = idp.issue(&claims);
    let (s1, user, _) = raw(
        &router,
        "POST",
        "/api/auth/sso/assertion",
        Some(json!({"provider":"okta-test","payload":token,"signature":""})),
        None,
    )
    .await;
    assert_eq!(s1, StatusCode::OK, "{user}");
    assert_eq!(user["email"], "newbie@tatutime.test");
    assert_eq!(user["role"], "member");

    // The session cookie works like a local one.
    let cookie = {
        let (_s, _b, c) = raw(
            &router,
            "POST",
            "/api/auth/sso/assertion",
            Some(json!({"provider":"okta-test","payload":token,"signature":""})),
            None,
        )
        .await;
        c.unwrap_or_default()
    };
    let (sm, me, _) = raw(&router, "GET", "/api/auth/me", None, Some(&cookie)).await;
    assert_eq!(sm, StatusCode::OK, "{me}");
    assert_eq!(me["name"], "New Bie");

    // Bad signature -> 401, and no user created for it.
    let (sb, _, _) = raw(
        &router,
        "POST",
        "/api/auth/sso/assertion",
        Some(json!({"provider":"okta-test","payload":"aGVhZGVy.cGF5bG9hZC5mb3JnZWQ.bad","signature":""})),
        None,
    )
    .await;
    assert_eq!(sb, StatusCode::UNAUTHORIZED);

    // Local login still works alongside SSO.
    let (sl, _, _) = raw(
        &router,
        "POST",
        "/api/auth/login",
        Some(json!({"email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    assert_eq!(sl, StatusCode::OK);
}

#[tokio::test]
async fn sso_admin_group_setting_is_resolved_for_new_and_existing_users() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let idp = tatu_time::sso::SignedTokenIdp::new(
        "okta-test",
        "stub-secret",
        vec!["tatutime.test".into()],
    );
    std::fs::write(
        store.root().join("config.json"),
        r#"{"sso_admin_group":"tt-admins"}"#,
    )
    .unwrap();
    let registry = Arc::new(tatu_time::sso::SsoRegistry::new(vec![Arc::new(
        tatu_time::sso::SignedTokenIdp::new(
            "okta-test",
            "stub-secret",
            vec!["tatutime.test".into()],
        ),
    )]));
    let state = AppState::with_session(
        store,
        Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        )),
    )
    .with_sso(registry)
    .with_env(Arc::new(|_| None));
    let router = tatu_time::build_router(state);
    let claims = |groups: Vec<String>| tatu_time::sso::TokenClaims {
        sub: "u1".into(),
        email: "member@tatutime.test".into(),
        name: "Member".into(),
        groups,
        exp: chrono::Utc::now().timestamp() + 300,
    };

    // First login provisions a member; the stored group mapping promotes the
    // existing account on its next verified assertion.
    let first = idp.issue(&claims(vec![]));
    let (status, user, _) = raw(
        &router,
        "POST",
        "/api/auth/sso/assertion",
        Some(json!({"provider":"okta-test","payload":first,"signature":""})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{user}");
    assert_eq!(user["role"], "member");

    let second = idp.issue(&claims(vec!["tt-admins".into()]));
    let (status, user, _) = raw(
        &router,
        "POST",
        "/api/auth/sso/assertion",
        Some(json!({"provider":"okta-test","payload":second,"signature":""})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{user}");
    assert_eq!(user["role"], "admin");
}

#[tokio::test]
async fn sso_empty_allowed_domains_denies_and_provisions_nothing() {
    // #260: SSO with an empty allow-list must fail closed — the assertion is
    // rejected and no account is JIT-provisioned (the old allow-all is gone).
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let idp = tatu_time::sso::SignedTokenIdp::new("okta-test", "stub-secret", vec![]);
    let registry = Arc::new(tatu_time::sso::SsoRegistry::new(vec![Arc::new(idp)]));
    let state = AppState::with_session(
        store,
        Arc::new(Session::new(
            b"test-session-secret-0000000000000032".to_vec(),
            3600,
            false,
        )),
    )
    .with_sso(registry);
    let router = tatu_time::build_router(state);

    let (sa, _, _) = raw(
        &router,
        "POST",
        "/api/auth/bootstrap",
        Some(json!({"name":"Admin","email":ADMIN_EMAIL,"password":ADMIN_PW})),
        None,
    )
    .await;
    assert_eq!(sa, StatusCode::CREATED);
    let admin_cookie = login_cookie(&router, ADMIN_EMAIL, ADMIN_PW).await;

    // A correctly-signed assertion but an empty allow-list: rejected, never
    // provisioned.
    let idp = tatu_time::sso::SignedTokenIdp::new("okta-test", "stub-secret", vec![]);
    let claims = tatu_time::sso::TokenClaims {
        sub: "u1".into(),
        email: "Intruder@tatutime.test".into(),
        name: "Intruder".into(),
        groups: vec![],
        exp: chrono::Utc::now().timestamp() + 300,
    };
    let token = idp.issue(&claims);
    let (s1, body, _) = raw(
        &router,
        "POST",
        "/api/auth/sso/assertion",
        Some(json!({"provider":"okta-test","payload":token,"signature":""})),
        None,
    )
    .await;
    assert_eq!(s1, StatusCode::BAD_REQUEST, "{body}");

    // Only the bootstrap admin exists: nothing was provisioned for the domain.
    let (su, users, _) = raw(&router, "GET", "/api/users", None, Some(&admin_cookie)).await;
    assert_eq!(su, StatusCode::OK, "{users}");
    assert_eq!(users["users"].as_array().map(Vec::len), Some(1), "{users}");
}

#[tokio::test]
async fn sso_unknown_provider_is_400() {
    let (app, _d) = app().await;
    let (s, _) = json_req(
        &app,
        "POST",
        "/api/auth/sso/assertion",
        Some(json!({"provider":"nope","payload":"x","signature":""})),
    )
    .await;
    // app() client is authenticated; endpoint is public but provider unknown.
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sso_assertion_is_csrf_exempt_pre_session() {
    // Review A6: a real IdP POST-binding cannot send X-CSRF-Protection; the
    // route must reach the handler (400 unknown provider), not 403 csrf.
    let (app, _d) = app().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/auth/sso/assertion")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"provider":"nope","payload":"x"}).to_string(),
        ))
        .unwrap();
    let res = app.router.clone().oneshot(req).await.expect("oneshot");
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
