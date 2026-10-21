// imports.js — admin Harvest import view (#403).
//
// Staging -> reviewed mapping -> preview -> explicit commit. Every value is
// inserted with textContent; the source bytes never reach this module (only
// counts/hashes and the computed preview), and the original invoice PDF is
// opened through the admin-only archived-member route.

import { api } from './api.js';
import { $, announce, clearFormError, el, formatMoney, showFormError, statusChip } from './dom.js';

const BASE = '/admin/imports/harvest';
let currentBatch = null;
let currentPreview = null;

function panelVisible() {
    const panel = $('settings-panel-import');
    return Boolean(panel && !panel.hidden);
}

function showError(err) {
    const target = $('import-error');
    if (target) showFormError(target, err);
    announce(err.message || 'The import request failed.');
}

function clearError() {
    const target = $('import-error');
    if (target) clearFormError(target);
}

function batchId() {
    if (!currentBatch) {
        announce('Create or open a batch first.');
        return null;
    }
    return currentBatch.id;
}

function renderBatch(batch) {
    currentBatch = batch;
    $('import-batch-label').textContent =
        `Batch ${batch.id} · ${batch.namespace} · ${batch.state}`;
    $('import-choices').value = JSON.stringify(batch.choices, null, 2);
    const committed = batch.state === 'committed';
    for (const id of ['import-time-upload', 'import-expenses-upload', 'import-invoices-upload',
        'import-save', 'import-preview']) {
        $(id).disabled = committed;
    }
    if (committed) {
        $('import-ready').textContent = `Committed ${batch.commit?.committed_at || ''}`;
        $('import-commit').disabled = true;
    }
}

export async function refreshImports() {
    if (!panelVisible() || !$('import-history')) return;
    $('import-status').textContent = 'Loading import history...';
    try {
        const data = await api.get(`${BASE}?limit=50`);
        const tbody = $('import-history').querySelector('tbody');
        tbody.textContent = '';
        for (const batch of data.items || []) {
            const sources = ['time', 'expenses', 'invoices']
                .filter((kind) => batch.files?.[kind])
                .join(', ') || 'none';
            tbody.appendChild(el('tr', {}, [
                el('th', { attrs: { scope: 'row' }, text: batch.created_at }),
                el('td', { attrs: { 'data-heading': 'Namespace' }, text: batch.namespace }),
                el('td', { attrs: { 'data-heading': 'State' } }, [statusChip(batch.state)]),
                el('td', { attrs: { 'data-heading': 'Sources' }, text: sources }),
                el('td', { attrs: { 'data-heading': 'Actions' } }, [
                    el('button', {
                        type: 'button', cls: 'link', text: 'Open',
                        on: { click: () => openBatch(batch.id) },
                    }),
                ]),
            ]));
        }
        $('import-status').textContent = `${data.total || 0} batch(es).`;
    } catch (err) {
        $('import-status').textContent = `Could not load import history: ${err.message}`;
    }
}

export async function newBatch() {
    clearError();
    try {
        const batch = await api.post(BASE, { namespace: 'harvest' });
        renderBatch(batch);
        $('import-ready').textContent = 'Draft created. Upload at least one source.';
        await refreshImports();
    } catch (err) {
        showError(err);
    }
}

export async function openBatch(id) {
    clearError();
    currentPreview = null;
    $('import-commit').disabled = true;
    try {
        renderBatch(await api.get(`${BASE}/${id}`));
        $('import-ready').textContent = 'Batch loaded. Recompute the preview after any change.';
    } catch (err) {
        showError(err);
    }
}

async function upload(kind) {
    const id = batchId();
    if (!id) return;
    const input = $(`import-${kind}-file`);
    const file = input?.files?.[0];
    if (!file) {
        announce('Choose a file first.');
        return;
    }
    clearError();
    $('import-status').textContent = `Uploading ${file.name}...`;
    try {
        const batch = await api.upload(
            `${BASE}/${id}/files/${kind}`,
            file,
            kind === 'invoices' ? 'application/zip' : 'text/csv',
        );
        renderBatch(batch);
        $('import-status').textContent = `Staged ${file.name} (${file.size} bytes).`;
        announce('Source staged. Recompute the preview.');
    } catch (err) {
        $('import-status').textContent = '';
        showError(err);
    }
}

async function saveMapping() {
    const id = batchId();
    if (!id) return;
    clearError();
    let choices;
    try {
        choices = JSON.parse($('import-choices').value || '{}');
    } catch {
        showError(new Error('Mapping is not valid JSON.'));
        return;
    }
    try {
        renderBatch(await api.put(`${BASE}/${id}/review`, choices));
        $('import-ready').textContent = 'Mapping saved. Recompute the preview.';
        $('import-commit').disabled = true;
        announce('Mapping saved.');
    } catch (err) {
        showError(err);
    }
}

function renderPreview(preview) {
    currentPreview = preview;
    const tbody = $('import-totals').querySelector('tbody');
    tbody.textContent = '';
    for (const row of preview.totals || []) {
        tbody.appendChild(el('tr', {}, [
            el('th', { attrs: { scope: 'row' }, text: row.source }),
            el('td', { attrs: { 'data-heading': 'Currency' }, text: row.currency }),
            el('td', {
                attrs: { 'data-heading': 'Accepted' },
                text: `${row.accepted_count} · ${formatMoney(row.accepted_minor)}`,
            }),
            el('td', {
                attrs: { 'data-heading': 'Skipped' },
                text: `${row.skipped_count} · ${formatMoney(row.skipped_minor)}`,
            }),
            el('td', { attrs: { 'data-heading': 'Cost' }, text: formatMoney(row.cost_minor) }),
            el('td', {
                attrs: { 'data-heading': 'Opening paid' },
                text: formatMoney(row.opening_paid_minor),
            }),
        ]));
    }
    const rows = $('import-rows').querySelector('tbody');
    rows.textContent = '';
    const append = (source, row) => {
        const amount = row.amount_minor ?? row.total_minor;
        const issues = (row.issues || []).length;
        rows.appendChild(el('tr', {}, [
            el('th', { attrs: { scope: 'row' }, text: source }),
            el('td', {
                attrs: { 'data-heading': 'Record' },
                text: `${row.record ?? row.original_name ?? row.source_key}`,
            }),
            el('td', { attrs: { 'data-heading': 'Decision' } }, [statusChip(row.decision)]),
            el('td', {
                attrs: { 'data-heading': 'Amount' },
                text: amount === undefined ? '' : formatMoney(amount),
            }),
            el('td', { attrs: { 'data-heading': 'Issues' }, text: String(issues) }),
        ]));
    };
    for (const row of preview.time || []) append('time', row);
    for (const row of preview.expenses || []) append('expense', row);
    for (const row of preview.invoices || []) append('invoice', row);
    $('import-issues').textContent = preview.blocking
        ? `${preview.blocking} blocking issue(s); commit stays disabled.`
        : 'No blocking issues.';
    $('import-ready').textContent = preview.ready
        ? `Ready to commit (revision ${preview.revision.slice(0, 12)}…).`
        : 'Not ready: review mappings and issues.';
    $('import-commit').disabled = !preview.ready;
}

async function previewBatch() {
    const id = batchId();
    if (!id) return;
    clearError();
    try {
        renderPreview(await api.post(`${BASE}/${id}/preview`));
        announce('Preview computed.');
    } catch (err) {
        showError(err);
    }
}

async function commitBatch() {
    const id = batchId();
    if (!id || !currentPreview?.ready) return;
    clearError();
    try {
        const outcome = await api.post(`${BASE}/${id}/commit`, {
            revision: currentPreview.revision,
        });
        $('import-ready').textContent =
            `Committed ${outcome.entries.length} entr(ies), ${outcome.expenses.length} expense(s), ${outcome.invoices.length} invoice(s).`;
        $('import-commit').disabled = true;
        announce('Import committed.');
        await refreshImports();
        await openBatch(id);
    } catch (err) {
        showError(err);
        await refreshImports();
    }
}

let wired = false;

export function wireImports() {
    if (wired || !$('import-new')) return;
    wired = true;
    $('import-new').addEventListener('click', newBatch);
    // Entering the Import subtab must load its history; refreshSettings only runs
    // when the top-level Settings tab is activated.
    $('settings-tab-import').addEventListener('click', () => {
        refreshImports();
    });
    $('import-time-upload').addEventListener('click', () => upload('time'));
    $('import-expenses-upload').addEventListener('click', () => upload('expenses'));
    $('import-invoices-upload').addEventListener('click', () => upload('invoices'));
    $('import-save').addEventListener('click', saveMapping);
    $('import-preview').addEventListener('click', previewBatch);
    $('import-commit').addEventListener('click', commitBatch);
}
