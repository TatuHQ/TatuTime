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
const CARD_REGISTERS = {
    'user-table': ['Name', 'Email', 'Role', 'Status', 'Rates', 'Actions'],
    'submission-table': ['Week', 'Entries', 'State', 'Comment', 'Actions'],
    'secret-table': ['Key', 'Value', 'Actions'],
    'item-table': ['Name', 'Type', 'Default price', 'Status', 'Actions'],
    'rec-table': ['Customer', 'Cadence', 'Bills', 'Retainer', 'Last billed to', 'Status', 'Actions'],
    'ret-table': ['Customer / project', 'Balance', 'Status', 'Last activity', 'Actions'],
};
// Summary/reference tables: a plain official table inside the card.
const SUMMARY_REGISTERS = {
    'config-table': ['Setting', 'Value', 'Source'],
    'template-vars': ['Variable', 'Example'],
    'report-table': ['Group', 'Currency', 'Hours', 'Amount', 'Entries'],
};
// Deliberately app-owned: the timesheet grids and the editable line editor.
// The Invoices register is phase 2d — the real aria-sort + .p-table__sort-button
// pattern (there is no `p-table--sortable` class in Vanilla 4.59).
const LEFT_ALONE = ['day-table', 'week-table', 'cal-grid', 'invoice-table', 'ed-lines'];

function headers(table) {
    return [...table.querySelectorAll('thead th')].map((th) => th.textContent.trim());
}

function setup() {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
    const stub = `
window.reg = {
  api, state, renderUsers, refreshSubmissions, refreshCatalog,
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
    assert.equal(rows[0].querySelector('td[data-heading="Type"]').textContent, 'service');
    assert.equal(rows[0].querySelector('td[data-heading="Default price"]').textContent, 'EUR 125.00');
    assert.equal(document.getElementById('item-table').hidden, false);
    dom.window.close();
});

test('retainer and schedule registers render rows with mobile-card headings', async () => {
    const { dom, document, reg } = setup();
    reg.state.customers = [{ id: 'c1', name: 'ACME', currency: 'EUR', default_rate_minor: 6000, active: true, email: '' }];
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
