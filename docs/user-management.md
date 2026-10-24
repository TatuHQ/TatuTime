# User management

Administrators manage accounts under **Settings → Users**. Search by name or
email, filter by role and status, and use **+ User** to create an account. The
edit dialog changes the profile, role, status and optional billing/cost rates.
Rate inputs use decimal amounts; the API and storage use integer minor units.
Tasks have no rate overrides.

Passwords are set on creation or through the separate **Change password**
dialog. There is no invitation email and no automated password-reset delivery.
Password fields are masked, never prefilled, cleared on close and never returned
by the API. Passwords are hashed with Argon2id. Password, role and active-state
changes invalidate existing sessions; ordinary profile changes do not.

Self-deletion, self-deactivation and self-demotion are blocked. The storage layer
also enforces at least one active administrator and unique email addresses under
its writer lock. Concurrent stale updates return 409 and must be retried after
reloading. Email-index updates follow the authoritative user documents.

Delete is permitted only for accounts without entries, expenses, submissions,
reimbursement claims or a running timer. Deactivate accounts with history
instead; their records remain available to administrators. Audit events record
account changes but never passwords or hashes. Members cannot access
user-management endpoints or the Users settings tab.

Endpoints: `GET/POST /api/users`, `PUT/DELETE /api/users/{id}` and
`PUT /api/users/{id}/password`. The checked-in
[OpenAPI contract](../openapi.json) documents their inputs, errors and public
user responses. Negative, fractional or out-of-range rates and unknown fields are
rejected before persistence.

## Single sign-on

SSO (OIDC/signed-token, plus a SAML stub IdP) is additive: local password login
keeps working alongside it.

Enable a provider with its vault secret (`oidc.client_secret` or
`saml.signing_secret`). SAML also requires a pinned certificate fingerprint
(`saml.cert_fingerprint`). Restrict the email domains the provider may assert
with `sso_allowed_domains`. That setting resolves **environment >
`config.json` > vault**:

1. `TATU_SSO_ALLOWED_DOMAINS`;
2. `sso_allowed_domains` in `config.json`;
3. the vault key `sso.allowed_domains` as the fallback.

**Fail-closed allow-list.** Only listed domains are trusted. A correctly signed
assertion for any other domain is rejected and no account is created. An empty
or unset allow-list denies *every* domain. Because that is a misconfiguration,
the server logs a loud boot error and registers **no** identity provider —
nothing is just-in-time provisioned and `/api/auth/sso/providers` advertises
nothing. The warning is not fatal: local login and the rest of the timesheet keep
working, but SSO stays disabled until at least one domain is configured.

A verified assertion for a new user provisions a member account just in time;
the IdP is the password store, so the local password is unusable. Provider
secrets resolve from the vault first and the environment second.

**Admin-group limitation.** `TATU_SSO_ADMIN_GROUP` is not read uniformly. A
newly provisioned user takes the value through the configuration resolver
(environment > `config.json` > default), while an existing user signing in again
has their role refreshed from the **environment variable only**. Set
`TATU_SSO_ADMIN_GROUP` if the mapping must apply to existing SSO users. See
[Configuration](configuration.md#known-limitations).

---

Last verified against TatuTime 0.1.41.
