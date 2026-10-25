// billing.js — TatuTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api, fetchAll } from './api.js';
import { $, addDays, announce, askConfirm, askPrompt, clearFormError, el, formatMoney, isoDate, mondayOf, rowAction, showFormError, singleFlight, statusChip } from './dom.js';
import { parseHundredths, refreshInvoices } from './invoices.js';
import { pdfDeadline, renderPdfDocument } from './pdf.js';
import { customerName, state } from './state.js';

// ------------------------------------------------ advanced billing gate ----
//
// Recurring invoice schedules (#136) and retainers (#144) are not finished, so
// the GUI does not offer them. One switch controls the whole surface: the
// `data-advanced-billing` attribute on <html> in web/index.html. `"off"` (the
// shipped value) hides the `#advanced-billing` block through the stylesheet and
// makes boot.js skip its data fetches; changing that single attribute to `"on"`
// turns the tools back on. The API, the stored records and the business logic
// are untouched — this is a visibility switch only.
export const ADVANCED_BILLING_ENABLED =
  document.documentElement.getAttribute('data-advanced-billing') !== 'off';

// Mirror the switch onto the container itself, so the block is inert even when
// the stylesheet has not loaded: `hidden` removes the whole subtree from the
// tab order and from the accessibility tree.
const advancedBilling = document.getElementById('advanced-billing');
if (advancedBilling) advancedBilling.hidden = !ADVANCED_BILLING_ENABLED;

// --------------------------------------------------- invoice wizard (#134) --
//
// Harvest-shaped staged flow over the SAME server pipeline: preview via
// POST /invoices/preview (nothing persisted), save via the existing
// POST /invoices with project selection. Draft-only: issue/email/sync stay
// separate explicit actions on the table.

export const iw = { step: 1, from: '', to: '', preview: null };

// Live candidate-document preview state. A new render supersedes the previous
// one by sequence, and every blob URL / PDF.js document is released exactly
// once — the same discipline as the invoice detail panel (#133/#349).
let iwPreviewSeq = 0;
let iwPreviewDispose = null;
let iwPreviewUrl = '';

export function iwGrouping() {
  return $('iw-grouping')?.value || 'project';
}

function iwPreviewPayload() {
  return {
    customer_id: $('iw-customer').value,
    from: iw.from || $('iw-from').value,
    to: iw.to || $('iw-to').value,
    include_expenses: $('iw-expenses').checked,
    project_codes: iwSelectedProjects(),
    line_grouping: iwGrouping(),
  };
}

function releaseIwPreview() {
  iwPreviewDispose?.();
  iwPreviewDispose = null;
  if (iwPreviewUrl) URL.revokeObjectURL(iwPreviewUrl);
  iwPreviewUrl = '';
}

/// Whether a response is a real PDF before handing it to the viewer (a proxy
/// error page under a 200 must not paint a blank frame).
function iwIsPdf(blob, bytes) {
  if (blob && typeof blob.type === 'string' && blob.type && blob.type !== 'application/pdf') return false;
  const sig = [0x25, 0x50, 0x44, 0x46, 0x2d]; // "%PDF-"
  return bytes.length >= sig.length && sig.every((b, i) => bytes[i] === b);
}

/// Render the candidate invoice — the exact selection and grouping the wizard
/// will save — beside the review list. Stateless: POST /invoices/preview/pdf
/// persists nothing.
export async function iwRefreshPreview() {
  const host = $('iw-preview');
  const status = $('iw-preview-state');
  if (!host || !status || iw.step !== 3) return;
  const seq = ++iwPreviewSeq;
  const current = () => seq === iwPreviewSeq && iw.step === 3;
  releaseIwPreview();
  host.textContent = '';
  const payload = iwPreviewPayload();
  if (!payload.customer_id || !payload.from || !payload.to || payload.from > payload.to) {
    status.hidden = false;
    status.textContent = 'Pick a customer and a valid period to preview the document.';
    return;
  }
  status.hidden = false;
  status.textContent = 'Rendering preview…';
  try {
    const result = await pdfDeadline(
      api.requestBlob('POST', '/invoices/preview/pdf', payload),
      20000,
      'The preview request timed out. Change a choice to retry.',
    );
    if (!current()) return;
    const blob = result.blob;
    const bytes = new Uint8Array(await blob.arrayBuffer());
    if (!current()) return;
    if (!iwIsPdf(blob, bytes)) {
      status.textContent = 'Preview unavailable — the server did not return a PDF.';
      return;
    }
    const url = URL.createObjectURL(new Blob([bytes], { type: 'application/pdf' }));
    if (!current()) {
      URL.revokeObjectURL(url);
      return;
    }
    iwPreviewUrl = url;
    const viewer = el('div', { cls: 'pdf-viewer' });
    const open = el('a', {
      text: 'Open PDF',
      attrs: { href: url, target: '_blank', rel: 'noopener' },
    });
    host.replaceChildren(viewer, el('div', { cls: 'pdf-actions' }, [open]));
    const dispose = await renderPdfDocument(viewer, bytes, current);
    if (!current()) {
      dispose();
      return;
    }
    iwPreviewDispose = dispose;
    status.hidden = true;
    status.textContent = '';
  } catch (error) {
    if (!current()) return;
    status.hidden = false;
    status.textContent = `Preview unavailable — ${error.message}`;
  }
}

export function iwPresetRange(preset) {
  const today = isoDate(new Date());
  const d = new Date(`${today}T00:00:00Z`);
  const first = (y, m) => `${y}-${String(m + 1).padStart(2, '0')}-01`;
  const lastDay = (y, m) => new Date(Date.UTC(y, m + 1, 0)).getUTCDate();
  if (preset === 'this-month') {
    iw.from = first(d.getUTCFullYear(), d.getUTCMonth());
    iw.to = `${iw.from.slice(0, 7)}-${String(lastDay(d.getUTCFullYear(), d.getUTCMonth())).padStart(2, '0')}`;
  } else if (preset === 'last-month') {
    const m = d.getUTCMonth() === 0 ? 11 : d.getUTCMonth() - 1;
    const y = d.getUTCMonth() === 0 ? d.getUTCFullYear() - 1 : d.getUTCFullYear();
    iw.from = first(y, m);
    iw.to = `${iw.from.slice(0, 7)}-${String(lastDay(y, m)).padStart(2, '0')}`;
  } else if (preset === 'this-week') {
    iw.from = mondayOf(today);
    iw.to = addDays(iw.from, 6);
  } else {
    return; // custom: inputs keep whatever the user typed
  }
  $('iw-from').value = iw.from;
  $('iw-to').value = iw.to;
}

export async function iwLoadProjects() {
  iw.from = $('iw-from').value;
  iw.to = $('iw-to').value;
  const box = $('iw-projects');
  box.textContent = '';
  const cid = $('iw-customer').value;
  if (!cid || !iw.from || !iw.to || iw.from > iw.to) {
    box.appendChild(el('span', { cls: 'p-form-help-text', text: 'Pick a valid period first.' }));
    return;
  }
  const [cust, entries, invoices] = await Promise.all([
    api.get(`/customers/${cid}`),
    // #334: the period's entries may be several pages; the unbilled totals the
    // wizard offers must not depend on which page came first.
    fetchAll(`/entries?from=${iw.from}&to=${iw.to}`, 'entries'),
    api.get('/invoices'),
  ]);
  const billed = new Set();
  for (const inv of invoices.invoices || []) {
    if (inv.status !== 'draft') for (const l of inv.lines || []) if (l.entry_id) billed.add(l.entry_id);
  }
  const unbilledBy = new Map(); // project -> hundredths
  for (const e of entries.rows) {
    if (e.customer_id !== cid || !e.billable || e.imported?.origin.invoiced || billed.has(e.id) || !e.project_code) continue;
    const k = e.project_code;
    unbilledBy.set(k, (unbilledBy.get(k) || 0) + Math.round(Number(e.hours) * 100));
  }
  const projects = (await api.get(`/customers/${cid}/projects`)).projects || [];
  let shown = 0;
  for (const p of projects) {
    if (p.active === false) continue;
    const hours = unbilledBy.get(p.code);
    if (!hours) continue;
    shown += 1;
    // #232 phase 2b: the generated project tick-box adopts the official
    // p-form__group / p-form__label / p-form__control vocabulary, like every
    // static field in web/index.html.
    const cb = el('input', { cls: 'p-form__control', attrs: { type: 'checkbox', id: `iw-p-${p.code}`, value: p.code } });
    box.appendChild(
      el('div', { cls: 'p-form__group' }, [
        el('label', { cls: 'p-form__label', attrs: { for: cb.id } }, [
          cb,
          el('span', { text: `${p.code} — ${p.name} · ${(hours / 100).toFixed(2)} h unbilled` }),
        ]),
      ]),
    );
  }
  if (!shown) box.appendChild(el('span', { cls: 'p-form-help-text', text: 'No uninvoiced billable work in this period.' }));
}

export function iwShow() {
  for (const n of [1, 2, 3]) $(`iw-step${n}`).hidden = n !== iw.step;
  if (iw.step !== 3) releaseIwPreview();
  const titles = ['customer and source', 'period and projects', 'review and save'];
  $('iw-title').textContent = `Create from tracked work — ${iw.step} of 3: ${titles[iw.step - 1]}`;
  $('iw-back').hidden = iw.step === 1;
  $('iw-next').hidden = iw.step === 3;
  $('iw-save').hidden = iw.step !== 3;
  clearFormError($('iw-error'));
}

// #349: the wizard is a `p-modal` dialog opened on demand by `+ Invoice` rather
// than an inline card that used to render open at boot. The #134 flow is a
// multi-step form whose project tick-boxes (#iw-projects), the #iw-error alert
// and its aria-live status line are all written into the same fixed DOM tree by
// iwInit()/iwShow()/iwLoadProjects(), so every id, label and announcement is
// unchanged and the 1-of-3 sequence (POST /invoices/preview → POST /invoices)
// is untouched. Opening preserves `iw.step` and the entered values: pressing
// `+ Invoice` while the dialog is already open — or after a close — only shows
// it again and re-focuses the current step's first control, so half-entered work
// is never discarded.
export function iwOpen() {
  const dlg = $('invoice-wizard');
  const alreadyOpen = dlg.open === true || dlg.hasAttribute('open');
  // #232 increment 4: the native <dialog> owns the top layer, the focus trap and
  // the return of focus to the opener; jsdom has no modal support, so fall back
  // to the [open] attribute exactly as edOpen() does.
  if (!alreadyOpen) {
    if (typeof dlg.showModal === 'function') dlg.showModal();
    else dlg.setAttribute('open', '');
  }
  const first = iw.step === 2 ? $('iw-preset') : iw.step === 3 ? $('iw-save') : $('iw-customer');
  first.focus();
  if (iw.step === 3) iwRefreshPreview().catch?.(() => {});
  announce(`Invoice wizard open — step ${iw.step} of 3.`);
}

// Closing the wizard is not a cancel: the steps keep their state, so a later
// `+ Invoice` resumes exactly where the user left off and nothing half-entered
// is thrown away — nothing was persisted at all, so the #143 "no partial
// persistence" rule holds. The native <dialog> returns focus to #invoice-new via
// dom.js initDialogFocusReturn. Silent on the live region on purpose: there is no
// state change to announce, and the successful save below must be free to
// announce the draft it created.
export function iwClose() {
  releaseIwPreview();
  const dlg = $('invoice-wizard');
  if (typeof dlg.close === 'function') dlg.close();
  else dlg.removeAttribute('open');
}

export function iwSelectedProjects() {
  return [...$('iw-projects').querySelectorAll('input:checked')].map((c) => c.value);
}

export async function iwNext() {
  if (iw.step === 1) {
    if (!$('iw-customer').value) {
      showFormError($('iw-error'), { message: 'Pick a customer.' });
      return;
    }
    iw.step = 2;
    iwShow();
    await iwLoadProjects();
    return;
  }
  if (iw.step === 2) {
    const picks = iwSelectedProjects();
    if (!$('iw-from').value || !$('iw-to').value) {
      showFormError($('iw-error'), { message: 'Enter the period.' });
      return;
    }
    iw.from = $('iw-from').value;
    iw.to = $('iw-to').value;
    try {
      iw.preview = await api.post('/invoices/preview', {
        customer_id: $('iw-customer').value,
        from: iw.from,
        to: iw.to,
        include_expenses: $('iw-expenses').checked,
        project_codes: picks,
        line_grouping: iwGrouping(),
      });
      iwRenderReview();
      iw.step = 3;
      iwShow();
      iwRefreshPreview().catch?.(() => {});
    } catch (err) {
      showFormError($('iw-error'), err);
    }
  }
}

export function iwRenderReview() {
  const d = iw.preview;
  if (!d) return;
  const grouping = iwGrouping();
  const dl = $('iw-summary');
  dl.textContent = '';
  const sum = (label, v) => {
    dl.appendChild(el('dt', { text: label }));
    dl.appendChild(el('dd', { text: v }));
  };
  sum('Customer', customerName(d.customer_id));
  sum('Period', `${d.period_from} → ${d.period_to}`);
  sum('Currency', d.currency);
  sum('Lines', String((d.lines || []).length));
  sum('Total', `${formatMoney(d.total_minor)} ${d.currency}`);
  const ul = $('iw-lines');
  ul.textContent = '';
  const groups = new Map();
  for (const l of d.lines || []) {
    const key = grouping === 'single'
      ? 'All time entries'
      : grouping === 'project'
        ? (l.project_code || 'general')
        : grouping === 'task'
          ? `${l.project_code || 'general'}${l.task_code ? ` / ${l.task_code}` : ''}`
          : `${l.date} ${l.project_code || ''} ${l.note || l.kind}`;
    if (!groups.has(key)) groups.set(key, { h: 0, amount: 0, n: 0 });
    const g = groups.get(key);
    g.h += l.hours || 0;
    g.amount += l.amount_minor;
    g.n += 1;
  }
  for (const [key, g] of groups) {
    const detail = grouping === 'line'
      ? `${g.n} line(s)`
      : `${(g.h / 100).toFixed(2)} h · ${g.n} line(s)`;
    ul.appendChild(el('li', { text: `${key}: ${detail} — ${formatMoney(g.amount)} ${d.currency}` }));
  }
}

export async function iwSave() {
  // QA-01: a double click on Save draft must not create two invoices from the
  // same tracked work.
  return singleFlight($('iw-save'), iwSaveBody);
}

async function iwSaveBody() {
  const picks = iwSelectedProjects();
  try {
    const created = await api.post('/invoices', {
      customer_id: $('iw-customer').value,
      from: iw.from,
      to: iw.to,
      include_expenses: $('iw-expenses').checked,
      project_codes: picks,
      line_grouping: iwGrouping(),
    });
    iw.step = 1;
    iw.preview = null;
    iwShow();
    iwClose();
    await refreshInvoices();
    // Saving returns to the invoice register instead of opening the editor:
    // the draft is listed there and can be previewed or issued from its row.
    announce(`Draft ${created.number} created from ${created.lines.length} line(s) — preview it, then Issue to lock and document it.`);
  } catch (err) {
    showFormError($('iw-error'), err);
  }
}

export function iwInit() {
  $('iw-back').addEventListener('click', () => {
    iw.step = Math.max(1, iw.step - 1);
    iwShow();
    if (iw.step === 2) iwLoadProjects().catch?.(() => { });
  });
  $('iw-next').addEventListener('click', () => iwNext().catch?.(() => { }));
  $('iw-save').addEventListener('click', iwSave);
  $('iw-preset').addEventListener('change', () => {
    iwPresetRange($('iw-preset').value);
    iwLoadProjects().catch?.(() => { });
  });
  $('iw-from').addEventListener('change', () => iwLoadProjects().catch?.(() => { }));
  $('iw-to').addEventListener('change', () => iwLoadProjects().catch?.(() => { }));
  $('iw-grouping').addEventListener('change', () => {
    iwRenderReview();
    iwRefreshPreview().catch?.(() => {});
  });
  $('iw-expenses').addEventListener('change', () => {
    if (iw.step === 3) iwRefreshPreview().catch?.(() => {});
  });
  iwPresetRange('this-month');
  iwShow();
}

// ------------------------------------------------ product/service catalog --

export let catalogItems = [];

export async function refreshCatalog() {
  try {
    catalogItems = (await api.get('/admin/item-types')).item_types || [];
  } catch {
    catalogItems = [];
    return; // members: section is hidden anyway
  }
  const tbody = $('item-table').querySelector('tbody');
  tbody.textContent = '';
  for (const it of catalogItems) {
    const actions = [
      el('button', {
        type: 'button', cls: 'link', text: 'Edit', data: { icon: 'edit' },
        on: {
          click: () => {
            $('item-id').value = it.id;
            $('item-name').value = it.name;
            $('item-desc').value = it.description || '';
            $('item-price').value = (it.default_price_minor / 100).toFixed(2);
            $('item-currency').value = it.currency;
            $('item-save').textContent = 'Update item';
            $('item-cancel').hidden = false;
          },
        },
      }),
      el('button', {
        type: 'button', cls: 'link', text: it.active ? 'Archive' : 'Restore',
        on: {
          click: async () => {
            try {
              await api.put(`/admin/item-types/${it.id}`, {
                name: it.name, kind: it.kind, description: it.description || '',
                default_price_minor: it.default_price_minor, currency: it.currency,
                active: !it.active,
              });
              announce(it.active ? 'Item archived.' : 'Item restored.');
              await refreshCatalog();
            } catch (err) {
              announce(`Catalog change failed: ${err.message}`);
            }
          },
        },
      }),
      el('button', {
        type: 'button', cls: 'danger', text: 'Delete', data: { icon: 'delete' },
        on: {
          click: async () => {
            if (!(await askConfirm(`Delete catalog item "${it.name}"? Existing invoice lines keep their snapshot.`))) return;
            try {
              await api.del(`/admin/item-types/${it.id}`);
              announce('Item deleted.');
              await refreshCatalog();
            } catch (err) {
              announce(`Delete failed: ${err.message}`);
            }
          },
        },
      }),
    ];
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' }, text: it.name }),
        el('td', { cls: 'num', attrs: { 'data-heading': 'Default price' }, text: `${it.currency} ${formatMoney(it.default_price_minor)}` }),
        el('td', { attrs: { 'data-heading': 'Status' } }, [statusChip(it.active ? 'active' : 'archived', it.active)]),
        el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, actions),
      ]),
    );
  }
  $('item-table').hidden = catalogItems.length === 0;
  $('item-empty').hidden = catalogItems.length !== 0;
  for (const row of $('ed-lines').querySelectorAll('tbody tr')) {
    row.__attachItemSelect?.();
  }
}

export async function saveCatalogItem(evt) {
  evt.preventDefault();
  // QA-01: one catalog item per click.
  return singleFlight($('item-save'), saveCatalogItemBody);
}

async function saveCatalogItemBody() {
  clearFormError($('item-error'));
  const id = $('item-id').value;
  const body = {
    name: $('item-name').value.trim(),
    kind: catalogItems.find(item => item.id === id)?.kind || 'service',
    description: $('item-desc').value.trim(),
    default_price_minor: parseHundredths($('item-price').value) || 0,
    currency: $('item-currency').value.trim().toUpperCase() || null,
    active: true,
  };
  try {
    if (id) await api.put(`/admin/item-types/${id}`, body);
    else await api.post('/admin/item-types', body);
    announce(id ? 'Catalog item updated.' : 'Catalog item added.');
    $('item-id').value = '';
    $('item-form').reset();
    $('item-price').value = '0';
    $('item-currency').value = 'EUR';
    $('item-save').textContent = 'Save item';
    $('item-cancel').hidden = true;
    await refreshCatalog();
  } catch (err) {
    showFormError($('item-error'), err);
  }
}

// ------------------------------------------------------------ retainers ----
//
// #144: retainer balances over the Invoices tab. The GUI never shows a
// stored balance — the server returns balance_minor reconciled from the
// ledger, and the history view renders that same ledger. Add/draw/create
// carry idempotency keys, so a double click can never double-post money.

export async function refreshRetainers() {
  let rows = [];
  try {
    rows = (await api.get('/retainers')).retainers || [];
  } catch {
    return; // members: admin surface
  }
  const tbody = $('ret-table').querySelector('tbody');
  tbody.textContent = '';
  for (const r of rows) {
    const actions = [
      el('button', {
        type: 'button', cls: 'link', text: 'History',
        on: { click: () => retShow(r.id) }
      }),
    ];
    if (r.status === 'open') {
      actions.push(
        el('button', {
          type: 'button', cls: 'link', text: 'Add funds',
          on: { click: () => retAdd(r) }
        }),
        el('button', {
          type: 'button', cls: 'link', text: 'Draw funds',
          on: { click: () => retDraw(r) }
        }),
        el('button', {
          type: 'button', cls: 'danger', text: 'Close',
          on: { click: () => retClose(r) }
        }),
      );
    }
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' }, text: `${customerName(r.customer_id)} / ${r.project_code}` }),
        el('td', { cls: 'num', attrs: { 'data-heading': 'Balance' }, text: `${r.currency} ${formatMoney(Math.max(r.balance_minor, 0))}` }),
        el('td', { attrs: { 'data-heading': 'Status' } }, [statusChip(r.status, r.status === 'open')]),
        el('td', { attrs: { 'data-heading': 'Last activity' }, text: r.last_activity ? String(r.last_activity).slice(0, 10) : '\u2014' }),
        el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, actions),
      ]),
    );
  }
  $('ret-table').hidden = rows.length === 0;
  $('ret-empty').hidden = rows.length !== 0;
}

export async function retAdd(r) {
  const amt = await askPrompt(`Add funds in ${r.currency} (e.g. 250.00):`, '');
  if (!amt) return;
  const minor = parseHundredths(amt);
  if (!Number.isFinite(minor) || minor <= 0) {
    announce('Enter a positive amount.');
    return;
  }
  try {
    const out = await api.post(`/retainers/${r.id}/credit`, {
      amount_minor: minor, reason: '', idempotency_key: crypto.randomUUID(),
    });
    announce(`Retainer credited. Balance: ${r.currency} ${formatMoney(out.balance_minor)}.`);
    await refreshRetainers();
  } catch (err) {
    announce(`Add funds failed: ${err.message}`);
  }
}

export async function retDraw(r) {
  const amt = await askPrompt(`Draw funds from ${r.currency} ${formatMoney(r.balance_minor)} balance:`, '');
  if (!amt) return;
  const minor = parseHundredths(amt);
  if (!Number.isFinite(minor) || minor <= 0) {
    announce('Enter a positive amount.');
    return;
  }
  const reason = ((await askPrompt('Draw reason (required):')) || '').trim();
  if (!reason) {
    announce('Draw cancelled — a reason is required.');
    return;
  }
  try {
    await api.post(`/retainers/${r.id}/draw`, {
      amount_minor: minor, reason, idempotency_key: crypto.randomUUID(),
    });
    announce('Draw recorded against the retainer (this is not an invoice payment).');
    await refreshRetainers();
  } catch (err) {
    announce(`Draw failed: ${err.message}`);
  }
}

export async function retClose(r) {
  if (!(await askConfirm(`Close the retainer for ${customerName(r.customer_id)} / ${r.project_code}? History stays readable; funds can no longer be added or drawn.`))) return;
  try {
    await api.post(`/retainers/${r.id}`, {});
    announce('Retainer closed.');
    await refreshRetainers();
  } catch (err) {
    announce(`Close failed: ${err.message}`);
  }
}

export async function retShow(id) {
  try {
    const d = await api.get(`/retainers/${id}`);
    const ret = d.retainer;
    $('ret-detail').hidden = false;
    $('ret-detail-title').textContent =
      `Ledger \u2014 ${customerName(ret.customer_id)} / ${ret.project_code} (${ret.currency} ${formatMoney(Math.max(d.balance_minor, 0))})`;
    const ul = $('ret-history');
    ul.textContent = '';
    for (const t of ret.transactions) {
      const sign = t.kind === 'draw' ? '\u2212' : '+';
      ul.appendChild(el('li', {
        text: `${String(t.created_at).slice(0, 10)} \u2014 ${t.kind} ${sign}${formatMoney(t.amount_minor)}${t.reason ? ` \u00b7 ${t.reason}` : ''}`,
      }));
    }
    if (!ul.children.length) ul.appendChild(el('li', { cls: 'p-form-help-text', text: 'No transactions yet.' }));
    announce('Retainer ledger shown.');
  } catch (err) {
    announce(`History failed: ${err.message}`);
  }
}

export async function createRetainer(evt) {
  evt.preventDefault();
  // QA-01: one retainer per click (creation is not keyed by the customer and
  // project, so a second POST would create a second record).
  return singleFlight($('ret-add'), createRetainerBody);
}

async function createRetainerBody() {
  clearFormError($('ret-error'));
  const opening = parseHundredths($('ret-opening').value || '0');
  try {
    await api.post('/retainers', {
      customer_id: $('ret-customer').value,
      project_code: $('ret-project').value,
      opening_amount_minor: Number.isFinite(opening) ? opening : 0,
      idempotency_key: crypto.randomUUID(),
    });
    announce('Retainer created.');
    $('ret-opening').value = '0';
    await refreshRetainers();
  } catch (err) {
    showFormError($('ret-error'), err);
  }
}

// ------------------------------------------------- recurring schedules ----
//
// #136: management UI for the Phase-4 recurring engine (#26). The backend
// already generates drafts for ACTIVE schedules; this surface lists them and
// pauses/resumes/deletes without touching the billing cursor
// (last_period_end), so a resumed schedule continues where it stopped.

export async function refreshSchedules() {
  let schedules = [];
  try {
    schedules = (await api.get('/schedules')).schedules || [];
  } catch {
    return; // members cannot see this admin surface
  }
  const tbody = $('rec-table').querySelector('tbody');
  tbody.textContent = '';
  const action = rowAction; // #189: shared builder
  for (const scd of schedules) {
    const active = scd.active !== false;
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' }, text: customerName(scd.customer_id) }),
        el('td', { attrs: { 'data-heading': 'Cadence' }, text: scd.cadence }),
        el('td', { attrs: { 'data-heading': 'Bills' }, text: scd.mode === 'retainer' ? 'Fixed retainer' : 'Time + expenses' }),
        el('td', {
          cls: 'num',
          attrs: { 'data-heading': 'Retainer' },
          text: scd.mode === 'retainer' ? `${scd.currency} ${formatMoney(scd.retainer_amount_minor)}` : '\u2014',
        }),
        el('td', { attrs: { 'data-heading': 'Last billed to' }, text: scd.last_period_end || 'never' }),
        el('td', { attrs: { 'data-heading': 'Status' } }, [
          statusChip(active ? 'active' : 'paused', active),
        ]),
        el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, [
          action('link', active ? 'Pause' : 'Resume', () => toggleSchedule(scd),
            active ? 'Pause generation (drafts stop until resumed)' : 'Resume generation'),
          action('danger', 'Delete', () => removeSchedule(scd), 'Delete the schedule'),
        ]),
      ]),
    );
  }
  $('rec-table').hidden = schedules.length === 0;
  $('rec-empty').hidden = schedules.length !== 0;
}

export async function addRecurring(evt) {
  evt.preventDefault();
  // QA-01: one schedule per click.
  return singleFlight($('rec-add'), addRecurringBody);
}

async function addRecurringBody() {
  clearFormError($('rec-error'));
  const cid = $('rec-customer').value;
  const c = state.customers.find((x) => x.id === cid);
  const body = {
    customer_id: cid,
    cadence: $('rec-cadence').value,
    mode: $('rec-mode').value,
    retainer_amount_minor: Math.round(Number($('rec-amount').value || 0) * 100),
    currency: c ? c.currency : 'EUR',
    active: true,
  };
  try {
    await api.post('/schedules', body);
    announce('Recurring schedule added.');
    $('rec-amount').value = '0';
    await refreshSchedules();
  } catch (err) {
    showFormError($('rec-error'), err);
  }
}

export async function toggleSchedule(scd) {
  try {
    const next = await api.put(`/schedules/${scd.id}`, { active: scd.active === false });
    announce(next.active ? 'Schedule resumed.' : 'Schedule paused.');
    await refreshSchedules();
  } catch (err) {
    announce(`Schedule change failed: ${err.message}`);
  }
}

export async function removeSchedule(scd) {
  if (!(await askConfirm(`Delete the ${scd.cadence} schedule for ${customerName(scd.customer_id)}? Already-generated drafts are unaffected.`))) return;
  try {
    await api.del(`/schedules/${scd.id}`);
    announce('Schedule deleted.');
    await refreshSchedules();
  } catch (err) {
    announce(`Delete failed: ${err.message}`);
  }
}

