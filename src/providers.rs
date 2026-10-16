//! Shared plumbing for the integration providers (#101).
//!
//! Before this module, `sso`, `accounting` and `calendar_oauth`
//! each re-implemented the `vault.get(k).or_else(env)` ladder — with drifted
//! precedence — and each kept a near-identical name-keyed registry. The
//! ladders live here once, and `Registry<T>` replaces the copies.
//!
//! Two resolution lanes exist *by design* and must not be collapsed:
//! - **secrets** (#77): admin-entered vault values win; env is only the
//!   boot fallback for deployments where the vault key is absent. A secret
//!   can never enter `config.json` (the whitelist refuses secret-shaped keys,
//!   #94), so config.json never participates.
//! - **non-secret knobs** (#94): env > config.json > vault > none — the
//!   documented precedence for public settings like base URLs and client ids.

use std::sync::Arc;

use crate::appconfig::{self, AppConfig};
use crate::vault::SecretVault;

/// A provider is addressable by its stable name.
pub trait NamedProvider {
    fn name(&self) -> &str;
}

/// Name-keyed registry of enabled providers — the single generic behind
/// `PaymentRegistry`, `AccountingRegistry` and `SsoRegistry` (#101).
pub struct Registry<T: ?Sized> {
    providers: Vec<Arc<T>>,
}

impl<T: ?Sized> Default for Registry<T> {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
        }
    }
}

impl<T: NamedProvider + ?Sized> Registry<T> {
    #[must_use]
    pub fn new(providers: Vec<Arc<T>>) -> Self {
        Self { providers }
    }

    /// Look a provider up by name. Case-insensitive: SSO historically
    /// tolerated casing drift in IdP names, and accounting names are
    /// lowercase constants, so widening the other cannot change any
    /// existing resolution.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<T>> {
        self.providers
            .iter()
            .find(|p| p.name().eq_ignore_ascii_case(name))
            .cloned()
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.providers.iter().map(|p| p.name().to_owned()).collect()
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        !self.providers.is_empty()
    }
}

/// The secret lane (#77): vault first, env only as the fallback when the
/// vault holds nothing for `vault_key`.
#[must_use]
pub fn resolve_secret(vault: Option<&SecretVault>, vault_key: &str, env: &str) -> Option<String> {
    vault
        .and_then(|v| v.get(vault_key))
        .or_else(|| std::env::var(env).ok())
}

/// The public-knob lane (#94): env > config.json > vault > env-on-vault-key.
/// `cfg_key` is the `config.json` whitelist name, `vault_key`/`env` the
/// legacy vault/env spellings of the same knob.
#[must_use]
pub fn resolve_setting(
    cfg: &AppConfig,
    vault: Option<&SecretVault>,
    cfg_key: &str,
    vault_key: &str,
    env: &str,
) -> Option<String> {
    cfg.get_optional_str(cfg_key, &appconfig::process_env)
        .or_else(|| resolve_secret(vault, vault_key, env))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::SecretVault;

    fn vault_with(root: &std::path::Path, key: &str, value: &str) -> SecretVault {
        let vault = SecretVault::open(root, Some("0123456789abcdef0123456789abcdef"))
            .unwrap()
            .unwrap();
        vault.put(key, value).unwrap();
        vault
    }

    #[test]
    fn resolve_secret_prefers_the_vault() {
        // #262 Tier 3: the secret precedence chain (vault first, env fallback)
        // had no direct test. The env name is deliberately one this process
        // cannot have set, so an unset env is a deterministic `None`.
        let dir = tempfile::tempdir().unwrap();
        let vault = vault_with(dir.path(), "svc.token", "from-vault");
        assert_eq!(
            resolve_secret(Some(&vault), "svc.token", "TUCANO_262_UNSET_TOKEN"),
            Some("from-vault".to_owned())
        );
        assert_eq!(
            resolve_secret(None, "svc.token", "TUCANO_262_UNSET_TOKEN"),
            None,
            "no vault and no env is None, never an empty string"
        );
    }

    #[test]
    fn resolve_setting_prefers_config_then_vault() {
        // #262 Tier 3: config.json outranks the vault fallback; with no config
        // entry the vault supplies the value. `qbo_base_url` is an optional
        // string with a Null default, so an unset env is a real `None`.
        let dir = tempfile::tempdir().unwrap();
        let cfg = {
            let mut c = crate::appconfig::AppConfig::load(dir.path()).unwrap();
            let mut patch = serde_json::Map::new();
            patch.insert(
                "qbo_base_url".into(),
                serde_json::json!("https://cfg.example"),
            );
            c.update(patch).unwrap();
            c
        };
        let vault = vault_with(dir.path(), "qbo.base_url", "https://vault.example");
        assert_eq!(
            resolve_setting(
                &cfg,
                Some(&vault),
                "qbo_base_url",
                "qbo.base_url",
                "TUCANO_QBO_BASE_URL"
            ),
            Some("https://cfg.example".to_owned()),
            "config.json outranks the vault fallback"
        );
        let empty = crate::appconfig::AppConfig::empty();
        assert_eq!(
            resolve_setting(
                &empty,
                Some(&vault),
                "qbo_base_url",
                "qbo.base_url",
                "TUCANO_QBO_BASE_URL"
            ),
            Some("https://vault.example".to_owned())
        );
    }
}
