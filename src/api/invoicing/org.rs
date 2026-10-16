use crate::api::*;

/// The org identity singleton (`org_profile.json`, #138); absent = defaults.
pub(crate) fn load_org(app: &AppState) -> Result<crate::domain::OrgProfile, ApiError> {
    Ok(app
        .store
        .read_json_rel::<crate::domain::OrgProfile>("org_profile.json")?
        .unwrap_or_default())
}

/// The issuing organisation for invoice documents and emails, resolved ONCE
/// here so there is a single source of truth (#138): `org_profile.name` when
/// set, else the boot-time `org_name` config key (#94), else its default.
/// Legal id and address come from the profile only.
pub(crate) fn org_for(app: &AppState) -> Result<crate::pdf::Org, ApiError> {
    // #187: a corrupt org_profile.json must surface (500), not silently
    // render invoices with the fallback identity via `unwrap_or_default()`.
    let profile = load_org(app)?;
    let name = if profile.name.trim().is_empty() {
        app.cfg()
            .get_str("org_name", &crate::appconfig::process_env)
    } else {
        profile.name.trim().to_string()
    };
    Ok(crate::pdf::Org {
        name,
        legal_id: profile.legal_id.clone(),
        address: profile.address.clone(),
        accent: profile.accent.clone(),
    })
}

/// `GET /admin/org` — company identity (#138).
pub async fn org_get(State(app): State<AppState>) -> ApiResult {
    Ok(Json(load_org(&app)?).into_response())
}

/// Validate + `PUT /admin/org` (#138). Invalid values fail before anything
/// is persisted; empty name legitimately means "fall back to config".
pub async fn org_put(
    State(app): State<AppState>,
    ValidJson(profile): ValidJson<crate::domain::OrgProfile>,
) -> ApiResult {
    let mut errors = Vec::new();
    if profile.name.chars().count() > 120 {
        errors.push(FieldError::new("name", "at most 120 characters"));
    }
    if profile.legal_id.chars().count() > 60 {
        errors.push(FieldError::new("legal_id", "at most 60 characters"));
    }
    // #146 presentation knobs, validated here so nothing inert is stored.
    if profile.from_name.chars().count() > 120 {
        errors.push(FieldError::new("from_name", "at most 120 characters"));
    }
    if !profile.reply_to.trim().is_empty() && !crate::email::valid_address(profile.reply_to.trim())
    {
        errors.push(FieldError::new("reply_to", "not a valid email address"));
    }
    if !profile.accent.is_empty() && crate::pdf::parse_accent(&profile.accent).is_none() {
        errors.push(FieldError::new("accent", "expected a #rrggbb hex color"));
    }
    let mut billing_errors = Vec::new();
    crate::domain::validate_customer_billing_fields(
        &profile.address,
        &[],
        0,
        0,
        &None,
        &mut billing_errors,
    );
    errors.append(&mut billing_errors);
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    app.store.write_json_rel("org_profile.json", &profile)?;
    Ok(Json(profile).into_response())
}
