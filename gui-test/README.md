# GUI harness (jsdom)

Headless smoke test that loads `web/index.html` + `web/app.js` into jsdom and
drives the real forms against a running server (default `http://localhost:8099`).

```sh
cargo build --release
TUCANO_DATA_DIR=$(mktemp -d) TUCANO_PORT=8099 TUCANO_STRIPE_FAKE=1 ./target/release/tucano-time &
cd gui-test && npm install && npm test
```

`npm test` is the live-server smoke flow only. The full CI gate is
`node contrast.mjs`, the offline jsdom unit suites (`npm run test:jsdom` —
every `*.test.mjs` file) and finally `npm test`, in that order; run all three
locally before opening a GUI PR.

Checks cover: login, day view (#12) rows/week strip/copy-forward, inline add
form, week grid (#13), customers/projects/tasks, reports, invoices, expenses,
submissions, the inline `<dialog>` confirm/prompt flows (#102) and the config
card. Runs in CI as the `gui-test` job (#102); keep it green through any GUI
refactor.

## Waiting for the UI (condition-waits) — #265

The suite drives the real app in jsdom, so every interaction — a click, a form
submit, a filter `change` — ends in an asynchronous fetch plus a re-render.
Asserting straight after the interaction races that render: the check passes
only when the runner happens to be fast, which is what produced the recurring
`gui-test` flake family (the "self-hosted runner lost communication"/early-assert
symptoms and the `copy-previous-day` timing failure).

**Never assert on wall-clock.** Wait for a condition instead:

- `waitFor(predicate, { timeoutMs = 5000, intervalMs = 10, message })` from
  [`wait-for.mjs`](./wait-for.mjs) polls the state the next check depends on
  (element present, text updated, view rendered, dialog open, list settled) and
  **fails loudly** with `waitFor: timed out after …ms waiting for …` when it
  never holds — it never silently continues. It is reusable from every harness
  file.
- `settle(message)` in [`gui.mjs`](./gui.mjs) waits for the requests the UI
  itself started to drain (the harness counts them in its `fetch` wrapper) plus a
  two-poll quiet window, so the re-render after the last response has had a turn
  to run. Use it where the next check reads server-fed data and there is no
  single crisp DOM condition to key on.
- Slow responses and stale-response races are driven deterministically: a test
  holds the response open (a deferred resolver) and releases it once the newer
  render has landed, instead of sleeping and hoping.

No assertion is weakened and no timeout is lengthened to paper over a race — a
check that only passes because it sleeps longer is a failed check. The harness
contains no fixed-duration wait, and
[`condition-waits.test.mjs`](./condition-waits.test.mjs) (part of
`npm run test:jsdom`) fails CI if one is reintroduced, naming the file and line.
`gui-test/node_modules` is gitignored, so run the gates in the pinned `node:22-slim`
image (`npm ci` first) or a local Node 22 with the dependencies installed.

## Visual evidence (screenshots) — #140/#142/#133/#141

The suite itself never compares pixels (deliberately OS-fragile); when a
review needs visual evidence, capture it manually against a local server:

```sh
cargo build
TUCANO_DATA_DIR=/tmp/tt-shots TUCANO_PORT=8080 TUCANO_STRIPE_FAKE=1 ./target/debug/tucano-time
# sign in as the bootstrapped admin in a browser at http://localhost:8080
```

- Desktop (1280px): Timesheets → Calendar segment (#140 month grid), the
  grouped sidebar rail with the 🧾 Invoice / ⏱ Timer shortcuts (#142), and
  Invoices → select a row to open the document preview (#133).
- Narrow (720px, devtools device width): the same three views — rail groups
  wrap horizontally, the calendar collapses columns, the preview card sits
  under the table.
- The #141 E2E workflow check asserts the same surfaces structurally
  (aria-labels, selected states, totals, draft status) — screenshots from
  these steps are its visual companion, not a test input.
