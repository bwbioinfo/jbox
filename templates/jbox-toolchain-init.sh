#!/bin/sh
# Run as jbox after its tmpfs home is mounted. Never write to the read-only rootfs.
set -eu
seed="${1:-/usr/local/rustup}"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
mkdir -p "$CARGO_HOME/bin" "$HOME/.local/bin"
if [ ! -d "$seed/toolchains" ]; then
    # A project may deliberately replace the default image/toolchain.
    exit 0
fi
mkdir -p "$RUSTUP_HOME/toolchains"
# Preserve user-selected defaults and separately installed toolchains on reentry.
if [ ! -e "$RUSTUP_HOME/settings.toml" ]; then
    cp -n "$seed/settings.toml" "$RUSTUP_HOME/settings.toml"
fi
for toolchain in "$seed"/toolchains/*; do
    [ -d "$toolchain" ] || continue
    name="${toolchain##*/}"
    target="$RUSTUP_HOME/toolchains/$name"
    if [ ! -e "$target" ] && [ ! -L "$target" ]; then
        ln -s "$toolchain" "$target"
    fi
done
# Use a separately installed user toolchain for new targets/components. The
# baked toolchain stays immutable without copying hundreds of MB into tmpfs.
