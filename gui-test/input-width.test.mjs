// #232 phase 2c regression guard: no checkbox/radio may be forced to
// `width: 100%` by an unscoped box rule.
//
// Why this exists. `#expense-dialog input { width: 100% }` has specificity
// (1,0,1) — one id — and the app's tick-box sizing,
// `.p-form__group input[type="checkbox"]`, is (0,2,1) — no id. The cascade
// compares the id count BEFORE the class count, so the unscoped rule won and
// the #expense-billable box painted 100% wide instead of 1rem. jsdom computes
// no CSS cascade, so every jsdom suite stayed green while the box was visibly
// wrong; a real browser saw it. This static guard pins it without a browser,
// in the style of [`css-tokens.test.mjs`](./css-tokens.test.mjs) and
// [`dialog-display.test.mjs`](./dialog-display.test.mjs).
//
// It fails when:
//   * any selector in web/styles.css that matches a checkbox/radio resolves a
//     `width` of `100%` (an unscoped `input { width: 100% }`-style rule that
//     out-specifies the tick-box sizing), or
//   * the styled sheet/HTML stops parsing, so the check cannot pass vacuously.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';

const css = readFileSync(new URL('../web/styles.css', import.meta.url), 'utf8');
const html = readFileSync(new URL('../web/index.html', import.meta.url), 'utf8');

// Rules of one pinned sheet, as `{ selector, body, order }`. `@`-frames are
// skipped and their inner rules collected individually (the two pinned sheets
// hold no brace inside a quoted value; jsdom's parse of the HTML is separate).
function parseRules(src) {
    const clean = src.replace(/\/\*[\s\S]*?\*\//g, '');
    const rules = [];
    const stack = [];
    let start = 0;
    for (let i = 0; i < clean.length; i += 1) {
        const ch = clean[i];
        if (ch === '{') {
            stack.push({ prelude: clean.slice(start, i).trim(), bodyStart: i + 1 });
            start = i + 1;
        } else if (ch === '}') {
            const frame = stack.pop();
            if (frame && !frame.prelude.startsWith('@')) {
                rules.push({ selector: frame.prelude, body: clean.slice(frame.bodyStart, i), order: rules.length });
            }
            start = i + 1;
        }
    }
    return rules;
}

// Conservative (id, class-ish, type) specificity; `:not(...)` counts as
// pseudo-class + its argument, `::`-pseudo-elements are ignored.
function specificity(selector) {
    const ids = (selector.match(/#[\w-]+/g) || []).length;
    const classes = (selector.match(/\.[\w-]+/g) || []).length
        + (selector.match(/\[[^\]]*\]/g) || []).length
        + (selector.match(/:(?!:)[\w-]+/g) || []).length;
    const rest = selector.replace(/[.#][\w-]+|\[[^\]]*\]|::?[\w-]+(\([^)]*\))?/g, '');
    const types = (rest.match(/[\w-]+/g) || []).length;
    return { ids, classes, types };
}
const higher = (a, b) => a.ids > b.ids
    || (a.ids === b.ids && a.classes > b.classes)
    || (a.ids === b.ids && a.classes === b.classes && a.types > b.types);

// Every `width: <value>` declaration, flattened to one entry per selector part.
function widthDeclarations(rules) {
    const out = [];
    for (const rule of rules) {
        const m = rule.body.match(/(?:^|[;{\s])width\s*:\s*([^;}]+)/);
        if (!m) continue;
        for (const sel of rule.selector.split(',').map((s) => s.trim()).filter(Boolean)) {
            out.push({ sel, value: m[1].trim(), order: rule.order });
        }
    }
    return out;
}

// The winning `width` for `el` among the declarations that match it: highest
// specificity wins, ties go to the later rule (source order).
function winningWidth(el, decls) {
    let best = null;
    for (const d of decls) {
        let matches = false;
        try { matches = el.matches(d.sel); } catch { matches = false; } // pseudo-elements etc. never match
        if (!matches) continue;
        const spec = specificity(d.sel);
        if (!best || !higher(best.spec, spec)) best = { value: d.value, sel: d.sel, spec };
    }
    return best;
}

const rules = parseRules(css);
const decls = widthDeclarations(rules);
const dom = new JSDOM(html, { url: 'http://localhost/' });
const doc = dom.window.document;
const controls = [...doc.querySelectorAll('input[type="checkbox"], input[type="radio"]')];

test('the guard parses the shipped sheet and static markup', () => {
    assert.ok(rules.length > 100, `too few rules parsed from styles.css (${rules.length}) — the parser has broken`);
    assert.ok(decls.length > 0, 'no `width` declaration parsed from styles.css — the guard regex has broken');
    assert.ok(controls.length >= 8, `expected the app's checkbox/radio controls in index.html, found ${controls.length}`);
    // A brace leaking into a body means the scan desynchronised and every
    // conclusion below is unfounded — fail loudly instead of passing vacuously.
    const leaked = rules.filter((r) => /[{}]/.test(r.body));
    assert.deepEqual(leaked, [], 'a rule body still contains a brace — the CSS scan desynchronised');
});

test('no checkbox/radio is forced to width:100% by web/styles.css (#232 phase 2c)', () => {
    const offenders = [];
    for (const el of controls) {
        const win = winningWidth(el, decls);
        if (win && /%$/.test(win.value) && win.value !== 'auto') {
            offenders.push(`#${el.id || el.name || '(unnamed)'} -> ${win.value} from \`${win.sel}\``);
        }
    }
    assert.deepEqual(
        offenders,
        [],
        `an unscoped width rule out-specifies the tick-box sizing (a checkbox/radio resolves to a % width): ${offenders.join('; ')}`,
    );
});

test('the #expense-billable tick box resolves to the 1rem tick-box size (#232 phase 2c)', () => {
    const el = doc.getElementById('expense-billable');
    assert.ok(el, '#expense-billable exists in index.html');
    const win = winningWidth(el, decls);
    assert.ok(win, 'a width rule matches #expense-billable');
    assert.equal(
        win.value,
        '1rem',
        `#expense-billable must keep the 1rem tick-box size, but \`${win.sel}\` set width:${win.value}`,
    );
});
