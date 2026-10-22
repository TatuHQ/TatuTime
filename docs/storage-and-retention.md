# Storage and retention

TucanoTime uses the filesystem as its store: one pretty-printed JSON document
per record below `TUCANO_DATA_DIR`. There is no database. The API is the only
writer, writes are atomic (temporary file + rename), and the GUI never reads
storage — it is a presentation layer over the API.

The canonical layout list is the “Architecture invariants” section of
[`AGENTS.md`](../AGENTS.md); this page is the human summary.

## Layout

| Path | Contents |
| --- | --- |
| `customers/<id>.json` | Customer, including its default currency and rate |
| `customers/<id>/projects/<CODE>.json` | Project, with its own currency and rate |
| `customers/<id>/projects/<CODE>/tasks/<TASK>.json` | Optional task tier |
| `entries/<YYYY-MM-DD>/<id>.json` | Time entries, one document per entry in a per-day folder |
| `expenses/<id>.json` | Expenses, including optional receipt metadata |
| `invoices/<id>.json` | Invoice document |
| `invoices/<id>.pdf` | PDF archived at issue; never rewritten |
| `invoices/.seq.json` | Sequential invoice-number ledger |
| `invoices/.timeline.<id>.json` | Per-invoice timeline sidecar |
| `users/<id>.json` | User account (password hash never serialised to clients) |
| `categories/<id>.json` | Expense categories |
| `submissions/<id>.json` | Weekly timesheet submissions |
| `claims/<id>.json` | Expense reimbursement claims |
| `retainers/`, `schedules/`, `timers/`, `items/`, `notifications/` | Retainer ledgers, recurring schedules, running timers, catalog items and notifications |
| `sync/accounting.json` | Accounting-sync state |
| `config.json` | Externalised configuration |
| `secrets.bin` | AES-256-GCM encrypted integration credentials |
| `vault.key` | Auto-created vault master key, mode `0600` |
| `session.key` | Auto-created session signing key, mode `0600` |
| `audit.log` | JSON-lines security event log |
| `revoked.json` | Revoked session ids |
| `scheduler.json` | Scheduler last-run state |
| `email_reminders.json` | Overdue-invoice email reminder last-run state |
| `.server.lock` | Single-instance guard |
| `.tucanotime.lock` | Cross-process writer lock serialising atomic writes |
| `archive/entries/<YYYY-MM>/<YYYY-MM-DD>/<id>.json` | Cold entry tree written only by the archival pass |
| `archive/entries/.manifest.<UTC stamp>-<random>.json` | Per-run sha256 manifest of the documents a pass relocated |

Indexes are dot files (`invoices/.idx.numbers.json`, `users/.idx.emails.json`,
`entries/.idx.users.json`, `expenses/.idx.users.json`,
`archive/entries/.idx.ids.json`). Dot files are never documents: the collection
reader skips them, so they do not count toward the document cap and are never
returned by a listing. An index is a hint, never a source of truth; a stale or
damaged index is rebuilt or bypassed.

## Exact money and time

- Amounts are **minor units** (`u64` cents). Hours are **hundredths** (`u32`).
  Floating-point values never touch persisted data.
- A **project always carries its own currency and rate**. The customer holds the
  default used to prefill new projects and to resolve legacy project documents
  written before those fields were required.
- Tasks carry no currency or rate overrides. Legacy task billing fields are
  ignored on load.

These invariants are recorded in [ADR-001](architecture/adr-001-filesystem-and-seams.md).

## Invoice permanence

An invoice line snapshots the resolved rate when the invoice is generated. Issue
the invoice and the archived document becomes the record of what was billed:

- Draft invoices lock nothing and can be edited or deleted.
- A non-draft invoice (`issued`, `partly_paid`, `paid`, `written_off`) freezes
  every entry and expense it references.
- Settling or writing off an invoice never releases the referenced records.
- `already_invoiced` excludes every non-draft invoice, so a settled or
  written-off invoice is never billed again either.
- Deleting the invoice itself — `DELETE /api/invoices/{id}`, admin tier, any
  status — is the only operation that releases what it locked: the document,
  its archived PDF (an imported original included), its payment ledger and its
  `invoices/.timeline.<id>.json` sidecar go in one writer-locked write, a
  dangling `supersedes`/`superseded_by` link is cleared, and `audit.log`
  records `invoice_deleted`.

The GUI reflects this by hiding edit/delete actions on invoiced rows and showing
a lock that links to the invoice. See [Security](security.md#record-locks-and-invoice-permanence)
for the lock providers.

## Entry growth and archival

Entries accumulate one day folder per day the installation is used. A small team
writes very little data, but the number of day folders and the size of the
per-user index grow with the age of the installation, and id-addressed reads and
lifetime scans (budget totals) walk those folders.

The retention decision is recorded in
[ADR-003](architecture/adr-003-entry-retention-and-archival.md):

- **There is no automatic deletion or expiring job.**
- A whole day folder can be relocated to the cold tree
  `archive/entries/<YYYY-MM>/<YYYY-MM-DD>/<id>.json`, preserving the same bytes
  and writing a per-run sha256 manifest.
- A day may move only when none of its entries is locked or referenced by **any
  non-draft invoice**. Locked and invoiced days are never archived.
- Every reader merges hot and cold data: a relocated day still reads by date,
  range and id, and lifetime scans count it exactly once. On a duplicate, the hot
  copy wins.
- Writing to a cold entry rehydrates it into the hot tree, leaving exactly one
  document.
- The relocated day's keys are removed from `entries/.idx.users.json` so the
  index shrinks with the documents.

The cold tree and its id index are included in normal backups. An operator-run
archival pass is a maintenance action, not a background scheduler job; the
mechanics and preconditions are in ADR-003.

---

Last verified against TucanoTime 0.1.41.
