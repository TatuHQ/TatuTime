// #232 phase 2a: the remaining bespoke registers are now Vanilla p-card
// shells. Row-per-record registers adopt the official `p-table--mobile-card`
// pattern (the renderers supply the matching `data-heading` labels); the
// summary/reference registers keep a plain official table inside the card. This
// offline jsdom suite guards the classes, the p-card ancestry, the frozen
// column order and that each renderer still produces the same rows.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

// Card shells whose rows are the mobile-card register (columns stay frozen).
// SET-14 moved the runtime-config and template-variable summaries here, and the
// read-only audit log (SET-11) joined them.
const CARD_REGISTERS = {
    'user-table': ['Name', 'Email', 'Role', 'Status', 'Rates', 'Actions'],
    'submission-table': ['Week', 'Entries', 'State', 'Comment', 'Actions'],
    'secret-table': ['Key', 'Value', 'Actions'],
    'item-table': ['Name', 'Default price', 'Status', 'Actions'],
    'config-table': ['Setting', 'Value', 'Source', 'Applies'],
    'template-vars': ['Variable', 'Example'],
    'audit-table': ['Time', 'Event', 'Subject'],
    'rec-table': ['Customer', 'Cadence', 'Bills', 'Retainer', 'Last billed to', 'Status', 'Actions'],
    'ret-table': ['Customer / project', 'Balance', 'Status', 'Last activity', 'Actions'],
};
// Summary/reference tables: a plain official table inside the card.
const SUMMARY_REGISTERS = {
    'report-table': ['Group', 'Currency', 'Hours', 'Amount', 'Entries'],
};
// Deliberately app-owned: the timesheet grids and the editable line editor.
// The Invoices register is phase 2d — the real aria-sort + .p-table__sort-button
// pattern (there is no `p-table--sortable` class in Vanilla 4.59) — plus the
// mobile-card rows introduced for the phone layout. It is asserted separately
// below because its scroll wrapper is `.invoice-table-scroll`, not `.table-scroll`.
const LEFT_ALONE = ['day-table', 'week-table', 'cal-grid', 'ed-lines'];

function headers(table) {
    return [...table.querySelectorAll('thead th')].map((th) => th.textContent.trim());
}

function setup() {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
    const stub = `
window.reg = {
    api, state, renderUsers, refreshSubmissions, refreshCatalog, saveCatalogItem, refreshCustomerTable,
  refreshSchedules, refreshRetainers,
  setUsers: (list) => { managedUsers = list; },
};`;
    dom.window.eval(`${app}\n${stub}`);
    return { dom, document: dom.window.document, reg: dom.window.reg };
}

test('converted registers are p-card shells with unchanged columns (#232 phase 2a)', () => {
    const dom = new JSDOM(html, { url: 'http://localhost/' });
    const doc = dom.window.document;
    for (const [id, cols] of Object.entries(CARD_REGISTERS)) {
        const table = doc.getElementById(id);
        assert.ok(table, `#${id} exists`);
        assert.ok(table.classList.contains('p-table--mobile-card'), `#${id} uses p-table--mobile-card`);
        assert.ok(table.closest('.p-card'), `#${id} sits in a p-card`);
        assert.ok(table.closest('.table-scroll'), `#${id} keeps its overflow wrapper`);
        assert.deepEqual(headers(table), cols, `#${id} column order unchanged`);
    }
    for (const [id, cols] of Object.entries(SUMMARY_REGISTERS)) {
        const table = doc.getElementById(id);
        assert.ok(table, `#${id} exists`);
        assert.ok(table.closest('.p-card'), `#${id} sits in a p-card`);
        assert.ok(!table.classList.contains('p-table--mobile-card'), `#${id} stays a plain table`);
        assert.deepEqual(headers(table), cols, `#${id} column order unchanged`);
    }
    for (const id of LEFT_ALONE) {
        const table = doc.getElementById(id);
        assert.ok(table, `#${id} exists`);
        assert.ok(!table.classList.contains('p-table--mobile-card'), `#${id} left app-owned`);
    }
    dom.window.close();
});

test('invoice register uses the mobile-card rows without changing its columns', () => {
    const dom = new JSDOM(html, { url: 'http://localhost/' });
    const doc = dom.window.document;
    const table = doc.getElementById('invoice-table');
    assert.ok(table, '#invoice-table exists');
    assert.ok(table.classList.contains('p-table--mobile-card'), '#invoice-table uses p-table--mobile-card');
    assert.ok(table.closest('.invoice-table-scroll'), '#invoice-table keeps its own scroll wrapper');
    assert.deepEqual(headers(table),
        ['Number', 'Customer', 'Period', 'Total', 'Balance', 'Status', 'Actions'],
        '#invoice-table column order unchanged');
    dom.window.close();
});

test('phone shortcuts are present without taking desktop space', () => {
    const dom = new JSDOM(html, { url: 'http://localhost/' });
    const doc = dom.window.document;
    assert.ok(doc.getElementById('mobile-nav'), 'the phone bottom bar exists');
    assert.deepEqual(
        [...doc.querySelectorAll('#mobile-nav button')].map((b) => b.id),
        ['mnav-timesheets', 'mnav-invoices', 'mnav-add', 'mnav-menu'],
        'the bottom bar carries the four shortcuts');
    assert.equal(doc.querySelectorAll('#entry-quick [data-hours]').length, 5, 'entry dialog offers five quick durations');
    dom.window.close();
});

test('user register renders the same rows with mobile-card headings', () => {
    const { dom, document, reg } = setup();
    reg.setUsers([
        { id: 'u1', name: 'Ada Lovelace', email: 'ada@example.test', role: 'admin', active: true, default_rate_minor: 6000, cost_rate_minor: 3000 },
        { id: 'u2', name: 'Bob', email: 'bob@example.test', role: 'member', active: false, default_rate_minor: 0, cost_rate_minor: 0 },
    ]);
    reg.renderUsers();
    const rows = document.querySelectorAll('#user-table tbody tr');
    assert.equal(rows.length, 2, 'two fixture users render');
    assert.equal(rows[0].cells[0].textContent, 'Ada Lovelace');
    assert.equal(rows[0].querySelector('td[data-heading="Email"]').textContent, 'ada@example.test');
    assert.equal(rows[0].querySelector('td[data-heading="Rates"]').textContent, '60.00 / 30.00');
    assert.equal(rows[0].querySelector('td[data-heading="Actions"]').querySelectorAll('button').length, 4);
    assert.equal(document.getElementById('user-empty').hidden, true);
    dom.window.close();
});

test('icon actions retain accessible names, official tooltips and keyboard dismissal (#393)', async () => {
    const { dom, document, reg } = setup();
    reg.setUsers([{ id: 'u1', name: 'Ada', email: 'ada@example.test', role: 'admin', active: true, default_rate_minor: 0, cost_rate_minor: 0 }]);
    reg.renderUsers();
    const buttons = [...document.querySelectorAll('#user-table td[data-icon-actions="1"] button')];
    assert.deepEqual(buttons.map(button => button.getAttribute('aria-label')), ['Edit', 'Deactivate', 'Change password', 'Delete']);
    for (const button of buttons) {
        assert.ok(button.classList.contains('icon-action'));
        assert.ok(button.classList.contains('p-tooltip--top-center'));
        assert.ok(button.title);
        assert.ok(button.querySelector(`i.p-icon--${button.dataset.icon}[aria-hidden="true"]`));
        assert.equal([...button.childNodes].some(node => node.nodeType === 3 && node.textContent.trim()), false, 'no visible text node remains');
        const tooltip = document.getElementById(button.getAttribute('aria-describedby'));
        assert.equal(tooltip.getAttribute('role'), 'tooltip');
        assert.equal(tooltip.textContent, button.getAttribute('aria-label'));
        button.dispatchEvent(new dom.window.KeyboardEvent('keydown', { key: 'Escape' }));
        assert.ok(button.classList.contains('is-tooltip-dismissed'));
        button.dispatchEvent(new dom.window.FocusEvent('focus'));
        assert.equal(button.classList.contains('is-tooltip-dismissed'), false);
    }
    buttons[0].click();
    assert.equal(document.getElementById('user-id').value, 'u1', 'the original edit handler remains functional');
    reg.state.customers = [{ id: 'c1', name: 'Customer', currency: 'EUR', default_rate_minor: 0, active: true }];
    await reg.refreshCustomerTable();
    const project = document.querySelector('#customer-table button[aria-label="Projects"]');
    assert.equal(project.dataset.icon, 'menu');
    assert.equal(document.querySelector('#tab-invoices i').className, 'p-icon--credit-card');
    assert.equal(document.querySelector('#tab-expenses i').className, 'p-icon--gift');
    const css = readFileSync(new URL('../web/styles.css', import.meta.url), 'utf8');
    assert.match(css, /button\.icon-action\s*\{[^}]*min-width:\s*44px;[^}]*min-height:\s*44px;/);
    const vendor = readFileSync(new URL('../web/vendor/ubuntu/components.css', import.meta.url), 'utf8');
    for (const token of ['.p-icon--credit-card', '.p-icon--gift', '.p-icon--email', '.p-icon--show', '.p-tooltip__message']) assert.ok(vendor.includes(token));
    dom.window.close();
});

test('submission register renders rows with mobile-card headings', async () => {
    const { dom, document, reg } = setup();
    reg.api.get = async (path) => (path === '/submissions'
        ? {
            submissions: [
                { id: 's1', week_start: '2026-10-05', week_end: '2026-10-11', state: 'submitted', comment: 'looks good', entry_ids: ['e1', 'e2'] },
            ],
        }
        : {});
    await reg.refreshSubmissions();
    const rows = document.querySelectorAll('#submission-table tbody tr');
    assert.equal(rows.length, 1);
    assert.equal(rows[0].querySelector('td[data-heading="Entries"]').textContent, '2');
    assert.equal(rows[0].querySelector('td[data-heading="State"]').textContent.trim(), 'submitted');
    assert.equal(rows[0].querySelector('td[data-heading="Comment"]').textContent, 'looks good');
    assert.equal(document.getElementById('submission-empty').hidden, true);
    dom.window.close();
});

test('catalog register renders rows with mobile-card headings', async () => {
    const { dom, document, reg } = setup();
    reg.api.get = async (path) => (path === '/admin/item-types'
        ? {
            item_types: [
                { id: 'i1', name: 'Consulting', kind: 'service', description: '', default_price_minor: 12500, currency: 'EUR', active: true },
            ],
        }
        : {});
    await reg.refreshCatalog();
    const rows = document.querySelectorAll('#item-table tbody tr');
    assert.equal(rows.length, 1);
    assert.equal(rows[0].querySelector('td[data-heading="Type"]'), null);
    assert.equal(rows[0].querySelector('td[data-heading="Default price"]').textContent, 'EUR 125.00');
    assert.equal(document.getElementById('item-table').hidden, false);
    dom.window.close();
});

test('catalog edits preserve legacy kind without offering a product/service control', async () => {
    const { dom, document, reg } = setup();
    const existing = {id:'legacy1',name:'Existing item',kind:'product',description:'Keep me',default_price_minor:5000,currency:'EUR',active:true};
    reg.api.get = async () => ({item_types:[existing]});
    let saved;
    reg.api.put = async (path, body) => { saved = {path,body}; return existing; };
    await reg.refreshCatalog();
    document.querySelector('#item-table tbody button').click();
    document.getElementById('item-name').value = 'Updated item';
    await reg.saveCatalogItem({preventDefault(){}});
    assert.equal(saved.path, '/admin/item-types/legacy1');
    assert.equal(saved.body.kind, 'product');
    assert.equal(saved.body.name, 'Updated item');
    assert.equal(document.getElementById('item-kind'), null);
    dom.window.close();
});

test('retainer and schedule registers render rows with mobile-card headings', async () => {
    const { dom, document, reg } = setup();
    reg.state.customers = [{ id: 'c1', name: 'TatuTime', currency: 'EUR', default_rate_minor: 6000, active: true, email: '' }];
    reg.api.get = async (path) => {
        if (path === '/retainers') {
            return {
                retainers: [
                    { id: 'r1', customer_id: 'c1', project_code: 'ACM', currency: 'EUR', balance_minor: 50000, status: 'open', last_activity: '2026-10-01T09:00:00Z' },
                ],
            };
        }
        if (path === '/schedules') {
            return {
                schedules: [
                    { id: 'sc1', customer_id: 'c1', cadence: 'monthly', mode: 'retainer', currency: 'EUR', retainer_amount_minor: 25000, last_period_end: '2026-09-30', active: true },
                ],
            };
        }
        return {};
    };
    await reg.refreshRetainers();
    await reg.refreshSchedules();
    const retRow = document.querySelector('#ret-table tbody tr');
    assert.ok(retRow, 'retainer row renders');
    assert.equal(retRow.querySelector('td[data-heading="Balance"]').textContent, 'EUR 500.00');
    assert.equal(retRow.querySelector('td[data-heading="Last activity"]').textContent, '2026-10-01');
    const recRow = document.querySelector('#rec-table tbody tr');
    assert.ok(recRow, 'schedule row renders');
    assert.equal(recRow.querySelector('td[data-heading="Cadence"]').textContent, 'monthly');
    assert.equal(recRow.querySelector('td[data-heading="Retainer"]').textContent, 'EUR 250.00');
    dom.window.close();
});

// #F follow-up: the invoice filters are the same horizontal primitive as the
// other registers (a `.filter-row` of `.filter-field` groups), and the Open|All
// status control is a proper ARIA toggle group inside its own field — not a
// view tablist and not a bare pair of buttons floating beside the row.
test('the invoice filter row uses the house primitive and ARIA toggle state (#F)', () => {
    const dom = new JSDOM(html, { url: 'http://localhost/' });
    const doc = dom.window.document;
    const row = doc.getElementById('inv-filter-row');
    const status = doc.getElementById('inv-list-head');
    assert.ok(row.classList.contains('filter-row'), 'dropdowns use the shared .filter-row');
    assert.equal(status.nextElementSibling, row, 'Open/All is above the dropdown row');
    const fields = [...row.querySelectorAll('.filter-field')];
    assert.equal(fields.length, 5, 'three filters and two custom date fields');
    assert.deepEqual(fields.slice(0, 3).map(field => field.querySelector('input,select')?.id),
        ['invoice-search', 'inv-f-customer', 'inv-f-period'], 'Number, Customer, Time matches the table order');
    for (const id of ['invoice-search', 'inv-f-customer', 'inv-f-period', 'inv-f-from', 'inv-f-to']) {
        const control = doc.getElementById(id);
        assert.ok(control, `#${id} exists`);
        assert.ok(control.closest('.filter-field'), `#${id} is wrapped in its own field group`);
    }
    const group = status.querySelector('.filter-field .seg');
    assert.ok(group, 'the status control sits in its own filter field');
    assert.equal(group.getAttribute('role'), 'group', 'a group, never a tablist');
    assert.equal(group.getAttribute('aria-labelledby'), 'inv-status-label',
        'the group is named by its field label');
    assert.equal(group.getAttribute('aria-label'), null, 'the label is the single source of the name');
    assert.equal(doc.getElementById('inv-status-label').textContent.trim(), 'Status');
    assert.equal(row.querySelector('[role="tablist"], [role="tab"]'), null, 'no tab pattern is introduced');
    const buttons = [...group.querySelectorAll('button')];
    assert.deepEqual(buttons.map((button) => button.id), ['inv-tab-open', 'inv-tab-all']);
    for (const button of buttons) {
        assert.ok(button.classList.contains('seg-btn'), button.id + ' keeps the segment skin');
        assert.equal(button.type, 'button', button.id + ' stays a real button');
        assert.ok(['true', 'false'].includes(button.getAttribute('aria-pressed')),
            button.id + ' exposes its pressed state');
    }
    assert.equal(buttons.filter((button) => button.getAttribute('aria-pressed') === 'true').length, 1,
        'exactly one status is pressed');
    assert.equal(row.querySelector('.cols-pop'), null, 'there is no Columns control');
    assert.equal(doc.getElementById('inv-clear'), null, 'there is no Clear all control');
    assert.equal(doc.getElementById('inv-f-project'), null, 'only the requested three filters remain');
    dom.window.close();
});
