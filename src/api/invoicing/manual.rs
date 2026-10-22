use super::{content_for, get_invoice_or_404, lifecycle::record_timeline, load_template};
use crate::api::*;
use crate::domain::InvoiceTimelineEventKind;

// ------------------------------------------------- manual lines & edit ----

/// Body for `POST /invoices/manual` (#143).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualInvoiceInput {
    #[serde(default)]
    pub document: crate::domain::InvoiceDocument,
    pub customer_id: Uuid,
    /// Absent = the customer's currency.
    #[serde(default)]
    pub currency: Option<Currency>,
    #[serde(default)]
    pub tax_hundredths: u16,
    #[serde(default)]
    pub discount_hundredths: u16,
    pub lines: Vec<crate::domain::ManualLineInput>,
}

/// A draft line as submitted (#187). Mirrors `domain::InvoiceLine` but is an
/// INPUT type with `deny_unknown_fields`: the shared validation invariant
/// ("every payload rejected before any write") could not apply to the stored
/// type without also making every legacy invoice document strict at read
/// time (a future added-then-removed field would brick `list_invoices`, and
/// via #185's fail-closed locks, entry editing). Conversion is field-by-field;
/// `amount_minor` is always recomputed server-side for manual lines.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceLineInput {
    pub kind: crate::domain::LineKind,
    pub date: chrono::NaiveDate,
    #[serde(default)]
    pub entry_id: Option<Uuid>,
    #[serde(default)]
    pub expense_id: Option<Uuid>,
    #[serde(default)]
    pub project_code: Option<crate::domain::ProjectCode>,
    #[serde(default)]
    pub task_code: Option<crate::domain::ProjectCode>,
    #[serde(default)]
    pub hours: Option<crate::domain::Hours>,
    #[serde(default)]
    pub rate_minor: Option<u64>,
    #[serde(default)]
    pub amount_minor: u64,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub quantity_hundredths: Option<u32>,
    #[serde(default)]
    pub unit_price_minor: Option<u64>,
    #[serde(default)]
    pub item_kind: Option<crate::domain::LineItemKind>,
}

impl From<&InvoiceLineInput> for crate::domain::InvoiceLine {
    fn from(l: &InvoiceLineInput) -> Self {
        crate::domain::InvoiceLine {
            kind: l.kind,
            date: l.date,
            entry_id: l.entry_id,
            expense_id: l.expense_id,
            project_code: l.project_code.clone(),
            task_code: l.task_code.clone(),
            hours: l.hours,
            rate_minor: l.rate_minor,
            amount_minor: l.amount_minor,
            note: l.note.clone(),
            quantity_hundredths: l.quantity_hundredths,
            unit_price_minor: l.unit_price_minor,
            item_kind: l.item_kind,
        }
    }
}

/// Body for `PUT /invoices/{id}` (draft edit) and
/// `POST /invoices/{id}/revise` (correction revision). Tracked lines must come
/// back exactly as stored (identity and amounts are enforced server-side);
/// manual lines are free. `status` is only meaningful on a revision — a draft
/// is issued from the invoice view, not by editing it.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceEditInput {
    #[serde(default)]
    pub document: Option<crate::domain::InvoiceDocument>,
    #[serde(default)]
    pub currency: Option<Currency>,
    pub lines: Vec<InvoiceLineInput>,
    #[serde(default)]
    pub tax_hundredths: u16,
    #[serde(default)]
    pub discount_hundredths: u16,
    /// Reviewed settlement status of the correction revision. Absent keeps the
    /// original's status. `draft` returns the corrected document to a draft
    /// (it can then be changed and issued again).
    #[serde(default)]
    pub status: Option<crate::domain::InvoiceStatus>,
}

fn percent_field(value: u16, field: &str, errors: &mut Vec<FieldError>) {
    if value > 10_000 {
        errors.push(FieldError::new(field, "at most 100.00 percent"));
    }
}

fn validate_document(
    document: &crate::domain::InvoiceDocument,
    today: chrono::NaiveDate,
    errors: &mut Vec<FieldError>,
) {
    for (field, value, max) in [
        ("document.po_number", document.po_number.as_str(), 120),
        (
            "document.subject",
            document.subject.as_deref().unwrap_or(""),
            120,
        ),
        (
            "document.notes",
            document.notes.as_deref().unwrap_or(""),
            4000,
        ),
    ] {
        if value.chars().count() > max || value.contains('\0') {
            errors.push(FieldError::new(
                field,
                format!("at most {max} characters, without null bytes"),
            ));
        }
    }
    for (field, text, max) in [
        (
            "document.subject",
            document.subject.as_deref().unwrap_or(""),
            120,
        ),
        (
            "document.notes",
            document.notes.as_deref().unwrap_or(""),
            4000,
        ),
    ] {
        crate::template::validate_field(field, text, max, errors);
    }
    if let Some(subject) = &document.subject
        && (subject.contains(['\r', '\n'])
            || !crate::template::multiline_tokens(subject).is_empty())
    {
        errors.push(FieldError::new(
            "document.subject",
            "must be a single line without multiline variables",
        ));
    }
    if let Some(terms) = &document.payment_terms {
        crate::domain::validate_payment_terms(Some(terms), errors);
    }
    if let Some(due) = document.due_date
        && due < document.invoice_date.unwrap_or(today)
    {
        errors.push(FieldError::new(
            "document.due_date",
            "must not precede invoice date",
        ));
    }
}

/// Create a draft invoice from manual Product/Service lines (#143). Money is
/// computed server-side with integer math; the draft locks nothing (#8).
pub async fn create_manual_invoice(
    State(app): State<AppState>,
    actor: AuthUser,
    ValidJson(input): ValidJson<ManualInvoiceInput>,
) -> ApiResult {
    let customer = get_customer(&app.store, input.customer_id)?;
    let mut errors = Vec::new();
    validate_document(&input.document, app.clock.today(), &mut errors);
    if input.lines.is_empty() {
        errors.push(FieldError::new("lines", "at least one line is required"));
    }
    percent_field(input.tax_hundredths, "tax_hundredths", &mut errors);
    percent_field(
        input.discount_hundredths,
        "discount_hundredths",
        &mut errors,
    );
    // Thread the clock's date in: the domain is clock-invariant (#261).
    let today = app.clock.today();
    let lines = crate::domain::validate_manual_lines(&input.lines, today, &mut errors);
    validate_line_projects(&app, &customer.id, &lines, &mut errors)?;
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let subtotal = lines.iter().map(|l| l.amount_minor).sum();
    let totals =
        crate::domain::invoice_totals(subtotal, input.tax_hundredths, input.discount_hundredths);
    let draft = Invoice {
        supersedes: None,
        superseded_by: None,
        imported: None,
        document: input.document,
        id: Uuid::new_v4(),
        number: String::new(),
        customer_id: customer.id,
        currency: input
            .currency
            .clone()
            .unwrap_or_else(|| customer.currency.clone()),
        period_from: today,
        period_to: today,
        lines,
        total_minor: totals.total_minor,
        status: crate::domain::InvoiceStatus::Draft,
        created_at: app.clock.now(),
        issued_at: None,
        due_date: None,
        paid_at: None,
        payment_reference: String::new(),
        pdf: None,
        payments: vec![],
        write_off_reason: String::new(),
        written_off_at: None,
        tax_hundredths: input.tax_hundredths,
        discount_hundredths: input.discount_hundredths,
        subject: String::new(),
        terms: None,
    };
    let created = app.store.create_invoice(draft)?;
    record_timeline(
        &app,
        created.id,
        InvoiceTimelineEventKind::Created,
        "Invoice created.".into(),
        actor.0.name.clone(),
        None,
    )
    .await;
    Ok((StatusCode::CREATED, Json(created)).into_response())
}

fn validate_line_projects(
    app: &AppState,
    customer_id: &Uuid,
    lines: &[crate::domain::InvoiceLine],
    errors: &mut Vec<FieldError>,
) -> Result<(), ApiError> {
    // #187: one store read per call (was one per line), and a store failure
    // surfaces as its own error instead of an `.unwrap_or(false)` that turned
    // an IO fault into a bogus "unknown project" 422.
    if lines.iter().all(|l| l.project_code.is_none()) {
        return Ok(());
    }
    let projects = app.store.list_projects(*customer_id)?;
    for (i, l) in lines.iter().enumerate() {
        if let Some(code) = &l.project_code
            && !projects.iter().any(|p| &p.code == code)
        {
            errors.push(FieldError::new(
                format!("lines[{i}].project_code"),
                "unknown project for this customer",
            ));
        }
    }
    Ok(())
}

struct EditedInvoice {
    document: crate::domain::InvoiceDocument,
    currency: Currency,
    lines: Vec<crate::domain::InvoiceLine>,
    tax_hundredths: u16,
    discount_hundredths: u16,
    total_minor: u64,
}

/// Validate and assemble an edit payload against the stored invoice. Tracked
/// lines must come back exactly as snapshotted; manual lines are re-priced;
/// legacy/imported fixed lines that carry only an amount keep it verbatim.
fn assemble_edit(
    app: &AppState,
    invoice: &Invoice,
    edit: &InvoiceEditInput,
) -> Result<EditedInvoice, ApiError> {
    let mut errors = Vec::new();
    let document = edit
        .document
        .clone()
        .unwrap_or_else(|| invoice.document.clone());
    validate_document(&document, app.clock.today(), &mut errors);
    if edit
        .currency
        .as_ref()
        .is_some_and(|currency| currency != &invoice.currency)
        && invoice
            .lines
            .iter()
            .any(|line| line.entry_id.is_some() || line.expense_id.is_some())
    {
        errors.push(FieldError::new(
            "currency",
            "cannot change the currency of tracked work",
        ));
    }
    if edit.lines.is_empty() {
        errors.push(FieldError::new("lines", "at least one line is required"));
    }
    if edit.lines.len() > crate::domain::MANUAL_LINE_MAX {
        errors.push(FieldError::new(
            "lines",
            format!("at most {} lines", crate::domain::MANUAL_LINE_MAX),
        ));
    }
    percent_field(edit.tax_hundredths, "tax_hundredths", &mut errors);
    percent_field(edit.discount_hundredths, "discount_hundredths", &mut errors);
    let stored_tracked: Vec<&crate::domain::InvoiceLine> = invoice
        .lines
        .iter()
        .filter(|l| l.entry_id.is_some() || l.expense_id.is_some())
        .collect();
    let mut out_lines: Vec<crate::domain::InvoiceLine> = Vec::new();
    for (i, l) in edit.lines.iter().enumerate() {
        let is_tracked = l.entry_id.is_some() || l.expense_id.is_some();
        if is_tracked {
            // Tracked lines survive exactly as snapshotted (the rate-snapshot
            // guarantee of #8): identity + numbers must match the stored one.
            let Some(orig) = stored_tracked
                .iter()
                .find(|o| o.entry_id == l.entry_id && o.expense_id == l.expense_id)
            else {
                errors.push(FieldError::new(
                    format!("lines[{i}]"),
                    "tracked lines must come from this invoice",
                ));
                continue;
            };
            let candidate: crate::domain::InvoiceLine = l.into();
            if &candidate != *orig {
                errors.push(FieldError::new(
                    format!("lines[{i}]"),
                    "tracked lines are immutable (rate snapshot)",
                ));
                continue;
            }
            out_lines.push((*orig).clone());
            continue;
        }
        // Legacy and imported fixed lines carry only an amount: preserve it
        // verbatim rather than demanding the manual quantity x price shape.
        let bare_amount = l.item_kind.is_none()
            && l.quantity_hundredths.is_none()
            && l.unit_price_minor.is_none();
        if bare_amount {
            if l.kind != crate::domain::LineKind::Fixed {
                errors.push(FieldError::new(
                    format!("lines[{i}].kind"),
                    "a line without an entry or expense must be Fixed",
                ));
                continue;
            }
            let mut line: crate::domain::InvoiceLine = l.into();
            line.amount_minor = l.amount_minor;
            line.note = l.note.trim().to_string();
            line.item_kind = Some(crate::domain::LineItemKind::Service);
            line.quantity_hundredths = Some(100);
            line.unit_price_minor = Some(l.amount_minor);
            out_lines.push(line);
            continue;
        }
        // Manual line: re-priced server-side. Uses the SAME bounds as
        // POST /invoices/manual (#187), so the two never diverge.
        let qty = l.quantity_hundredths.unwrap_or(0);
        let price = l.unit_price_minor.unwrap_or(0);
        if l.kind != crate::domain::LineKind::Fixed || l.item_kind.is_none() {
            errors.push(FieldError::new(
                format!("lines[{i}].kind"),
                "manual lines are Fixed with an item kind",
            ));
            continue;
        }
        crate::domain::check_manual_line(
            &l.note,
            qty,
            price,
            &format!("lines[{i}].note"),
            &format!("lines[{i}].quantity_hundredths"),
            &format!("lines[{i}].unit_price_minor"),
            &mut errors,
        );
        let mut line: crate::domain::InvoiceLine = l.into();
        line.amount_minor = crate::domain::manual_amount_minor(qty, price);
        line.note = l.note.trim().to_string();
        out_lines.push(line);
    }
    for original in &stored_tracked {
        if out_lines
            .iter()
            .filter(|line| {
                line.entry_id == original.entry_id && line.expense_id == original.expense_id
            })
            .count()
            != 1
        {
            errors.push(FieldError::new(
                "lines",
                "every tracked line must be retained exactly once",
            ));
        }
    }
    validate_line_projects(app, &invoice.customer_id, &out_lines, &mut errors)?;
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let subtotal = out_lines.iter().map(|l| l.amount_minor).sum();
    let totals =
        crate::domain::invoice_totals(subtotal, edit.tax_hundredths, edit.discount_hundredths);
    Ok(EditedInvoice {
        document,
        currency: edit
            .currency
            .clone()
            .unwrap_or_else(|| invoice.currency.clone()),
        lines: out_lines,
        tax_hundredths: edit.tax_hundredths,
        discount_hundredths: edit.discount_hundredths,
        total_minor: totals.total_minor,
    })
}

/// Status consistency for a correction revision. A draft carries no payments;
/// issued carries none either (a payment makes it partly paid); partly paid
/// needs a payment between zero and the total; paid may rely on the legacy
/// no-ledger rule or match the total exactly.
fn validate_status(
    status: crate::domain::InvoiceStatus,
    total_minor: u64,
    payments: &[crate::domain::InvoicePayment],
    errors: &mut Vec<FieldError>,
) {
    let paid: u64 = payments.iter().map(|payment| payment.amount_minor).sum();
    if paid > total_minor {
        errors.push(FieldError::new(
            "lines",
            "recorded payments exceed the corrected total",
        ));
    }
    match status {
        crate::domain::InvoiceStatus::Draft => {
            if !payments.is_empty() {
                errors.push(FieldError::new(
                    "status",
                    "a draft cannot keep recorded payments",
                ));
            }
        }
        crate::domain::InvoiceStatus::Issued => {
            if !payments.is_empty() {
                errors.push(FieldError::new(
                    "status",
                    "an issued invoice cannot carry payments; use partly paid",
                ));
            }
        }
        crate::domain::InvoiceStatus::PartlyPaid => {
            if paid == 0 || paid >= total_minor {
                errors.push(FieldError::new(
                    "status",
                    "partly paid needs a payment above zero and below the total",
                ));
            }
        }
        crate::domain::InvoiceStatus::Paid => {
            if !payments.is_empty() && paid != total_minor {
                errors.push(FieldError::new(
                    "status",
                    "paid needs recorded payments equal to the total",
                ));
            }
        }
        crate::domain::InvoiceStatus::WrittenOff => {}
    }
}

/// Edit a draft invoice (#143): replace the line set and percentages under a
/// Draft-only guard. Tracked lines must be preserved verbatim; manual lines
/// are re-priced from their integer quantity x unit price. 409 for anything
/// that is not a draft; validation failures persist nothing.
pub async fn update_invoice_draft(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
    ValidJson(edit): ValidJson<InvoiceEditInput>,
) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    if invoice.status != crate::domain::InvoiceStatus::Draft {
        return Err(ApiError::conflict(
            "only a draft invoice is edited in place; use the correction revision action",
        ));
    }
    if edit
        .status
        .is_some_and(|status| status != crate::domain::InvoiceStatus::Draft)
    {
        return Err(ApiError::validation(vec![FieldError::new(
            "status",
            "issue the draft from the invoice view instead of editing its status",
        )]));
    }
    let edited = assemble_edit(&app, &invoice, &edit)?;
    let candidate = Invoice {
        document: edited.document,
        currency: edited.currency,
        lines: edited.lines,
        total_minor: edited.total_minor,
        tax_hundredths: edited.tax_hundredths,
        discount_hundredths: edited.discount_hundredths,
        // #256: the loaded draft is not read again below — move it into the
        // candidate instead of deep-cloning the whole document (the store then
        // stamps identity in place rather than cloning again).
        ..invoice
    };
    let updated = app.store.replace_invoice_draft(id, candidate)?;
    record_timeline(
        &app,
        updated.id,
        InvoiceTimelineEventKind::Updated,
        format!("Invoice draft updated ({} lines).", updated.lines.len()),
        actor.0.name.clone(),
        None,
    )
    .await;
    Ok(Json(updated).into_response())
}

/// Create a correction revision of an issued or imported invoice. The
/// original document and its archived PDF are left intact; the revision keeps
/// the number, gets its own id and (unless it is a draft) its own PDF, and the
/// original is marked `superseded_by`. The reviewed status may return the
/// corrected document to a draft so it can be edited and issued again.
pub async fn revise_invoice(
    State(app): State<AppState>,
    actor: AuthUser,
    Path(id): Path<Uuid>,
    ValidJson(edit): ValidJson<InvoiceEditInput>,
) -> ApiResult {
    let invoice = get_invoice_or_404(&app, id)?;
    if invoice.status == crate::domain::InvoiceStatus::Draft {
        return Err(ApiError::conflict(
            "a draft invoice is edited in place, not revised",
        ));
    }
    if invoice.superseded_by.is_some() {
        return Err(ApiError::conflict(
            "this invoice already has a correction revision",
        ));
    }
    let edited = assemble_edit(&app, &invoice, &edit)?;
    let status = edit.status.unwrap_or(invoice.status);
    let mut errors = Vec::new();
    validate_status(status, edited.total_minor, &invoice.payments, &mut errors);
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let document_due = edited.document.due_date;
    let document_terms = edited.document.payment_terms;
    let now = app.clock.now();
    let mut revision = Invoice {
        imported: None,
        supersedes: Some(invoice.id),
        superseded_by: None,
        document: edited.document,
        id: Uuid::new_v4(),
        number: invoice.number.clone(),
        customer_id: invoice.customer_id,
        currency: edited.currency,
        period_from: invoice.period_from,
        period_to: invoice.period_to,
        lines: edited.lines,
        total_minor: edited.total_minor,
        status,
        created_at: now,
        issued_at: None,
        due_date: None,
        paid_at: None,
        payment_reference: invoice.payment_reference.clone(),
        tax_hundredths: edited.tax_hundredths,
        discount_hundredths: edited.discount_hundredths,
        pdf: None,
        payments: invoice.payments.clone(),
        write_off_reason: String::new(),
        written_off_at: None,
        // Resolved below from the reviewed document/customer/template, so a
        // raw placeholder subject can never be frozen onto the revision.
        subject: String::new(),
        terms: invoice.terms,
    };
    match status {
        crate::domain::InvoiceStatus::Draft => {
            revision.payments.clear();
        }
        crate::domain::InvoiceStatus::Issued | crate::domain::InvoiceStatus::PartlyPaid => {
            revision.issued_at = Some(now);
            revision.due_date = document_due.or(invoice.due_date);
            revision.terms = document_terms.or(invoice.terms);
        }
        crate::domain::InvoiceStatus::Paid => {
            revision.issued_at = Some(now);
            revision.due_date = document_due.or(invoice.due_date);
            revision.terms = document_terms.or(invoice.terms);
            revision.paid_at = invoice.paid_at.or(Some(now));
        }
        crate::domain::InvoiceStatus::WrittenOff => {
            revision.issued_at = Some(now);
            revision.due_date = document_due.or(invoice.due_date);
            revision.terms = document_terms.or(invoice.terms);
            revision.paid_at = invoice.paid_at;
            revision.written_off_at = invoice.written_off_at.or(Some(now));
            revision.write_off_reason = if invoice.write_off_reason.is_empty() {
                "Status set on correction revision".into()
            } else {
                invoice.write_off_reason.clone()
            };
        }
    }
    if status != crate::domain::InvoiceStatus::Draft {
        // Resolve the reviewed subject exactly as issuing would: the editor may
        // carry the org template's placeholders (`%customer_name% ...`), and
        // those must be interpolated, never frozen raw onto the revision.
        let customer = app
            .store
            .get_customer(invoice.customer_id)?
            .ok_or_else(|| ApiError::not_found("customer"))?;
        let template = load_template(&app)?;
        let resolved = content_for(&revision, &customer, &template);
        revision.subject = resolved.subject.unwrap_or_default();
    }
    let pdf = if status == crate::domain::InvoiceStatus::Draft {
        None
    } else {
        Some(super::render_pdf_now(&app, &revision)?)
    };
    let saved = app
        .store
        .create_invoice_revision(invoice.id, revision, pdf.as_deref(), now)?;
    record_timeline(
        &app,
        saved.id,
        InvoiceTimelineEventKind::Created,
        format!(
            "Correction revision of {} ({} lines).",
            invoice.number,
            saved.lines.len()
        ),
        actor.0.name.clone(),
        None,
    )
    .await;
    record_timeline(
        &app,
        invoice.id,
        InvoiceTimelineEventKind::Updated,
        format!("Superseded by correction revision {}.", saved.id),
        actor.0.name.clone(),
        None,
    )
    .await;
    Ok((StatusCode::CREATED, Json(saved)).into_response())
}
