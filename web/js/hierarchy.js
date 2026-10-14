// hierarchy.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { API_BASE, api } from './api.js';
import { iw } from './billing.js';
import { refreshPanel, switchTab } from './boot.js';
import { $, announce, askAlert, askConfirm, clearFormError, el, fillSelect, formatMoney, showFormError, statusChip } from './dom.js';
import { ed } from './invoices.js';
import { createSeqGuard } from './seq.js';
import { customerName, fillCustomerSelect, fillProjectSelect, loadCustomers, loadProjects, state } from './state.js';
import { jumpToEntry } from './timesheet.js';

const projectSeq = createSeqGuard();
const taskSeq = createSeqGuard();

// ---------------------------------------------------------- customers ---

export async function refreshCustomerTable() {
  const tbody = $('customer-table').querySelector('tbody');
  tbody.textContent = '';
  for (const c of state.customers) {
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' } }, [
          // #182: no selection state — the action opens the Projects section
          // filtered to this customer.
          el('button', { type: 'button', cls: 'link', text: c.name,
            on: { click: () => openProjectsFor(c.id) } }),
        ]),
        el('td', { cls: 'num', attrs: { 'data-heading': 'Default rate' }, text: `${c.currency} ${formatMoney(c.default_rate_minor)}` }),
        el('td', { attrs: { 'data-heading': 'Status' } }, [
          statusChip(c.active ? 'Active' : 'Inactive', c.active),
        ]),
        el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, [
          el('button', {
            cls: 'link',
            type: 'button',
            text: 'Projects',
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
}

export function startCustomerEdit(c) {
  $('customer-id').value = c.id;
  $('customer-name').value = c.name;
  $('customer-currency').value = c.currency;
  $('customer-rate').value = (c.default_rate_minor / 100).toFixed(2);
  $('customer-active').checked = c.active;
  $('customer-email').value = c.email || '';
  // Invoice document fields (#116).
  const terms = c.payment_terms || null;
  $('customer-terms').value = terms ? terms.kind : '';
  $('customer-terms-days').value = terms && terms.days != null ? terms.days : '';
  $('customer-terms-days-field').hidden = !terms || terms.kind !== 'custom';
  $('customer-subject').value = c.invoice_subject || '';
  $('customer-notes').value = c.invoice_notes || '';
  const a = c.address || {};
  $('cust-street').value = a.street || '';
  $('cust-city').value = a.city || '';
  $('cust-postal').value = a.postal_code || '';
  $('cust-country').value = a.country || '';
  $('cust-tax').value = c.tax_hundredths ? (c.tax_hundredths / 100).toFixed(2) : '0';
  $('cust-discount').value = c.discount_hundredths ? (c.discount_hundredths / 100).toFixed(2) : '0';
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

export async function saveCustomer(evt) {
  evt.preventDefault();
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
  const body = {
    name: $('customer-name').value.trim(),
    currency: $('customer-currency').value.trim().toUpperCase(),
    default_rate_minor: Math.round(Number($('customer-rate').value) * 100),
    active: $('customer-active').checked,
    email: $('customer-email').value.trim(),
    payment_terms,
    invoice_subject: $('customer-subject').value.trim(),
    invoice_notes: $('customer-notes').value.trim(),
    // #139: percent inputs are decimal but persist as integer hundredths.
    address: street || city || postal || country ? { street, city, postal_code: postal, country } : null,
    contacts: readContacts(),
    tax_hundredths: Math.round(Number($('cust-tax').value || 0) * 100),
    discount_hundredths: Math.round(Number($('cust-discount').value || 0) * 100),
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
      // Dependent reset (#182 DoD): clearing the parent filter clears the
      // child selection and restores the section's empty state.
      state.tasksCustomer = null;
      state.tasksProject = null;
      $('task-filter-customer').value = '';
      $('task-filter-project').textContent = '';
      $('task-filter-project').disabled = true;
      $('task-filter-project').appendChild(el('option', { attrs: { value: '' }, text: 'Choose a customer first…' }));
      $('task-table').hidden = true;
      $('tasks-empty-state').hidden = false;
    }
    delete state.projectsByCustomer[c.id];
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
    ...state.customers.map((c) => ({ value: c.id, text: c.active ? c.name : `${c.name} (inactive)`, selected: c.id === selectedId })),
  ]);
  select.value = selectedId || '';
}

// Customer row / "Projects" action: open the Projects Setup section
// filtered to that customer (#182). switchTab suppresses refreshPanel (the
// caller refreshes explicitly — same contract as jumpToEntry), so the
// scoped load happens right here.
export async function openProjectsFor(cid) {
  state.projectsFilter = cid || null;
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
  const cids = state.projectsFilter ? [state.projectsFilter] : state.customers.map((c) => c.id);
  const rows = [];
  for (const cid of cids) {
    for (const p of await loadProjects(cid)) rows.push({ cid, p });
  }
  if (!isProjectCurrent()) return; // #219: newer render won
  $('project-scope-label').textContent = state.projectsFilter
    ? `— ${customerName(state.projectsFilter)}` : '— all customers';
  const tbody = $('project-table').querySelector('tbody');
  tbody.textContent = '';
  for (const { cid, p } of rows) {
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' } }, [
          el('button', { type: 'button', cls: 'link', text: p.code,
            attrs: { title: `Manage tasks for ${p.code}` },
            on: { click: () => openTasksFor(cid, p.code) } }),
        ]),
        el('td', { attrs: { 'data-heading': 'Customer' }, text: customerName(cid) }),
        el('td', { attrs: { 'data-heading': 'Name' }, text: p.name }),
        el('td', { attrs: { 'data-heading': 'Currency' }, text: p.currency || 'customer' }),
        el('td', { cls: 'num', attrs: { 'data-heading': 'Rate' }, text: p.rate_minor != null ? formatMoney(p.rate_minor) : 'customer' }),
        el('td', { attrs: { 'data-heading': 'Status' } }, [
          statusChip(p.active ? 'Active' : 'Inactive', p.active),
        ]),
        el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, [
          el('button', {
            cls: 'link',
            type: 'button',
            text: 'Tasks',
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
    empty.textContent = state.projectsFilter
      ? `No projects for ${customerName(state.projectsFilter)} yet. Use + Project to add one.`
      : 'No projects yet. Use + Project to add one.';
  }
  updateHierarchyActions();
}

export function fillParentCustomers(select, selectedId) {
  fillSelect(select, [
    { value: '', text: 'Choose a customer...' },
    ...state.customers.map((customer) => ({ value: customer.id, text: customer.name })),
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
    // Dependent reset (#182): the Tasks filters must not point at a deleted
    // project — clear the child pick and its empty state takes over.
    if (state.tasksCustomer === cid && state.tasksProject === p.code) {
      state.tasksProject = null;
      $('task-filter-project').value = '';
      $('task-table').hidden = true;
      $('tasks-empty-state').hidden = false;
      updateHierarchyActions();
    }
    await refreshProjectTable();
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
  switchTab('tab-tasks');
  await loadCustomers();
  await refreshTaskTable();
}

export async function refreshTaskTable() {
  // #182: the section is driven by its two dependent filter selects; tasks
  // need both parents, so any missing pick shows the empty state.
  const isTaskCurrent = taskSeq();
  fillHierarchyFilter($('task-filter-customer'), state.tasksCustomer, 'Choose a customer…');
  const projSel = $('task-filter-project');
  projSel.disabled = !state.tasksCustomer;
  if (state.tasksCustomer) {
    const projects = await loadProjects(state.tasksCustomer);
    fillSelect(projSel, [
      { value: '', text: 'Choose a project…' },
      ...projects.map((p) => ({ value: p.code, text: p.code, selected: p.code === state.tasksProject })),
    ]);
    projSel.value = state.tasksProject || '';
  } else {
    fillSelect(projSel, [{ value: '', text: 'Choose a customer first…' }]);
  }
  if (!isTaskCurrent()) return; // #219
  updateHierarchyActions();
  const empty = $('tasks-empty-state');
  const both = state.tasksCustomer && state.tasksProject;
  if (!both) {
    $('task-scope-label').textContent = '';
    $('task-table').hidden = true;
    empty.hidden = false;
    // #182: the empty state explains the next step in the dependent pair.
    empty.textContent = !state.tasksCustomer
      ? 'Choose a customer and a project to list their tasks.'
      : 'Choose a project to list this customer’s tasks.';
    return;
  }
  $('task-scope-label').textContent = `— ${customerName(state.tasksCustomer)} / ${state.tasksProject}`;
  const data = await api.get(
    `/customers/${state.tasksCustomer}/projects/${encodeURIComponent(state.tasksProject)}/tasks`,
  );
  if (!isTaskCurrent()) return; // #219: a newer filter won
  const tasks = data.tasks || [];
  const tbody = $('task-table').querySelector('tbody');
  tbody.textContent = '';
  for (const t of tasks) {
    tbody.appendChild(
      el('tr', {}, [
        el('th', { attrs: { scope: 'row' }, text: t.code }),
        el('td', { attrs: { 'data-heading': 'Name' }, text: t.name }),
        el('td', { attrs: { 'data-heading': 'Status' } }, [
          statusChip(t.active ? 'Active' : 'Inactive', t.active),
        ]),
        el('td', { cls: 'actions-col', attrs: { 'data-heading': 'Actions' } }, [
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
  // #182: a selected project with no tasks still explains the next step.
  $('task-table').hidden = tasks.length === 0;
  empty.hidden = tasks.length !== 0;
  if (tasks.length === 0) {
    empty.textContent = 'No tasks on this project yet. Use + Task to add one.';
  }
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
    ? `Tasks: choose a project for ${customerName(state.tasksCustomer)}.`
    : 'Task filters cleared.');
});
$('task-filter-project').addEventListener('change', async () => {
  state.tasksProject = $('task-filter-project').value || null;
  await refreshTaskTable();
  announce(state.tasksProject
    ? `Tasks for ${customerName(state.tasksCustomer)} / ${state.tasksProject}.`
    : 'Project filter cleared — choose a project to list tasks.');
});
export async function refreshCustomerPickers() {
  // C7: every customer picker (day form, invoices, expenses, timer) refreshes
  // from one place — previously a newly added customer could not be invoiced
  // until a full page reload.
  for (const id of ['entry-customer', 'invoice-customer', 'expense-customer', 'timer-customer', 'rec-customer', 'ed-customer', 'iw-customer', 'ret-customer', 'inv-f-customer']) {
    const picker = $(id);
    if (picker) {
      fillCustomerSelect(picker, picker.value, true);
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

export async function runReport(evt) {
  evt.preventDefault();
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

