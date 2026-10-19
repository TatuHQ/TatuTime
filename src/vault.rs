//! Encrypted credential vault (#77). Admin-entered integration secrets
//! (SMTP passwords, OAuth client secrets, calendar feed URLs, …) are
//! stored **encrypted at rest** with AES-256-GCM under a master key.
//!
//! The key is created by default on first boot (`<data>/vault.key`, 24 CSPRNG
//! bytes encoded base64url without padding — exactly 32 ASCII characters).
//! `TUCANO_SECRET_KEY` / `TUCANO_SECRET_KEY_FILE` remain an optional override
//! and always win. Values are never returned to any client (only a masked hint)
//! and never logged. If no key can be resolved or created the vault is disabled
//! (fail-closed) rather than storing plaintext.
//!
//! The whole key→value map is encrypted as one blob (`secrets.bin` =
//! nonce ‖ ciphertext). Integrations consume it via [`SecretVault::get`].

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, Generate, KeyInit, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
/// Raw CSPRNG bytes behind the generated 32-character key (192 bits).
const GENERATED_KEY_BYTES: usize = 24;

/// Where the active master key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Env,
    EnvFile,
    KeyFile,
}

impl KeySource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::EnvFile => "env_file",
            Self::KeyFile => "key_file",
        }
    }
}

/// Why the vault is not usable at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    /// `vault_enabled` is false in configuration.
    Config,
    /// `TUCANO_VAULT_ENABLED` resolved from the environment to a valid
    /// `false` (an invalid value is ignored instead, L2).
    Env,
    /// The generated key file could not be written (read-only volume, …).
    WriteFailed,
}

impl DisabledReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Env => "env",
            Self::WriteFailed => "write_failed",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error(
        "vault is disabled (enable it in Settings → Security or set TUCANO_VAULT_ENABLED=true)"
    )]
    Disabled,
    #[error("invalid master key")]
    BadKey,
    #[error("could not decrypt the vault (wrong key or corrupt file)")]
    Decrypt,
    #[error("io failure")]
    Io,
}

pub struct SecretVault {
    path: PathBuf,
    key: [u8; KEY_LEN],
    source: KeySource,
    fingerprint: String,
    data: Mutex<BTreeMap<String, String>>,
    /// Distinguishes this instance's temp file from any other live vault that
    /// writes the same `secrets.bin`. The process-wide gate already serialises
    /// the admin paths; a per-instance tag makes the temp name collision-proof
    /// even if an embedding opens the store twice.
    tmp_tag: u64,
}

impl std::fmt::Debug for SecretVault {
    /// Never include the key (or the ciphertext map) in a debug rendering.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretVault")
            .field("source", &self.source)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// The resolved boot-time vault state. `vault` and `source` are `Some` exactly
/// when the vault is enabled; `disabled` names the reason otherwise. The raw
/// key never appears here — only its fingerprint.
/// The resolved boot-time vault state. `vault` and `source` are `Some` exactly
/// when the vault is enabled; `disabled` names the reason otherwise. The raw
/// key never appears here — only its fingerprint.
#[derive(Debug, Clone)]
pub struct VaultBoot {
    pub vault: Option<Arc<SecretVault>>,
    pub source: Option<KeySource>,
    pub disabled: Option<DisabledReason>,
    /// `<root>/vault.key` when that file was used or created.
    pub key_file: Option<PathBuf>,
    pub fingerprint: Option<String>,
    /// An env key (`TUCANO_SECRET_KEY` or `TUCANO_SECRET_KEY_FILE`) is set, so
    /// the operator owns the key out-of-band and the admin API cannot rotate or
    /// remove it.
    pub external_override: bool,
}

impl VaultBoot {
    /// A boot state where no vault is available for the given reason.
    #[must_use]
    pub fn disabled(reason: DisabledReason) -> Self {
        Self {
            vault: None,
            source: None,
            disabled: Some(reason),
            key_file: None,
            fingerprint: None,
            external_override: false,
        }
    }

    /// Wrap an opened vault, copying its source and fingerprint so the status
    /// view never has to reach for the key material.
    #[must_use]
    pub fn from_vault(
        vault: Arc<SecretVault>,
        source: KeySource,
        key_file: Option<PathBuf>,
        external_override: bool,
    ) -> Self {
        let fingerprint = Some(vault.fingerprint().to_string());
        Self {
            vault: Some(vault),
            source: Some(source),
            disabled: None,
            key_file,
            fingerprint,
            external_override,
        }
    }
}

/// A live, swappable vault handle. `AppState` holds this so the admin API can
/// enable, disable or regenerate the key without a restart.
pub struct VaultSlot {
    inner: RwLock<VaultBoot>,
    /// Process-wide serialisation of key lifecycle changes against secret
    /// writes (H1). A `PUT /api/admin/secrets` that took its `Arc<SecretVault>`
    /// before an enable/disable/regenerate would otherwise persist under a key
    /// that is no longer installed.
    gate: Mutex<()>,
}

impl VaultSlot {
    #[must_use]
    pub fn new(boot: VaultBoot) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(boot),
            gate: Mutex::new(()),
        })
    }

    /// Serialise every secret write against a key lifecycle change.
    ///
    /// Callers acquire this **before** reading the status or resolving the
    /// vault, and hold it through the whole read-check-write-install sequence.
    /// `list_secrets` and `GET /api/admin/vault` never take it. Poisoning is
    /// ignored: a panic in one handler must not wedge the admin API forever.
    pub fn gate(&self) -> std::sync::MutexGuard<'_, ()> {
        match self.gate.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    #[must_use]
    pub fn disabled(reason: DisabledReason) -> Arc<Self> {
        Self::new(VaultBoot::disabled(reason))
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, VaultBoot> {
        match self.inner.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// The current vault, if enabled.
    #[must_use]
    pub fn get(&self) -> Option<Arc<SecretVault>> {
        self.read().vault.clone()
    }

    /// Snapshot of the current boot state.
    #[must_use]
    pub fn status(&self) -> VaultBoot {
        self.read().clone()
    }

    /// Replace the boot state (enable/disable/regenerate).
    pub fn install(&self, boot: VaultBoot) {
        match self.inner.write() {
            Ok(mut g) => *g = boot,
            Err(p) => *p.into_inner() = boot,
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.read().vault.is_some()
    }
}

/// Boot-time vault resolution (#94, credential-vault defaults).
///
/// Resolution order, first match wins:
///
/// 1. `enabled == false` → disabled; a store on disk is a hard error because it
///    would be unreadable, and configured key material is ignored with a warning.
/// 2. `TUCANO_SECRET_KEY` non-empty → used verbatim (exactly 32 bytes).
/// 3. `TUCANO_SECRET_KEY_FILE` non-empty → read + trim (exactly 32 bytes).
///    Unreadable is a hard error, except when it names `<root>/vault.key`, no
///    store exists and the file is missing: it is then generated there.
/// 4. `<root>/vault.key` exists → read + trim (exactly 32 bytes, never
///    overwritten).
/// 5. `secrets.bin` exists without a key → hard error (fail-closed).
/// 6. Otherwise generate `<root>/vault.key` (0600, atomic). A write failure
///    degrades to [`DisabledReason::WriteFailed`] and boot continues.
///
/// The returned error strings are operator-facing, not API-facing.
pub fn open_for_boot(
    root: &Path,
    env_key: Option<String>,
    env_key_file: Option<String>,
    enabled: bool,
) -> Result<VaultBoot, String> {
    let store = root.join("secrets.bin");
    let has_store = store.exists();
    let key_path = root.join("vault.key");
    let env_key = env_key.filter(|s| !s.is_empty());
    let env_key_file = env_key_file.filter(|s| !s.is_empty());

    if let (Some(inline), Some(file)) = (&env_key, &env_key_file)
        && let Ok(content) = fs::read_to_string(file)
        && content.trim() != inline
    {
        tracing::warn!(
            "both TUCANO_SECRET_KEY and TUCANO_SECRET_KEY_FILE are set and differ; using TUCANO_SECRET_KEY"
        );
    }

    if !enabled {
        if has_store {
            return Err(store_without_key_message());
        }
        let external_override = env_key.is_some() || env_key_file.is_some();
        if external_override {
            tracing::warn!(
                "the credential vault is disabled; ignoring configured TUCANO_SECRET_KEY(_FILE)"
            );
        }
        let mut boot = VaultBoot::disabled(DisabledReason::Config);
        boot.external_override = external_override;
        return Ok(boot);
    }

    let external_override = env_key.is_some() || env_key_file.is_some();

    if let Some(raw) = env_key {
        let vault = open_with_message(root, &raw, KeySource::Env)
            .map_err(|e| format!("TUCANO_SECRET_KEY is configured but invalid: {e}"))?;
        return Ok(VaultBoot::from_vault(
            Arc::new(vault),
            KeySource::Env,
            None,
            external_override,
        ));
    }

    if let Some(file) = env_key_file {
        let candidate = Path::new(&file);
        if !candidate.exists() {
            // Carve-out: the env points at the managed key file, which is
            // simply missing on first boot. Generate it instead of failing.
            if same_path(&file, &key_path) && !has_store {
                return generate_key_file(root, external_override);
            }
            return Err(format!(
                "cannot read TUCANO_SECRET_KEY_FILE {file}: file does not exist"
            ));
        }
        let content = read_existing_key_file(candidate)
            .map_err(|e| format!("TUCANO_SECRET_KEY_FILE {file} is unusable: {e}"))?;
        let raw = content.trim();
        if raw.len() != KEY_LEN {
            return Err(format!(
                "TUCANO_SECRET_KEY_FILE {file} is configured but invalid: invalid master key"
            ));
        }
        let vault = open_with_message(root, raw, KeySource::EnvFile)
            .map_err(|e| format!("TUCANO_SECRET_KEY_FILE {file} is invalid: {e}"))?;
        return Ok(VaultBoot::from_vault(
            Arc::new(vault),
            KeySource::EnvFile,
            None,
            external_override,
        ));
    }

    if key_path.exists() {
        let content = read_existing_key_file(&key_path)
            .map_err(|e| format!("cannot read the vault key file: {e}"))?;
        let raw = content.trim();
        if raw.len() != KEY_LEN {
            return Err(
                "the vault key file is invalid: invalid master key (refusing to overwrite it)"
                    .to_string(),
            );
        }
        let vault = open_with_message(root, raw, KeySource::KeyFile)
            .map_err(|e| format!("the vault key file is invalid: {e}"))?;
        return Ok(VaultBoot::from_vault(
            Arc::new(vault),
            KeySource::KeyFile,
            Some(key_path),
            external_override,
        ));
    }

    if has_store {
        return Err(store_without_key_message());
    }

    generate_key_file(root, external_override)
}

/// Read an existing key file, tightening a group/world-accessible mode to
/// `0600` (M4). A key another local user can read is a hard boot failure when
/// the tighter mode cannot be applied (read-only volume, immutable file, …):
/// the alternative is running with a key the operator believes is private.
fn read_existing_key_file(path: &Path) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;

    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms).map_err(|e| {
            format!(
                "the key file is accessible by other users (mode {mode:03o}) and could not be \
                 tightened to 0600: {e} — fix its permissions before starting"
            )
        })?;
        tracing::warn!(
            path = %path.display(),
            previous_mode = format!("{mode:03o}"),
            "tightened the vault key file permissions to 0600"
        );
    }
    fs::read_to_string(path).map_err(|e| e.to_string())
}

fn store_without_key_message() -> String {
    "vault store found on disk (secrets.bin) but TUCANO_SECRET_KEY(_FILE) is missing \
     — supply the key that protected it (or restore it from backup) before starting; \
     refusing to run with a disabled Settings tab"
        .to_string()
}

fn open_with_message(root: &Path, raw: &str, source: KeySource) -> Result<SecretVault, String> {
    match SecretVault::open_with(root, raw, source) {
        Ok(vault) => Ok(vault),
        Err(e) if root.join("secrets.bin").exists() => Err(format!(
            "vault store exists but could not be opened: {e} — check the configured key; \
             the app will not start with an unreadable Settings tab"
        )),
        Err(e) => Err(e.to_string()),
    }
}

/// Whether `candidate` names the same file as the managed `key_path`, even when
/// the file does not exist yet (canonicalise the parent, compare the names).
fn same_path(candidate: &str, key_path: &Path) -> bool {
    let candidate = Path::new(candidate);
    if candidate == key_path {
        return true;
    }
    match (candidate.parent(), key_path.parent()) {
        (Some(cp), Some(kp)) if candidate.file_name() == key_path.file_name() => {
            match (fs::canonicalize(cp), fs::canonicalize(kp)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
        }
        _ => false,
    }
}

/// Create `<root>/vault.key` with fresh CSPRNG material (0600, atomic) and open
/// the vault on it. A write failure is not fatal: the caller boots with the
/// vault disabled (`WriteFailed`) and nothing is stored in plaintext.
fn generate_key_file(root: &Path, external_override: bool) -> Result<VaultBoot, String> {
    match write_new_key_file(root) {
        Ok(raw) => {
            let vault = SecretVault::open_with(root, &raw, KeySource::KeyFile)
                .map_err(|e| format!("the generated vault key could not be opened: {e}"))?;
            let fingerprint = vault.fingerprint().to_string();
            tracing::info!(
                file = "vault.key",
                fingerprint = %fingerprint,
                "created a new credential-vault key"
            );
            Ok(VaultBoot::from_vault(
                Arc::new(vault),
                KeySource::KeyFile,
                Some(root.join("vault.key")),
                external_override,
            ))
        }
        Err(e) => {
            tracing::error!(
                "could not create vault.key ({e}); the credential vault stays disabled — \
                 check the data directory's permissions or set TUCANO_SECRET_KEY(_FILE)"
            );
            let mut boot = VaultBoot::disabled(DisabledReason::WriteFailed);
            boot.external_override = external_override;
            Ok(boot)
        }
    }
}

/// Generate and atomically write `<root>/vault.key` with mode 0600.
///
/// Returns the raw 32-character key so the caller can open the vault; it is
/// never logged or serialised. 24 CSPRNG bytes encode to exactly 32 ASCII
/// characters with base64url-no-pad (192 bits of entropy).
pub(crate) fn write_new_key_file(root: &Path) -> Result<String, VaultError> {
    use rand::TryRng;

    let mut bytes = [0u8; GENERATED_KEY_BYTES];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| VaultError::Io)?;
    let key = URL_SAFE_NO_PAD.encode(bytes);
    debug_assert_eq!(key.len(), KEY_LEN);
    crate::auth::write_private_file(&root.join("vault.key"), key.as_bytes())
        .map_err(|_| VaultError::Io)?;
    Ok(key)
}

impl SecretVault {
    /// Open (or create) the vault. `master_key` should come from
    /// `TUCANO_SECRET_KEY` (exactly 32 bytes). Returns `Ok(None)` when no key
    /// is provided — the caller treats the feature as disabled.
    pub fn open(root: &Path, master_key: Option<&str>) -> Result<Option<Self>, VaultError> {
        match master_key.filter(|s| !s.is_empty()) {
            Some(raw) => Self::open_with(root, raw, KeySource::Env).map(Some),
            None => Ok(None),
        }
    }

    /// Open the vault with a known key source. The key must be exactly 32
    /// bytes; the raw key is copied into the struct and never exposed.
    pub fn open_with(root: &Path, master_key: &str, source: KeySource) -> Result<Self, VaultError> {
        let key_bytes = master_key.as_bytes();
        if key_bytes.len() != KEY_LEN {
            return Err(VaultError::BadKey);
        }
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(key_bytes);
        let fingerprint = fingerprint_of(&key);

        let path = root.join("secrets.bin");
        let data = if path.exists() {
            let blob = fs::read(&path).map_err(|_| VaultError::Io)?;
            let json = Self::decrypt(&key, &blob)?;
            serde_json::from_slice(&json).map_err(|_| VaultError::Decrypt)?
        } else {
            BTreeMap::new()
        };
        static TMP_TAG: AtomicU64 = AtomicU64::new(0);

        Ok(Self {
            path,
            key,
            source,
            fingerprint,
            data: Mutex::new(data),
            tmp_tag: TMP_TAG.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// How the active key was supplied.
    #[must_use]
    pub fn source(&self) -> KeySource {
        self.source
    }

    /// `sha256:<first 4 bytes hex>` — safe to display, useless for recovery.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new_from_slice(&self.key).expect("32-byte key")
    }

    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, VaultError> {
        let nonce = Nonce::<Aes256Gcm>::generate();
        let ct = self
            .cipher()
            .encrypt(&nonce, plaintext)
            .map_err(|_| VaultError::Decrypt)?;
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ct);
        Ok(blob)
    }

    fn decrypt(key: &[u8; KEY_LEN], blob: &[u8]) -> Result<Vec<u8>, VaultError> {
        if blob.len() < NONCE_LEN {
            return Err(VaultError::Decrypt);
        }
        let (nonce_bytes, ct) = blob.split_at(NONCE_LEN);
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| VaultError::BadKey)?;
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce_bytes).map_err(|_| VaultError::Decrypt)?;
        cipher.decrypt(&nonce, ct).map_err(|_| VaultError::Decrypt)
    }

    fn persist(&self, map: &BTreeMap<String, String>) -> Result<(), VaultError> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let json = serde_json::to_vec(map).map_err(|_| VaultError::Decrypt)?;
        let blob = self.encrypt(&json)?;
        // tmp+rename: a concurrent backup must never capture a half-written
        // encrypted map (review B7). The tag makes the temp name unique per
        // vault instance, and the blob is fsynced before the rename (L5).
        let tmp =
            self.path
                .with_extension(format!("bin.{}.{}.tmp", std::process::id(), self.tmp_tag));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|_| VaultError::Io)?;
        let flushed = file.write_all(&blob).and_then(|_| file.sync_all());
        drop(file);
        if flushed.is_err() {
            let _ = fs::remove_file(&tmp);
            return Err(VaultError::Io);
        }
        if fs::rename(&tmp, &self.path).is_err() {
            let _ = fs::remove_file(&tmp);
            return Err(VaultError::Io);
        }
        Ok(())
    }

    /// Persist the map, or delete the store when it became empty so "store
    /// present" always means "has entries" (an install that removes its last
    /// credential returns to a clean state).
    fn persist_or_clear(&self, map: &BTreeMap<String, String>) -> Result<(), VaultError> {
        if map.is_empty() {
            match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(VaultError::Io),
            }
        } else {
            self.persist(map)
        }
    }

    /// Store/overwrite a secret. `key` is a provider path e.g. `tucano.api_token`.
    pub fn put(&self, key: &str, value: &str) -> Result<(), VaultError> {
        let mut map = lock(&self.data);
        map.insert(key.to_string(), value.to_string());
        self.persist(&map)
    }

    /// Store/overwrite several secrets in **one** encrypted write. Used by the
    /// SMTP settings form so a partially applied configuration (new host, old
    /// port) is never persisted: either the whole map lands or the 500 leaves
    /// the previous settings in place.
    pub fn put_many(&self, entries: &[(String, String)]) -> Result<(), VaultError> {
        let mut map = lock(&self.data);
        for (key, value) in entries {
            map.insert(key.clone(), value.clone());
        }
        self.persist(&map)
    }

    pub fn get(&self, key: &str) -> Option<String> {
        lock(&self.data).get(key).cloned()
    }

    pub fn remove(&self, key: &str) -> Result<bool, VaultError> {
        let mut map = lock(&self.data);
        let removed = map.remove(key).is_some();
        self.persist_or_clear(&map)?;
        Ok(removed)
    }

    /// Configured secret keys (names only — never values).
    pub fn keys(&self) -> Vec<String> {
        lock(&self.data).keys().cloned().collect()
    }

    /// A masked hint for display. Only the final **2** characters are shown
    /// (review A8 — 4 tail chars plus a length bucket is more than a display
    /// hint needs); enough to tell two keys apart, not to leak an edge.
    pub fn hint(&self, key: &str) -> String {
        match lock(&self.data).get(key) {
            Some(v) if v.len() >= 2 => format!("••••{}", &v[v.len() - 2..]),
            Some(_) => "••••".to_string(),
            None => String::new(),
        }
    }
}

fn fingerprint_of(key: &[u8; KEY_LEN]) -> String {
    let digest = Sha256::digest(key);
    format!("sha256:{}", crate::hex_lower(&digest[..4]))
}

fn lock(
    m: &Mutex<BTreeMap<String, String>>,
) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const KEY: &str = "unit-test-vault-key-000000000000"; // 32 bytes, non-secret fixture

    #[test]
    fn disabled_without_key() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SecretVault::open(dir.path(), None).unwrap().is_none());
        assert!(SecretVault::open(dir.path(), Some("")).unwrap().is_none());
    }

    #[test]
    fn rejects_bad_key_length() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            SecretVault::open(dir.path(), Some("tooshort")),
            Err(VaultError::BadKey)
        ));
    }

    #[test]
    fn fingerprint_is_sha256_prefix_and_source_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let vault = SecretVault::open_with(dir.path(), KEY, KeySource::EnvFile).unwrap();
        assert_eq!(vault.source(), KeySource::EnvFile);
        assert_eq!(vault.fingerprint().len(), "sha256:".len() + 8);
        assert!(vault.fingerprint().starts_with("sha256:"));
        assert!(!vault.fingerprint().contains(KEY));
    }

    #[test]
    fn roundtrip_masks_and_persists_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let vault = SecretVault::open(dir.path(), Some(KEY)).unwrap().unwrap();
        vault
            .put("tucano.api_token", "unit-test-secret-value-abcdef")
            .unwrap();
        assert_eq!(
            vault.get("tucano.api_token").as_deref(),
            Some("unit-test-secret-value-abcdef")
        );
        assert_eq!(vault.hint("tucano.api_token"), "••••ef");
        assert_eq!(vault.keys(), vec!["tucano.api_token".to_string()]);

        // On-disk blob must not contain the plaintext.
        let blob = fs::read(dir.path().join("secrets.bin")).unwrap();
        assert!(
            !blob.windows(10).any(|w| w == b"secret-value"),
            "plaintext leaked to disk"
        );

        // Reopen: value survives (decrypted with the same key).
        let v2 = SecretVault::open(dir.path(), Some(KEY)).unwrap().unwrap();
        assert_eq!(
            v2.get("tucano.api_token").as_deref(),
            Some("unit-test-secret-value-abcdef")
        );
    }

    #[test]
    fn wrong_key_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        SecretVault::open(dir.path(), Some(KEY))
            .unwrap()
            .unwrap()
            .put("k", "v")
            .unwrap();
        assert!(matches!(
            SecretVault::open(dir.path(), Some("wrong-vault-key-0000000000000000")),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn removing_the_last_secret_deletes_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let vault = SecretVault::open(dir.path(), Some(KEY)).unwrap().unwrap();
        vault.put("only", "value").unwrap();
        let store = dir.path().join("secrets.bin");
        assert!(store.exists());
        assert!(vault.remove("only").unwrap());
        assert!(
            !store.exists(),
            "an empty vault must not leave a store behind"
        );
        assert!(vault.get("only").is_none());
        // Removing a missing key from an empty map is still fine and leaves no file.
        assert!(!vault.remove("other").unwrap());
        assert!(!store.exists());
    }

    // ------------------------------------------------------ boot policy tests --

    #[test]
    fn first_boot_generates_a_32_byte_key_and_second_reuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("vault.key");

        let boot = open_for_boot(dir.path(), None, None, true).unwrap();
        assert!(boot.vault.is_some());
        assert_eq!(boot.source, Some(KeySource::KeyFile));
        assert_eq!(boot.disabled, None);
        assert!(!boot.external_override);
        assert_eq!(boot.key_file.as_deref(), Some(key_file.as_path()));
        let bytes = fs::read(&key_file).unwrap();
        assert_eq!(bytes.len(), KEY_LEN, "generated key must be 32 bytes");
        let mode = fs::metadata(&key_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the key file must be private");
        let fingerprint = boot.fingerprint.clone();
        assert!(fingerprint.as_deref().unwrap().starts_with("sha256:"));

        let boot2 = open_for_boot(dir.path(), None, None, true).unwrap();
        assert_eq!(boot2.fingerprint, fingerprint, "the file must be reused");
        assert_eq!(fs::read(&key_file).unwrap(), bytes);
    }

    #[test]
    fn existing_invalid_key_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("vault.key");
        fs::write(&key_file, b"not-a-valid-key").unwrap();
        let err = open_for_boot(dir.path(), None, None, true).unwrap_err();
        assert!(err.contains("invalid master key"), "{err}");
        assert_eq!(fs::read(&key_file).unwrap(), b"not-a-valid-key");
    }

    #[test]
    fn env_key_wins_over_a_generated_file() {
        let dir = tempfile::tempdir().unwrap();
        let generated = open_for_boot(dir.path(), None, None, true).unwrap();
        let generated_fingerprint = generated.fingerprint.clone();

        let env_key = "abcdefghijklmnopqrstuvwxyz012345".to_string();
        let boot = open_for_boot(dir.path(), Some(env_key), None, true).unwrap();
        assert_eq!(boot.source, Some(KeySource::Env));
        assert!(boot.external_override);
        assert_ne!(boot.fingerprint, generated_fingerprint);
        assert_eq!(boot.key_file, None);
    }

    #[test]
    fn env_key_of_wrong_length_hard_fails() {
        let dir = tempfile::tempdir().unwrap();
        let err = open_for_boot(dir.path(), Some("short".into()), None, true).unwrap_err();
        assert!(err.contains("TUCANO_SECRET_KEY"), "{err}");
    }

    #[test]
    fn unreadable_env_key_file_hard_fails_except_the_managed_carve_out() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("elsewhere.key");
        let err =
            open_for_boot(dir.path(), None, Some(missing.display().to_string()), true).unwrap_err();
        assert!(err.contains("TUCANO_SECRET_KEY_FILE"), "{err}");

        // The env file names <root>/vault.key: first boot generates it there.
        let managed = dir.path().join("vault.key");
        let boot =
            open_for_boot(dir.path(), None, Some(managed.display().to_string()), true).unwrap();
        assert_eq!(boot.source, Some(KeySource::KeyFile));
        assert!(
            boot.external_override,
            "the env file was explicitly configured"
        );
        assert!(managed.exists());
        assert_eq!(fs::read(&managed).unwrap().len(), KEY_LEN);
    }

    #[test]
    fn env_key_file_of_wrong_length_hard_fails() {
        let dir = tempfile::tempdir().unwrap();
        let kf = dir.path().join("vault.key");
        fs::write(&kf, "too-short").unwrap();
        let err =
            open_for_boot(dir.path(), None, Some(kf.display().to_string()), true).unwrap_err();
        assert!(err.contains("invalid master key"), "{err}");
        assert_eq!(fs::read(&kf).unwrap(), b"too-short");
    }

    #[test]
    fn store_without_a_key_or_with_the_wrong_key_hard_fails() {
        let dir = tempfile::tempdir().unwrap();
        let key = "abcdefghijklmnopqrstuvwxyz012345".to_string();
        {
            let boot = open_for_boot(dir.path(), Some(key.clone()), None, true).unwrap();
            boot.vault.unwrap().put("smtp.host", "relay.local").unwrap();
        }

        let missing = open_for_boot(dir.path(), None, None, true);
        assert!(missing.is_err(), "secrets.bin without its key must refuse");
        assert!(missing.err().unwrap().contains("secrets.bin"));

        let wrong = open_for_boot(
            dir.path(),
            Some("9999999999999999999999999999999999".to_string()),
            None,
            true,
        );
        assert!(wrong.is_err(), "a wrong key must not silently disable");
    }

    #[test]
    fn wrong_length_key_without_a_store_hard_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert!(open_for_boot(dir.path(), Some("short".into()), None, true).is_err());
    }

    #[test]
    fn disabled_config_is_fine_without_a_store_but_fatal_with_one() {
        let dir = tempfile::tempdir().unwrap();
        let boot = open_for_boot(dir.path(), None, None, false).unwrap();
        assert!(boot.vault.is_none());
        assert_eq!(boot.disabled, Some(DisabledReason::Config));

        let key = "abcdefghijklmnopqrstuvwxyz012345".to_string();
        {
            let boot = open_for_boot(dir.path(), Some(key), None, true).unwrap();
            boot.vault.unwrap().put("k", "v").unwrap();
        }
        assert!(
            open_for_boot(dir.path(), None, None, false).is_err(),
            "a store must not be silently orphaned by disabling the vault"
        );
    }

    #[test]
    fn generation_write_failure_degrades_to_write_failed() {
        let dir = tempfile::tempdir().unwrap();
        // `write_private_file` writes `<path>.tmp` first (`vault.tmp` here); a
        // directory at that path makes the open fail deterministically,
        // regardless of uid.
        fs::create_dir(dir.path().join("vault.tmp")).unwrap();
        let boot = open_for_boot(dir.path(), None, None, true).unwrap();
        assert!(boot.vault.is_none());
        assert_eq!(boot.disabled, Some(DisabledReason::WriteFailed));
        assert_eq!(boot.source, None);
        assert!(!dir.path().join("vault.key").exists());
    }

    #[test]
    fn vault_slot_swaps_and_reports_status() {
        let dir = tempfile::tempdir().unwrap();
        let disabled = VaultSlot::disabled(DisabledReason::Config);
        assert!(!disabled.is_enabled());
        assert_eq!(disabled.status().disabled, Some(DisabledReason::Config));
        assert!(disabled.get().is_none());

        let boot = open_for_boot(dir.path(), None, None, true).unwrap();
        let fingerprint = boot.fingerprint.clone();
        disabled.install(boot);
        assert!(disabled.is_enabled());
        assert_eq!(disabled.status().fingerprint, fingerprint);
        assert_eq!(disabled.get().unwrap().source(), KeySource::KeyFile);
    }

    #[test]
    fn gate_is_exclusive_and_survives_a_poisoned_holder() {
        let slot = VaultSlot::disabled(DisabledReason::Config);
        let held = slot.gate();
        assert!(matches!(
            slot.gate.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        drop(held);
        drop(slot.gate());

        let poisoned = VaultSlot::disabled(DisabledReason::Config);
        let holder = poisoned.clone();
        let _ = std::thread::spawn(move || {
            let _guard = holder.gate();
            panic!("poison the gate");
        })
        .join();
        // The gate is still usable after the panic: the admin API is not wedged.
        drop(poisoned.gate());
    }

    #[test]
    fn a_lax_key_file_mode_is_tightened_on_read() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("vault.key");
        fs::write(&key_file, KEY.as_bytes()).unwrap();
        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o644)).unwrap();

        let boot = open_for_boot(dir.path(), None, None, true).unwrap();
        assert!(
            boot.vault.is_some(),
            "a lax mode must not disable the vault"
        );
        assert_eq!(
            fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
            0o600,
            "group/world access must be removed"
        );
        assert_eq!(fs::read(&key_file).unwrap(), KEY.as_bytes());
    }

    #[test]
    fn persist_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let vault = SecretVault::open(dir.path(), Some(KEY)).unwrap().unwrap();
        vault.put("smtp.host", "relay.local").unwrap();
        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        assert!(dir.path().join("secrets.bin").exists());
    }
}
