# AI Agent Rules — Tucano Time

Rules and guidelines for AI agents working on Tucano Time.

## Shared rules

Organisation-wide rules live in [`AgentRules/`](AgentRules/) and apply to every repository.
Read them before making changes. This file records only what is specific to this repository.

**`AgentRules/` is synced automatically from
[TucanoAgentRules](https://github.com/TucanoTechnology/TucanoAgentRules), the single source of
truth shared with [TucanoTestAPI](https://github.com/TucanoTechnology/TucanoTestAPI) and
[TucanoTestGUI](https://github.com/TucanoTechnology/TucanoTestGUI). Do not edit
files under `AgentRules/` in this repository — submit changes against TucanoAgentRules instead and
they will arrive here as an automated pull request.**

---

## Repository-specific rules

### What this project is

TucanoTime is a timesheet system: time entries recorded per day/week against a
customer and project code, with per-customer and per-project currency and hourly
rate. A single Rust binary serves the REST API, the browser GUI and the API docs;
data is file-based JSON (no database), mirroring the TucanoTest suite philosophy.
The GUI is a presentation layer and nothing more — it never reads storage.

### Branching

This repository is **`main`-only**: there is no `develop` branch and none is
expected. Every change — feature, fix, chore or dependency bump — is proposed as
a pull request branched from `main` and merged back into `main`, and Dependabot
targets `main` too. The shared rules in
[`AgentRules/coding/branching-and-git.md`](AgentRules/coding/branching-and-git.md)
describe a Gitflow flow with a `develop` branch; for this repository, read them
as requiring the default branch only and substitute `main` for `develop`
throughout. That divergence is deliberate and repository-specific — `AgentRules/`
is auto-synced from TucanoAgentRules and must not be edited here, so reconciling
the upstream wording is an upstream change, not a local one.

### Releases

The crate version in [`Cargo.toml`](Cargo.toml) **is** the release version: a
PR bumps the patch inside the change that lands, and the merged commit is the
one that gets tagged. **Bump the patch in your PR.**

Releasing is no longer a manual step.
[`.github/workflows/release.yml`](.github/workflows/release.yml) runs on every
push to `main` and releases the commit it is given: it tags that commit
`v<version>` (annotated, as every existing tag is) and creates the GitHub
Release with generated notes.

A version that is already tagged falls back to build metadata instead of
colliding, so no commit can go unreleased: a merge that carries no bump — a
Dependabot dependency bump, which cannot edit `Cargo.toml`, or any human PR that
forgets the bump — is released as `v<version>+<short-sha>`, semver build metadata
identifying the commit. The workflow never deletes, moves or rewrites an existing
tag or release, and re-running it on a commit that already has its tag and
release is a no-op. That is what makes the "a release on every commit that lands
in `main`" rule hold by construction (#352); the carve-out recorded by hand on
the `v0.1.8` release notes is no longer needed.

### Architecture invariants

- Money is **minor units** (`u64` cents) and hours are **hundredths** (`u32`);
  floats never touch persisted values. Rate resolution: a project always
  carries its own currency + rate (required, #11); the customer holds the
  *default* used only to prefill new projects and to resolve legacy documents
  (see `domain::effective_rates`, `domain::project_from_bytes`).
- Tasks carry no currency or rate overrides. Legacy task billing fields are
  ignored on load; task entries use the existing person/project/customer rate
  resolution and project/customer currency resolution.
- Storage layout below `TUCANO_DATA_DIR` (the API is the only writer):
  `customers/<id>.json`, `customers/<id>/projects/<CODE>.json`,
  `customers/<id>/projects/<CODE>/tasks/<TASK>.json` (optional task tier, #38),
  `entries/<YYYY-MM-DD>/<id>.json`, `invoices/<id>.json`, `users/<id>.json`,
  `categories/<id>.json`, `expenses/<id>.json`, `submissions/<id>.json`,
  plus `audit.log` (#52), `revoked.json` (#45), `scheduler.json` (#61),
  `secrets.bin` (#77, AES-256-GCM encrypted), `config.json` + `session.key`
  (#94), `.server.lock` (single-instance guard), the `retainers/`, `claims/`,
  `schedules/`, `timers/`, `items/` and `notifications/` doc folders,
  `sync/accounting.json` (#33), archived
  `invoices/<id>.pdf` (#113), `invoices/.seq.json` (number ledger, B3),
  `invoices/.timeline.<id>.json` (#349 per-invoice timeline sidecar) and
  the `*.idx.*` side indexes (D3): `invoices/.idx.numbers.json`,
  `users/.idx.emails.json`, the #335 per-user read indexes
  `entries/.idx.users.json` + `expenses/.idx.users.json`, and the #350 cold id
  index `archive/entries/.idx.ids.json` (written only by the archival pass; every
  read treats it as a hint), plus the cold tree that
  #349 retention primitive relocates whole day folders to —
  `archive/entries/<YYYY-MM>/<YYYY-MM-DD>/<id>.json`, written by an
  operator-run `archive_entries_before` pass (same bytes, with a per-run sha256
  manifest; every reader merges it back in — a relocated day still reads by date,
  range and id, and the lifetime scans count it exactly once, the #350 read story).
  Dot files are never documents — the collection reader skips them, so a side
  index — the timeline sidecar included — stays out of the `MAX_DOCS` count.
  Atomic writes (tmp + rename).
- **Secret vault (#77):** admin-entered integration credentials are encrypted at
  rest with `TUCANO_SECRET_KEY` (32 bytes) and never returned to clients (masked
  hints only) or logged. Fail-closed: unset key ⇒ vault disabled.
- **Locking (#18 seam):** entry edits/deletes and expense deletes consult
  `CombinedLocks`, which composes `InvoiceLock` (entries **and** expenses on
  any **non-draft** invoice — #8/#114. **Invoiced records are permanent:**
  issuing freezes them and settling (`paid`) or writing off (`written_off`)
  does **not** release them, because the archived invoice document is the
  record of what was billed and the timesheet must never drift from it. The
  only way back to editable is before issue: drafts lock nothing and can be
  deleted; the GUI reflects this by hiding the actions of invoiced rows and
  showing a lock that links to the invoice), `SubmissionLock` (entries in a
  submitted/approved week, #16) and `ClaimLock` (expenses on a
  submitted/approved reimbursement claim, #24 — the #190 follow-up, no longer
  a direct scan in `api::expenses`). Reads are never blocked. If the lock
  state cannot be verified the write is refused fail-closed with 503
  `lock_unavailable` (#185) — never allowed because the check was
  inconclusive. All three providers answer from one memoized
  `store::LockSnapshot` keyed on the source collections' directory mtimes
  (#255): a per-record check no longer re-reads every invoice/submission/claim
  document, a changed directory stamp forces a single re-scan (so the set can
  never go stale behind an atomic tmp+rename write), and a scan failure is
  never cached.
- Every payload is validated against the contract before any write
  (`deny_unknown_fields` + `domain::validate_*`); no partial persistence.
  Errors use the single JSON shape in `openapi.json`; responses never expose
  paths or raw filesystem errors.
- The **contract is `openapi.json`** (checked in, served at `/openapi.json`,
  rendered at `/docs`). Route changes and contract changes land in the same
  commit. Range queries are capped at 400 days.
- **Page routing:** the JSON API is mounted under `/api` (`nest` in
  `build_router`); only `/healthz`, `/openapi.json`, `/docs` and the GUI stay
  at the root. Each GUI view owns a top-level path (`/timesheets/day|week|
  calendar`, `/customers`, `/projects`, `/tasks`, `/invoices`, `/expenses`,
  `/approvals`, `/reports`, `/settings/security|users|invoices|catalog|
  expenses`). `spa_page` (lib.rs) is the case-insensitive allowlist serving
  `index.html`; unknown paths keep the JSON 404. `TAB_ROUTES`/`DEFAULT_ROUTE`
  in `web/js/route.js` mirror that list; `tests::route_parity` fails the build if the two drift (#264).
- `web/` is dependency-free vanilla HTML/CSS/JS embedded via rust-embed (no
  build step). All DOM data is inserted with `textContent`, never `innerHTML`.
- **Auth (#19):** users are file-based (`users/<id>.json`), passwords argon2id,
  sessions are HMAC-signed HttpOnly cookies (`tt_session`). Data routes require a
  valid session; `/api/users` requires `admin`. First-run `/api/auth/bootstrap` creates
  the initial admin only while no users exist. Key: `TUCANO_SESSION_SECRET[_FILE]`
  else the auto-created `<data>/session.key`, so restarts keep sessions (#94);
  the `Secure` cookie flag is the independent `TUCANO_SECURE_COOKIES` axis
  (default on iff `TUCANO_ENV=production`; `=0` allows plain-HTTP LAN serving,
  review A5). `password_hash` is never serialised to clients.
- **SSO (#32, #260):** identity providers (OIDC/signed-token, SAML stub) are
  additive to local login and JIT-provision members from a verified assertion.
  `allowed_domains` is a **fail-closed allow-list**: only listed domains are
  trusted and an empty/unset list denies every domain — `registry_from_vault`
  then registers no provider and logs a loud boot error (#260, mirroring vault
  #77). Provider secrets stay in the vault/env, never in the repo or responses.
- **RBAC / private-per-user (#51):** entries and expenses carry `user_id` (the
  author). A **member** sees/edits only their own records; an **admin** sees all
  and owns `/api/invoices` (admin tier) and submission approval. Enforce via
  `visible_to()` on reads and by ownership check on mutations (a non-owned record
  returns 404, not 403, to avoid leaking existence). Customers/projects/tasks/
  categories are shared org-wide reference data (any authenticated user).
- **Invoices (#8):** generated from a period's billable, not-yet-invoiced entries; each line snapshots the resolved rate at generation, so a later rate change never rewrites an issued invoice. Issuing sets `status=issued`, which the `InvoiceLock` (a `lock::EntryLock` provider wired in `AppState`) uses to reject edits/deletes of referenced entries with 409. Draft invoices do not lock. Every builder (the API draft/preview pipeline and the recurring scheduler) resolves already-invoiced lines through one shared predicate, `domain::already_invoiced`, which excludes **every non-draft** invoice: settling (`paid`/`partly_paid`) or writing off an invoice releases its entries from the edit lock but never re-bills them (#248). Invoice number is sequential (`INV-0001`).
- **Getting paid is provider-free (by design):** there is **no** hosted-checkout
  or payment-provider integration — no Stripe, no PayPal, no webhook receiver,
  no demo-flag env vars. An invoice answers "how does this client pay us" from
  `Customer::payment_details` (bank name, account name/number, IBAN, BIC,
  reference, free-text instructions; `domain::PaymentDetails`), rendered into
  the GUI's *Get paid* panel, the invoice/reminder emails ("How to pay") and
  the document PDF, plus the `%payment_details%` template variable. Settlement
  is recorded manually (`POST /invoices/{id}/pay`, ledger `method: "manual"`);
  nothing in the invoice lifecycle prompts for, blocks on, or depends on an
  external payment provider. Do not reintroduce one.
- **HTTP status codes (C8):** a `POST` returns **201** when it mints a new,
  client-addressable resource (`POST /api/customers`, `/api/entries`,
  `/api/invoices`, `/api/claims` → a new reimbursement claim, …).
  It returns **200** when it is a *command or transition on an existing
  resource* and hands back that resource's new state (`/invoices/{id}/issue`,
  `/invoices/{id}/pay`, `/timer/stop` — which materialises the derived entry
  but is a stop of the existing timer — and `/claims/{id}/submit`). The two
  borderline cases are deliberate, not drift; the codes are pinned in
  `openapi.json`.

### Commands

| Task | Command |
| --- | --- |
| Run (GUI + API on :8080, data in `./Data`) | `cargo run` |
| All tests (domain units + HTTP contract) | `cargo test --all-targets` |
| Format check / apply | `cargo fmt --all -- --check` / `cargo fmt --all` |
| Lint (CI treats all clippy lints as errors) | `cargo clippy --all-targets -- -D warnings` |
| Latest version of a crate | `cargo info <crate>` (or crates.io API) |
| Container image | `docker build -t tucanotime .` |
| Container run | `docker run -d -p 8080:8080 -v tucanotime-data:/data tucanotime` |

The toolchain is pinned by `rust-toolchain.toml` and the Dockerfile/CI images
carry the same version; bump all three in one intentional PR.

### Test policy beyond the shared rules

- Behaviour changes ship with contract tests in `tests/contract.rs` (in-process
  router, temp data dir); cover the invalid shapes too — rejection without
  persistence is part of the contract.
- Accessibility: the GUI keeps keyboard-navigable tabs, labelled fields,
  `aria-live` announcements and passing contrast in light/dark themes; check
  new views against the same bar.


<!-- shared-rules-table:begin -->
| Area | Rules |
| --- | --- |
| API & data contracts | [`AgentRules/coding/api-and-data-contracts.md`](AgentRules/coding/api-and-data-contracts.md) |
| Branching & git | [`AgentRules/coding/branching-and-git.md`](AgentRules/coding/branching-and-git.md) |
| Code review | [`AgentRules/coding/code-review.md`](AgentRules/coding/code-review.md) |
| Dependencies | [`AgentRules/coding/dependencies.md`](AgentRules/coding/dependencies.md) |
| Peak-hour warnings | [`AgentRules/generic/peak-hours.md`](AgentRules/generic/peak-hours.md) |
| Principles | [`AgentRules/generic/principles.md`](AgentRules/generic/principles.md) |
| Ticket template | [`AgentRules/project-management/ticket-template.md`](AgentRules/project-management/ticket-template.md) |
| Ticket workflow | [`AgentRules/project-management/workflow.md`](AgentRules/project-management/workflow.md) |
| Security | [`AgentRules/security/security.md`](AgentRules/security/security.md) |
| Contract tests | [`AgentRules/test/contract.md`](AgentRules/test/contract.md) |
| Performance tests | [`AgentRules/test/performance.md`](AgentRules/test/performance.md) |
| Browser & end-to-end tests | [`AgentRules/test/ui.md`](AgentRules/test/ui.md) |
| Unit tests | [`AgentRules/test/unit.md`](AgentRules/test/unit.md) |
<!-- shared-rules-table:end -->
