# Rust toolchain for developing jbox itself. The runtime base keeps jcode and
# the guest SSH daemon; the Rust stage supplies a current toolchain compatible
# with edition 2024.
FROM rust:1.98-bookworm AS rust

FROM jbox/jcode:local-v4-1001-1001

USER root
COPY --from=rust /usr/local/cargo /usr/local/cargo
COPY --from=rust /usr/local/rustup /usr/local/rustup
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*
# sshd supplies a conservative PATH for remote commands. Keep small wrappers
# in /usr/local/bin so shells and jcode tool processes find the Rust toolchain
# even when Docker's image ENV is not retained by the SSH login path.
RUN for tool in cargo rustc rustdoc rustup; do \
        printf '%s\n' '#!/bin/sh' 'export RUSTUP_HOME=/usr/local/rustup' \
            "exec /usr/local/cargo/bin/$tool \"\$@\"" > "/usr/local/bin/$tool"; \
        chmod 0755 "/usr/local/bin/$tool"; \
    done

ENV PATH=/usr/local/cargo/bin:${PATH} \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/home/jbox/.cargo
