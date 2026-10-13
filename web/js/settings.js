// settings.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { showAuth } from './auth.js';
import { refreshCatalog } from './billing.js';
import { $, announce, askConfirm, clearFormError, el, fillSelect, formatMoney, showFormError } from './dom.js';

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
      el('td', { text: user.email }),
      el('td', { text: user.role }),
      el('td', { text: user.active ? 'active' : 'inactive' }),
      el('td', { text: `${formatMoney(user.default_rate_minor)} / ${formatMoney(user.cost_rate_minor)}` }),
      el('td', { cls: 'actions-col' }, [
        el('button', { cls: 'link', type: 'button', text: 'Edit', on: { click: () => editUser(user) } }),
        el('button', { cls: 'link', type: 'button', text: user.active ? 'Deactivate' : 'Activate', on: { click: () => toggleUserActive(user.id, !user.active) } }),
        el('button', { cls: 'link', type: 'button', text: 'Change password', on: { click: () => changeUserPassword(user) } }),
        el('button', { cls: 'link danger', type: 'button', text: 'Delete', on: { click: () => deleteUser(user) } }),
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
  // #189: independent admin surfaces load in parallel (was a sequential
  // six-request waterfall behind the Settings tab).
  await Promise.all([
    refreshUsers(),
    refreshSecrets(),
    refreshConfig(),
    refreshOrgProfile(),
    refreshCatalog(),
    refreshInvoiceTemplate(),
  ]);
}

export async function refreshSecrets() {
  try {
    const data = await api.get('/admin/secrets');
    $('vault-status').textContent = 'Credential vault: enabled (encrypted at rest).';
    $('secret-form').hidden = false;
    const tbody = $('secret-table').querySelector('tbody');
    tbody.textContent = '';
    const secrets = data.secrets || [];
    for (const s of secrets) {
      tbody.appendChild(
        el('tr', {}, [
          el('th', { attrs: { scope: 'row' }, text: s.key }),
          el('td', { text: s.hint }),
          el('td', { cls: 'actions-col' }, [
            el('button', {
              cls: 'danger',
              type: 'button',
              text: 'Delete',
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
      $('secret-table').hidden = true;
    } else {
      announce(err.message);
    }
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

export async function refreshConfig() {
  let data;
  try {
    data = await api.get('/admin/config');
    // #130: be honest about delivery. A 200 from the email endpoints with
    // transport "disabled" means logged, not sent.
    const es = $('email-status');
    if (es) {
      es.textContent = data.email_transport === 'smtp'
        ? 'Email delivery: SMTP configured — invoices are actually sent.'
        : 'Email delivery: NOT configured — messages are logged only. Set smtp.host in credentials above or TUCANO_SMTP_HOST.';
    }
  } catch {
    $('config-table').hidden = true;
    $('config-form').hidden = true;
    return;
  }
  configRows = data.config || [];
  const tbody = $('config-table').querySelector('tbody');
  tbody.textContent = '';
  tbody.append(
    ...configRows.map((row) =>
      el('tr', {}, [
        el('th', { attrs: { scope: 'row', title: row.description }, text: row.key }),
        el('td', { cls: 'num', text: configValueForInput(row.value) }),
        el('td', {}, [
          el('span', { cls: row.source === 'default' ? 'badge' : 'badge on', text: row.source }),
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
  }
  try {
    await api.request('PUT', '/admin/config', { [key]: value });
    announce(`Saved ${key}.`);
    await refreshConfig();
  } catch (err) {
    showFormError($('config-error'), err);
  }
}

// ------------------------------------------------- invoice template (#116) --

export let templateVarsLoaded = false;

// ------------------------------------------------- company identity (#138) --
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
    $('org-accent').value = o.accent || '';
    paintAccent();
  } catch { /* members cannot manage the org profile */ }
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
          el('td', { text: v.example }),
        ]),
      );
    }
    templateVarsLoaded = true;
    await refreshTemplatePreview();
  } catch {
    /* members cannot see the template; tab stays hidden for them */
  }
}

export async function refreshTemplatePreview() {
  const card = $('template-preview');
  try {
    const data = await api.get('/invoices');
    const invoices = (data.invoices || []).filter((i) => i.status !== 'draft');
    if (invoices.length === 0) {
      card.hidden = true;
      return;
    }
    const newest = invoices[invoices.length - 1];
    const doc = await api.get(`/invoices/${newest.id}/document`);
    $('template-preview-subject').textContent = doc.subject || '';
    // The server-returned HTML is reduced to plain text through the DOM (no
    // markup is ever inserted into this page: the textContent rule holds).
    const parsed = new DOMParser().parseFromString(doc.html || '', 'text/html');
    $('template-preview-body').textContent = parsed.body.textContent;
    card.hidden = false;
  } catch {
    card.hidden = true;
  }
}

export async function saveInvoiceTemplate(evt) {
  evt.preventDefault();
  clearFormError($('template-error'));
  const kind = $('template-terms').value;
  let payment_terms = null;
  if (kind) {
    payment_terms = { kind };
    if (kind === 'custom') payment_terms.days = Number($('template-terms-days').value);
  }
  try {
    await api.put('/admin/invoice-template', {
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
    });
    announce('Invoice template saved.');
    await refreshInvoiceTemplate();
  } catch (err) {
    showFormError($('template-error'), err);
  }
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

