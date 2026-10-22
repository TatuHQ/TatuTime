//! Invoicing: draft/issue/pay lifecycle (#8, #27), email delivery (#35)
//! and accounting sync (#33). Getting paid is deliberate: the invoice shows
//! the customer's own payment details and settlement is recorded manually —
//! no hosted-checkout or payment-provider integration exists here.

mod catalog;
mod document;
mod email;
mod lifecycle;
mod manual;
mod org;
mod preview;
mod sync;

use crate::api::{
    ApiError, AppState, FieldError, Invoice, InvoiceError, default_active_true, generate_invoice,
};
use uuid::Uuid;

pub use catalog::{create_item_type, delete_item_type, list_item_types, update_item_type};
pub use document::{
    invoice_document, invoice_pdf, invoice_pdf_preview, invoice_template_get, invoice_template_put,
};
pub use email::{
    EmailCopyInput, InvoiceEmailInput, money_for_email, send_invoice_email, send_invoice_email_copy,
};
pub use lifecycle::{
    PayInput, WriteOffInput, create_invoice, delete_invoice, get_invoice_handler,
    invoice_export_csv, invoice_report_handler, invoice_summary, invoice_timeline, issue_invoice,
    list_invoices, mark_invoice_sent, pay_invoice, preview_invoice, preview_invoice_pdf,
    write_off_invoice,
};
pub use manual::{
    InvoiceEditInput, InvoiceLineInput, ManualInvoiceInput, create_manual_invoice, revise_invoice,
    update_invoice_draft,
};
pub use org::{org_get, org_put};
pub use preview::invoice_template_preview;
pub use sync::{SyncInput, sync_invoice, sync_status};

pub(crate) use document::{
    content_for, invoice_pdf_for_delivery, load_template, render_pdf_now, resolve_due_date,
    resolve_terms,
};
pub(crate) use lifecycle::get_invoice_or_404;
pub(crate) use org::{load_org, org_for};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceInput {
    pub customer_id: Uuid,
    pub from: String,
    pub to: String,
    /// Include billable expenses in the period (default true, #25).
    #[serde(default = "default_active_true")]
    pub include_expenses: bool,
    /// Restrict billing to these projects of the customer (#134); absent or
    /// empty = every project with eligible work in the period.
    #[serde(default)]
    pub project_codes: Vec<crate::domain::ProjectCode>,
    /// How the invoice document folds tracked time into its line table
    /// (#new). Presentation only; the stored lines stay one-per-entry. Absent
    /// keeps the historical "one row per entry" document.
    #[serde(default)]
    pub line_grouping: crate::domain::LineGrouping,
}

/// Shared selection pipeline for POST /invoices and /invoices/preview:
/// gather sources, honour project filtering (#134), build the draft.
#[allow(clippy::too_many_arguments)]
fn build_draft(
    app: &AppState,
    customer: &crate::domain::Customer,
    from: chrono::NaiveDate,
    to: chrono::NaiveDate,
    include_expenses: bool,
    project_codes: &[crate::domain::ProjectCode],
    line_grouping: crate::domain::LineGrouping,
) -> Result<Invoice, ApiError> {
    // #256: one directory walk of the customer's projects, reused for both the
    // unknown-code check and the selection filter (the staged wizard hit it
    // twice).
    let all_projects = app.store.list_projects(customer.id)?;
    if !project_codes.is_empty() {
        for code in project_codes {
            if !all_projects.iter().any(|p| &p.code == code) {
                return Err(ApiError::validation(vec![FieldError::new(
                    "project_codes",
                    format!("unknown project {} for this customer", code.0),
                )]));
            }
        }
    }
    let projects: Vec<_> = if project_codes.is_empty() {
        all_projects
    } else {
        all_projects
            .into_iter()
            .filter(|p| project_codes.contains(&p.code))
            .collect()
    };
    let users = app.store.list_users()?;
    let entries = app.store.list_range(from, to)?;
    let expenses = app.store.list_expenses()?;
    let invoices = app.store.list_invoices()?;
    // #248: the ONE shared "already invoiced" predicate. Every non-draft
    // invoice (issued, partly paid, paid, written off) contributes its lines as
    // billed work, so no builder can re-bill a settled period.
    let (excluded_entries, excluded_expenses) = crate::domain::already_invoiced(&invoices);
    let sources = crate::domain::InvoiceSources {
        projects: &projects,
        users: &users,
        entries: &entries,
        expenses: &expenses,
        excluded_entries: &excluded_entries,
        excluded_expenses: &excluded_expenses,
        include_expenses,
        restrict_projects: !project_codes.is_empty(),
    };
    generate_invoice(String::new(), customer, &sources, from, to, app.clock.now())
        .map(|mut draft| {
            draft.document.line_grouping = line_grouping;
            draft
        })
        .map_err(invoice_error)
}

fn invoice_error(e: InvoiceError) -> ApiError {
    match e {
        InvoiceError::NothingToInvoice => {
            ApiError::conflict("no billable, not-yet-invoiced work in this period")
        }
        InvoiceError::MixedCurrency { a, b } => ApiError::conflict(format!(
            "the period mixes currencies ({a} and {b}); an invoice is single-currency"
        )),
    }
}
