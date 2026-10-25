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
  paymentRailRow, paymentRailRows, renumberPaymentRails, setPaymentRails, addPaymentRail,
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
  assert.match(label, /Monthly open versus closed amounts/,
    `the accessible name states the Open/Closed pairing: ${label}`);
  assert.match(label, /open 1800\.00/, `summary must ride the name: ${label}`);
  assert.match(label, /closed 0\.00/, `the settled series is Closed, never Paid: ${label}`);
  // Children of role="img" are presentational — the old sr-only child is gone.
  assert.equal(chart.querySelector('.sr-only'), null,
    'no DOM summary inside a role="img" subtree');
  dom.window.close();
});

test('payment-rail rows are labelled fieldsets with keyboard-operable controls (#354)', () => {
  const { dom, t, document } = setup();
  t.setPaymentRails([{
    currency: 'EUR', account_name: 'Tatu', iban: 'EX000000000000000000', instructions: 'Quote INV-1',
  }]);
  const row = document.querySelector('#pay-rails .payment-rail');
  assert.ok(row, 'the rail renders as a fieldset');
  assert.equal(row.tagName, 'FIELDSET');
  assert.equal(row.querySelector('legend').textContent, 'Payment rail 1', 'the legend names the group');
  for (const control of row.querySelectorAll('input, textarea')) {
    assert.ok(control.id, `${control.dataset.railField} has an id`);
    assert.equal(control.labels.length, 1, `${control.id} has exactly one label`);
    assert.equal(control.labels[0].getAttribute('for'), control.id, `${control.id} label targets it`);
    assert.ok(control.labels[0].textContent.trim(), `${control.id} has a visible name`);
    assert.notEqual(control.tabIndex, -1, `${control.id} stays in the tab order`);
  }
  // The currency input is a free 3-letter entry with a datalist of common codes.
  const currency = document.getElementById('pay-rail-0-currency');
  assert.equal(currency.getAttribute('list'), 'pay-rail-currencies');
  assert.equal(currency.maxLength, 3);
  assert.ok(document.getElementById('pay-rail-currencies').options.length >= 4);

  // Add/remove are real buttons, outside any click-only control: a keyboard
  // user activates them with Enter/Space like every other button.
  const add = document.getElementById('pay-rail-add');
  assert.equal(add.tagName, 'BUTTON');
  assert.equal(add.getAttribute('type'), 'button');
  assert.notEqual(add.tabIndex, -1);
  const remove = row.querySelector('button');
  assert.equal(remove.tagName, 'BUTTON');
  assert.equal(remove.getAttribute('type'), 'button');
  assert.equal(remove.getAttribute('aria-label'), 'Remove payment rail 1');
  assert.ok(remove.textContent.trim(), 'the remove control also has visible text');

  t.addPaymentRail();
  assert.equal(t.paymentRailRows().length, 2);
  t.paymentRailRows()[0].querySelector('button').click();
  assert.equal(t.paymentRailRows().length, 1, 'clicking the remove button removes its row');
  assert.equal(document.getElementById('pay-rail-0-currency').value, '', 'the surviving row renumbers');
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
  for (const want of ['Line description', 'Quantity', 'Unit price', 'Linked project']) {
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
    if (dlg.id === 'navigation-drawer') {
      assert.ok(dlg.classList.contains('navigation-drawer'));
      assert.equal(dlg.getAttribute('aria-label'), 'Application navigation');
      assert.equal(dlg.getAttribute('aria-modal'), 'true');
      assert.equal(document.getElementById('navigation-open').getAttribute('aria-controls'), dlg.id);
      continue;
    }
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

test('every dialog ships a visible cancel button (#141)', () => {
  // The + Track time dialog shipped its only way out with `hidden` (it was only
  // revealed while editing), and the setup wizard hid its only dismiss button on
  // its last step — a popup you cannot visibly back out of. Every native dialog
  // must carry a cancel affordance that is not hidden in the markup.
  const document = new JSDOM(html).window.document;
  const dialogs = [...document.querySelectorAll('dialog')];
  assert.ok(dialogs.length >= 10, `expected the full dialog set, got ${dialogs.length}`);
  for (const dlg of dialogs) {
    if (dlg.id === 'navigation-drawer') {
      const close = document.querySelector('#sidebar #navigation-close');
      assert.ok(close && !close.hidden, 'the relocated sidebar carries the drawer close control');
      assert.equal(close.getAttribute('aria-label'), 'Close navigation');
      continue;
    }
    // Close is the same affordance: a document preview says Close (the receipt
    // dialog), and the rule is a visible way out, not a particular word.
    const cancels = [...dlg.querySelectorAll('button')]
      .filter((b) => /cancel|close/i.test(`${b.id} ${b.textContent}`));
    assert.ok(cancels.length > 0, `${dlg.id} has no cancel button`);
    assert.ok(
      cancels.some((b) => !b.hasAttribute('hidden')),
      `${dlg.id} ships its cancel button hidden — every popup needs a visible way out`,
    );
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
  walk(parse(vendorCss), {
    visit: 'Rule', enter(node) {
      const values = {};
      node.block.children.forEach((declaration) => {
        if (declaration.type === 'Declaration') values[declaration.property] = generate(declaration.value);
      });
      if (values['--vf-color-background-default'] && values['--vf-color-text-default']) themes.push(values);
    }
  });
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


// ----------------------------------------------- tablists (#232 increment 5) --
//
// The app tablists are now the official p-tabs pattern: `p-tabs__list` on the
// role="tablist" element, every tab wrapped in `p-tabs__item` with the button
// carrying `p-tabs__link`. These pin the vocabulary, the full ARIA tab wiring
// and the active/inactive contrast the migration had to keep.

test('every tablist adopts the official p-tabs vocabulary with the full ARIA tab wiring (#232 inc 5)', () => {
  const document = new JSDOM(html).window.document;
  const tablists = [...document.querySelectorAll('[role="tablist"]')];
  assert.ok(tablists.length >= 3, `expected the three app tablists, got ${tablists.length}`);
  for (const list of tablists) {
    const name = list.id || list.getAttribute('aria-label');
    assert.ok(list.classList.contains('p-tabs__list'), `${name} is the official p-tabs__list`);
    const tabs = [...list.querySelectorAll('[role="tab"]')];
    assert.ok(tabs.length > 1, `${name} holds its tabs`);
    for (const tab of tabs) {
      assert.ok(tab.classList.contains('p-tabs__link'), `${tab.id} carries p-tabs__link`);
      assert.ok(tab.parentElement.classList.contains('p-tabs__item'), `${tab.id} sits in a p-tabs__item`);
      assert.ok(['true', 'false'].includes(tab.getAttribute('aria-selected')), `${tab.id} declares aria-selected`);
      const panel = document.getElementById(tab.getAttribute('aria-controls'));
      assert.ok(panel, `${tab.id} points at a panel`);
      assert.equal(panel.getAttribute('role'), 'tabpanel', `${tab.id} controls a tabpanel`);
      assert.equal(panel.getAttribute('aria-labelledby'), tab.id, `${tab.id} gives its panel its name`);
    }
    const tabStops = tabs.filter((tab) => tab.tabIndex === 0);
    assert.equal(tabStops.length, 1, `${name} keeps a roving tabindex (one tab stop)`);
    assert.equal(tabStops[0].getAttribute('aria-selected'), 'true', `${name}'s tab stop is the selected tab`);
  }
});

test('the bespoke tab skins are retired in favour of the p-tabs adapter (#232 inc 5)', () => {
  assert.match(stylesCss, /\.sidebar \.p-tabs__link\[aria-selected="true"\]/, 'the rail adapter is present');
  assert.ok(!/\.tabs \[role="tab"\]/.test(stylesCss), 'the bespoke .tabs [role="tab"] skin is gone');
  assert.ok(!/\.seg \[role="tab"\]/.test(stylesCss), 'the bespoke .seg [role="tab"] segment skin is gone');
  // `.seg` survives for the invoice Open|All toggle group, which is a
  // role="group" of aria-pressed buttons — not a tablist.
  assert.match(stylesCss, /\.seg \{\n\s+display: inline-flex/, 'the .seg shell stays for the toggle group');
});
test('the Open|All filter keeps its .seg semantics under the Ubuntu skin (#F)', () => {
  const { dom, document } = setup();
  const group = document.querySelector('#inv-list-head .seg');
  assert.ok(group, 'the status toggle is still there (#F)');
  assert.equal(group.closest('.filter-field') !== null, true,
    'the toggle is a filter field, like every control beside it');
  assert.equal(group.getAttribute('role'), 'group', 'a group, never a tablist');
  assert.equal(group.getAttribute('aria-labelledby'), 'inv-status-label',
    'the group takes its accessible name from its field label');
  const buttons = [...group.querySelectorAll('button')];
  assert.equal(buttons.length, 2, 'Open | All');
  for (const button of buttons) {
    assert.ok(button.classList.contains('seg-btn'), button.id + ' keeps the segment skin');
    assert.ok(['true', 'false'].includes(button.getAttribute('aria-pressed')),
      button.id + ' stays an aria-pressed toggle');
  }
  assert.equal(group.querySelector('[role="tab"]'), null, 'no tab pattern is introduced');
  // The restyle is the app-owned skin over the same group: the pressed segment
  // is filled with the accent tint in the rail's selected-tab ink (the pairing
  // contrast.mjs pins for both themes) and carries a bottom indicator.
  assert.match(stylesCss, /\.seg \.seg-btn\[aria-pressed="true"\] \{ background: var\(--accent-soft\); color: var\(--brand-deep\)/,
    'the pressed segment carries the Ubuntu skin on the app ink');
  // The row itself is the shared primitive, and the status control shares the
  // fields' control height and underline boundary instead of a floating box.
  assert.match(stylesCss, /\.filter-row,\n\.hierarchy-filters \{ display: flex; flex-wrap: wrap; align-items: flex-end; gap: var\(--space-sm\); \}/,
    'the register filter row is the shared .filter-row primitive');
  assert.match(stylesCss, /\.seg \{\n\s+display: inline-flex; align-items: stretch; height: var\(--control-height\);/,
    'the status control is sized to the design-system control height');
  assert.match(stylesCss, /\.seg \{\n\s+display: inline-flex;[^}]*border-bottom: 1px solid var\(--field-rule\);/,
    'the status control uses the same underline boundary as the fields');
  assert.ok(!/\.inv-filters/.test(stylesCss), 'the view-specific wrapper and its padding override are gone');
  dom.window.close();
});


test('the selected tab is signalled by the text colour, not a faint tint, in both themes (#232 inc 5)', () => {
  // The classic tab contrast gap is an active state carried by a low-contrast
  // tint. Vanilla paints the selected indicator bar with --vf-color-text-default
  // — the same token as the tab's own ink — so the active tab is never lighter
  // than the inactive one; assert that pairing *and* its AA ratio per theme.
  assert.match(vendorCss, /\[aria-selected=true\]::before\{[^}]*background-color:var\(--vf-color-text-default\)/,
    'the selected bar is painted in the tab text colour');
  const themes = [];
  walk(parse(vendorCss), {
    visit: 'Rule', enter(node) {
      const values = {};
      node.block.children.forEach((declaration) => {
        if (declaration.type === 'Declaration') values[declaration.property] = generate(declaration.value);
      });
      if (values['--vf-color-text-default'] && values['--vf-color-background-default']) themes.push(values);
    }
  });
  assert.ok(themes.length >= 2, 'light + dark token sets');
  for (const theme of themes) {
    const ratio = contrast(theme['--vf-color-text-default'], theme['--vf-color-background-default']);
    assert.ok(ratio >= 4.5, `tab ink on the tab background: ${ratio.toFixed(2)} < 4.5`);
  }
});
