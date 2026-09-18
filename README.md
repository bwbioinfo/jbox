# jbox

`jbox` creates disposable Kata microVM workspaces for [jcode](https://github.com/1jehuang/jcode). It uses session-specific Git worktrees as the only code persistence layer, never bind-mounting the launching checkout into the guest.

> **MVP status:** the Git isolation, state, Docker/Kata lifecycle, jcode SSH bridge, dedicated credentials, multi-repository config, inspection, safe cleanup, and TTL watcher are implemented. The target Arch/Manjaro host was validated with Kata 4 runtime-rs and Docker.

## Versioning

Jbox uses semantic versions in the form `a.b.c`. Additive features increment
the middle component, for example `0.2.0` to `0.3.0`. Compatible fixes and
documentation-only releases increment `c`, while breaking compatibility changes
increment `a`.

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
jbox init --tool ripgrep --tool jq
jbox .
```

`jbox init` creates a `.jbox.toml` for the current Git repository and, when it
does not already exist, a `.jbox/Dockerfile` that extends jbox's runtime image.
It never overwrites either file. Pass `--tool` repeatedly to install Debian
packages in the generated image, for example `jbox init --tool ripgrep --tool
jq`. For any other customization, edit the generated Dockerfile and add normal
Dockerfile instructions. Its content is hashed, so the next `jbox .` rebuilds
the project image automatically when it changes.

Every jbox base image includes the Beads CLI (`bd` and its `beads` alias),
installed by the upstream checksum-verifying installer. Run `bd init` from a
guest worktree when the project should use Beads issue tracking.

Declare one or more GitHub skill sources with `[[jcode.skills]]`. Jbox runs
`gh skill install` **inside the guest** before the Jcode daemon starts, placing
the skills in `~/.agents/skills`, where Jcode discovers them. Sources therefore
need `network.internet = true`, `git.network = true`, and
`git.credentials = "github-cli"`. The host GitHub CLI login is mounted only as
the guest's read-only `hosts.yml`, so private repositories such as
`bwbioinfo/skills` authenticate without exposing host SSH keys or the rest of
`~/.config`.

```toml
[[jcode.skills]]
# Omit `skill` to install all discovered skills from this private repository.
repository = "bwbioinfo/skills"

[[jcode.skills]]
repository = "K-Dense-AI/scientific-agent-skills"
skill = "scanpy"
# pin = "v1.2.3"
# allow_hidden_dirs = true
```

Jbox uses `--dir /home/jbox/.agents/skills` rather than a named `--agent`,
because GitHub CLI has no `jcode` agent target. Skill installations and their
GitHub CLI metadata are session-scoped. They never modify the host checkout or
host credentials. Provider and model selection is inherited from the local host
Jcode client when omitted. Project settings can override this in a
session-scoped guest Jcode configuration. The generated template pins OpenAI
`gpt-5.6-terra`, high reasoning effort, and fast mode off:

To make a skill a reliable default rather than merely an available choice, add
session-wide instructions. Jbox renders this as the guest's global
`~/AGENTS.md`, which Jcode loads after the project `AGENTS.md`; it does not
write to or alter any Git worktree:

```toml
[jcode.agent]
instructions = """
Use the `work-with-geonic` skill for all work in this workspace.
Use the `jbox` skill for workspace and isolation tasks.
Use the `jcode` skill for Jcode configuration and remote sessions.
"""
```

These instructions apply to new Jcode conversations in the jbox session.

```toml
[jcode]
default_provider = "openai"
default_model = "gpt-5.6-terra"
openai_reasoning_effort = "high"
openai_service_tier = "off" # Equivalent to Jcode's `/fast default off`.
```

Remove any of these keys to inherit that individual value from the host Jcode
client. The generated guest `config.toml` is session-scoped and contains no
credentials.

`jbox .` creates a session such as `bright-otter-a1b2c3`, prints each worktree branch and base commit, starts the guest, and opens the local jcode TUI. The VM remains alive after the TUI disconnects. Reconnect or inspect it with:

```bash
jbox ls # only sessions containing the current repository
jbox ls --all # every session across all repositories
jbox attach bright-otter-a1b2c3
jbox shell bright-otter-a1b2c3
# Omit the session in a repository. Jbox uses the only matching worktree,
# or presents a picker when multiple retained worktrees match.
jbox status
jbox diff # shows diffs for every repository in the selected Jbox project session
jbox resume # restarts a stopped guest, or attaches when its selected guest is already running
jbox status bright-otter-a1b2c3
jbox diff bright-otter-a1b2c3
jbox accept bright-otter-a1b2c3 --into main
# Select a session and accept every repository it contains.
jbox accept --all
jbox stop bright-otter-a1b2c3
jbox clean bright-otter-a1b2c3
```

When jbox opens Jcode or a guest shell from an interactive terminal, it first
renders a boxed **📦 JBOX GUEST** marker and sets the terminal title to
`[📦 JBOX] <session> · <workspace>`. This distinguishes the Kata-isolated
session in terminal tabs and window lists. Jcode's native remote header still
identifies the SSH host; jbox does not alter Jcode's own TUI theme.

From inside a host repository, every session-targeting command first finds only
retained sessions whose worktree belongs to that repository. If exactly one
matches, jbox states which worktree it selected and proceeds without a picker.
If several match, it presents the numbered selection UI. This applies to
`attach`, `shell`, `status`, `diff`, `stop`, `accept`, `accept --all`, `rebase`,
`resume`, `clean`, and `ls`. `jbox ls --all` is the explicit global inventory.
Operations that can change lifecycle or Git state still ask for their normal
final confirmation. `jbox accept` defaults to the currently
checked-out branch and accepts only that one repository from a multi-repository
session, avoiding a failed or accidental merge of sibling repositories. Pass a
session explicitly with
`jbox accept <session> --into <branch>` to retain the existing all-repository
acceptance workflow.

When an interactive single-repository `jbox accept` finds that its host branch
and guest branch diverged, it explains that a fast-forward is unavailable and
offers to create a merge immediately. Confirm the merge offer and the normal
final confirmation to proceed. If Git reports conflicts, the host merge is
paused while the jbox session stays available: resolve and stage the files, run
`jbox accept --continue`, or use `jbox accept --abort` to leave the session
unchanged. `--merge` remains available when you want to request a merge up
front, including direct session-ID and `--all` workflows.

`jbox accept --all` adds that all-repository workflow to the repository-scoped
selection UI. Select a session from any participating host repository, inspect
the complete branch plan, and confirm once. By default each host repository is
accepted into its own currently checked-out branch, so a session may span
repositories using different branch names. Pass `--into <branch>` to require
the same target in every repository. Jbox imports and preflights every snapshot
before fast-forwarding any host branch, and a running guest remains available
for further commits and later accepts.

Similarly, `jbox rebase` selects a session through the current repository, then
preflights **every** retained worktree before stopping its shared guest. With no
`--onto`, each worktree rebases onto the branch currently checked out in its own
host repository. `--onto <branch>` applies one explicit branch to every
repository. Jbox prints the complete plan and asks once. When the guest is
running, rebase states clearly that it takes the shared guest offline only
briefly, then automatically restarts it after every rebase succeeds. Attached
Jcode clients may display reconnecting during that maintenance window and then
resume their saved conversation. It never stops a guest
when a sibling has uncommitted work, unresolved conflicts, a detached host
checkout, or an invalid target branch. Git can still discover a content conflict
while rebasing one repository after the guest is stopped. In that case completed
earlier rebases and the stopped session are retained: resolve with `jbox resolve`,
run `git rebase --continue`, then rerun `jbox rebase` to continue the remaining
repositories.

Attach and shell consider running worktrees and begin in the selected guest
mount. Status and diff inspect only that worktree. Stop and clean remain
operations on the whole development machine, so their confirmations state how
many sibling repository worktrees they affect. `jbox clean --all` retains the
previous all-session maintenance workflow.

`jbox resume` considers stopped retained sessions containing the current
repository, then starts the selected session with its existing worktrees and image. It
creates fresh per-session SSH credentials, reuses completed session-local
skills, and never rebuilds the image. To preserve data, resume refuses a
retained worktree with uncommitted changes because recreating guest-only Git
metadata requires a hard reset. In an interactive terminal, jbox lists those
worktrees and offers to checkpoint their changes onto only their session
branches before resuming. Declining leaves all files and branches unchanged, so
you may instead commit or stash manually. `jbox resume <session>` is available for a direct
selection. If Docker or Kata stops a guest outside jbox, the next jbox command
detects the stale runtime, imports its committed guest history, marks the
session stopped, and makes it resumable. Transient Beads coordination locks do
not count as user changes.

If a retained worktree has a paused rebase or merge, jbox never checkpoints
conflict markers. From the affected host repository run `jbox resolve` and
select its worktree. This opens a **local host shell** in the generated
worktree, without starting a guest or mounting another path. Resolve and stage
the files, run `git rebase --continue` or `git merge --continue`, exit, then
run `jbox resume`.
restart from any directory.

When a session branch has been fast-forwarded or otherwise merged into another
local host branch, selectors label it `accepted` and `jbox clean` permits its
removal. A branch with uncommitted files or commits not reachable from another
local branch remains `changes` and still requires `--force`.

The current `.jbox.toml` must still resolve to the same repositories and guest
mount locations as when the session was created. If it does not, jbox leaves
the retained worktrees untouched and asks you to inspect, accept, or clean them
before creating a new session.

`jbox accept <session> --into <branch>` accepts the latest **committed** snapshot
from every session repository into the named local host branch without stopping
the guest. The target branch must already be checked out and clean in every
host repository, and acceptance is fast-forward only by default. Uncommitted
guest changes remain in the retained session and later commits can be accepted
again. A normal refusal never changes the host checkout.

### Accepting dirty work safely

Choose an explicit mode for the type of work that needs preserving:

```bash
# Commit visible agent changes in the generated jbox worktree, then accept.
jbox accept <session> --into main --checkpoint

# Preserve tracked and untracked edits in the normal host checkout, accept a
# fast-forward snapshot, then reapply those edits.
jbox accept <session> --into main --stash-host

# Explicitly merge a guest snapshot into a diverged host branch.
jbox accept <session> --into main --merge
jbox accept <session> --into main --stash-host --merge
```

`--checkpoint` commits only the generated jbox worktree. It never stages or
commits files in the normal host checkout. It refuses a detached worktree or
an unresolved merge/rebase rather than forcing a commit. `--stash-host` uses a
named, recoverable Git stash and drops it only after a successful reapply.

If `--merge` creates a host conflict, acceptance is **paused**, not discarded:
jbox lists every unresolved host file and retains the jbox session branch.
Resolve and stage the files in the host checkout, then run
`jbox accept --continue` and choose the matching worktree to create the merge
commit. To abandon only the host-side merge while retaining the jbox worktree,
run `jbox accept --abort` and choose that worktree. Neither option stops the

An internal detached watcher checks TTL every minute. TTL uses the most recent create, attach, shell, or accept timestamp. Expiry stops the guest and retains its worktrees. `jbox clean` refuses when a worktree has staged, unstaged, untracked, or post-base commits unless `--force` is explicit.

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
# Omit an individual key to inherit that setting from the host Jcode client.
default_provider = "openai"
default_model = "gpt-5.6-terra"
openai_reasoning_effort = "high"
openai_service_tier = "off"

[git]
network = true
credentials = "jbox"

[git.author]
# Uses host global Git user.name and user.email without mounting ~/.gitconfig.
inherit_host = true
# name = "Override Name"
# email = "override@example.com"

[[mounts]]
source = "./test-data"
target = "/data"
writable = false
```

Each repository is resolved and canonicalized before `git worktree add -b jbox/<session>/<repo>`. A dirty original checkout is safe: the worktree starts at its `HEAD`, not its uncommitted state. Submodules are initialized in the worktree. The host checkout itself is not mounted.

To deliberately bring current work in, use `jbox run --include-host-changes`. Jbox builds a private Git patch using a temporary index, then applies it only to its generated worktree. It includes staged, unstaged, and nonignored untracked files without writing to the host checkout. The guest sees the combined snapshot as ordinary **unstaged** work, so its original staged/unstaged split is normalized. Ignored files and changes inside nested submodules are not transferred. Because the host remains dirty, use `jbox accept --stash-host` when accepting a guest checkpoint that touches the same paths, or commit/stash the host work yourself first. Use an explicit `[[mounts]]` entry for large generated or ignored artifacts that should be available in a guest.

To test **uncommitted guest changes** with host-native tooling, run this from the participating original checkout:

```bash
jbox overlay <session>
# run native tests against the temporary host overlay
jbox overlay <session> --undo
```

`overlay` refuses a dirty or advanced host checkout, saves a private reversible patch under the retained session, and applies the guest changes as unstaged host files. `--undo` reverses only that patch, preserving unrelated test output. If the same overlaid files were edited while testing, undo refuses rather than overwrite them. Resolve those edits first, then retry. Jbox will not clean a session while its overlay remains active.

With no `image.dockerfile`, jbox copies the locally installed `jcode` launcher and distribution binary into a cached local Debian-based image. This bakes jcode, Git, certificates, SSH client/server, Bash, and the jbox guest entrypoint into the image. The image tag includes the local UID/GID so the guest workspace is writable without mounting host account state. For a project Dockerfile, jbox resolves that host-specific base and supplies it as `JBOX_BASE_IMAGE` automatically:

```dockerfile
ARG JBOX_BASE_IMAGE
FROM ${JBOX_BASE_IMAGE}
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
- Git defaults to a newly generated dedicated SSH key at `$XDG_DATA_HOME/jbox/credentials/git/id_ed25519`, not a copied host key or forwarded agent. Register its `.pub` file with the Git provider before guest `git push` works.
- Guest worktrees receive only `user.name` and `user.email` from the host global Git configuration. jbox writes those values to the isolated guest Git metadata and never mounts the host `.gitconfig`. `[git.author]` can override either field per project.

### GitHub CLI HTTPS credential mode

Set `git.credentials = "github-cli"` to let a guest authenticate to GitHub over
HTTPS using an existing host `gh auth login`. jbox mounts only
`$XDG_CONFIG_HOME/gh/hosts.yml` read-only at the guest's GitHub CLI location and
uses `gh auth setup-git` to configure Git's HTTPS credential helper. It does not
mount the rest of the GitHub CLI configuration, any Git credential helper store,
`~/.ssh`, or an SSH agent. In this mode jbox does not create or mount its
dedicated Git SSH key. The guest can use the bearer token in `hosts.yml`, so
enable this only for a Kata guest and repository configuration you trust.

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
