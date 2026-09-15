# jbox

`jbox` creates disposable Kata microVM workspaces for [jcode](https://github.com/1jehuang/jcode). It uses session-specific Git worktrees as the only code persistence layer, never bind-mounting the launching checkout into the guest.

> **MVP status:** the Git isolation, state, Docker/Kata lifecycle, jcode SSH bridge, dedicated credentials, multi-repository config, inspection, safe cleanup, and TTL watcher are implemented. The target Arch/Manjaro host was validated with Kata 4 runtime-rs and Docker.

## Implementation plan and result

1. Resolve a versioned project config, canonicalize every path, and create a branch/worktree for every participating repository.
2. Build or reuse a local OCI image with jcode and SSH baked in.
3. Run that image under Kata, exposing only generated worktrees and explicit approved mounts.
4. Connect the local jcode TUI to the guest daemon through an ephemeral SSH key.
5. Persist only dedicated jbox credentials and Git worktrees. Stop expired VMs without discarding changes.

The shipped Rust implementation follows this plan. It has narrow `Engine` and `ContainerSpec` contracts, keeping the Kata runtime separate from CLI, config, Git, image, state, credentials, and path validation layers.

## Why Docker + Kata for this MVP

The initial preference was Podman + Kata. Current Arch-based Kata deployment is rootful, however, while Podman's strongest host-isolation networking configuration is rootless. This makes the combination unsuitable as the default secure path on this target. Docker is installed on this host and its Kata runtime registration is a direct, supported OCI-runtime integration, so this MVP selects **Docker + Kata** behind the engine abstraction.

Host investigation found hardware virtualization available (`/dev/kvm`, Intel VT-x, and `kvm_intel`). The current AUR package is [`kata-all-bin`](https://aur.archlinux.org/packages/kata-all-bin). Kata's [quick start](https://kata-containers.github.io/kata-containers/quick-start-guide/) and [installation guide](https://kata-containers.github.io/kata-containers/installation/) describe the runtime model. Kata 4.x defaults to the `runtime-rs` shim, while the older Go `kata-runtime` remains available but deprecated.

### Host setup for Arch/Manjaro

Run these commands yourself because they modify the host and need administrator access:

```bash
yay -S kata-all-bin
sudo modprobe vhost_vsock vhost_net
```

`/dev/kvm` and `/dev/vhost-vsock` must be available. The commands above load the
Kata VSOCK and guest-networking modules for the current boot. Persist them after a
successful smoke test with a root-owned `/etc/modules-load.d/kata-containers.conf`
containing `vhost_vsock` and `vhost_net`.

`kata-all-bin` 4.x packages the supported `runtime-rs` shim at
`/opt/kata/runtime-rs/bin/containerd-shim-kata-v2`. Register that shim with Docker
using the packaged QEMU runtime-rs configuration. Merge this `runtimes` entry into
an existing `/etc/docker/daemon.json`, rather than overwriting any existing daemon
settings. In particular, retain existing runtimes such as NVIDIA:

```json
{
  "runtimes": {
    "kata": {
      "runtimeType": "/opt/kata/runtime-rs/bin/containerd-shim-kata-v2",
      "options": {
        "ConfigPath": "/opt/kata/share/defaults/kata-containers/runtime-rs/configuration-qemu-runtime-rs.toml"
      }
    }
  }
}
```

Then restart Docker and verify it before using jbox:

```bash
sudo systemctl restart docker
docker info --format '{{json .Runtimes}}'
docker run --runtime kata --rm alpine:latest uname -r
jbox doctor
```

The `ConfigPath` avoids needing a system-wide copy. If a local override is required,
the correct source for this package is
`/opt/kata/share/defaults/kata-containers/runtime-rs/configuration-qemu-runtime-rs.toml`,
not the absent legacy `configuration-qemu.toml`. `jbox doctor` refuses to launch
unless Docker reports a runtime named `kata`.

## Use

```bash
cargo install --path .
cd ~/src/project
jbox .
```

`jbox .` creates a session such as `bright-otter-a1b2c3`, prints each worktree branch and base commit, starts the guest, and opens the local jcode TUI. The VM remains alive after the TUI disconnects. Reconnect or inspect it with:

```bash
jbox ls
jbox attach bright-otter-a1b2c3
jbox shell bright-otter-a1b2c3
jbox status bright-otter-a1b2c3
jbox diff bright-otter-a1b2c3
jbox stop bright-otter-a1b2c3
jbox clean bright-otter-a1b2c3
```

An internal detached watcher checks TTL every minute. TTL uses the most recent create, attach, or shell timestamp. Expiry stops the guest and retains its worktrees. `jbox clean` refuses when a worktree has staged, unstaged, untracked, or post-base commits unless `--force` is explicit.

## `.jbox.toml`

Configuration lives in the primary repository root. Unknown fields and malformed paths fail closed.

```toml
version = 1

[runtime]
backend = "kata"

[resources]
cpus = 8
memory = "16G"
disk = "40G"   # recorded for future VM disk sizing, not enforced by Docker/Kata yet
ttl = "24h"

[image]
dockerfile = ".jbox/Dockerfile"

[workspace]
mount = "/workspace/project"

[[repos]]
path = "../infrastructure"
mount = "/workspace/infrastructure"

[[repos]]
path = "../api"
mount = "/workspace/api"

[network]
internet = true
host = false
lan = false

[jcode]
persistent_credentials = true

[git]
network = true
credentials = "jbox"

[[mounts]]
source = "./test-data"
target = "/data"
writable = false
```

Each repository is resolved and canonicalized before `git worktree add -b jbox/<session>/<repo>`. A dirty original checkout is safe: the worktree starts at its `HEAD`, not its uncommitted state. Submodules are initialized in the worktree. The host checkout itself is not mounted.

With no `image.dockerfile`, jbox copies the locally installed `jcode` launcher and distribution binary into a cached local Debian-based image. This bakes jcode, Git, certificates, SSH client/server, Bash, and the jbox guest entrypoint into the image. The image tag includes the local UID/GID so the guest workspace is writable without mounting host account state. A project Dockerfile should begin with the tag printed by `jbox`, for example:

```dockerfile
FROM jbox/jcode:local-v4-1001-1001
RUN apt-get update && apt-get install -y --no-install-recommends ripgrep
```

Project image tags include the Dockerfile SHA-256 and are reused when unchanged.

## Security model and current limitations

Implemented invariants:

- The original checkout is never passed to Docker. Only jbox-owned worktrees under `$XDG_DATA_HOME/jbox/sessions/<id>/worktrees` are mounted writable.
- Extra mounts canonicalize symlinks and reject original repositories, `$HOME/.ssh`, `$HOME/.aws`, `$HOME/.config`, jbox state, `/run/user`, SSH-agent paths, Docker/Podman sockets, and `/`.
- Extra mounts default to read-only. Guest targets must be absolute, non-root paths without traversal components.
- Containers use Kata, drop all Linux capabilities and add back only `SETGID`, `SETUID`, `SYS_CHROOT`, `CHOWN`, `AUDIT_WRITE`, and `KILL`. Debian `sshd` needs these to drop privileges, use its pre-auth chroot, allocate an interactive PTY, write its guest audit record, and clean up a UID-switched session. They also use `no-new-privileges`, a read-only root filesystem, restrictive tmpfs mounts, a PID limit, and no `--privileged`, devices, SSH-agent forwarding, or runtime socket mounts.
- SSH is published to a session-specific `127.0.0.0/8` loopback address. Every session has a fresh local bridge SSH key and dedicated local SSH agent, stored 0600/0700 in its session directory.
- Current jcode native SSH uses the normal OpenSSH known-hosts database and has no per-connection known-hosts option. jbox therefore appends a tagged, session-specific loopback host key (`# jbox:<session>`) to the host's `~/.ssh/known_hosts`, then removes exactly that tagged entry on `stop` or `clean`. It never mounts the host `.ssh` directory or forwards its SSH agent to the guest.
- Jcode auth persists only in `$XDG_DATA_HOME/jbox/credentials/jcode`, not the user's normal jcode configuration. The first guest login populates this dedicated state.
- Git uses a newly generated dedicated key at `$XDG_DATA_HOME/jbox/credentials/git/id_ed25519`, not a copied host key or forwarded agent. Register its `.pub` file with the Git provider before guest `git push` works.

### Explicit local provider import

Jcode's built-in SSH import only transfers selected Jcode-managed OpenAI or Claude OAuth logins. To make the other supported locally configured providers available to **new** jbox guests, use the explicit jbox importer:

```bash
jbox credentials import --all        # preview only, copies nothing
jbox credentials import --all --yes  # one-time copy into jbox-managed state
```

It copies only a documented allowlist of provider credential stores: Jcode OAuth files, Jcode provider `*.env` API-key files, and supported external stores for Codex, Claude Code, Gemini CLI, GitHub Copilot, OpenCode, pi, OpenClaw, and Hermes. Jcode-managed files and API-key files become available directly. External-client stores are staged at their documented guest paths for Jcode's guest-side external-source consent flow, so they are not claimed configured until `jcode auth status --json` in the guest reports them available. It never mounts or copies `~/.ssh`, arbitrary Jcode configuration, shell configuration, keyrings, or an entire home directory. Existing jbox-managed credential copies are retained unless `--replace` is explicit. The copied credentials live under `$XDG_DATA_HOME/jbox/credentials/jcode`, owned 0700 with files mode 0600, and are mounted into guests from there rather than from the host locations.

The operation intentionally requires `--yes` because it gives the disposable guest usable provider credentials. Credentials can refresh independently and OAuth refresh-token rotation can invalidate a host login. Host-local endpoint providers such as LM Studio are not imported because the guest's `localhost` is not the host and `host = false` forbids relying on that connection.

**Network limitation:** Docker's normal bridge provides the required Internet access, but it cannot by itself prove host and LAN denial. jbox does not claim that it can. Apply host firewall rules to the Docker bridge before treating `host = false` and `lan = false` as strict policy. `internet = false` does enforce Docker `--network none`. Networking is explicitly isolated in the engine policy so a future rootless Podman, namespace firewall, or dedicated egress gateway backend can enforce the full policy.

**Resource limitation:** CPU and memory limits are enforced by Docker. The declared disk size is retained in state/config but not enforced by the Docker/Kata MVP.

## Verification

```bash
cargo fmt --check
cargo test
cargo clippy -- -D warnings
cargo run -- doctor
```

The test suite verifies TTL/config path validation and proves that a worktree starts from `HEAD` rather than a dirty host checkout. End-to-end validation on the target host booted a Kata guest, connected the local jcode TUI through the managed loopback SSH bridge, verified the guest jcode daemon and Git worktree, and then exercised `status`, `stop`, and safe `clean`.
