// Settings restructure: seven focused sections, the Email (SMTP) section and
// the app-wide heading/card pattern. These tests drive the real index.html +
// app.js in jsdom and pin:
//   * the tab strip order, each route's panel, and member visibility;
//   * Company saving through PUT /admin/org;
//   * the Invoice template saving through PUT /admin/invoice-template, with
//     its server-rendered %variable% cheat-sheet;
//   * the Email section: GET prefill (never the password), PUT of changed
//     values only, the vault-disabled path, delivery status and test-send
//     success/422/502 with single-flight;
//   * the recipient dialog's Settings link route;
//   * the a11y contract (headings/cards, labelled fields, live regions,
//     horizontal keyboard navigation).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

const EMAIL = {
    vault_enabled: true,
    configured: true,
    transport: 'smtp',
    host: 'smtp.acme.test',
    port: 587,
    username: 'mailer',
    from: 'billing@acme.test',
    has_password: true,
};

const ORG = {
    name: 'Acme',
    legal_id: 'BE0123456789',
    address: { street: 'Harbour 9', city: 'Antwerp', postal_code: '2000', country: 'BE' },
    from_name: 'Acme Billing',
    reply_to: 'billing@acme.test',
    accent: '#e95420',
};

const TEMPLATE = {
    template: {
        subject: 'Invoice %invoice_number%',
        body: 'Dear %customer_name%',
        footer: 'Bank: BE00',
        payment_terms: { kind: 'net_30' },
        labels: {
            description: 'Work',
            quantity: 'Hours',
            unit_price: 'Rate',
            subtotal: 'Subtotal',
            discount: 'Discount',
            tax: 'VAT',
            total: 'Due',
        },
    },
    variables: [
        { name: '%customer_name%', example: 'Acme' },
        { name: '%invoice_number%', example: 'INV-0001' },
        { name: '%invoice_issue_month%', example: 'March' },
    ],
};

function setup({
    email = EMAIL,
    putError = null,
    postResult = { sent_to: 'qa@acme.test', transport: 'smtp' },
    postError = null,
} = {}) {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/settings/users' });
    const { window } = dom;
    window.HTMLDialogElement.prototype.showModal = function showModal() {
        this.open = true;
        this.setAttribute('open', '');
    };
    window.HTMLDialogElement.prototype.close = function close() {
        this.open = false;
        this.removeAttribute('open');
    };
    const prelude = `
      window.calls = { get: [], put: [], post: [], preview: [] };
      window.__email = ${JSON.stringify(email)};
      window.__org = ${JSON.stringify(ORG)};
      window.__template = ${JSON.stringify(TEMPLATE)};
      window.__putError = ${JSON.stringify(putError)};
      window.__postResult = ${JSON.stringify(postResult)};
      window.__postError = ${JSON.stringify(postError)};
      // The live preview's transport. Tests replace __previewHandler to hold a
      // render open, or to answer 422/304.
      window.__previewHandler = null;
      api.get = async (path) => {
        window.calls.get.push(path);
        if (path === '/auth/status') return { initialised: true };
        if (path === '/auth/sso/providers') return { providers: [] };
        if (path === '/auth/me') throw new Error('no session');
        if (path === '/admin/email') return window.__email;
        if (path === '/admin/org') return window.__org;
        if (path === '/admin/invoice-template') return window.__template;
        return {};
      };
      api.put = async (path, body) => {
        window.calls.put.push({ path, body });
        if (window.__putError) {
          const raw = window.__putError;
          const err = new Error(raw.message);
          err.status = raw.status;
          err.payload = { error: { code: raw.code || 'error', message: raw.message } };
          throw err;
        }
        if (path === '/admin/email') return { ...window.__email, ...body, configured: true };
        return body;
      };
      api.post = async (path, body) => {
        window.calls.post.push({ path, body });
        if (window.__postError) {
          const raw = window.__postError;
          const err = new Error(raw.message);
          err.status = raw.status;
          err.payload = { error: { code: raw.code || 'error', message: raw.message } };
          throw err;
        }
        return window.__postResult;
      };
      api.requestBlob = async (method, path, body, opts) => {
        window.calls.preview.push({ method, path, body, ifNoneMatch: (opts || {}).ifNoneMatch ?? null });
        if (window.__previewHandler) return window.__previewHandler(method, path, body, opts);
        return { notModified: false, etag: '"preview-etag-1"', blob: { type: 'application/pdf' }, status: 200 };
      };
      window.createdUrls = [];
      window.revokedUrls = [];
      // A monotonic sequence, so clearing the ledger after the opening render
      // does not renumber the next URL.
      window.urlSeq = 0;
      URL.createObjectURL = (blob) => {
        window.urlSeq += 1;
        const url = 'blob:preview-' + window.urlSeq;
        window.createdUrls.push(url);
        return url;
      };
      URL.revokeObjectURL = (url) => window.revokedUrls.push(url);
      initTabs();
      initSettingsTabs();
    `;
    window.eval(`${app}\n${prelude}\nwindow.t = {
      applyRoute, normaliseRoute, showAccount, initSettingsTabs,
      refreshOrgProfile, saveOrgProfile, refreshInvoiceTemplate, saveInvoiceTemplate,
      refreshEmailSettings, saveEmailSettings, sendTestEmail, EMAIL_SETTINGS_ROUTE,
      initTemplatePreview, renderTemplatePreview, scheduleTemplatePreview,
      releaseTemplatePreview, openTemplatePreviewInNewTab, templatePayload,
      TEMPLATE_PREVIEW_DEBOUNCE_MS,
      calls: () => window.calls,
    };`);
    return { dom, document: window.document, window, t: window.t };
}

const tabs = (document) => [...document.querySelectorAll('#settings-tabs [role="tab"]')];
const visibleTabs = (document) => tabs(document).filter((tab) => !tab.hidden).map((tab) => tab.id);
const selected = (document) => tabs(document).filter((tab) => tab.getAttribute('aria-selected') === 'true').map((tab) => tab.id);
const setValue = (document, id, value) => {
    document.getElementById(id).value = value;
};

const ROUTES = [
    ['/settings/users', 'settings-tab-users', 'settings-panel-users'],
    ['/settings/company', 'settings-tab-company', 'settings-panel-company'],
    ['/settings/invoice-template', 'settings-tab-invoice-template', 'settings-panel-invoice-template'],
    ['/settings/catalog', 'settings-tab-catalog', 'settings-panel-catalog'],
    ['/settings/expenses', 'settings-tab-expenses', 'settings-panel-expenses'],
    ['/settings/email', 'settings-tab-email', 'settings-panel-email'],
    ['/settings/security', 'settings-tab-security', 'settings-panel-security'],
];

// ------------------------------------------------------------- tab order ----

test('Settings lists the seven focused tabs in order for an admin', () => {
    const { dom, document } = setup();
    assert.deepEqual(tabs(document).map((tab) => tab.id), ROUTES.map(([, tab]) => tab));
    assert.deepEqual(
        tabs(document).map((tab) => tab.textContent.trim()),
        ['Users', 'Company', 'Invoice template', 'Products & services', 'Expense categories', 'Email', 'Security & runtime'],
    );
    for (const [tab, panelId] of ROUTES.map(([, tab, panel]) => [tab, panel])) {
        const panel = document.getElementById(panelId);
        assert.ok(panel, `${panelId} exists`);
        assert.equal(panel.getAttribute('role'), 'tabpanel');
        assert.equal(panel.getAttribute('aria-labelledby'), tab);
    }
    dom.window.close();
});

test('each Settings route activates its own panel', () => {
    const { dom, document, t } = setup();
    for (const [route, tabId, panelId] of ROUTES) {
        assert.equal(t.normaliseRoute(route), route, `${route} is a known route`);
        assert.equal(t.applyRoute(route), true, `${route} applies`);
        assert.deepEqual(selected(document), [tabId], `${route} selects ${tabId}`);
        assert.equal(document.getElementById(panelId).hidden, false, `${route} shows ${panelId}`);
        for (const [, , other] of ROUTES) {
            if (other !== panelId) assert.equal(document.getElementById(other).hidden, true, `${other} stays hidden`);
        }
    }
    // /settings/invoices is gone: bookmarks fall back like any unknown path.
    assert.equal(t.normaliseRoute('/settings/invoices'), null);
    dom.window.close();
});

test('a member sees only Expense categories', () => {
    const { dom, document, window, t } = setup();
    t.showAccount({ role: 'member', name: 'Mem' });
    t.initSettingsTabs();
    assert.deepEqual(visibleTabs(document), ['settings-tab-expenses']);
    assert.deepEqual(selected(document), ['settings-tab-expenses']);
    assert.equal(document.getElementById('settings-panel-expenses').hidden, false);
    for (const [route, , panelId] of ROUTES) {
        if (route === '/settings/expenses') continue;
        assert.equal(document.getElementById(panelId).hidden, true, `${panelId} is closed for a member`);
    }
    // A member deep link into an admin section is refused, not rendered.
    assert.equal(t.applyRoute('/settings/email'), true, 'the Settings section still opens');
    assert.deepEqual(selected(document), ['settings-tab-expenses'], 'the URL cannot override the tier');
    window.close?.();
    dom.window.close();
});

// --------------------------------------------------------------- company ----

test('Company loads its identity and saves via PUT /admin/org', async () => {
    const { dom, document, t } = setup();
    await t.refreshOrgProfile();
    assert.equal(document.getElementById('org-name').value, 'Acme');
    assert.equal(document.getElementById('org-city').value, 'Antwerp');
    assert.equal(document.getElementById('org-accent').value, '#e95420');
    assert.equal(document.getElementById('accent-swatch').style.background !== '', true, 'the swatch paints');

    setValue(document, 'org-name', 'Acme Invoice Co');
    await t.saveOrgProfile({ preventDefault() { } });
    const call = t.calls().put.find((entry) => entry.path === '/admin/org');
    assert.deepEqual(JSON.parse(JSON.stringify(call.body)), {
        name: 'Acme Invoice Co',
        legal_id: 'BE0123456789',
        from_name: 'Acme Billing',
        reply_to: 'billing@acme.test',
        accent: '#e95420',
        address: { street: 'Harbour 9', city: 'Antwerp', postal_code: '2000', country: 'BE' },
    });
    dom.window.close();
});

// ------------------------------------------------------- invoice template ----

test('Invoice template loads the cheat-sheet and saves labels with the template', async () => {
    const { dom, document, t } = setup();
    await t.refreshInvoiceTemplate();
    assert.equal(document.getElementById('template-subject').value, 'Invoice %invoice_number%');
    assert.equal(document.getElementById('lb-total').value, 'Due');
    const rows = [...document.querySelectorAll('#template-vars tbody tr')];
    assert.equal(rows.length, 3, 'the %variable% cheat-sheet comes from the server');
    assert.match(rows.map((row) => row.textContent).join(' '), /%invoice_issue_month%/);
    assert.equal(t.calls().preview.length, 0, 'a hidden section never renders a preview');

    setValue(document, 'lb-total', 'Amount due');
    await t.saveInvoiceTemplate({ preventDefault() { } });
    const call = t.calls().put.find((entry) => entry.path === '/admin/invoice-template');
    assert.equal(call.body.labels.total, 'Amount due');
    assert.equal(call.body.labels.description, 'Work');
    assert.equal(call.body.payment_terms.kind, 'net_30');
    dom.window.close();
});

// -------------------------------------------------- live template preview ---
//
// The preview card replaces the retired "newest invoice" document: it renders
// the *candidate* template against the API's sample invoice, so it works with no
// invoice in the store and reflects unsaved edits.

/// Open the section with the stored template loaded, and wait for the render the
/// visibility change starts. The ledgers are cleared after it, so each test only
/// sees the requests and URLs it caused.
async function openPreview(t, window, document) {
    await t.refreshInvoiceTemplate();
    assert.equal(t.calls().preview.length, 0, 'a hidden section renders nothing');
    t.initTemplatePreview();
    document.getElementById('settings-panel-invoice-template').hidden = false;
    await waitFor(() => t.calls().preview.length === 1, {
        message: 'the visible section to render its first preview',
    });
    await waitFor(() => document.getElementById('template-preview-frame').getAttribute('src') !== null, {
        message: 'the first preview to be framed',
    });
    t.calls().preview.length = 0;
    window.createdUrls.length = 0;
    window.revokedUrls.length = 0;
}

test('typing in the body and labels debounces one preview POST with the form payload', async () => {
    const { dom, document, window, t } = setup();
    await openPreview(t, window, document);
    const body = document.getElementById('template-body');
    const total = document.getElementById('lb-total');
    body.value = 'Dear %customer_name%, please pay.';
    body.dispatchEvent(new window.Event('input', { bubbles: true }));
    total.value = 'Amount due';
    total.dispatchEvent(new window.Event('input', { bubbles: true }));
    total.dispatchEvent(new window.Event('change', { bubbles: true }));
    await waitFor(() => t.calls().preview.length === 1, { message: 'the debounced render' });
    await waitFor(() => document.getElementById('template-preview-status').textContent === 'Preview updated', {
        message: 'the debounced render to finish',
    });
    // One event-loop turn (not a fixed wait): any timer the burst had queued
    // would run now, so a trailing request would be counted below.
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(t.calls().preview.length, 1, 'one render for the whole burst');
    const call = t.calls().preview[0];
    assert.equal(call.method, 'POST');
    assert.equal(call.path, '/admin/invoice-template/preview');
    assert.deepEqual(JSON.parse(JSON.stringify(call.body)), {
        subject: 'Invoice %invoice_number%',
        body: 'Dear %customer_name%, please pay.',
        footer: 'Bank: BE00',
        payment_terms: { kind: 'net_30' },
        labels: {
            description: 'Work',
            quantity: 'Hours',
            unit_price: 'Rate',
            subtotal: 'Subtotal',
            discount: 'Discount',
            tax: 'VAT',
            total: 'Amount due',
        },
    }, 'the preview sends exactly what Save would');
    assert.equal(call.ifNoneMatch, '"preview-etag-1"', 'the conditional request carries the last ETag');
    dom.window.close();
});

test('each render mints one object URL, revokes the one it replaced and releases on teardown', async () => {
    const { dom, document, window, t } = setup();
    await openPreview(t, window, document);
    const frame = document.getElementById('template-preview-frame');
    const first = frame.getAttribute('src');
    document.getElementById('template-body').value = 'Second render';
    document.getElementById('template-body').dispatchEvent(new window.Event('input', { bubbles: true }));
    await waitFor(() => window.createdUrls.length === 1, { message: 'the debounced render to mint a URL' });
    assert.deepEqual([...window.createdUrls], ['blob:preview-2'], 'one URL per render');
    assert.deepEqual([...window.revokedUrls], ['blob:preview-1'], 'the replaced URL is revoked, not pinned');
    assert.equal(frame.getAttribute('src'), 'blob:preview-2');

    // A second render revokes exactly the URL it replaces.
    document.getElementById('template-body').value = 'Third render';
    await t.renderTemplatePreview();
    assert.deepEqual([...window.createdUrls], ['blob:preview-2', 'blob:preview-3']);
    assert.deepEqual([...window.revokedUrls], ['blob:preview-1', 'blob:preview-2']);
    assert.notEqual(frame.getAttribute('src'), first);

    // Hiding the section is a teardown: the frame and its URL are released.
    document.getElementById('settings-panel-invoice-template').hidden = true;
    await waitFor(() => window.revokedUrls.includes('blob:preview-3'), {
        message: 'hiding the section to revoke the framed URL',
    });
    assert.equal(frame.hasAttribute('src'), false, 'no URL survives the hidden section');
    dom.window.close();
});

test('Open in new tab opens the blob URL currently framed', async () => {
    const { dom, document, window, t } = setup();
    await openPreview(t, window, document);
    const frame = document.getElementById('template-preview-frame');
    const current = frame.getAttribute('src');
    const opened = [];
    window.open = (url, target) => {
        opened.push({ url, target });
        return { opener: null };
    };
    document.getElementById('template-preview-open').click();
    assert.deepEqual(opened, [{ url: current, target: '_blank' }], 'the tab shows what the preview shows');

    t.releaseTemplatePreview();
    document.getElementById('template-preview-open').click();
    assert.match(document.getElementById('template-preview-status').textContent, /not rendered yet/i,
        'with nothing framed, the control explains instead of opening a dead tab');
    dom.window.close();
});

test('a 422 keeps the last good preview, shows the field errors and retries on the next edit', async () => {
    const { dom, document, window, t } = setup();
    await openPreview(t, window, document);
    const frame = document.getElementById('template-preview-frame');
    const good = frame.getAttribute('src');
    window.__previewHandler = async () => {
        const err = new Error('payload failed validation');
        err.status = 422;
        err.payload = {
            error: {
                code: 'validation_failed',
                fields: [{ field: 'body', message: 'unknown variable %bogus%' }],
            },
        };
        throw err;
    };
    document.getElementById('template-body').value = 'pay %bogus%';
    document.getElementById('template-body').dispatchEvent(new window.Event('input', { bubbles: true }));
    await waitFor(() => t.calls().preview.length === 1, { message: 'the invalid render' });
    await waitFor(() => document.getElementById('template-error').hidden === false, {
        message: 'the field error to render inline',
    });
    assert.equal(frame.getAttribute('src'), good, 'the last good document stays framed');
    assert.match(document.getElementById('template-error').textContent, /body: unknown variable %bogus%/);
    assert.match(document.getElementById('template-preview-status').textContent, /fix the highlighted fields/i);
    assert.equal(document.getElementById('template-body').value, 'pay %bogus%', 'the edit is never lost');

    window.__previewHandler = null;
    document.getElementById('template-body').value = 'pay %customer_name%';
    document.getElementById('template-body').dispatchEvent(new window.Event('input', { bubbles: true }));
    await waitFor(() => t.calls().preview.length === 2, { message: 'the retry render' });
    await waitFor(() => document.getElementById('template-preview-status').textContent === 'Preview updated', {
        message: 'the retry to succeed',
    });
    assert.equal(t.calls().preview[1].body.body, 'pay %customer_name%', 'the retry carries the newest edit');
    assert.equal(document.getElementById('template-error').hidden, true, 'success clears the field error');
    dom.window.close();
});

test('a 304 keeps the framed document and only updates the status', async () => {
    const { dom, document, window, t } = setup();
    await openPreview(t, window, document);
    const frame = document.getElementById('template-preview-frame');
    const good = frame.getAttribute('src');
    window.__previewHandler = async (method, path, body, opts) => ({
        notModified: true,
        etag: (opts || {}).ifNoneMatch,
        blob: null,
        status: 304,
    });
    document.getElementById('lb-total').value = 'Due now';
    await t.renderTemplatePreview();
    assert.equal(t.calls().preview.length, 1);
    assert.equal(t.calls().preview[0].ifNoneMatch, '"preview-etag-1"');
    assert.equal(frame.getAttribute('src'), good, 'a 304 never blanks the frame');
    assert.deepEqual([...window.createdUrls], [], 'a 304 mints no URL');
    assert.deepEqual([...window.revokedUrls], [], 'a 304 revokes nothing');
    assert.equal(document.getElementById('template-preview-status').textContent, 'Preview updated');
    dom.window.close();
});

test('edits during an in-flight render coalesce into one follow-up with the newest state', async () => {
    const { dom, document, window, t } = setup();
    await openPreview(t, window, document);
    let release;
    let renders = 0;
    window.__previewHandler = async () => {
        renders += 1;
        if (renders === 1) await new Promise((resolve) => { release = resolve; });
        return { notModified: false, etag: `"etag-${renders}"`, blob: { type: 'application/pdf' }, status: 200 };
    };
    document.getElementById('template-body').value = 'first edit';
    const started = t.renderTemplatePreview();
    // Two more edits while the first response is held open.
    document.getElementById('template-body').value = 'second edit';
    t.renderTemplatePreview();
    document.getElementById('template-body').value = 'newest edit';
    const joined = t.renderTemplatePreview();
    assert.equal(renders, 1, 'only one request is in flight');
    release();
    await started;
    await joined;
    assert.equal(renders, 2, 'exactly one follow-up render, never a storm');
    assert.equal(t.calls().preview.length, 2);
    assert.equal(t.calls().preview[1].body.body, 'newest edit', 'the follow-up carries the newest state');
    assert.deepEqual([...window.createdUrls], ['blob:preview-2', 'blob:preview-3'],
        'the two applied renders each minted one URL');
    assert.deepEqual([...window.revokedUrls], ['blob:preview-1', 'blob:preview-2'],
        'each applied render revoked exactly the URL it replaced');
    dom.window.close();
});

test('the preview card is a titled, labelled, politely announced region', () => {
    const { dom, document } = setup();
    const card = document.getElementById('template-preview');
    assert.ok(card, 'the card always renders: it no longer depends on an invoice existing');
    assert.equal(card.hidden, false);
    assert.equal(document.getElementById('template-preview-subject'), null, 'the retired text preview is gone');
    const frame = document.getElementById('template-preview-frame');
    assert.ok(frame.getAttribute('title').length > 20, 'the frame has a descriptive title');
    const status = document.getElementById('template-preview-status');
    assert.equal(status.getAttribute('role'), 'status', 'the status line is a status region');
    assert.equal(status.getAttribute('aria-live'), 'polite');
    for (const id of ['template-preview-refresh', 'template-preview-open']) {
        const button = document.getElementById(id);
        assert.equal(button.tagName, 'BUTTON', `${id} is a real button`);
        assert.equal(button.getAttribute('type'), 'button');
        assert.ok(button.textContent.trim(), `${id} has a visible label`);
    }
    assert.equal(document.querySelectorAll('#template-preview input, #template-preview select').length, 0,
        'no click-only controls live in the preview card');
    dom.window.close();
});

// ----------------------------------------------------------------- email ----

test('Email prefill never puts the password in the DOM and shows the delivery status', async () => {
    const { dom, document, t } = setup();
    await t.refreshOrgProfile();
    await t.refreshEmailSettings();
    assert.equal(document.getElementById('email-host').value, 'smtp.acme.test');
    assert.equal(document.getElementById('email-port').value, '587');
    assert.equal(document.getElementById('email-username').value, 'mailer');
    assert.equal(document.getElementById('email-from').value, 'billing@acme.test');
    assert.equal(document.getElementById('email-password').value, '', 'the password is never rendered');
    assert.match(document.getElementById('email-password').placeholder, /stored — leave blank to keep/);
    assert.equal(document.getElementById('email-status').textContent, 'Email delivery: SMTP smtp.acme.test:587');
    assert.equal(document.getElementById('email-test-to').value, 'billing@acme.test',
        'the test recipient defaults to the org reply-to');
    assert.equal(document.getElementById('email-form').querySelectorAll('input[disabled]').length, 0,
        'the form is editable when the vault is enabled');
    dom.window.close();
});

test('saving Email sends only changed values and omits a blank password', async () => {
    const { dom, document, t } = setup();
    await t.refreshEmailSettings();
    setValue(document, 'email-host', 'smtp.new.test');
    await t.saveEmailSettings({ preventDefault() { } });
    const call = t.calls().put.find((entry) => entry.path === '/admin/email');
    assert.deepEqual(JSON.parse(JSON.stringify(call.body)), { host: 'smtp.new.test', port: 587 },
        'username/from were unchanged and the blank password is omitted');
    dom.window.close();
});

test('saving Email carries changed credentials and a typed password', async () => {
    const { dom, document, t } = setup();
    await t.refreshEmailSettings();
    setValue(document, 'email-username', 'other');
    setValue(document, 'email-from', 'other@acme.test');
    setValue(document, 'email-password', 's3cret');
    await t.saveEmailSettings({ preventDefault() { } });
    const call = t.calls().put.find((entry) => entry.path === '/admin/email');
    assert.deepEqual(JSON.parse(JSON.stringify(call.body)), {
        host: 'smtp.acme.test',
        port: 587,
        username: 'other',
        from: 'other@acme.test',
        password: 's3cret',
    });
    dom.window.close();
});

test('a disabled vault disables the Email form with an actionable notice', async () => {
    const { dom, document, t } = setup({
        email: { ...EMAIL, vault_enabled: false, configured: false, host: '', username: '', from: '', has_password: false },
    });
    await t.refreshEmailSettings();
    const notice = document.getElementById('email-vault-notice');
    assert.equal(notice.hidden, false, 'the notice is visible');
    assert.match(notice.textContent, /TUCANO_SECRET_KEY/);
    assert.equal(document.getElementById('email-status').textContent,
        'Email delivery: NOT configured — messages are only logged.');
    const controls = [...document.querySelectorAll('#email-form input, #email-form button, #email-test-form input, #email-test-form button')];
    assert.ok(controls.length > 0 && controls.every((control) => control.disabled), 'every control is disabled');
    dom.window.close();
});

test('a 503 vault_disabled on save disables the form and explains it', async () => {
    const { dom, document, t } = setup({
        putError: { status: 503, code: 'vault_disabled', message: 'credential vault is not enabled' },
    });
    await t.refreshEmailSettings();
    setValue(document, 'email-host', 'smtp.other.test');
    await t.saveEmailSettings({ preventDefault() { } });
    const notice = document.getElementById('email-vault-notice');
    assert.equal(notice.hidden, false);
    assert.match(notice.textContent, /TUCANO_SECRET_KEY/);
    assert.equal(document.getElementById('email-error').hidden, false, 'the server error is shown inline');
    assert.equal(document.getElementById('email-error').dataset.errorCode, 'vault_disabled');
    assert.equal(document.getElementById('email-host').disabled, true, 'the form is disabled, not left failing');
    dom.window.close();
});

test('a test send posts once and announces the recipient', async () => {
    const { dom, document, window, t } = setup();
    await t.refreshEmailSettings();
    setValue(document, 'email-test-to', 'qa@acme.test');
    const first = t.sendTestEmail({ preventDefault() { } });
    const second = t.sendTestEmail({ preventDefault() { } });
    await Promise.all([first, second]);
    assert.equal(t.calls().post.length, 1, 'single-flight: a double click sends once');
    assert.deepEqual(JSON.parse(JSON.stringify(t.calls().post[0])), { path: '/admin/email/test', body: { to: 'qa@acme.test' } });
    assert.match(document.getElementById('email-test-result').textContent, /qa@acme\.test/);
    assert.match(window.document.getElementById('live-region').textContent, /qa@acme\.test/);
    dom.window.close();
});

test('422 email_not_configured explains the missing transport in the notice', async () => {
    const { dom, document, t } = setup({
        postError: { status: 422, code: 'email_not_configured', message: 'email delivery is not configured' },
    });
    await t.refreshEmailSettings();
    setValue(document, 'email-test-to', 'qa@acme.test');
    await t.sendTestEmail({ preventDefault() { } });
    const error = document.getElementById('email-test-error');
    assert.equal(error.hidden, false);
    assert.match(error.textContent, /not configured/i);
    assert.match(error.textContent, /SMTP host/i);
    assert.equal(error.dataset.errorCode, 'email_not_configured');
    dom.window.close();
});

test('502 email_failed shows the relay failure', async () => {
    const { dom, document, window, t } = setup({
        postError: { status: 502, code: 'email_failed', message: 'relay refused the message' },
    });
    await t.refreshEmailSettings();
    setValue(document, 'email-test-to', 'qa@acme.test');
    await t.sendTestEmail({ preventDefault() { } });
    const error = document.getElementById('email-test-error');
    assert.equal(error.hidden, false);
    assert.match(error.textContent, /relay refused the message/);
    assert.equal(error.dataset.errorCode, 'email_failed');
    assert.match(window.document.getElementById('live-region').textContent, /relay refused/);
    dom.window.close();
});

// ------------------------------------------------------ recipient dialog ----

test('the recipient dialog Settings link names the Email section', () => {
    const { dom, t } = setup();
    assert.equal(t.EMAIL_SETTINGS_ROUTE, '/settings/email');
    dom.window.close();
});

// --------------------------------------------------------------- a11y ------

test('every Settings panel uses the heading slot and a headed card', () => {
    const { dom, document } = setup();
    for (const [, , panelId] of ROUTES) {
        const panel = document.getElementById(panelId);
        const head = panel.querySelector(':scope > .ts-head');
        assert.ok(head, `${panelId} leads with .ts-head`);
        assert.ok(head.querySelector('h2'), `${panelId} has a section title in its heading slot`);
        const card = panel.querySelector(':scope > .p-card');
        assert.ok(card, `${panelId} continues with a p-card`);
        assert.ok(card.querySelector(':scope > .p-card__header h2'), `${panelId}'s card is headed`);
        assert.ok(card.querySelector(':scope > .p-card__content'), `${panelId}'s card has content`);
    }
    dom.window.close();
});

test('every Email control is labelled and the live regions are declared', () => {
    const { dom, document } = setup();
    for (const control of document.querySelectorAll('#email-form input, #email-test-form input')) {
        assert.equal(control.labels.length, 1, `${control.id} has exactly one label`);
        assert.ok(control.labels[0].textContent.trim(), `${control.id} has an accessible name`);
        assert.ok(control.labels[0].classList.contains('p-form__label'), `${control.id}'s label is official`);
        assert.ok(control.classList.contains('p-form__control'), `${control.id} is an official control`);
    }
    for (const id of ['email-status', 'email-test-result']) {
        const region = document.getElementById(id);
        assert.equal(region.getAttribute('role'), 'status', `${id} is a status region`);
        assert.equal(region.getAttribute('aria-live'), 'polite', `${id} announces politely`);
    }
    for (const id of ['email-vault-notice', 'email-error', 'email-test-error']) {
        assert.equal(document.getElementById(id).getAttribute('role'), 'alert', `${id} is an alert`);
    }
    dom.window.close();
});

test('the Settings tab strip navigates with the horizontal key contract', () => {
    const { dom, document } = setup();
    document.getElementById('panel-settings').hidden = false;
    const press = (key) => document.activeElement.dispatchEvent(
        new dom.window.KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }),
    );
    document.getElementById('settings-tab-users').focus();
    press('ArrowRight');
    assert.equal(document.activeElement.id, 'settings-tab-company', 'ArrowRight → next section');
    press('End');
    assert.equal(document.activeElement.id, 'settings-tab-security', 'End → last section');
    press('Home');
    assert.equal(document.activeElement.id, 'settings-tab-users', 'Home → first section');
    press('ArrowLeft');
    assert.equal(document.activeElement.id, 'settings-tab-security', 'ArrowLeft wraps to the last');
    press('ArrowDown');
    assert.equal(document.activeElement.id, 'settings-tab-security', 'ArrowDown is inert on a horizontal tablist');
    dom.window.close();
});
