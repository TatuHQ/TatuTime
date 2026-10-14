// calendar.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { $, announce, el, isoDate, ubuntuIcon } from './dom.js';
import { activateSeg } from './route.js';
import { createSeqGuard } from './seq.js';
import { MONTH_NAMES, fmtHM, refreshDay, showEntryForm } from './timesheet.js';

const calSeq = createSeqGuard();

// ------------------------------------------------------ calendar view ----
//
// #140: a month grid of LOGGED time inside the Timesheets segment group.
// Read-only by design — a day click hands over to the Day view (and its
// dialog, #145) where editing and locks already live. The imported Google/MS
// calendar events (#36) keep their own section; this never touches them.

export const calState = { month: '' }; // 'YYYY-MM'; empty = follow the displayed day

export function shiftMonth(month, delta) {
  const [yy, mm] = month.split('-').map(Number);
  return new Date(Date.UTC(yy, mm - 1 + delta, 1)).toISOString().slice(0, 7);
}

export function monthName(month) {
  const [yy, mm] = month.split('-').map(Number);
  return `${MONTH_NAMES[mm - 1]} ${yy}`;
}

export async function refreshTimeCal() {
  const today = isoDate(new Date());
  if (!calState.month) calState.month = (($('day-date').value || today).slice(0, 7));
  const month = calState.month;
  const isCalCurrent = calSeq();
  const [yy, mm] = month.split('-').map(Number);
  const last = new Date(Date.UTC(yy, mm, 0)).getUTCDate();
  const monthStart = `${month}-01`;
  const monthEnd = `${month}-${String(last).padStart(2, '0')}`;
  $('cal-label').textContent = monthName(month);
  const data = await api.get(`/entries?from=${monthStart}&to=${monthEnd}`);
  if (!isCalCurrent()) return; // #188: a newer month change superseded this
  const entries = data.entries || [];
  const byDate = new Map();
  let totalHundredths = 0;
  for (const e of entries) {
    const h = Math.round(Number(e.hours) * 100);
    totalHundredths += h;
    if (!byDate.has(e.date)) byDate.set(e.date, []);
    byDate.get(e.date).push({ ...e, hundredths: h });
  }
  // The displayed month already names itself in the bar's centre label, so the
  // readout is just the labelled number ("Total hours 26:01").
  $('cal-total').textContent = fmtHM(totalHundredths);
  const tbody = $('cal-grid').querySelector('tbody');
  tbody.textContent = '';
  const lead = (new Date(`${monthStart}T00:00:00Z`).getUTCDay() + 6) % 7;
  let row = el('tr');
  for (let i = 0; i < lead; i += 1) row.appendChild(el('td', { cls: 'cal-out' }));
  for (let d = 1; d <= last; d += 1) {
    const date = `${month}-${String(d).padStart(2, '0')}`;
    const dayEntries = byDate.get(date) || [];
    const daySum = dayEntries.reduce((a, e) => a + e.hundredths, 0);
    // The cell carries the DAY TOTAL only — never a per-entry list (#140), so a
    // busy day stays legible at a glance and the detail stays in the Day view.
    // The top row mirrors the Day view's day box (#12/#108): the date number
    // top-left and a per-day status icon top-right — success when the day has
    // an entry, history when it is empty.
    const kids = [
      el('span', { cls: 'cal-head' }, [
        el('span', { cls: 'cal-num', text: String(d) }),
        el('span', { cls: 'cal-status', attrs: { 'aria-hidden': 'true' } }, [
          ubuntuIcon(dayEntries.length ? 'success' : 'history'),
        ]),
      ]),
    ];
    if (dayEntries.length) {
      kids.push(el('span', { cls: 'cal-hours', text: fmtHM(daySum) }));
    }
    const cell = el(
      'td',
      { cls: `cal-cell${dayEntries.length ? ' has' : ''}${date === today ? ' today' : ''}` },
      [
        dayEntries.length
          ? el('button', {
              type: 'button',
              cls: 'cal-day',
              on: {
                click: () => {
                  activateSeg('ts-day');
                  $('day-date').value = date;
                  refreshDay().catch?.(() => {});
                  announce(`Opened ${date} in the Day view.`);
                },
              },
              attrs: {
                'aria-label': `${d} ${monthName(month)}: ${fmtHM(daySum)} across ${
                  dayEntries.length
                } entr${dayEntries.length === 1 ? 'y' : 'ies'} — open Day view`,
              },
            }, kids)
          : el('div', { cls: 'cal-day cal-plain' }, kids),
      ],
    );
    row.appendChild(cell);
    if ((lead + d) % 7 === 0) {
      tbody.appendChild(row);
      row = el('tr');
    }
  }
  if (row.children.length) {
    while (row.children.length < 7) row.appendChild(el('td', { cls: 'cal-out' }));
    tbody.appendChild(row);
  }
}

// ---------------------------------------------------------------- calendar --

export async function refreshCalendar(date) {
  let events = [];
  try {
    const data = await api.get(`/calendar/events?from=${date}&to=${date}`);
    events = data.events || [];
    $('calendar-section').hidden = false;
  } catch {
    $('calendar-section').hidden = true;
    return;
  }
  const ul = $('calendar-list');
  ul.textContent = '';
  for (const ev of events) {
    const start = new Date(ev.start).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    ul.appendChild(
      el('li', { cls: 'cal-event' }, [
        el('span', { text: `${start} — ${ev.title}` }),
        el('button', {
          type: 'button',
          cls: 'link',
          text: 'Log time',
          on: {
            click: () => {
              showEntryForm(true);
              $('entry-date').value = date;
              $('entry-hours').value = '';
              $('entry-note').value = ev.title;
              $('entry-hours').focus();
              $('entry-form').scrollIntoView({ behavior: 'smooth', block: 'center' });
            },
          },
        }),
      ]),
    );
  }
  $('calendar-section').hidden = events.length === 0;
}

