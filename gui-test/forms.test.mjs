// #232 phase 2b: the app's forms are the official Vanilla form patterns. The
// bespoke `.field` wrapper, `.hint` and `.form-error` skins are retired for
// `p-form__group` / `p-form__label` / `p-form__control` / `p-form-help-text` /
// `p-form-validation__message` (in a `p-form-validation is-error` wrapper), and
// every button-creation path now states its own `data-icon`/`data-positive`
// instead of relying on `decorateUbuntuButton`'s label/type/id fallbacks, which
// phase 2e then deleted outright. This
// offline jsdom suite (same harness as registers.test.mjs / chips.test.mjs)
// pins the vocabulary, the label<->control pairing, the preserved field
// contract, the CSS that left the app sheet and the button attribute coverage.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';
import { loadAppSource } from './load-app.mjs';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const app = loadAppSource();
const stylesCss = readFileSync(new URL('../web/styles.css', import.meta.url), 'utf8');
const vendorCss = readFileSync(new URL('../web/vendor/ubuntu/components.css', import.meta.url), 'utf8');
// Selectors only: comments carry the "#232 phase 2b" rationale and do mention
// the retired classes.
const stylesRules = stylesCss.replace(/\/\*[\s\S]*?\*\//g, '');

// The 22 form/panel error messages (`<p id="…-error" role="alert">`), now the
// official validation message inside an `is-error` group. `#invoice-error` left
// with the legacy direct draft generator (#349).
const ERROR_IDS = [
    'auth-error', 'entry-error', 'iw-error', 'ed-error',
    'rec-error', 'ret-error', 'expense-error', 'expense-category-error',
    'submission-error', 'user-error', 'user-password-error', 'secret-error',
    'config-error', 'category-error', 'org-error', 'template-error',
    'item-error', 'wz-error', 'customer-error', 'project-error', 'task-error',
    'week-row-error',
];

function shell() {
    const dom = new JSDOM(html, { url: 'http://localhost/' });
    return { dom, document: dom.window.document };
}

function setup() {
    const dom = new JSDOM(html, { runScripts: 'outside-only', url: 'http://localhost/' });
    dom.window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
    dom.window.HTMLDialogElement.prototype.close = function () { this.open = false; };
    const stub = `
window.f = { api, state, refreshCatalog, iwLoadProjects, fillSelect, $, decorateUbuntuButton };`;
    dom.window.eval(`${app}\n${stub}`);
    return { dom, document: dom.window.document, f: dom.window.f };
}

// ------------------------------------------------------------- vocabulary --

test('the shell ships the official form vocabulary and no bespoke class (#232 phase 2b)', () => {
    const { dom, document } = shell();
    const groups = document.querySelectorAll('.p-form__group');
    // 125/120/120 after #349 removed the legacy direct draft generator's three
    // field rows (customer / from / to) along with its form.
    assert.equal(groups.length, 125, 'every former .field wrapper is a p-form__group');
    assert.equal(document.querySelectorAll('.p-form__label').length, 120, 'one official label per field row');
    assert.equal(document.querySelectorAll('.p-form__control').length, 120, 'one official control per field row');
    // 32 after the rebase: upstream's Get paid box contributes one help text
    // (#357), and the #349 invoice detail panel adds the history state line plus
    // the delivery-record note in the message dialog.
    assert.equal(document.querySelectorAll('.p-form-help-text').length, 32, 'the hints became p-form-help-text');
    assert.equal(document.querySelectorAll('.p-form-validation__message').length, ERROR_IDS.length,
        'every error message is the official validation message');
    assert.equal(document.querySelectorAll('.p-form-validation.is-error').length, ERROR_IDS.length,
        'each validation message sits in an is-error group');
    assert.equal(document.querySelectorAll('.field').length, 0, 'no .field wrapper survives');
    assert.equal(document.querySelectorAll('.hint').length, 0, 'no .hint survives');
    assert.equal(document.querySelectorAll('.form-error').length, 0, 'no .form-error survives');
    // The filter bars and the dialog prompt stay app-owned on purpose: they are
    // horizontal/compact controls, not stacked form rows.
    assert.equal(document.querySelectorAll('.filter-field').length, 5, 'the compact filter controls stay app-owned');
    dom.window.close();
});

test('every p-form__group pairs its label with its own control (#232 phase 2b)', () => {
    const { dom, document } = shell();
    const groups = [...document.querySelectorAll('.p-form__group')];
    let paired = 0;
    const composed = [];
    for (const group of groups) {
        const label = [...group.children].find((child) => child.tagName === 'LABEL');
        if (!label) {
            // The composed rows carry no label of their own: the four grouped
            // fieldsets (their rows are nested groups) and the "Contacts" row,
            // whose caption is a span referenced by the fieldset heading.
            composed.push(`${group.tagName}${group.id ? '#' + group.id : ''}`);
            continue;
        }
        assert.ok(label.classList.contains('p-form__label'), `label in ${group.id || 'a group'} is p-form__label`);
        const control = label.control || document.getElementById(label.getAttribute('for'));
        assert.ok(control, `label ${label.getAttribute('for')} resolves to a control`);
        assert.ok(group.contains(control), `label ${label.getAttribute('for')} targets a control in its own group`);
        assert.ok(control.classList.contains('p-form__control'), `${control.id} is p-form__control`);
        assert.equal(control.labels.length, 1, `${control.id} keeps exactly one label association`);
        assert.equal(control.labels[0], label, `${control.id} keeps its original label`);
        assert.ok(label.textContent.trim().length > 0, `${control.id} keeps an accessible name`);
        paired += 1;
    }
    // 120 after #349 removed the legacy direct draft generator's three field
    // rows (customer / from / to) along with its form.
    assert.equal(paired, 120, 'all 120 label/control pairs were verified');
    assert.equal(composed.length, 5, 'five composed rows have no label of their own');
    assert.equal(composed.filter((tag) => tag === 'FIELDSET').length, 4, 'four grouped fieldsets');
    assert.equal(composed.filter((tag) => tag.startsWith('DIV')).length, 1, 'the Contacts row');
    dom.window.close();
});

// ------------------------------------------------- preserved field contract --

test('the migrated fields keep their ids, order and validation attributes (#232 phase 2b)', () => {
    const { dom, document } = shell();
    const row = (group) => [
        group.querySelector('label').getAttribute('for'),
        group.querySelector('.p-form__control').id,
        group.querySelector('.p-form__control').tagName,
    ];
    const entry = [...document.querySelectorAll('#entry-form .p-form__group')].map(row);
    assert.deepEqual(entry, [
        ['entry-customer', 'entry-customer', 'SELECT'],
        ['entry-project', 'entry-project', 'SELECT'],
        ['entry-task', 'entry-task', 'SELECT'],
        ['entry-hours', 'entry-hours', 'INPUT'],
        ['entry-note', 'entry-note', 'TEXTAREA'],
        ['entry-billable', 'entry-billable', 'INPUT'],
    ], 'the Day-entry dialog keeps its field order and labels');
    const weekRow = [...document.querySelectorAll('#week-row-form .p-form__group')].map(row);
    assert.deepEqual(weekRow, [
        ['week-row-customer', 'week-row-customer', 'SELECT'],
        ['week-row-project', 'week-row-project', 'SELECT'],
        ['week-row-task', 'week-row-task', 'SELECT'],
    ], 'the Week add-row dialog asks only customer/project/task');
    const auth = [...document.querySelectorAll('#auth-form .p-form__group')].map(row);
    assert.deepEqual(auth, [
        ['auth-name', 'auth-name', 'INPUT'],
        ['auth-email', 'auth-email', 'INPUT'],
        ['auth-password', 'auth-password', 'INPUT'],
    ]);
    // The attributes the migration must not have touched.
    const hours = document.getElementById('entry-hours');
    assert.deepEqual([hours.type, hours.min, hours.max, hours.step, hours.required, hours.getAttribute('inputmode')],
        ['number', '0.01', '24', '0.01', true, 'decimal']);
    assert.deepEqual([hours.name, hours.getAttribute('aria-invalid')], ['hours', null]);
    const note = document.getElementById('entry-note');
    assert.deepEqual([note.maxLength, note.rows, note.name], [500, 2, 'note']);
    const email = document.getElementById('auth-email');
    assert.deepEqual([email.type, email.autocomplete, email.required], ['email', 'username', true]);
    const accent = document.getElementById('org-accent');
    assert.deepEqual([accent.getAttribute('pattern'), accent.maxLength, accent.placeholder],
        ['^#[0-9a-fA-F]{6}$', 7, '#E95420'], 'the accent pattern/maxlength survive');
    const termsDays = document.getElementById('template-terms-days');
    assert.deepEqual([termsDays.type, termsDays.min, termsDays.max, termsDays.step], ['number', '1', '365', '1']);
    const currency = document.getElementById('expense-currency');
    assert.deepEqual([currency.maxLength, currency.placeholder, currency.required], [3, 'EUR', true]);
    dom.window.close();
});

// ---------------------------------------------------------------- the CSS --

test('web/styles.css no longer owns the form skins, and keeps the app-owned rhythm (#232 phase 2b)', () => {
    for (const selector of ['\\.field\\b', '\\.hint\\b', '\\.form-error\\b']) {
        assert.equal(new RegExp(selector).test(stylesRules), false,
            `${selector.replace(/\\\\/g, '')} must not survive as a selector`);
    }
    // The classes the app switched to really are in the vendored bundle.
    for (const cls of ['p-form__group', 'p-form__label', 'p-form__control', 'p-form-help-text', 'p-form-validation__message']) {
        assert.ok(vendorCss.includes(cls), `${cls} comes from the vendored Vanilla build`);
    }
    // App-owned rhythm Vanilla does not express: a constant .65rem row rhythm
    // (Vanilla spaces groups only inside .p-form--stacked, at >= 1036px), the
    // tick-box size the base width:100% rule does not cover, the fieldset frame
    // that overrides Vanilla's tinted/clipping base fieldset, and the help-text
    // margins that the bundle would otherwise pull up (.p-form-help-text sits
    // after headings in this app, not after an input).
    assert.ok(/\.p-form__group \{ margin-bottom: \.65rem; \}/.test(stylesRules), 'the group row rhythm');
    assert.ok(/\.p-form__group input\[type="checkbox"\], \.theme-switch input \{ min-height: 0; width: 1rem; height: 1rem; \}/.test(stylesRules),
        'the tick-box size');
    assert.ok(/fieldset\.p-form__group \{/.test(stylesRules), 'the fieldset frame');
    assert.ok(/fieldset\.p-form__group > legend \{/.test(stylesRules), 'the fieldset legend');
    assert.ok(/\.customer-extra-details fieldset\.p-form__group \{ margin-top: \.5rem; \}/.test(stylesRules),
        'the nested fieldset rhythm');
    assert.ok(/\.p-form-help-text \{ margin: revert; \}/.test(stylesRules), 'the help-text margin override');
    assert.ok(/#wizard-dialog \.p-form__group:last-child \{ margin-bottom: 0; \}/.test(stylesRules),
        'the wizard keeps no trailing gap before its action row');
});

// --------------------------------------------------------------- errors ----

test('every error stays a role=alert validation message while hidden (#232 phase 2b)', () => {
    const { dom, document } = shell();
    const seen = [];
    for (const id of ERROR_IDS) {
        const el = document.getElementById(id);
        assert.ok(el, `#${id} exists`);
        assert.equal(el.getAttribute('role'), 'alert', `#${id} stays role=alert`);
        assert.equal(el.className, 'p-form-validation__message', `#${id} is the official message`);
        assert.equal(el.hidden, true, `#${id} ships hidden (the JS reveals it)`);
        const wrapper = el.parentElement;
        assert.equal(wrapper.className, 'p-form-validation is-error', `#${id} sits in an is-error group`);
        assert.equal(el.getAttribute('aria-describedby'), null);
        seen.push(id);
    }
    assert.deepEqual(seen, ERROR_IDS, 'the error set is unchanged (ids are the JS contract)');
    assert.equal(document.querySelectorAll('[role="alert"]').length, ERROR_IDS.length,
        'no extra role=alert node appeared');
    dom.window.close();
});

// ------------------------------------------------- button attribute contract --
//
// #232 phase 2e: `decorateUbuntuButton` no longer infers anything — its old
// submit-type / class / id-allowlist / label-matching fallbacks (and the
// label-mutating "+"-strip they brought) are deleted, so decoration is a pure
// function of `data-icon` and `data-positive`. This guard asserts both halves of
// that contract: (1) the decorator adds nothing when the two attributes are
// absent, so reintroducing a fallback fails here; and (2) no rendered button
// relies on the deleted fallback — every button that needs a colour or a glyph
// states it, with the coverage pinned so dropping an attribute also fails.

test('decoration is attribute-only and every rendered button states what it needs (#232 phase 2e)', () => {
    const { dom, document, f } = setup();
    const label = (b) => b.id || b.textContent.trim() || b.outerHTML.slice(0, 40);

    // (1) No inference. Decorate a clone of every shell button with its
    // decoration attributes stripped: nothing may be re-derived from the label,
    // the type or the id.
    for (const button of document.querySelectorAll('button')) {
        if (button.getAttribute('role') === 'tab' || button.classList.contains('p-chip__dismiss')) continue;
        const clone = button.cloneNode(true);
        clone.removeAttribute('data-icon');
        clone.removeAttribute('data-positive');
        clone.classList.remove('p-button', 'p-button--positive');
        clone.querySelectorAll('i[class*="p-icon--"]').forEach((icon) => icon.remove());
        assert.equal(clone.querySelector('[class*="p-icon--"]'), null, 'the clone starts with no icon');
        f.decorateUbuntuButton(clone);
        assert.equal(clone.querySelector('[class*="p-icon--"]'), null,
            `${label(button)} must not gain an icon without data-icon`);
        assert.equal(clone.classList.contains('p-button--positive'), false,
            `${label(button)} must not become positive without data-positive`);
        if (!clone.classList.contains('link') && !clone.classList.contains('danger')) {
            assert.ok(clone.classList.contains('p-button'), `${label(button)} still gets the neutral skin`);
        }
    }

    // (2) Coverage. A form submit control is always the primary action here, so
    // it must say so; the legacy `.primary` class may decorate a button but must
    // never be the only positive signal (that fallback is deleted).
    const submits = [...document.querySelectorAll('button[type="submit"]')];
    assert.ok(submits.length > 0, 'the shell has form submit controls');
    for (const b of submits) assert.equal(b.dataset.positive, '1', `submit ${label(b)} states data-positive`);
    for (const b of document.querySelectorAll('button.primary')) {
        assert.equal(b.dataset.positive, '1', `.primary button ${label(b)} also states data-positive`);
    }

    // Pinned coverage: dropping a data-* attribute from any shell button makes
    // one of these counts drop and fails the guard.
    const icons = [...document.querySelectorAll('button[data-icon]')];
    assert.equal(icons.length, 17, 'every icon-bearing shell button states data-icon');
    const vocab = {};
    for (const b of icons) vocab[b.dataset.icon] = (vocab[b.dataset.icon] || 0) + 1;
    assert.deepEqual(vocab, { plus: 12, copy: 2, play: 2, stop: 1 }, 'the shell icon vocabulary is pinned');
    // 33 after #349 removed the legacy direct draft generator (whose `Generate
    // draft` carried data-positive). The count is pinned so a new positive
    // action updates this guard on purpose: `+ Manual Invoice` is the secondary
    // action and deliberately carries no data-positive, and the wizard dialog's
    // `Close` is a plain secondary button, so neither adds to it.
    assert.equal(document.querySelectorAll('button[data-positive]').length, 33,
        'every positive shell button states data-positive');

    // The deleted label-mutation leaves no trace: no rendered label keeps a "+".
    assert.equal(/<button[^>]*>\s*\+/.test(html), false, 'no button label carries a leading "+"');
    dom.window.close();
});

test('the JS builders emit the official fields and explicit button icons (#232 phase 2b)', async () => {
    const { dom, document, f } = setup();
    f.api.get = async (path) => {
        if (path === '/customers/c1') return { customer: { id: 'c1', name: 'ACME', currency: 'EUR' } };
        if (path.startsWith('/entries?')) return { entries: [{ id: 'e1', customer_id: 'c1', billable: true, project_code: 'SM-1', hours: 2 }] };
        if (path === '/invoices') return { invoices: [] };
        if (path === '/customers/c1/projects') return { projects: [{ code: 'SM-1', name: 'Smoke', active: true }] };
        return {};
    };
    // A <select> ignores a .value with no matching option, so fill it first.
    f.fillSelect(f.$('iw-customer'), [{ value: 'c1', text: 'ACME' }]);
    f.$('iw-from').value = '2026-10-01';
    f.$('iw-to').value = '2026-10-31';
    await f.iwLoadProjects();
    const group = document.querySelector('#iw-projects .p-form__group');
    assert.ok(group, 'the generated project tick box is a p-form__group');
    assert.equal(document.querySelector('#iw-projects .field'), null, 'the generated markup is not the bespoke .field');
    const label = group.querySelector('label.p-form__label');
    const box = group.querySelector('input.p-form__control');
    assert.equal(label.getAttribute('for'), 'iw-p-SM-1');
    assert.equal(box.id, 'iw-p-SM-1');
    assert.equal(box.type, 'checkbox');
    assert.equal(label.control, box, 'the generated label still owns its control');

    // The empty state is the official help text, not the bespoke hint.
    f.api.get = async (path) => (path === '/invoices' ? { invoices: [] }
        : path === '/customers/c1/projects' ? { projects: [] }
            : path === '/customers/c1' ? { customer: { id: 'c1' } } : { entries: [] });
    await f.iwLoadProjects();
    assert.equal(document.querySelector('#iw-projects .p-form-help-text').textContent,
        'No uninvoiced billable work in this period.');

    // Row actions state their own icon (they used to be derived from the label).
    f.api.get = async () => ({ item_types: [{ id: 'i1', name: 'Consulting', kind: 'service', description: '', default_price_minor: 12500, currency: 'EUR', active: true }] });
    await f.refreshCatalog();
    const actions = [...document.querySelectorAll('#item-table tbody button')];
    assert.deepEqual(actions.map((b) => b.dataset.icon), ['edit', undefined, 'delete'],
        'the Edit/Delete row actions state their icon; Archive/Restore has none');
    dom.window.close();
});
