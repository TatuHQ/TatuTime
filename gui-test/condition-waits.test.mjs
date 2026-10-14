// #265 guard: the harness must never regress to fixed-delay waits. `tick(n)`
// asserted against wall-clock and passed only when the runner happened to be
// fast enough — the documented root cause of the recurring `gui-test` flake
// family. Every wait must therefore be a *condition* wait: wait-for.mjs's
// waitFor(predicate) or gui.mjs's settle() over the requests the UI started.
//
// The only timers allowed in the harness are the ones inside the primitive
// itself. This file is part of `npm run test:jsdom`, so a reintroduced sleep
// fails CI with the file and line named.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';

const dir = new URL('./', import.meta.url);
const SELF = 'condition-waits.test.mjs';
const PRIMITIVE = 'wait-for.mjs';
const GUARDED = readdirSync(dir)
  .filter((name) => name.endsWith('.mjs') && name !== SELF && name !== PRIMITIVE)
  .sort();

// A fixed wait is a timer whose *duration* decides the outcome: any positive
// numeric delay. A 0ms "queue me a turn of the event loop" yield is not one.
const FORBIDDEN = [
  { what: 'a fixed-duration tick call', re: /\btick\s*\(\s*[1-9]/ },
  { what: 'a fixed-duration timer', re: /setTimeout\s*\([^)]*,\s*[1-9]\d*\s*\)/ },
  { what: 'a named fixed delay', re: /\b(?:sleep|delay|pause)\s*\(\s*[1-9]/ },
];

test('the harness waits on conditions, never on fixed delays (#265)', () => {
  assert.ok(GUARDED.length >= 10, `expected the harness files, got ${GUARDED.length}: ${GUARDED}`);
  const offenders = [];
  for (const name of GUARDED) {
    readFileSync(new URL(name, dir), 'utf8')
      .split('\n')
      .forEach((line, index) => {
        for (const { what, re } of FORBIDDEN) {
          if (re.test(line)) offenders.push(`${name}:${index + 1} ${what}: ${line.trim()}`);
        }
      });
  }
  assert.deepEqual(offenders, [], 'these waits must become waitFor()/settle() instead');
});
