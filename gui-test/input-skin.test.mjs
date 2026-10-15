// #232 phase 2f regression guard: every `<input>` must declare a `type`, and
// that type must be one the vendored Vanilla sheet actually skins.
//
// Why this exists. Vanilla skins form controls with *attribute* selectors —
// `[type=file], textarea, [type=text], [type=date], … , select { … }` — and
// ships no `input { … }` base rule of its own
// (`web/vendor/ubuntu/components.css`). An `<input>` written without a `type`
// matches none of them and keeps the user agent's skin, so in "Add customer"
// the Name field (no `type`) sat directly beside the Billing email field
// (`type="email"`) with a different padding, border and background. The app's
// own sheet never re-skins these controls, and jsdom computes no cascade, so no
// other guard could see it.
//
// The companion real-browser probe (`browser-smoke.py::VANILLA_INPUT_SKIN`)
// compares the computed skin of every control against a reference control; this
// static guard pins the markup rule that makes that possible.
//
// It fails when:
//   * any `<input>` in web/index.html carries no explicit `type`, or
//   * an input declares a `type` Vanilla does not skin and that is not one of
//     the non-text controls treated on their own (checkbox/radio/file/…), or
//   * the vendored sheet stops declaring the skinned list, so the check above
//     cannot pass vacuously.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';

const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');
const vendor = readFileSync(new URL('../web/vendor/ubuntu/components.css', import.meta.url), 'utf8');

// The single rule that carries the text-control skin, read straight out of the
// vendored sheet so a Vanilla bump cannot silently narrow the guard:
// `[type=file],textarea,[type=text],…,[type=tel],select{…}`. `textarea` and
// `select` are element selectors in that same rule, not `[type=…]` values.
const skinRule = vendor.match(/\[type=file\][^{]*\{/);
const skinned = new Set(
    (skinRule ? skinRule[0].match(/\[type=[\w-]+\]/g) || [] : []).map((s) => s.slice(6, -1)),
);

// Control types that carry their own treatment, so an explicit `type` is enough
// even though the text-control skin does not list them.
const OWN_TREATMENT = new Set([
    'checkbox', 'radio', 'hidden', 'file', 'submit', 'reset', 'button', 'image',
    'range', 'color',
]);

const dom = new JSDOM(html, { url: 'http://localhost/' });
const { document } = dom.window;
const inputs = [...document.querySelectorAll('input')];

test('the guard reads the vendored skin list and the shipped markup (#232 phase 2f)', () => {
    assert.ok(skinned.size >= 10, `too few skinned input types parsed from components.css (${skinned.size}) — the guard regex has broken`);
    assert.ok(skinned.has('text') && skinned.has('email'), `the parsed skin list lost text/email: ${[...skinned].join(', ')}`);
    assert.ok(inputs.length > 50, `too few inputs parsed from index.html (${inputs.length}) — the markup scan has broken`);
});

test('every input declares a type Vanilla actually skins (#232 phase 2f)', () => {
    const missing = inputs
        .filter((el) => !el.getAttribute('type'))
        .map((el) => el.id || el.name || '(unnamed)');
    assert.deepEqual(
        missing,
        [],
        `input(s) without an explicit type keep the user agent skin instead of Vanilla's: ${missing.join(', ')}`,
    );
    const unskinned = [];
    for (const el of inputs) {
        const type = el.getAttribute('type').toLowerCase();
        if (skinned.has(type) || OWN_TREATMENT.has(type)) continue;
        unskinned.push(`#${el.id || el.name || '(unnamed)'} type=${type}`);
    }
    assert.deepEqual(
        unskinned,
        [],
        `input type(s) no Vanilla rule skins, so the field would look different from its neighbours: ${unskinned.join('; ')}`,
    );
});

test('the app prompt field keeps an explicit text type (#232 phase 2f)', () => {
    // `#dlg-input` retires its bespoke border/radius/background so it wears the
    // same official skin as every other field — which only works while it
    // declares `type="text"` (the app dialog still wraps it in a bare <label>).
    assert.equal(document.getElementById('dlg-input').getAttribute('type'), 'text');
});
