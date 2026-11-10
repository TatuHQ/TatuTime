// reports.js — the Reports section and its report registry.
//
// Every report follows one interface so a new report is an entry here plus a
// render function, never a new view or a fork of the filter plumbing:
//
//   key      stable value in REPORTS (the picker option value)
//   label    what the picker and announcements call it
//   filters  extra controls rendered into the shared filter row
//   load     (range, values) -> JSON from the API
//   render   (data, range, values) -> chart + table
//   csv      optional (range, values) -> export URL for the shared link
//
// The range (From/To) and the picker belong to the shell; a report owns only
// what is genuinely different about it.
import { API_BASE, api } from './api.js';
import { $, announce, el, panelRefresh } from './dom.js';
import { MONTH_NAMES } from './timesheet.js';

const SERIES_LIMIT = 8;
const CHART_HEIGHT = 170; // px, the tallest stacked column

const monthLabel = (month) => {
  const [year, number] = String(month).split('-');
  const index = Number(number) - 1;
  return `${MONTH_NAMES[index] || number} ${year}`;
};

const hours = (value) => (Math.round(Number(value || 0) * 100) / 100).toFixed(2);

const groupLabel = (group) => ({
  customer: 'Customer',
  project: 'Project',
  task: 'Task',
})[group] || group;

// Palette classes are 1-based (series-1 … series-8); the combined "Other"
// series stays neutral so it reads as a remainder rather than another customer.
const seriesClass = (row, index) =>
  row.key === '__other__' ? 'series-other' : `series-${index + 1}`;

// ------------------------------------------------------------ time by month --

/// One row per group and one cell per month. Values are already ordered by
/// total hours (the API does that), so the chart's stacking order is stable.
export const REPORTS = {
  'time-by-month': {
    label: 'Time by month',
    filters: [
      {
        id: 'group',
        label: 'Group by',
        value: 'customer',
        options: [
          { value: 'customer', text: 'Customer' },
          { value: 'project', text: 'Project' },
          { value: 'task', text: 'Task' },
        ],
      },
      {
        id: 'billable',
        label: 'Work',
        value: '',
        options: [
          { value: '', text: 'All' },
          { value: 'true', text: 'Billable' },
          { value: 'false', text: 'Non-billable' },
        ],
      },
    ],
    async load({ from, to }, values) {
      const params = new URLSearchParams({ from, to, group: values.group });
      if (values.billable) params.set('billable', values.billable);
      return api.get(`/reports/monthly?${params.toString()}`);
    },
    csv({ from, to }) {
      return `${API_BASE}/reports/export.csv?from=${encodeURIComponent(from)}&to=${encodeURIComponent(to)}`;
    },
    render: renderTimeByMonth,
  },
};

// --------------------------------------------------------------- filter row --

/// Render one report's own filter controls. They live inside the shared
/// `.filter-row`, so a future report's fields line up with From/To without any
/// extra layout code.
function renderFilters(report) {
  const host = $('report-filters');
  host.replaceChildren();
  for (const field of report.filters || []) {
    const id = `report-filter-${field.id}`;
    const select = el('select', { attrs: { id } });
    for (const option of field.options) {
      select.appendChild(el('option', { value: option.value, text: option.text }));
    }
    select.value = field.value ?? field.options[0]?.value ?? '';
    host.appendChild(el('div', { cls: 'filter-field' }, [
      el('label', { text: field.label, attrs: { for: id } }),
      select,
    ]));
  }
}

/// The current values of a report's own controls, keyed by descriptor id.
function filterValues(report) {
  const values = {};
  for (const field of report.filters || []) {
    const select = $(`report-filter-${field.id}`);
    values[field.id] = select ? select.value : (field.value ?? '');
  }
  return values;
}

function activeReport() {
  return REPORTS[$('report-kind').value] || REPORTS[Object.keys(REPORTS)[0]];
}

function range() {
  return { from: $('report-from').value, to: $('report-to').value };
}

// ---------------------------------------------------------------- rendering --

/// Draws a stacked monthly chart: one column per month, one segment per group.
/// The segment heights are computed from the same hours the table shows. The
/// chart is `role="img"` with a month-by-month accessible name; the table below
/// carries the exact values, so the bars never need to be read individually.
function drawMonthlyChart(data, shown, months) {
  const chart = $('report-chart');
  chart.textContent = '';
  $('report-legend').replaceChildren();
  const totals = months.map((_, m) => shown.reduce((sum, row) => sum + (row.months[m]?.hours || 0), 0));
  const max = Math.max(...totals, 1);
  const labels = [];
  months.forEach((month, m) => {
    const total = totals[m];
    labels.push(`${monthLabel(month)}: ${hours(total)} hours`);
    const bars = el('div', { cls: 'report-bars' });
    for (const [index, row] of shown.entries()) {
      const value = row.months[m]?.hours || 0;
      if (value <= 0) continue;
      bars.appendChild(el('span', {
        cls: `report-bar ${seriesClass(row, index)}`,
        style: `height:${Math.max(2, Math.round((value / max) * CHART_HEIGHT))}px`,
        attrs: {
          title: `${monthLabel(month)} \u2014 ${row.label}: ${hours(value)} hours`,
        },
      }));
    }
    if (bars.childNodes.length === 0) {
      bars.appendChild(el('span', {
        cls: 'report-bar-zero',
        attrs: { title: `${monthLabel(month)}: no hours` },
      }));
    }
    chart.appendChild(el('div', { cls: 'report-month-col' }, [
      bars,
      el('span', { cls: 'report-month-label', text: monthLabel(month) }),
    ]));
  });
  chart.setAttribute(
    'aria-label',
    `Monthly hours by ${groupLabel(data.group)}. ${labels.join('; ')}. Exact values are in the table below.`,
  );

  const legend = $('report-legend');
  for (const [index, row] of shown.entries()) {
    legend.appendChild(el('span', { cls: 'report-legend-item' }, [
      el('span', { cls: `report-swatch ${seriesClass(row, index)}`, attrs: { 'aria-hidden': 'true' } }),
      el('span', { text: row.label }),
      el('span', { cls: 'report-legend-total', text: `${hours(row.hours)} h` }),
    ]));
  }
}

/// Top groups stay individually coloured; the remainder becomes one "Other"
/// series so a large customer list cannot turn the chart into confetti. The
/// table below still lists every row.
function chartSeries(data) {
  if (data.rows.length <= SERIES_LIMIT) return { shown: data.rows, other: 0 };
  const shown = data.rows.slice(0, SERIES_LIMIT);
  const rest = data.rows.slice(SERIES_LIMIT);
  const other = rest.reduce((sum, row) => sum + row.hours, 0);
  shown.push({
    key: '__other__',
    label: `Other (${rest.length} groups)`,
    hours: other,
    months: data.months.map((month) => ({
      month,
      hours: rest.reduce((sum, row) => sum + (row.months.find((cell) => cell.month === month)?.hours || 0), 0),
    })),
  });
  return { shown, other: rest.length };
}

function renderTable(data) {
  const table = $('report-table');
  const head = $('report-table-head');
  const body = table.querySelector('tbody');
  const foot = table.querySelector('tfoot');
  head.replaceChildren();
  body.replaceChildren();
  foot.replaceChildren();
  head.appendChild(el('th', { attrs: { scope: 'col' }, text: 'Group' }));
  for (const month of data.months) {
    head.appendChild(el('th', { cls: 'num', attrs: { scope: 'col' }, text: monthLabel(month) }));
  }
  head.appendChild(el('th', { cls: 'num', attrs: { scope: 'col' }, text: 'Total' }));

  for (const row of data.rows) {
    body.appendChild(el('tr', {}, [
      el('th', { attrs: { scope: 'row' }, text: row.label }),
      ...data.months.map((month) => {
        const value = row.months.find((cell) => cell.month === month)?.hours || 0;
        return el('td', { cls: 'num', text: value ? hours(value) : '\u2014' });
      }),
      el('td', { cls: 'num', text: hours(row.hours) }),
    ]));
  }

  const totals = data.months.map((month) => data.rows.reduce(
    (sum, row) => sum + (row.months.find((cell) => cell.month === month)?.hours || 0), 0));
  const grand = totals.reduce((sum, value) => sum + value, 0);
  foot.appendChild(el('tr', {}, [
    el('th', { attrs: { scope: 'row' }, text: 'Total' }),
    ...totals.map((value) => el('td', { cls: 'num', text: hours(value) })),
    el('td', { cls: 'num', text: hours(grand) }),
  ]));

  const empty = $('report-empty');
  empty.hidden = data.rows.length !== 0;
  empty.textContent = 'No time recorded in this period for this grouping.';
  table.hidden = data.rows.length === 0;
}

function renderTimeByMonth(data, _range, values) {
  const chart = $('report-chart');
  chart.textContent = '';
  $('report-legend').replaceChildren();
  $('report-chart-title').textContent = `Hours by ${data.group}`;
  $('report-chart-subtitle').textContent = `${data.from} to ${data.to}`;
  $('report-total').textContent =
    `${hours(data.total_hours)} h total (billable ${hours(data.billable_hours)} / non-billable ${hours(data.nonbillable_hours)})`;
  renderTable(data);

  const notes = [];
  if (values?.billable) {
    notes.push(`Rows are filtered to ${values.billable === 'true' ? 'billable' : 'non-billable'} work; the header total covers every visible entry.`);
  }
  if (data.rows.length === 0) {
    chart.appendChild(el('p', { cls: 'report-chart-empty', text: 'No hours in this period.' }));
    chart.setAttribute('aria-label', 'No hours in this period.');
    $('report-chart-note').textContent = notes.join(' ');
    return;
  }

  const { shown, other } = chartSeries(data);
  drawMonthlyChart(data, shown, data.months);
  if (other) {
    notes.push(`The chart shows the top ${SERIES_LIMIT} groups; ${other} more are combined in Other. The table lists every group.`);
  }
  $('report-chart-note').textContent = notes.join(' ');
}

// --------------------------------------------------------------- the runner --

export async function runReport(evt) {
  evt?.preventDefault();
  return panelRefresh('panel-reports', 'the report', runReportBody).catch(() => { });
}

async function runReportBody() {
  const report = activeReport();
  if (!report) return;
  const bounds = range();
  if (!bounds.from || !bounds.to) return;
  const values = filterValues(report);
  const csv = $('csv-link');
  if (report.csv) {
    csv.href = report.csv(bounds, values);
    csv.hidden = false;
  } else {
    csv.hidden = true;
  }
  const data = await report.load(bounds, values);
  report.render(data, bounds, values);
  const count = data.rows?.length ?? 0;
  announce(`${report.label}: ${count} row${count === 1 ? '' : 's'}, ${hours(data.total_hours)} hours.`);
}

/// Build the picker and the active report's fields, and wire the shared form.
/// Called once from boot after the shell exists; the picker itself is static in
/// `index.html`, so the registry is the only list to extend.
export function initReports() {
  const picker = $('report-kind');
  picker.replaceChildren();
  for (const id of Object.keys(REPORTS)) {
    picker.appendChild(el('option', { value: id, text: REPORTS[id].label }));
  }
  const sync = () => {
    renderFilters(activeReport());
    runReport().catch(() => { });
  };
  picker.addEventListener('change', sync);
  $('report-form').addEventListener('submit', runReport);
  renderFilters(activeReport());
}
