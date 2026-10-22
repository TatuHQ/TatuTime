// Harvest import workspace (#403): mapping controls are built from the preview
// source fields, the original PDF link targets the archived member, and a
// double confirm cannot issue two commits.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM, VirtualConsole } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();
const SHA = 'b'.repeat(64);

function response(status, body) {
    return {
        ok: status >= 200 && status < 300,
        status,
        headers: { get: () => null },
        text: async () => (body === undefined ? '' : JSON.stringify(body)),
    };
}

function batch(overrides = {}) {
    return {
        id: '11111111-1111-1111-1111-111111111111',
        namespace: 'harvest',
        state: 'draft',
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
        files: { time: { name: 'time.csv', sha256: 'a'.repeat(64), bytes: 10, uploaded_at: '2026-01-01T00:00:00Z' } },
        choices: {
            users: [], customers: [], projects: [], tasks: [], categories: [],
            invoices: [], excluded: [], invoice_links: [],
        },
        ...overrides,
    };
}

function preview(ready) {
    return {
        revision: 'c'.repeat(64),
        ready,
        blocking: ready ? 0 : 1,
        totals: [],
        issues: ready ? [] : [{
            source: 'time', record: 1, line: 2, field: 'user_id',
            message: 'Choose who this row belongs to', blocking: true,
        }],
        time: [{
            source_key: 'time:1', record: 1, line: 2, decision: ready ? 'new' : 'unresolved',
            source_client: 'Acme', source_project: 'Website', source_code: 'WEB',
            source_person: 'Ada Lovelace', source_employee_id: 'E1', source_task: 'Design',
            invoiced: true, issues: [],
        }],
        expenses: [],
        invoices: [{
            source_key: 'invoice:1', original_name: 'H-1.pdf', pdf_sha256: SHA,
            decision: ready ? 'new' : 'unresolved', customer_id: 'c1',
            original_number: 'H-1', currency: 'EUR', invoice_date: '2026-01-31',
            period_from: '2026-01-01', period_to: '2026-01-31', total_minor: 10000,
            opening_paid_minor: 0, status: 'issued', suggestions: null, issues: [],
        }],
    };
}

async function harness({
    ready = false, commitStatus = 200, getBatch = batch(), previewBody = null,
    customers = [{ id: 'c1', name: 'Acme' }],
    projectsByCustomer = { c1: [{ code: 'WEB', name: 'Website' }] },
} = {}) {
    const calls = { commit: 0, preview: 0, discard: 0, uploads: [] };
    const dom = new JSDOM(html, {
        runScripts: 'outside-only',
        pretendToBeVisual: true,
        url: 'http://localhost/',
        virtualConsole: new VirtualConsole(),
    });
    dom.window.HTMLDialogElement.prototype.showModal = function showModal() { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function close() { this.open = false; };
    dom.window.fetch = async (url, opts = {}) => {
        const method = opts.method || 'GET';
        const path = String(url);
        if (path.endsWith('/api/users')) return response(200, { users: [{ id: 'u1', name: 'Ada', email: 'ada@test.local' }] });
        if (path.endsWith('/api/customers')) return response(200, { customers });
        if (path.endsWith('/api/categories')) return response(200, { categories: [{ id: 'cat1', name: 'Travel' }] });
        if (path.endsWith('/projects')) {
            const customerId = path.match(/customers\/([^/]+)\/projects$/)?.[1];
            return response(200, { projects: projectsByCustomer[customerId] || [] });
        }
        if (path.endsWith('/tasks')) return response(200, { tasks: [{ code: 'DESIGN', name: 'Design' }] });
        if (path.includes('/imports/harvest?limit=')) return response(200, { total: 0, items: [] });
        if (path.endsWith('/admin/imports/harvest') && method === 'POST') return response(201, batch());
        if (method === 'PUT' && path.includes('/files/')) {
            calls.uploads.push(path);
            return response(200, batch());
        }
        if (path.endsWith('/review')) return response(200, getBatch);
        if (path.endsWith('/preview')) {
            calls.preview += 1;
            return response(200, previewBody || preview(ready));
        }
        if (path.endsWith('/commit')) {
            calls.commit += 1;
            if (commitStatus !== 200) {
                return response(commitStatus, { error: { code: 'conflict', message: 'preview is stale' } });
            }
            return response(200, {
                committed_at: '2026-01-02T00:00:00Z', revision: 'c'.repeat(64),
                entries: ['e1'], expenses: [], invoices: ['H-1'],
                customers: [], projects: [], tasks: [], categories: [],
            });
        }
        if (method === 'DELETE' && path.includes('/admin/imports/harvest/')) {
            calls.discard += 1;
            return response(204, undefined);
        }
        // GET one batch
        return response(200, getBatch);
    };
    dom.window.eval(app);
    // The workspace's own controls are wired by boot() in the real app; the
    // harness wires them directly so filter/click behaviour is the shipped one.
    dom.window.wireImports();
    return { dom, calls };
}

function attachFile(dom, inputId, name, type) {
    const input = dom.window.document.getElementById(inputId);
    const file = new dom.window.File(['data'], name, { type });
    Object.defineProperty(input, 'files', { value: [file], configurable: true });
    return input;
}

test('source tabs build only their own mapping and link the original PDF', async () => {
    const { dom } = await harness({ ready: false });
    await dom.window.openBatch(batch().id);
    await waitFor(() => dom.window.document.querySelector('#import-mapping h3'), {
        message: 'mapping controls to render',
    });
    const document = dom.window.document;
    const mapping = document.getElementById('import-mapping');
    assert.match(mapping.textContent, /People/);
    assert.match(mapping.textContent, /Customers/);
    assert.match(mapping.textContent, /Projects/);
    assert.match(mapping.textContent, /Tasks/);
    assert.doesNotMatch(mapping.textContent, /Invoices/, 'the Time tab does not show invoice cards');
    assert.equal(document.getElementById('import-tab-time').getAttribute('aria-selected'), 'true');
    assert.equal(document.getElementById('import-panel-time').hidden, false);
    assert.equal(document.getElementById('import-panel-invoices').hidden, true);
    assert.equal(document.getElementById('import-time-upload').disabled, false);

    document.getElementById('import-tab-invoices').click();
    await waitFor(() => document.getElementById(`import-inv-total-${SHA}`), {
        message: 'the invoice mapping card',
    });
    assert.equal(document.getElementById('import-tab-invoices').getAttribute('aria-selected'), 'true');
    assert.equal(document.getElementById('import-panel-invoices').hidden, false);
    assert.equal(document.getElementById('import-panel-time').hidden, true);
    assert.doesNotMatch(mapping.textContent, /People/);
    assert.match(mapping.textContent, /Invoices/);
    const pdf = [...mapping.querySelectorAll('a')].find((a) => a.textContent === 'Original PDF');
    assert.ok(pdf, 'invoice PDF link exists');
    assert.match(pdf.getAttribute('href'), /\/invoices\/b{64}\/pdf$/);
    assert.equal(document.getElementById('import-commit').disabled, true);
    dom.window.close();
});

test('project choices are limited to the customer mapped for that source client', async () => {
    const { dom } = await harness({
        customers: [{ id: 'c1', name: 'Acme' }, { id: 'c2', name: 'Beta' }],
        projectsByCustomer: {
            c1: [{ code: 'WEB', name: 'Website' }],
            c2: [{ code: 'APP', name: 'App' }],
        },
    });
    await dom.window.openBatch(batch().id);
    await waitFor(() => dom.window.document.querySelector('#import-mapping h3'), {
        message: 'mapping controls to render',
    });
    const document = dom.window.document;
    const customer = document.getElementById('import-customer-Acme');
    assert.deepEqual([...customer.options].map((option) => option.value),
        ['', '__create', 'c1', 'c2'], 'existing customers are offered');
    let project = document.getElementById('import-project-Acme|Website|WEB');
    assert.deepEqual([...project.options].map((option) => option.value),
        ['', '__skip', '__create'], 'no existing project is offered before the customer is mapped');

    customer.value = 'c1';
    customer.dispatchEvent(new dom.window.Event('change'));
    project = document.getElementById('import-project-Acme|Website|WEB');
    assert.deepEqual([...project.options].map((option) => option.value),
        ['', '__skip', '__create', 'c1|WEB'], 'only the mapped customer projects appear');
    assert.match(project.textContent, /Acme — WEB Website/);
    assert.doesNotMatch(project.textContent, /Beta|APP/, 'another customer project never leaks in');

    project.value = 'c1|WEB';
    project.dispatchEvent(new dom.window.Event('change'));
    customer.value = 'c2';
    customer.dispatchEvent(new dom.window.Event('change'));
    project = document.getElementById('import-project-Acme|Website|WEB');
    assert.equal(project.value, '', 'a project from the previous customer is cleared');
    assert.deepEqual([...project.options].map((option) => option.value),
        ['', '__skip', '__create', 'c2|APP'], 'the picker follows the new customer');
    dom.window.close();
});

test('invoice cards take their values and labels from the PDF suggestions', async () => {
    const withSuggestions = preview(false);
    withSuggestions.invoices[0] = {
        ...withSuggestions.invoices[0],
        customer_id: null, original_number: '', currency: null, invoice_date: null,
        period_from: null, period_to: null, total_minor: null, opening_paid_minor: 0, status: null,
        suggestions: {
            pages: 1, text_bytes: 10, text_truncated: false, status: 'suggestions', reason: null,
            suggestions: [
                { field: 'customer', value: 'Acme', amount_minor: null, page: 1, evidence: 'Invoice for Acme', confidence: 'low' },
                { field: 'invoice_number', value: '1', amount_minor: null, page: 1, evidence: 'Invoice ID 1', confidence: 'medium' },
                { field: 'currency', value: 'EUR', amount_minor: null, page: 1, evidence: 'Subtotal EUR 5,760.00', confidence: 'low' },
                { field: 'invoice_date', value: '2025-02-01', amount_minor: null, page: 1, evidence: 'Issue date 01/02/2025', confidence: 'medium' },
                { field: 'period_from', value: '2025-01-01', amount_minor: null, page: 1, evidence: 'Billing month preceding the issue date', confidence: 'medium' },
                { field: 'period_to', value: '2025-01-31', amount_minor: null, page: 1, evidence: 'Billing month preceding the issue date', confidence: 'medium' },
                { field: 'subtotal', value: 'EUR 5,760.00', amount_minor: 576000, page: 1, evidence: 'Subtotal EUR 5,760.00', confidence: 'medium' },
                { field: 'payments', value: 'EUR -5,760.00', amount_minor: 576000, page: 1, evidence: 'Payments EUR -5,760.00', confidence: 'medium' },
                { field: 'status', value: 'paid', amount_minor: null, page: 1, evidence: 'Payments compared with Subtotal', confidence: 'medium' },
            ],
        },
    };
    const { dom } = await harness({ ready: false, previewBody: withSuggestions });
    await dom.window.openBatch(batch().id);
    dom.window.document.getElementById('import-tab-invoices').click();
    await waitFor(() => dom.window.document.getElementById(`import-inv-total-${SHA}`), {
        message: 'the prefilled invoice card',
    });
    const document = dom.window.document;
    assert.equal(document.getElementById(`import-inv-number-${SHA}`).value, '1');
    assert.equal(document.getElementById(`import-inv-customer-${SHA}`).value, 'c1');
    assert.equal(document.getElementById(`import-inv-currency-${SHA}`).value, 'EUR');
    assert.equal(document.getElementById(`import-inv-date-${SHA}`).value, '2025-02-01');
    assert.equal(document.getElementById(`import-inv-from-${SHA}`).value, '2025-01-01');
    assert.equal(document.getElementById(`import-inv-to-${SHA}`).value, '2025-01-31');
    assert.equal(document.getElementById(`import-inv-total-${SHA}`).value, '5760.00');
    assert.equal(document.getElementById(`import-inv-paid-${SHA}`).value, '5760.00');
    assert.equal(document.getElementById(`import-inv-status-${SHA}`).value, 'paid');
    const mapping = document.getElementById('import-mapping');
    for (const label of ['Invoice ID', 'Invoice for', 'Issue date', 'Subtotal', 'Payments']) {
        assert.match(mapping.textContent, new RegExp(label));
    }
    assert.match(mapping.textContent, /PDF: Acme/);
    dom.window.close();
});

test('overriding Subtotal and Payments recomputes the import status and keeps the override', async () => {
    const body = preview(false);
    body.invoices[0] = {
        ...body.invoices[0],
        customer_id: null, original_number: '6', currency: 'USD',
        invoice_date: '2025-05-01', period_from: '2025-05-01', period_to: '2025-05-31',
        total_minor: 0, opening_paid_minor: 0, status: 'issued',
        suggestions: {
            pages: 1, text_bytes: 10, text_truncated: false, status: 'suggestions', reason: null,
            suggestions: [
                { field: 'customer', value: 'Acme', amount_minor: null, page: 1, evidence: '', confidence: 'low' },
                { field: 'invoice_number', value: '6', amount_minor: null, page: 1, evidence: '', confidence: 'medium' },
                { field: 'currency', value: 'USD', amount_minor: null, page: 1, evidence: '', confidence: 'low' },
                { field: 'invoice_date', value: '2025-05-01', amount_minor: null, page: 1, evidence: '', confidence: 'medium' },
                { field: 'period_from', value: '2025-05-01', amount_minor: null, page: 1, evidence: '', confidence: 'medium' },
                { field: 'period_to', value: '2025-05-31', amount_minor: null, page: 1, evidence: '', confidence: 'medium' },
                { field: 'amount_due', value: 'USD $0.00', amount_minor: 0, page: 1, evidence: '', confidence: 'medium' },
                { field: 'status', value: 'issued', amount_minor: null, page: 1, evidence: '', confidence: 'medium' },
            ],
        },
    };
    const { dom } = await harness({ ready: false, previewBody: body });
    await dom.window.openBatch(batch().id);
    dom.window.document.getElementById('import-tab-invoices').click();
    await waitFor(() => dom.window.document.getElementById(`import-inv-total-${SHA}`), {
        message: 'the invoice card',
    });
    const document = dom.window.document;
    const total = document.getElementById(`import-inv-total-${SHA}`);
    const paid = document.getElementById(`import-inv-paid-${SHA}`);
    total.value = '162.50';
    total.dispatchEvent(new dom.window.Event('input'));
    paid.value = '162.50';
    paid.dispatchEvent(new dom.window.Event('input'));
    assert.equal(document.getElementById(`import-inv-status-${SHA}`).value, 'paid');
    const choices = JSON.parse(document.getElementById('import-choices').value);
    const saved = choices.invoices.find((choice) => choice.sha256 === SHA);
    assert.equal(saved.total_minor, 16250, 'the reviewed total overrides the PDF');
    assert.equal(saved.opening_paid_minor, 16250, 'the reviewed payment overrides the PDF');
    assert.equal(saved.status, 'paid');
    dom.window.close();
});

test('double confirm issues one commit request', async () => {
    const { dom, calls } = await harness({ ready: true });
    await dom.window.openBatch(batch().id);
    await waitFor(() => dom.window.document.getElementById('import-commit').disabled === false, {
        message: 'a ready preview to enable commit',
    });
    const first = dom.window.commitBatch();
    const second = dom.window.commitBatch();
    await Promise.all([first, second]);
    assert.equal(calls.commit, 1, 'the second activation is ignored while committing');
    dom.window.close();
});

test('a stale-preview conflict is reported and nothing is committed twice', async () => {
    const { dom, calls } = await harness({ ready: true, commitStatus: 409 });
    await dom.window.openBatch(batch().id);
    await waitFor(() => dom.window.document.getElementById('import-commit').disabled === false, {
        message: 'a ready preview to enable commit',
    });
    await dom.window.commitBatch();
    assert.equal(calls.commit, 1);
    assert.match(dom.window.document.getElementById('import-error').textContent, /stale/i);
    dom.window.close();
});

test('one, two or three sources stage independently and a retry replaces the failed one', async () => {
    const { dom, calls } = await harness({ ready: false });
    await dom.window.openBatch(batch().id);
    attachFile(dom, 'import-time-file', 'time.csv', 'text/csv');
    attachFile(dom, 'import-expenses-file', 'expenses.csv', 'text/csv');
    attachFile(dom, 'import-invoices-file', 'invoices.zip', 'application/zip');
    await dom.window.upload('time');
    await dom.window.upload('expenses');
    await dom.window.upload('invoices');
    assert.deepEqual(
        calls.uploads.map((path) => path.split('/').pop()),
        ['time', 'expenses', 'invoices'],
        'every source kind stages through its own route',
    );

    // A rejected upload reports why, keeps the staged batch, and a second
    // attempt for that kind is accepted.
    const { dom: failing, calls: failed } = await harness({ ready: false });
    const baseFetch = failing.window.fetch;
    let reject = true;
    failing.window.fetch = async (url, opts = {}) => {
        if ((opts.method || 'GET') === 'PUT' && String(url).includes('/files/time') && reject) {
            reject = false;
            return response(422, {
                error: { code: 'validation_failed', message: 'time CSV cannot be parsed: 1 issue(s)' },
            });
        }
        return baseFetch(url, opts);
    };
    await failing.window.openBatch(batch().id);
    attachFile(failing, 'import-time-file', 'time.csv', 'text/csv');
    await failing.window.upload('time');
    assert.match(failing.window.document.getElementById('import-error').textContent, /cannot be parsed/);
    await failing.window.upload('time');
    assert.equal(failed.uploads.length, 1, 'the retry staged the replacement source');
    dom.window.close();
    failing.window.close();
});

test('row and issue filters page the preview without refetching it', async () => {
    const { dom, calls } = await harness({ ready: false });
    await dom.window.openBatch(batch().id);
    await waitFor(() => dom.window.document.querySelector('#import-rows tbody tr'), {
        message: 'preview rows to render',
    });
    const previews = calls.preview;
    const document = dom.window.document;
    assert.equal(document.querySelectorAll('#import-rows tbody tr').length, 1, 'only the Time rows');
    assert.equal(document.querySelectorAll('#import-issue-list tbody tr').length, 1);

    const rowFilter = document.getElementById('import-row-filter');
    rowFilter.value = 'blocking';
    rowFilter.dispatchEvent(new dom.window.Event('change'));
    assert.equal(document.querySelectorAll('#import-rows tbody tr').length, 0);
    assert.match(document.getElementById('import-row-page').textContent, /No rows match this filter/);

    rowFilter.value = 'accepted';
    rowFilter.dispatchEvent(new dom.window.Event('change'));
    assert.equal(document.querySelectorAll('#import-rows tbody tr').length, 0, 'nothing is accepted yet');

    rowFilter.value = 'all';
    rowFilter.dispatchEvent(new dom.window.Event('change'));
    assert.equal(document.querySelectorAll('#import-rows tbody tr').length, 1);

    const issueFilter = document.getElementById('import-issue-filter');
    issueFilter.value = 'all';
    issueFilter.dispatchEvent(new dom.window.Event('change'));
    assert.match(document.getElementById('import-issue-page').textContent, /Issues 1–1 of 1/);
    issueFilter.value = 'blocking';
    issueFilter.dispatchEvent(new dom.window.Event('change'));
    assert.equal(document.querySelectorAll('#import-issue-list tbody tr').length, 1);

    document.getElementById('import-tab-invoices').click();
    assert.equal(document.querySelectorAll('#import-rows tbody tr').length, 1, 'only the invoice row');
    assert.equal(document.querySelectorAll('#import-issue-list tbody tr').length, 0);
    assert.equal(calls.preview, previews, 'filtering and tab switches never recompute the preview');
    dom.window.close();
});

test('discarding a draft batch confirms once, deletes it and clears the workspace', async () => {
    const { dom, calls } = await harness({ ready: false });
    await dom.window.openBatch(batch().id);
    await waitFor(() => dom.window.document.querySelector('#import-mapping h3'), {
        message: 'the batch workspace to render',
    });
    const pending = dom.window.discardBatch();
    await waitFor(() => dom.window.document.getElementById('dlg-ok').textContent === 'Discard batch', {
        message: 'a discard confirmation',
    });
    dom.window.document.getElementById('dlg-ok').click();
    await pending;
    assert.equal(calls.discard, 1, 'one DELETE for the batch');
    const document = dom.window.document;
    assert.equal(document.getElementById('import-batch-label').textContent, '');
    assert.equal(document.getElementById('import-mapping').textContent, '');
    assert.equal(document.getElementById('import-commit').disabled, true);
    assert.match(document.getElementById('import-status').textContent, /discarded/i);
    dom.window.close();
});

test('a committed batch offers no discard and never calls DELETE', async () => {
    const committed = batch({ state: 'committed', commit: { committed_at: '2026-01-02T00:00:00Z' } });
    const { dom, calls } = await harness({ ready: true, getBatch: committed });
    await dom.window.openBatch(committed.id);
    await dom.window.discardBatch();
    assert.equal(calls.discard, 0);
    assert.equal(dom.window.document.getElementById('import-discard').disabled, true);
    dom.window.close();
});
