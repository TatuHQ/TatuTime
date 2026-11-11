// Regression guard for the phone quick-navigation bar (#451 follow-up).
//
// The bar used to be `position: fixed` inside the document. On Android Chrome
// that anchors it to the layout viewport, so while the browser animated its
// URL bar after a route change the bar floated above the bottom edge and left
// a blank strip underneath. The shell is now exactly one dynamic viewport tall
// (`100dvh`) with the bar as the last in-flow flex row, so the two edges cannot
// come apart.
//
// jsdom computes no layout, so — like the other static guards
// [`css-tokens.test.mjs`](./css-tokens.test.mjs) and
// [`dialog-display.test.mjs`](./dialog-display.test.mjs) — this test reads the
// shipped CSS and markup instead of a rendered page.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8');
const css = read('../web/styles.css');
const html = read('../web/index.html');

// The body of a phone media query: braces are counted so a nested rule can
// never end the slice early. The sheet has several `max-width: 720px` blocks;
// the caller picks the one it needs by content.
function mediaBodies(source, prelude) {
    const bodies = [];
    let from = 0;
    for (;;) {
        const at = source.indexOf(prelude, from);
        if (at === -1) break;
        const open = source.indexOf('{', at);
        let depth = 0;
        let i = open;
        for (; i < source.length; i += 1) {
            if (source[i] === '{') depth += 1;
            else if (source[i] === '}' && --depth === 0) break;
        }
        bodies.push(source.slice(open + 1, i));
        from = i;
    }
    assert.ok(bodies.length > 0, `media query missing: ${prelude}`);
    return bodies;
}

const phone = mediaBodies(css, '@media (max-width: 720px)')
    .find((body) => body.includes('.mobile-navigation {'));
assert.ok(phone, 'phone block for the quick navigation is missing');

function declarations(source, selector) {
    const at = source.indexOf(selector);
    assert.notEqual(at, -1, `rule missing: ${selector}`);
    const open = source.indexOf('{', at);
    return source.slice(open + 1, source.indexOf('}', open));
}

test('the phone shell anchors quick navigation to the dynamic viewport', () => {
    const app = declarations(phone, '.app {');
    assert.match(app, /height:\s*100dvh/, 'the shell must take the dynamic viewport height');
    assert.match(app, /height:\s*var\(--app-height\)/,
        'the visible height reported by visualViewport must win over dvh');

    const nav = declarations(phone, '.mobile-navigation {');
    assert.doesNotMatch(nav, /position:\s*fixed/,
        'an in-flow bar cannot drift while the browser animates its URL bar');
    assert.doesNotMatch(nav, /safe-area-inset-bottom/,
        'no reserved inset strip may paint under the bar (the page is not viewport-fit=cover)');
    assert.match(nav, /order:\s*2/, 'the bar is the last flex row, after the action bar');
    assert.match(phone, /main\s*{[^}]*overflow-y:\s*auto/,
        'the panels scroll inside <main>, not the document');

    const actions = declarations(phone, '.mobile-actions:not([hidden]) {');
    assert.match(actions, /order:\s*1/, 'the pinned action sits directly above the tabs');
});

test('the phone primary actions move into the pinned bar', () => {
    assert.match(phone, /#day-add,\s*#expense-new,\s*#invoice-new\s*{\s*display:\s*none/,
        'the in-panel primary buttons must not duplicate the pinned action');
});

test('the version footer stays off the phone', () => {
    const foot = declarations(phone, '.foot {');
    assert.match(foot, /display:\s*none/,
        'the build string belongs to desktop/About, not between the entries and the bar');
});

test('the phone week strip is one flat row of seven days', () => {
    const strip = declarations(phone, '.week-strip {');
    assert.match(strip, /grid-template-columns:\s*repeat\(7,/,
        'seven days share one row so the strip cannot wrap onto a second line');

    const day = declarations(phone, '.ws-day.p-card,');
    assert.match(day, /background:\s*transparent/,
        'a phone day is a flat cell, not a card');
    assert.match(day, /border:\s*0/, 'no card border on the phone strip');
    assert.match(day, /flex-direction:\s*column/,
        'weekday, date and hours stack in one column');

    const date = declarations(phone, '.ws-date {');
    assert.match(date, /border-radius:\s*50%/, 'the selected date sits in a circle');

    const selected = declarations(phone, '.ws-day.selected .ws-date {');
    assert.match(selected, /background:\s*var\(--fg\)/,
        'the selected day is marked by a filled circle, not a card tint');
});

test('the quick navigation does not duplicate the header drawer button', () => {
    const start = html.indexOf('id="mobile-navigation"');
    assert.notEqual(start, -1, 'quick navigation missing from the markup');
    const nav = html.slice(start, html.indexOf('</nav>', start));
    const ids = [...nav.matchAll(/<button[^>]*id="(mobile-[\w-]+)"/g)].map((m) => m[1]);
    assert.deepEqual(ids, ['mobile-time', 'mobile-expenses', 'mobile-invoices'],
        'the drawer is opened from the masthead button only');
});
