// timesheet.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { refreshCalendar } from './calendar.js';
import { $, addDays, announce, askAlert, askConfirm, askPrompt, clearFormError, el, isoDate, mondayOf, rowAction, showFormError, statusChip, ubuntuIcon } from './dom.js';
import { isOpenInvoice } from './invoices.js';
import { refreshNotifications } from './notifications.js';
import { showTimesheet } from './route.js';
import { createSeqGuard } from './seq.js';
import { customerName, fillProjectSelect, fillTaskSelect, state } from './state.js';

const daySeq = createSeqGuard();

// ----------------------------------------------------------------- day ---

// Hundredths of an hour as `H:MM` (e.g. 425 -> 4:15).
export function fmtHM(hundredths) {
  const totalMin = Math.round(hundredths * 0.6);
  const h = Math.floor(totalMin / 60);
  const m = totalMin % 60;
  return `${h}:${String(m).padStart(2, '0')}`;
}

export function dayDate() {
  return $('day-date').value;
}

export const DAY_NAMES = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
export const MONTH_NAMES = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

export function setDayLabel() {
  const d = dayDate();
  const [y, m, day] = d.split('-').map(Number);
  const dt = new Date(Date.UTC(y, m - 1, day));
  // Mockup format (#108): "Monday, 28 Sep".
  $('day-label').textContent = `${DAY_NAMES[dt.getUTCDay()]}, ${day} ${MONTH_NAMES[m - 1]}`;
}

// The week strip: totals per day for the week containing the selected day.
export async function refreshWeekStrip() {
  const d = dayDate();
  const start = mondayOf(d);
  const end = addDays(start, 6);
  let entries = [];
  try {
    const data = await api.get(`/entries?from=${start}&to=${end}`);
    entries = data.entries || [];
  } catch {
    $('week-strip').textContent = '';
    return;
  }
  const totals = {};
  for (const e of entries) totals[e.date] = (totals[e.date] || 0) + Math.round(e.hours * 100);
  const strip = $('week-strip');
  strip.textContent = '';
  for (let i = 0; i < 7; i += 1) {
    const date = addDays(start, i);
    const [y, m, dd] = date.split('-').map(Number);
    strip.appendChild(
      el(
        'button',
        {
          type: 'button',
          cls: 'ws-day p-card' + (date === d ? ' selected' : ''),
          data: { date },
          attrs: date === d ? { 'aria-current': 'date' } : {},
          on: { click: () => selectDay(date) },
        },
        [
          el('span', {
            cls: 'ws-name',
            text: new Date(Date.UTC(y, m - 1, dd)).toUTCString().slice(0, 3),
          }),
          el('span', { cls: 'ws-date', text: String(dd) }),
          el('span', { cls: 'ws-total num', text: fmtHM(totals[date] || 0) }),
          el('span', { cls: 'ws-clock', attrs: { 'aria-hidden': 'true' } }, [ubuntuIcon(totals[date] ? 'success' : 'history')]),
        ],
      ),
    );
  }
  const sum = Object.values(totals).reduce((a, b) => a + b, 0);
  strip.appendChild(el('span', { cls: 'ws-week num', text: `Week total ${fmtHM(sum)}` }));
}

export async function selectDay(date) {
  $('day-date').value = date;
  await refreshDay();
}

export function navigateDay(delta) {
  const next = addDays(dayDate(), delta);
  $('day-date').value = next;
  refreshDay();
}

export async function refreshDay() {
  const date = dayDate();
  if (!date) return;
  const isDayCurrent = daySeq();
  setDayLabel();
  const data = await api.get(`/entries?date=${encodeURIComponent(date)}`);
  if (!isDayCurrent()) return; // #188: a newer navigation superseded this
  const rows = data.entries || [];
  // #108: rows carry a lock indicator when the entry sits on an issued
  // invoice or a submitted/approved week (same lookup the grid uses, cached).
  const locked = await weekLockIds();
  if (!isDayCurrent()) return; // #188: same, after the lock round-trip
  const tbody = $('day-table').querySelector('tbody');
  tbody.textContent = '';
  let total = 0;
  for (const e of rows) {
    total += Math.round(e.hours * 100);
    const projName = (state.projectsByCustomer[e.customer_id] || []).find((p) => p.code === e.project_code);
    const custLine =
      customerName(e.customer_id) + (e.task_code ? ` · ${e.task_code}` : '');
    const hoursCell = el('td', { cls: 'num entry-hours' }, [
      el('span', { text: fmtHM(Math.round(e.hours * 100)) }),
      locked.has(e.id)
        ? el('span', {
            cls: 'lock',
            attrs: {
              role: 'img',
              'aria-label': 'locked',
              title: 'On an issued invoice or submitted week — editing is blocked',
            },
          }, [ubuntuIcon('lock-locked')])
        : null,
    ]);
    const tr = el(
      'tr',
      { data: { entryId: e.id } }, // C7: target the exact row on jump-to-entry
      [
        el('td', { cls: 'entry-main' }, [
          el('div', {
            cls: 'entry-project',
            text: projName ? `${e.project_code} — ${projName.name}` : e.project_code,
          }),
          el('div', { cls: 'entry-customer', text: custLine }),
        ]),
        // #145-era stacked note line becomes its own column: scannable
        // against the header, badge stays with the note it qualifies.
        el(
          'td',
          { cls: 'entry-note', text: e.note || '', attrs: { title: e.note || '' } },
          e.billable ? [] : [statusChip('non-billable')],
        ),
        hoursCell,
        // Row actions use the shared link/danger pattern (the rest of the
        // registers); the bespoke 'pill' treatment was the odd one out and
        // read as a boxed button inside a boxed row (design policy: neutral
        // text actions in tables, positive/negative reserved for primary
        // and destructive page-level actions).
        el('td', { cls: 'actions-col' }, [
          rowAction('link', 'Edit', () => startEdit(e), undefined, 'edit'),
          rowAction('danger', 'Delete', () => removeEntry(e), undefined, 'delete'),
        ]),
      ],
    );
    tbody.appendChild(tr);
  }
  $('day-total').textContent = fmtHM(total);
  $('day-table').hidden = rows.length === 0;
  $('day-empty').hidden = rows.length !== 0;
  $('entry-date').value = date;
  $('entry-form-date').textContent = date ? `For ${date}` : '';
  // D2: the strip and calendar pulls are independent — fetch them at once.
  await Promise.all([refreshWeekStrip(), refreshCalendar(date), refreshNotifications()]);
}

// Copy-forward (#12, restyled #108): pending rows for the projects worked
// `ago` days before the current one, saved only when the user fills in hours
// (hours must be >= 0.01 to persist).
export async function copyPreviousDay(ago = 1) {
  const target = dayDate();
  const prev = addDays(target, -ago);
  const list = $('copy-rows');
  list.textContent = '';
  let rows = [];
  try {
    const data = await api.get(`/entries?date=${prev}`);
    rows = data.entries || [];
  } catch (err) {
    announce(`Could not read ${prev}: ${err.message}`);
    return;
  }
  if (rows.length === 0) {
    announce(`No entries on ${prev} to copy.`);
    return;
  }
  const seen = new Set();
  for (const e of rows) {
    const key = `${e.customer_id}|${e.project_code}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const input = el('input', {
      type: 'number',
      cls: 'copy-hours',
      attrs: {
        min: '0.01',
        max: '24',
        step: '0.01',
        'aria-label': `Hours for ${e.project_code} on ${target}`,
        placeholder: 'h',
      },
    });
    const li = el('li', { cls: 'copy-row' }, [
      el('span', {
        cls: 'copy-label',
        text: `${customerName(e.customer_id)} / ${e.project_code}`,
      }),
      input,
      el('button', {
        type: 'button',
        text: 'Save',
        on: {
          click: async () => {
            const hours = Number(input.value);
            if (!(hours >= 0.01 && hours <= 24)) {
              announce('Enter hours between 0.01 and 24.');
              input.focus();
              return;
            }
            try {
              await api.post('/entries', {
                date: target,
                customer_id: e.customer_id,
                project_code: e.project_code,
                task_code: e.task_code || null,
                hours,
                note: '',
                billable: e.billable !== false,
              });
              li.remove();
              announce(`Copied ${e.project_code} to ${target}.`);
              await refreshDay();
            } catch (err) {
              announce(`Copy failed: ${err.message}`);
            }
          },
        },
      }),
    ]);
    list.appendChild(li);
  }
  announce(`${seen.size} project row(s) ready — enter hours to copy them to ${target}.`);
}

export let entryOpener = null; // focus return target when the dialog closes (#145)
export let entrySaving = false; // guards Escape/close while a save is in flight

// The Day entry form is a modal dialog now (#145): open records the opener
// so closing restores focus; jsdom lacks showModal so the attribute
// fallback (same pattern as #102/#111) keeps the harness honest.
export function showEntryForm(show) {
  const dlg = $('entry-dialog');
  if (show) {
    if (!(dlg.open === true || dlg.hasAttribute('open'))) entryOpener = document.activeElement;
    if (typeof dlg.showModal === 'function') dlg.showModal();
    else dlg.setAttribute('open', '');
    return;
  }
  if (dlg.open === true || dlg.hasAttribute('open')) {
    if (typeof dlg.close === 'function') dlg.close();
    else dlg.removeAttribute('open');
  }
  if (entryOpener && document.contains(entryOpener) && typeof entryOpener.focus === 'function') {
    entryOpener.focus();
  }
  entryOpener = null;
}

export function startEdit(e) {
  showEntryForm(true);
  $('entry-id').value = e.id;
  $('entry-date').value = e.date;
  $('entry-customer').value = e.customer_id;
  fillProjectSelect($('entry-project'), e.customer_id, e.project_code);
  fillTaskSelect($('entry-task'), e.customer_id, e.project_code, e.task_code);
  $('entry-hours').value = e.hours;
  $('entry-note').value = e.note || '';
  $('entry-billable').checked = e.billable !== false;
  $('entry-form-title').textContent = `Edit entry ${e.id.slice(0, 8)}`;
  $('entry-save').textContent = 'Update entry';
  $('entry-cancel').hidden = false;
  clearFormError($('entry-error'));
  $('entry-customer').focus(); // #189: the earlier entry-hours focus was immediately stolen
  $('entry-form').scrollIntoView({ behavior: 'smooth', block: 'center' });
}

export function resetEntryForm() {
  $('entry-id').value = '';
  $('entry-hours').value = '';
  $('entry-note').value = '';
  $('entry-billable').checked = true;
  $('entry-date').value = $('day-date').value || isoDate(new Date());
  $('entry-project').value = '';
  fillTaskSelect($('entry-task'), '', null, null);
  $('entry-form-title').textContent = 'Add an entry';
  $('entry-save').textContent = 'Save entry';
  $('entry-cancel').hidden = true;
  clearFormError($('entry-error'));
}

export async function removeEntry(e) {
  const label = `${e.hours}h on ${e.project_code}`;
  if (!(await askConfirm(`Delete this entry (${label})? This cannot be undone.`))) return;
  try {
    await api.del(`/entries/${e.id}`);
    announce(`Deleted ${label}.`);
    await refreshDay();
  } catch (err) {
    announce(`Delete failed: ${err.message}`);
  }
}

export async function saveEntry(evt) {
  evt.preventDefault();
  clearFormError($('entry-error'));
  const id = $('entry-id').value;
  const projectId = $('entry-project').value;
  if (!projectId) {
    showFormError($('entry-error'), { message: 'Select a project code.' });
    return;
  }
  const body = {
    date: $('entry-date').value,
    customer_id: $('entry-customer').value,
    project_code: projectId,
    task_code: $('entry-task').value || null,
    hours: Number($('entry-hours').value),
    note: $('entry-note').value,
    billable: $('entry-billable').checked,
  };
  entrySaving = true;
  try {
    if (id) {
      await api.put(`/entries/${id}`, body);
      announce('Entry updated.');
    } else {
      await api.post('/entries', body);
      announce('Entry added.');
    }
    // #145: saving returns to the timesheet — the dialog closes for creates
    // too; Track time re-opens it with the day kept for the next entry.
    const savedDate = body.date;
    // Keep the table aligned with whatever day was just written, even if it
    // differs from the one being viewed.
    $('day-date').value = savedDate;
    resetEntryForm();
    showEntryForm(false);
    $('day-add').focus();
    await refreshDay();
  } catch (err) {
    showFormError($('entry-error'), err);
  } finally {
    entrySaving = false;
  }
}

// ---------------------------------------------------------------- week ---
// Spreadsheet-style grid (#13): one row per customer+project, Mon–Sun cells
// are editable hour inputs. Blur/Enter saves through the API; clearing a
// cell deletes the entry after confirm. Extra rows (add-row / copy-last-week)
// live client-side until their cells get hours.

export const weekState = {
  extraRows: [], // [{customer_id, project_code, task_code, week}] — week-scoped (#128/#137)
  rowKeys: new Set(), // `${customer_id}::${project_code}` shown in this week (#127)
  days: [],
  seq: 0, // render guard: only the newest fetch may touch the DOM
};

// Entry ids locked by submitted weeks or issued invoices. Fetched fresh on
// every grid render — locks can appear while the grid is open (#8/#16).
export let weekLockCache = { at: 0, ids: null }; // D2: 5s TTL — one commit reuses it

export async function weekLockIds() {
  const nowMs = Date.now();
  if (weekLockCache.ids && nowMs - weekLockCache.at < 5000) return weekLockCache.ids;
  const locked = new Set();
  // #189: the two reads are independent — overlap them (was sequential).
  const [subsRes, invsRes] = await Promise.allSettled([
    api.get('/submissions'),
    api.get('/invoices'), // admin-only; a member rejection is the normal arm
  ]);
  if (subsRes.status === 'fulfilled') {
    for (const s of subsRes.value.submissions || []) {
      if (s.state === 'submitted' || s.state === 'approved') {
        for (const id of s.entry_ids || []) locked.add(id);
      }
    }
  }
  if (invsRes.status === 'fulfilled') {
    for (const i of invsRes.value.invoices || []) {
      if (isOpenInvoice(i)) for (const l of i.lines || []) if (l.entry_id) locked.add(l.entry_id);
    }
  }
  weekLockCache = { at: Date.now(), ids: locked };
  return locked;
}

// #108: the day view now shares this cache, so a mutation that changes lock
// state (submit/decide/issue/pay) must drop it — otherwise a refresh within
// the TTL renders stale locks.
export function invalidateLockCache() {
  weekLockCache = { at: 0, ids: null };
}

export function weekShort(dateStr) {
  const [y, m, d] = dateStr.split('-').map(Number);
  const dt = new Date(y, m - 1, d);
  return `${String(d).padStart(2, '0')} ${MONTH_NAMES[m - 1]}`; // #189: one month table
}

export async function refreshWeek() {
  const anchor = $('week-date').value;
  if (!anchor) return;
  const seq = ++weekState.seq;
  const start = mondayOf(anchor);
  $('week-date').value = start;
  const days = [];
  for (let i = 0; i < 7; i += 1) days.push(addDays(start, i));
  weekState.days = days;
  const end = days[6];
  const today = isoDate(new Date());
  $('week-label').textContent = `${weekShort(start)} – ${weekShort(end)} ${end.slice(0, 4)}`;

  const data = await api.get(`/entries?from=${start}&to=${end}`);
  const entries = data.entries || [];
  // Days in the displayed week that carry at least one entry — drives the
  // per-column status icon (mirrors the Day view's week strip).
  const entryDates = new Set(entries.map((e) => e.date));
  const locked = await weekLockIds();
  if (seq !== weekState.seq) return; // a newer render superseded this one

  // #137: a line is identified by customer + project + TASK (empty task =
  // project-level), so tasks of one project no longer collapse into a row.
  const keyFor = (e) => `${e.customer_id}::${e.project_code}::${e.task_code || ''}`;
  const byKey = new Map();
  const rowFor = (customerId, projectCode, taskCode) => {
    const k = `${customerId}::${projectCode}::${taskCode || ''}`;
    if (!byKey.has(k))
      byKey.set(k, { customer_id: customerId, project_code: projectCode, task_code: taskCode || '', cells: {}, ids: {}, notes: {} });
    return byKey.get(k);
  };
  for (const e of entries) {
    const row = rowFor(e.customer_id, e.project_code, e.task_code || '');
    row.cells[e.date] = (row.cells[e.date] || 0) + Math.round(e.hours * 100);
    (row.ids[e.date] = row.ids[e.date] || []).push(e.id);
    if (e.note) row.notes[e.date] = e.note;
  }
  for (const r of weekState.extraRows)
    if (r.week === start) rowFor(r.customer_id, r.project_code, r.task_code);
  weekState.rowKeys = new Set(byKey.keys()); // what "Add row" must not offer again (#127)

  const table = $('week-table');
  table.textContent = '';

  const htr = el('tr', {}, [
    el('th', { attrs: { scope: 'col' }, text: 'Project' }),
    ...days.map((d) => {
      const [yy, mm, dd] = d.split('-').map(Number);
      // Mirror the Day view's day box (#12/#108): the date sits top-left and
      // the per-day status icon — success when the day has an entry, history
      // when it is empty — sits top-right inside the header cell.
      const hasEntry = entryDates.has(d);
      return el(
        'th',
        {
          attrs: { scope: 'col' },
          cls: d === today ? 'today' : '',
          data: { date: d },
        },
        [
          el('span', { cls: 'wk-date', text: `${dd} ${MONTH_NAMES[mm - 1]}` }),
          el('span', { cls: 'wk-dow', text: new Date(d).toUTCString().slice(0, 3) }),
          el('span', { cls: 'wk-clock', attrs: { 'aria-hidden': 'true' } }, [
            ubuntuIcon(hasEntry ? 'success' : 'history'),
          ]),
        ],
      );
    }),
    el('th', { attrs: { scope: 'col' }, cls: 'num', text: 'Week' }),
  ]);
  table.appendChild(el('thead', {}, [htr]));

  const tbody = el('tbody');
  const sorted = Array.from(byKey.values()).sort(
    (a, b) =>
      customerName(a.customer_id).localeCompare(customerName(b.customer_id)) ||
      a.project_code.localeCompare(b.project_code) ||
      a.task_code.localeCompare(b.task_code),
  );
  const dayTotals = days.map(() => 0);
  for (const row of sorted) {
    let weekTotal = 0;
    let anyLocked = false;
    const projects = state.projectsByCustomer[row.customer_id] || [];
    const projName = projects.find((p) => p.code === row.project_code);
    const cells = days.map((d, i) => {
      const hundredths = row.cells[d];
      const ids = row.ids[d] || [];
      if (hundredths) {
        weekTotal += hundredths;
        dayTotals[i] += hundredths;
      }
      const isLocked = ids.some((id) => locked.has(id));
      if (isLocked) anyLocked = true;
      const input = el('input', {
        type: 'number',
        cls: 'cell-input',
        value: hundredths ? (hundredths / 100).toFixed(2) : '',
        attrs: {
          min: '0.01',
          max: '24',
          step: '0.01',
          inputmode: 'decimal',
          'aria-label': `${row.project_code}${row.task_code ? ' / ' + row.task_code : ''}, ${customerName(row.customer_id)}, ${d}: hours`,
          ...(isLocked ? { disabled: '' } : {}),
        },
        data: {
          customer: row.customer_id,
          project: row.project_code,
          task: row.task_code,
          date: d,
          ids: ids.join(','),
          was: hundredths ? (hundredths / 100).toFixed(2) : '',
        },
      });
      const children = [input];
      // #126: cells that hold an entry always show the note affordance —
      // filled ¶ with a note, faint ✎ to add one — editing IN the grid via
      // the #102 dialog instead of jumping to the day view.
      if (ids.length > 0) {
        const hasNote = Boolean(row.notes[d]);
        children.push(
          el('button', {
            type: 'button',
            cls: hasNote ? 'note-flag' : 'note-flag note-empty',
            attrs: {
              title: row.notes[d] || 'Add a note',
              'aria-label': hasNote
                ? `Note: ${row.notes[d]}. Activate to edit.`
                : `Add a note for ${row.project_code} on ${d}`,
            },
            on: { click: () => editCellNote(ids[0], isLocked) },
          }, [ubuntuIcon(hasNote ? 'comments' : 'edit')]),
        );
      }
      return el(
        'td',
        { cls: `cell${hundredths ? ' has' : ''}${isLocked ? ' locked' : ''}` },
        children,
      );
    });
    const tr = el(
      'tr',
      { cls: anyLocked ? 'has-lock' : '' },
      [
        el('th', { attrs: { scope: 'row' }, cls: 'row-band' }, [
          el('div', {
            cls: 'entry-project',
            text: `${row.project_code}${row.task_code ? ' · ' + row.task_code : ''}${projName && !row.task_code ? ` — ${projName.name}` : ''}`,
            attrs: row.task_code ? { title: `${row.project_code} / ${row.task_code}` } : {},
          }),
          el('div', { cls: 'entry-customer', text: customerName(row.customer_id) }),
        ]),
        ...cells,
        el('td', { cls: 'num row-total' }, [
          el('span', { text: fmtHM(weekTotal) }),
          anyLocked
            ? el('span', {
                cls: 'lock',
                attrs: { role: 'img', 'aria-label': 'locked', title: 'This row holds locked entries' },
              }, [ubuntuIcon('lock-locked')])
            : null,
        ]),
      ],
    );
    tbody.appendChild(tr);
  }
  if (sorted.length === 0) {
    tbody.appendChild(el('tr', {}, [
      el('td', { cls: 'week-empty-notice', attrs: { colspan: String(days.length + 2) } }, [
        el('strong', { text: 'No time logged' }),
        el('p', { text: "You haven't recorded any time yet this week." }),
      ]),
    ]));
  }
  table.appendChild(tbody);

  const grandRow = el('tr', {}, [
    el('th', { attrs: { scope: 'row' }, text: 'Day totals' }),
    ...dayTotals.map((t) => el('td', { cls: 'num day-total', text: t ? fmtHM(t) : '0' })),
    el('td', { cls: 'num week-total', text: fmtHM(dayTotals.reduce((a, b) => a + b, 0)) }),
  ]);
  table.appendChild(el('tfoot', {}, [grandRow]));

}

// Inline cell save (#13): blur or Enter commits the typed hours.
export async function commitCell(input) {
  const raw = input.value.trim();
  const was = input.dataset.was || '';
  if (raw === was) return;
  const customerId = input.dataset.customer;
  const projectCode = input.dataset.project;
  const date = input.dataset.date;
  const ids = (input.dataset.ids || '').split(',').filter(Boolean);
  try {
    if (raw === '') {
      if (ids.length === 0) return;
      const label = `${was}h on ${projectCode}, ${date}`;
      if (!(await askConfirm(`Delete this entry (${label})?`))) {
        input.value = was;
        return;
      }
      for (const id of ids) await api.del(`/entries/${id}`);
      announce(`Deleted ${label}.`);
    } else if (ids.length === 1) {
      const existing = await api.get(`/entries/${ids[0]}`);
      await api.put(`/entries/${ids[0]}`, {
        date,
        customer_id: customerId,
        project_code: projectCode,
        task_code: existing.task_code || null,
        hours: Number(raw),
        note: existing.note || '',
        billable: existing.billable !== false,
      });
      announce(`Saved ${raw}h — ${projectCode}, ${date}.`);
    } else if (ids.length > 1) {
      const merge = await askConfirm(
        `This cell combines ${ids.length} entries for ${projectCode} on ${date}. Saving sets the first to ${raw}h and removes the others.`,
      );
      if (!merge) {
        input.value = was;
        return;
      }
      const existing = await api.get(`/entries/${ids[0]}`);
      await api.put(`/entries/${ids[0]}`, {
        date,
        customer_id: customerId,
        project_code: projectCode,
        task_code: existing.task_code || null,
        hours: Number(raw),
        note: existing.note || '',
        billable: existing.billable !== false,
      });
      for (const id of ids.slice(1)) await api.del(`/entries/${id}`);
      announce(`Merged ${projectCode} on ${date} to ${raw}h.`);
    } else {
      await api.post('/entries', {
        date,
        customer_id: customerId,
        project_code: projectCode,
        task_code: input.dataset.task || null,
        hours: Number(raw),
        note: '',
        billable: true,
      });
      announce(`Added ${raw}h — ${projectCode}${input.dataset.task ? ' / ' + input.dataset.task : ''}, ${date}.`);
    }
    await refreshWeek();
  } catch (err) {
    // Keep the typed value visible; surface the error without losing work.
    announce(`Save failed: ${err.message}`);
    input.focus();
    try {
      await refreshWeek();
      const again = [...document.querySelectorAll('#week-table input.cell-input')].find(
        (i) =>
          i.dataset.customer === customerId && i.dataset.project === projectCode &&
          (i.dataset.task || '') === (input.dataset.task || '') && i.dataset.date === date,
      );
      if (again) {
        again.value = raw;
        again.focus();
      }
    } catch { /* grid refresh failure is secondary */ }
  }
}

export function navigateWeek(delta) {
  $('week-date').value = addDays($('week-date').value, delta * 7);
  refreshWeek();
}

// Append an extra grid row for the displayed week; returns false when the
// line already exists (#127/#128 — the row set belongs to one week).
export function addWeekRow(customerId, projectCode, taskCode, week) {
  if (
    weekState.extraRows.some(
      (r) =>
        r.week === week &&
        r.customer_id === customerId &&
        r.project_code === projectCode &&
        (r.task_code || '') === (taskCode || ''),
    )
  ) {
    return false;
  }
  weekState.extraRows.push({ customer_id: customerId, project_code: projectCode, task_code: taskCode || '', week });
  return true;
}

// Copy the source week's PROJECT LINES into the displayed week — never the
// time entries (#128). Rows are week-scoped, re-copying is idempotent, and
// the announcement reports what actually happened.
// In-place note editing for week-grid cells (#126). The dialog pre-fills
// the current note; save PUTs the entry with its hours untouched; cancel
// leaves the record unchanged (never a partial write). Locked entries are
// read-only — the note is shown, not edited, matching the cell's lock.
export async function editCellNote(id, isLocked) {
  try {
    const e = await api.get(`/entries/${id}`);
    if (isLocked) {
      await askAlert(
        e.note
          ? `Locked entry note (${e.project_code}, ${e.date}): ${e.note}`
          : `This entry is locked (issued invoice or submitted week), so no note can be added.`,
      );
      return;
    }
    const note = await askPrompt(`Note for ${e.project_code} on ${e.date}:`, e.note || '');
    if (note === null) {
      announce('Note unchanged.');
      return;
    }
    await api.put(`/entries/${id}`, {
      date: e.date,
      customer_id: e.customer_id,
      project_code: e.project_code,
      task_code: e.task_code || null,
      hours: e.hours,
      note: note.trim(),
      billable: e.billable !== false,
    });
    announce('Note saved.');
    await refreshWeek();
  } catch (err) {
    announce(`Note failed: ${err.message}`);
  }
}

export async function copyLastWeek(weeksAgo = 1) {
  const targetWeek = $('week-date').value; // Monday of the displayed week
  const start = addDays(targetWeek, -7 * weeksAgo);
  const end = addDays(start, 6);
  try {
    const data = await api.get(`/entries?from=${start}&to=${end}`);
    const seen = new Set();
    let added = 0;
    for (const e of data.entries || []) {
      const k = `${e.customer_id}|${e.project_code}|${e.task_code || ''}`;
      if (seen.has(k)) continue;
      seen.add(k);
      if (addWeekRow(e.customer_id, e.project_code, e.task_code || '', targetWeek)) added += 1;
    }
    await refreshWeek();
    if (seen.size === 0) {
      announce(`Nothing to copy: no project lines in the week of ${start}.`);
    } else if (added === 0) {
      announce(`Already copied: all ${seen.size} project row(s) from ${start} are in this week. No hours copied.`);
    } else {
      announce(`Copied ${added} project row(s) from ${start} — hours left blank, nothing saved yet.`);
    }
  } catch (err) {
    announce(`Copy failed: ${err.message}`);
  }
}

export async function jumpToEntry(entryId) {
  const e = await api.get(`/entries/${entryId}`);
  showTimesheet('ts-day'); // suppressed — we await the refresh ourselves
  $('day-date').value = e.date;
  await refreshDay();
  startEdit(e);
  const target = document.querySelector(`#day-table tbody tr[data-entry-id="${entryId}"]`);
  if (target) target.scrollIntoView({ behavior: 'smooth' });
}

