// #232 increment 6 regression guard: every custom property referenced by a
// `var(--…)` in web/styles.css must be defined by the theme/palette tokens — in
// BOTH themes. This closes the class of bug that produced the deferred
// `#dlg-input { color: var(--text) }` defect reported by increment 4 (the token
// is `--fg`): the CSS still parsed and shipped, but the declaration silently did
// nothing because the property was never defined anywhere. A stylesheet that
// resolves in light but not dark (or vice versa) fails here too, and the two
// dark-theme blocks are pinned to the same token set so they cannot drift.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const css = readFileSync(new URL('../web/styles.css', import.meta.url), 'utf8');

// Every `var(--name)` reference (any fallback after the comma is irrelevant:
// the token itself must still exist).
const referenced = new Set();
for (const m of css.matchAll(/var\(\s*(--[\w-]+)/g)) referenced.add(m[1]);

// The declaration body of the named cascade block (first `{ … }` after it).
function body(selector) {
  const at = css.indexOf(selector);
  assert.notEqual(at, -1, `cascade block not found: ${selector}`);
  const open = css.indexOf('{', at);
  const close = css.indexOf('}', open);
  return css.slice(open + 1, close);
}
const definedIn = (text) => {
  const set = new Set();
  for (const m of text.matchAll(/(--[\w-]+)\s*:/g)) set.add(m[1]);
  return set;
};

const light = definedIn(body(':root {'));
const systemDark = definedIn(body(':root:not([data-theme="light"])')); // @media prefers-color-scheme: dark
const explicitDark = definedIn(body(':root[data-theme="dark"]'));

test('styles.css actually defines and references custom properties (#232 inc 6)', () => {
  assert.ok(referenced.size > 0, 'no var(--…) references parsed — the guard regex has broken');
  assert.ok(light.size > 0, 'no custom properties parsed from the light :root block');
});

test('every var(--…) in styles.css resolves in the light theme (#232 inc 6)', () => {
  const missing = [...referenced].filter((token) => !light.has(token)).sort();
  assert.deepEqual(
    missing,
    [],
    `undefined custom ${missing.length === 1 ? 'property' : 'properties'} referenced by styles.css: ${missing.join(', ')}`,
  );
});

test('every var(--…) in styles.css resolves in the dark theme (#232 inc 6)', () => {
  // `:root` matches in dark too, so its tokens cascade into both dark blocks;
  // assert against the union so a token defined only in an override still counts.
  const dark = new Set([...light, ...systemDark, ...explicitDark]);
  const missing = [...referenced].filter((token) => !dark.has(token)).sort();
  assert.deepEqual(missing, [], `undefined in dark: ${missing.join(', ')}`);
});

test('the two dark-theme blocks define the same token set (#232 inc 6)', () => {
  assert.deepEqual(
    [...systemDark].sort(),
    [...explicitDark].sort(),
    'the prefers-color-scheme dark block and [data-theme="dark"] have drifted apart',
  );
});

// ---------------------------------------------------------------------------
// #12 day-view week strip: the day box is an explicit 2×2 grid, one item per cell
// — the date number (.ws-date) at the top-left, the per-day status icon
// (.ws-clock) at the top-right, the weekday name (.ws-name) at the bottom-left
// and the hours total (.ws-total) at the bottom-right. This pins the *placement
// invariant* — not exact declaration strings or line numbers — so the layout
// cannot silently regress to a shared or overlapping cell. It also pins that the
// total's `num` class is not dead weight.
//
// The `body()` helper above locates a block by the first textual match of the
// selector, which is unsafe here: `.ws-clock` also appears in a comment and in
// the icon-sizing rule `.ws-clock [class*="p-icon--"]`, both *before* the grid
// rule. This guard therefore parses the sheet itself and reads the top-level
// (depth-0) rule; a `@media` override nested inside an at-rule is ignored, so an
// unrelated media query (e.g. the 480px `.ws-day.p-card` min-height) cannot
// shadow or break the check.

// Rules as `{ selector, body, depth }`; `depth` is the number of enclosing
// at-rule frames, so depth 0 is a base rule and a rule inside `@media` is deeper.
function rulesOf(src) {
  const clean = src.replace(/\/\*[\s\S]*?\*\//g, '');
  const rules = [];
  const stack = [];
  let start = 0;
  for (let i = 0; i < clean.length; i += 1) {
    const ch = clean[i];
    if (ch === '{') {
      stack.push({ prelude: clean.slice(start, i).trim(), bodyStart: i + 1, depth: stack.length });
      start = i + 1;
    } else if (ch === '}') {
      const frame = stack.pop();
      if (frame && !frame.prelude.startsWith('@')) {
        rules.push({ selector: frame.prelude, body: clean.slice(frame.bodyStart, i), depth: frame.depth });
      }
      start = i + 1;
    }
  }
  return rules;
}
const shippedRules = rulesOf(css);

// The body of the (top-level) rule whose selector list contains exactly
// `selector`. Whitespace inside the selector list is irrelevant.
function baseBody(selector) {
  const rule = shippedRules.find((r) => r.depth === 0
    && r.selector.split(',').map((s) => s.trim()).includes(selector));
  assert.ok(rule, `top-level rule not found: ${selector} { … }`);
  return rule.body;
}

// One `<prop>: <value>` declaration from a body, tolerant of declaration order
// and whitespace. Returns null when the property is not declared.
function declared(text, prop) {
  const m = text.match(new RegExp(`(?:^|[;{\\s])${prop}\\s*:\\s*([^;}]+)`, 'i'));
  return m ? m[1].trim() : null;
}

// The set of grid rows a `grid-row` value occupies, or null when the value is
// open-ended/relative (which the guard refuses to guess at). `2 / span 2`
// occupies rows {2, 3}; `1` occupies {1}; `2 / 3` occupies {2}.
function gridRows(value) {
  if (value === null) return null;
  const [a, b] = value.toLowerCase().replace(/\s+/g, ' ').split(' / ').map((s) => s.trim());
  const line = (s) => (/^\d+$/.test(s) ? Number(s) : null);
  const span = (s) => {
    const m = (s || '').match(/^span (\d+)$/);
    return m ? Number(m[1]) : null;
  };
  if (b === undefined) {
    const n = line(a);
    if (n !== null) return new Set([n]);
    const s = span(a);
    return s !== null ? new Set(Array.from({ length: s }, (_, i) => i + 1)) : null;
  }
  const start = line(a);
  if (start === null) return null;
  const endLine = line(b);
  if (endLine !== null) {
    return endLine > start
      ? new Set(Array.from({ length: endLine - start }, (_, i) => start + i))
      : null;
  }
  const spanCount = span(b);
  return spanCount !== null ? new Set(Array.from({ length: spanCount }, (_, i) => start + i)) : null;
}

// The set of grid columns a `grid-column` value occupies, or null when it is
// open-ended/relative. Same shape as `gridRows`, except `span n` is not expanded
// (there is no context-free column count), so those values stay unguessed.
function gridColumns(value) {
  if (value === null) return null;
  const [a, b] = value.toLowerCase().replace(/\s+/g, ' ').split(' / ').map((s) => s.trim());
  const line = (s) => (/^\d+$/.test(s) ? Number(s) : null);
  if (b === undefined) {
    const n = line(a);
    return n !== null ? new Set([n]) : null;
  }
  const start = line(a);
  const endLine = line(b);
  if (start === null || endLine === null || endLine <= start) return null;
  return new Set(Array.from({ length: endLine - start }, (_, i) => start + i));
}

// The single cell a grid item occupies, or null when either axis is not one
// unambiguous integer line (an open-ended/relative value the guard refuses to
// guess at, e.g. `1 / -1` or `span 2`).
function cellOf(selector) {
  const rows = gridRows(declared(baseBody(selector), 'grid-row'));
  const columns = gridColumns(declared(baseBody(selector), 'grid-column'));
  if (!rows || rows.size !== 1 || !columns || columns.size !== 1) return null;
  return { row: [...rows][0], column: [...columns][0] };
}
const cellKey = (cell) => (cell ? `${cell.row}:${cell.column}` : null);

// The four items of the day box, in the required 2×2 arrangement.
const PLACED = {
  '.ws-date': { row: 1, column: 1 }, // top-left
  '.ws-clock': { row: 1, column: 2 }, // top-right
  '.ws-name': { row: 2, column: 1 }, // bottom-left
  '.ws-total': { row: 2, column: 2 }, // bottom-right
};

test('the week-strip base rules parse, so the placement guard cannot pass vacuously (#12)', () => {
  for (const selector of ['.ws-day.p-card', ...Object.keys(PLACED)]) {
    assert.ok(baseBody(selector).length > 0, `${selector} has an empty base rule`);
  }
  for (const selector of Object.keys(PLACED)) {
    const body = baseBody(selector);
    const row = declared(body, 'grid-row');
    const column = declared(body, 'grid-column');
    assert.ok(
      gridRows(row)?.size === 1 && gridColumns(column)?.size === 1,
      `${selector} needs an explicit single-cell grid-row/grid-column (got \`${row}\` / \`${column}\`)`,
    );
  }
});

test('the four week-strip items occupy the four distinct cells of the day box (#12)', () => {
  const cells = Object.fromEntries(
    Object.keys(PLACED).map((selector) => [selector, cellKey(cellOf(selector))]),
  );
  for (const [selector, cell] of Object.entries(cells)) {
    assert.ok(cell, `${selector} must resolve to exactly one grid cell`);
  }
  const keys = Object.values(cells);
  assert.deepEqual(
    [...new Set(keys)].length,
    keys.length,
    `two week-strip items share a cell: ${JSON.stringify(cells)}`,
  );
  assert.deepEqual(
    cells,
    Object.fromEntries(Object.entries(PLACED).map(([s, c]) => [s, `${c.row}:${c.column}`])),
    'the week-strip items drifted out of the required date/clock/name/total 2×2 cells',
  );
});

test('the week-strip status icon sits in the top-right cell and stays in flow (#12)', () => {
  assert.deepEqual(
    cellOf('.ws-clock'),
    PLACED['.ws-clock'],
    '.ws-clock must occupy the top-right cell (grid-row 1 / grid-column 2)',
  );
  const body = baseBody('.ws-clock');
  assert.match(
    (declared(body, 'justify-self') || '').trim(),
    /^end$/i,
    '.ws-clock must be pushed to the end edge (justify-self: end)',
  );
  const position = declared(body, 'position');
  assert.ok(
    position === null || !/^(absolute|fixed)$/i.test(position),
    `.ws-clock must stay in the grid flow (position: static or omitted), got position: ${position}`,
  );
});

test('the hours total hugs the bottom-right edge and its `num` class is not dead weight (#12)', () => {
  assert.deepEqual(
    cellOf('.ws-total'),
    PLACED['.ws-total'],
    '.ws-total must occupy the bottom-right cell (grid-row 2 / grid-column 2)',
  );
  assert.match(
    (declared(baseBody('.ws-total'), 'justify-self') || '').trim(),
    /^end$/i,
    '.ws-total must be pushed to the end edge (justify-self: end)',
  );
  // `num` is a table-scoped utility (td.num, th.num); .ws-total is a span, so
  // the pair has to be named in that rule's selector list or the class is inert.
  const num = baseBody('.ws-total.num');
  assert.match(
    (declared(num, 'text-align') || '').trim(),
    /^right$/i,
    '.ws-total.num must be right-aligned by the shared `num` rule (else the class does nothing)',
  );
});

test('the week-strip day box keeps row-gap 0 so an icon-less box gains no empty line (#12)', () => {
  const body = baseBody('.ws-day.p-card');
  const rowGap = declared(body, 'row-gap')
    ?? (declared(body, 'gap') || '').split(/\s+/).filter(Boolean)[0]
    ?? null;
  assert.ok(
    rowGap === null || Number.parseFloat(rowGap) === 0,
    `.ws-day.p-card must not add a non-zero row-gap (a box with no status icon must gain no empty line), got \`${rowGap}\``,
  );
});

// #12/#108 follow-up (found in Firefox, invisible to jsdom): a `display` other
// than `table-cell` on a `<th>` drops its table-cell role. The browser then
// wraps it in an anonymous table cell and the whole header row collapses into a
// single column, so the day titles no longer line up with their day columns.
// The 2x2 header grid must therefore live on an inner element (`.wk-head`), and
// no rule may hand a `#week-table` `<th>` a different `display`.
test('no week-table rule replaces the table-cell display of a <th> (#12)', () => {
  const subjectIsTh = (part) => {
    const last = part.split(/[\s>+~]+/).filter(Boolean).pop() || '';
    return /^th(\b|\[|:|#|\.)/.test(last);
  };
  const offenders = [];
  for (const rule of shippedRules) {
    const display = declared(rule.body, 'display');
    if (display === null || /^table-cell$/i.test(display.trim())) continue;
    for (const part of rule.selector.split(',').map((s) => s.trim())) {
      if (part.includes('#week-table') && subjectIsTh(part)) offenders.push(`${part} { display: ${display.trim()} }`);
    }
  }
  assert.deepEqual(
    offenders,
    [],
    `a <th> in #week-table is styled with a non-table-cell display, which makes the browser wrap it in an anonymous cell and collapse the header row: ${offenders.join('; ')}`,
  );
});

test('the week-strip day box declares an explicit two-column, two-row grid (#12)', () => {
  const body = baseBody('.ws-day.p-card');
  // Count top-level tracks only: a space inside `minmax(0, 1fr)` is not a split.
  const tracks = (prop) => (declared(body, prop) || '')
    .trim()
    .split(/\s+(?![^()]*\))/)
    .filter(Boolean).length;
  assert.equal(
    tracks('grid-template-columns'),
    2,
    `.ws-day.p-card must declare a 2-track grid-template-columns, got \`${declared(body, 'grid-template-columns')}\``,
  );
  assert.equal(
    tracks('grid-template-rows'),
    2,
    `.ws-day.p-card must declare a 2-track grid-template-rows, got \`${declared(body, 'grid-template-rows')}\``,
  );
});

test('the week day-column header grid is declared on the inner .wk-head span (#12)', () => {
  const body = baseBody('#week-table.grid thead th .wk-head');
  assert.match(
    (declared(body, 'display') || '').trim(),
    /^grid$/i,
    'the 2x2 header grid belongs on .wk-head, not on the <th>',
  );
  assert.equal(baseBody('#week-table.grid thead th .wk-date').includes('grid-row'), true,
    'the date/clock/weekday placement rules still target the .wk-head children');
});
