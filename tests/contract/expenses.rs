use crate::support::*;

use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;

// ---------------------------------------------------------------- expenses --

#[tokio::test]
async fn category_and_expense_crud_with_references() {
    let (app, _d) = app().await;
    let (s0, defaults) = json_req(&app, "GET", "/api/categories", None).await;
    assert_eq!(s0, StatusCode::OK);
    let default_names = defaults["categories"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|category| category["name"].as_str())
        .collect::<Vec<_>>();
    for name in ["Food", "Travel", "Lodging", "Supplies", "Software"] {
        assert!(
            default_names.contains(&name),
            "missing default category {name}"
        );
    }
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;

    // Not a seeded default: "Travel" already exists from the list call, and
    // duplicate names are now a 409 (SET-12).
    let (sc, cat) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Conference"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED);
    let cat_id = cat["id"].as_str().unwrap();

    let (su, updated) = json_req(
        &app,
        "PUT",
        &format!("/api/categories/{cat_id}"),
        Some(json!({"name":"Work travel","default_billable":true,"active":true})),
    )
    .await;
    assert_eq!(su, StatusCode::OK);
    assert_eq!(updated["name"], "Work travel");

    let (se, exp) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"project_code":"P1","category_id":cat_id,
            "amount_minor":12500,"currency":"EUR","note":"flight"
        })),
    )
    .await;
    assert_eq!(se, StatusCode::CREATED, "{exp}");
    assert_eq!(exp["billable"], true); // default

    // Category can't be deleted while an expense references it.
    let (sd, _) = json_req(&app, "DELETE", &format!("/api/categories/{cat_id}"), None).await;
    assert_eq!(sd, StatusCode::CONFLICT);

    // Filter expenses by customer.
    let (sf, list) = json_req(
        &app,
        "GET",
        &format!("/api/expenses?customer_id={cid}"),
        None,
    )
    .await;
    assert_eq!(sf, StatusCode::OK);
    assert_eq!(list["expenses"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn expense_rejects_unknown_project_and_category() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (s, body) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"project_code":"NOPE",
            "category_id":"00000000-0000-0000-0000-000000000000","amount_minor":100,"currency":"EUR"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let fields: Vec<&str> = body["error"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["field"].as_str().unwrap())
        .collect();
    assert!(fields.contains(&"project_code"));
    assert!(fields.contains(&"category_id"));
}

#[tokio::test]
async fn expense_receipt_roundtrips() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (_, e) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":500,"currency":"EUR",
            "receipt_name":"hotel.png","receipt_b64":"aGVsbG8="
        })),
    )
    .await;
    let eid = e["id"].as_str().unwrap();
    let (_, got) = json_req(&app, "GET", &format!("/api/expenses/{eid}"), None).await;
    assert_eq!(got["receipt_name"], "hotel.png");
    assert_eq!(got["receipt_b64"], "aGVsbG8=");
}

/// A receipt rides inline (#23), so the transport cap has to clear the field
/// cap: a scanned page is megabytes of base64, and while the body limit sat
/// below the field rule it answered `413 length limit exceeded` before any
/// handler saw the payload — the receipt could never be stored at all. This
/// pins both rules: what the dialog promises (up to ~3 MB of file) is accepted
/// whole, and what the API refuses comes back as the app's own 422 naming
/// `receipt_b64`, never as a bare 413 from the transport.
#[tokio::test]
async fn a_megabyte_receipt_is_stored_and_an_over_long_one_is_the_apps_own_422() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    // ~1 MB of base64: the order of magnitude of a scanned receipt.
    let stored = "A".repeat(1_000_000);
    let (s, e) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":500,"currency":"EUR",
            "receipt_name":"scan.pdf","receipt_b64":stored
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    let eid = e["id"].as_str().unwrap();
    let (_, got) = json_req(&app, "GET", &format!("/api/expenses/{eid}"), None).await;
    assert_eq!(
        got["receipt_b64"].as_str().map(str::len),
        Some(1_000_000),
        "the bytes travel whole"
    );

    // Past the field rule the answer is the app's error shape, and nothing is
    // written: the caller learns which field was wrong, not that a body limit
    // tripped somewhere in front of the router.
    let too_big = "A".repeat(4_000_001);
    let (s, body) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":500,"currency":"EUR",
            "receipt_name":"huge.pdf","receipt_b64":too_big
        })),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["fields"][0]["field"], "receipt_b64");
    assert_eq!(body["error"]["fields"][0]["message"], "receipt too large");
}

/// A converted expense keeps the amount as entered, its currency and the rate
/// that produced the billed pair, and the billed pair stays authoritative.
#[tokio::test]
async fn expense_records_the_conversion_it_was_entered_with() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    // USD 100.00 spent, billed as EUR 92.06 at the published 2026-10-01 rate.
    // The rate is padded and carries trailing zeros on purpose: what is stored
    // must be the digits the caller wrote, normalised — never a float's
    // rendering of them (#11).
    let (s, e) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":9206,"currency":"EUR",
            "original_amount_minor":10000,"original_currency":"USD",
            "exchange_rate":" 0.920600 ","exchange_rate_date":"2026-10-01",
            "note":"conference fee"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    assert_eq!(
        e["amount_minor"], 9206,
        "the billed amount is authoritative"
    );
    assert_eq!(e["currency"], "EUR");
    assert_eq!(e["original_amount_minor"], 10000);
    assert_eq!(e["original_currency"], "USD");
    assert_eq!(e["exchange_rate"], "0.9206");
    assert_eq!(e["exchange_rate_date"], "2026-10-01");

    // The provenance survives the document round-trip, on both read paths.
    let eid = e["id"].as_str().unwrap();
    let (_, got) = json_req(&app, "GET", &format!("/api/expenses/{eid}"), None).await;
    assert_eq!(got["exchange_rate"], "0.9206");
    assert_eq!(got["exchange_rate_date"], "2026-10-01");
    let (_, list) = json_req(&app, "GET", "/api/expenses", None).await;
    assert_eq!(list["expenses"][0]["original_currency"], "USD");

    // An expense that was never converted carries none of the four keys, so a
    // reader can tell "billed as entered" from "converted" without guessing.
    let (sp, plain) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-03","customer_id":cid,"amount_minor":500,"currency":"EUR"})),
    )
    .await;
    assert_eq!(sp, StatusCode::CREATED, "{plain}");
    for key in [
        "original_amount_minor",
        "original_currency",
        "exchange_rate",
        "exchange_rate_date",
    ] {
        assert!(
            plain.get(key).is_none(),
            "an unconverted expense must not carry {key}: {plain}"
        );
    }
}

/// The conversion group is all-or-nothing, a rate is a positive six-decimal
/// number (never a float), and a rejected payload persists nothing.
#[tokio::test]
async fn expense_conversion_is_all_or_nothing_and_never_a_float() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    // The billed pair is EUR; the amount was entered in USD.
    let full = || {
        json!({
            "date":"2026-10-02","customer_id":cid,"amount_minor":9206,"currency":"EUR",
            "original_amount_minor":10000,"original_currency":"USD",
            "exchange_rate":"0.9206","exchange_rate_date":"2026-10-01"
        })
    };
    let without = |keys: &[&str]| {
        let mut body = full();
        for key in keys {
            body.as_object_mut().unwrap().remove(*key);
        }
        body
    };
    let (s, ok) = json_req(&app, "POST", "/api/expenses", Some(full())).await;
    assert_eq!(s, StatusCode::CREATED, "{ok}");

    let mut cases: Vec<(Value, &str, &str)> = vec![
        (
            without(&["original_amount_minor"]),
            "original_amount_minor",
            "the amount as entered is missing",
        ),
        (
            without(&["original_currency"]),
            "original_currency",
            "the entered currency is missing",
        ),
        (
            without(&["exchange_rate"]),
            "exchange_rate",
            "the applied rate is missing",
        ),
    ];
    // No sign, no exponent, no thousands separator, no seventh decimal, no
    // magnitude beyond the documented ceiling, and no zero.
    for bad in [
        "0",
        "0.0",
        "-1",
        "+1",
        "1e3",
        "1.2345678",
        "1000001",
        "abc",
        "1,5",
    ] {
        let mut body = full();
        body["exchange_rate"] = json!(bad);
        cases.push((
            body,
            "exchange_rate",
            "a rate that is not positive and 6-decimal",
        ));
    }
    let mut same_currency = full();
    same_currency["original_currency"] = json!("EUR");
    cases.push((
        same_currency,
        "original_currency",
        "the billed currency converted with itself",
    ));
    let mut bad_date = full();
    bad_date["exchange_rate_date"] = json!("yesterday");
    cases.push((
        bad_date,
        "exchange_rate_date",
        "a rate date that is not a date",
    ));
    let mut huge = full();
    huge["original_amount_minor"] = json!(100_000_001);
    cases.push((
        huge,
        "original_amount_minor",
        "an original amount above the billed ceiling",
    ));

    for (body, field, why) in cases {
        let (status, err) = json_req(&app, "POST", "/api/expenses", Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}: {err}");
        let fields: Vec<&str> = err["error"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["field"].as_str().unwrap())
            .collect();
        assert!(
            fields.contains(&field),
            "{why}: expected {field} in {fields:?}"
        );
        // Rejection without persistence is part of the contract.
        let (_, list) = json_req(&app, "GET", "/api/expenses", None).await;
        assert_eq!(
            list["expenses"].as_array().unwrap().len(),
            1,
            "{why}: the refused payload must not be persisted"
        );
    }
}

/// An edit replaces the record: new values land, the identity and history stay,
/// and a field the payload omits is cleared — the same PUT semantics the entries
/// endpoint documents.
#[tokio::test]
async fn expense_update_replaces_the_record_and_keeps_its_identity() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    new_project(&app, &cid, "P2", json!({})).await;
    let (s, created) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"project_code":"P1","amount_minor":9206,
            "currency":"EUR","original_amount_minor":10000,"original_currency":"USD",
            "exchange_rate":"0.9206","exchange_rate_date":"2026-10-01","note":"conference fee"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_string();

    let (su, updated) = json_req(
        &app,
        "PUT",
        &format!("/api/expenses/{id}"),
        Some(json!({
            "date":"2026-10-09","customer_id":cid,"project_code":"P2","amount_minor":4200,
            "currency":"EUR","billable":false,"note":"taxi"
        })),
    )
    .await;
    assert_eq!(su, StatusCode::OK, "{updated}");
    assert_eq!(updated["id"], created["id"], "the id survives the edit");
    assert_eq!(
        updated["created_at"], created["created_at"],
        "creation history is kept"
    );
    assert_eq!(
        updated["user_id"], created["user_id"],
        "an edit never rewrites the author (#51)"
    );
    assert_eq!(updated["date"], "2026-10-09");
    assert_eq!(updated["project_code"], "P2");
    assert_eq!(updated["amount_minor"], 4200);
    assert_eq!(updated["billable"], false);
    assert_eq!(updated["note"], "taxi");
    assert!(
        updated["original_currency"].is_null() && updated["exchange_rate"].is_null(),
        "a conversion the payload omits is cleared by a full replacement: {updated}"
    );

    // The stored document agrees, on both read paths.
    let (_, got) = json_req(&app, "GET", &format!("/api/expenses/{id}"), None).await;
    assert_eq!(got["note"], "taxi");
    assert_eq!(got["project_code"], "P2");
    assert!(got["original_amount_minor"].is_null());
    let (_, list) = json_req(&app, "GET", "/api/expenses", None).await;
    assert_eq!(list["expenses"][0]["note"], "taxi");

    // The receipt is part of the record too: an edit replaces or clears it like
    // any other field (#23).
    let (sr, with_receipt) = json_req(
        &app,
        "PUT",
        &format!("/api/expenses/{id}"),
        Some(json!({
            "date":"2026-10-09","customer_id":cid,"amount_minor":4200,"currency":"EUR",
            "receipt_name":"taxi.png","receipt_b64":"aGVsbG8="
        })),
    )
    .await;
    assert_eq!(sr, StatusCode::OK, "{with_receipt}");
    assert_eq!(with_receipt["receipt_name"], "taxi.png");
    assert_eq!(with_receipt["receipt_b64"], "aGVsbG8=");
    let (sc, cleared) = json_req(
        &app,
        "PUT",
        &format!("/api/expenses/{id}"),
        Some(json!({
            "date":"2026-10-09","customer_id":cid,"amount_minor":4200,"currency":"EUR"
        })),
    )
    .await;
    assert_eq!(sc, StatusCode::OK, "{cleared}");
    assert!(
        cleared.get("receipt_name").is_none() && cleared.get("receipt_b64").is_none(),
        "an omitted receipt is cleared by a full replacement: {cleared}"
    );

    // A later edit can put a conversion back, normalised exactly as on create.
    let (s2, reconverted) = json_req(
        &app,
        "PUT",
        &format!("/api/expenses/{id}"),
        Some(json!({
            "date":"2026-10-09","customer_id":cid,"amount_minor":4200,"currency":"EUR",
            "original_amount_minor":4500,"original_currency":"USD","exchange_rate":"1.080000"
        })),
    )
    .await;
    assert_eq!(s2, StatusCode::OK, "{reconverted}");
    assert_eq!(reconverted["original_amount_minor"], 4500);
    assert_eq!(reconverted["exchange_rate"], "1.08");
    assert!(
        reconverted["exchange_rate_date"].is_null(),
        "the rate's date is optional: {reconverted}"
    );
}

/// The update path answers like the create and the delete: an unowned record is
/// a 404 (never a 403 — that would leak its existence, #51), a bad shape is a
/// 422 that changes nothing, and an expense frozen by an issued invoice is a
/// 409 — permanently: settlement does not release it.
#[tokio::test]
async fn expense_update_refuses_unowned_bad_and_locked_expenses() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    new_project(&app, &cid, "P1", json!({})).await;
    let (_, x) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"project_code":"P1","amount_minor":5000,
            "currency":"EUR","note":"parts"
        })),
    )
    .await;
    let xid = x["id"].as_str().unwrap().to_string();
    let path = format!("/api/expenses/{xid}");
    let body = json!({
        "date":"2026-10-02","customer_id":cid,"project_code":"P1","amount_minor":5000,
        "currency":"EUR","note":"tampered"
    });

    // No session at all.
    let (s_anon, _, _) = raw(&app.router, "PUT", &path, Some(body.clone()), None).await;
    assert_eq!(s_anon, StatusCode::UNAUTHORIZED, "member-tier route");

    // A member editing someone else's expense: 404, and the note is untouched.
    let member = member_cookie(&app, "eve@test.local", "evepass123").await;
    let (s_other, _, _) = raw(&app.router, "PUT", &path, Some(body.clone()), Some(&member)).await;
    assert_eq!(
        s_other,
        StatusCode::NOT_FOUND,
        "an unowned expense must not leak its existence"
    );
    let (_, still) = json_req(&app, "GET", &path, None).await;
    assert_eq!(still["note"], "parts");

    // A shape the create would refuse: unknown project → 422, nothing persisted.
    let (s_bad, bad) = json_req(
        &app,
        "PUT",
        &path,
        Some(json!({
            "date":"2026-10-02","customer_id":cid,"project_code":"NOPE","amount_minor":5000,
            "currency":"EUR","note":"tampered"
        })),
    )
    .await;
    assert_eq!(s_bad, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    let (_, unchanged) = json_req(&app, "GET", &path, None).await;
    assert_eq!(unchanged["note"], "parts");
    assert_eq!(unchanged["project_code"], "P1");

    // Issued invoice → the expense is frozen against edits, exactly as it is
    // against deletes (#18/#218) — and INVOICED IS PERMANENT: settling the
    // invoice no longer releases the record.
    let (_, inv) = json_req(
        &app,
        "POST",
        "/api/invoices",
        Some(json!({"customer_id":cid,"from":"2026-10-01","to":"2026-10-07"})),
    )
    .await;
    let iid = inv["id"].as_str().unwrap().to_string();
    json_req(&app, "POST", &format!("/api/invoices/{iid}/issue"), None).await;
    let (s_locked, locked) = json_req(&app, "PUT", &path, Some(body.clone())).await;
    assert_eq!(s_locked, StatusCode::CONFLICT, "{locked}");
    assert!(
        locked["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("issued invoice"),
        "{locked}"
    );
    json_req(
        &app,
        "POST",
        &format!("/api/invoices/{iid}/pay"),
        Some(json!({"amount_minor": 5000, "reference": "wire-1"})),
    )
    .await;
    let (s_after, after) = json_req(&app, "PUT", &path, Some(body)).await;
    assert_eq!(
        s_after,
        StatusCode::CONFLICT,
        "a settled invoice keeps its expense locked: {after}"
    );
}

#[tokio::test]
async fn reimbursement_claim_lifecycle_locks_expenses() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (_, e1) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"amount_minor":5000,"currency":"EUR"})),
    )
    .await;
    let (_, e2) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-03","customer_id":cid,"amount_minor":3000,"currency":"EUR"})),
    )
    .await;
    let ids = json!([e1["id"], e2["id"]]);

    // Create draft claim.
    let (s, claim) = json_req(
        &app,
        "POST",
        "/api/claims",
        Some(json!({"title":"Oct costs","expense_ids":ids})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{claim}");
    assert_eq!(claim["state"], "draft");
    assert_eq!(claim["total_minor"], 8000);
    let claim_id = claim["id"].as_str().unwrap().to_string();
    let xid = e1["id"].as_str().unwrap().to_string();

    // Draft does not lock; submit then it locks.
    assert_eq!(
        json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    // (that deleted e1; recreate for the lock test)
    let (_, e1b) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"amount_minor":5000,"currency":"EUR"})),
    )
    .await;
    let xid = e1b["id"].as_str().unwrap().to_string();
    let (_, claim2) = json_req(
        &app,
        "POST",
        "/api/claims",
        Some(json!({"title":"Oct costs","expense_ids":[e1b["id"], e2["id"]]})),
    )
    .await;
    let cid2 = claim2["id"].as_str().unwrap().to_string();
    json_req(&app, "POST", &format!("/api/claims/{cid2}/submit"), None).await;
    assert_eq!(
        json_req(&app, "DELETE", &format!("/api/expenses/{xid}"), None)
            .await
            .0,
        StatusCode::CONFLICT,
        "submitted claim locks expense"
    );

    // Admin approves.
    let (sd, decided) = json_req(
        &app,
        "POST",
        &format!("/api/claims/{cid2}/decision"),
        Some(json!({"decision":"approve"})),
    )
    .await;
    assert_eq!(sd, StatusCode::OK, "{decided}");
    assert_eq!(decided["state"], "approved");
    let _ = claim_id;
}

#[tokio::test]
async fn claim_rejects_duplicate_expense_ids() {
    // #187: [id, id] summed the expense amount twice into the persisted
    // claim total while referencing one expense.
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap().to_string();
    let (_s, x) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(
            json!({"date":"2026-10-02","customer_id":cid,"amount_minor":2500,
                    "currency":"EUR","note":"taxi"}),
        ),
    )
    .await;
    let (s, b) = json_req(
        &app,
        "POST",
        "/api/claims",
        Some(json!({"title":"dup","expense_ids":[x["id"], x["id"]]})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
}

// ----------------------------------------------------- Tier 2: missing 422s --

/// #262 Tier 2: categories POST and PUT reject invalid shapes (missing/blank
/// name, unknown fields) with 422 and persist nothing.
#[tokio::test]
async fn category_invalid_shapes_are_422_without_persistence() {
    let (app, _d) = app().await;
    let (s_missing, body) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"default_billable": true})),
    )
    .await;
    assert_eq!(s_missing, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    for bad in [
        json!({"name": "   "}),
        json!({"name": "Travel", "bogus": 1}),
        json!({"name": "T", "default_billable": "yes"}),
    ] {
        let label = bad.to_string();
        let (s, _) = json_req(&app, "POST", "/api/categories", Some(bad)).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "payload: {label}");
    }

    // A valid create, then an invalid update leaves the stored value intact.
    let (sc, cat) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Travel"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cat}");
    let cat_id = cat["id"].as_str().unwrap();
    let (s_up, _) = json_req(
        &app,
        "PUT",
        &format!("/api/categories/{cat_id}"),
        Some(json!({"name": ""})),
    )
    .await;
    assert_eq!(s_up, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, list) = json_req(&app, "GET", "/api/categories", None).await;
    assert!(
        list["categories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == cat["id"] && c["name"] == "Travel"),
        "refused update must not change the stored category"
    );
}

/// SET-12: category names are unique org-wide, trimmed and case-insensitive on
/// both create and rename, with the row being renamed excluded from its own
/// check. A duplicate is a 409 `conflict` and persists nothing.
#[tokio::test]
async fn category_names_are_unique_case_insensitively_with_self_exclusion() {
    let (app, _d) = app().await;
    let (s1, first) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Food"})),
    )
    .await;
    assert_eq!(s1, StatusCode::CREATED, "{first}");
    let first_id = first["id"].as_str().unwrap().to_string();

    for duplicate in ["Food", " food ", "FOOD"] {
        let (s, body) = json_req(
            &app,
            "POST",
            "/api/categories",
            Some(json!({"name": duplicate})),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "{duplicate}: {body}");
        assert_eq!(body["error"]["code"], "conflict", "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("already exists"),
            "{body}"
        );
    }
    let (_, list) = json_req(&app, "GET", "/api/categories", None).await;
    let food_rows = list["categories"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"].as_str().unwrap().eq_ignore_ascii_case("food"))
        .count();
    assert_eq!(food_rows, 1, "duplicates never persist: {list}");

    let (s2, second) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Travel"})),
    )
    .await;
    assert_eq!(s2, StatusCode::CREATED, "{second}");
    let second_id = second["id"].as_str().unwrap().to_string();

    // Renaming onto another row's name (any case, padded) is refused.
    let (s3, body3) = json_req(
        &app,
        "PUT",
        &format!("/api/categories/{second_id}"),
        Some(json!({"name":" food "})),
    )
    .await;
    assert_eq!(s3, StatusCode::CONFLICT, "{body3}");
    assert_eq!(body3["error"]["code"], "conflict", "{body3}");
    let (_, after_conflict) = json_req(&app, "GET", "/api/categories", None).await;
    let kept = after_conflict["categories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == second_id.as_str())
        .unwrap();
    assert_eq!(kept["name"], "Travel", "refused rename persists nothing");

    // A different case of its OWN name is a no-op rename, not a conflict.
    let (s4, renamed) = json_req(
        &app,
        "PUT",
        &format!("/api/categories/{second_id}"),
        Some(json!({"name":"TRAVEL"})),
    )
    .await;
    assert_eq!(s4, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["name"], "TRAVEL");

    let (s5, first_renamed) = json_req(
        &app,
        "PUT",
        &format!("/api/categories/{first_id}"),
        Some(json!({"name":"FOOD"})),
    )
    .await;
    assert_eq!(s5, StatusCode::OK, "{first_renamed}");
    assert_eq!(first_renamed["name"], "FOOD");
}

/// #262 Tier 2: the claim workflow's error shapes — submit-twice conflicts, a
/// malformed decision is 422, and an unknown claim is 404.
#[tokio::test]
async fn claim_error_shapes_are_honest() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    let (sc, cat) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Travel"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cat}");
    let (_, exp) = json_req(
        &app,
        "POST",
        "/api/expenses",
        Some(json!({"date":"2026-10-02","customer_id":cid,"category_id":cat["id"],"amount_minor":1000,"currency":"EUR"})),
    )
    .await;
    let (scc, claim) = json_req(
        &app,
        "POST",
        "/api/claims",
        Some(json!({"title":"Oct costs","expense_ids":[exp["id"]]})),
    )
    .await;
    assert_eq!(scc, StatusCode::CREATED, "{claim}");
    let claim_id = claim["id"].as_str().unwrap();

    // Submit a draft succeeds; a second submit is a 409 (no longer a draft).
    assert_eq!(
        json_req(
            &app,
            "POST",
            &format!("/api/claims/{claim_id}/submit"),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        json_req(
            &app,
            "POST",
            &format!("/api/claims/{claim_id}/submit"),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // A malformed decision is 422 and leaves the claim submitted.
    let (s_dec, body) = json_req(
        &app,
        "POST",
        &format!("/api/claims/{claim_id}/decision"),
        Some(json!({"decision":"nope"})),
    )
    .await;
    assert_eq!(s_dec, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    // An unknown claim id is a 404, never a phantom state change.
    assert_eq!(
        json_req(
            &app,
            "POST",
            "/api/claims/00000000-0000-0000-0000-000000000000/submit",
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // Submitting an unknown claim is 404; the known one is untouched.
    let (_, listed) = json_req(&app, "GET", "/api/claims", None).await;
    assert_eq!(listed["claims"][0]["state"], "submitted");
}

// ------------------------------------------------------- pagination (#334) --

/// #334: the expense register is the view with no range filter, so it is the
/// one that can outgrow a page. A page says whether more follow, and following
/// the cursor reaches every expense exactly once, newest first.
#[tokio::test]
async fn expense_pages_report_more_and_the_cursor_walks_every_row_once() {
    let (app, _d) = app().await;
    let c = new_customer(&app, "Tucano Time", "EUR", 6000).await;
    let cid = c["id"].as_str().unwrap();
    new_project(&app, cid, "P1", json!({})).await;
    let (sc, cat) = json_req(
        &app,
        "POST",
        "/api/categories",
        Some(json!({"name":"Travel"})),
    )
    .await;
    assert_eq!(sc, StatusCode::CREATED, "{cat}");
    let cat_id = cat["id"].as_str().unwrap();

    // Five expenses over two days, two of them sharing a day.
    for date in [
        "2026-10-01",
        "2026-10-01",
        "2026-10-02",
        "2026-10-02",
        "2026-10-02",
    ] {
        let (s, body) = json_req(
            &app,
            "POST",
            "/api/expenses",
            Some(json!({
                "date": date, "customer_id": cid, "project_code": "P1",
                "category_id": cat_id, "amount_minor": 1000, "currency": "EUR"
            })),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
    }

    // The whole set in one page.
    let (s, all) = json_req(&app, "GET", "/api/expenses?limit=100", None).await;
    assert_eq!(s, StatusCode::OK, "{all}");
    assert_eq!(
        all["next_cursor"],
        Value::Null,
        "a page that ends the set reports no more"
    );
    let rows = all["expenses"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    let every: Vec<String> = rows
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    let dates: Vec<&str> = rows.iter().map(|e| e["date"].as_str().unwrap()).collect();
    assert!(
        dates.windows(2).all(|w| w[0] >= w[1]),
        "the register walks (date, id) descending; got {dates:?}"
    );

    let mut walked: Vec<String> = Vec::new();
    let mut url = String::from("/api/expenses?limit=2");
    let mut pages = 0;
    loop {
        let (s, body) = json_req(&app, "GET", &url, None).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let page = body["expenses"].as_array().unwrap();
        assert!(page.len() <= 2, "a page never exceeds 'limit'");
        walked.extend(page.iter().map(|e| e["id"].as_str().unwrap().to_string()));
        pages += 1;
        match body["next_cursor"].as_str() {
            Some(cursor) => {
                assert!(
                    walked.len() < every.len(),
                    "only a truncated page may name a next page"
                );
                assert_ne!(
                    cursor,
                    walked.last().unwrap(),
                    "the cursor is a token, not the row id"
                );
                url = format!("/api/expenses?limit=2&cursor={cursor}");
            }
            None => {
                assert_eq!(walked.len(), every.len(), "the last page ended the set");
                break;
            }
        }
    }
    assert_eq!(
        walked, every,
        "the walk reaches every row exactly once, newest first"
    );
    assert_eq!(pages, 3, "five rows at two per page is three pages");
}

/// #334: the same field-named 422s as the entry list, on the endpoint a caller
/// is most likely to reach for a whole-history read.
#[tokio::test]
async fn expense_limit_and_cursor_refusals_are_field_named_422s() {
    let (app, _d) = app().await;

    for bad in ["1001", "0", "abc", ""] {
        let (s, body) = json_req(
            &app,
            "GET",
            &format!("/api/expenses?from=2026-10-01&to=2026-10-02&limit={bad}"),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "limit={bad}: {body}");
        assert_eq!(body["error"]["code"], "validation_failed");
        assert_eq!(body["error"]["fields"][0]["field"], "limit", "limit={bad}");
    }

    let (s, body) = json_req(&app, "GET", "/api/expenses?cursor=nonsense", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["fields"][0]["field"], "cursor");

    // The ceiling is accepted, on an empty collection too.
    let (ok, body) = json_req(&app, "GET", "/api/expenses?limit=1000", None).await;
    assert_eq!(ok, StatusCode::OK, "{body}");
    assert_eq!(body["next_cursor"], Value::Null);
    assert_eq!(body["expenses"].as_array().unwrap().len(), 0);
}
