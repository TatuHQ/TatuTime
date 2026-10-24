# Code, refactor and data-layout review — TatuTime (2026-10-15)

> *Historical snapshot — not current guidance. See the reviews index.*

Branch: `fix/gui-diagnosable-refusals` (three commits on `23e3af9`). Method: read the
source, run every gate in the repository, and drive the running server with crafted
payloads — each claim below carries the `file:line` or the command that produced it.
Priorities are SECURITY → CORRECTNESS → MAINTAINABILITY → EFFICIENCY, and must-fix
findings are separated from suggestions, per `AgentRules/coding/code-review.md`.

**Scope honestly stated:** the audit covered `src/lib.rs`, `src/store.rs`,
`src/domain.rs`, `src/api/*`, `web/js/*` and the `gui-test/` harness in the areas
this change touches (storage, expenses/receipts, error surfaces, routing). The
payment, calendar, SSO and accounting modules were only checked at the level of
their already-tested invariants (§4) — they were not re-audited line by line, and
nothing in this document should be read as a clean bill of health for them.

## 1. Must-fix findings, fixed in this branch

### 1.1 The request cap sat below the field rule: a receipt could never be stored · CORRECTNESS · M
`src/lib.rs` read bodies through `RequestBodyLimitLayer::new(256 * 1024)`, while
`validate_expense_input` accepts 4 MB of base64 (`src/domain.rs:1147-1153`) and the
dialog promises "up to 3 MB" (`web/index.html`). Reproduced against a running
server: a 100 KB receipt → `201`, a 1 MB receipt → **`413 length limit exceeded`**
without ever reaching a handler. axum's own 2 MB `DefaultBodyLimit` compounded it
(a 4 MB body answered `400 failed to read the request body`).
**Fixed:** one `MAX_REQUEST_BODY_BYTES` (6 MiB) for both layers, above the field
rule, so the handler — not the transport — answers, in the app's own shape.
Pinned by `a_megabyte_receipt_is_stored_and_an_over_long_one_is_the_apps_own_422`
and the realigned `oversized_body_is_rejected_without_persistence`.

### 1.2 A hollow receipt was storable, and the register hid it · CORRECTNESS · S
A 0-byte file stored `receipt_name` with `receipt_b64: ""`; the register's mark
keyed on the bytes, so the row looked like it had no receipt at all. Three such
records exist in the live data dir (`data/expenses/5f0048c8-*.json`,
`bd8dd360-*.json`, `89a6f851-*.json` — `receipt_name: "123.pdf"`,
`receipt_b64: ""`).
**Fixed:** the dialog refuses an empty file and anything over 3 MB before the
round trip (`web/js/expenses.js`), the column names the stored file as the link
that opens it, and a legacy hollow record says "the attached receipt is empty"
instead of pretending. Pinned by `gui-test/receipt.test.mjs`.

### 1.3 A wrong-typed field was reported as a field that does not exist · CORRECTNESS · S
`api::deser_error` took the first backticked token of serde's message, but serde
quotes the *value* there. Reproduced: `POST /api/customers {"name":42,…}` answered
`{"field":"42","message":"wrong type or format"}` — a key nobody has, and no
statement of what was wrong, on the one path that tells a caller what to fix.
**Fixed:** the field comes only from serde's field markers; otherwise it stays
empty and the reason is serde's own sentence with the position of the offending
value, and the GUI renders an empty field as the reason alone (no dangling colon).
The existing `wrong_type_is_validation_failure` asserted neither, which is why this
survived; it now asserts both, plus three unit tests for the message shapes.

### 1.4 `TATU_WEB_DIR` let a running server serve a GUI newer than its own contract · OPERABILITY · S
Not a code defect, but the failure mode that produced the report that started this
work: with `TATU_WEB_DIR` set (`src/lib.rs:745-791`) the served modules are the
working tree while the Rust is fixed at compile time, so a process started before
a pull hands out today's dialog over yesterday's API — which reads as
`original_amount_minor: unknown field`. Reproduced end to end (the running process
served a file created seven hours after it started), then fixed operationally by
restarting it. **Made self-diagnosing:** an `unknown field` rejection now says the
server is older than the page (`web/js/dom.js`).

## 2. Findings left open

### 2.1 No pagination on the list endpoints · CORRECTNESS/SCALE · MED · S
`GET /api/entries` takes `date|from|to` (`src/api/entries_reports.rs:91-98`) and
`GET /api/expenses` `customer_id|from|to` (`src/api/expenses.rs:54-61`); the range
is capped at 400 days (`MAX_RANGE_DAYS`) and a collection at `MAX_DOCS = 100_000`
(`src/store.rs:22-38`), so **one request can serialise 100 000 documents**. This is
the first thing multi-user growth breaks, before I/O does.
**Recommend:** a `limit` (with a default) plus a cursor or explicit windowing on
both endpoints, and a contract test that a wide range is truncated, not dropped.

### 2.2 Per-user reads deserialise every user's documents · SCALE · MED · M
Entries (`entries/<YYYY-MM-DD>/<uuid>.json`, `src/store.rs:1349-1353`) and expenses
(`expenses/<uuid>.json`) live in shared collections; `user_id` is on the documents
(`Entry` `src/domain.rs:1655`, `Expense` `:999`, `ExpenseClaim` `:1270`, `Timer`
`:1297`, `Submission` `:1422`) and filtering happens in the API layer through
`visible_to` (`src/api/mod.rs:322`). A member's day/week request therefore reads
and decodes every user's entries for those days.
**Recommend (keeps the documented layout):** a rebuildable per-user side index —
`entries/.idx.user.<uuid>.json` (date → ids) and the expense equivalent —
maintained under the same write lock and skipped by the collection reader exactly
like `invoices/.idx.numbers.json` and `users/.idx.emails.json` (`src/store.rs:380`
defines "a dot file is not a document"; the index rebuild is already tested at
`src/store.rs:2472-2523`). A missing or stale index must fall back to a scan, so it
can never lose data. Cost: one read-modify-write map per write, the shape
`record_invoice_number` already has (`src/store.rs:461-465`).

### 2.3 Nothing ties the storage layout to its documentation · MAINTAINABILITY · LOW · S
`AGENTS.md` lists the layout, and `src/store.rs` re-derives it (`Entity::DIR`
constants, `src/store.rs:148-225`; the customer tree at `src/store.rs:1185-1290`;
per-user notifications at `:1089`). Both are correct today — path components are
validated (`ProjectCode::parse`, `src/domain.rs:123-134`: alphanumeric plus `-`/`_`,
≤ 24 chars; ids are `Uuid`; dates are `NaiveDate`), so traversal input cannot reach
the filesystem as the module doc claims (`src/store.rs:1-8`) — but the two lists can
drift silently.
**Recommend:** a guard in the spirit of `tests::route_parity`: assert the directory
set the store creates and uses equals the documented set.

### 2.4 References that never expire · SCALE · LOW · M
`entries/<date>/` grows forever: queries stay bounded by `MAX_RANGE_DAYS`, but no
maintenance job archives or vacuums old days (only `.tmp` pruning runs, at open).
**Recommend:** an ADR for retention/archival before a multi-year instance is
expected, with the same atomicity rules as the rest of the store.

## 3. Refactor suggestions (non-blocking)

- **`tests/contract.rs` is 9 150 lines.** Split it the way `src/api/*` is split — a
  `tests/contract/` tree with one module per area. Mechanical, no behaviour change,
  and the file's size is now the largest friction in reviewing a change.
- **`web/js/expenses.js` carries four concerns** (categories, the expense dialog,
  receipt I/O, submissions). The receipt half — read/refuse, preview, download, and
  its dialog wiring — is self-contained and would mirror the existing module split
  (#264) as `web/js/receipt.js`.
- **The GUI's failure surfaces deserve a README section.** `web/js/dom.js` +
  `web/js/api.js` now hold three deliberate explainers (a refused verb names what
  the endpoint takes; an `unknown field` names a server older than the page; a
  field-less failure reads as its reason). The next feature should reuse them
  rather than invent per-dialog wording.
- **Naming a field for a serde type mismatch** needs `serde_path_to_error` or an
  offset→key mapping over the request body. Deliberately not taken here: it is a
  dependency decision, not a reflex, and §1.3's fix already stops the wrong answer.

## 4. Verified good (so nobody re-audits them blind)

- **Layout invariants:** one document per `Uuid`, atomic writes (tmp + rename),
  `.tmp` pruning at open, dot files excluded from collections, indexes rebuildable
  from the collection itself.
- **Locking (#18/#185/#255):** the memoized `LockSnapshot` keyed on directory
  mtimes, fail-closed `503 lock_unavailable`, and the shared
  `domain::already_invoiced` predicate are used consistently on the paths this
  change touched.
- **Secrets and identity:** the vault fails closed without `TATU_SECRET_KEY`
  (#77), SSO's `allowed_domains` is a fail-closed allow-list (#260), and
  `password_hash` never reaches a response.
- **Money and time:** minor units and hundredths only; the receipt is the single
  place bytes enter a document, and it is bounded by length rather than parsed.
- **The gates, all green at this commit:** `cargo test --all-targets`
  (154 lib + 175 contract, 1 ignored), `cargo clippy --all-targets -- -D warnings`,
  `cargo fmt --check`, `node contrast.mjs`, every `gui-test/*.test.mjs` file,
  `node gui.mjs` against a live server (324 checks) and
  `python3 gui-test/browser-smoke.py` (129 checks, real Firefox).

## 5. Suggested follow-up tickets

1. `limit`/cursor on the list endpoints (§2.1) — MED, small.
2. Per-user side index for entries and expenses (§2.2) — MED, medium.
3. Layout guard against `AGENTS.md` (§2.3) — LOW, small.
4. Split `tests/contract.rs` (§3) — LOW, medium.
5. Retention/archival ADR for `entries/<date>/` (§2.4) — LOW, medium.
