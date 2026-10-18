# TucanoTime

A timesheet system: record time on a daily and weekly basis, organised by
**customer** and **project code**, each carrying its own **currency and hourly
rate**. Built as part of the [Tucano Technology](https://github.com/TucanoTechnology)
suite — file-based (no database), one container, browser GUI.

## Features

- **First-run wizard** — new users land on a guided modal that creates the first customer and project and drops them straight into a ready entry form; the 🚀 sidebar entry reopens it any time (#111).
- **Day view** — add, edit and delete line items for a single date.
- **Week grid** — Mon–Sun overview per customer/project; click a cell to add or adjust.
- **Customers, projects & tasks** — three Setup sections with hierarchy filters (#182): projects filterable by customer, tasks by customer and project; a customer sets the default currency/rate and each project carries its own currency and rate.
- **Exact money** — rates in minor units and hours in hundredths; no floats on persisted values.
- **Reports** — totals grouped by customer, project or ISO week; **CSV export** (formula-injection safe).
- **Invoices** — generate a draft from a period's billable work, snapshotting each entry's rate; issuing **permanently locks** its entries (invoiced time can never be changed — the Timesheet views show a lock that links to the invoice), **archives a PDF** for download, email attachment (#113) and audited PDF copies to third parties such as the accountant (#112), and the invoice preview embeds that actual document; record **full or partial payments** (#114, a per-invoice ledger with a partly-paid state) or **write off** uncollectable balances, and an **outstanding/overdue** dashboard tracks the balance still owed.
- **Invoice overview** — open/paid tiles, a year-navigable monthly Open-vs-Paid chart, Open/All lists with search, customer/project/date filters, sortable columns and column visibility — row selection (click or keyboard) opens the document preview (#135).
- **Tracked-work wizard** — a staged create-from-time flow (customer → period & projects with uninvoiced-hours per project → live review via `POST /api/invoices/preview`) that saves a draft only after the review; project-scoped generation excludes other projects' work (#134).
- **Retainers** — advance funds per customer/project as an append-only ledger: opening, add funds, draws with reason, closable; balances are always reconciled from transactions, never stored (#144).
- **Appearance & messages** — invoice document accent (printed into the archived PDF) and sender display name / Reply-To, all honored by the real transports — no inert settings; credentials stay in the vault (#146).
- **Catalog & labels** — reusable Product/Service line types prefill manual invoice lines (archive keeps history); field labels rename the document's wording without touching API names (#147).
- **Invoice templates** — org-wide subject/body/footer with `%variable%` placeholders (markdown subset), per-customer notes, subject overrides and payment terms (Upon Receipt / NET 15–45 / custom) driving the due date at issue (#116).
- **Getting paid** — no hosted-checkout or payment-provider integration: each invoice shows the customer's own payment details, and settlement is recorded manually (full or partial) on the invoice ledger. Accounting sync (#33) remains available on the API and a background retry job, but is no longer surfaced as per-row invoice buttons (#129).
- **Expenses** — record costs per project with categories, billable flags, and optional attached receipts; billable expenses roll into invoices.
- **Timesheet submissions** — submit a week for approval; submitted/approved weeks **lock** their entries (shared lock seam); rejecting releases them.
- **Also shipped** — running **timer** and month calendar of logged time (#14/#140); imported **calendar events** with Google OAuth (#15/#36); **SSO sign-in** (OIDC/signed-token, JIT provisioning — Beta adapters, #32) with a **fail-closed allowed-domains list** (#260); per-user **notifications** (#22/#52); **recurring invoice schedules** (weekly/monthly/quarterly, drafts still need an explicit issue, #26); **expense reimbursement claims** (#24); **budgets** burn alerts + per-project budget vs actual (#30) and **profitability** reporting (revenue vs labour cost, admin-only, #29); **email delivery** of invoices with the archived PDF attached (#35/#130); role-based access — members see only their own time (#51); an **audit log** (#52) and an operator **backup/restore CLI** on the same image (#94).

## Quick start (Docker)

The image is one stripped binary on a **distroless** base (no shell, no package
manager) and runs as uid **10001** with a read-only root filesystem: only the
`/data` volume and a small `/tmp` tmpfs are writable.

```sh
docker build -t tucanotime .
docker run -d --name tucanotime \
  --read-only --tmpfs /tmp \
  --cap-drop ALL --security-opt no-new-privileges \
  -p 127.0.0.1:8080:8080 \
  -v tucanotime-data:/data \
  tucanotime
```

Or with Compose, where that hardening is already declared for both services:

```sh
docker compose up -d                            # http://127.0.0.1:8080/
TUCANO_HOST_ADDR=0.0.0.0 docker compose up -d   # publish on the LAN instead
```

Open <http://localhost:8080/> — the API docs are at <http://localhost:8080/docs>.
The whole JSON API lives under `/api`, which leaves the top-level paths as
bookmarkable GUI views — `/timesheets/day|week|calendar`, `/customers`,
`/projects`, `/tasks`, `/invoices`, `/expenses`, `/approvals`, `/reports` and
`/settings/security|users|invoices|catalog|expenses` (case-insensitive).
All data lives in the mounted volume; back it up like any document folder.

## Persistence & upgrades (#94)

**Everything an installation knows lives in one place: the data dir**
(`TUCANO_DATA_DIR`, `/data` in the image). Containers are disposable; the
volume is the installation:

| What | Where | Survives `up -d` / image update |
|---|---|---|
| Users, entries, invoices, expenses, notifications… | `<data>/…` JSON documents | ✅ (volume) |
| Integration credentials (Settings tab) | `<data>/secrets.bin`, AES-256-GCM | ✅ — **requires** the same `TUCANO_SECRET_KEY[_FILE]` |
| Runtime settings (reminder cadence, SSO group/domains, OAuth redirect, provider base URLs, demo flags…) | `<data>/config.json`, env > file > default | ✅ (volume; no env needed) |
| Session signing key | `<data>/session.key`, auto-created 0600 on first boot | ✅ — restarts no longer log everyone out |
| Scheduler last-run state, revocations, audit log | `<data>/…` | ✅ |

Precedence everywhere is **explicit env → `config.json` → built-in
default**, so you can pin values with env at the OS layer or persist them on
the volume via `PUT /api/admin/config` / `GET /api/admin/config` (masked to the
non-secret whitelist by construction).

Secret files (bind-mount / Docker secrets) work for both keys:
`TUCANO_SESSION_SECRET_FILE` and `TUCANO_SECRET_KEY_FILE` — and that is the
**recommended** way to supply them: the operator mounts a file, never exports a
secret into the environment or the Compose file (see the commented `secrets:`
block in [`docker-compose.yml`](docker-compose.yml)). If the vault store exists
on disk but the key is missing or wrong, the app **refuses to start** with a
clear message — a silently disabled Settings tab has historically looked like
lost data, and we'd rather stop than mislead.

### Upgrading an image

```sh
docker compose pull && docker compose up -d   # nothing to reconfigure
```

### Backup / restore

```sh
docker compose run --rm tucanotime-cli --backup /backups/monday.tar.gz
# restore (stop the server first; refuses non-empty dirs without --force):
docker compose stop tucanotime
docker compose run --rm tucanotime-cli --restore /backups/monday.tar.gz
docker compose up -d tucanotime
```

Archives are plain `tar.gz` with a `manifest.json` (version + sha256 per
file); restores verify checksums before writing, and warn if the archive
carries a vault store while no key is configured.

### Bind mounts

The container runs as uid **10001**; bind-mounted host dirs need to be
writable by it (`chown -R 10001:10001 ./Data`), or just use a named volume.
A named volume is the recommended default — it is the only writable path the
hardened container needs.

### Container security

The server needs no writable path outside `/data`, so the shipped image and
both Compose services are locked down by default:

| Setting | Effect |
|---|---|
| `read_only: true` + `tmpfs: ["/tmp"]` | read-only root filesystem; only `/data` (volume) and `/tmp` (tmpfs) accept writes |
| `cap_drop: ["ALL"]` | no Linux capabilities at all |
| `security_opt: ["no-new-privileges:true"]` | no setuid/setgid escalation (enforced by the runtime, not a label) |
| `user: "10001:10001"` | numeric, non-root uid/gid fixed in the image and in Compose |
| `init: true` | a tiny init reaps orphans and forwards SIGTERM |
| `stop_grace_period: 30s` | the server finishes in-flight writes before exit |
| `ports: 127.0.0.1:8080:8080` | loopback-only by default; set `TUCANO_HOST_ADDR=0.0.0.0` for LAN use |
| distroless runtime | no shell, no package manager, no apt state — `docker run --entrypoint sh` fails by design |

The image base is pinned by digest, the runtime layer installs nothing, and the
builder compiles dependencies in a separate cached layer, so rebuilds after a
source edit recompile only this crate.

## Authentication

TucanoTime is single- or multi-user with **session-cookie auth** (argon2-hashed
passwords, HMAC-signed HttpOnly cookies). No users exist initially:

- On first visit the app shows a **create-administrator** form (the first account
  becomes an `admin`). After that, login is required for all data.
- Sessions survive restarts out of the box: the signing key is
  `TUCANO_SESSION_SECRET` / `TUCANO_SESSION_SECRET_FILE` when set, else an
  auto-created `<data>/session.key` on the volume (#94) — no operator setup
  needed, and container updates keep everybody signed in.
- Cookies carry `Secure` when `TUCANO_ENV=production` **and** are plain-HTTP-LAN
  friendly via the independent `TUCANO_SECURE_COOKIES=0` axis (A5): the key
  controls secret-strength policy, the flag controls only the cookie attribute.

Environment variables (all optional; precedence env > `config.json` > default):

| Variable | Effect |
|---|---|
| `TUCANO_DATA_DIR` | data folder (`/data` in the image) |
| `TUCANO_PORT` | listen port, default `8080` |
| `TUCANO_SESSION_SECRET[_FILE]` | session signing key; unset ⇒ auto-persisted `<data>/session.key` |
| `TUCANO_SECRET_KEY[_FILE]` | 32-byte vault key; unset ⇒ vault disabled (fail-closed) |
| `TUCANO_ENV=production` | strict secret policy + `Secure` cookies by default |
| `TUCANO_SECURE_COOKIES=0` | drop the `Secure` flag for plain-HTTP serving on a trusted LAN |
| `TUCANO_MAX_DOCS` | per-collection document cap, enforced on writes (a create past it is refused with 409 `collection_full`); reads always list what is stored, so a pre-existing over-cap directory stays recoverable |

```sh
# Preferred: mount the key as a file (uid 10001 must be able to read it).
mkdir -p secrets && openssl rand -hex 32 > secrets/session.key && chmod 600 secrets/session.key
docker run -d --name tucanotime \
  --read-only --tmpfs /tmp \
  --cap-drop ALL --security-opt no-new-privileges \
  -p 127.0.0.1:8080:8080 \
  -v tucanotime-data:/data \
  --mount type=bind,src="$PWD/secrets/session.key",dst=/run/secrets/session.key,readonly \
  -e TUCANO_SESSION_SECRET_FILE=/run/secrets/session.key \
  tucanotime
```

`TUCANO_SESSION_SECRET` (plaintext env) still works, but a file mount keeps the
secret out of `docker inspect` and the process environment.

Roles: `admin` (user management) and `member` (timesheet work).

**SSO (Beta, #32)** is additive to local password login. Only email domains
listed in `TUCANO_SSO_ALLOWED_DOMAINS` / `config.json:sso_allowed_domains` /
vault `sso.allowed_domains` are trusted. An **empty allow-list is fail-closed**
(#260): the server logs a loud boot error, registers no identity provider and
just-in-time provisions nobody until a domain is configured.

**Integration credentials (#77):** admins store provider secrets (OAuth,
SMTP, ICS feed URLs) in the **Settings** tab; they are encrypted at rest with
AES-256-GCM under `TUCANO_SECRET_KEY` (a 32-byte env var) and never returned or
logged. Without `TUCANO_SECRET_KEY` the vault is disabled (fail-closed).
`secrets.bin` holds the encrypted blob.

## Development

```sh
cargo run                     # serve GUI + API on :8080 (data in ./Data)
cargo test                    # unit + contract tests
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

### GUI live reload (dev mode)

To serve GUI assets from disk on every request (no Rust rebuild needed for HTML/CSS/JS changes), start the server with:

```sh
TUCANO_WEB_DIR=./web cargo run
```

**How it works:** when `TUCANO_WEB_DIR` is set, static files are read from that directory on every request rather than from the compiled-in bundle.

**Auto-reload:** a tiny script is injected into HTML pages that opens an SSE connection to `/_dev/reload`. The server polls the `web/` directory every 300 ms and fires a reload event whenever any file's modification time changes, causing the browser to refresh automatically on save.

**Production:** without `TUCANO_WEB_DIR`, the embedded bundle runs as always and `/_dev/reload` returns 404 — no dev overhead in production.

Storage is file-based JSON under `TUCANO_DATA_DIR` (the API is the only
writer). The canonical layout — customers/projects/tasks, entries by day,
invoices (+ archived PDFs and the `.seq.json` number ledger), users,
categories, expenses, submissions, and the `config.json` / `session.key` /
`secrets.bin` / `audit.log` / `revoked.json` / `scheduler.json` state files —
is documented in [`AGENTS.md`](AGENTS.md) "Architecture invariants" so there
is exactly one list to keep current.

The REST surface is defined by [`openapi.json`](openapi.json) and served at
`/openapi.json`; change both together. See [`AGENTS.md`](AGENTS.md) for the
rules AI agents follow in this repository.

Code and documentation style is recorded in
[`docs/coding-style.md`](docs/coding-style.md) — the Rust style bar this
repository holds itself to, with
[*Rust by Example*](https://doc.rust-lang.org/rust-by-example/) as its reference
exemplar for idiomatic, example-led Rust.

## Architecture decision records

- [ADR-001 — Filesystem storage, entity ownership, and integration seams](docs/architecture/adr-001-filesystem-and-seams.md)
- [ADR-002 — E-invoicing standard and provider scope](docs/architecture/adr-002-einvoicing-scope.md) (decision record for #148: EN 16931 core; Peppol BIS 3.0 UBL + Factur-X generation at v1; transport only through a provider port — state-clearance schemes and PDP certification are explicitly out of scope)

## License

[GNU Affero General Public License v3.0](LICENSE).
