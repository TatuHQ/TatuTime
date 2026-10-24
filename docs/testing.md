# Testing

TatuTime has four layers of checks: Rust unit and contract tests, an offline
GUI suite in jsdom, a real-browser smoke gate, and the CI jobs around them.
Run the fast Rust gates on every change and the GUI gates before touching
`web/`.

## Rust tests

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

- **Unit tests** live beside the code in `src/` and cover domain rules, money
  and hours arithmetic, locking, vault and session behaviour, parsing and
  storage helpers.
- **Contract tests** live in [`tests/contract/`](../tests/contract/main.rs).
  They drive the real router in-process against a temporary data directory and
  diff the route surface and payloads against
  [`openapi.json`](../openapi.json). The suite covers valid and invalid shapes,
  rejection before persistence, the documented error shape, RBAC, lock
  refusals, invoice lifecycle, vault and configuration endpoints, email, SSO,
  accounting and scheduler behaviour.
- `cargo test --all-targets` runs both groups. CI uses
  `cargo test --locked --all-targets`, so the committed `Cargo.lock` must be the
  resolver's own output.

## GUI suite (jsdom and live server)

The jsdom harness lives in [`gui-test/`](../gui-test/README.md) and loads
`web/index.html`, concatenating the ES modules in `web/js/` (see
[`gui-test/load-app.mjs`](../gui-test/load-app.mjs)); `web/app.js` is the browser
entry point. The live smoke flow drives a running server.

```sh
cargo build --release
TATU_DATA_DIR=$(mktemp -d) TATU_PORT=8099 ./target/release/tatu-time &
cd gui-test
npm ci
node contrast.mjs
npm run test:jsdom
node gui.mjs
```

- `node contrast.mjs` checks light/dark contrast tokens.
- `npm run test:jsdom` runs every `*.test.mjs` file offline: routing, forms,
  dialogs, accessibility, pagination, settings, theme, component contracts and
  condition waits.
- `node gui.mjs` (also `npm test`) is the live smoke flow against
  `http://localhost:8099`.
- `npm run build:ubuntu` regenerates the vendored Vanilla assets; it is not
  needed for a normal test run.

Assertions wait for conditions, never for a fixed duration.
[`gui-test/README.md`](../gui-test/README.md) explains the harness, the
condition-wait helpers and the deliberate coverage gaps of jsdom.

## Real-browser gate

The jsdom suite cannot see the CSS cascade, module URL resolution or console
errors. [`gui-test/browser-smoke.py`](../gui-test/browser-smoke.py) closes that
gap with a real Firefox driven through Marionette; it uses only the Python
standard library.

```sh
cargo build --locked --release
python3 gui-test/browser-smoke.py
python3 gui-test/browser-smoke.py --dev-web-dir
```

It boots the release binary on a free port with a fresh temporary data
directory, signs in as a throwaway admin and walks every GUI route. It asserts
that the page loads, that no console message appears, that every module resolves
with a JavaScript MIME type, that closed dialogs are not laid out, that both
themes render and that every view has a layout box. `--dev-web-dir` additionally
proves the `TATU_WEB_DIR` live-reload path. Firefox must be on `PATH`; set
`TATU_FIREFOX` to point at a specific binary. Exit codes: `0` pass, `1`
assertion failure, `2` the gate could not run.

## Performance

There is no separate benchmark crate or timing test in this repository. The
performance-sensitive paths are bounded by design and covered structurally:

- date-range queries are capped at 400 days;
- list endpoints use keyset pagination with a bounded page size;
- per-collection writes are capped by `max_docs`;
- reads use side indexes instead of re-scanning every document;
- the lock providers answer from one memoised snapshot.

Any new performance test follows the shared rules in
[`AgentRules/test/performance.md`](../AgentRules/test/performance.md): one
measured characteristic, a threshold set before the run, a stable environment and
a recorded baseline.

## CI gates

The workflow is [`.github/workflows/ci.yml`](../.github/workflows/ci.yml).

| Job | What it checks |
| --- | --- |
| `lint-test` | `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked --all-targets` in the pinned Rust image |
| `build-image` | Builds the container on pull requests and publishes to GHCR on pushes to `main` |
| `actionlint` | Lints the GitHub Actions workflows |
| `gui-test` | Builds the release binary, runs the real-browser gate twice (embedded and `--dev-web-dir`), then the jsdom/contrast/live suites |
| `secret-scan` | Runs gitleaks over the full git history |
| `supply-chain` | Runs `cargo deny check` and generates a best-effort CycloneDX SBOM |

The shared testing rules are references, not duplicated here:
[`AgentRules/test/unit.md`](../AgentRules/test/unit.md),
[`AgentRules/test/contract.md`](../AgentRules/test/contract.md),
[`AgentRules/test/ui.md`](../AgentRules/test/ui.md) and
[`AgentRules/test/performance.md`](../AgentRules/test/performance.md).

---

Last verified against TatuTime 0.1.41.
