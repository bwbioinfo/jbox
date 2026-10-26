# Recurring multi-repository Beads startup failure review

Date: October 26, 2026. Review task: `jbox-6b1`.

## Summary

The repeated `quiet-lizard-cba110` failure is consistent with a **too-small
cumulative bootstrap budget**, not evidence of an SSH networking failure or one
hung Beads command. An isolated real Kata launch using the actual nanite
snapshot now reproduces the timeout. A finite-budget positive control reaches
SSH and preserves all 544 task-record IDs without removing the stale-port
metadata or weakening the integrity checks.

This was a review and diagnostic experiment. No Jbox application source, existing
user guest, original repository configuration, or host Beads database was changed.

## Prioritized findings

### P1: The entire workspace bootstrap must fit into 60 seconds

`src/image.rs:119` gives initialization/import, prefix validation, export,
record-ID comparison, and hook installation one shared `timeout -k 5s 60s`.
Initialization success at `:163` does not finish the bootstrap: further commands
at `:169-200` must still run before SSH starts at `:212`.

The actual-data negative control measured `bd init` at 52 seconds, the following
prefix query at 3 seconds, and export at 4 seconds. All three returned exit 0.
The deadline then killed the remaining shell work. No observed Beads command
needed to hang for this failure to occur.

The outer SSH readiness allowance is independent. `src/paths.rs:690-692` computes
`min(60 + 60 * max(repository_count, 1), 300)`, explaining the 300-second message
for four repositories. It cannot rescue an inner 60-second failure. Raising only
the inner limit also leaves the outer budget inconsistent with serial hydration
of multiple repositories. Five near-limit healthy hydrations can consume the
entire current 300-second ceiling before other overhead. Kill-after grace matters
for a failing workspace, but must not be summed as if all workspaces could time
out: the first failure exits the entire entrypoint.

**Follow-up:** `jbox-rpd`. Use an explicit bounded policy with measured headroom,
coordinate guest and host deadlines, and preserve fail-closed hydration. Do not
start SSH against an incompletely checked database just to hide the failure.

### P2: Already-embedded metadata retains stale host connection hints

Nanite's tracked metadata at the reported commit already says
`"dolt_mode": "embedded"`, while retaining `dolt_server_host`, deprecated
`dolt_server_port`, and `dolt_database`. Its portable YAML also says
`dolt.auto-start: false`; Jbox supplies its own guest startup environment.

`src/image.rs:133-141` rewrites only metadata explicitly marked `server`. It does
not remove stale hints in an already-embedded file. This explains why the
warning persists despite the embedded-mode setup. Both diagnostic runs retained
these fields; the positive control succeeded while still printing the warning.

The warning is **not proof of a cross-project connection, host database access,
or the fatal timeout cause**. Jbox explicitly supplies embedded-mode environment
settings, and the host live databases were not mounted. Sanitizing guest-only
connection hints is nevertheless worthwhile defense in depth and diagnostic
hygiene, especially when several repositories share a guest.

**Follow-up:** `jbox-j1v`. Normalize the isolated guest copy with structured JSON
validation and safe path handling. Do not edit/reset the host database or host
metadata just to silence the warning.

### P2: Existing diagnostics can obscure the phase and erase evidence

`src/engine.rs:370-378` takes only the last 50 Docker log lines and concatenates
stdout before stderr. That output does not preserve cross-stream chronology.
A success message appearing before a warning does not establish exact execution
order. Guest-exit paths fetch logs, but a still-running guest that hits a host
readiness deadline can be removed by `src/lib.rs:548-553,2708-2712` without the
same startup-log collection. Port-deadline cleanup has a similar gap.

Include timestamped workspace/phase start and finish events, durations, and exit
codes, with ordered log retention on every startup failure. The diagnostic
wrappers used below demonstrate the information missing from ordinary output.
This remains part of `jbox-rpd`.

### P2: Interrupted fresh initialization can skip tracked gitignore restoration

`src/image.rs:157-167` backs up a tracked `.gitignore`, runs `bd init`, then
restores the backup only after successful return. The earlier EXIT trap at
`:127-129` removes only the saved JSONL temporary file. An init error or timeout
can bypass `.gitignore` restoration. This is a source-level recovery risk,
**not observed in the nanite reproduction**, where initialization returned 0.

**Follow-up:** `jbox-bq8`. Make restoration failure-safe, preserve the original
error, and address retained-worktree recovery when SIGKILL cannot run a trap.

### Recovery consequence: Failed first launches are not retained sessions

Initial SSH failure attempts to stop the guest and remove worktrees/session
state before `state.save` (`src/lib.rs:548-553,586`). A failed first launch may
therefore have no resumable ID. Cleanup errors are ignored in
`src/lib.rs:3130-3136`, so complete removal should be verified rather than
assumed. A retained resume failure follows a different path and keeps its
worktrees (`src/lib.rs:2708-2712`).

The existing 60-second policy is still in production source. Repeatedly launching
or refreshing an image from that unchanged source is not a repair. After a real
fix, retained sessions need explicit image refresh; failed initial launches may
need a new launch instead. Do not clean or reset unrelated user state.

## Real-data public-interface validation

The original sibling nanite repository was available locally for this recurrence
review. Its clean HEAD exactly matched the supplied failed commit:
`e9af9ccdccdf603e214e47713c7e455d9a2d183f`. The portable export had **544 records,
1,158,640 bytes**, and initialization reported **70 deferred dependencies wired**.

Both experiments used a private no-hardlink clone of that commit, the actual
`target/debug/jbox` public CLI, real Kata startup, a private HOME/XDG tree, and
Beads 1.3.2 from the existing base image. A transparent command wrapper forwarded
arguments to the unchanged `/opt/beads/bd` and printed timestamps and status.
The real Jbox entrypoint, import/export logic, ID checks, and hook setup remained
in place. No host live Beads database or personal credentials entered the guest.

| Observation | Original-bound negative control | Budget-only positive control |
| --- | --- | --- |
| Slurm job / result | **1476**, completed `0:0` because the expected guest failure was observed | **1478**, completed `0:0` after public startup/export/cleanup checks passed |
| Guest | `ember-sparrow-09e4e2` | `eager-mink-b38e77` |
| Inner budget | Original **60 seconds** | Review-only finite **180 seconds** |
| `bd init` | 52 seconds, exit 0 | 53 seconds, exit 0 |
| Post-init prefix query | 3 seconds, exit 0 | 3 seconds, exit 0 |
| Bootstrap export | 4 seconds, exit 0 | 6 seconds, exit 0 |
| SSH | Guest exited with bootstrap 124 before SSH | SSH started and public `jbox shell` succeeded |
| Stale-port warning | Present | Still present, yet startup succeeded |
| Record verification | Bootstrap interrupted after export | Export count 544 and exact sorted ID-set SHA-256 matched the original snapshot |

For the positive control only, a disposable image put a transparent `timeout`
wrapper earlier in PATH. It changed just the exact `-k 5s 60s` bootstrap invocation
to `-k 5s 180s` and delegated to real `/usr/bin/timeout`. This was a **diagnostic
intervention**, not a product implementation or a proposed unbounded bypass.
The host's normal single-repository readiness budget stayed unchanged.

The positive-control ID-set SHA-256 was
`218dbcd55154bf420b6d335db13483e2a1ecb0b5a3fc74b51b0687e6ad81df34`.
Public `stop` and `clean` removed its owned guest/session/worktree. The failed
negative-control guest was also absent after startup cleanup. Original nanite
JSONL, metadata, and YAML were byte-identical before and after both experiments,
and the original repository stayed clean.

### Scope and limitations

These experiments use real nanite task data and actual Jbox/Kata/SSH integration,
not 544 newly generated records. They establish a concrete cumulative-budget
failure and a successful bounded control for that dataset in the tested setting.

They are **single-repository, 2-CPU/4-GiB** diagnostic runs on the existing base
runtime, not the full original four-repository, 8-CPU/16-GiB IaC project image,
private-skills setup, or credential policy. Transparent timing wrappers add a
small amount of overhead, and runs were sequential rather than a statistical
performance benchmark. Bridge egress was permitted for supported Jbox SSH;
Git networking and provider credentials were disabled. No provider inference or
interactive Jcode TUI test was performed. Successful ID comparison does not
independently attest to every dependency edge or issue field.

The evidence now supports increasing and coordinating the bounded budget, but
does not claim that a production patch was implemented, deployed, or accepted
against every original repository. `jbox-rpd`, `jbox-j1v`, and `jbox-bq8` remain open.

## Requested swarm families

The requested GPT-6.1/Claude-5.5 preference was retained with `bd remember`.
An independent GPT-6.1 Terra source review completed. Advertised Claude-5.5 OAuth
routes failed before inspection, and the explicit Copilot Claude Opus 5.5 route
also returned unsupported-model HTTP 400. No successful Claude review is claimed,
and no older-family model was substituted for these reviews. A route-mismatch
feedback attempt was not sent because telemetry is disabled.

## Local evidence

- `/home/geonic/.jcode/scratch/jbox-real-nanite-review-1476.log`
- `/home/geonic/.jcode/scratch/nr.cf0l3qsi/diagnostic.log`
- `/home/geonic/.jcode/scratch/jbox-nanite-budget-control-1478.log`
- `/home/geonic/.jcode/scratch/nr.a7jwv9i4/diagnostic.log`
- `/home/geonic/.jcode/scratch/jbox-gpt61-timeout-review.md`
- `/home/geonic/.jcode/scratch/jbox-real-nanite-review.py`
- `/home/geonic/.jcode/scratch/jbox-real-nanite-budget-control.py`

No application source changed. Existing full quality-gate results are documented
in `review-bootstrap-timeout-2026-10-26.md`; this additional review used the actual
public runtime path and `git diff --check` for its documentation/tracking changes.

## Whole-result replay after the completed review

Task `jbox-b2v` reran the complete mapped validation sequence **after** review
commit `975c6b3`, not just the checks that preceded the findings. Slurm **1480**
ran October 26, 2026, **17:06:57 to 17:12:02 EDT**, and completed **0:0**.
Its final marker was `FINAL_RESULT_ALL_CHECKS_PASSED`. Cargo verified the actual
CLI build against the unchanged final source before the public-interface checks. Subsequent
changes only record these results in documentation and the portable issue export.

| Requirement / changed review output | Fresh check over the whole result | Observed result |
| --- | --- | --- |
| Current source remains buildable and CLI usable | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`, `cargo run -- --help` | All exited 0; **187 Rust tests** passed across all targets, help printed normally. |
| Installer regressions remain intact | Python unittest discovery from `tests` | **30 tests passed**, exit 0. |
| Fail-closed timeout and integrity conclusions | Rerun the seven extracted-shell boundary checks | All seven passed: healthy success, export timeout 124, bounded slow success, distinct import failure 9, missing-record rejection, wrong-prefix rejection before import, and prefix-query failure 7. Inputs stayed unchanged. These are explicitly synthetic checks. |
| Real pinned Beads supports fresh and retained hydration | Rerun isolated Docker check with actual Beads 1.3.2 and synthetic 544 records | Both passes completed under their original 60s bounds, exported 544 records and installed mode-0700 hooks. Container exited 0 in **35.044s**. The noexec-mount hook-execution limitation remains. |
| Public launch and SSH work through the full entrypoint | Real `jbox run --new --no-attach`, then public `shell` | Session `mellow-badger-7d440d` reached SSH in **44.263s**. Shell reported `/workspace/project`, UID 1001, and no Docker socket. |
| Isolation boundaries are real, not mocked | Docker inspection of the public lifecycle guest | Kata, read-only root, non-privileged mode, and every bind source below the private acceptance root were asserted. No provider or Git credentials were enabled. |
| Public rejection remains explicit | Public launch with `network.internet=false` | Exit 1 in **0.011s**, unsupported-offline-SSH error and no saved session. Positive runs explicitly used supported bridge networking. |
| Status, diff and reuse remain usable | Public `ls`, `status`, `diff`, and repeated `run --no-attach` | All exited 0. Repeated run took **0.117s** and retained the exact same container ID. |
| Retained startup and data preservation work | Public `stop`, `run --no-attach`, and a second public-shell export | Same session resumed in **22.434s**. Independent assertions compared both exported ID sets for **exact equality** with all **63** tracked task IDs in the committed Jbox review snapshot `975c6b3`. Its source JSONL stayed byte-identical. |
| Actual nanite still reproduces the original inner-bound failure | Fresh public launch of the actual `e9af9cc` portable snapshot with the original 60s budget | `lunar-mink-6d68bb` exited before SSH with bootstrap 124; CLI exited 1 after **79.003s**. Init returned 0 in **50s**, config returned 0 in **4s**, then export started without an END marker before timeout. This repeat does not claim export returned successfully. |
| A finite budget intervention changes the actual outcome | Same real nanite input, transparent timing, and diagnostic-only 180s control | `cosmic-bison-ca9c9a` reached SSH; CLI exited 0 after **82.292s**. Init/config/bootstrap export returned 0 in **54s / 4s / 5s**. Public-shell export returned **544 records** with the exact original sorted-ID hash recorded above. |
| Deprecated-port warning is not sufficient to cause failure | Observe logs in the successful real-data control | The warning persisted while SSH and all 544-ID checks passed. No cross-project-access or host-DB safety claim is inferred solely from that success. |
| Both failure and success clean up owned resources | Independent post-driver Git, Docker and session-state assertions for all three public runs | No owned container or session JSON remained, and each private clone had only its original registered worktree. Positive paths used public stop/clean. |
| Original nanite input is not damaged by the review | Hash JSONL, metadata and YAML before/after each public driver, compare original Git status | Every hash and original status matched. Its live host Beads database was never mounted or opened by these experiments. |
| Findings and recovery guidance match final source | Independent GPT-6.1 Terra read-only audit of both review documents, line references and Git history | Source findings and citations matched. Review commits changed only documentation/tracking, not application source. Audit qualifications are retained below. |
| No unimplemented fix is presented as accepted | Inspect the three implementation follow-ups and final diff | `jbox-rpd`, `jbox-j1v` and `jbox-bq8` remain open. No production timeout, metadata or restoration fix was deployed. |

The repeated negative control was interrupted in export, whereas the earlier
negative control recorded export returning before the remaining shell was killed.
This strengthens the **cumulative-budget** diagnosis without identifying a single
deterministically stuck operation. Durations printed by the transparent wrapper
are whole-second measurements, not a precise breakdown of every shell cost.

The independent audit also checked the outer-budget arithmetic: one repository
gets 120s, four get 300s, and five still get 300s. Host readiness timing starts
after guest execution has already begun, so comparing those policy values is
not a synchronized prediction of exactly when a multi-repository launch fails.

### Remaining acceptance limits, not hidden passes

This replay still does **not** accept the full original four-repository IaC image,
8-CPU/16-GiB configuration, credentials, private skills, interactive TUI or provider
inference. Issue-field and dependency-edge equivalence beyond record IDs was not
independently attested. Interrupted-init `.gitignore` recovery, stale metadata
normalization, host-deadline log retention, and explicit image-refresh recovery
remain source-reviewed risks or implementation work, not runtime-tested fixes.
No destructive fault injection was performed on retained user sessions.

The feedback loop therefore closes the requested **review and its stated checks**,
not a production-repair goal. The additional validation produces reproducible
observations and sharper phase qualifications rather than a claim of faster or
fixed application startup.

Replay artifacts:

- `/home/geonic/.jcode/scratch/jbox-whole-result-1480.log`
- `/home/geonic/.jcode/scratch/jbox-whole-result.sbatch`
- `/home/geonic/.jcode/scratch/jbox-whole-result-replay.py`
- `/home/geonic/.jcode/scratch/ka.ctrbrvrz/acceptance.log`
- `/home/geonic/.jcode/scratch/nr.b756as6k/diagnostic.log`
- `/home/geonic/.jcode/scratch/nr.ha_e56z3/diagnostic.log`
- `/home/geonic/.jcode/scratch/jbox-whole-review-audit.md`
