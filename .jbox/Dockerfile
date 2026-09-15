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

ENV PATH=/usr/local/cargo/bin:${PATH} \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/home/jbox/.cargo
USER jbox
