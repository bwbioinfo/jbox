ARG JBOX_BASE_IMAGE
FROM ${JBOX_BASE_IMAGE}

# The default image supplies Rust/Cargo/rustfmt/Clippy, native build tools,
# Python/pip/venv/uv, Node/npm/pnpm, ripgrep, jq, sqlite3, rsync, ShellCheck,
# rclone and all 13 ArtCraft CLIs.
# rust-toolchain.toml still selects this repository's pinned compiler through
# rustup. Add only project-specific packages or build steps here.
