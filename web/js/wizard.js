// wizard.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { $, announce, clearFormError, fillSelect, showDialog, showFormError } from './dom.js';
import { refreshCustomerPickers, refreshCustomerTable } from './hierarchy.js';
import { fillProjectSelect, loadCustomers, state } from './state.js';

// ------------------------------------------------------------ wizard (#111) --
//
// First-run setup: welcome -> customer -> project -> done. Uses the existing
// POST /customers and POST /customers/{id}/projects routes (no backend
// surface), so every step that completes is already persisted — "set up
// later" never loses a finished customer or project. Reopening with at least
// one customer jumps straight to the project step. All data through
// textContent; step changes announce via the dialog's aria-live status.

export const WIZ = { step: 0, customer: null, project: null };
export const WIZ_TITLES = ['Welcome', 'Add your first customer', 'Add a project', 'All set'];

// jsdom hosts still lack the <dialog> modal API — same fallback as #102's
// showDialog, so the harness drives the identical button flow.
export function wizardShow() {
  const dlg = $('wizard-dialog');
  if (typeof dlg.showModal === 'function') dlg.showModal();
  else dlg.setAttribute('open', '');
}

export function wizardHide() {
  const dlg = $('wizard-dialog');
  if (typeof dlg.close === 'function') dlg.close();
  else dlg.removeAttribute('open');
}

export function openWizard(opts = {}) {
  WIZ.customer = null;
  WIZ.project = null;
  // Fresh first login starts at the welcome; a manual reopen with existing
  // customers starts at the project step (selection is not persisted).
  WIZ.step = opts.fresh || state.customers.length === 0 ? 0 : 2;
  $('wz-project-code').value = '';
  $('wz-project-name').value = '';
  wizardShow();
  wizRender();
}

export function wizRender() {
  for (let n = 0; n < 4; n++) $(`wz-step-${n}`).hidden = n !== WIZ.step;
  $('wz-title').textContent = WIZ_TITLES[WIZ.step];
  $('wz-status').textContent = WIZ.step === 3
    ? 'Setup complete.'
    : `Step ${WIZ.step + 1} of 4: ${WIZ_TITLES[WIZ.step]}`;
  $('wz-back').hidden = WIZ.step === 0 || WIZ.step === 3;
  $('wz-next').textContent =
    WIZ.step === 1 ? 'Create customer'
      : WIZ.step === 2 ? 'Create project'
        : WIZ.step === 3 ? 'Start recording time' : 'Next';
  clearFormError($('wz-error'));
  if (WIZ.step === 2) wizPrepareProject();
  const focusTarget = { 0: 'wz-next', 1: 'wz-cust-name', 2: 'wz-project-code', 3: 'wz-next' }[WIZ.step];
  const f = $(focusTarget);
  if (f) f.focus();
}

export function wizTargetCustomer() {
  if (WIZ.customer) return WIZ.customer;
  const cid = $('wz-project-customer').value;
  return state.customers.find((c) => c.id === cid) || null;
}

export function wizPrepareProject() {
  const needsPick = !WIZ.customer;
  $('wz-pick-customer-field').hidden = !needsPick;
  if (needsPick) {
    fillSelect(
      $('wz-project-customer'),
      state.customers.map((c) => ({ value: c.id, text: c.name })),
    ); // #189: third fillSelect argument was silently ignored
  }
  wizPrefillFromCustomer(); // #11 prefill rule: currency + rate from the customer
}

export function wizPrefillFromCustomer() {
  const c = wizTargetCustomer();
  if (!c) return;
  $('wz-project-currency').value = c.currency;
  $('wz-project-rate').value = (c.default_rate_minor / 100).toFixed(2);
}

export async function wizNext() {
  if (WIZ.step === 0) {
    WIZ.step = 1;
    return wizRender();
  }
  if (WIZ.step === 1) return wizCreateCustomer();
  if (WIZ.step === 2) return wizCreateProject();
  wizardHide();
  return wizFinish();
}

export async function wizCreateCustomer() {
  const name = $('wz-cust-name').value.trim();
  if (!name) {
    $('wz-error').textContent = 'name: required, at most 120 characters';
    $('wz-error').hidden = false;
    return;
  }
  try {
    WIZ.customer = await api.post('/customers', {
      name,
      currency: $('wz-cust-currency').value.trim().toUpperCase(),
      default_rate_minor: Math.round(Number($('wz-cust-rate').value) * 100),
      active: true,
    });
    await loadCustomers();
    await refreshCustomerTable();
    await refreshCustomerPickers();
    WIZ.step = 2;
    wizRender();
  } catch (err) {
    showFormError($('wz-error'), err);
  }
}

export async function wizCreateProject() {
  const c = wizTargetCustomer();
  const code = $('wz-project-code').value.trim();
  if (!c || !code) {
    $('wz-error').textContent = !c ? 'pick a customer first' : 'code: required';
    $('wz-error').hidden = false;
    return;
  }
  try {
    WIZ.project = await api.post(`/customers/${c.id}/projects`, {
      code,
      name: $('wz-project-name').value.trim(),
      currency: $('wz-project-currency').value.trim().toUpperCase(),
      rate_minor: Math.round(Number($('wz-project-rate').value) * 100),
    });
    $('wz-done-text').textContent =
      `Customer "${c.name}" and project "${WIZ.project.code}" are ready.`;
    WIZ.step = 3;
    wizRender();
  } catch (err) {
    showFormError($('wz-error'), err);
  }
}

export async function wizFinish() {
  // Land on the Day view with the new pair preselected: the entry form opens
  // ready for the first time entry (#111 DoD).
  $('tab-timesheet').click();
  $('day-add').click(); // resets + shows the inline entry form
  if (WIZ.customer && WIZ.project) {
    $('entry-customer').value = WIZ.customer.id;
    await fillProjectSelect($('entry-project'), WIZ.customer.id, WIZ.project.code);
    $('entry-customer').value = WIZ.customer.id; // fillProjectSelect does not touch it; defensive
    $('entry-hours').focus();
    announce('Setup complete — record your first entry.');
  }
}

export function initWizard() {
  $('wizard-open').addEventListener('click', () => openWizard());
  $('wz-next').addEventListener('click', () => wizNext());
  $('wz-back').addEventListener('click', () => {
    WIZ.step = Math.max(0, WIZ.step - 1);
    wizRender();
  });
  $('wz-later').addEventListener('click', wizardHide);
  $('wz-project-customer').addEventListener('change', wizPrefillFromCustomer);
  // Keyboard flow: Enter advances exactly like clicking Next.
  $('wizard-dialog').addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && e.target.tagName !== 'BUTTON') {
      e.preventDefault();
      wizNext();
    }
  });
}

