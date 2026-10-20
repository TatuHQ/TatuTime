// Advanced billing gate: recurring schedules (#136) and retainers (#144) are
// not finished, so the GUI does not offer them. One switch controls the whole
// surface — the `data-advanced-billing` attribute on <html> in web/index.html.
//
// These checks pin the visibility decision, not the feature's API: the block's
// markup and JS stay in place (the contract tests in tests/contract/retainers.rs
// and scheduler.rs still exercise the endpoints untouched), the gate hides it
// from rendering/the accessibility tree/the tab order, and flipping the single
// attribute brings it back.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const routeSource = readFileSync(new URL('../web/js/route.js', import.meta.url), 'utf8');
const stylesCss = readFileSync(new URL('../web/styles.css', import.meta.url), 'utf8');
// Selectors only: the comments above the rule explain the gate by name.
const stylesRules = stylesCss.replace(/\/\*[\s\S]*?\*\//g, '');
const app = loadAppSource();

const GATE = 'data-advanced-billing="off"';
const ENABLED_HTML = html.replace(GATE, 'data-advanced-billing="on"');

// Member boot stub (same shape as routing.test.mjs), recording every path the
// boot asks for so the gate can prove the hidden tools are not even fetched.
const bootStub = `
window.__paths = [];
api.get = async (path) => {
  window.__paths.push(path);
  if (path === '/auth/status') return { initialised: true };
  if (path === '/auth/me') return { id: 'm1', name: 'Mem', email: 'm@x.test', role: 'member' };
  if (path === '/auth/sso/providers') return { providers: [] };
  return { entries: [], customers: [], projects: [], submissions: [], expenses: [],
           claims: [], notifications: [], categories: [], item_types: [],
           invoices: [], users: [], schedules: [], retainers: [], timer: null };
};
api.post = async () => ({});
`;

async function bootAt(url, source = html) {
    const dom = new JSDOM(source, { runScripts: 'outside-only', url });
    dom.window.HTMLDialogElement.prototype.showModal = function showModal() { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function close() { this.open = false; };
    dom.window.eval(`${app}\n${bootStub}`);
    // The boot flag, not the footer copy: the footer names the reported build.
    await waitFor(() => dom.window.document.documentElement.dataset.booted === 'ready', {
        message: 'the app to finish booting at ' + url,
    });
    return dom;
}

const tabSelected = (document, id) =>
    document.getElementById(id).getAttribute('aria-selected') === 'true';

test('the shell ships the advanced billing gate off and keeps its markup (#144/#136)', () => {
    const document = new JSDOM(html).window.document;
    assert.equal(document.documentElement.getAttribute('data-advanced-billing'), 'off',
        'the one-line switch ships off while the tools are unfinished');

    const block = document.getElementById('advanced-billing');
    assert.ok(block, 'the block stays in the DOM so the tools can return');
    assert.equal(block.tagName, 'DETAILS', 'the entry point is the block summary itself');
    assert.match(block.querySelector('summary').textContent,
        /Advanced billing tools: recurring schedules and retainers/i,
        'the summary is the only entry point and it is inside the hidden block');

    // The retained DOM is the JS contract: forms, registers, empty states and the
    // retainer ledger all keep their ids.
    for (const id of [
        'rec-form', 'rec-customer', 'rec-cadence', 'rec-mode', 'rec-amount', 'rec-add',
        'rec-table', 'rec-empty',
        'ret-form', 'ret-customer', 'ret-project', 'ret-opening', 'ret-add',
        'ret-table', 'ret-empty', 'ret-detail', 'ret-history',
    ]) {
        assert.ok(document.getElementById(id), `#${id} stays in the retained DOM`);
    }

    // The stylesheet is the pre-JS half of the gate: hidden from the first paint,
    // and `display: none` removes the whole subtree from the accessibility tree
    // and the tab order (it is not a still-tabbable node).
    assert.match(stylesRules,
        /html\[data-advanced-billing="off"\] #advanced-billing \{ display: none; \}/,
        'the stylesheet hides the block from the single shell attribute');
});

test('the advanced tools have no route, tab or deep-link affordance', () => {
    const document = new JSDOM(html).window.document;
    // No rail entry, tab, panel, anchor or route names either feature.
    assert.equal(document.getElementById('tab-retainers'), null);
    assert.equal(document.getElementById('tab-schedules'), null);
    assert.equal(document.querySelector('#tabs [aria-controls="panel-retainers"]'), null);
    assert.equal(/href="[^"]*(retainer|schedule)/i.test(html), false,
        'no link advertises either feature');
    assert.equal(/['"]\/(?:retainers?|schedules?)(?:\/|['"])/.test(routeSource), false,
        'TAB_ROUTES holds no retainer/schedule view');
    // The API root paths are not SPA pages either (src/lib.rs SPA_ROUTES).
    assert.equal(/['"]\/retainers?['"]/.test(routeSource), false);
    assert.equal(/['"]\/schedules?['"]/.test(routeSource), false);
});

test('the app hides the block and skips its fetches while the gate is off (#144/#136)', async () => {
    const dom = await bootAt('http://localhost/invoices');
    const { document } = dom.window;
    const block = document.getElementById('advanced-billing');
    assert.equal(document.documentElement.getAttribute('data-advanced-billing'), 'off');
    assert.equal(block.hidden, true,
        'the module mirrors the gate onto the container, so it is inert without CSS too');

    // The Invoices view itself is untouched.
    assert.ok(tabSelected(document, 'tab-invoices'), 'Invoices remains reachable');
    assert.equal(document.getElementById('panel-invoices').hidden, false);
    assert.ok(document.querySelectorAll('#invoice-table').length === 1);

    // Hidden tools are not fetched: the admin-tier reads are skipped, not merely
    // hidden (a member boot never asks for data no view can show).
    assert.equal(dom.window.__paths.includes('/schedules'), false, '/schedules is not fetched');
    assert.equal(dom.window.__paths.includes('/retainers'), false, '/retainers is not fetched');
    dom.window.close();
});

test('one attribute turns the tools back on, fetches included (#144/#136)', async () => {
    const dom = await bootAt('http://localhost/invoices', ENABLED_HTML);
    const { document } = dom.window;
    assert.equal(document.documentElement.getAttribute('data-advanced-billing'), 'on');
    assert.equal(document.getElementById('advanced-billing').hidden, false,
        'the same block returns with the single attribute flip');
    await waitFor(() => dom.window.__paths.includes('/schedules')
        && dom.window.__paths.includes('/retainers'), {
        message: 'the enabled boot to fetch the schedules and retainers again',
    });
    dom.window.close();
});

test('the removed deep links fall back to the default view, never an empty panel', async () => {
    for (const path of ['/retainers', '/schedules']) {
        const dom = await bootAt('http://localhost' + path);
        const { document } = dom.window;
        assert.equal(dom.window.location.pathname, '/timesheets/day',
            `${path} is not an SPA view and canonicalises to the default`);
        assert.ok(tabSelected(document, 'tab-timesheet'), `${path} lands on Timesheets`);
        assert.equal(document.getElementById('advanced-billing').hidden, true,
            `${path} never reveals the hidden tools`);
        dom.window.close();
    }
});
