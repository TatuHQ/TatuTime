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
        if (path.endsWith('/api/customers')) return response(200, { customers: [{ id: 'c1', name: 'Acme' }] });
        if (path.endsWith('/api/categories')) return response(200, { categories: [{ id: 'cat1', name: 'Travel' }] });
        if (path.endsWith('/projects')) return response(200, { projects: [{ code: 'WEB', name: 'Website' }] });
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

test('mapping controls are built from source values and link the original PDF', async () => {
    const { dom } = await harness({ ready: false });
    await dom.window.openBatch(batch().id);
    await waitFor(() => dom.window.document.querySelector('#import-mapping h3'), {
        message: 'mapping controls to render',
    });
    const mapping = dom.window.document.getElementById('import-mapping');
    assert.match(mapping.textContent, /People/);
    assert.match(mapping.textContent, /Customers/);
    assert.match(mapping.textContent, /Projects/);
    assert.match(mapping.textContent, /Tasks/);
    assert.match(mapping.textContent, /Invoices/);
    const pdf = [...mapping.querySelectorAll('a')].find((a) => a.textContent === 'Original PDF');
    assert.ok(pdf, 'invoice PDF link exists');
    assert.match(pdf.getAttribute('href'), /\/invoices\/b{64}\/pdf$/);
    assert.equal(dom.window.document.getElementById('import-commit').disabled, true);
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
    assert.equal(document.querySelectorAll('#import-rows tbody tr').length, 2, 'time and invoice rows');
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
    assert.equal(document.querySelectorAll('#import-rows tbody tr').length, 2);

    const issueFilter = document.getElementById('import-issue-filter');
    issueFilter.value = 'all';
    issueFilter.dispatchEvent(new dom.window.Event('change'));
    assert.match(document.getElementById('import-issue-page').textContent, /Issues 1–1 of 1/);
    issueFilter.value = 'blocking';
    issueFilter.dispatchEvent(new dom.window.Event('change'));
    assert.equal(document.querySelectorAll('#import-issue-list tbody tr').length, 1);
    assert.equal(calls.preview, previews, 'filtering never recomputes the preview');
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
