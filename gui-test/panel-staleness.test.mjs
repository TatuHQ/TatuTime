// QA-10: a failed panel refresh used to leave the previous rows on screen with
// no visible error (the live region was the only signal). The shared
// `panelRefresh` contract now shows a `role="alert"` banner inside the active
// view, keeps the last known good rows, and offers a keyboard-reachable Retry
// that re-runs the same loader; a successful retry removes the banner.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

function harness() {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
    const stub = `
window.t = {
  api, state, refreshExpenses, refreshCustomersView, refreshSubmissions,
  setFail: (v) => { window.fail = v; },
  calls: () => window.calls,
};`;
    dom.window.eval(`${app}\n${stub}`);
    return { dom, document: dom.window.document, window: dom.window, t: dom.window.t };
}

// The panels already contain the static form-error `<p role="alert">` nodes, so
// the banner is identified by its own `data-panel` marker.
const bannerIn = (document, panelId) =>
    document.querySelector(`#${panelId} [role="alert"][data-panel]`);

test('QA-10: a failed expense refresh keeps the rows and shows a Retry banner', async () => {
    const { dom, document, window, t } = harness();
    window.fail = false;
    window.calls = 0;
    t.api.get = async (path) => {
        if (path.startsWith('/expenses')) {
            window.calls += 1;
            if (window.fail) throw new Error('fetch failed');
            return {
                expenses: [
                    { id: 'x1', date: '2026-10-01', customer_id: 'c1', project_code: 'SM-1', amount_minor: 500, currency: 'EUR', billable: true },
                ],
            };
        }
        if (path === '/customers') return { customers: [] };
        return {};
    };
    t.state.customers = [{ id: 'c1', name: 'TatuTime', active: true, currency: 'EUR', default_rate_minor: 0 }];

    await t.refreshExpenses();
    const rows = () => document.querySelectorAll('#expense-table tbody tr').length;
    assert.equal(rows(), 1, 'the successful load rendered its row');
    assert.equal(bannerIn(document, 'panel-expenses'), null, 'no banner while the data is fresh');
    const good = t.calls();
    assert.equal(good, 1);

    t.setFail(true);
    await assert.rejects(t.refreshExpenses(), /fetch failed/);
    const banner = bannerIn(document, 'panel-expenses');
    assert.ok(banner, 'the failure is rendered visibly inside the Expenses panel');
    assert.equal(banner.closest('section').id, 'panel-expenses');
    assert.match(banner.textContent, /Refresh failed/);
    assert.match(banner.textContent, /fetch failed/);
    assert.match(banner.textContent, /Showing data from the last successful load\./,
        'staleness is explicit rather than implied');
    assert.equal(rows(), 1, 'the last known good rows are not blanked');

    const retry = banner.querySelector('button');
    assert.ok(retry, 'the banner carries a Retry control');
    assert.equal(retry.textContent, 'Retry');
    assert.equal(retry.disabled, false, 'Retry is available');
    assert.equal(retry.tabIndex >= 0 || retry.tagName === 'BUTTON', true, 'Retry is keyboard reachable');
    assert.ok(retry.getAttribute('aria-label'), 'Retry has an explicit accessible name');

    t.setFail(false);
    retry.click();
    await waitFor(() => bannerIn(document, 'panel-expenses') === null, {
        message: 'the banner to clear after a successful retry',
    });
    assert.ok(t.calls() > good + 1, 'Retry issued a fresh request');
    assert.equal(rows(), 1, 'the retried load rendered again');
    dom.window.close();
});

test('QA-10: the Customers view shows the same banner and retry behaviour', async () => {
    const { dom, document, window, t } = harness();
    window.fail = false;
    window.calls = 0;
    t.api.get = async (path) => {
        if (path === '/customers') {
            window.calls += 1;
            if (window.fail) throw new Error('server lost');
            return {
                customers: [
                    { id: 'c1', name: 'TatuTime', currency: 'EUR', default_rate_minor: 6000, active: true, email: '' },
                ],
            };
        }
        return {};
    };

    await t.refreshCustomersView();
    const rows = () => document.querySelectorAll('#customer-table tbody tr').length;
    assert.equal(rows(), 1);

    t.setFail(true);
    await assert.rejects(t.refreshCustomersView(), /server lost/);
    const banner = bannerIn(document, 'panel-customers');
    assert.ok(banner, 'the Customers panel renders the visible error');
    assert.match(banner.textContent, /Showing data from the last successful load\./);
    assert.equal(rows(), 1, 'the customer rows stay on screen');

    t.setFail(false);
    banner.querySelector('button').click();
    await waitFor(() => bannerIn(document, 'panel-customers') === null, {
        message: 'the Customers banner to clear after Retry',
    });
    assert.equal(rows(), 1);
    dom.window.close();
});

test('QA-10: the banner is announced and Retry is focusable and labelled', async () => {
    const { dom, document, window, t } = harness();
    // A view is only focusable while it is the active one (the repro clicks the
    // Customers tab); the hidden tabpanel would make focus() a no-op.
    document.getElementById('panel-expenses').hidden = false;
    window.fail = false;
    t.api.get = async (path) => {
        if (path.startsWith('/expenses')) {
            if (window.fail) throw new Error('offline');
            return { expenses: [] };
        }
        return {};
    };
    t.state.customers = [];
    await t.refreshExpenses();
    t.setFail(true);
    await assert.rejects(t.refreshExpenses());

    const banner = bannerIn(document, 'panel-expenses');
    assert.equal(banner.getAttribute('role'), 'alert', 'the failure is announced to assistive tech');
    assert.equal(banner.getAttribute('data-panel'), 'panel-expenses');
    const retry = banner.querySelector('button');
    retry.focus();
    assert.equal(document.activeElement, retry, 'Retry takes focus');
    assert.equal(retry.getAttribute('aria-label'), 'Retry refreshing the expense register');
    dom.window.close();
});

test('QA-10: a first-load failure does not claim stale data it never had', async () => {
    const { dom, document, window, t } = harness();
    window.fail = true;
    t.api.get = async (path) => {
        if (path === '/submissions') throw new Error('offline');
        return {};
    };
    await assert.rejects(t.refreshSubmissions(), /offline/);
    const banner = bannerIn(document, 'panel-submissions');
    assert.ok(banner);
    assert.match(banner.textContent, /No data has been loaded yet\./);
    dom.window.close();
});
