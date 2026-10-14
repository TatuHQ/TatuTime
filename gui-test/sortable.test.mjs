// #232 phase 2d: the Invoices register is Vanilla 4.59's REAL sortable-table
// pattern — `aria-sort` on the `<th>` plus a `.p-table__sort-button`.
//
// There is NO `.p-table--sortable` class in Vanilla 4.59: the pattern ships as
// the SCSS mixin `vf-p-table-sortable` (gui-test/ubuntu-components.scss), which
// compiles to the `table th[aria-sort]` + `.p-table__sort-button` rules in the
// vendored bundle. The migration plan's earlier "gains `p-table--sortable`"
// prose was wrong and is corrected with this increment.
//
// This offline suite pins (a) the static contract — the sortable columns and
// their order, the `data-col` keys, and the initial `aria-sort` (Number
// ascending, the rest `none`) — and (b) the sort semantics `dashFilter` owns:
// default by Number ascending, numeric columns compared numerically, string
// columns by resolved value, and direction toggling. The end-to-end click
// behaviour is driven live in gui.mjs and the real-browser gate.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

const SORT_COLUMNS = ['number', 'customer', 'period', 'total', 'balance', 'status'];

test('the invoice register uses Vanilla\'s real sortable pattern (#232 phase 2d)', () => {
    const dom = new JSDOM(html, { url: 'http://localhost/' });
    const doc = dom.window.document;
    const ths = [...doc.querySelectorAll('#invoice-table thead th')];
    assert.equal(ths.length, 7, 'six sortable columns plus the Actions column');
    const buttons = ths.flatMap((th) => [...th.querySelectorAll('button')]);
    assert.deepEqual(
        buttons.map((b) => b.getAttribute('data-col')),
        SORT_COLUMNS,
        'sortable columns and their order are unchanged',
    );
    assert.ok(buttons.every((b) => b.classList.contains('p-table__sort-button')), 'every sort control is a .p-table__sort-button');
    assert.ok(buttons.every((b) => b.type === 'button'), 'the sort controls stay keyboard-reachable buttons');
    assert.ok(buttons.every((b) => b.closest('th').tagName === 'TH'), 'each sort control owns its <th>');
    assert.equal(ths[6].querySelector('button'), null, 'the Actions column is not sortable');
    assert.deepEqual(
        ths.map((th) => th.getAttribute('aria-sort')),
        ['ascending', 'none', 'none', 'none', 'none', 'none', null],
        'the initial aria-sort reflects the default sort (Number ascending) and marks the rest none',
    );
    dom.window.close();
});

function setup() {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
    dom.window.eval(`${app}\nwindow.t = { dash, dashFilter, state };`);
    return dom;
}

const INVOICES = [
    { id: 'a', number: 'INV-0002', customer_id: 'c1', period_from: '2026-02-01', period_to: '2026-02-28', total_minor: 20000, currency: 'EUR', status: 'issued' },
    { id: 'b', number: 'INV-0001', customer_id: 'c2', period_from: '2026-01-01', period_to: '2026-01-31', total_minor: 5000, currency: 'EUR', status: 'paid' },
    { id: 'c', number: 'INV-0003', customer_id: 'c1', period_from: '2026-03-01', period_to: '2026-03-31', total_minor: 10000, currency: 'EUR', status: 'draft' },
];

function sortedNumbers(t, sortKey, sortDir) {
    t.dash.openOnly = false; // include the draft/paid fixtures
    t.dash.sortKey = sortKey;
    t.dash.sortDir = sortDir;
    // Array.from runs in the Node realm, so the result compares strictly
    // (deepStrictEqual checks the prototype realm; a jsdom-realm Array would
    // never match a Node-realm literal).
    return Array.from(t.dashFilter(INVOICES), (i) => i.number);
}

test('dashFilter keeps the sort contract (#232 phase 2d)', () => {
    const dom = setup();
    const t = dom.window.t;
    t.state.customers = [
        { id: 'c1', name: 'Acme', currency: 'EUR' },
        { id: 'c2', name: 'Zeta', currency: 'EUR' },
    ];
    // default sort is Number ascending, toggling to descending
    assert.equal(t.dash.sortKey, 'number');
    assert.equal(t.dash.sortDir, 1);
    assert.deepEqual(sortedNumbers(t, 'number', 1), ['INV-0001', 'INV-0002', 'INV-0003']);
    assert.deepEqual(sortedNumbers(t, 'number', -1), ['INV-0003', 'INV-0002', 'INV-0001']);
    // numeric columns compare numerically, not lexicographically
    assert.deepEqual(sortedNumbers(t, 'total', 1), ['INV-0001', 'INV-0003', 'INV-0002']);
    assert.deepEqual(sortedNumbers(t, 'total', -1), ['INV-0002', 'INV-0003', 'INV-0001']);
    // string columns sort by resolved value (customer name: Acme < Zeta)
    assert.deepEqual(sortedNumbers(t, 'customer', 1), ['INV-0002', 'INV-0003', 'INV-0001']);
    // period sorts by period_to
    assert.deepEqual(sortedNumbers(t, 'period', 1), ['INV-0001', 'INV-0002', 'INV-0003']);
    // an unknown key falls back to Number
    assert.deepEqual(sortedNumbers(t, 'bogus', 1), ['INV-0001', 'INV-0002', 'INV-0003']);
    dom.window.close();
});
