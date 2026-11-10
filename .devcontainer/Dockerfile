FROM node:24-trixie-slim AS node

FROM rust:1.99.1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
    ca-certificates curl firefox-esr git openssh-client python3 sudo \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add rustfmt clippy \
    && useradd --create-home --shell /bin/bash --uid 1000 vscode \
    && printf 'vscode ALL=(root) NOPASSWD:ALL\n' > /etc/sudoers.d/vscode \
    && chmod 0440 /etc/sudoers.d/vscode \
    && chmod -R a+rwX /usr/local/cargo /usr/local/rustup

COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s /usr/local/lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx

ENV TATU_FIREFOX=/usr/bin/firefox-esr
USER vscode