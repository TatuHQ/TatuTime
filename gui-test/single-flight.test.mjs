// QA-01: a double submit must not create the record twice.
//
// The Day entry form was the reported defect (two `POST /api/entries`, one
// "Entry added." announcement), and the audit found the same missing guard on
// every other create form that has no natural server key. These tests drive the
// real handlers twice while the first request is still in flight and assert:
// exactly one write, a disabled + `aria-busy` control during the request, and a
// control restored by `finally` when the save fails.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

function harness({ prelude = '', expose = '' } = {}) {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
    if (!dom.window.Element.prototype.scrollIntoView) dom.window.Element.prototype.scrollIntoView = () => { };
    dom.window.eval(`${app}\n${prelude}\nwindow.t = { ${expose} };`);
    return { dom, document: dom.window.document, window: dom.window, t: dom.window.t };
}

// One request gate + the read endpoints the handlers touch after a successful
// write. The gate keeps the first save in flight while the second event fires.
const GATED_API = `
  window.posts = [];
  window.gate = new Promise((resolve) => { window.release = resolve; });
  if (window.crypto && !window.crypto.randomUUID) {
    window.crypto.randomUUID = () => 'test-idempotency-key';
  }
  api.post = (path, body) => {
    window.posts.push({ path, body });
    return window.gate.then(() => ({
      id: 'x1', number: 'INV-0001', lines: [], name: (body && body.name) || 'x',
      total_minor: 0, currency: 'EUR', code: (body && body.code) || 'SM-1',
    }));
  };
  api.put = (path, body) => api.post(path, body);
  api.get = async (path) => {
    if (path === '/customers') return { customers: [] };
    if (path === '/categories') return { categories: [] };
    if (path === '/admin/item-types') return { item_types: [] };
    if (path === '/schedules') return { schedules: [] };
    if (path === '/retainers') return { retainers: [] };
    if (path.startsWith('/entries')) return { entries: [] };
    if (path === '/submissions') return { submissions: [] };
    if (path === '/invoices') return { invoices: [] };
    if (path.startsWith('/calendar/events')) return { events: [] };
    if (path === '/notifications') return { notifications: [] };
    if (path.startsWith('/expenses')) return { expenses: [] };
    return {};
  };
`;

const busy = (button) => button.disabled === true && button.getAttribute('aria-busy') === 'true';

function seedCustomer(t, document, selectId) {
    t.state.customers = [{
        id: 'c1', name: 'TatuTime', currency: 'EUR', default_rate_minor: 6000, active: true,
    }];
    if (selectId) {
        t.fillSelect(document.getElementById(selectId), [{ value: 'c1', text: 'TatuTime' }]);
        document.getElementById(selectId).value = 'c1';
    }
}

// ------------------------------------------------------- the reported case --

test('QA-01: two rapid submit events post the entry exactly once', async () => {
    const { dom, document, window, t } = harness({
        prelude: GATED_API,
        expose: `
      api, state, fillSelect, saveEntry,
      posts: () => window.posts,
      release: () => window.release(),
      entrySavingNow: () => entrySaving,
    `,
    });
    seedCustomer(t, document, 'entry-customer');
    t.fillSelect(document.getElementById('entry-project'), [{ value: 'SM-1', text: 'SM-1' }]);
    document.getElementById('entry-project').value = 'SM-1';
    document.getElementById('day-date').value = '2026-10-15';
    document.getElementById('entry-date').value = '2026-10-15';
    document.getElementById('entry-hours').value = '2';
    document.getElementById('entry-note').value = '';
    document.getElementById('entry-billable').checked = true;

    const form = document.getElementById('entry-form');
    // The real wiring is `$('entry-form').addEventListener('submit', saveEntry)`
    // in wireEntryForm(); attach the same listener so the events are the ones the
    // app handles, not a synthetic shortcut.
    form.addEventListener('submit', t.saveEntry);
    const submit = () => form.dispatchEvent(
        new window.Event('submit', { bubbles: true, cancelable: true }),
    );
    submit();
    submit();

    assert.equal(t.posts().length, 1, 'the second submit must not reach the API');
    assert.equal(t.posts()[0].path, '/entries');
    const save = document.getElementById('entry-save');
    assert.ok(busy(save), 'Save entry is disabled and aria-busy while the request is in flight');
    // The shell wraps long labels across source lines; rendered/accessible
    // text collapses whitespace, so compare that form.
    assert.equal(save.textContent.replace(/\s+/g, ' ').trim(), 'Save entry',
        'the busy control still reads sensibly');
    assert.equal(t.entrySavingNow(), true, 'the dialog close/Escape guard sees the in-flight save');

    t.release();
    await waitFor(() => save.disabled === false, { message: 'Save entry to be re-enabled' });
    assert.equal(t.posts().length, 1, 'still exactly one POST after the save settles');
    assert.equal(save.hasAttribute('aria-busy'), false);
    assert.equal(t.entrySavingNow(), false);
    assert.equal(document.getElementById('entry-dialog').hasAttribute('open'), false);
    dom.window.close();
});

test('QA-01: a failed entry save re-enables the button and shows the error', async () => {
    const { dom, document, window, t } = harness({
        prelude: `
      window.posts = [];
      api.post = (path) => { window.posts.push(path); return Promise.reject(new Error('server down')); };
      api.get = async () => ({ entries: [], submissions: [], invoices: [], notifications: [], events: [] });
    `,
        expose: 'api, state, fillSelect, saveEntry, posts: () => window.posts, entrySavingNow: () => entrySaving',
    });
    seedCustomer(t, document, 'entry-customer');
    t.fillSelect(document.getElementById('entry-project'), [{ value: 'SM-1', text: 'SM-1' }]);
    document.getElementById('entry-project').value = 'SM-1';
    document.getElementById('day-date').value = '2026-10-15';
    document.getElementById('entry-date').value = '2026-10-15';
    document.getElementById('entry-hours').value = '2';

    const form = document.getElementById('entry-form');
    form.addEventListener('submit', t.saveEntry);
    const submit = () => form.dispatchEvent(
        new window.Event('submit', { bubbles: true, cancelable: true }),
    );
    submit();
    submit();

    const save = document.getElementById('entry-save');
    await waitFor(() => save.disabled === false, { message: 'Save entry to be re-enabled after failure' });
    assert.equal(t.posts().length, 1, 'the rejection must not be retried by the second event');
    assert.equal(t.entrySavingNow(), false);
    assert.equal(save.hasAttribute('aria-busy'), false);
    const error = document.getElementById('entry-error');
    assert.equal(error.hidden, false, 'the failure is surfaced on the form');
    assert.match(error.textContent, /server down/);
    dom.window.close();
});

// --------------------------------------------------------- the other forms --
//
// Each case is a create form whose handler had no single-flight guard and whose
// record has no natural server key, so a second POST is a duplicate record (not
// a 409 the server would refuse). Every case drives the handler twice with the
// first request held open by the shared gate.

const CASES = [
    {
        name: 'expense',
        button: 'expense-save',
        prepare(t, doc) {
            seedCustomer(t, doc, 'expense-customer');
            doc.getElementById('expense-date').value = '2026-10-15';
            doc.getElementById('expense-amount').value = '10.00';
            doc.getElementById('expense-currency').value = 'EUR';
        },
        run: (t) => t.saveExpense({ preventDefault() { } }),
    },
    {
        name: 'customer',
        button: 'customer-save',
        prepare(t, doc) {
            doc.getElementById('customer-name').value = 'TatuTime';
            doc.getElementById('customer-currency').value = 'EUR';
            doc.getElementById('customer-rate').value = '60.00';
        },
        run: (t) => t.saveCustomer({ preventDefault() { } }),
    },
    {
        name: 'category',
        button: 'category-save',
        prepare(t, doc) {
            doc.getElementById('category-name').value = 'Travel';
        },
        run: (t) => t.addCategory({ preventDefault() { } }),
    },
    {
        name: 'quick-add category',
        button: null,
        buttonSelector: '#expense-category-form [type="submit"]',
        prepare(t, doc) {
            doc.getElementById('expense-category-name').value = 'Travel';
        },
        run: (t) => t.addExpenseCategory({ preventDefault() { } }),
    },
    {
        name: 'catalog item',
        button: 'item-save',
        prepare(t, doc) {
            doc.getElementById('item-name').value = 'Consulting';
            doc.getElementById('item-price').value = '125.00';
            doc.getElementById('item-currency').value = 'EUR';
        },
        run: (t) => t.saveCatalogItem({ preventDefault() { } }),
    },
    {
        name: 'recurring schedule',
        button: 'rec-add',
        prepare(t, doc) {
            seedCustomer(t, doc, 'rec-customer');
            doc.getElementById('rec-amount').value = '0';
        },
        run: (t) => t.addRecurring({ preventDefault() { } }),
    },
    {
        name: 'retainer',
        button: 'ret-add',
        prepare(t, doc) {
            seedCustomer(t, doc, 'ret-customer');
            t.fillSelect(doc.getElementById('ret-project'), [{ value: 'SM-1', text: 'SM-1' }]);
            doc.getElementById('ret-project').value = 'SM-1';
            doc.getElementById('ret-opening').value = '0';
        },
        run: (t) => t.createRetainer({ preventDefault() { } }),
    },
    {
        name: 'manual invoice',
        button: 'ed-save',
        prepare(t, doc) {
            seedCustomer(t, doc, 'ed-customer');
            t.edOpen(null);
        },
        run: (t) => t.edSave(),
    },
    {
        name: 'invoice wizard',
        button: 'iw-save',
        prepare(t, doc) {
            seedCustomer(t, doc, 'iw-customer');
            t.iw.from = '2026-10-01';
            t.iw.to = '2026-10-31';
        },
        run: (t) => t.iwSave(),
    },
    {
        name: 'wizard customer',
        button: 'wz-next',
        prepare(t, doc) {
            doc.getElementById('wz-cust-name').value = 'TatuTime';
            doc.getElementById('wz-cust-currency').value = 'EUR';
            doc.getElementById('wz-cust-rate').value = '60.00';
        },
        run: (t) => t.wizCreateCustomer(),
    },
];

test('QA-01: every guarded create form refuses a second submit while the first is in flight', async () => {
    for (const testCase of CASES) {
        const { dom, document, t } = harness({
            prelude: GATED_API,
            expose: `
        api, state, fillSelect, iw, edOpen, edSave,
        saveExpense, saveCustomer, addCategory, addExpenseCategory,
        saveCatalogItem, addRecurring, createRetainer, iwSave, wizCreateCustomer,
        posts: () => window.posts,
        release: () => window.release(),
      `,
        });
        testCase.prepare(t, document);
        const button = testCase.button
            ? document.getElementById(testCase.button)
            : document.querySelector(testCase.buttonSelector);

        testCase.run(t);
        testCase.run(t);

        await waitFor(() => t.posts().length > 0, { message: `${testCase.name}: first write after prefilling` });
        assert.equal(t.posts().length, 1, `${testCase.name}: a double submit must issue one write`);
        assert.ok(busy(button), `${testCase.name}: the save control is disabled and aria-busy`);

        t.release();
        await waitFor(() => button.disabled === false, {
            message: `${testCase.name}: the save control to be re-enabled`,
        });
        assert.equal(t.posts().length, 1, `${testCase.name}: still one write after the save settles`);
        assert.equal(button.hasAttribute('aria-busy'), false, `${testCase.name}: aria-busy cleared`);
        dom.window.close();
    }
});
