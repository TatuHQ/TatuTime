// The admin startup wizard (web/js/setup.js): first-run surface, step
// validation, the exact request shapes every step sends, the escapable
// no-brick fallback, and the dialog's a11y contract.
//
// Loads the real index.html + module graph in jsdom and lets the app's own
// DOMContentLoaded handler run, so the first-run path under test is the one a
// browser takes (initAuth -> openAdminWizard), never a re-implementation.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';
import { waitFor } from './wait-for.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();

const byId = (document, id) => document.getElementById(id);

// A first-run instance: /auth/status says no users exist, SSO has no providers,
// and every other call is recorded so a test can assert exactly what was sent.
// `window.__org` lets a test change the org the GET returns.
function prelude(extra = '') {
    return `
window.__calls = { get: [], post: [], put: [] };
window.__org = { name: 'Tucano Ltd' };
api.get = async (path) => {
  window.__calls.get.push(path);
  if (path === '/auth/status') return { initialised: false };
  if (path === '/auth/sso/providers') return { providers: [] };
  if (path === '/admin/config') return { config: [{ key: 'base_currency', value: 'GBP' }] };
  if (path === '/auth/me') return { id: 'u1', name: 'Ada', email: 'ada@example.test', role: 'admin' };
  if (path === '/admin/org') return window.__org;
  throw new Error('unexpected GET ' + path);
};
api.post = async (path, body) => {
  window.__calls.post.push({ path, body });
  return { id: 'u1', name: body.name, email: body.email, role: 'admin' };
};
api.put = async (path, body) => {
  window.__calls.put.push({ path, body });
  return path === '/admin/config' ? { ok: true } : body;
};
window.awTest = {
  aw, api, state, showAccount, initAdminWizard, openAdminWizard,
  closeAdminWizard, offerOrgSetup, awNext, awBack, awSkip,
};
${extra}`;
}

function makeDom(extra = '') {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
    if (!dom.window.Element.prototype.scrollIntoView) dom.window.Element.prototype.scrollIntoView = () => { };
    dom.window.eval(`${app}\n${prelude(extra)}`);
    return dom;
}

async function firstRun(extra = '') {
    const dom = makeDom(extra);
    await waitFor(
        () => byId(dom.window.document, 'admin-wizard').open === true,
        { message: 'the first-run admin wizard to open' },
    );
    return dom;
}

async function waitStep(document, step) {
    await waitFor(
        () => !byId(document, `aw-step-${step}`).hidden,
        { message: `step ${step} to be shown` },
    );
}

function setValue(document, id, value) {
    byId(document, id).value = value;
}

// Create the administrator through the real step-2 submit so later steps run
// against a `created` wizard, exactly as a first-run user reaches them.
async function createAdmin(document) {
    byId(document, 'aw-next').click();
    await waitStep(document, 2);
    setValue(document, 'aw-name', 'Ada Lovelace');
    setValue(document, 'aw-email', 'ada@example.test');
    setValue(document, 'aw-password', 'correct-horse');
    setValue(document, 'aw-confirm', 'correct-horse');
    byId(document, 'aw-next').click();
    await waitStep(document, 3);
}

test('first run opens the admin wizard at step 1, not the bare create-admin card', async () => {
    const dom = await firstRun();
    const { document } = dom.window;
    assert.equal(byId(document, 'admin-wizard').open, true, 'the wizard is the primary surface');
    assert.equal(!byId(document, 'aw-step-1').hidden, true, 'step 1 is shown');
    assert.equal(byId(document, 'aw-step-2').hidden, true, 'later steps stay hidden');
    assert.equal(byId(document, 'auth-overlay').hidden, true, 'the create-admin card is not the primary surface');
    assert.equal(dom.window.awTest.aw.step, 1);
    dom.window.close();
});

test('step 2 blocks mismatched and short passwords without a request', async () => {
    const dom = await firstRun();
    const { document } = dom.window;
    byId(document, 'aw-next').click();
    await waitStep(document, 2);
    setValue(document, 'aw-name', 'Ada');
    setValue(document, 'aw-email', 'ada@example.test');
    setValue(document, 'aw-password', 'short');
    setValue(document, 'aw-confirm', 'short');
    byId(document, 'aw-next').click();
    await waitFor(() => !byId(document, 'aw-error').hidden, { message: 'the short-password error' });
    assert.match(byId(document, 'aw-error').textContent, /at least 8 characters/);
    assert.equal(byId(document, 'aw-password').getAttribute('aria-invalid'), 'true');
    assert.equal(dom.window.__calls.post.length, 0, 'a short password never reaches the API');

    setValue(document, 'aw-password', 'correct-horse');
    setValue(document, 'aw-confirm', 'different-horse');
    byId(document, 'aw-next').click();
    await waitFor(
        () => /do not match/.test(byId(document, 'aw-error').textContent),
        { message: 'the mismatch error' },
    );
    assert.equal(byId(document, 'aw-confirm').getAttribute('aria-invalid'), 'true');
    assert.equal(dom.window.__calls.post.length, 0, 'mismatched passwords never reach the API');
    assert.equal(byId(document, 'aw-step-2').hidden, false, 'the user stays on step 2');
    dom.window.close();
});

test('step 2 sends exactly one BootstrapInput and advances on 201', async () => {
    const dom = await firstRun();
    const { document } = dom.window;
    await createAdmin(document);
    assert.equal(dom.window.__calls.post.length, 1, 'bootstrap is issued once');
    // JSON round-trip: the recorded body lives in the jsdom realm, so a strict
    // prototype comparison would fail on identical data.
    assert.deepEqual(JSON.parse(JSON.stringify(dom.window.__calls.post[0])), {
        path: '/auth/bootstrap',
        body: { name: 'Ada Lovelace', email: 'ada@example.test', password: 'correct-horse' },
    });
    assert.equal(byId(document, 'aw-step-3').hidden, false, 'a 201 advances to the organisation step');
    assert.match(byId(document, 'aw-status').textContent, /Administrator created/);
    dom.window.close();
});

test('a 403 initialised on bootstrap moves the user to the sign-in card', async () => {
    const dom = await firstRun(`
api.post = async (path, body) => {
  window.__calls.post.push({ path, body });
  const err = new Error('an administrator already exists');
  err.status = 403;
  err.payload = { error: { code: 'initialised', message: 'an administrator already exists' } };
  throw err;
};
`);
    const { document } = dom.window;
    byId(document, 'aw-next').click();
    await waitStep(document, 2);
    setValue(document, 'aw-name', 'Ada');
    setValue(document, 'aw-email', 'ada@example.test');
    setValue(document, 'aw-password', 'correct-horse');
    setValue(document, 'aw-confirm', 'correct-horse');
    byId(document, 'aw-next').click();
    await waitFor(
        () => byId(document, 'auth-overlay').hidden === false,
        { message: 'the sign-in card to replace the wizard' },
    );
    assert.equal(byId(document, 'admin-wizard').open, false, 'the wizard closes');
    assert.equal(byId(document, 'auth-title').textContent, 'Sign in');
    assert.equal(byId(document, 'auth-name-field').hidden, true, 'the sign-in card is not the setup card');
    dom.window.close();
});

test('step 3 sends only the accepted OrgProfile field names', async () => {
    const dom = await firstRun();
    const { document } = dom.window;
    await createAdmin(document);
    setValue(document, 'aw-org-name', 'Tucano Ltd');
    setValue(document, 'aw-org-legal', 'VAT-123');
    setValue(document, 'aw-org-street', '1 Main Street');
    setValue(document, 'aw-org-city', 'Springfield');
    setValue(document, 'aw-org-postal', '12345');
    setValue(document, 'aw-org-country', 'PT');
    setValue(document, 'aw-org-accent', '#ff8800');
    byId(document, 'aw-next').click();
    await waitStep(document, 4);
    assert.deepEqual(JSON.parse(JSON.stringify(dom.window.__calls.put)), [{
        path: '/admin/org',
        body: {
            name: 'Tucano Ltd',
            legal_id: 'VAT-123',
            address: { street: '1 Main Street', city: 'Springfield', postal_code: '12345', country: 'PT' },
            accent: '#ff8800',
        },
    }]);
    dom.window.close();
});

test('step 3 "Set this up later" advances without an org request', async () => {
    const dom = await firstRun();
    const { document } = dom.window;
    await createAdmin(document);
    byId(document, 'aw-skip').click();
    await waitStep(document, 4);
    assert.equal(dom.window.__calls.put.length, 0, 'skipping the organisation writes nothing');
    dom.window.close();
});

test('step 4 pre-fills from /admin/config and sends only base_currency', async () => {
    const dom = await firstRun();
    const { document } = dom.window;
    await createAdmin(document);
    byId(document, 'aw-skip').click();
    await waitStep(document, 4);
    await waitFor(
        () => byId(document, 'aw-currency').value === 'GBP',
        { message: 'the configured base currency to pre-fill step 4' },
    );
    setValue(document, 'aw-currency', 'eur');
    byId(document, 'aw-next').click();
    await waitStep(document, 5);
    assert.deepEqual(JSON.parse(JSON.stringify(dom.window.__calls.put)), [{
        path: '/admin/config',
        body: { base_currency: 'EUR' },
    }]);
    dom.window.close();
});

test('step 5 re-reads /auth/me and /admin/org and shows what persisted', async () => {
    const dom = await firstRun();
    const { document } = dom.window;
    await createAdmin(document);
    byId(document, 'aw-skip').click();
    await waitStep(document, 4);
    byId(document, 'aw-skip').click();
    await waitStep(document, 5);
    await waitFor(
        () => byId(document, 'aw-review-name').textContent === 'Ada',
        { message: 'the review to render the persisted administrator' },
    );
    assert.ok(dom.window.__calls.get.includes('/auth/me'), 'the review re-reads /auth/me');
    assert.ok(dom.window.__calls.get.includes('/admin/org'), 'the review re-reads /admin/org');
    assert.equal(byId(document, 'aw-review-email').textContent, 'ada@example.test');
    assert.equal(byId(document, 'aw-review-org').textContent, 'Tucano Ltd');
    dom.window.close();
});

test('Escape leaves the user able to create the first administrator', async () => {
    const dom = await firstRun();
    const { document } = dom.window;
    byId(document, 'admin-wizard').dispatchEvent(new dom.window.KeyboardEvent('keydown', {
        key: 'Escape', bubbles: true, cancelable: true,
    }));
    await waitFor(
        () => byId(document, 'admin-wizard').open === false,
        { message: 'the wizard to close on Escape' },
    );
    assert.equal(byId(document, 'auth-overlay').hidden, false, 'the create-admin card becomes visible');
    assert.equal(byId(document, 'auth-title').textContent, 'Create the first administrator');
    assert.equal(byId(document, 'auth-name-field').hidden, false, 'the name field is available');
    assert.equal(byId(document, 'auth-submit').textContent, 'Create admin');
    dom.window.close();
});

test('an admin with an empty organisation gets a dismissible prompt from step 3', async () => {
    const dom = await firstRun();
    const { document } = dom.window;
    // Close the first-run wizard, then behave as a signed-in admin whose
    // organisation name is still empty.
    dom.window.awTest.closeAdminWizard();
    dom.window.awTest.showAccount({ name: 'Ada', email: 'ada@example.test', role: 'admin' });
    dom.window.__org = { name: '' };
    await dom.window.awTest.offerOrgSetup();
    assert.equal(byId(document, 'aw-prompt').hidden, false, 'the dashboard prompt is offered');
    byId(document, 'aw-prompt-open').click();
    await waitFor(
        () => byId(document, 'admin-wizard').open === true,
        { message: 'the wizard to reopen from the prompt' },
    );
    assert.equal(byId(document, 'aw-step-3').hidden, false, 'the re-entry starts at the organisation step');
    assert.equal(document.activeElement.id, 'aw-org-name', 'focus lands on the first field');
    dom.window.close();
});

test('the wizard keeps its dialog a11y contract across every step', async () => {
    const dom = await firstRun();
    const { document } = dom.window;
    const dlg = byId(document, 'admin-wizard');
    assert.equal(dlg.getAttribute('role'), 'dialog');
    assert.equal(dlg.getAttribute('aria-modal'), 'true');
    assert.equal(dlg.getAttribute('aria-labelledby'), 'aw-title');
    assert.equal(byId(document, 'aw-status').getAttribute('role'), 'status');
    assert.equal(byId(document, 'aw-status').getAttribute('aria-live'), 'polite');

    for (const input of dlg.querySelectorAll('input')) {
        assert.ok(input.id, 'every wizard input has an id');
        assert.ok(
            dlg.querySelector(`label[for="${input.id}"]`),
            `${input.id} has a label bound by for/id`,
        );
    }

    // Step changes are announced and focus lands on the first control.
    assert.match(byId(document, 'aw-status').textContent, /Step 1 of 5: Welcome/);
    assert.equal(document.activeElement.id, 'aw-next');
    byId(document, 'aw-step-1').dispatchEvent(new dom.window.KeyboardEvent('keydown', {
        key: 'Enter', bubbles: true, cancelable: true,
    }));
    await waitStep(document, 2);
    assert.match(byId(document, 'aw-status').textContent, /Step 2 of 5: Administrator/);
    assert.equal(document.activeElement.id, 'aw-name', 'Enter advances and focuses the first field');

    await createAdmin(document);
    assert.match(byId(document, 'aw-status').textContent, /Step 3 of 5: Organisation/);
    assert.equal(document.activeElement.id, 'aw-org-name');
    byId(document, 'aw-skip').click();
    await waitStep(document, 4);
    assert.equal(document.activeElement.id, 'aw-currency');
    byId(document, 'aw-skip').click();
    await waitStep(document, 5);
    assert.equal(document.activeElement.id, 'aw-next');
    assert.match(byId(document, 'aw-status').textContent, /Step 5 of 5: Review/);
    dom.window.close();
});
