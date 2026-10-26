# Dependency and tooling review — TatuTime (2026-10-26)

> *Historical snapshot — not current guidance. See the reviews index.*

Branch: `chore/deps-refresh-2026-10` (from `main` @ `56b8d87`). Scope: every
version this repository pins or references — the Rust toolchain, the Cargo
manifest and lockfile, the npm harness, the CI images/actions/tools, and the
container base images.

**Method:** every "latest" below was read from the ecosystem's own source on
2026-10-26, not from memory:

| Ecosystem | Source |
| --- | --- |
| Rust stable | `https://static.rust-lang.org/dist/channel-rust-stable.toml`, `rust-lang/rust` releases API |
| Crates | `https://index.crates.io` for all 239 locked packages; `https://crates.io/api/v1/crates/<name>` for direct dependencies |
| npm | `https://registry.npmjs.org/<package>/latest`, `https://nodejs.org/dist/index.json` |
| Container images | Docker Hub tag API for `rust`/`node`, `docker manifest inspect` for tag digests |
| GitHub Actions/tools | `api.github.com` releases and `git/ref/tags` (the pinned SHA is resolved to its tag) |

## 1. Rust toolchain

| Item | Was | Latest stable (2026-10-26) | Action |
| --- | --- | --- | --- |
| Rust channel | 1.98.0 | **1.99.1** (released 2026-10-15) | bumped |
| `rust-version` (MSRV) | 1.98 | — | aligned to 1.99 |
| Builder image | `rust:1.98.0-slim-trixie` | `rust:1.99.1-slim-trixie` (digest `1a9397…`) | bumped, digest refreshed |
| CI `container:` / `run:` images | `rust:1.98.0-slim-trixie` | same as builder | bumped in all four places |
| Runtime base | `cc-debian13:nonroot@83b9b50…` | same tag, current digest `3ee0029…` | digest refreshed |

The three pins recorded by `AGENTS.md` (toolchain file, Dockerfile, CI) moved
together, as did the documentation that names the version.

## 2. Cargo dependencies

All 27 direct dependencies (and four dev-dependencies) were checked against
crates.io's `max_stable_version`. **25 of the 27 were already current.** Two
patch releases were behind:

| Crate | Was | Latest stable | Action |
| --- | --- | --- | --- |
| `uuid` | 1.28.0 | 1.29.0 | bumped in `Cargo.toml` |
| `zip` | 9.0.5 | 9.1.2 | bumped in `Cargo.toml` |

`cargo update` also picked up the compatible transitive releases the lockfile
had missed: `cc` 1.9.0→1.10.0, `ctutils` 0.4.5→0.4.6, `smallvec`
1.16.3→1.16.4, `webpki-roots` 1.0.10→1.0.11, and `typed-path`
0.12.4→0.13.0 (required by `zip` 9.1.2). Nothing else changed: `serde`,
`serde_json`, `axum`, `tokio` (1.54.1), `tower-http` 0.7.1, `tracing`,
`tracing-subscriber`, `chrono`, `thiserror`, `rust-embed`, `flate2`, `argon2`,
`hmac`, `sha2`, `base64`, `rand`, `aes-gcm`, `ureq`, `pdf-writer`, `skrifa`,
`futures-util`, `lettre`, `lopdf`, `csv`, `tower`, `http-body-util` and
`tempfile` were already at their latest stable release.

**Deliberately not changed:** the transitive crates whose upstream owners pin an
older major (`matchit` 0.8 from axum 0.8.9, `parking_lot` 0.12 from tokio,
`windows-*` 0.52–0.62, `redox_syscall` 0.5, `brotli-decompressor` 5,
`alloc-*` 2/0.2). Forcing a newer major here would mean patching a dependency's
dependency, which the dependency rules do not allow; they will move when axum,
tokio or ureq adopt them.

Also noted, not actionable: `fs2` 0.4.3 is the newest release but dates from
2018. It is still the documented latest stable and no advisory covers it, so it
stays; replacing it needs a ticket first.

## 3. Container and CI tooling

| Item | Was | Latest | Action |
| --- | --- | --- | --- |
| `node` image (GUI harness) | `node:22-slim` | **`node:24-slim`** (Node 24.22.1, the current LTS "Krypton") | bumped in CI, dependabot note and `gui-test/README.md` |
| `docker/login-action` | v4.6.0 (`dbcb813…`) | **v4.7.0** (`81c6f93…`) | SHA bump |
| `actions/checkout` | v7.0.1 (`3d3c42e…`) | v7.0.1 | current |
| `docker/metadata-action` | v6.2.0 (`dc80280…`) | v6.2.0 | current |
| `docker/build-push-action` | v7.4.0 (`c3c9e26…`) | v7.4.0 | current |
| `actions/upload-artifact` | v7.0.2 (`cf430e0…`) | v7.0.2 | current |
| `actionlint` | 1.7.12 | 1.7.12 | current |
| `gitleaks` | 8.30.1 | 8.30.1 | current |
| `cargo-deny` | 0.20.2 | 0.20.2 | current |
| `cargo-cyclonedx` | 0.5.9 | 0.5.9 | current |

The host-provided tools the workflows preflight (`curl`, `jq`, `git`,
`firefox`, `python3`) stay unpinned by design: the jobs fail loudly when one is
missing rather than running against a version nobody recorded.

## 4. npm harness

`jsdom` 30.1.3, `css-tree` 3.2.1 and `pdfjs-dist` 6.4.299 were current.
`sass` moved 1.105.2 → **1.106.1**.

`vanilla-framework` 4.59.0 has 4.61.0 available but stays pinned: dependabot is
configured to ignore it on purpose, because moving it is a vendor rebuild
(`gui-test/build-ubuntu-assets.mjs` regenerates `web/vendor/ubuntu/` together
with the manifest hashes) and needs a human review of the resulting CSS, not an
automated bump. That remains an open, deliberate hold rather than an oversight.

The Sass bump was proven not to change the vendor build: compiling the pinned
`ubuntu-components.scss` with 1.105.2 and 1.106.1 produced byte-identical CSS,
so no asset rebuild was needed here.

## 5. Upgrade order applied

1. **Toolchain first** — `rust-toolchain.toml`, `Cargo.toml` MSRV, Dockerfile
   builder, CI images. A compiler bump can surface new lints, so it is verified
   before any dependency moves.
2. **Cargo dependencies** — direct pins, then `cargo update` for the lockfile.
3. **npm harness** — `sass`, lockfile regenerated with the new Node LTS.
4. **CI tooling** — Node image and `docker/login-action`.
5. **Runtime base digest** — distroless refresh.
6. **Documentation and version bump** — docs and the `0.2.1` crate bump.

## 6. Verification

| Gate | Result |
| --- | --- |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --all-targets -- -D warnings` | clean on 1.99.1 |
| `cargo test --locked --all-targets` | 299 passed |
| `cargo deny check advisories licenses sources bans` | all four pass |
| `npm ci` + `contrast.mjs` + 261 jsdom tests on Node 24 | pass |
| `node gui.mjs` against the release binary | all GUI checks passed |
| `docker build` with the new builder and runtime digests | built; the image served `/healthz` (`version: 0.2.1`) under the compose hardening flags |

Baseline before the upgrade was green (same 299 tests), so the pass above is a
like-for-like comparison rather than a first run.
