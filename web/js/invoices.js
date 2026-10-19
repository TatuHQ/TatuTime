// invoices.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { catalogItems } from './billing.js';
import { startApp, switchTab } from './boot.js';
import { $, announce, askConfirm, askPrompt, clearFormError, el, formatMoney, panelRefresh, rowAction, showFormError, singleFlight, statusChip } from './dom.js';
import { convertMinor, currencyCode, fetchRateQuote } from './fx.js';
import { applyRoute, normaliseRoute } from './route.js';
import { createSeqGuard } from './seq.js';
import { customerName, state } from './state.js';
import { MONTH_NAMES, invalidateLockCache } from './timesheet.js';

const invoiceSeq = createSeqGuard();

// The Settings view that owns SMTP configuration. Kept in ONE place so the
// recipient dialog's "Open Settings" lands on the Email section.
export const EMAIL_SETTINGS_ROUTE = '/settings/email';

// -------------------------------------------------------------- invoices ---


export function refreshInvoices() {
  // QA-10: a failed load keeps the last invoice rows and shows the visible
  // banner with Retry instead of leaving the register looking current.
  return panelRefresh('panel-invoices', 'the invoice register', refreshInvoicesBody);
}

async function refreshInvoicesBody() {
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
          'Edit the draft lines and percentages'),
        action('link', 'Issue', () => issueInvoice(inv)),
        action('danger', 'Delete', () => removeInvoice(inv.id)),
      );
    } else if (open) {
      actions.push(
        action('link', 'Record payment', () => recordPayment(inv, balance),
          'Record a full or partial payment (partly paid keeps the rest open)'),

        action('link', 'PDF', () => downloadInvoicePdf(inv.id, inv.number),
          'Download the archived invoice PDF'),
        action('link', 'Email', (evt) => emailInvoice(inv, evt.currentTarget),
          'Choose recipients and send this invoice (PDF attached)'),
        action('link', 'Email copy', () => emailInvoiceCopy(inv.id),
          'Send a PDF copy to another address, e.g. the accountant'),
        action('danger', 'Write off', () => writeOffInvoice(inv, balance),
          'Forgive the remaining balance (final; excluded from outstanding)'),
      );
    } else if (inv.status === 'paid') {
      actions.push(
        action('link', 'PDF', () => downloadInvoicePdf(inv.id, inv.number),
          'Download the archived invoice PDF'),
        action('link', 'Email', (evt) => emailInvoice(inv, evt.currentTarget),
          'Choose recipients and send this invoice (PDF attached)'),
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
    // The frozen subject (#354): snapshot at issue, display only. It rides
    // under the invoice number as a quiet subtitle, never as a new column and
    // never as editable text.
    const subject = String(inv.subject || '').trim();
    tbody.appendChild(
      el('tr', {
        attrs: {
          tabindex: '0',
          'aria-label': `Invoice ${inv.number}${subject ? ` — ${subject}` : ''} — activate to preview`,
        },
        on: {
          click: (e) => {
            if (!e.target.closest('button')) openInvoicePreview(inv, null);
          },
          keydown: (e) => {
            if (e.key === 'Enter') openInvoicePreview(inv, e.currentTarget);
          },
        },
      }, [
        el('th', { attrs: { scope: 'row' } }, [
          el('span', { text: inv.number }),
          subject ? el('span', { cls: 'inv-row-subject', text: subject }) : null,
        ]),
        el('td', { text: customerName(inv.customer_id) }),
        el('td', { text: `${inv.period_from} → ${inv.period_to}` }),
        el('td', { cls: 'num', text: `${inv.currency} ${formatMoney(inv.total_minor)}` }),
        el('td', { cls: 'num', text: open ? `${inv.currency} ${formatMoney(balance)}` : '\u2014' }),
        el('td', {}, [
          // The chip's second argument is the emphasis flag, and `open` here is
          // isOpenInvoice (collectable now): sent and partly paid read positive
          // while draft, paid and written off stay neutral. The label is the
          // ratified lifecycle vocabulary (#349) -- presentation only.
          statusChip(statusLabel(inv.status), open),
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
      // #349: awaited so callers that act on the refreshed panel (the delivery
      // actions) see the new render and history, not the pre-action state.
      await openInvoicePreview({ ...fresh }, opener && document.contains(opener) ? opener : null);
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
  // The count band speaks the same UK lifecycle vocabulary as the register
  // chips (#359): the persisted `issued` status reads "Sent", so the band and
  // the chips can never disagree.
  $('invoice-summary').textContent =
    `Draft ${s.draft} · Sent ${s.issued}${s.overdue ? ` (${s.overdue} overdue)` : ''}` +
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

// ------------------------------------------- invoice email recipients ---
//
// #139 follow-up: choosing who receives an invoice. Both delivery entry points
// -- the detail panel's `Send email` and the register's Email row action --
// open this dialog instead of posting immediately, so the admin sees which
// addresses are on file, can tick more than one and can add one that is not.
//
// The SMTP probe is advisory: when the transport is not configured the server
// would only log the message, so the dialog says so, disables Send and links
// to Settings rather than pretending a delivery happened.
const sendEmail = {
  invoice: null,
  opener: null,
  defaultRecipient: '',
  configured: true,
};

/// Addresses known for a customer, billing first. The flagged billing contact
/// wins; without one the legacy `email` is the default. Duplicates collapse
/// case-insensitively onto the first spelling.
export function emailCandidates(customer) {
  const seen = new Set();
  const list = [];
  const add = (email, label, checked) => {
    const address = String(email || '').trim();
    const key = address.toLowerCase();
    if (!address || seen.has(key)) return;
    seen.add(key);
    list.push({ email: address, label, checked });
  };
  const contactLabel = (contact, billing) => {
    const name = String(contact.name || '').trim() || contact.email;
    const role = String(contact.role || '').trim();
    return `${name}${role ? ` — ${role}` : ''}${billing ? ' (billing)' : ''}`;
  };
  const contacts = (customer?.contacts || []).filter((c) => c && String(c.email || '').trim());
  // At most one contact is flagged billing (domain::validate_customer_input).
  const billing = contacts.find((c) => c.billing) || null;
  if (billing) add(billing.email, contactLabel(billing, true), true);
  else if (String(customer?.email || '').trim()) add(customer.email, 'Billing email', true);
  for (const contact of contacts) {
    if (contact !== billing) add(contact.email, contactLabel(contact, contact.billing), false);
  }
  // The legacy top-level address stays selectable even when a contact is
  // flagged, unless it is the same address the contact already contributed.
  if (String(customer?.email || '').trim()) add(customer.email, 'Customer email', false);
  return list;
}

/// Split the comma-separated "Add another email" field. Every entry must be
/// non-empty, carry exactly one `@` and no whitespace or control characters;
/// the first invalid entry names itself in the returned error.
export function parseExtraEmails(raw) {
  const parts = String(raw || '').split(',').map((s) => s.trim()).filter(Boolean);
  const emails = [];
  for (const address of parts) {
    const [local, domain, ...rest] = address.split('@');
    const invalid = !local || !domain || rest.length > 0
      || /\s/.test(address)
      || /[\u0000-\u001f\u007f]/.test(address)
      || address.length > 320;
    if (invalid) return { emails: [], error: `“${address}” is not a valid email address.` };
    emails.push(address);
  }
  return { emails, error: '' };
}

// Case-insensitive de-duplication: first spelling and first order win, the
// same rule the server applies to the list it receives.
function uniqueRecipients(list) {
  const seen = new Set();
  const out = [];
  for (const address of list) {
    const key = String(address).trim().toLowerCase();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    out.push(String(address).trim());
  }
  return out;
}

function renderSendEmailCandidates(candidates) {
  const host = $('send-email-recipients');
  host.textContent = '';
  candidates.forEach((candidate, index) => {
    const id = `send-email-r-${index}`;
    const box = el('input', {
      attrs: { type: 'checkbox', id, value: candidate.email },
      on: { change: () => clearFormError($('send-email-error')) },
    });
    box.checked = candidate.checked;
    // The label owns the box (`for`/`id`), and the address itself is the
    // supporting text so the admin never ticks an address they cannot see.
    host.appendChild(el('label', { attrs: { for: id } }, [
      box,
      el('span', {}, [
        el('span', { text: candidate.label }),
        el('span', { cls: 'send-email-contact-meta', text: ` — ${candidate.email}` }),
      ]),
    ]));
  });
  $('send-email-empty').hidden = candidates.length !== 0;
}

async function resolveCustomer(customerId) {
  const known = state.customers.find((c) => c.id === customerId);
  if (known) return known;
  try {
    return await api.get(`/customers/${customerId}`);
  } catch {
    return null; // the dialog still opens: the extra-address field works
  }
}

/// Open the recipient dialog for `inv`. `opener` is the control focus returns
/// to on close; the register passes its row button, the panel passes `#ip-send`.
export async function openSendEmailDialog(inv, opener = null) {
  const dlg = $('send-email-dialog');
  if (!dlg || !inv) return;
  sendEmail.invoice = inv;
  sendEmail.opener = opener || (document.activeElement !== document.body ? document.activeElement : null);
  clearFormError($('send-email-error'));
  $('send-email-result').hidden = true;
  $('send-email-result').textContent = '';
  $('send-email-notice').hidden = true;
  $('send-email-extra').value = '';
  $('send-email-settings').dataset.route = EMAIL_SETTINGS_ROUTE;
  $('send-email-invoice').textContent = `${inv.number || 'Draft'} — ${customerName(inv.customer_id)}`;
  const customer = await resolveCustomer(inv.customer_id);
  const candidates = emailCandidates(customer);
  sendEmail.defaultRecipient = (candidates.find((c) => c.checked) || {}).email || '';
  renderSendEmailCandidates(candidates);
  if (!(dlg.open === true || dlg.hasAttribute('open'))) {
    if (typeof dlg.showModal === 'function') dlg.showModal();
    else dlg.setAttribute('open', '');
  }
  // Focus moves into the dialog: the first checkbox, or Send when the customer
  // has no address on file.
  const first = $('send-email-recipients').querySelector('input[type="checkbox"]');
  (first || $('send-email-send') || $('send-email-close')).focus();
  refreshSendEmailNotice();
}

async function refreshSendEmailNotice() {
  let settings = null;
  try {
    settings = await api.get('/admin/email');
  } catch (err) {
    // Advice only for the fail-closed vault answer (503); any other failure
    // leaves Send usable, where the send itself reports the problem.
    if (err?.status !== 503) return;
    settings = { configured: false };
  }
  if (!sendEmail.invoice) return; // closed while the probe was in flight
  const configured = !settings || settings.configured !== false;
  sendEmail.configured = configured;
  $('send-email-notice').hidden = configured;
  $('send-email-send').disabled = !configured;
  if (configured) $('send-email-send').removeAttribute('aria-disabled');
  else $('send-email-send').setAttribute('aria-disabled', 'true');
}

export function closeSendEmailDialog() {
  const dlg = $('send-email-dialog');
  if (!dlg) return;
  const opener = sendEmail.opener;
  sendEmail.invoice = null;
  sendEmail.opener = null;
  $('send-email-send').disabled = false;
  $('send-email-send').removeAttribute('aria-disabled');
  $('send-email-error').hidden = true;
  $('send-email-error').textContent = '';
  if (dlg.open === true || dlg.hasAttribute('open')) {
    if (typeof dlg.close === 'function') dlg.close();
    else dlg.removeAttribute('open');
  }
  if (opener && opener.isConnected && typeof opener.focus === 'function') opener.focus();
}

/// `Open Settings` from the SMTP notice. One route constant decides the target.
export function openEmailSettings() {
  closeSendEmailDialog();
  const route = normaliseRoute(EMAIL_SETTINGS_ROUTE) || EMAIL_SETTINGS_ROUTE;
  if (!applyRoute(route)) announce('Settings are not available for this account.');
}

/// Send to the checked addresses plus the typed extras. A single, untouched
/// default selection omits the body entirely, which keeps the historical
/// server-side billing-address default (and is byte-for-byte the old request).
export async function confirmSendEmail() {
  const inv = sendEmail.invoice;
  if (!inv) return;
  clearFormError($('send-email-error'));
  const extra = parseExtraEmails($('send-email-extra').value);
  if (extra.error) {
    showFormError($('send-email-error'), new Error(extra.error));
    return;
  }
  const checked = [...$('send-email-recipients').querySelectorAll('input[type="checkbox"]:checked')]
    .map((box) => box.value);
  const recipients = uniqueRecipients([...checked, ...extra.emails]);
  if (!recipients.length) {
    showFormError($('send-email-error'), new Error('Choose at least one recipient.'));
    return;
  }
  if (recipients.length > 10) {
    showFormError($('send-email-error'), new Error('At most 10 recipients can be emailed at once.'));
    return;
  }
  const onlyDefault = recipients.length === 1
    && extra.emails.length === 0
    && sendEmail.defaultRecipient !== ''
    && recipients[0].toLowerCase() === sendEmail.defaultRecipient.toLowerCase();
  let res;
  try {
    res = await singleFlight($('send-email-send'), () => api.post(
      `/invoices/${inv.id}/email`,
      onlyDefault ? undefined : { recipients },
    ));
  } catch (err) {
    // 409 draft, 422 validation/no billing email and 502 transport failures all
    // render inline in the dialog's existing error shape; nothing is closed.
    showFormError($('send-email-error'), err);
    return;
  }
  if (!res) return; // a second click while the first request was in flight
  const delivered = Array.isArray(res.recipients) && res.recipients.length
    ? res.recipients.join(', ')
    : res.sent_to;
  const message = emailOutcome(res, `Invoice emailed to ${delivered} (PDF attached).`);
  $('send-email-result').textContent = message;
  $('send-email-result').hidden = false;
  announce(message);
  if (res.transport !== 'smtp') {
    // The race the transport contract allows: the probe said configured but
    // this send was a logged no-op. Keep the settings link in reach.
    $('send-email-notice').hidden = false;
    $('send-email-send').disabled = true;
  }
  // #349: the delivery updates the panel's history and watermark; re-render
  // behind the dialog instead of closing it under the admin.
  refreshInvoices().catch(() => { });
}

/// The register's Email row action: choose recipients before sending.
export function emailInvoice(inv, opener = null) {
  return openSendEmailDialog(inv, opener);
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
// The Trends chart (monthly Open-vs-Closed with year navigation) + Open/All
// lists with search, client/project/date filters, sortable columns and column
// visibility. Everything derives from the
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

// "Collectable right now": issued or partly paid — drives the Record-payment
// affordance, the balance column and which row actions appear. NOT the lock
// predicate: entry/expense locks are permanent for ANY non-draft invoice
// (settling no longer releases them — see weekLockIndex in timesheet.js and
// the InvoiceLock provider in src/lock.rs). NOT the Open register filter either
// — see isClosedInvoice below. #217 heritage: this one predicate replaced four
// ad-hoc copies that had drifted.
export function isOpenInvoice(inv) {
  return inv.status === 'issued' || inv.status === 'partly_paid';
}

// The ratified Open/Closed grouping (the owner's definition): an invoice is
// **Closed** once it is paid or written off and **Open** in every other state —
// draft, issued (sent) and partly paid. This is the single home of that rule,
// used by the register's Open filter and by the Trends chart's Closed series. It
// is deliberately NOT isOpenInvoice, which keeps the narrower "collectable now"
// meaning above: a draft is open for the register but is not collectable, so it
// never offers "Record payment".
export function isClosedInvoice(inv) {
  return inv.status === 'paid' || inv.status === 'written_off';
}

// #349: the register's Status chip presents the ratified lifecycle vocabulary.
// The owner names the state of a delivered invoice **Sent**, while the persisted
// status (and the API enum) stays `issued`; `partly_paid` keeps its own wording.
// This is a presentation map only -- nothing here is persisted or sent to the
// API -- and an unknown status still degrades to a readable word rather than an
// empty chip.
const STATUS_LABELS = {
  draft: 'Draft',
  issued: 'Sent',
  partly_paid: 'Partly paid',
  paid: 'Paid',
  written_off: 'Written off',
};

/** The Status chip's label for a persisted invoice status (#349). */
export function statusLabel(status) {
  const key = String(status);
  return STATUS_LABELS[key] ?? key.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase());
}

export function invBalance(inv) {
  const paid = (inv.payments || []).reduce((a, p) => a + p.amount_minor, 0);
  if (inv.status === 'paid' || inv.status === 'written_off') return 0;
  return Math.max(inv.total_minor - paid, 0);
}

export function dashFilter(invoices) {
  let rows = invoices;
  // The Open view means "not closed": draft | issued | partly_paid. A freshly
  // saved draft is therefore listed under the default filter.
  if (dash.openOnly) rows = rows.filter((inv) => !isClosedInvoice(inv));
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

// -------------------------------------------------------- trends chart (#F) --
//
// The chart is presentation only: the same balances and ledger entries the
// register shows, in minor units, and nothing here is ever written back. Three
// things #F fixes in the view:
//
//   * the month axis is a *name*, not a letter — three-letter codes that
//     collapse to one letter at narrow widths. The variant is chosen in CSS
//     (no re-render on resize) and the short one is `aria-hidden`, so no screen
//     reader is ever read the same month twice;
//   * hovering a bar answers the question the chart raises: which month, how
//     much open, how much paid;
//   * quantities in other currencies are no longer silently dropped. With a
//     `base_currency` configured (appconfig, #D) every currency is combined in
//     that unit and the rate used is printed under the chart; without one the
//     historical single-currency view stands, and it now *says* so.
//
// The second series is **Closed**, not "Paid" (the ratified Open/Closed
// grouping): it counts every invoice that has left the open set — paid AND
// written off — so the hover, the subtitle and the accessible name all say
// "Closed", while the persisted status enum (`paid`/`written_off`) is untouched.

// The three-letter axis table is the app's one month table (#189,
// `timesheet.js`), imported rather than copied; only the long names are new
// here, because the hover sentence says "March 2026", not "Mar 2026".
const MONTH_FULL = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];

const emptySeries = () => ({ open: Array(12).fill(0), closed: Array(12).fill(0) });

/// The open/closed month series for the selected year, **grouped by source
/// currency** (#F). Grouping first is what keeps the money honest once a
/// conversion is applied: sums are converted, never the individual invoices, so
/// rounding cannot accumulate across them (#11).
///
///   * `open` — the outstanding balance of every invoice still in the open set
///     (draft | issued | partly paid), bucketed by the invoice's period end, as
///     before.
///   * `closed` — every amount that has left that set this year: each payment by
///     its `received_at`, a legacy settled invoice (no ledger) by `paid_at`, and a
///     written-off invoice's **forgiven remainder** by `written_off_at`. Adding
///     only the remainder — never `total_minor` — is what stops a partly-paid
///     then written-off invoice from counting its received payment twice.
export function chartSeriesByCurrency(invoices) {
  const byCur = new Map();
  const bucket = (cur) => {
    if (!byCur.has(cur)) byCur.set(cur, emptySeries());
    return byCur.get(cur);
  };
  for (const i of invoices) {
    const series = bucket(i.currency || '');
    const cy = String(i.period_to).slice(0, 4) === String(dash.year);
    if (cy && !isClosedInvoice(i)) {
      series.open[Number(String(i.period_to).slice(5, 7)) - 1] += invBalance(i);
    }
    const paidMinor = (i.payments || []).reduce((a, p) => a + p.amount_minor, 0);
    for (const p of i.payments || []) {
      const m = String(p.received_at).slice(0, 7);
      if (m.slice(0, 4) === String(dash.year)) series.closed[Number(m.slice(5, 7)) - 1] += p.amount_minor;
    }
    if (!i.payments || i.payments.length === 0) {
      const pm = String(i.paid_at || '').slice(0, 7);
      if (i.status === 'paid' && pm.slice(0, 4) === String(dash.year)) {
        series.closed[Number(pm.slice(5, 7)) - 1] += i.total_minor;
      }
    }
    if (i.status === 'written_off') {
      // The domain stamps `written_off_at` on the write-off, so a forgiven
      // balance is bucketed by the month it was actually forgiven. A document
      // that somehow lacks the stamp (a legacy hand-edited file) falls back to
      // the invoice's period end — the same month the Open series already uses
      // for this invoice — rather than inventing a date.
      const wm = String(i.written_off_at || i.period_to).slice(0, 7);
      if (wm.slice(0, 4) === String(dash.year)) {
        series.closed[Number(wm.slice(5, 7)) - 1] += Math.max(i.total_minor - paidMinor, 0);
      }
    }
  }
  return byCur;
}

/// The base currency, read once per boot from the same `GET /admin/config` the
/// settings card reads (#D). A 403 (member), an absent entry and an explicit
/// `null` all mean the same thing — **no base currency** — so no conversion and
/// never an invented rate. Cached: the chart re-renders on every filter change.
let baseCurrencyCache; // undefined = not looked up yet
export async function baseCurrency() {
  if (baseCurrencyCache !== undefined) return baseCurrencyCache;
  try {
    const data = await api.get('/admin/config');
    const row = (data.config || []).find((c) => c.key === 'base_currency');
    baseCurrencyCache = currencyCode(row ? row.value : '');
  } catch {
    baseCurrencyCache = '';
  }
  return baseCurrencyCache;
}

/// `0.921400` → `0.9214`. The published rate is a decimal string and stays one:
/// only trailing zeros are trimmed for display, no float arithmetic.
function trimRate(rate) {
  const text = String(rate);
  return text.includes('.') ? text.replace(/0+$/, '').replace(/\.$/, '') : text;
}

/// `1 USD = 0.9214 EUR (2026-10-15)` — the rates every converted number on
/// screen was produced with, plus the reference date when the table carries one.
function rateCaption(quotes, base) {
  const parts = quotes.map((q) => `1 ${q.from} = ${trimRate(q.rate)} ${base}`);
  const dates = [...new Set(quotes.map((q) => q.date).filter(Boolean))].sort();
  return dates.length ? `${parts.join(' \u00b7 ')} (${dates.join(', ')})` : parts.join(' \u00b7 ');
}

/// Draws one 12-month, 2-bar-per-month series (24 bars, #135) and the single
/// line of context under it.
function drawChart({ series, unit, subtitle, note }) {
  $('inv-year-label').textContent = String(dash.year);
  $('inv-chart-title').textContent = subtitle;
  const max = Math.max(...series.open, ...series.closed, 1);
  const chart = $('inv-chart');
  chart.textContent = '';
  const labels = [];
  for (let m = 0; m < 12; m += 1) {
    // Both bars of a month carry the same sentence, so hovering either one
    // answers for the month: period, then open and closed in the chart's unit.
    const hover =
      `${MONTH_FULL[m]} ${dash.year} \u2014 Open ${unit} ${formatMoney(series.open[m])}` +
      ` \u00b7 Closed ${unit} ${formatMoney(series.closed[m])}`;
    const col = el('div', { cls: 'inv-col-chart' }, [
      el('div', { cls: 'inv-bars' }, [
        el('span', {
          cls: 'inv-bar open',
          style: `height:${Math.max(2, Math.round((series.open[m] / max) * 54))}px`,
          attrs: { title: hover },
        }),
        el('span', {
          cls: 'inv-bar closed',
          style: `height:${Math.max(2, Math.round((series.closed[m] / max) * 54))}px`,
          attrs: { title: hover },
        }),
      ]),
      // Full code by default, single letter on a narrow screen: two variants of
      // one label, toggled by a media query so the width is a CSS decision and
      // the short one is never announced (aria-hidden).
      el('span', { cls: 'inv-clabel' }, [
        el('span', { cls: 'inv-clabel-full', text: MONTH_NAMES[m] }),
        el('span', { cls: 'inv-clabel-short', attrs: { 'aria-hidden': 'true' }, text: MONTH_NAMES[m][0] }),
      ]),
    ]);
    chart.appendChild(col);
    labels.push(`${MONTH_NAMES[m]}: open ${formatMoney(series.open[m])}, closed ${formatMoney(series.closed[m])}`);
  }
  // #191: children of a role="img" node are presentational to AT, so the
  // per-month summary must ride the accessible NAME, not the subtree.
  chart.setAttribute('aria-label', `Monthly open versus closed amounts in ${unit}, ${dash.year}. ${labels.join('; ')}`);
  $('inv-chart-note').textContent = note;
}

/// Every currency combined into `base` (#F). Sums were converted once, above;
/// the caption names the rate. A rate that cannot be quoted leaves the
/// unconverted view standing with a note — never a fabricated 1:1 and never a
/// silent partial sum.
async function drawConverted(byCur, codes, base, primary) {
  const unit = byCur.has(primary) ? primary : codes[0];
  const unconverted = (note) => drawChart({
    series: byCur.get(unit) || emptySeries(),
    unit,
    subtitle: `Open vs closed (${unit}) \u2014 ${dash.year}`,
    note,
  });
  const quotes = [];
  for (const cur of codes) {
    if (cur === base) continue;
    try {
      quotes.push(await fetchRateQuote(cur, base));
    } catch {
      unconverted(`Exchange rates unavailable \u2014 showing ${unit} unconverted.`);
      return;
    }
  }
  const totals = emptySeries();
  for (const [cur, series] of byCur) {
    // The base unit needs no rate at all: its own amounts are already there.
    const rate = cur === base ? null : quotes.find((q) => q.from === cur)?.rate;
    for (let m = 0; m < 12; m += 1) {
      const open = rate === null ? series.open[m] : convertMinor(series.open[m], rate);
      const closed = rate === null ? series.closed[m] : convertMinor(series.closed[m], rate);
      if (!Number.isFinite(open) || !Number.isFinite(closed)) {
        unconverted(`Exchange rates unavailable \u2014 showing ${unit} unconverted.`);
        return;
      }
      totals.open[m] += open;
      totals.closed[m] += closed;
    }
  }
  drawChart({ series: totals, unit: base, subtitle: `Open vs closed (${base}) \u2014 ${dash.year}`, note: rateCaption(quotes, base) });
}

export function dashRenderOverview(invoices) {
  // #349: the Total open / Paid in tiles are retired, so the overview renders
  // the chart only. The year label lives in the promoted Trends header
  // (web/index.html) and is still driven from here, so the year navigation
  // (inv-year-prev / inv-year-next, web/js/boot.js) keeps working.
  const byCur = chartSeriesByCurrency(invoices);
  const codes = [...byCur.keys()].filter(Boolean);
  const primary = dash.currency || invoices.find((i) => i.status !== 'draft')?.currency || 'EUR';
  const unit = byCur.has(primary) ? primary : (codes[0] || primary);
  // First paint: the historical single-currency view. A mixed set is not
  // silently trimmed — the note names what is on screen and why.
  drawChart({
    series: byCur.get(unit) || emptySeries(),
    unit,
    subtitle: `Open vs closed (${unit}) \u2014 ${dash.year}`,
    note: codes.length > 1
      ? `Showing ${unit} only \u2014 set a base currency in Settings to combine ${codes.join(', ')}.`
      : '',
  });
  // Second paint (#F): with a base currency configured, re-draw in that unit.
  // The rates are read once per render — never once per bar — and a failure
  // leaves the first paint exactly as it is.
  baseCurrency()
    .then((base) => {
      if (!base || codes.length === 0) return null;
      if (codes.length === 1 && codes[0] === base) return null; // already the base unit
      return drawConverted(byCur, codes, base, primary);
    })
    .catch(() => { });
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

// ------------------------------------------- invoice detail panel (#349) --
//
// Selecting a register row opens the invoice detail view (#133): the invoice's
// *rendered document*, its activity history, and the two delivery actions.
//
// The document is the live render from GET /invoices/{id}/pdf/preview, fetched
// with the app's own credentialed request and framed from a client-side blob:
// URL. Framing the API path directly is refused by the browser -- the global
// X-Frame-Options: DENY is deliberate and stays -- while a blob URL is
// same-origin by construction, which is exactly what `frame-src 'self' blob:`
// already permits. Nothing the server returns is ever inserted as markup: the
// document is a binary body, and every string of the history enters the DOM
// through textContent / the el() builder.

export let invoicePreview = null; // { id, inv, opener }

/// The blob URL of the document currently framed, kept in one place so every
/// re-render and every close revokes exactly the URL it minted -- a
/// multi-hundred-kilobyte PDF must not stay alive in an object URL the panel
/// stopped showing (the rule the receipt preview follows, #332).
let previewPdfUrl = '';

/// The history the panel last rendered, so a delivery action can tell an
/// idempotent repeat (no new entry) from a first transition without a second
/// request.
let previewTimeline = [];

function releasePreviewPdfUrl() {
  if (previewPdfUrl) URL.revokeObjectURL(previewPdfUrl);
  previewPdfUrl = '';
}

/// Bumped by every render of the document region. The panel can be closed and
/// the *same* invoice reopened while an earlier fetch is still in flight, so the
/// invoice id alone cannot tell two renders apart -- only the newest render may
/// frame a document, report a failure, or own a blob URL.
let previewRenderSeq = 0;

/// True while `seq`'s render is still the one the panel wants: the newest render
/// *and* still the invoice on screen. Both halves are needed -- a close-and-
/// reopen of the same invoice leaves the id equal but the render superseded.
function previewRenderCurrent(seq, id) {
  return seq === previewRenderSeq && invoicePreview !== null && invoicePreview.id === id;
}

export function invoiceBalance(inv) {
  // #189: one balance semantic (the four copies had already drifted on the
  // `|| 0` default). invBalance owns the rule; this stays as the alias the
  // preview path uses.
  return invBalance(inv);
}

/// `on Oct 1, 2026 at 11:26 AM` -- the exact shape the history is read in.
/// The app has no shared timestamp formatter (calendar.js formats a calendar
/// event's time with `toLocaleTimeString` and nothing else), so the parts are
/// named explicitly with `Intl.DateTimeFormat` in en-US: a fixed, unambiguous
/// 12-hour form with the US month abbreviation, never the host locale's. The
/// machine-readable RFC 3339 value rides the `<time datetime>` attribute, so
/// nothing is lost to the display format.
export function formatTimelineStamp(at) {
  const d = new Date(at);
  if (Number.isNaN(d.getTime())) return String(at);
  const date = new Intl.DateTimeFormat('en-US', { month: 'short', day: 'numeric', year: 'numeric' }).format(d);
  const time = new Intl.DateTimeFormat('en-US', { hour: 'numeric', minute: '2-digit', hour12: true }).format(d);
  return `on ${date} at ${time}`;
}

/// A PDF preview must actually be a PDF: a proxy error page or a JSON body
/// returned under a 200 would otherwise paint a blank frame with no
/// explanation. The file signature is the first five bytes, `%PDF-`.
function isPdfBody(blob, bytes) {
  if (blob && typeof blob.type === 'string' && blob.type && blob.type !== 'application/pdf') return false;
  const sig = [0x25, 0x50, 0x44, 0x46, 0x2d]; // "%PDF-"
  if (bytes.length < sig.length) return false;
  return sig.every((b, i) => bytes[i] === b);
}

/// Fetch the live render and frame it. A failure surfaces as a sentence in
/// `#ip-pdf-state` -- never a blank frame, and never the response as markup.
async function renderPreviewPdf(inv) {
  const host = $('ip-pdf');
  const state = $('ip-pdf-state');
  const seq = ++previewRenderSeq;
  host.textContent = '';
  releasePreviewPdfUrl();
  const id = inv.id;
  try {
    const blob = await api.getBlob(`/invoices/${id}/pdf/preview`);
    if (!previewRenderCurrent(seq, id)) return; // superseded by a newer selection
    const bytes = new Uint8Array(await blob.arrayBuffer());
    if (!isPdfBody(blob, bytes)) {
      state.textContent = `Preview unavailable — the server did not return a PDF (${blob.type || 'no content type'}).`;
      state.hidden = false;
      return;
    }
    // Re-wrapped with an explicit PDF type so the browser's own viewer renders
    // it inline even when the response carried a generic content type.
    const url = URL.createObjectURL(new Blob([bytes], { type: 'application/pdf' }));
    // The `arrayBuffer()` await above is a real suspension point, so a newer
    // render (or a close and reopen of the same invoice) can have started since
    // the guard. The URL just minted is ours alone and was never installed, so
    // revoke it here rather than overwrite: a superseded render must revoke
    // exactly the URL it minted, never the newer render's -- overwriting that
    // one is what used to pin a few hundred KB until the next render or close.
    if (!previewRenderCurrent(seq, id)) {
      URL.revokeObjectURL(url);
      return;
    }
    previewPdfUrl = url;
    host.appendChild(el('iframe', {
      cls: 'ip-frame',
      attrs: { id: 'ip-frame', src: previewPdfUrl, title: `Invoice ${inv.number} PDF preview` },
    }));
    state.hidden = true;
    state.textContent = '';
  } catch (err) {
    if (!previewRenderCurrent(seq, id)) return;
    state.textContent = `Preview unavailable — ${err.message}`;
    state.hidden = false;
  }
}

/// One history entry: the description, the actor's display name and the human
/// timestamp, plus `View message` on a delivery. Every value is plain text.
function timelineEntry(ev) {
  const item = el('li', { cls: 'ip-tl-item' }, [
    el('p', { cls: 'ip-tl-desc', text: ev.description || '' }),
    el('p', { cls: 'ip-tl-actor', text: ev.actor_name || '' }),
    el('p', { cls: 'ip-tl-when' }, [
      el('time', { attrs: { datetime: String(ev.at) }, text: formatTimelineStamp(ev.at) }),
    ]),
  ]);
  if (ev.kind === 'email_sent' && ev.meta) {
    item.appendChild(el('button', {
      cls: 'link',
      type: 'button',
      text: 'View message',
      attrs: { 'aria-label': `View the delivery record for ${formatTimelineStamp(ev.at)}` },
      on: { click: () => showDeliveryRecord(ev) },
    }));
  }
  return item;
}

/// Render `GET /invoices/{id}/timeline` (newest-first) into the ordered list.
/// An empty history says so; a 404/403/network failure reports itself honestly
/// rather than leaving a fabricated entry behind.
async function renderTimeline(inv) {
  const list = $('ip-timeline');
  const state = $('ip-timeline-state');
  list.textContent = '';
  state.hidden = true;
  state.textContent = '';
  const id = inv.id;
  try {
    const data = await api.get(`/invoices/${id}/timeline`);
    if (!invoicePreview || invoicePreview.id !== id) return;
    const events = data.events || [];
    previewTimeline = events;
    if (events.length === 0) {
      state.textContent = 'No history yet.';
      state.hidden = false;
      return;
    }
    for (const ev of events) list.appendChild(timelineEntry(ev));
  } catch (err) {
    if (!invoicePreview || invoicePreview.id !== id) return;
    previewTimeline = [];
    state.textContent = `History unavailable — ${err.message}`;
    state.hidden = false;
  }
}

/// `View message`: what was *retained* about an email send -- the recipient, the
/// subject, the From identity and the transport it went through. The message
/// body was deliberately never persisted, so the popup presents itself as the
/// delivery record and says the body is not kept rather than pretending to show
/// a message. A transport other than `smtp` is named as a non-delivery.
export function showDeliveryRecord(ev) {
  const meta = (ev && ev.meta) || {};
  const fields = $('message-fields');
  fields.textContent = '';
  const row = (label, value) => {
    if (value === undefined || value === null || value === '') return;
    fields.appendChild(el('dt', { text: label }));
    fields.appendChild(el('dd', { text: String(value) }));
  };
  $('message-dialog-title').textContent = 'Email delivery record';
  row('To', meta.to);
  row('Subject', meta.subject);
  row('From', meta.from_email ? `${meta.from_name} <${meta.from_email}>` : meta.from_name);
  const delivered = meta.transport === 'smtp';
  row('Transport', delivered ? 'smtp' : `${meta.transport || 'unknown'} — not delivered`);
  $('message-note').textContent = delivered
    ? 'The message body is not retained — this is the delivery record kept for the invoice.'
    : 'The message body is not retained, and no SMTP transport was configured: nothing left the building for this record.';
  const dlg = $('message-dialog');
  if (typeof dlg.showModal === 'function') dlg.showModal();
  else dlg.setAttribute('open', ''); // jsdom fallback (no modal support here)
  $('message-close').focus();
}

export function closeDeliveryRecord() {
  const dlg = $('message-dialog');
  if (typeof dlg.close === 'function') dlg.close();
  else dlg.removeAttribute('open');
}

export async function openInvoicePreview(inv, opener) {
  // A re-render of the panel the user already has open (after a payment, a
  // delivery, any refresh) must not re-announce the preview: the live region is
  // how every action reports itself, and a second "Previewing invoice …" would
  // overwrite the announcement of whatever just happened.
  const reopening = Boolean(invoicePreview && invoicePreview.id === inv.id && !$('invoice-preview').hidden);
  invoicePreview = { id: inv.id, inv, opener: opener || null };
  previewTimeline = [];
  const card = $('invoice-preview');
  card.hidden = false;
  // #349: the detail panel carries no amounts/facts readout beside the
  // document -- the invoice's identity travels on the framed document's own
  // accessible name (the iframe title). #354 adds only its frozen subject as a
  // plain-text subtitle and one sentence about when it was frozen; the region
  // is still the rendered PDF plus that identity and the history.
  const draft = inv.status === 'draft';
  // `subject` is snapshotted at issue, so an issued invoice shows exactly what
  // was billed while a draft (which has no frozen subject yet) says it still
  // follows the live settings. Both are plain text and never editable here —
  // the API owns the freeze.
  const subject = String(inv.subject || '').trim();
  const subjectLine = $('ip-subject');
  subjectLine.textContent = subject;
  subjectLine.hidden = !subject;
  const documentNote = $('ip-document-note');
  documentNote.hidden = false;
  documentNote.textContent = draft
    ? 'This draft follows the current invoice template and customer settings; its subject, terms and due date are frozen when you issue it.'
    : 'Subject, payment terms and due date are fixed at issue — later template or customer changes do not alter this invoice.';
  // ---- Get paid: the collect-payment box on the invoice. No hosted
  // checkout exists by design, so this shows the customer's own payment
  // details, what has been settled and when, and offers manual
  // settlement. A draft gets no box: it is not collectable yet, and
  // nothing here gates it.
  const gp = $('ip-getpaid');
  gp.hidden = draft;
  const gpFacts = $('ip-getpaid-facts');
  const gpPay = $('ip-getpaid-pay');
  const gpHist = $('ip-getpaid-history');
  gpFacts.textContent = '';
  gpPay.textContent = '';
  gpHist.textContent = '';
  if (!draft) {
    const pair = (list) => (label, value) => {
      list.appendChild(el('dt', { text: label }));
      list.appendChild(el('dd', { text: value }));
    };
    const gf = pair(gpFacts);
    const gpr = pair(gpPay);
    const bal = invoiceBalance(inv);
    const settled = (inv.payments || []).reduce((a, p) => a + p.amount_minor, 0);
    gf('Outstanding', bal ? `${inv.currency} ${formatMoney(bal)}` : 'Settled');
    if (settled) gf('Recorded so far', `${inv.currency} ${formatMoney(settled)}`);
    if (inv.status === 'written_off') gf('Balance', 'Written off — no further collection');
    if (inv.due_date && bal) {
      const today = new Date().toISOString().slice(0, 10);
      gf('Due', `${inv.due_date}${inv.due_date < today ? ' (overdue)' : ''}`);
    }
    const cust = state.customers.find((x) => x.id === inv.customer_id);
    const pd = cust && cust.payment_details;
    const payRows = pd
      ? [
        ['Bank', pd.bank_name],
        ['Account name', pd.account_name],
        ['Account number', pd.account_number],
        ['IBAN', pd.iban],
        ['BIC/SWIFT', pd.bic],
        ['Reference', pd.reference],
      ].filter((r) => r[1] && String(r[1]).trim())
      : [];
    const instr = pd && pd.instructions ? String(pd.instructions).trim() : '';
    const hasPay = payRows.length > 0 || !!instr;
    $('ip-getpaid-paytitle').hidden = !hasPay;
    $('ip-getpaid-none').hidden = hasPay;
    for (const [label, value] of payRows) gpr(label, String(value).trim());
    const gpInstr = $('ip-getpaid-instr');
    gpInstr.hidden = !instr;
    gpInstr.textContent = instr;
    for (const p of inv.payments || []) {
      gpHist.appendChild(
        el('p', {
          text: `${String(p.received_at).slice(0, 10)} — ${inv.currency} ${formatMoney(p.amount_minor)}`
            + (p.reference ? ` (${p.reference})` : '') + ` · ${p.method}`,
        }),
      );
    }
    const gpBtn = $('ip-record-payment');
    gpBtn.hidden = !(isOpenInvoice(inv) && bal > 0);
    gpBtn.onclick = () => recordPayment(inv, bal);
  }
  // The preview region frames the rendered PDF (this batch, #349): the
  // endpoint renders on demand, so a draft has a real render and the old
  // "no archived PDF for a draft" dead end is gone; #ip-pdf-state is the
  // loading/failure line only.
  $('ip-pdf-state').hidden = false;
  $('ip-pdf-state').textContent = 'Rendering preview…';
  // Download still means the *archived* document (#113), which a draft does not
  // have yet, so it stays hidden for a draft while the live render is framed.
  $('ip-download').hidden = inv.status === 'draft';
  card.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
  if (!reopening) announce(`Previewing invoice ${inv.number}.`);
  await Promise.all([renderPreviewPdf(inv), renderTimeline(inv)]);
}

export function closeInvoicePreview() {
  if (!invoicePreview) return;
  const { opener } = invoicePreview;
  invoicePreview = null;
  previewTimeline = [];
  releasePreviewPdfUrl();
  $('ip-pdf').textContent = '';
  $('ip-pdf-state').hidden = true;
  $('ip-pdf-state').textContent = '';
  $('ip-subject').textContent = '';
  $('ip-subject').hidden = true;
  $('ip-document-note').textContent = '';
  $('ip-document-note').hidden = true;
  $('ip-timeline').textContent = '';
  $('ip-timeline-state').hidden = true;
  $('ip-timeline-state').textContent = '';
  $('invoice-preview').hidden = true;
  if (opener && document.contains(opener)) opener.focus();
}

// Open a specific invoice's preview from another view — the Timesheets lock
// links call this: show the Invoices panel, refresh the register, then
// preview exactly that invoice. Admin-tier like every invoice surface; the
// lock links only render for users whose lock index saw the invoice.
export async function focusInvoice(id) {
  try {
    switchTab('tab-invoices');
    await refreshInvoices();
    const inv = await api.get(`/invoices/${id}`);
    await openInvoicePreview(inv, null);
  } catch (err) {
    announce(err.message);
  }
}

/// `Send email` in the detail panel opens the recipient dialog; the POST (and
/// its honest transport reporting, #130) happens on the dialog's Send so the
/// admin sees who will receive the invoice first.
export function sendInvoiceEmail(inv, opener = null) {
  return openSendEmailDialog(inv, opener);
}

/// `Mark as sent`: POST /invoices/{id}/sent. A **409** (a draft) is reported as
/// the plain refusal it is, and the transition is **idempotent** -- when the
/// refreshed history gains no entry the repeat is announced as already sent,
/// never as a failure and never as a duplicate.
export async function markInvoiceSent(inv) {
  const tracked = Boolean(invoicePreview && invoicePreview.id === inv.id);
  const before = tracked ? previewTimeline.length : -1;
  let sentAt;
  try {
    const res = await api.post(`/invoices/${inv.id}/sent`);
    sentAt = res.sent_at;
  } catch (err) {
    announce(err.status === 409
      ? `Cannot mark as sent: ${err.message}.`
      : `Mark as sent failed: ${err.message}`);
    return;
  }
  await refreshInvoices();
  const after = tracked ? previewTimeline.length : -1;
  const stamp = formatTimelineStamp(sentAt);
  announce(tracked && after === before
    ? `Already marked as sent ${stamp} — no duplicate history entry.`
    : `Invoice marked as sent ${stamp}.`);
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
    attrs: {
      type: 'number', min: '0.01', step: '0.01', inputmode: 'decimal', style: 'width:7ch', 'aria-label': 'Quantity',
      value: line && line.quantity_hundredths ? (line.quantity_hundredths / 100).toFixed(2) : '1.00'
    },
  });
  const price = el('input', {
    cls: 'num',
    attrs: {
      type: 'number', min: '0', step: '0.01', inputmode: 'decimal', style: 'width:9ch', 'aria-label': 'Unit price',
      value: line && line.unit_price_minor != null ? (line.unit_price_minor / 100).toFixed(2) : '0.00'
    },
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
  ed.currency = draft ? draft.currency : (state.customers.find((c) => c.id === $('ed-customer').value)?.currency || 'EUR');
  $('ed-title').textContent = draft ? `Edit draft ${draft.number}` : 'Manual invoice';
  $('ed-customer').disabled = Boolean(draft);
  const tbody = $('ed-lines').querySelector('tbody');
  tbody.textContent = '';
  for (const l of ed.tracked) tbody.appendChild(edTrackedRow(l));
  const manuals = draft ? (draft.lines || []).filter((l) => !l.entry_id && !l.expense_id) : [];
  if (manuals.length === 0 && !draft) tbody.appendChild(edLineRow());
  for (const l of manuals) tbody.appendChild(edLineRow(l));
  $('ed-tax').value = draft ? (draft.tax_hundredths / 100).toFixed(2) : '0';
  $('ed-discount').value = draft ? (draft.discount_hundredths / 100).toFixed(2) : '0';
  clearFormError($('ed-error'));
  // #349: the editor is a native <dialog class="p-modal"> popup; jsdom has no
  // modal support, so fall back to the [open] attribute the harness asserts on.
  const dlg = $('invoice-editor');
  if (typeof dlg.showModal === 'function') dlg.showModal();
  else dlg.setAttribute('open', '');
  edRecalc();
  $('ed-lines').querySelector('tbody input')?.focus();
  announce(draft ? `Editing draft ${draft.number}.` : 'Manual invoice editor open.');
}

export function edClose() {
  ed.id = null;
  const dlg = $('invoice-editor');
  if (typeof dlg.close === 'function') dlg.close();
  else dlg.removeAttribute('open');
}

// #349: Cancel is the explicit way out of the manual editor. The editor only
// ever mutates its own DOM, so closing it discards the draft with no request at
// all — there is nothing half-saved to undo (the repo's "no partial
// persistence" rule). The native <dialog> closes and initDialogFocusReturn
// (web/js/dom.js) hands focus back to the + Manual Invoice button that opened it.
export function edCancel() {
  edClose();
  announce('Manual invoice cancelled — nothing was saved.');
}

export async function edSave() {
  // QA-01: a double click on Save draft must not create two drafts.
  return singleFlight($('ed-save'), edSaveBody);
}

async function edSaveBody() {
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

