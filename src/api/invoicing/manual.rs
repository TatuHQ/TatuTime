use super::{get_invoice_or_404, lifecycle::record_timeline};
use crate::api::*;
use crate::domain::InvoiceTimelineEventKind;

// ------------------------------------------------- manual lines & edit ----

/// Body for `POST /invoices/manual` (#143).
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualInvoiceInput {
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

/// Body for `PUT /invoices/{id}` (#143): draft-only full replacement of the
/// line set + percentages. Tracked lines must come back exactly as stored
/// (identity and amounts are enforced server-side); manual lines are free.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceEditInput {
    pub lines: Vec<InvoiceLineInput>,
    #[serde(default)]
    pub tax_hundredths: u16,
    #[serde(default)]
    pub discount_hundredths: u16,
}

fn percent_field(value: u16, field: &str, errors: &mut Vec<FieldError>) {
    if value > 10_000 {
        errors.push(FieldError::new(field, "at most 100.00 percent"));
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
        return Err(ApiError::conflict("only a draft invoice can be edited"));
    }
    let mut errors = Vec::new();
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
                    "tracked lines must come from this draft",
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
        } else {
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
    }
    validate_line_projects(&app, &invoice.customer_id, &out_lines, &mut errors)?;
    if !errors.is_empty() {
        return Err(ApiError::validation(errors));
    }
    let subtotal = out_lines.iter().map(|l| l.amount_minor).sum();
    let totals =
        crate::domain::invoice_totals(subtotal, edit.tax_hundredths, edit.discount_hundredths);
    let candidate = Invoice {
        lines: out_lines,
        total_minor: totals.total_minor,
        tax_hundredths: edit.tax_hundredths,
        discount_hundredths: edit.discount_hundredths,
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
