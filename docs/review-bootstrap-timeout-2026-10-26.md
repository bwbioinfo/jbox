# Guest Beads bootstrap timeout review validation

Date: October 26, 2026. Review: `jbox-3mj`. Additional validation:
`jbox-4d1`. Open implementation follow-up: `jbox-rpd`.

## Confirmed diagnosis

The reported guest exit is caused immediately by the per-workspace bootstrap
hitting its 60-second bound. `src/image.rs:119` wraps initialization or import,
prefix validation, export, record-ID comparison, and hook installation in
`timeout -k 5s 60s`. Its failure branch exits before SSH starts. The independent
host readiness budget in `src/paths.rs:690` cannot extend that inner deadline.

The log reports all 544 issues committed, but that does not demonstrate that
initialization returned or that subsequent commands completed. The precise slow
command remains unidentified. `src/engine.rs:370-378` retrieves only the last
50 log lines and concatenates stdout before stderr, so the displayed combined
log is not a reliable chronological interleaving. Upstream and deprecated-port
warnings are not themselves the reported fatal error.

## Complete local quality gates

Slurm job **1467** completed with exit `0:0`:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run -- --help
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py' -v
```

All **187 Rust tests** and **30 Python tests** passed. These include the earlier
34 focused bootstrap and SSH-readiness tests, rather than being additional
independent copies of those checks.

## Additional executable boundary checks

An isolated harness extracts the existing shell bootstrap through its integrity
checks and substitutes temporary `su` and `bd` executables. All seven checks
passed in job **1468**:

1. A healthy fast bootstrap succeeds.
2. A slow export times out with exit 124 after an import-success message.
3. The same slow export succeeds with a larger budget.
4. An import command failure propagates its own exit code, not 124.
5. An export missing an expected record fails closed.
6. An unexpected prefix fails before import.
7. A failed prefix query propagates its exit code.

The input JSONL remained unchanged in each check. This is synthetic validation,
not evidence that export was the slow command in the reported guest.

## Real pinned Beads, synthetic 544-record dataset

Job **1470** ran the current hydration shell block against the existing image
`685d610a4e6c`, containing Beads **1.3.2** (`9b65b9e198ac`). The disposable
container had no host mounts, no network, no credentials, all capabilities
dropped, a read-only root, and an unprivileged UID. Its workspace and home were
private tmpfs directories. The `su` wrapper was replaced by `/bin/sh -c` because
the container was already unprivileged. The original 60-second per-workspace
bootstrap bound was retained.

Fresh initialization of 544 synthetic issues passed in approximately 13 seconds.
Retained-database rehydration then passed in approximately 10 seconds. Both
passes exported exactly 544 records, passed the bootstrap's ID comparison, and
installed a mode-0700 pre-commit hook. Total container runtime was 24.685 seconds.
The owned container was automatically removed after exit.

The first real check, in job 1468, completed hydration and exported all records,
but its post-check incorrectly expected `test -x` to succeed on the private
`noexec` tmpfs. That job exited 1. The repeat printed mount information confirming
`noexec` and checked the hook's existence and mode instead. This does not validate
executing a Git hook on an executable workspace mount.

The real check used ordinary Docker rather than the complete Kata/SSH launch,
and simple generated records rather than nanite's actual issues, dependencies,
memories, or metadata. Therefore **544 records alone do not reproduce the
reported failure**, and a longer timeout alone is not a proven repair. No
existing user guest or nanite database was changed.

## Recovery and follow-up constraints

A failed initial launch attempts cleanup before saving session state
(`src/lib.rs:548-553,586`), so its ID may not be resumable. A retained-session
startup failure preserves its worktrees (`src/lib.rs:2708-2712`), although the
bootstrap may already have changed guest-local Beads files. After an actual
image fix, retained sessions need explicit `--refresh-image`; simply rebuilding
the image does not replace their saved image selection. Fresh failures may
require launching again instead.

Keep the fail-closed integrity checks. Follow-up `jbox-rpd` should add timestamped
phase diagnostics, measure the actual workload, and coordinate any bounded
configurable bootstrap budget with host readiness, serial workspace count, and
kill grace. Do not delete or reset the host Beads database as a timeout workaround.

An independent read-only source review using the OpenAI `gpt-5.6-terra` route
confirmed the core timeout and sequencing findings. The earlier preferred
reviewer route failed before inspecting the source and supplied no evidence.

## Local evidence artifacts

Artifacts are local validation outputs, not repository fixtures:

- `/home/geonic/.jcode/scratch/jbox-review-full-validation-1467.log`
- `/home/geonic/.jcode/scratch/jbox-timeout-boundary-validation-1468.log`
- `/home/geonic/.jcode/scratch/jbox-real-beads-repeat-validation-1470.log`
- `/home/geonic/.jcode/scratch/jbox-extra-timeout-validation.py`
- `/home/geonic/.jcode/scratch/jbox-real-beads-synthetic-validation.py`

Slurm controller records were checked with `scontrol show job`. The local
accounting daemon was unavailable to `sacct`, so no accounting-history claim is
made. No application source was changed or fix implemented by this validation.
