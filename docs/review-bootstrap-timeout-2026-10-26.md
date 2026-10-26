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

## Actual public CLI, Kata, and SSH acceptance

Follow-up `jbox-546` addresses the earlier validation gap: direct Docker and
extracted-shell checks alone do not exercise the end-user launch path. Slurm
job **1472** completed with exit `0:0` after **76 seconds**, driving the real
`target/debug/jbox` executable through public commands. No Jbox source was
copied into a replacement harness or mocked. The orchestration script only
invoked the actual CLI, collected its output, and asserted observations.

The workload was a no-hardlink clone of this real repository at `4d7b4ad`,
including its **59 real tracked task records**, not newly generated issue
fixtures. Only the clone's configuration was narrowed to one repository, no
skills, no Git credentials or Git networking, no provider credentials, and an
ephemeral Jcode home. HOME, XDG state, SSH keys, and known-host entries were
private to `/home/geonic/.jcode/scratch/ka.pmucbs6_`.

The first public `run --new --no-attach` with `network.internet=false` exited 1
with the documented unsupported-offline-SSH error, before session creation.
The positive control explicitly enabled Jbox's supported bridge transport.
It therefore did permit network egress, unlike the earlier direct-Docker check.
No authenticated service, provider inference, credential import, host firewall
change, or additional runtime capability was requested.

Actual session **`nimble-otter-7b4c4a`** used existing immutable image
`jbox/jcode:local-v11-1001-1001-1d35700d120d911e`:

| Public-interface requirement | Observed behavior |
| --- | --- |
| Fresh launch reaches SSH through the complete entrypoint | `jbox run --new --no-attach` exited 0 in **40.773 seconds**, publishing `127.0.0.198:22`. |
| Actual runtime isolation | Docker inspection showed `kata`, a read-only root, no privileged mode, and every bind source beneath the isolated acceptance root. |
| Correct workspace and non-root SSH user | Public `jbox shell SESSION` reported `/workspace/project` and UID `1001`. The guest Docker socket was absent. |
| Real task snapshot survives hydration | `bd export --include-memories` invoked inside the public shell returned **exactly the same 59 IDs** as the real tracked export. |
| Inspection does not expose bootstrap noise as edits | Public `status` and `diff` succeeded, with empty uncommitted and staged diffs and no unique commits. |
| Repeated launch reuses the running guest | A second public `run --no-attach` exited 0 in **0.117 seconds**, and Docker inspection confirmed the exact same container ID. |
| Retained restart actually rehydrates and reaches SSH | Public `stop` retained worktrees, and public `run --no-attach` resumed the same session in **20.526 seconds**. This uses Jbox's normal retained-session restart flow. |
| Retained task records survive | A second public `shell` export returned exactly the original 59 IDs, verified against both the fresh export and tracked snapshot. |
| Original source task data is unchanged | The clone's JSONL remained byte-identical to `git show 4d7b4ad:.beads/issues.jsonl`, SHA-256 `c1454a4066b70b619da2bf0a127ecf3f687348f49c2dc25bf760891aa94630b7`. |
| Owned resources are removed by public cleanup | Public `stop` and `clean` exited 0, public `ls --all` reported no sessions, Docker had no matching container, and `git worktree list` contained only the original clone. |

This closes the **validation-path** gap with observable end-user behavior,
rather than treating a mock or direct container launch as Jbox acceptance.
The improvement is stronger, directly measured evidence and recovery guidance,
not a claim that application startup was repaired or made faster. No startup
implementation changed, and `jbox-rpd` remains open.

The initial validation above did not exercise the actual nanite 544-record
snapshot or its full multi-repository configuration. Passing the real Jbox
repository workload alone did not establish that the original timeout was fixed
or identify its slow command. A later recurrence review located the exact local
nanite commit, reproduced the cumulative timeout, and passed a finite-budget
control. Those subsequent observations and their narrower single-repository
limits are recorded in `review-recurring-bootstrap-2026-10-26.md`. Existing user
sessions were not restarted, reconfigured, or cleaned. Interactive Jcode TUI
operation and authenticated provider inference remain outside acceptance scope.

## Local evidence artifacts

Artifacts are local validation outputs, not repository fixtures:

- `/home/geonic/.jcode/scratch/jbox-review-full-validation-1467.log`
- `/home/geonic/.jcode/scratch/jbox-timeout-boundary-validation-1468.log`
- `/home/geonic/.jcode/scratch/jbox-real-beads-repeat-validation-1470.log`
- `/home/geonic/.jcode/scratch/jbox-extra-timeout-validation.py`
- `/home/geonic/.jcode/scratch/jbox-real-beads-synthetic-validation.py`
- `/home/geonic/.jcode/scratch/jbox-public-kata-acceptance-1472.log`
- `/home/geonic/.jcode/scratch/ka.pmucbs6_/acceptance.log`
- `/home/geonic/.jcode/scratch/jbox-public-kata-acceptance.py`

Slurm controller records were checked with `scontrol show job`. The local
accounting daemon was unavailable to `sacct`, so no accounting-history claim is
made. No application source was changed or fix implemented by this validation.
