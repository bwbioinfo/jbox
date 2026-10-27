# Reviewed stable upstream releases as of 2026-10-24. Keep Debian suites aligned.
# Pin versions so a Jbox upgrade invalidates the content-addressed image cache.
FROM rust:1.99.1-slim-bookworm AS rust
RUN rustup component add rustfmt clippy rust-analyzer

FROM node:26.12.1-bookworm-slim AS node
# Approve only pnpm's lifecycle scripts, which install its native binary.
RUN npm install --global --prefix /usr/local npm@11.21.1 pnpm@12.15.0 --allow-scripts=pnpm --no-audit --no-fund \
    && npm --version && pnpm --version \
    && npm cache clean --force
# Frontend CLIs are convenience defaults. Applications still install their own
# dependencies and lockfile-selected versions locally.
RUN npm install --global --prefix /usr/local --ignore-scripts --no-audit --no-fund \
        typescript@7.0.2 vite@8.3.5 eslint@10.13.0 react@19.3.1 react-dom@19.3.1 @playwright/test@1.64.1 \
    && npm cache clean --force

FROM ghcr.io/astral-sh/uv:0.13.5 AS uv
FROM rclone/rclone:1.75.2 AS rclone

# The official Python image includes pip, venv, headers, and runtime libraries.
FROM python:3.15.1-slim-bookworm
ARG JBOX_UID=1000
ARG JBOX_GID=1000

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bash \
        build-essential \
        ca-certificates \
        cmake \
        curl \
        file \
        fontconfig \
        fonts-dejavu-core \
        fonts-liberation \
        git \
        gzip \
        jq \
        less \
        libssl-dev \
        ninja-build \
        openssh-client \
        openssh-server \
        pkg-config \
        procps \
        ripgrep \
        rsync \
        shellcheck \
        sqlite3 \
        tar \
        unzip \
        xz-utils \
        zip \
    && rm -rf /var/lib/apt/lists/*

COPY --from=rust /usr/local/cargo /usr/local/cargo
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
COPY --from=uv /uv /uvx /usr/local/bin/
# Copy only the static CLI, not rclone's image config or FUSE setup.
COPY --from=rclone /usr/local/bin/rclone /usr/local/bin/rclone
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
    && ln -s ../lib/node_modules/pnpm/pnpm /usr/local/bin/pnpm \
    && ln -s ../lib/node_modules/pnpm/pnpx /usr/local/bin/pnpx \
    && ln -s ../lib/node_modules/typescript/bin/tsc /usr/local/bin/tsc \
    && ln -s ../lib/node_modules/vite/bin/vite.js /usr/local/bin/vite \
    && ln -s ../lib/node_modules/eslint/bin/eslint.js /usr/local/bin/eslint \
    && ln -s ../lib/node_modules/@playwright/test/cli.js /usr/local/bin/playwright

# Browser revisions are tied to Playwright's version and shared read-only with
# the guest. Project-local Playwright versions may require other revisions.
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
RUN playwright install --with-deps chromium firefox webkit \
    && chmod -R a+rX /opt/ms-playwright \
    && rm -rf /var/lib/apt/lists/*

# SSH remote commands do not reliably retain Docker ENV or read shell profiles.
# Wrappers keep rustup's toolchains discoverable while Cargo caches stay per-user.
RUN for tool in cargo rustc rustdoc rustup rustfmt cargo-fmt cargo-clippy clippy-driver rust-analyzer; do \
        printf '%s\n' '#!/bin/sh' \
            'export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"' \
            "exec /usr/local/cargo/bin/$tool \"\$@\"" > "/usr/local/bin/$tool"; \
        chmod 0755 "/usr/local/bin/$tool"; \
    done

RUN mkdir -p -m 0755 /etc/apt/keyrings \
    && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && echo 'deb [arch=amd64 signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main' > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/*
# Pin Beads instead of executing a floating installer with source-build fallbacks.
# Preserve upstream license and release notes alongside the verified binary.
RUN curl -fsSL --retry 3 https://github.com/gastownhall/beads/releases/download/v1.3.2/beads_1.3.2_linux_amd64.tar.gz -o /tmp/beads.tar.gz \
    && echo 'c6ea3281f6ff82628351b2d0967c9eb85862c8a0ec0ebc24e63e645c8b02462e  /tmp/beads.tar.gz' | sha256sum -c - \
    && mkdir -p /opt/beads \
    && tar -xzf /tmp/beads.tar.gz -C /opt/beads bd LICENSE README.md CHANGELOG.md \
    && ln -s /opt/beads/bd /usr/local/bin/bd \
    && rm -f /tmp/beads.tar.gz \
    && bd version

# Official Linux x86_64 release, reviewed 2026-10-24. Keep the complete bundle
# together so Quarto can discover its bundled Pandoc, Deno, and resources.
RUN curl -fsSL --retry 3 https://github.com/quarto-dev/quarto-cli/releases/download/v1.10.19/quarto-1.10.19-linux-amd64.tar.gz -o /tmp/quarto.tar.gz \
    && echo '8e38c2b2df6a6dbd369ea16e15acded6894545258d13a02f777f81ad18fd7973  /tmp/quarto.tar.gz' | sha256sum -c - \
    && mkdir -p /opt/quarto \
    && tar -xzf /tmp/quarto.tar.gz -C /opt/quarto --strip-components=1 \
    && ln -s /opt/quarto/bin/quarto /usr/local/bin/quarto \
    && rm -f /tmp/quarto.tar.gz

COPY install-artcraft.py artcraft-releases.json /usr/local/lib/jbox/
RUN python /usr/local/lib/jbox/install-artcraft.py --manifest /usr/local/lib/jbox/artcraft-releases.json

# Never inherit a repository's host-managed Dolt server settings in a guest.
ENV BEADS_DOLT_SERVER_MODE=embedded BEADS_DOLT_AUTO_START=true
ENV RUSTUP_HOME=/home/jbox/.rustup \
    CARGO_HOME=/home/jbox/.cargo \
    NPM_CONFIG_PREFIX=/home/jbox/.local \
    PNPM_HOME=/home/jbox/.local/bin \
    PATH=/home/jbox/.cargo/bin:/home/jbox/.local/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin
RUN groupadd --gid "$JBOX_GID" jbox \
    && useradd --uid "$JBOX_UID" --gid "$JBOX_GID" -m -s /bin/bash jbox \
    && mkdir -p /run/sshd /home/jbox/.jcode /home/jbox/.ssh /home/jbox/.cargo /home/jbox/.local/bin /usr/local/etc \
    && printf '%s\n' 'prefix=${HOME}/.local' > /usr/local/etc/npmrc \
    && chmod -R go-w /usr/local/cargo /usr/local/rustup \
    && chown -R jbox:jbox /home/jbox

COPY jcode /usr/local/bin/jcode
COPY jcode-linux-x86_64.bin /usr/local/bin/jcode-linux-x86_64.bin
COPY jbox-entrypoint /usr/local/bin/jbox-entrypoint
COPY jbox-beads-pre-commit /usr/local/bin/jbox-beads-pre-commit
COPY jbox-toolchain-init /usr/local/bin/jbox-toolchain-init
RUN chmod 0755 /usr/local/bin/jcode /usr/local/bin/jcode-linux-x86_64.bin /usr/local/bin/jbox-entrypoint /usr/local/bin/jbox-beads-pre-commit /usr/local/bin/jbox-toolchain-init \
    && /usr/local/bin/jcode --no-update --version \
    && printf '%s\n' 'Port 2222' 'PasswordAuthentication no' 'PermitRootLogin no' 'AllowUsers jbox' 'AuthorizedKeysFile .ssh/authorized_keys' \
        'SetEnv PATH=/home/jbox/.cargo/bin:/home/jbox/.local/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin RUSTUP_HOME=/home/jbox/.rustup CARGO_HOME=/home/jbox/.cargo NPM_CONFIG_PREFIX=/home/jbox/.local PNPM_HOME=/home/jbox/.local/bin PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright' \
        > /etc/ssh/sshd_config.d/jbox.conf

# Fail image builds early if the non-root guest cannot use the toolchains.
RUN su -s /bin/sh jbox -c 'set -eu; jbox-toolchain-init; rustc --version; cargo --version; cargo fmt --version; cargo clippy --version; rust-analyzer --version; python --version; python -m pip --version; python -c "import ssl, sqlite3, venv"; node --version; npm --version; npx --version; rg --version; jq --version; cmake --version; ninja --version; test -w "$RUSTUP_HOME"; test -w "$CARGO_HOME"; test -w "$NPM_CONFIG_PREFIX"'
RUN su -s /bin/sh jbox -c 'set -eu; uv --version; uvx --version; pnpm --version; sqlite3 :memory: "SELECT 1;"; rsync --version; shellcheck --version; rclone version; bd version; test -w "$PNPM_HOME"'
RUN su -s /bin/sh jbox -c 'set -eu; tsc --version; vite --version; eslint --version; playwright --version; node -e "const p = require(\"/usr/local/lib/node_modules/@playwright/test\"); for (const b of [p.chromium, p.firefox, p.webkit]) require(\"fs\").accessSync(b.executablePath(), require(\"fs\").constants.X_OK); require(\"/usr/local/lib/node_modules/react\"); require(\"/usr/local/lib/node_modules/react-dom\");"'
# Exercise bundled rendering resources, not just the launcher, without an
# execution engine, browser, TeX installation, or network access.
RUN su -s /bin/sh jbox -c 'set -eu; quarto --version; work=$(mktemp -d); trap '\''rm -rf "$work"'\'' EXIT; printf "# Jbox Quarto smoke test\n" > "$work/smoke.qmd"; quarto render "$work/smoke.qmd" --to html --no-execute; test -s "$work/smoke.html"'
# Repeat startup and OCR discovery checks with the final non-root guest identity.
RUN su -s /bin/sh jbox -c 'python /usr/local/lib/jbox/install-artcraft.py --manifest /usr/local/lib/jbox/artcraft-releases.json --verify-only'
EXPOSE 2222
