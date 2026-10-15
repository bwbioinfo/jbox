# Reviewed stable upstream releases as of 2026-10-15. Keep Debian suites aligned.
# Pin versions so a Jbox upgrade invalidates the content-addressed image cache.
FROM rust:1.99.0-slim-bookworm AS rust
RUN rustup component add rustfmt clippy rust-analyzer

FROM node:26.12.0-bookworm-slim AS node
# pnpm 12 installs a native binary through its package lifecycle scripts.
RUN npm install --global --prefix /usr/local pnpm@12.11.2 --no-audit --no-fund \
    && pnpm --version \
    && npm cache clean --force

FROM ghcr.io/astral-sh/uv:0.13.1 AS uv
FROM rclone/rclone:1.75.2 AS rclone

# The official Python image includes pip, venv, headers, and runtime libraries.
FROM python:3.15.0-slim-bookworm
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
    && ln -s ../lib/node_modules/pnpm/pnpx /usr/local/bin/pnpx

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
RUN curl -fsSL https://raw.githubusercontent.com/gastownhall/beads/main/scripts/install.sh | bash && bd version

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
    && printf '%s\n' 'Port 2222' 'PasswordAuthentication no' 'PermitRootLogin no' 'AllowUsers jbox' 'AuthorizedKeysFile .ssh/authorized_keys' \
        'SetEnv PATH=/home/jbox/.cargo/bin:/home/jbox/.local/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin RUSTUP_HOME=/home/jbox/.rustup CARGO_HOME=/home/jbox/.cargo NPM_CONFIG_PREFIX=/home/jbox/.local PNPM_HOME=/home/jbox/.local/bin' \
        > /etc/ssh/sshd_config.d/jbox.conf

# Fail image builds early if the non-root guest cannot use the toolchains.
RUN su -s /bin/sh jbox -c 'set -eu; jbox-toolchain-init; rustc --version; cargo --version; cargo fmt --version; cargo clippy --version; rust-analyzer --version; python --version; python -m pip --version; python -c "import ssl, sqlite3, venv"; node --version; npm --version; npx --version; rg --version; jq --version; cmake --version; ninja --version; test -w "$RUSTUP_HOME"; test -w "$CARGO_HOME"; test -w "$NPM_CONFIG_PREFIX"'
RUN su -s /bin/sh jbox -c 'set -eu; uv --version; uvx --version; pnpm --version; sqlite3 :memory: "SELECT 1;"; rsync --version; shellcheck --version; rclone version; test -w "$PNPM_HOME"'
# Some upstream CLIs intentionally return exit 1 for --help; --version is strict.
RUN su -s /bin/sh jbox -c 'set -eu; for tool in photocraft vectorcraft filmcraft lightcraft pdfcraft effectcraft designcraft figcraft wordcraft soundcraft deckcraft cadcraft gridcraft; do "$tool-cli" --version; done'
EXPOSE 2222
