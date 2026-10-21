// imports.js — admin Harvest import view (#403).
//
// Staging -> reviewed mapping -> preview -> explicit commit. Source values are
// shown next to labelled controls for the admin to map; nothing is inferred.
// Every value is inserted with textContent, source bytes never reach this
// module, and original invoice PDFs open through the admin-only archived-member
// route.

import { api } from './api.js';
import { $, announce, clearFormError, el, formatMoney, showFormError, statusChip } from './dom.js';

const BASE = '/admin/imports/harvest';
let currentBatch = null;
let currentPreview = null;
let draft = null;
let options = { users: [], customers: [], categories: [], projects: [], tasks: [] };
let optionsLoaded = false;
let committing = false;

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

function personKey(person, employeeId) {
    return `${person.trim().toLowerCase()}|${employeeId.trim().toLowerCase()}`;
}

function blankChoices() {
    return {
        users: [], customers: [], projects: [], tasks: [], categories: [],
        invoices: [], excluded: [], invoice_links: [],
    };
}

function renderBatch(batch) {
    currentBatch = batch;
    $('import-batch-label').textContent = `Batch ${batch.id} · ${batch.namespace} · ${batch.state}`;
    draft = { ...blankChoices(), ...(batch.choices || {}) };
    for (const key of Object.keys(blankChoices())) {
        if (!Array.isArray(draft[key])) draft[key] = blankChoices()[key];
    }
    $('import-choices').value = JSON.stringify(draft, null, 2);
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

async function loadOptions() {
    if (optionsLoaded) return;
    const [users, customers, categories] = await Promise.all([
        api.get('/users'),
        api.get('/customers'),
        api.get('/categories'),
    ]);
    const projects = [];
    const tasks = [];
    for (const customer of customers.customers || []) {
        const list = await api.get(`/customers/${customer.id}/projects`);
        for (const project of list.projects || []) {
            projects.push({ ...project, customer_id: customer.id, customer_name: customer.name });
            const taskList = await api.get(`/customers/${customer.id}/projects/${project.code}/tasks`);
            for (const task of taskList.tasks || []) {
                tasks.push({
                    ...task, customer_id: customer.id, customer_name: customer.name,
                    project_code: project.code, project_name: project.name,
                });
            }
        }
    }
    options = {
        users: users.users || [],
        customers: customers.customers || [],
        categories: categories.categories || [],
        projects,
        tasks,
    };
    optionsLoaded = true;
}

function selectControl(id, value, choices, onChange) {
    const select = el('select', {
        cls: 'import-map-control', attrs: { id }, on: { change: (event) => onChange(event.target.value) },
    });
    for (const [optionValue, text] of choices) {
        const option = el('option', { value: optionValue, text });
        if (optionValue === value) option.selected = true;
        select.appendChild(option);
    }
    return select;
}

function textControl(id, value, onInput, type = 'text') {
    return el('input', {
        cls: 'import-map-control',
        attrs: { id, type, value: value ?? '' },
        on: { input: (event) => onInput(event.target.value) },
    });
}

function checkboxControl(id, checked, onChange) {
    const input = el('input', { cls: 'import-map-control', attrs: { id, type: 'checkbox' } });
    input.checked = Boolean(checked);
    input.addEventListener('change', () => onChange(input.checked));
    return input;
}

function labelFor(id, text) {
    return el('label', { cls: 'import-map-label', attrs: { for: id }, text });
}

function section(title, headers, rows) {
    return el('section', { cls: 'import-map-section' }, [
        el('h3', { text: title }),
        el('div', { cls: 'table-wrap' }, [
            el('table', { cls: 'import-map-table' }, [
                el('thead', {}, [el('tr', {}, headers.map((header) => el('th', { attrs: { scope: 'col' }, text: header })))]),
                el('tbody', {}, rows),
            ]),
        ]),
    ]);
}

function personLabel(row) {
    return `${row.source_person}${row.source_employee_id ? ` (${row.source_employee_id})` : ''}`;
}

function ensurePerson(row) {
    const key = personKey(row.source_person, row.source_employee_id);
    let choice = draft.users.find((entry) => personKey(entry.person, entry.employee_id || '') === key);
    if (!choice) {
        choice = { person: row.source_person, employee_id: row.source_employee_id || '', user_id: null, skip: false };
        draft.users.push(choice);
    }
    return choice;
}

function ensureCustomer(sourceClient) {
    let choice = draft.customers.find((entry) => entry.source_client === sourceClient);
    if (!choice) {
        choice = { action: 'unresolved', source_client: sourceClient };
        draft.customers.push(choice);
    }
    return choice;
}

function ensureProject(sourceClient, sourceProject, sourceCode) {
    const key = `${sourceClient}|${sourceProject}|${sourceCode}`;
    let choice = draft.projects.find((entry) =>
        `${entry.source_client}|${entry.source_project}|${entry.source_code || ''}` === key);
    if (!choice) {
        choice = {
            action: 'unresolved', source_client: sourceClient,
            source_project: sourceProject, source_code: sourceCode,
        };
        draft.projects.push(choice);
    }
    return choice;
}

function ensureTask(sourceTask) {
    let choice = draft.tasks.find((entry) => entry.source_task === sourceTask);
    if (!choice) {
        choice = { action: 'unresolved', source_task: sourceTask };
        draft.tasks.push(choice);
    }
    return choice;
}

function ensureCategory(sourceCategory) {
    let choice = draft.categories.find((entry) => entry.source_category === sourceCategory);
    if (!choice) {
        choice = { action: 'unresolved', source_category: sourceCategory };
        draft.categories.push(choice);
    }
    return choice;
}

function ensureInvoice(row) {
    let choice = draft.invoices.find((entry) => entry.sha256 === row.pdf_sha256);
    if (!choice) {
        choice = {
            sha256: row.pdf_sha256,
            customer_id: row.customer_id || null,
            original_number: row.original_number || '',
            currency: row.currency || '',
            invoice_date: row.invoice_date || null,
            period_from: row.period_from || null,
            period_to: row.period_to || null,
            total_minor: row.total_minor ?? null,
            opening_paid_minor: row.opening_paid_minor || 0,
            status: row.status || null,
            excluded: false,
        };
        draft.invoices.push(choice);
    }
    return choice;
}

function moneyInput(id, minor, onChange) {
    return textControl(id, minor == null ? '' : formatMoney(minor), (value) => {
        const number = Number(value);
        onChange(Number.isFinite(number) ? Math.round(number * 100) : null);
    }, 'number');
}

function renderMapping(preview) {
    const host = $('import-mapping');
    if (!host) return;
    host.textContent = '';
    if (!preview) {
        host.appendChild(el('p', { cls: 'p-form-help-text', text: 'Upload a source and run Preview to build the mapping controls.' }));
        return;
    }
    const timeRows = preview.time || [];
    const expenseRows = preview.expenses || [];
    const rows = [...timeRows, ...expenseRows];

    const people = new Map();
    for (const row of rows) people.set(personKey(row.source_person, row.source_employee_id), row);
    host.appendChild(section('People', ['Source person', 'Existing user'], [...people.values()].map((row) => {
        const choice = ensurePerson(row);
        const select = selectControl(
            `import-user-${personKey(row.source_person, row.source_employee_id)}`,
            choice.skip ? '__skip' : (choice.user_id || ''),
            [['', 'Select user…'], ['__skip', 'Skip attribution'],
            ...options.users.map((user) => [user.id, `${user.name} <${user.email}>`])],
            (value) => {
                if (value === '__skip') {
                    choice.skip = true;
                    choice.user_id = null;
                } else {
                    choice.skip = false;
                    choice.user_id = value || null;
                }
            },
        );
        return el('tr', {}, [
            el('th', { attrs: { scope: 'row' }, text: personLabel(row) }),
            el('td', {}, [labelFor(select.id, `Map ${personLabel(row)}`), select]),
        ]);
    })));

    const clients = [...new Set(rows.map((row) => row.source_client))];
    host.appendChild(section('Customers', ['Source client', 'Map to'], clients.map((client) => {
        const choice = ensureCustomer(client);
        const selected = choice.action === 'existing' ? choice.customer_id
            : choice.action === 'create' ? '__create' : '';
        const select = selectControl(`import-customer-${client}`, selected,
            [['', 'Select customer…'], ['__create', 'Create customer'],
            ...options.customers.map((customer) => [customer.id, customer.name])],
            (value) => {
                if (value === '__create') {
                    draft.customers[draft.customers.indexOf(choice)] = {
                        action: 'create', source_client: client, name: client,
                        currency: 'EUR', default_rate_minor: 0,
                    };
                } else if (value) {
                    draft.customers[draft.customers.indexOf(choice)] = {
                        action: 'existing', source_client: client, customer_id: value,
                    };
                } else {
                    choice.action = 'unresolved';
                }
            });
        return el('tr', {}, [
            el('th', { attrs: { scope: 'row' }, text: client }),
            el('td', {}, [labelFor(select.id, `Map customer ${client}`), select]),
        ]);
    })));

    const projectKeys = new Set(rows.map((row) => `${row.source_client}|${row.source_project}|${row.source_code}`));
    host.appendChild(section('Projects', ['Source project', 'Map to'], [...projectKeys].map((key) => {
        const row = rows.find((entry) => `${entry.source_client}|${entry.source_project}|${entry.source_code}` === key);
        const choice = ensureProject(row.source_client, row.source_project, row.source_code);
        const selected = choice.action === 'existing' ? `${choice.customer_id}|${choice.code}`
            : choice.action === 'create' ? '__create' : choice.action === 'skip' ? '__skip' : '';
        const select = selectControl(`import-project-${key}`, selected,
            [['', 'Select project…'], ['__skip', 'Skip project (expenses only)'], ['__create', 'Create project'],
            ...options.projects.map((project) => [
                `${project.customer_id}|${project.code}`,
                `${project.customer_name} — ${project.code} ${project.name}`,
            ])],
            (value) => {
                if (value === '__create') {
                    draft.projects[draft.projects.indexOf(choice)] = {
                        action: 'create', source_client: row.source_client,
                        source_project: row.source_project, source_code: row.source_code,
                        customer_id: null, code: row.source_code || '', name: row.source_project,
                        currency: 'EUR', rate_minor: 0,
                    };
                } else if (value === '__skip') {
                    draft.projects[draft.projects.indexOf(choice)] = {
                        action: 'skip', source_client: row.source_client,
                        source_project: row.source_project, source_code: row.source_code,
                    };
                } else if (value) {
                    const [customerId, code] = value.split('|');
                    draft.projects[draft.projects.indexOf(choice)] = {
                        action: 'existing', source_client: row.source_client,
                        source_project: row.source_project, source_code: row.source_code,
                        customer_id: customerId, code,
                    };
                } else {
                    choice.action = 'unresolved';
                }
            });
        return el('tr', {}, [
            el('th', { attrs: { scope: 'row' }, text: `${row.source_client} — ${row.source_project} (${row.source_code || 'no code'})` }),
            el('td', {}, [labelFor(select.id, `Map project ${row.source_project}`), select]),
        ]);
    })));

    const taskNames = [...new Set(rows.map((row) => row.source_task).filter(Boolean))];
    if (taskNames.length) {
        host.appendChild(section('Tasks', ['Source task', 'Map to'], taskNames.map((task) => {
            const choice = ensureTask(task);
            const selected = choice.action === 'existing' ? choice.code
                : choice.action === 'create' ? '__create' : choice.action === 'skip' ? '__skip' : '';
            const select = selectControl(`import-task-${task}`, selected,
                [['', 'Select task…'], ['__skip', 'Skip task'], ['__create', 'Create task'],
                ...options.tasks.map((entry) => [entry.code, `${entry.customer_name} / ${entry.project_code} — ${entry.code}`])],
                (value) => {
                    if (value === '__create') {
                        draft.tasks[draft.tasks.indexOf(choice)] = {
                            action: 'create', source_task: task, code: task, name: task,
                        };
                    } else if (value === '__skip') {
                        draft.tasks[draft.tasks.indexOf(choice)] = { action: 'skip', source_task: task };
                    } else if (value) {
                        draft.tasks[draft.tasks.indexOf(choice)] = { action: 'existing', source_task: task, code: value };
                    } else {
                        choice.action = 'unresolved';
                    }
                });
            return el('tr', {}, [
                el('th', { attrs: { scope: 'row' }, text: task }),
                el('td', {}, [labelFor(select.id, `Map task ${task}`), select]),
            ]);
        })));
    }

    const categories = [...new Set(expenseRows.map((row) => row.source_category).filter(Boolean))];
    if (categories.length) {
        host.appendChild(section('Expense categories', ['Source category', 'Map to'], categories.map((category) => {
            const choice = ensureCategory(category);
            const selected = choice.action === 'existing' ? choice.category_id
                : choice.action === 'create' ? '__create' : '';
            const select = selectControl(`import-category-${category}`, selected,
                [['', 'Select category…'], ['__create', 'Create category'],
                ...options.categories.map((entry) => [entry.id, entry.name])],
                (value) => {
                    if (value === '__create') {
                        draft.categories[draft.categories.indexOf(choice)] = {
                            action: 'create', source_category: category, name: category, billable: true,
                        };
                    } else if (value) {
                        draft.categories[draft.categories.indexOf(choice)] = {
                            action: 'existing', source_category: category, category_id: value,
                        };
                    } else {
                        choice.action = 'unresolved';
                    }
                });
            return el('tr', {}, [
                el('th', { attrs: { scope: 'row' }, text: category }),
                el('td', {}, [labelFor(select.id, `Map category ${category}`), select]),
            ]);
        })));
    }

    if ((preview.invoices || []).length) {
        host.appendChild(el('h3', { text: 'Invoices' }));
        const card = el('div', { cls: 'import-invoice-list' });
        for (const row of preview.invoices) {
            const choice = ensureInvoice(row);
            const pdf = el('a', {
                attrs: {
                    href: `${BASE}/${currentBatch.id}/invoices/${row.pdf_sha256}/pdf`,
                    target: '_blank', rel: 'noopener',
                },
                text: 'Original PDF',
            });
            card.appendChild(el('fieldset', { cls: 'import-invoice-card' }, [
                el('legend', { text: row.original_name }),
                el('div', { cls: 'import-invoice-grid' }, [
                    el('div', {}, [labelFor(`import-inv-number-${row.pdf_sha256}`, 'Legal number'),
                    textControl(`import-inv-number-${row.pdf_sha256}`, choice.original_number, (value) => { choice.original_number = value; })]),
                    el('div', {}, [labelFor(`import-inv-customer-${row.pdf_sha256}`, 'Customer'),
                    selectControl(`import-inv-customer-${row.pdf_sha256}`, choice.customer_id || '',
                        [['', 'Select customer…'], ...options.customers.map((customer) => [customer.id, customer.name])],
                        (value) => { choice.customer_id = value || null; })]),
                    el('div', {}, [labelFor(`import-inv-currency-${row.pdf_sha256}`, 'Currency'),
                    textControl(`import-inv-currency-${row.pdf_sha256}`, choice.currency || '', (value) => { choice.currency = value; })]),
                    el('div', {}, [labelFor(`import-inv-date-${row.pdf_sha256}`, 'Invoice date'),
                    textControl(`import-inv-date-${row.pdf_sha256}`, choice.invoice_date || '', (value) => { choice.invoice_date = value || null; }, 'date')]),
                    el('div', {}, [labelFor(`import-inv-from-${row.pdf_sha256}`, 'Period from'),
                    textControl(`import-inv-from-${row.pdf_sha256}`, choice.period_from || '', (value) => { choice.period_from = value || null; }, 'date')]),
                    el('div', {}, [labelFor(`import-inv-to-${row.pdf_sha256}`, 'Period to'),
                    textControl(`import-inv-to-${row.pdf_sha256}`, choice.period_to || '', (value) => { choice.period_to = value || null; }, 'date')]),
                    el('div', {}, [labelFor(`import-inv-total-${row.pdf_sha256}`, 'Total'),
                    moneyInput(`import-inv-total-${row.pdf_sha256}`, choice.total_minor, (value) => { choice.total_minor = value; })]),
                    el('div', {}, [labelFor(`import-inv-paid-${row.pdf_sha256}`, 'Opening paid'),
                    moneyInput(`import-inv-paid-${row.pdf_sha256}`, choice.opening_paid_minor, (value) => { choice.opening_paid_minor = value ?? 0; })]),
                    el('div', {}, [labelFor(`import-inv-status-${row.pdf_sha256}`, 'Status'),
                    selectControl(`import-inv-status-${row.pdf_sha256}`, choice.status || '',
                        [['', 'Select status…'], ['issued', 'Issued'], ['partly_paid', 'Partly paid'],
                        ['paid', 'Paid'], ['written_off', 'Written off']],
                        (value) => { choice.status = value || null; })]),
                    el('div', {}, [labelFor(`import-inv-exclude-${row.pdf_sha256}`, 'Exclude'),
                    checkboxControl(`import-inv-exclude-${row.pdf_sha256}`, choice.excluded,
                        (checked) => { choice.excluded = checked; })]),
                ]),
                el('p', { cls: 'p-form-help-text' }, [pdf]),
            ]));
        }
        host.appendChild(card);
    }
    $('import-choices').value = JSON.stringify(draft, null, 2);
}

export async function refreshImports() {
    if (!panelVisible() || !$('import-history')) return;
    $('import-status').textContent = 'Loading import history...';
    try {
        const data = await api.get(`${BASE}?limit=50`);
        const tbody = $('import-history').querySelector('tbody');
        tbody.textContent = '';
        for (const batch of data.items || []) {
            const sources = ['time', 'expenses', 'invoices'].filter((kind) => batch.files?.[kind]).join(', ') || 'none';
            tbody.appendChild(el('tr', {}, [
                el('th', { attrs: { scope: 'row' }, text: batch.created_at }),
                el('td', { attrs: { 'data-heading': 'Namespace' }, text: batch.namespace }),
                el('td', { attrs: { 'data-heading': 'State' } }, [statusChip(batch.state)]),
                el('td', { attrs: { 'data-heading': 'Sources' }, text: sources }),
                el('td', { attrs: { 'data-heading': 'Actions' } }, [
                    el('button', { type: 'button', cls: 'link', text: 'Open', on: { click: () => openBatch(batch.id) } }),
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
        await loadOptions();
        const batch = await api.post(BASE, { namespace: 'harvest' });
        renderBatch(batch);
        currentPreview = null;
        $('import-mapping').textContent = '';
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
        await loadOptions();
        renderBatch(await api.get(`${BASE}/${id}`));
        $('import-ready').textContent = 'Batch loaded. Recompute the preview after any change.';
        if (currentBatch.files && Object.values(currentBatch.files).some(Boolean)) {
            await previewBatch();
        }
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
        const batch = await api.upload(`${BASE}/${id}/files/${kind}`, file,
            kind === 'invoices' ? 'application/zip' : 'text/csv');
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
    try {
        $('import-choices').value = JSON.stringify(draft, null, 2);
        renderBatch(await api.put(`${BASE}/${id}/review`, draft));
        $('import-ready').textContent = 'Mapping saved. Recompute the preview.';
        $('import-commit').disabled = true;
        announce('Mapping saved.');
    } catch (err) {
        showError(err);
    }
}

function invoiceLinkOptions(preview) {
    return (preview.invoices || []).map((invoice) =>
        [invoice.pdf_sha256, `${invoice.original_number || invoice.original_name}`]);
}

function linkRow(preview, sourceKey, invoiced) {
    if (!invoiced || !(preview.invoices || []).length) return null;
    const existing = draft.invoice_links.find((link) => link.source_key === sourceKey);
    const select = selectControl(`import-link-${sourceKey}`, existing?.invoice_sha256 || '',
        [['', 'Link to invoice…'], ...invoiceLinkOptions(preview)], () => { });
    const button = el('button', {
        type: 'button', cls: 'link', text: 'Link',
        on: {
            click: () => {
                const sha = select.value;
                draft.invoice_links = draft.invoice_links.filter((link) => link.source_key !== sourceKey);
                if (sha) draft.invoice_links.push({ source_key: sourceKey, invoice_sha256: sha });
                $('import-choices').value = JSON.stringify(draft, null, 2);
                announce(sha ? 'Row linked; recompute the preview.' : 'Link removed; recompute the preview.');
            },
        },
    });
    return el('div', { cls: 'import-row-link' }, [select, button]);
}

function renderPreview(preview) {
    currentPreview = preview;
    const tbody = $('import-totals').querySelector('tbody');
    tbody.textContent = '';
    for (const row of preview.totals || []) {
        tbody.appendChild(el('tr', {}, [
            el('th', { attrs: { scope: 'row' }, text: row.source }),
            el('td', { attrs: { 'data-heading': 'Currency' }, text: row.currency }),
            el('td', { attrs: { 'data-heading': 'Accepted' }, text: `${row.accepted_count} · ${formatMoney(row.accepted_minor)}` }),
            el('td', { attrs: { 'data-heading': 'Skipped' }, text: `${row.skipped_count} · ${formatMoney(row.skipped_minor)}` }),
            el('td', { attrs: { 'data-heading': 'Cost' }, text: formatMoney(row.cost_minor) }),
            el('td', { attrs: { 'data-heading': 'Opening paid' }, text: formatMoney(row.opening_paid_minor) }),
        ]));
    }
    const rows = $('import-rows').querySelector('tbody');
    rows.textContent = '';
    const append = (source, row) => {
        const amount = row.amount_minor ?? row.total_minor;
        const issues = (row.issues || []).length;
        const link = source === 'invoice' ? el('a', {
            attrs: {
                href: `${BASE}/${currentBatch.id}/invoices/${row.pdf_sha256}/pdf`,
                target: '_blank', rel: 'noopener',
            },
            text: 'PDF',
        }) : linkRow(preview, row.source_key, row.invoiced);
        rows.appendChild(el('tr', {}, [
            el('th', { attrs: { scope: 'row' }, text: source }),
            el('td', { attrs: { 'data-heading': 'Record' }, text: `${row.record ?? row.original_name ?? row.source_key}` }),
            el('td', { attrs: { 'data-heading': 'Decision' } }, [statusChip(row.decision)]),
            el('td', { attrs: { 'data-heading': 'Amount' }, text: amount === undefined ? '' : formatMoney(amount) }),
            el('td', { attrs: { 'data-heading': 'Issues' }, text: String(issues) }),
            el('td', { attrs: { 'data-heading': 'Link' } }, [link]),
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
    renderMapping(preview);
}

async function previewBatch() {
    const id = batchId();
    if (!id) return;
    clearError();
    try {
        $('import-choices').value = JSON.stringify(draft, null, 2);
        renderBatch(await api.put(`${BASE}/${id}/review`, draft));
        renderPreview(await api.post(`${BASE}/${id}/preview`));
        announce('Preview computed.');
    } catch (err) {
        showError(err);
    }
}

async function commitBatch() {
    const id = batchId();
    if (!id || !currentPreview?.ready || committing) return;
    committing = true;
    $('import-commit').disabled = true;
    clearError();
    try {
        const outcome = await api.post(`${BASE}/${id}/commit`, { revision: currentPreview.revision });
        $('import-ready').textContent =
            `Committed ${outcome.entries.length} entr(ies), ${outcome.expenses.length} expense(s), ${outcome.invoices.length} invoice(s).`;
        announce('Import committed.');
        await refreshImports();
        await openBatch(id);
    } catch (err) {
        showError(err);
        await refreshImports();
    } finally {
        committing = false;
        $('import-commit').disabled = !currentPreview?.ready;
    }
}

let wired = false;

export function wireImports() {
    if (wired || !$('import-new')) return;
    wired = true;
    $('import-new').addEventListener('click', newBatch);
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
