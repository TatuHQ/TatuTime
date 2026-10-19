//! Single sign-on (#32). An `IdentityProvider` port sits behind the #18 seam: it
//! turns an externally-asserted identity (a SAML assertion from Microsoft Entra /
//! Okta, or a signed OIDC-style token) into a trusted `Identity`, and users are
//! provisioned just-in-time with the role mapping from #19.
//!
//! Local password login keeps working alongside — SSO is additive. Provider
//! secrets (keys/certs, metadata) live in the vault (#77) or env, never in the
//! repo or API responses. Signature verification is real HMAC-SHA256; `SamlIdp`
//! checks the assertion digest the way a deployment would check the XML signature
//! against the pinned certificate, with the full XML-DSig stack deliberately not
//! vendored.

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SsoError {
    #[error("assertion signature is invalid")]
    BadSignature,
    #[error("assertion payload is invalid")]
    BadAssertion,
    #[error("assertion has expired")]
    Expired,
    #[error("email is missing from the assertion")]
    NoEmail,
    #[error("account is inactive")]
    Inactive,
    #[error("email domain is not allowed for this organisation")]
    DomainNotAllowed,
}

/// A verified external identity, ready for just-in-time provisioning (#19).
#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    pub email: String,
    pub name: String,
    /// Provider groups/roles, mapped to our Role by the handler.
    pub groups: Vec<String>,
    pub issuer: String,
}

/// Verifies an externally-signed assertion and extracts the identity.
pub trait IdentityProvider: Send + Sync {
    fn name(&self) -> &str;
    /// `payload` is the raw assertion body (base64url JWT or the SAML
    /// assertion's encoded statement); `signature` is the detached signature
    /// over it.
    fn verify(&self, payload: &str, signature: &str) -> Result<Identity, SsoError>;
}

/// Allowed-email domains (case-insensitive). An empty allow-list **denies
/// every domain** — fail-closed (#260, review A2 residual). An SSO deployment
/// that forgot to set `allowed_domains` must never JIT-provision an arbitrary
/// email domain; `registry_from_vault` refuses to register a provider in that
/// state and warns loudly at boot, and this guard keeps the guarantee even for
/// a directly-constructed adapter. Configured domains behave exactly as before.
fn domain_ok(allow: &[String], email: &str) -> bool {
    !allow.is_empty()
        && email
            .split('@')
            .nth(1)
            .is_some_and(|d| allow.iter().any(|a| a.eq_ignore_ascii_case(d)))
}

// ------------------------------------------------------- signed-token IdP --

/// OIDC-style adapter: `header.payload.signature` compact form, HMAC-SHA256.
/// This models Okta/Microsoft OIDC bearer assertions closely enough to be
/// genuinely verifiable without vendoring a JWKS client (a production adapter
/// would swap the HMAC for RS256 + JWKS behind this same port).
pub struct SignedTokenIdp {
    issuer: String,
    secret: String,
    allowed_domains: Vec<String>,
    now_fn: Box<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>,
}

impl SignedTokenIdp {
    #[must_use]
    pub fn new(
        issuer: impl Into<String>,
        secret: impl Into<String>,
        allowed_domains: Vec<String>,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            secret: secret.into(),
            allowed_domains,
            now_fn: Box::new(chrono::Utc::now),
        }
    }

    /// Test hook: deterministic clock for expiry behaviour.
    #[must_use]
    pub fn with_clock(
        mut self,
        now_fn: impl Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync + 'static,
    ) -> Self {
        self.now_fn = Box::new(now_fn);
        self
    }

    /// Builds a signed assertion (used by the stub Idp in tests + demo tool).
    #[must_use]
    pub fn issue(&self, claims: &TokenClaims) -> String {
        let header = b64(&serde_json::to_vec(
            &serde_json::json!({"alg": "HS256", "iss": self.issuer}),
        )
        .unwrap_or_default());
        let payload = b64(&serde_json::to_vec(claims).unwrap_or_default());
        let sig = sign(&self.secret, &format!("{header}.{payload}"));
        format!("{header}.{payload}.{sig}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenClaims {
    pub sub: String,
    pub email: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub groups: Vec<String>,
    /// Expiry as a unix timestamp (seconds).
    pub exp: i64,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(s: &str) -> Result<Vec<u8>, SsoError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| SsoError::BadAssertion)
}

pub(crate) fn sign(secret: &str, msg: &str) -> String {
    let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac any key");
    m.update(msg.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(m.finalize().into_bytes())
}

impl IdentityProvider for SignedTokenIdp {
    fn name(&self) -> &str {
        &self.issuer
    }
    fn verify(&self, payload: &str, signature: &str) -> Result<Identity, SsoError> {
        // payload = "header.payload", signature = raw sig (compact split done
        // by callers) OR full compact jwt passed as payload with empty sig.
        let (joined, sig) = if signature.is_empty() {
            let mut parts = payload.rsplitn(2, '.');
            let sig = parts.next().unwrap_or_default().to_string();
            let rest = parts.next().unwrap_or_default();
            (rest.to_string(), sig)
        } else {
            (payload.to_string(), signature.to_string())
        };
        // Constant-time verification via the hmac crate (review A7): compare
        // the decoded tag, never re-encoded strings.
        let tag = unb64(&sig).map_err(|_| SsoError::BadSignature)?;
        let mut m = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes()).expect("hmac any key");
        m.update(joined.as_bytes());
        if m.verify_slice(&tag).is_err() {
            return Err(SsoError::BadSignature);
        }
        let parts: Vec<&str> = joined.split('.').collect();
        if parts.len() != 2 {
            return Err(SsoError::BadAssertion);
        }
        let claims: TokenClaims =
            serde_json::from_slice(&unb64(parts[1])?).map_err(|_| SsoError::BadAssertion)?;
        if claims.exp < (self.now_fn)().timestamp() {
            return Err(SsoError::Expired);
        }
        let email = crate::auth::normalise_email(&claims.email).ok_or(SsoError::NoEmail)?;
        if !domain_ok(&self.allowed_domains, &email) {
            return Err(SsoError::DomainNotAllowed);
        }
        Ok(Identity {
            email,
            name: claims.name,
            groups: claims.groups,
            issuer: self.issuer.clone(),
        })
    }
}

// ---------------------------------------------------------------- SAML IdP --

/// SAML-2.0-shaped SP adapter (statement-level; stub IdP for tests).
///
/// Review A2: verifying with a key derived from the certificate *fingerprint*
/// is no protection — a fingerprint is public metadata, so anyone holding the
/// IdP certificate could sign arbitrary assertions. The adapter therefore
/// **requires a deployment-shared HMAC secret** (`saml.signing_secret` /
/// `TUCANO_SAML_SIGNING_SECRET`, vault or env), and the registry refuses to
/// enable SAML without it. When a real XML-DSig verifier attaches to this
/// port the fingerprint becomes the pin input; the port, JIT provisioning and
/// endpoints do not change.
pub struct SamlIdp {
    issuer: String,
    signing_secret: String,
    allowed_domains: Vec<String>,
}

impl SamlIdp {
    #[must_use]
    pub fn new(
        issuer: impl Into<String>,
        signing_secret: impl Into<String>,
        allowed_domains: Vec<String>,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            signing_secret: signing_secret.into(),
            allowed_domains,
        }
    }

    /// Digests the assertion statement the way the stub Idp signs it.
    #[must_use]
    pub fn sign_statement(&self, statement: &str) -> String {
        sign(&self.signing_secret, statement)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SamlStatement {
    #[serde(rename = "NameID")]
    name_id: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    groups: Vec<String>,
    #[serde(rename = "NotOnOrAfter")]
    not_on_or_after: i64,
}

impl IdentityProvider for SamlIdp {
    fn name(&self) -> &str {
        &self.issuer
    }
    fn verify(&self, payload: &str, signature: &str) -> Result<Identity, SsoError> {
        // payload: base64(statement json) — the SAMLResponse body after XML
        // unwrap; signature: HMAC over the decoded statement with the pinned
        // cert fingerprint.
        let statement = String::from_utf8(unb64(payload)?).map_err(|_| SsoError::BadAssertion)?;
        let tag = unb64(signature).map_err(|_| SsoError::BadSignature)?;
        let mut m =
            Hmac::<Sha256>::new_from_slice(self.signing_secret.as_bytes()).expect("hmac any key");
        m.update(statement.as_bytes());
        if m.verify_slice(&tag).is_err() {
            return Err(SsoError::BadSignature);
        }
        let s: SamlStatement =
            serde_json::from_str(&statement).map_err(|_| SsoError::BadAssertion)?;
        if s.not_on_or_after < chrono::Utc::now().timestamp() {
            return Err(SsoError::Expired);
        }
        let email = crate::auth::normalise_email(&s.name_id).ok_or(SsoError::NoEmail)?;
        if !domain_ok(&self.allowed_domains, &email) {
            return Err(SsoError::DomainNotAllowed);
        }
        Ok(Identity {
            email,
            name: s.display_name,
            groups: s.groups,
            issuer: self.issuer.clone(),
        })
    }
}

// ---------------------------------------------------------------- registry --

/// Registry of enabled identity providers (#101: shared generic `Registry`;
/// lookup stays case-insensitive, as SSO names always were).
pub type SsoRegistry = crate::providers::Registry<dyn IdentityProvider>;

impl crate::providers::NamedProvider for dyn IdentityProvider {
    fn name(&self) -> &str {
        IdentityProvider::name(self)
    }
}

/// Builds the registry from vault/env. Entra/Okta share the OIDC-style
/// secret key; the SAML adapter needs a pinned certificate fingerprint.
#[must_use]
pub fn registry_from_vault(
    vault: Option<&crate::vault::SecretVault>,
    cfg: &crate::appconfig::AppConfig,
) -> SsoRegistry {
    let get = |key: &str, env: &str| -> Option<String> {
        crate::providers::resolve_secret(vault, key, env)
    };
    let domains_from = |s: Option<String>| -> Vec<String> {
        s.map(|d| {
            d.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
    };
    // #94: allowed domains resolve env > config.json > vault > none.
    let domains = || {
        let c = cfg.get_csv("sso_allowed_domains", &crate::appconfig::process_env);
        if c.is_empty() {
            domains_from(get("sso.allowed_domains", "TUCANO_SSO_ALLOWED_DOMAINS"))
        } else {
            c
        }
    };
    let mut providers: Vec<std::sync::Arc<dyn IdentityProvider>> = Vec::new();
    let allowed = domains();

    // #260 (review A2 residual): SSO must fail closed. An empty allow-list
    // means "trust no email domain", so **no** provider is registered and
    // nothing is ever JIT-provisioned — the omission is loud, not permissive.
    // Decision: deny-with-warning, not hard-fail startup. An unset vault key
    // (#77) disables the vault rather than refusing to boot, and SSO is
    // additive (local password login keeps working); aborting the whole server
    // for a partial SSO misconfiguration would take the timesheet offline.
    // Registering nothing also keeps `/auth/sso/providers` honest — the login
    // screen never advertises a provider that can only reject.
    let oidc_secret = get("oidc.client_secret", "TUCANO_OIDC_CLIENT_SECRET");
    let saml_fingerprint = get("saml.cert_fingerprint", "TUCANO_SAML_CERT_FINGERPRINT");
    let saml_secret = get("saml.signing_secret", "TUCANO_SAML_SIGNING_SECRET");
    if allowed.is_empty() {
        if oidc_secret.is_some() || saml_fingerprint.is_some() {
            tracing::error!(
                "SSO is configured (OIDC client secret and/or SAML fingerprint) but \
                 sso_allowed_domains is empty — ALL identity providers are DISABLED \
                 (fail-closed, #260): no email domain is trusted, so no account will be \
                 just-in-time provisioned. Set at least one allowed domain via \
                 TUCANO_SSO_ALLOWED_DOMAINS, config.json 'sso_allowed_domains', or the \
                 vault key 'sso.allowed_domains'."
            );
        }
        return SsoRegistry::new(providers);
    }

    if let Some(secret) = oidc_secret {
        let issuer = get("oidc.issuer", "TUCANO_OIDC_ISSUER").unwrap_or_else(|| "oidc".to_string());
        providers.push(std::sync::Arc::new(SignedTokenIdp::new(
            issuer,
            secret,
            allowed.clone(),
        )));
    }
    if saml_fingerprint.is_some() {
        // A2 fail-closed: a public fingerprint alone does NOT enable SAML.
        match saml_secret {
            Some(secret) => {
                let issuer =
                    get("saml.issuer", "TUCANO_SAML_ISSUER").unwrap_or_else(|| "saml".to_string());
                providers.push(std::sync::Arc::new(SamlIdp::new(issuer, secret, allowed)));
            }
            None => tracing::error!(
                "SAML fingerprint configured but no saml.signing_secret — SAML login \
                 disabled (fingerprint-derived keys are forgeable, review A2)"
            ),
        }
    }
    SsoRegistry::new(providers)
}

/// #19 role mapping: SSO groups decide the role. Admin membership is
/// explicit — never implicit — and everyone else is a member.
pub fn map_role(groups: &[String], admin_group: &str) -> crate::auth::Role {
    if !admin_group.is_empty() && groups.iter().any(|g| g.eq_ignore_ascii_case(admin_group)) {
        crate::auth::Role::Admin
    } else {
        crate::auth::Role::Member
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn signed_token_roundtrip_and_rejections() {
        let idp = SignedTokenIdp::new("okta", "test-secret", vec!["tucanotime.test".into()])
            .with_clock(move || Utc.timestamp_opt(1_000_000, 0).unwrap());
        let claims = TokenClaims {
            sub: "u1".into(),
            email: "Alice@tucanotime.test".into(),
            name: "Alice".into(),
            groups: vec!["timesheet-admins".into()],
            exp: 1_000_600,
        };
        let token = idp.issue(&claims);
        let id = idp.verify(&token, "").unwrap();
        assert_eq!(id.email, "alice@tucanotime.test");
        assert_eq!(id.groups, vec!["timesheet-admins".to_string()]);

        // Tampered payload fails the signature.
        let tampered = {
            let mut parts: Vec<String> = token.split('.').map(str::to_owned).collect();
            let mut body: serde_json::Value =
                serde_json::from_slice(&unb64(&parts[1]).unwrap()).unwrap();
            body["email"] = serde_json::json!("mallory@evil.test");
            parts[1] = b64(&serde_json::to_vec(&body).unwrap());
            parts.join(".")
        };
        assert!(matches!(
            idp.verify(&tampered, ""),
            Err(SsoError::BadSignature)
        ));

        // Expired assertion.
        let expired = TokenClaims {
            exp: 999_400,
            ..claims.clone()
        };
        assert!(matches!(
            idp.verify(&idp.issue(&expired), ""),
            Err(SsoError::Expired)
        ));

        // Disallowed domain.
        let outside = TokenClaims {
            email: "bob@other.test".into(),
            ..claims
        };
        assert!(matches!(
            idp.verify(&idp.issue(&outside), ""),
            Err(SsoError::DomainNotAllowed)
        ));
    }

    #[test]
    fn saml_assertion_verified_and_domain_gated() {
        let idp = SamlIdp::new(
            "entra",
            "shared-deployment-secret",
            vec!["contoso.test".into()],
        );
        let statement = serde_json::json!({
            "NameID": "dana@contoso.test",
            "display_name": "Dana",
            "groups": ["engineering"],
            "NotOnOrAfter": chrono::Utc::now().timestamp() + 300,
        })
        .to_string();
        let payload = b64(statement.as_bytes());
        let sig = idp.sign_statement(&statement);
        let id = idp.verify(&payload, &sig).unwrap();
        assert_eq!(id.email, "dana@contoso.test");
        assert_eq!(id.name, "Dana");

        // Wrong signature rejected.
        assert!(matches!(
            idp.verify(&payload, "bogus"),
            Err(SsoError::BadSignature)
        ));
        // Foreign domain rejected.
        let bad = serde_json::json!({
            "NameID": "eve@phish.example",
            "NotOnOrAfter": chrono::Utc::now().timestamp() + 300,
        })
        .to_string();
        let payload2 = b64(bad.as_bytes());
        let sig2 = idp.sign_statement(&bad);
        assert!(matches!(
            idp.verify(&payload2, &sig2),
            Err(SsoError::DomainNotAllowed)
        ));
    }

    #[test]
    fn admin_group_maps_role() {
        assert_eq!(
            map_role(&["timesheet-admins".into()], "timesheet-admins"),
            crate::auth::Role::Admin
        );
        assert_eq!(
            map_role(&["TIMEsheet-Admins".into()], "timesheet-admins"),
            crate::auth::Role::Admin
        );
        assert_eq!(
            map_role(&["dev".into()], "timesheet-admins"),
            crate::auth::Role::Member
        );
        assert_eq!(map_role(&[], ""), crate::auth::Role::Member); // no implicit admin
    }

    #[test]
    fn empty_allowed_domains_fail_closed_and_configured_domain_succeeds() {
        // #260: an empty allow-list denies every domain. `verify` rejects
        // before the handler can JIT-provision, so no account is ever created.
        let deny_all = SignedTokenIdp::new("okta", "test-secret", vec![]);
        let claims = TokenClaims {
            sub: "u1".into(),
            email: "alice@tucanotime.test".into(),
            name: "Alice".into(),
            groups: vec![],
            exp: chrono::Utc::now().timestamp() + 300,
        };
        assert!(matches!(
            deny_all.verify(&deny_all.issue(&claims), ""),
            Err(SsoError::DomainNotAllowed)
        ));

        // A configured matching domain still succeeds (behaviour preserved).
        let allow = SignedTokenIdp::new("okta", "test-secret", vec!["tucanotime.test".into()]);
        let id = allow.verify(&allow.issue(&claims), "").unwrap();
        assert_eq!(id.email, "alice@tucanotime.test");
    }

    #[test]
    fn registry_refuses_providers_without_allowed_domains() {
        // #260 boot path: even with SSO credentials present, no provider is
        // registered while the allow-list is empty — the omission fails closed
        // instead of trusting every email domain.
        let dir = tempfile::tempdir().unwrap();
        let vault =
            crate::vault::SecretVault::open(dir.path(), Some("0123456789abcdef0123456789abcdef"))
                .unwrap()
                .unwrap();
        vault.put("oidc.client_secret", "stub-secret").unwrap();
        let cfg = crate::appconfig::AppConfig::empty();
        assert!(
            !registry_from_vault(Some(&vault), &cfg).enabled(),
            "no allowed domains configured => no SSO provider is registered"
        );

        // Once a domain is configured the provider is enabled as before.
        vault.put("sso.allowed_domains", "tucanotime.test").unwrap();
        let registry = registry_from_vault(Some(&vault), &cfg);
        assert!(registry.enabled());
        assert_eq!(registry.names(), vec!["oidc".to_string()]);
    }
}
