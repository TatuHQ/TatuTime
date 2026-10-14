// #232 increment 6 regression guard: every custom property referenced by a
// `var(--…)` in web/styles.css must be defined by the theme/palette tokens — in
// BOTH themes. This closes the class of bug that produced the deferred
// `#dlg-input { color: var(--text) }` defect reported by increment 4 (the token
// is `--fg`): the CSS still parsed and shipped, but the declaration silently did
// nothing because the property was never defined anywhere. A stylesheet that
// resolves in light but not dark (or vice versa) fails here too, and the two
// dark-theme blocks are pinned to the same token set so they cannot drift.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const css = readFileSync(new URL('../web/styles.css', import.meta.url), 'utf8');

// Every `var(--name)` reference (any fallback after the comma is irrelevant:
// the token itself must still exist).
const referenced = new Set();
for (const m of css.matchAll(/var\(\s*(--[\w-]+)/g)) referenced.add(m[1]);

// The declaration body of the named cascade block (first `{ … }` after it).
function body(selector) {
  const at = css.indexOf(selector);
  assert.notEqual(at, -1, `cascade block not found: ${selector}`);
  const open = css.indexOf('{', at);
  const close = css.indexOf('}', open);
  return css.slice(open + 1, close);
}
const definedIn = (text) => {
  const set = new Set();
  for (const m of text.matchAll(/(--[\w-]+)\s*:/g)) set.add(m[1]);
  return set;
};

const light = definedIn(body(':root {'));
const systemDark = definedIn(body(':root:not([data-theme="light"])')); // @media prefers-color-scheme: dark
const explicitDark = definedIn(body(':root[data-theme="dark"]'));

test('styles.css actually defines and references custom properties (#232 inc 6)', () => {
  assert.ok(referenced.size > 0, 'no var(--…) references parsed — the guard regex has broken');
  assert.ok(light.size > 0, 'no custom properties parsed from the light :root block');
});

test('every var(--…) in styles.css resolves in the light theme (#232 inc 6)', () => {
  const missing = [...referenced].filter((token) => !light.has(token)).sort();
  assert.deepEqual(
    missing,
    [],
    `undefined custom ${missing.length === 1 ? 'property' : 'properties'} referenced by styles.css: ${missing.join(', ')}`,
  );
});

test('every var(--…) in styles.css resolves in the dark theme (#232 inc 6)', () => {
  // `:root` matches in dark too, so its tokens cascade into both dark blocks;
  // assert against the union so a token defined only in an override still counts.
  const dark = new Set([...light, ...systemDark, ...explicitDark]);
  const missing = [...referenced].filter((token) => !dark.has(token)).sort();
  assert.deepEqual(missing, [], `undefined in dark: ${missing.join(', ')}`);
});

test('the two dark-theme blocks define the same token set (#232 inc 6)', () => {
  assert.deepEqual(
    [...systemDark].sort(),
    [...explicitDark].sort(),
    'the prefers-color-scheme dark block and [data-theme="dark"] have drifted apart',
  );
});
