// expenses.js — TatuTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.
// The receipt half lives in ./receipt.js (#332).


import { api, fetchAll } from './api.js';
import { $, announce, askConfirm, clearFormError, el, fillSelect, formatMoney, iconActionsCell, panelRefresh, showFormError, singleFlight, statusChip } from './dom.js';
import { customerName, fillCustomerSelect, fillProjectSelect, loadProjects, state } from './state.js';
import { adoptExpenseRate, billedCurrency, currencyCode, refreshExpenseConversion, resetExpenseConversion, toMinor } from './fx.js';
import { invalidateLockCache, refreshDay } from './timesheet.js';
import { getDraftReceipt, readReceipt, RECEIPT_NOTE, receiptCell, setDraftReceipt } from './receipt.js';

// ---------------------------------------------------------------- expenses --

export let categoryRefreshSequence = 0;

export async function refreshCategories() {
  const sequence = ++categoryRefreshSequence;
  const data = await api.get('/categories');
  if (sequence !== categoryRefreshSequence) return;
  state.categories = data.categories || [];
  const ul = $('category-list');
  ul.textContent = '';
  for (const cat of state.categories) {
    ul.appendChild(
      el(
        'li',
        { cls: 'p-chip' },
        [
          el('span', { cls: 'p-chip__value', text: cat.name }),
          el('button', {
            type: 'button',
            cls: 'chip-edit',
            text: 'Edit',
            data: { icon: 'edit' },
            attrs: { 'aria-label': `Edit category ${cat.name}` },
            on: { click: () => startCategoryEdit(cat) },
          }),
          // #232 increment 3: the official chip dismiss affordance — icon-only
          // (the Vanilla close glyph) with an accessible name.
          el('button', {
            type: 'button',
            cls: 'p-chip__dismiss',
            attrs: { 'aria-label': `Delete category ${cat.name}`, title: `Delete category ${cat.name}` },
            on: { click: () => removeCategory(cat) },
          }),
        ],
      ),
    );
  }
  const select = $('expense-category');
  if (select) await fillCategorySelect(select.value);
}

// Settings → Expense categories uses the same add/edit shape as Users: the
// heading slot's + Category (or a chip's Edit) opens this dialog.
export function openCategoryDialog(category = null) {
  $('category-form').reset();
  $('category-id').value = category ? category.id : '';
  $('category-name').value = category ? category.name : '';
  $('category-dialog-title').textContent = category ? 'Edit category' : 'Add category';
  $('category-save').textContent = category ? 'Save category' : 'Add category';
  clearFormError($('category-error'));
  $('category-dialog').showModal();
  $('category-name').focus();
}

export function startCategoryEdit(category) {
  openCategoryDialog(category);
}

export function resetCategoryForm() {
  $('category-id').value = '';
  $('category-form').reset();
  $('category-dialog-title').textContent = 'Add category';
  $('category-save').textContent = 'Add category';
  clearFormError($('category-error'));
}

export function closeCategoryDialog() {
  const dialog = $('category-dialog');
  if (dialog.open) dialog.close();
  resetCategoryForm();
}

export async function removeCategory(category) {
  try {
    await api.del(`/categories/${category.id}`);
    await refreshCategories();
    announce(`Category ${category.name} deleted.`);
  } catch (err) {
    announce(err.message);
  }
}

export async function addCategory(evt) {
  evt.preventDefault();
  // QA-01: one POST /categories per click, not one per event.
  return singleFlight($('category-save'), async () => {
    clearFormError($('category-error'));
    const id = $('category-id').value;
    try {
      const existing = id ? (state.categories || []).find((category) => category.id === id) : null;
      const body = {
        name: $('category-name').value.trim(),
        default_billable: existing ? existing.default_billable : true,
        active: existing ? existing.active : true,
      };
      if (id) await api.put(`/categories/${id}`, body);
      else await api.post('/categories', body);
      closeCategoryDialog();
      await refreshCategories();
      announce(id ? 'Category updated.' : 'Category added.');
    } catch (err) {
      showFormError($('category-error'), err);
    }
  });
}

export const ADD_CATEGORY_VALUE = '__add_expense_category__';

export async function fillCategorySelect(preferred = '') {
  const select = $('expense-category');
  if (!select) return;
  const current = preferred || select.value;
  fillSelect(select, [
    { value: '', text: 'None' },
    ...(state.categories || []).filter((cat) => cat.active).map((cat) => ({ value: cat.id, text: cat.name })),
    { value: ADD_CATEGORY_VALUE, text: '+ Add new category…' },
  ]);
  select.value = (state.categories || []).some((cat) => cat.id === current && cat.active) ? current : '';
}

export async function addExpenseCategory(evt) {
  evt.preventDefault();
  // QA-01: the quick-add dialog has no `#...-save` id of its own, so the guard
  // sits on the form's actual submit control.
  const saveButton = $('expense-category-form').querySelector('[type="submit"]');
  return singleFlight(saveButton, async () => {
    clearFormError($('expense-category-error'));
    try {
      const created = await api.post('/categories', {
        name: $('expense-category-name').value.trim(),
        default_billable: true,
        active: true,
      });
      $('expense-category-form').reset();
      $('expense-category-dialog').close();
      await refreshCategories();
      await fillCategorySelect(created.id);
      announce(`Category ${created.name} added and selected.`);
      $('expense-category').focus();
    } catch (err) {
      showFormError($('expense-category-error'), err);
    }
  });
}

/// A fresh draft: no id, no receipt, and the dialog back to its create wording.
export function resetExpenseForm() {
  $('expense-id').value = '';
  $('expense-receipt').value = '';
  $('expense-receipt-clear').checked = false;
  $('expense-receipt-clear-field').hidden = true;
  $('expense-receipt-note').textContent = RECEIPT_NOTE;
  $('expense-dialog-title').textContent = 'Record an expense';
  $('expense-save').textContent = 'Save expense';
  setDraftReceipt(null);
  resetExpenseConversion();
}

/// Load an existing expense into the dialog. The record's own rate and rate date
/// are adopted rather than re-quoted, so saving an untouched conversion cannot
/// silently restate it at today's published rate.
export async function startExpenseEdit(x) {
  resetExpenseForm();
  $('expense-id').value = x.id;
  $('expense-dialog-title').textContent = `Edit expense ${x.id.slice(0, 8)}`;
  $('expense-save').textContent = 'Save changes';
  $('expense-date').value = x.date;
  fillCustomerSelect($('expense-customer'), x.customer_id, true);
  await fillProjectSelect($('expense-project'), x.customer_id, x.project_code || '');
  await fillCategorySelect(x.category_id || '');
  $('expense-amount').value = formatMoney(x.amount_minor);
  $('expense-currency').value = x.original_currency || x.currency;
  $('expense-rate').value = x.exchange_rate || '';
  $('expense-rate-date').value = x.exchange_rate_date || '';
  $('expense-billable').checked = x.billable;
  $('expense-note').value = x.note || '';
  if (x.receipt_b64) {
    const draft = { name: x.receipt_name || 'receipt', b64: x.receipt_b64 };
    setDraftReceipt(draft);
    $('expense-receipt-clear-field').hidden = false;
    $('expense-receipt-note').textContent =
      `Attached: ${draft.name}. Choose a file to replace it, or tick Remove.`;
  }
  adoptExpenseRate(
    currencyCode($('expense-currency').value),
    billedCurrency(),
  );
  $('expense-dialog').showModal();
  $('expense-date').focus();
  await refreshExpenseConversion();
}

export function refreshExpenses() {
  // QA-10: a failed register load keeps the last rows and shows the banner.
  return panelRefresh('panel-expenses', 'the expense register', refreshExpensesBody);
}

async function refreshExpensesBody() {
  // #334: the register is the one view with no range filter, so it is the one
  // that can outgrow a single page. It follows every cursor, and says so if the
  // client-side cap stopped the walk — a register that quietly showed the first
  // page would be the silent cut this bound exists to prevent.
  const { rows: all, truncated } = await fetchAll('/expenses', 'expenses');
  if (truncated) {
    announce(`Showing the first ${all.length} expenses — more exist than this view can page through.`);
  }
  // The register is scoped by customer and project, both defaulting to "all"
  // (an unset filter is a scope, not a gap — same shape as the Tasks section).
  const custSel = $('expense-filter-customer');
  fillSelect(custSel, [
    { value: '', text: 'All customers' },
    ...state.customers.map((c) => ({
      value: c.id,
      text: c.active !== false ? c.name : `${c.name} (inactive)`,
      selected: c.id === state.expensesCustomer,
    })),
  ]);
  custSel.value = state.expensesCustomer || '';
  const projSel = $('expense-filter-project');
  // Projects belong to a customer, so concrete choices only exist once one is
  // picked; with all customers the pick rests on "All projects".
  const projects = state.expensesCustomer ? await loadProjects(state.expensesCustomer) : [];
  fillSelect(projSel, [
    { value: '', text: 'All projects' },
    ...projects.map((p) => ({ value: p.code, text: p.code, selected: p.code === state.expensesProject })),
  ]);
  projSel.disabled = false;
  projSel.value = state.expensesProject || '';
  const expenses = all.filter((x) =>
    (!state.expensesCustomer || x.customer_id === state.expensesCustomer)
    && (!state.expensesProject || (x.project_code || '') === state.expensesProject));
  const tbody = $('expense-table').querySelector('tbody');
  tbody.textContent = '';
  for (const x of expenses) {
    const c = (state.categories || []).find((k) => k.id === x.category_id);
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' }, text: x.date }),
        el('td', { attrs: { 'data-heading': 'Customer' }, text: customerName(x.customer_id) }),
        el('td', { attrs: { 'data-heading': 'Project' }, text: x.project_code || '' }),
        el('td', { attrs: { 'data-heading': 'Category' }, text: c ? c.name : '' }),
        // No `num`: the amount reads left-aligned with its column header, like
        // every other register (the utility right-aligns).
        el('td', { attrs: { 'data-heading': 'Amount' } }, [
          el('span', { text: `${x.currency} ${formatMoney(x.amount_minor)}` }),
          // A converted expense keeps the amount as entered: the register shows
          // the billed value the invoice will use and, beside it, what was paid.
          x.original_currency
            ? el('span', {
              cls: 'fx-original',
              text: ` (${x.original_currency} ${formatMoney(x.original_amount_minor)} @ ${x.exchange_rate})`,
            })
            : null,
        ]),
        el('td', { attrs: { 'data-heading': 'Billable' }, text: x.billable ? 'yes' : 'no' }),
        // The stored file's own name, as the link that opens its preview — the
        // column answers "is there a receipt?" without opening the record.
        el('td', { attrs: { 'data-heading': 'Receipt' } }, [receiptCell(x)]),
        iconActionsCell(x.imported?.invoiced ? [statusChip('Invoiced in Harvest')] : [
          el('button', {
            cls: 'link',
            type: 'button',
            text: 'Edit',
            data: { icon: 'edit' },
            on: { click: () => startExpenseEdit(x) },
          }),
          el('button', {
            cls: 'danger',
            type: 'button',
            text: 'Delete',
            data: { icon: 'delete' },
            on: {
              click: async () => {
                if (!(await askConfirm('Delete this expense?'))) return;
                // #188: announce failures like every other mutation; an
                // un-caught rejection here meant a silent, unreported delete.
                try {
                  await api.del(`/expenses/${x.id}`);
                  await refreshExpenses();
                  announce('Expense deleted.');
                } catch (err) {
                  announce(`Expense delete failed: ${err.message}`);
                }
              },
            },
          }),
        ]),
      ]),
    );
  }
  $('expense-empty').hidden = expenses.length !== 0;
}

export async function saveExpense(evt) {
  evt.preventDefault();
  // QA-01: two submits must not record the expense twice.
  return singleFlight($('expense-save'), saveExpenseBody);
}

async function saveExpenseBody() {
  clearFormError($('expense-error'));
  const id = $('expense-id').value;
  const enteredMinor = toMinor($('expense-amount').value) || 0;
  const entered = currencyCode($('expense-currency').value);
  // The currency the picked customer/project bills in. The amount is already in
  // it unless the user said otherwise; then the dialog's rate converts it and
  // the payload carries the provenance the API records.
  const billed = billedCurrency();
  const rate = $('expense-rate').value.trim();
  const converting = Boolean(entered && billed && entered !== billed);
  if (converting && !(Number.parseFloat(rate) > 0)) {
    showFormError(
      $('expense-error'),
      new Error(`Enter the ${entered}/${billed} exchange rate, or clear the amount's currency.`),
    );
    $('expense-rate').focus();
    return;
  }
  const body = {
    date: $('expense-date').value,
    customer_id: $('expense-customer').value,
    // Billed in the customer/project currency: the entered amount converted at
    // the rate, in whole minor units, rounded exactly once (#11).
    amount_minor: converting ? Math.round(enteredMinor * Number.parseFloat(rate)) : enteredMinor,
    currency: converting || !entered ? billed : entered,
    billable: $('expense-billable').checked,
    note: $('expense-note').value,
  };
  if (converting) {
    body.original_amount_minor = enteredMinor;
    body.original_currency = entered;
    body.exchange_rate = rate;
    if ($('expense-rate-date').value) body.exchange_rate_date = $('expense-rate-date').value;
  }
  if ($('expense-project').value) body.project_code = $('expense-project').value;
  if ($('expense-category').value && $('expense-category').value !== ADD_CATEGORY_VALUE) {
    body.category_id = $('expense-category').value;
  }
  // The receipt: a file picked now replaces whatever was stored, an untouched
  // field sends the record's own receipt back (a PUT replaces the whole record),
  // and the Remove box clears it. Reading the file can refuse it (empty, or over
  // the size the API stores) — that is the dialog's answer to the user, not a
  // rejected save, so it surfaces on the form and stops here.
  const picked = $('expense-receipt').files && $('expense-receipt').files[0];
  let receipt = null;
  try {
    receipt = picked
      ? await readReceipt(picked)
      : $('expense-receipt-clear').checked ? null : getDraftReceipt();
  } catch (err) {
    showFormError($('expense-error'), err);
    $('expense-receipt').focus();
    return;
  }
  if (receipt) {
    body.receipt_name = receipt.name;
    body.receipt_b64 = receipt.b64;
  }
  try {
    if (id) await api.put(`/expenses/${id}`, body);
    else await api.post('/expenses', body);
    resetExpenseForm();
    $('expense-dialog').close();
    announce(id ? 'Expense updated.' : 'Expense added.');
    await refreshExpenses();
  } catch (err) {
    showFormError($('expense-error'), err);
  }
}

// ------------------------------------------------------------ submissions --

export function refreshSubmissions() {
  // QA-10: the approvals register follows the same stale-data contract.
  return panelRefresh('panel-submissions', 'the approvals register', refreshSubmissionsBody);
}

async function refreshSubmissionsBody() {
  const data = await api.get('/submissions');
  const subs = data.submissions || [];
  const tbody = $('submission-table').querySelector('tbody');
  tbody.textContent = '';
  for (const s of subs) {
    const actions = [];
    if (s.state === 'submitted') {
      actions.push(
        el('button', {
          cls: 'link',
          type: 'button',
          text: 'Approve',
          on: { click: () => decideSubmission(s.id, 'approve') },
        }),
        el('button', {
          cls: 'danger',
          type: 'button',
          text: 'Reject',
          on: { click: () => decideSubmission(s.id, 'reject') },
        }),
      );
    }
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' }, text: `${s.week_start} → ${s.week_end}` }),
        el('td', { cls: 'num', attrs: { 'data-heading': 'Entries' }, text: String(s.entry_ids.length) }),
        el('td', { attrs: { 'data-heading': 'State' } }, [
          statusChip(s.state, s.state === 'approved'),
        ]),
        el('td', { attrs: { 'data-heading': 'Comment' }, text: s.comment || '' }),
        el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, actions),
      ]),
    );
  }
  $('submission-empty').hidden = subs.length !== 0;
}

export async function submitWeek(evt) {
  evt.preventDefault();
  clearFormError($('submission-error'));
  try {
    await api.post('/submissions', { week_start: $('submission-week').value });
    invalidateLockCache(); // the submitted week now locks its entries
    announce('Timesheet submitted.');
    await refreshSubmissions();
    await refreshDay();
  } catch (err) {
    showFormError($('submission-error'), err);
  }
}

export async function decideSubmission(id, decision) {
  try {
    await api.post(`/submissions/${id}/decision`, { decision, comment: '' });
    invalidateLockCache(); // approve keeps the lock, reject releases it
    announce(`Timesheet ${decision === 'approve' ? 'approved' : 'rejected'}.`);
    await refreshSubmissions();
  } catch (err) {
    announce(err.message);
  }
}

