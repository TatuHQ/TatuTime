# API

The machine-readable contract is [`openapi.json`](../openapi.json). It is served
at `/openapi.json` and rendered as interactive documentation at `/docs`. Route
changes and contract changes land together; this page summarises the conventions
without duplicating the schema.

## Mount points

- The whole JSON API is mounted under `/api`.
- Only `/healthz`, `/openapi.json`, `/docs` and the browser GUI stay at the
  root.
- Requests outside the API and the GUI allowlist receive the normal JSON 404.

## GUI routes

Each GUI view owns a top-level path. The server allowlist is `SPA_ROUTES` in
[`src/lib.rs`](../src/lib.rs#L271); the client mirror is `TAB_ROUTES` in
[`web/js/route.js`](../web/js/route.js). Matching is case-insensitive and
ignores a trailing slash.

| Route | View |
| --- | --- |
| `/` | Entry point; redirects to the default view |
| `/timesheets` | Entry point; canonicalised to a concrete timesheet view |
| `/timesheets/day` | Day timesheet |
| `/timesheets/week` | Week grid |
| `/timesheets/calendar` | Month calendar |
| `/customers` | Customers |
| `/projects` | Projects |
| `/tasks` | Tasks |
| `/invoices` | Invoices |
| `/expenses` | Expenses |
| `/approvals` | Approvals |
| `/reports` | Reports |
| `/settings` | Entry point; canonicalised to a concrete settings view |
| `/settings/users` | Users |
| `/settings/company` | Company identity |
| `/settings/invoice-template` | Invoice template |
| `/settings/catalog` | Catalog |
| `/settings/expenses` | Expense categories |
| `/settings/email` | Email |
| `/settings/security` | Security and runtime configuration |

## Authentication tiers

Requests use the `tt_session` HttpOnly cookie. A request without a valid session
on a protected route gets `401 unauthenticated` before method routing.

| Tier | Routes |
| --- | --- |
| Open | `/healthz`, `/openapi.json`, `/docs`, and the pre-session auth endpoints (`bootstrap`, `login`, `logout`, `status`, `me`, SSO providers and assertion) |
| Member | Customers, projects, tasks, entries, reports, categories, expenses, FX, submissions, claims, timer, calendar events and notifications |
| Admin | Users, audit, retainers, profitability and budget reports, all invoice routes, accounting sync, invoice template and org profile, item types, config, email, vault and secrets, schedules and calendar OAuth |

A member may read and edit only their own entries and expenses; an unowned
record answers 404. Customers, projects, tasks and categories are shared
reference data readable by any signed-in user. The complete route-to-tier table
is `API_ROUTES` in [`src/lib.rs`](../src/lib.rs#L88).

## Import Provenance Revision 0.1.51

The closed `Entry`, `Expense` and `Invoice` response schemas accept optional,
read-only `imported` provenance. Existing stored documents and requests remain
valid; consumers validating responses against a closed schema must adopt this
revision before reading imported records. Ordinary create/update payloads cannot
set or remove provenance. Entry updates preserve it and cannot reassign imported
billing references. Externally invoiced time/expenses are permanently frozen
and excluded from invoice generation without requiring a local invoice link.

Imported time resolves its historical currency, billable rate and cost rate,
including explicit zeros. Original hours retain exported monetary snapshots;
allowed hours changes recompute amounts using the imported rates. Imported
invoices carry a reviewed opening paid amount separately from later payment
ledger events. Original imported PDFs are integrity-checked and never regenerated
or altered for preview or delivery. A missing/corrupt original fails closed.
The admin-only import workflow is exposed under `/api/admin/imports/harvest`
(create, raw source upload, review, preview, rows, exact-revision commit and
discard of a non-committed batch); see
[`harvest-import.md`](harvest-import.md).

## Contract Revision 0.1.48

### Customer Invoice Prefixes

Application 0.1.48 also extends the closed `Customer` and `CustomerInput`
schemas with optional `invoice_prefix`. Old stored customer documents and
requests remain valid. Older response validators must adopt this schema before
consuming customers with a configured prefix; empty prefixes remain omitted.
On create, omission/null uses default `INV-0001` numbering. On update,
omission/null preserves the current prefix; an empty string explicitly clears it.
Prefixes normalise to uppercase with surrounding spaces and trailing hyphens
removed. The result must be 1–20 ASCII letters, digits or internal hyphens,
beginning with a letter/digit; `INV` and control characters are rejected.
Raw inputs are limited to 64 characters.

New drafts use `CUST1-001`, `CUST1-002`, etc. Each prefix has a durable sequence;
two customers choosing the same prefix share it to avoid duplicate numbers.
Changing a prefix affects only future drafts and never renames an existing
draft or issued invoice. Clearing it resumes the original global sequence.
Deleted numbers are never reused. Manual, tracked-work and recurring creation
all use the same writer-locked allocator. The additive revision remains within
the existing pre-1.0 API rather than introducing a new route version.

### Invoice Documents

The existing invoice endpoints remain under `/api/invoices`. The closed
`ManualInvoiceInput` and `InvoiceEditInput` schemas now explicitly accept an
optional `document` object: `po_number`, `invoice_date`, `due_date`,
`payment_terms`, `subject` and `notes`. `InvoiceEditInput` also accepts currency
changes for wholly manual drafts. Omitted edit fields preserve stored values;
null nested overrides restore inheritance, while an empty notes string suppresses
default notes. Unknown nested fields, invalid dates/terms/template variables and
oversized text are rejected before writes. Tracked lines cannot be removed,
duplicated or changed. Non-draft updates remain 409.

This is an additive pre-1.0 contract revision shipped in application 0.1.48,
not a new URL version. Existing request bodies remain valid. Consumers that
validate responses against a closed schema must use the matching checked-in
OpenAPI revision before relying on the new optional `Invoice.document` field.

## CSRF header

Mutating requests (`POST`, `PUT`, `DELETE`) must carry:

```
X-CSRF-Protection: 1
```

The only exemptions are `/api/auth/login`, `/api/auth/bootstrap` and
`/api/auth/sso/assertion`, which run before a session exists. A missing header
answers `403` with `{"error":{"code":"csrf","message":"missing X-CSRF-Protection header"}}`.

## Status conventions

- **201 Created** when a `POST` mints a new, client-addressable resource:
  `POST /api/customers`, `/api/entries`, `/api/invoices` and `/api/claims`.
- **200 OK** when the request is a command or transition on an existing
  resource and returns its new state: `POST /api/invoices/{id}/issue`,
  `/api/invoices/{id}/pay`, `/api/timer/stop` and `/api/claims/{id}/submit`.
- **204 No Content** where the operation returns no body, for example setting a
  secret.
- `DELETE /api/invoices/{id}` deletes an invoice in **any** status and answers
  **204**; any other state transition the current status forbids — including
  revising an original that already has a correction — answers **409**.

## Invoice document fields

The invoice document model gained these fields; the schemas live in
[`openapi.json`](../openapi.json):

- `Invoice.subject` — the resolved subject, frozen at issue. Empty on drafts and
  legacy issued documents. The subject precedence is the frozen snapshot, then
  `Customer.invoice_subject`, then the template subject, then the legacy
  `Invoice {n} from {org}` wording.
- `Invoice.terms` — the `PaymentTerms` actually used to resolve `due_date` at
  issue (a snapshot, never a live lookup). Absent when the legacy
  net-14-from-period-to fallback applied.
- `Customer` / `CustomerInput.legal_id` — the customer's company/registration
  number (≤ 60 characters, no control characters), printed on documents.
- `OrgProfile.legal_id_label` — the display prefix for the org's `legal_id`
  (`Tax ID`, `VAT`, `Company number`; ≤ 40 characters).
- `LabelOverrides` gains display-only, ≤ 40-character keys for the reference
  document's wording: `from`, `invoice_for`, `invoice_id`, `issue_date`,
  `due_date`, `subject`, `payment_heading`, `item_type`, `payments`,
  `amount_due`. `payment_heading` renames the generated payment block (default
  `Payment details`).
- `InvoiceTemplate.notes` — the org-wide free text of the document's `Notes`
  section (≤ 4000 characters, markdown subset, `%variables%` supported; optional
  and omitted from the stored bytes when empty). It prints under the `Notes`
  heading after the template footer and before the customer's `invoice_notes`
  and the generated `Payment details` block.
- `PaymentDetails.rails` — up to four `PaymentRail` entries for multi-currency
  customers. `currency` (3 uppercase letters) and `account_name` are required;
  the remaining account fields are optional and shape-checked (`iban` 5-34
  alphanumerics, `bic`/`swift` 8 or 11). The flat fields remain authoritative
  for the single-rail case; nothing is migrated.

An install that has never saved `invoice_template.json` resolves to the seeded
defaults: `payment_terms = {"kind": "net_30"}` and the subject
`%customer_name% invoice for %period_month_padded% %period_year%`. The template
variable set now also includes `%period_month%`, `%period_month_number%`,
`%period_month_padded%` and `%period_year%`, derived from the invoice period.

## Correction revisions

`POST /api/invoices/{id}/revise` (admin tier) corrects a sent or imported
invoice without rewriting it. The body is `InvoiceEditInput` — now also
accepting an optional `status` — and answers **201** with the new revision:

- `Invoice.supersedes` / `Invoice.superseded_by` are nullable ids. A revision
  carries `supersedes = <original id>`; the original is left untouched and
  marked `superseded_by = <revision id>`. Both stay readable and downloadable;
  only the revision is counted by the summary, reports, CSV and reminders.
- The revision reuses the number, gets its own id and its own archived PDF
  (`409` for a draft revision, which has no PDF until it is issued). A revision
  whose `status` is `draft` locks nothing, is edited in place through the usual
  draft endpoint and is issued again with the same number.
- `409` for a draft invoice (edit it in place instead) and for an original that
  already has a correction; `422` when the status contradicts the recorded
  payments.

## Errors

Every failure uses one JSON shape:

```json
{
  "error": {
    "code": "validation_failed",
    "message": "payload failed validation",
    "fields": [{ "field": "hours", "message": "must be positive" }]
  }
}
```

`fields` appears only when the failure is tied to specific fields. Responses
never expose filesystem paths, stack traces or raw OS errors; those stay in the
server log. The error-code vocabulary is the `Error.code` enum in the contract.

Payloads are validated before any write (`deny_unknown_fields` plus domain
validation), so an invalid request changes nothing. The two transient refusals
carry a `Retry-After` header:

- `503 lock_unavailable` — the invoice, submission or claim lock state could not
  be verified; the write is refused fail-closed.
- `503 write_lock_busy` — another writer held the cross-process lock past the
  deadline.

## Pagination

`GET /api/entries` and `GET /api/expenses` use keyset pagination:

- `limit` defaults to `200` and must be between 1 and `1000`.
- `cursor` is the opaque token returned as `next_cursor` by the previous page;
  `null` means the set ended.
- A `limit` or `cursor` the service did not issue is refused with
  `422 validation_failed` naming the field; it is never silently clamped or
  restarted from page one.
- Entries walk `(date, id)` ascending; expenses walk it descending (newest
  first). A write between two requests can neither skip nor repeat a row.

## Range queries

Date-range queries are capped at **400 days, inclusive**. A wider request is
refused with `400 bad_request` rather than walking the tree. Reports and CSV
exports use the same cap.

## Content and transport

- JSON request bodies are capped by the transport; an oversized body answers
  `413 payload_too_large` without echoing the limit.
- `GET /api/reports/export.csv` and `/api/invoices/export.csv` return CSV with
  formula-injection protection.
- The OpenAPI document is the authority for request and response schemas,
  including the 201/200 distinction and the security requirements per route.

---

Last verified against TucanoTime 0.1.42.
