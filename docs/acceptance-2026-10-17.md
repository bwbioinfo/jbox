# Branch reconciliation acceptance, 2026-10-17

## Scope and result

All unique work from the local and remote branches was reconciled into `main`
without deleting branches or changing the pre-existing active session worktree.
Merge `96baf7c` retains both divergent histories. The canonical ArtCraft installer
is the hardened Python installer with a JSON manifest, VectorCraft 0.12.0, and
bounded version/help/command/OCR checks. OpenAI transport configuration and the
Git broken-ref fallback remain included.

Actual host acceptance, not only unit tests, subsequently exercised the public
CLI against a new, credential-free disposable Git repository. Docker and Kata
were available on this host. No host runtime socket, device, credentials, private
skills or additional Linux capabilities were exposed to a guest.

## Clean image and guest

The first actual `jbox run <project> --new --no-attach` built the new default
image from the pinned upstream toolchain images and downloaded/checksum-verified
all 13 ArtCraft releases. Both root and final non-root build checks passed.
The first fresh Kata startup then exposed a real regression: root-owned OCI
mount ancestors prevented toolchain initialization from creating `.local/bin`.

The follow-up fix prepares only the fixed directories backed by the dedicated
home tmpfs. It rejects symlinks and checks the longest matching mount target,
including bind-mounted descendants. Because root lacks `DAC_OVERRIDE`, it owns
only the private home tmpfs root temporarily, prepares directories before
restoring child-to-parent ownership, and restores the home owner on both success
and shell exit. It does not change directory modes, recursively change owners,
modify bind-mounted directories or add capabilities.

The corrected image and project image were:

- Base: `jbox/jcode:local-v11-1000-1000-9beb6c44e1999fb1`
- Project: `jbox/project:f5bda91955449dda`

Actual SSH into new Kata session `harbor-penguin-4c1036` observed:

| Check | Observed result |
| --- | --- |
| Container runtime / root filesystem | `kata` / read-only |
| Dropped capabilities | `ALL` |
| Added capabilities | Existing `AUDIT_WRITE`, `CHOWN`, `KILL`, `SETGID`, `SETUID`, `SYS_CHROOT` only |
| Interactive command user | UID/GID 1000, `jbox` |
| Private home | `jbox:jbox`, mode `0700` |
| `.local`, `.local/bin`, `.local/share` | Owned by `jbox`, writable to the guest |
| Rust / Cargo | 1.99.0, including working rustfmt and Clippy commands |
| Python / pip | 3.15.0 / 26.2.1 |
| uv / uvx | 0.13.1 |
| Node / npm / pnpm | 26.12.0 / 11.20.0 / 12.11.2 |
| rclone | 1.75.2 |
| ArtCraft installed verification | `Verified all 13 ArtCraft CLIs` |
| GridCraft `SUM(1,2,3)` | `6.0` |
| PdfCraft `run ocr_status` | Root JSON object with `available: true`, English models found |
| VectorCraft SVG to PDF and PNG | Nonempty 4146-byte PDF and 821-byte PNG, no warnings |
| Reopened exported PDF | 128 by 96 artboard, two paths, no warnings |
| Generated OpenAI transport | `[provider] openai_transport = "https"` |
| Jcode server socket | Present with mode `0600` |
| Docker socket inside guest | Absent |

The installed verification command used both during the build and over real SSH:

```bash
python /usr/local/lib/jbox/install-artcraft.py \
  --manifest /usr/local/lib/jbox/artcraft-releases.json --verify-only
```

## Public lifecycle and isolation

The real `status` and `diff` commands inspected the session. A commit created
over SSH in `/workspace/project` produced
`977fc167e6a0274a9366fc448ea0cd299494d309`. Its new file remained absent from the
original host fixture until explicit acceptance.

`jbox stop` retained the worktree. `jbox run <project> --no-attach` resumed the
same session and preserved the exact guest commit and file contents. After
waiting for the asynchronously started Jcode socket, the resumed guest also
completed the vector exports above. `jbox accept <session> --into main`
fast-forwarded only the disposable host repository while the guest remained
running. A second stop followed by `jbox clean` removed the guest container and
session metadata, leaving only the original fixture worktree.

A second fresh session, `indigo-shark-f96638`, used real reference directories
mounted read-only at `/home/jbox/.config` and writable at `/home/jbox/.pi`.
Startup and SSH passed. Host UID, GID, modes and nanosecond change timestamps
were identical before and after startup. In particular, `.config/gh` was not
created through the reference mount. The session was stopped and cleaned, and
the two still-empty fixture directories were removed. An initial attempt to
mount fixtures under the protected host `.jcode` directory was correctly
rejected, without relaxing that policy.

## Quality gates and evidence

The final complete local gate passed:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run -- --help
python3 -m unittest discover -s tests -p 'test_*.py' -v
git diff --check
```

Results: 178 Rust tests and 30 Python tests passed. New Rust coverage checks
mount classification, tmpfs-backed bind exclusion, fixed-path preparation and
ownership restoration ordering. A separate read-only reviewer found no blocker
in the final ownership repair.

Host-local transcripts are retained under `$JCODE_SCRATCH_DIR`:

- `jbox-reconcile-acceptance/live-run.log`: actual clean build and initial failure.
- `jbox-reconcile-acceptance/live-run-leaf-first.log`: corrected fresh startup.
- `jbox-reconcile-acceptance/guest-tooling.log`: real SSH and installed tooling.
- `jbox-reconcile-acceptance/lifecycle.log`: stop and successful retained resume.
- `jbox-reconcile-acceptance/accepted-clean.log`: vector exports, acceptance and cleanup.
- `jbox-reconcile-acceptance/mount-boundaries-final.log`: actual mount preservation.
- `jbox-live-final-quality.log`: final local quality gates.

## Limits and follow-up

This acceptance checks startup for all 13 applications and representative
spreadsheet, vector and PDF-model workflows. It does not repeat a complete
content workflow for every ArtCraft application or perform image-only PDF OCR.
It does not make authenticated OpenAI requests, validate Slurm integration or
claim stronger host/LAN network filtering than the existing runtime provides.

An early lifecycle assertion checked the Jcode socket immediately after SSH
became available. Because startup is asynchronous, that check was retried with
a bounded readiness wait and passed without a code change.

The inherited force-staged ignored-file snapshot limitation is tracked as
`jbox-rr7`. An observed intermittent timeout-test failure is tracked as
`jbox-3yg`. A 109-byte SSH-agent socket path under a deliberately long isolated
XDG path failed with the Unix socket path-length limit, independently reproduced
with `ssh-agent -D -a`. Acceptance passed with a shorter isolated data path.
Better path handling and diagnostics are tracked as `jbox-fri`.
