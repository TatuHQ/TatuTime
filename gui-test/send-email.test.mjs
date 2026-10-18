// Invoice email recipients (#139 follow-up): both delivery entry points open a
// recipient dialog instead of sending immediately. These tests drive the real
// index.html + app.js in jsdom and pin:
//   * `#ip-send` opens the dialog and sends nothing;
//   * the flagged billing contact is first and checked, other on-file
//     addresses are listed unchecked;
//   * every ticked address is sent in one ordered request (and the untouched
//     default omits the body entirely);
//   * typed extras merge and de-duplicate, invalid ones block the request;
//   * a disabled SMTP transport is explained, Send is disabled and the Settings
//     route comes from the single module constant;
//   * 409/422 render inline without closing the dialog;
//   * the dialog's a11y contract (labels, alert, live region, focus, Escape).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

const CUSTOMER = {
    id: 'c1',
    name: 'ACME',
    email: 'legacy@acme.test',
    contacts: [
        { name: 'Ada', role: 'Accounts', email: 'ada@acme.test', billing: true },
        { name: 'Bob', role: 'Operations', email: 'bob@acme.test', billing: false },
    ],
};
const INVOICE = { id: 'i1', number: 'INV-0001', customer_id: 'c1' };

function setup({ adminEmail = { configured: true, transport: 'smtp' }, postResult, postError = null } = {}) {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    const { window } = dom;
    window.HTMLDialogElement.prototype.showModal = function showModal() {
        this.open = true;
        this.setAttribute('open', '');
    };
    window.HTMLDialogElement.prototype.close = function close() {
        this.open = false;
        this.removeAttribute('open');
    };
    if (!window.Element.prototype.scrollIntoView) window.Element.prototype.scrollIntoView = () => { };
    const prelude = `
      window.calls = [];
      window.__adminEmail = ${JSON.stringify(adminEmail)};
      window.__postError = ${JSON.stringify(postError)};
      window.__postResult = ${JSON.stringify(postResult || {
        sent_to: 'ada@acme.test',
        recipients: ['ada@acme.test'],
        transport: 'smtp',
    })};
      api.get = async (path) => {
        // The auth boot settles on the sign-in card (the keyboard.test.mjs
        // harness): no session, so startApp never runs and the dialog tests own
        // focus and the invoice panels.
        if (path === '/auth/status') return { initialised: true };
        if (path === '/auth/sso/providers') return { providers: [] };
        if (path === '/auth/me') throw new Error('no session');
        if (path === '/admin/email') {
          if (window.__adminEmail === 503) { const e = new Error('vault disabled'); e.status = 503; throw e; }
          return window.__adminEmail;
        }
        if (path === '/customers') return { customers: [${JSON.stringify(CUSTOMER)}] };
        return {};
      };
      api.post = async (path, body) => {
        window.calls.push({ path, body });
        if (window.__postError) {
          const raw = window.__postError;
          const err = new Error(raw.message);
          err.status = raw.status;
          err.payload = { error: { code: raw.code || 'error', message: raw.message } };
          throw err;
        }
        return window.__postResult;
      };
      window.__setPreview = (inv) => { invoicePreview = { id: inv.id, inv, opener: null }; };
      window.__setCustomer = () => { state.customers = [${JSON.stringify(CUSTOMER)}]; };
      wireWeekGrid();
    `;
    window.eval(`${app}\n${prelude}\nwindow.t = {
      state, openSendEmailDialog, closeSendEmailDialog, confirmSendEmail,
      emailCandidates, parseExtraEmails, sendInvoiceEmail, EMAIL_SETTINGS_ROUTE,
      setPreview: (inv) => window.__setPreview(inv),
      seed: () => window.__setCustomer(),
      calls: () => window.calls,
    };`);
    window.document.getElementById('panel-invoices').hidden = false;
    // The detail panel is open whenever `#ip-send` is reachable; jsdom refuses
    // to focus a control inside a hidden subtree, so mirror that state.
    window.document.getElementById('invoice-preview').hidden = false;
    window.t.seed();
    return { dom, document: window.document, window, t: window.t };
}

// The DOMContentLoaded auth boot focuses the sign-in card; wait for it so the
// dialog's own focus move is the last one and the assertion is deterministic.
const settleBoot = (document) => waitFor(() => document.activeElement.id === 'auth-email', {
    message: 'the sign-in handler to focus the email field',
});

const dlgOf = (document) => document.getElementById('send-email-dialog');
const isOpen = (dlg) => dlg.open === true || dlg.hasAttribute('open');
const boxes = (document) => [...document.querySelectorAll('#send-email-recipients input[type="checkbox"]')];
const checked = (document) => boxes(document).filter((b) => b.checked).map((b) => b.value);
const click = (window, node) => node.dispatchEvent(new window.Event('click', { bubbles: true }));

// ------------------------------------------------------------ opening ------

test('clicking #ip-send opens the recipient dialog and sends nothing', async () => {
    const { dom, document, window, t } = setup();
    await settleBoot(document);
    t.setPreview(INVOICE);
    const opener = document.getElementById('ip-send');
    opener.focus();
    click(window, opener);
    await waitFor(() => isOpen(dlgOf(document)), { message: 'the recipient dialog to open' });
    assert.equal(t.calls().length, 0, 'opening the dialog must not POST anything');
    assert.equal(t.state.isAdmin, false, 'the harness never loads the register');
    assert.deepEqual(checked(document), ['ada@acme.test'], 'the billing address is checked by default');
    assert.ok(dlgOf(document).contains(document.activeElement), 'focus moves into the dialog on open');
    // Escape closes and returns focus to the opener.
    dlgOf(document).dispatchEvent(new window.KeyboardEvent('keydown', {
        key: 'Escape', bubbles: true, cancelable: true,
    }));
    await waitFor(() => !isOpen(dlgOf(document)), { message: 'Escape to close the dialog' });
    assert.equal(document.activeElement.id, 'ip-send', 'focus returns to the opener');
    dom.window.close();
});

test('the billing contact leads the list and other on-file addresses follow unchecked', async () => {
    const { dom, document, t } = setup();
    await t.openSendEmailDialog(INVOICE, null);
    const list = boxes(document);
    assert.deepEqual(list.map((b) => b.value), ['ada@acme.test', 'bob@acme.test', 'legacy@acme.test'],
        'billing contact first, then the other contact, then the legacy address');
    assert.deepEqual(list.map((b) => b.checked), [true, false, false], 'only the billing address is pre-ticked');
    assert.deepEqual([...t.emailCandidates({ ...CUSTOMER, contacts: [] }).map((c) => c.email)],
        ['legacy@acme.test'], 'without contacts the legacy email is the default');
    assert.deepEqual([...t.emailCandidates({ id: 'c2', email: '', contacts: [] })], [],
        'no address on file yields no candidates');
    dom.window.close();
});

// ------------------------------------------------------------ sending ------

test('two ticked contacts send one request with both addresses in order', async () => {
    const { dom, document, window, t } = setup({
        postResult: {
            sent_to: 'ada@acme.test, bob@acme.test',
            recipients: ['ada@acme.test', 'bob@acme.test'],
            transport: 'smtp',
        },
    });
    await t.openSendEmailDialog(INVOICE, null);
    boxes(document)[1].click();
    click(window, document.getElementById('send-email-send'));
    await waitFor(() => t.calls().length === 1, { message: 'the send request' });
    assert.equal(t.calls().length, 1, 'exactly one request');
    const call = t.calls()[0];
    assert.equal(call.path, '/invoices/i1/email', 'the invoice email endpoint');
    assert.deepEqual([...call.body.recipients], ['ada@acme.test', 'bob@acme.test'],
        'both addresses travel in one ordered list');
    const result = document.getElementById('send-email-result');
    assert.equal(result.hidden, false, 'the result is announced inline');
    assert.match(result.textContent, /ada@acme\.test, bob@acme\.test/,
        'the success message names every recipient');
    dom.window.close();
});

test('the untouched single default omits the body entirely', async () => {
    const { dom, document, window, t } = setup();
    await t.openSendEmailDialog(INVOICE, null);
    click(window, document.getElementById('send-email-send'));
    await waitFor(() => t.calls().length === 1, { message: 'the default send' });
    assert.equal(t.calls()[0].body, undefined, 'the default selection keeps the historical no-body request');
    dom.window.close();
});

test('a typed extra address is merged and de-duplicated', async () => {
    const { dom, document, window, t } = setup();
    await t.openSendEmailDialog(INVOICE, null);
    document.getElementById('send-email-extra').value = 'ADA@acme.test, extra@client.test';
    click(window, document.getElementById('send-email-send'));
    await waitFor(() => t.calls().length === 1, { message: 'the merged send' });
    assert.deepEqual([...t.calls()[0].body.recipients], ['ada@acme.test', 'extra@client.test'],
        'the extra merges after the ticked address and the case-insensitive duplicate drops');
    dom.window.close();
});

test('an invalid typed address blocks the request with an inline error', async () => {
    const { dom, document, window, t } = setup();
    await t.openSendEmailDialog(INVOICE, null);
    document.getElementById('send-email-extra').value = 'not-an-email';
    click(window, document.getElementById('send-email-send'));
    await waitFor(() => document.getElementById('send-email-error').hidden === false,
        { message: 'the inline validation error' });
    assert.equal(t.calls().length, 0, 'nothing is sent for an invalid address');
    assert.match(document.getElementById('send-email-error').textContent, /not a valid email address/);
    assert.ok(isOpen(dlgOf(document)), 'the dialog stays open for correction');
    assert.deepEqual([...t.parseExtraEmails('a@b.test, c@d.test').emails], ['a@b.test', 'c@d.test']);
    assert.ok(t.parseExtraEmails('a@b@c.test').error, 'two @ characters are refused');
    assert.ok(t.parseExtraEmails('a b@c.test').error, 'whitespace is refused');
    dom.window.close();
});

// ------------------------------------------------- SMTP guidance / errors --

test('an unconfigured SMTP transport disables Send and points at the Settings route', async () => {
    const { dom, document, window, t } = setup({ adminEmail: { configured: false, transport: 'disabled' } });
    await t.openSendEmailDialog(INVOICE, null);
    await waitFor(() => document.getElementById('send-email-notice').hidden === false,
        { message: 'the SMTP guidance notice' });
    const notice = document.getElementById('send-email-notice');
    assert.equal(notice.getAttribute('role'), 'alert', 'the guidance is announced as an alert');
    assert.match(notice.textContent, /not\s+configured/i);
    assert.equal(document.getElementById('send-email-send').disabled, true, 'Send is disabled while unconfigured');
    assert.equal(document.getElementById('send-email-settings').dataset.route, t.EMAIL_SETTINGS_ROUTE,
        'Open Settings points at the single route constant');
    assert.equal(t.EMAIL_SETTINGS_ROUTE, '/settings/security', 'the constant still names the current Settings view');
    assert.equal(t.calls().length, 0);
    // A 503 vault_disabled answer is the same guidance, not a dialog crash.
    const second = setup({ adminEmail: 503 });
    await second.t.openSendEmailDialog(INVOICE, null);
    await waitFor(() => second.document.getElementById('send-email-notice').hidden === false,
        { message: 'the 503 guidance notice' });
    assert.equal(second.document.getElementById('send-email-send').disabled, true);
    second.dom.window.close();
    dom.window.close();
});

test('a 409 draft renders inline without closing the dialog', async () => {
    const { dom, document, window, t } = setup({
        postError: { status: 409, code: 'conflict', message: 'issue the invoice before emailing it' },
    });
    await t.openSendEmailDialog(INVOICE, null);
    click(window, document.getElementById('send-email-send'));
    await waitFor(() => document.getElementById('send-email-error').hidden === false,
        { message: 'the 409 error' });
    assert.match(document.getElementById('send-email-error').textContent, /issue the invoice before emailing it/);
    assert.ok(isOpen(dlgOf(document)), 'a 409 keeps the dialog open');
    assert.equal(document.getElementById('send-email-send').disabled, false, 'Send is usable again');
    dom.window.close();
});

test('a 422 validation error renders inline without closing the dialog', async () => {
    const { dom, document, window, t } = setup({
        postError: { status: 422, code: 'validation_error', message: 'customer has no billing email on file' },
    });
    await t.openSendEmailDialog(INVOICE, null);
    click(window, document.getElementById('send-email-send'));
    await waitFor(() => document.getElementById('send-email-error').hidden === false,
        { message: 'the 422 error' });
    assert.match(document.getElementById('send-email-error').textContent, /no billing email on file/);
    assert.ok(isOpen(dlgOf(document)), 'a 422 keeps the dialog open');
    dom.window.close();
});

// ---------------------------------------------------------------- a11y -----

test('the recipient dialog labels every control and moves focus in and out', async () => {
    const { dom, document, window, t } = setup();
    await settleBoot(document);
    const opener = document.getElementById('ip-send');
    opener.focus();
    await t.openSendEmailDialog(INVOICE, opener);
    const dlg = dlgOf(document);
    assert.equal(dlg.tagName, 'DIALOG', 'native dialog semantics');
    assert.equal(dlg.getAttribute('aria-modal'), 'true');
    assert.ok(dlg.classList.contains('p-modal'), 'the official p-modal overlay');
    const labelledby = document.getElementById('send-email-title');
    assert.equal(dlg.getAttribute('aria-labelledby'), 'send-email-title');
    assert.ok(dlg.contains(labelledby), 'the accessible name resolves inside the dialog');
    for (const box of boxes(document)) {
        assert.equal(box.labels.length, 1, `${box.id} has exactly one label`);
        assert.equal(box.labels[0].getAttribute('for'), box.id, `${box.id} is bound by for/id`);
    }
    const extra = document.getElementById('send-email-extra');
    assert.equal(extra.labels.length, 1, 'the extra-address input is labelled');
    assert.equal(document.getElementById('send-email-notice').getAttribute('role'), 'alert');
    const result = document.getElementById('send-email-result');
    assert.equal(result.getAttribute('role'), 'status');
    assert.equal(result.getAttribute('aria-live'), 'polite');
    assert.equal(document.activeElement, boxes(document)[0], 'focus lands on the first checkbox');
    assert.ok(dlg.contains(document.activeElement), 'focus is inside the dialog');
    t.closeSendEmailDialog();
    assert.equal(document.activeElement, opener, 'closing restores focus to the opener');
    dom.window.close();
});
