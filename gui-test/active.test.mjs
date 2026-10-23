// Active/inactive reference data in the pickers.
//
// Inactive customers/projects/tasks are retired from NEW records (timesheets,
// expenses, timers, retainers, recurring schedules, invoices) so the lists stay
// short, while the stored documents remain readable and editable — a form
// already referencing an inactive value keeps it selectable, and the invoice
// register/filter still names the full history. Offline jsdom against the real
// index.html and modules.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

const customers = [
  { id: 'c1', name: 'Active Co', currency: 'EUR', default_rate_minor: 6000, active: true, email: '' },
  { id: 'c2', name: 'Retired Co', currency: 'EUR', default_rate_minor: 6000, active: false, email: '' },
];
const projectsByCustomer = {
  c1: [
    { code: 'P1', name: 'Current work', currency: 'EUR', rate_minor: 6000, active: true },
    { code: 'P2', name: 'Old work', currency: 'EUR', rate_minor: 6000, active: false },
  ],
  c2: [
    { code: 'P2', name: 'Old work', currency: 'EUR', rate_minor: 6000, active: false },
  ],
};
const tasksByProject = {
  'c1/P1': [
    { code: 'T1', name: 'Current task', active: true, customer_id: 'c1', project_code: 'P1' },
    { code: 'T2', name: 'Old task', active: false, customer_id: 'c1', project_code: 'P1' },
  ],
  'c2/P2': [
    { code: 'T2', name: 'Old task', active: false, customer_id: 'c2', project_code: 'P2' },
  ],
};

function setup() {
  const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
  dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
  dom.window.Element.prototype.scrollIntoView = function () { };
  const stub = `
window.calls = { put: [] };
api.get = async (path) => {
  if (path === '/auth/status') return new Promise(() => { });
  if (path === '/customers') return { customers: ${JSON.stringify(customers)} };
  let m = path.match(/^\\/customers\\/([^/]+)\\/projects$/);
  if (m) return { projects: ${JSON.stringify(projectsByCustomer)}[m[1]] || [] };
  m = path.match(/^\\/customers\\/([^/]+)\\/projects\\/([^/]+)\\/tasks$/);
  if (m) return { tasks: ${JSON.stringify(tasksByProject)}[m[1] + '/' + decodeURIComponent(m[2])] || [] };
  return {};
};
api.put = async (path, body) => { window.calls.put.push({ path, body }); return body; };
state.customers = ${JSON.stringify(customers)};
initTabs();
window.h = {
  state, fillCustomerSelect, fillProjectSelect, fillTaskSelect,
  refreshCustomerPickers, resetEntryForm, startEdit, edOpen,
};`;
  dom.window.eval(`${app}\n${stub}`);
  return { dom, h: dom.window.h, document: dom.window.document };
}

// Read option arrays in the test realm: the harness's own arrays belong to the
// jsdom realm and would fail `deepStrictEqual` on prototype identity alone.
const values = (document, id) => [...document.getElementById(id).options].map((o) => o.value);
const texts = (document, id) => [...document.getElementById(id).options].map((o) => o.textContent);

test('new-entry pickers offer active references only', async () => {
  const { dom, h, document } = setup();
  h.fillCustomerSelect(document.getElementById('entry-customer'), '', true);
  assert.deepEqual(values(document, 'entry-customer'), ['', 'c1'], 'retired customer is not offered for a new entry');
  assert.deepEqual(texts(document, 'entry-customer'), ['Choose a customer…', 'Active Co']);

  await h.fillProjectSelect(document.getElementById('entry-project'), 'c1', '');
  assert.deepEqual(values(document, 'entry-project'), ['', 'P1'], 'retired project is not offered for a new entry');

  await h.fillTaskSelect(document.getElementById('entry-task'), 'c1', 'P1', '');
  assert.deepEqual(values(document, 'entry-task'), ['', 'T1'], 'retired task is not offered for a new entry');
  dom.window.close();
});

test('a record being edited keeps its inactive customer, project and task', async () => {
  const { dom, h, document } = setup();
  // Simulate the picker refresh that populated the form before the edit.
  h.fillCustomerSelect(document.getElementById('entry-customer'), 'c2', true);
  await h.fillProjectSelect(document.getElementById('entry-project'), 'c1', 'P2');
  await h.fillTaskSelect(document.getElementById('entry-task'), 'c1', 'P1', 'T2');

  assert.deepEqual(values(document, 'entry-customer'), ['', 'c1', 'c2'], 'the edited (inactive) customer remains selectable alongside active ones');
  assert.match(texts(document, 'entry-customer')[2], /\(inactive\)$/);
  assert.deepEqual(values(document, 'entry-project'), ['', 'P1', 'P2'], 'the edited (inactive) project remains selectable alongside active ones');
  assert.match(texts(document, 'entry-project')[2], /\(inactive\)$/);
  assert.deepEqual(values(document, 'entry-task'), ['', 'T1', 'T2'], 'the edited (inactive) task remains selectable alongside active ones');
  assert.match(texts(document, 'entry-task')[2], /\(inactive\)$/);

  // Editing an entry rebuilds the customer picker around its own reference.
  h.startEdit({
    id: 'e1', date: '2026-10-20', customer_id: 'c2', project_code: 'P2', task_code: 'T2',
    hours: 1, note: '', billable: true,
  });
  await waitFor(() => document.getElementById('entry-project').value === 'P2'
    && document.getElementById('entry-task').value === 'T2', {
    message: 'the edit dialog to load the inactive references',
  });
  assert.equal(document.getElementById('entry-customer').value, 'c2');
  assert.equal(document.getElementById('entry-project').value, 'P2');
  assert.equal(document.getElementById('entry-task').value, 'T2');
  dom.window.close();
});

test('history and filter surfaces keep inactive references', async () => {
  const { dom, h, document } = setup();
  // The invoice register filter is a history surface: it must be able to name
  // a retired customer so their old invoices remain reachable.
  await h.refreshCustomerPickers();
  assert.deepEqual(values(document, 'inv-f-customer'), ['', 'c1', 'c2']);
  // A NEW invoice starts from active customers only...
  assert.deepEqual(values(document, 'iw-customer'), ['', 'c1']);
  assert.deepEqual(values(document, 'ed-customer'), ['', 'c1']);
  // ...but editing an existing invoice rebuilds the picker around its own
  // (possibly retired) customer instead of losing it.
  h.edOpen({
    id: 'inv1', number: 'INV-0001', customer_id: 'c2', status: 'draft',
    currency: 'EUR', lines: [],
  });
  assert.equal(document.getElementById('ed-customer').value, 'c2');
  assert.deepEqual(values(document, 'ed-customer'), ['', 'c1', 'c2']);
  // Operational pickers stay short, even after the common refresh.
  assert.deepEqual(values(document, 'entry-customer'), ['', 'c1']);
  assert.deepEqual(values(document, 'timer-customer'), ['', 'c1']);
  assert.deepEqual(values(document, 'rec-customer'), ['', 'c1']);
  assert.deepEqual(values(document, 'ret-customer'), ['', 'c1']);
  dom.window.close();
});

test('a new entry never inherits a retired customer from the previous edit', () => {
  const { dom, h, document } = setup();
  const picker = document.getElementById('entry-customer');
  h.fillCustomerSelect(picker, 'c2', true);
  assert.equal(picker.value, 'c2');
  h.resetEntryForm();
  assert.equal(picker.value, '', 'the Add form starts without the retired customer');
  assert.deepEqual(values(document, 'entry-customer'), ['', 'c1']);
  dom.window.close();
});
