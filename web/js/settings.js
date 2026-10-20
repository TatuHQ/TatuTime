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
      el('td', { cls: 'actions-col', data: { iconActions: '1' }, attrs: { 'data-heading': 'Actions' } }, [
        el('button', { cls: 'link', type: 'button', text: 'Edit', data: { icon: 'edit' }, on: { click: () => editUser(user) } }),
        el('button', { cls: 'link', type: 'button', text: user.active ? 'Deactivate' : 'Activate', data: { icon: user.active ? 'stop' : 'play' }, on: { click: () => toggleUserActive(user.id, !user.active) } }),
        el('button', { cls: 'link', type: 'button', text: 'Change password', data: { icon: 'lock-locked' }, on: { click: () => changeUserPassword(user) } }),
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
  // six-request waterfall behind the Settings tab). `refreshVault` memoises its
  // in-flight request, so `refreshSecrets` joins it (it needs the vault state to
  // decide whether the CRUD is usable) instead of issuing a second GET.
  await Promise.all([
    refreshVault(),
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

// --------------------------------------------------- vault key lifecycle ---
//
// The credential vault's key is the one admin setting the API reports as a
// *state* (GET /api/admin/vault) rather than a value: the key itself never
// leaves the server, so this card shows only the fingerprint and the fixed
// file name `vault.key`. `state.vault` caches the last status so
// `refreshSecrets` can gate the hidden CRUD without a second round-trip.

/// The sentence shared by the detail line and the disabled buttons' titles.
/// A count of 1 reads naturally ("1 credential is stored"), never "1 credentials".
export function vaultCredentialNote(count) {
  return `${count} credential${count === 1 ? '' : 's'} ${count === 1 ? 'is' : 'are'} stored; delete ${count === 1 ? 'it' : 'them'} first.`;
}

/// Render one GET /api/admin/vault payload. The wording follows `source` and
/// `disabled_reason`; every action's visibility/enabled state follows the
/// payload's `can_*` flags, so the GUI can never offer a transition the server
/// refuses for a reason it already knew.
export function renderVault(status) {
  const card = $('vault-card');
  if (!card) return;
  card.hidden = false;
  const statusLine = $('vault-status');
  const detail = $('vault-detail');
  const warning = $('vault-warning');
  const enable = $('vault-enable');
  const regenerate = $('vault-regenerate');
  const disable = $('vault-disable');
  warning.hidden = true;
  warning.textContent = '';
  for (const button of [enable, regenerate, disable]) {
    button.hidden = true;
    button.disabled = false;
    button.removeAttribute('title');
    button.removeAttribute('aria-describedby');
  }
  const source = status.source;
  if (status.enabled) {
    if (source === 'env' || source === 'env_file') {
      statusLine.textContent = 'Vault enabled — key supplied by the environment (override).';
      let text = 'The key is managed outside the app; the Settings actions do not change it.';
      if (status.key_file_present) {
        text += ` A ${status.key_file_name || 'vault.key'} also exists in the data directory and is ignored while the override is set.`;
      }
      detail.textContent = text;
      return;
    }
    statusLine.textContent = 'Vault enabled — the key was created automatically.';
    let text = `Key file ${status.key_file_name || 'vault.key'} on the data volume. Fingerprint ${status.fingerprint || 'unavailable'}. Back up this file with your data.`;
    const count = Number(status.secrets_count) || 0;
    if (count > 0) text += ` ${vaultCredentialNote(count)}`;
    detail.textContent = text;
    regenerate.hidden = false;
    disable.hidden = false;
    regenerate.disabled = !status.can_regenerate;
    disable.disabled = !status.can_disable;
    if (count > 0) {
      for (const button of [regenerate, disable]) {
        button.title = vaultCredentialNote(count);
        button.setAttribute('aria-describedby', 'vault-detail');
      }
    }
    return;
  }
  const reason = status.disabled_reason;
  if (reason === 'env') {
    statusLine.textContent = 'Vault disabled by TUCANO_VAULT_ENABLED.';
    detail.textContent = 'Remove the variable to use the Settings controls.';
    return;
  }
  if (reason === 'write_failed') {
    statusLine.textContent = 'Vault disabled — the key could not be written.';
    detail.textContent = 'The data directory is not writable; fix ownership/permissions and try again.';
  } else {
    statusLine.textContent = 'Vault disabled — no key is configured.';
    detail.textContent = 'Enable it to store integration credentials.';
  }
  enable.hidden = !status.can_enable;
}

// One HTTP round-trip shared by every caller in the same refresh pass.
let vaultInFlight = null;

export function refreshVault() {
  if (vaultInFlight) return vaultInFlight;
  vaultInFlight = (async () => {
    try {
      const payload = await api.get('/admin/vault');
      state.vault = payload;
      clearFormError($('vault-error'));
      renderVault(payload);
      return payload;
    } catch (err) {
      if (err.status === 403) {
        // Member: the whole Security section is hidden for them; stay silent.
        $('vault-card').hidden = true;
        return null;
      }
      announce(`Vault status could not be loaded: ${err.message}`);
      return null;
    } finally {
      vaultInFlight = null;
    }
  })();
  return vaultInFlight;
}

function showVaultError(message, code = null) {
  const target = $('vault-error');
  target.textContent = message;
  if (code) target.dataset.errorCode = code;
  else delete target.dataset.errorCode;
  target.hidden = false;
}

// Map the documented refusals onto the copy contract. A `vault_in_use` or
// `vault_managed_externally` answer means the local view was stale, so the
// status is re-fetched first and the explanation is rendered after it.
async function handleVaultError(err) {
  const code = err.payload?.error?.code;
  if (code === 'vault_in_use') {
    await refreshVault();
    const count = Number(state.vault?.secrets_count) || 0;
    showVaultError(count > 0
      ? vaultCredentialNote(count)
      : 'The vault still holds credentials; delete them first.', code);
    return;
  }
  if (code === 'vault_managed_externally') {
    await refreshVault();
    showVaultError('The key is managed by the environment.', code);
    return;
  }
  if (code === 'vault_disabled') {
    // Regenerate against a disabled vault: the refreshed state is the answer.
    await refreshVault();
    return;
  }
  if (code === 'data_dir_not_writable') {
    showVaultError('The data directory is not writable; fix ownership/permissions and try again.', code);
    return;
  }
  showFormError($('vault-error'), err);
}

let vaultBusy = false;

async function runVaultAction(request, outcome) {
  if (vaultBusy) return;
  vaultBusy = true;
  clearFormError($('vault-error'));
  try {
    const result = await request();
    await refreshVault();
    // The secrets CRUD depends on the vault being enabled: re-render it too, so
    // the section appears (or disappears) with the new state.
    await refreshSecrets();
    announce(typeof outcome === 'function' ? outcome(result) : outcome);
  } catch (err) {
    await handleVaultError(err);
  } finally {
    vaultBusy = false;
  }
}

export async function enableVault() {
  return runVaultAction(
    () => api.post('/admin/vault/enable'),
    (result) => (result?.created_key === false
      ? 'Vault enabled. Existing key reused.'
      : 'Vault enabled. Key created.'),
  );
}

export async function disableVault() {
  if (!await askConfirm(
    'Disable the credential vault? The generated vault.key is deleted and anything encrypted with it becomes unreadable. This cannot be undone.',
    'Disable vault',
  )) return;
  return runVaultAction(
    () => api.post('/admin/vault/disable', { confirm: true }),
    'Vault disabled.',
  );
}

export async function regenerateVault() {
  if (!await askConfirm(
    'Regenerate the vault key? A new vault.key replaces the current one, and anything encrypted with the old key becomes unreadable. This cannot be undone.',
    'Regenerate key',
  )) return;
  return runVaultAction(
    () => api.post('/admin/vault/regenerate', { confirm: true }),
    'Vault key regenerated.',
  );
}

export async function refreshSecrets() {
  // The vault decides whether the CRUD is usable at all. When it is disabled
  // the list endpoint answers 503 by design, so it is never even called — the
  // vault card explains the state instead of this section reporting an error.
  const vault = state.vault || (await refreshVault());
  if (!vault) {
    $('secret-form').hidden = true;
    $('secret-card').hidden = true;
    return;
  }
  if (!vault.enabled) {
    $('secret-form').hidden = true;
    $('secret-card').hidden = true;
    return;
  }
  $('secret-form').hidden = false;
  $('secret-card').hidden = false;
  try {
    const data = await api.get('/admin/secrets');
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
    announce(err.message);
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
  // M3: read-only knobs (today only `vault_enabled`) are listed by GET but are
  // written through their dedicated endpoint, never this generic form; the
  // vault card in Settings → Security is their control.
  fillSelect(
    sel,
    configRows
      .filter((row) => row.writable !== false)
      .map((row) => ({ value: row.key, text: row.key })),
  );
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
    $('org-legal-label').value = o.legal_id_label || '';
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
    // The sample document is rendered with this org identity (name, address,
    // legal id + label), so a fresh profile invalidates the framed preview's
    // tag: the next render must not answer `304` for bytes that show the old
    // company block (#354).
    templatePreviewEtag = null;
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
        legal_id_label: $('org-legal-label').value.trim(),
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

// The document-label overrides (#147/#354), in the order the invoice reads
// them. Every id is `lb-<key with underscores as dashes>`; the key IS the API
// field, so this list is the one place the form, the payload and the preview
// agree on the set.
export const LABEL_KEYS = [
  'description', 'quantity', 'unit_price', 'subtotal', 'discount', 'tax', 'total',
  'from', 'invoice_for', 'invoice_id', 'issue_date', 'due_date',
  'subject', 'payment_heading', 'item_type', 'payments', 'amount_due',
];

export async function refreshInvoiceTemplate() {
  try {
    const data = await api.get('/admin/invoice-template');
    const t = data.template || {};
    $('template-subject').value = t.subject || '';
    $('template-body').value = t.body || '';
    $('template-footer').value = t.footer || '';
    $('template-notes').value = t.notes || '';
    const lb = t.labels || {};
    for (const k of LABEL_KEYS) {
      $(`lb-${k.replace(/_/g, '-')}`).value = lb[k] || '';
    }
    // The framed bytes may belong to an older stored template / org profile:
    // dropping the tag makes the next render unconditional, so a reload can
    // never answer `304` for a document the page should replace (#354).
    templatePreviewEtag = null;
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
  'template-subject', 'template-body', 'template-footer', 'template-notes',
  'template-terms', 'template-terms-days',
  ...LABEL_KEYS.map((key) => `lb-${key.replace(/_/g, '-')}`),
];

/// True while the first render waits for the document's `load` event. Framing a
/// blob-URL PDF inserts an iframe, which makes the browser lay the frame out;
/// doing that while the page is still loading is what made Firefox log "Layout
/// was forced before the page was fully loaded" on the next view (#365).
let templatePreviewAwaitingLoad = false;
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

/// A blank copy of the shell's frame (same id, class and title, no `src`).
/// `releaseTemplatePreview` swaps the live frame for one of these: merely
/// clearing `src` leaves the PDF viewer's document in place, and Firefox flushes
/// layout on it when the page is navigated away — the "Layout was forced before
/// the page was fully loaded" warning the next view's console drain catches
/// (#365). The invoice detail panel tears its frame down the same way (it
/// empties its host node). The clone is taken from the markup so the attributes
/// stay declared in one place.
let templatePreviewFrameTemplate = null;

function blankTemplatePreviewFrame() {
  const frame = $('template-preview-frame');
  if (!frame) return null;
  if (!templatePreviewFrameTemplate) {
    templatePreviewFrameTemplate = frame.cloneNode(false);
    templatePreviewFrameTemplate.removeAttribute('src');
  }
  return templatePreviewFrameTemplate.cloneNode(false);
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
    notes: $('template-notes').value,
    payment_terms,
    // #147/#354 display labels ride the same template store (no parallel
    // truth). Every key travels, including the blank ones, so clearing an
    // override is the same edit as setting one.
    labels: Object.fromEntries(
      LABEL_KEYS.map((key) => [key, $(`lb-${key.replace(/_/g, '-')}`).value.trim()]),
    ),
  };
}

/// Drop the frame and its URL. Called when the section is hidden or the page is
/// torn down; a render still in flight is invalidated rather than applied.
export function releaseTemplatePreview() {
  templatePreviewGeneration += 1;
  // A render that was waiting for `load` must not frame after the section has
  // been left; the one-shot listener checks this flag and does nothing.
  templatePreviewAwaitingLoad = false;
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
  if (!frame) return;
  frame.removeAttribute('src');
  // Replace the element: removing the PDF document from the DOM is what stops
  // the viewer, and it must happen before the browser tears the page down.
  const blank = blankTemplatePreviewFrame();
  if (blank && frame.parentNode) frame.parentNode.replaceChild(blank, frame);
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

/// #365: the sidebar's tab strip hides the whole Settings section without
/// changing any inner panel's `hidden`, so the observer in `initTemplatePreview`
/// cannot see that switch. The router calls this when Settings comes back, so a
/// template that was already loaded renders its preview again instead of
/// showing the frame it released when the section was hidden.
export function resumeTemplatePreview() {
  if (templatePreviewLoaded) scheduleTemplatePreview(0);
}

/// Request a render after the user pauses typing. `delay: 0` is used when the
/// section becomes visible or the stored template finishes loading.
export function scheduleTemplatePreview(delay = TEMPLATE_PREVIEW_DEBOUNCE_MS) {
  if (!templatePanelShown()) return;
  // #365: never frame the preview before the document has finished loading.
  // The boot-time route selection can show this section while `readyState` is
  // still "loading"; setting the frame's src there forces layout mid-parse and
  // Firefox warns about it (the message surfaced on the *next* view because the
  // console drain for this one had already run). Wait for `load`, then keep the
  // usual debounce.
  if (document.readyState !== 'complete') {
    if (!templatePreviewAwaitingLoad) {
      templatePreviewAwaitingLoad = true;
      window.addEventListener('load', () => {
        if (!templatePreviewAwaitingLoad) return; // released before load
        templatePreviewAwaitingLoad = false;
        scheduleTemplatePreview(delay);
      }, { once: true });
    }
    return;
  }
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
  for (const key of LABEL_KEYS) {
    $(`lb-${key.replace(/_/g, '-')}`).value = '';
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
  if (data.vault_enabled) {
    setEmailVaultNotice(false);
    setEmailControlsEnabled(true);
  } else {
    setEmailVaultNotice(true);
    setEmailControlsEnabled(false);
  }
}

/// The Email panel's vault notice. It points at the one surface that owns the
/// key lifecycle (Settings → Security & runtime) instead of naming an env var:
/// the admin can enable the vault there, with no restart and no shell access.
function setEmailVaultNotice(visible) {
  const notice = $('email-vault-notice');
  const text = $('email-vault-notice-text');
  if (!visible) {
    text.textContent = '';
    notice.hidden = true;
    return;
  }
  text.textContent = 'The credential vault is disabled — enable it in Security & runtime.';
  notice.hidden = false;
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
        setEmailVaultNotice(true);
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

