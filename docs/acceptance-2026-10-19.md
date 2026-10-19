# Jcode image/version acceptance, October 19, 2026

## Report and cause

The reported native SSH client was Jcode `v0.99.0 (774613c90)`, but the
running `crisp-yak-beeaf9` guest contained `v0.98.1 (1c7426eb1)`. Its server
rejected the newer client's `account_usage` request. Jbox itself was `0.15.1`.

The base-image tag hashed bundled templates and release manifests, but not the
local Jcode launcher or executable copied into the image. A cache hit occurred
before discovering the local distribution, so a Jcode-only update could reuse
an older base image and its dependent project image. Existing retained sessions
also deliberately keep their original image until explicitly refreshed.

## Changes in Jbox 0.15.2

- Resolve the local launcher symlink and snapshot its adjacent Linux executable
  before consulting the image cache. Hash both staged files by content, as well
  as all existing bundled inputs. Executable hashing uses a bounded buffer.
- Build from a private temporary context, so concurrent builds and subsequent
  host upgrades cannot overwrite the already-hashed Docker inputs. Temporary
  contexts are removed after success, cache hits, and errors.
- Launch the guest daemon by its absolute baked path with `--no-update`, and
  smoke-check that executable during the image build.
- Before attaching, query local and guest executable versions with automatic
  updates disabled. Compare the complete version/build identity, reject failed
  or malformed queries, and impose a 15-second bound on each query.
- Disable automatic updates for the final local SSH client invocation too.
  Reject mismatches with explicit `stop` and `resume --refresh-image` commands,
  without restarting the guest or changing its worktrees, image selection,
  conversation marker, or activity timestamp.

The executable preflight relies on the normal Jbox lifecycle: the daemon starts
from the absolute image executable on a read-only root filesystem, with automatic
update checks disabled. The native SSH bridge subsequently performs its own
active-daemon capability negotiation. This does not attest to an arbitrary
manually replaced daemon or a custom image that overrides that lifecycle. No
additional debug-control permissions were enabled.

## Observed checks

| Requirement | Observation |
| --- | --- |
| Launcher or executable upgrades invalidate images | Unit tests change each independently, including a same-length binary-only change at the same path. Both base fingerprint and dependent project fingerprint change. |
| Symlinked installations retain matching distribution files | Unit test switches a launcher symlink between two release directories and observes distinct executable fingerprints. |
| Concurrent contexts and host changes cannot alter a staged build | Independent staging paths have the same fingerprint for unchanged inputs. Later host changes leave earlier snapshots unchanged. Dropping a context removes it. |
| Broken local installations do not hide behind a cache hit | Staging rejects a missing adjacent executable before the Docker lookup. |
| Matching attachment still preserves continuity | Real CLI regression fixture verifies mount and resume arguments through fake SSH, with `--no-update` required on both checks and final launch. |
| Mismatch and failed queries are safe | CLI regression covers old release, same release/different build, empty output, and nonzero query exit. No TUI launch or Docker lifecycle mutation occurs, and session state/activity/image/markers remain unchanged. Unit tests also cover multiline/control-character output and a hung process. |
| Reported live mismatch is recognized | The actual patched CLI rejects `crisp-yak-beeaf9`, reporting exact local `0.99.0` and guest `0.98.1` identities and the explicit refresh commands. The user's existing guest remains running. |
| Fresh real guest gets the host build | A real default image and dependent project image were built. An isolated, credential-free Kata guest reported `jcode v0.99.0 (774613c90)`, exactly matching the host. |
| Real native SSH handshake sees the matching daemon | In scratch session `kind-alpaca-453755`, the actual native client printed `Remote: jbox@127.0.0.161` with workspace `/workspace/project` and `v0.99.0 (774613c90)`. It then refused the deliberately noninteractive terminal. This verifies the handshake, not interactive TUI operation or provider inference. |
| Repeated invocation reuses the recorded guest | A second real `run --no-attach` retained the same container ID. The owned acceptance session was then stopped and cleaned successfully. |

Built images:

```text
jbox/jcode:local-v11-1001-1001-2a6fa6e051d303d9
jbox/project:31253bceeeffc84b
```

The real native acceptance log is
`/home/geonic/.jcode/scratch/vf.6aZCNG/acceptance.log`.
All HOME/XDG state, test Git branches, SSH keys, and known-host entries belonged
to the isolated fixture. A private SSH wrapper selected only the fixture's
verified known-hosts file, because OpenSSH's default home lookup does not follow
an overridden HOME. Host-key verification was not disabled. No personal provider
credentials were imported, and no inference or OAuth flow was requested.

An initial fixture path exceeded the SSH-agent Unix socket path limit. The
shorter isolated path passed. Actionable diagnostics for this separate issue
are tracked in `jbox-7zy`.

## Quality gates

All required gates passed on the final source changes:

```text
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets                 # 187 Rust tests
cargo run -- --help
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*.py' -v
                                        # 30 Python tests
git diff --check
```

## Existing-session recovery

Jbox does not automatically interrupt running sessions. After installing the
updated CLI, the affected session requires an explicit refresh:

```bash
jbox stop crisp-yak-beeaf9
jbox resume crisp-yak-beeaf9 --refresh-image
```

Run these from a participating repository. Review/checkpoint dirty worktrees
when prompted. Worktrees and branches are retained. Saved Jcode conversation
continuity requires `jcode.persistent_credentials = true`, the default. In-flight
processes do not survive stopping a VM.
