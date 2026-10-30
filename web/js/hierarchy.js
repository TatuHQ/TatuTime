// hierarchy.js — TatuTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { API_BASE, api } from './api.js';
import { iw } from './billing.js';
import { refreshPanel, switchTab } from './boot.js';
import { $, announce, askAlert, askConfirm, clearFormError, el, fillSelect, formatMoney, iconActionsCell, panelRefresh, showFormError, singleFlight, statusChip } from './dom.js';
import { ed } from './invoices.js';
import { createSeqGuard } from './seq.js';
import { customerName, fillCustomerSelect, fillProjectSelect, loadCustomers, loadProjects, state } from './state.js';
import { jumpToEntry } from './timesheet.js';

const projectSeq = createSeqGuard();
const taskSeq = createSeqGuard();

// The Setup registers share one status filter vocabulary ('', 'true', 'false').
// A missing `active` is active, matching the server default for legacy docs.
const matchesStatus = (record, status) =>
  !status || String(record.active !== false) === status;
const statusWord = (status) => (status === 'false' ? 'inactive' : 'active');

// ---------------------------------------------------------- customers ---

export async function refreshCustomerTable() {
  $('customer-filter-active').value = state.customersStatus || '';
  const customers = state.customers.filter((c) => matchesStatus(c, state.customersStatus));
  const tbody = $('customer-table').querySelector('tbody');
  tbody.textContent = '';
  for (const c of customers) {
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' } }, [
          // #182: no selection state — the action opens the Projects section
          // filtered to this customer.
          el('button', {
            type: 'button', cls: 'link', text: c.name,
            on: { click: () => openProjectsFor(c.id) }
          }),
        ]),
        // No `num` here: the rate column reads left-aligned with its header
        // (the `num` utility right-aligns, which left the values stranded).
        el('td', { attrs: { 'data-heading': 'Default rate' }, text: `${c.currency} ${formatMoney(c.default_rate_minor)}` }),
        el('td', { attrs: { 'data-heading': 'Status' } }, [
          statusChip(c.active !== false ? 'Active' : 'Inactive', c.active !== false),
        ]),
        iconActionsCell([
          el('button', {
            cls: 'link',
            type: 'button',
            text: 'Projects',
            data: { icon: 'menu' },
            on: { click: () => openProjectsFor(c.id) },
          }),
          el('button', {
            cls: 'link',
            type: 'button',
            text: 'Edit',
            data: { icon: 'edit' },
            on: { click: () => startCustomerEdit(c) },
          }),
          el('button', {
            cls: 'danger',
            type: 'button',
            text: 'Delete',
            data: { icon: 'delete' },
            on: { click: () => removeCustomer(c) },
          }),
        ]),
      ]),
    );
  }
  $('customer-table').hidden = customers.length === 0;
  const empty = $('customers-empty-state');
  empty.hidden = customers.length !== 0;
  if (!empty.hidden) {
    empty.textContent = state.customersStatus
      ? `No ${statusWord(state.customersStatus)} customers.`
      : 'No customers yet. Use + Customer to add one.';
  }
}

// QA-10: the Customers view's navigation refresh (fetch + render + pickers)
// runs through the shared stale-panel wrapper, so a failed load leaves the rows
// and shows the visible banner with Retry.
export function refreshCustomersView() {
  return panelRefresh('panel-customers', 'the customer register', async () => {
    await loadCustomers();
    await refreshCustomerTable();
    await refreshCustomerPickers();
  });
}

export function startCustomerEdit(c) {
  $('customer-id').value = c.id;
  $('customer-name').value = c.name;
  $('customer-currency').value = c.currency;
  $('customer-rate').value = (c.default_rate_minor / 100).toFixed(2);
  $('customer-active').checked = c.active;
  $('customer-email').value = c.email || '';
  $('customer-invoice-prefix').value = c.invoice_prefix || '';
  // Invoice document fields (#116).
  const terms = c.payment_terms || null;
  $('customer-terms').value = terms ? terms.kind : '';
  $('customer-terms-days').value = terms && terms.days != null ? terms.days : '';
  $('customer-terms-days-field').hidden = !terms || terms.kind !== 'custom';
  $('customer-subject').value = c.invoice_subject || '';
  $('customer-notes').value = c.invoice_notes || '';
  $('customer-legal-id').value = c.legal_id || '';
  const a = c.address || {};
  $('cust-street').value = a.street || '';
  $('cust-city').value = a.city || '';
  $('cust-postal').value = a.postal_code || '';
  $('cust-country').value = a.country || '';
  $('cust-tax').value = c.tax_hundredths ? (c.tax_hundredths / 100).toFixed(2) : '0';
  $('cust-discount').value = c.discount_hundredths ? (c.discount_hundredths / 100).toFixed(2) : '0';
  // Payment details: the customer's own "how to pay" block (no provider
  // integration exists — these strings drive Get paid, emails, documents).
  const pd = c.payment_details || {};
  $('pay-bank-name').value = pd.bank_name || '';
  $('pay-bank-address').value = pd.bank_address || '';
  $('pay-account-name').value = pd.account_name || '';
  $('pay-account-number').value = pd.account_number || '';
  $('pay-account-type').value = pd.account_type || '';
  $('pay-routing-number').value = pd.routing_number || '';
  $('pay-iban').value = pd.iban || '';
  $('pay-bic').value = pd.bic || '';
  $('pay-reference').value = pd.reference || '';
  $('pay-instructions').value = pd.instructions || '';
  // Multi-currency rails (#354): a legacy document has none, which is exactly
  // the single-rail default above.
  setPaymentRails(Array.isArray(pd.rails) ? pd.rails : []);
  setContacts(c.contacts || []);
  $('customer-save').textContent = 'Update customer';
  $('customer-dialog-title').textContent = 'Edit customer';
  clearFormError($('customer-error'));
  if (!$('customer-dialog').open) $('customer-dialog').showModal();
  $('customer-name').focus();
}

// ---- customer contacts editor (#139) — rows are plain inputs read on save.
export function contactRow(c = { name: '', role: '', email: '', billing: false }) {
  // #191: each field gets a real accessible name (placeholder is unreliable).
  const mk = (ph, val, type = 'text') =>
    el('input', { attrs: { placeholder: ph, 'aria-label': ph, value: val, type, maxlength: ph === 'Email' ? 200 : 120 } });
  const name = mk('Name', c.name);
  const role = mk('Role', c.role);
  const email = mk('Email', c.email, 'email');
  const box = el('input', { attrs: { type: 'checkbox' } });
  box.checked = !!c.billing;
  const row = el('div', { cls: 'contact-row' }, [
    name,
    role,
    email,
    el('label', { cls: 'contact-billing' }, [box, el('span', { text: ' billing' })]),
    el('button', {
      type: 'button', cls: 'link danger', text: 'Remove',
      on: { click: () => row.remove() },
    }),
  ]);
  row.__read = () => ({
    name: name.value.trim(), role: role.value.trim(), email: email.value.trim(), billing: box.checked,
  });
  return row;
}

export function readContacts() {
  return [...$('contact-rows').children].map((r) => r.__read()).filter((c) => c.name || c.email);
}

export function setContacts(list) {
  $('contact-rows').textContent = '';
  for (const c of list) $('contact-rows').appendChild(contactRow(c));
}

// ------------------------------------- payment rails (multi-currency, #354) --
//
// `PaymentDetails.rails` holds one account per currency. The flat fields above
// stay the default single-rail block and are never renamed; these rows are
// additive display data printed after them. Every control is labelled (a real
// <label for>), every row is a fieldset whose legend names it, and the
// add/remove controls are real <button>s, so the list is keyboard operable
// without a mouse. Nothing here calls innerHTML: values only ever reach the DOM
// through `el`/`textContent`.

export const MAX_PAYMENT_RAILS = 4;

/// The rail fields in the order the dialog asks for them. `key` is the stored
/// JSON name, so the ids `pay-rail-<i>-<key>` line up with the API's
/// `payment_details.rails[i].<key>` error fields (see dom.js fieldControlIds).
export const PAYMENT_RAIL_FIELDS = [
  ['currency', 'Currency (ISO-4217)', 3, 'EUR'],
  ['account_name', 'Account name', 120, ''],
  ['account_number', 'Account number', 64, ''],
  ['account_type', 'Account type', 40, 'Checking'],
  ['iban', 'IBAN', 34, 'EX000000000000000000'],
  ['bic', 'BIC/SWIFT', 11, ''],
  ['bank_name', 'Bank name', 120, ''],
  ['bank_address', 'Bank address', 200, ''],
  ['routing_number', 'Routing number', 40, ''],
  ['swift', 'SWIFT', 11, ''],
];

export function paymentRailRows() {
  return [...$('pay-rails').children];
}

/// Renumber after an add/remove: ids, label targets, legends and the remove
/// button's accessible name all follow the row's position, so an error field
/// like `payment_details.rails[1].iban` always finds the visible second row.
export function renumberPaymentRails() {
  const rows = paymentRailRows();
  rows.forEach((row, index) => {
    row.dataset.railIndex = String(index);
    row.__legend.textContent = `Payment rail ${index + 1}`;
    for (const { key, label, control } of row.__fields) {
      control.id = `pay-rail-${index}-${key.replace(/_/g, '-')}`;
      label.setAttribute('for', control.id);
    }
    row.__remove.setAttribute('aria-label', `Remove payment rail ${index + 1}`);
  });
  const add = $('pay-rail-add');
  if (add) add.disabled = rows.length >= MAX_PAYMENT_RAILS;
  const status = $('pay-rails-status');
  if (status) {
    status.textContent = rows.length >= MAX_PAYMENT_RAILS
      ? `All ${MAX_PAYMENT_RAILS} payment rails are in use. Remove one to add another.`
      : '';
  }
}

/// One rail row. `rail` is a stored `PaymentRail` (object) or `{}` for a new
/// row; `__read()` returns the payload shape, omitting blank optional fields so
/// the server stores `None` rather than empty strings.
export function paymentRailRow(rail = {}) {
  const legend = el('legend', { text: 'Payment rail' });
  const fields = [];
  const children = [legend];
  for (const [key, text, maxlength, placeholder] of PAYMENT_RAIL_FIELDS) {
    const group = el('div', { cls: 'p-form__group' });
    const label = el('label', { cls: 'p-form__label', text });
    const attrs = { type: 'text', maxlength: String(maxlength), autocomplete: 'off' };
    if (placeholder) attrs.placeholder = placeholder;
    if (key === 'currency') attrs.list = 'pay-rail-currencies';
    const control = el('input', { cls: 'p-form__control', attrs });
    control.dataset.railField = key;
    control.value = rail[key] == null ? '' : String(rail[key]);
    group.append(label, control);
    children.push(group);
    fields.push({ key, label, control });
  }
  const instructionsGroup = el('div', { cls: 'p-form__group' });
  const instructionsLabel = el('label', { cls: 'p-form__label', text: 'Instructions (shown last)' });
  const instructions = el('textarea', {
    cls: 'p-form__control',
    attrs: { maxlength: '500', rows: '2' },
  });
  instructions.dataset.railField = 'instructions';
  instructions.value = rail.instructions == null ? '' : String(rail.instructions);
  instructionsGroup.append(instructionsLabel, instructions);
  children.push(instructionsGroup);
  fields.push({ key: 'instructions', label: instructionsLabel, control: instructions });

  const remove = el('button', {
    type: 'button',
    cls: 'link danger',
    text: 'Remove rail',
    on: { click: () => removePaymentRail(row) },
  });
  children.push(el('div', { cls: 'buttons' }, [remove]));

  const row = el('fieldset', { cls: 'payment-rail' }, children);
  row.__legend = legend;
  row.__fields = fields;
  row.__remove = remove;
  row.__read = () => {
    const value = (key) => row.__fields.find((f) => f.key === key).control.value.trim();
    const out = {
      currency: value('currency').toUpperCase(),
      // The struct requires this key even when the rail carries an IBAN and no
      // local account; an empty string is the honest "not provided".
      account_name: value('account_name'),
    };
    for (const key of ['account_number', 'account_type', 'iban', 'bic', 'bank_name',
      'bank_address', 'routing_number', 'swift', 'instructions']) {
      const v = value(key);
      if (v) out[key] = v;
    }
    return out;
  };
  return row;
}

export function setPaymentRails(list) {
  $('pay-rails').textContent = '';
  for (const rail of (Array.isArray(list) ? list : []).slice(0, MAX_PAYMENT_RAILS)) {
    $('pay-rails').appendChild(paymentRailRow(rail && typeof rail === 'object' ? rail : {}));
  }
  renumberPaymentRails();
}

export function addPaymentRail() {
  if (paymentRailRows().length >= MAX_PAYMENT_RAILS) {
    announce(`At most ${MAX_PAYMENT_RAILS} payment rails.`);
    return null;
  }
  const row = paymentRailRow({});
  $('pay-rails').appendChild(row);
  renumberPaymentRails();
  row.__fields[0].control.focus();
  announce(`Payment rail ${paymentRailRows().length} added.`);
  return row;
}

export function removePaymentRail(row) {
  if (!row || !row.parentNode) return;
  row.remove();
  renumberPaymentRails();
  $('pay-rail-add').focus();
  announce('Payment rail removed.');
}

export function readPaymentRails() {
  // Every visible row, including a blank one: keeping the row indexes aligned
  // is what lets a server/client error name `rails[1]` and mark the second
  // visible row's input. The save path drops fully blank rows afterwards.
  return paymentRailRows().map((row) => row.__read());
}

/// A row with no values at all is an untouched "Add payment rail" click, not a
/// rail: it is neither validated nor sent.
export function railHasContent(rail) {
  return Object.values(rail).some((value) => value !== '');
}

/// The client-side half of the server's rail rules: currency is a 3-letter
/// uppercase ISO code, IBAN 5-34 alphanumerics, BIC/SWIFT exactly 8 or 11.
/// Returns the same `{field, message}` shape the API sends, so showFormError
/// marks the right control and the message reads like any other 422.
export function paymentRailError(index, field, message) {
  const name = `payment_details.rails[${index}].${field}`;
  return {
    message: `${name}: ${message}`,
    payload: {
      error: {
        code: 'validation_failed',
        message: 'Payment details are invalid.',
        fields: [{ field: name, message }],
      },
    },
  };
}

export function validatePaymentRails(rails) {
  for (let index = 0; index < rails.length; index += 1) {
    const rail = rails[index];
    if (!railHasContent(rail)) continue;
    if (!/^[A-Z]{3}$/.test(rail.currency || '')) {
      return paymentRailError(index, 'currency', 'required, a 3-letter uppercase ISO code');
    }
    // Required on a rail, exactly as the API enforces it: a rail that names no
    // account holder cannot be paid into.
    if (!(rail.account_name || '').trim()) {
      return paymentRailError(index, 'account_name', 'required, at most 120 characters');
    }
    const iban = rail.iban || '';
    if (iban && !/^[A-Za-z0-9]{5,34}$/.test(iban)) {
      return paymentRailError(index, 'iban', 'must be 5-34 letters/digits');
    }
    for (const key of ['bic', 'swift']) {
      const code = rail[key] || '';
      if (code && !/^[A-Za-z0-9]{8}(?:[A-Za-z0-9]{3})?$/.test(code)) {
        return paymentRailError(index, key, 'must be 8 or 11 letters/digits');
      }
    }
  }
  return null;
}

export async function saveCustomer(evt) {
  evt.preventDefault();
  // QA-01: a double click must not create two customers.
  return singleFlight($('customer-save'), saveCustomerBody);
}

async function saveCustomerBody() {
  clearFormError($('customer-error'));
  const id = $('customer-id').value;
  // Payment terms (#116): the select carries the kind; only `custom` sends days.
  const kind = $('customer-terms').value;
  let payment_terms = null;
  if (kind) {
    payment_terms = { kind };
    if (kind === 'custom') payment_terms.days = Number($('customer-terms-days').value);
  }
  const street = $('cust-street').value.trim();
  const city = $('cust-city').value.trim();
  const postal = $('cust-postal').value.trim();
  const country = $('cust-country').value.trim();
  // Payment details: blank everywhere = not configured (server normalises
  // too; sending null clears a previously stored block).
  const pay = {
    bank_name: $('pay-bank-name').value.trim(),
    bank_address: $('pay-bank-address').value.trim(),
    account_name: $('pay-account-name').value.trim(),
    account_number: $('pay-account-number').value.trim(),
    account_type: $('pay-account-type').value.trim(),
    routing_number: $('pay-routing-number').value.trim(),
    iban: $('pay-iban').value.trim(),
    bic: $('pay-bic').value.trim(),
    reference: $('pay-reference').value.trim(),
    instructions: $('pay-instructions').value.trim(),
  };
  // Rails first, against the visible rows (so an error names the row the user
  // is looking at), then the payload carries only the rows with content.
  const railRows = readPaymentRails();
  const railProblem = validatePaymentRails(railRows);
  if (railProblem) {
    showFormError($('customer-error'), railProblem);
    $('customer-form').querySelector('[aria-invalid="true"]')?.focus();
    return;
  }
  const rails = railRows.filter((rail) => railHasContent(rail));
  const hasPay = Object.values(pay).some((v) => v) || rails.length > 0;
  const body = {
    name: $('customer-name').value.trim(),
    currency: $('customer-currency').value.trim().toUpperCase(),
    default_rate_minor: Math.round(Number($('customer-rate').value) * 100),
    active: $('customer-active').checked,
    email: $('customer-email').value.trim(),
    invoice_prefix: $('customer-invoice-prefix').value.trim(),
    legal_id: $('customer-legal-id').value.trim(),
    payment_terms,
    invoice_subject: $('customer-subject').value.trim(),
    invoice_notes: $('customer-notes').value.trim(),
    // #139: percent inputs are decimal but persist as integer hundredths.
    address: street || city || postal || country ? { street, city, postal_code: postal, country } : null,
    contacts: readContacts(),
    tax_hundredths: Math.round(Number($('cust-tax').value || 0) * 100),
    discount_hundredths: Math.round(Number($('cust-discount').value || 0) * 100),
    payment_details: hasPay ? { ...pay, rails } : null,
  };
  try {
    if (id) await api.put(`/customers/${id}`, body);
    else await api.post('/customers', body);
    await loadCustomers();
    await refreshCustomerTable();
    await refreshCustomerPickers();
    announce(id ? 'Customer updated.' : 'Customer added.');
    cancelCustomerEdit();
  } catch (err) {
    showFormError($('customer-error'), err);
  }
}

export function cancelCustomerEdit() {
  $('customer-id').value = '';
  $('customer-form').reset();
  $('customer-active').checked = true;
  $('customer-terms-days-field').hidden = true;
  setContacts([]);
  setPaymentRails([]);
  $('customer-save').textContent = 'Add customer';
  $('customer-dialog-title').textContent = 'Add customer';
  $('customer-dialog').close();
  $('customer-new').focus();
  clearFormError($('customer-error'));
}

export async function removeCustomer(c) {
  if (!(await askConfirm(`Delete customer "${c.name}"?`))) return;
  try {
    await api.del(`/customers/${c.id}`);
    await loadCustomers();
    await refreshCustomerTable();
    await refreshCustomerPickers();
    // C7: reset the project panel — it used to keep pointing at the deleted
    // customer, so the next project save 404'd, and leak its cache entry.
    if (state.projectsFilter === c.id) {
      state.projectsFilter = null;
      $('project-table').hidden = true;
      $('projects-empty-state').hidden = false;
    }
    if (state.tasksCustomer === c.id) {
      // The deleted customer was the Tasks filter: fall back to the "all" scope
      // and re-render, rather than faking an empty state.
      state.tasksCustomer = null;
      state.tasksProject = null;
    }
    delete state.projectsByCustomer[c.id];
    await refreshTaskTable();
    announce('Customer deleted.');
  } catch (err) {
    announce(err.message);
  }
}

export function updateHierarchyActions() {
  // #182: creation buttons are always available in their own section (the
  // dialog carries the required parent pick); titles/hints reflect scope.
  $('project-new').title = state.projectsFilter
    ? `Add project for ${customerName(state.projectsFilter)}` : 'Add a project for any customer';
  $('task-new').title = state.tasksProject
    ? `Add task for ${state.tasksProject}` : 'Add a task — pick the parents in the dialog';
  $('project-dialog-parent').textContent = state.projectsFilter ? customerName(state.projectsFilter) : '';
  $('task-dialog-parent').textContent = state.tasksCustomer && state.tasksProject
    ? `${customerName(state.tasksCustomer)} / ${state.tasksProject}` : '';
}

// Populate a hierarchy filter select with customers (#182). The placeholder
// text differs per section ("All customers" vs "Choose a customer…").
export function fillHierarchyFilter(select, selectedId, placeholder) {
  fillSelect(select, [
    { value: '', text: placeholder },
    ...state.customers.map((c) => ({ value: c.id, text: c.active !== false ? c.name : `${c.name} (inactive)`, selected: c.id === selectedId })),
  ]);
  select.value = selectedId || '';
}

// Customer row / "Projects" action: open the Projects Setup section
// filtered to that customer (#182). switchTab suppresses refreshPanel (the
// caller refreshes explicitly — same contract as jumpToEntry), so the
// scoped load happens right here.
export async function openProjectsFor(cid) {
  state.projectsFilter = cid || null;
  // A contextual jump must show that customer's work: a status filter left
  // behind in the Projects section would otherwise make the target look empty.
  state.projectsStatus = '';
  switchTab('tab-projects');
  await loadCustomers();
  await refreshProjectTable();
}

export async function refreshProjectTable() {
  // #182: the section lists every customer's projects, or one customer's when
  // the filter is set. Row data carries its own customer id for the actions.
  // #219: refreshProjectTable is the single owner of the filter select's
  // value — saveProject re-scopes the table, and every caller (row action,
  // save, filter change, tab re-show) renders in sync from here.
  const isProjectCurrent = projectSeq();
  fillHierarchyFilter($('proj-filter-customer'), state.projectsFilter, 'All customers');
  $('proj-filter-active').value = state.projectsStatus || '';
  const cids = state.projectsFilter ? [state.projectsFilter] : state.customers.map((c) => c.id);
  const allRows = [];
  for (const cid of cids) {
    for (const p of await loadProjects(cid)) allRows.push({ cid, p });
  }
  if (!isProjectCurrent()) return; // #219: newer render won
  const rows = allRows.filter(({ p }) => matchesStatus(p, state.projectsStatus));
  const tbody = $('project-table').querySelector('tbody');
  tbody.textContent = '';
  for (const { cid, p } of rows) {
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' } }, [
          el('button', {
            type: 'button', cls: 'link', text: p.code,
            attrs: { title: `Manage tasks for ${p.code}` },
            on: { click: () => openTasksFor(cid, p.code) }
          }),
        ]),
        el('td', { attrs: { 'data-heading': 'Customer' }, text: customerName(cid) }),
        el('td', { attrs: { 'data-heading': 'Name' }, text: p.name }),
        el('td', { attrs: { 'data-heading': 'Currency' }, text: p.currency || 'customer' }),
        el('td', { attrs: { 'data-heading': 'Rate' }, text: p.rate_minor != null ? formatMoney(p.rate_minor) : 'customer' }),
        el('td', { attrs: { 'data-heading': 'Status' } }, [
          statusChip(p.active !== false ? 'Active' : 'Inactive', p.active !== false),
        ]),
        iconActionsCell([
          el('button', {
            cls: 'link',
            type: 'button',
            text: 'Tasks',
            data: { icon: 'copy' },
            on: { click: () => openTasksFor(cid, p.code) },
          }),
          el('button', {
            cls: 'link',
            type: 'button',
            text: 'Edit',
            data: { icon: 'edit' },
            on: { click: () => startProjectEdit(cid, p) },
          }),
          el('button', {
            cls: 'danger',
            type: 'button',
            text: 'Delete',
            data: { icon: 'delete' },
            on: { click: () => removeProject(cid, p) },
          }),
        ]),
      ]),
    );
  }
  $('project-table').hidden = rows.length === 0;
  const empty = $('projects-empty-state');
  empty.hidden = rows.length !== 0;
  if (!empty.hidden) {
    const scope = state.projectsFilter ? ` for ${customerName(state.projectsFilter)}` : '';
    empty.textContent = state.projectsStatus
      ? `No ${statusWord(state.projectsStatus)} projects${scope}.`
      : state.projectsFilter
        ? `No projects for ${customerName(state.projectsFilter)} yet. Use + Project to add one.`
        : 'No projects yet. Use + Project to add one.';
  }
  updateHierarchyActions();
}

// QA-10: the Projects view's navigation refresh, through the shared wrapper.
export function refreshProjectsView() {
  return panelRefresh('panel-projects', 'the projects register', async () => {
    await loadCustomers();
    await refreshProjectTable();
  });
}

export function fillParentCustomers(select, selectedId) {
  fillSelect(select, [
    { value: '', text: 'Choose a customer...' },
    ...state.customers
      .filter((customer) => customer.active !== false || customer.id === selectedId)
      .map((customer) => ({
        value: customer.id,
        text: customer.active !== false ? customer.name : `${customer.name} (inactive)`,
      })),
  ]);
  select.value = selectedId || '';
}

export function prefillProjectParent() {
  const cid = $('project-parent-customer').value;
  const customer = state.customers.find((customer) => customer.id === cid);
  $('project-currency').value = customer ? customer.currency : '';
  $('project-rate').value = customer ? formatMoney(customer.default_rate_minor) : '';
  $('project-dialog-parent').textContent = customer ? customer.name : '';
}

export async function fillTaskParents(customerId, projectCode, editing = false) {
  fillParentCustomers($('task-parent-customer'), customerId);
  $('task-parent-customer').disabled = editing;
  await fillProjectSelect($('task-parent-project'), customerId, projectCode);
  $('task-parent-project').disabled = editing;
  $('task-dialog-parent').textContent = customerId && projectCode ? `${customerName(customerId)} / ${projectCode}` : '';
}

export function startProjectEdit(cid, p) {
  // #182: the row carries its own customer (the table can span all of them).
  fillParentCustomers($('project-parent-customer'), cid);
  $('project-parent-customer').disabled = true;
  $('project-original-code').value = p.code;
  $('project-code').value = p.code;
  $('project-name').value = p.name;
  $('project-currency').value = p.currency || '';
  $('project-rate').value = p.rate_minor != null ? formatMoney(p.rate_minor) : '';
  $('project-active').checked = p.active;
  $('project-save').textContent = 'Update project';
  $('project-dialog-title').textContent = 'Edit project';
  clearFormError($('project-error'));
  if (!$('project-dialog').open) $('project-dialog').showModal();
  $('project-code').focus();
}

export async function saveProject(evt) {
  evt.preventDefault();
  clearFormError($('project-error'));
  const cid = $('project-parent-customer').value;
  const original = $('project-original-code').value;
  if (!cid) {
    showFormError($('project-error'), { message: 'Select a customer.' });
    return;
  }
  const code = $('project-code').value.trim().toUpperCase();
  const currency = $('project-currency').value.trim().toUpperCase();
  const rate = $('project-rate').value.trim();
  if (!currency || !rate) {
    showFormError($('project-error'), {
      message: 'Currency and hourly rate are required for a project.',
    });
    return;
  }
  const body = {
    code,
    name: $('project-name').value.trim(),
    currency,
    rate_minor: Math.round(Number(rate) * 100),
    active: $('project-active').checked,
  };
  try {
    if (original) await api.put(`/customers/${cid}/projects/${encodeURIComponent(original)}`, body);
    else await api.post(`/customers/${cid}/projects`, body);
    // #182: scope the section to the saved project's customer so the new or
    // renamed row is on screen; carry a rename into the Tasks filters.
    state.projectsFilter = cid;
    if (original && state.tasksCustomer === cid && state.tasksProject === original) {
      state.tasksProject = code;
    }
    await refreshProjectTable();
    announce(original ? 'Project updated.' : 'Project added.');
    cancelProjectEdit();
  } catch (err) {
    showFormError($('project-error'), err);
  }
}

export function cancelProjectEdit() {
  $('project-original-code').value = '';
  $('project-form').reset();
  $('project-active').checked = true;
  $('project-save').textContent = 'Add project';
  $('project-dialog-title').textContent = 'Add project';
  const customer = state.customers.find((customer) => customer.id === state.projectsFilter);
  if (customer) {
    $('project-currency').value = customer.currency;
    $('project-rate').value = formatMoney(customer.default_rate_minor);
  }
  $('project-dialog').close();
  $('project-new').focus();
  clearFormError($('project-error'));
}

export async function removeProject(cid, p) {
  if (!(await askConfirm(`Delete project ${p.code}?`))) return;
  try {
    await api.del(`/customers/${cid}/projects/${encodeURIComponent(p.code)}`);
    // Dependent reset (#182): the Tasks filter must not point at a deleted
    // project — drop it back to "all projects" and re-render.
    if (state.tasksCustomer === cid && state.tasksProject === p.code) {
      state.tasksProject = null;
    }
    await refreshProjectTable();
    await refreshTaskTable();
    announce('Project deleted.');
  } catch (err) {
    await askAlert(err.message); // #189: was double-reported (announce + dialog)
  }
}

// ---------------------------------------------------------------- tasks ---

// Project row / "Tasks" action: open the Tasks section with both parent
// filters set (#182). Explicit refresh for the same switchTab reason.
export async function openTasksFor(cid, pcode) {
  state.tasksCustomer = cid || null;
  state.tasksProject = pcode || null;
  // Same reason as openProjectsFor: the row action is a jump to that record's
  // children, not a request to keep an unrelated status filter.
  state.tasksStatus = '';
  switchTab('tab-tasks');
  await loadCustomers();
  await refreshTaskTable();
}

export async function refreshTaskTable() {
  // #182: the section is filtered by customer and project. Both default to
  // "all" — an unset filter is a SCOPE here, not a missing parent — so the list
  // is never blocked. The API lists one project's tasks, so an "all" scope fans
  // out across the projects it covers.
  const isTaskCurrent = taskSeq();
  fillHierarchyFilter($('task-filter-customer'), state.tasksCustomer, 'All customers');
  $('task-filter-active').value = state.tasksStatus || '';
  const projSel = $('task-filter-project');
  // Projects belong to a customer, so concrete choices only exist once one is
  // picked; with "all customers" the pick rests on "All projects".
  const projects = state.tasksCustomer ? await loadProjects(state.tasksCustomer) : [];
  fillSelect(projSel, [
    { value: '', text: 'All projects' },
    ...projects.map((p) => ({ value: p.code, text: p.code, selected: p.code === state.tasksProject })),
  ]);
  projSel.disabled = false;
  projSel.value = state.tasksProject || '';
  if (!isTaskCurrent()) return; // #219
  updateHierarchyActions();

  const cids = state.tasksCustomer ? [state.tasksCustomer] : state.customers.map((c) => c.id);
  const allTasks = [];
  for (const cid of cids) {
    for (const p of await loadProjects(cid)) {
      if (state.tasksProject && p.code !== state.tasksProject) continue;
      const data = await api.get(
        `/customers/${cid}/projects/${encodeURIComponent(p.code)}/tasks`,
      );
      for (const t of data.tasks || []) {
        // A row must know its own parents whatever the filter says (#182).
        allTasks.push({
          ...t,
          customer_id: t.customer_id || cid,
          project_code: t.project_code || p.code,
        });
      }
    }
  }
  if (!isTaskCurrent()) return; // #219: a newer filter won

  const tasks = allTasks.filter((t) => matchesStatus(t, state.tasksStatus));
  const tbody = $('task-table').querySelector('tbody');
  tbody.textContent = '';
  // The scope spans projects, so each row names its parent project; project
  // codes are per customer, so across customers the name is part of that.
  const parentOf = (t) =>
    state.tasksCustomer ? t.project_code : `${customerName(t.customer_id)} / ${t.project_code}`;
  for (const t of tasks) {
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' }, text: t.code }),
        el('td', { attrs: { 'data-heading': 'Project' }, text: parentOf(t) }),
        el('td', { attrs: { 'data-heading': 'Name' }, text: t.name }),
        el('td', { attrs: { 'data-heading': 'Status' } }, [
          statusChip(t.active !== false ? 'Active' : 'Inactive', t.active !== false),
        ]),
        iconActionsCell([
          el('button', {
            cls: 'link',
            type: 'button',
            text: 'Edit',
            data: { icon: 'edit' },
            on: { click: () => startTaskEdit(t) },
          }),
          el('button', {
            cls: 'danger',
            type: 'button',
            text: 'Delete',
            data: { icon: 'delete' },
            on: { click: () => removeTask(t) },
          }),
        ]),
      ]),
    );
  }
  $('task-table').hidden = tasks.length === 0;
  const empty = $('tasks-empty-state');
  empty.hidden = tasks.length !== 0;
  if (tasks.length === 0) {
    const scope = state.tasksProject
      ? ' on this project'
      : state.tasksCustomer ? ` for ${customerName(state.tasksCustomer)}` : '';
    empty.textContent = state.tasksStatus
      ? `No ${statusWord(state.tasksStatus)} tasks${scope}.`
      : state.tasksProject
        ? 'No tasks on this project yet. Use + Task to add one.'
        : 'No tasks yet. Use + Task to add one.';
  }
}

// QA-10: the Tasks view's navigation refresh, through the shared wrapper.
export function refreshTasksView() {
  return panelRefresh('panel-tasks', 'the tasks register', async () => {
    await loadCustomers();
    await refreshTaskTable();
  });
}

export async function startTaskEdit(t) {
  // #182: a task knows its own parents (rows can outlive the filter state).
  await fillTaskParents(t.customer_id, t.project_code, true);
  $('task-original-code').value = t.code;
  $('task-code').value = t.code;
  $('task-name').value = t.name;
  $('task-active').checked = t.active;
  $('task-save').textContent = 'Update task';
  $('task-dialog-title').textContent = 'Edit task';
  clearFormError($('task-error'));
  if (!$('task-dialog').open) $('task-dialog').showModal();
  $('task-code').focus();
}

export function cancelTaskEdit() {
  $('task-original-code').value = '';
  $('task-form').reset();
  $('task-active').checked = true;
  $('task-save').textContent = 'Add task';
  $('task-dialog-title').textContent = 'Add task';
  $('task-dialog').close();
  $('task-new').focus();
  clearFormError($('task-error'));
}

export async function saveTask(evt) {
  evt.preventDefault();
  clearFormError($('task-error'));
  const cid = $('task-parent-customer').value;
  const pcode = $('task-parent-project').value;
  const original = $('task-original-code').value;
  if (!cid || !pcode) {
    showFormError($('task-error'), { message: 'Select a customer and project.' });
    return;
  }
  const code = $('task-code').value.trim().toUpperCase();
  const body = { code, name: $('task-name').value.trim(), active: $('task-active').checked };
  const base = `/customers/${cid}/projects/${encodeURIComponent(pcode)}/tasks`;
  try {
    if (original) await api.put(`${base}/${encodeURIComponent(original)}`, body);
    else await api.post(base, body);
    // #182: land the filters on whatever the dialog actually saved.
    state.tasksCustomer = cid;
    state.tasksProject = pcode;
    await refreshTaskTable();
    announce(original ? 'Task updated.' : 'Task added.');
    cancelTaskEdit();
  } catch (err) {
    showFormError($('task-error'), err);
  }
}

export async function removeTask(t) {
  if (!(await askConfirm(`Delete task ${t.code}?`))) return;
  const cid = t.customer_id; // #182: from the row, not shared selection state
  const pcode = t.project_code;
  try {
    await api.del(
      `/customers/${cid}/projects/${encodeURIComponent(pcode)}/tasks/${encodeURIComponent(t.code)}`,
    );
    await refreshTaskTable();
    announce('Task deleted.');
  } catch (err) {
    await askAlert(err.message); // #189: was double-reported (announce + dialog)
  }
}

// #182: section filters. Changing/clearing a parent resets its children.
$('customer-filter-active').addEventListener('change', async () => {
  state.customersStatus = $('customer-filter-active').value;
  await refreshCustomerTable();
  announce(state.customersStatus
    ? `Showing ${statusWord(state.customersStatus)} customers.`
    : 'Showing all customers.');
});
$('proj-filter-active').addEventListener('change', async () => {
  state.projectsStatus = $('proj-filter-active').value;
  await refreshProjectTable();
  announce(state.projectsStatus
    ? `Showing ${statusWord(state.projectsStatus)} projects.`
    : 'Showing all projects.');
});
$('task-filter-active').addEventListener('change', async () => {
  state.tasksStatus = $('task-filter-active').value;
  await refreshTaskTable();
  announce(state.tasksStatus
    ? `Showing ${statusWord(state.tasksStatus)} tasks.`
    : 'Showing all tasks.');
});
$('proj-filter-customer').addEventListener('change', async () => {
  state.projectsFilter = $('proj-filter-customer').value || null;
  await refreshProjectTable();
  announce(state.projectsFilter
    ? `Projects filtered to ${customerName(state.projectsFilter)}.`
    : 'Showing projects for all customers.');
});
$('task-filter-customer').addEventListener('change', async () => {
  state.tasksCustomer = $('task-filter-customer').value || null;
  state.tasksProject = null; // dependent reset (#182)
  await refreshTaskTable();
  announce(state.tasksCustomer
    ? `Tasks for ${customerName(state.tasksCustomer)} — all projects.`
    : 'Tasks for all customers and projects.');
});
$('task-filter-project').addEventListener('change', async () => {
  state.tasksProject = $('task-filter-project').value || null;
  await refreshTaskTable();
  announce(state.tasksProject
    ? `Tasks for ${customerName(state.tasksCustomer)} / ${state.tasksProject}.`
    : state.tasksCustomer
      ? `All projects for ${customerName(state.tasksCustomer)}.`
      : 'Tasks for all customers and projects.');
});
export async function refreshCustomerPickers() {
  // C7: every customer picker (day form, invoices, expenses, timer) refreshes
  // from one place — previously a newly added customer could not be invoiced
  // until a full page reload.
  //
  // The invoice-register filter is a history surface, so it keeps inactive
  // customers; every picker that creates an operational record offers active
  // ones only. A form already editing an inactive customer still keeps that
  // value selectable (see fillCustomerSelect).
  for (const id of ['entry-customer', 'expense-customer', 'timer-customer', 'rec-customer', 'ed-customer', 'iw-customer', 'ret-customer', 'inv-f-customer']) {
    const picker = $(id);
    if (picker) {
      fillCustomerSelect(picker, picker.value, true, id === 'inv-f-customer');
      // #135: for the dashboard filter the placeholder means "no filter".
      if (id === 'inv-f-customer' && picker.options.length) picker.options[0].text = 'All customers';
    }
  }
  const keep = $('entry-customer').value;
  if (keep && !state.customers.some((c) => c.id === keep)) {
    await fillProjectSelect($('entry-project'), '', null);
  }
}

// ------------------------------------------------------------- reports ---

export function runReport(evt) {
  evt.preventDefault();
  // QA-10: a failed report fetch keeps the previous rows and shows the banner.
  // The form listener has no caller to hand the rejection to, so it stops here.
  return panelRefresh('panel-reports', 'the report', runReportBody).catch(() => { });
}

async function runReportBody() {
  const from = $('report-from').value;
  const to = $('report-to').value;
  const group = $('report-group').value;
  const billable = $('report-billable').value;
  let url = `/reports/summary?from=${from}&to=${to}&group=${group}`;
  if (billable) url += `&billable=${billable}`;
  const summary = await api.get(url);
  const tbody = $('report-table').querySelector('tbody');
  tbody.textContent = '';
  for (const row of summary.rows) {
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' }, text: row.label }),
        el('td', { text: row.currency }),
        el('td', { cls: 'num', text: row.hours.toFixed(2) }),
        el('td', { cls: 'num', text: `${row.currency} ${formatMoney(row.amount_minor)}` }),
        el('td', { cls: 'num', text: String(row.entries) }),
      ]),
    );
  }
  $('report-total').textContent = `${summary.total_hours.toFixed(2)}  (billable ${summary.billable_hours.toFixed(2)} / non-billable ${summary.nonbillable_hours.toFixed(2)})`;
  const link = $('csv-link');
  link.href = `${API_BASE}/reports/export.csv?from=${from}&to=${to}`;
  link.hidden = false;
  announce(`Report ready: ${summary.rows.length} rows.`);
}

