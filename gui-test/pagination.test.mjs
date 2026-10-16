// #334: the list endpoints (`/entries`, `/expenses`) answer one *page* at a
// time and say whether more follow (`next_cursor`). A view that rendered only
// the first page would quietly drop the tail — the silent cut the bound exists
// to prevent — so these tests drive the real views against a fake API that
// serves every collection in pages of two, and assert what the *user* sees.
//
// Two things are proved here that the contract suite cannot: the views follow
// the cursor to the last page (no row is lost), and when the client-side page
// cap stops the walk the view says so instead of looking complete.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

const CUSTOMER = { id: 'c1', name: 'ACME', currency: 'EUR', default_rate_minor: 6000, active: true };

/// Seven expenses, served exactly as the API orders them: `(date, id)`
/// descending, so the walk has to rely on the id tie-break inside a date.
const EXPENSES = Array.from({ length: 7 }, (_, i) => ({
    id: `x${i}`,
    date: i < 4 ? '2026-10-07' : '2026-10-06',
    customer_id: 'c1',
    amount_minor: 1000 + i,
    currency: 'EUR',
    billable: true,
})).sort((a, b) => (a.date === b.date
    ? (a.id < b.id ? 1 : -1)
    : (a.date < b.date ? 1 : -1)));

/// Five entries of one day, served in the API's documented order — `(date, id)`
/// ascending. That order deliberately differs from the order they were
/// recorded in: a timesheet reads like a diary, so the view must restore
/// creation order rather than render whichever page order it happened to get.
const DAY = '2026-10-07';
const ENTRIES = [
    { id: 'aaa', created_at: `${DAY}T12:00:00Z`, hours: 3 },
    { id: 'bbb', created_at: `${DAY}T08:00:00Z`, hours: 1 },
    { id: 'ccc', created_at: `${DAY}T09:00:00Z`, hours: 2 },
    { id: 'ddd', created_at: `${DAY}T10:00:00Z`, hours: 4 },
    { id: 'eee', created_at: `${DAY}T11:00:00Z`, hours: 5 },
].map((e) => ({ ...e, date: DAY, customer_id: 'c1', project_code: 'P1', billable: true }));

const API_ORDER = [...ENTRIES].sort((a, b) => (a.date === b.date
    ? (a.id < b.id ? -1 : 1)
    : (a.date < b.date ? -1 : 1)));

/// The cursor is opaque to the client: it must come back byte for byte, so the
/// fake refuses anything it did not mint. A client that parsed, re-encoded or
/// dropped the cursor fails here instead of silently re-reading page one.
function cursorIndex(raw) {
    const match = /^cursor-(\d+)$/.exec(raw || '');
    assert.ok(match, `the client must echo the opaque cursor verbatim, got ${JSON.stringify(raw)}`);
    return Number(match[1]);
}

/// One page of `rows`: `limit` defaults to 2, and `next_cursor` is null exactly
/// on the page that ends the set.
function page(rows, key, query) {
    const params = new URLSearchParams(query || '');
    const limit = Number(params.get('limit') || 2);
    const cursor = params.get('cursor');
    const start = cursor ? cursorIndex(cursor) : 0;
    const slice = rows.slice(start, start + limit);
    const next = start + slice.length;
    return { [key]: slice, next_cursor: next < rows.length ? `cursor-${next}` : null };
}

/// The app in jsdom with its HTTP layer replaced by the fake above — every call
/// a view makes lands there, and is recorded.
async function setup({ endless = false } = {}) {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    const { window } = dom;
    window.HTMLDialogElement.prototype.showModal = function showModal() { this.open = true; };
    window.HTMLDialogElement.prototype.close = function close() { this.open = false; };
    window.eval(`${app}
window.f = { api, state, refreshExpenses, refreshDay, MAX_PAGES };`);
    const calls = [];
    window.f.api.get = async (path) => {
        calls.push(path);
        if (path === '/auth/status') return { initialised: true };
        if (path === '/auth/me') return { id: 'a1', name: 'Admin', email: 'a@x.test', role: 'admin' };
        if (path === '/auth/sso/providers') return { providers: [] };
        if (path === '/categories') return { categories: [] };
        if (path === '/customers') return { customers: [CUSTOMER] };
        const [route, query] = path.split('?');
        if (route === '/expenses') {
            if (!endless) return page(EXPENSES, 'expenses', query);
            // A collection larger than the client's page cap: every page is full and
            // always claims a successor, so the walk can only end at the cap.
            const start = (query || '').includes('cursor=')
                ? cursorIndex(new URLSearchParams(query).get('cursor'))
                : 0;
            const limit = Number(new URLSearchParams(query || '').get('limit') || 2);
            const slice = Array.from({ length: limit }, (_, i) => ({
                ...EXPENSES[0],
                id: `endless-${start + i}`,
            }));
            return { expenses: slice, next_cursor: `cursor-${start + limit}` };
        }
        // Every entry query — the day, the strips, the week grid — is served from
        // the same five rows, paged the same way.
        if (route === '/entries') return page(API_ORDER, 'entries', query);
        return {};
    };
    window.document.getElementById('day-date').value = DAY;
    // The fake must be in place before boot, so no request escapes to jsdom.
    window.document.dispatchEvent(new window.Event('DOMContentLoaded', { bubbles: true }));
    await waitFor(() => window.document.getElementById('version').textContent === 'TucanoTime', {
        message: 'the app to finish booting',
    });
    window.f.state.customers = [CUSTOMER];
    return { window, document: window.document, calls };
}

/// A window left open keeps the runner's event loop alive.
function harness(t, parts) {
    t.after(() => parts.window.close());
    return parts;
}

test('the expenses register walks every page: no row is dropped', async (t) => {
    const { window, document, calls } = harness(t, await setup());
    calls.length = 0; // the walk under test, not boot's own fetches
    await window.f.refreshExpenses();
    const rows = [...document.querySelectorAll('#expense-table tbody tr')];
    assert.equal(rows.length, EXPENSES.length, 'every expense of every page is rendered');
    assert.deepEqual(
        rows.map((tr) => tr.querySelector('th').textContent),
        EXPENSES.map((e) => e.date),
        'in the API order, page after page',
    );
    assert.deepEqual(
        calls.filter((p) => p.startsWith('/expenses')),
        ['/expenses', '/expenses?cursor=cursor-2', '/expenses?cursor=cursor-4', '/expenses?cursor=cursor-6'],
        'four pages, the cursor echoed verbatim, the walk stopping when next_cursor is null',
    );
});

test('the day view walks every page and shows the entries in diary order', async (t) => {
    const { window, document, calls } = harness(t, await setup());
    calls.length = 0;
    await window.f.refreshDay();
    const ids = [...document.querySelectorAll('#day-table tbody tr')].map((tr) => tr.dataset.entryId);
    assert.deepEqual(ids, ['bbb', 'ccc', 'ddd', 'eee', 'aaa'],
        'all five entries of the day, in the order they were recorded');
    const asked = calls.filter((p) => p.startsWith('/entries?date='));
    assert.equal(asked.length, 3, 'five entries at two per page is three requests');
    assert.equal(asked.filter((p) => !p.includes('cursor=')).length, 1, 'only the first page has no cursor');
});

test('a walk stopped by the client page cap is announced, not hidden', async (t) => {
    const { window, document, calls } = harness(t, await setup({ endless: true }));
    calls.length = 0;
    await window.f.refreshExpenses();
    const rows = [...document.querySelectorAll('#expense-table tbody tr')];
    assert.equal(rows.length, window.f.MAX_PAGES * 2, 'the cap bounds how many rows the view holds');
    assert.match(document.getElementById('live-region').textContent, /Showing the first \d+ expenses/,
        'the register says it shows one bounded set instead of looking complete');
});
