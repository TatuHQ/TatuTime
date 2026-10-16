// expenses.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { $, announce, askConfirm, clearFormError, el, fillSelect, formatMoney, showFormError, statusChip } from './dom.js';
import { customerName, fillCustomerSelect, fillProjectSelect, loadProjects, state } from './state.js';
import { adoptExpenseRate, billedCurrency, currencyCode, refreshExpenseConversion, resetExpenseConversion, toMinor } from './fx.js';
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

// ---------------------------------------------------------------- receipt ---
//
// The API keeps a receipt inline with its expense (#23): the file's name and its
// bytes as base64. The dialog owns one draft receipt — the file just picked, or
// the one the record being edited already carries, because a PUT replaces the
// whole record and an untouched receipt has to travel back with the payload.

let draftReceipt = null;

/// The record the open preview is showing, so its Download button saves exactly
/// what the dialog displays.
let previewRecord = null;

const RECEIPT_NOTE = 'Images and PDFs up to 3 MB, stored with the expense.';

/// The largest receipt the API stores, in *file* bytes: the server refuses more
/// than 4,000,000 characters of base64 (`domain::validate_expense_input`), which
/// is a 3 MB file once encoded. Checking here names the file and its size instead
/// of spending a round trip to be told the payload was too large.
export const MAX_RECEIPT_BYTES = 3_000_000;

/// `bytes` as a short human size, for the two refusals below.
function receiptSize(bytes) {
  return bytes >= 1024 * 1024
    ? `${(bytes / (1024 * 1024)).toFixed(1)} MB`
    : `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

/// A picked file, shaped the way the API stores it. Two shapes are refused here
/// rather than stored: an empty file (a name with no bytes is a receipt the
/// register cannot open and the user cannot tell from a missing one — storing it
/// silently is how a hollow record gets written) and one over the server's cap.
async function readReceipt(file) {
  if (!file.size) {
    throw new Error(`${file.name} is empty (0 bytes) — attach a file with content.`);
  }
  if (file.size > MAX_RECEIPT_BYTES) {
    throw new Error(
      `${file.name} is ${receiptSize(file.size)} — the largest receipt is 3 MB.`,
    );
  }
  const data = await new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error('That file could not be read.'));
    reader.onload = () => resolve(String(reader.result || ''));
    reader.readAsDataURL(file);
  });
  const comma = data.indexOf(',');
  const b64 = comma >= 0 ? data.slice(comma + 1) : '';
  if (!b64) throw new Error(`${file.name} could not be read — attach it again.`);
  return { name: file.name, b64 };
}

/// Hand a stored receipt to the browser as a download under its own name.
export function downloadReceipt(x) {
  if (!x || !x.receipt_b64) return;
  const link = document.createElement('a');
  link.href = `data:${receiptMime(x.receipt_name)};base64,${x.receipt_b64}`;
  link.download = x.receipt_name || `receipt-${x.date}`;
  link.rel = 'noopener';
  document.body.appendChild(link);
  link.click();
  link.remove();
}

/// The type a stored receipt is handed back under, so the browser picks its own
/// viewer. Storage keeps the file name (#23) and nothing else, so the extension
/// is the only hint there is; anything unknown stays opaque and downloads.
function receiptMime(name) {
  const ext = String(name || '').toLowerCase().split('.').pop();
  return {
    png: 'image/png',
    jpg: 'image/jpeg',
    jpeg: 'image/jpeg',
    gif: 'image/gif',
    webp: 'image/webp',
    bmp: 'image/bmp',
    svg: 'image/svg+xml',
    tif: 'image/tiff',
    tiff: 'image/tiff',
    heic: 'image/heic',
    pdf: 'application/pdf',
  }[ext] || 'application/octet-stream';
}

/// The stored base64 as the bytes a blob can carry.
function receiptBlob(b64, mime) {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return new Blob([bytes], { type: mime });
}

/// The blob URL the open preview is showing, so closing revokes it: a receipt of
/// a few megabytes must not be held alive by a dialog the user has dismissed.
let previewUrl = '';

function releasePreviewUrl() {
  if (previewUrl) URL.revokeObjectURL(previewUrl);
  previewUrl = '';
}

/// Open a stored receipt in its preview: an image as an image (a data: URL, which
/// the CSP already allows for images), anything else — a PDF — framed from a blob
/// URL the browser renders with its own viewer (`frame-src 'self' blob:`). The
/// bytes already travel with the record, so the preview costs no request, and the
/// dialog's Download button saves the very bytes it is showing.
export function previewReceipt(x) {
  if (!x || !x.receipt_b64) return;
  const name = x.receipt_name || `receipt-${x.date}`;
  const mime = receiptMime(name);
  const host = $('receipt-preview');
  host.textContent = '';
  releasePreviewUrl();
  previewRecord = x;
  $('receipt-dialog-title').textContent = name;
  host.appendChild(
    mime.startsWith('image/')
      ? el('img', {
        attrs: { id: 'receipt-image', src: `data:${mime};base64,${x.receipt_b64}`, alt: `Receipt ${name}` },
      })
      : el('iframe', {
        attrs: { id: 'receipt-frame', src: previewUrl = URL.createObjectURL(receiptBlob(x.receipt_b64, mime)), title: `Receipt ${name}` },
      }),
  );
  $('receipt-dialog').showModal();
  $('receipt-close').focus();
}

/// Let go of what the open preview holds: the blob URL it minted, the record it
/// would download, and the document element in the dialog.
function clearReceiptPreview() {
  releasePreviewUrl();
  previewRecord = null;
  $('receipt-preview').textContent = '';
}

/// Dismiss the preview. The Close button runs this directly rather than leaning on
/// the dialog's `close` event, so the teardown happens on the path the user takes
/// — and the event keeps Escape and any other native dismissal just as clean.
export function closeReceiptPreview() {
  $('receipt-dialog').close();
  clearReceiptPreview();
}

/// Wire the preview dialog once, at boot: Download saves what is open, Close
/// dismisses it, and every close path releases the bytes.
export function initReceiptPreview() {
  $('receipt-close').addEventListener('click', closeReceiptPreview);
  $('receipt-download').addEventListener('click', () => downloadReceipt(previewRecord));
  $('receipt-dialog').addEventListener('close', clearReceiptPreview);
}

/// The register's receipt cell: the stored file's own name as the link that
/// opens the preview when the record carries bytes, and a dash when it does not
/// — announced, because "no receipt" is an answer, not an empty cell. A record
/// whose name was stored with no bytes (an empty file, before the dialog refused
/// them) says so rather than offering a preview of nothing.
function receiptCell(x) {
  const name = x.receipt_name || `receipt-${x.date}`;
  if (x.receipt_b64) {
    return el('button', {
      cls: 'link',
      type: 'button',
      data: { icon: 'file' },
      text: name,
      attrs: {
        'aria-label': `Preview the receipt for ${x.date} (${name})`,
        title: `Preview ${name}`,
      },
      on: { click: () => previewReceipt(x) },
    });
  }
  return el('span', {}, [
    el('span', { attrs: { 'aria-hidden': 'true' }, text: '\u2014' }),
    el('span', {
      cls: 'sr-only',
      text: x.receipt_name ? 'The attached receipt is empty' : 'No receipt attached',
    }),
  ]);
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
  draftReceipt = null;
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
    draftReceipt = { name: x.receipt_name || 'receipt', b64: x.receipt_b64 };
    $('expense-receipt-clear-field').hidden = false;
    $('expense-receipt-note').textContent =
      `Attached: ${draftReceipt.name}. Choose a file to replace it, or tick Remove.`;
  }
  adoptExpenseRate(
    currencyCode($('expense-currency').value),
    billedCurrency(),
  );
  $('expense-dialog').showModal();
  $('expense-date').focus();
  await refreshExpenseConversion();
}

export async function refreshExpenses() {
  const data = await api.get('/expenses');
  const all = data.expenses || [];
  // The register is scoped by customer and project, both defaulting to "all"
  // (an unset filter is a scope, not a gap — same shape as the Tasks section).
  const custSel = $('expense-filter-customer');
  fillSelect(custSel, [
    { value: '', text: 'All customers' },
    ...state.customers.map((c) => ({
      value: c.id,
      text: c.active ? c.name : `${c.name} (inactive)`,
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
        el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, [
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
      : $('expense-receipt-clear').checked ? null : draftReceipt;
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

