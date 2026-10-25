# Getting started

TatuTime records time against a customer and a project code, then turns that
time into invoices. Each customer and each project carries its own currency and
hourly rate. One Rust binary serves the REST API, the browser GUI and the API
documentation; all data is stored as JSON documents in one data directory, so
there is no database to install or run.

## What you get

- **Timesheets** — a day view, a Monday-to-Sunday week grid and a month calendar.
- **Customers, projects and tasks** — a three-level hierarchy. A customer holds
  the default currency and rate; a project always has its own currency and rate
  and is prefilled from the customer; tasks are an optional third level.
- **Exact money and time** — amounts are stored in minor units (cents) and hours
  in hundredths, never as floating-point numbers.
- **Reports** — totals grouped by customer, project or ISO week, with a
  formula-injection-safe CSV export.
- **Invoices** — generate a draft from a period's billable work, review it, issue
  it, archive its PDF, email it and record full or partial payments. Issued
  invoices permanently freeze the time they bill.
- **Expenses, claims and retainers** — project expenses with categories and
  optional receipts, reimbursement claims with approval, and per-customer
  retainer ledgers.
- **Catalog and templates** — reusable product/service lines and an org-wide
  invoice template with `%variable%` placeholders.
- **Also available** — a running timer, imported calendar events (ICS or Google
  OAuth), local and SSO sign-in, notifications, recurring invoice schedules,
  budgets and profitability reports, and an operator backup/restore command.

For the invoicing rules in detail, see
[Invoicing and expenses](invoicing-and-expenses.md).

## First run

No accounts exist in a fresh installation.

1. Start the server (see the [README](../README.md) for local and container
   commands) and open <http://localhost:8080/>.
2. The **admin startup wizard** opens with five steps: welcome, administrator,
   organisation, defaults and review. The administrator step creates the first
   `admin` account; the organisation step records the company identity; the
   defaults step writes the optional base currency used by mixed-currency
   invoice charts.
3. The wizard then enters the application. A separate **first customer/project
   wizard** opens to create a customer, then a project, and leaves the entry form
   open with both selected.
4. After the first account exists, every data route requires a session. The
   bootstrap endpoint refuses to create a second administrator.

## Roles

- **Admin** — manages users and owns invoices, expenses approval, claims
  decisions, retainers, catalog, templates and configuration.
- **Member** — records their own time and expenses, sees only their own records
  and can submit a week for approval.

Customer, project, task and category definitions are shared organisation-wide
reference data; any signed-in user can read them.

## GUI map

| Purpose | URL |
| --- | --- |
| Day timesheet | `/timesheets/day` |
| Week grid | `/timesheets/week` |
| Month calendar | `/timesheets/calendar` |
| Customers | `/customers` |
| Projects | `/projects` |
| Tasks | `/tasks` |
| Invoices | `/invoices` |
| Expenses | `/expenses` |
| Approvals | `/approvals` |
| Reports | `/reports` |
| Users | `/settings/users` |
| Company identity | `/settings/company` |
| Invoice template | `/settings/invoice-template` |
| Products and services | `/settings/catalog` |
| Expense categories | `/settings/expenses` |
| Email | `/settings/email` |
| Security and runtime configuration | `/settings/security` |

`/`, `/timesheets` and `/settings` are entry points that redirect to a concrete
view. Paths are matched case-insensitively and ignore a trailing slash. The
authoritative list is `SPA_ROUTES` in
[`src/lib.rs`](../src/lib.rs#L271); the machine-readable API contract is
[`openapi.json`](../openapi.json).

## Your first timesheet

1. Open **Customers** and create a customer. Enter its default currency and
   default hourly rate.
2. Open **Projects** and create a project under that customer. Give it a code, a
   name and its own currency and rate (prefilled from the customer). Add a task
   if your work is broken down that finely.
3. Open **Timesheets → Day**. Pick the date, choose the customer and project,
   enter the hours and an optional description, and save. Repeat for more lines,
   or use the week grid to fill several days.
4. Open **Reports** to check the totals by customer, project or week. Use the CSV
   export when you need the numbers in a spreadsheet.

## Your first invoice

You must be an administrator to create invoices.

1. Open **Invoices** and start a tracked-work invoice. Choose the customer, the
   period and the projects; the wizard shows the uninvoiced hours per project and
   previews the draft before anything is saved.
2. Save the draft, review the lines and the total, then **issue** it. Issuing
   assigns the next sequential number, freezes the billed lines at the rate
   resolved at generation time and archives a PDF.
3. Download or email the PDF, then record a full or partial payment when the
   money arrives. Alternatively, write off the outstanding balance with a
   reason.
4. Invoice, submission and claim locks make the billed records permanent. The
   timesheet shows a lock on invoiced rows instead of edit and delete actions.

The full lifecycle, payment details, reminders, templates and expense handling
are described in [Invoicing and expenses](invoicing-and-expenses.md).

---

Last verified against TatuTime 0.1.41.
