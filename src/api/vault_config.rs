//! Secret vault and externalised configuration (#77, #94): the admin surface
//! for masked secret CRUD, the whitelisted `config.json` knobs, the credential
//! vault's lifecycle (inspect/enable/disable/regenerate) and the #52 audit-log
//! view.

use super::*;

/// `GET /admin/config` (#94): the effective externalised configuration —
/// every whitelisted non-secret knob with its resolved value, the source it
/// came from (env / config.json / default) and its default. The whitelist has
/// no secret material by construction, so nothing needs masking; anything
/// shaped like a secret cannot even be persisted (whitelist enforced on write).
pub async fn admin_config(State(app): State<AppState>) -> ApiResult {
    let env_fn = |key: &str| app.env_var(key);
    let effective = app.cfg().effective(&env_fn);
    // #190: no `path` — the on-disk location of config.json must never be
    // serialised to a client (AGENTS.md: responses never expose paths). The
    // GUI's config table reads `config`/`source`/`email_transport` only.
    Ok(Json(serde_json::json!({
        "config": effective,
        "vault_enabled": app.vault.is_enabled(),
        // #130: Settings shows whether email actually delivers ("smtp") or
        // is a logged no-op ("disabled" until smtp.host is configured).
        "email_transport": app.email.transport(),
    }))
    .into_response())
}

/// `PUT /admin/config` (#94): patch whitelisted keys into `<data>/config.json`.
/// Unknown or secret-shaped keys are refused, and so is any read-only knob:
/// `vault_enabled` is writable only through the vault lifecycle endpoints
/// (M3), because persisting `false` next to a `secrets.bin` would brick the
/// next boot. Takes effect on the next boot for boot-time knobs and
/// immediately for request-time ones (SSO domains/admin group, OAuth redirect).
pub async fn update_config(
    State(app): State<AppState>,
    ValidJson(patch): ValidJson<serde_json::Map<String, serde_json::Value>>,
) -> ApiResult {
    // QA-12: a corrupt config.json is a *stored document* failure (500
    // corrupt_document), not the opaque `internal`. `AppConfig::load` returns
    // the path and parse error for the log; the client only learns to look
    // there.
    let mut cfg =
        crate::appconfig::AppConfig::load(app.store.root()).map_err(ApiError::corrupt_document)?;
    cfg.update(patch).map_err(|e| {
        ApiError::validation(vec![FieldError::new(
            e.key.unwrap_or_else(|| "config".to_string()),
            e.message,
        )])
    })?;
    // Reflect the change for request-time readers.
    app.reload_config(Arc::new(cfg));
    Ok(Json(serde_json::json!({ "ok": true })).into_response())
}

fn valid_secret_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 100
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub(crate) fn require_vault(app: &AppState) -> Result<Arc<crate::vault::SecretVault>, ApiError> {
    app.vault.get().ok_or_else(vault_disabled)
}

/// The fail-closed answer when a secret endpoint is called on a disabled vault.
fn vault_disabled() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "vault_disabled",
        "the credential vault is disabled — enable it in Settings → Security, \
         or set TATU_VAULT_ENABLED=true",
    )
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretInput {
    pub value: String,
}

/// List configured secret keys with masked hints — never the values (#77).
pub async fn list_secrets(State(app): State<AppState>) -> ApiResult {
    let vault = require_vault(&app)?;
    let secrets: Vec<serde_json::Value> = vault
        .keys()
        .into_iter()
        .map(|k| serde_json::json!({ "key": k, "hint": vault.hint(&k) }))
        .collect();
    Ok(Json(serde_json::json!({ "secrets": secrets })).into_response())
}

/// Store/overwrite a secret (admin only). The value is never echoed back.
pub async fn set_secret(
    State(app): State<AppState>,
    Path(key): Path<String>,
    ValidJson(input): ValidJson<SecretInput>,
) -> ApiResult {
    {
        // H1: take the gate *before* resolving the vault. A handle obtained
        // before a concurrent disable/regenerate would write `secrets.bin`
        // under a key that is no longer installed.
        let _gate = app.vault.gate();
        let vault = require_vault(&app)?;
        if !valid_secret_key(&key) {
            return Err(ApiError::validation(vec![FieldError::new(
                "key",
                "use letters, digits, '.', '_' or '-' (max 100)",
            )]));
        }
        vault
            .put(&key, &input.value)
            .map_err(|e| ApiError::internal(format!("vault write failed: {e}")))?;
    }
    app.audit.record("secret_set", &key, app.clock.now());
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Delete a secret (admin only).
pub async fn delete_secret(State(app): State<AppState>, Path(key): Path<String>) -> ApiResult {
    let removed = {
        let _gate = app.vault.gate();
        let vault = require_vault(&app)?;
        vault
            .remove(&key)
            .map_err(|e| ApiError::internal(format!("vault write failed: {e}")))?
    };
    if !removed {
        return Err(ApiError::not_found("secret"));
    }
    app.audit.record("secret_deleted", &key, app.clock.now());
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ------------------------------------------------------- key lifecycle ------

/// `GET /api/admin/vault` — always 200, even when disabled, so Settings can
/// render the disabled state. The body never contains the key, a key path or
/// any host path: `key_file_name` is the fixed literal `vault.key`.
pub async fn vault_status(State(app): State<AppState>) -> ApiResult {
    Ok(no_store(Json(vault_status_json(&app)).into_response()))
}

fn source_name(source: Option<crate::vault::KeySource>) -> &'static str {
    match source {
        None => "none",
        Some(s) => s.as_str(),
    }
}

fn disabled_name(reason: Option<crate::vault::DisabledReason>) -> Option<&'static str> {
    reason.map(crate::vault::DisabledReason::as_str)
}

fn vault_status_json(app: &AppState) -> serde_json::Value {
    let boot = app.vault.status();
    let enabled = boot.vault.is_some();
    let source = source_name(boot.source);
    let disabled_reason = disabled_name(boot.disabled);
    let secrets_count = boot.vault.as_ref().map_or(0, |v| v.keys().len());
    let key_file_present = boot.key_file.is_some();
    // M1: an operator-set `TATU_VAULT_ENABLED` (valid boolean) owns the
    // lifecycle, so neither direction is offered as an admin action.
    let env_managed = env_managed_flag(app).is_some();
    let can_enable = !enabled
        && !boot.external_override
        && !env_managed
        && boot.disabled != Some(crate::vault::DisabledReason::Env);
    let can_disable = enabled
        && secrets_count == 0
        && !boot.external_override
        && !env_managed
        && boot.source == Some(crate::vault::KeySource::KeyFile);
    serde_json::json!({
        "enabled": enabled,
        "source": source,
        "disabled_reason": disabled_reason,
        "fingerprint": boot.fingerprint,
        "key_file_name": if key_file_present { Some("vault.key") } else { None },
        "key_file_present": key_file_present,
        "secrets_count": secrets_count,
        "external_override": boot.external_override,
        "can_enable": can_enable,
        "can_disable": can_disable,
        "can_regenerate": can_disable,
    })
}

/// Enable/regenerate responses additionally report whether the key file was
/// minted (`true`) or an existing key was reused (`false`).
fn with_created_key(mut status: serde_json::Value, created_key: bool) -> serde_json::Value {
    if let serde_json::Value::Object(ref mut map) = status {
        map.insert(
            "created_key".to_string(),
            serde_json::Value::Bool(created_key),
        );
    }
    status
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        http::header::CACHE_CONTROL,
        http::HeaderValue::from_static("no-store"),
    );
    response
}

/// The confirmation body shared by disable and regenerate.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultConfirmInput {
    pub confirm: bool,
}

fn require_confirmation(input: &VaultConfirmInput) -> Result<(), ApiError> {
    if input.confirm {
        Ok(())
    } else {
        Err(ApiError::validation(vec![FieldError::new(
            "confirm",
            "this action requires confirm: true",
        )]))
    }
}

fn vault_error(status: StatusCode, code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(status, code, message)
}

fn already_enabled() -> ApiError {
    vault_error(
        StatusCode::CONFLICT,
        "vault_already_enabled",
        "the credential vault is already enabled",
    )
}

fn managed_externally() -> ApiError {
    vault_error(
        StatusCode::CONFLICT,
        "vault_managed_externally",
        "the vault lifecycle is managed outside the app (TATU_VAULT_ENABLED, \
         TATU_SECRET_KEY or TATU_SECRET_KEY_FILE is set)",
    )
}

/// Resolve `TATU_VAULT_ENABLED` exactly as boot does (env > config.json >
/// default). `Some(enabled)` means the environment carries a **valid**
/// boolean, so the operator owns the flag and the admin API must not override
/// it (M1). An invalid value falls through to config/default, matching boot
/// (L2: a typo like `=no` must not disable the admin controls).
fn env_managed_flag(app: &AppState) -> Option<bool> {
    let def = crate::appconfig::key_def("vault_enabled")?;
    let cfg = app.cfg();
    let env = |key: &str| app.env_var(key);
    match cfg.resolve(def, &env) {
        (serde_json::Value::Bool(enabled), crate::appconfig::Source::Env) => Some(enabled),
        _ => None,
    }
}

fn refuse_env_managed(app: &AppState) -> Result<(), ApiError> {
    if env_managed_flag(app).is_some() {
        Err(managed_externally())
    } else {
        Ok(())
    }
}

fn vault_in_use() -> ApiError {
    vault_error(
        StatusCode::CONFLICT,
        "vault_in_use",
        "the vault still holds credentials; delete them before changing the key",
    )
}

fn persist_vault_enabled(app: &AppState, enabled: bool) -> Result<(), ApiError> {
    let mut cfg =
        crate::appconfig::AppConfig::load(app.store.root()).map_err(ApiError::corrupt_document)?;
    // The lifecycle endpoint is the sanctioned writer for this read-only knob
    // (M3); `set_lifecycle_bool` validates the value and persists the file.
    cfg.set_lifecycle_bool("vault_enabled", enabled)
        .map_err(|e| {
            tracing::error!(error = %e.message, "could not persist vault_enabled");
            ApiError::data_dir_not_writable()
        })?;
    app.reload_config(Arc::new(cfg));
    Ok(())
}

/// `POST /api/admin/vault/enable` — resolve/generate a key, persist
/// `vault_enabled=true`, then install the slot. 201 when a new key file was
/// minted, 200 when an existing key was reused.
pub async fn vault_enable(State(app): State<AppState>) -> ApiResult {
    let created_key = {
        // H1: hold the gate across the whole read-check-write-install
        // sequence; the status is read *after* acquiring it, never before.
        let _gate = app.vault.gate();
        let current = app.vault.status();
        if current.vault.is_some() {
            return Err(already_enabled());
        }
        if current.external_override {
            return Err(managed_externally());
        }
        refuse_env_managed(&app)?;

        let key_path = app.store.root().join("vault.key");
        let existed = key_path.exists();
        let boot = crate::vault::open_for_boot(
            app.store.root(),
            app.env_var("TATU_SECRET_KEY"),
            app.env_var("TATU_SECRET_KEY_FILE"),
            true,
        )
        .map_err(|e| {
            tracing::error!("vault enable failed: {e}");
            ApiError::internal("the vault key could not be resolved".to_string())
        })?;
        if boot.vault.is_none() {
            // `open_for_boot` only returns a disabled boot here on a write failure.
            return Err(ApiError::data_dir_not_writable());
        }
        let created_key = !existed
            && boot.key_file.is_some()
            && boot.source == Some(crate::vault::KeySource::KeyFile);

        persist_vault_enabled(&app, true)?;
        app.vault.install(boot);
        created_key
    };

    if created_key {
        record_audit(&app, "vault_key_created", "vault.key").await;
    }
    record_audit(&app, "vault_enabled", "vault").await;

    let status = with_created_key(vault_status_json(&app), created_key);
    let response = Json(status).into_response();
    let response = if created_key {
        let mut response = response;
        *response.status_mut() = StatusCode::CREATED;
        response
    } else {
        response
    };
    Ok(no_store(response))
}

/// `POST /api/admin/vault/disable` — refuses while credentials exist, persists
/// `vault_enabled=false`, installs the disabled state and only then deletes
/// the generated key file (best effort).
pub async fn vault_disable(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<VaultConfirmInput>,
) -> ApiResult {
    require_confirmation(&input)?;
    let delete_key = {
        let _gate = app.vault.gate();
        let current = app.vault.status();
        if current.external_override {
            return Err(managed_externally());
        }
        refuse_env_managed(&app)?;
        if let Some(vault) = current.vault.as_ref()
            && !vault.keys().is_empty()
        {
            return Err(vault_in_use());
        }

        // H2: persist first. A failed write leaves the slot and the key file
        // untouched, so an enabled server can never point at a deleted key.
        persist_vault_enabled(&app, false)?;
        // The authoritative state is now "disabled"; install it before the
        // best-effort cleanup so a delete failure cannot resurrect a half state.
        app.vault.install(crate::vault::VaultBoot::disabled(
            crate::vault::DisabledReason::Config,
        ));
        if current.source == Some(crate::vault::KeySource::KeyFile) {
            current.key_file
        } else {
            None
        }
    };
    // H2: a leftover key file without its store is harmless — the next boot
    // ignores it. A deletion failure is logged, not fatal.
    if let Some(path) = delete_key {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                error = %e,
                "could not delete the vault key file after disabling the vault; \
                 it is ignored while no vault store exists"
            ),
        }
    }
    record_audit(&app, "vault_disabled", "vault").await;
    Ok(no_store(Json(vault_status_json(&app)).into_response()))
}

/// `POST /api/admin/vault/regenerate` — only with zero stored credentials and
/// no external override. Mints a fresh key file and keeps the vault enabled.
pub async fn vault_regenerate(
    State(app): State<AppState>,
    ValidJson(input): ValidJson<VaultConfirmInput>,
) -> ApiResult {
    require_confirmation(&input)?;
    {
        let _gate = app.vault.gate();
        let current = app.vault.status();
        if current.external_override {
            return Err(managed_externally());
        }
        refuse_env_managed(&app)?;
        if current.vault.is_none() {
            return Err(vault_disabled());
        }
        if current.vault.as_ref().is_some_and(|v| !v.keys().is_empty()) {
            return Err(vault_in_use());
        }

        let root = app.store.root();
        let raw = crate::vault::write_new_key_file(root).map_err(|e| {
            tracing::error!("could not regenerate the vault key: {e}");
            ApiError::data_dir_not_writable()
        })?;
        let vault =
            crate::vault::SecretVault::open_with(root, &raw, crate::vault::KeySource::KeyFile)
                .map_err(|e| {
                    ApiError::internal(format!("the regenerated vault key is unusable: {e}"))
                })?;
        let boot = crate::vault::VaultBoot::from_vault(
            Arc::new(vault),
            crate::vault::KeySource::KeyFile,
            Some(root.join("vault.key")),
            false,
        );
        app.vault.install(boot);
    }
    record_audit(&app, "vault_key_regenerated", "vault.key").await;
    let status = with_created_key(vault_status_json(&app), true);
    Ok(no_store(Json(status).into_response()))
}
