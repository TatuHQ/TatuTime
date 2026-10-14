// boot.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { addRecurring, createRetainer, iwInit, refreshCatalog, refreshRetainers, refreshSchedules, saveCatalogItem } from './billing.js';
import { calState, refreshTimeCal, shiftMonth } from './calendar.js';
import { $, addDays, announce, clearFormError, decorateUbuntuButton, isoDate, mondayOf } from './dom.js';
import { ADD_CATEGORY_VALUE, addCategory, addExpense, addExpenseCategory, fillCategorySelect, refreshCategories, refreshExpenses, refreshSubmissions, resetCategoryForm, submitWeek } from './expenses.js';
import { cancelCustomerEdit, cancelProjectEdit, cancelTaskEdit, contactRow, fillParentCustomers, fillTaskParents, prefillProjectParent, refreshCustomerPickers, refreshCustomerTable, refreshProjectTable, refreshTaskTable, runReport, saveCustomer, saveProject, saveTask } from './hierarchy.js';
import { closeInvoicePreview, dash, dashApplyColumns, downloadInvoicePdf, ed, edClose, edLineRow, edOpen, edRecalc, edSave, generateInvoice, invoicePreview, refreshInvoiceSummary, refreshInvoices } from './invoices.js';
import { markNotificationsRead, refreshNotifications } from './notifications.js';
import { initRouter, initSegTabs, initSettingsTabs, initTabs, refreshTimesheetView, segActiveId, showTimesheet } from './route.js';
import { addSecret, closeUserForm, closeUserPassword, newUserForm, paintAccent, refreshSettings, renderUsers, saveConfig, saveInvoiceTemplate, saveOrgProfile, saveUser, saveUserPassword } from './settings.js';
import { customerName, fillProjectSelect, fillTaskSelect, loadCustomers, state } from './state.js';
import { discardTimer, refreshTimer, startTimer, stopTimer } from './timer.js';
import { commitCell, copyLastWeek, copyPreviousDay, entrySaving, navigateDay, navigateWeek, refreshDay, refreshWeek, resetEntryForm, saveEntry, selectDay, showEntryForm, weekState } from './timesheet.js';
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
      await refreshSchedules();
      await refreshRetainers();
    },
    'tab-expenses': refreshExpenses,
    'tab-submissions': refreshSubmissions,
    'tab-customers': async () => {
      await loadCustomers();
      await refreshCustomerTable();
      await refreshCustomerPickers();
    },
    // #182: the hierarchy sections pull their own scoped data on show.
    'tab-projects': async () => {
      await loadCustomers();
      await refreshProjectTable();
    },
    'tab-tasks': async () => {
      await loadCustomers();
      await refreshTaskTable();
    },
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
    refreshTimeCal().catch?.(() => {});
  });
  $('cal-next').addEventListener('click', () => {
    calState.month = shiftMonth(calState.month || $('day-date').value.slice(0, 7), 1);
    refreshTimeCal().catch?.(() => {});
  });
  $('cal-today').addEventListener('click', () => {
    calState.month = isoDate(new Date()).slice(0, 7);
    refreshTimeCal().catch?.(() => {});
  });
}

// Invoice dashboard controls (#135/#189).
export function wireInvoiceDashboard() {
  // #135 dashboard controls
  $('inv-tab-open').addEventListener('click', () => {
    dash.openOnly = true;
    $('inv-tab-open').setAttribute('aria-pressed', 'true');
    $('inv-tab-all').setAttribute('aria-pressed', 'false');
    refreshInvoices().catch?.(() => {});
  });
  $('inv-tab-all').addEventListener('click', () => {
    dash.openOnly = false;
    $('inv-tab-all').setAttribute('aria-pressed', 'true');
    $('inv-tab-open').setAttribute('aria-pressed', 'false');
    refreshInvoices().catch?.(() => {});
  });
  $('invoice-search').addEventListener('input', () => {
    dash.q = $('invoice-search').value;
    refreshInvoices().catch?.(() => {});
  });
  $('inv-f-customer').addEventListener('change', () => {
    dash.customer = $('inv-f-customer').value;
    // #189: refreshInvoices chains refreshInvoiceSummary with the shared
    // payload — the old double call fired two renders and three GETs.
    refreshInvoices().catch?.(() => {});
  });
  $('inv-f-project').addEventListener('change', () => {
    dash.project = $('inv-f-project').value;
    refreshInvoices().catch?.(() => {});
  });
  $('inv-f-from').addEventListener('change', () => {
    dash.from = $('inv-f-from').value;
    refreshInvoices().catch?.(() => {});
  });
  $('inv-f-to').addEventListener('change', () => {
    dash.to = $('inv-f-to').value;
    refreshInvoices().catch?.(() => {});
  });
  for (const cb of document.querySelectorAll('.inv-col')) {
    cb.addEventListener('change', dashApplyColumns);
  }
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
      refreshInvoices().catch?.(() => {});
    });
  }
  $('inv-year-prev').addEventListener('click', () => {
    dash.year -= 1;
    refreshInvoiceSummary().catch?.(() => {});
  });
  $('inv-year-next').addEventListener('click', () => {
    dash.year += 1;
    refreshInvoiceSummary().catch?.(() => {});
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
  $('ip-close').addEventListener('click', closeInvoicePreview);
  $('ip-download').addEventListener('click', () => {
    if (invoicePreview) downloadInvoicePdf(invoicePreview.inv.id, invoicePreview.inv.number);
  });
  $('invoice-preview').addEventListener('keydown', (e) => {
    if (e.key === 'Escape') closeInvoicePreview();
  });
  $('org-form').addEventListener('submit', saveOrgProfile);
  $('org-accent').addEventListener('input', paintAccent);
  $('manual-new').addEventListener('click', () => edOpen(null));
  iwInit(); // #134 staged invoice wizard
  $('ed-close').addEventListener('click', edClose);
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
  $('lb-reset').addEventListener('click', () => {
    for (const k of ['lb-description', 'lb-quantity', 'lb-unit-price', 'lb-subtotal', 'lb-discount', 'lb-tax', 'lb-total']) $(k).value = '';
    announce('Built-in labels restored (save the template to apply).');
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

  $('invoice-form').addEventListener('submit', generateInvoice);

  $('category-form').addEventListener('submit', addCategory);
  $('category-cancel').addEventListener('click', resetCategoryForm);
  $('expense-new').addEventListener('click', () => {
    $('expense-form').reset();
    $('expense-date').value = isoDate(new Date());
    $('expense-billable').checked = true;
    fillCategorySelect();
    $('expense-dialog').showModal();
    $('expense-customer').focus();
  });
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
  $('expense-category-cancel').addEventListener('click', () => $('expense-category-dialog').close());
  $('expense-form').addEventListener('submit', addExpense);
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
    const c = state.customers.find((x) => x.id === e.target.value);
    if (c) $('expense-currency').value = c.currency;
  });

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
  initSettingsTabs();
  initRouter(); // deep-link each view to a stable path (page routing)
  initWizard(); // first-run setup wizard (#111)

  wireDayAndCalendar();
  wireInvoiceDashboard();
  wireEntryForm();
  wireWeekGrid();
  wireSetupPanels(today);
  await loadCustomers();
  await refreshCustomerPickers(); // fills all four pickers, no fetches
  refreshSchedules().catch?.(() => {});
  refreshCatalog().catch?.(() => {});
  refreshRetainers().catch?.(() => {});
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
  refreshCustomerTable(),
  refreshInvoices(),
  refreshTimer(),
  refreshNotifications(),
  refreshCategories(),
  fillCategorySelect(),
  refreshExpenses(),
  refreshSubmissions(),
  refreshSettings(),
  ]);
  $('version').textContent = 'TucanoTime';
}

