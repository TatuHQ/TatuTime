// settings.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { showAuth } from './auth.js';
import { refreshCatalog } from './billing.js';
import { $, announce, askConfirm, clearFormError, el, fillSelect, formatMoney, showFormError, singleFlight, statusChip } from './dom.js';
import { state } from './state.js';

// --------------------------------------------------------------- settings --

export let managedUsers = [];
export let userSaving = false;
export let userDialogGeneration = 0;
export let passwordDialogGeneration = 0;
export let passwordSaving = false;
export let passwordReturnFocus = null;

export async function refreshUsers() {
  if ($('settings-tab-users').hidden) return;
  $('user-list-status').textContent = 'Loading users...';
  try {
    const data = await api.get('/users');
    managedUsers = data.users || [];
    renderUsers();
  } catch (err) {
    $('user-list-status').textContent = `Could not load users: ${err.message}`;
  }
}

export function renderUsers() {
  const search = $('user-search').value.trim().toLowerCase();
  const role = $('user-filter-role').value;
  const active = $('user-filter-active').value;
  const users = managedUsers.filter((user) =>
    (!search || `${user.name} ${user.email}`.toLowerCase().includes(search))
    && (!role || user.role === role)
    && (!active || String(user.active) === active));
  const tbody = $('user-table').querySelector('tbody');
  tbody.textContent = '';
  for (const user of users) {
    const row = el('tr', {}, [
      el('th', { attrs: { scope: 'row' }, text: user.name }),
      el('td', { attrs: { 'data-heading': 'Email' }, text: user.email }),
      el('td', { attrs: { 'data-heading': 'Role' }, text: user.role }),
      el('td', { attrs: { 'data-heading': 'Status' }, text: user.active ? 'active' : 'inactive' }),
      el('td', { attrs: { 'data-heading': 'Rates' }, text: `${formatMoney(user.default_rate_minor)} / ${formatMoney(user.cost_rate_minor)}` }),
      el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, [
        el('button', { cls: 'link', type: 'button', text: 'Edit', data: { icon: 'edit' }, on: { click: () => editUser(user) } }),
        el('button', { cls: 'link', type: 'button', text: user.active ? 'Deactivate' : 'Activate', on: { click: () => toggleUserActive(user.id, !user.active) } }),
        el('button', { cls: 'link', type: 'button', text: 'Change password', on: { click: () => changeUserPassword(user) } }),
        el('button', { cls: 'link danger', type: 'button', text: 'Delete', data: { icon: 'delete' }, on: { click: () => deleteUser(user) } }),
      ]),
    ]);
    tbody.appendChild(row);
  }
  $('user-empty').hidden = users.length !== 0;
  $('user-list-status').textContent = `${users.length} of ${managedUsers.length} users`;
}

export function editUser(user) {
  userDialogGeneration += 1;
  $('user-id').value = user.id;
  $('user-name').value = user.name;
  $('user-email').value = user.email;
  $('user-role').value = user.role;
  $('user-active').checked = user.active;
  $('user-default-rate').value = formatMoney(user.default_rate_minor);
  $('user-cost-rate').value = formatMoney(user.cost_rate_minor);
  $('user-password').value = '';
  $('user-password').required = false;
  $('user-password-field').hidden = true;
  $('user-save').textContent = 'Save user';
  $('user-dialog-title').textContent = 'Edit user';
  $('user-error').hidden = true;
  $('user-dialog').showModal();
  $('user-name').focus();
}

export function newUserForm() {
  userDialogGeneration += 1;
  $('user-form').reset();
  $('user-id').value = '';
  $('user-active').checked = true;
  $('user-password').required = true;
  $('user-password-field').hidden = false;
  $('user-save').textContent = 'Add user';
  $('user-dialog-title').textContent = 'Add user';
  $('user-error').hidden = true;
  $('user-form').querySelector('details').open = false;
}

export function closeUserForm() {
  $('user-dialog').close();
  newUserForm();
  $('user-new').focus();
}

export function userRateMinor(value) {
  const token = value.trim() || '0';
  if (!/^\d+(?:\.\d{1,2})?$/.test(token)) throw new Error('Rates must be non-negative amounts with at most 2 decimal places.');
  const [whole, fractional = ''] = token.split('.');
  const minor = Number(whole) * 100 + Number(fractional.padEnd(2, '0'));
  if (!Number.isSafeInteger(minor) || minor > 100000000) throw new Error('Hourly rates must not exceed 1000000.00.');
  return minor;
}

export async function saveUser(evt) {
  evt.preventDefault();
  if (userSaving) return;
  clearFormError($('user-error'));
  const id = $('user-id').value;
  const generation = userDialogGeneration;
  const body = {
    name: $('user-name').value.trim(),
    email: $('user-email').value.trim(),
    role: $('user-role').value,
    active: $('user-active').checked,
  };
  const password = $('user-password').value;
  if (!id) {
    body.password = password;
    if (!password) {
      showFormError($('user-error'), new Error('Password is required for a new user.'));
      return;
    }
  }
  try {
    body.default_rate_minor = userRateMinor($('user-default-rate').value);
    body.cost_rate_minor = userRateMinor($('user-cost-rate').value);
    userSaving = true;
    $('user-save').disabled = true;
    if (id) await api.put(`/users/${id}`, body);
    else await api.post('/users', body);
    announce(id ? 'User updated.' : 'User added.');
    if (generation === userDialogGeneration) closeUserForm();
    await refreshUsers();
  } catch (err) {
    if (!handleUserAuthError(err) && generation === userDialogGeneration) showFormError($('user-error'), err);
  } finally {
    userSaving = false;
    $('user-save').disabled = false;
  }
}

export async function toggleUserActive(id, active) {
  try {
    await api.put(`/users/${id}`, { active });
    await refreshUsers();
    announce(active ? 'User activated.' : 'User deactivated.');
  } catch (err) {
    announce(err.message);
  }
}

export function changeUserPassword(user) {
  passwordDialogGeneration += 1;
  passwordReturnFocus = document.activeElement;
  $('user-password-form').reset();
  $('user-password-id').value = user.id;
  $('user-password-title').textContent = `Change password for ${user.name}`;
  clearFormError($('user-password-error'));
  $('user-password-dialog').showModal();
  $('user-new-password').focus();
}

export function closeUserPassword() {
  passwordDialogGeneration += 1;
  $('user-password-dialog').close();
  $('user-password-form').reset();
  if (passwordReturnFocus?.isConnected) passwordReturnFocus.focus();
}

export function handleUserAuthError(error) {
  if (error.status !== 401 && error.status !== 403) return false;
  closeUserForm();
  closeUserPassword();
  showAuth('login');
  return true;
}

export async function saveUserPassword(event) {
  event.preventDefault();
  if (passwordSaving) return;
  const generation = passwordDialogGeneration;
  const id = $('user-password-id').value;
  const value = $('user-new-password').value;
  clearFormError($('user-password-error'));
  try {
    passwordSaving = true;
    $('user-password-form').querySelector('[type="submit"]').disabled = true;
    await api.put(`/users/${id}/password`, { password: value });
    if (generation === passwordDialogGeneration) closeUserPassword();
    announce('Password changed; existing sessions were revoked.');
  } catch (err) {
    if (!handleUserAuthError(err) && generation === passwordDialogGeneration) showFormError($('user-password-error'), err);
  } finally {
    passwordSaving = false;
    $('user-password-form').querySelector('[type="submit"]').disabled = false;
  }
}

export async function deleteUser(user) {
  if (!await askConfirm(`Delete ${user.name}? This is permanent and cannot be undone.`)) return;
  try {
    await api.del(`/users/${user.id}`);
    await refreshUsers();
    announce(`${user.name} was deleted.`);
  } catch (err) {
    announce(err.message);
  }
}

export async function refreshSettings() {
  // SET-02: every loader here is admin-tier; the only member-visible Settings
  // section is Expense categories, owned by expenses.js. A member boot used to
  // fire six admin GETs and leave "administrator role required" in the live
  // region (and an error in the hidden Users status line), so the whole set is
  // gated on the role the session already told us.
  if (!state.isAdmin) return;
  // #189: independent admin surfaces load in parallel (was a sequential
  // six-request waterfall behind the Settings tab).
  await Promise.all([
    refreshUsers(),
    refreshSecrets(),
    refreshConfig(),
    refreshOrgProfile(),
    refreshCatalog(),
    refreshInvoiceTemplate(),
    refreshEmailSettings(),
    refreshAuditLog(),
  ]);
}

export async function refreshSecrets() {
  try {
    const data = await api.get('/admin/secrets');
    $('vault-status').textContent = 'Credential vault: enabled (encrypted at rest).';
    $('secret-form').hidden = false;
    $('secret-card').hidden = false;
    const tbody = $('secret-table').querySelector('tbody');
    tbody.textContent = '';
    const secrets = data.secrets || [];
    for (const s of secrets) {
      tbody.appendChild(
        el('tr', {}, [
          el('th', { attrs: { scope: 'row' }, text: s.key }),
          el('td', { attrs: { 'data-heading': 'Value' }, text: s.hint }),
          el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, [
            el('button', {
              cls: 'danger',
              type: 'button',
              text: 'Delete',
              data: { icon: 'delete' },
              on: { click: () => deleteSecret(s.key) },
            }),
          ]),
        ]),
      );
    }
    $('secret-empty').hidden = secrets.length !== 0;
  } catch (err) {
    if (err.status === 503) {
      $('vault-status').textContent = 'Credential vault disabled — set TUCANO_SECRET_KEY to enable.';
      $('secret-form').hidden = true;
      $('secret-card').hidden = true;
    } else {
      announce(err.message);
    }
  }
}

// SET-11: the read-only audit log (GET /audit, admin tier). The API returns the
// newest events first as `{ ts, event, subject }` — there is no separate actor
// field, so the subject column shows whatever identity the event recorded
// (an email, an id, an invoice number, a provider name). No write path exists
// here by design: the log is append-only.
function auditTime(ts) {
  if (!ts) return '';
  // The API sends RFC3339 UTC; show it locale-free so the table is stable.
  return ts.replace('T', ' ').replace(/\.\d+Z$/, ' UTC').replace(/Z$/, ' UTC');
}

export async function refreshAuditLog() {
  if (!state.isAdmin) return;
  const status = $('audit-status');
  if (!status) return;
  status.textContent = 'Loading audit events…';
  try {
    const data = await api.get('/audit');
    const events = data.events || [];
    const tbody = $('audit-table').querySelector('tbody');
    tbody.textContent = '';
    for (const event of events) {
      tbody.appendChild(
        el('tr', {}, [
          el('td', { attrs: { 'data-heading': 'Time' }, text: auditTime(event.ts) }),
          el('td', { attrs: { 'data-heading': 'Event' }, text: event.event || '' }),
          el('td', { attrs: { 'data-heading': 'Subject' }, text: event.subject || '' }),
        ]),
      );
    }
    $('audit-empty').hidden = events.length !== 0;
    status.textContent = events.length ? `${events.length} recent events, newest first.` : '';
  } catch (err) {
    if (err.status === 403) {
      // Member: the card is admin-only, so stay silent and out of the way.
      $('audit-card').hidden = true;
      status.textContent = '';
      return;
    }
    $('audit-empty').hidden = true;
    status.textContent = `Could not load the audit log: ${err.message}`;
  }
}

// Effective runtime configuration card (#94). Admin surface; members get a
// silent hide. Values resolve env > config.json > default; applying an edit
// persists it to config.json on the data volume so restarts keep it.
export let configRows = [];

export function configValueForInput(v) {
  if (v === null || v === undefined) return '';
  if (typeof v === 'boolean') return String(v);
  if (typeof v === 'number') return String(v);
  return v;
}

// SET-04: keys the server reads once at boot and stores in a long-lived
// component, so editing them here only takes effect after a restart. Derived
// from the call sites: `max_docs` (Store::open), `reminder_days`
// (EmailReminderJob), `sso_allowed_domains` (SsoRegistry at AppState::boot) and
// the two accounting base URLs (AccountingRegistry at AppState::boot). The SSO
// admin group, calendar redirect and OAuth client ids are resolved per request.
export const RESTART_REQUIRED_KEYS = new Set([
  'max_docs',
  'reminder_days',
  'sso_allowed_domains',
  'qbo_base_url',
  'xero_base_url',
]);

// SET-08: `Kind::Currency` is the one server kind whose `validate` accepts
// `null` (meaning "clear"); every other null-defaulted kind expects a string.
export const NULLABLE_CONFIG_KEYS = new Set(['base_currency']);

// SET-15: true only once the stored config really arrived. Save stays disabled
// otherwise, so a failed load can never be overwritten with blanks.
let configLoaded = false;

export async function refreshConfig() {
  let data;
  try {
    data = await api.get('/admin/config');
  } catch (err) {
    configLoaded = false;
    $('config-save').disabled = true;
    $('config-card').dataset.loaded = '0';
    if (err.status === 403) {
      // Member: the section is hidden for them; stay silent.
      $('config-card').hidden = true;
      $('config-form').hidden = true;
      return;
    }
    announce(`Runtime configuration could not be loaded: ${err.message}. Saving is disabled until it loads.`);
    return;
  }
  configLoaded = true;
  $('config-card').hidden = false;
  $('config-form').hidden = false;
  $('config-card').dataset.loaded = '1';
  $('config-save').disabled = false;
  configRows = data.config || [];
  const tbody = $('config-table').querySelector('tbody');
  tbody.textContent = '';
  tbody.append(
    ...configRows.map((row) =>
      el('tr', {}, [
        el('th', { attrs: { scope: 'row', title: row.description }, text: row.key }),
        el('td', { cls: 'num', attrs: { 'data-heading': 'Value' }, text: configValueForInput(row.value) }),
        el('td', { attrs: { 'data-heading': 'Source' } }, [
          statusChip(row.source, row.source !== 'default'),
        ]),
        el('td', { attrs: { 'data-heading': 'Applies' } }, [
          RESTART_REQUIRED_KEYS.has(row.key)
            ? statusChip('after restart')
            : el('span', { text: 'immediately' }),
        ]),
      ]),
    ),
  );
  const sel = $('config-key');
  fillSelect(sel, configRows.map((row) => ({ value: row.key, text: row.key })));
  // Pre-fill the value box when the key changes.
  sel.onchange = () => {
    const row = configRows.find((c) => c.key === sel.value);
    $('config-value').value = row ? configValueForInput(row.value) : '';
  };
  sel.onchange();
}

export async function saveConfig(evt) {
  evt.preventDefault();
  if (!configLoaded) {
    announce('Runtime configuration is not loaded yet — reload the section before saving.');
    return;
  }
  // SET-05: a double submit writes once.
  return singleFlight($('config-save'), async () => {
    clearFormError($('config-error'));
    const key = $('config-key').value;
    const raw = $('config-value').value;
    const row = configRows.find((c) => c.key === key) || {};
    let value = raw;
    if (typeof row.value === 'number') {
      if (!/^-?\d+$/.test(raw)) {
        showFormError($('config-error'), { message: 'Enter a whole number.' });
        return;
      }
      value = Number(raw);
    } else if (typeof row.value === 'boolean') {
      if (raw !== 'true' && raw !== 'false') {
        showFormError($('config-error'), { message: 'Enter true or false.' });
        return;
      }
      value = raw === 'true';
    } else if (NULLABLE_CONFIG_KEYS.has(key) && raw.trim() === '') {
      // SET-08: the API clears a Currency knob with an explicit `null` (an
      // empty string is refused). It is the only kind that accepts null —
      // OptionalStr/Csv rows are cleared by an empty string instead — so this
      // stays limited to the keys the server treats as nullable.
      value = null;
    }
    try {
      await api.request('PUT', '/admin/config', { [key]: value });
      announce(`Saved ${key}.`);
      await refreshConfig();
    } catch (err) {
      showFormError($('config-error'), err);
    }
  });
}

// ------------------------------------------------- company identity (#138) --
// The reply-to address doubles as the default recipient for the Email tab's
// "Send test email" field; nothing else reads it here.
export let orgReplyTo = '';

// SET-15: the form is only saveable once the stored identity arrived.
let orgLoaded = false;

export async function refreshOrgProfile() {
  try {
    const o = await api.get('/admin/org');
    $('org-name').value = o.name || '';
    $('org-legal').value = o.legal_id || '';
    const a = o.address || {};
    $('org-street').value = a.street || '';
    $('org-city').value = a.city || '';
    $('org-postal').value = a.postal_code || '';
    $('org-country').value = a.country || '';
    $('org-from').value = o.from_name || '';
    $('org-reply').value = o.reply_to || '';
    orgReplyTo = o.reply_to || '';
    if (!$('email-test-to').value) $('email-test-to').value = orgReplyTo;
    $('org-accent').value = o.accent || '';
    paintAccent();
    orgLoaded = true;
    $('org-form').dataset.loaded = '1';
    $('org-save').disabled = false;
  } catch (err) {
    orgLoaded = false;
    $('org-form').dataset.loaded = '0';
    $('org-save').disabled = true;
    // A member cannot manage the org profile: silence. Any other failure is
    // announced, because saving the blank form would overwrite stored data.
    if (err.status !== 403) {
      announce(`Company information could not be loaded: ${err.message}. Saving is disabled until it loads.`);
    }
  }
}

export function paintAccent() {
  // Safe styling only: assign a validated color to one element's background
  // (#146). No innerHTML anywhere.
  const hex = $('org-accent').value.trim();
  const sw = $('accent-swatch');
  sw.style.background = /^#[0-9a-fA-F]{6}$/.test(hex) ? hex : '';
  sw.title = sw.style.background ? `Totals on new documents: ${hex}` : 'Built-in ink';
}

export async function saveOrgProfile(evt) {
  evt.preventDefault();
  if (!orgLoaded) {
    announce('Company information is not loaded yet — reload the section before saving.');
    return;
  }
  // SET-05: a double submit writes once.
  return singleFlight($('org-save'), async () => {
    clearFormError($('org-error'));
    const street = $('org-street').value.trim();
    const city = $('org-city').value.trim();
    const postal = $('org-postal').value.trim();
    const country = $('org-country').value.trim();
    try {
      await api.put('/admin/org', {
        name: $('org-name').value.trim(),
        legal_id: $('org-legal').value.trim(),
        // #146 presentation rides the same org store — no parallel truth.
        from_name: $('org-from').value.trim(),
        reply_to: $('org-reply').value.trim(),
        accent: $('org-accent').value.trim(),
        address: street || city || postal || country
          ? { street, city, postal_code: postal, country }
          : null,
      });
      announce('Company information saved.');
      await refreshOrgProfile();
    } catch (err) {
      showFormError($('org-error'), err);
    }
  });
}

export async function refreshInvoiceTemplate() {
  try {
    const data = await api.get('/admin/invoice-template');
    const t = data.template || {};
    $('template-subject').value = t.subject || '';
    $('template-body').value = t.body || '';
    $('template-footer').value = t.footer || '';
    const lb = t.labels || {};
    for (const k of ['description', 'quantity', 'unit_price', 'subtotal', 'discount', 'tax', 'total']) {
      $(`lb-${k.replace('_', '-')}`).value = lb[k] || '';
    }
    const terms = t.payment_terms || null;
    $('template-terms').value = terms ? terms.kind : '';
    $('template-terms-days').value = terms && terms.days != null ? terms.days : '';
    $('template-terms-days-field').hidden = !terms || terms.kind !== 'custom';
    // Variable cheat-sheet rendered from the server table (same source the
    // validator enforces).
    const tbody = $('template-vars').querySelector('tbody');
    tbody.textContent = '';
    for (const v of data.variables || []) {
      tbody.appendChild(
        el('tr', {}, [
          el('th', { attrs: { scope: 'row' }, text: v.name }),
          el('td', { attrs: { 'data-heading': 'Example' }, text: v.example }),
        ]),
      );
    }
    // The live preview may only render what the form actually shows, so it
    // waits for this fill. `initTemplatePreview` renders when the section is
    // shown; a refresh while it is already visible renders immediately.
    templatePreviewLoaded = true;
    $('template-form').dataset.loaded = '1';
    $('template-save').disabled = false;
    if (templatePanelShown()) scheduleTemplatePreview(0);
  } catch (err) {
    templatePreviewLoaded = false;
    $('template-form').dataset.loaded = '0';
    $('template-save').disabled = true;
    // Members cannot see the template (the tab stays hidden for them): silence.
    // Any other failure is announced, so Save cannot overwrite the stored
    // template with the blank form.
    if (err.status !== 403) {
      announce(`Invoice template could not be loaded: ${err.message}. Saving is disabled until it loads.`);
    }
  }
}

// ------------------------------------------- live template preview (#new) --
//
// The Invoice template section renders the *candidate* template -- exactly what
// Save would send -- against the API's sample invoice, so the page shows the PDF
// while it is being edited and no invoice has to exist. The card owns one frame:
// a render mints one object URL and revokes the one it replaced, so a document
// the panel stopped showing never stays alive behind a pinned URL (the rule the
// receipt and invoice previews follow, #332/#349).

/// How long typing must pause before a render is requested: long enough that a
/// burst of keystrokes costs one PDF, short enough to feel live.
export const TEMPLATE_PREVIEW_DEBOUNCE_MS = 500;

/// The controls whose edits change the rendered document. Both `input` and
/// `change` are observed: typing fires `input`, a pick or the number spinner
/// fires `change`.
const TEMPLATE_PREVIEW_FIELDS = [
  'template-subject', 'template-body', 'template-footer',
  'template-terms', 'template-terms-days',
  'lb-description', 'lb-quantity', 'lb-unit-price', 'lb-subtotal',
  'lb-discount', 'lb-tax', 'lb-total',
];

/// The blob URL currently framed -- the only URL this panel may revoke.
let templatePreviewUrl = '';
/// The ETag of the framed bytes, or null when nothing is framed. A release
/// drops the frame, so it also drops the ETag: answering `304` for bytes the
/// page no longer holds would leave the frame blank.
let templatePreviewEtag = null;
let templatePreviewTimer = null;
let templatePreviewInFlight = false;
let templatePreviewPending = false;
let templatePreviewRun = Promise.resolve();
/// Bumped by every release: a response that arrives after the section was
/// hidden (or the page torn down) must neither frame a document nor mint a URL.
let templatePreviewGeneration = 0;
/// True once the form holds the stored template: before that, a render would
/// preview an empty form rather than what was saved.
let templatePreviewLoaded = false;
let templatePreviewWired = false;

const templatePanel = () => $('settings-panel-invoice-template');
const templatePanelShown = () => {
  const panel = templatePanel();
  return !!panel && !panel.hidden;
};

function setTemplatePreviewStatus(text) {
  const status = $('template-preview-status');
  if (status) status.textContent = text;
}

/// The exact payload Save sends. Building it once means the preview can never
/// drift from what would be persisted.
export function templatePayload() {
  const kind = $('template-terms').value;
  let payment_terms = null;
  if (kind) {
    payment_terms = { kind };
    if (kind === 'custom') payment_terms.days = Number($('template-terms-days').value);
  }
  return {
    subject: $('template-subject').value.trim(),
    body: $('template-body').value,
    footer: $('template-footer').value,
    payment_terms,
    // #147 display labels ride the same template store (no parallel truth).
    labels: {
      description: $('lb-description').value.trim(),
      quantity: $('lb-quantity').value.trim(),
      unit_price: $('lb-unit-price').value.trim(),
      subtotal: $('lb-subtotal').value.trim(),
      discount: $('lb-discount').value.trim(),
      tax: $('lb-tax').value.trim(),
      total: $('lb-total').value.trim(),
    },
  };
}

/// Drop the frame and its URL. Called when the section is hidden or the page is
/// torn down; a render still in flight is invalidated rather than applied.
export function releaseTemplatePreview() {
  templatePreviewGeneration += 1;
  if (templatePreviewTimer !== null) {
    clearTimeout(templatePreviewTimer);
    templatePreviewTimer = null;
  }
  templatePreviewPending = false;
  if (templatePreviewUrl) {
    URL.revokeObjectURL(templatePreviewUrl);
    templatePreviewUrl = '';
  }
  templatePreviewEtag = null;
  const frame = $('template-preview-frame');
  if (frame) frame.removeAttribute('src');
}

/// Open the framed PDF in a new tab. The current blob URL is the document on
/// screen, so the new tab shows exactly what the preview shows.
export function openTemplatePreviewInNewTab() {
  if (!templatePreviewUrl) {
    setTemplatePreviewStatus('Preview not rendered yet — edit the template or press Refresh.');
    return;
  }
  const opened = window.open(templatePreviewUrl, '_blank');
  if (opened) opened.opener = null;
  else setTemplatePreviewStatus('The browser blocked the new tab — allow pop-ups to open the preview.');
}

/// Request a render after the user pauses typing. `delay: 0` is used when the
/// section becomes visible or the stored template finishes loading.
export function scheduleTemplatePreview(delay = TEMPLATE_PREVIEW_DEBOUNCE_MS) {
  if (!templatePanelShown()) return;
  if (templatePreviewTimer !== null) clearTimeout(templatePreviewTimer);
  templatePreviewTimer = setTimeout(() => {
    templatePreviewTimer = null;
    renderTemplatePreview();
  }, delay);
}

/// One render, coalesced: at most one request is in flight, and edits made
/// while it runs schedule exactly one follow-up with the newest form state.
/// `force` skips the conditional request (the toolbar's Refresh).
export function renderTemplatePreview({ force = false } = {}) {
  if (!templatePanelShown()) return Promise.resolve();
  if (templatePreviewInFlight) {
    templatePreviewPending = true;
    return templatePreviewRun;
  }
  templatePreviewInFlight = true;
  const generation = templatePreviewGeneration;
  templatePreviewRun = (async () => {
    try {
      let runForce = force;
      do {
        templatePreviewPending = false;
        await renderTemplatePreviewOnce(runForce);
        runForce = false; // only the explicit Refresh re-fetches unconditionally
      } while (templatePreviewPending && generation === templatePreviewGeneration);
    } finally {
      templatePreviewInFlight = false;
    }
  })();
  return templatePreviewRun;
}

async function renderTemplatePreviewOnce(force) {
  const frame = $('template-preview-frame');
  if (!frame) return;
  const generation = templatePreviewGeneration;
  setTemplatePreviewStatus('Rendering…');
  try {
    const result = await api.requestBlob(
      'POST',
      '/admin/invoice-template/preview',
      templatePayload(),
      { ifNoneMatch: force ? null : templatePreviewEtag },
    );
    if (generation !== templatePreviewGeneration) return; // section closed mid-flight
    if (result.notModified) {
      // The bytes on screen are still the bytes this template renders: keep the
      // frame, say so in the live region.
      setTemplatePreviewStatus('Preview updated');
      return;
    }
    const previous = templatePreviewUrl;
    templatePreviewUrl = URL.createObjectURL(result.blob);
    frame.setAttribute('src', templatePreviewUrl);
    if (previous) URL.revokeObjectURL(previous);
    templatePreviewEtag = result.etag || null;
    clearFormError($('template-error'));
    setTemplatePreviewStatus('Preview updated');
  } catch (err) {
    if (generation !== templatePreviewGeneration) return;
    if (err.status === 422) {
      // Field errors belong on the form, where the user is typing; the frame
      // keeps the last good document and nothing typed is discarded.
      showFormError($('template-error'), err);
      setTemplatePreviewStatus('Preview not updated — fix the highlighted fields.');
    } else {
      // A transient failure (403, network, 5xx) is a sentence, not an alert
      // storm, and never blanks the document already on screen.
      setTemplatePreviewStatus(`Preview unavailable — ${err.message}`);
    }
  }
}

/// Wire the preview card once. Editing any template/label control schedules a
/// debounced render; hiding the section releases the frame and the URL.
export function initTemplatePreview() {
  if (templatePreviewWired) return;
  const panel = templatePanel();
  if (!panel) return;
  templatePreviewWired = true;
  for (const id of TEMPLATE_PREVIEW_FIELDS) {
    const control = $(id);
    if (!control) continue;
    control.addEventListener('input', () => scheduleTemplatePreview());
    control.addEventListener('change', () => scheduleTemplatePreview());
  }
  $('template-preview-refresh')?.addEventListener('click', () => {
    renderTemplatePreview({ force: true });
  });
  $('template-preview-open')?.addEventListener('click', openTemplatePreviewInNewTab);
  if (typeof MutationObserver === 'function') {
    // The section is hidden/shown by the tab strip (router.js), not by this
    // module; observing the attribute keeps the release and the first render
    // tied to what the user can actually see.
    new MutationObserver(() => {
      if (panel.hidden) releaseTemplatePreview();
      else if (templatePreviewLoaded) scheduleTemplatePreview(0);
    }).observe(panel, { attributes: true, attributeFilter: ['hidden'] });
  }
  window.addEventListener('pagehide', releaseTemplatePreview);
}

/// SET-07: clear every label override and re-render the preview. The preview
/// listens to `input`/`change`; assigning `.value` fires neither, so the framed
/// document used to stay stale until the next keystroke. One explicit render
/// keeps the card honest ("Use built-in labels" is itself an edit).
export function resetBuiltInLabels() {
  for (const k of ['lb-description', 'lb-quantity', 'lb-unit-price', 'lb-subtotal', 'lb-discount', 'lb-tax', 'lb-total']) {
    $(k).value = '';
  }
  announce('Built-in labels restored (save the template to apply).');
  scheduleTemplatePreview(0);
}

export async function saveInvoiceTemplate(evt) {
  evt.preventDefault();
  if (!templatePreviewLoaded) {
    announce('Invoice template is not loaded yet — reload the section before saving.');
    return;
  }
  // SET-05: a double submit writes once.
  return singleFlight($('template-save'), async () => {
    clearFormError($('template-error'));
    try {
      await api.put('/admin/invoice-template', templatePayload());
      announce('Invoice template saved.');
      await refreshInvoiceTemplate();
    } catch (err) {
      showFormError($('template-error'), err);
    }
  });
}

// ------------------------------------------------------------- email (#35) --
// SMTP transport settings live in the vault (the password is write-only) and
// are resolved on every send, so saving here takes effect without a restart.

export let emailSettings = null;

export function renderEmailStatus(data) {
  const status = $('email-status');
  if (!status) return;
  status.textContent = data.configured
    ? `Email delivery: SMTP ${data.host}:${data.port}`
    : 'Email delivery: NOT configured — messages are only logged.';
}

function setEmailControlsEnabled(enabled) {
  for (const id of ['email-form', 'email-test-form']) {
    const form = $(id);
    if (!form) continue;
    for (const control of form.querySelectorAll('input, select, textarea, button')) {
      control.disabled = !enabled;
    }
  }
}

export async function refreshEmailSettings() {
  let data;
  try {
    data = await api.get('/admin/email');
  } catch (err) {
    // Members cannot read admin settings; every other failure is announced.
    if (err.status !== 403) announce(err.message);
    return;
  }
  emailSettings = data;
  renderEmailStatus(data);
  $('email-host').value = data.host || '';
  $('email-port').value = String(data.port || 25);
  $('email-username').value = data.username || '';
  $('email-from').value = data.from || 'noreply@localhost';
  // The password never reaches the DOM: only whether one is stored.
  $('email-password').value = '';
  $('email-password').placeholder = data.has_password
    ? 'stored — leave blank to keep'
    : 'No password stored';
  if (!$('email-test-to').value) $('email-test-to').value = orgReplyTo || '';
  const notice = $('email-vault-notice');
  if (data.vault_enabled) {
    notice.hidden = true;
    notice.textContent = '';
    setEmailControlsEnabled(true);
  } else {
    notice.textContent =
      'Email settings need the credential vault. Set TUCANO_SECRET_KEY to enable encrypted storage, then restart and reload this page.';
    notice.hidden = false;
    setEmailControlsEnabled(false);
  }
}

export async function saveEmailSettings(evt) {
  evt.preventDefault();
  // SET-05: a double submit writes once.
  return singleFlight($('email-save'), async () => {
    clearFormError($('email-error'));
    const current = emailSettings || {};
    const body = {
      host: $('email-host').value.trim(),
      port: Number($('email-port').value),
    };
    // Only changed optional values travel: an untouched username/from keeps its
    // stored value server-side, and a blank password never wipes the stored one.
    const username = $('email-username').value.trim();
    if (username !== (current.username || '')) body.username = username;
    const from = $('email-from').value.trim();
    if (from !== (current.from || '')) body.from = from;
    const password = $('email-password').value;
    if (password) body.password = password;
    try {
      emailSettings = await api.put('/admin/email', body);
      announce('Email settings saved; the next send uses them immediately.');
      await refreshEmailSettings();
    } catch (err) {
      if (err.status === 503 || err.payload?.error?.code === 'vault_disabled') {
        const notice = $('email-vault-notice');
        notice.textContent =
          'The credential vault is disabled — set TUCANO_SECRET_KEY and restart to save email settings.';
        notice.hidden = false;
        setEmailControlsEnabled(false);
      }
      showFormError($('email-error'), err);
    }
  });
}

export async function sendTestEmail(evt) {
  if (evt) evt.preventDefault();
  clearFormError($('email-test-error'));
  $('email-test-result').textContent = '';
  // QA-01: a double click must not send two test messages.
  return singleFlight($('email-test-send'), async () => {
    const to = $('email-test-to').value.trim();
    try {
      const res = await api.post('/admin/email/test', { to });
      const sentTo = res.sent_to || to;
      $('email-test-result').textContent = `Test email sent to ${sentTo}.`;
      announce(`Test email sent to ${sentTo}.`);
      return res;
    } catch (err) {
      const code = err.payload?.error?.code;
      if (code === 'email_not_configured') {
        const explained = new Error(
          'Email delivery is not configured — save an SMTP host above first.',
        );
        explained.status = err.status;
        explained.payload = err.payload;
        showFormError($('email-test-error'), explained);
      } else {
        showFormError($('email-test-error'), err);
      }
      announce(err.message);
      return undefined;
    }
  });
}

export async function addSecret(evt) {
  evt.preventDefault();
  clearFormError($('secret-error'));
  const key = $('secret-key').value.trim();
  try {
    await api.put(`/admin/secrets/${encodeURIComponent(key)}`, { value: $('secret-value').value });
    $('secret-key').value = '';
    $('secret-value').value = '';
    announce('Secret saved.');
    await refreshSettings();
  } catch (err) {
    showFormError($('secret-error'), err);
  }
}

export async function deleteSecret(key) {
  if (!(await askConfirm(`Delete secret "${key}"?`))) return;
  try {
    await api.del(`/admin/secrets/${encodeURIComponent(key)}`);
    await refreshSettings();
    announce('Secret deleted.');
  } catch (err) {
    announce(err.message);
  }
}

