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
    /// Strict `true`/`false`. Env accepts both spellings case-insensitively;
    /// anything else is ignored (and, in a `PUT`, refused).
    Bool {
        default: bool,
    },
    /// One of a fixed set of lower-case tokens. File/env values are matched
    /// case-insensitively and stored in lower case; anything else is refused.
    Choice {
        values: &'static [&'static str],
        default: &'static str,
    },
    /// Wall-clock `HH:MM` in 24-hour form, stored zero-padded. Scheduling reads
    /// it as a time in the server's local time zone.
    TimeOfDay {
        default: &'static str,
    },
}

pub struct KeyDef {
    /// Key inside config.json.
    pub name: &'static str,
    /// Overriding environment variable.
    pub env: &'static str,
    pub kind: Kind,
    /// Whether `PUT /api/admin/config` may change this key. A read-only key is
    /// still listed by `GET /api/admin/config`; its dedicated lifecycle
    /// endpoint is the only writer (M3: `vault_enabled`).
    pub writable: bool,
    pub description: &'static str,
}

/// The complete whitelist of externalisable knobs. Anything not here cannot be
/// read from or written to config.json.
pub const KEYS: &[KeyDef] = &[
    KeyDef {
        name: "reminder_days",
        writable: true,
        env: "TATU_REMINDER_DAYS",
        kind: Kind::Int {
            min: 1,
            max: 365,
            default: 7,
        },
        description: "Days between overdue-invoice email reminders",
    },
    KeyDef {
        name: "org_name",
        writable: true,
        env: "TATU_ORG_NAME",
        kind: Kind::Str {
            default: "TatuTime",
        },
        description: "Fallback organisation name, used only when the Company name is blank",
    },
    KeyDef {
        name: "sso_admin_group",
        writable: true,
        env: "TATU_SSO_ADMIN_GROUP",
        kind: Kind::Str { default: "" },
        description: "IdP group that maps to the admin role on SSO login",
    },
    KeyDef {
        name: "sso_allowed_domains",
        writable: true,
        env: "TATU_SSO_ALLOWED_DOMAINS",
        kind: Kind::Csv,
        description: "Comma-separated email domains allowed for SSO",
    },
    KeyDef {
        name: "calendar_oauth_redirect",
        writable: true,
        env: "TATU_CALENDAR_OAUTH_REDIRECT",
        kind: Kind::Str {
            default: "/api/calendar/oauth/callback",
        },
        description: "Redirect URI registered with the OAuth calendar app",
    },
    KeyDef {
        name: "google_calendar_client_id",
        writable: true,
        env: "TATU_GOOGLE_CLIENT_ID",
        kind: Kind::OptionalStr,
        description: "Google OAuth client id (public half of the pair; the secret stays env/vault)",
    },
    KeyDef {
        name: "ms_calendar_client_id",
        writable: true,
        env: "TATU_MS_CLIENT_ID",
        kind: Kind::OptionalStr,
        description: "Microsoft OAuth client id (secret stays env/vault)",
    },
    KeyDef {
        name: "qbo_base_url",
        writable: true,
        env: "TATU_QBO_BASE_URL",
        kind: Kind::OptionalStr,
        description: "Override the QuickBooks API base URL",
    },
    KeyDef {
        name: "xero_base_url",
        writable: true,
        env: "TATU_XERO_BASE_URL",
        kind: Kind::OptionalStr,
        description: "Override the Xero API base URL",
    },
    KeyDef {
        name: "max_docs",
        writable: true,
        env: "TATU_MAX_DOCS",
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
        writable: true,
        env: "TATU_BASE_CURRENCY",
        kind: Kind::Currency,
        description: "Optional base currency for mixed-currency invoice Trends (ISO 4217, e.g. EUR); unset disables the conversion",
    },
    // Scheduled backups (#new): the three knobs behind Settings → Backup.
    // `backup_enabled` gates the job; `backup_cadence` and `backup_time` say
    // when it runs. The time is interpreted in the server's local time zone,
    // which the admin API reports alongside these values.
    KeyDef {
        name: "backup_enabled",
        writable: true,
        env: "TATU_BACKUP_ENABLED",
        kind: Kind::Bool { default: false },
        description: "Run scheduled ZIP backups of the data directory",
    },
    KeyDef {
        name: "backup_cadence",
        writable: true,
        env: "TATU_BACKUP_CADENCE",
        kind: Kind::Choice {
            values: &["daily", "weekly", "monthly"],
            default: "daily",
        },
        description: "Backup cadence: daily, weekly (Monday) or monthly (1st)",
    },
    KeyDef {
        name: "backup_time",
        writable: true,
        env: "TATU_BACKUP_TIME",
        kind: Kind::TimeOfDay { default: "02:00" },
        description: "Backup time of day (HH:MM) in the server's local time zone",
    },
    // Credential-vault enablement: the key is created by default, so this knob
    // only exists so an operator (or the admin API) can turn the vault off.
    // It is not a secret and carries no key material.
    KeyDef {
        name: "vault_enabled",
        // M3: the dedicated lifecycle endpoints are the only writer; a generic
        // PUT must not brick the next boot by disabling a vault with a store.
        writable: false,
        env: "TATU_VAULT_ENABLED",
        kind: Kind::Bool { default: true },
        description: "Enable the credential vault (a master key is created on first boot); env override or stored configuration",
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

/// A refused `PUT /admin/config`: the offending key (when there is one) and a
/// client-safe message. The key becomes the `FieldError.field`, so the caller
/// learns which knob failed without any filesystem detail (SET-13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub key: Option<String>,
    pub message: String,
}

impl ConfigError {
    fn keyed(key: &str, message: impl Into<String>) -> Self {
        Self {
            key: Some(key.to_string()),
            message: message.into(),
        }
    }
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
                Kind::Choice { values, .. } => {
                    let norm = raw.trim().to_ascii_lowercase();
                    if values.contains(&norm.as_str()) {
                        Value::String(norm)
                    } else {
                        tracing::warn!(key = def.name, value = %raw, "invalid choice from env, ignoring");
                        Value::Null
                    }
                }
                Kind::TimeOfDay { .. } => match parse_time_of_day(&raw) {
                    Some((h, m)) => Value::String(format!("{h:02}:{m:02}")),
                    None => {
                        tracing::warn!(key = def.name, value = %raw, "invalid time from env, ignoring");
                        Value::Null
                    }
                },
                Kind::Currency => match validate_currency(&raw) {
                    Ok(norm) => Value::String(norm),
                    Err(_) => {
                        tracing::warn!(key = def.name, value = %raw, "invalid currency from env, ignoring");
                        Value::Null
                    }
                },
                Kind::Bool { .. } => {
                    if raw.eq_ignore_ascii_case("true") {
                        Value::Bool(true)
                    } else if raw.eq_ignore_ascii_case("false") {
                        Value::Bool(false)
                    } else {
                        tracing::warn!(key = def.name, value = %raw, "invalid bool from env, ignoring");
                        Value::Null
                    }
                }
            };
            if v.is_null() {
                // fall through to file/default on a parse failure
            } else {
                return (v, Source::Env);
            }
        }
        if let Some(v) = self.stored.get(def.name) {
            // A hand-edited config.json is still an input: a Choice/TimeOfDay
            // value that no longer parses falls through to the default instead
            // of silently disabling the feature.
            let valid = match (def.kind, v) {
                (Kind::Choice { values, .. }, Value::String(s)) => values.contains(&s.as_str()),
                (Kind::TimeOfDay { .. }, Value::String(s)) => parse_time_of_day(s).is_some(),
                _ => true,
            };
            if valid {
                return (v.clone(), Source::File);
            }
            tracing::warn!(key = def.name, value = %v, "invalid value in config.json, using default");
        }
        let d = match def.kind {
            Kind::Int { default, .. } => Value::from(default),
            Kind::Str { default } => Value::String(default.to_string()),
            Kind::OptionalStr | Kind::Csv | Kind::Currency => Value::Null,
            Kind::Bool { default } => Value::Bool(default),
            Kind::Choice { default, .. } | Kind::TimeOfDay { default } => {
                Value::String(default.to_string())
            }
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
    pub fn get_bool(&self, name: &str, env: &dyn Fn(&str) -> Option<String>) -> bool {
        let Some(def) = key_def(name) else {
            tracing::error!(key = name, "unknown config key requested");
            return false;
        };
        matches!(self.resolve(def, env).0, Value::Bool(true))
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
    /// normalised value to store, or an error message for the API. `Str`/`Csv`
    /// values are trimmed, and a `Str` whose kind carries a non-empty default
    /// (a required identity such as `org_name`) refuses a whitespace-only value
    /// instead of persisting a blank that would render an empty issuer (#SET-13).
    pub fn validate(def: &'static KeyDef, value: &Value) -> Result<Value, String> {
        match (def.kind, value) {
            (Kind::Int { min, max, .. }, Value::Number(n)) => match n.as_i64() {
                Some(v) if (min..=max).contains(&v) => Ok(Value::from(v)),
                Some(v) => Err(format!("{v} outside {min}..={max}")),
                None => Err("not an integer".into()),
            },
            (Kind::Csv, Value::String(s)) | (Kind::Str { .. }, Value::String(s)) => {
                let trimmed = s.trim();
                if trimmed.len() > 500 {
                    return Err("too long".into());
                }
                if let Kind::Str { default } = def.kind
                    && !default.is_empty()
                    && trimmed.is_empty()
                {
                    return Err(format!("{} must not be blank", def.name));
                }
                Ok(Value::String(trimmed.to_string()))
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
            (Kind::Choice { values, .. }, Value::String(s)) => {
                let norm = s.trim().to_ascii_lowercase();
                if values.contains(&norm.as_str()) {
                    Ok(Value::String(norm))
                } else {
                    Err(format!("must be one of {}", values.join(", ")))
                }
            }
            (Kind::TimeOfDay { .. }, Value::String(s)) => match parse_time_of_day(s) {
                Some((h, m)) => Ok(Value::String(format!("{h:02}:{m:02}"))),
                None => Err("must be a 24-hour time in HH:MM form".into()),
            },
            (Kind::Bool { .. }, Value::Bool(b)) => Ok(Value::Bool(*b)),
            _ => Err(format!(
                "{} expects {}",
                def.name,
                match def.kind {
                    Kind::Int { .. } => "an integer",
                    Kind::Csv | Kind::Str { .. } | Kind::OptionalStr => "a string",
                    Kind::Currency => "a three-letter currency code",
                    Kind::Bool { .. } => "true or false",
                    Kind::Choice { .. } => "one of the allowed values",
                    Kind::TimeOfDay { .. } => "a 24-hour time in HH:MM form",
                }
            )),
        }
    }

    /// Merge a validated patch and write the whole file atomically.
    pub fn update(&mut self, patch: Map<String, Value>) -> Result<(), ConfigError> {
        for (k, v) in patch {
            match key_def(&k) {
                Some(def) => {
                    if !def.writable {
                        return Err(ConfigError::keyed(
                            &k,
                            format!(
                                "{k} is managed by the vault lifecycle endpoints; use \
                                 POST /api/admin/vault/enable or POST /api/admin/vault/disable"
                            ),
                        ));
                    }
                    let v = Self::validate(def, &v).map_err(|m| ConfigError::keyed(&k, m))?;
                    if v.is_null() {
                        self.stored.remove(&k);
                    } else {
                        self.stored.insert(k, v);
                    }
                }
                // Unknown or secret-shaped keys are refused by the whitelist.
                None => {
                    return Err(ConfigError::keyed(
                        &k,
                        format!("unknown configuration key: {k}"),
                    ));
                }
            }
        }
        self.persist()
            .map_err(|message| ConfigError { key: None, message })
    }

    /// Persist a lifecycle flag that `PUT /api/admin/config` is not allowed to
    /// write (M3). The dedicated vault endpoints are the only callers; the
    /// value still goes through the same kind validation as a generic patch.
    pub fn set_lifecycle_bool(&mut self, name: &str, value: bool) -> Result<(), ConfigError> {
        let def = key_def(name).ok_or_else(|| ConfigError {
            key: Some(name.to_string()),
            message: format!("unknown configuration key: {name}"),
        })?;
        let stored =
            Self::validate(def, &Value::Bool(value)).map_err(|m| ConfigError::keyed(name, m))?;
        self.stored.insert(name.to_string(), stored);
        self.persist()
            .map_err(|message| ConfigError { key: None, message })
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
                    Kind::Bool { default } => Value::Bool(default),
                    Kind::Choice { default, .. } | Kind::TimeOfDay { default } => {
                        Value::String(default.to_string())
                    }
                };
                serde_json::json!({
                    "key": def.name,
                    "value": value,
                    "source": match source {
                        Source::Env => "env", Source::File => "file", Source::Default => "default"
                    },
                    "default": default,
                    "writable": def.writable,
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

/// Parse a 24-hour wall-clock time in `HH:MM` (leading zero optional) and
/// return `(hour, minute)` in `0..=23`, `0..=59`. The scheduled-backup job
/// interprets the pair in the server's local time zone.
#[must_use]
pub fn parse_time_of_day(value: &str) -> Option<(u32, u32)> {
    let (hour, minute) = value.trim().split_once(':')?;
    if hour.is_empty() || minute.is_empty() || minute.len() > 2 {
        return None;
    }
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
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
        let env = env_from(&[("TATU_REMINDER_DAYS", "3")]);
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
        let env = env_from(&[("TATU_SSO_ALLOWED_DOMAINS", "tatutime.test, other.test ,")]);
        assert_eq!(
            AppConfig::empty().get_csv("sso_allowed_domains", &env),
            vec!["tatutime.test".to_string(), "other.test".to_string()]
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

    #[test]
    fn bool_precedence_defaults_and_invalid_values() {
        let def = key_def("vault_enabled").expect("vault_enabled is whitelisted");
        let env_none = env_from(&[]);
        assert_eq!(
            AppConfig::empty().resolve(def, &env_none),
            (Value::Bool(true), Source::Default),
            "the vault defaults to enabled"
        );

        // file beats default
        let cfg = file(r#"{"vault_enabled": false}"#);
        assert_eq!(
            cfg.resolve(def, &env_none),
            (Value::Bool(false), Source::File)
        );
        assert!(!cfg.get_bool("vault_enabled", &env_none));

        // env beats file, in either spelling
        for raw in ["true", "TRUE", "True"] {
            let pairs = [("TATU_VAULT_ENABLED", raw)];
            let env = env_from(&pairs);
            assert_eq!(
                cfg.resolve(def, &env),
                (Value::Bool(true), Source::Env),
                "{raw}"
            );
        }
        let env = env_from(&[("TATU_VAULT_ENABLED", "false")]);
        assert!(!AppConfig::empty().get_bool("vault_enabled", &env));

        // An invalid env value is ignored, so the file/default still wins.
        let invalid = env_from(&[("TATU_VAULT_ENABLED", "yes")]);
        assert_eq!(
            cfg.resolve(def, &invalid),
            (Value::Bool(false), Source::File),
            "invalid env bool must not leak into the effective value"
        );

        // Writes accept a JSON boolean only and persist nothing on rejection.
        assert_eq!(
            AppConfig::validate(def, &Value::Bool(false)).unwrap(),
            Value::Bool(false)
        );
        assert!(AppConfig::validate(def, &Value::String("false".into())).is_err());
        assert!(AppConfig::validate(def, &Value::from(0)).is_err());

        // M3: the generic config endpoint is not a writer for this knob. The
        // dedicated lifecycle endpoints are, because a stored `false` next to a
        // `secrets.bin` would hard-fail the next boot.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        let mut cfg = AppConfig::load(dir.path()).unwrap();
        let mut patch = Map::new();
        patch.insert("vault_enabled".into(), Value::Bool(false));
        let err = cfg.update(patch).unwrap_err();
        assert_eq!(err.key.as_deref(), Some("vault_enabled"));
        assert!(
            err.message.contains("/api/admin/vault"),
            "the refusal must point at the lifecycle endpoints: {err:?}"
        );
        assert!(
            AppConfig::load(dir.path())
                .unwrap()
                .stored
                .get("vault_enabled")
                .is_none(),
            "a refused PUT must persist nothing"
        );

        // `GET /api/admin/config` still lists it, flagged read-only.
        let rows = AppConfig::empty().effective(&env_none);
        let row = rows.iter().find(|r| r["key"] == "vault_enabled").unwrap();
        assert_eq!(row["writable"], false);
        let days = rows.iter().find(|r| r["key"] == "reminder_days").unwrap();
        assert_eq!(days["writable"], true);
    }

    #[test]
    fn backup_keys_are_validated_and_normalised() {
        let env_none = env_from(&[]);
        let cadence = key_def("backup_cadence").unwrap();
        let time = key_def("backup_time").unwrap();

        // Defaults describe the documented feature.
        assert_eq!(
            AppConfig::empty().resolve(cadence, &env_none).0,
            Value::String("daily".into())
        );
        assert_eq!(
            AppConfig::empty().resolve(time, &env_none).0,
            Value::String("02:00".into())
        );

        // Case-insensitive choices and shorthand times normalise on write.
        assert_eq!(
            AppConfig::validate(cadence, &Value::String("WEEKLY".into())).unwrap(),
            Value::String("weekly".into())
        );
        assert_eq!(
            AppConfig::validate(time, &Value::String("3:5".into())).unwrap(),
            Value::String("03:05".into())
        );
        assert!(AppConfig::validate(cadence, &Value::String("hourly".into())).is_err());
        assert!(AppConfig::validate(time, &Value::String("24:00".into())).is_err());
        assert!(AppConfig::validate(time, &Value::String("02:60".into())).is_err());
        assert!(AppConfig::validate(time, &Value::String("2pm".into())).is_err());

        // Env overrides parse through the same rules.
        let env = env_from(&[
            ("TATU_BACKUP_CADENCE", "Monthly"),
            ("TATU_BACKUP_TIME", "07:15"),
            ("TATU_BACKUP_ENABLED", "true"),
        ]);
        let cfg = AppConfig::empty();
        assert_eq!(
            cfg.resolve(cadence, &env),
            (Value::String("monthly".into()), Source::Env)
        );
        assert_eq!(
            cfg.resolve(time, &env),
            (Value::String("07:15".into()), Source::Env)
        );
        assert!(cfg.get_bool("backup_enabled", &env));

        // A hand-edited invalid file value degrades to the default, never to a
        // silently broken schedule.
        let loaded = file(r#"{"backup_cadence": "hourly", "backup_time": "99:99"}"#);
        assert_eq!(
            loaded.resolve(cadence, &env_none),
            (Value::String("daily".into()), Source::Default)
        );
        assert_eq!(
            loaded.resolve(time, &env_none),
            (Value::String("02:00".into()), Source::Default)
        );
    }

    #[test]
    fn time_of_day_parser_bounds() {
        assert_eq!(parse_time_of_day("00:00"), Some((0, 0)));
        assert_eq!(parse_time_of_day("23:59"), Some((23, 59)));
        assert_eq!(parse_time_of_day("7:5"), Some((7, 5)));
        assert_eq!(parse_time_of_day("24:00"), None);
        assert_eq!(parse_time_of_day("12:60"), None);
        assert_eq!(parse_time_of_day("noon"), None);
        assert_eq!(parse_time_of_day(""), None);
    }
}
