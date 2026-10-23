# syntax=docker/dockerfile:1.7

# ---------------------------------------------------------------------------
# Builder: the exact toolchain rust-toolchain.toml pins, so the image is built
# by the same compiler CI and developers use. Pinned by tag *and* digest: the
# digest is what makes the build reproducible, the tag keeps it readable.
# ---------------------------------------------------------------------------
FROM rust:1.98.0-slim-trixie@sha256:17d1ba895198f9934c6314ec5346a0d5115372f3243390c3d731e242f35c2f27 AS builder

WORKDIR /build
ENV CARGO_TERM_COLOR=never \
  CARGO_TARGET_DIR=/build/target

# Build identity for /healthz and the GUI footer. The build context carries no
# `.git`, so the commit cannot be read here: CI's image job passes it as a build
# arg, and a bare `docker build` reports `unknown` rather than inventing one.
ARG TUCANO_BUILD=unknown
ENV TUCANO_BUILD=${TUCANO_BUILD}

# Dependency layer (E6): compile the ~136 crates from the manifest + lockfile
# alone, against empty stub sources. Nothing in this layer depends on `src/`,
# `web/` or `openapi.json`, so a code or asset edit never recompiles a
# dependency. The registry and target dir live in BuildKit cache mounts, so
# repeated builds reuse the compiled artefacts even when the layers above
# them are invalidated.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
  --mount=type=cache,target=/build/target,sharing=locked \
  mkdir -p src /build/empty-data \
  && printf 'fn main() {}\n' > src/main.rs \
  && : > src/lib.rs \
  && cargo build --release --locked

# Application layer: the real sources, then the assets. `web/` and
# `openapi.json` are embedded at compile time (rust-embed + include_str!), so
# the release binary is the entire application — and a GUI-only edit only
# rebuilds this last step.
RUN rm -rf src
COPY build.rs ./build.rs
COPY src ./src
COPY web ./web
COPY openapi.json ./
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
  --mount=type=cache,target=/build/target,sharing=locked \
  # `COPY` preserves the host mtimes of `src/`, which can be *older* than the
  # fingerprints the stub build left in the cache mount — cargo would then
  # call the crate fresh and ship the stub binary. Dropping the local
  # package's artefacts forces a real rebuild while every dependency stays
  # compiled (that is the whole point of the layer above).
  cargo clean --release --package tucano-time \
  && cargo build --release --locked \
  && cp target/release/tucano-time /build/tucano-time

# ---------------------------------------------------------------------------
# Runtime: distroless cc-debian13 — glibc + libgcc + the CA bundle, no shell,
# no package manager, no apt state. The Debian 13 userland matches the builder's
# glibc (a Debian 12 base would be older than the libc the binary links).
# Pinned by digest for reproducibility.
#
# Harden at runtime (compose does this by default):
#   --read-only --cap-drop ALL --security-opt no-new-privileges --tmpfs /tmp
# ---------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2

LABEL org.opencontainers.image.title="TucanoTime" \
  org.opencontainers.image.description="TucanoTime timesheet server (API, GUI and contract in one binary)" \
  org.opencontainers.image.vendor="Tucano Technology" \
  org.opencontainers.image.licenses="AGPL-3.0-or-later" \
  org.opencontainers.image.url="https://github.com/TucanoTechnology/TucanoTime" \
  org.opencontainers.image.source="https://github.com/TucanoTechnology/TucanoTime" \
  org.opencontainers.image.documentation="https://github.com/TucanoTechnology/TucanoTime#README" \
  org.opencontainers.image.base.name="gcr.io/distroless/cc-debian13:nonroot"

# Fixed uid/gid so bind mounts and volumes have a stable owner. No passwd entry
# is needed: the image runs with a numeric user id.
COPY --from=builder --chown=10001:10001 /build/empty-data /data
COPY --from=builder --chown=10001:10001 /build/empty-data /Backup
COPY --from=builder --chown=10001:10001 /build/tucano-time /usr/local/bin/tucano-time

USER 10001:10001
ENV TUCANO_DATA_DIR=/data \
  TUCANO_PORT=8080
EXPOSE 8080
# `/Backup` is where Settings → Backup writes its ZIPs (sibling of the data
# dir). It is a volume so `--read-only` still allows backups; the operator can
# mount a host directory there instead.
VOLUME ["/data", "/Backup"]

# Graceful shutdown on `docker stop` (the server installs its own SIGTERM
# handler and finishes in-flight writes).
STOPSIGNAL SIGTERM

# Container liveness (#94): the same binary probes its own /healthz, so the
# runtime image needs no curl/wget — and distroless has no shell to need one.
HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/tucano-time", "--health"]

ENTRYPOINT ["/usr/local/bin/tucano-time"]
