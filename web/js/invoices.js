// invoices.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { catalogItems } from './billing.js';
import { startApp } from './boot.js';
import { $, announce, askConfirm, askPrompt, clearFormError, el, formatMoney, rowAction, showFormError, statusChip } from './dom.js';
import { createSeqGuard } from './seq.js';
import { customerName, state } from './state.js';
import { invalidateLockCache, weekLockIds } from './timesheet.js';

const invoiceSeq = createSeqGuard();

// -------------------------------------------------------------- invoices ---


export async function refreshInvoices() {
  // #188: GET /invoices is admin-tier (src/lib.rs). Previously an un-caught
  // 403 rejection aborted member boot (Promise.all in startApp), so members
  // never got the page title, timer wiring or version label.
  if (!state.isAdmin) return;
  const isInvoiceCurrent = invoiceSeq();
  const data = await api.get('/invoices');
  if (!isInvoiceCurrent()) return; // a newer refresh superseded this one
  const invoices = dashFilter(data.invoices || []); // #135 filters + sort
  const tbody = $('invoice-table').querySelector('tbody');
  tbody.textContent = '';
  const action = rowAction; // #189: shared builder
  for (const inv of invoices) {
    const actions = [];
    // Balance semantics (#114): legacy paid docs without a ledger count as
    // fully settled; a written-off balance is forgiven, not outstanding.
    const balance = invBalance(inv); // #189: shared semantic
    const open = isOpenInvoice(inv);
    if (inv.status === 'draft') {
      actions.push(
        action('link', 'Edit', () => api.get(`/invoices/${inv.id}`).then(edOpen, (e) => announce(e.message)),
          'Edit the draft lines and percentages (#143)'),
        action('link', 'Issue', () => issueInvoice(inv)),
        action('danger', 'Delete', () => removeInvoice(inv.id)),
      );
    } else if (open) {
      actions.push(
        action('link', 'Record payment', () => recordPayment(inv, balance),
          'Record a full or partial payment (partly paid keeps the rest open)'),

        action('link', 'PDF', () => downloadInvoicePdf(inv.id, inv.number),
          'Download the archived invoice PDF'),
        action('link', 'Email', () => emailInvoice(inv.id),
          'Send this invoice (PDF attached) to the customer billing email'),
        action('link', 'Email copy', () => emailInvoiceCopy(inv.id),
          'Send a PDF copy to another address, e.g. the accountant'),
        action('danger', 'Write off', () => writeOffInvoice(inv, balance),
          'Forgive the remaining balance (final; excluded from outstanding)'),
      );
    } else if (inv.status === 'paid') {
      actions.push(
        action('link', 'PDF', () => downloadInvoicePdf(inv.id, inv.number),
          'Download the archived invoice PDF'),
        action('link', 'Email', () => emailInvoice(inv.id),
          'Send this invoice (PDF attached) to the customer billing email'),
        action('link', 'Email copy', () => emailInvoiceCopy(inv.id),
          'Send a PDF copy to another address, e.g. the accountant'),
      );
    }
    // #133: selecting the row (its Preview button, a click anywhere on the
    // row, or Enter with the row focused) opens the live document preview.
    actions.unshift(
      action('link', 'Preview', (evt) => openInvoicePreview(inv, evt.currentTarget),
        'Preview this invoice document'),
    );
    tbody.appendChild(
      el('tr', {
        attrs: { tabindex: '0', 'aria-label': `Invoice ${inv.number} — activate to preview` },
        on: {
          click: (e) => {
            if (!e.target.closest('button')) openInvoicePreview(inv, null);
          },
          keydown: (e) => {
            if (e.key === 'Enter') openInvoicePreview(inv, e.currentTarget);
          },
        },
      }, [
        el('th', { attrs: { scope: 'row' }, text: inv.number }),
        el('td', { text: customerName(inv.customer_id) }),
        el('td', { text: `${inv.period_from} → ${inv.period_to}` }),
        el('td', { cls: 'num', text: `${inv.currency} ${formatMoney(inv.total_minor)}` }),
        el('td', { cls: 'num', text: open ? `${inv.currency} ${formatMoney(balance)}` : '\u2014' }),
        el('td', {}, [
          statusChip(inv.status.replace('_', ' '), open),
        ]),
        el('td', { cls: 'actions-col' }, actions),
      ]),
    );
  }
  $('invoice-empty').hidden = invoices.length !== 0;
  $('invoice-empty').textContent = (data.invoices || []).length && !invoices.length
    ? 'No invoices match the current filters.'
    : 'No invoices yet.';
  dashApplyColumns(); // #135 column visibility
  if (invoicePreview) {
    const fresh = invoices.find((i) => i.id === invoicePreview.id);
    if (fresh) {
      const opener = invoicePreview.opener;
      openInvoicePreview({ ...fresh }, opener && document.contains(opener) ? opener : null);
    } else {
      closeInvoicePreview();
    }
  }
  await refreshInvoiceSummary(data.invoices || []); // #189: reuse the list we just fetched
}

export async function refreshInvoiceSummary(preloaded) {
  const s = await api.get('/invoices/summary');
  const outstanding = Object.entries(s.outstanding || {})
    .map(([cur, minor]) => `${cur} ${formatMoney(minor)}`)
    .join(', ');
  $('invoice-summary').textContent =
    `Draft ${s.draft} · Issued ${s.issued}${s.overdue ? ` (${s.overdue} overdue)` : ''}` +
    (s.partly_paid ? ` · Partly paid ${s.partly_paid}` : '') +
    ` · Paid ${s.paid}` +
    (s.written_off ? ` · Written off ${s.written_off}` : '') +
    (outstanding ? ` · Outstanding: ${outstanding}` : '');
  // #135 overview band: tiles + monthly chart (balances & ledger are the truth)
  // #189: the invoice list is passed through from refreshInvoices (and from
  // the chained call at its foot) instead of a third GET /invoices per
  // filter change; standalone callers still fetch once.
  try {
    const all = preloaded ?? (await api.get('/invoices')).invoices ?? [];
    if (all.length && !dash.currency) dash.currency = all.find((i) => i.status !== 'draft')?.currency || '';
    dashRenderOverview(all);
    // project filter options follow the customer filter
    const projSel = $('inv-f-project');
    const keep = projSel.value;
    projSel.textContent = '';
    projSel.appendChild(el('option', { attrs: { value: '' }, text: 'All projects' }));
    const seen = new Set();
    for (const i of all) {
      if (dash.customer && i.customer_id !== dash.customer) continue;
      for (const l of i.lines || []) {
        if (l.project_code && !seen.has(l.project_code)) {
          seen.add(l.project_code);
          projSel.appendChild(el('option', { attrs: { value: l.project_code }, text: l.project_code }));
        }
      }
    }
    if (keep) projSel.value = keep;
  } catch { /* overview is additive; table remains authoritative */ }
}

// Record a full or partial payment (#114). The amount is prefilled with the
// outstanding balance; paying the exact balance settles the invoice.
export async function recordPayment(inv, balance) {
  const amount = await askPrompt(
    `Amount to record (${inv.currency} ${formatMoney(balance)} outstanding):`,
    formatMoney(balance),
  );
  if (amount === null) return; // dialog dismissed
  const trimmed = String(amount).trim();
  if (!trimmed) {
    announce('Record payment cancelled.');
    return;
  }
  const minor = Math.round(Number(trimmed) * 100);
  if (!Number.isFinite(minor) || minor <= 0) {
    announce('Enter a positive amount, e.g. 150.00.');
    return;
  }
  const reference = (await askPrompt('Payment reference (optional):')) || '';
  try {
    const paid = await api.post(`/invoices/${inv.id}/pay`, { reference, amount_minor: minor });
    invalidateLockCache(); // full settlement releases the invoice's entry locks
    if (paid.status === 'paid') {
      announce('Invoice marked paid.');
    } else {
      const rest = invBalance(paid); // #189: shared semantic
      announce(`Payment recorded. ${paid.currency} ${formatMoney(rest)} still outstanding.`);
    }
    await refreshInvoices();
  } catch (err) {
    announce(err.message);
  }
}

// Forgive the remaining balance (#114): the reason is required and the
// state is final, so a prompt + confirm gate the action.
export async function writeOffInvoice(inv, balance) {
  const reason = await askPrompt(
    `Write off ${inv.currency} ${formatMoney(balance)} of ${inv.number} — reason (required):`,
  );
  const r = String(reason ?? '').trim();
  if (!r) {
    announce('Write off cancelled.');
    return;
  }
  if (!(await askConfirm(`Write off ${inv.number}? The balance stops counting as outstanding.`))) return;
  try {
    await api.post(`/invoices/${inv.id}/write-off`, { reason: r });
    // #188: a write-off leaves `is_open()` (src/lock.rs locks only open
    // invoices), so it RELEASES the entry locks — the same #108 cache rule
    // that issue/pay/submit/decide follow applies here.
    invalidateLockCache();
    announce(`${inv.number} written off.`);
    await refreshInvoices();
  } catch (err) {
    announce(err.message);
  }
}

export async function generateInvoice(evt) {
  evt.preventDefault();
  clearFormError($('invoice-error'));
  try {
    await api.post('/invoices', {
      customer_id: $('invoice-customer').value,
      from: $('invoice-from').value,
      to: $('invoice-to').value,
    });
    announce('Draft invoice generated.');
    await refreshInvoices();
  } catch (err) {
    showFormError($('invoice-error'), err);
  }
}

// Same resolution order as the server (#116): customer terms → org template
// terms → legacy net-14 from the period end. Only the confirm text; the
// server always owns the persisted value.
export async function dueDatePreview(inv) {
  try {
    const c = state.customers.find((x) => x.id === inv.customer_id);
    let terms = c && c.payment_terms ? c.payment_terms : null;
    if (!terms) {
      const t = await api.get('/admin/invoice-template');
      terms = (t.template || {}).payment_terms || null;
    }
    const fixed = { upon_receipt: 0, net_15: 15, net_20: 20, net_30: 30, net_45: 45 };
    const d = new Date();
    if (terms) {
      const days = terms.kind === 'custom' ? Number(terms.days) : fixed[terms.kind];
      if (!Number.isFinite(days)) return null;
      d.setTime(d.getTime() + days * 86400000);
      return { date: d.toISOString().slice(0, 10), label: terms.kind };
    }
    d.setTime(new Date(`${inv.period_to}T00:00:00Z`).getTime() + 14 * 86400000);
    return { date: d.toISOString().slice(0, 10), label: 'legacy net-14' };
  } catch {
    return null; // terms are a hint only; never block the action
  }
}

export async function issueInvoice(inv) {
  const preview = await dueDatePreview(inv);
  const due = preview
    ? ` It will be due ${preview.date} (${preview.label}).`
    : '';
  if (!(await askConfirm(`Issue this invoice?${due} Its entries will be locked from editing.`))) return;
  const id = inv.id;
  try {
    await api.post(`/invoices/${id}/issue`);
    invalidateLockCache(); // issuing locks its entries
    announce('Invoice issued.');
    await refreshInvoices();
  } catch (err) {
    announce(err.message);
  }
}

// One phrasing for a delivered/queued send (#130): "disabled" means the
// transport is a logged no-op — the UI must not claim the customer got it.
export function emailOutcome(res, base) {
  return res.transport === 'smtp'
    ? base
    : `NOT sent (SMTP not configured) — would go to ${res.sent_to}. Set smtp.host in Settings.`;
}

export async function emailInvoice(id) {
  try {
    const res = await api.post(`/invoices/${id}/email`);
    announce(emailOutcome(res, `Invoice emailed to ${res.sent_to} (PDF attached).`));
  } catch (err) {
    announce(`Email failed: ${err.message}`);
  }
}

// Sends a PDF copy of the invoice to any address — e.g. the accountant
// (#112). Recipient via the inline dialog (#102); the server validates and
// refuses drafts/invalid shapes before sending anything.
export async function emailInvoiceCopy(id) {
  const to = ((await askPrompt('Send a PDF copy to this email address:')) || '').trim();
  if (!to) {
    announce('Email copy cancelled.');
    return;
  }
  try {
    const res = await api.post(`/invoices/${id}/email-copy`, { to });
    announce(emailOutcome(res, `PDF copy sent to ${res.sent_to}.`));
  } catch (err) {
    announce(`Email copy failed: ${err.message}`);
  }
}

// Download the archived invoice PDF (#113). Fetch the binary as a blob (so
// the session cookie is sent and errors are surfaced), then hand it to a
// transient <a download>. Announcement goes through aria-live via announce().
export async function downloadInvoicePdf(id, number) {
  announce('Preparing PDF…');
  try {
    const blob = await api.getBlob(`/invoices/${id}/pdf`);
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${number}.pdf`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    // Some browsers cancel a download if the URL is revoked synchronously.
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    announce(`Invoice PDF downloaded (${blob.size} bytes).`);
  } catch (err) {
    announce(`Download failed: ${err.message}`);
  }
}

// -------------------------------------------------- invoice dashboard (#135) --
//
// Overview band (open/paid tiles, monthly Open-vs-Paid chart with year
// navigation) + Open/All lists with search, client/project/date filters,
// sortable columns and column visibility. Everything derives from the
// existing GET /invoices + ledger semantics (#114 balances) — no records
// are ever mutated by filtering/sorting; row selection still opens the
// #133 preview.

export const dash = {
  year: new Date().getUTCFullYear(),
  openOnly: true,
  q: '',
  customer: '',
  project: '',
  from: '',
  to: '',
  sortKey: 'number',
  sortDir: 1,
  currency: '',
};

// The server locks entry edits/deletes while an invoice is OPEN —
// InvoiceStatus::is_open() == issued | partly_paid (src/domain.rs); settled
// and written-off invoices release their entries (#114 lifecycle, #216).
// #217: one predicate, previously four ad-hoc copies that had drifted —
// weekLockIds checked only 'issued', so partly-paid invoices showed their
// entries as editable while the server answered every save with 409.
export function isOpenInvoice(inv) {
  return inv.status === 'issued' || inv.status === 'partly_paid';
}

export function invBalance(inv) {
  const paid = (inv.payments || []).reduce((a, p) => a + p.amount_minor, 0);
  if (inv.status === 'paid' || inv.status === 'written_off') return 0;
  return Math.max(inv.total_minor - paid, 0);
}

export function dashFilter(invoices) {
  let rows = invoices;
  if (dash.openOnly) rows = rows.filter(isOpenInvoice);
  const q = dash.q.trim().toLowerCase();
  if (q) {
    rows = rows.filter(
      (i) =>
        i.number.toLowerCase().includes(q) ||
        customerName(i.customer_id).toLowerCase().includes(q) ||
        (i.lines || []).some((l) => (l.note || '').toLowerCase().includes(q)),
    );
  }
  if (dash.customer) rows = rows.filter((i) => i.customer_id === dash.customer);
  if (dash.project) rows = rows.filter((i) => (i.lines || []).some((l) => l.project_code === dash.project));
  if (dash.from) rows = rows.filter((i) => i.period_to >= dash.from);
  if (dash.to) rows = rows.filter((i) => i.period_from <= dash.to);
  const key = {
    number: (i) => i.number,
    customer: (i) => customerName(i.customer_id),
    period: (i) => i.period_to,
    total: (i) => i.total_minor,
    balance: (i) => invBalance(i),
    status: (i) => i.status,
  }[dash.sortKey] || ((i) => i.number);
  return [...rows].sort((a, b) => {
    const x = key(a), y = key(b);
    const cmp = typeof x === 'number' ? x - y : String(x).localeCompare(String(y));
    return cmp * dash.sortDir;
  });
}

export function dashChartMonths(invoices) {
  const open = Array(12).fill(0);
  const paid = Array(12).fill(0);
  for (const i of invoices) {
    const cy = i.period_to.slice(0, 4) === String(dash.year);
    if (cy && isOpenInvoice(i)) {
      open[Number(i.period_to.slice(5, 7)) - 1] += invBalance(i);
    }
    for (const p of i.payments || []) {
      const m = String(p.received_at).slice(0, 7);
      if (m.slice(0, 4) === String(dash.year)) paid[Number(m.slice(5, 7)) - 1] += p.amount_minor;
    }
    if (!i.payments || i.payments.length === 0) {
      const pm = String(i.paid_at || '').slice(0, 7);
      if (i.status === 'paid' && pm.slice(0, 4) === String(dash.year)) {
        paid[Number(pm.slice(5, 7)) - 1] += i.total_minor;
      }
    }
  }
  return { open, paid };
}

export function dashRenderOverview(invoices) {
  const cur = dash.currency || invoices.find((i) => i.status !== 'draft')?.currency || 'EUR';
  const mine = invoices.filter((i) => i.currency === cur);
  const totalOpen = mine.reduce((a, i) => a + invBalance(i), 0);
  const months = dashChartMonths(mine);
  const totalPaid = months.paid.reduce((a, b) => a + b, 0);
  const multi = new Set(invoices.filter((i) => i.status !== 'draft').map((i) => i.currency));
  const openByCur = {};
  for (const i of invoices) {
    if (isOpenInvoice(i)) {
      openByCur[i.currency] = (openByCur[i.currency] || 0) + invBalance(i);
    }
  }
  const tileOpen = multi.size > 1
    ? Object.entries(openByCur).map(([c, v]) => `${c} ${formatMoney(v)}`).join(' · ')
    : `${cur} ${formatMoney(totalOpen)}`;
  $('inv-tile-open').textContent = tileOpen;
  $('inv-tile-paid').textContent = `${cur} ${formatMoney(totalPaid)}`;
  $('inv-year-label').textContent = String(dash.year);
  $('inv-chart-title').textContent = `Open vs paid (${cur}) — ${dash.year}`;
  const max = Math.max(...months.open, ...months.paid, 1);
  const chart = $('inv-chart');
  chart.textContent = '';
  const names = ['J', 'F', 'M', 'A', 'M', 'J', 'J', 'A', 'S', 'O', 'N', 'D'];
  const labels = [];
  const bars = [];
  for (let m = 0; m < 12; m += 1) {
    const col = el('div', { cls: 'inv-col-chart' }, [
      el('div', { cls: 'inv-bars' }, [
        el('span', {
          cls: 'inv-bar open',
          style: `height:${Math.max(2, Math.round((months.open[m] / max) * 54))}px`,
          attrs: { title: `Open ${cur} ${formatMoney(months.open[m])}` },
        }),
        el('span', {
          cls: 'inv-bar paid',
          style: `height:${Math.max(2, Math.round((months.paid[m] / max) * 54))}px`,
          attrs: { title: `Paid ${cur} ${formatMoney(months.paid[m])}` },
        }),
      ]),
    ]);
    col.appendChild(el('span', { cls: 'inv-clabel', text: names[m] }));
    bars.push(col);
    labels.push(`${names[m]}: open ${formatMoney(months.open[m])}, paid ${formatMoney(months.paid[m])}`);
  }
  // #191: children of a role="img" node are presentational to AT, so the
  // per-month summary must ride the accessible NAME, not the subtree.
  chart.setAttribute('aria-label', `Monthly open versus paid amounts, ${dash.year}. ${labels.join('; ')}`);
  for (const b of bars) chart.appendChild(b);
}

export function dashApplyColumns() {
  const on = new Set([...document.querySelectorAll('.inv-col:checked')].map((c) => c.value));
  const table = $('invoice-table');
  const idx = { customer: 1, period: 2, total: 3, balance: 4, status: 5 };
  for (const [name, i] of Object.entries(idx)) {
    const hide = !on.has(name);
    table.querySelectorAll('tr').forEach((row) => {
      const cell = row.children[i];
      if (cell) cell.hidden = hide;
    });
  }
}

// ------------------------------------------------ invoice document preview --
//
// #133: selecting an invoice row opens a live preview built from
// GET /invoices/{id}/document (#116). The returned HTML is reduced to text
// through the DOM (childNodes' textContent) — markup is NEVER injected, the
// invariant holds; the full-format document remains the Download PDF action
// (#113). Drafts have no archived PDF yet: the panel says so honestly.

export let invoicePreview = null; // { id, inv, opener }

export function invoiceBalance(inv) {
  // #189: one balance semantic (the four copies had already drifted on the
  // `|| 0` default). invBalance owns the rule; this stays as the alias the
  // preview path uses.
  return invBalance(inv);
}

export async function openInvoicePreview(inv, opener) {
  invoicePreview = { id: inv.id, inv, opener: opener || null };
  const card = $('invoice-preview');
  card.hidden = false;
  const facts = $('ip-facts');
  facts.textContent = '';
  const dl = (label, value) => {
    facts.appendChild(el('dt', { text: label }));
    facts.appendChild(el('dd', { text: value }));
  };
  dl('Number', inv.number);
  dl('Customer', customerName(inv.customer_id));
  dl('Period', `${inv.period_from} → ${inv.period_to}`);
  dl('Status', inv.status.replace(/_/g, ' '));
  dl('Total', `${inv.currency} ${formatMoney(inv.total_minor)}`);
  const bal = invoiceBalance(inv);
  dl('Balance', bal ? `${inv.currency} ${formatMoney(bal)}` : 'Settled');
  if (inv.due_date) dl('Due', inv.due_date);
  if (inv.write_off_reason) dl('Written off', inv.write_off_reason);
  $('ip-subject').textContent = '';
  $('ip-letter').textContent = '';
  const draft = inv.status === 'draft';
  $('ip-download').hidden = draft;
  $('ip-pdf-state').hidden = !draft;
  if (draft) {
    $('ip-pdf-state').textContent =
      'Draft — issuing the invoice archives its PDF document. Nothing is locked or sent yet.';
  } else {
    try {
      const doc = await api.get(`/invoices/${inv.id}/document`);
      if (!invoicePreview || invoicePreview.id !== inv.id) return; // superseded
      $('ip-subject').textContent = doc.subject || '';
      const parsed = new DOMParser().parseFromString(doc.html || '', 'text/html');
      $('ip-letter').textContent = [...parsed.body.childNodes]
        .map((n) => (n.textContent || '').trim())
        .filter(Boolean)
        .join('\n');
    } catch {
      $('ip-letter').textContent = 'Preview unavailable — the document could not be rendered.';
    }
  }
  card.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  announce(`Previewing invoice ${inv.number}.`);
}

export function closeInvoicePreview() {
  if (!invoicePreview) return;
  const { opener } = invoicePreview;
  invoicePreview = null;
  $('invoice-preview').hidden = true;
  if (opener && document.contains(opener)) opener.focus();
}

export async function removeInvoice(id) {
  if (!(await askConfirm('Delete this draft invoice?'))) return;
  try {
    await api.del(`/invoices/${id}`);
    await refreshInvoices();
    announce('Invoice deleted.');
  } catch (err) {
    announce(err.message);
  }
}

// --------------------------------------------------- invoice editor (#143) --
//
// Manual Product/Service lines and draft editing. Money stays integer:
// decimal inputs are parsed to hundredths/minor units WITHOUT floats (string
// math), amounts and totals mirror the server's exact formulas. Tracked
// lines from an existing draft show read-only and round-trip verbatim —
// the #8 rate snapshots are untouchable, and saving a draft never issues,
// emails, locks or syncs.

export const ed = { id: null, tracked: [], currency: '' };

// '12.34' -> 1234 (hundredths) without ever touching a float.
export function parseHundredths(str) {
  const s = String(str).trim();
  if (!s) return 0;
  const neg = s.startsWith('-');
  const body = neg ? s.slice(1) : s;
  const [i, f = ''] = body.split('.');
  const ff = (f + '00').slice(0, 2);
  const n = Number(i || 0) * 100 + Number(ff);
  if (!Number.isFinite(n) || n < 0) return NaN;
  return neg ? -n : n;
}

export const edAmount = (qtyH, priceMinor) => Math.floor((qtyH * priceMinor + 50) / 100);
export const edPercent = (v, hundredths) => Math.floor((v * hundredths + 5000) / 10000);

export function edTotals(subtotal, taxH, discH) {
  const discount = edPercent(subtotal, discH);
  const net = subtotal - discount;
  const tax = edPercent(net, taxH);
  return { subtotal, discount, tax, total: net + tax };
}

export function edLineRow(line = null) {
  const desc = el('input', { attrs: { type: 'text', maxlength: 500, placeholder: 'Description', 'aria-label': 'Line description', value: line ? line.note : '' } });
  const itemSel = el('select', { attrs: { 'aria-label': 'Catalog item type (prefills this line)' } });
  const kind = el('select', { attrs: { 'aria-label': 'Product or service' } }, [
    el('option', { attrs: { value: 'product', selected: !line || line.item_kind !== 'service' ? '' : null }, text: 'Product' }),
    el('option', { attrs: { value: 'service' }, text: 'Service' }),
  ]);
  if (line && line.item_kind === 'service') kind.value = 'service';
  const qty = el('input', {
    cls: 'num',
    attrs: { type: 'number', min: '0.01', step: '0.01', inputmode: 'decimal', style: 'width:7ch', 'aria-label': 'Quantity',
      value: line && line.quantity_hundredths ? (line.quantity_hundredths / 100).toFixed(2) : '1.00' },
  });
  const price = el('input', {
    cls: 'num',
    attrs: { type: 'number', min: '0', step: '0.01', inputmode: 'decimal', style: 'width:9ch', 'aria-label': 'Unit price',
      value: line && line.unit_price_minor != null ? (line.unit_price_minor / 100).toFixed(2) : '0.00' },
  });
  const amount = el('td', { cls: 'num' });
  const row = el('tr', {}, [
    el('td', {}, [itemSel, desc]),
    el('td', {}, [kind]),
    el('td', {}, [qty]),
    el('td', {}, [price]),
    amount,
    el('td', { cls: 'actions-col' }, [
      el('button', { type: 'button', cls: 'link danger', text: 'Remove', on: { click: () => { row.remove(); edRecalc(); } } }),
    ]),
  ]);
  const read = () => {
    const q = parseHundredths(qty.value);
    const p = parseHundredths(price.value);
    return {
      description: desc.value.trim(),
      item_kind: kind.value,
      quantity_hundredths: q,
      unit_price_minor: p,
      amount_minor: Number.isFinite(q) && Number.isFinite(p) ? edAmount(q, p) : NaN,
    };
  };
  row.__attachItemSelect = () => {
    const keep = itemSel.value;
    itemSel.textContent = '';
    itemSel.appendChild(el('option', { attrs: { value: '' }, text: '— custom —' }));
    for (const it of catalogItems.filter((x) => x.active)) {
      itemSel.appendChild(
        el('option', { attrs: { value: it.id }, text: `${it.name} (${it.currency} ${formatMoney(it.default_price_minor)})` }),
      );
    }
    if (keep) itemSel.value = keep;
  };
  row.__attachItemSelect();
  itemSel.addEventListener('change', () => {
    const it = catalogItems.find((x) => x.id === itemSel.value);
    if (!it) return;
    // Prefill only — every field stays editable (#147).
    if (!desc.value.trim()) desc.value = it.description || it.name;
    kind.value = it.kind;
    if (!Number(price.value)) price.value = (it.default_price_minor / 100).toFixed(2);
    edRecalc();
  });
  row.__read = read;
  row.__render = () => {
    const r = read();
    amount.textContent = Number.isFinite(r.amount_minor) ? formatMoney(r.amount_minor) : '—';
  };
  for (const inp of [desc, kind, qty, price]) inp.addEventListener('input', edRecalc);
  return row;
}

export function edTrackedRow(line) {
  return el('tr', {}, [
    el('td', { text: line.note || (line.project_code || '') }),
    el('td', { text: 'tracked' }),
    el('td', { cls: 'num', text: line.hours ? (line.hours / 100).toFixed(2) : '1.00' }),
    el('td', { cls: 'num', text: line.rate_minor != null ? formatMoney(line.rate_minor) : formatMoney(line.amount_minor) }),
    el('td', { cls: 'num', text: formatMoney(line.amount_minor) }),
    el('td', {}),
  ]);
}

export function edRecalc() {
  const tbody = $('ed-lines').querySelector('tbody');
  for (const row of tbody.querySelectorAll('tr')) row.__render?.();
  const manual = [...tbody.querySelectorAll('tr')].map((r) => r.__read?.()).filter(Boolean);
  const trackedSub = ed.tracked.reduce((a, l) => a + l.amount_minor, 0);
  const subtotal = trackedSub + manual.reduce((a, l) => a + (Number.isFinite(l.amount_minor) ? l.amount_minor : 0), 0);
  const t = edTotals(
    subtotal,
    parseHundredths($('ed-tax').value),
    parseHundredths($('ed-discount').value),
  );
  const cur = ed.currency || 'EUR';
  const dl = $('ed-totals');
  dl.textContent = '';
  for (const [label, v] of [['Subtotal', t.subtotal], ['Discount', -t.discount], ['VAT', t.tax], ['Total', t.total]]) {
    dl.appendChild(el('dt', { text: label }));
    dl.appendChild(el('dd', { cls: 'num', text: `${v < 0 ? '-' : ''}${formatMoney(Math.abs(v))} ${cur}` }));
  }
}

export function edOpen(draft) {
  ed.id = draft ? draft.id : null;
  ed.tracked = draft ? (draft.lines || []).filter((l) => l.entry_id || l.expense_id) : [];
  ed.currency = draft ? draft.currency : (state.customers.find((c) => c.id === $('invoice-customer').value)?.currency || 'EUR');
  $('ed-title').textContent = draft ? `Edit draft ${draft.number}` : 'Manual invoice';
  $('ed-customer').disabled = Boolean(draft);
  if (!draft) $('ed-customer').value = $('invoice-customer').value || $('ed-customer').value;
  const tbody = $('ed-lines').querySelector('tbody');
  tbody.textContent = '';
  for (const l of ed.tracked) tbody.appendChild(edTrackedRow(l));
  const manuals = draft ? (draft.lines || []).filter((l) => !l.entry_id && !l.expense_id) : [];
  if (manuals.length === 0 && !draft) tbody.appendChild(edLineRow());
  for (const l of manuals) tbody.appendChild(edLineRow(l));
  $('ed-tax').value = draft ? (draft.tax_hundredths / 100).toFixed(2) : '0';
  $('ed-discount').value = draft ? (draft.discount_hundredths / 100).toFixed(2) : '0';
  clearFormError($('ed-error'));
  $('invoice-editor').hidden = false;
  edRecalc();
  $('invoice-editor').scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  $('ed-lines').querySelector('tbody input')?.focus();
  announce(draft ? `Editing draft ${draft.number}.` : 'Manual invoice editor open.');
}

export function edClose() {
  ed.id = null;
  $('invoice-editor').hidden = true;
}

export async function edSave() {
  clearFormError($('ed-error'));
  const manual = [...$('ed-lines').querySelectorAll('tbody tr')]
    .map((r) => r.__read?.())
    .filter(Boolean);
  const lines = manual.map((m) => ({
    description: m.description, item_kind: m.item_kind,
    quantity_hundredths: m.quantity_hundredths, unit_price_minor: m.unit_price_minor,
  }));
  const taxH = parseHundredths($('ed-tax').value);
  const discH = parseHundredths($('ed-discount').value);
  if (!Number.isFinite(taxH) || !Number.isFinite(discH) || lines.some((l) => !Number.isFinite(l.quantity_hundredths) && l.quantity_hundredths !== 0)) {
    showFormError($('ed-error'), { message: 'Enter valid numbers in every line.' });
    return;
  }
  try {
    if (!ed.id) {
      const created = await api.post('/invoices/manual', {
        customer_id: $('ed-customer').value,
        tax_hundredths: taxH,
        discount_hundredths: discH,
        lines,
      });
      announce(`Draft ${created.number} saved (total ${formatMoney(created.total_minor)} ${created.currency}).`);
    } else {
      const payloadLines = [
        ...ed.tracked,
        ...manual.map((m) => ({
          kind: 'fixed',
          date: new Date().toISOString().slice(0, 10),
          note: m.description,
          item_kind: m.item_kind,
          quantity_hundredths: m.quantity_hundredths,
          unit_price_minor: m.unit_price_minor,
          amount_minor: m.amount_minor,
          entry_id: null, expense_id: null, project_code: null, task_code: null,
          hours: null, rate_minor: null,
        })),
      ];
      const updated = await api.put(`/invoices/${ed.id}`, {
        lines: payloadLines, tax_hundredths: taxH, discount_hundredths: discH,
      });
      announce(`Draft ${updated.number} updated (total ${formatMoney(updated.total_minor)} ${updated.currency}).`);
    }
    edClose();
    await refreshInvoices();
  } catch (err) {
    showFormError($('ed-error'), err);
  }
}

