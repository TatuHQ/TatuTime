// boot.js — TatuTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { ADVANCED_BILLING_ENABLED, addRecurring, createRetainer, iw, iwClose, iwInit, iwLoadProjects, iwOpen, iwShow, refreshCatalog, refreshRetainers, refreshSchedules, saveCatalogItem } from './billing.js';
import { calState, refreshTimeCal, shiftMonth } from './calendar.js';
import { $, addDays, announce, clearFormError, decorateUbuntuButton, isoDate, mondayOf } from './dom.js';
import { ADD_CATEGORY_VALUE, addCategory, addExpenseCategory, closeCategoryDialog, fillCategorySelect, openCategoryDialog, refreshCategories, refreshExpenses, refreshSubmissions, resetExpenseForm, saveExpense, submitWeek } from './expenses.js';
import { refreshExpenseConversion, resetExpenseConversion, showConverted } from './fx.js';
import { addPaymentRail, cancelCustomerEdit, cancelProjectEdit, cancelTaskEdit, contactRow, fillParentCustomers, fillTaskParents, prefillProjectParent, refreshCustomerPickers, refreshCustomerTable, refreshCustomersView, refreshProjectTable, refreshProjectsView, refreshTaskTable, refreshTasksView, runReport, saveCustomer, saveProject, saveTask } from './hierarchy.js';
import { closeDeliveryRecord, closeInvoicePreview, closeSendEmailDialog, confirmSendEmail, dash, downloadInvoicePdf, ed, edCancel, edClose, edHasChanges, edLineRow, edLoadDefaults, edOpen, edRecalc, edSave, initInvoiceDropdowns, invoicePreview, markInvoiceSent, openEmailSettings, refreshInvoiceSummary, refreshInvoices, sendInvoiceEmail, setInvoicePeriod } from './invoices.js';
import { markNotificationsRead, refreshNotifications } from './notifications.js';
import { initReceiptPreview } from './receipt.js';
import { initSelectMenus } from './select.js';
import { applyRoute, initRouter, initSegTabs, initSettingsTabs, initTabs, normaliseRoute, refreshTimesheetView, segActiveId, showTimesheet } from './route.js';
import { addSecret, closeUserForm, closeUserPassword, disableVault, enableVault, initTemplatePreview, newUserForm, paintAccent, refreshAuditLog, refreshSettings, regenerateVault, renderUsers, resetBuiltInLabels, runBackupNow, saveBackupSettings, saveConfig, saveEmailSettings, saveInvoiceTemplate, saveOrgProfile, saveUser, saveUserPassword, sendTestEmail } from './settings.js';
import { wireImports } from './imports.js';
import { customerName, fillProjectSelect, fillTaskSelect, loadCustomers, state } from './state.js';
import { discardTimer, refreshTimer, startTimer, stopTimer } from './timer.js';
import { commitCell, copyLastWeek, copyPreviousDay, entrySaving, navigateDay, navigateWeek, openWeekRowForm, refreshDay, refreshWeek, resetEntryForm, resetWeekRowForm, saveEntry, saveWeekRow, selectDay, showEntryForm, showWeekRowForm, weekState } from './timesheet.js';
import { initWizard, openWizard } from './wizard.js';

// ---------------------------------------------------------------- boot ---

export let suppressPanelRefresh = false; // C7: avoid double-fetch when jumping panels

export function switchTab(tabId) {
  const tab = $(tabId);
  if (!tab) return;
  suppressPanelRefresh = true;
  tab.click();
  suppressPanelRefresh = false;
}

// Reloads a panel's data when its tab is shown (keeps views fresh after
// changes made from another panel or the API).
export function refreshPanel(tabId) {
  if (suppressPanelRefresh) return; // caller will refresh explicitly
  // C7: customers & settings were missing — re-shown panels re-pull now.
  const fn = {
    'tab-timesheet': refreshTimesheetView,
    'tab-invoices': async () => {
      await refreshInvoices();
      // Advanced billing tools are hidden while unfinished (see billing.js):
      // their DOM stays in place, but a hidden panel is not refreshed.
      if (ADVANCED_BILLING_ENABLED) {
        await refreshSchedules();
        await refreshRetainers();
      }
    },
    'tab-expenses': refreshExpenses,
    'tab-submissions': refreshSubmissions,
    // QA-10: the Setup registers refresh through their view wrappers, so a
    // failed navigation load leaves the last rows and shows the visible banner
    // with Retry (the wrapper owns load + render + the stale-panel contract).
    'tab-customers': refreshCustomersView,
    // #182: the hierarchy sections pull their own scoped data on show.
    'tab-projects': refreshProjectsView,
    'tab-tasks': refreshTasksView,
    'tab-settings': refreshSettings,
  }[tabId];
  // #188: panel refreshes were dispatched un-awaited and un-caught; any
  // rejection bypassed the announce() convention and died as a console-only
  // unhandled rejection. Surface failures through the live region instead.
  if (fn) Promise.resolve(fn()).catch((err) => announce(`Refresh failed: ${err.message}`));
}

// Wires the app listeners and loads the first data. Called once authenticated.
// Day actions + logged-time calendar nav (#140). Extracted from
// startApp (#189).
export function wireDayAndCalendar() {
  $('day-add').addEventListener('click', async () => {
    // The heading-row button serves the whole Timesheets section: with the
    // Week segment active it aims the entry at today within the displayed
    // week and flips to the Day view first (the old #week-track behaviour).
    if (segActiveId === 'ts-week') {
      const today = isoDate(new Date());
      const days = weekState.days.length === 7 ? weekState.days : [today];
      $('day-date').value = days.includes(today) ? today : days[0];
      showTimesheet('ts-day');
      await refreshDay();
    }
    resetEntryForm();
    showEntryForm(true);
    $('entry-customer').focus();
  });
  $('day-prev').addEventListener('click', () => navigateDay(-1));
  $('day-next').addEventListener('click', () => navigateDay(1));
  $('day-today').addEventListener('click', () => selectDay(isoDate(new Date())));
  $('day-add-bottom').addEventListener('click', () => $('day-add').click());
  $('copy-previous').addEventListener('click', () => copyPreviousDay());
  // #140: calendar navigation.
  $('cal-prev').addEventListener('click', () => {
    calState.month = shiftMonth(calState.month || $('day-date').value.slice(0, 7), -1);
    refreshTimeCal().catch?.(() => { });
  });
  $('cal-next').addEventListener('click', () => {
    calState.month = shiftMonth(calState.month || $('day-date').value.slice(0, 7), 1);
    refreshTimeCal().catch?.(() => { });
  });
  $('cal-month').addEventListener('click', () => {
    calState.month = isoDate(new Date()).slice(0, 7);
    refreshTimeCal().catch?.(() => { });
  });
}

// Invoice dashboard controls (#135/#189).
export function wireInvoiceDashboard() {
  initInvoiceDropdowns();
  $('inv-f-period').addEventListener('change', () => {
    setInvoicePeriod($('inv-f-period').value);
    refreshInvoices().catch?.(() => { });
  });
  for (const [id, shift] of [['inv-period-prev', -1], ['inv-period-next', 1]]) {
    $(id).addEventListener('click', () => {
      setInvoicePeriod(dash.periodMode, shift);
      refreshInvoices().catch?.(() => { });
    });
  }
  // #135 dashboard controls
  $('inv-tab-open').addEventListener('click', () => {
    dash.openOnly = true;
    $('inv-tab-open').setAttribute('aria-pressed', 'true');
    $('inv-tab-all').setAttribute('aria-pressed', 'false');
    refreshInvoices().catch?.(() => { });
  });
  $('inv-tab-all').addEventListener('click', () => {
    dash.openOnly = false;
    $('inv-tab-all').setAttribute('aria-pressed', 'true');
    $('inv-tab-open').setAttribute('aria-pressed', 'false');
    refreshInvoices().catch?.(() => { });
  });
  $('invoice-search').addEventListener('input', () => {
    dash.q = $('invoice-search').value;
    refreshInvoices().catch?.(() => { });
  });
  $('inv-f-customer').addEventListener('change', () => {
    dash.customer = $('inv-f-customer').value;
    // #189: refreshInvoices chains refreshInvoiceSummary with the shared
    // payload — the old double call fired two renders and three GETs.
    refreshInvoices().catch?.(() => { });
  });
  $('inv-f-from').addEventListener('change', () => {
    dash.from = $('inv-f-from').value;
    refreshInvoices().catch?.(() => { });
  });
  $('inv-f-to').addEventListener('change', () => {
    dash.to = $('inv-f-to').value;
    refreshInvoices().catch?.(() => { });
  });
  for (const btn of document.querySelectorAll('.p-table__sort-button')) {
    btn.addEventListener('click', () => {
      const col = btn.getAttribute('data-col');
      if (dash.sortKey === col) dash.sortDir = -dash.sortDir;
      else { dash.sortKey = col; dash.sortDir = 1; }
      // #232 phase 2d: Vanilla's real p-table__sort-button pattern reflects
      // the sort with aria-sort on the <th> — the active column carries
      // ascending/descending and every other sortable column is marked
      // `none` (the ARIA "sortable but not currently sorted" state).
      for (const b2 of document.querySelectorAll('.p-table__sort-button')) {
        b2.closest('th').setAttribute('aria-sort', 'none');
      }
      btn.closest('th').setAttribute('aria-sort', dash.sortDir === 1 ? 'ascending' : 'descending');
      refreshInvoices().catch?.(() => { });
    });
  }
  $('inv-year-prev').addEventListener('click', () => {
    dash.year -= 1;
    refreshInvoiceSummary().catch?.(() => { });
  });
  $('inv-year-next').addEventListener('click', () => {
    dash.year += 1;
    refreshInvoiceSummary().catch?.(() => { });
  });
}

// Entry dialog form wiring (#141/#145/#189).
export function wireEntryForm() {
  $('entry-form').addEventListener('submit', saveEntry);
  $('entry-cancel').addEventListener('click', () => {
    resetEntryForm();
    showEntryForm(false);
    announce('Entry cancelled — nothing was saved.');
  });
  $('entry-dialog').addEventListener('cancel', (e) => {
    // native Escape on real browsers: never close mid-save, cancel otherwise
    if (entrySaving) e.preventDefault();
    else {
      e.preventDefault();
      resetEntryForm();
      showEntryForm(false);
      announce('Entry cancelled — nothing was saved.');
    }
  });
  $('entry-dialog').addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && !entrySaving) {
      // jsdom never fires 'cancel'; emulate it here (browsers do both).
      resetEntryForm();
      showEntryForm(false);
      announce('Entry cancelled — nothing was saved.');
    }
  });
  $('entry-customer').addEventListener('change', async (e) => {
    await fillProjectSelect($('entry-project'), e.target.value, null);
    fillTaskSelect($('entry-task'), '', null, null);
    $('entry-project').focus();
  });
  $('entry-project').addEventListener('change', async (e) => {
    await fillTaskSelect($('entry-task'), $('entry-customer').value, e.target.value, null);
  });

}

// Week grid: inline cell editing via delegated events (#13/#189).
export function wireWeekGrid() {
  // Week grid (#13): inline cell editing via delegated events.
  const wt = $('week-table');
  wt.addEventListener('focusout', (e) => {
    if (e.target.classList && e.target.classList.contains('cell-input')) commitCell(e.target);
  });
  wt.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && e.target.classList && e.target.classList.contains('cell-input')) {
      e.preventDefault();
      commitCell(e.target);
    }
  });
  $('week-date').addEventListener('change', () => refreshWeek());
  $('week-prev').addEventListener('click', () => navigateWeek(-1));
  $('week-next').addEventListener('click', () => navigateWeek(1));
  $('week-today').addEventListener('click', () => {
    $('week-date').value = isoDate(new Date());
    refreshWeek();
  });
  $('week-copy-previous').addEventListener('click', () => copyLastWeek());
  // Add-row dialog (#172 follow-up): pick customer/project/task, then fill the
  // row's day cells. Wired here so the week grid owns every week control.
  $('week-add-row').addEventListener('click', () => openWeekRowForm());
  $('week-row-form').addEventListener('submit', saveWeekRow);
  $('week-row-cancel').addEventListener('click', () => {
    resetWeekRowForm();
    showWeekRowForm(false);
    announce('Row not added.');
  });
  $('week-row-dialog').addEventListener('cancel', (e) => {
    // native Escape: never leave a half-typed row behind.
    e.preventDefault();
    resetWeekRowForm();
    showWeekRowForm(false);
    announce('Row not added.');
  });
  $('week-row-dialog').addEventListener('keydown', (e) => {
    if (e.key === 'Escape') {
      // jsdom never fires 'cancel'; emulate it here (browsers do both).
      resetWeekRowForm();
      showWeekRowForm(false);
      announce('Row not added.');
    }
  });
  $('week-row-customer').addEventListener('change', async (e) => {
    await fillProjectSelect($('week-row-project'), e.target.value, null);
    fillTaskSelect($('week-row-task'), '', null, null);
    $('week-row-project').focus();
  });
  $('week-row-project').addEventListener('change', async (e) => {
    await fillTaskSelect($('week-row-task'), $('week-row-customer').value, e.target.value, null);
  });
  $('ip-close').addEventListener('click', closeInvoicePreview);
  $('ip-download').addEventListener('click', () => {
    if (invoicePreview) downloadInvoicePdf(invoicePreview.inv.id, invoicePreview.inv.number);
  });
  // #349: the delivery actions act on the invoice the detail panel is showing.
  // Send email opens the recipient dialog first (#139 follow-up), so the admin
  // chooses who receives it; the dialog's Send re-renders the panel, so the
  // SENT watermark and the new history entry appear without a reload, and it
  // reports the transport honestly (a disabled transport is announced as NOT
  // sent).
  $('ip-send').addEventListener('click', () => {
    if (invoicePreview) sendInvoiceEmail(invoicePreview.inv, $('ip-send'));
  });
  // The recipient dialog's own controls: Send posts the ticked list (or omits
  // the body for the untouched default), Cancel/Escape dismiss it, and the
  // SMTP notice's button moves to the Settings route.
  $('send-email-send').addEventListener('click', confirmSendEmail);
  $('send-email-close').addEventListener('click', closeSendEmailDialog);
  $('send-email-settings').addEventListener('click', openEmailSettings);
  // Correcting a typed address clears its inline error, like every other form.
  $('send-email-extra').addEventListener('input', () => clearFormError($('send-email-error')));
  $('send-email-dialog').addEventListener('cancel', (event) => {
    // native Escape on a real browser: dismiss it, never leave it half-open.
    event.preventDefault();
    closeSendEmailDialog();
  });
  $('send-email-dialog').addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      // jsdom never fires 'cancel'; emulate it here (browsers do both).
      event.preventDefault();
      closeSendEmailDialog();
    }
  });
  // Mark as sent is idempotent server-side and 409s on a draft; both answers are
  // reported plainly, never as a silent success and never as a duplicate entry.
  $('ip-mark-sent').addEventListener('click', () => {
    if (invoicePreview) markInvoiceSent(invoicePreview.inv);
  });
  // The `View message` popup: the delivery record of an `email_sent` entry.
  $('message-close').addEventListener('click', closeDeliveryRecord);
  $('message-dialog').addEventListener('cancel', (event) => {
    // native Escape on a real browser: dismiss it, never leave it half-open.
    event.preventDefault();
    closeDeliveryRecord();
  });
  $('message-dialog').addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      // jsdom never fires 'cancel'; emulate it here (browsers do both).
      event.preventDefault();
      closeDeliveryRecord();
    }
  });
  $('invoice-preview').addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closeInvoicePreview();
  });
  $('org-form').addEventListener('submit', saveOrgProfile);
  $('org-accent').addEventListener('input', paintAccent);
  // #349: the two creation actions lead the section — `+ Invoice` reveals the
  // tracked-work wizard (#134), `+ Manual Invoice` opens the manual editor
  // (#143) as a p-modal popup.
  // #349/#new: `+ Invoice` starts the creation wizard (customer → period →
  // grouping, with a live PDF preview); the manual editor stays reachable from
  // the wizard's tracked-import path and when editing an existing draft.
  $('invoice-new').addEventListener('click', iwOpen);
  $('manual-new').addEventListener('click', () => edOpen(null));
  $('ed-import').addEventListener('click', async () => {
    if (ed.id) return;
    const customer = $('ed-customer').value;
    if (!customer) { showFormError($('ed-error'), { message: 'Choose a customer before importing tracked work.' }); return; }
    if (edHasChanges() && !(await askConfirm('Import tracked work instead? Unsaved invoice fields and manual items will be discarded.'))) return;
    edClose();
    $('iw-customer').value = customer;
    iw.step = 2;
    iwShow();
    iwOpen();
    iwLoadProjects().catch(error => showFormError($('iw-error'), error));
  });
  iwInit(); // #134 staged invoice wizard
  // #349: the wizard is a p-modal dialog, so Close, the native `cancel` event
  // and Escape all close it. Closing keeps the entered steps (iwClose), so
  // re-pressing `+ Invoice` never discards half-entered work.
  $('iw-close').addEventListener('click', iwClose);
  $('invoice-wizard').addEventListener('cancel', (event) => {
    // native Escape on a real browser: dismiss it, never leave it half-open.
    event.preventDefault();
    iwClose();
  });
  $('invoice-wizard').addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      // jsdom never fires 'cancel'; emulate it here (browsers do both).
      event.preventDefault();
      iwClose();
    }
  });
  $('ed-cancel').addEventListener('click', edCancel);
  $('ed-preview').addEventListener('click', () => edSave(true));
  $('ed-defaults').addEventListener('click', () => { ed.defaultsPromise = edLoadDefaults(true); });
  $('ed-customer').addEventListener('change', () => {
    if (ed.id) return;
    const customer = state.customers.find(item => item.id === $('ed-customer').value);
    ed.currency = customer?.currency || 'EUR';
    $('ed-currency').value = ed.currency;
    ed.defaultsPromise = edLoadDefaults(false);
    edRecalc();
  });
  window.addEventListener('beforeunload', event => {
    if ($('invoice-editor').open && edHasChanges()) { event.preventDefault(); event.returnValue = ''; }
  });
  for (const id of ['ed-subject', 'ed-notes']) $(id).addEventListener('input', () => { $(id).dataset.touched = '1'; });
  $('ed-currency').addEventListener('input', () => { ed.currency = $('ed-currency').value.trim().toUpperCase(); edRecalc(); });
  $('invoice-editor').addEventListener('cancel', (event) => {
    // native Escape on a real browser: cancel, never a half-open popup.
    event.preventDefault();
    edCancel();
  });
  $('invoice-editor').addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      // jsdom never fires 'cancel'; emulate it here (browsers do both).
      event.preventDefault();
      edCancel();
    }
  });
  $('ed-add').addEventListener('click', () => {
    const row = edLineRow();
    $('ed-lines').querySelector('tbody').appendChild(row);
    row.querySelector('input').focus();
    edRecalc();
  });
  $('ed-save').addEventListener('click', edSave);
  $('ed-tax').addEventListener('input', edRecalc);
  $('ed-discount').addEventListener('input', edRecalc);
  $('item-form').addEventListener('submit', saveCatalogItem);
  $('item-cancel').addEventListener('click', () => {
    $('item-id').value = '';
    $('item-form').reset();
    $('item-price').value = '0';
    $('item-currency').value = 'EUR';
    $('item-save').textContent = 'Save item';
    $('item-cancel').hidden = true;
  });
  $('ret-form').addEventListener('submit', createRetainer);
  $('ret-detail-close').addEventListener('click', () => {
    $('ret-detail').hidden = true;
  });
  $('ret-customer').addEventListener('change', async () => {
    await fillProjectSelect($('ret-project'), $('ret-customer').value, null);
  });
  $('rec-form').addEventListener('submit', addRecurring);
  $('contact-add').addEventListener('click', () => {
    if ($('contact-rows').children.length >= 10) {
      announce('At most 10 contacts per customer.');
      return;
    }
    $('contact-rows').appendChild(contactRow()).focus?.();
    $('contact-rows').lastChild.querySelector('input')?.focus();
  });
  $('template-form').addEventListener('submit', saveInvoiceTemplate);
  $('template-terms').addEventListener('change', () => {
    $('template-terms-days-field').hidden = $('template-terms').value !== 'custom';
  });
  $('customer-form').addEventListener('submit', saveCustomer);
  $('customer-new').addEventListener('click', () => {
    cancelCustomerEdit();
    $('customer-form').querySelectorAll('details').forEach((details) => { details.open = false; });
    $('customer-dialog').showModal();
    $('customer-name').focus();
  });
  $('customer-dialog').addEventListener('cancel', (event) => {
    event.preventDefault();
    cancelCustomerEdit();
  });
  $('customer-dialog').addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      cancelCustomerEdit();
    }
  });
  $('customer-cancel').addEventListener('click', cancelCustomerEdit);
}

// Customers/projects/tasks, billing settings, expenses, timer wiring (#189).
// `today` is passed in: the submission-week seed belongs to the boot date
// seeding, not the wiring block (see #189 extraction).
export function wireSetupPanels(today) {
  // #354: the customer dialog's payment-rail list. The add control is wired
  // with the other setup-panel controls; each row's Remove button is wired by
  // the row builder itself (one <button> per row, no delegated handlers).
  $('pay-rail-add').addEventListener('click', addPaymentRail);
  // #116: the day input only makes sense for `custom` terms.
  $('customer-terms').addEventListener('change', () => {
    $('customer-terms-days-field').hidden = $('customer-terms').value !== 'custom';
  });
  $('project-form').addEventListener('submit', saveProject);
  $('project-new').addEventListener('click', () => {
    // #182: always available; the parent pick is prefilled from the section
    // filter (staying editable to move/re-target when no filter is set).
    cancelProjectEdit();
    fillParentCustomers($('project-parent-customer'), state.projectsFilter);
    $('project-parent-customer').disabled = false;
    if (state.projectsFilter) prefillProjectParent();
    $('project-dialog').showModal();
    $('project-code').focus();
  });
  $('project-cancel').addEventListener('click', cancelProjectEdit);
  $('project-parent-customer').addEventListener('change', prefillProjectParent);
  $('task-form').addEventListener('submit', saveTask);
  $('task-new').addEventListener('click', async () => {
    // #182: always available; parents prefilled from the section filters.
    cancelTaskEdit();
    await fillTaskParents(state.tasksCustomer || '', state.tasksProject || '');
    $('task-dialog').showModal();
    $('task-code').focus();
  });
  $('task-cancel').addEventListener('click', cancelTaskEdit);
  $('task-parent-customer').addEventListener('change', async () => {
    const cid = $('task-parent-customer').value;
    $('task-parent-project').textContent = '';
    await fillProjectSelect($('task-parent-project'), cid, null);
    $('task-dialog-parent').textContent = cid ? customerName(cid) : '';
  });
  $('task-parent-project').addEventListener('change', () => {
    const cid = $('task-parent-customer').value;
    const projectCode = $('task-parent-project').value;
    $('task-dialog-parent').textContent = cid && projectCode ? `${customerName(cid)} / ${projectCode}` : '';
  });
  for (const [dialogId, cancel] of [['project-dialog', cancelProjectEdit], ['task-dialog', cancelTaskEdit]]) {
    $(dialogId).addEventListener('cancel', (event) => {
      event.preventDefault();
      cancel();
    });
    $(dialogId).addEventListener('keydown', (event) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        cancel();
      }
    });
  }

  $('report-form').addEventListener('submit', runReport);

  $('category-form').addEventListener('submit', addCategory);
  $('category-new').addEventListener('click', () => openCategoryDialog());
  $('category-cancel').addEventListener('click', closeCategoryDialog);
  $('expense-new').addEventListener('click', async () => {
    $('expense-form').reset();
    resetExpenseForm(); // no id, no receipt, the create wording
    $('expense-date').value = isoDate(new Date());
    $('expense-billable').checked = true;
    fillCategorySelect();
    $('expense-dialog').showModal();
    $('expense-customer').focus();
    // Derive the billed currency (and clear the conversion fields) for the
    // draft, then again on every pick that can change it.
    await refreshExpenseConversion();
  });
  $('expense-receipt').addEventListener('change', () => {
    const file = $('expense-receipt').files && $('expense-receipt').files[0];
    $('expense-receipt-note').textContent = file
      ? `${file.name} (${Math.max(1, Math.round(file.size / 1024))} KB) — saved with the expense.`
      : 'Images and PDFs up to 3 MB, stored with the expense.';
  });
  // Receipt preview (#23): the register's receipt link opens the stored document
  // in its own dialog, from the bytes the record already carries.
  initReceiptPreview();
  // Currency conversion: the rate the dialog carries comes from GET /api/fx and
  // is re-derived whenever a pick can change the billed currency. Every failure
  // is announced — a lookup that quietly did nothing would leave a field that
  // looks filled but bills the wrong amount.
  const reconvert = () => {
    refreshExpenseConversion().catch((err) => announce(`Rate lookup failed: ${err.message}`));
  };
  for (const id of ['expense-customer', 'expense-project', 'expense-currency']) {
    $(id).addEventListener('change', reconvert);
  }
  $('expense-amount').addEventListener('input', showConverted);
  $('expense-rate').addEventListener('input', showConverted);
  $('expense-cancel').addEventListener('click', () => $('expense-dialog').close());
  let expenseCategoryBeforeQuickAdd = '';
  $('expense-category').addEventListener('change', () => {
    if ($('expense-category').value === ADD_CATEGORY_VALUE) {
      $('expense-category').value = expenseCategoryBeforeQuickAdd;
      $('expense-category-name').value = '';
      clearFormError($('expense-category-error'));
      $('expense-category-dialog').showModal();
      $('expense-category-name').focus();
      return;
    }
    expenseCategoryBeforeQuickAdd = $('expense-category').value;
  });
  $('expense-category-form').addEventListener('submit', addExpenseCategory);
  // Expense register filters (#182 pattern): customer and project, both "all"
  // by default, with the dependent project reset when the customer changes.
  $('expense-filter-customer').addEventListener('change', async () => {
    state.expensesCustomer = $('expense-filter-customer').value || null;
    state.expensesProject = null;
    await refreshExpenses();
    announce(state.expensesCustomer
      ? `Expenses for ${customerName(state.expensesCustomer)} — all projects.`
      : 'Expenses for all customers and projects.');
  });
  $('expense-filter-project').addEventListener('change', async () => {
    state.expensesProject = $('expense-filter-project').value || null;
    await refreshExpenses();
    announce(state.expensesProject
      ? `Expenses for ${customerName(state.expensesCustomer)} / ${state.expensesProject}.`
      : state.expensesCustomer
        ? `All projects for ${customerName(state.expensesCustomer)}.`
        : 'Expenses for all customers and projects.');
  });
  $('expense-category-cancel').addEventListener('click', () => $('expense-category-dialog').close());
  $('expense-form').addEventListener('submit', saveExpense);
  $('submission-form').addEventListener('submit', submitWeek);
  $('user-form').addEventListener('submit', saveUser);
  $('user-new').addEventListener('click', () => {
    newUserForm();
    $('user-dialog').showModal();
    $('user-name').focus();
  });
  $('user-cancel').addEventListener('click', closeUserForm);
  $('user-search').addEventListener('input', renderUsers);
  $('user-filter-role').addEventListener('change', renderUsers);
  $('user-filter-active').addEventListener('change', renderUsers);
  $('user-password-form').addEventListener('submit', saveUserPassword);
  $('user-password-cancel').addEventListener('click', closeUserPassword);
  for (const [dialogId, close] of [['user-dialog', closeUserForm], ['user-password-dialog', closeUserPassword]]) {
    $(dialogId).addEventListener('cancel', (event) => { event.preventDefault(); close(); });
    $(dialogId).addEventListener('keydown', (event) => {
      if (event.key === 'Escape') { event.preventDefault(); close(); }
    });
  }
  $('secret-form').addEventListener('submit', addSecret);
  $('config-form').addEventListener('submit', saveConfig);
  $('backup-form').addEventListener('submit', saveBackupSettings);
  $('backup-run').addEventListener('click', runBackupNow);
  $('email-form').addEventListener('submit', saveEmailSettings);
  $('email-test-form').addEventListener('submit', sendTestEmail);
  $('timer-open').addEventListener('click', () => {
    const expanded = $('timerbar').hidden;
    $('timerbar').hidden = !expanded;
    $('timer-open').setAttribute('aria-expanded', String(expanded));
    if (expanded) $('timer-customer').focus();
  });
  $('timer-cancel').addEventListener('click', () => {
    $('timerbar').hidden = true;
    $('timer-open').setAttribute('aria-expanded', 'false');
    $('timer-open').focus();
  });
  $('timer-start').addEventListener('click', startTimer);
  $('notif-read').addEventListener('click', markNotificationsRead);
  $('timer-stop').addEventListener('click', stopTimer);
  $('timer-discard').addEventListener('click', discardTimer);
  $('timer-customer').addEventListener('change', async (e) => {
    await fillProjectSelect($('timer-project'), e.target.value, null);
  });
  $('submission-week').value = mondayOf(today);
  $('expense-customer').addEventListener('change', async (e) => {
    await fillProjectSelect($('expense-project'), e.target.value, null);
    // The amount's currency is deliberately NOT forced to the customer's here:
    // it is the currency the amount was paid in, and `refreshExpenseConversion`
    // (fx.js) prefills it while it still holds a value we derived — otherwise
    // switching customer would silently reinterpret an amount already entered in
    // another currency, and blank the conversion readout under the user.
  });
  wireSettingsControls();
  wireImports();
}

// Guarded so the router can send the Email panel's vault notice to the one
// surface that owns the key lifecycle. The target is the existing
// `/settings/security` route — no new route is introduced.
export const VAULT_SETTINGS_ROUTE = '/settings/security';

export function openVaultSettings() {
  const route = normaliseRoute(VAULT_SETTINGS_ROUTE) || VAULT_SETTINGS_ROUTE;
  if (!applyRoute(route)) announce('Settings are not available for this account.');
}

// Settings controls that must answer a plain click (#SET-01 test email,
// #SET-07 label reset, #SET-11 audit refresh). Exported so the jsdom suite can
// attach the real listeners instead of calling the handlers directly — calling
// `sendTestEmail()` itself is exactly why the dead button went unnoticed.
export function wireSettingsControls() {
  $('email-test-send').addEventListener('click', sendTestEmail);
  $('lb-reset').addEventListener('click', resetBuiltInLabels);
  $('audit-refresh').addEventListener('click', refreshAuditLog);
  $('vault-enable').addEventListener('click', enableVault);
  $('vault-regenerate').addEventListener('click', regenerateVault);
  $('vault-disable').addEventListener('click', disableVault);
  $('email-vault-settings').addEventListener('click', openVaultSettings);
}

// Wires the app listeners and loads the first data. Called once authenticated.
// #189: the listener blocks live in the focused wire*() functions above;
// what remains here is seeding + the boot sequence itself.
export async function startApp() {
  const today = isoDate(new Date());
  $('day-date').value = today;
  $('entry-date').value = today;
  $('week-date').value = today;
  $('report-from').value = mondayOf(today);
  $('report-to').value = addDays(mondayOf(today), 6);

  initTabs();
  document.querySelectorAll('button').forEach(decorateUbuntuButton);
  initSegTabs(); // Day | Week inside the Timesheets section (#107)
  // Before the tab strips can show the section: the preview card releases its
  // frame when hidden and renders the stored template once it is visible.
  initTemplatePreview();
  initSettingsTabs();
  initRouter(); // deep-link each view to a stable path (page routing)
  initWizard(); // first-run setup wizard (#111)

  wireDayAndCalendar();
  wireInvoiceDashboard();
  wireEntryForm();
  wireWeekGrid();
  wireSetupPanels(today);
  initSelectMenus();
  await loadCustomers();
  await refreshCustomerPickers(); // fills all four pickers, no fetches
  // Advanced billing tools are hidden while unfinished (see billing.js): their
  // admin-tier reads are skipped, not merely hidden, so a boot never fetches
  // data no view can show.
  if (ADVANCED_BILLING_ENABLED) refreshSchedules().catch?.(() => { });
  // SET-02: the catalog is an admin-tier read; a member boot must not fetch it.
  if (state.isAdmin) refreshCatalog().catch?.(() => { });
  if (ADVANCED_BILLING_ENABLED) refreshRetainers().catch?.(() => { });
  // First-run trigger (#111): zero customers after a successful login opens
  // the wizard as a modal. Once a customer exists it never auto-opens — the
  // sidebar entry reopens it manually.
  if ((state.customers || []).length === 0) openWizard({ fresh: true });
  // D1: paint the day view first, then fetch the independent panels together
  // (~17 sequential RTTs used to gate first paint; now ~2).
  const firstPaint = refreshDay();
  $('expense-date').value = today;
  await Promise.all([
    firstPaint,
    refreshWeek(),
    // #140: a deep link (or a plain reload) straight to the Calendar is selected
    // by applyRoute() with the panel refresh suppressed — the router trusts this
    // boot sequence to paint — and it only painted Day and Week, so the month
    // grid stayed empty until the segment was clicked. Paint the linked segment.
    segActiveId === 'ts-cal' ? refreshTimeCal() : null,
    refreshCustomerTable(),
    // #182: Projects and Tasks were only ever painted by refreshPanel(), which
    // applyRoute() suppresses while it selects a tab (the router trusts the boot
    // sequence to paint). A deep link - or a plain reload - of /projects or /tasks
    // therefore rendered an empty register until the tab was clicked again.
    refreshProjectTable(),
    refreshTaskTable(),
    refreshInvoices(),
    refreshTimer(),
    refreshNotifications(),
    refreshCategories(),
    fillCategorySelect(),
    refreshExpenses(),
    refreshSubmissions(),
    refreshSettings(),
    // Footer stamp: concurrent with the panel fetches, so it costs no extra
    // round-trip and is painted before the boot flag below.
    paintBuildStamp(),
  ]);
  // Boot sentinel (#265): the GUI tests (and any external smoke check) wait on
  // this attribute rather than on footer copy, which now names a build.
  document.documentElement.dataset.booted = 'ready';
}

/// The footer's build stamp: the running binary's crate version and the commit
/// it was built from, both read from `/healthz` — the API is the only source,
/// so the GUI can never hardcode a version and then lie about a server that was
/// upgraded underneath it. The footer is cosmetic: a probe that fails (or a
/// server too old to answer with a version) leaves the static `TatuTime`
/// branding in place instead of breaking the boot.
async function paintBuildStamp() {
  if (typeof fetch !== 'function') return; // jsdom harnesses stub the API, not fetch
  try {
    const res = await fetch('/healthz', { headers: {}, credentials: 'same-origin' });
    if (!res.ok) return;
    const info = await res.json();
    if (typeof info !== 'object' || info === null) return;
    if (typeof info.version !== 'string' || !info.version) return;
    $('version').textContent = `TatuTime v${info.version}`;
    if (typeof info.build === 'string' && info.build) {
      const build = $('build');
      build.textContent = `· build ${info.build}`;
      build.hidden = false;
    }
  } catch {
    /* keep the static branding */
  }
}

