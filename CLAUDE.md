# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

## First-run Beads safety

In a fresh clone, run `bd bootstrap --yes` **before** `bd prime` or any other
Beads command. Bootstrap non-destructively imports the tracked
`.beads/issues.jsonl` when no local database exists, and validates an existing
database without overwriting it. Then run `bd prime` for full workflow context.

If an existing local database is missing issues that are present in the tracked
JSONL export, recover it from the repository root with:

```bash
bd import --dry-run
bd import
bd stats
```

`bd import` upserts the tracked export and does not delete database-only issues.
Do not use `bd init --force` as a recovery command.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->


## Build & Test

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run -- --help
```

`rust-toolchain.toml` pins the compiler and supplies the `rustfmt` and Clippy
components required by these checks.

## Architecture Overview

- `src/main.rs` parses and dispatches the CLI.
- `src/lib.rs` orchestrates session lifecycle, Git acceptance, synchronization,
  and recovery flows.
- `src/config.rs`, `src/paths.rs`, and `src/state.rs` validate project policy,
  restrict host data, and store session metadata.
- `src/git.rs`, `src/image.rs`, and `src/engine.rs` isolate worktrees, build
  images, and invoke Docker with the Kata runtime.

## Conventions & Patterns

Keep host-changing operations explicit and non-interactive. Preserve isolation
invariants, and add focused unit tests for lifecycle or Git behavior changes.
