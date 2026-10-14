// expenses.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { $, announce, askConfirm, clearFormError, el, fillSelect, formatMoney, showFormError, statusChip } from './dom.js';
import { customerName, state } from './state.js';
import { invalidateLockCache, refreshDay } from './timesheet.js';

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

export function startCategoryEdit(category) {
  $('category-id').value = category.id;
  $('category-name').value = category.name;
  $('category-save').textContent = 'Save category';
  $('category-cancel').hidden = false;
  $('category-name').focus();
}

export function resetCategoryForm() {
  $('category-id').value = '';
  $('category-form').reset();
  $('category-save').textContent = 'Add category';
  $('category-cancel').hidden = true;
  clearFormError($('category-error'));
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
    resetCategoryForm();
    await refreshCategories();
    announce(id ? 'Category updated.' : 'Category added.');
  } catch (err) {
    showFormError($('category-error'), err);
  }
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
}

export async function refreshExpenses() {
  const data = await api.get('/expenses');
  const expenses = data.expenses || [];
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
        el('td', { cls: 'num', attrs: { 'data-heading': 'Amount' }, text: `${x.currency} ${formatMoney(x.amount_minor)}` }),
        el('td', { attrs: { 'data-heading': 'Billable' }, text: x.billable ? 'yes' : 'no' }),
        el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, [
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

export async function addExpense(evt) {
  evt.preventDefault();
  clearFormError($('expense-error'));
  const body = {
    date: $('expense-date').value,
    customer_id: $('expense-customer').value,
    amount_minor: Math.round(Number($('expense-amount').value) * 100),
    currency: $('expense-currency').value.trim().toUpperCase(),
    billable: $('expense-billable').checked,
    note: $('expense-note').value,
  };
  if ($('expense-project').value) body.project_code = $('expense-project').value;
  if ($('expense-category').value) body.category_id = $('expense-category').value;
  try {
    await api.post('/expenses', body);
    $('expense-amount').value = '';
    $('expense-note').value = '';
    $('expense-dialog').close();
    announce('Expense added.');
    await refreshExpenses();
  } catch (err) {
    showFormError($('expense-error'), err);
  }
}

// ------------------------------------------------------------ submissions --

export async function refreshSubmissions() {
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

