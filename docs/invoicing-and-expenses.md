# Invoicing and expenses

This page describes the billing lifecycle: how time and expenses become an
invoice, what freezing an invoice means, how payment and reminders work, and how
expenses, claims, retainers, catalog items and templates fit in.

## Invoice lifecycle

An invoice moves through these statuses:

| Status | Meaning | Editable? |
| --- | --- | --- |
| `draft` | Generated or created manually, not yet issued | Yes; it locks nothing |
| `issued` | Issued and delivered; the archived document is the record | No; its entries and expenses are frozen |
| `partly_paid` | Some payments recorded, balance still open | No; frozen |
| `paid` | The ledger covers the total | No; frozen |
| `written_off` | The remaining balance was forgiven with a reason | No; frozen |

Any status can still be **corrected** through a revision or **deleted** outright
(see [Corrections and resending](#corrections-and-resending) and
[Frozen records](#frozen-records)).

### Generate a draft

Choose **New invoice** to open the shared draft workspace. Select the customer,
then enter manual invoice items or choose **Import tracked work** to
select a work period, projects and expenses. Import is optional; it creates a
tracked draft and opens that draft in the same editor for review. It does not
append work to an existing invoice. Unsaved manual changes must be discarded
explicitly before switching to import.

The workspace supports PO reference, invoice date, optional due date/terms,
subject and notes, as well as tax, discount, manual-line project links and manual
row ordering. Source-backed time/expense lines remain read-only and must be
retained exactly once. Currency cannot change once a draft contains tracked
work. Save is explicit; **Save & preview** saves first and then renders the PDF.
The Catalog and draft editor do not distinguish products from services. Legacy
type metadata is retained internally for compatibility; no type selection is
required and editing an existing item preserves its stored value.
Cancel asks before discarding changes, and leaving the page warns about unsaved
edits. Invoice numbers remain server-assigned at draft creation.

Customers can set an optional **Invoice prefix** in their customer form. `CUST1`,
`CUST2` and `MONKEY` produce independent sequences beginning `CUST1-001`,
`CUST2-001` and `MONKEY-001`. Prefixes are uppercased; trailing hyphens are
optional. Two customers with the same prefix share its sequence. Existing
drafts and issued invoices keep their numbers when the prefix changes; changing
back resumes that prefix's sequence. A blank prefix retains the original
`INV-0001` format and global sequence. Deleting a draft never recycles its number.
Allocation takes the higher of the durable counter and the highest number
actually on file, then refuses any exact collision, so a restored or hand-edited
counter can never reissue an existing invoice number.
Prefixes allow up to 20 letters/digits/internal hyphens and cannot be `INV`.

Subject and notes prefill from customer/template settings; blank explicit notes
suppress default notes. **Reset document defaults** restores those
fields and inherited terms, with a visible restored/unchanged/error result. The
catalog selector prefills a manual line from saved items; it is not the description
field, and is hidden when no saved items exist. Editor dropdowns use anchored
menus within the invoice dialog. Explicit due dates take precedence and print without
a potentially contradictory `(Net N)` suffix. All overrides are draft-only.

A tracked-work invoice is generated from a period's **billable, not-yet-invoiced**
entries and expenses:

- The builder excludes every entry and expense already on **any non-draft
  invoice**, not only on an open one. A paid or written-off invoice is never
  billed twice.
- Each time line snapshots the resolved rate at generation time, so a later rate
  change never rewrites the invoice.
- An invoice is single-currency. Selecting projects or expenses in different
  currencies is refused with a clear error instead of converting silently.
- The tracked-work wizard (`+ Invoice`) is the creation path: choose the
  customer, the period (defaulting to the first and last day of the current
  month), the projects with uninvoiced work and how time is grouped on the
  document (`single`, `project`, `task` or `line`). The review step renders the
  candidate PDF live beside the summary; `POST /api/invoices/preview/pdf`
  persists nothing. The draft is saved only after review.
- If the timesheet changes between saving a draft and issuing it, issue is
  refused with `409 draft_out_of_date`; regenerate the draft and issue again.

A manual invoice can also be created from fixed lines and catalog items. Manual
amounts are recomputed on the server from quantity × unit price with integer
math, and the document may apply a discount and VAT.

### Browse and preview

Open/All is on its own row, above the filters in table order: Number, Customer,
Time. There is no Clear all or Columns control; all table columns stay visible.
All time is the initial period; Month, Quarter and Year
have previous/next navigation, and Custom range exposes from/to dates. Period
filters use the invoice's document date, then its issue/creation date for older
records. Importing tracked work uses work dates instead.

Invoice previews use locally bundled PDF.js rather than a browser-specific PDF
plugin. Page and zoom controls, accessible page text, loading/errors, Retry and
an Open PDF fallback remain available. No invoice is uploaded to an external
viewer. Issued-download PDFs remain write-once archives; new rendering/layout
changes never overwrite those files.
New issued archives and customer email attachments have no preview watermark.
For older archives carrying the application DRAFT/SENT/PAID stamp, email delivery
uses an in-memory copy with only that known stamp removed; the archived file is
not modified. Successful SMTP acceptance closes the recipient popup; validation
or transport errors leave it open for correction.
If an embedded browser does not start its PDF worker promptly, the viewer uses
PDF.js's local main-thread fallback. Network and rendering stages have deadlines;
a stalled request reports a retryable error rather than loading indefinitely.
Invoice filter option menus are anchored to their filter buttons, including when
the row wraps on mobile; Escape dismisses a menu and returns focus to its filter.

### Issue

When no company name is configured, the fallback issuer name is `TucanoTime`.
An explicit company profile or configured organisation name takes precedence.

Issuing:

- retains the number allocated at draft creation under the writer lock,
  from `invoices/.seq.json` (default) or `invoices/.seq.prefixes.json` (custom);
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

In the GUI, a successful **Issue** opens the invoice email recipient dialog
pre-filled with the customer's on-file billing address, so the document can be
sent straight away. Sending is still an explicit choice: closing the dialog
leaves the invoice issued.

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
never drift from it. The only editable state is before issue; drafts lock
nothing.

**Every invoice is deletable.** `DELETE /api/invoices/{id}` (admin tier, `204`)
works on a draft, an issued, a settled, a written-off or an imported invoice —
the register offers *Delete* on every row. One writer-locked operation removes
the document, its archived PDF (an imported original included), its payment
ledger and its timeline sidecar; a superseded original's revision (or a deleted
revision's original) has the dangling link cleared, and `audit.log` records
`invoice_deleted`. Because the lock provider only sees invoices that exist,
deleting a non-draft invoice releases the entries and expenses it locked — the
deliberate escape hatch, which is why the GUI confirms first and names what will
be removed.

The GUI hides edit/delete actions on invoiced rows and shows a lock that links to
the invoice. The API answers `409` with a reason for an attempted change, and
`503 lock_unavailable` if the lock state cannot be verified. See
[Security](security.md#record-locks-and-invoice-permanence).

## Corrections and resending

Editing a sent (issued/partly paid/paid/written off) or imported invoice never
rewrites it: `POST /api/invoices/{id}/revise` creates a **correction revision**
with its own id and the same number. The original keeps its document and
archived PDF untouched, is marked `superseded_by`, stays visible in the register
and remains downloadable, but is excluded from every active aggregate (summary,
outstanding, reports, profitability, CSV and reminders). The revision carries
`supersedes`, owns its own PDF and is the document that is emailed again.

The edit form can set the revision's **status** manually, including **draft** so
a sent invoice can return for correction: a draft revision locks nothing, can be
edited in place, and is issued again from the invoice view (same number). Status
choices are validated against the recorded payments: a draft or issued revision
cannot keep payments, partly paid needs a payment between zero and the total,
paid either matches the total or relies on the legacy no-ledger rule, and no
status may carry payments above the corrected total.

A draft invoice is still edited in place with `PUT /api/invoices/{id}`; the
revision endpoint refuses it (`409`). Only the active revision can be revised
again; the superseded original cannot. A revision's subject is interpolated
exactly as issuing resolves it, so a raw `%customer_name%` placeholder from the
org template is never frozen onto a revision or its PDF.

## PDFs, email and reminders

- `GET /api/invoices/{id}/pdf` downloads the archived PDF; the preview endpoint
  renders the same document without an archive.
- The document layout is: the `INVOICE` title with the `Subject` row directly
  beneath it (one blank line, the value close to its label); the invoice
  metadata (`Invoice ID`, `PO number`, `Issue date`, `Due date`, `Period`) as a
  compact two-column grid **above** the identity blocks; a `From` block on the
  left (name, one line per address line, the org legal id under its configured
  label — `Tax ID` by default, with the value set close to the label) and an
  `Invoice for` block on the right (customer name, one line per address line,
  `Company number`), both starting at the same y and using the same column
  width; a `Total hours` line over the time lines; the
  `Item type | Description | Quantity | Unit price | Amount` table; the totals
  (`Subtotal`, plus `Payments`/`Amount due` once money was received, otherwise
  the bold `Total`) as the table's final rows, with labels right-aligned under
  Unit price and values right-aligned under Amount. The document embeds the
  shipped **Ubuntu** TrueType faces. A document with no subject and no label
  overrides keeps the historical `Invoice <number>` title and prints no Subject
  row.
- Time rows in the table follow the invoice's frozen `line_grouping`:
  `single` prints one `Time entries` row for the period, `project` one row per
  project, `task` one row per project and task, and `line` (the default and the
  legacy behaviour) one row per time entry. A grouped row prints the shared
  resolved rate (the project rate in the normal case) in **Unit price**; when
  its entries resolved different rates, no single price applies and the column
  prints `-`. Expenses and manual lines are never folded into a time row. The
  stored invoice lines remain one per tracked entry, so locks, rate snapshots
  and `draft_is_stale` are unaffected.
- Below the totals, the body footer prints in this order:
  1. the template **footer**;
  2. the **Notes** section — the template `notes`, then the customer's
     `invoice_notes`, under one `Notes` heading. The section is omitted entirely
     when both are empty (no empty heading);
  3. the generated **Payment details** block, heading renameable through the
     `payment_heading` label.
- Every page's footer carries the credit line `Generated by {org} on TucanoTime.`
  on the left and `Page N of M` on the right.
- The payment block is one canonical sequence on every surface: the flat rows
  and flat instructions first, then each currency rail's rows (starting with a
  `Currency` row) and instructions, separated by a dashed rule. The PDF, the
  invoice/reminder email ("How to pay") and the `%payment_details%` variable
  render exactly the same content; a customer with rails never loses the flat
  block. A multi-line value interpolated into a template renders one paragraph
  per line instead of collapsing into one wrapped line.
- Document money is `EUR €1,200.00` (code, symbol, grouped amount) and a payment
  is `EUR -€1,300.00`; emails keep their `1,200.00 EUR` wording. Dates on the
  document are `dd/mm/yyyy`, and an issued invoice's due date carries the terms
  suffix from its own snapshot (`(Net 30)`, `(Due on receipt)`).
- `POST /api/invoices/{id}/email` sends the invoice with the PDF attached to the
  customer. When the invoice includes expense lines whose expense has a stored
  receipt, the recipient dialog offers **Include expense receipts (N)**; ticking
  it attaches those files (at most 20 and 25 MB total) alongside the invoice PDF.
  The delivery's timeline entry (`email_sent`) records the receipt names and
  count, and **View message** shows them, so the history says exactly what left
  the building. `GET /api/invoices/{id}/receipts` returns the names only, for
  that dialog; the bytes never leave the server until an email is sent.
  `POST /api/invoices/{id}/email-copy` sends an ad-hoc copy to a third party
  such as an accountant; it is audited but is not a customer delivery.
- **The accountant copy is one click.** *Settings → Company* stores
  `OrgProfile.accountant_email`, and the register's **Email copy** action opens
  its dialog pre-filled with that address (editable, with a visible Cancel).
  Marking an invoice paid — the *Record payment* action or a status change in
  the editor — offers the same dialog, so the accountant gets the document at
  the moment it settles. With no address on file the paid offer points at the
  setting instead of opening an empty prompt.
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

Recurring invoice schedules and the retainer administration panel are hidden
in the browser GUI while those tools are being finished: the page shell sets
`data-advanced-billing="off"` on `<html>`, which removes the `#advanced-billing`
block from rendering, the accessibility tree and the tab order. Setting that
one attribute to `on` restores the panels. The REST API for both features
remains available and unchanged.

## Catalog and document templates

- **Products and services** are reusable manual invoice line types with a
  default price and currency. Archiving keeps history; it does not delete past
  lines.
- **Invoice templates** hold the org-wide subject, body, footer and notes with a
  closed set of `%variable%` placeholders and a deliberately small markdown
  subset (no tables, links, images or raw HTML). Unknown placeholders are
  rejected when the template is saved. Per-customer notes, subject overrides and
  payment terms refine the document.
- The template `notes` field (max 4000 characters) prints in the document's
  `Notes` section under the footer; the per-customer `invoice_notes` prints in
  the same section after it. `%variables%` and the markdown subset work there,
  exactly as in the footer. The field is optional and an install that never
  saved it keeps its existing bytes.
- `%payment_details%` interpolates the customer's canonical payment block. It is
  accepted in the body, footer and notes, but **not** in a subject: the value
  spans lines, and a subject is header-bound, so a subject that references it is
  refused with `422 validation_failed` when the template (or the customer's
  `invoice_subject`) is saved — an issue can never freeze a broken subject.
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
  `payments`, `amount_due`). `payment_heading` renames the generated payment
  block (default `Payment details`); it does not add text — use the template
  `notes` field for free text. The `Notes` section heading is not an override.
- The customer's company number (`Customer.legal_id`) and the prefix printed
  before the org's own id (`OrgProfile.legal_id_label`, e.g. `Tax ID` or `VAT`)
  feed the issuer/customer blocks.
- The invoice accent colour and the sender display name / Reply-To are honoured
  by the PDF and the real email transport. The accountant email is recipient
  data only: it never prints on a document and is validated as an email address
  before it is stored.

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

Last verified against TucanoTime 0.1.44.
