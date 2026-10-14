// #232 increment 4 regression guard: a closed `<dialog>` must stay non-rendered.
//
// Why the jsdom suites missed it: jsdom never computes the CSS cascade, so every
// suite stayed green while the app was visibly broken. PR #290 moved the popups
// onto the Vanilla `p-modal` overlay, which means the native `<dialog>` now
// carries `p-modal` and the vendored `components.css` styles `.p-modal` with
// `display: flex` — an AUTHOR declaration. The cascade compares a declaration's
// ORIGIN before its specificity, so that author rule beats the user-agent
// stylesheet's `dialog:not([open]) { display: none }` *whatever* the UA rule's
// specificity: a closed dialog is no longer non-rendered and every popup painted
// itself as a fixed, full-viewport overlay on load.
//
// CSS cannot go back into the UA origin, so the closed state has to be re-asserted
// by an author rule that out-specifies the vendored class. This guard fails when:
//   * a class applied to a `<dialog>` gets a `display` from any stylesheet and no
//     `:not([open]) { display: none }` author rule neutralises it closed, or
//   * an author rule sets `display` on the dialog element itself without the guard
//     (now, or introduced later), or
//   * the guard exists but is out-specified by the rule it must beat, or
//   * the static markup ships an `open` dialog (the guard is then bypassed).
//
// Style follows the sibling static guards [`css-tokens.test.mjs`](./css-tokens.test.mjs)
// and [`condition-waits.test.mjs`](./condition-waits.test.mjs): no browser, no new
// dependency, a plain read of the shipped CSS/HTML.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8');
const styles = read('../web/styles.css');
const vendor = read('../web/vendor/ubuntu/components.css');
const html = read('../web/index.html');

// --------------------------------------------------------------- CSS parsing --
//
// `@`-rules are frames rather than rules, and the rules they contain are
// collected individually. Nothing here tries to be a CSS string tokenizer: the
// two pinned sheets contain no brace inside a quoted value, and the hygiene test
// below asserts exactly that (any desync leaks a `{`/`}` into a declaration body,
// which the parser then reports).
function parseRules(css) {
    const src = css.replace(/\/\*[\s\S]*?\*\//g, '');
    const rules = [];
    const stack = [];
    let start = 0;
    for (let i = 0; i < src.length; i += 1) {
        const ch = src[i];
        if (ch === '{') {
            stack.push({ prelude: src.slice(start, i).trim(), bodyStart: i + 1 });
            start = i + 1;
        } else if (ch === '}') {
            const frame = stack.pop();
            if (frame) {
                if (!frame.prelude.startsWith('@')) {
                    rules.push({ selector: frame.prelude, body: src.slice(frame.bodyStart, i) });
                }
                start = i + 1;
            }
        }
    }
    return rules;
}

// A declaration lands on the *subject* of a complex selector: the last compound,
// i.e. what follows the final combinator. `dialog input { … }` styles an input,
// `dialog.p-modal { … }` styles the dialog itself.
function subjectCompounds(selector) {
    return selector
        .split(',')
        .map((part) => part.split(/[>+~]|\s+/).filter(Boolean).pop() || '')
        .filter(Boolean);
}

// Does this compound style the `<dialog>` element itself (not a descendant merely
// scoped by it)? Class/id runs are stripped first so `.p-modal`/`#entry-dialog`
// cannot masquerade as the `dialog` type selector.
function targetsDialog(compound) {
    return /(?:^|[^\w-])dialog(?![\w-])/.test(compound.replace(/[.#][\w-]+/g, ''));
}

const classSet = (compound) => new Set((compound.match(/\.[\w-]+/g) || []).map((c) => c.slice(1)));

const declares = (body, prop) => new RegExp(`(?:^|[;{\\s])${prop}\\s*:`).test(body);
const declaredValue = (body, prop) => {
    const m = body.match(new RegExp(`(?:^|[;{\\s])${prop}\\s*:\\s*([^;}]+)`));
    return m ? m[1].trim() : null;
};
// Does this selector only apply in ONE of the two dialog states?
const isStateScoped = (selector) => /:not\(\s*\[open\]\s*\)|\[open\]/.test(selector);

// Deliberately conservative specificity (id, class-ish, type): `:not([open])` is
// counted twice (pseudo-class + its argument) and `::`-pseudo-elements are ignored.
// Over-counting a guard can only make the comparison stricter, never looser.
function specificity(selector) {
    const ids = (selector.match(/#[\w-]+/g) || []).length;
    const classes = (selector.match(/\.[\w-]+/g) || []).length
        + (selector.match(/\[[^\]]*\]/g) || []).length
        + (selector.match(/:(?!:)[\w-]+/g) || []).length;
    const rest = selector.replace(/[.#][\w-]+|\[[^\]]*\]|::?[\w-]+(\([^)]*\))?/g, '');
    const types = (rest.match(/[\w-]+/g) || []).length;
    return { ids, classes, types, score: ids * 1e6 + classes * 1e3 + types };
}

const higher = (a, b) => a.ids > b.ids
    || (a.ids === b.ids && a.classes > b.classes)
    || (a.ids === b.ids && a.classes === b.classes && a.types > b.types);

// The author rule that re-asserts the native non-rendered state for a closed
// dialog. Only web/styles.css is scanned: the vendored bundle is a pinned third
// party build and must stay untouched.
const guards = parseRules(styles).filter((rule) => /:not\(\s*\[open\]\s*\)/.test(rule.selector)
    && declares(rule.body, 'display')
    && /display\s*:\s*none/.test(rule.body)
    && subjectCompounds(rule.selector).some(targetsDialog));

const dialogTags = [...html.matchAll(/<dialog\b[^>]*>/g)].map((m) => m[0]);
const dialogClasses = new Set(dialogTags.flatMap((tag) => {
    const cls = tag.match(/class="([^"]*)"/);
    return cls ? cls[1].split(/\s+/).filter(Boolean) : [];
}));

// ------------------------------------------------------------------- hygiene --

test('the guard parses the shipped sheets (#232 inc 4 regression)', () => {
    assert.ok(parseRules(styles).length > 50, 'too few rules parsed from styles.css — the parser has broken');
    assert.ok(parseRules(vendor).length > 200, 'too few rules parsed from the vendored bundle — the parser has broken');
    // A brace leaking into a declaration body means the scan desynchronised (a
    // brace inside a quoted string would do it) and every conclusion below is
    // then unfounded — fail loudly instead of passing vacuously.
    for (const [name, rules] of [['web/styles.css', parseRules(styles)], ['web/vendor/ubuntu/components.css', parseRules(vendor)]]) {
        const leaked = rules.filter((rule) => /[{}]/.test(rule.body));
        assert.deepEqual(leaked, [], `${name}: ${leaked.length} rule bodies still contain a brace — the CSS scan desynchronised`);
    }
    assert.ok(dialogTags.length >= 10, `expected every popup to be a native <dialog>, found ${dialogTags.length}`);
    assert.ok(dialogClasses.size > 0, 'no class found on any <dialog> in index.html');
});

test('every popup is closed in the static markup (#102)', () => {
    const open = dialogTags.filter((tag) => /\sopen(?:[=\s>])/.test(tag));
    assert.deepEqual(open, [], `a dialog ships open, which bypasses the closed-state guard: ${open.join(' ')}`);
});

// --------------------------------------------------------------- the guard --

test('a `display` on a class carried by a <dialog> is neutralised while closed (#232 inc 4 regression)', () => {
    // Every stylesheet class that hands out a `display` to an element the app marks
    // as a popup, with the strongest specificity it does so at.
    const offenders = new Map();
    for (const [sheet, rules] of [['web/vendor/ubuntu/components.css', parseRules(vendor)], ['web/styles.css', parseRules(styles)]]) {
        for (const rule of rules) {
            if (!declares(rule.body, 'display')) continue;
            // A rule that already only applies to one state is not an offender when
            // it says `display: none` — that IS the closed-state guard.
            if (isStateScoped(rule.selector) && declaredValue(rule.body, 'display') === 'none') continue;
            for (const compound of subjectCompounds(rule.selector)) {
                for (const cls of classSet(compound)) {
                    if (!dialogClasses.has(cls)) continue;
                    const decl = {
                        sheet,
                        selector: rule.selector.trim(),
                        spec: specificity(rule.selector),
                        value: declaredValue(rule.body, 'display'),
                    };
                    const prev = offenders.get(cls);
                    if (!prev || higher(decl.spec, prev.spec)) offenders.set(cls, decl);
                }
            }
        }
    }

    assert.ok(offenders.size > 0, 'no `display`-setting class found on a dialog — the detection has broken');
    const unguarded = [];
    for (const [cls, decl] of offenders) {
        const guard = guards.find((rule) => subjectCompounds(rule.selector)
            .some((compound) => targetsDialog(compound) && classSet(compound).has(cls) && higher(specificity(rule.selector), decl.spec)));
        if (!guard) unguarded.push(`.${cls} (display from ${decl.sheet}: \`${decl.selector}\`)`);
    }
    assert.deepEqual(
        unguarded,
        [],
        `a closed <dialog> carrying these classes is still rendered by an author \`display\` — the UA's \`dialog:not([open]) { display: none }\` loses to ANY author declaration, so an author \`:not([open])\` guard with a higher specificity is required: ${unguarded.join(', ')}`,
    );
});

test('no author rule paints the dialog element itself without the closed-state guard (#232 inc 4 regression)', () => {
    const unguarded = parseRules(styles)
        .filter((rule) => declares(rule.body, 'display'))
        .filter((rule) => subjectCompounds(rule.selector).some(targetsDialog))
        .filter((rule) => !/\[open\]/.test(rule.selector))
        .filter((rule) => !guards.some((guard) => higher(specificity(guard.selector), specificity(rule.selector))))
        .map((rule) => `${rule.selector} { … display: … }`);
    assert.deepEqual(
        unguarded,
        [],
        `these rules set \`display\` on a <dialog> element and would render it while closed: ${unguarded.join(', ')}`,
    );
});

test('the closed-state guard out-specifies the vendored overlay it must beat (#232 inc 4 regression)', () => {
    const vendorDisplay = parseRules(vendor)
        .filter((rule) => declares(rule.body, 'display'))
        .flatMap((rule) => subjectCompounds(rule.selector).map((compound) => ({ rule, compound })))
        .filter(({ compound }) => [...classSet(compound)].some((cls) => dialogClasses.has(cls)));

    assert.ok(vendorDisplay.length > 0, 'the vendored overlay no longer sets `display` — this guard can be re-evaluated');
    for (const { rule } of vendorDisplay) {
        const spec = specificity(rule.selector);
        const winner = guards.find((guard) => higher(specificity(guard.selector), spec));
        assert.ok(
            winner,
            `no closed-state guard out-specifies the vendored \`${rule.selector.trim()}\` — an equal or lower specificity loses on origin/specificity and the popup would paint on load`,
        );
    }
});
