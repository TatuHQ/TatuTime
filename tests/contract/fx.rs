use crate::support::*;

use axum::http::StatusCode;

/// A minimal ECB-shaped reference document for the offline FX tests. The
/// endpoint's real source is the ECB's published daily feed; this seam keeps
/// the maths, the reference date and the fail-closed path testable with no
/// network at all.
const ECB_SAMPLE: &str = r#"<Cube time='2026-10-14'>
      <Cube currency='USD' rate='1.0831'/>
      <Cube currency='GBP' rate='0.85210'/>
    </Cube>"#;

#[tokio::test]
async fn fx_requires_a_session_and_validates_both_codes() {
    let (app, _d) = app().await;
    let (s, _, _) = raw(&app.router, "GET", "/api/fx?from=USD&to=EUR", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "member-tier route");

    let (s, body) = json_req(&app, "GET", "/api/fx?from=US&to=EUR", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (s, body) = json_req(&app, "GET", "/api/fx?from=EURO&to=EUR", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
    let (s, body) = json_req(&app, "GET", "/api/fx?from=USD", None).await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "a missing parameter is a bad request: {body}"
    );
}

#[tokio::test]
async fn fx_currency_with_itself_is_the_identity_and_needs_no_table() {
    let (app, _d) = app().await;
    let (s, body) = json_req(&app, "GET", "/api/fx?from=eur&to=EUR", None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["from"], "EUR", "codes are normalised: {body}");
    assert_eq!(body["rate"], 1.0);
    assert_eq!(body["source"], "identity");
    assert!(
        body["date"].is_null(),
        "no published table was consulted: {body}"
    );
}

#[tokio::test]
async fn fx_serves_the_reference_table_and_fails_closed_without_one() {
    let (app, _d) = app().await;
    tucano_time::fx::set_rates_document(Some(ECB_SAMPLE.to_string()));
    let (s, body) = json_req(&app, "GET", "/api/fx?from=USD&to=EUR", None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(
        body["date"], "2026-10-14",
        "the rate travels with its date: {body}"
    );
    assert_eq!(body["source"], "ecb");
    let usd_eur = body["rate"].as_f64().expect("a numeric rate");
    assert!((usd_eur - 1.0 / 1.0831).abs() < 1e-9, "{body}");

    // A cross rate is derived from the two EUR legs.
    let (s, body) = json_req(&app, "GET", "/api/fx?from=GBP&to=USD", None).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let gbp_usd = body["rate"].as_f64().expect("a numeric rate");
    assert!((gbp_usd - 1.0831 / 0.85210).abs() < 1e-9, "{body}");

    // A currency the table does not list is refused, never guessed.
    let (s, body) = json_req(&app, "GET", "/api/fx?from=USD&to=XXX", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // An unusable document fails closed: a 503, not a made-up rate.
    tucano_time::fx::set_rates_document(Some("not a rates document".to_string()));
    let (s, body) = json_req(&app, "GET", "/api/fx?from=USD&to=EUR", None).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "fx_unavailable");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("enter a rate manually"),
        "{body}"
    );

    // Leave the process as it was found for every other test.
    tucano_time::fx::set_rates_document(None);
}
