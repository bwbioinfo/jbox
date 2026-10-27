# Beads bootstrap timeout fix and acceptance, 2026-10-27

## Cause and implementation

The reported Nanite failure exhausted the old **60-second cumulative workspace bootstrap budget**, not the 300-second SSH probe alone. Initialization can print that all 544 issues were imported before prefix verification, export, ID checks, and hook installation finish. The previous real-data negative and finite-180-second control are documented in [the recurring-failure review](review-recurring-bootstrap-2026-10-26.md). A stale-port warning also occurred in a successful control, so that warning alone did not explain the failure.

Jbox 0.15.3 changes the production policy:

- `[beads].bootstrap_timeout_seconds` defaults to **180**, accepts integers **1 through 3600**, and rejects zero rather than allowing an unlimited wait.
- The guest validates the forwarded value and bounds the entire hydration and verification operation with the existing five-second kill grace. No bootstrap integrity check is bypassed and SSH remains unavailable until all workspace bootstraps finish.
- Host readiness allows **60 + (timeout + 5) * actual Beads workspace count** seconds. Non-Beads repositories do not inflate this count and there is no independent 300-second truncation.
- Timestamped workspace and command-phase START/END/FAIL messages include elapsed time and status. The host retrieves the latest 200 timestamped log lines before removing a failed guest and merges Docker stdout/stderr chronologically.
- The base-image cache namespace changes from `local-v11` to `local-v12`, in addition to the existing staged-content fingerprint, so a newly generated base cannot silently use the old entrypoint.

The primary repository can request additional finite headroom:

```toml
[beads]
bootstrap_timeout_seconds = 600
```

## Acceptance workload and isolation

Acceptance runs the actual Jbox CLI, normal image manager, installed Docker/Kata runtime, actual pinned Beads binary, and public SSH shell. It does not substitute a copied entrypoint, a Beads mock, or a timeout wrapper. Shell mocks in the regression suite are supplemental failure-injection evidence only.

Four private no-hardlink clones use the commits from the user's failing launch:

| Repository | Input commit | Guest mount | Issue IDs |
| --- | --- | --- | ---: |
| mpgi-iac | `c025ad0c2320270e90305b46b9c37dd279af1982` | `/workspace/iac` | 274 |
| nanite | `e9af9ccdccdf603e214e47713c7e455d9a2d183f` | `/workspace/nanite` | 544 |
| heliXplore | `f25bb62584899bed85c2bac68cceb60e655bc56c` | `/workspace/heliXplore` | 96 |
| porectl | `a438f5dafc473cf0512dde091d17d3aaa7a503b1` | `/workspace/porectl` | No tracked Beads snapshot |

Mpgi-iac has 284 portable records, including ten memory records, but only 274 issue IDs. Comparisons below concern exact issue ID sets, not complete issue-field, dependency, or memory equivalence.

The acceptance configuration deliberately uses the standard base image, two CPUs and 4 GiB, an isolated HOME/XDG tree, explicit supported bridge networking, no Git network access or credentials, no inherited host author, no provider credentials, and no skills. Docker inspection asserts Kata runtime, read-only root filesystem, non-privileged operation, and exclusively private acceptance-root bind sources. The public guest shell verifies UID 1001, all four workspace mounts, and absence of `/var/run/docker.sock`.

Source repositories' portable Beads files and Git status are captured before the run and checked afterward. Runs 1505/1506 encountered concurrent non-Beads edits in the live mpgi-iac checkout, so those runs did not certify whole-checkout status stability. An additional read-only postcheck verified all eleven available live portable Beads inputs byte-equal to the pinned private sources. Final run 1508 used uncontended private source baselines at the same four commits, retained every source and isolation guard, and verified identical before/after HEAD, clean Git status, and all eleven portable-file hashes. Host live Beads databases are not mounted or reset.

## Feedback and corrective iterations

1. **Slurm 1501:** 25 focused image tests passed after implementing bounded guest phases.
2. **Slurm 1502:** the full gate passed 198 Rust tests and 30 Python tests. Real production startup with a deliberately valid but insufficient one-second budget failed before SSH with `phase=init`, `status=124`, and `budget=1s`. However, the independent resource check failed: a dirty unpublished worktree could not be removed non-forcibly, and subsequent directory deletion left its Git registration behind. This was a failed acceptance run, not a pass.
3. The cleanup regression now exercises real Git isolation and dirty tracked/untracked files. Fresh launch rollback was changed to unregister bootstrap-dirtied worktrees without touching the source checkout or retained-session work. Independent review also identified Docker polling errors that escaped startup cleanup. A common post-run failure path now retrieves logs, attempts stop, and preserves the original failure and any cleanup error.
4. **Slurm 1503:** the complete gate passed **202 Rust tests and 30 Python tests**, followed by successful actual four-repository acceptance. The one-second negative left no owned guest, saved session, or extra Git worktree registration. The production-default positive reached SSH, exact issue IDs survived fresh and retained bootstraps, the running session was reused, and public stop/clean removed all owned resources. Review then identified additional storage-preservation edge cases when Docker stop or Git restoration fails.
5. Fresh rollback now returns cleanup failures, retains the enclosing directory unless cleanup succeeds, and preflights saved host Git links before restoring isolated metadata. Failed or uncertain container removal preserves storage and reports recovery steps. A typed `GuestStartUncertain` error preserves storage unconditionally on a Docker run client failure, since an immediate absence probe cannot rule out an in-flight daemon request. GPT6.1 independent focused inspection confirmed that final ambiguity was addressed, with no additional blocker in that check.
6. **Slurm 1505:** the full gate passed **207 Rust tests and 30 Python tests**, release version/help worked, and all real public workflow checks passed. Cold launch was **153.252 seconds**, retained launch **106.303 seconds**. Nanite's fresh bootstrap took **68 seconds**, exceeding the old 60-second limit while successfully finishing with the new production budget. However, the overall job exited 1 because the final original-repository Git-status equality assertion changed while unrelated edits were being made in the original mpgi-iac checkout. This is not an overall job pass. Its guest mounts were exclusively private clones, its public stop/clean and all four registration checks passed, and those concurrent source edits were not reset or included in this patch.
7. **Slurm 1506** reran the whole final source after the typed Docker uncertainty safeguard. All required gates passed: formatting, Clippy with warnings denied, **208 Rust tests** (193 library, 8 CLI, 3 continuity, 4 init/update), CLI help, **30 Python tests**, release build, and release version/help. All public acceptance checks also passed, but the overall job again exited 1 at the original-checkout status guard when a concurrent mpgi-iac validation document appeared. That constraint is reported, not relabeled a clean overall job. Follow-up `jbox-98i` tracks durable source-baseline and concurrency diagnostics for this local acceptance tooling.

### Observed Slurm 1503 public workflow

- Negative guest: `warm-heron-99735d`, CLI exit 1 after 21.273 seconds including guest startup. Workspace timeout logged duration 3 seconds, status 124, and last active phase `init`. All four private Git registries contained only their original checkout afterward.
- Positive guest: `wise-otter-02407c`, cold public launch **114.898 seconds**, retained launch **70.695 seconds**. Progress correctly reported a **615-second** host budget for three Beads workspaces.
- Fresh workspace bootstrap durations: mpgi-iac **30 seconds**, nanite **51 seconds**, heliXplore **17 seconds**, all status 0 with budget 180.
- Retained durations: mpgi-iac **21 seconds**, nanite **33 seconds**, heliXplore **8 seconds**, all status 0. Retained paths exercised prefix checks, import dry-run, import, export, ID checks, and hook setup.
- Public `status`, `diff`, `run --no-attach`, `stop`, and `clean` returned successfully. Repeated run preserved the exact container ID, and stopped-session restart preserved the session ID.
- Both fresh and retained `jbox shell` exports produced these exact sorted-ID hashes:

| Repository | Count | SHA-256 of newline-joined sorted IDs |
| --- | ---: | --- |
| mpgi-iac | 274 | `28e84cb149beeeea3f616a1e4c9c7429d0e591b5bafe391095cce5931b9b2df9` |
| nanite | 544 | `218dbcd55154bf420b6d335db13483e2a1ecb0b5a3fc74b51b0687e6ad81df34` |
| heliXplore | 96 | `d6efdda37c5fd85d0c714f6b14c84937b975ac727c1b30abe4524763d18aba3a` |

The positive used `jbox/jcode:local-v12-1001-1001-f073512a71a46d67`. `docker image inspect --format '{{.Id}}'` reports `sha256:ed7209439668f50ea6ada26ea5af501dd8ca1353119b323e2c5198902631d25e` for that image.

## Final whole-result observations, Slurm 1506

- Actual release artifact: `target/release/jbox`, version **0.15.3**, SHA-256 `d520ed05c6ca632ab3cf6784d279b96b808423106cde133bd8f395a9508137bb`.
- Public CLI rejected policy **0** and **3601** before starting a guest.
- The real one-second negative, `bright-caribou-5af164`, failed before SSH, identified `init` and status 124, and left no owned guest, saved session, or additional worktree registration.
- Production-default guest `clever-cat-434345` reached SSH in **144.543 seconds**. Workspace durations were **42 / 66 / 25 seconds** for mpgi-iac / nanite / heliXplore. The Nanite operation exceeded the old 60-second budget and completed under the unchanged production default of 180.
- Public shell exports matched all **274 / 544 / 96** issue IDs and the hashes above. `status` and `diff` succeeded. Repeated `run --no-attach` reused the exact container ID.
- Public stopped-session restart kept the session ID and reached SSH in **91.821 seconds**. Retained durations were **18 / 48 / 19 seconds**, and every exact issue ID set matched again.
- Public stop/clean succeeded. Independent Docker inspection found neither owned container, no session metadata remained, and all four private Git registries contained only their original checkout.
- The final whole-original Git-status assertion failed amid concurrent mpgi-iac edits. It did **not** prevent the public workflow from running. At **14:54 UTC**, a separate read-only check verified every available original `.beads/issues.jsonl`, `metadata.json`, and `config.yaml` against its immutable private input: **eleven byte-equal files** across the four repositories. The changing mpgi-iac checkout was not reset, committed, or modified by this task.
- The installed `~/.cargo/bin/jbox` remains **0.15.2**, SHA-256 `eccd0604af0cd0092d0f86deb76748ec177cf03bb103699cc70de74704481696`. A validated workspace artifact is not an installed upgrade.

### Closed whole-result feedback loop, Slurm 1508

On **2026-10-27 at 15:09 UTC**, the complete rerun finished with controller-reported `JobState=COMPLETED ExitCode=0:0`. The accounting database was unavailable to `sacct`, so the actual controller result was independently obtained with `scontrol show job 1508`. Unlike runs 1505/1506, this is an overall acceptance pass, not just a successful public workflow followed by a failed guard.

- The whole final result passed formatting, Clippy with warnings denied, **208 Rust tests and 34 Python tests**, CLI help, and release build/version/help. The release SHA-256 above is unchanged. A later staged whitespace check removed only one extra EOF blank line from the manual driver. An explicit AST comparison verified no executable change from run 1508, and **Slurm 1509** reran the complete required quality gate and driver help successfully on the exact final files (`jbox-final-acceptance-tooling-gate.log`).
- The tracked driver `tests/manual_bootstrap_acceptance.py` used uncontended no-hardlink source baselines under `/home/geonic/.jcode/scratch/jbs.t0Dw2x`, at the four exact input commits above. It then created separate working clones for the actual public workflow and private configuration commits. The live user checkouts were not reset or modified. This changes the source baseline location, not the production CLI, payloads, integrity checks, or runtime.
- Public invalid-policy checks rejected **0** and **3601** before guest startup. The valid one-second negative, `brisk-caribou-849787`, failed in **18.861 seconds** including startup, exposed `init` and timeout status 124, and left no owned container, session metadata, or additional Git worktree registration.
- Default-180-second guest `keen-hare-672508` reached SSH in **113.117 seconds**. Fresh workspace bootstrap durations were **31 / 53 / 19 seconds**. Stopped-session restart retained the same session and reached SSH in **80.087 seconds**, with bootstrap durations **23 / 36 / 11 seconds**. These observations do not replace the earlier measured **66-second** Nanite success demonstrating work beyond the old limit.
- Actual public `shell`, `status`, `diff`, running-session reuse, stop, retained run, and clean succeeded. Fresh and retained SSH exports matched every **274 / 544 / 96** issue ID and the hashes above. Docker inspection verified Kata, read-only root, non-privileged operation, private-root bind sources, and environment timeout180/count3. Final cleanup verified both owned containers absent, no saved session, and exactly one original worktree registration per private repository.
- The independently reloaded `source_manifest_before.json` and `source_manifest_after.json` were identical for **all four** source HEADs, clean Git statuses, and **all eleven** available portable files. Porectl's absent issues snapshot remained absent. `overall_result.json` reported `workflow_pass=true`, `workflow_error=null`, `guard_errors=[]`, `cleanup_errors=[]`, and `overall_pass=true`.
- Four focused driver regressions cover multiple changed source statuses, portable-file creation/removal, capture failures that do not skip later sources, and independent workflow/guard/cleanup results. The driver prints `OVERALL_ACCEPTANCE_PASS` only after all these result boundaries succeed. Run 1508 additionally printed `WHOLE_RESULT_ACCEPTANCE_PASS` after the complete gate and real workflow.

The durable local evidence is `/home/geonic/.jcode/scratch/jbox-uncontended.vSr1Qz.log` and `/home/geonic/.jcode/scratch/bp.lh0lsk59/` containing `acceptance.log`, both source manifests, and `overall_result.json`. This closes the source-baseline follow-up `jbox-98i`. Historical run 1505/1506 overall failures remain failures and their live-checkout concurrency limitation is not retroactively certified.

## Requirement-to-check map

| Requirement / changed output | Concrete check | Final observed result |
| --- | --- | --- |
| Default 180, finite integer range, no unlimited zero | Config and executable shell boundaries, public invalid-policy rejection | Tests passed, public 0/3601 rejected before guest creation, actual environment 180 |
| Larger finite budget permits healthy work, stalled work still fails | Slow-success/stall regressions, real one-second negative, production-default four-repository positive | Tests passed, real negative status 124, Nanite 66s completed, all three DBs reached SSH |
| All bootstrap integrity checks remain before SSH | Prefix/incomplete-export/custom-hook rejection tests, phase tests, actual fresh/retained exact-ID exports | Tests passed, negative published no running session, exact 274/544/96 IDs matched twice |
| Serial host budget includes grace and no 300s cap | Formula boundaries, large counts, configured 600s, overflow and forwarding tests, actual progress | Tests passed, three-database progress reported 615s |
| Fresh and retained policy forwarding | Container-spec tests, actual Docker environment and retained public shell | Passed, 180s/count3 environment and retained database exports verified |
| Actionable bounded diagnostics before cleanup | Chronological merge/tail-size and original-error tests, real negative phase log | Tests passed, actual `init` START and timeout status/budget visible before resource deletion |
| New launch cannot use an old cached base entrypoint | `local-v12`, content fingerprint regressions, actual image and runtime behavior | Tests passed, actual v12 image used and budget180 phases observed |
| Safe fresh cleanup and retained work preservation | Real Git dirty/failure/preservation tests, typed uncertainty test, real failure resource inspection and retained workflow | Tests passed, real negative and final clean left no extra registrations or containers. Failed-daemon behavior not fault-injected into the user's daemon |
| Public interfaces and packaging | Complete local gate, release version/help, real run/shell/status/diff/reuse/stop/clean | Slurm1508: 208 Rust + 34 Python passed, release 0.15.3, every public workflow command passed |
| Portable source inputs and no live host DB mounts | Persisted all-source manifests, byte hashes, Docker private-root bind-source inspection | Slurm1508: all four pinned HEADs/clean statuses and 11 hashes unchanged, porectl snapshot absent, all binds private. Earlier live-checkout guards remain externally contended |
| Whole-result success cannot hide a source or cleanup failure | Driver result-boundary regressions, persisted result and actual Slurm controller result | Tests passed, final workflow/overall true with empty guard/cleanup errors, controller COMPLETED 0:0 |

## Reproduction and rollout

Required local quality gate:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run -- --help
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py' -v
cargo build --release
./target/release/jbox --version
```

The tracked manual driver is opt-in and requires local clones containing the four exact commits in the workload table, an available Docker/Kata runtime, and a built release. Its import and `--help` do not create files or start Docker. Run heavy validation through the project's local Slurm workflow. For example, inside that allocation:

```bash
PYTHONDONTWRITEBYTECODE=1 python3 tests/manual_bootstrap_acceptance.py \
  --source-root /path/to/uncontended/four-repository-sources \
  --scratch-root /short/path/outside/repositories \
  --cli "$PWD/target/release/jbox"
```

Use a short scratch path to stay within SSH's Unix-socket path limit. The driver prints its new private root and preserves the before/after manifests and overall result there, including on caught workflow failures. It clones the named local inputs, commits only private acceptance configuration, exercises real public commands, and independently verifies results. The source baseline must be uncontended for a clean source-status guard. Local evidence paths are not portable CI dependencies.

Historical scratch evidence remains under `/home/geonic/.jcode/scratch/`: `jbox-production-bootstrap-acceptance.py`, `jbox-bootstrap-fix-1502.log`, `jbox-bootstrap-fix-1503.log`, `jbox-bootstrap-fix-1505.log`, `jbox-bootstrap-fix-1506.log`, and `bp.vvg91qgn/acceptance.log`. Both earlier overall job failures at the externally contended original-status guard are preserved.

The user's installed binary is not replaced by a workspace build. To install after reviewing the change:

```bash
cargo install --path . --locked --force
jbox --version
```

With the updated CLI, retry a failed first launch with `jbox run --new PATH`. For an existing retained session, stop it first, then run `jbox resume SESSION --refresh-image`. Editing timeout configuration alone cannot change an old saved image's hard-coded 60-second entrypoint. Do not reinitialize the host database or disable bootstrap checks.

## Scope and remaining limitations

The workload uses the original four repository snapshots, but does not reproduce the user's custom IaC image, 8-CPU/16-GiB allocation, credential/skill integrations, provider login, or interactive Jcode TUI. Timings are observations on this machine, not performance guarantees. Broader policy values above 180 are covered by validation/forwarding and focused executable tests, not an hour-long real guest stall.

Separate pre-existing issues `jbox-j1v` (retained embedded metadata normalization) and `jbox-bq8` (tracked `.gitignore` restoration after interrupted init) are not silently marked fixed by a timeout increase. The acceptance checks exact issue IDs, not every field, relation, or memory record. Initial-launch cleanup failures must preserve recoverable storage rather than deleting it to make a resource check appear clean. Failed-stop and in-flight-daemon behavior is covered by focused safety/error tests and independent inspection, not by interrupting the user's Docker daemon or forcing a real daemon transport failure.
