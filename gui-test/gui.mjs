// Headless GUI smoke test: load the real index.html + app.js into jsdom,
// wire fetch to the live server, run boot(), then simulate adding an entry
// and creating a customer through the actual forms. Verifies the presentation
// layer drives the API and renders results.
//
// Waiting convention (#265): never assert against wall-clock. Either waitFor()
// the condition the next check depends on, or settle() the requests the
// previous interaction started — see gui-test/README.md.

import { JSDOM, VirtualConsole } from 'jsdom';
import fs from 'node:fs';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

// CI serves on :8099; local runs can point elsewhere (TT_GUI_BASE) so a
// dev's harness server can never shadow the runner's own container.
const BASE = process.env.TT_GUI_BASE || 'http://localhost:8099';

const html = fs.readFileSync('../web/index.html', 'utf8');
const appJs = loadAppSource();

const vc = new VirtualConsole();
vc.on('jsdomError', (e) => console.error('[jsdom error]', e.message));
vc.on('error', (m) => console.error('[console.error]', m));

const dom = new JSDOM(html, { runScripts: 'outside-only', pretendToBeVisual: true, url: BASE + '/', virtualConsole: vc });
const { window } = dom;

// jsdom does not implement the native dialog open/close methods. Preserve the
// reflected `open` state so popup flows can be asserted without changing app code.
if (window.HTMLDialogElement && !window.HTMLDialogElement.prototype.showModal) {
  window.HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute('open', '');
  };
  window.HTMLDialogElement.prototype.close = function close() {
    this.removeAttribute('open');
  };
}

// Cookie-aware fetch for the whole harness: attaches the session once logged in.
// Requests are counted so settle() can wait for the UI's own work to drain
// instead of sleeping a fixed tick (#265).
const _fetch = globalThis.fetch.bind(globalThis);
let SESSION_COOKIE = '';
let inFlight = 0;
function fetch(url, init = {}) {
  const headers = { ...(init.headers || {}) };
  if (SESSION_COOKIE && !('Cookie' in headers)) headers.Cookie = SESSION_COOKIE;
  headers['X-CSRF-Protection'] = '1';
  inFlight += 1;
  const done = () => { inFlight -= 1; };
  return _fetch(url, { ...init, headers }).then(
    (response) => { done(); return response; },
    (error) => { done(); throw error; },
  );
}

// window.fetch resolves root-relative paths against BASE and uses the wrapper.
window.fetch = (input, init = {}) => {
  const url = typeof input === 'string' && input.startsWith('/') ? BASE + input : input;
  return fetch(url, init);
};
// No window.confirm/prompt/alert stubs: the app drives one inline <dialog>
// (#102) and the checks below exercise it like a user would.
if (!window.Element.prototype.scrollIntoView) window.Element.prototype.scrollIntoView = () => {};
// jsdom has no blob URLs; the #113 download check stubs these and captures
// the transient <a download> instead of letting jsdom attempt a navigation.
window.URL.createObjectURL = () => 'blob:jsdom-stub';
window.URL.revokeObjectURL = () => {};

// --- authenticate + seed against the live server (fresh data dir) ---
const postJson = (path, body) =>
  fetch(path.startsWith('http') ? path : BASE + path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
await postJson(BASE + '/api/auth/bootstrap', { name: 'Admin', email: 'admin@test.local', password: 'supersecret1' });
const loginRes = await postJson(BASE + '/api/auth/login', { email: 'admin@test.local', password: 'supersecret1' });
SESSION_COOKIE = (loginRes.headers.get('set-cookie') || '').split(';')[0];
// NOTE: no API seeding of customers/projects here — the first-run setup
// wizard (#111) creates ACME + P-9 through the real UI below, exercising the
// trigger, the steps and the prefill rule end-to-end.

// Helpers for the waiting convention (#265).
const byId = (id) => window.document.getElementById(id);
const dialogOpen = (el) => el.open === true || el.hasAttribute('open');

// Wait until the work the interactions so far started has drained: no request
// the UI opened is still in flight, and a two-poll quiet window gives the
// re-render that follows the last one a turn to run. Used where the next check
// reads server-fed data (a table, a total, a live announcement); a purely local
// DOM state is waited for directly with waitFor().
async function settle(message = 'the UI to finish its outstanding work') {
  let quietPolls = 0;
  await waitFor(() => {
    if (inFlight > 0) {
      quietPolls = 0;
      return false;
    }
    quietPolls += 1;
    return quietPolls >= 2;
  }, { message });
}

// Inject and run the real app, then fire DOMContentLoaded.
window.eval(appJs);
window.document.dispatchEvent(new window.Event('DOMContentLoaded', { bubbles: true }));
await waitFor(() => byId('version').textContent === 'TucanoTime', {
  message: 'the app to finish booting after DOMContentLoaded',
});

let failures = 0;
function check(name, cond) {
  console.log(`${cond ? 'PASS' : 'FAIL'}  ${name}`);
  if (!cond) failures++;
}

// #135: the invoice table defaults to the Open tab; row checks that need
// drafts/paid go through the All tab first.
const showAllInvoices = async () => {
  window.document.getElementById('inv-tab-all').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the invoice table to reload for the All tab');
};

// ---- FIRST-RUN WIZARD (#111): auto-open on zero customers ----
const wzDlg = window.document.getElementById('wizard-dialog');
const wzOpen = () => wzDlg.open === true || wzDlg.hasAttribute('open');
check('wizard auto-opens as a modal on first login with no customers', wzOpen());
check('wizard starts on the welcome step', !window.document.getElementById('wz-step-0').hidden);
window.document.getElementById('wz-later').dispatchEvent(new window.Event('click', { bubbles: true }));
check('"Cancel" dismisses the wizard cleanly', !wzOpen());
// The setup wizard is a utility action after core and secondary navigation.
// #191 moved the utility action out of the tablist (ARIA required-owned-
// children); #125's invariant is that the wizard stays the LAST sidebar
// action, sitting between the tablist and the colour-mode switch.
const navButtons = [...window.document.querySelectorAll('nav[aria-label="Sections"] button')].map((b) => b.id);
const tablistButtons = [...window.document.querySelectorAll('#tabs button')].map((b) => b.id);
check('wizard entry is the last sidebar action, outside the tablist (#125/#191)',
  navButtons[navButtons.length - 1] === 'wizard-open' && !tablistButtons.includes('wizard-open'));

// #142: grouped rail + shortcuts.
check('core MVP navigation comes first, Setup split into three (#142/#182)',
  [...window.document.querySelectorAll('#tabs [role="tab"]')].slice(0, 5).map((t) => t.id).join(',') ===
    'tab-timesheet,tab-customers,tab-projects,tab-tasks,tab-invoices');
// #16 is hidden for now (the approval process is not implemented), so the rail
// lists only the tools that are really available: hidden entries drop out of the
// visible order and must not be offering themselves to the user.
const railTabs = [...window.document.querySelectorAll('#tabs [role="tab"]')]
  .filter((t) => !t.hidden).map((t) => t.id);
check('implemented secondary tools are grouped after the MVP destinations',
  railTabs.slice(5).join(',') === 'tab-expenses,tab-reports,tab-settings' // after MVP + Setup tabs (#182)
  && [...window.document.querySelectorAll('#tabs .nav-label')].map((n) => n.textContent).join(',') ===
    'Track time,Setup,Billing,More tools');
check('the Approvals rail entry is hidden until its process exists (#16)',
  window.document.getElementById('tab-submissions').hidden === true
  && !railTabs.includes('tab-submissions'));
check('redundant Invoice and Timer shortcut buttons are absent',
  !window.document.getElementById('shortcut-invoices') && !window.document.getElementById('shortcut-timer'));
const timerOpen = window.document.getElementById('timer-open');
const timerSetup = window.document.getElementById('timerbar');
// Heading-slot layout: Track time lives in .ts-head beside the Day|Week|
// Calendar segments (one button for the whole section); the bars keep the
// timer cluster, centered navigation and quiet links.
check('Track time sits in the timesheet heading row beside the segments',
  window.document.getElementById('day-add').parentElement.className.includes('ts-head')
  && window.document.getElementById('day-add').nextElementSibling.id === 'ts-seg');
check('bars hold timer/nav/links only',
  window.document.getElementById('timer-open').parentElement.className.includes('day-actions')
  && window.document.getElementById('week-today').parentElement.className.includes('day-end')
  && !window.document.getElementById('week-track'));
check('timer selections are hidden until requested', timerSetup.hidden);
timerOpen.click();
check('Start timer reveals setup and focuses customer',
  !timerSetup.hidden && timerOpen.getAttribute('aria-expanded') === 'true'
  && window.document.activeElement.id === 'timer-customer');
window.document.getElementById('timer-cancel').click();
check('Cancel hides timer setup and returns focus to its action',
  timerSetup.hidden && timerOpen.getAttribute('aria-expanded') === 'false'
  && window.document.activeElement === timerOpen);
window.document.getElementById('wizard-open').dispatchEvent(new window.Event('click', { bubbles: true }));
check('the icon reopens the wizard fresh', wzOpen() && !window.document.getElementById('wz-step-0').hidden);
window.document.getElementById('wz-next').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !byId('wz-step-1').hidden && byId('wz-step-0').hidden, {
  message: 'the wizard to advance to the customer step',
});
check('Next advances to the customer step', !window.document.getElementById('wz-step-1').hidden && window.document.getElementById('wz-step-0').hidden);
// #229: the first-customer field is prefilled with the example name, so a
// fresh setup needs no typing (still editable).
check('customer name is prefilled with the Tucano Time example',
  window.document.getElementById('wz-cust-name').value === 'Tucano Time');
// Clear the prefill and submit: an empty name must refuse to save.
window.document.getElementById('wz-cust-name').value = '';
window.document.getElementById('wz-next').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !byId('wz-error').hidden && !byId('wz-step-1').hidden, {
  message: 'the wizard to show its inline customer-name error',
});
check('empty customer name shows an inline error (no silent partial save)',
  !window.document.getElementById('wz-error').hidden
  && !window.document.getElementById('wz-step-1').hidden);
window.document.getElementById('wz-cust-name').value = 'ACME';
window.document.getElementById('wz-cust-currency').value = 'eur';
window.document.getElementById('wz-cust-rate').value = '60.00';
window.document.getElementById('wz-next').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !byId('wz-step-2').hidden && byId('wz-project-currency').value === 'EUR', {
  message: 'the wizard to save ACME and prefill the project step',
});
check('wizard advanced to the project step', !window.document.getElementById('wz-step-2').hidden);
check('project currency/rate prefilled from the customer default (#11)',
  window.document.getElementById('wz-project-currency').value === 'EUR'
  && window.document.getElementById('wz-project-rate').value === '60.00');
window.document.getElementById('wz-project-code').value = 'p-9';
window.document.getElementById('wz-project-name').value = 'Portal';
window.document.getElementById('wz-next').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !byId('wz-step-3').hidden && byId('wz-done-text').textContent.includes('ACME'), {
  message: 'the wizard to create the project and reach the done step',
});
const custs111 = (await (await fetch(BASE + '/api/customers', { headers: { Cookie: SESSION_COOKIE } })).json()).customers;
const acme111 = custs111.find((c) => c.name === 'ACME');
const projs111 = acme111
  ? (await (await fetch(`${BASE}/api/customers/${acme111.id}/projects`, { headers: { Cookie: SESSION_COOKIE } })).json()).projects
  : [];
check('wizard created customer + project through the real routes',
  !!acme111 && acme111.default_rate_minor === 6000
  && projs111.some((p) => p.code === 'P-9' && p.currency === 'EUR' && p.rate_minor === 6000));
check('done step names what was set up',
  !window.document.getElementById('wz-step-3').hidden
  && window.document.getElementById('wz-done-text').textContent.includes('ACME'));
// #141: the last step used to hide the wizard's only dismiss button.
check('the wizard keeps a visible Cancel on its last step (#141)',
  (() => {
    const b = window.document.getElementById('wz-later');
    return !b.hidden && /^cancel$/i.test(b.textContent.trim());
  })());
// Finish: dialog closes, Day view with the pair preselected.
window.document.getElementById('wz-next').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !wzOpen() && byId('entry-project').value === 'P-9', {
  message: 'the wizard to close and preselect the new customer and project',
});
check('wizard closed on finish', !wzOpen());
check('lands on the Timesheets (Day) view',
  window.document.getElementById('tab-timesheet').getAttribute('aria-selected') === 'true');
check('new customer + project preselected in the entry form',
  window.document.getElementById('entry-customer').value === acme111.id
  && window.document.getElementById('entry-project').value === 'P-9');
// Reopen with customers present jumps straight to the project step.
window.document.getElementById('wizard-open').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => wzOpen() && !byId('wz-step-2').hidden && byId('wz-step-0').hidden, {
  message: 'the reopened wizard to jump to the project step',
});
check('reopen with existing customers skips straight to the project step',
  wzOpen() && !window.document.getElementById('wz-step-2').hidden && window.document.getElementById('wz-step-0').hidden);
window.document.getElementById('wz-later').dispatchEvent(new window.Event('click', { bubbles: true }));
// Never auto-opens again once a customer exists: boot a second app instance
// against the same (now non-empty) server.
const dom2 = new JSDOM(html, { runScripts: 'outside-only', pretendToBeVisual: true, url: BASE + '/', virtualConsole: vc });
dom2.window.fetch = window.fetch;
if (!dom2.window.Element.prototype.scrollIntoView) dom2.window.Element.prototype.scrollIntoView = () => {};
dom2.window.URL.createObjectURL = () => 'blob:jsdom-stub';
dom2.window.URL.revokeObjectURL = () => {};
dom2.window.eval(appJs);
dom2.window.document.dispatchEvent(new window.Event('DOMContentLoaded', { bubbles: true }));
await waitFor(() => dom2.window.document.getElementById('version').textContent === 'TucanoTime', {
  message: 'the second app instance to finish booting',
});
check('wizard never auto-opens again once a customer exists',
  !(dom2.window.document.getElementById('wizard-dialog').open === true
    || dom2.window.document.getElementById('wizard-dialog').hasAttribute('open')));
dom2.window.close();

// ---- picker shows the ACME the wizard just created ----
const customerSelect = window.document.getElementById('entry-customer');
check('customer picker populated', [...customerSelect.options].some((o) => o.textContent.includes('ACME')));

// ---- pick ACME, load its project, fill the entry form and submit ----
const acmeOpt = [...customerSelect.options].find((o) => o.textContent.includes('ACME'));
customerSelect.value = acmeOpt.value;
customerSelect.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => [...byId('entry-project').options].some((o) => o.value === 'P-9'), {
  message: 'the project picker to load the ACME projects',
});

const projectSelect = window.document.getElementById('entry-project');
const p9 = [...projectSelect.options].find((o) => o.value === 'P-9');
check('project picker loaded P-9 from API', !!p9);
projectSelect.value = 'P-9';

window.document.getElementById('entry-date').value = '2026-11-11';
window.document.getElementById('entry-hours').value = '4.25';
window.document.getElementById('entry-note').value = 'gui-test entry';

const entryForm = window.document.getElementById('entry-form');
entryForm.dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await waitFor(() => byId('day-total').textContent.trim() === '4:15', {
  message: 'the day table to re-render with the new 4.25h entry',
});

// ---- verify the entry actually reached the server through the form ----
const day = await (await fetch(BASE + '/api/entries?date=2026-11-11')).json();
const made = day.entries.find((e) => e.hours === 4.25 && e.project_code === 'P-9');
check('form submit created the entry via API', !!made);
check('entry note stored as typed', made && made.note === 'gui-test entry');

// ---- day table re-rendered and shows the new row ----
const rows = window.document.querySelectorAll('#day-table tbody tr');
check('day table renders the new entry row', rows.length >= 1);
const totals = window.document.getElementById('day-total').textContent;
check('day total reflects 4.25h as H:MM', totals.trim() === '4:15');

// ---- live region announced the success (accessibility) ----
const live = window.document.getElementById('live-region').textContent;
check('aria-live region announced an add', /added|updated/i.test(live));

// ---- DAY-ENTRY DIALOG (#145) ----
{
  const edlg = window.document.getElementById('entry-dialog');
  const isOpen = () => edlg.open === true || edlg.hasAttribute('open');
  check('adding an entry closes the Day popup', !isOpen());
  check('successful save returns focus to Track time',
    window.document.activeElement.id === 'day-add');
  check('dialog kept the day just logged (#145)', window.document.getElementById('entry-date').value === '2026-11-11');
  window.document.getElementById('day-add').click();
  window.document.getElementById('entry-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await waitFor(() => isOpen() && !byId('entry-error').hidden, {
    message: 'the invalid entry to keep the dialog open with its error',
  });
  check('invalid entry keeps the popup open with its error',
    isOpen() && !window.document.getElementById('entry-error').hidden);
  edlg.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  await waitFor(
    () => !isOpen() && /Entry cancelled . nothing was saved/i.test(byId('live-region').textContent),
    { message: 'Escape to close the entry dialog and announce the cancel' },
  );
  check('Escape cancels and closes the dialog (#145)', !isOpen());
  check('cancel announces nothing was saved (#145)',
    /Entry cancelled . nothing was saved/i.test(window.document.getElementById('live-region').textContent));
  const before = (await (await fetch(BASE + '/api/entries?date=2026-11-11', { headers: { Cookie: SESSION_COOKIE } })).json()).entries.length;
  window.document.getElementById('entry-hours').value = '7';
  window.document.getElementById('entry-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await settle('the ignored submit to leave the app idle');
  const after = (await (await fetch(BASE + '/api/entries?date=2026-11-11', { headers: { Cookie: SESSION_COOKIE } })).json()).entries.length;
  check('cancelling then not submitting saved nothing (#145)', after === before);
  // Edit opens the dialog pre-filled with the Update label.
  const editBtn = [...window.document.querySelectorAll('#day-table tbody button')].find((b) => /edit/i.test(b.textContent));
  if (editBtn) {
    editBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
    await waitFor(() => isOpen() && byId('entry-save').textContent === 'Update entry' && byId('entry-hours').value !== '', {
      message: 'the edit action to open the dialog pre-filled',
    });
    check('edit opens the dialog pre-filled (#145)',
      isOpen() && window.document.getElementById('entry-save').textContent === 'Update entry'
      && window.document.getElementById('entry-hours').value !== '');
    window.document.getElementById('entry-cancel').dispatchEvent(new window.Event('click', { bubbles: true }));
    await waitFor(() => !isOpen(), { message: 'the cancelled edit to close the dialog' });
    check('cancel edit returns to the timesheet (#145)', !isOpen());
  }
}


// ---- inline <dialog> (#102): delete confirms through it, cancel keeps ----
const dlgNode = window.document.getElementById('app-dialog');
const isOpen = () => dlgNode.open === true || dlgNode.hasAttribute('open');
const dayRow0 = window.document.querySelector('#day-table tbody tr');
const dlgDel = [...dayRow0.querySelectorAll('button')].find((b) => b.textContent === 'Delete');
dlgDel.dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => isOpen(), { message: 'the delete action to open the inline dialog' });
check('delete click opens the inline dialog', isOpen());
check(
  'dialog message names the entry being deleted',
  /Delete this entry \(4\.25h on P-9\)/.test(window.document.getElementById('dlg-message').textContent),
);
window.document.getElementById('dlg-cancel').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !isOpen(), { message: 'the confirm dialog to close on cancel' });
check('dialog closes on cancel', !isOpen());
const kept = await (await fetch(BASE + '/api/entries?date=2026-11-11')).json();
check('cancel keeps the entry (nothing deleted)', kept.entries.some((e) => e.id === made.id));
// Confirm path: same flow, OK accepts and the entry disappears.
dlgDel.dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => isOpen(), { message: 'the confirm dialog to reopen for the delete' });
window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
await settle('the confirmed delete to reach the server and re-render the day');
const gone = await (await fetch(BASE + '/api/entries?date=2026-11-11')).json();
check('confirming in the dialog deletes the entry', !gone.entries.some((e) => e.id === made.id));
// Restore the entry (later checks assert the 4.25h day) and re-render.
await postJson('/api/entries', {
  date: '2026-11-11',
  customer_id: acmeOpt.value,
  project_code: 'P-9',
  hours: 4.25,
  note: 'gui-test entry',
  billable: true,
});
window.document.getElementById('day-prev').dispatchEvent(new window.Event('click', { bubbles: true }));
await settle('the previous-day render');
window.document.getElementById('day-next').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => {
  const selected = window.document.querySelector('#week-strip .ws-day.selected');
  return byId('day-date').value === '2026-11-11'
    && !!selected && selected.dataset.date === '2026-11-11' && selected.textContent.includes('4:15');
}, { message: 'the day navigator to return to 2026-11-11 with its week-strip total' });

// ---- DAY REDESIGN (#12): row structure, week strip, navigator, copy-forward ----
const mainCell = rows[0].querySelector('.entry-main');
// #145-columns: the note moved from the stacked main cell to its own
// Notes column (asserted again with headers in the #141 section below).
check(
  'entry row shows project / customer, note lives in its own column',
  mainCell.textContent.includes('P-9') && mainCell.textContent.includes('ACME')
    && !mainCell.textContent.includes('gui-test entry')
    && rows[0].querySelector('.entry-note').textContent.includes('gui-test entry'),
);
const stripDays = window.document.querySelectorAll('#week-strip .ws-day');
check('week strip renders seven days', stripDays.length === 7);
const selDay = window.document.querySelector('#week-strip .ws-day.selected');
check('selected day emphasised with H:MM total', selDay && selDay.dataset.date === '2026-11-11' && selDay.textContent.includes('4:15'));
check('week strip shows week total', window.document.querySelector('.ws-week').textContent.trim() === 'Week total 4:15');

window.document.getElementById('day-next').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => {
  const selected = window.document.querySelector('#week-strip .ws-day.selected');
  return byId('day-date').value === '2026-11-12'
    && !!selected && selected.dataset.date === '2026-11-12';
}, { message: 'the day navigator to move forward to 2026-11-12' });
check('day navigator moves forward', window.document.getElementById('day-date').value === '2026-11-12');

const copyButton = window.document.getElementById('copy-previous');
check('copy previous day is a button rather than a dropdown',
  copyButton.tagName === 'BUTTON' && !window.document.getElementById('copy-days'));
copyButton.click();
await waitFor(() => {
  const row = window.document.querySelector('#copy-rows .copy-row');
  return !!row && row.textContent.includes('P-9');
}, { message: 'copy previous day to list the P-9 row' });
const copyRow = window.document.querySelector('#copy-rows .copy-row');
check('copy previous day lists the P-9 row', !!copyRow && copyRow.textContent.includes('P-9'));
copyRow.querySelector('input').value = '3';
copyRow.querySelector('button').dispatchEvent(new window.Event('click', { bubbles: true }));
await settle('the copied row to save through the API');
const day12 = await (await fetch(BASE + '/api/entries?date=2026-11-12')).json();
const copied = day12.entries.find((e) => e.project_code === 'P-9' && e.hours === 3);
check('copied row saved through the API', !!copied);
// Clean up so later week-view assertions stay at 4.25h.
await fetch(`${BASE}/api/entries/${copied.id}`, { method: 'DELETE', headers: { Cookie: SESSION_COOKIE } });

// ---- create a brand-new customer through the customers form ----
// jsdom 30.1.2 implements the spec "being rendered" focusability rule: an
// element under a display:none ancestor is no longer focusable, like every
// real browser. The other sections already navigate to their panel before
// driving its forms; do the same here instead of relying on the old lenient
// no-op-free focus into the hidden customers panel.
window.document.getElementById('tab-customers').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !byId('panel-customers').hidden, { message: 'the Customers section to show' });
const custForm = window.document.getElementById('customer-form');
const customerDialog = window.document.getElementById('customer-dialog');
check('customer form is in a closed popup initially',
  custForm.closest('dialog') === customerDialog && !customerDialog.open);
window.document.getElementById('customer-new').click();
check('+ Customer opens a blank popup and focuses Name',
  customerDialog.open && window.document.activeElement.id === 'customer-name'
  && window.document.getElementById('customer-id').value === '');
window.document.getElementById('customer-cancel').click();
check('Cancel closes the customer popup', !customerDialog.open);
window.document.getElementById('customer-new').click();
customerDialog.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
check('Escape closes the customer popup', !customerDialog.open);
window.document.getElementById('customer-new').click();
window.document.getElementById('customer-name').value = 'Globex';
window.document.getElementById('customer-currency').value = 'usd';
window.document.getElementById('customer-rate').value = '45.50';
custForm.dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the customer form to save Globex and refresh every picker');

const customers = (await (await fetch(BASE + '/api/customers')).json()).customers;
const globex = customers.find((c) => c.name === 'Globex');
check('customer form created Globex', !!globex);
check('customer save closes the popup and restores focus',
  !customerDialog.open && window.document.activeElement.id === 'customer-new');
// C7 regression: every customer picker refreshes immediately (used to need a
// page reload, so a new customer could not be invoiced/expensed/timed).
const optHas = (id, name) => [...window.document.getElementById(id).options].some((o) => o.textContent.includes(name));
check('invoice picker sees the new customer', optHas('invoice-customer', 'Globex'));
check('expense picker sees the new customer', optHas('expense-customer', 'Globex'));
check('timer picker sees the new customer', optHas('timer-customer', 'Globex'));
check('currency normalised to USD', globex && globex.currency === 'USD');
check('rate stored as 4550 minor units', globex && globex.default_rate_minor === 4550);

// ---- reports tab runs a query and renders totals ----
const reportForm = window.document.getElementById('report-form');
window.document.getElementById('report-from').value = '2026-11-01';
window.document.getElementById('report-to').value = '2026-11-30';
window.document.getElementById('report-group').value = 'project';
reportForm.dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await waitFor(() => window.document.querySelectorAll('#report-table tbody tr').length >= 1, {
  message: 'the report query to render grouped rows',
});
const reportRows = window.document.querySelectorAll('#report-table tbody tr');
check('report renders grouped rows', reportRows.length >= 1);
const csvHref = window.document.getElementById('csv-link').getAttribute('href');
check('CSV download link wired', csvHref && csvHref.includes('export.csv'));

// ---- tab keyboard nav moves selection ----
const tabDay = window.document.getElementById('ts-day');
tabDay.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
await waitFor(() => byId('ts-week').getAttribute('aria-selected') === 'true', {
  message: 'the arrow key to move the Day|Week tab selection',
});
check('arrow key switches tab', window.document.getElementById('ts-week').getAttribute('aria-selected') === 'true');

// ---- WEEK GRID VIEW (#13): inline editable cells ----
const tabWeek = window.document.getElementById('ts-week');
tabWeek.dispatchEvent(new window.Event('click', { bubbles: true }));
window.document.getElementById('week-date').value = '2026-11-11';
window.document.getElementById('week-date').dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => {
  const row = window.document.querySelector('#week-table tbody tr');
  return !!row && row.textContent.includes('ACME') && row.textContent.includes('P-9');
}, { message: 'the week grid to render the ACME / P-9 row' });
const wkRows = window.document.querySelectorAll('#week-table tbody tr');
const wkRowText = wkRows[0] ? wkRows[0].textContent : '';
check('week grid renders the ACME / P-9 row', wkRowText.includes('ACME') && wkRowText.includes('P-9'));
const cellOf = (date) => window.document.querySelector(`#week-table input.cell-input[data-date="${date}"]`);
const filledCell = cellOf('2026-11-11');
check('filled cell is an editable input holding 4.25', filledCell && filledCell.value === '4.25');
check('week row total is H:MM', wkRows[0].querySelector('.row-total').textContent.trim() === '4:15');
const dayTotals = [...window.document.querySelectorAll('#week-table tfoot .day-total')].map((t) => t.textContent.trim());
check('day totals row renders H:MM per day', dayTotals.length === 7 && dayTotals[2] === '4:15' && dayTotals.filter((t) => t !== '4:15').every((t) => t === '0'));
check('week total footer is H:MM', window.document.querySelector('#week-table tfoot .week-total').textContent.trim() === '4:15');

// Inline save: type into the empty 2026-11-09 cell and blur -> POST.
const emptyCell = cellOf('2026-11-09');
emptyCell.value = '2.5';
emptyCell.dispatchEvent(new window.Event('focusout', { bubbles: true }));
await waitFor(() => window.document.querySelector('#week-table tbody tr .row-total').textContent.trim() === '6:45', {
  message: 'the inline cell save to update the week row total to 6:45',
});
const sep9 = await (await fetch(BASE + '/api/entries?date=2026-11-09')).json();
check('inline cell save created an entry via POST', sep9.entries.some((e) => e.hours === 2.5 && e.project_code === 'P-9'));
check('row total updated to 6:45 after inline save', window.document.querySelector('#week-table tbody tr .row-total').textContent.trim() === '6:45');

// Clear-to-delete: blank the same cell and blur -> confirm + DELETE.
const savedCell = cellOf('2026-11-09');
check('cleared cell re-rendered with the saved value', savedCell && savedCell.value === '2.50');
savedCell.value = '';
savedCell.dispatchEvent(new window.Event('focusout', { bubbles: true }));
await waitFor(() => byId('app-dialog').open === true || byId('app-dialog').hasAttribute('open'), {
  message: 'clearing a cell to ask for confirmation in the dialog',
});
check(
  'clearing a cell asks for confirmation in the dialog',
  window.document.getElementById('app-dialog').open === true
    || window.document.getElementById('app-dialog').hasAttribute('open'),
);
window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
await settle('the cleared cell to delete through the API');
const sep9b = await (await fetch(BASE + '/api/entries?date=2026-11-09')).json();
check('clearing the cell deleted the entry', sep9b.entries.every((e) => e.hours !== 2.5));

// #126: the note affordance edits IN the grid — dialog opens, cancel is safe.
const flag = window.document.querySelector('#week-table .note-flag');
check('note affordance shown on entry cells', !!flag);
flag.dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => byId('app-dialog').open === true || byId('app-dialog').hasAttribute('open'), {
  message: 'the note editor to open in the grid',
});
const noteDlgOpen = window.document.getElementById('app-dialog').open === true
  || window.document.getElementById('app-dialog').hasAttribute('open');
check('note editor opens in the grid (#126)', noteDlgOpen);
window.document.getElementById('dlg-cancel').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => /Note unchanged/i.test(byId('live-region').textContent), {
  message: 'the cancelled note edit to announce the note is unchanged',
});
check('cancel leaves the note unchanged (#126)',
  /Note unchanged/i.test(window.document.getElementById('live-region').textContent));
window.document.getElementById('ts-week').dispatchEvent(new window.Event('click', { bubbles: true }));

// #172 follow-up: "+ Add row" is back as a BUTTON that opens the
// customer/project/task dialog — the old inline pickers stay deleted.
check('Week + Add row is a dialog button with no inline pickers (#172 follow-up)',
  window.document.getElementById('week-add-row')?.tagName === 'BUTTON'
  && window.document.getElementById('week-row-dialog')?.classList.contains('p-modal')
  && ['week-add-project', 'week-add-task', 'week-add-confirm', 'week-add-cancel']
    .every((id) => !window.document.getElementById(id)));
check('copy previous week is a button rather than a dropdown',
  window.document.getElementById('week-copy-previous').tagName === 'BUTTON'
  && !window.document.getElementById('week-copy-weeks'));

// ---- PROJECT FORM: prefill from customer + required currency/rate (#11) ----
const tabCust = window.document.getElementById('tab-customers');
tabCust.dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !byId('panel-customers').hidden, { message: 'the Customers section to show' });
// #182: three Setup sections replace the combined workspace.
check('Setup nav exposes Customers, Projects and Tasks sections',
  ['tab-customers', 'tab-projects', 'tab-tasks'].every((id) => window.document.getElementById(id)));
check('projects section explains how to begin with an accessible empty state',
  !window.document.getElementById('projects-empty-state').hidden);
check('each section carries its own creation toolbar',
  window.document.querySelectorAll('#panel-customers .hierarchy-actions button').length === 1
  && window.document.querySelectorAll('#panel-projects .hierarchy-actions button').length === 1
  && window.document.querySelectorAll('#panel-tasks .hierarchy-actions button').length === 1);
// #182 follow-up: the create action leads the section (top-left, alone in the
// header) and the customer filter sits below the header where the old
// "Projects - all customers" caption was, still naming the select.
check('Projects leads with + Project and puts the customer filter under the header',
  window.document.querySelector('#panel-projects .ts-head button')?.id === 'project-new'
  && !window.document.querySelector('#panel-projects .hierarchy-actions .filter-field')
  && window.document.querySelector('#panel-projects .hierarchy-filters')?.nextElementSibling?.id === 'projects-empty-state'
  && /customer/i.test(window.document.querySelector('#panel-projects label[for="proj-filter-customer"]')?.textContent || '')
  && window.document.getElementById('project-scope-label') === null);
// #182 follow-up: Tasks gets the same treatment - the action leads the header
// and both dependent filters sit below it in place of the old scope caption.
check('Tasks leads with + Task and puts both dependent filters under the header',
  window.document.querySelector('#panel-tasks .ts-head button')?.id === 'task-new'
  && !window.document.querySelector('#panel-tasks .hierarchy-actions .filter-field')
  && window.document.querySelectorAll('#panel-tasks .hierarchy-filters .filter-field').length === 2
  && window.document.querySelector('#panel-tasks .hierarchy-filters')?.nextElementSibling?.id === 'tasks-empty-state'
  && /customer/i.test(window.document.querySelector('#panel-tasks label[for="task-filter-customer"]')?.textContent || '')
  && /project/i.test(window.document.querySelector('#panel-tasks label[for="task-filter-project"]')?.textContent || '')
  && window.document.getElementById('task-scope-label') === null);
// Every register's creation action sits ABOVE its card, in the same heading
// slot Timesheets gives Track time — not inside the card background.
check('register actions sit above the card, like Timesheets',
  ['#panel-customers', '#panel-projects', '#panel-tasks', '#panel-expenses'].every((panel) => {
    const row = window.document.querySelector(`${panel} .ts-head`);
    const button = row && row.querySelector('button');
    return !!button && !button.closest('.p-card')
      && row.nextElementSibling?.classList.contains('p-card');
  }));
// Heading-slot cleanup: no duplicate section H2 under the masthead H1, and
// the create action is the panel's first interactive element.
check('panels lead with their action, not a duplicate H2',
  window.document.querySelector('#panel-customers h2') === null
  && window.document.querySelector('#panel-reports h2') === null
  && window.document.querySelector('#panel-expenses .ts-head button').id === 'expense-new'
  && window.document.querySelector('#panel-settings > .settings-head h2') === null);
// Click "Projects" on the ACME row: cross-navigation filters the Projects
// section (#182) — no shared selection state anymore.
const projBtn = [...window.document.querySelectorAll('#customer-table button')].find((b) => b.textContent === 'Projects');
projBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
// The checks below read the *rendered* scoped list (scope label, empty state and
// rows). Waiting only for the panel and the picker raced the table re-render the
// click starts — the recurring gui-test flake (#265), green only when the runner
// happened to be fast. Await the post-condition the checks actually read.
const scopedProjectRows = () => [...window.document.querySelectorAll('#project-table tbody tr')];
// #182: the filter select's value IS the scope - the old scope caption is gone,
// so its reappearance would be a regression.
const projectsScopedToAcme = () => !byId('panel-projects').hidden
  && byId('proj-filter-customer').value === acmeOpt.value
  && byId('project-scope-label') === null
  && byId('projects-empty-state').hidden
  && scopedProjectRows().length >= 1
  && scopedProjectRows().every((r) => r.cells[1].textContent === 'ACME');
await waitFor(projectsScopedToAcme, {
  message: 'the Projects section to render rows scoped to ACME',
});
check('row action opens the Projects section scoped to that customer',
  !window.document.getElementById('panel-projects').hidden
  && window.document.getElementById('proj-filter-customer').value === acmeOpt.value
  && window.document.getElementById('project-scope-label') === null);
check('scoped list renders that customer only (empty state replaced by rows)',
  window.document.getElementById('projects-empty-state').hidden
  && [...window.document.querySelectorAll('#project-table tbody tr')].every((r) => r.cells[1].textContent === 'ACME'));
const projectDialog = window.document.getElementById('project-dialog');
window.document.getElementById('project-new').click();
check('+ Project opens its popup with selected customer context',
  projectDialog.open && window.document.getElementById('project-dialog-parent').textContent === 'ACME'
  && window.document.activeElement.id === 'project-code');
const projectParent = window.document.getElementById('project-parent-customer');
check('project popup preselects its customer dropdown', projectParent.value === acmeOpt.value && !projectParent.disabled);
projectParent.value = globex.id;
projectParent.dispatchEvent(new window.Event('change', { bubbles: true }));
check('changing project customer prefills its currency and rate',
  window.document.getElementById('project-currency').value === 'USD'
  && window.document.getElementById('project-rate').value === '45.50');
projectParent.value = acmeOpt.value;
projectParent.dispatchEvent(new window.Event('change', { bubbles: true }));
check('project currency prefilled from customer', window.document.getElementById('project-currency').value === 'EUR');
check('project rate prefilled from customer default', window.document.getElementById('project-rate').value === '60.00');
window.document.getElementById('project-currency').value = '';
window.document.getElementById('project-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
check('invalid project keeps its popup open with an error',
  projectDialog.open && !window.document.getElementById('project-error').hidden);
window.document.getElementById('project-cancel').click();
check('project Cancel closes without saving', !projectDialog.open);
window.document.getElementById('project-new').click();
// Create a project through the form with an override rate.
window.document.getElementById('project-code').value = 'mkt-2';
window.document.getElementById('project-rate').value = '95.00';
window.document.getElementById('project-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the project form to save MKT-2');
const projs = (await (await fetch(BASE + `/api/customers/${acmeOpt.value}/projects`)).json()).projects;
const mkt = projs.find((p) => p.code === 'MKT-2');
check('project form created MKT-2 with prefilled EUR + override rate', mkt && mkt.currency === 'EUR' && mkt.rate_minor === 9500);
check('project save closes popup and restores toolbar focus',
  !projectDialog.open && window.document.activeElement.id === 'project-new');

// ---- TASK MANAGER (#38): add a task to MKT-2, then log time against it ----
// settle() drains the POST the form started, but the table rebuild is the *next*
// request: await the row this block edits, not merely a quiet moment (#265).
await waitFor(() => [...window.document.querySelectorAll('#project-table tbody tr')]
  .some((r) => r.cells[0].textContent === 'MKT-2'), {
  message: 'the Projects table to render the saved MKT-2 row',
});
const projRows = [...window.document.querySelectorAll('#project-table tbody tr')];
const mktRow = projRows.find((r) => r.cells[0].textContent === 'MKT-2');
const projectEdit = [...mktRow.querySelectorAll('button')].find((button) => button.textContent === 'Edit');
projectEdit.click();
check('project Edit opens its populated popup',
  projectDialog.open && window.document.getElementById('project-code').value === 'MKT-2'
  && window.document.getElementById('project-dialog-title').textContent === 'Edit project');
check('editing a project locks its parent customer', projectParent.disabled && projectParent.value === acmeOpt.value);
projectDialog.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
check('project Escape closes its popup', !projectDialog.open);
const tasksBtn = [...mktRow.querySelectorAll('button')].find((b) => b.textContent === 'Tasks');
tasksBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
// Same weak-predicate shape as the Projects action above: the checks read the
// rendered scoped state (both pickers, the scope label, the empty state), so
// await that render rather than the panel plus one picker.
const tasksScopedToMkt2 = () => !byId('panel-tasks').hidden
  && byId('task-filter-customer').value === acmeOpt.value
  && byId('task-filter-project').value === 'MKT-2'
  && byId('task-scope-label') === null
  && !byId('tasks-empty-state').hidden;
await waitFor(tasksScopedToMkt2, {
  message: 'the Tasks section to render scoped to ACME / MKT-2',
});
check('project row Tasks action opens the Tasks section with both filters set',
  !window.document.getElementById('panel-tasks').hidden
  && window.document.getElementById('task-filter-customer').value === acmeOpt.value
  && window.document.getElementById('task-filter-project').value === 'MKT-2'
  && window.document.getElementById('task-scope-label') === null);
{ // #182-follow-up: both task filters offer an "all" scope, so clearing the
  // customer widens the list instead of blocking it.
  const custSel = window.document.getElementById('task-filter-customer');
  custSel.value = '';
  custSel.dispatchEvent(new window.Event('change', { bubbles: true }));
  const projSel = window.document.getElementById('task-filter-project');
  await waitFor(() => projSel.value === '' && projSel.options[0].textContent === 'All projects'
    && /all customers and projects/i.test(byId('live-region').textContent), {
    message: 'clearing the customer to widen the task list to every customer',
  });
  check('clearing the customer widens the list and keeps the project pick usable',
    projSel.value === '' && projSel.disabled === false
    && custSel.options[0].textContent === 'All customers'
    && projSel.options[0].textContent === 'All projects'
    && /all customers and projects/i.test(byId('live-region').textContent)
    && !byId('tasks-empty-state').hidden
    && /No tasks yet/.test(byId('tasks-empty-state').textContent));
  custSel.value = acmeOpt.value;
  custSel.dispatchEvent(new window.Event('change', { bubbles: true }));
  await waitFor(() => [...byId('task-filter-project').options].some((o) => o.value === 'MKT-2'), {
    message: 're-selecting the customer to repopulate its project choices',
  });
  projSel.value = 'MKT-2';
  projSel.dispatchEvent(new window.Event('change', { bubbles: true }));
  await waitFor(() => /No tasks on this project yet/.test(byId('tasks-empty-state').textContent), {
    message: 'the scoped task list to render its empty state',
  });
  check('re-selecting the scoped project restores its scoped state',
    !window.document.getElementById('tasks-empty-state').hidden
    && /No tasks on this project yet/.test(window.document.getElementById('tasks-empty-state').textContent)
    && window.document.getElementById('task-scope-label') === null);
}
const taskDialog = window.document.getElementById('task-dialog');
window.document.getElementById('task-new').click();
await waitFor(() => dialogOpen(taskDialog) && byId('task-dialog-parent').textContent === 'ACME / MKT-2', {
  message: 'the task popup to open with its customer and project path',
});
check('+ Task opens its popup with the customer/project path',
  taskDialog.open && window.document.getElementById('task-dialog-parent').textContent === 'ACME / MKT-2'
  && window.document.activeElement.id === 'task-code');
const taskCustomer = window.document.getElementById('task-parent-customer');
const taskProject = window.document.getElementById('task-parent-project');
check('task popup preselects customer and project dropdowns',
  taskCustomer.value === acmeOpt.value && taskProject.value === 'MKT-2');
taskCustomer.value = globex.id;
taskCustomer.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => ![...taskProject.options].some((option) => option.value === 'MKT-2'), {
  message: 'changing the task customer to drop the unrelated project option',
});
check('changing task customer clears unrelated project options',
  ![...taskProject.options].some((option) => option.value === 'MKT-2'));
taskCustomer.value = acmeOpt.value;
taskCustomer.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => [...taskProject.options].some((option) => option.value === 'MKT-2'), {
  message: 're-selecting ACME to restore the MKT-2 project option',
});
taskProject.value = 'MKT-2';
window.document.getElementById('task-code').value = 't1';
window.document.getElementById('task-name').value = 'Sprint';
check('task popup has no rate or currency override controls',
  !window.document.getElementById('task-rate') && !window.document.getElementById('task-currency'));
window.document.getElementById('task-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the task form to save T1');
const tasks = (await (await fetch(BASE + `/api/customers/${acmeOpt.value}/projects/MKT-2/tasks`)).json()).tasks;
const t1 = tasks.find((t) => t.code === 'T1');
check('task created without billing override fields',
  t1 && !('rate_minor' in t1) && !('currency' in t1));
check('task save closes its popup', !taskDialog.open);
const taskEdit = [...window.document.querySelectorAll('#task-table tbody button')].find((button) => button.textContent === 'Edit');
taskEdit.click();
await waitFor(() => taskDialog.open && byId('task-code').value === 'T1', {
  message: 'task Edit to open its populated popup',
});
check('task Edit opens its populated popup',
  taskDialog.open && window.document.getElementById('task-code').value === 'T1');
check('editing a task locks its parent dropdowns', taskCustomer.disabled && taskProject.disabled);
taskDialog.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
check('task Escape closes its popup', !taskDialog.open);

// Entry form: choose MKT-2 -> task picker loads T1 -> log 1h against it.
const custSel2 = window.document.getElementById('entry-customer');
custSel2.value = acmeOpt.value;
custSel2.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => [...byId('entry-project').options].some((o) => o.value === 'MKT-2'), {
  message: 'the entry project picker to load MKT-2',
});
const projSel = window.document.getElementById('entry-project');
projSel.value = 'MKT-2';
projSel.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => [...byId('entry-task').options].some((o) => o.value === 'T1'), {
  message: 'the entry task picker to load T1',
});
const taskSel = window.document.getElementById('entry-task');
check('entry task picker loaded T1', [...taskSel.options].some((o) => o.value === 'T1'));
taskSel.value = 'T1';
window.document.getElementById('entry-date').value = '2026-12-01';
window.document.getElementById('entry-hours').value = '1';
window.document.getElementById('entry-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the task-based entry to save');
const dec = (await (await fetch(BASE + '/api/entries?date=2026-12-01')).json()).entries;
check('entry logged against task T1', dec.some((e) => e.task_code === 'T1'));

// ---- INVOICES (#8): generate a draft for ACME's billable work, then issue ----
const tabInv = window.document.getElementById('tab-invoices');
tabInv.dispatchEvent(new window.Event('click', { bubbles: true }));
window.document.getElementById('invoice-customer').value = acmeOpt.value;
window.document.getElementById('invoice-from').value = '2026-12-01';
window.document.getElementById('invoice-to').value = '2026-12-31';
window.document.getElementById('invoice-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the invoice form to generate the ACME draft');
const invs = (await (await fetch(BASE + '/api/invoices')).json()).invoices;
const acmeInv = invs.find((i) => i.customer_id === acmeOpt.value);
check('draft invoice generated', !!acmeInv && acmeInv.status === 'draft');
check('invoice total reflects billable work (MKT-2 1h x 95 = 95.00)', acmeInv && acmeInv.total_minor === 9500);
// Issue it via the API and confirm the invoiced entry is now locked.
const issueRes = await fetch(`${BASE}/api/invoices/${acmeInv.id}/issue`, { method: 'POST', headers: { Cookie: SESSION_COOKIE } });
check('invoice issues', issueRes.status === 200);
const invoicedEntryId = acmeInv.lines[0].entry_id;
const editRes = await fetch(`${BASE}/api/entries/${invoicedEntryId}`, {
  method: 'PUT',
  headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE },
  body: JSON.stringify({ date: '2026-12-01', customer_id: acmeOpt.value, project_code: 'MKT-2', hours: 1 }),
});
check('issuing locks the invoiced entry (edit -> 409)', editRes.status === 409);

// ---- INVOICE PDF (#113): archived at issue, downloadable, hinted in JSON ----
const issuedBody = await issueRes.clone().json();
check('issue persists the pdf hint', issuedBody.pdf && /^INV-\d+\.pdf$/.test(issuedBody.pdf.filename) && issuedBody.pdf.sha256.length === 64 && issuedBody.pdf.bytes > 100);
const pdfRes = await fetch(`${BASE}/api/invoices/${acmeInv.id}/pdf`, { headers: { Cookie: SESSION_COOKIE } });
const pdfHead = new Uint8Array(await pdfRes.arrayBuffer().then((b) => b.slice(0, 8)));
check(
  'GET /invoices/{id}/pdf serves the archived PDF',
  pdfRes.status === 200
    && (pdfRes.headers.get('content-type') || '').includes('application/pdf')
    && String.fromCharCode(...pdfHead) === '%PDF-1.4',
);
check('PDF download names the file by invoice number', /filename="INV-\d+\.pdf"/.test(pdfRes.headers.get('content-disposition') || ''));

// ---- INVOICE PREVIEW (#133): row selection shows the live document ----
{
  // Re-pull the table so the rows reflect the API-side issue (the invoice was
  // issued over fetch, not through the UI).
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => [...window.document.querySelectorAll('#invoice-table tbody button')]
    .some((b) => b.textContent === 'Preview' && b.closest('tr').textContent.includes(acmeInv.number)), {
    message: 'the issued invoice row to offer Preview',
  });
  const pvBtn = [...window.document.querySelectorAll('#invoice-table tbody button')]
    .find((b) => b.textContent === 'Preview' && b.closest('tr').textContent.includes(acmeInv.number));
  check('invoice rows offer a Preview action (#133)', !!pvBtn);
  pvBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('invoice-preview').hidden === false && byId('ip-subject').textContent.includes(acmeInv.number), {
    message: 'the preview panel to render the issued invoice document',
  });
  const panel = window.document.getElementById('invoice-preview');
  check('preview panel opens on selection (#133)', panel.hidden === false);
  const facts = window.document.getElementById('ip-facts').textContent;
  check('facts name number, total and balance (#133)',
    facts.includes(acmeInv.number) && facts.includes('Total') && facts.includes('Balance'));
  check('subject resolves from /document (#133)',
    window.document.getElementById('ip-subject').textContent.includes(acmeInv.number));
  check('letter is textContent-only, never injected markup (#133)',
    window.document.getElementById('ip-letter').children.length === 0);
  check('issued invoice offers Download PDF from the preview (#133)',
    window.document.getElementById('ip-download').hidden === false);
  panel.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  await waitFor(() => panel.hidden === true, { message: 'Escape to close the preview' });
  check('Escape closes the preview (#133)', panel.hidden === true);
  await showAllInvoices(); // #135 default Open tab hides drafts
  // A draft says "no document yet" instead of offering a 409 download.
  await fetch(BASE + '/api/entries', {
    method: 'POST',
    headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' },
    body: JSON.stringify({ date: '2029-01-05', customer_id: acmeOpt.value, project_code: 'P-9', hours: 1 }),
  });
  const draft = await (await fetch(BASE + '/api/invoices', {
    method: 'POST',
    headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' },
    body: JSON.stringify({ customer_id: acmeOpt.value, from: '2029-01-01', to: '2029-01-31' }),
  })).json();
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => [...window.document.querySelectorAll('#invoice-table tbody button')]
    .some((b) => b.textContent === 'Preview' && b.closest('tr').textContent.includes(draft.number)), {
    message: 'the draft invoice row to offer Preview',
  });
  const dBtn = [...window.document.querySelectorAll('#invoice-table tbody button')]
    .find((b) => b.textContent === 'Preview' && b.closest('tr').textContent.includes(draft.number));
  dBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('ip-download').hidden === true && byId('ip-pdf-state').hidden === false, {
    message: 'the draft preview to explain the missing archive',
  });
  check('draft preview explains the missing archive, hides Download (#133)',
    window.document.getElementById('ip-download').hidden === true
    && window.document.getElementById('ip-pdf-state').hidden === false
    && /Draft/.test(window.document.getElementById('ip-pdf-state').textContent));
  // cleanup: delete the draft so later checks see the expected list
  await fetch(BASE + `/api/invoices/${draft.id}`, { method: 'DELETE', headers: { Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' } });
}

// ---- CALENDAR VIEW (#140): month grid of logged time ----
{
  await fetch(BASE + '/api/entries', {
    method: 'POST',
    headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' },
    body: JSON.stringify({ date: '2026-12-25', customer_id: acmeOpt.value, project_code: 'P-9', hours: 3.5, note: 'xmas call' }),
  });
  window.document.getElementById('ts-cal').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('panel-calendar').hidden === false, { message: 'the calendar month grid to show' });
  const calPanel = window.document.getElementById('panel-calendar');
  check('calendar tab shows the month grid (#140)', calPanel.hidden === false);
  // Navigate to Dec 2026 (initial month follows the displayed day; walk forward).
  let guard = 0;
  while (!window.document.getElementById('cal-label').textContent.includes('Dec 2026') && guard < 24) {
    const label = window.document.getElementById('cal-label').textContent;
    window.document.getElementById('cal-next').dispatchEvent(new window.Event('click', { bubbles: true }));
    await waitFor(() => window.document.getElementById('cal-label').textContent !== label, {
      message: 'the calendar to render the next month',
    });
    guard += 1;
  }
  check('month navigation reaches Dec 2026 (#140)',
    window.document.getElementById('cal-label').textContent.includes('Dec 2026'));
  await waitFor(() => [...calPanel.querySelectorAll('td.cal-cell.has button')]
    .some((b) => (b.getAttribute('aria-label') || '').includes('25 Dec 2026')), {
    message: 'the Dec 2026 grid to render the 25 Dec entry',
  });
  const xmas = [...calPanel.querySelectorAll('td.cal-cell.has button')]
    .find((b) => (b.getAttribute('aria-label') || '').includes('25 Dec 2026'));
  check('day cell shows the day total, not the entries (#140)',
    !!xmas && xmas.querySelector('.cal-hours')?.textContent.trim() === '3:30'
    && xmas.querySelectorAll('.cal-chip, .cal-entry').length === 0);
  check('the month readout is a labelled "Total hours" value (#140)',
    /^\d+:\d{2}$/.test(window.document.getElementById('cal-total').textContent)
    && window.document.querySelector('.cal-total-label')?.textContent.trim() === 'Total hours');
  if (xmas) {
    xmas.dispatchEvent(new window.Event('click', { bubbles: true }));
    await waitFor(() => byId('ts-day').getAttribute('aria-selected') === 'true'
      && byId('day-date').value === '2026-12-25'
      && /Opened 2026-12-25/.test(byId('live-region').textContent), {
      message: 'the clicked calendar day to hand over to the Day view',
    });
    check('clicking a day hands over to the Day view (#140)',
      window.document.getElementById('ts-day').getAttribute('aria-selected') === 'true'
      && window.document.getElementById('day-date').value === '2026-12-25'
      && /Opened 2026-12-25/.test(window.document.getElementById('live-region').textContent));
  }
  const empty = [...calPanel.querySelectorAll('td.cal-cell:not(.has) .cal-plain')].length;
  check('entry-less days render plain (#140)', empty > 20);
  const curMonth = new Date().toISOString().slice(0, 7);
  const curMonthName = ['Jan','Feb','Mar','Apr','May','Jun','Jul','Aug','Sep','Oct','Nov','Dec'][Number(curMonth.slice(5,7)) - 1];
  check('the month bar mirrors the Day/Week bar and the Today button is gone (#140)',
    window.document.getElementById('cal-month').parentElement.className.includes('day-end')
    && window.document.getElementById('cal-label').parentElement.className.includes('day-nav')
    && !window.document.getElementById('cal-today'));
  window.document.getElementById('cal-month').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('cal-label').textContent.includes(curMonthName), {
    message: `Return to this month to jump back to the current month (${curMonthName})`,
  });
  check('Return to this month jumps to the current month (#140)',
    window.document.getElementById('cal-label').textContent.includes(curMonthName));
  // #231: with the Calendar segment active, leave Timesheets via the sidebar rail.
  // The panel sat *outside* panel-timesheet once, so nothing hid it and the month
  // grid stayed visible on every section. jsdom's `hidden` does not inherit, so
  // assert both halves: the panel nests inside the section, and navigation hides
  // the section.
  window.document.getElementById('ts-cal').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => !byId('panel-calendar').hidden && byId('panel-calendar').closest('#panel-timesheet') !== null, {
    message: 'the calendar segment panel to nest inside the timesheet section',
  });
  check('calendar segment panel nests inside the timesheet section (#231)',
    window.document.getElementById('panel-calendar').closest('#panel-timesheet') !== null
    && window.document.getElementById('panel-calendar').hidden === false);
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('panel-timesheet').hidden === true, {
    message: 'navigating away to hide the timesheet section',
  });
  check('navigating away hides the timesheet section that contains the calendar (#231)',
    window.document.getElementById('panel-timesheet').hidden === true);
  window.document.getElementById('tab-timesheet').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => !byId('panel-timesheet').hidden && !byId('panel-calendar').hidden
    && byId('ts-cal').getAttribute('aria-selected') === 'true', {
    message: 'returning to Timesheets to keep the calendar segment selected',
  });
  check('returning to Timesheets keeps the calendar segment selected and shown (#231)',
    window.document.getElementById('panel-timesheet').hidden === false
    && window.document.getElementById('panel-calendar').hidden === false
    && window.document.getElementById('ts-cal').getAttribute('aria-selected') === 'true');
  window.document.getElementById('ts-week').dispatchEvent(new window.Event('click', { bubbles: true }));
}

// ---- FIELD LABELS + CATALOG (#147) ----
{
  window.document.getElementById('tab-settings').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => [...window.document.querySelectorAll('#settings-tabs [role="tab"]')].map((tab) => tab.id).join(',') ===
    'settings-tab-security,settings-tab-users,settings-tab-invoice,settings-tab-catalog,settings-tab-expenses', {
    message: 'the Settings section to render its tab bar',
  });
  check('Settings tab bar includes Users, Security, Invoice documents, and Products & services',
    [...window.document.querySelectorAll('#settings-tabs [role="tab"]')].map((tab) => tab.id).join(',') ===
    'settings-tab-security,settings-tab-users,settings-tab-invoice,settings-tab-catalog,settings-tab-expenses');
  check('Settings opens on Security & runtime by default',
    window.document.getElementById('settings-tab-security').getAttribute('aria-selected') === 'true'
    && !window.document.getElementById('settings-panel-security').hidden);
  window.document.getElementById('settings-tab-users').dispatchEvent(new window.Event('click', { bubbles: true }));
  check('Users tab shows the admin-management table',
    window.document.getElementById('settings-tab-users').getAttribute('aria-selected') === 'true'
    && !window.document.getElementById('settings-panel-users').hidden
    && window.document.querySelector('#user-table tbody tr') !== null);
  const userDialog = window.document.getElementById('user-dialog');
  window.document.getElementById('user-new').click();
  check('+ User opens a popup and focuses Name', userDialog.open && window.document.activeElement.id === 'user-name');
  userDialog.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  check('user Escape closes without saving', !userDialog.open);
  window.document.getElementById('user-new').click();
  window.document.getElementById('user-name').value = 'Managed QA';
  window.document.getElementById('user-email').value = 'managed-qa@test.local';
  window.document.getElementById('user-password').value = 'managed-password1';
  window.document.getElementById('user-default-rate').value = '45.67';
  window.document.getElementById('user-cost-rate').value = '12.34';
  window.document.getElementById('user-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await settle('the user form to save the new user');
  const usersResult = await (await fetch(BASE + '/api/users')).json();
  const managed = usersResult.users.find((user) => user.email === 'managed-qa@test.local');
  check('user popup creates exact minor-unit rates and closes',
    managed && managed.default_rate_minor === 4567 && managed.cost_rate_minor === 1234 && !userDialog.open);
  check('user responses never expose password fields', !JSON.stringify(usersResult).includes('password'));
  const userSearch = window.document.getElementById('user-search');
  userSearch.value = 'managed-qa';
  userSearch.dispatchEvent(new window.Event('input', { bubbles: true }));
  check('user name/email search narrows the list', window.document.querySelectorAll('#user-table tbody tr').length === 1);
  const userRoleFilter = window.document.getElementById('user-filter-role');
  userRoleFilter.value = 'admin';
  userRoleFilter.dispatchEvent(new window.Event('change', { bubbles: true }));
  check('user role filter has an honest empty state', !window.document.getElementById('user-empty').hidden);
  userRoleFilter.value = '';
  userRoleFilter.dispatchEvent(new window.Event('change', { bubbles: true }));
  const userAction = (text) => [...window.document.querySelectorAll('#user-table tbody button')].find((button) => button.textContent === text);
  userAction('Edit').click();
  check('user Edit hides and clears the password field', userDialog.open && window.document.getElementById('user-password-field').hidden && !window.document.getElementById('user-password').value);
  window.document.getElementById('user-name').value = 'Managed Updated';
  window.document.getElementById('user-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await waitFor(() => !userDialog.open && window.document.querySelector('#user-table tbody').textContent.includes('Managed Updated'), {
    message: 'the user edit to save, close the popup and re-render the table',
  });
  check('user edit saves and closes', !userDialog.open && window.document.querySelector('#user-table tbody').textContent.includes('Managed Updated'));
  userAction('Change password').click();
  const passwordDialog = window.document.getElementById('user-password-dialog');
  const newPassword = window.document.getElementById('user-new-password');
  check('password change uses an empty masked popup', passwordDialog.open && newPassword.type === 'password' && !newPassword.value);
  newPassword.value = 'short';
  window.document.getElementById('user-password-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await waitFor(() => passwordDialog.open && !byId('user-password-error').hidden, {
    message: 'the short password to keep the popup open with its error',
  });
  check('invalid password stays open with an error', passwordDialog.open && !window.document.getElementById('user-password-error').hidden);
  newPassword.value = 'managed-new-password1';
  window.document.getElementById('user-password-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await waitFor(() => !passwordDialog.open && !newPassword.value, {
    message: 'the successful password change to close and clear the secret',
  });
  check('successful password change closes and clears secret', !passwordDialog.open && !newPassword.value);
  userAction('Deactivate').click();
  await settle('the deactivation to reach the server and re-render the user table');
  const userStatusFilter = window.document.getElementById('user-filter-active');
  userStatusFilter.value = 'false';
  userStatusFilter.dispatchEvent(new window.Event('change', { bubbles: true }));
  // The filter re-renders asynchronously (#265): wait for the deactivated user
  // to appear instead of reading the pre-deactivation DOM, which passed only by
  // luck and crashed (userAction('Activate') undefined) on a loaded runner.
  await waitFor(() => !!userAction('Activate'), {
    message: 'the inactive filter to re-render with the deactivated user',
  });
  check('inactive filter shows deactivated user', !!userAction('Activate'));
  userAction('Activate').click();
  await waitFor(() => !window.document.getElementById('user-empty').hidden, {
    message: 'the inactive results to empty after reactivation',
  });
  check('reactivation removes user from inactive results', !window.document.getElementById('user-empty').hidden);
  userStatusFilter.value = '';
  userStatusFilter.dispatchEvent(new window.Event('change', { bubbles: true }));
  // Same race (#265): the all-status re-render must land before Delete acts.
  await waitFor(() => !!userAction('Delete'), {
    message: 'the all-status re-render to offer the Delete action',
  });
  userAction('Delete').click();
  await waitFor(() => dialogOpen(byId('app-dialog')), { message: 'the delete-user confirm dialog to open' });
  window.document.getElementById('dlg-ok').click();
  await waitFor(() => !window.document.getElementById('user-empty').hidden, {
    message: 'the confirmed deletion to remove the unused user',
  });
  check('confirmed deletion removes unused user', !window.document.getElementById('user-empty').hidden);
  userSearch.value = '';
  userSearch.dispatchEvent(new window.Event('input', { bubbles: true }));
  window.document.getElementById('settings-tab-catalog').dispatchEvent(new window.Event('click', { bubbles: true }));
  check('Products & services tab shows the catalog only',
    window.document.getElementById('settings-tab-catalog').getAttribute('aria-selected') === 'true'
    && !window.document.getElementById('settings-panel-catalog').hidden
    && window.document.getElementById('settings-panel-invoice').hidden);
  window.document.getElementById('item-name').value = 'Site visit';
  window.document.getElementById('item-kind').value = 'service';
  window.document.getElementById('item-desc').value = 'On-site engineering visit';
  window.document.getElementById('item-price').value = '120.00';
  window.document.getElementById('item-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await settle('the catalog item form to save Site visit');
  const cat = (await (await fetch(BASE + '/api/admin/item-types', { headers: { Cookie: SESSION_COOKIE } })).json()).item_types;
  check('catalog item created via the form (#147)', cat.length === 1 && cat[0].name === 'Site visit' && cat[0].default_price_minor === 12000);
  window.document.getElementById('settings-tab-invoice').dispatchEvent(new window.Event('click', { bubbles: true }));
  check('Invoice documents tab shows template and field-label settings',
    window.document.getElementById('settings-tab-invoice').getAttribute('aria-selected') === 'true'
    && !window.document.getElementById('settings-panel-invoice').hidden
    && window.document.getElementById('settings-panel-catalog').hidden);
  window.document.getElementById('lb-total').value = 'Amount due';
  window.document.getElementById('lb-description').value = 'Work performed';
  window.document.getElementById('template-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await settle('the invoice template to save its field labels');
  const tpl = (await (await fetch(BASE + '/api/admin/invoice-template', { headers: { Cookie: SESSION_COOKIE } })).json()).template;
  check('field labels persist with the template (#147)',
    tpl.labels.total === 'Amount due' && tpl.labels.description === 'Work performed');
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => !byId('panel-invoices').hidden, { message: 'the Invoices section to show' });
  window.document.getElementById('manual-new').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => window.document.querySelector('#ed-lines tbody tr') !== null, {
    message: 'the manual invoice editor to open with a line row',
  });
  const edRow = window.document.querySelector('#ed-lines tbody tr');
  const itemSel = edRow.querySelector('select');
  check('editor row offers catalog items (#147)', [...itemSel.options].some((o) => o.textContent.includes('Site visit')));
  itemSel.value = [...itemSel.options].find((o) => o.textContent.includes('Site visit')).value;
  itemSel.dispatchEvent(new window.Event('change', { bubbles: true }));
  await waitFor(() => edRow.querySelectorAll('input[type=number]')[1].value === '120.00', {
    message: 'the catalog selection to prefill the line price',
  });
  const nums = edRow.querySelectorAll('input[type=number]');
  check('catalog selection prefills description and price (#147)',
    edRow.querySelector('input[type=text]').value.includes('On-site') && nums[1].value === '120.00');
  window.document.getElementById('ed-close').dispatchEvent(new window.Event('click', { bubbles: true }));
}

// ---- MANUAL LINES + DRAFT EDITOR (#143) ----
{
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the Invoices section to reload before opening the manual editor');
  window.document.getElementById('invoice-customer').value = acmeOpt.value;
  window.document.getElementById('manual-new').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('invoice-editor').hidden === false && byId('invoice-editor').querySelectorAll('tbody tr').length === 1, {
    message: 'the manual editor to open with one line row',
  });
  const edPanel = window.document.getElementById('invoice-editor');
  check('manual editor opens with a line row (#143)', edPanel.hidden === false
    && edPanel.querySelectorAll('tbody tr').length === 1);
  const fill = (row, desc, kind, qty, price) => {
    row.querySelector('input[type=text]').value = desc;
    row.querySelectorAll('select')[1].value = kind; // [0] is the #147 catalog picker
    const nums = row.querySelectorAll('input[type=number]');
    nums[0].value = qty;
    nums[1].value = price;
    for (const elx of row.querySelectorAll('input, select')) elx.dispatchEvent(new window.Event('input', { bubbles: true }));
  };
  fill(edPanel.querySelector('tbody tr'), 'Office seat', 'product', '2.50', '40.00');
  window.document.getElementById('ed-add').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => edPanel.querySelectorAll('tbody tr').length === 2, {
    message: 'the editor to append a second line row',
  });
  fill([...edPanel.querySelectorAll('tbody tr')][1], 'Setup service', 'service', '1.00', '50.00');
  window.document.getElementById('ed-discount').value = '10';
  window.document.getElementById('ed-discount').dispatchEvent(new window.Event('input', { bubbles: true }));
  window.document.getElementById('ed-tax').value = '21';
  window.document.getElementById('ed-tax').dispatchEvent(new window.Event('input', { bubbles: true }));
  await waitFor(() => byId('ed-totals').textContent.includes('163.35'), {
    message: 'the editor totals to recompute with tax and discount',
  });
  const totalsTxt = window.document.getElementById('ed-totals').textContent;
  check('live totals use exact integer math (#143)',
    totalsTxt.includes('150.00') && totalsTxt.includes('-15.00') && totalsTxt.includes('28.35') && totalsTxt.includes('163.35'),
    totalsTxt);
  window.document.getElementById('ed-save').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the manual draft to save');
  const mInvs = (await (await fetch(BASE + '/api/invoices', { headers: { Cookie: SESSION_COOKIE } })).json()).invoices;
  const manual = mInvs.find((i) => i.total_minor === 16335 && i.status === 'draft');
  check('manual draft saved with server-computed totals (#143)', !!manual && manual.lines.length === 2
    && manual.lines[0].amount_minor === 10000 && manual.lines[1].item_kind === 'service');
  check('manual save announced and closed the editor (#143)',
    edPanel.hidden === true && /Draft INV-\d+ saved \(total 163\.35 EUR\)/.test(window.document.getElementById('live-region').textContent));
  check('manual draft locks nothing (entries untouched) (#143)',
    !(await (await fetch(BASE + `/api/invoices/${manual.id}/pdf`, { headers: { Cookie: SESSION_COOKIE } }))).ok);
  // Re-edit the draft through the Edit action and save unchanged.
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the invoice table to reload for the draft Edit action');
  const editBtn = [...window.document.querySelectorAll('#invoice-table tbody button')]
    .find((b) => b.textContent === 'Edit' && b.closest('tr').textContent.includes(manual.number));
  check('draft rows offer Edit (#143)', !!editBtn);
  editBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('ed-lines').querySelectorAll('tbody tr').length === 2, {
    message: 'the editor to reopen with the stored manual lines',
  });
  check('editor reopens with stored manual lines (#143)',
    window.document.getElementById('ed-lines').querySelectorAll('tbody tr').length === 2
    && [...window.document.querySelectorAll('#ed-lines tbody input[type=text]')].some((x) => x.value === 'Office seat'));
  window.document.getElementById('ed-save').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the idempotent re-save to finish');
  const after = await (await fetch(BASE + `/api/invoices/${manual.id}`, { headers: { Cookie: SESSION_COOKIE } })).json();
  check('idempotent re-save keeps totals (#143)', after.total_minor === 16335 && after.lines.length === 2);
}

// ---- TRACKED-WORK INVOICE WIZARD (#134) ----
{
  for (const [date, hours] of [['2027-03-03', 2], ['2027-03-04', 1]]) {
    await fetch(BASE + '/api/entries', {
      method: 'POST',
      headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' },
      body: JSON.stringify({ date, customer_id: acmeOpt.value, project_code: 'MKT-2', hours }),
    });
  }
  const invsBefore = (await (await fetch(BASE + '/api/invoices', { headers: { Cookie: SESSION_COOKIE } })).json()).invoices.length;
  window.document.getElementById('iw-customer').value = acmeOpt.value;
  window.document.getElementById('iw-next').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('iw-step2').hidden === false, {
    message: 'the invoice wizard to advance to step 2',
  });
  check('wizard advances to step 2 with period controls (#134)',
    window.document.getElementById('iw-step2').hidden === false
    && !!window.document.getElementById('iw-preset')
    && !!window.document.getElementById('iw-from'));
  window.document.getElementById('iw-preset').value = 'custom';
  window.document.getElementById('iw-preset').dispatchEvent(new window.Event('change', { bubbles: true }));
  window.document.getElementById('iw-from').value = '2027-03-01';
  window.document.getElementById('iw-from').dispatchEvent(new window.Event('change', { bubbles: true }));
  await settle('the wizard period to reload from the new start date');
  window.document.getElementById('iw-to').value = '2027-03-31';
  window.document.getElementById('iw-to').dispatchEvent(new window.Event('change', { bubbles: true }));
  await waitFor(() => !!byId('iw-p-MKT-2'), {
    message: 'the wizard to list the MKT-2 project for the period',
  });
  const cb = window.document.getElementById('iw-p-MKT-2');
  check('wizard finds the March work for MKT-2 (#134)', !!cb && /3\.00/.test(cb.closest('label').textContent));
  cb.checked = true;
  window.document.getElementById('iw-next').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('iw-step3').hidden === false && byId('iw-summary').textContent.includes('285.00'), {
    message: 'the wizard review step to show the grouped 285.00 total',
  });
  const review = window.document.getElementById('iw-summary').textContent + ' | ' + window.document.getElementById('iw-lines').textContent;
  check('review shows grouped lines and the 285.00 total (#134)',
    review.includes('285.00') && /MKT-2/.test(review));
  check('review is step 3 of the wizard (#134)',
    window.document.getElementById('iw-step3').hidden === false
    && window.document.getElementById('iw-save').hidden === false);
  const invsMid = (await (await fetch(BASE + '/api/invoices', { headers: { Cookie: SESSION_COOKIE } })).json()).invoices.length;
  check('preview persisted nothing before Save (#134)', invsMid === invsBefore);
  window.document.getElementById('iw-save').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('iw-step1').hidden === false, {
    message: 'the wizard to save the draft and reset to step 1',
  });
  const invsAfter = (await (await fetch(BASE + '/api/invoices', { headers: { Cookie: SESSION_COOKIE } })).json()).invoices;
  const wizardDraft = invsAfter.find((i) => i.total_minor === 28500 && i.status === 'draft');
  check('Save creates exactly one draft from the selection (#134)',
    invsAfter.length === invsBefore + 1 && !!wizardDraft && wizardDraft.lines.length === 2);
  check('draft save announced without issuing (#134)',
    /Draft INV-\d+ created.*Issue/.test(window.document.getElementById('live-region').textContent));
  // the wizard reset back to step 1
  check('wizard returns to step 1 after save (#134)',
    window.document.getElementById('iw-step1').hidden === false && window.document.getElementById('iw-title').textContent.includes('1 of 3'));
}

// ---- RETAINERS (#144): create, add, draw, history, guards ----
{
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the Invoices section to reload before creating the retainer');
  window.document.getElementById('ret-customer').value = acmeOpt.value;
  window.document.getElementById('ret-customer').dispatchEvent(new window.Event('change', { bubbles: true }));
  await waitFor(() => window.document.getElementById('ret-project').options.length > 1, {
    message: 'the retainer project picker to load the customer projects',
  });
  const projSel = window.document.getElementById('ret-project');
  projSel.value = projSel.options[projSel.options.length - 1].value; // P-9 (seeded by wizard earlier)
  window.document.getElementById('ret-opening').value = '500.00';
  window.document.getElementById('ret-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await waitFor(() => window.document.querySelector('#ret-table tbody tr') !== null, {
    message: 'the created retainer to render in the table',
  });
  let rets = (await (await fetch(BASE + '/api/retainers', { headers: { Cookie: SESSION_COOKIE } })).json()).retainers;
  check('retainer created with opening funds (#144)', rets.length === 1 && rets[0].balance_minor === 50000 && rets[0].status === 'open');
  const row = window.document.querySelector('#ret-table tbody tr');
  check('balance and status render from the ledger (#144)',
    row.textContent.includes('500.00') && /open/i.test(row.textContent));
  // Draw without a reason must cancel cleanly (no ledger growth).
  row.querySelectorAll('button').forEach((b) => {
    if (b.textContent === 'History') b.dispatchEvent(new window.Event('click', { bubbles: true }));
  });
  await waitFor(() => byId('ret-detail').hidden === false && /opening\s\+500\.00/.test(byId('ret-history').textContent), {
    message: 'the retainer history panel to show the opening entry',
  });
  check('history panel shows the opening entry (#144)',
    window.document.getElementById('ret-detail').hidden === false
    && /opening\s\+500\.00/.test(window.document.getElementById('ret-history').textContent));
  window.document.getElementById('ret-detail-close').dispatchEvent(new window.Event('click', { bubbles: true }));
  // Add funds via the dialog flow (amount step then confirm step of draw tested separately).
  row.querySelectorAll('button').forEach((b) => {
    if (b.textContent === 'Add funds') b.dispatchEvent(new window.Event('click', { bubbles: true }));
  });
  await waitFor(() => dialogOpen(byId('app-dialog')), { message: 'the add-funds prompt to open' });
  window.document.getElementById('dlg-input').value = '100.00';
  window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the add-funds dialog to commit its ledger entry');
  rets = (await (await fetch(BASE + '/api/retainers', { headers: { Cookie: SESSION_COOKIE } })).json()).retainers;
  check('add funds credits the ledger (#144)', rets[0].balance_minor === 60000);
  // Draw with a reason.
  const row2 = window.document.querySelector('#ret-table tbody tr');
  row2.querySelectorAll('button').forEach((b) => {
    if (b.textContent === 'Draw funds') b.dispatchEvent(new window.Event('click', { bubbles: true }));
  });
  await waitFor(() => dialogOpen(byId('app-dialog')), { message: 'the draw-funds prompt to open' });
  window.document.getElementById('dlg-input').value = '25.00';
  window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the draw-funds reason step to open');
  window.document.getElementById('dlg-input').value = 'advance against March work';
  window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the draw to commit and reduce the ledger balance');
  rets = (await (await fetch(BASE + '/api/retainers', { headers: { Cookie: SESSION_COOKIE } })).json()).retainers;
  check('draw reduces balance with reason (#144)', rets[0].balance_minor === 57500
    && /draw recorded/i.test(window.document.getElementById('live-region').textContent));
  // Over-draw is refused by the API and announced.
  row2.querySelectorAll('button').forEach((b) => {
    if (b.textContent === 'Draw funds') b.dispatchEvent(new window.Event('click', { bubbles: true }));
  });
  await waitFor(() => dialogOpen(byId('app-dialog')), { message: 'the over-draw prompt to open' });
  window.document.getElementById('dlg-input').value = '999.00';
  window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the over-draw reason step to open');
  window.document.getElementById('dlg-input').value = 'greed';
  window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the refused over-draw to settle');
  rets = (await (await fetch(BASE + '/api/retainers', { headers: { Cookie: SESSION_COOKIE } })).json()).retainers;
  check('overdraw refused, balance intact (#144)', rets[0].balance_minor === 57500
    && /Draw failed/i.test(window.document.getElementById('live-region').textContent));
}

// ---- RECURRING SCHEDULE MANAGEMENT (#136) ----
{
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the Invoices section to reload before creating the schedule');
  window.document.getElementById('rec-customer').value = acmeOpt.value;
  window.document.getElementById('rec-mode').value = 'retainer';
  window.document.getElementById('rec-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await waitFor(() => !byId('rec-error').hidden, {
    message: 'the amount-less retainer schedule to show its inline error',
  });
  check('retainer schedule with 0 amount shows inline error (#136)',
    !window.document.getElementById('rec-error').hidden
    && /retainer/i.test(window.document.getElementById('rec-error').textContent));
  check('rejected schedule persisted nothing (#136)',
    (await (await fetch(BASE + '/api/schedules', { headers: { Cookie: SESSION_COOKIE } })).json()).schedules.length === 0);
  window.document.getElementById('rec-mode').value = 'time';
  window.document.getElementById('rec-cadence').value = 'quarterly';
  window.document.getElementById('rec-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await waitFor(() => [...window.document.querySelectorAll('#rec-table tbody button')].some((b) => b.textContent === 'Pause'), {
    message: 'the created schedule to render with its Pause action',
  });
  const recs = (await (await fetch(BASE + '/api/schedules', { headers: { Cookie: SESSION_COOKIE } })).json()).schedules;
  check('schedule created via the form (#136)', recs.length === 1 && recs[0].cadence === 'quarterly' && recs[0].active === true);
  const pauseBtn = [...window.document.querySelectorAll('#rec-table tbody button')].find((b) => b.textContent === 'Pause');
  check('active schedule offers Pause (#136)', !!pauseBtn);
  pauseBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => {
    const row = window.document.querySelector('#rec-table tbody tr');
    return !!row && [...row.querySelectorAll('button')].some((b) => b.textContent === 'Resume');
  }, { message: 'the paused schedule row to re-render and offer Resume' });
  check('pause persists and announces (#136)',
    (await (await fetch(BASE + '/api/schedules', { headers: { Cookie: SESSION_COOKIE } })).json()).schedules[0].active === false
    && /paused/i.test(window.document.getElementById('live-region').textContent));
  const row = window.document.querySelector('#rec-table tbody tr');
  check('paused badge + Last billed cursor render (#136)',
    /paused/i.test(row.textContent) && /never/i.test(row.textContent));
  const resumeBtn = [...row.querySelectorAll('button')].find((b) => b.textContent === 'Resume');
  resumeBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => {
    const row = window.document.querySelector('#rec-table tbody tr');
    return !!row && [...row.querySelectorAll('button')].some((b) => b.textContent === 'Pause');
  }, { message: 'the resumed schedule row to re-render and offer Pause again' });
  check('resume works (#136)',
    (await (await fetch(BASE + '/api/schedules', { headers: { Cookie: SESSION_COOKIE } })).json()).schedules[0].active === true);
  const delBtn = [...window.document.querySelectorAll('#rec-table tbody button')].find((b) => b.textContent === 'Delete');
  delBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => dialogOpen(byId('app-dialog')), { message: 'the delete-schedule confirm dialog to open' });
  window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => !window.document.getElementById('rec-empty').hidden, {
    message: 'the deleted schedule to leave the empty state visible',
  });
  check('delete removes the schedule (#136)',
    (await (await fetch(BASE + '/api/schedules', { headers: { Cookie: SESSION_COOKIE } })).json()).schedules.length === 0
    && !window.document.getElementById('rec-empty').hidden);
}


// ---- INVOICE OVERVIEW (#135): tabs, search, sort, columns, tiles, keyboard ----
{
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => window.document.getElementById('inv-chart').querySelectorAll('.inv-bar').length === 24, {
    message: 'the invoice overview tiles and chart to render',
  });
  check('overview tiles + chart render (#135)',
    /EUR \d/.test(window.document.getElementById('inv-tile-open').textContent)
    && window.document.getElementById('inv-chart').querySelectorAll('.inv-bar').length === 24);
  const countRows = () => window.document.querySelectorAll('#invoice-table tbody tr').length;
  const openRows = countRows();
  window.document.getElementById('inv-tab-all').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the invoice table to reload for the All tab');
  check('All tab shows at least the Open rows (#135)', countRows() >= openRows);
  const sBox = window.document.getElementById('invoice-search');
  sBox.value = 'ZZZ-no-match';
  sBox.dispatchEvent(new window.Event('input', { bubbles: true }));
  await waitFor(() => window.document.getElementById('invoice-empty').hidden === false, {
    message: 'the unmatched invoice search to show its empty state',
  });
  check('search empty-state is explicit (#135)',
    window.document.getElementById('invoice-empty').hidden === false
    && /match the current filters/i.test(window.document.getElementById('invoice-empty').textContent));
  sBox.value = 'INV';
  sBox.dispatchEvent(new window.Event('input', { bubbles: true }));
  await waitFor(() => countRows() >= 1, { message: 'the INV search to match rows again' });
  check('search matches by number (#135)', countRows() >= 1);
  sBox.value = '';
  sBox.dispatchEvent(new window.Event('input', { bubbles: true }));
  await settle('the cleared invoice search to re-render before switching tabs');
  window.document.getElementById('inv-tab-all').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => countRows() > 1, { message: 'the All tab to list the invoices to sort' });
  // sort by total desc then asc
  const totalBtn = window.document.querySelector('.p-table__sort-button[data-col="total"]');
  const sortedTotals = () => [...window.document.querySelectorAll('#invoice-table tbody tr')].map((r) => r.children[3].textContent);
  totalBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => totalBtn.closest('th').getAttribute('aria-sort') === 'ascending', {
    message: 'sorting by total to render ascending',
  });
  const asc = sortedTotals();
  totalBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => JSON.stringify(sortedTotals()) !== JSON.stringify(asc)
    && totalBtn.closest('th').getAttribute('aria-sort') === 'descending', {
    message: 'sorting by total again to re-render the rows in the opposite order',
  });
  const desc = sortedTotals();
  const headerSorts = () => [...window.document.querySelectorAll('#invoice-table thead th')].map((th) => th.getAttribute('aria-sort'));
  check('sorting by total toggles direction (#135)', asc.length > 1 && JSON.stringify(asc) !== JSON.stringify(desc)
    && totalBtn.closest('th').getAttribute('aria-sort') === 'descending'
    && headerSorts().filter((a) => a === 'ascending' || a === 'descending').length === 1
    && headerSorts().filter((a) => a === 'none').length === 5);
  // #232 phase 2d: sorting another column moves aria-sort off the previous one
  const custBtn = window.document.querySelector('.p-table__sort-button[data-col="customer"]');
  custBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => custBtn.closest('th').getAttribute('aria-sort') === 'ascending'
    && totalBtn.closest('th').getAttribute('aria-sort') === 'none', {
    message: 'sorting by customer to move aria-sort off the total column',
  });
  check('sorting another column moves and resets aria-sort (#232 phase 2d)',
    custBtn.closest('th').getAttribute('aria-sort') === 'ascending'
    && totalBtn.closest('th').getAttribute('aria-sort') === 'none'
    && headerSorts().filter((a) => a === 'ascending' || a === 'descending').length === 1);
  // column toggle
  const colCb = window.document.querySelector('.inv-col[value="customer"]');
  colCb.checked = false;
  colCb.dispatchEvent(new window.Event('change', { bubbles: true }));
  await waitFor(() => window.document.querySelector('#invoice-table tbody tr').children[1].hidden === true, {
    message: 'the column toggle to hide the customer column',
  });
  check('column toggle hides the customer column (#135)',
    window.document.querySelector('#invoice-table tbody tr').children[1].hidden === true);
  colCb.checked = true;
  colCb.dispatchEvent(new window.Event('change', { bubbles: true }));
  // keyboard selection opens the #133 preview
  const kbRow = window.document.querySelector('#invoice-table tbody tr');
  kbRow.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
  await waitFor(() => byId('invoice-preview').hidden === false
    && /Previewing invoice/.test(byId('live-region').textContent), {
    message: 'keyboard Enter on a row to open the preview',
  });
  check('keyboard Enter on a row opens the preview (#135/#133)',
    window.document.getElementById('invoice-preview').hidden === false
    && /Previewing invoice/.test(window.document.getElementById('live-region').textContent));
  window.document.getElementById('ip-close').dispatchEvent(new window.Event('click', { bubbles: true }));
  // leave the All tab on: downstream checks (paid rows etc.) need every status
}

// ---- EMAIL (#35): set a billing email, then email the issued invoice ----
const custObj = (await (await fetch(BASE + `/api/customers/${acmeOpt.value}`, { headers: { Cookie: SESSION_COOKIE } })).json());
custObj.email = 'billing@acme.test';
const putC = await fetch(`${BASE}/api/customers/${acmeOpt.value}`, {
  method: 'PUT',
  headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE },
  body: JSON.stringify({ name: custObj.name, currency: custObj.currency, default_rate_minor: custObj.default_rate_minor, active: custObj.active, email: 'billing@acme.test' }),
});
check('customer email round-trips via API', putC.status === 200 && (await putC.json()).email === 'billing@acme.test');
const emailRes = await fetch(`${BASE}/api/invoices/${acmeInv.id}/email`, { method: 'POST', headers: { Cookie: SESSION_COOKIE } });
const emailBody = await emailRes.json();
check('invoice email endpoint sends to the billing address', emailRes.status === 200 && emailBody.sent_to === 'billing@acme.test');

// The GUI shows an Email button on the issued invoice and announces the send.
window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
// Tab is shown -> refreshInvoices() re-pulls; wait for the issued-state Email
// action instead of racing the refresh (#265).
await waitFor(() => [...window.document.querySelectorAll('#invoice-table tbody button')].some((b) => b.textContent === 'Email'), {
  message: 'the invoice table to re-pull and offer the Email action',
});
const emailBtn = [...window.document.querySelectorAll('#invoice-table tbody button')].find((b) => b.textContent === 'Email');
check('issued invoice renders an Email button', !!emailBtn);
if (emailBtn) {
  emailBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => /billing@acme.test/.test(byId('live-region').textContent), {
    message: 'the Email action to announce the send',
  });
  {
  const t = window.document.getElementById('live-region').textContent;
  // #130 honesty: either a real send (smtp) or an explicit NOT-sent with the
  // address — never a bare success claim when the transport is disabled.
  check('Email button announces honestly',
    /billing@acme.test/.test(t) && /Invoice emailed to|NOT sent \(SMTP not configured\)/.test(t));
}
}

// ---- PDF DOWNLOAD (#113): GUI button fetches the blob and saves it ----
const pdfBtn = [...window.document.querySelectorAll('#invoice-table tbody button')].find((b) => b.textContent === 'PDF');
check('issued invoice renders a PDF download button', !!pdfBtn);
if (pdfBtn) {
  // Capture the transient <a download> instead of letting jsdom navigate:
  // the app's contract is "blob fetch (cookie intact) -> object URL -> a.click".
  let savedName = null;
  const origCreate = window.document.createElement.bind(window.document);
  window.document.createElement = (tag) => {
    const node = origCreate(tag);
    if (tag === 'a') node.addEventListener('click', (e) => { e.preventDefault(); savedName = node.download; });
    return node;
  };
  pdfBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => savedName !== null && /Invoice PDF downloaded/i.test(byId('live-region').textContent), {
    message: 'the PDF download to click its numbered blob link',
  });
  window.document.createElement = origCreate;
  check('PDF button saves the numbered file', !!savedName && /^INV-\d+\.pdf$/.test(savedName));
  check('PDF download announces success', /Invoice PDF downloaded/i.test(window.document.getElementById('live-region').textContent));

// ---- EMAIL COPY (#112): the GUI prompts for a recipient, then audited ----
const copyBtn = [...window.document.querySelectorAll('#invoice-table tbody button')].find((b) => b.textContent === 'Email copy');
check('issued invoice renders an Email copy button', !!copyBtn);
if (copyBtn) {
  copyBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => dialogOpen(byId('app-dialog')), { message: 'the Email copy prompt to open' });
  const dlg = window.document.getElementById('app-dialog');
  check('Email copy prompts for a recipient address', dlg.open === true || dlg.hasAttribute('open'));
  window.document.getElementById('dlg-input').value = 'accountant@gui.test';
  window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => /accountant@gui.test/.test(byId('live-region').textContent), {
    message: 'the Email copy to announce its recipient',
  });
  check('Email copy announces honestly', /accountant@gui.test/.test(window.document.getElementById('live-region').textContent));
  const audit = await (await fetch(BASE + '/api/audit', { headers: { Cookie: SESSION_COOKIE } })).json();
  check(
    'Email copy lands in the audit log with the recipient',
    audit.events.some((e) => e.event === 'invoice_email_copy' && e.subject.includes('accountant@gui.test')),
  );
}
}

// ---- PAYMENTS (#34) + #129: Pay link / Sync buttons removed from rows; ----
// ---- the checkout + webhook flow is covered via the API directly.      ----
const rowBtns = () => [...window.document.querySelectorAll('#invoice-table tbody button')].map((b) => b.textContent);
check('invoice rows no longer render a Pay link button (#129)', !rowBtns().includes('Pay link'));
check('invoice rows no longer render a Sync button (#129)', !rowBtns().includes('Sync'));
{
  // A signed webhook (fake-mode Stripe, signature ignored) marks the invoice paid.
  const chk = await (await fetch(`${BASE}/api/invoices/${acmeInv.id}/checkout`, { method: 'POST', headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' }, body: JSON.stringify({ provider: 'stripe' }) })).json();
  const whBody = JSON.stringify({ type: 'checkout.session.completed', payment_status: 'paid', amount_minor: 9500, currency: 'EUR', client_reference_id: chk.reference, metadata: { invoice_number: acmeInv.number } });
  const whRes = await fetch(`${BASE}/api/payments/webhook/stripe`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: whBody });
  const whJson = await whRes.json();
  check('webhook marks the invoice paid', whRes.status === 200 && whJson.status === 'paid');
  const afterInv = (await (await fetch(BASE + '/api/invoices', { headers: { Cookie: SESSION_COOKIE } })).json()).invoices.find((i) => i.id === acmeInv.id);
  check('invoice now paid with a provider reference', afterInv.status === 'paid' && /stripe:/.test(afterInv.payment_reference));

  // ---- ACCOUNTING SYNC (#33): the status endpoint stays live (API-only now) ----
  const stRes = await fetch(BASE + '/api/sync/accounting', { headers: { Cookie: SESSION_COOKIE } });
  const stJson = await stRes.json();
  check('accounting status endpoint responds', stRes.status === 200 && Array.isArray(stJson.records));
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => rowBtns().includes('PDF') && rowBtns().includes('Email copy'), {
    message: 'the paid row to re-render with its PDF and Email copy actions',
  });
  check('paid row still offers PDF + Email copy after the trim (#129)',
    rowBtns().includes('PDF') && rowBtns().includes('Email copy'));

  // ---- SSO (#32): provider discovery endpoint (login screen advertises them) ----
  const ssoRes = await fetch(BASE + '/api/auth/sso/providers');
  const ssoJson = await ssoRes.json();
  check('SSO providers endpoint responds pre-session', ssoRes.status === 200 && Array.isArray(ssoJson.providers));
  check('login screen hides SSO box with none configured', window.document.getElementById('sso-box').hidden === true);
}

// ---- EXPENSES (#23): popup entry, seeded categories, and Settings CRUD ----
const tabExp = window.document.getElementById('tab-expenses');
tabExp.dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !byId('panel-expenses').hidden, { message: 'the Expenses section to show' });
let cats = (await (await fetch(BASE + '/api/categories')).json()).categories;
check('fresh installation seeds five common expense categories',
  ['Food', 'Travel', 'Lodging', 'Supplies', 'Software'].every((name) => cats.some((category) => category.name === name)));
check('expense page has no inline record/category editor',
  !!window.document.getElementById('expense-new')
  && window.document.getElementById('expense-dialog').open === false
  && !window.document.getElementById('category-form').closest('#panel-expenses'));
window.document.getElementById('expense-new').dispatchEvent(new window.Event('click', { bubbles: true }));
check('+ Expense opens a popup', window.document.getElementById('expense-dialog').open === true);
const catPicker = window.document.getElementById('expense-category');
check('category selector includes the five defaults and quick-add option',
  ['Food', 'Travel', 'Lodging', 'Supplies', 'Software'].every((name) => [...catPicker.options].some((option) => option.textContent === name))
  && [...catPicker.options].some((option) => option.value === '__add_expense_category__'));
catPicker.value = '__add_expense_category__';
catPicker.dispatchEvent(new window.Event('change', { bubbles: true }));
check('Add new category opens the quick-add popup', window.document.getElementById('expense-category-dialog').open === true);
window.document.getElementById('expense-category-name').value = 'Client Visit';
window.document.getElementById('expense-category-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the quick-added expense category to save and select');
cats = (await (await fetch(BASE + '/api/categories')).json()).categories;
const clientVisit = cats.find((category) => category.name === 'Client Visit');
check('quick-added category is persisted and selected', !!clientVisit && catPicker.value === clientVisit.id);
window.document.getElementById('expense-date').value = '2026-10-05';
window.document.getElementById('expense-customer').value = acmeOpt.value;
window.document.getElementById('expense-customer').dispatchEvent(new window.Event('change', { bubbles: true }));
await settle('the expense form to follow the customer change');
window.document.getElementById('expense-category').value = clientVisit.id;
window.document.getElementById('expense-amount').value = '125.00';
window.document.getElementById('expense-currency').value = 'EUR';
window.document.getElementById('expense-note').value = 'client visit flight';
window.document.getElementById('expense-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the expense to save through the popup');
const exps = (await (await fetch(BASE + '/api/expenses')).json()).expenses;
const ex = exps.find((e) => e.note === 'client visit flight');
check('expense created via popup with category + amount', ex && ex.amount_minor === 12500 && ex.category_id === clientVisit.id);
check('successful expense save closes popup', window.document.getElementById('expense-dialog').open === false);

// ---- CURRENCY CONVERSION: what was paid beside what is billed ----
// The dialog derives the billed currency from the picked customer/project and
// the rate from GET /api/fx, which fails closed with 503 when no rate table is
// reachable — so a rate typed by hand must work on its own, and that is what
// this block exercises end to end (no network required).
window.document.getElementById('expense-new').dispatchEvent(new window.Event('click', { bubbles: true }));
window.document.getElementById('expense-date').value = '2026-10-06';
window.document.getElementById('expense-customer').value = acmeOpt.value;
window.document.getElementById('expense-customer').dispatchEvent(new window.Event('change', { bubbles: true }));
await settle('the expense dialog to derive the billed currency');
check('the dialog prefills the amount currency from the customer, so no conversion happens',
  byId('expense-currency').value === 'EUR'
  && byId('expense-rate').disabled === true
  && /already in the customer\/project currency/.test(byId('expense-rate-note').textContent));
window.document.getElementById('expense-amount').value = '100.00';
window.document.getElementById('expense-currency').value = 'USD';
window.document.getElementById('expense-currency').dispatchEvent(new window.Event('change', { bubbles: true }));
await settle('the conversion fields to follow the currency change');
check('a foreign amount asks for a rate and names the pair',
  byId('expense-rate').disabled === false
  && byId('expense-rate-label').textContent === 'Exchange rate (USD \u2192 EUR)');
byId('expense-rate').value = '0.9206';
byId('expense-rate').dispatchEvent(new window.Event('input', { bubbles: true }));
byId('expense-rate-date').value = '2026-10-01';
check('the dialog shows the value billed in the customer/project currency',
  byId('expense-converted').textContent === 'USD 100.00 = EUR 92.06');
// Without a rate the billed value is unknown: the dialog refuses rather than
// billing the entered number as if it were already in the customer currency.
byId('expense-rate').value = '';
byId('expense-rate').dispatchEvent(new window.Event('input', { bubbles: true }));
window.document.getElementById('expense-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the refused conversion to settle');
check('a converted amount without a rate is refused before the API is called',
  byId('expense-dialog').open === true && /exchange rate/i.test(byId('expense-error').textContent));
byId('expense-rate').value = '0.9206';
byId('expense-rate').dispatchEvent(new window.Event('input', { bubbles: true }));
window.document.getElementById('expense-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the converted expense to save');
const converted = (await (await fetch(BASE + '/api/expenses')).json()).expenses
  .find((e) => e.original_currency === 'USD');
check('the saved expense keeps the entered amount, its rate and the billed value',
  converted && converted.currency === 'EUR' && converted.amount_minor === 9206
  && converted.original_amount_minor === 10000 && converted.exchange_rate === '0.9206'
  && converted.exchange_rate_date === '2026-10-01');
await waitFor(() => [...window.document.querySelectorAll('#expense-table tbody tr')]
  .some((r) => r.textContent.includes('USD 100.00 @')), {
  message: 'the converted expense to appear in the register',
});
const convRow = [...window.document.querySelectorAll('#expense-table tbody tr')]
  .find((r) => r.textContent.includes('USD 100.00 @'));
check('the register shows the billed value and the amount as entered',
  convRow.textContent.includes('EUR 92.06') && convRow.textContent.includes('(USD 100.00 @ 0.9206)'));

// The masthead finder uses the same endpoint. A currency with itself needs no
// rate table at all, so this holds whatever the host's network does.
window.document.getElementById('fx-from').value = 'EUR';
window.document.getElementById('fx-to').value = 'EUR';
window.document.getElementById('fx-amount').value = '1';
window.document.getElementById('fx-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await waitFor(() => byId('fx-result').textContent === 'EUR 1.00 = EUR 1.00.', {
  message: 'the finder identity conversion',
});
check('the top-bar finder converts a currency with itself', true);
window.document.getElementById('fx-from').value = 'US';
window.document.getElementById('fx-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await waitFor(() => /three-letter/.test(byId('fx-result').textContent), {
  message: 'the finder to refuse a code that is not a currency',
});
check('the top-bar finder asks for a real currency code instead of calling the API', true);
// #182 pattern: the expense register filters by customer and project, both
// defaulting to "all".
const expRows = () => [...window.document.querySelectorAll('#expense-table tbody tr')];
const expCustSel = window.document.getElementById('expense-filter-customer');
const expProjSel = window.document.getElementById('expense-filter-project');
check('expense filters default to all customers and all projects',
  expCustSel.options[0].textContent === 'All customers'
  && expProjSel.options[0].textContent === 'All projects'
  && expCustSel.value === '' && expProjSel.value === '');
const expBefore = expRows().length;
check('every expense is listed without a filter', expBefore >= 1);
expCustSel.value = ex.customer_id;
expCustSel.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => expCustSel.value === ex.customer_id
  && /all projects/i.test(byId('live-region').textContent), {
  message: 'the expense customer filter to re-render',
});
check('the expense customer filter narrows the register to one customer',
  expRows().length >= 1 && expRows().length <= expBefore
  && expRows().every((r) => r.cells[1].textContent === expRows()[0].cells[1].textContent));
expCustSel.value = '';
expCustSel.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => expRows().length === expBefore
  && /all customers and projects/i.test(byId('live-region').textContent), {
  message: 'clearing the expense customer filter to restore every expense',
});
check('clearing the expense customer filter restores every expense',
  expRows().length === expBefore && expProjSel.options[0].textContent === 'All projects');
window.document.getElementById('tab-settings').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !byId('panel-settings').hidden, { message: 'the Settings section to show' });
window.document.getElementById('settings-tab-expenses').dispatchEvent(new window.Event('click', { bubbles: true }));
check('Expense categories has a dedicated Settings panel',
  window.document.getElementById('settings-tab-expenses').getAttribute('aria-selected') === 'true'
  && !window.document.getElementById('settings-panel-expenses').hidden);
const editClientVisit = window.document.querySelector('#category-list button[aria-label="Edit category Client Visit"]');
editClientVisit.dispatchEvent(new window.Event('click', { bubbles: true }));
window.document.getElementById('category-name').value = 'Client visit costs';
window.document.getElementById('category-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the edited expense category to save');
cats = (await (await fetch(BASE + '/api/categories')).json()).categories;
check('Settings category form updates existing category via PUT',
  cats.some((category) => category.id === clientVisit.id && category.name === 'Client visit costs'));

// ---- SUBMISSIONS (#16): submit a fresh week via the GUI ----
const fresh = await (await fetch(BASE + '/api/entries', { method: 'POST', headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE }, body: JSON.stringify({ date: '2027-01-05', customer_id: acmeOpt.value, project_code: 'P-9', hours: 2 }) })).json();
// #16 (Approvals) is hidden for now, so its rail entry is unhidden just for this
// flow: the tab, its panel and the API stay implemented and tested, and the entry
// returns to the rail (and the tab to the rail's visible order) with one
// attribute when the process exists.
const tabSub = window.document.getElementById('tab-submissions');
tabSub.hidden = false;
tabSub.dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => !byId('panel-submissions').hidden, { message: 'the Approvals panel to show' });
tabSub.hidden = true;
window.document.getElementById('submission-week').value = '2027-01-04';
window.document.getElementById('submission-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the week submission to be created');
const subs = (await (await fetch(BASE + '/api/submissions')).json()).submissions;
const jan = subs.find((x) => x.week_start === '2027-01-04');
check('submission created via GUI', jan && jan.state === 'submitted' && jan.entry_ids.includes(fresh.id));
const lockRes = await fetch(`${BASE}/api/entries/${fresh.id}`, { method: 'PUT', headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE }, body: JSON.stringify({ date: '2027-01-05', customer_id: acmeOpt.value, project_code: 'P-9', hours: 3 }) });
check('submitted week locks its entries (edit -> 409)', lockRes.status === 409);

// ---- WEEK GRID (#13): add row, copy last week, lock column ----
window.document.getElementById('ts-week').dispatchEvent(new window.Event('click', { bubbles: true }));
const weekDateEl = window.document.getElementById('week-date');
weekDateEl.value = '2026-12-07';
weekDateEl.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => window.document.querySelector('#week-table tbody .week-empty-notice') !== null, {
  message: 'the empty week to render its notice row',
});
const emptyWeekNotice = window.document.querySelector('#week-table tbody .week-empty-notice');
check('empty week keeps its table and headers visible',
  !window.document.getElementById('week-table').hidden
  && window.document.querySelectorAll('#week-table thead th').length === 9);
check('empty week notice spans every column inside the table',
  emptyWeekNotice && emptyWeekNotice.colSpan === 9
  && emptyWeekNotice.querySelector('strong').textContent === 'No time logged'
  && emptyWeekNotice.textContent.includes("You haven't recorded any time yet this week."));

// Copy last week's project rows into an empty week (MKT-2 has 2026-12-01).
const copyWeeks = window.document.getElementById('week-copy-previous');
copyWeeks.click();
await waitFor(() => [...window.document.querySelectorAll('#week-table tbody tr')].some((r) => r.textContent.includes('MKT-2')), {
  message: 'copy-from-last-week to add the MKT-2 row',
});
const wkBody = window.document.querySelector('#week-table tbody');
const copiedRow = [...wkBody.querySelectorAll('tr')].find((r) => r.textContent.includes('MKT-2'));
check('copy-from-last-week added the MKT-2 row', !!copiedRow);
check('copied row starts empty', !!copiedRow && [...copiedRow.querySelectorAll('input.cell-input')].every((i) => i.value === ''));
check('copied project rows replace the empty-week notice',
  !window.document.querySelector('#week-table .week-empty-notice'));
check('copied row total is 0:00', copiedRow && copiedRow.querySelector('.row-total').textContent.trim() === '0:00');

// #128: re-copy is idempotent and says so; empty copied rows belong to THIS
// week (gone when viewing another week, back when returning).
copyWeeks.click();
await waitFor(() => /Already copied|already in this week/i.test(byId('live-region').textContent), {
  message: 'the re-copy to announce its idempotency',
});
const rowsAfter2 = [...window.document.querySelectorAll('#week-table tbody tr')].filter((r) => r.textContent.includes('MKT-2')).length;
check('re-copy adds no duplicate rows (#128)', rowsAfter2 === 1);
check('re-copy announces idempotency (#128)',
  /Already copied|already in this week/i.test(window.document.getElementById('live-region').textContent));
weekDateEl.value = '2026-12-14';
weekDateEl.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => ![...window.document.querySelectorAll('#week-table tbody tr')].some((r) => r.textContent.includes('MKT-2')), {
  message: 'the other week to render without the copied rows',
});
check('copied empty rows do not leak into other weeks (#128)',
  ![...window.document.querySelectorAll('#week-table tbody tr')].some((r) => r.textContent.includes('MKT-2')));
weekDateEl.value = '2026-12-07';
weekDateEl.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => [...window.document.querySelectorAll('#week-table tbody tr')].some((r) => r.textContent.includes('MKT-2')), {
  message: 'the copied rows to return when the week is redisplayed',
});
check('copied rows return when the week is redisplayed (#128)',
  [...window.document.querySelectorAll('#week-table tbody tr')].some((r) => r.textContent.includes('MKT-2')));

// Lock column: the submitted 2027-01 week renders its cell disabled.
weekDateEl.value = '2027-01-05';
weekDateEl.dispatchEvent(new window.Event('change', { bubbles: true }));
await waitFor(() => window.document.querySelector('#week-table input.cell-input[data-date="2027-01-05"]') !== null, {
  message: 'the submitted week to render its locked cell',
});
const janRow = [...window.document.querySelectorAll('#week-table tbody tr')].find(
  (r) => r.textContent.includes('P-9') && r.textContent.includes('ACME'),
);
const lockedCell = janRow && janRow.querySelector('input.cell-input[data-date="2027-01-05"]');
check('submitted-week cell holds the hours', !!lockedCell && lockedCell.value === '2.00');
check('submitted-week cell renders locked (disabled input)', !!lockedCell && lockedCell.disabled === true && lockedCell.closest('td').classList.contains('locked'));
check('locked row marked for the lock column', !!lockedCell && lockedCell.closest('tr').classList.contains('has-lock'));

// ---- CONFIG (#94): effective config renders + edit persists + source flips ----
window.document.getElementById('tab-settings').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => window.document.querySelectorAll('#config-table tbody tr').length >= 5, {
  message: 'the config table to render its whitelisted rows',
});
window.document.getElementById('settings-tab-security').dispatchEvent(new window.Event('click', { bubbles: true }));
const cfgRows = [...window.document.querySelectorAll('#config-table tbody tr')];
check('config table renders whitelisted rows', cfgRows.length >= 5);
const daysRow = cfgRows.find((r) => r.cells[0].textContent === 'reminder_days');
check('reminder_days row shows default source', !!daysRow && daysRow.cells[2].textContent === 'default');
if (daysRow) {
  window.document.getElementById('config-key').value = 'reminder_days';
  window.document.getElementById('config-value').value = '12';
  window.document.getElementById('config-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await waitFor(() => {
    const row = [...window.document.querySelectorAll('#config-table tbody tr')]
      .find((r) => r.cells[0].textContent === 'reminder_days');
    return !!row && row.cells[1].textContent === '12';
  }, { message: 'the edited config row to re-render with its new value' });
  const after = [...window.document.querySelectorAll('#config-table tbody tr')].find((r) => r.cells[0].textContent === 'reminder_days');
  check('edit persists to config.json and source becomes file', after.cells[1].textContent === '12' && after.cells[2].textContent === 'file');
}
// Non-whitelisted keys cannot be written even via the API.
const badCfg = await fetch(BASE + '/api/admin/config', { method: 'PUT', headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' }, body: '{"smtp.password":"nope"}' });
check('secret-shaped config keys are refused', badCfg.status === 422);

// ---- INVOICE TEMPLATE (#116): admin editor, cheat-sheet, rejection, preview ----
window.document.getElementById('tab-settings').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => /Email delivery:/i.test(byId('email-status').textContent), {
  message: 'the Settings email transport state to render',
});
window.document.getElementById('settings-tab-security').dispatchEvent(new window.Event('click', { bubbles: true }));
check('Settings discloses the email transport state (#130)',
  /Email delivery:/i.test(window.document.getElementById('email-status').textContent));
const varRows = [...window.document.querySelectorAll('#template-vars tbody tr')];
window.document.getElementById('settings-tab-invoice').dispatchEvent(new window.Event('click', { bubbles: true }));
// ---- APPEARANCE & MESSAGES (#146) ----
{
  const set = (id, v) => { const el = window.document.getElementById(id); el.value = v; };
  set('org-from', 'Tucano Billing Desk');
  set('org-reply', 'invoices@acme.test');
  set('org-accent', '#e95420');
  window.document.getElementById('org-accent').dispatchEvent(new window.Event('input', { bubbles: true }));
  check('accent swatch paints from validated hex (#146)',
    window.document.getElementById('accent-swatch').style.background.includes('233') ||
    window.document.getElementById('accent-swatch').style.background !== '');
  window.document.getElementById('org-name').value = 'Acme Invoice Co';
  window.document.getElementById('org-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await settle('the organisation sender settings to save');
  const o = await (await fetch(BASE + '/api/admin/org', { headers: { Cookie: SESSION_COOKIE } })).json();
  check('sender display name + reply-to + accent persist (#146)',
    o.from_name === 'Tucano Billing Desk' && o.reply_to === 'invoices@acme.test' && o.accent === '#e95420');
  // Invalid reply-to refuses with an inline error and stores nothing.
  set('org-reply', 'not-an-email');
  window.document.getElementById('org-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await waitFor(() => !byId('org-error').hidden && /reply/i.test(byId('org-error').textContent), {
    message: 'the invalid Reply-To to show its inline error',
  });
  const o2 = await (await fetch(BASE + '/api/admin/org', { headers: { Cookie: SESSION_COOKIE } })).json();
  check('invalid Reply-To shows inline error and persists nothing (#146)',
    !window.document.getElementById('org-error').hidden
    && /reply/i.test(window.document.getElementById('org-error').textContent)
    && o2.reply_to === 'invoices@acme.test');
  set('org-reply', 'invoices@acme.test');
}

check('company identity form saves and reloads (#138)', await (async () => {
  window.document.getElementById('org-name').value = 'Tucano SRL';
  window.document.getElementById('org-legal').value = 'BE0987654321';
  window.document.getElementById('org-city').value = 'Milan';
  window.document.getElementById('org-country').value = 'IT';
  window.document.getElementById('org-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await settle('the company identity form to save');
  const o = await (await fetch(BASE + '/api/admin/org', { headers: { Cookie: SESSION_COOKIE } })).json();
  return o.name === 'Tucano SRL' && o.legal_id === 'BE0987654321' && o.address.country === 'IT'
    && !window.document.getElementById('org-error').hidden === false;
})());
const tplCleared = await fetch(BASE + '/api/admin/org', { method: 'PUT', headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' }, body: JSON.stringify({ name: '', legal_id: '', address: null }) });
check('empty org name restores the config fallback (#138)', tplCleared.status === 200);
check('template editor renders the variable cheat-sheet from the server', varRows.length >= 12 && varRows.some((r) => r.textContent.includes('%invoice_issue_month%')));
// Unknown variable: inline error, no save.
window.document.getElementById('template-subject').value = 'Invoice %invoice_number%';
window.document.getElementById('template-body').value = 'Dear %customer_name%,\n\n- consulting work';
window.document.getElementById('template-footer').value = 'Wire to IBAN TT16';
window.document.getElementById('template-terms').value = 'net_30';
window.document.getElementById('template-terms').dispatchEvent(new window.Event('change', { bubbles: true }));
window.document.getElementById('template-body').value = 'bad %nope% here';
window.document.getElementById('template-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await waitFor(() => !byId('template-error').hidden, {
  message: 'the unknown template variable to show its inline error',
});
check('unknown %variable% shows an inline error', !window.document.getElementById('template-error').hidden
  && window.document.getElementById('template-error').textContent.includes('%nope%'));
// Fix it and save; terms select toggles the days field for custom.
window.document.getElementById('template-body').value = 'Dear %customer_name%,\n\n- consulting work';
window.document.getElementById('template-terms').value = 'custom';
window.document.getElementById('template-terms').dispatchEvent(new window.Event('change', { bubbles: true }));
check('custom terms reveal the days input', !window.document.getElementById('template-terms-days-field').hidden);
window.document.getElementById('template-terms-days').value = '21';
window.document.getElementById('template-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the template with custom terms to save');
const tpl = await (await fetch(BASE + '/api/admin/invoice-template', { headers: { Cookie: SESSION_COOKIE } })).json();
check('template saved with custom terms', tpl.template.body.includes('%customer_name%') && tpl.template.payment_terms.kind === 'custom' && tpl.template.payment_terms.days === 21);
check('template save announced', /template saved/i.test(window.document.getElementById('live-region').textContent));

// ---- CUSTOMER INVOICE FIELDS (#116): terms, subject, notes via the form ----
window.document.getElementById('tab-customers').dispatchEvent(new window.Event('click', { bubbles: true }));
window.document.getElementById('customer-name').value = 'Termy LLC';
window.document.getElementById('customer-currency').value = 'EUR';
window.document.getElementById('customer-rate').value = '50';
window.document.getElementById('customer-terms').value = 'upon_receipt';
window.document.getElementById('customer-subject').value = 'INV %invoice_number% for %customer_name%';
window.document.getElementById('customer-notes').value = 'Please quote the invoice number.';
window.document.getElementById('customer-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
await settle('the Termy LLC customer to save');
const termCusts = (await (await fetch(BASE + '/api/customers', { headers: { Cookie: SESSION_COOKIE } })).json()).customers;
const termy = termCusts.find((c) => c.name === 'Termy LLC');
check('customer invoice fields persist via the form', !!termy && termy.payment_terms.kind === 'upon_receipt'
  && termy.invoice_subject.includes('%invoice_number%') && termy.invoice_notes.includes('quote the invoice'));

  // #139: address, VAT and a billing contact through the same form.
  window.document.getElementById('customer-name').value = 'AddrCo';
  window.document.getElementById('customer-currency').value = 'EUR';
  window.document.getElementById('customer-rate').value = '40';
  window.document.getElementById('cust-street').value = 'Harbour 9';
  window.document.getElementById('cust-city').value = 'Antwerp';
  window.document.getElementById('cust-postal').value = '2000';
  window.document.getElementById('cust-country').value = 'BE';
  window.document.getElementById('cust-tax').value = '21';
  window.document.getElementById('contact-add').dispatchEvent(new window.Event('click', { bubbles: true }));
  {
    const row = window.document.querySelector('#contact-rows .contact-row');
    const inputs = row.querySelectorAll('input');
    inputs[0].value = 'Ada Finance';
    inputs[1].value = 'Finance';
    inputs[2].value = 'ada@addrco.test';
    inputs[3].checked = true; // billing contact
  }
  window.document.getElementById('customer-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await settle('the AddrCo customer to save');
  const addrCusts = (await (await fetch(BASE + '/api/customers', { headers: { Cookie: SESSION_COOKIE } })).json()).customers;
  const addrCo = addrCusts.find((x) => x.name === 'AddrCo');
  check('customer address + VAT persist via the form (#139)',
    !!addrCo && addrCo.address.country === 'BE' && addrCo.tax_hundredths === 2100);
  check('billing contact persists via the form (#139)',
    !!addrCo && addrCo.contacts.length === 1 && addrCo.contacts[0].billing === true
    && addrCo.contacts[0].email === 'ada@addrco.test');

// Document preview card uses GET /invoices/{id}/document on the newest invoice.
window.document.getElementById('tab-settings').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => byId('template-preview').hidden === false && byId('template-preview-subject').textContent.includes('Invoice '), {
  message: 'the template preview card to render the subject',
});
const prev = window.document.getElementById('template-preview');
check('template preview card shows the rendered subject', !prev.hidden
  && window.document.getElementById('template-preview-subject').textContent.includes('Invoice '));
check('preview body is plain text (textContent-only rule)', window.document.getElementById('template-preview-body').children.length === 0
  && window.document.getElementById('template-preview-body').textContent.includes('consulting work'));

// ---- PARTIAL PAYMENTS & WRITE-OFF (#114): record payment + write off via GUI ----
const paid2028 = await (await fetch(BASE + '/api/entries', {
  method: 'POST',
  headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' },
  body: JSON.stringify({ date: '2028-02-02', customer_id: acmeOpt.value, project_code: 'MKT-2', hours: 4 }),
})).json();
const inv114 = await (await fetch(BASE + '/api/invoices', {
  method: 'POST',
  headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' },
  body: JSON.stringify({ customer_id: acmeOpt.value, from: '2028-02-01', to: '2028-02-29' }),
})).json();
await fetch(`${BASE}/api/invoices/${inv114.id}/issue`, { method: 'POST', headers: { Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' } });
window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
await waitFor(() => [...window.document.querySelectorAll('#invoice-table tbody tr')]
  .filter((r) => r.textContent.includes(inv114.number)).length === 1, {
  message: 'the issued invoice row to render with its balance',
});
const balCells = [...window.document.querySelectorAll('#invoice-table tbody tr')]
  .filter((r) => r.textContent.includes(inv114.number))
  .map((r) => r.children[4].textContent.trim());
check('open invoice shows the outstanding Balance', balCells.length === 1 && balCells[0].includes('380.00'), `cells=${balCells}`);
const recBtn = [...window.document.querySelectorAll('#invoice-table tbody button')].find((b) => b.textContent === 'Record payment'
  && b.closest('tr').textContent.includes(inv114.number));
check('issued invoice renders Record payment (replacing Mark paid)', !!recBtn);
if (recBtn) {
  recBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => /^\d+\.\d\d$/.test(byId('dlg-input').value), {
    message: 'the record-payment dialog to prefill the outstanding balance',
  });
  const dlgIn = window.document.getElementById('dlg-input');
  check('record-payment dialog prefills the balance', /^\d+\.\d\d$/.test(dlgIn.value));
  const owed = dlgIn.value;
  dlgIn.value = '10.00';
  window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the payment reference step to open');
  window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true })); // reference step: empty ok
  await waitFor(() => /still outstanding/i.test(byId('live-region').textContent), {
    message: 'the partial payment to announce the remaining balance',
  });
  const after = await (await fetch(`${BASE}/api/invoices/${inv114.id}`, { headers: { Cookie: SESSION_COOKIE } })).json();
  check('partial payment recorded as partly_paid with a ledger line', after.status === 'partly_paid' && after.payments.length === 1 && after.payments[0].amount_minor === 1000);
  check('record payment announces remaining balance', /still outstanding/i.test(window.document.getElementById('live-region').textContent));
  // Write off the rest.
  const woBtn = [...window.document.querySelectorAll('#invoice-table tbody button')].find((b) => b.textContent === 'Write off'
    && b.closest('tr').textContent.includes(inv114.number));
  check('partly_paid invoice offers Write off', !!woBtn);
  if (woBtn) {
    woBtn.dispatchEvent(new window.Event('click', { bubbles: true }));
    await waitFor(() => dialogOpen(byId('app-dialog')), { message: 'the write-off reason prompt to open' });
    window.document.getElementById('dlg-input').value = 'goodwill waiver';
    window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true }));
    await settle('the write-off confirm step to open');
    window.document.getElementById('dlg-ok').dispatchEvent(new window.Event('click', { bubbles: true })); // confirm step
    await waitFor(() => /written off/i.test(byId('live-region').textContent), {
      message: 'the write-off to be announced',
    });
    const wo = await (await fetch(`${BASE}/api/invoices/${inv114.id}`, { headers: { Cookie: SESSION_COOKIE } })).json();
    check('write-off finalizes with the reason', wo.status === 'written_off' && wo.write_off_reason === 'goodwill waiver');
    check('write-off announced', /written off/i.test(window.document.getElementById('live-region').textContent));
  }
}

// ---- E2E WORKFLOW (#141): Day -> Week -> tracked review -> DRAFT ----
//
// Deterministic timesheet-to-invoice journey on isolated fixtures. Stops at
// Draft: never calls Issue/Email/Copy/Checkout/Sync, asserts the draft locks
// nothing, and tears every fixture down again.
{
  const tag = 'WF' + String(Date.now()).slice(-6);
  const ev = (elx, type) => elx.dispatchEvent(new window.Event(type, { bubbles: true }));
  const getJson = async (path) => (await fetch(BASE + path, { headers: { Cookie: SESSION_COOKIE } })).json();
  const postJson = async (path, body) => {
    const r = await fetch(BASE + path, {
      method: 'POST',
      headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' },
      body: JSON.stringify(body),
    });
    return { status: r.status, json: await r.json().catch(() => ({})) };
  };
  // Fixtures: customers through the GUI (keeps the shared pickers fresh),
  // project + task over the API on the unique customer.
  window.document.getElementById('tab-customers').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => !byId('panel-customers').hidden, { message: 'the Customers section to show' });
  const mkCustomer = async (name) => {
    window.document.getElementById('customer-name').value = name;
    window.document.getElementById('customer-currency').value = 'EUR';
    window.document.getElementById('customer-rate').value = '90';
    window.document.getElementById('customer-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
    await settle(`the fixture customer ${name} to save`);
    return (await getJson('/api/customers')).customers.find((c) => c.name === name);
  };
  const wfCust = await mkCustomer(`WF-${tag}`);
  const weCust = await mkCustomer(`WE-${tag}`);
  await postJson(`/api/customers/${wfCust.id}/projects`, { code: 'W1', currency: 'EUR', rate_minor: 7000 });
  await postJson(`/api/customers/${wfCust.id}/projects/W1/tasks`, { code: 'T1', name: 'Focus work' });

  // ---- DAY: one billable entry with a note through the dialog (#145) ----
  window.document.getElementById('tab-timesheet').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => !byId('panel-timesheet').hidden, { message: 'the Timesheets section to show' });
  window.document.getElementById('ts-day').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('ts-day').getAttribute('aria-selected') === 'true', {
    message: 'the Day segment to be selected',
  });
  const wfDay = window.document.getElementById('day-date');
  wfDay.value = '2031-04-11';
  ev(wfDay, 'change');
  await waitFor(() => {
    const selected = window.document.querySelector('#week-strip .ws-day.selected');
    return byId('day-date').value === '2031-04-11'
      && !!selected && selected.dataset.date === '2031-04-11';
  }, { message: 'the Day view to load 2031-04-11' });
  window.document.getElementById('day-add').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => dialogOpen(byId('entry-dialog')), { message: 'the Track time entry dialog to open' });
  const wfDlg = window.document.getElementById('entry-dialog');
  check('Track time opens the entry dialog (#141/#145)', wfDlg.open === true || wfDlg.hasAttribute('open'));
  check('the Track time popup offers a visible Cancel (#141)',
    (() => {
      const b = window.document.getElementById('entry-cancel');
      return !b.hidden && /^cancel$/i.test(b.textContent.trim());
    })());
  const wfCustSel = window.document.getElementById('entry-customer');
  wfCustSel.value = wfCust.id;
  ev(wfCustSel, 'change');
  await waitFor(() => [...byId('entry-project').options].some((o) => o.value === 'W1'), {
    message: 'the entry project picker to load W1',
  });
  const wfProjSel = window.document.getElementById('entry-project');
  wfProjSel.value = 'W1';
  ev(wfProjSel, 'change');
  await waitFor(() => [...byId('entry-task').options].some((o) => o.value === 'T1'), {
    message: 'the entry task picker to load T1',
  });
  window.document.getElementById('entry-task').value = 'T1';
  window.document.getElementById('entry-hours').value = '3.25';
  window.document.getElementById('entry-note').value = 'wf: legal research';
  window.document.getElementById('entry-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true }));
  await waitFor(() => byId('day-total').textContent.trim() === '3:15', {
    message: 'the Day view to reconcile the new 3.25h entry',
  });
  wfDlg.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  await waitFor(() => !dialogOpen(byId('entry-dialog')), { message: 'Escape to close the entry dialog' });
  const wfEntry = (await getJson('/api/entries?date=2031-04-11')).entries
    .find((e) => e.customer_id === wfCust.id && e.project_code === 'W1');
  check('day entry created on the task, billable, with note (#141)',
    !!wfEntry && wfEntry.hours === 3.25 && wfEntry.task_code === 'T1'
    && /legal research/.test(wfEntry.note) && wfEntry.billable === true);
  check('Day view reconciles the entry into its total (#141)',
    window.document.getElementById('day-total').textContent.trim() === '3:15'
    && [...window.document.querySelectorAll('#day-table tbody tr')].some((r) => r.textContent.includes('W1'))
    && wfDlg.open !== true && !wfDlg.hasAttribute('open'));
  const dayNoteRow = [...window.document.querySelectorAll('#day-table tbody tr')]
    .find((r) => r.textContent.includes('W1'));
  check('notes are their own column with a header, main cell no longer stacks them',
    [...window.document.querySelectorAll('#day-table thead th')].map((th) => th.textContent).join(',')
      === 'Project / customer,Notes,Hours,Actions'
    && /legal research/.test(dayNoteRow.cells[1].textContent)
    && !/legal research/.test(dayNoteRow.cells[0].textContent));

  // ---- WEEK: the same entry as a task-level cell, totals reconciled ----
  const wfWeek = window.document.getElementById('week-date');
  wfWeek.value = '2031-04-07';
  ev(wfWeek, 'change');
  await waitFor(() => window.document.querySelector('#week-table input[data-date="2031-04-11"][data-task="T1"]') !== null, {
    message: 'the week grid to render the W1 \u00b7 T1 task-level row',
  });
  const wfRow = [...window.document.querySelectorAll('#week-table tbody tr')].find((r) => r.textContent.includes('W1 \u00b7 T1'));
  const wfCell = window.document.querySelector('#week-table input[data-date="2031-04-11"][data-task="T1"]');
  check('week renders the task-level row with the entry cell (#141/#137)',
    !!wfRow && !!wfCell && wfCell.value === '3.25');
  check('cell accessible name carries project, task and date (#141)',
    /W1 \/ T1/.test((wfCell && wfCell.getAttribute('aria-label')) || '')
    && /2031-04-11/.test((wfCell && wfCell.getAttribute('aria-label')) || ''));
  check('week row total matches the day total (#141)',
    !!wfRow && wfRow.querySelector('.row-total').textContent.trim() === '3:15');

  // ---- TRACKED INVOICE WIZARD (#134): review, then save DRAFT ----
  window.document.getElementById('tab-invoices').dispatchEvent(new window.Event('click', { bubbles: true }));
  await settle('the Invoices section to reload before reading the draft count');
  const invsBefore = (await getJson('/api/invoices')).invoices.length;
  window.document.getElementById('iw-customer').value = wfCust.id;
  window.document.getElementById('iw-next').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('iw-step2').hidden === false, { message: 'the invoice wizard to advance to step 2' });
  window.document.getElementById('iw-preset').value = 'custom';
  ev(window.document.getElementById('iw-preset'), 'change');
  const wfFrom = window.document.getElementById('iw-from');
  wfFrom.value = '2031-04-01';
  ev(wfFrom, 'change');
  await settle('the wizard period to reload from the new start date');
  const wfTo = window.document.getElementById('iw-to');
  wfTo.value = '2031-04-30';
  ev(wfTo, 'change');
  await waitFor(() => /W1.*3\.25 h unbilled/.test(byId('iw-projects').textContent), {
    message: 'the wizard to list the fixture project with its unbilled hours',
  });
  const wfBox = window.document.getElementById('iw-projects');
  check('wizard lists exactly the fixture project with unbilled hours (#141)',
    /W1.*3\.25 h unbilled/.test(wfBox.textContent)
    && wfBox.querySelectorAll('input[type=checkbox]').length === 1);
  wfBox.querySelector('input[type=checkbox]').checked = true;
  window.document.getElementById('iw-next').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => /227\.50/.test(byId('iw-summary').textContent + byId('iw-lines').textContent), {
    message: 'the wizard review to total the fixture line at 227.50 EUR',
  });
  const wfReview = window.document.getElementById('iw-summary').textContent + ' | ' + window.document.getElementById('iw-lines').textContent;
  check('review shows the exact line, currency and 227.50 EUR (#141)',
    /227\.50/.test(wfReview) && /EUR/.test(wfReview) && /W1/.test(wfReview));
  check('review is persisted nothing before Save (#141)',
    (await getJson('/api/invoices')).invoices.length === invsBefore);
  window.document.getElementById('iw-save').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => /Draft INV-\d+ created from 1 line/.test(byId('live-region').textContent), {
    message: 'saving the wizard selection to announce the draft',
  });
  check('save announced a draft with Issue still ahead (#141)',
    /Draft INV-\d+ created from 1 line/.test(window.document.getElementById('live-region').textContent));
  const wfInv = (await getJson('/api/invoices')).invoices.find((i) => i.customer_id === wfCust.id);
  check('exactly one draft exists, locked nothing (#141)',
    (await getJson('/api/invoices')).invoices.length === invsBefore + 1
    && !!wfInv && wfInv.status === 'draft' && /^INV-/.test(wfInv.number));
  const wfLine = wfInv.lines[0];
  check('draft line is the reviewed entry with snapshot money (#141)',
    wfInv.lines.length === 1 && wfLine.entry_id === wfEntry.id && wfLine.task_code === 'T1'
    && wfLine.hours === 3.25 && wfLine.rate_minor === 7000 && wfLine.amount_minor === 22750);
  const wfPdf = await fetch(`${BASE}/api/invoices/${wfInv.id}/pdf`, { headers: { Cookie: SESSION_COOKIE } });
  const wfPut = await fetch(`${BASE}/api/entries/${wfEntry.id}`, {
    method: 'PUT',
    headers: { 'content-type': 'application/json', Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' },
    body: JSON.stringify({ date: '2031-04-11', customer_id: wfCust.id, project_code: 'W1', task_code: 'T1', hours: 3.25, note: 'wf: legal research', billable: true }),
  });
  check('draft issued nothing: PDF 409 and source entry still editable (#141)',
    wfPdf.status === 409 && wfPut.status === 200);

  // ---- DASHBOARD states (#135): hidden on Open, found on All + search ----
  const wfOpen = window.document.getElementById('inv-tab-open');
  const wfAll = window.document.getElementById('inv-tab-all');
  const wfSearch = window.document.getElementById('invoice-search');
  wfOpen.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => wfOpen.getAttribute('aria-pressed') === 'true', {
    message: 'the Open tab to become the active invoice filter',
  });
  wfSearch.value = wfInv.number;
  ev(wfSearch, 'input');
  await settle('the draft-number search to re-render the Open list');
  const openHas = [...window.document.querySelectorAll('#invoice-table tbody tr')]
    .some((r) => r.textContent.includes(wfInv.number));
  wfAll.dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => [...window.document.querySelectorAll('#invoice-table tbody tr')].some((r) => r.textContent.includes(wfInv.number)), {
    message: 'the All tab to list the searched draft',
  });
  const allHas = [...window.document.querySelectorAll('#invoice-table tbody tr')]
    .some((r) => r.textContent.includes(wfInv.number));
  check('draft invisible on Open, searchable on All; tab states honest (#141/#135)',
    !openHas && allHas && wfAll.getAttribute('aria-pressed') === 'true'
    && wfOpen.getAttribute('aria-pressed') === 'false');
  wfSearch.value = '';
  ev(wfSearch, 'input');
  await settle('the cleared draft-number search to re-render');

  // ---- NO BILLABLE WORK: explains itself, persists nothing (#141) ----
  window.document.getElementById('iw-customer').value = weCust.id;
  window.document.getElementById('iw-next').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => byId('iw-step2').hidden === false, { message: 'the invoice wizard to advance to step 2' });
  window.document.getElementById('iw-preset').value = 'custom';
  ev(window.document.getElementById('iw-preset'), 'change');
  const weFrom = window.document.getElementById('iw-from');
  weFrom.value = '2031-05-01';
  ev(weFrom, 'change');
  await settle('the wizard period to reload from the new start date');
  const weTo = window.document.getElementById('iw-to');
  weTo.value = '2031-05-31';
  ev(weTo, 'change');
  await waitFor(() => /No uninvoiced billable work/i.test(byId('iw-projects').textContent), {
    message: 'the empty period to explain there is no billable work',
  });
  check('empty period explains there is no billable work (#141)',
    /No uninvoiced billable work/i.test(window.document.getElementById('iw-projects').textContent));
  window.document.getElementById('iw-next').dispatchEvent(new window.Event('click', { bubbles: true }));
  await waitFor(() => !byId('iw-error').hidden, {
    message: 'forcing an empty period to show its inline error',
  });
  const weErr = window.document.getElementById('iw-error');
  check('forcing an empty period errors inline and drafts nothing (#141)',
    weErr.hidden === false && /no billable|nothing/i.test(weErr.textContent)
    && (await getJson('/api/invoices')).invoices.every((i) => i.customer_id !== weCust.id));

  // ---- TEARDOWN: no fixture survives (#141) ----
  const del = async (path) => {
    const r = await fetch(BASE + path, { method: 'DELETE', headers: { Cookie: SESSION_COOKIE, 'X-CSRF-Protection': '1' } });
    if (r.status >= 300) console.log(`TEARDOWN ${r.status} ${path} ${(await r.text()).slice(0, 120)}`);
    return r;
  };
  await del(`/api/invoices/${wfInv.id}`);
  await del(`/api/entries/${wfEntry.id}`);
  await del(`/api/customers/${wfCust.id}/projects/W1/tasks/T1`);
  await del(`/api/customers/${wfCust.id}/projects/W1`);
  await del(`/api/customers/${wfCust.id}`);
  await del(`/api/customers/${weCust.id}`);
  const left = await getJson('/api/customers');
  const leftInv = await getJson('/api/invoices');
  const leftEnt = await getJson('/api/entries?date=2031-04-11');
  console.log('LEFT ' + JSON.stringify(left.customers.map((c) => [c.name, c.id.slice(0, 8)])));
  const goneCust = left.customers.every((c) => !c.name.startsWith(`WF-${tag}`) && !c.name.startsWith(`WE-${tag}`));
  const goneInv = leftInv.invoices.every((i) => i.id !== wfInv.id);
  const goneEnt = leftEnt.entries.every((e) => e.id !== wfEntry.id);
  check('teardown removed fixture customers (#141)', goneCust);
  check('teardown removed the draft (#141)', goneInv);
  check('teardown removed the entry (#141)', goneEnt);
}

const memberDom = new JSDOM(html, { runScripts: 'outside-only', url: BASE + '/' });
memberDom.window.eval(`${appJs}\nshowAccount({ role: "member", name: "Member" }); initSettingsTabs();`);
const memberDocument = memberDom.window.document;
check('members can manage expense categories without admin Settings panels',
  !memberDocument.getElementById('tab-settings').hidden
  && !memberDocument.getElementById('settings-tab-expenses').hidden
  && !memberDocument.getElementById('settings-panel-expenses').hidden
  && memberDocument.getElementById('settings-tab-security').hidden
  && memberDocument.getElementById('settings-panel-security').hidden);
memberDom.window.close();

console.log(`\n${failures === 0 ? 'ALL GUI CHECKS PASSED' : failures + ' GUI CHECK(S) FAILED'}`);
process.exit(failures === 0 ? 0 : 1);

// ---- WEEK GRID VIEW: render a project x day grid and click an empty cell ----
console.log('--- extra: week view ---');
