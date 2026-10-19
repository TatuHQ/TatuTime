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
| `/settings/catalog` | Products and services |
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
- A delete of a non-draft invoice, or any other state transition the current
  status forbids, answers **409**.

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

Last verified against TucanoTime 0.1.41.
