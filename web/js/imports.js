// imports.js — admin Harvest import view (#403).
//
// Staging -> reviewed mapping -> preview -> explicit commit. Source values are
// shown next to labelled controls for the admin to map; nothing is inferred.
// Every value is inserted with textContent, source bytes never reach this
// module, and original invoice PDFs open through the admin-only archived-member
// route.

import { api } from './api.js';
import { $, announce, askConfirm, clearFormError, el, formatMoney, showFormError, statusChip } from './dom.js';

const BASE = '/admin/imports/harvest';
const PAGE_SIZE = 25;
// One tab per source. `row` is the name the preview uses for rows, issues and
// totals; `kind` is the upload/staging name.
const SOURCE_TABS = {
    time: { row: 'time', label: 'Time' },
    expenses: { row: 'expense', label: 'Expenses' },
    invoices: { row: 'invoice', label: 'Invoices' },
};
let importSource = 'time';
let currentBatch = null;
let currentPreview = null;
let draft = null;
let options = { users: [], customers: [], categories: [], projects: [], tasks: [] };
let optionsLoaded = false;
let committing = false;
let rowFilter = 'all';
let rowPage = 0;
let issueFilter = 'all';
let issuePage = 0;
// Project selects rendered by the current mapping: key -> { select, choice }.
// A customer change rebuilds each one in place, so focus and scroll position
// stay where the admin left them.
let importProjectSelects = new Map();

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

// The `import-choices` box mirrors the in-memory mapping, so a value the admin
// types (and the settlement it implies) is visible without a save round trip.
function serializeChoices() {
    const box = $('import-choices');
    if (box) box.value = JSON.stringify(draft, null, 2);
}

function renderBatch(batch) {
    currentBatch = batch;
    $('import-batch-label').textContent = `Batch ${batch.id} · ${batch.namespace} · ${batch.state}`;
    draft = { ...blankChoices(), ...(batch.choices || {}) };
    for (const key of Object.keys(blankChoices())) {
        if (!Array.isArray(draft[key])) draft[key] = blankChoices()[key];
    }
    serializeChoices();
    const committed = batch.state === 'committed';
    for (const id of ['import-time-upload', 'import-expenses-upload', 'import-invoices-upload',
        'import-save', 'import-preview', 'import-discard']) {
        $(id).disabled = committed;
    }
    if (committed) {
        $('import-ready').textContent = `Committed ${batch.commit?.committed_at || ''}`;
        $('import-commit').disabled = true;
    }
}

function sourceKey() {
    return SOURCE_TABS[importSource].row;
}

function setImportSource(kind, moveFocus = false) {
    if (!SOURCE_TABS[kind]) return;
    importSource = kind;
    for (const key of Object.keys(SOURCE_TABS)) {
        const tab = $(`import-tab-${key}`);
        const panel = $(`import-panel-${key}`);
        if (!tab || !panel) continue;
        const active = key === kind;
        tab.setAttribute('aria-selected', String(active));
        tab.tabIndex = active ? 0 : -1;
        panel.hidden = !active;
    }
    if (moveFocus) $(`import-tab-${kind}`)?.focus();
    if (currentPreview) renderPreview(currentPreview);
    else renderMapping(null);
}

function wireImportTabs() {
    const tabs = Object.keys(SOURCE_TABS)
        .map((kind) => $(`import-tab-${kind}`))
        .filter(Boolean);
    const activate = (tab, moveFocus) => {
        setImportSource(tab.id.replace('import-tab-', ''), moveFocus);
    };
    for (const tab of tabs) {
        tab.addEventListener('click', () => activate(tab, false));
        tab.addEventListener('keydown', (event) => {
            const index = tabs.indexOf(tab);
            let next = null;
            if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
            if (event.key === 'ArrowLeft') next = (index - 1 + tabs.length) % tabs.length;
            if (event.key === 'Home') next = 0;
            if (event.key === 'End') next = tabs.length - 1;
            if (next !== null) {
                event.preventDefault();
                activate(tabs[next], true);
            }
        });
    }
}

async function loadOptions(force = false) {
    if (optionsLoaded && !force) return;
    const [users, customers, categories] = await Promise.all([
        api.get('/users'),
        api.get('/customers'),
        api.get('/categories'),
    ]);
    const projects = [];
    const tasks = [];
    // Fetch each customer's projects in parallel; the old sequential walk made
    // a large register feel like the dropdowns had failed to load.
    const projectLists = await Promise.all(
        (customers.customers || []).map(async (customer) => ({
            customer,
            list: await api.get(`/customers/${customer.id}/projects`),
        })),
    );
    const taskCalls = [];
    for (const { customer, list } of projectLists) {
        for (const project of list.projects || []) {
            projects.push({ ...project, customer_id: customer.id, customer_name: customer.name });
            taskCalls.push({ customer, project });
        }
    }
    const taskLists = await Promise.all(
        taskCalls.map(async ({ customer, project }) => ({
            customer,
            project,
            list: await api.get(`/customers/${customer.id}/projects/${project.code}/tasks`),
        })),
    );
    for (const { customer, project, list } of taskLists) {
        for (const task of list.tasks || []) {
            tasks.push({
                ...task, customer_id: customer.id, customer_name: customer.name,
                project_code: project.code, project_name: project.name,
            });
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

/// The customer a source client is mapped to, or null while unresolved /
/// pending creation (a project cannot be picked until the customer exists).
function importMappedCustomerId(sourceClient) {
    const choice = draft.customers.find((entry) => entry.source_client === sourceClient);
    return choice?.action === 'existing' ? choice.customer_id : null;
}

/// Projects offered for a source client: only those of the customer it is
/// already mapped to. With no existing customer there is nothing to list —
/// "Create project" and, for expenses, "Skip project" remain available.
function importProjectsFor(sourceClient) {
    const customerId = importMappedCustomerId(sourceClient);
    if (!customerId) return [];
    return options.projects.filter((project) => project.customer_id === customerId);
}

/// The project select's option list for one source client, preserving a
/// selected legacy value even when it is no longer in the filtered set (the
/// preview will report it; the select never silently points at a missing item).
function importProjectChoices(sourceClient, selected) {
    const mapped = Boolean(importMappedCustomerId(sourceClient));
    const choices = [
        ['', mapped ? 'Select project…' : 'Select a customer first…'],
        ['__skip', 'Skip project (expenses only)'],
        ['__create', 'Create project'],
    ];
    for (const project of importProjectsFor(sourceClient)) {
        choices.push([
            `${project.customer_id}|${project.code}`,
            `${project.code} — ${project.name}`,
        ]);
    }
    if (selected && !choices.some(([value]) => value === selected)) {
        choices.push([selected, 'Saved project (not listed for this customer)']);
    }
    return choices;
}

/// A project choice is only selectable under the source client's current
/// customer. Reset a stale customer-bound selection to unresolved so a customer
/// change cannot silently keep a project from the previous customer.
function normalizeImportProjectChoice(choice) {
    if (choice.action !== 'existing') return;
    const customerId = importMappedCustomerId(choice.source_client);
    if (customerId && choice.customer_id === customerId) return;
    choice.action = 'unresolved';
    choice.customer_id = null;
    choice.code = null;
}

function importProjectChoiceValue(choice) {
    return choice.action === 'existing' ? `${choice.customer_id}|${choice.code}`
        : choice.action === 'create' ? '__create' : choice.action === 'skip' ? '__skip' : '';
}

/// Rebuild one project select from the customer it is now mapped to, resetting
/// a selection that belongs to another customer.
function fillImportProjectSelect(select, choice) {
    normalizeImportProjectChoice(choice);
    const selected = importProjectChoiceValue(choice);
    select.textContent = '';
    for (const [value, text] of importProjectChoices(choice.source_client, selected)) {
        const option = el('option', { value, text });
        if (value === selected) option.selected = true;
        select.appendChild(option);
    }
    select.value = selected;
}

/// A customer mapping changed: every project under that source client now sees
/// only that customer's projects, and a project from the previous customer is
/// reset instead of being kept invisibly.
function syncImportProjectSelects(sourceClient) {
    for (const { select, choice } of importProjectSelects.values()) {
        if (choice.source_client !== sourceClient) continue;
        fillImportProjectSelect(select, choice);
    }
    serializeChoices();
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

/// Replace a choice's fields without replacing the object itself. Select
/// handlers and the project-sync map hold references to the choice, so keeping
/// its identity is what makes a second mapping change (or a customer change
/// narrowing the project list) update the same entry instead of writing to a
/// stale array index.
function replaceImportChoice(choice, next) {
    for (const key of Object.keys(choice)) delete choice[key];
    Object.assign(choice, next);
}

function pdfSuggestions(row) {
    const found = new Map();
    for (const suggestion of row?.suggestions?.suggestions || []) {
        if (!found.has(suggestion.field)) found.set(suggestion.field, suggestion);
    }
    return found;
}

function pdfText(found, field) {
    return found.get(field)?.value ?? '';
}

function pdfAmount(found, field) {
    return found.get(field)?.amount_minor ?? null;
}

// The import follows the document, not a guess: the printed total is the
// Subtotal (or Total/Amount due when the exporter prints no subtotal row), the
// Payments credit is the amount already settled, and payments equal to the
// total mean `paid` while anything else means `issued`.
function invoiceDefaults(row) {
    const found = pdfSuggestions(row);
    const total = pdfAmount(found, 'total') ?? pdfAmount(found, 'subtotal') ?? pdfAmount(found, 'amount_due');
    const payments = pdfAmount(found, 'payments');
    const status = pdfText(found, 'status')
        || (total == null ? null : (payments != null && payments === total ? 'paid' : 'issued'));
    const customerName = pdfText(found, 'customer');
    const customer = customerName
        ? options.customers.find((entry) => entry.name.trim().toLowerCase() === customerName.trim().toLowerCase())
        : null;
    return {
        customerId: customer?.id ?? null,
        customerName,
        originalNumber: pdfText(found, 'invoice_number'),
        currency: pdfText(found, 'currency'),
        invoiceDate: pdfText(found, 'invoice_date') || null,
        periodFrom: pdfText(found, 'period_from') || null,
        periodTo: pdfText(found, 'period_to') || null,
        total,
        payments,
        status,
        openingPaid: status === 'paid' && payments != null ? payments : 0,
    };
}

// Keep the reviewed settlement in step with the two amounts while the admin
// edits the card: equal means paid, a positive part means partly paid, zero
// means issued. An explicit status choice wins (`data-touched`).
function syncInvoiceSettlement(choice, sha) {
    const select = $(`import-inv-status-${sha}`);
    if (!select) return;
    const total = choice.total_minor;
    const paid = choice.opening_paid_minor ?? 0;
    if (total != null && select.dataset.touched !== '1') {
        select.value = paid === total ? 'paid' : paid > 0 ? 'partly_paid' : 'issued';
        choice.status = select.value;
    }
    serializeChoices();
}

function ensureInvoice(row) {
    const defaults = invoiceDefaults(row);
    let choice = draft.invoices.find((entry) => entry.sha256 === row.pdf_sha256);
    if (!choice) {
        choice = {
            sha256: row.pdf_sha256,
            customer_id: row.customer_id || defaults.customerId,
            original_number: row.original_number || defaults.originalNumber,
            currency: row.currency || defaults.currency,
            invoice_date: row.invoice_date || defaults.invoiceDate,
            period_from: row.period_from || defaults.periodFrom,
            period_to: row.period_to || defaults.periodTo,
            total_minor: row.total_minor ?? defaults.total,
            opening_paid_minor: row.opening_paid_minor || defaults.openingPaid,
            status: row.status || defaults.status,
            excluded: false,
        };
        draft.invoices.push(choice);
    } else {
        // A mapping saved before the PDF pass (or by an earlier build) can hold
        // blank fields: fill only the ones still empty, never overwrite a
        // reviewed value.
        if (!choice.original_number && defaults.originalNumber) choice.original_number = defaults.originalNumber;
        if (choice.customer_id == null && defaults.customerId) choice.customer_id = defaults.customerId;
        if (!choice.currency && defaults.currency) choice.currency = defaults.currency;
        if (!choice.invoice_date && defaults.invoiceDate) choice.invoice_date = defaults.invoiceDate;
        if (!choice.period_from && defaults.periodFrom) choice.period_from = defaults.periodFrom;
        if (!choice.period_to && defaults.periodTo) choice.period_to = defaults.periodTo;
        if (choice.total_minor == null && defaults.total != null) choice.total_minor = defaults.total;
        if ((choice.opening_paid_minor ?? 0) === 0 && defaults.openingPaid) choice.opening_paid_minor = defaults.openingPaid;
        if (!choice.status && defaults.status) choice.status = defaults.status;
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
    importProjectSelects = new Map();
    if (!preview) {
        host.appendChild(el('p', { cls: 'p-form-help-text', text: 'Upload a source and run Preview to build the mapping controls.' }));
        return;
    }
    if (importSource === 'invoices') {
        renderInvoices(host, preview);
        serializeChoices();
        return;
    }
    const timeRows = importSource === 'time' ? (preview.time || []) : [];
    const expenseRows = importSource === 'expenses' ? (preview.expenses || []) : [];
    const rows = [...timeRows, ...expenseRows];
    if (!rows.length) {
        host.appendChild(el('p', {
            cls: 'p-form-help-text',
            text: `No ${SOURCE_TABS[importSource].label.toLowerCase()} rows in this batch.`,
        }));
        serializeChoices();
        return;
    }

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
                    replaceImportChoice(choice, {
                        action: 'create', source_client: client, name: client,
                        currency: 'EUR', default_rate_minor: 0,
                    });
                } else if (value) {
                    replaceImportChoice(choice, {
                        action: 'existing', source_client: client, customer_id: value,
                    });
                } else {
                    replaceImportChoice(choice, { action: 'unresolved', source_client: client });
                }
                syncImportProjectSelects(client);
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
        const select = selectControl(`import-project-${key}`, '', [['', 'Select project…']],
            (value) => {
                if (value === '__create') {
                    replaceImportChoice(choice, {
                        action: 'create', source_client: row.source_client,
                        source_project: row.source_project, source_code: row.source_code,
                        customer_id: null, code: row.source_code || '', name: row.source_project,
                        currency: 'EUR', rate_minor: 0,
                    });
                } else if (value === '__skip') {
                    replaceImportChoice(choice, {
                        action: 'skip', source_client: row.source_client,
                        source_project: row.source_project, source_code: row.source_code,
                    });
                } else if (value) {
                    const [customerId, code] = value.split('|');
                    replaceImportChoice(choice, {
                        action: 'existing', source_client: row.source_client,
                        source_project: row.source_project, source_code: row.source_code,
                        customer_id: customerId, code,
                    });
                } else {
                    replaceImportChoice(choice, {
                        action: 'unresolved', source_client: row.source_client,
                        source_project: row.source_project, source_code: row.source_code,
                    });
                }
                serializeChoices();
            });
        importProjectSelects.set(key, { select, choice });
        fillImportProjectSelect(select, choice);
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
                        replaceImportChoice(choice, {
                            action: 'create', source_task: task, code: task, name: task,
                        });
                    } else if (value === '__skip') {
                        replaceImportChoice(choice, { action: 'skip', source_task: task });
                    } else if (value) {
                        replaceImportChoice(choice, { action: 'existing', source_task: task, code: value });
                    } else {
                        replaceImportChoice(choice, { action: 'unresolved', source_task: task });
                    }
                    serializeChoices();
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
                        replaceImportChoice(choice, {
                            action: 'create', source_category: category, name: category, billable: true,
                        });
                    } else if (value) {
                        replaceImportChoice(choice, {
                            action: 'existing', source_category: category, category_id: value,
                        });
                    } else {
                        replaceImportChoice(choice, { action: 'unresolved', source_category: category });
                    }
                    serializeChoices();
                });
            return el('tr', {}, [
                el('th', { attrs: { scope: 'row' }, text: category }),
                el('td', {}, [labelFor(select.id, `Map category ${category}`), select]),
            ]);
        })));
    }

    serializeChoices();
}

function renderInvoices(host, preview) {
    if (!(preview.invoices || []).length) {
        host.appendChild(el('p', { cls: 'p-form-help-text', text: 'No invoices staged in this batch.' }));
        return;
    }
    host.appendChild(el('h3', { text: 'Invoices' }));
    const card = el('div', { cls: 'import-invoice-list' });
    for (const row of preview.invoices) {
        const choice = ensureInvoice(row);
        const defaults = invoiceDefaults(row);
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
                el('div', {}, [labelFor(`import-inv-number-${row.pdf_sha256}`, 'Invoice ID'),
                textControl(`import-inv-number-${row.pdf_sha256}`, choice.original_number, (value) => { choice.original_number = value; })]),
                el('div', {}, [labelFor(`import-inv-customer-${row.pdf_sha256}`, 'Invoice for'),
                selectControl(`import-inv-customer-${row.pdf_sha256}`, choice.customer_id || '',
                    [['', 'Select customer…'], ...options.customers.map((customer) => [customer.id, customer.name])],
                    (value) => { choice.customer_id = value || null; }),
                ...(defaults.customerName
                    ? [el('p', { cls: 'p-form-help-text', text: `PDF: ${defaults.customerName}` })]
                    : [])]),
                el('div', {}, [labelFor(`import-inv-currency-${row.pdf_sha256}`, 'Currency'),
                textControl(`import-inv-currency-${row.pdf_sha256}`, choice.currency || '', (value) => { choice.currency = value; })]),
                el('div', {}, [labelFor(`import-inv-date-${row.pdf_sha256}`, 'Issue date'),
                textControl(`import-inv-date-${row.pdf_sha256}`, choice.invoice_date || '', (value) => { choice.invoice_date = value || null; }, 'date')]),
                el('div', {}, [labelFor(`import-inv-from-${row.pdf_sha256}`, 'Period from'),
                textControl(`import-inv-from-${row.pdf_sha256}`, choice.period_from || '', (value) => { choice.period_from = value || null; }, 'date')]),
                el('div', {}, [labelFor(`import-inv-to-${row.pdf_sha256}`, 'Period to'),
                textControl(`import-inv-to-${row.pdf_sha256}`, choice.period_to || '', (value) => { choice.period_to = value || null; }, 'date')]),
                el('div', {}, [labelFor(`import-inv-total-${row.pdf_sha256}`, 'Subtotal'),
                moneyInput(`import-inv-total-${row.pdf_sha256}`, choice.total_minor, (value) => {
                    choice.total_minor = value;
                    syncInvoiceSettlement(choice, row.pdf_sha256);
                })]),
                el('div', {}, [labelFor(`import-inv-paid-${row.pdf_sha256}`, 'Payments'),
                moneyInput(`import-inv-paid-${row.pdf_sha256}`, choice.opening_paid_minor, (value) => {
                    choice.opening_paid_minor = value ?? 0;
                    syncInvoiceSettlement(choice, row.pdf_sha256);
                })]),
                el('div', {}, [labelFor(`import-inv-status-${row.pdf_sha256}`, 'Status'),
                selectControl(`import-inv-status-${row.pdf_sha256}`, choice.status || '',
                    [['', 'Select status…'], ['issued', 'Issued'], ['partly_paid', 'Partly paid'],
                    ['paid', 'Paid'], ['written_off', 'Written off']],
                    (value) => {
                        choice.status = value || null;
                        $(`import-inv-status-${row.pdf_sha256}`).dataset.touched = '1';
                    })]),
                el('div', {}, [labelFor(`import-inv-exclude-${row.pdf_sha256}`, 'Exclude'),
                checkboxControl(`import-inv-exclude-${row.pdf_sha256}`, choice.excluded,
                    (checked) => { choice.excluded = checked; })]),
            ]),
            el('p', { cls: 'p-form-help-text' }, [pdf]),
        ]));
    }
    host.appendChild(card);
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
        await loadOptions(true);
        const batch = await api.post(BASE, { namespace: 'harvest' });
        renderBatch(batch);
        resetPreview();
        $('import-mapping').textContent = '';
        $('import-ready').textContent =
            `Draft created. Upload the ${SOURCE_TABS[importSource].label.toLowerCase()} source.`;
        await refreshImports();
    } catch (err) {
        showError(err);
    }
}

export async function openBatch(id) {
    clearError();
    resetPreview();
    $('import-commit').disabled = true;
    try {
        await loadOptions();
        renderBatch(await api.get(`${BASE}/${id}`));
        const staged = ['time', 'expenses', 'invoices'].filter((kind) => currentBatch.files?.[kind]);
        if (staged.length && !staged.includes(importSource)) setImportSource(staged[0]);
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
        serializeChoices();
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
                serializeChoices();
                announce(sha ? 'Row linked; recompute the preview.' : 'Link removed; recompute the preview.');
            },
        },
    });
    return el('div', { cls: 'import-row-link' }, [select, button]);
}

function rowMatches(row, filter) {
    const issues = row.issues || [];
    if (filter === 'issues') return issues.length > 0;
    if (filter === 'blocking') return issues.some((issue) => issue.blocking);
    if (filter === 'accepted') return row.decision === 'new' || row.decision === 'correction';
    if (filter === 'skipped') return ['duplicate', 'excluded', 'conflict'].includes(row.decision);
    return true;
}

function issueMatches(issue, filter) {
    return filter === 'blocking' ? issue.blocking === true : true;
}

function clampPage(page, total) {
    const last = Math.max(0, Math.ceil(total / PAGE_SIZE) - 1);
    return Math.min(page, last);
}

function pageLabel(elementId, page, total, noun) {
    const target = $(elementId);
    if (!target) return;
    if (!total) {
        target.textContent = `No ${noun} match this filter.`;
        return;
    }
    const start = page * PAGE_SIZE + 1;
    const end = Math.min(start + PAGE_SIZE - 1, total);
    target.textContent = `${noun[0].toUpperCase()}${noun.slice(1)} ${start}–${end} of ${total}.`;
}

function previewRows() {
    if (importSource === 'time') {
        return (currentPreview?.time || []).map((row) => ({ source: 'time', row }));
    }
    if (importSource === 'expenses') {
        return (currentPreview?.expenses || []).map((row) => ({ source: 'expense', row }));
    }
    return (currentPreview?.invoices || []).map((row) => ({ source: 'invoice', row }));
}

function renderRows() {
    const filtered = previewRows().filter(({ row }) => rowMatches(row, rowFilter));
    rowPage = clampPage(rowPage, filtered.length);
    const tbody = $('import-rows').querySelector('tbody');
    tbody.textContent = '';
    const start = rowPage * PAGE_SIZE;
    for (const { source, row } of filtered.slice(start, start + PAGE_SIZE)) {
        const amount = row.amount_minor ?? row.total_minor;
        const issues = (row.issues || []).length;
        const link = source === 'invoice' ? el('a', {
            attrs: {
                href: `${BASE}/${currentBatch.id}/invoices/${row.pdf_sha256}/pdf`,
                target: '_blank', rel: 'noopener',
            },
            text: 'PDF',
        }) : linkRow(currentPreview, row.source_key, row.invoiced);
        tbody.appendChild(el('tr', {}, [
            el('th', { attrs: { scope: 'row' }, text: source }),
            el('td', { attrs: { 'data-heading': 'Record' }, text: `${row.record ?? row.original_name ?? row.source_key}` }),
            el('td', { attrs: { 'data-heading': 'Decision' } }, [statusChip(row.decision)]),
            el('td', { attrs: { 'data-heading': 'Amount' }, text: amount === undefined ? '' : formatMoney(amount) }),
            el('td', { attrs: { 'data-heading': 'Issues' }, text: String(issues) }),
            el('td', { attrs: { 'data-heading': 'Link' } }, [link]),
        ]));
    }
    pageLabel('import-row-page', rowPage, filtered.length, 'rows');
    $('import-row-prev').disabled = rowPage === 0;
    $('import-row-next').disabled = start + PAGE_SIZE >= filtered.length;
}

function renderIssues() {
    const filtered = (currentPreview?.issues || [])
        .filter((issue) => issue.source === sourceKey() && issueMatches(issue, issueFilter));
    issuePage = clampPage(issuePage, filtered.length);
    const tbody = $('import-issue-list').querySelector('tbody');
    tbody.textContent = '';
    const start = issuePage * PAGE_SIZE;
    for (const issue of filtered.slice(start, start + PAGE_SIZE)) {
        const where = issue.record === undefined
            ? (issue.member || '—')
            : `${issue.record}${issue.line === undefined ? '' : ` · line ${issue.line}`}`;
        tbody.appendChild(el('tr', {}, [
            el('th', { attrs: { scope: 'row' }, text: issue.source }),
            el('td', { attrs: { 'data-heading': 'Record' }, text: where }),
            el('td', { attrs: { 'data-heading': 'Field' }, text: issue.field }),
            el('td', { attrs: { 'data-heading': 'Message' }, text: issue.message }),
            el('td', { attrs: { 'data-heading': 'Blocking' }, text: issue.blocking ? 'yes' : 'no' }),
        ]));
    }
    pageLabel('import-issue-page', issuePage, filtered.length, 'issues');
    $('import-issue-prev').disabled = issuePage === 0;
    $('import-issue-next').disabled = start + PAGE_SIZE >= filtered.length;
}

function renderPreview(preview) {
    currentPreview = preview;
    rowPage = 0;
    issuePage = 0;
    $('import-row-filter').value = rowFilter;
    $('import-issue-filter').value = issueFilter;
    const tbody = $('import-totals').querySelector('tbody');
    tbody.textContent = '';
    for (const row of (preview.totals || []).filter((entry) => entry.source === sourceKey())) {
        tbody.appendChild(el('tr', {}, [
            el('th', { attrs: { scope: 'row' }, text: row.source }),
            el('td', { attrs: { 'data-heading': 'Currency' }, text: row.currency }),
            el('td', { attrs: { 'data-heading': 'Accepted' }, text: `${row.accepted_count} · ${formatMoney(row.accepted_minor)}` }),
            el('td', { attrs: { 'data-heading': 'Skipped' }, text: `${row.skipped_count} · ${formatMoney(row.skipped_minor)}` }),
            el('td', { attrs: { 'data-heading': 'Cost' }, text: formatMoney(row.cost_minor) }),
            el('td', { attrs: { 'data-heading': 'Payments' }, text: formatMoney(row.opening_paid_minor) }),
        ]));
    }
    renderRows();
    renderIssues();
    $('import-issues').textContent = preview.blocking
        ? `${preview.blocking} blocking issue(s) across staged sources; commit stays disabled.`
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
        // Refresh the mapping lists on every preview: customers/projects added
        // while this batch was open must be selectable without reopening it.
        await loadOptions(true);
        serializeChoices();
        renderBatch(await api.put(`${BASE}/${id}/review`, draft));
        const sent = JSON.stringify(draft);
        renderPreview(await api.post(`${BASE}/${id}/preview`));
        // Rendering the mapping fills defaults from the PDF suggestions (and
        // from the preview's own source rows). Persist them and recompute once
        // so a single Preview click also reflects the prefilled invoice cards.
        if (JSON.stringify(draft) !== sent) {
            renderBatch(await api.put(`${BASE}/${id}/review`, draft));
            renderPreview(await api.post(`${BASE}/${id}/preview`));
        }
        announce('Preview computed.');
    } catch (err) {
        showError(err);
    }
}

function resetPreview() {
    currentPreview = null;
    $('import-totals').querySelector('tbody').textContent = '';
    $('import-rows').querySelector('tbody').textContent = '';
    $('import-issue-list').querySelector('tbody').textContent = '';
    $('import-row-page').textContent = '';
    $('import-issue-page').textContent = '';
    $('import-issues').textContent = '';
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

async function discardBatch() {
    const id = batchId();
    if (!id) return;
    if (currentBatch?.state === 'committed') {
        announce('A committed import is financial history and cannot be discarded.');
        return;
    }
    if (!(await askConfirm(
        'Discard this batch and every staged source? Records already committed by an import are never removed.',
        'Discard batch',
    ))) return;
    clearError();
    try {
        await api.del(`${BASE}/${id}`);
        currentBatch = null;
        draft = null;
        resetPreview();
        $('import-batch-label').textContent = '';
        $('import-mapping').textContent = '';
        $('import-choices').value = '';
        $('import-ready').textContent = 'Batch discarded. Upload sources into a new batch.';
        $('import-commit').disabled = true;
        $('import-status').textContent = 'Batch discarded.';
        announce('Import batch discarded.');
        await refreshImports();
    } catch (err) {
        showError(err);
        await refreshImports();
    }
}

let wired = false;

export function wireImports() {
    if (wired || !$('import-new')) return;
    wired = true;
    wireImportTabs();
    // Every mapping control edits `draft` in place; mirror it into the box so
    // the visible JSON never lags behind a typed amount or a derived status.
    for (const type of ['input', 'change']) {
        $('import-mapping').addEventListener(type, () => {
            if (draft) serializeChoices();
        });
    }
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
    $('import-discard').addEventListener('click', discardBatch);
    $('import-row-filter').addEventListener('change', (event) => {
        rowFilter = event.target.value;
        rowPage = 0;
        renderRows();
    });
    $('import-issue-filter').addEventListener('change', (event) => {
        issueFilter = event.target.value;
        issuePage = 0;
        renderIssues();
    });
    $('import-row-prev').addEventListener('click', () => {
        rowPage = Math.max(0, rowPage - 1);
        renderRows();
    });
    $('import-row-next').addEventListener('click', () => {
        rowPage += 1;
        renderRows();
    });
    $('import-issue-prev').addEventListener('click', () => {
        issuePage = Math.max(0, issuePage - 1);
        renderIssues();
    });
    $('import-issue-next').addEventListener('click', () => {
        issuePage += 1;
        renderIssues();
    });
}
