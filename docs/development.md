# Development

## Toolchain and local workflow

The toolchain is pinned by [`rust-toolchain.toml`](../rust-toolchain.toml):
Rust `1.99.1` with `rustfmt` and `clippy`, edition 2024. The Dockerfile and CI
images carry the same version; bump all three together in one intentional
change.

```sh
cargo run
```

This serves the GUI and API at <http://localhost:8080>, with data in `./Data`.
Overrides for a local run:

```sh
TATU_DATA_DIR=$(mktemp -d) TATU_PORT=8099 cargo run
```

Useful checks before opening a pull request:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

The GUI and the OpenAPI contract are compiled into the binary. `web/` is
dependency-free vanilla HTML/CSS/JS embedded with rust-embed; there is no GUI
build step. The API contract is [`openapi.json`](../openapi.json), served at
`/openapi.json` and rendered at `/docs`; route and contract changes belong in the
same change.

## GUI live reload

To serve GUI assets from disk on every request, so HTML/CSS/JS edits need no
Rust rebuild:

```sh
TATU_WEB_DIR=./web cargo run
```

When `TATU_WEB_DIR` is set, static files are read from that directory on every
request. A small script is injected into HTML pages that opens an SSE connection
to `/_dev/reload`; the server polls `web/` every 300 ms and reloads the browser
when a file's modification time changes. Without `TATU_WEB_DIR`, the embedded
bundle is served and `/_dev/reload` returns 404.

A running server with `TATU_WEB_DIR` can therefore serve a newer page than the
contract compiled into its binary. Restart the server after changing Rust code
or `openapi.json`.

## Repository layout

| Path | Contents |
| --- | --- |
| `src/main.rs` | Process entry point, operator CLI (`--backup`, `--restore`, `--health`, `--version`) and shutdown handling |
| `src/lib.rs` | Library surface, router construction, `API_ROUTES` and `SPA_ROUTES` |
| `src/api/` | HTTP handlers, validation, pagination and the admin surface |
| `src/domain/` | Entities and pure rules: entries, invoices, expenses, customers, claims, catalog |
| `src/store/` | File-based storage, indexes, cold tree, transactions and lock snapshot |
| `src/pdf/`, `src/template.rs` | PDF rendering and the `%variable%` invoice template |
| `web/` | Vanilla GUI assets, including the vendored Ubuntu/Vanilla bundle |
| `tests/contract/` | In-process HTTP contract suite |
| `gui-test/` | jsdom harness, live smoke flow and real-browser gate |
| `docs/` | Human documentation and architecture decision records |
| `deploy/unraid/` | Unraid template and runbook |
| `openapi.json` | The API contract |
| `Dockerfile`, `docker-compose.yml` | Container image and hardened stack |

## Code style

The Rust style bar is recorded in [Code style](coding-style.md): idiomatic,
example-led Rust, short module and item documentation, comments that state the
present contract, and a single source of truth per rule.

The organisation-wide engineering rules live under
[`AgentRules/`](../AgentRules/README.md). They apply to changes in this
repository; the pages here do not duplicate them.

## Contributing and versioning

- The repository is **`main`-only**. Branch from `main`, open a pull request
  against `main`, and merge back into `main`. Dependabot also targets `main`.
- **Bump the patch version in `Cargo.toml` in the change that lands.** The crate
  version is the release version.
- [`.github/workflows/release.yml`](../.github/workflows/release.yml) runs on
  every push to `main`, tags the merged commit `v<version>` and creates the
  GitHub Release with generated notes. If that version is already tagged on an
  earlier commit, the commit is released as `v<version>+<short-sha>` instead;
  tags and releases are never moved or rewritten.
- Behaviour changes ship with contract tests. The shared test policies are in
  [`AgentRules/test/`](../AgentRules/test/).
- Container changes keep the hardening declared in
  [`docker-compose.yml`](../docker-compose.yml) and the
  [`Dockerfile`](../Dockerfile): read-only root, dropped capabilities,
  `no-new-privileges` and uid/gid 10001.

## Licence

TatuTime is licensed under the
[GNU Affero General Public License v3.0 or later](../LICENSE)
(`AGPL-3.0-or-later`).

---

Last verified against TatuTime 0.1.41.
