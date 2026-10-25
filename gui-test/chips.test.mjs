// #232 increment 3: status pills and notifications are now the official Vanilla
// components. This suite pins the class vocabulary per status variant (so the
// migration is regression-guarded, not merely visual) and re-checks the
// foreground/background pairs the increment introduces for WCAG AA in both
// framework themes. Offline jsdom against the real index.html + app.js, in the
// style of a11y.test.mjs / ubuntu.test.mjs.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();
const vendorCss = readFileSync(new URL('../web/vendor/ubuntu/components.css', import.meta.url), 'utf8');
const stylesCss = readFileSync(new URL('../web/styles.css', import.meta.url), 'utf8');

function setup() {
  const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
  dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
  dom.window.eval(`${app}
window.t = { state, statusChip, notificationNode, notificationSeverity, refreshCustomerTable };`);
  return { dom, t: dom.window.t, document: dom.window.document };
}

// --------------------------------------------------------------- classes --

test('status pills use the official chip vocabulary for both variants (#232 inc 3)', () => {
  const { dom, t, document } = setup();
  const neutral = t.statusChip('Inactive');
  const emphasised = t.statusChip('Active', true);
  document.body.append(neutral, emphasised);
  assert.equal(neutral.className, 'p-chip is-inline');
  assert.equal(neutral.querySelector('.p-chip__value').textContent, 'Inactive');
  assert.equal(neutral.textContent, 'Inactive');
  assert.equal(emphasised.className, 'p-chip is-inline p-chip--positive');
  assert.equal(emphasised.textContent, 'Active');
  assert.equal(document.querySelector('.badge'), null, 'no bespoke badge vocabulary survives');
  dom.window.close();
});

test('register rows carry the chip variant matching the record state (#232 inc 3)', async () => {
  const { dom, t, document } = setup();
  t.state.customers = [
    { id: 'c1', name: 'TatuTime', currency: 'EUR', default_rate_minor: 6000, active: true },
    { id: 'c2', name: 'Globex', currency: 'EUR', default_rate_minor: 4500, active: false },
  ];
  await t.refreshCustomerTable();
  const chips = [...document.querySelectorAll('#customer-table tbody .p-chip')];
  assert.equal(chips.length, 2);
  assert.ok(chips[0].classList.contains('p-chip--positive'), 'active record is the positive chip');
  assert.equal(chips[1].classList.contains('p-chip--positive'), false, 'inactive record is the neutral chip');
  assert.deepEqual(chips.map((chip) => chip.textContent), ['Active', 'Inactive']);
  dom.window.close();
});

test('notifications render the canonical p-notification markup (#232 inc 3)', () => {
  const { dom, t, document } = setup();
  assert.equal(t.notificationSeverity('no_time_today'), 'information', 'known kind keeps its severity');
  assert.equal(t.notificationSeverity('some_future_kind'), 'information', 'unknown kind defaults to information');
  const known = t.notificationNode({ kind: 'no_time_today', title: 'No time logged', body: 'Track today.' });
  const empty = t.notificationNode({ kind: 'no_time_today', title: 'Title only' });
  document.body.append(known, empty);
  assert.equal(known.className, 'p-notification p-notification--information');
  assert.equal(known.querySelector('.p-notification__content .p-notification__title').textContent, 'No time logged');
  assert.equal(known.querySelector('.p-notification__content .p-notification__message').textContent, 'Track today.');
  assert.equal(empty.querySelector('.p-notification__message'), null, 'empty body emits no message node');
  dom.window.close();
});

test('announcement surfaces stay the polite live region and alert errors (#232 inc 3)', () => {
  const { dom, document } = setup();
  const live = document.getElementById('live-region');
  assert.equal(live.getAttribute('role'), 'status');
  assert.equal(live.getAttribute('aria-live'), 'polite');
  assert.equal(live.classList.contains('p-notification'), false, 'live region is not a notification node');
  // #232 phase 2b: the error paragraphs are the official validation messages now.
  assert.ok(document.querySelectorAll('.p-form-validation__message[role="alert"]').length > 0, 'field errors stay role=alert');
  // The Day view's empty state is the notification chip alone: the duplicate
  // `#day-empty` paragraph it used to show beside it is gone.
  assert.equal(document.getElementById('day-empty'), null, 'no duplicated day empty-state notice');
  dom.window.close();
});

// -------------------------------------------------------------- contrast --

// The chip/notification colours come from the pinned Vanilla token set, whose
// light values live under `:root,.is-light` and dark under `.is-dark`. The chip
// surfaces are translucent, so each pair is composited over the app's own
// surface token for the matching theme — the closest a static audit can get to
// what a browser paints. Like contrast.mjs this names every pair it checks.
function ruleBody(css, selector) {
  for (const candidate of [selector + '{', selector + ' {']) {
    const at = css.indexOf(candidate);
    if (at === -1) continue;
    const before = css[at - 1];
    if (at !== 0 && !'}, \n'.includes(before)) continue;
    const open = css.indexOf('{', at + selector.length);
    return css.slice(open + 1, css.indexOf('}', open));
  }
  assert.fail(`selector not found: ${selector}`);
}

function token(body, name) {
  const match = body.match(new RegExp(`--${name}:([^;}]*)`));
  assert.ok(match, `token not found: ${name}`);
  return match[1].trim();
}

function rgba(value) {
  let match = value.match(/^#([0-9a-f]{3}|[0-9a-f]{6})$/i);
  if (match) {
    const hex = match[1].length === 3 ? match[1].replace(/./g, (digit) => digit + digit) : match[1];
    const n = parseInt(hex, 16);
    return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255, a: 1 };
  }
  match = value.match(/^rgba?\(([^)]+)\)$/i);
  if (match) {
    const parts = match[1].split(',').map((part) => parseFloat(part));
    return { r: parts[0], g: parts[1], b: parts[2], a: parts[3] ?? 1 };
  }
  match = value.match(/^hsla?\(([^)]+)\)$/i);
  assert.ok(match, `unparsed colour: ${value}`);
  const [hue, sat, light, alpha = 1] = match[1].split(',').map((part) => parseFloat(part));
  const s = sat / 100;
  const l = light / 100;
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const hp = (((hue % 360) + 360) % 360) / 60;
  const x = c * (1 - Math.abs((hp % 2) - 1));
  const [r1, g1, b1] = hp < 1 ? [c, x, 0] : hp < 2 ? [x, c, 0] : hp < 3 ? [0, c, x]
    : hp < 4 ? [0, x, c] : hp < 5 ? [x, 0, c] : [c, 0, x];
  const m = l - c / 2;
  return { r: Math.round((r1 + m) * 255), g: Math.round((g1 + m) * 255), b: Math.round((b1 + m) * 255), a: alpha };
}

const over = (fg, bg) => ({
  r: fg.r * fg.a + bg.r * (1 - fg.a),
  g: fg.g * fg.a + bg.g * (1 - fg.a),
  b: fg.b * fg.a + bg.b * (1 - fg.a),
  a: 1,
});

function luminance({ r, g, b }) {
  const ch = (channel) => {
    const v = channel / 255;
    return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b);
}

function ratio(a, b) {
  const [hi, lo] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (hi + 0.05) / (lo + 0.05);
}

test('chip + notification colour pairs meet AA in both themes (#232 inc 3)', () => {
  const lightTokens = ruleBody(vendorCss, ':root,.is-light');
  const darkTokens = ruleBody(vendorCss, '.is-dark');
  const surfaces = {
    light: token(ruleBody(stylesCss, ':root'), 'surface'),
    dark: token(ruleBody(stylesCss, ':root[data-theme="dark"]'), 'surface'),
  };
  const variants = ['neutral', 'positive', 'caution', 'negative', 'information'];
  for (const [theme, tokens] of [['light', lightTokens], ['dark', darkTokens]]) {
    const text = rgba(token(tokens, 'vf-color-text-default'));
    const surface = rgba(surfaces[theme]);
    for (const variant of variants) {
      const bg = over(rgba(token(tokens, `vf-color-background-${variant}-default`)), surface);
      const r = ratio(text, bg);
      assert.ok(r >= 4.5, `${theme} chip --${variant} text: ${r.toFixed(2)} < 4.5`);
    }
    const r = ratio(text, rgba(token(tokens, 'vf-color-background-default')));
    assert.ok(r >= 4.5, `${theme} notification text: ${r.toFixed(2)} < 4.5`);
  }
});
