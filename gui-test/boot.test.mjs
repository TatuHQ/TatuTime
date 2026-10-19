// #188: GUI boot & async correctness. Boots the real app.js in jsdom and
// guards the fixes: el() style passthrough (chart heights), member-safe boot
// (admin-tier /invoices), dashboard customer filter population, write-off
// lock-cache invalidation, and the day-refresh stale-response guard.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor, yieldToEventLoop } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

function makeDom(prelude = '', url = 'http://localhost/') {
  const dom = new JSDOM(html, { runScripts: 'outside-only', url });
  dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
  // jsdom lays nothing out, so it has no scrollIntoView; the invoice detail
  // panel calls it when it opens (#349). Stub it exactly as gui.mjs does.
  if (!dom.window.Element.prototype.scrollIntoView) dom.window.Element.prototype.scrollIntoView = () => { };
  const exports = `${app}
${prelude}
window.bootTest = {
  dashRenderOverview, refreshInvoices, refreshCustomerPickers,
  writeOffInvoice, navigateDay, weekLockIndex, state,
  refreshWeek, openWeekRowForm, saveWeekRow, fillProjectSelect, refreshTimeCal, calState,
  openInvoicePreview, closeInvoicePreview, showDeliveryRecord, formatTimelineStamp,
  // #349 journeys: the invoice action row, the year stepper and the manual
  // editor are wired by the focused wire*() functions; expose them so a test
  // drives the same listeners the app registers at boot.
  dash, refreshInvoiceSummary, wireInvoiceDashboard, wireWeekGrid,
  iwInit, iwOpen, edOpen, edSave, edCancel, ed,
  peekLockCache: () => weekLockCache,
  liveText: () => document.getElementById('live-region').textContent,
  api,
};`;
  dom.window.eval(exports);
  return { dom, context: dom.window.bootTest, document: dom.window.document };
}

test('the invoice chart renders real bar heights via el() style (#188)', () => {
  const { dom, context, document } = makeDom();
  const invoices = [
    {
      id: 'i1', number: 'INV-0001', status: 'issued', currency: 'EUR',
      period_to: '2026-03-10', total_minor: 180000, payments: [],
    },
    {
      id: 'i2', number: 'INV-0002', status: 'partly_paid', currency: 'EUR',
      period_to: '2026-06-20', total_minor: 90000,
      payments: [{ amount_minor: 50000, received_at: '2026-06-30T00:00:00Z' }],
    },
  ];
  context.dashRenderOverview(invoices);
  const bars = [...document.querySelectorAll('#inv-chart .inv-bar')];
  assert.equal(bars.length, 24, 'twelve months x open+closed');
  for (const bar of bars) {
    assert.match(
      bar.getAttribute('style') || '',
      /height:\s*\d+px/,
      'computed bar height must reach the DOM (#188: el() dropped style)',
    );
  }
  const tallest = Math.max(
    ...bars.map((b) => Number(/height:(\d+)px/.exec(b.getAttribute('style'))[1])),
  );
  assert.ok(tallest > 2, `non-zero data renders taller than the 2px floor (got ${tallest})`);
  dom.window.close();
});
test('the chart labels months in words and names the period on hover (#F)', () => {
  const { dom, context, document } = makeDom();
  context.dashRenderOverview([
    {
      id: 'i1', number: 'INV-0001', status: 'issued', currency: 'EUR',
      period_to: '2026-03-10', total_minor: 180000, payments: [],
    },
  ]);
  // The month axis is a name, not a letter: three-letter codes by default, with
  // a single-letter variant the CSS media query swaps in below 480px. Both are
  // in the DOM; only the full one is announced (the short one is aria-hidden).
  const labels = [...document.querySelectorAll('#inv-chart .inv-clabel-full')];
  assert.deepEqual(
    labels.map((s) => s.textContent),
    ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'],
    '3-letter month codes, January first',
  );
  const shorts = [...document.querySelectorAll('#inv-chart .inv-clabel-short')];
  assert.deepEqual(shorts.map((s) => s.textContent), labels.map((s) => s.textContent[0]),
    'one letter per month');
  for (const short of shorts) {
    assert.equal(short.getAttribute('aria-hidden'), 'true',
      'the short variant is never announced, so no month is read twice');
  }
  // Hovering a bar answers for the whole month: period, open and closed.
  const march = document.querySelectorAll('#inv-chart .inv-col-chart')[2];
  assert.equal(march.querySelector('.inv-bar.open').getAttribute('title'),
    'March 2026 \u2014 Open EUR 1800.00 \u00b7 Closed EUR 0.00');
  assert.equal(march.querySelector('.inv-bar.closed').getAttribute('title'),
    march.querySelector('.inv-bar.open').getAttribute('title'),
    'both bars of a month describe the same month');
  dom.window.close();
});

test('the chart combines currencies into base_currency and prints the rate (#F)', async () => {
  const { dom, context, document } = makeDom(`
api.get = async (path) => {
  if (path === '/admin/config') return { config: [{ key: 'base_currency', value: 'EUR' }] };
  if (path.startsWith('/fx?')) {
    return { from: 'USD', to: 'EUR', rate: '0.921400', date: '2026-10-15', source: 'ECB' };
  }
  throw new Error('unexpected ' + path);
};
`);
  context.dashRenderOverview([
    {
      id: 'e1', number: 'INV-1', status: 'issued', currency: 'EUR',
      period_to: '2026-03-10', total_minor: 100000, payments: [],
    },
    {
      id: 'u1', number: 'INV-2', status: 'issued', currency: 'USD',
      period_to: '2026-03-20', total_minor: 200000, payments: [],
    },
  ]);
  // The first paint is already captioned ("showing EUR only"), so wait for the
  // converted caption itself, not merely for a non-empty note.
  await waitFor(() => /1 USD = 0\.9214 EUR/.test(document.getElementById('inv-chart-note').textContent), {
    message: 'the converted chart to render once the rates resolve',
  });
  // The rate is the published decimal string, with the date it was published for.
  assert.equal(document.getElementById('inv-chart-note').textContent,
    '1 USD = 0.9214 EUR (2026-10-15)');
  // Per-currency sums are converted once, then added: 1000.00 EUR + 1842.80 EUR.
  assert.equal(document.getElementById('inv-chart-title').textContent,
    'Open vs closed (EUR) \u2014 2026');
  const march = document.querySelectorAll('#inv-chart .inv-col-chart')[2];
  assert.equal(march.querySelector('.inv-bar.open').getAttribute('title'),
    'March 2026 \u2014 Open EUR 2842.80 \u00b7 Closed EUR 0.00');
  assert.equal(document.querySelectorAll('#inv-chart .inv-bar').length, 24,
    'a combined chart is still 12 months x 2 series');
  dom.window.close();
});

test('a failing FX table shows the unconverted values and says so (#F)', async () => {
  const { dom, context, document } = makeDom(`
api.get = async (path) => {
  if (path === '/admin/config') return { config: [{ key: 'base_currency', value: 'EUR' }] };
  if (path.startsWith('/fx?')) {
    const err = new Error('no published table');
    err.status = 503;
    throw err;
  }
  throw new Error('unexpected ' + path);
};
`);
  context.dashRenderOverview([
    {
      id: 'e1', number: 'INV-1', status: 'issued', currency: 'EUR',
      period_to: '2026-03-10', total_minor: 100000, payments: [],
    },
    {
      id: 'u1', number: 'INV-2', status: 'issued', currency: 'USD',
      period_to: '2026-03-20', total_minor: 200000, payments: [],
    },
  ]);
  await waitFor(
    () => /Exchange rates unavailable/.test(document.getElementById('inv-chart-note').textContent),
    { message: 'the fail-closed note to replace the chart caption' },
  );
  assert.equal(document.getElementById('inv-chart-note').textContent,
    'Exchange rates unavailable \u2014 showing EUR unconverted.');
  // Never a fabricated 1:1 and never a partial sum: the USD invoice is not
  // silently added to the EUR one, and no rate is invented.
  const march = document.querySelectorAll('#inv-chart .inv-col-chart')[2];
  assert.equal(march.querySelector('.inv-bar.open').getAttribute('title'),
    'March 2026 \u2014 Open EUR 1000.00 \u00b7 Closed EUR 0.00');
  dom.window.close();
});

test('a member boots the full app despite admin-tier 403s (#188)', async () => {
  // The real boot path: jsdom fires DOMContentLoaded, app.js runs
  // initAuth() -> startApp(). Admin-tier endpoints answer exactly what a
  // member sees (403); before #188 the /invoices rejection aborted startApp.
  const stub = `
api.get = async (path) => {
  if (path === '/auth/status') return { initialised: true };
  if (path === '/auth/me') return { id: 'm1', name: 'Mem', email: 'm@x.test', role: 'member' };
  if (path === '/auth/sso/providers') return { providers: [] };
  if (path === '/invoices' || path === '/invoices/summary' || path === '/users'
      || path === '/schedules' || path === '/retainers' || path.startsWith('/admin/')) {
    const err = new Error('administrator role required');
    err.status = 403;
    throw err;
  }
  return { entries: [], customers: [], projects: [], submissions: [], expenses: [],
           claims: [], notifications: [], categories: [], item_types: [], timer: null };
};
api.post = async () => ({});
`;
  const { dom, document } = makeDom(stub);
  // Wait for boot completion (the DOMContentLoaded handler is async): #265.
  await waitFor(() => document.getElementById('version').textContent === 'TucanoTime', {
    message: 'the member boot to complete',
  });
  assert.equal(
    document.getElementById('version').textContent,
    'TucanoTime',
    'startApp must run to completion for members',
  );
  assert.equal(document.getElementById('auth-overlay').hidden, true, 'member is signed in');
  const live = document.getElementById('live-region').textContent;
  assert.ok(!live.includes('Failed to start'), `no boot error announcement (got "${live}")`);
  dom.window.close();
});

test('dashboard customer filter is populated by refreshCustomerPickers (#188)', async () => {
  const { dom, context, document } = makeDom();
  context.state.customers = [
    { id: 'c1', name: 'ACME', currency: 'EUR', active: true, default_rate_minor: 6000 },
    { id: 'c2', name: 'Globex', currency: 'USD', active: true, default_rate_minor: 9000 },
  ];
  await context.refreshCustomerPickers();
  const filter = document.getElementById('inv-f-customer');
  assert.equal(filter.options.length, 3, 'placeholder + two customers');
  assert.equal(filter.options[0].text, 'All customers');
  assert.equal(filter.options[0].value, '', 'placeholder means "no filter"');
  assert.equal(filter.options[1].value, 'c1');
  dom.window.close();
});

test('write-off drops the lock cache because it releases entry locks (#188)', async () => {
  const { dom, context } = makeDom(`
askPrompt = async () => 'customer never paid';
askConfirm = async () => true;
refreshInvoices = async () => {};
`);
  context.api.get = async (path) => {
    if (path === '/submissions') {
      return { submissions: [{ state: 'submitted', entry_ids: ['e1'] }] };
    }
    if (path === '/invoices') return { invoices: [] };
    return {};
  };
  const index = await context.weekLockIndex();
  assert.ok(index.ids.has('e1'), 'lock cache warm from a submitted week');
  assert.ok(context.peekLockCache().ids === index.ids, 'weekLockIndex populated the TTL cache');
  context.api.post = async () => ({});
  await context.writeOffInvoice({ id: 'i1', number: 'INV-0001', currency: 'EUR' }, 6000);
  assert.equal(
    context.peekLockCache().ids,
    null,
    'write-off changes the invoice set (src/lock.rs), so the cache must be invalidated',
  );
  dom.window.close();
});

test('every issued invoice locks its entries — invoiced is permanent (#217)', async () => {
  // The GUI's optimistic lock preview must agree with InvoiceLock: ANY
  // non-draft invoice freezes its lines forever (settling no longer
  // releases them), or the register would offer actions the server 409s.
  const { dom, context } = makeDom();
  context.api.get = async (path) => {
    if (path === '/submissions') return { submissions: [] };
    if (path === '/invoices') {
      return {
        invoices: [
          { id: 'a', number: 'INV-1', status: 'issued', lines: [{ entry_id: 'e-issued' }] },
          { id: 'b', number: 'INV-2', status: 'partly_paid', lines: [{ entry_id: 'e-partly' }] },
          { id: 'c', number: 'INV-3', status: 'paid', lines: [{ entry_id: 'e-paid' }] },
          { id: 'd', number: 'INV-4', status: 'written_off', lines: [{ entry_id: 'e-off' }] },
          { id: 'e', number: 'INV-5', status: 'draft', lines: [{ entry_id: 'e-draft' }] },
        ]
      };
    }
    return {};
  };
  const index = await context.weekLockIndex();
  assert.ok(index.ids.has('e-issued'), 'issued locks');
  assert.ok(index.ids.has('e-partly'), 'partly_paid locks (was missing before #217)');
  assert.ok(index.ids.has('e-paid'), 'paid keeps the lock — invoiced is permanent');
  assert.ok(index.ids.has('e-off'), 'written_off keeps the lock');
  assert.ok(!index.ids.has('e-draft'), 'draft never locks');
  // The invoice map names the freeze's owner for the lock link, first
  // invoice wins when several reference one entry. (Field compares, not
  // deepEqual: the object was built inside the jsdom realm.)
  const paidRef = index.invoiced.get('e-paid');
  assert.equal(paidRef.id, 'c');
  assert.equal(paidRef.number, 'INV-3');
  assert.equal(index.invoiced.has('e-draft'), false, 'drafts never get a link');
  dom.window.close();
});

test('rapid day navigation renders the newest response, not an older one (#188)', async () => {
  const { dom, context, document } = makeDom();
  let releaseStale;
  context.api.get = (path) => {
    if (path.startsWith('/entries?date=')) {
      const date = decodeURIComponent(path.slice('/entries?date='.length));
      const body = {
        entries: [{
          id: `e-${date}`,
          date,
          hours: date === '2026-10-01' ? 1 : 5,
          customer_id: 'c',
          project_code: 'P1',
          billable: true,
          note: '',
        }],
      };
      if (date === '2026-10-01') {
        // The FIRST (older) navigation resolves LAST: hold it open until the
        // newest render has landed, then release it deterministically (#265)
        // instead of racing a fixed sleep. Without the sequence guard its 1h
        // render would clobber the 5h one.
        return new Promise((done) => {
          releaseStale = () => done(body);
        });
      }
      return Promise.resolve(body);
    }
    return Promise.resolve({ submissions: [], invoices: [], notifications: [], entries: [] });
  };
  document.getElementById('day-date').value = '2026-09-30';
  const stale = context.navigateDay(1); // → 10-01, held open
  const newest = context.navigateDay(1); // → 10-02, resolves immediately
  await newest;
  await waitFor(() => document.getElementById('day-total').textContent === '5:00', {
    message: 'the newest day response to render its 5:00 total',
  });
  releaseStale(); // let the stale 10-01 response land last ...
  await stale;    // ... and its (guarded) render attempt run to completion
  await yieldToEventLoop();
  assert.equal(
    document.getElementById('day-total').textContent,
    '5:00',
    'a stale day response must not overwrite the newest render',
  );
  dom.window.close();
});

test('rapid calendar navigation renders the newest month response (#188)', async () => {
  const { dom, context, document } = makeDom();
  context.calState.month = '2026-06';
  let releaseJune;
  context.api.get = (path) => {
    if (path.includes('from=2026-06-01')) {
      // Held open so the older month resolves LAST, deterministically (#265):
      // the newest render must already be on screen when it arrives.
      return new Promise((done) => {
        releaseJune = () => done({
          entries: [{
            id: 'june', date: '2026-06-03', hours: 1, project_code: 'OLD',
          }]
        });
      });
    }
    return Promise.resolve({
      entries: [{
        id: 'july', date: '2026-07-04', hours: 5, project_code: 'NEW',
      }]
    });
  };
  context.calState.month = '2026-06';
  const older = context.refreshTimeCal(); // June, held open
  context.calState.month = '2026-07';
  const newer = context.refreshTimeCal(); // July, resolves immediately
  await newer;
  await waitFor(() => document.getElementById('cal-total').textContent === '5:00', {
    message: 'the newest month response to render its 5:00 total',
  });
  releaseJune(); // the stale June response arrives late ...
  await older;   // ... and its (guarded) render attempt runs to completion
  await yieldToEventLoop();
  assert.equal(document.getElementById('cal-label').textContent, 'Jul 2026');
  assert.equal(document.getElementById('cal-total').textContent, '5:00');
  assert.equal(document.querySelector('#cal-grid .cal-hours')?.textContent, '5:00');
  dom.window.close();
});

test('calendar day cells show a per-day status icon and the day total only (#140)', async () => {
  const { dom, context, document } = makeDom();
  context.calState.month = '2026-06';
  context.api.get = () => Promise.resolve({
    entries: [
      { id: 'a', date: '2026-06-03', hours: 1.5, project_code: 'ALPHA' },
      { id: 'b', date: '2026-06-03', hours: 2, project_code: 'BETA' },
      { id: 'c', date: '2026-06-03', hours: 0.5, project_code: 'GAMMA' },
      { id: 'd', date: '2026-06-03', hours: 0.25, project_code: 'DELTA' },
    ]
  });
  await context.refreshTimeCal();
  const has = document.querySelector('#cal-grid td.cal-cell.has');
  assert.ok(has, 'a day with entries renders as an active cell');
  assert.ok(has.querySelector('.cal-status .p-icon--success'),
    'a logged day shows the success status icon');
  assert.equal(has.querySelector('.cal-hours').textContent, '4:15',
    'the cell shows the day total, never a per-entry list');
  assert.equal(has.querySelectorAll('.cal-entry, .cal-chip').length, 0,
    'no per-project rows or chips are rendered');
  const plain = document.querySelector('#cal-grid td.cal-cell:not(.has) .cal-plain');
  assert.ok(plain.querySelector('.cal-status .p-icon--history'),
    'an empty day shows the history status icon');
  assert.equal(plain.querySelector('.cal-hours'), null, 'an empty day shows no total');
  dom.window.close();
});

test('week column headers mirror the day box and drop the "This week" prefix (#12)', async () => {
  const { dom, context, document } = makeDom();
  context.api.get = async (path) => {
    if (path.startsWith('/submissions')) return { submissions: [] };
    if (path.startsWith('/invoices')) return { invoices: [] };
    if (path.startsWith('/entries')) return {
      entries: [
        { id: 'e1', date: '2026-06-03', hours: 2, customer_id: 'c1', project_code: 'ALPHA' },
      ]
    };
    return {};
  };
  document.getElementById('week-date').value = '2026-06-01'; // a Monday
  await context.refreshWeek();
  const hdr = (date) => document.querySelector(`#week-table thead th[data-date="${date}"]`);
  const wed = hdr('2026-06-03');
  assert.ok(wed.querySelector('.wk-clock .p-icon--success'),
    'a logged day column shows the success status icon');
  assert.ok(hdr('2026-06-01').querySelector('.wk-clock .p-icon--history'),
    'an empty day column shows the history status icon');
  assert.equal(wed.querySelector('.wk-date').textContent, '3 Jun',
    'the date leads the header cell like the Day view');
  assert.equal(document.getElementById('week-label').textContent, '01 Jun \u2013 07 Jun 2026',
    'the week label no longer prefixes "This week"');
  dom.window.close();
});

test('week "+ Add row" appends an empty project row and focuses its first cell (#172 follow-up)', async () => {
  const { dom, context, document } = makeDom();
  // jsdom only focuses an element that is actually rendered, and the Week panel
  // ships hidden — reveal it so the focus-lands-on-the-cell assertion is honest.
  document.getElementById('panel-week').hidden = false;
  context.state.customers = [{ id: 'c1', name: 'ACME', active: true }];
  context.api.get = async (path) => {
    if (path.startsWith('/submissions')) return { submissions: [] };
    if (path.startsWith('/invoices')) return { invoices: [] };
    if (path.startsWith('/entries')) return { entries: [] };
    if (path.endsWith('/tasks')) return { tasks: [] };
    if (path.startsWith('/customers/c1/projects')) {
      return { projects: [{ code: 'P-9', name: 'Site', active: true }] };
    }
    return {};
  };
  document.getElementById('week-date').value = '2026-06-01'; // a Monday
  context.openWeekRowForm();
  const dlg = document.getElementById('week-row-dialog');
  assert.ok(dlg.open === true || dlg.hasAttribute('open'), 'the add-row dialog opens');
  const customer = document.getElementById('week-row-customer');
  assert.equal(customer.value, '', 'the customer starts on the placeholder');
  assert.ok([...customer.options].some((o) => o.value === 'c1'), 'customers are offered');
  assert.ok(!document.getElementById('week-row-hours'), 'the dialog carries no hours field');

  // The customer change handler loads the projects; drive that fill directly.
  customer.value = 'c1';
  await context.fillProjectSelect(document.getElementById('week-row-project'), 'c1', null);
  const project = document.getElementById('week-row-project');
  assert.ok([...project.options].some((o) => o.value === 'P-9'), 'the project list is loaded');
  project.value = 'P-9';

  await context.saveWeekRow({ preventDefault() { } });
  assert.equal(dlg.open === true || dlg.hasAttribute('open'), false, 'the dialog closes on add');
  const row = [...document.querySelectorAll('#week-table tbody th')]
    .find((th) => th.textContent.includes('P-9'));
  assert.ok(row, 'an empty P-9 row is added to the grid');
  const first = row.closest('tr').querySelector('input.cell-input');
  assert.equal(first.value, '', 'the new row starts empty \u2014 hours are typed per cell');
  assert.equal(first.dataset.date, '2026-06-01', 'the first cell is Monday of the displayed week');
  assert.equal(document.activeElement, first, 'focus lands on the first day cell');

  // Re-adding the same line is refused without touching the grid.
  context.openWeekRowForm();
  document.getElementById('week-row-customer').value = 'c1';
  await context.fillProjectSelect(document.getElementById('week-row-project'), 'c1', null);
  document.getElementById('week-row-project').value = 'P-9';
  await context.saveWeekRow({ preventDefault() { } });
  const err = document.getElementById('week-row-error');
  assert.equal(err.hidden, false, 'a duplicate row reports a validation error');
  assert.match(err.textContent, /already in this week/);
  assert.equal(
    [...document.querySelectorAll('#week-table tbody th')]
      .filter((th) => th.textContent.includes('P-9')).length,
    1,
    'the duplicate is not added twice',
  );
  dom.window.close();
});

test('a deep link straight to the Calendar paints the month grid (#140)', async () => {
  // Regression: startApp() painted Day and Week only, and applyRoute() selects
  // the segment with the panel refresh suppressed, so reloading
  // /timesheets/calendar left the month grid empty until the segment was
  // clicked. The static shell's empty <tbody> hid this from structural probes.
  const stub = `
api.get = async (path) => {
  if (path === '/auth/status') return { initialised: true };
  if (path === '/auth/me') return { id: 'm1', name: 'Mem', email: 'm@x.test', role: 'member' };
  if (path === '/auth/sso/providers') return { providers: [] };
  if (path.startsWith('/entries?')) return { entries: [
    { id: 'e1', date: '2026-10-07', hours: 2, customer_id: 'c1', project_code: 'P-9' },
  ] };
  if (path === '/invoices' || path === '/invoices/summary' || path === '/users'
      || path === '/schedules' || path === '/retainers' || path.startsWith('/admin/')) {
    const err = new Error('administrator role required');
    err.status = 403;
    throw err;
  }
  return { entries: [], customers: [], projects: [], submissions: [], expenses: [],
           claims: [], notifications: [], categories: [], item_types: [], timer: null };
};
api.post = async () => ({});
`;
  const { dom, document } = makeDom(stub, 'http://localhost/timesheets/calendar');
  await waitFor(() => document.getElementById('version').textContent === 'TucanoTime', {
    message: 'the boot to complete on the Calendar deep link',
  });
  assert.equal(document.getElementById('ts-cal').getAttribute('aria-selected'), 'true',
    'the deep link selects the Calendar segment');
  assert.equal(document.getElementById('panel-calendar').hidden, false, 'the month panel is shown');
  const cells = document.querySelectorAll('#cal-grid tbody td.cal-cell');
  assert.ok(cells.length >= 28, `the month grid painted its day cells (got ${cells.length})`);
  assert.match(
    document.getElementById('cal-total').textContent,
    /^\d+:\d{2}$/,
    'refreshTimeCal ran on boot (the Total hours readout is painted)',
  );
  dom.window.close();
});

// ------------------------------------------- invoice detail panel (#349) ----
//
// The detail panel shows the invoice's rendered PDF (framed from a client-side
// blob: URL, because the global X-Frame-Options: DENY refuses the API path
// itself) and its activity history. These run offline in jsdom: the blob fetch
// is stubbed, the URL factory is stubbed (jsdom has no blob URLs), and the
// history comes from a stubbed response so the shape is pinned exactly.

test('the detail panel frames the rendered PDF and renders the history (#349)', async () => {
  const { dom, context, document } = makeDom(`
api.getBlob = async (path) => {
  if (path !== '/invoices/i1/pdf/preview') throw new Error('unexpected blob ' + path);
  // "%PDF-1.4"
  return new Blob([new Uint8Array([0x25, 0x50, 0x44, 0x46, 0x2d, 0x31, 0x2e, 0x34])], { type: 'application/pdf' });
};
api.get = async (path) => {
  if (path !== '/invoices/i1/timeline') throw new Error('unexpected ' + path);
  return { events: [
    { kind: 'email_sent', description: 'Sent invoice to ap@x.test, from Acme <a@x.test>.',
      actor_name: 'Emanuele Ciurleo', at: '2026-10-01T11:26:00Z',
      meta: { to: 'ap@x.test', subject: 'INV-0001', from_name: 'Acme', from_email: 'a@x.test', transport: 'smtp' } },
    { kind: 'created', description: 'Invoice created.', actor_name: 'Emanuele Ciurleo', at: '2026-09-28T10:12:00Z' },
  ] };
};
`);
  dom.window.URL.createObjectURL = () => 'blob:test-1';
  dom.window.URL.revokeObjectURL = () => { };
  await context.openInvoicePreview({
    id: 'i1', number: 'INV-0001', status: 'issued', currency: 'EUR',
    period_from: '2026-09-01', period_to: '2026-09-30', total_minor: 1000, lines: [],
  }, null);
  assert.equal(document.getElementById('invoice-preview').hidden, false, 'the panel opens');
  const frame = document.getElementById('ip-frame');
  assert.ok(frame, 'the rendered PDF is framed');
  assert.equal(frame.getAttribute('src'), 'blob:test-1', 'the frame shows the client-side blob URL');
  assert.equal(document.getElementById('ip-pdf').children.length, 1,
    'the preview region holds the frame and nothing else (#349)');
  assert.equal(document.getElementById('ip-pdf-state').hidden, true, 'no state line while the PDF shows');
  assert.equal(document.getElementById('ip-download').hidden, false,
    'an issued invoice can still download its archived PDF (#113)');
  const items = [...document.querySelectorAll('#ip-timeline li')];
  assert.equal(items.length, 2, 'both events render, newest first');
  assert.match(items[0].querySelector('.ip-tl-desc').textContent, /^Sent invoice to/);
  assert.equal(items[0].querySelector('.ip-tl-actor').textContent, 'Emanuele Ciurleo');
  const when = items[0].querySelector('.ip-tl-when time');
  assert.equal(when.getAttribute('datetime'), '2026-10-01T11:26:00Z',
    'the machine-readable RFC 3339 value is preserved on <time datetime>');
  // The display shape, computed independently with the same Intl rules so the
  // assertion holds whatever the runner's timezone is: "on Oct 1, 2026 at 11:26 AM".
  const d = new Date('2026-10-01T11:26:00Z');
  const want = `on ${new Intl.DateTimeFormat('en-US', { month: 'short', day: 'numeric', year: 'numeric' }).format(d)}`;
  const wantTime = new Intl.DateTimeFormat('en-US', { hour: 'numeric', minute: '2-digit', hour12: true }).format(d);
  assert.equal(when.textContent, `${want} at ${wantTime}`);
  assert.match(when.textContent, /^on [A-Z][a-z]{2} \d{1,2}, \d{4} at \d{1,2}:\d{2} (AM|PM)$/);
  // View message: the delivery record only, and only on the delivery.
  const buttons = [...document.querySelectorAll('#ip-timeline button')];
  assert.equal(buttons.length, 1, 'only the email_sent entry offers View message');
  assert.equal(buttons[0].textContent, 'View message');
  assert.equal(buttons[0].tagName, 'BUTTON', 'it is a button, never a navigating link');
  buttons[0].dispatchEvent(new dom.window.Event('click', { bubbles: true }));
  const rec = document.getElementById('message-fields');
  assert.match(rec.textContent, /ap@x\.test/, 'the recipient that was actually used');
  assert.match(rec.textContent, /INV-0001/, 'the retained subject');
  assert.match(rec.textContent, /Acme <a@x\.test>/, 'the From identity');
  assert.ok([...rec.children].every((n) => n.children.length === 0), 'the record is text only');
  assert.match(document.getElementById('message-note').textContent, /not retained/);
  assert.ok(!/Apologies|Dear/.test(document.getElementById('message-dialog').textContent),
    'the popup never fabricates a message body');
  // Closing releases the frame and the history.
  context.closeInvoicePreview();
  assert.equal(document.getElementById('invoice-preview').hidden, true);
  assert.equal(document.getElementById('ip-pdf').children.length, 0, 'the frame is released on close');
  assert.equal(document.querySelectorAll('#ip-timeline li').length, 0);
  dom.window.close();
});

test('the frozen subject rides the register row and the detail panel (#354)', async () => {
  const subject = 'Acme Widgets GmbH invoice for 08 2026';
  const { dom, context, document } = makeDom(`
api.get = async (path) => {
  if (path === '/invoices') return { invoices: [
    { id: 'i1', number: 'INV-0001', status: 'issued', customer_id: 'c1', currency: 'EUR',
      period_from: '2026-08-01', period_to: '2026-08-31', total_minor: 1000, payments: [],
      subject: ${JSON.stringify(subject)} },
    { id: 'i2', number: 'INV-0002', status: 'draft', customer_id: 'c1', currency: 'EUR',
      period_from: '2026-09-01', period_to: '2026-09-30', total_minor: 1000, payments: [], subject: '' },
  ] };
  if (path === '/invoices/summary') return { draft: 1, issued: 1, overdue: 0, partly_paid: 0, paid: 0, written_off: 0, outstanding: {} };
  if (path === '/customers') return { customers: [{ id: 'c1', name: 'Acme Widgets GmbH', currency: 'EUR', default_rate_minor: 0, active: true }] };
  if (path.endsWith('/timeline')) return { events: [] };
  if (path === '/admin/config') return { config: [] };
  return {};
};
api.getBlob = async () => new Blob([new Uint8Array([0x25, 0x50, 0x44, 0x46, 0x2d])], { type: 'application/pdf' });
`);
  dom.window.URL.createObjectURL = () => 'blob:subject';
  dom.window.URL.revokeObjectURL = () => { };
  context.state.isAdmin = true;
  await context.refreshInvoices();
  await waitFor(() => document.querySelectorAll('#invoice-table tbody tr').length === 2, {
    message: 'the register to render both invoices',
  });
  const rows = [...document.querySelectorAll('#invoice-table tbody tr')];
  assert.equal(rows[0].querySelector('.inv-row-subject').textContent, subject,
    'the issued invoice shows its frozen subject as a subtitle');
  assert.match(rows[0].getAttribute('aria-label'), /Acme Widgets GmbH invoice for 08 2026/);
  assert.equal(rows[1].querySelector('.inv-row-subject'), null,
    'a draft has no frozen subject yet');

  await context.openInvoicePreview({
    id: 'i1', number: 'INV-0001', status: 'issued', currency: 'EUR',
    total_minor: 1000, payments: [], subject,
  }, null);
  assert.equal(document.getElementById('ip-subject').hidden, false);
  assert.equal(document.getElementById('ip-subject').textContent, subject);
  assert.match(document.getElementById('ip-document-note').textContent, /fixed at issue/i);

  await context.openInvoicePreview({
    id: 'i2', number: 'INV-0002', status: 'draft', currency: 'EUR', total_minor: 1000, payments: [],
  }, null);
  assert.equal(document.getElementById('ip-subject').hidden, true, 'a draft shows no frozen subject');
  assert.match(document.getElementById('ip-document-note').textContent, /frozen when you issue it/i);
  context.closeInvoicePreview();
  assert.equal(document.getElementById('ip-subject').hidden, true, 'closing clears the subject line');
  assert.equal(document.getElementById('ip-document-note').hidden, true);
  dom.window.close();
});

test('a non-PDF preview body is reported, never framed (#349)', async () => {
  const { dom, context, document } = makeDom(`
api.getBlob = async () => new Blob([new Uint8Array([0x7b, 0x22, 0x65])], { type: 'application/json' });
api.get = async () => ({ events: [] });
`);
  dom.window.URL.createObjectURL = () => 'blob:never';
  await context.openInvoicePreview({ id: 'x1', number: 'INV-9', status: 'draft' }, null);
  assert.equal(document.getElementById('ip-pdf').children.length, 0, 'a non-PDF body is never framed');
  assert.equal(document.getElementById('ip-pdf-state').hidden, false);
  assert.match(document.getElementById('ip-pdf-state').textContent, /did not return a PDF/);
  assert.equal(document.getElementById('ip-download').hidden, true, 'a draft has no archived PDF to download');
  assert.equal(document.getElementById('ip-timeline-state').textContent, 'No history yet.',
    'an empty history says so instead of rendering a fabricated entry');
  dom.window.close();
});

test('a superseded preview render revokes the URL it minted (#C4)', async () => {
  // Closing the panel and reopening the *same* invoice while the first fetch is
  // still in flight used to pass the id guard, so the older response overwrote
  // previewPdfUrl without revoking the URL the newer render had minted. The
  // render counter makes the older one revoke its own URL instead.
  const { dom, context, document } = makeDom(`
let releaseFirst;
const firstGate = new Promise((resolve) => { releaseFirst = resolve; });
let call = 0;
api.getBlob = async () => {
  call += 1;
  // "%PDF-1.4"
  const bytes = new Uint8Array([0x25, 0x50, 0x44, 0x46, 0x2d, 0x31, 0x2e, 0x34]);
  if (call === 1) {
    // The first render is held inside its own arrayBuffer() await, so the
    // supersession lands after it has already passed the first guard.
    return { type: 'application/pdf', arrayBuffer: () => firstGate.then(() => bytes.buffer) };
  }
  return new Blob([bytes], { type: 'application/pdf' });
};
api.get = async () => ({ events: [] });
window.releaseFirst = () => releaseFirst();
`);
  const created = [];
  const revoked = [];
  dom.window.URL.createObjectURL = () => {
    const url = `blob:render-${created.length + 1}`;
    created.push(url);
    return url;
  };
  dom.window.URL.revokeObjectURL = (url) => revoked.push(url);
  const inv = { id: 'i1', number: 'INV-0001', status: 'issued' };

  const superseded = context.openInvoicePreview(inv, null); // render 1, held open
  await yieldToEventLoop(); // it reaches the arrayBuffer() await, past the guard
  context.closeInvoicePreview();
  await context.openInvoicePreview(inv, null); // render 2, the same invoice

  assert.equal(document.getElementById('ip-frame').getAttribute('src'), 'blob:render-1',
    'the surviving frame is the newest render');
  dom.window.releaseFirst(); // render 1 resumes and finds itself superseded
  await superseded;

  assert.deepEqual(created, ['blob:render-1', 'blob:render-2'],
    'each render minted exactly one URL');
  assert.deepEqual(revoked, ['blob:render-2'],
    "the superseded render's URL is revoked, not left pinned");
  assert.equal(document.getElementById('ip-frame').getAttribute('src'), 'blob:render-1',
    'the newer frame still displays after the stale render resolves');
  assert.equal(document.querySelectorAll('#ip-pdf iframe').length, 1, 'one frame, never two');
  context.closeInvoicePreview();
  assert.deepEqual([...revoked].sort(), ['blob:render-1', 'blob:render-2'],
    'every minted URL is revoked exactly once');
  dom.window.close();
});

test('a failed preview or history fetch is reported honestly (#349)', async () => {
  const { dom, context, document } = makeDom(`
api.getBlob = async () => { const e = new Error('HTTP 403'); e.status = 403; throw e; };
api.get = async () => { const e = new Error('GET /api/invoices/x/timeline failed: HTTP 500'); e.status = 500; throw e; };
`);
  await context.openInvoicePreview({ id: 'x1', number: 'INV-9', status: 'issued' }, null);
  assert.equal(document.getElementById('ip-pdf').children.length, 0);
  assert.match(document.getElementById('ip-pdf-state').textContent, /Preview unavailable/);
  assert.match(document.getElementById('ip-pdf-state').textContent, /HTTP 403/);
  assert.match(document.getElementById('ip-timeline-state').textContent, /History unavailable/);
  assert.match(document.getElementById('ip-timeline-state').textContent, /HTTP 500/);
  assert.equal(document.querySelectorAll('#ip-timeline li').length, 0, 'a failure fabricates nothing');
  dom.window.close();
});

test('the timestamp helper is the requested "on Oct 1, 2026 at 11:26 AM" shape (#349)', () => {
  const { dom, context } = makeDom();
  const d = new Date('2026-10-01T11:26:00Z');
  const date = new Intl.DateTimeFormat('en-US', { month: 'short', day: 'numeric', year: 'numeric' }).format(d);
  const time = new Intl.DateTimeFormat('en-US', { hour: 'numeric', minute: '2-digit', hour12: true }).format(d);
  assert.equal(context.formatTimelineStamp('2026-10-01T11:26:00Z'), `on ${date} at ${time}`);
  assert.match(context.formatTimelineStamp('2026-09-28T10:12:00Z'), /^on Sep 28, 2026 at /);
  assert.match(context.formatTimelineStamp('2026-09-28T10:12:00Z'), / (AM|PM)$/);
  dom.window.close();
});

// ------------------------------- creation journeys + trends gaps (#349) ----
//
// The batch's own suites drive these journeys live in the browser gate; the
// tests here pin the *same* behaviours fast and deterministically in jsdom, and
// close the two gaps the batch's jsdom suites left open: the year stepper
// actually moving the year, and the unset-base-currency honesty for a mixed
// set. Every assertion reads a real property of the rendered DOM.

test('+ Invoice opens the tracked-work wizard as a modal at step 1 and + Manual Invoice opens the editor (#349)', () => {
  const { dom, context, document } = makeDom();
  context.state.customers = [{ id: 'c1', name: 'ACME', currency: 'EUR' }];
  context.iwInit();
  context.wireWeekGrid();
  const wizard = document.getElementById('invoice-wizard');
  const wizardOpen = () => wizard.open === true || wizard.hasAttribute('open');
  assert.equal(wizardOpen(), false, 'the wizard dialog starts closed -- never open at boot');
  assert.ok(wizard.classList.contains('p-modal'), 'the wizard is the p-modal overlay');
  assert.equal(wizard.querySelector('h3'), null, 'the wizard no longer renders as the inline p-card');
  document.getElementById('invoice-new').dispatchEvent(new dom.window.Event('click', { bubbles: true }));
  assert.equal(wizardOpen(), true, '+ Invoice opens the wizard as a modal');
  assert.ok(wizard.querySelector('.p-modal__dialog .p-modal__footer'),
    'the wizard renders inside a p-modal__dialog with a footer');
  assert.equal(document.getElementById('iw-step1').hidden, false, 'step 1 is shown');
  assert.equal(document.getElementById('iw-step2').hidden, true, 'step 2 is still hidden');
  assert.equal(document.getElementById('iw-step3').hidden, true, 'step 3 is still hidden');
  assert.match(document.getElementById('iw-title').textContent, /^Create from tracked work \u2014 1 of 3:/);
  // Closing the dialog discards nothing: the step is preserved, so re-pressing
  // + Invoice resumes where the user left off (never a half-entered restart).
  document.getElementById('iw-close').dispatchEvent(new dom.window.Event('click', { bubbles: true }));
  assert.equal(wizardOpen(), false, 'Close dismisses the wizard dialog');
  document.getElementById('invoice-new').dispatchEvent(new dom.window.Event('click', { bubbles: true }));
  assert.equal(wizardOpen(), true, '+ Invoice reopens the wizard');
  assert.equal(document.getElementById('iw-step1').hidden, false, 'the wizard step is preserved');
  // The shared keydown Escape fallback (jsdom never fires `cancel`) is wired too.
  wizard.dispatchEvent(new dom.window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  assert.equal(wizardOpen(), false, 'Escape closes the wizard dialog');
  document.getElementById('invoice-new').dispatchEvent(new dom.window.Event('click', { bubbles: true }));
  assert.equal(wizardOpen(), true, 'the wizard stays usable after Escape');
  const dlg = document.getElementById('invoice-editor');
  const isOpen = () => dlg.open === true || dlg.hasAttribute('open');
  assert.equal(isOpen(), false, 'the editor starts closed');
  document.getElementById('manual-new').dispatchEvent(new dom.window.Event('click', { bubbles: true }));
  assert.equal(isOpen(), true, '+ Manual Invoice opens the manual editor modal');
  assert.equal(document.getElementById('ed-title').textContent, 'Manual invoice');
  assert.equal(document.getElementById('ed-lines').querySelectorAll('tbody tr').length, 1, 'one empty line row');
  dom.window.close();
});

// The editor's select has no options in jsdom (the app fills it from the server
// at boot); add the one option a real session would already carry, so the value
// the Save reads is the one a user typed.
function addCustomerOption(document, id, label) {
  const opt = document.createElement('option');
  opt.value = id;
  opt.textContent = label;
  document.getElementById('ed-customer').appendChild(opt);
}

test('the manual editor Cancel saves nothing and Save posts exactly one manual draft (#349/#143)', async () => {
  const { dom, context, document } = makeDom('refreshInvoices = async () => {};');
  context.state.customers = [{ id: 'c1', name: 'ACME', currency: 'EUR' }];
  context.wireWeekGrid();
  const posts = [];
  context.api.post = async (path, body) => {
    posts.push({ path, body });
    return { number: 'INV-0042', total_minor: 2000, currency: 'EUR' };
  };
  const dlg = document.getElementById('invoice-editor');
  const isOpen = () => dlg.open === true || dlg.hasAttribute('open');
  const openEditor = () => document.getElementById('manual-new')
    .dispatchEvent(new dom.window.Event('click', { bubbles: true }));

  // Cancel is the explicit way out: it discards the draft with no request at
  // all (the "no partial persistence" rule -- nothing half-saved to undo).
  openEditor();
  addCustomerOption(document, 'c1', 'ACME');
  document.getElementById('ed-customer').value = 'c1';
  document.getElementById('ed-lines').querySelector('tbody tr')
    .querySelector('input[type=text]').value = 'Should not persist';
  document.getElementById('ed-cancel').dispatchEvent(new dom.window.Event('click', { bubbles: true }));
  assert.equal(isOpen(), false, 'Cancel closes the editor');
  assert.equal(posts.length, 0, 'Cancel issues no request at all');
  assert.match(context.liveText(), /cancelled/);

  // Save: exactly one POST /invoices/manual, carrying the line as typed, with
  // money in integer hundredths/minor units.
  openEditor();
  addCustomerOption(document, 'c1', 'ACME');
  document.getElementById('ed-customer').value = 'c1';
  const row = document.getElementById('ed-lines').querySelector('tbody tr');
  row.querySelector('input[type=text]').value = 'Consulting';
  const nums = row.querySelectorAll('input[type=number]');
  nums[0].value = '2';      // quantity
  nums[1].value = '10.00';  // unit price
  await context.edSave();
  assert.equal(posts.length, 1, 'Save issues exactly one request');
  assert.equal(posts[0].path, '/invoices/manual');
  assert.equal(posts[0].body.customer_id, 'c1');
  assert.equal(posts[0].body.lines.length, 1);
  assert.equal(posts[0].body.lines[0].description, 'Consulting');
  assert.equal(posts[0].body.lines[0].quantity_hundredths, 200, 'quantity in hundredths, never a float');
  assert.equal(posts[0].body.lines[0].unit_price_minor, 1000, 'unit price in minor units');
  assert.equal(isOpen(), false, 'Save closes the editor');
  assert.match(context.liveText(), /Draft INV-0042 saved/);
  dom.window.close();
});

test('the .day-nav year stepper moves the chart to another year (#F)', async () => {
  const { dom, context, document } = makeDom(`
const __YEAR__ = new Date().getUTCFullYear();
api.get = async (path) => {
  if (path === '/invoices/summary') return { draft: 0, issued: 1, paid: 0, outstanding: {} };
  if (path === '/invoices') return { invoices: [{
    id: 'i1', number: 'INV-0001', status: 'issued', currency: 'EUR',
    period_to: __YEAR__ + '-03-10', total_minor: 180000, payments: [],
  }] };
  if (path === '/admin/config') return { config: [] };
  throw new Error('unexpected ' + path);
};
`);
  context.state.customers = [];
  context.wireInvoiceDashboard();
  const year = new Date().getUTCFullYear();
  context.dash.year = year;
  await context.refreshInvoiceSummary();
  assert.equal(document.getElementById('inv-year-label').textContent, String(year));
  const marchOpen = () => document.querySelectorAll('#inv-chart .inv-col-chart')[2]
    .querySelector('.inv-bar.open').getAttribute('title');
  assert.equal(marchOpen(), `March ${year} \u2014 Open EUR 1800.00 \u00b7 Closed EUR 0.00`);
  // The stepper is the app's own .day-nav period bar (chevron buttons).
  const nav = document.getElementById('inv-year-prev').parentElement;
  assert.equal(nav.tagName, 'NAV');
  assert.ok(nav.className.includes('day-nav'), 'the year stepper rides the shared .day-nav bar');

  const click = (id) => document.getElementById(id)
    .dispatchEvent(new dom.window.Event('click', { bubbles: true }));
  click('inv-year-prev');
  await waitFor(() => document.getElementById('inv-year-label').textContent === String(year - 1), {
    message: 'the year stepper to move the chart back a year',
  });
  assert.equal(context.dash.year, year - 1, 'the stepper decrements the rendered year');
  assert.match(document.getElementById('inv-chart-title').textContent, new RegExp(String(year - 1)));
  assert.equal(marchOpen(), `March ${year - 1} \u2014 Open EUR 0.00 \u00b7 Closed EUR 0.00`,
    'the chart re-renders for the new year -- last year holds no March data');
  click('inv-year-next');
  await waitFor(() => document.getElementById('inv-year-label').textContent === String(year));
  click('inv-year-next');
  await waitFor(() => document.getElementById('inv-year-label').textContent === String(year + 1));
  assert.equal(context.dash.year, year + 1, 'the stepper increments past the original year');
  dom.window.close();
});

test('a mixed-currency set with no base currency says so and drops no currency silently (#F)', () => {
  const { dom, context, document } = makeDom(`
api.get = async (path) => {
  if (path === '/admin/config') return { config: [] };
  throw new Error('unexpected ' + path);
};
`);
  const year = new Date().getUTCFullYear();
  context.dashRenderOverview([
    { id: 'e1', number: 'INV-1', status: 'issued', currency: 'EUR', period_to: `${year}-03-10`, total_minor: 100000, payments: [] },
    { id: 'u1', number: 'INV-2', status: 'issued', currency: 'USD', period_to: `${year}-03-20`, total_minor: 200000, payments: [] },
  ]);
  // The first paint is synchronous and names what is on screen and why --
  // never a fabricated rate, never a silently dropped currency (#F).
  assert.equal(document.getElementById('inv-chart-note').textContent,
    'Showing EUR only \u2014 set a base currency in Settings to combine EUR, USD.');
  const march = document.querySelectorAll('#inv-chart .inv-col-chart')[2];
  assert.equal(march.querySelector('.inv-bar.open').getAttribute('title'),
    `March ${year} \u2014 Open EUR 1000.00 \u00b7 Closed EUR 0.00`,
    'the USD invoice is not silently added to the EUR one');
  dom.window.close();
});

test('the single-letter month variant is hidden by default, revealed only at narrow widths (#F)', () => {
  const css = readFileSync(new URL('../web/styles.css', import.meta.url), 'utf8');
  assert.match(css, /\.inv-clabel-short \{ display: none; \}/,
    'the one-letter variant is off by default, so the three-letter name is what shows');
  assert.match(css, /\.inv-clabel-full \{ display: none; \}\s*\.inv-clabel-short \{ display: inline; \}/,
    'below 480px the one-letter variant replaces the three-letter one');
});
