# GUI harness (jsdom + real browser)

Headless smoke test that loads `web/index.html` into jsdom and concatenates the
ES modules in `web/js/` (see [`load-app.mjs`](./load-app.mjs)); `web/app.js` is
the browser entry point. It drives the real forms against a running server
(default `http://localhost:8099`).

```sh
cargo build --release
TUCANO_DATA_DIR=$(mktemp -d) TUCANO_PORT=8099 ./target/release/tucano-time &
cd gui-test && npm install && npm test
```

`npm test` is the live-server smoke flow only. The full CI gate is
`node contrast.mjs`, the offline jsdom unit suites (`npm run test:jsdom` —
every `*.test.mjs` file), `npm test` and then the real-browser gate
(`python3 gui-test/browser-smoke.py`, see below), in that order; run all four
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

The suites read the app from `../web/…`, so a container run must mount **both**
trees: `gui-test` as the working directory and `web` next to it. This is exactly
what CI does (`.github/workflows/ci.yml`, `gui-test` job):

```sh
docker run --rm --network host \
  -v "$PWD/gui-test:/gui" -v "$PWD/web:/web:ro" \
  -e TT_GUI_BASE="http://localhost:$TT_PORT" \
  -w /gui node:22-slim \
  bash -c "npm ci --no-audit --no-fund && node contrast.mjs && npm run test:jsdom --silent && node gui.mjs"
```

Mounting only `gui-test` (e.g. `-v "$PWD/gui-test:/w" -w /w`) leaves `../web/…`
unresolvable, so the suites fail on missing files instead of on the app.

**Blind spot to know about.** [`load-app.mjs`](./load-app.mjs) concatenates the
module sources in file order into one scope, so the jsdom suites resolve no
module URL, enforce no MIME type, drive no CSS cascade and never evaluate in the
browser's module order. A specifier that resolves to the wrong URL (the #264
regression), a module cycle, a boot-time exception, a CSP violation or a cascade
regression are therefore invisible here. Structural coverage lives in
`tests/contract/gui_assets.rs::gui_module_graph_is_served_with_js_mime` (each specifier
resolved against the URL of the module that declares it: 200 + JS MIME per hop),
[`module-order.test.mjs`](./module-order.test.mjs) (the real graph evaluated in
the browser's depth-first post-order) and
[`dialog-display.test.mjs`](./dialog-display.test.mjs) (the closed-dialog
cascade guard) — and the *behavioural* coverage is the real-browser gate below.

## GUI failure surfaces — reuse, don't reinvent (#333)

Three deliberate explainers live in the app's failure path (review §3). A new
dialog or form **reuses** them instead of hand-rolling wording, and **never hides
the server's own reason**:

1. **A refusal without the app's error shape** — [`requestFailure()`](../web/js/api.js:14).
   Names the method, the path and the verbs the endpoint takes (from the `Allow`
   header): `PUT /api/expenses/abc failed: HTTP 405 (this endpoint accepts GET,
   DELETE)`. The server's own message wins whenever it sent one; the fallback is
   for a proxy page or a build too old to know the route, where a bare `HTTP 405`
   would leave a developer guessing between a wrong verb, a stale server and a
   proxy in the middle.
2. **A field the server does not know** — [`showFormError()`](../web/js/dom.js:247).
   An `unknown field` reason (the `deny_unknown_fields` rejection in
   `api::deser_error`) appends one note: `— the running server is older than this
   page: restart or rebuild it.` serde only refuses a key that was actually sent,
   so this is never a bad name in the payload.
3. **A failure with no field** — same function. When serde cannot tie the refusal
   to a key, `field` is empty and the reason is printed alone; never `": reason"`
   with a dangling colon.

**Why a page can be newer than the server answering it.** `TUCANO_WEB_DIR` serves
the working tree from disk while the binary's Rust is fixed at compile time (and a
debug build embeds `web/` from disk per request — rust-embed), so a long-running
process hands out today's GUI over yesterday's contract. That drift is exactly the
failure mode (1) and (2) name. `showFormError` adds only the stale-server note on
top of the server's field text — its reason is never replaced with GUI wording.

**What pins each surface:**

- [`api.test.mjs`](./api.test.mjs) — the wording of (1) and of (2)/(3), offline in
  jsdom (drives `requestFailure` and `showFormError` directly).
- [`gui.mjs`](./gui.mjs) — the live half: *"a verb the endpoint refuses names the
  request and the allowed verbs"* (`DELETE /api/expenses` → 405 with the JSON
  `method_not_allowed` shape and `Allow: GET, POST`).
- `tests/contract/auth.rs::a_wrong_method_answers_the_error_shape_and_the_allow_header`
  — the server's half of the contract (shape, `Allow`, a templated path).

## Real-browser gate (`browser-smoke.py`) — the three shipped defects

Three user-visible defects reached production through a fully green CI because
every other GUI guard runs in jsdom: a cascade regression that painted every
closed dialog on load (#296), and an ES-module graph whose specifiers resolved
to URLs that do not exist, so the browser refused every module on a non-JS MIME
type (#297).

[`browser-smoke.py`](./browser-smoke.py) closes that gap. It is a **standard library
only** Python 3 script (`socket`, `http.client`, `subprocess`, `json`, `base64`)
that speaks Firefox's **Marionette** wire protocol directly — no geckodriver, no
Playwright/Puppeteer, no npm package, no Rust crate.

```sh
cargo build --locked --release            # or: the CI container build step
python3 gui-test/browser-smoke.py         # embedded/production assets
python3 gui-test/browser-smoke.py --dev-web-dir   # TUCANO_WEB_DIR=./web + live-reload probe
```

It boots the release binary on a **free ephemeral port** with a **fresh `mktemp -d`
data dir** (never the developer's `data/`), kills the server *and* Firefox on every
exit path and prints a `PASS`/`FAIL` line per assertion. Exit codes: `0` all
passed, `1` assertion failure(s), `2` the gate could not run (no Firefox/binary,
protocol error). It asserts:

1. the shell loads: HTTP 200 + `text/html`, `readyState === 'complete'`, title;
2. **no console message at all** — a live `nsIConsoleService` listener installed
   from the parent process *before* the first navigation, so a boot exception or a
   CSP violation cannot slip through a race. The only exclusion is
   `resource://gre/`-sourced Firefox-internal noise (a JSWindowActor teardown race
   that any fast programmatic navigation of a form-heavy page can trigger);
3. the module graph **executed** in the browser: every specifier fetched against
   its declaring module's URL (200 + JS MIME, the #297 class) *and* the app booted
   (the auth overlay, hidden in the markup, is revealed only by a module);
4. the **CSS cascade is real**: every closed `<dialog>` computes `display: none`,
   a deliberately opened one is laid out, and both `/styles.css` and
   `/vendor/ubuntu/components.css` loaded with rules applied;
5. light/dark: toggling the app's own `data-theme` control changes the computed
   background and foreground;
6. an **authenticated walk** — a throwaway admin via `/api/auth/bootstrap`, sign-in
   through the real form, then every route in `TAB_ROUTES` (`/timesheets/day…week…
   calendar`, `/customers`, `/projects`, `/tasks`, `/invoices`, `/expenses`,
   `/approvals`, `/reports`, `/settings/*`), asserting per view that its tab is
   selected, its panel is not hidden **and has a layout box**, its primary region
   is rendered, and no console error appeared.

`--dev-web-dir` additionally proves the dev live-reload contract: the reload client
is an external `/_dev/reload.js` asset (never an inline script the app's own
`script-src 'self'` would block), served with a JS MIME type, and an edit under
`web/` reloads the browser by itself.

It is wired into the `gui-test` job in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)
as a host step (the pinned containers carry no browser). A preflight step fails —
never skips — when Firefox or `python3` is missing, and the job runs the gate
twice: embedded/production assets, then the `--dev-web-dir` dev-mode probe.

## Visual evidence (screenshots) — #140/#142/#133/#141

The suite itself never compares pixels (deliberately OS-fragile); when a
review needs visual evidence, capture it manually against a local server:

```sh
cargo build
TUCANO_DATA_DIR=/tmp/tt-shots TUCANO_PORT=8080 ./target/debug/tucano-time
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
