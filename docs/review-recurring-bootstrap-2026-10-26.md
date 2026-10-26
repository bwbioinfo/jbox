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
