// #182: the Setup workspace split — three sections (Customers / Projects /
// Tasks), customer-scoped project filter, dependent customer+project filters
// on tasks, dependent resets and accessible empty/announced states. Offline
// jsdom against the real index.html + app.js.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

const customers = [
  { id: 'c1', name: 'Tucano Time', currency: 'EUR', default_rate_minor: 6000, active: true, email: '' },
  { id: 'c2', name: 'Globex', currency: 'USD', default_rate_minor: 4550, active: true, email: '' },
];
const projectsByCustomer = {
  c1: [
    { code: 'ACM', name: 'Web', currency: 'EUR', rate_minor: 6000, active: true },
    { code: 'MKT-2', name: 'Marketing', currency: 'EUR', rate_minor: 9500, active: true },
  ],
  c2: [
    { code: 'GX-1', name: 'Labs', currency: 'USD', rate_minor: 4550, active: true },
  ],
};
const tasksByProject = {
  'c1/MKT-2': [
    { code: 'W-1', name: 'Weekly', active: true, customer_id: 'c1', project_code: 'MKT-2' },
  ],
};

function setup({ customerList = customers } = {}) {
  const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
  dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
  const stub = `
window.calls = { post: [], put: [] };
api.get = async (path) => {
  // The app's DOMContentLoaded boot probes /auth/status. The offline harness
  // drives the wiring directly, so leave that probe pending: otherwise the
  // first-run admin wizard opens at the first await and moves focus away from
  // the dialog under test.
  if (path === '/auth/status') return new Promise(() => { });
  if (path === '/customers') return { customers: ${JSON.stringify(customerList)} };
  let m = path.match(/^\\/customers\\/([^/]+)\\/projects$/);
  if (m) return { projects: ${JSON.stringify(projectsByCustomer)}[m[1]] || [] };
  m = path.match(/^\\/customers\\/([^/]+)\\/projects\\/([^/]+)\\/tasks$/);
  if (m) return { tasks: ${JSON.stringify(tasksByProject)}[m[1] + '/' + decodeURIComponent(m[2])] || [] };
  return {};
};
api.post = async (path, body) => { window.calls.post.push({ path, body }); return body; };
api.put = async (path, body) => { window.calls.put.push({ path, body }); return body; };
initTabs();
window.h = {
  state, openProjectsFor, openTasksFor, refreshProjectTable, refreshTaskTable,
  switchTab, api, calls: () => window.calls, wireSetupPanels,
  startCustomerEdit, cancelCustomerEdit, saveCustomer,
  setPaymentRails, readPaymentRails, addPaymentRail, removePaymentRail,
  paymentRailRow, paymentRailRows, renumberPaymentRails, validatePaymentRails,
};`;
  dom.window.eval(`${app}\n${stub}`);
  return { dom, h: dom.window.h, document: dom.window.document };
}

// #265: wait for the render a filter/navigation call started (the state the
// assertions read) instead of sleeping a fixed tick that only passed when the
// runner happened to be fast.
const rowsIn = (document, selector) => [...document.querySelectorAll(selector)];

test('Setup exposes three sibling sections with tab/panel wiring (#182)', () => {
  const { dom, document } = setup();
  const tabs = ['tab-customers', 'tab-projects', 'tab-tasks']
    .map((id) => document.getElementById(id));
  assert.ok(tabs.every((t) => t && t.getAttribute('role') === 'tab'), 'three tab buttons');
  const panels = tabs.map((t) => document.getElementById(t.getAttribute('aria-controls')));
  assert.ok(panels.every((p) => p && p.getAttribute('role') === 'tabpanel'), 'matching panels');
  // The rail's visible order: Customers/Projects/Tasks sit together in the Setup
  // group, expenses belongs to Billing, and the tools (Reports) are grouped in
  // the collapsed disclosure while Settings is pinned last, at the rail's bottom.
  // #16 (Approvals) is hidden for now — the approval process is not implemented —
  // so it keeps its slot and panel while dropping out of that order.
  assert.deepEqual(
    [...document.querySelectorAll('#tabs [role="tab"]')]
      .filter((t) => !t.hidden).map((t) => t.id),
    ['tab-timesheet', 'tab-customers', 'tab-projects', 'tab-tasks',
      'tab-invoices', 'tab-expenses', 'tab-settings'],
    'Customers/Projects/Tasks sit together in the Setup group',
  );
  assert.ok(document.getElementById('tab-reports').closest('details'),
    'Reports groups with the tools in the disclosure');
  assert.equal(document.getElementById('tab-settings').closest('details'), null,
    'Settings is a destination, not a tool');
  assert.equal(document.getElementById('tab-submissions').hidden, true,
    'Approvals stays on the rail, hidden, until its process exists (#16)');
  assert.equal(document.getElementById('tab-reports').hidden, true,
    'Reports stays on the rail, hidden, until its suite is reimplemented');
  dom.window.close();
});

test('section navigation shows one panel at a time and scopes project rows (#182)', async () => {
  const { dom, h, document } = setup();
  await h.openProjectsFor(null); // all customers
  await waitFor(() => document.getElementById('panel-projects').hidden === false
    && rowsIn(document, '#project-table tbody tr').length === 3, {
    message: 'the all-customers project list to render',
  });
  assert.equal(document.getElementById('panel-projects').hidden, false);
  assert.equal(document.getElementById('panel-customers').hidden, true);
  const allRows = [...document.querySelectorAll('#project-table tbody tr')];
  assert.equal(allRows.length, 3, 'all customers by default');
  assert.equal(allRows[0].cells[1].textContent, 'Tucano Time', 'Customer column present');
  // Cross-navigation from the Customers section filters the Projects section.
  await h.openProjectsFor('c2');
  await waitFor(() => rowsIn(document, '#project-table tbody tr').length === 1, {
    message: 'the project list to re-render scoped to Globex',
  });
  const scoped = [...document.querySelectorAll('#project-table tbody tr')];
  assert.equal(scoped.length, 1);
  assert.equal(scoped[0].cells[0].textContent.trim(), 'GX-1');
  assert.equal(document.getElementById('project-scope-label'), null,
    'the old scope caption is gone: the filter select itself carries the scope');
  assert.equal(document.getElementById('proj-filter-customer').value, 'c2', 'filter select reflects scope');
  // Clearing the filter returns to the all-customers view (#182 DoD).
  const sel = document.getElementById('proj-filter-customer');
  sel.value = '';
  sel.dispatchEvent(new dom.window.Event('change', { bubbles: true }));
  await waitFor(() => rowsIn(document, '#project-table tbody tr').length === 3, {
    message: 'clearing the filter to re-render all customers',
  });
  assert.equal(document.querySelectorAll('#project-table tbody tr').length, 3);
  assert.match(document.getElementById('live-region').textContent, /all customers/i);
  dom.window.close();
});

test('tasks filters scope by customer and project, and both default to all (#182)', async () => {
  const { dom, h, document } = setup();
  await h.openTasksFor('c1', 'MKT-2');
  await waitFor(() => document.getElementById('panel-tasks').hidden === false
    && document.getElementById('task-filter-project').value === 'MKT-2', {
    message: 'the task list to re-render scoped to Tucano Time / MKT-2',
  });
  assert.equal(document.getElementById('panel-tasks').hidden, false);
  assert.equal(document.getElementById('task-filter-customer').value, 'c1');
  const proj = document.getElementById('task-filter-project');
  assert.equal(proj.disabled, false, 'the project filter is always usable');
  assert.deepEqual(
    [...proj.options].map((o) => o.value),
    ['', 'ACM', 'MKT-2'],
    'project choices are scoped to the selected customer only',
  );
  assert.equal(proj.options[0].textContent, 'All projects', 'the child filter offers "all"');
  assert.equal(document.getElementById('task-filter-customer').options[0].textContent, 'All customers');
  assert.equal(proj.value, 'MKT-2');
  assert.equal(document.querySelector('#task-table tbody tr th').textContent, 'W-1');
  assert.equal(document.getElementById('tasks-empty-state').hidden, true);

  // #182-follow-up: clearing the customer widens to every customer and project
  // rather than blocking the list — an unset filter is a scope, not a gap.
  const cust = document.getElementById('task-filter-customer');
  cust.value = '';
  cust.dispatchEvent(new dom.window.Event('change', { bubbles: true }));
  await waitFor(() => proj.value === '' && document.getElementById('task-table').hidden === false
    && /all customers and projects/i.test(document.getElementById('live-region').textContent), {
    message: 'clearing the customer to widen the task list',
  });
  assert.equal(proj.value, '', 'child selection reset with the parent');
  assert.equal(proj.disabled, false, 'the project pick stays usable ("All projects")');
  assert.deepEqual(
    [...proj.options].map((o) => o.textContent),
    ['All projects'],
    'with no customer picked, no concrete project can be named',
  );
  assert.equal(rowsIn(document, '#task-table tbody tr').length, 1,
    'the unscoped list shows every task (W-1 lives in Tucano Time / MKT-2)');
  assert.equal(document.getElementById('tasks-empty-state').hidden, true);
  assert.match(document.getElementById('live-region').textContent, /all customers and projects/i);
  dom.window.close();
});

test('an all-projects scope lists a customer\'s tasks and names each row\'s project (#182)', async () => {
  const { dom, h, document } = setup();
  await h.openTasksFor('c1', '');
  // Customer picked, project left on "all": every task of that customer lists,
  // each row naming the project it came from.
  await waitFor(() => rowsIn(document, '#task-table tbody tr').length === 1, {
    message: 'the all-projects task list to render Tucano Time\'s tasks',
  });
  assert.equal(document.getElementById('task-filter-project').options[0].textContent, 'All projects');
  assert.equal(
    document.querySelector('#task-table tbody td[data-heading="Project"]').textContent,
    'MKT-2',
    'a row names its parent project when the scope spans projects',
  );
  assert.equal(document.getElementById('tasks-empty-state').hidden, true);
  const proj = document.getElementById('task-filter-project');
  proj.value = 'MKT-2';
  proj.dispatchEvent(new dom.window.Event('change', { bubbles: true }));
  await waitFor(() => document.querySelector('#task-table tbody tr th')?.textContent === 'W-1'
    && /Tucano Time \/ MKT-2/.test(document.getElementById('live-region').textContent), {
    message: 'the scoped task list to render its W-1 row',
  });
  assert.equal(document.querySelector('#task-table tbody tr th').textContent, 'W-1');
  assert.match(document.getElementById('live-region').textContent, /Tucano Time \/ MKT-2/);
  dom.window.close();
});

test('refreshProjectTable is the single owner of the filter select value (#219)', async () => {
  // saveProject re-scopes state without touching the select; the render must
  // still leave the control truthful (before #219 it showed a stale value).
  const { dom, h, document } = setup();
  await h.openProjectsFor('c1');
  await waitFor(() => document.getElementById('proj-filter-customer').value === 'c1', {
    message: 'the project list to render scoped to Tucano Time',
  });
  h.state.projectsFilter = 'c2'; // what saveProject does after saving elsewhere
  await h.refreshProjectTable();
  await waitFor(() => document.getElementById('proj-filter-customer').value === 'c2'
    && rowsIn(document, '#project-table tbody tr').length === 1, {
    message: 'the re-render to re-sync the filter select from state',
  });
  assert.equal(document.getElementById('proj-filter-customer').value, 'c2',
    'select re-syncs from state on every render');
  assert.equal(document.querySelectorAll('#project-table tbody tr').length, 1);
  dom.window.close();
});

test('the customer legal id round-trips on create and edit (#354)', async () => {
  const edited = {
    id: 'c1', name: 'Tucano Time', currency: 'EUR', default_rate_minor: 6000, active: true,
    email: '', legal_id: 'BE-0999',
  };
  const { dom, h, document } = setup({ customerList: [edited] });
  const field = (id, value) => { document.getElementById(id).value = value; };

  // Create: the dialog's legal id is sent as `legal_id`.
  h.cancelCustomerEdit();
  field('customer-name', 'Tucano Time');
  field('customer-currency', 'EUR');
  field('customer-rate', '90');
  field('customer-legal-id', 'BR-12345678');
  await h.saveCustomer({ preventDefault() { } });
  const created = h.calls().post[0];
  assert.equal(created.path, '/customers');
  assert.equal(created.body.legal_id, 'BR-12345678', 'create sends the legal id');

  // Edit: the stored id is loaded, the label resolves it, and the change is PUT.
  h.startCustomerEdit(edited);
  const input = document.getElementById('customer-legal-id');
  assert.equal(input.value, 'BE-0999', 'edit loads the stored legal id');
  assert.equal(input.maxLength, 60, 'the input is bounded like the API field');
  assert.equal(input.labels.length, 1);
  assert.match(input.labels[0].textContent, /legal \/ registration\s+id/i);
  assert.match(document.getElementById('customer-legal-id-help').textContent, /Company number/);
  field('customer-legal-id', 'BE-1000');
  await h.saveCustomer({ preventDefault() { } });
  const updated = h.calls().put[0];
  assert.equal(updated.path, '/customers/c1');
  assert.equal(updated.body.legal_id, 'BE-1000', 'edit sends the changed legal id');
  dom.window.close();
});

test('payment rails add, remove, renumber and validate against the right input (#354)', async () => {
  const { dom, h, document } = setup();
  const rows = () => [...document.querySelectorAll('#pay-rails .payment-rail')];
  h.wireSetupPanels('2026-10-19');
  h.cancelCustomerEdit();
  // The rails live in the customer dialog, which the app opens before Add is
  // reachable; jsdom only focuses controls inside an open dialog (the setup
  // stub's showModal records that state).
  document.getElementById('customer-dialog').showModal();

  // Add: a real button appends a labelled fieldset and focuses its currency.
  const add = document.getElementById('pay-rail-add');
  assert.equal(add.tagName, 'BUTTON');
  assert.equal(add.type, 'button');
  // The shell wraps long labels across source lines; rendered/accessible
  // text collapses whitespace, so compare that form.
  assert.ok(add.textContent.replace(/\s+/g, ' ').includes('Add payment rail'));
  add.click();
  assert.equal(rows().length, 1);
  assert.equal(document.activeElement.id, 'pay-rail-0-currency', 'focus lands on the new rail');
  for (const control of rows()[0].querySelectorAll('input, textarea')) {
    assert.equal(control.labels.length, 1, `${control.id} has exactly one label`);
    assert.equal(control.labels[0].getAttribute('for'), control.id);
  }

  // Two rails, then remove the first: ids and accessible names renumber.
  add.click();
  document.getElementById('pay-rail-0-currency').value = 'eur';
  document.getElementById('pay-rail-0-account-name').value = 'Tucano Time EUR';
  document.getElementById('pay-rail-0-iban').value = 'EX000000000000000000';
  document.getElementById('pay-rail-1-currency').value = 'USD';
  document.getElementById('pay-rail-1-account-name').value = 'Tucano Time USD';
  const firstRemove = rows()[0].querySelector('button');
  assert.equal(firstRemove.tagName, 'BUTTON');
  assert.equal(firstRemove.getAttribute('aria-label'), 'Remove payment rail 1');
  h.removePaymentRail(rows()[0]);
  assert.equal(rows().length, 1);
  assert.equal(document.getElementById('pay-rail-0-currency').value, 'USD', 'the remaining rail renumbers to index 0');
  assert.equal(rows()[0].querySelector('button').getAttribute('aria-label'), 'Remove payment rail 1');

  // The list is bounded at four with the add control disabled.
  add.click();
  add.click();
  add.click();
  assert.equal(rows().length, 4);
  assert.equal(add.disabled, true);
  assert.match(document.getElementById('pay-rails-status').textContent, /All 4 payment rails are in use/);
  assert.equal(h.addPaymentRail(), null, 'a fifth rail is refused');
  while (rows().length > 0) h.removePaymentRail(rows()[0]);
  assert.equal(add.disabled, false);

  // Validation: a short IBAN is refused before the API is called, and the
  // error marks the row input named by the API's field path.
  h.setPaymentRails([{ currency: 'EUR', account_name: 'Tucano', iban: 'AB' }]);
  document.getElementById('customer-name').value = 'Rails Co';
  document.getElementById('customer-currency').value = 'EUR';
  document.getElementById('customer-rate').value = '80';
  await h.saveCustomer({ preventDefault() { } });
  assert.equal(h.calls().post.length, 0, 'an invalid rail never reaches the server');
  assert.match(document.getElementById('customer-error').textContent,
    /payment_details\.rails\[0\]\.iban: must be 5-34 letters\/digits/);
  assert.equal(document.getElementById('pay-rail-0-iban').getAttribute('aria-invalid'), 'true');
  assert.equal(document.activeElement.id, 'pay-rail-0-iban', 'focus moves to the field to fix');

  // Round trip: currency upper-cases, a blank optional field is omitted and an
  // untouched added row is not stored as a rail.
  document.getElementById('pay-rail-0-iban').value = 'de89370400440532013000';
  add.click(); // a blank row: not a rail
  await h.saveCustomer({ preventDefault() { } });
  const body = h.calls().post[0].body;
  assert.equal(body.payment_details.rails.length, 1, 'the blank row is dropped');
  assert.deepEqual(JSON.parse(JSON.stringify(body.payment_details.rails[0])), {
    currency: 'EUR',
    account_name: 'Tucano',
    iban: 'de89370400440532013000',
  }, 'uppercase currency, optional blanks omitted, required keys present');
  dom.window.close();
});
