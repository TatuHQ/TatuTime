# User Management

Administrators manage accounts under Settings > Users. Search by name or email,
filter by role/status, and use + User to create an account. Edit changes profile,
role, status and optional billing/cost rates. Rate inputs use decimal amounts;
the API and storage use integer minor units. Tasks have no rate overrides.

Passwords are set on creation or through the separate Change password dialog.
There is no invitation email or automated password-reset delivery. Password
fields are masked, never prefilled, cleared on close, and never returned by the
API. Passwords are hashed with Argon2id. Password, role and active-state changes
invalidate existing sessions; ordinary profile changes do not.

Self-deletion, self-deactivation and self-demotion are blocked. The storage
layer also enforces at least one active administrator and unique email addresses
under its writer lock. Concurrent stale updates return 409 and must be retried
after reloading. Email-index updates follow the authoritative user documents.

Delete is permitted only for accounts without entries, expenses, submissions,
reimbursement claims or a running timer. Deactivate accounts with history instead;
their records remain available to administrators. Audit events record account
changes but never passwords or hashes. Members cannot access user-management
endpoints or the Users Settings tab.

Endpoints: GET/POST /users, PUT/DELETE /users/{id}, PUT /users/{id}/password.
The checked-in OpenAPI contract documents their inputs, errors and public user
responses. Negative, fractional or out-of-range rates and unknown fields are
rejected before persistence.

## Single sign-on (SSO)

SSO (OIDC/signed-token, plus the SAML stub IdP, #32) is additive — local
password login keeps working alongside it. Enable a provider with its vault
secret (`oidc.client_secret` / `saml.signing_secret`, #77) and restrict the
email domains it may assert with `sso_allowed_domains` (env
`TUCANO_SSO_ALLOWED_DOMAINS`, `config.json`, or the vault key
`sso.allowed_domains`; precedence env > file > vault).

**Fail-closed allow-list (#260).** Only domains listed in
`sso_allowed_domains` are trusted: a correctly-signed assertion for any other
domain is rejected and no account is created. An **empty or unset** allow-list
denies *every* domain. Because that is a misconfiguration, the server logs a
loud boot error and registers **no** identity provider — nothing is
just-in-time provisioned and `/auth/sso/providers` advertises nothing. The
warning is not fatal (local login and the rest of the timesheet keep working),
but SSO stays disabled until at least one domain is configured. This mirrors
the vault (#77: an unset key disables the vault) and lock (#185) fail-closed
conventions.