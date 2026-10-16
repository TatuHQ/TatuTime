use crate::api::*;

// ------------------------------------------------- product/service catalog --

fn validate_item_type(input: &crate::domain::ItemTypeInput) -> Result<(), ApiError> {
    let mut errors = Vec::new();
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        errors.push(FieldError::new("name", "required, at most 120 characters"));
    }
    if input.description.chars().count() > 500 {
        errors.push(FieldError::new("description", "at most 500 characters"));
    }
    if input.default_price_minor > 100_000_000 {
        errors.push(FieldError::new("default_price_minor", "at most 100000000"));
    }
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    Ok(())
}

/// `GET /admin/item-types` — catalog incl. archived (#147).
pub async fn list_item_types(State(app): State<AppState>) -> ApiResult {
    Ok(Json(serde_json::json!({ "item_types": app.store.list_item_types()? })).into_response())
}

pub async fn create_item_type(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<crate::domain::ItemTypeInput>,
) -> ApiResult {
    validate_item_type(&input)?;
    let item = crate::domain::ItemType {
        id: Uuid::new_v4(),
        name: input.name.trim().to_string(),
        description: input.description.trim().to_string(),
        kind: input.kind,
        default_price_minor: input.default_price_minor,
        currency: input
            .currency
            .unwrap_or_else(|| crate::domain::Currency("EUR".into())),
        active: input.active,
        created_at: app.clock.now(),
    };
    app.store.put_item_type(&item)?;
    Ok((StatusCode::CREATED, Json(item)).into_response())
}

pub async fn update_item_type(
    State(app): State<AppState>,
    Path(id): Path<Uuid>,
    ValidJson(input): ValidJson<crate::domain::ItemTypeInput>,
) -> ApiResult {
    let Some(existing) = app.store.get_item_type(id)? else {
        return Err(ApiError::not_found("item type"));
    };
    validate_item_type(&input)?;
    let updated = crate::domain::ItemType {
        id,
        name: input.name.trim().to_string(),
        description: input.description.trim().to_string(),
        kind: input.kind,
        default_price_minor: input.default_price_minor,
        currency: input.currency.unwrap_or(existing.currency),
        active: input.active,
        created_at: existing.created_at,
    };
    app.store.put_item_type(&updated)?;
    Ok(Json(updated).into_response())
}

/// Delete a catalog item; invoice lines keep their snapshots (#147).
pub async fn delete_item_type(State(app): State<AppState>, Path(id): Path<Uuid>) -> ApiResult {
    app.store.delete_item_type(id)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
