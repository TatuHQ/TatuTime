# Harvest import implementation status

Harvest import is being developed under #394. The CSV parser (#395), protected
financial-history foundation (#398), bounded ZIP/PDF inspection (#399), unified
preview planner (#400), journaled commit (#401), admin API (#402) and the
`/settings/import` workflow (#403) are implemented. #404 is the remaining
end-to-end verification ticket.

## Financial-history foundation

Optional, read-only import provenance on entries, expenses and invoices
preserves source identity without creating fake invoices or payment events.
Imported time retains its original currency, rate, cost rate and monetary
snapshots, including explicit zeros. Normal entries retain the existing
person/project/customer rate precedence. Externally invoiced time and expenses
are excluded from new invoice generation and permanently locked; they do not
need a local invoice association to remain protected.

Imported invoices have a reviewed opening paid amount separate from later
payment ledger records. Preview, download and delivery preserve the original
PDF bytes and verify their SHA-256. Missing or changed original PDFs fail
explicitly instead of being regenerated from the current template.

These fields cannot be supplied through ordinary entry/expense write payloads.
The 0.1.51 response-schema compatibility revision is documented in `api.md`.

## Invoice ZIP container reader

`harvest_invoices::inspect_invoice_zip(bytes)` reads ZIP members into memory;
it never extracts archive paths to disk. It rejects encrypted/nonregular
members, unsafe or duplicate paths, unsupported files and duplicate PDF bytes.
Limits are 5 MiB compressed, 1,000 archive entries, 500 PDFs, 8 MiB per PDF,
50 MiB actual expanded bytes and a maximum 200:1 compression ratio. Actual
decompressed sizes and CRC errors are checked, not only declared sizes.

This is container validation only: the PDF signature/end marker is checked but
does not prove a well-formed document. No invoice status, recipient, legal
invoice number or service period is inferred from an archive filename.

## Bounded PDF metadata suggestions

`harvest_invoices::inspect_invoice_text(&InvoiceSource)` parses the member's
bytes with lopdf and returns `PdfInspection`. It is strictly best-effort and
produces **suggestions only**:

- At most 50 pages, 1 MiB of decompressed page content per page and 256 KiB of
  collected text; the text budget is reported as `text_truncated` rather than
  silently overrun.
- Suggestions are drawn only from labelled lines in the document's own text
  (`Invoice Number:`, `Invoice Date:`, `Bill To:`, `Currency:`, `Total:`,
  `Amount Due:`). Amounts are integer minor units with correct grouping; dates
  must be unambiguous ISO `YYYY-MM-DD` values.
- Each suggestion carries the 1-based source page, the source line as
  `evidence`, and a confidence. Nothing is inferred from the filename, and the
  original PDF stays authoritative.
- Encrypted documents, unparseable structure, more than 50 pages, undecodable
  page content, scanned documents (no extractable text) and documents with no
  recognisable metadata all return `ManualReview` or `NoText` with a blocking
  reason. There is no OCR and no outbound/cloud call.

A suggestion never commits metadata. #400 still has to map the customer, review
the legal number, currency, dates and totals, reconcile lines and decide
exclusions before any record may be written (#401).

Run the ZIP and PDF tests with `cargo test --lib harvest_invoices::tests`. To
inspect a private sample without extraction, set `HARVEST_INVOICE_ZIP` and run
the ignored
`harvest_invoices::tests::private_invoice_archive_is_inspected_without_extraction`
test. Only counts are printed; real invoices must not become tracked fixtures.

## Implemented parser

`harvest::parse(ReportKind, bytes)` supports Harvest detailed time reports and
expense reports independently. It preserves source fields, including additional
columns, and returns normalized rows with logical record numbers and physical
line ranges. Quoted commas, escaped quotes, multiline notes, UTF-8 BOM and CRLF
are supported. Headers normalize case, surrounding whitespace and the optional
question mark on boolean column names.

Malformed CSV, invalid UTF-8, missing/duplicate headers and oversized input reject
the file. Limits are 1 MiB, 10,000 logical records, 64 columns and 80 bytes per
header. Row-level problems return structured issues without dropping or
truncating the source rows. A later preview must block confirmation while those
issues remain unresolved.

Hours use signed integer hundredths only in the parser's intermediate rows;
the persisted positive-only `Hours` model is unchanged. Amounts and rates use
integer minor units, never floats. Monetary grouping such as `"16,200.0"` is
accepted only with correctly placed separators. Unsupported precision, overflow,
invalid booleans and inconsistent exported time amounts are reported.

An equal-and-opposite time pair is identified only when the match is unique in
both directions and agrees on date, person/employee identity, customer, project,
task, currency, rates, amounts and billing status. Both rows remain in the
report. Duplicate candidates and unmatched negatives require explicit review;
the parser never applies corrections or writes records.

Expense approval and units remain source metadata. Foreign amounts or exchange
rates mentioned inside notes are never inferred. The project name does not
override a row's calendar year.

## Verification

Run the anonymized parser tests:

```sh
cargo test --lib harvest::tests
```

An opt-in ignored test can validate private exports locally. Set
`HARVEST_TIME_CSV` and `HARVEST_EXPENSE_CSV` to the two local file paths, then run:

```sh
cargo test --lib harvest::tests::private_exports_are_read_only_and_reconcile -- --ignored --nocapture
```

It reads the files without copying them or printing source row values; only
counts and field-level issues are reported. Never add real exports to Git or
upload them to issue trackers.

## Preview, API and commit

`domain::import_plan::plan` builds one deterministic preview from any
combination of the three sources. Source identity is
`provider:namespace:<canonical row hash>:<occurrence>` (invoice identity is the
exact PDF SHA-256), so overlapping exports collide and legitimate identical
rows keep distinct occurrences. Every reference is an explicit reviewed
mapping; unresolved references, ambiguous corrections, invoice
number/hash conflicts and missing financial/status fields are blocking issues.
The revision hash covers the parsed sources and the reviewed choices; commit
rejects a stale revision.

Admin-only routes live under `/api/admin/imports/harvest`:

- `POST /` creates a batch; `GET /` lists history; `GET /{id}` returns one batch.
- `PUT|GET /{id}/files/{time|expenses|invoices}` stages/downloads exactly one
  validated source (1 MiB CSV, 5 MiB ZIP).
- `PUT /{id}/review` replaces the reviewed mapping; `POST /{id}/preview`
  recomputes the plan; `GET /{id}/rows` paginates rows, decisions and issues.
- `POST /{id}/commit` requires the exact ready revision.
- `GET /{id}/invoices/{sha256}/pdf` returns the original archived member.
- `GET /settings/import` is the admin GUI workspace.

A commit re-parses the staged sources and rebuilds the preview **inside the
store writer lock**. It journals every path in `imports/.journal.<id>.json`
before the first write, uses deterministic ids (a retry returns the same
records), writes original PDFs byte-for-byte, and advances only the matching
`INV-`/known-prefix high-water mark. A failure removes exactly the journaled
paths; `Store::open` rolls back an interrupted `Prepared` journal before serving
traffic. `imports/` is admin-only and never exposed to member routes.

The Settings workspace builds labelled mapping tables from the preview's
original source values: people, customers, projects, tasks and expense
categories can be mapped to existing records or created explicitly, while each
invoice gets a review card with its legal number, customer, currency, dates,
period, total, opening paid amount and status. Original PDFs are linked directly
from the member, and an externally billed row can be explicitly linked to an
invoice PDF. One reconciliation table shows accepted/skipped totals per source
and currency. The advanced JSON view remains available for operators but is no
longer the primary control.

## Remaining delivery

1. End-to-end verification (#404): private-sample confirmation and any
   browser-only cases not covered by the synthetic three-source contract test
   (time + expenses + original invoice PDF, including backup/restore hash
   equality) or the import-specific jsdom suite.
2. Any documented follow-ups found there (for example a killable PDF worker if a
   real sample defeats the bounded parser).