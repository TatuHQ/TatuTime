# Configuration

TucanoTime takes its configuration from environment variables, from
`<data>/config.json` inside the data directory, and from the credential vault
for provider secrets. This page is the authoritative reference; the source of
truth is [`src/appconfig.rs`](../src/appconfig.rs), the read sites in
[`src/`](../src/), and the Compose interpolation in
[`docker-compose.yml`](../docker-compose.yml).

## Precedence

There are four independent precedence rules.

1. **Externalised non-secret knobs** (`config.json` whitelist): explicit
   environment variable **>** `config.json` entry **>** built-in default.
   `GET /api/admin/config` reports the effective value, its source
   (`env`, `file` or `default`) and whether a key is writable.
2. **Provider secrets**: credential vault **>** environment variable.
   `config.json` never participates — it is plaintext and its whitelist refuses
   secret-shaped keys.
3. **Session signing key**: `TUCANO_SESSION_SECRET` **>**
   `TUCANO_SESSION_SECRET_FILE` **>** the auto-created `<data>/session.key`.
4. **Vault master key**: `TUCANO_SECRET_KEY` **>** `TUCANO_SECRET_KEY_FILE`
   **>** the auto-created `<data>/vault.key`. An external key always wins and is
   never copied into the data directory.

When both a secret variable and its `_FILE` variant are set, the plain variable
wins. Prefer the `_FILE` variant in containers: the value never appears in
`docker inspect`, the Compose file or the process environment.

## `config.json` keys

`PUT /api/admin/config` may only write the whitelisted keys below. Unknown and
secret-shaped keys are refused with `422 validation_failed`; an out-of-range or
malformed value is refused the same way and changes nothing.

| Key | Type and range | Default | Purpose |
| --- | --- | --- | --- |
| `reminder_days` | integer, 1–365 | `7` | Days between overdue-invoice email reminders |
| `org_name` | non-blank string | `TucanoTime` | Fallback organisation name when the Company name is blank |
| `sso_admin_group` | string | empty | IdP group mapped to the admin role on SSO login (see [Known limitations](#known-limitations)) |
| `sso_allowed_domains` | comma-separated string | empty | Email domains trusted for SSO; empty denies every domain |
| `calendar_oauth_redirect` | string | `/api/calendar/oauth/callback` | Redirect URI registered with the OAuth calendar app |
| `google_calendar_client_id` | optional string | unset | Google OAuth client id (the secret stays in the vault or environment) |
| `ms_calendar_client_id` | optional string | unset | Microsoft OAuth client id |
| `qbo_base_url` | optional string | unset | Override the QuickBooks API base URL |
| `xero_base_url` | optional string | unset | Override the Xero API base URL |
| `max_docs` | integer, 10–1,000,000 | `50000` | Per-collection document cap enforced on writes |
| `base_currency` | three ASCII letters, stored uppercase | unset | Optional base currency for mixed-currency invoice trends; unset disables conversion |
| `vault_enabled` | boolean | `true` | Credential-vault enablement; **read-only** through `PUT` |

### `GET /api/admin/config`

Returns the effective table with `key`, `value`, `source`
(`env`/`file`/`default`), `default`, `writable` and `description`, plus
`vault_enabled` and `email_transport` (`smtp` or `disabled`). No secret is
returned: the whitelist contains none.

### `PUT /api/admin/config`

Takes a JSON object containing whitelisted keys only and writes them atomically
to `<data>/config.json`. Request-time knobs (SSO domains and admin group, OAuth
redirect) take effect immediately; boot-time knobs take effect on the next
restart or when the relevant code path next resolves them.

`vault_enabled` is deliberately **not writable** through this endpoint. A
`PUT` that tries to change it is refused with `422 validation_failed`, because
persisting `false` next to an existing `secrets.bin` would make the next boot
unable to open the store. The dedicated vault lifecycle endpoints
(`POST /api/admin/vault/enable`, `/disable`, `/regenerate`) are the only writers.
Disabling is refused while any credential is stored.

## Environment variables

### Core

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TUCANO_DATA_DIR` | `Data` locally, `/data` in the image | Directory holding every document and state file | — |
| `TUCANO_PORT` | `8080` | TCP port the server and the health probe use | — |
| `TUCANO_ENV` | unset (`production` in Compose) | `production` enables the strict session-secret policy and Secure cookies by default | — |
| `TUCANO_SECURE_COOKIES` | on iff `TUCANO_ENV=production` | `0`/`false` drops the cookie `Secure` flag for plain-HTTP LAN serving | — |
| `TUCANO_MAX_DOCS` | `50000` (10–1,000,000) | Per-collection document cap; a create past the cap is refused with `409 collection_full` | — |
| `RUST_LOG` | `info` | Tracing filter (`tracing-subscriber` environment filter) | — |

The store also defines a compile-time `MAX_DOCS` constant of 100,000 in
[`src/store/mod.rs`](../src/store/mod.rs#L58), used when the store is opened
outside the server boot path (for example in tests). The effective server
default is the whitelisted `max_docs` value, 50,000, and its range is 10 to
1,000,000.

### Security and vault

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TUCANO_SESSION_SECRET` | auto `<data>/session.key` | HMAC key for session cookies; at least 32 bytes in production | `TUCANO_SESSION_SECRET_FILE` |
| `TUCANO_SECRET_KEY` | auto `<data>/vault.key` | Credential-vault master key; exactly 32 bytes (the generated key is 32 base64url characters) | `TUCANO_SECRET_KEY_FILE` |
| `TUCANO_VAULT_ENABLED` | `true` | Enables the credential vault; a valid `true`/`false` makes the lifecycle operator-managed | — |

`TUCANO_SESSION_SECRET` is required in production only when the server cannot
create or regenerate `<data>/session.key` (for example a read-only data
directory). The auto-created key keeps sessions valid across restarts and
container upgrades. If the key is missing and cannot be persisted, production
refuses to start; outside production the warning says sessions are ephemeral.

### Authentication and SSO

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TUCANO_SSO_ALLOWED_DOMAINS` | empty (denies every domain) | Comma-separated email domains trusted for SSO; resolves env > `config.json` > vault | Vault fallback `sso.allowed_domains` |
| `TUCANO_SSO_ADMIN_GROUP` | empty | IdP group mapped to the admin role; see [Known limitations](#known-limitations) | — |
| `TUCANO_OIDC_CLIENT_SECRET` | unset | Enables the OIDC/signed-token provider | Vault `oidc.client_secret` wins |
| `TUCANO_OIDC_ISSUER` | `oidc` | Provider name shown by the login endpoint | Vault `oidc.issuer` wins |
| `TUCANO_SAML_CERT_FINGERPRINT` | unset | Pinned SAML certificate fingerprint | Vault `saml.cert_fingerprint` wins |
| `TUCANO_SAML_SIGNING_SECRET` | unset | Shared HMAC secret; SAML refuses to enable without it | Vault `saml.signing_secret` wins |
| `TUCANO_SAML_ISSUER` | `saml` | Provider name shown by the login endpoint | Vault `saml.issuer` wins |

SSO is additive: local password login keeps working. An empty allow-list is
fail-closed — no identity provider is registered and nobody is
just-in-time provisioned — but it is not a fatal boot error, because the
timesheet itself stays available.

### Email

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TUCANO_SMTP_HOST` | unset (email disabled) | SMTP relay hostname; unset logs mail instead of sending | Vault `smtp.host` wins |
| `TUCANO_SMTP_PORT` | `25` | 465 = verified implicit TLS; 587 = required STARTTLS; other ports = trusted plaintext relay | Vault `smtp.port` wins |
| `TUCANO_SMTP_FROM` | `noreply@localhost` | Envelope/From address | Vault `smtp.from` wins |
| `TUCANO_SMTP_USERNAME` | unset | Username for optional `AUTH LOGIN` | Vault `smtp.username` wins |
| `TUCANO_SMTP_PASSWORD` | unset | Password for optional `AUTH LOGIN` | Vault `smtp.password` wins |

Port 465 uses certificate-verified implicit TLS; port 587 requires STARTTLS and
never sends credentials when the upgrade is unavailable. Other ports retain the
plaintext local-relay mode and must be used only on a trusted network or through
a TLS sidecar. Configuration entered in **Settings → Email** is stored in the
vault and wins over these variables. Hosted providers commonly require the full
email address as username, an app password, and a permitted From address. A
previous port-465 timeout reported as `Resource temporarily unavailable (os error
11)` can indicate a plaintext/TLS mismatch, not invalid credentials.

For Fastmail use `smtp.fastmail.com`, port `465` (implicit TLS), your full
Fastmail email address as the username, and an app-specific password with mail
access. A regular account password will not work. AUTH PLAIN is supported over
the verified TLS connection. Port `587` is the alternative with required STARTTLS.
Keep the From address set to an address or alias Fastmail permits you to send as.

### Integrations

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TUCANO_CALENDAR_ICS` | unset | ICS feed URL or local file path for calendar events | Vault `calendar.ics_url` wins |
| `TUCANO_CALENDAR_PROVIDER` | unset | `google` or `microsoft` for OAuth calendar import | Vault `calendar.provider` wins |
| `TUCANO_CALENDAR_OAUTH_REDIRECT` | `/api/calendar/oauth/callback` | Redirect URI registered with the OAuth app | `config.json` key `calendar_oauth_redirect` |
| `TUCANO_GOOGLE_CLIENT_ID` | unset | Google OAuth client id (public half) | `config.json` key `google_calendar_client_id` |
| `TUCANO_GOOGLE_CLIENT_SECRET` | unset | Google OAuth client secret | Vault `calendar.google.client_secret` wins |
| `TUCANO_GOOGLE_TOKEN` | unset | Google access token | Vault `calendar.google.access_token` wins |
| `TUCANO_GOOGLE_REFRESH_TOKEN` | unset | Google refresh token | Vault `calendar.google.refresh_token` wins |
| `TUCANO_GOOGLE_CALENDAR` | `primary` | Google calendar id | Vault `calendar.google.id` wins |
| `TUCANO_MS_CLIENT_ID` | unset | Microsoft OAuth client id (public half) | `config.json` key `ms_calendar_client_id` |
| `TUCANO_MS_CLIENT_SECRET` | unset | Microsoft OAuth client secret | Vault `calendar.microsoft.client_secret` wins |
| `TUCANO_MS_TOKEN` | unset | Microsoft access token | Vault `calendar.microsoft.access_token` wins |
| `TUCANO_MS_REFRESH_TOKEN` | unset | Microsoft refresh token | Vault `calendar.microsoft.refresh_token` wins |
| `TUCANO_QBO_TOKEN` | unset | QuickBooks Online OAuth token; enables the provider | Vault `qbo.token` wins |
| `TUCANO_QBO_BASE_URL` | provider default | QuickBooks API base URL override | `config.json` key `qbo_base_url` |
| `TUCANO_XERO_TOKEN` | unset | Xero OAuth token; enables the provider | Vault `xero.token` wins |
| `TUCANO_XERO_BASE_URL` | provider default | Xero API base URL override | `config.json` key `xero_base_url` |

Calendar OAuth is configured by an administrator in **Settings → Security**
and **Settings → Email**; accounting sync is configured through the vault and
the admin API. A provider is enabled only when its token exists.

### Operations

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TUCANO_REMINDER_DAYS` | `7` (1–365) | Days between overdue-invoice reminder emails | `config.json` key `reminder_days` |
| `TUCANO_ORG_NAME` | `TucanoTime` | Fallback organisation name when the Company profile name is blank | `config.json` key `org_name` |
| `TUCANO_BASE_CURRENCY` | unset | ISO 4217 base currency for mixed-currency invoice trends; unset disables conversion | `config.json` key `base_currency` |

### Compose-only interpolation

These variables are read by Docker Compose, not by the binary.

| Variable | Default | Purpose |
| --- | --- | --- |
| `TUCANO_HOST_ADDR` | `127.0.0.1` | Host address the published port binds to; `0.0.0.0` exposes the GUI on the LAN |
| `TUCANO_HOST_PORT` | `8080` | Host port mapped to container port `8080` |
| `TUCANO_BACKUP_DIR` | `./backups` | Host directory mounted at `/backups` in the `cli` profile |
| `TUCANO_SECRETS_DIR` | `./secrets` | Host directory used by the commented external-key `secrets:` recipe |

### Development and test only

| Variable | Default | Purpose |
| --- | --- | --- |
| `TUCANO_WEB_DIR` | unset | Serve GUI assets from a directory on every request and enable the dev live-reload endpoint; production uses the embedded bundle |
| `TUCANO_FX_RATES_FILE` | unset | Load exchange rates from a local file instead of the network; used by tests and offline runs |
| `TUCANO_FIREFOX` | unset (uses `firefox` on `PATH`) | Browser executable used by [`gui-test/browser-smoke.py`](../gui-test/browser-smoke.py); not read by the server |

## Secret files

Create the directory outside version control and give it to the container
user (uid 10001), keeping the key files readable only by that user:

```sh
mkdir -p secrets
chmod 755 secrets                              # traversable by uid 10001
openssl rand -hex 32 > secrets/session.key     # 64 hex characters = 32 bytes
openssl rand -base64 24 > secrets/vault.key    # 32 base64 characters = 32 bytes
chmod 600 secrets/session.key secrets/vault.key
sudo chown 10001:10001 secrets/*.key
```

`chmod` alone is not enough: a host-owned `0600` file is unreadable inside the
distroless container, so the vault key would fail to open (a hard boot failure)
and the session key would be silently ignored. If the container runs with a
different `--user`, `chown` the files to that uid/gid instead.

Mount the files read-only and point the `_FILE` variables at them:

```sh
docker run -d --name tucanotime \
  --read-only --tmpfs /tmp \
  --cap-drop ALL --security-opt no-new-privileges \
  -p 127.0.0.1:8080:8080 \
  -v tucanotime-data:/data \
  --mount type=bind,src="$PWD/secrets/session.key",dst=/run/secrets/session.key,readonly \
  --mount type=bind,src="$PWD/secrets/vault.key",dst=/run/secrets/vault.key,readonly \
  -e TUCANO_SESSION_SECRET_FILE=/run/secrets/session.key \
  -e TUCANO_SECRET_KEY_FILE=/run/secrets/vault.key \
  tucanotime
```

The file contents are trimmed before use. `TUCANO_SESSION_SECRET` shorter than
32 bytes is a hard failure in production. A too-short or unreadable
`TUCANO_SESSION_SECRET_FILE` is ignored and the auto-created
`<data>/session.key` is used instead; if that file cannot be persisted,
production refuses to start. Outside production shorter values are accepted
with a warning. A vault key must be exactly 32 bytes after trimming:
`openssl rand -hex 32` prints 64 characters and is rejected for the vault key.
`./secrets/` is listed in [`.gitignore`](../.gitignore) and
[`.dockerignore`](../.dockerignore), so key material is never staged in git or
baked into the image.

## Credential vault lifecycle

On first boot the server creates `<data>/vault.key` automatically: 24 CSPRNG
bytes encoded as 32 base64url characters (192 bits), written atomically with
mode `0600`. An existing key file that is group- or world-readable is tightened
to `0600` on read; if that cannot be applied, boot fails with an operator
message.

The vault opens only under the right key:

- A `secrets.bin` store with no reachable key is a **hard boot failure**.
- A malformed key (not exactly 32 bytes) or a wrong key is a **hard boot
  failure**.
- `TUCANO_VAULT_ENABLED=false` (or `vault_enabled: false`) with a `secrets.bin`
  on disk is also a **hard boot failure**: the stored credentials would be
  unreadable. Disable the vault through the lifecycle endpoint only while no
  credential is stored.
- The app never silently regenerates a key over an existing store.
- If the app cannot **write** a newly generated key, the vault is disabled for
  that boot (`vault_disabled`, reason `write_failed`) and the server continues
  running. This is different from a store that cannot be opened: nothing is
  stored in plaintext, and Settings can be used once the data directory is
  writable.

While `TUCANO_SECRET_KEY`, `TUCANO_SECRET_KEY_FILE` or a valid
`TUCANO_VAULT_ENABLED` is set, the vault is **operator-managed**: the enable,
disable and regenerate endpoints refuse with `409 vault_managed_externally`,
and `can_enable`/`can_disable` report the same to the GUI. Remove the variable
to hand the lifecycle back to the app. The key itself is never returned to a
client or written to a log; **Settings → Security** shows only its source, the
fixed file name `vault.key` and a `sha256:…` fingerprint.

Removing the last stored secret deletes `secrets.bin`.

> **Backup trade-off.** The generated `vault.key` sits beside `secrets.bin`, so
> a copy of the data directory contains both the ciphertext and the key.
> Protect that backup accordingly, keep an extra copy of the key elsewhere, or
> supply the key externally with `TUCANO_SECRET_KEY_FILE` and leave it out of
> the volume.

## Known limitations

- **`TUCANO_SSO_ADMIN_GROUP` is not read uniformly.** A user provisioned
  just-in-time reads the value through the configuration resolver
  (environment > `config.json` > default). An existing user signing in again
  has their role refreshed from the **environment variable only**
  ([`src/api/auth.rs`](../src/api/auth.rs#L271)). If you need the admin-group
  mapping to apply to existing SSO users, set `TUCANO_SSO_ADMIN_GROUP` rather
  than only `sso_admin_group` in `config.json`.
- **Nonstandard SMTP ports use plaintext.** Ports 465 and 587 enable verified
  TLS automatically; use a trusted local relay or TLS sidecar for other ports.
- **`vault_enabled` is read-only through `PUT /api/admin/config`.** Only the
  dedicated lifecycle endpoints write it.

---

Last verified against TucanoTime 0.1.41.
