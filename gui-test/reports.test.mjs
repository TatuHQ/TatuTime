// Reports: the shared monthly time report and the registry interface every
// future report plugs into. Offline jsdom against the real index.html and the
// real modules, with the API stubbed at the fixture boundary.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

const fixture = (overrides = {}) => ({
  group: 'project',
  from: '2026-10-01',
  to: '2026-11-30',
  months: ['2026-10', '2026-11'],
  rows: [
    {
      key: 'c1:P1', label: 'TatuTime / P1', hours: 3.5, entries: 2,
      months: [
        { month: '2026-10', hours: 2 },
        { month: '2026-11', hours: 1.5 },
      ],
    },
    {
      key: 'c2:G1', label: 'Globex / G1', hours: 1, entries: 1,
      months: [
        { month: '2026-10', hours: 1 },
        { month: '2026-11', hours: 0 },
      ],
    },
  ],
  total_hours: 4.5,
  billable_hours: 3,
  nonbillable_hours: 1.5,
  ...overrides,
});

function setup() {
  const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/reports' });
  const stub = `
window.rp = { api, initReports, runReport, REPORTS };
`;
  dom.window.eval(`${app}\n${stub}`);
  dom.window.rp.initReports();
  return { dom, document: dom.window.document, rp: dom.window.rp };
}

async function submit(document, values = {}) {
  document.getElementById('report-from').value = values.from || '2026-10-01';
  document.getElementById('report-to').value = values.to || '2026-11-30';
  if (values.group) document.getElementById('report-filter-group').value = values.group;
  document.getElementById('report-form')
    .dispatchEvent(new document.defaultView.Event('submit', { bubbles: true, cancelable: true }));
}

test('the monthly report renders a stacked chart above the exact table', async () => {
  const { dom, document, rp } = setup();
  const requested = [];
  rp.api.get = async (path) => { requested.push(path); return fixture(); };

  assert.deepEqual(
    [...document.querySelectorAll('#report-kind option')].map((o) => o.textContent),
    ['Time by month'],
    'the picker comes from the report registry',
  );
  assert.deepEqual(
    [...document.querySelectorAll('#report-filter-group option')].map((o) => o.textContent),
    ['Customer', 'Project', 'Task'],
    'the report contributes its own grouping control',
  );

  await submit(document, { group: 'project' });
  await waitFor(() => document.querySelectorAll('#report-table tbody tr').length === 2, {
    message: 'the monthly table to render',
  });

  const reportCall = requested.find((path) => path.startsWith('/reports/monthly?'));
  assert.ok(reportCall, 'the API path is the monthly report');
  assert.match(reportCall, /group=project/, 'the selected grouping reaches the query');

  const columns = document.querySelectorAll('#report-chart .report-month-col');
  assert.equal(columns.length, 2, 'one chart column per month');
  assert.equal(columns[0].querySelectorAll('.report-bar').length, 2, 'October stacks both groups');
  assert.equal(columns[1].querySelectorAll('.report-bar').length, 1, 'November omits a zero segment');
  assert.equal(document.querySelectorAll('#report-legend .report-legend-item').length, 2);
  assert.match(document.getElementById('report-chart').getAttribute('aria-label'),
    /Oct 2026: 3\.00 hours/);

  assert.deepEqual(
    [...document.querySelectorAll('#report-table-head th')].map((th) => th.textContent),
    ['Group', 'Oct 2026', 'Nov 2026', 'Total'],
  );
  const body = document.querySelector('#report-table tbody');
  assert.equal(body.rows.length, 2);
  assert.equal(body.rows[0].cells[0].textContent, 'TatuTime / P1');
  assert.equal(body.rows[0].cells[1].textContent, '2.00');
  assert.equal(body.rows[1].cells[2].textContent, '\u2014', 'a zero month reads as a dash');
  assert.deepEqual(
    [...document.querySelectorAll('#report-table tfoot td')].map((td) => td.textContent),
    ['3.00', '1.50', '4.50'],
    'the footer sums every row per month and in total',
  );
  assert.equal(document.getElementById('report-total').textContent,
    '4.50 h total (billable 3.00 / non-billable 1.50)');
  assert.equal(document.getElementById('report-empty').hidden, true);
  dom.window.close();
});

test('an empty period keeps the table hidden and says so', async () => {
  const { dom, document, rp } = setup();
  rp.api.get = async () => fixture({ rows: [], total_hours: 0, billable_hours: 0, nonbillable_hours: 0 });
  await submit(document);
  await waitFor(() => document.getElementById('report-empty').hidden === false, {
    message: 'the empty state to render',
  });
  assert.equal(document.getElementById('report-table').hidden, true);
  assert.match(document.getElementById('report-chart').textContent, /No hours in this period/);
  assert.equal(document.getElementById('report-empty').textContent,
    'No time recorded in this period for this grouping.');
  dom.window.close();
});

test('more than eight groups collapse into Other on the chart, never in the table', async () => {
  const { dom, document, rp } = setup();
  const rows = Array.from({ length: 10 }, (_, i) => ({
    key: `c${i}`, label: `Group ${i}`, hours: 1, entries: 1,
    months: [
      { month: '2026-10', hours: 1 },
      { month: '2026-11', hours: 0 },
    ],
  }));
  rp.api.get = async () => fixture({ rows, total_hours: 10, billable_hours: 10, nonbillable_hours: 0 });
  await submit(document);
  await waitFor(() => document.querySelectorAll('#report-table tbody tr').length === 10, {
    message: 'all rows to render in the table',
  });
  assert.equal(document.querySelectorAll('#report-legend .report-legend-item').length, 9,
    'the top eight plus Other');
  assert.match(document.querySelectorAll('#report-legend .report-legend-item')[8].textContent,
    /Other \(2 groups\)/);
  assert.match(document.getElementById('report-chart-note').textContent,
    /top 8 groups; 2 more/);
  dom.window.close();
});

test('a report is a registry entry: the shell renders a new one without code changes', async () => {
  const { dom, document, rp } = setup();
  rp.REPORTS.dummy = {
    label: 'Dummy report',
    filters: [{ id: 'mode', label: 'Mode', value: 'a', options: [{ value: 'a', text: 'A' }] }],
    async load() { return { marker: 'loaded' }; },
    render(data) { document.getElementById('report-chart-title').textContent = `Dummy ${data.marker}`; },
  };
  rp.initReports();
  assert.deepEqual(
    [...document.querySelectorAll('#report-kind option')].map((o) => o.textContent),
    ['Time by month', 'Dummy report'],
  );
  document.getElementById('report-from').value = '2026-10-01';
  document.getElementById('report-to').value = '2026-11-30';
  document.getElementById('report-kind').value = 'dummy';
  document.getElementById('report-kind')
    .dispatchEvent(new dom.window.Event('change', { bubbles: true }));
  await waitFor(() => document.getElementById('report-chart-title').textContent === 'Dummy loaded', {
    message: 'the registry report to load and render',
  });
  assert.equal(document.getElementById('report-filter-mode').value, 'a',
    'the new report receives its declared filter control');
  dom.window.close();
});
