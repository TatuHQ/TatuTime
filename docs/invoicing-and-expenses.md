# Invoicing and expenses

This page describes the billing lifecycle: how time and expenses become an
invoice, what freezing an invoice means, how payment and reminders work, and how
expenses, claims, retainers, catalog items and templates fit in.

## Invoice lifecycle

An invoice moves through these statuses:

| Status | Meaning | Editable? |
| --- | --- | --- |
| `draft` | Generated or created manually, not yet issued | Yes; it locks nothing and can be deleted |
| `issued` | Issued and delivered; the archived document is the record | No; its entries and expenses are frozen |
| `partly_paid` | Some payments recorded, balance still open | No; frozen |
| `paid` | The ledger covers the total | No; frozen |
| `written_off` | The remaining balance was forgiven with a reason | No; frozen |

### Generate a draft

A tracked-work invoice is generated from a period's **billable, not-yet-invoiced**
entries and expenses:

- The builder excludes every entry and expense already on **any non-draft
  invoice**, not only on an open one. A paid or written-off invoice is never
  billed twice.
- Each time line snapshots the resolved rate at generation time, so a later rate
  change never rewrites the invoice.
- An invoice is single-currency. Selecting projects or expenses in different
  currencies is refused with a clear error instead of converting silently.
- The tracked-work wizard lets you narrow generation to specific projects and
  shows the uninvoiced hours per project. A preview endpoint renders the draft
  without saving it; the draft is saved only after review.
- If the timesheet changes between saving a draft and issuing it, issue is
  refused with `409 draft_out_of_date`; regenerate the draft and issue again.

A manual invoice can also be created from fixed lines and catalog items. Manual
amounts are recomputed on the server from quantity × unit price with integer
math, and the document may apply a discount and VAT.

### Issue

Issuing:

- assigns the next sequential number (`INV-0001`, `INV-0002`, …) from the
  number ledger under the writer lock;
- sets `issued_at`, calculates `due_date` from the customer's payment terms
  (falling back to the org template default, then the legacy
  net-14-from-period-to rule), and freezes the status;
- freezes the resolved **subject** and the **payment terms actually used**
  (`Invoice.subject` / `Invoice.terms`), so a document prints `(Net 30)` from
  its own snapshot and can never disagree with `due_date`. Later template or
  customer edits do not rewrite an issued invoice; drafts keep resolving live.
  Legacy issued documents have neither field and render without a Subject row
  and without a `(Net N)` suffix;
- archives the rendered PDF at `invoices/<id>.pdf`, which is never rewritten;
- makes the invoice immutable and freezes every entry and expense it references.

A fresh install resolves an absent `invoice_template.json` to seeded defaults:
the subject `%customer_name% invoice for %period_month_padded% %period_year%`
(e.g. `Tucano Time invoice for 08 2026`) and `net_30` payment terms.
Saving a template explicitly replaces those defaults; a saved template with
`payment_terms: null` still uses the legacy net-14 fallback.

### Pay and write off

- `POST /api/invoices/{id}/pay` records a payment in the invoice's ledger. A
  partial amount moves the invoice to `partly_paid`; an amount that covers the
  balance moves it to `paid`. The balance is always derived from the ledger plus
  the total, never stored separately.
- `POST /api/invoices/{id}/write-off` requires a reason and forgives the
  remaining balance. A written-off invoice is final and excluded from
  outstanding totals.
- Settlement is **manual**. TucanoTime has no hosted checkout, card processor or
  payment-provider integration. The invoice tells the customer how to pay using
  the customer's own payment details (bank name, account name/number, IBAN, BIC,
  reference and free-text instructions). Those details appear in the **Get paid**
  panel, the invoice and reminder emails and the PDF, and are available as the
  `%payment_details%` template variable.
- Multi-currency customers may add up to four **structured payment rails**
  (`PaymentDetails.rails`), each with a 3-letter ISO currency, a required
  account holder name and that currency's account details. The flat fields stay
  authoritative for the single-rail case; rails are additive display data and
  are validated before anything is written — a blank `account_name`, currency or
  malformed IBAN/BIC is a `422` and persists nothing.
- Every state transition is recorded in the invoice timeline and the audit log.

## Frozen records

An issued invoice freezes its entries and expenses permanently. Settling
(`paid`/`partly_paid`) or writing off (`written_off`) does **not** release them:
the archived invoice is the evidence of what was billed and the timesheet must
never drift from it. The only editable state is before issue. Drafts lock
nothing and can be deleted.

The GUI hides edit/delete actions on invoiced rows and shows a lock that links to
the invoice. The API answers `409` with a reason for an attempted change, and
`503 lock_unavailable` if the lock state cannot be verified. See
[Security](security.md#record-locks-and-invoice-permanence).

## PDFs, email and reminders

- `GET /api/invoices/{id}/pdf` downloads the archived PDF; the preview endpoint
  renders the same document without an archive.
- The document layout is: the `INVOICE` title with the `Subject` row and the
  invoice metadata (`Invoice ID`, `Issue date`, `Due date`, `Period`) beside it;
  a `From` block (name, one line per address line, the org legal id under its
  configured label); an `Invoice for` block (customer name, one line per address
  line, `Company number`); a `Total hours` line over the time lines; the
  `Item type | Description | Quantity | Unit price | Amount` table; the totals
  (`Subtotal`, plus `Payments`/`Amount due` once money was received, otherwise the
  bold `Total`); the notes/payment section (the flat account, or one block per
  currency rail with a dashed rule between rails); and `Page N of M` on every
  page. A document with no subject and no label overrides keeps the historical
  `Invoice <number>` title and prints no Subject row.
- Document money is `EUR €1,200.00` (code, symbol, grouped amount) and a payment
  is `EUR -€1,300.00`; emails keep their `1,200.00 EUR` wording. Dates on the
  document are `dd/mm/yyyy`, and an issued invoice's due date carries the terms
  suffix from its own snapshot (`(Net 30)`, `(Due on receipt)`).
- `POST /api/invoices/{id}/email` sends the invoice with the PDF attached to the
  customer. `POST /api/invoices/{id}/email-copy` sends an ad-hoc copy to a third
  party such as an accountant; it is audited but is not a customer delivery.
- `POST /api/invoices/{id}/sent` records the manual "mark as sent" transition
  without sending anything.
- A daily scheduler job emails reminders for issued, unpaid invoices past their
  due date. `reminder_days` (`TUCANO_REMINDER_DAYS`, default 7) sets the minimum
  interval between reminders for the same invoice; the last-reminded date is
  persisted so a restart does not re-spam.
- If no SMTP host is configured, mail is logged and the transport reports
  `disabled`; invoice and reminder flows still work.

## Invoice overview and reports

The Invoices view shows open/paid tiles, a year-navigable monthly Open-vs-Paid
chart, searchable Open/All lists and sortable columns. Selecting a row opens the
document preview. When a base currency is configured, mixed-currency trend
values are converted once per currency group and the rate is shown; without a
base currency the chart says it is showing a single currency rather than
inventing a rate.

Summary, report and CSV export endpoints expose the same figures to
administrators.

## Expenses

- Expenses are recorded against a customer/project with a category, a billable
  flag, a date, an amount and an optional receipt.
- An expense may be entered in a different currency from the one the
  customer/project bills in. The dialog converts it at a rate you can edit, and
  the document keeps the original amount/currency and the rate as well as the
  billed value.
- Billable expenses roll into the next invoice for their period.
- Expenses on an issued invoice are frozen; deleting one answers `409`.
- Expense categories are shared reference data managed under
  **Settings → Expenses**.

## Reimbursement claims

A member groups expenses into a reimbursement claim and submits it. A submitted
or approved claim locks its expenses; rejecting a submitted claim releases them.
An administrator records the decision. A claim decides reimbursement to the
member; whether an expense is billable to a customer remains the expense's own
`billable` flag.

## Retainers

A retainer is an append-only ledger per customer/project:

- opening balance;
- additional funds;
- draws with a reason;
- closing.

The balance is always reconciled from the transactions, never stored as a
mutable figure. Retainer administration is an admin function.

## Catalog and document templates

- **Products and services** are reusable manual invoice line types with a
  default price and currency. Archiving keeps history; it does not delete past
  lines.
- **Invoice templates** hold the org-wide subject, body and footer with a closed
  set of `%variable%` placeholders and a deliberately small markdown subset
  (no tables, links, images or raw HTML). Unknown placeholders are rejected when
  the template is saved. Per-customer notes, subject overrides and payment terms
  refine the document.
- The billing-period variables are `%period_month%` (`October`),
  `%period_month_number%` (`10`), `%period_month_padded%` (`10`) and
  `%period_year%` (`2026`), derived from the invoice period (not the issue date)
  with integer arithmetic. The seeded subject is
  `%customer_name% invoice for %period_month_padded% %period_year%`.
- Subject precedence is: the issued invoice's frozen `subject`, then the
  customer's `invoice_subject`, then the template `subject`, then the legacy
  `Invoice {number} from {org}` wording. Drafts follow live template/customer
  edits; issued invoices never change their subject.
- **Field labels** rename the wording on the invoice document without changing
  API field names. Besides the line/total labels, the mechanism covers the
  reference document's headings and rows (`from`, `invoice_for`, `invoice_id`,
  `issue_date`, `due_date`, `subject`, `payment_heading`, `item_type`,
  `payments`, `amount_due`).
- The customer's company number (`Customer.legal_id`) and the prefix printed
  before the org's own id (`OrgProfile.legal_id_label`, e.g. `Tax ID` or `VAT`)
  feed the issuer/customer blocks.
- The invoice accent colour and the sender display name / Reply-To are honoured
  by the PDF and the real email transport.

## Accounting sync

Invoices and payments can be copied to QuickBooks Online or Xero through an
adapter. A provider is enabled when its token exists (vault `qbo.token` /
`xero.token` or `TUCANO_QBO_TOKEN` / `TUCANO_XERO_TOKEN`). Sync is:

- **idempotent** — keyed by provider and invoice with a deterministic remote id;
- **retryable** — failures are recorded and a daily job re-pushes them;
- **non-blocking** — a failed sync never fails the invoice flow.

Status is visible through `GET /api/sync/accounting`. Base URLs can be
overridden in configuration for sandbox or self-hosted endpoints.

---

Last verified against TucanoTime 0.1.41.
