//! Externalised configuration (#94): every non-secret knob gets a durable home in
//! `<data>/config.json`, with a fixed precedence — `explicit env var >
//! config.json entry > built-in default`.
//!
//! Secret material (vault key, session secret, SMTP passwords, webhook/tokens)
//! deliberately does NOT live here: config.json is plaintext next to the data it
//! protects. Secrets keep the env/vault story (#77) plus the `*_FILE` mounts, and
//! the whitelist below is enforced on write, so `PUT /api/admin/config` physically
//! cannot persist a secret by accident.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Where an effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Env,
    File,
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Int {
        min: i64,
        max: i64,
        default: i64,
    },
    Str {
        default: &'static str,
    },
    OptionalStr,
    /// Comma-separated list in both env and file.
    Csv,
    /// ISO 4217 shaped currency code: exactly three ASCII letters, stored in
    /// canonical uppercase so `eur` and `EUR` cannot both exist. Optional:
    /// unset means "no base currency configured" (never guessed from data).
    Currency,
}

pub struct KeyDef {
    /// Key inside config.json.
    pub name: &'static str,
    /// Overriding environment variable.
    pub env: &'static str,
    pub kind: Kind,
    pub description: &'static str,
}

/// The complete whitelist of externalisable knobs. Anything not here cannot be
/// read from or written to config.json.
pub const KEYS: &[KeyDef] = &[
    KeyDef {
        name: "reminder_days",
        env: "TUCANO_REMINDER_DAYS",
        kind: Kind::Int {
            min: 1,
            max: 365,
            default: 7,
        },
        description: "Days between overdue-invoice email reminders (#35)",
    },
    KeyDef {
        name: "org_name",
        env: "TUCANO_ORG_NAME",
        kind: Kind::Str {
            default: "TucanoTime",
        },
        description: "Issuing organisation shown on invoice emails and PDFs (#113)",
    },
    KeyDef {
        name: "sso_admin_group",
        env: "TUCANO_SSO_ADMIN_GROUP",
        kind: Kind::Str { default: "" },
        description: "IdP group that maps to the admin role on SSO login (#32)",
    },
    KeyDef {
        name: "sso_allowed_domains",
        env: "TUCANO_SSO_ALLOWED_DOMAINS",
        kind: Kind::Csv,
        description: "Comma-separated email domains allowed for SSO (#32)",
    },
    KeyDef {
        name: "calendar_oauth_redirect",
        env: "TUCANO_CALENDAR_OAUTH_REDIRECT",
        kind: Kind::Str {
            default: "/api/calendar/oauth/callback",
        },
        description: "Redirect URI registered with the OAuth calendar app (#36)",
    },
    KeyDef {
        name: "google_calendar_client_id",
        env: "TUCANO_GOOGLE_CLIENT_ID",
        kind: Kind::OptionalStr,
        description: "Google OAuth client id (public half of the pair; the secret stays env/vault) (#36)",
    },
    KeyDef {
        name: "ms_calendar_client_id",
        env: "TUCANO_MS_CLIENT_ID",
        kind: Kind::OptionalStr,
        description: "Microsoft OAuth client id (secret stays env/vault) (#36)",
    },
    KeyDef {
        name: "qbo_base_url",
        env: "TUCANO_QBO_BASE_URL",
        kind: Kind::OptionalStr,
        description: "Override the QuickBooks API base URL (#33)",
    },
    KeyDef {
        name: "xero_base_url",
        env: "TUCANO_XERO_BASE_URL",
        kind: Kind::OptionalStr,
        description: "Override the Xero API base URL (#33)",
    },
    KeyDef {
        name: "max_docs",
        env: "TUCANO_MAX_DOCS",
        kind: Kind::Int {
            min: 10,
            max: 1_000_000,
            default: 50_000,
        },
        description: "Per-collection document cap before writes are refused",
    },
    // Display-only preference, NOT a secret: it lives in config.json so it is
    // never masked and never fail-closed the way a vault entry is. It only
    // opts the invoice Trends chart into mixed-currency display through the FX
    // seam; it never rewrites a persisted customer/project/invoice currency.
    // Unset means "no base currency configured": mixed-currency display stays
    // off and nothing is converted — the value is never guessed from the data.
    KeyDef {
        name: "base_currency",
        env: "TUCANO_BASE_CURRENCY",
        kind: Kind::Currency,
        description: "Optional base currency for mixed-currency invoice Trends (ISO 4217, e.g. EUR); unset disables the conversion",
    },
];

pub fn key_def(name: &str) -> Option<&'static KeyDef> {
    KEYS.iter().find(|k| k.name == name)
}

/// The loaded file contents (only whitelisted keys are kept).
#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    stored: Map<String, Value>,
    path: PathBuf,
}

impl AppConfig {
    /// Load `<root>/config.json`. Missing file is fine; a corrupt one is
    /// surfaced to the caller (refusing to start is right for a broken store).
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join("config.json");
        let stored = match std::fs::read(&path) {
            Ok(bytes) => {
                let v: Value = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("corrupt {}: {e}", path.display()))?;
                v.as_object().cloned().unwrap_or_default()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        // Drop unknown keys defensively (written by an older/newer version).
        let stored = stored
            .into_iter()
            .filter(|(k, _)| key_def(k).is_some())
            .collect();
        Ok(Self { stored, path })
    }

    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    fn env_lookup(env: &dyn Fn(&str) -> Option<String>, def: &KeyDef) -> Option<String> {
        env(def.env).filter(|s| !s.is_empty())
    }

    /// Resolve one key with precedence env > file > default.
    pub fn resolve(
        &self,
        def: &'static KeyDef,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> (Value, Source) {
        if let Some(raw) = Self::env_lookup(env, def) {
            let v = match def.kind {                Kind::Int { min, max, .. } => raw
                    .parse::<i64>()
                    .ok()
                    .filter(|v| (min..=max).contains(v))
                    .map(Value::from)
                    .unwrap_or_else(|| {
                        tracing::warn!(key = def.name, value = %raw, "invalid int from env, ignoring");
                        Value::Null
                    }),
                Kind::Csv | Kind::Str { .. } | Kind::OptionalStr => Value::String(raw),
                Kind::Currency => match validate_currency(&raw) {
                    Ok(norm) => Value::String(norm),
                    Err(_) => {
                        tracing::warn!(key = def.name, value = %raw, "invalid currency from env, ignoring");
                        Value::Null
                    }
                },
            };
            if v.is_null() {
                // fall through to file/default on a parse failure
            } else {
                return (v, Source::Env);
            }
        }
        if let Some(v) = self.stored.get(def.name) {
            return (v.clone(), Source::File);
        }
        let d = match def.kind {
            Kind::Int { default, .. } => Value::from(default),
            Kind::Str { default } => Value::String(default.to_string()),
            Kind::OptionalStr | Kind::Csv | Kind::Currency => Value::Null,
        };
        (d, Source::Default)
    }

    /// Convenience typed accessors used by the boot/integration sites and by
    /// handlers on the request path. An unknown key never panics — one typo
    /// would otherwise abort an axum connection instead of returning a
    /// response — it is logged and degrades to the empty default (#261).
    pub fn get_int(&self, name: &str, env: &dyn Fn(&str) -> Option<String>) -> i64 {
        let Some(def) = key_def(name) else {
            tracing::error!(key = name, "unknown config key requested");
            return 0;
        };
        match self.resolve(def, env).0 {
            Value::Number(n) => n.as_i64().unwrap_or(0),
            _ => 0,
        }
    }
    pub fn get_str(&self, name: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
        let Some(def) = key_def(name) else {
            tracing::error!(key = name, "unknown config key requested");
            return String::new();
        };
        match self.resolve(def, env).0 {
            Value::String(s) => s,
            _ => String::new(),
        }
    }
    pub fn get_optional_str(
        &self,
        name: &str,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Option<String> {
        let Some(def) = key_def(name) else {
            tracing::error!(key = name, "unknown config key requested");
            return None;
        };
        match self.resolve(def, env).0 {
            Value::String(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
    pub fn get_csv(&self, name: &str, env: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
        let Some(def) = key_def(name) else {
            tracing::error!(key = name, "unknown config key requested");
            return Vec::new();
        };
        match self.resolve(def, env).0 {
            Value::String(s) => s
                .split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Validate one whitelisted patch value against its kind. Returns the
    /// normalised value to store, or an error message for the API.
    pub fn validate(def: &'static KeyDef, value: &Value) -> Result<Value, String> {
        match (def.kind, value) {
            (Kind::Int { min, max, .. }, Value::Number(n)) => match n.as_i64() {
                Some(v) if (min..=max).contains(&v) => Ok(Value::from(v)),
                Some(v) => Err(format!("{v} outside {min}..={max}")),
                None => Err("not an integer".into()),
            },
            (Kind::Csv, Value::String(s)) | (Kind::Str { .. }, Value::String(s)) => {
                if s.len() > 500 {
                    return Err("too long".into());
                }
                Ok(Value::String(s.clone()))
            }
            (Kind::OptionalStr, Value::String(s)) => {
                if s.len() > 300 {
                    return Err("too long".into());
                }
                Ok(Value::String(s.clone()))
            }
            // ISO 4217 shaped: exactly three ASCII letters, stored canonical
            // uppercase so `eur` and `EUR` cannot both exist. `null` clears it.
            (Kind::Currency, Value::String(s)) => validate_currency(s).map(Value::String),
            (Kind::Currency, Value::Null) => Ok(Value::Null),
            _ => Err(format!(
                "{} expects {}",
                def.name,
                match def.kind {
                    Kind::Int { .. } => "an integer",
                    Kind::Csv | Kind::Str { .. } | Kind::OptionalStr => "a string",
                    Kind::Currency => "a three-letter currency code",
                }
            )),
        }
    }

    /// Merge a validated patch and write the whole file atomically.
    pub fn update(&mut self, patch: Map<String, Value>) -> Result<(), String> {
        for (k, v) in patch {
            match key_def(&k) {
                Some(def) => {
                    let v = Self::validate(def, &v)?;
                    if v.is_null() {
                        self.stored.remove(&k);
                    } else {
                        self.stored.insert(k, v);
                    }
                }
                // Unknown or secret-shaped keys are refused by the whitelist.
                None => return Err(format!("unknown configuration key: {k}")),
            }
        }
        self.persist()
    }

    fn persist(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&Value::Object(self.stored.clone()))
            .map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The effective table for `GET /api/admin/config`: every whitelisted key with
    /// its resolved value, source, and a default. Secret material is not part
    /// of the whitelist, so nothing needs masking here.
    pub fn effective(&self, env: &dyn Fn(&str) -> Option<String>) -> Vec<Value> {
        KEYS.iter()
            .map(|def| {
                let (value, source) = self.resolve(def, env);
                let default = match def.kind {
                    Kind::Int { default, .. } => Value::from(default),
                    Kind::Str { default } => Value::String(default.to_string()),
                    Kind::OptionalStr | Kind::Csv | Kind::Currency => Value::Null,
                };
                serde_json::json!({
                    "key": def.name,
                    "value": value,
                    "source": match source {
                        Source::Env => "env", Source::File => "file", Source::Default => "default"
                    },
                    "default": default,
                    "description": def.description,
                })
            })
            .collect()
    }
}

/// Validate and normalise an ISO 4217 shaped currency code: exactly three
/// ASCII letters. Returns the canonical uppercase form (so `eur` and `EUR`
/// collapse to one stored value) or an error message for the API's 422 shape.
/// This is a shape check only — the code is not resolved to a symbol or rate.
pub fn validate_currency(code: &str) -> Result<String, String> {
    let bytes = code.as_bytes();
    if bytes.len() == 3 && bytes.iter().all(u8::is_ascii_alphabetic) {
        Ok(code.to_ascii_uppercase())
    } else {
        Err("must be exactly three ASCII letters (ISO 4217)".into())
    }
}

/// Process env adapter used at the real call sites; tests inject a map.
#[must_use]
pub fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(map: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = map
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn file(json: &str) -> AppConfig {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), json).unwrap();
        AppConfig::load(dir.path()).unwrap()
    }

    #[test]
    fn precedence_env_then_file_then_default() {
        let cfg = file(r#"{"reminder_days": 14}"#);
        // default when neither set
        let empty = AppConfig::empty();
        let env_none = env_from(&[]);
        assert_eq!(
            empty
                .resolve(key_def("reminder_days").unwrap(), &env_none)
                .0,
            Value::from(7)
        );
        // file wins over default
        assert_eq!(
            cfg.resolve(key_def("reminder_days").unwrap(), &env_none).0,
            Value::from(14)
        );
        // env wins over file
        let env = env_from(&[("TUCANO_REMINDER_DAYS", "3")]);
        let (v, src) = cfg.resolve(key_def("reminder_days").unwrap(), &env);
        assert_eq!(v, Value::from(3));
        assert_eq!(src, Source::Env);
    }

    #[test]
    fn unknown_and_secret_shaped_keys_refused_update_drops_unknown_load() {
        let mut cfg = AppConfig::empty();
        let mut patch = Map::new();
        patch.insert("smtp_password".into(), Value::String("hunter2".into()));
        assert!(cfg.update(patch).is_err());
        // load silently drops keys not on the whitelist
        let loaded = file(r#"{"bogus": 1, "reminder_days": 5}"#);
        let v = loaded
            .resolve(key_def("reminder_days").unwrap(), &env_from(&[]))
            .0;
        assert_eq!(v, Value::from(5));
        assert!(!loaded.stored.contains_key("bogus"));
    }

    #[test]
    fn range_validation_and_persist_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        let mut cfg = AppConfig::load(dir.path()).unwrap();
        let mut bad = Map::new();
        bad.insert("reminder_days".into(), Value::from(0)); // below min 1
        assert!(cfg.update(bad).is_err());
        let mut good = Map::new();
        good.insert("reminder_days".into(), Value::from(21));
        good.insert("sso_admin_group".into(), Value::String("tt-admins".into()));
        cfg.update(good).unwrap();
        let reloaded = AppConfig::load(dir.path()).unwrap();
        let env = env_from(&[]);
        assert_eq!(reloaded.get_int("reminder_days", &env), 21);
        assert_eq!(reloaded.get_str("sso_admin_group", &env), "tt-admins");
    }

    #[test]
    fn csv_parsing() {
        let env = env_from(&[("TUCANO_SSO_ALLOWED_DOMAINS", "acme.test, other.test ,")]);
        assert_eq!(
            AppConfig::empty().get_csv("sso_allowed_domains", &env),
            vec!["acme.test".to_string(), "other.test".to_string()]
        );
    }

    #[test]
    fn base_currency_shape_normalises_and_rejects() {
        // Uppercase canonical form; lowercase is normalised, not rejected.
        assert_eq!(validate_currency("eur").unwrap(), "EUR");
        assert_eq!(validate_currency("USD").unwrap(), "USD");
        // Wrong shape is refused: too short, too long, digit, empty, non-ASCII.
        assert!(validate_currency("US").is_err());
        assert!(validate_currency("USDD").is_err());
        assert!(validate_currency("12A").is_err());
        assert!(validate_currency("").is_err());
        assert!(validate_currency("€UR").is_err());

        // The whitelisted key is absent (unset) by default: no base currency,
        // so nothing is fabricated and the Trends conversion stays off.
        let cfg = AppConfig::empty();
        let env = env_from(&[]);
        let def = key_def("base_currency").unwrap();
        let (value, source) = cfg.resolve(def, &env);
        assert_eq!(value, Value::Null);
        assert_eq!(source, Source::Default);

        // A valid code round-trips and is stored uppercase; a bad one is
        // refused by validate() and never reaches the store.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        let mut cfg = AppConfig::load(dir.path()).unwrap();
        let mut good = Map::new();
        good.insert("base_currency".into(), Value::String("gbp".into()));
        cfg.update(good).unwrap();
        let reloaded = AppConfig::load(dir.path()).unwrap();
        assert_eq!(
            reloaded.get_optional_str("base_currency", &env),
            Some("GBP".to_string())
        );
        let mut bad = Map::new();
        bad.insert("base_currency".into(), Value::String("USDD".into()));
        assert!(cfg.update(bad).is_err());
        assert_eq!(
            AppConfig::load(dir.path())
                .unwrap()
                .get_optional_str("base_currency", &env),
            Some("GBP".to_string()),
            "a refused patch must persist nothing"
        );
    }
}
