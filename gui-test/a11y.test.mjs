// #191 stage 1: accessibility + Vanilla-adoption invariants, checked in
// jsdom against the real index.html/app.js: the invoice chart's accessible
// name carries the per-month summary (role="img" hides its subtree), the
// contact editor and invoice-line inputs have real accessible names, and
// explicit data-icon/data-positive drive the Ubuntu chrome even when labels
// no longer match the old text heuristics.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { parse, walk, generate } from 'css-tree';
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
window.t = {
  dashRenderOverview, state, api,
  contactRow, edLineRow, decorate: decorateUbuntuButton,
};`);
  return { dom, t: dom.window.t, document: dom.window.document };
}

test('invoice chart exposes its numbers through the accessible name (#191)', () => {
  const { dom, t, document } = setup();
  t.dashRenderOverview([
    {
      id: 'i1', number: 'INV-1', status: 'issued', currency: 'EUR',
      period_to: '2026-03-10', total_minor: 180000, payments: [],
    },
  ]);
  const chart = document.getElementById('inv-chart');
  assert.equal(chart.getAttribute('role'), 'img');
  const label = chart.getAttribute('aria-label') || '';
  assert.match(label, /open 1800\.00/, `summary must ride the name: ${label}`);
  // Children of role="img" are presentational — the old sr-only child is gone.
  assert.equal(chart.querySelector('.sr-only'), null,
    'no DOM summary inside a role="img" subtree');
  dom.window.close();
});

test('contact editor inputs have accessible names (#191)', () => {
  const { dom, t, document } = setup();
  const row = t.contactRow();
  document.body.appendChild(row);
  for (const input of row.querySelectorAll('input[type="text"], input[type="email"]')) {
    assert.ok(input.getAttribute('aria-label'), `aria-label required on ${input.placeholder}`);
  }
  dom.window.close();
});

test('invoice-line inputs have accessible names (#191)', () => {
  const { dom, t, document } = setup();
  const row = t.edLineRow();
  document.body.appendChild(row);
  const named = [...row.querySelectorAll('input, select')]
    .map((n) => n.getAttribute('aria-label') || n.getAttribute('placeholder') || '');
  for (const want of ['Line description', 'Quantity', 'Unit price', 'Product or service']) {
    assert.ok(named.includes(want), `missing accessible name: ${want}`);
  }
  dom.window.close();
});

test('data-icon/data-positive decorate buttons independently of label text (#191)', () => {
  const { dom, t, document } = setup();
  const button = document.createElement('button');
  // A renamed label ("Generate weekly statement") would have lost every
  // heuristic icon under the old text-matching; data-icon keeps it stable.
  button.dataset.icon = 'history';
  button.dataset.positive = '1';
  button.textContent = 'Generate weekly statement';
  document.body.appendChild(button);
  t.decorate(button);
  assert.ok(button.querySelector('[class*="p-icon--history"]'), 'explicit icon applied');
  assert.ok(button.classList.contains('p-button--positive'), 'explicit positive applied');
  dom.window.close();
});

test('the wizard button keeps its plus glyph without the label prefix (#191)', () => {
  const { dom, t, document } = setup();
  const button = document.getElementById('customer-new');
  t.decorate(button);
  assert.ok(button.querySelector('[class*="p-icon--plus"]'), 'plus icon from data-icon');
  assert.equal(button.textContent, 'Customer', 'label text unchanged by decoration');
  dom.window.close();
});

// ------------------------------------------------ dialogs (#232 increment 4) --
//
// The popups are now the official Vanilla p-modal pattern: the native <dialog>
// carries `p-modal` (keeping the top-layer/backdrop/inert/focus-trap from #102)
// and wraps a `p-modal__dialog` box with a `p-modal__header`/`p-modal__title`
// and a `p-modal__footer`. These pin the class vocabulary, the accessible name
// wiring and the dialog-chrome contrast so the migration cannot silently drift.

test('every dialog adopts the official p-modal pattern with an accessible name (#232 inc 4)', () => {
  const document = new JSDOM(html).window.document;
  const dialogs = [...document.querySelectorAll('dialog')];
  assert.ok(dialogs.length >= 10, `expected the full dialog set, got ${dialogs.length}`);
  for (const dlg of dialogs) {
    assert.ok(dlg.classList.contains('p-modal'), `${dlg.id} is the p-modal overlay`);
    assert.equal(dlg.getAttribute('aria-modal'), 'true', `${dlg.id} is aria-modal`);
    const labelledby = dlg.getAttribute('aria-labelledby');
    assert.ok(labelledby, `${dlg.id} names itself via aria-labelledby`);
    const title = document.getElementById(labelledby);
    assert.ok(title && dlg.contains(title), `${dlg.id} aria-labelledby resolves to its title`);
    // #229: the wizard keeps its p-card frame rather than a p-modal__dialog box.
    if (dlg.id === 'wizard-dialog') {
      assert.ok([...dlg.children].some((child) => child.classList.contains('p-card')), 'wizard keeps the p-card frame');
      continue;
    }
    const box = [...dlg.children].find((child) => child.classList.contains('p-modal__dialog'));
    assert.ok(box, `${dlg.id} has a p-modal__dialog box`);
    assert.ok(box.contains(title), `${dlg.id} title resolves inside the box`);
    assert.ok(title.classList.contains('p-modal__title'), `${dlg.id} title uses p-modal__title`);
    assert.ok(title.closest('.p-modal__header'), `${dlg.id} title sits in a p-modal__header`);
    assert.ok(box.querySelector('.p-modal__footer'), `${dlg.id} has a p-modal__footer`);
  }
});

test('the bespoke dialog skins are retired, keeping only the p-modal adapter (#232 inc 4)', () => {
  assert.ok(stylesCss.includes('dialog.p-modal'), 'the native dialog keeps the overlay role');
  assert.ok(!stylesCss.includes('dialog { max-height'), 'no generic <dialog> box rule survives');
  assert.ok(!stylesCss.includes('rgba(44, 0, 30, 0.55)'), 'the old aubergine backdrop is gone');
  assert.match(stylesCss, /#wizard-dialog \.p-card \{[^}]*max-width: 30rem/, 'wizard keeps its p-card frame');
});

test('dialog chrome meets AA in both framework themes (#232 inc 4)', () => {
  const themes = [];
  walk(parse(vendorCss), { visit: 'Rule', enter(node) {
    const values = {};
    node.block.children.forEach((declaration) => {
      if (declaration.type === 'Declaration') values[declaration.property] = generate(declaration.value);
    });
    if (values['--vf-color-background-default'] && values['--vf-color-text-default']) themes.push(values);
  } });
  assert.ok(themes.length >= 2, 'light + dark token sets');
  for (const theme of themes) {
    const ratio = contrast(theme['--vf-color-text-default'], theme['--vf-color-background-default']);
    assert.ok(ratio >= 4.5, `dialog text on chrome: ${ratio.toFixed(2)} < 4.5`);
  }
});

function contrast(a, b) {
  const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

function lum(colour) {
  const channel = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const { r, g, b } = parseColour(colour);
  return 0.2126 * channel(r / 255) + 0.7152 * channel(g / 255) + 0.0722 * channel(b / 255);
}

function parseColour(value) {
  value = String(value).trim();
  let match = value.match(/^#([0-9a-f]{3}|[0-9a-f]{6})$/i);
  if (match) {
    const hex = match[1].length === 3 ? match[1].replace(/./g, (digit) => digit + digit) : match[1];
    const n = parseInt(hex, 16);
    return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
  }
  match = value.match(/^rgba?\(([^)]+)\)$/i);
  if (match) {
    const parts = match[1].split(',').map((part) => parseFloat(part));
    return { r: parts[0], g: parts[1], b: parts[2] };
  }
  match = value.match(/^hsla?\(([^)]+)\)$/i);
  if (match) {
    const [hue, sat, light] = match[1].split(',').map((part) => parseFloat(part));
    const s = sat / 100;
    const l = light / 100;
    const c = (1 - Math.abs(2 * l - 1)) * s;
    const hp = (((hue % 360) + 360) % 360) / 60;
    const x = c * (1 - Math.abs((hp % 2) - 1));
    const [r1, g1, b1] = hp < 1 ? [c, x, 0] : hp < 2 ? [x, c, 0] : hp < 3 ? [0, c, x]
      : hp < 4 ? [0, x, c] : hp < 5 ? [x, 0, c] : [c, 0, x];
    const m = l - c / 2;
    return { r: Math.round((r1 + m) * 255), g: Math.round((g1 + m) * 255), b: Math.round((b1 + m) * 255) };
  }
  assert.fail(`unparsed colour: ${value}`);
}
