// #258: the sidebar is the app's *vertical* tablist (aria-orientation="vertical"
// in web/index.html), so its roving-tabindex keyboard navigation must follow
// that announced axis. Before #258 the handler only moved on
// ArrowLeft/ArrowRight/Home/End, so a user who followed the declared orientation
// and pressed ↓ could not move between sections at all. These tests drive the
// real index.html + app.js in jsdom: the vertical tablist responds to Up/Down,
// the horizontal tablists keep Left/Right, and both keep Home/End.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

// Wire the tab handlers without the network: every api call resolves, and
// /auth/status has no `initialised`, so initAuth() returns falsy and startApp()
// never runs to steal focus mid-test.
function setup() {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
    const stub = `
api.get = async () => ({});
api.post = async () => ({});
initTabs();         // vertical sidebar tablist (#tabs)
initSettingsTabs(); // horizontal tablist (#settings-tabs)
window.kb = { nextTabIndex };`;
    dom.window.eval(`${app}\n${stub}`);
    return { dom, kb: dom.window.kb, document: dom.window.document };
}

const activeId = (document) => (document.activeElement ? document.activeElement.id : '');

// Dispatch the key on whatever currently holds focus — exactly what a keyboard
// user does after a tab has moved focus with the roving tabindex.
function press(dom, key) {
    const target = dom.window.document.activeElement;
    target.dispatchEvent(new dom.window.KeyboardEvent('keydown', {
        key, bubbles: true, cancelable: true,
    }));
}

test('vertical sidebar tablist moves focus on ArrowDown/ArrowUp (#258)', () => {
    const { dom, document } = setup();
    assert.equal(
        document.getElementById('tabs').getAttribute('aria-orientation'),
        'vertical',
        'sidebar announces a vertical tablist',
    );
    document.getElementById('tab-timesheet').focus();
    assert.equal(activeId(document), 'tab-timesheet', 'starts on the selected section');

    press(dom, 'ArrowDown');
    assert.equal(activeId(document), 'tab-customers', 'ArrowDown → next sidebar section');
    press(dom, 'ArrowDown');
    assert.equal(activeId(document), 'tab-projects', 'ArrowDown again → next section');
    press(dom, 'ArrowUp');
    assert.equal(activeId(document), 'tab-customers', 'ArrowUp → previous sidebar section');
    dom.window.close();
});

test('vertical sidebar keeps Home/End and wrapping (#258)', () => {
    const { dom, document } = setup();
    document.getElementById('tab-timesheet').focus();
    press(dom, 'End');
    assert.equal(activeId(document), 'tab-settings', 'End → last section');
    press(dom, 'Home');
    assert.equal(activeId(document), 'tab-timesheet', 'Home → first section');
    press(dom, 'ArrowUp');
    assert.equal(activeId(document), 'tab-settings', 'ArrowUp wraps from the first to the last');
    dom.window.close();
});

test('horizontal tablist still moves on ArrowLeft/ArrowRight (#258)', () => {
    const { dom, document } = setup();
    assert.equal(
        document.getElementById('settings-tabs').hasAttribute('aria-orientation'),
        false,
        'horizontal is the ARIA default — the tablist needs no aria-orientation',
    );
    // The settings section owns this tablist and is hidden until shown; reveal it
    // so its tabs are focusable, as they are for a user on the Settings view.
    document.getElementById('panel-settings').hidden = false;
    document.getElementById('settings-tab-security').focus();
    assert.equal(activeId(document), 'settings-tab-security', 'starts on the first settings tab');
    press(dom, 'ArrowRight');
    assert.equal(activeId(document), 'settings-tab-users', 'ArrowRight → next settings tab');
    press(dom, 'ArrowLeft');
    assert.equal(activeId(document), 'settings-tab-security', 'ArrowLeft → previous settings tab');
    press(dom, 'ArrowDown');
    assert.equal(
        activeId(document),
        'settings-tab-security',
        'ArrowDown is ignored on a horizontal tablist',
    );
    dom.window.close();
});

test('nextTabIndex maps the arrow axis to the declared orientation (#258)', () => {
    const { kb, dom } = setup();
    // Vertical: Up/Down navigate, Left/Right are inert.
    assert.equal(kb.nextTabIndex('ArrowDown', 0, 3, 'vertical'), 1);
    assert.equal(kb.nextTabIndex('ArrowUp', 0, 3, 'vertical'), 2, 'wraps backwards');
    assert.equal(kb.nextTabIndex('ArrowRight', 0, 3, 'vertical'), null);
    assert.equal(kb.nextTabIndex('ArrowLeft', 0, 3, 'vertical'), null);
    // Horizontal (and the ARIA default when aria-orientation is absent).
    assert.equal(kb.nextTabIndex('ArrowRight', 0, 3, null), 1);
    assert.equal(kb.nextTabIndex('ArrowLeft', 0, 3, 'horizontal'), 2, 'wraps backwards');
    assert.equal(kb.nextTabIndex('ArrowDown', 0, 3, null), null);
    assert.equal(kb.nextTabIndex('ArrowUp', 0, 3, 'horizontal'), null);
    // Home/End are orientation-independent.
    assert.equal(kb.nextTabIndex('Home', 2, 3, 'vertical'), 0);
    assert.equal(kb.nextTabIndex('End', 0, 3, 'horizontal'), 2);
    dom.window.close();
});

// ------------------------------------------------ dialogs (#232 increment 4) --
//
// The popups adopt the official p-modal classes; their behaviour must not
// regress. The native <dialog> traps focus and returns it to the invoker on
// close in browsers — jsdom needs the explicit focus-return wiring, so these
// drive the real index.html + app.js (wireSetupPanels wires the dialog handlers,
// initDialog wires the shared confirm/prompt dialog).

function dialogSetup() {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
    const stub = `
api.get = async () => ({});
api.post = async () => ({});
initDialogFocusReturn(); // #232 inc 4: focus returns to the opener
initDialog();            // shared confirm/prompt/alert dialog
wireWeekGrid();          // customer/dialog open + cancel handlers (#189)
window.d = { showDialog };`;
    dom.window.eval(`${app}\n${stub}`);
    return { dom, d: dom.window.d, document: dom.window.document };
}

// The always-registered DOMContentLoaded handler calls showAuth() on a fresh
// harness (no session), which focuses the sign-in field. Let it settle before
// asserting anything about focus.
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

const dialogOpen = (dlg) => dlg.open === true || dlg.hasAttribute('open');
const escape = (dom, dlg) => dlg.dispatchEvent(
    new dom.window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }),
);

test('confirm dialog focuses in on open, ESC closes and returns focus (#232 inc 4)', async () => {
    const { dom, d, document } = dialogSetup();
    await settle();
    const invoker = document.getElementById('day-add');
    invoker.focus();
    const pending = d.showDialog({ title: 'Please confirm', message: 'Delete this?', okLabel: 'Delete', cancelLabel: 'Cancel' });
    const dlg = document.getElementById('app-dialog');
    assert.ok(dialogOpen(dlg), 'the shared dialog opens');
    assert.equal(document.activeElement.id, 'dlg-ok', 'focus moves into the dialog on open');
    assert.equal(document.getElementById('dlg-title').textContent, 'Please confirm', 'accessible name resolves from the title');
    escape(dom, dlg);
    assert.equal(await pending, 'cancel', 'Escape resolves as cancel');
    assert.equal(dialogOpen(dlg), false, 'ESC closes the dialog');
    assert.equal(document.activeElement.id, 'day-add', 'focus returns to the invoking control');
    dom.window.close();
});

test('customer dialog focuses in, ESC closes, focus and name resolve (#232 inc 4)', async () => {
    const { dom, document } = dialogSetup();
    await settle();
    document.getElementById('panel-customers').hidden = false; // the invoker must be focusable
    const invoker = document.getElementById('customer-new');
    invoker.focus();
    invoker.dispatchEvent(new dom.window.Event('click', { bubbles: true }));
    const dlg = document.getElementById('customer-dialog');
    assert.ok(dlg.classList.contains('p-modal'), 'dialog carries the p-modal overlay');
    assert.ok(dialogOpen(dlg), 'the customer dialog opens');
    assert.equal(document.activeElement.id, 'customer-name', 'focus moves into the dialog on open');
    assert.equal(
        document.getElementById(dlg.getAttribute('aria-labelledby')).textContent,
        'Add customer',
        'the dialog is named by its p-modal__title',
    );
    escape(dom, dlg);
    assert.equal(dialogOpen(dlg), false, 'ESC closes the dialog');
    assert.equal(document.activeElement.id, 'customer-new', 'focus returns to the invoking control');
    dom.window.close();
});
