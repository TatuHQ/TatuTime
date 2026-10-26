# TatuTime

Self-hosted timesheet and invoicing server — time per customer and project, exact money, invoices. Full documentation is in [docs/README.md](docs/README.md); licensed under [AGPL-3.0-or-later](LICENSE).

## Deployment identifiers (0.2.0)

0.2.0 is a clean-break deployment release: use the `TATU_` prefix for every environment variable,
the `tatutime` image, and the `tatutime-data` / `tatutime-backups` Compose volumes. The stored
documents are unchanged — the JSON layout, field names and `config.json` keys are the same — so an
existing data directory needs no conversion. Point `TATU_DATA_DIR` at it, update the deployment
variables to `TATU_*`, and start the new stack. See [Operations](docs/operations.md) for the current
names.

The default organisation name (`TATU_ORG_NAME`) is `TatuTime`; an explicit Company name in Settings
still wins.

## Interim brand mark

The official TatuHQ logo has not been drawn yet. The app uses the TatuHQ organisation avatar as a
placeholder in `web/assets/tatu.png` and the favicons, with the blue accent tokens in
`web/styles.css` derived from it. When the final artwork lands, replace the mark and favicons, then
update the `--brand` / `--brand-deep` / `--accent` tokens and the note in
[`docs/ubuntu-design.md`](docs/ubuntu-design.md).

## What an invoice looks like

A fresh install already produces a complete document: a `From` block (company,
address lines and legal id under its configured label) beside the `Invoice for`
block (customer, address and company number) and the invoice metadata —
`Subject`, `Invoice ID`, `Issue date`, `Due date`, `Period`. Dates print as
`dd/mm/yyyy` and money as `EUR €1,234.56`; hours are summarised as
`100h 00min`; the totals show `Subtotal` and either the bold `Total` or, once
money was received, `Payments` and `Amount due`; payment details print the
single flat account when no rails are configured, or one block per currency
rail otherwise. Every page is numbered.

The absent invoice template resolves to seeded defaults — the subject
`%customer_name% invoice for %period_month_padded% %period_year%` and `net_30`
terms — so the default PDF needs no setup. Issuing freezes the subject, payment
terms and due date (and the archived PDF is never rewritten); later template or
customer edits do not change an issued invoice. Details:
[docs/invoicing-and-expenses.md](docs/invoicing-and-expenses.md).

## Build locally

Prerequisite: Rust `1.99.1`, as pinned by [`rust-toolchain.toml`](rust-toolchain.toml). The crate uses edition 2024.

```sh
cargo run
```

This serves the GUI and API at <http://localhost:8080/>, stores data in `./Data`, renders the API documentation at <http://localhost:8080/docs>, serves the contract at <http://localhost:8080/openapi.json> and answers health checks at <http://localhost:8080/healthz>.

Override the defaults for a local run:

```sh
TATU_DATA_DIR=$(mktemp -d) TATU_PORT=8099 cargo run
```

Verification commands:

```sh
cargo test --all-targets
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Build a release binary:

```sh
cargo build --release
```

The release binary is written to `target/release/tatu-time`.

## Build and run the container

```sh
docker build -t tatutime .
```

Run it hardened. The root filesystem is read-only, only `/tmp` is a writable tmpfs besides the data volume, every capability is dropped, privilege escalation is blocked and the published port is bound to loopback:

```sh
docker run -d --name tatutime \
  --read-only --tmpfs /tmp \
  --cap-drop ALL --security-opt no-new-privileges \
  -p 127.0.0.1:8080:8080 \
  -v tatutime-data:/data \
  -v tatutime-backups:/Backup \
  tatutime
```

The image is a multi-stage build: the application is compiled in the pinned Rust builder and copied into a distroless runtime with no shell and no package manager. It runs as uid/gid `10001`, listens on port `8080`, keeps all state on the `/data` volume and uses `tatu-time --health` as its container healthcheck.

Or use Docker Compose, where the same hardening is declared for both services. It works with no `.env` file:

```sh
docker compose up -d                            # http://127.0.0.1:8080/
TATU_HOST_ADDR=0.0.0.0 docker compose up -d   # publish on the LAN instead
```

Compose keeps state on the named `tatutime-data` volume. The optional `cli` profile runs backup and restore against the same volume:

```sh
docker compose --profile cli run --rm tatutime-cli --backup /backups/monday.tar.gz
```

Published images are available from the project registry on pushes to `main`:

```
ghcr.io/tatuhq/tatutime:latest
ghcr.io/tatuhq/tatutime:<version>
ghcr.io/tatuhq/tatutime:sha-<short>
```

`:<version>` is the crate version of that release (the same number the
`v<version>` git tag carries), so it is the tag to pin.

## Variables available for containers

The tables below cover the variables a container can use. The full reference, including `_FILE` recipes, `config.json` keys and precedence, is in [docs/configuration.md](docs/configuration.md).

### Core

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TATU_DATA_DIR` | `Data` locally, `/data` in the image | Directory holding every document and state file | — |
| `TATU_BACKUP_DIR` | `Backup` beside the data dir | Directory where Settings → Backup writes its ZIPs; Compose also mounts it at `/Backup` (default named volume `tatutime-backups`) | — |
| `TATU_PORT` | `8080` | TCP port the server and health probe use | — |
| `TATU_ENV` | unset (`production` in Compose) | `production` enables the strict session-secret policy and Secure cookies by default | — |
| `TATU_SECURE_COOKIES` | on iff `TATU_ENV=production` | `0`/`false` drops the cookie `Secure` flag for plain-HTTP LAN serving | — |
| `TATU_MAX_DOCS` | `50000` (10–1,000,000) | Per-collection document cap; a create past it is refused with `409 collection_full` | — |
| `RUST_LOG` | `info` | Tracing filter | — |

### Security and vault

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TATU_SESSION_SECRET` | auto `<data>/session.key` | HMAC key for session cookies; at least 32 bytes in production | `TATU_SESSION_SECRET_FILE` |
| `TATU_SECRET_KEY` | auto `<data>/vault.key` | Credential-vault master key; exactly 32 bytes (the generated key is 32 base64url characters) | `TATU_SECRET_KEY_FILE` |
| `TATU_VAULT_ENABLED` | `true` | Enables the credential vault; a valid `true`/`false` makes the lifecycle operator-managed | — |

### Authentication and SSO

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TATU_SSO_ALLOWED_DOMAINS` | empty (denies every domain) | Comma-separated email domains trusted for SSO; env > `config.json` > vault | Vault fallback: `sso.allowed_domains` |
| `TATU_SSO_ADMIN_GROUP` | empty | IdP group mapped to the admin role; existing users read this variable directly | — |
| `TATU_OIDC_CLIENT_SECRET` | unset | Enables the OIDC/signed-token provider | Vault: `oidc.client_secret` |
| `TATU_OIDC_ISSUER` | `oidc` | Provider name | Vault: `oidc.issuer` |
| `TATU_SAML_CERT_FINGERPRINT` | unset | Pinned SAML certificate fingerprint | Vault: `saml.cert_fingerprint` |
| `TATU_SAML_SIGNING_SECRET` | unset | Shared HMAC secret; SAML refuses to enable without it | Vault: `saml.signing_secret` |
| `TATU_SAML_ISSUER` | `saml` | Provider name | Vault: `saml.issuer` |

### Email

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TATU_SMTP_HOST` | unset (email disabled) | SMTP relay hostname; unset logs mail instead of sending | Vault: `smtp.host` |
| `TATU_SMTP_PORT` | `25` | SMTP relay port | Vault: `smtp.port` |
| `TATU_SMTP_FROM` | `noreply@localhost` | Envelope/From address | Vault: `smtp.from` |
| `TATU_SMTP_USERNAME` | unset | Username for optional `AUTH LOGIN` | Vault: `smtp.username` |
| `TATU_SMTP_PASSWORD` | unset | Password for optional `AUTH LOGIN` | Vault: `smtp.password` |

Ports 465 (implicit TLS) and 587 (STARTTLS) use verified TLS; any other port uses
the plaintext local-relay mode (for a local relay such as MailHog on 1025).

#### Testing email locally with MailHog

The `docker-compose.mailhog.yml` overlay starts the app together with
[MailHog](https://github.com/mailhog/MailHog) and points the app at it:

```sh
docker compose -f docker-compose.yml -f docker-compose.mailhog.yml up -d
```

Open <http://127.0.0.1:8025> for the inbox. MailHog accepts SMTP on port 1025;
the app is configured with `TATU_SMTP_HOST=mailhog`,
`TATU_SMTP_PORT=1025` and `TATU_SMTP_FROM=invoices@tatutime.local`.
If those host ports are already in use, set `MAILHOG_UI_HOST_PORT` and
`MAILHOG_SMTP_HOST_PORT` — the app still reaches the container on 1025.
Vault-stored `smtp.*` settings still win, so use a fresh data volume (or clear
the SMTP settings in **Settings → Email**) when switching to MailHog.

A hardened [Mailpit](https://github.com/axllent/mailpit) overlay is also
available when you prefer its inbox or need to keep captured mail across
restarts:

```sh
docker compose -f docker-compose.yml -f docker-compose.email.yml up -d
```

Its web inbox is at <http://127.0.0.1:8025> and it accepts SMTP on 1025 inside
the compose network. The same `TATU_HOST_ADDR`, `TATU_MAILPIT_UI_PORT` and
`TATU_MAILPIT_SMTP_PORT` variables control the published ports. Use one
overlay at a time — both bind the same default host ports.

### Integrations

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TATU_CALENDAR_ICS` | unset | ICS feed URL or local file path | Vault: `calendar.ics_url` |
| `TATU_CALENDAR_PROVIDER` | unset | `google` or `microsoft` for OAuth calendar import | Vault: `calendar.provider` |
| `TATU_CALENDAR_OAUTH_REDIRECT` | `/api/calendar/oauth/callback` | Redirect URI registered with the OAuth app | — (`config.json`: `calendar_oauth_redirect`) |
| `TATU_GOOGLE_CLIENT_ID` | unset | Google OAuth client id (public half) | — (`config.json`: `google_calendar_client_id`) |
| `TATU_GOOGLE_CLIENT_SECRET` | unset | Google OAuth client secret | Vault: `calendar.google.client_secret` |
| `TATU_GOOGLE_TOKEN` | unset | Google access token | Vault: `calendar.google.access_token` |
| `TATU_GOOGLE_REFRESH_TOKEN` | unset | Google refresh token | Vault: `calendar.google.refresh_token` |
| `TATU_GOOGLE_CALENDAR` | `primary` | Google calendar id | Vault: `calendar.google.id` |
| `TATU_MS_CLIENT_ID` | unset | Microsoft OAuth client id (public half) | — (`config.json`: `ms_calendar_client_id`) |
| `TATU_MS_CLIENT_SECRET` | unset | Microsoft OAuth client secret | Vault: `calendar.microsoft.client_secret` |
| `TATU_MS_TOKEN` | unset | Microsoft access token | Vault: `calendar.microsoft.access_token` |
| `TATU_MS_REFRESH_TOKEN` | unset | Microsoft refresh token | Vault: `calendar.microsoft.refresh_token` |
| `TATU_QBO_TOKEN` | unset | QuickBooks Online OAuth token; enables the provider | Vault: `qbo.token` |
| `TATU_QBO_BASE_URL` | provider default | QuickBooks API base URL override | — (`config.json`: `qbo_base_url`) |
| `TATU_XERO_TOKEN` | unset | Xero OAuth token; enables the provider | Vault: `xero.token` |
| `TATU_XERO_BASE_URL` | provider default | Xero API base URL override | — (`config.json`: `xero_base_url`) |

### Operations

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TATU_REMINDER_DAYS` | `7` (1–365) | Days between overdue-invoice email reminders | — (`config.json`: `reminder_days`) |
| `TATU_ORG_NAME` | `TatuTime` | Fallback organisation name when the Company name is blank | — (`config.json`: `org_name`) |
| `TATU_BASE_CURRENCY` | unset | ISO 4217 base currency for mixed-currency invoice trends; unset disables conversion | — (`config.json`: `base_currency`) |
| `TATU_BACKUP_ENABLED` | `false` | Enable scheduled ZIP backups (Settings → Backup) | — (`config.json`: `backup_enabled`) |
| `TATU_BACKUP_CADENCE` | `daily` | `daily`, `weekly` (Monday) or `monthly` (1st) | — (`config.json`: `backup_cadence`) |
| `TATU_BACKUP_TIME` | `02:00` | Backup time (`HH:MM`) in the server's local time zone | — (`config.json`: `backup_time`) |

### Compose-only interpolation

These variables are read by Docker Compose, not by the binary.

| Variable | Default | Purpose | Secret / file variant |
| --- | --- | --- | --- |
| `TATU_HOST_ADDR` | `127.0.0.1` | Host address the published port binds to; `0.0.0.0` exposes the GUI on the LAN | — |
| `TATU_HOST_PORT` | `8080` | Host port mapped to container port `8080` | — |
| `TATU_SECRETS_DIR` | `./secrets` | Host directory used by the commented external-key `secrets:` recipe | — |

Development and test variables (`TATU_WEB_DIR`, `TATU_FX_RATES_FILE`,
`TATU_FIREFOX`) are documented in
[docs/configuration.md](docs/configuration.md#development-and-test-only).

Precedence: explicit environment **>** `config.json` **>** default for externalised knobs; the credential vault **>** environment for provider secrets; `TATU_SESSION_SECRET` **>** `TATU_SESSION_SECRET_FILE` **>** auto-created `<data>/session.key`; `TATU_SECRET_KEY` **>** `TATU_SECRET_KEY_FILE` **>** auto-created `<data>/vault.key`.

Prefer the `_FILE` variant for secrets in containers: the value stays out of `docker inspect` and the process environment. The full catalogue, `_FILE` recipes and the credential-vault lifecycle are in [docs/configuration.md](docs/configuration.md).
