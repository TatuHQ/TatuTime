use crate::support::*;

use axum::http::StatusCode;
use serde_json::json;

// --------------------------------------------------- RBAC / private-per-user --

#[tokio::test]
async fn member_sees_only_own_entries() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    // Admin logs one entry.
    let (_, admin_entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":2})),
    )
    .await;
    let admin_eid = admin_entry["id"].as_str().unwrap().to_string();

    // Create member Dana and log one entry as Dana.
    json_req(&app, "POST", "/api/users", Some(json!({"name":"Dana","email":"dana@test.local","password":"danapass123","role":"member"}))).await;
    let dana = login_cookie(&app.router, "dana@test.local", "danapass123").await;
    let (_, dana_entry, _) = raw(
        &app.router,
        "POST",
        "/api/entries",
        Some(json!({"date":"2026-10-02","customer_id":cid,"project_code":"P1","hours":3})),
        Some(&dana),
    )
    .await;
    let dana_eid = dana_entry["id"].as_str().unwrap().to_string();

    // Dana lists entries -> only her own.
    let (s, list, _) = raw(
        &app.router,
        "GET",
        "/api/entries?date=2026-10-02",
        None,
        Some(&dana),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let ids: Vec<&str> = list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![dana_eid.as_str()], "member sees only own entries");

    // Dana cannot read the admin's entry (404, not 403 — no existence leak).
    let (s404, _, _) = raw(
        &app.router,
        "GET",
        &format!("/api/entries/{admin_eid}"),
        None,
        Some(&dana),
    )
    .await;
    assert_eq!(s404, StatusCode::NOT_FOUND);

    // Admin sees both.
    let (_, admin_list) = json_req(&app, "GET", "/api/entries?date=2026-10-02", None).await;
    assert_eq!(admin_list["entries"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn member_cannot_invoice_or_approve() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    json_req(
        &app,
        "POST",
        "/api/users",
        Some(
            json!({"name":"Eve","email":"eve@test.local","password":"evepass123","role":"member"}),
        ),
    )
    .await;
    let eve = login_cookie(&app.router, "eve@test.local", "evepass123").await;

    let (s_inv, _, _) = raw(
        &app.router,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
        Some(&eve),
    )
    .await;
    assert_eq!(
        s_inv,
        StatusCode::FORBIDDEN,
        "member cannot create invoices"
    );
    let (s_users, _, _) = raw(&app.router, "GET", "/api/users", None, Some(&eve)).await;
    assert_eq!(s_users, StatusCode::FORBIDDEN, "member cannot list users");
    let (s_dec, _, _) = raw(
        &app.router,
        "POST",
        "/api/submissions/00000000-0000-0000-0000-000000000000/decision",
        Some(json!({"decision":"approve"})),
        Some(&eve),
    )
    .await;
    assert_eq!(s_dec, StatusCode::FORBIDDEN, "member cannot approve");
}

// ===========================================================================
// #262 — contract/unit gaps from the full-repo review.
//
// Tier 1: the dangerous, previously-dark paths (DELETE over HTTP, RBAC on
// non-owned *mutations*, the single-instance guard, the body-size cap, the
// range cap, and the README-visible operations no test ever hit). Tier 2:
// invalid-shape (422) coverage for endpoints that only had happy-path tests.
// Everything asserts rejection *without persistence* and the documented
// status/error shape — the bar AGENTS.md's test policy sets.
// ===========================================================================

// ---------------------------------------------------- Tier 1: dangerous paths --

/// #262(1): `DELETE /api/invoices/{id}` over HTTP — the path that was only
/// covered at the store level. Every invoice is deletable (204 → gone), and
/// deleting a non-draft invoice releases its entries; a member cannot reach the
/// admin-tier surface.
#[tokio::test]
async fn delete_invoice_over_http_is_admin_only_and_any_status() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({"rate_minor": 6000})).await;
    let (_, entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(3), "2026-10-02")),
    )
    .await;
    let eid = entry["id"].as_str().unwrap().to_string();

    // Draft: deletable.
    let (sc, draft) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{draft}");
    let did = draft["id"].as_str().unwrap().to_string();

    // A member never reaches the admin tier -> 403, and the draft survives.
    let member = member_cookie(&app, "dana@test.local", "danapass123").await;
    let (s_forbidden, _, _) = raw(
        &app.router,
        "DELETE",
        &format!("/api/invoices/{did}"),
        None,
        Some(&member),
    )
    .await;
    assert_eq!(s_forbidden, StatusCode::FORBIDDEN);
    assert_eq!(
        json_req(&app, "GET", &format!("/api/invoices/{did}"), None)
            .await
            .0,
        StatusCode::OK,
        "refused delete must not remove the draft"
    );

    // Admin deletes the draft -> 204, then it is gone (404).
    let (s_del, _) = json_req(&app, "DELETE", &format!("/api/invoices/{did}"), None).await;
    assert_eq!(s_del, StatusCode::NO_CONTENT);
    assert_eq!(
        json_req(&app, "GET", &format!("/api/invoices/{did}"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // Issued: deletable too, and the entry it locked becomes editable again.
    let (sc2, draft2) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    assert_eq!(sc2, StatusCode::CREATED, "{draft2}");
    let iid = draft2["id"].as_str().unwrap().to_string();
    let (si, issued) = json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    assert_eq!(si, StatusCode::OK, "{issued}");
    let (s_locked, _) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(entry_body(cid, "P1", json!(4), "2026-10-02")),
    )
    .await;
    assert_eq!(
        s_locked,
        StatusCode::CONFLICT,
        "issued invoices lock entries"
    );
    let (s_deleted, body) = json_req(&app, "DELETE", &format!("/api/invoices/{iid}"), None).await;
    assert_eq!(s_deleted, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        json_req(&app, "GET", &format!("/api/invoices/{iid}"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (s_released, released) = json_req(
        &app,
        "PUT",
        &format!("/api/entries/{eid}"),
        Some(entry_body(cid, "P1", json!(4), "2026-10-02")),
    )
    .await;
    assert_eq!(
        s_released,
        StatusCode::OK,
        "deleting the invoice releases its entries: {released}"
    );
}

/// #262(2): the happy-path `DELETE /api/entries/{id}` (204) — the timesheet's
/// primary correction path, previously proven only by its 409/503 refusals.
#[tokio::test]
async fn delete_entry_over_http_returns_204_and_removes_it() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (sc, entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(2), "2026-10-02")),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{entry}");
    let eid = entry["id"].as_str().unwrap().to_string();

    let (s, _, _) = raw_req(&app, "DELETE", &format!("/api/entries/{eid}")).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        json_req(&app, "GET", &format!("/api/entries/{eid}"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, list) = json_req(&app, "GET", "/api/entries?date=2026-10-02", None).await;
    assert!(
        list["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["id"] != entry["id"]),
        "deleted entry must leave the day list"
    );
}

/// #262(3): RBAC on non-owned **mutations** — a member's PUT/DELETE of another
/// user's entry or expense must be 404, never 403 (no existence leak, #51), and
/// the refused mutations must leave storage untouched.
#[tokio::test]
async fn non_owner_member_mutations_are_404_not_403() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    // The admin owns this entry and this expense.
    let (_, admin_entry) = json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(2), "2026-10-02")),
    )
    .await;
    let admin_eid = admin_entry["id"].as_str().unwrap().to_string();
    let (sc, cat) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Travel"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cat}");
    let (se, expense) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"category_id":cat["id"],"amount_minor":1000,"currency":"EUR"})),
    )
    .await;
    assert_eq!(se, StatusCode::CREATED, "{expense}");
    let exp_id = expense["id"].as_str().unwrap().to_string();

    let member = member_cookie(&app, "dana@test.local", "danapass123").await;

    let (s_put, _, _) = raw(
        &app.router,
        "PUT",
        &format!("/api/entries/{admin_eid}"),
        Some(entry_body(cid, "P1", json!(5), "2026-10-02")),
        Some(&member),
    )
    .await;
    assert_eq!(
        s_put,
        StatusCode::NOT_FOUND,
        "non-owner edit is 404, not 403"
    );
    let (s_del, _, _) = raw(
        &app.router,
        "DELETE",
        &format!("/api/entries/{admin_eid}"),
        None,
        Some(&member),
    )
    .await;
    assert_eq!(
        s_del,
        StatusCode::NOT_FOUND,
        "non-owner delete is 404, not 403"
    );
    let (s_exp, _, _) = raw(
        &app.router,
        "DELETE",
        &format!("/api/expenses/{exp_id}"),
        None,
        Some(&member),
    )
    .await;
    assert_eq!(
        s_exp,
        StatusCode::NOT_FOUND,
        "non-owner expense delete is 404"
    );

    // Nothing was mutated by the refused writes.
    let (_, still) = json_req(&app, "GET", &format!("/api/entries/{admin_eid}"), None).await;
    assert_eq!(still["hours"], 2);
    let (_, exp_still) = json_req(&app, "GET", &format!("/api/expenses/{exp_id}"), None).await;
    assert_eq!(exp_still["id"], expense["id"]);
}

/// #262(6c): `GET /api/submissions` and `GET /api/claims` — the owner-scoped
/// `visible_to()` list filtering, untested until now.
#[tokio::test]
async fn submissions_and_claims_lists_filter_by_owner() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    // The admin submits a week (needs an own, unlocked entry).
    json_req(
        &app,
        "POST",
        "/api/entries",
        Some(entry_body(cid, "P1", json!(2), "2026-09-28")),
    )
    .await;
    let (ss, sub) = json_req(
        &app,
        "POST",
        "/api/submissions",
        Some(json!({"week_start":"2026-09-28"})),
    )
    .await;
    assert_eq!(ss, StatusCode::CREATED, "{sub}");

    // A member with none of their own sees an empty list.
    let member = member_cookie(&app, "dana@test.local", "danapass123").await;
    let (sm, member_list, _) =
        raw(&app.router, "GET", "/api/submissions", None, Some(&member)).await;
    assert_eq!(sm, StatusCode::OK);
    assert!(
        member_list["submissions"].as_array().unwrap().is_empty(),
        "a member must not see another user's submission"
    );

    // The admin (owner) sees exactly their own.
    let (sa, admin_list) = json_req(&app, "GET", "/api/submissions", None).await;
    assert_eq!(sa, StatusCode::OK);
    assert_eq!(admin_list["submissions"].as_array().unwrap().len(), 1);
    assert_eq!(admin_list["submissions"][0]["id"], sub["id"]);

    // Claims: an empty, owner-filtered read is a valid 200.
    let (sc, claims) = json_req(&app, "GET", "/api/claims", None).await;
    assert_eq!(sc, StatusCode::OK);
    assert!(claims["claims"].as_array().unwrap().is_empty());
}

/// #335: a member's entry list reads through the per-user side index. The
/// answer must be byte-for-byte what the old whole-collection scan produced for
/// the member — own rows only, in the documented `(date, id)` ascending order,
/// page boundaries intact — and losing the index on disk must not lose a row.
#[tokio::test]
async fn member_entries_read_the_index_and_survive_its_loss() {
    let (app, d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;

    // The admin logs two entries (one per day).
    for date in ["2026-10-02", "2026-10-03"] {
        let (s, b) = json_req(
            &app,
            "POST",
            "/api/entries",
            Some(entry_body(&cid, "P1", json!(1), date)),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }
    // Dana logs three, two of them on the first day.
    let dana = member_cookie(&app, "dana@test.local", "danapass123").await;
    for date in ["2026-10-02", "2026-10-02", "2026-10-03"] {
        let (s, b, _) = raw(
            &app.router,
            "POST",
            "/api/entries",
            Some(entry_body(&cid, "P1", json!(2), date)),
            Some(&dana),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }

    // Dana's day read: only her two rows, and only hers.
    let (s, day, _) = raw(
        &app.router,
        "GET",
        "/api/entries?date=2026-10-02",
        None,
        Some(&dana),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(day["entries"].as_array().unwrap().len(), 2);

    // Paging over the indexed path: limit=1 walks both rows, in order, with no
    // foreign row and a cursor that ends the set.
    let (s, p1, _) = raw(
        &app.router,
        "GET",
        "/api/entries?date=2026-10-02&limit=1",
        None,
        Some(&dana),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let first: Vec<String> = p1["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(first.len(), 1);
    let cursor = p1["next_cursor"].as_str().unwrap().to_string();
    let (s, p2, _) = raw(
        &app.router,
        "GET",
        &format!("/api/entries?date=2026-10-02&limit=1&cursor={cursor}"),
        None,
        Some(&dana),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let second: Vec<String> = p2["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(second.len(), 1);
    assert!(p2["next_cursor"].is_null(), "the set ended after two rows");
    assert_ne!(first[0], second[0]);

    // Lose the index on disk: the member read falls back to the scan, stays
    // complete, and every foreign row stays hidden.
    std::fs::remove_file(d.path().join("data/entries/.idx.users.json")).unwrap();
    let (s, range, _) = raw(
        &app.router,
        "GET",
        "/api/entries?from=2026-10-02&to=2026-10-03",
        None,
        Some(&dana),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        range["entries"].as_array().unwrap().len(),
        3,
        "all three of Dana's rows survive the index loss"
    );

    // The admin still sees everyone (5 rows).
    let (_, admin) = json_req(
        &app,
        "GET",
        "/api/entries?from=2026-10-02&to=2026-10-03",
        None,
    )
    .await;
    assert_eq!(admin["entries"].as_array().unwrap().len(), 5);
}

/// #335: the expense list is indexed the same way — a member sees only their
/// own expenses, an admin sees all, and an index lost on disk changes nothing.
#[tokio::test]
async fn member_expenses_read_the_index_and_hide_others() {
    let (app, d) = app().await;
    let c = new_customer(&app, "TatuTime", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;

    let (s, admin_exp) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"amount_minor":100,"currency":"EUR"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{admin_exp}");

    let dana = member_cookie(&app, "dana@test.local", "danapass123").await;
    let (s, b, _) = raw(
        &app.router,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"amount_minor":250,"currency":"EUR"})),
        Some(&dana),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");

    // Dana sees only her own expense.
    let (s, mine, _) = raw(&app.router, "GET", "/api/expenses", None, Some(&dana)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(mine["expenses"].as_array().unwrap().len(), 1);

    // Lose the index: the answer is unchanged (the scan answers and heals).
    std::fs::remove_file(d.path().join("data/expenses/.idx.users.json")).unwrap();
    let (s, mine, _) = raw(&app.router, "GET", "/api/expenses", None, Some(&dana)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(mine["expenses"].as_array().unwrap().len(), 1);

    // The admin sees both.
    let (_, all) = json_req(&app, "GET", "/api/expenses", None).await;
    assert_eq!(all["expenses"].as_array().unwrap().len(), 2);
}
