// state.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { fillSelect } from './dom.js';

// ----------------------------------------------------------------- cache ---

export const state = {
  isAdmin: false, // #188: set from /api/auth/me; gates admin-tier panel loads at boot
  vault: null,             // last GET /admin/vault payload (settings.js)
  customers: [],           // Customer[]
  projectsByCustomer: {},  // id -> Project[]
  // #182: the Setup hierarchy is three sections with filter state instead of
  // the old shared workspace selection.
  projectsFilter: null,    // customer id | null = all customers
  expensesCustomer: null,  // expense filters (same "all" default as Tasks)
  expensesProject: null,
  tasksCustomer: null,     // customer id | null
  tasksProject: null,      // project code | null
};

export async function loadCustomers() {
  const data = await api.get('/customers');
  state.customers = data.customers || [];
}

export async function loadProjects(customerId) {
  const data = await api.get(`/customers/${customerId}/projects`);
  state.projectsByCustomer[customerId] = data.projects || [];
  return state.projectsByCustomer[customerId];
}

export function customerName(id) {
  const c = state.customers.find((x) => x.id === id);
  return c ? c.name : '(deleted customer)';
}

// ------------------------------------------------------------- customer ---

// Inactive reference data is retired from the pickers for NEW records: the
// lists stay short while the records themselves remain in place for history
// (entries, expenses and invoices still reference them). `includeInactive`
// opts a filter (where "all" must stay reachable) back into the full list;
// otherwise the value a form is already editing is always kept selectable, so
// an old entry can be opened and saved even when its customer/project/task was
// retired afterwards.
export function fillCustomerSelect(select, selectedId, includePlaceholder, includeInactive = false) {
  const opts = state.customers
    .filter((c) => includeInactive || c.active !== false || c.id === selectedId)
    .map((c) => ({
      value: c.id,
      text: c.active !== false ? c.name : `${c.name} (inactive)`,
      selected: c.id === selectedId,
    }));
  if (includePlaceholder) opts.unshift({ value: '', text: 'Choose a customer…' });
  fillSelect(select, opts);
}

export async function fillProjectSelect(select, customerId, selectedCode, includeInactive = false) {
  const sequence = (select.__projectSequence || 0) + 1;
  select.__projectSequence = sequence;
  select.textContent = '';
  if (!customerId) {
    fillSelect(select, [{ value: '', text: 'Choose a customer first…' }]);
    return;
  }
  const projects = await loadProjects(customerId);
  if (select.__projectSequence !== sequence) return;
  fillSelect(select, [
    { value: '', text: 'Choose a project…' },
    ...projects
      .filter((p) => includeInactive || p.active !== false || p.code === selectedCode)
      .map((p) => ({
        value: p.code,
        text: p.active !== false ? `${p.code} — ${p.name}` : `${p.code} — ${p.name} (inactive)`,
        selected: p.code === selectedCode,
      })),
  ]);
}

// Loads a project's tasks into the given select, with a leading "None".
export async function fillTaskSelect(select, customerId, projectCode, selectedCode, includeInactive = false) {
  // #188: per-select sequence guard (same pattern as fillProjectSelect) — an
  // older task list must never overwrite a newer one when the project picker
  // changes quickly.
  const sequence = (select.__taskSequence || 0) + 1;
  select.__taskSequence = sequence;
  const opts = [{ value: '', text: projectCode ? 'None' : 'Choose a project first…' }];
  if (customerId && projectCode) {
    const data = await api.get(
      `/customers/${customerId}/projects/${encodeURIComponent(projectCode)}/tasks`,
    );
    if (select.__taskSequence !== sequence) return;
    for (const t of data.tasks || []) {
      if (!(includeInactive || t.active !== false || t.code === selectedCode)) continue;
      opts.push({
        value: t.code,
        text: t.active !== false ? `${t.code} — ${t.name}` : `${t.code} — ${t.name} (inactive)`,
        selected: t.code === selectedCode,
      });
    }
  }
  fillSelect(select, opts);
}

