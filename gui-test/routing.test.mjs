// Page routing: every view has a stable URL path (see TAB_ROUTES in app.js and
// spa_page in src/lib.rs). Guards: (1) a deep link selects that view at boot,
// (2) case and trailing slashes are canonicalised, (3) '/' and unknown paths
// fall back to the default view, (4) member visibility wins over the URL,
// (5) user navigation updates the address bar, and (6) popstate switches the
// view back to match the URL.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

// Member boot stub (same shape as boot.test.mjs): admin-tier panels are hidden,
// every data route answers with empty collections.
const bootStub = `
api.get = async (path) => {
  if (path === '/auth/status') return { initialised: true };
  if (path === '/auth/me') return { id: 'm1', name: 'Mem', email: 'm@x.test', role: 'member' };
  if (path === '/auth/sso/providers') return { providers: [] };
  return { entries: [], customers: [], projects: [], submissions: [], expenses: [],
           claims: [], notifications: [], categories: [], item_types: [],
           invoices: [], users: [], schedules: [], retainers: [], timer: null };
};
api.post = async () => ({});
`;

async function bootAt(url) {
  const dom = new JSDOM(html, { runScripts: 'outside-only', url });
  dom.window.HTMLDialogElement.prototype.showModal = function showModal() { this.open = true; };
  dom.window.HTMLDialogElement.prototype.close = function close() { this.open = false; };
  dom.window.eval(`${app}\n${bootStub}`);
  // Wait for boot to complete (#265) instead of polling a fixed number of ticks.
  // The sentinel is the boot flag, not the footer copy: the footer now names the
  // build the server reports, so the exact text is not a boot signal.
  await waitFor(() => dom.window.document.documentElement.dataset.booted === 'ready', {
    message: 'the app to finish booting at ' + url,
  });
  assert.ok(dom.window.document.getElementById('version').textContent.startsWith('TatuTime'),
    'boot completed');
  return dom;
}

const tabSelected = (document, id) =>
  document.getElementById(id).getAttribute('aria-selected') === 'true';

test('a deep link selects that view at boot', async () => {
  const dom = await bootAt('http://localhost/customers');
  const { document } = dom.window;
  assert.ok(tabSelected(document, 'tab-customers'), 'Customers tab selected');
  assert.equal(document.getElementById('panel-customers').hidden, false);
  assert.equal(document.getElementById('page-title').textContent, 'Customers');
  assert.equal(dom.window.location.pathname, '/customers', 'canonical URL kept');
  dom.window.close();
});

test('case and trailing slash are canonicalised on the way in', async () => {
  const dom = await bootAt('http://localhost/Timesheets/Week/');
  const { document } = dom.window;
  assert.equal(dom.window.location.pathname, '/timesheets/week', 'lower-case canonical path');
  assert.ok(tabSelected(document, 'ts-week'), 'Week segment selected');
  assert.equal(document.getElementById('panel-week').hidden, false);
  dom.window.close();
});

test('the root lands on the default Timesheet day view', async () => {
  const dom = await bootAt('http://localhost/');
  assert.equal(dom.window.location.pathname, '/timesheets/day', '/ normalises to the default');
  assert.ok(tabSelected(dom.window.document, 'tab-timesheet'));
  assert.ok(tabSelected(dom.window.document, 'ts-day'));
  dom.window.close();
});

test('unknown client paths fall back to the default view', async () => {
  const dom = await bootAt('http://localhost/not-a-view');
  assert.equal(dom.window.location.pathname, '/timesheets/day', 'unknown path replaced by default');
  assert.ok(tabSelected(dom.window.document, 'tab-timesheet'));
  dom.window.close();
});

test('a member deep link into a hidden admin panel falls back inside Settings', async () => {
  const dom = await bootAt('http://localhost/settings/users');
  const { document } = dom.window;
  assert.ok(tabSelected(document, 'tab-settings'), 'Settings section still opens');
  assert.ok(tabSelected(document, 'settings-tab-expenses'),
    'members only see Expense categories, so that panel wins over the URL');
  assert.equal(dom.window.location.pathname, '/settings/expenses',
    'the URL follows the view that actually rendered');
  dom.window.close();
});

test('the hidden Approvals view falls back to the default route (#16 not yet implemented)', async () => {
  const dom = await bootAt('http://localhost/approvals');
  const { document } = dom.window;
  // The rail entry is hidden until the approval process exists, and a hidden tab
  // is an unavailable view — the deep link must not select a tab nobody can see.
  assert.equal(document.getElementById('tab-submissions').hidden, true,
    'the Approvals rail entry is hidden for now');
  assert.ok(tabSelected(document, 'tab-timesheet'), 'the default view is selected instead');
  assert.equal(document.getElementById('panel-submissions').hidden, true, 'the Approvals panel stays closed');
  assert.equal(dom.window.location.pathname, '/timesheets/day', 'the URL follows the view that rendered');
  dom.window.close();
});

test('the Approvals view returns the moment its rail entry does (#16)', async () => {
  const dom = await bootAt('http://localhost/timesheets/day');
  const { window, document } = dom.window;
  // One attribute — the whole tab, its panel and its route come back with it.
  document.getElementById('tab-submissions').hidden = false;
  window.history.pushState({ path: '/approvals' }, '', '/approvals');
  window.dispatchEvent(new window.PopStateEvent('popstate'));
  assert.ok(tabSelected(document, 'tab-submissions'), 'the unhidden tab is selectable again');
  assert.equal(document.getElementById('panel-submissions').hidden, false);
  assert.equal(window.location.pathname, '/approvals');
  dom.window.close();
});

test('navigation updates the address bar and popstate restores the view', async () => {
  const dom = await bootAt('http://localhost/');
  const { window, document } = dom.window;
  document.getElementById('tab-expenses')
    .dispatchEvent(new window.Event('click', { bubbles: true }));
  assert.equal(window.location.pathname, '/expenses', 'user navigation updates the URL');
  window.history.pushState({ path: '/customers' }, '', '/customers'); // pretend Back/Forward fired
  window.dispatchEvent(new window.PopStateEvent('popstate'));
  assert.ok(tabSelected(document, 'tab-customers'), 'the view is restored from the URL');
  assert.equal(document.getElementById('panel-customers').hidden, false);
  // Reports is hidden for now, so its URL is not a view: popstate leaves what is
  // up in place instead of opening a panel the rail does not offer.
  window.history.pushState({ path: '/reports' }, '', '/reports');
  window.dispatchEvent(new window.PopStateEvent('popstate'));
  assert.ok(tabSelected(document, 'tab-customers'), 'a hidden tab leaves the current view alone');
  assert.equal(document.getElementById('panel-reports').hidden, true, 'and keeps its panel closed');
  dom.window.close();
});

test('the hidden Reports view falls back to the default route', async () => {
  const dom = await bootAt('http://localhost/reports');
  const { document } = dom.window;
  assert.equal(document.getElementById('tab-reports').hidden, true,
    'the Reports rail entry is hidden for now');
  assert.ok(tabSelected(document, 'tab-timesheet'), 'the default view is selected instead');
  assert.equal(document.getElementById('panel-reports').hidden, true, 'the Reports panel stays closed');
  assert.equal(dom.window.location.pathname, '/timesheets/day', 'the URL follows the view that rendered');
  dom.window.close();
});

test('the Reports view returns the moment its rail entry does', async () => {
  const dom = await bootAt('http://localhost/timesheets/day');
  const { window, document } = dom.window;
  // One attribute, exactly as for Approvals (#16): the tab, its panel and its
  // route come back together, so reimplementing the suite is a markup change.
  document.getElementById('tab-reports').hidden = false;
  window.history.pushState({ path: '/reports' }, '', '/reports');
  window.dispatchEvent(new window.PopStateEvent('popstate'));
  assert.ok(tabSelected(document, 'tab-reports'), 'the unhidden tab is selectable again');
  assert.equal(document.getElementById('panel-reports').hidden, false);
  assert.equal(window.location.pathname, '/reports');
  dom.window.close();
});
