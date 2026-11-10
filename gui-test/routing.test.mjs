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
  if (path.startsWith('/reports/monthly')) return {
    group: 'customer', from: '2026-01-01', to: '2026-12-31', months: [], rows: [],
    total_hours: 0, billable_hours: 0, nonbillable_hours: 0,
  };
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
  // Reports is a real tool now, so popstate opens its panel.
  window.history.pushState({ path: '/reports' }, '', '/reports');
  window.dispatchEvent(new window.PopStateEvent('popstate'));
  assert.ok(tabSelected(document, 'tab-reports'), 'Reports is restored from the URL');
  assert.equal(document.getElementById('panel-reports').hidden, false, 'its panel opens');
  assert.equal(window.location.pathname, '/reports');
  dom.window.close();
});

test('a Reports deep link opens the section at boot', async () => {
  const dom = await bootAt('http://localhost/reports');
  const { document } = dom.window;
  assert.ok(tabSelected(document, 'tab-reports'), 'the Reports tab is selected');
  assert.equal(document.getElementById('panel-reports').hidden, false, 'the Reports panel is open');
  assert.equal(document.getElementById('page-title').textContent, 'Reports');
  assert.equal(dom.window.location.pathname, '/reports', 'the canonical URL is kept');
  dom.window.close();
});

test('mobile shortcuts open daily time and expenses and follow Back navigation', async () => {
  const dom = await bootAt('http://localhost/timesheets/week');
  const { window, document } = dom.window;
  document.getElementById('mobile-time').click();
  assert.equal(window.location.pathname, '/timesheets/day');
  assert.ok(tabSelected(document, 'ts-day'));
  assert.equal(document.getElementById('mobile-time').getAttribute('aria-current'), 'page');
  document.getElementById('mobile-expenses').click();
  assert.equal(window.location.pathname, '/expenses');
  assert.equal(document.getElementById('mobile-expenses').getAttribute('aria-current'), 'page');
  assert.equal(document.getElementById('mobile-time').hasAttribute('aria-current'), false);
  window.history.pushState({}, '', '/timesheets/week');
  window.dispatchEvent(new window.PopStateEvent('popstate'));
  assert.equal(document.getElementById('mobile-time').hasAttribute('aria-current'), false);
  assert.equal(document.getElementById('mobile-more').getAttribute('aria-current'), 'page');
  dom.window.close();
});

test('mobile invoice shortcut preserves member permissions', async () => {
  const dom = await bootAt('http://localhost/');
  assert.equal(dom.window.document.getElementById('mobile-invoices').hidden, true);
  dom.window.close();
});
