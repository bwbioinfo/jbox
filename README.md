# jbox

`jbox` creates disposable Kata microVM workspaces for [jcode](https://github.com/1jehuang/jcode). It uses session-specific Git worktrees as the only code persistence layer, never bind-mounting the launching checkout into the guest.

> **MVP status:** the Git isolation, state, Docker/Kata lifecycle, jcode SSH bridge, dedicated credentials, multi-repository config, inspection, safe cleanup, and TTL watcher are implemented. The target Arch/Manjaro host was validated with Kata 4 runtime-rs and Docker.

## Versioning

Jbox uses semantic versions in the form `a.b.c`. Additive features increment
the middle component, for example `0.2.0` to `0.3.0`. Compatible fixes and
documentation-only releases increment `c`, while breaking compatibility changes
increment `a`.

## Contributing

The repository pins its Rust toolchain and quality components in
[`rust-toolchain.toml`](rust-toolchain.toml). See the
[contributor development guide](CLAUDE.md#build--test) for the required local
formatting, Clippy, test, and CLI checks before submitting changes.

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

Install Kata yourself because package installation modifies the host and needs
administrator access. Then use `jbox prime` to load its guest-networking
modules for the current boot:

```bash
yay -S kata-all-bin
jbox prime
```

`jbox prime` explicitly invokes `sudo modprobe vhost_vsock` and `sudo modprobe
vhost_net`, verifies the result with `jbox doctor`, and never changes persistent
host configuration.
`/dev/kvm` and `/dev/vhost-vsock` must be available. Persist the loaded modules
after a successful smoke test with a root-owned
`/etc/modules-load.d/kata-containers.conf` containing `vhost_vsock` and
`vhost_net`.

When interactive session startup discovers either required module is missing,
jbox offers to run `jbox prime` before it creates a session. Non-interactive
invocations never invoke `sudo`; they fail with the command to run first.

`kata-all-bin` 4.x packages the supported `runtime-rs` shim at
`/opt/kata/runtime-rs/bin/containerd-shim-kata-v2`. Register that shim with Docker
using the packaged QEMU runtime-rs configuration. This complete command safely
updates the existing `/etc/docker/daemon.json`, preserving other valid JSON
settings and runtimes such as NVIDIA, then restarts and verifies Docker:

```bash
sudo python3 - <<'PY'
import json
from pathlib import Path

path = Path("/etc/docker/daemon.json")
path.parent.mkdir(parents=True, exist_ok=True)
config = {} if not path.exists() else json.loads(path.read_text())
runtimes = config.setdefault("runtimes", {})
runtimes["kata"] = {
    "runtimeType": "/opt/kata/runtime-rs/bin/containerd-shim-kata-v2",
    "options": {
        "ConfigPath": "/opt/kata/share/defaults/kata-containers/runtime-rs/configuration-qemu-runtime-rs.toml"
    },
}
path.write_text(json.dumps(config, indent=2) + "\n")
PY
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

Install Jbox from its source checkout:

```bash
cargo install --path .
```

Then, from the root of the Git repository you want to isolate, run:

```bash
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

The default Debian guest tooling also includes `rustfmt`, so Rust formatting
checks can run inside a newly built Jbox image. Existing running guests retain
their current image and toolchain until they are recreated.

The generated template installs every discoverable skill from
`bwbioinfo/skills`. Add other GitHub sources with `[[jcode.skills]]`. Jbox runs
`gh skill install` **inside the guest** before the Jcode daemon starts, placing
the skills in `~/.agents/skills`, where Jcode discovers them. Every skill source
needs `network.internet = true`. Set `private = true` for a source that requires
authentication. Jbox then requires either legacy `git.credentials =
"github-cli"` or an exact scoped guest-passthrough clone grant. Public sources
default to `private = false` and do not require a guest token.

Legacy `github-cli` mode mounts the host login as a guest read-only `hosts.yml`.
It does not expose host SSH keys or the rest of `~/.config`, but it is broad:
guest programs can use the bearer token with every permission it has. It is not
repository-scoped by `.jbox.toml`.

```toml
[[jcode.skills]]
# Omit `skill` to install all discovered skills from this private repository.
repository = "bwbioinfo/skills"
private = true

[[jcode.skills]]
repository = "K-Dense-AI/scientific-agent-skills"
skill = "scanpy"
# pin = "v1.2.3"
# allow_hidden_dirs = true
# private = false
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
Use the `jcode-jbox` skill for Jbox workspace, lifecycle, isolation, credential, Jcode configuration, remote-session, and authentication tasks.

Use subscription-backed providers efficiently and only within their terms.
At session start, before spawning workers, and after a provider error, check
`jcode auth status --json` and `jcode usage --json` in the guest. Treat only
guest-reported, authenticated providers and their live allowance windows as eligible.

For substantial work, split independent tasks and route new swarm workers to
the appropriate viable provider and model. Prefer the provider with the most
available included capacity that can complete the task reliably. Reserve scarce
or stronger capacity for planning, integration, difficult debugging, and
adversarial review. Use another viable provider for bounded implementation,
research, bulk reading, and mechanical verification.

Keep an active conversation on its provider unless a fresh worker has a clean
handoff. When a provider approaches a limit, stop assigning it new work and
route eligible new tasks to another viable provider. Do not create work merely
to consume allowance, retry quota failures to evade limits, use unapproved
accounts, or enable API or extra paid usage without explicit user approval.

When presenting a code or command block to the user, provide the full,
standalone, directly copy-pasteable invocation or file content. Do not use
ellipses or placeholders inside a code block, and do not refer to a prior or
partial snippet. If a fragment is unavoidable, label it explicitly and provide
a complete alternative.
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

`jbox .` creates a session on first use, prints each worktree branch and base
commit, starts the guest, and opens the local jcode TUI. On subsequent launches
from the same original Git repository (including its subdirectories), it
reconnects to the most recently active retained jbox session instead of creating
another one. A running guest is attached directly; a stopped guest is restarted
from its retained worktree, then attached. Jbox resumes the Jcode conversation
recorded for that repository mount, not a sibling repository in the same VM.
Jcode's saved conversation survives a guest restart when
`jcode.persistent_credentials = true` (the default). If no conversation has
been recorded yet, Jcode starts a new one. With persistence disabled, a
stopped guest retains its worktrees but starts a new Jcode conversation on
restart because its previous Jcode home was ephemeral. The VM remains
alive after the TUI disconnects. Use `jbox --new .` (or `jbox run --new PATH`)
to create a separate fresh workspace. `--include-host-changes` requires `--new`
so it cannot be silently ignored during reconnection. `--no-attach` reuses or
restarts the guest without opening Jcode. Cleaning a session removes its
worktree and continuity marker, so it is not eligible for automatic reconnection.
Run each of the following complete commands from a participating
host repository. Jbox selects its only matching session or presents a picker:

```bash
jbox ls
jbox ls --all
jbox attach
jbox shell
jbox status
jbox diff
jbox resume
jbox accept
jbox accept --all
jbox stop
jbox clean
```

An explicit `jbox attach SESSION` invoked from a participating repository also
opens that repository's guest workspace. When invoked elsewhere, it opens the
session's primary workspace.

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
resume the saved conversation for their repository workspace. Each repository
mount keeps its own Jcode conversation, so reattaching from one project
repository never resumes an agent working in a sibling repository. For an older
multi-repository session with a single shared continuity marker, Jbox retains
that conversation but starts a fresh repository-scoped conversation instead of
guessing its workspace. It never stops a guest
when a sibling has uncommitted work, unresolved conflicts, a detached host
checkout, or an invalid target branch. Git can still discover a content conflict
while rebasing one repository after the guest is stopped. In that case completed
earlier rebases and the stopped session are retained: resolve with `jbox resolve`,
run `git rebase --continue`, then rerun `jbox rebase` to continue the remaining
repositories.

### Synchronizing every project repository

`jbox sync` selects a retained session from any participating checkout, then
plans and operates on **every** repository in that session, including the
primary repository and all `[[repos]]` entries. Each host checkout must be on a
clean branch with a configured upstream. Jbox uses that exact upstream, never
guesses `origin`, and never assumes matching local and remote branch names.

```bash
# From any participating original host checkout, inspect every configured
# upstream, worktree, preview, overlay, and required recovery action.
jbox sync --dry-run

# Checkpoint visible generated-worktree files to session branches, then
# synchronize host branches, rebase guests, fast-forward accept, and push.
jbox sync --checkpoint
```

The first command contacts each configured upstream with a non-mutating fetch
check and prints the complete project plan. If any repository is detached,
dirty, missing an upstream, has a preview or overlay, has an unresolved Git
operation, or cannot reach its upstream, Jbox does not stop the guest or change
any host branch. It reports each blocker and its shortest recovery path. A
normal sync refuses visible guest files unless `--checkpoint` is explicit, so
it never creates an unexpected agent commit.

After confirmation, Jbox freezes a running guest, runs host `pull --rebase`
against each repository's configured upstream, rebases every guest worktree,
and fast-forward accepts each snapshot. Rebasing changes only unpushed local
commit IDs, never uses a force push, and stops for reviewed conflict resolution.
Jbox asks separately before pushing because independent remotes cannot be
atomic. Pass `--yes` only when both local rewrites and every normal push are
intended.

If Git pauses or a later remote push is rejected, the session remains stopped
with a durable per-repository synchronization journal. Resolve and stage the
reported rebase, run `git rebase --continue`, then run `jbox sync --continue`
from the affected original checkout. A continuation retries only unfinished
repositories. When a remote moved after local acceptance, it rebases the host
again and realigns the clean accepted guest branch before retrying the push, so
it does not duplicate guest commits. `jbox sync --abort` aborts paused rebases
only. It intentionally retains host updates, accepted commits, and any pushes
that already completed. While this journal exists, `jbox resume` and
`jbox clean` refuse so a new guest lifetime or cleanup cannot bypass unfinished
remote synchronization.

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
# Commit visible agent changes in the generated jbox worktree, then accept it
# into the current host branch.
jbox accept --checkpoint

# Preserve tracked and untracked edits in the normal host checkout, accept a
# fast-forward snapshot, then reapply those edits.
jbox accept --stash-host

# Explicitly merge a guest snapshot into a diverged host branch.
jbox accept --merge
jbox accept --stash-host --merge
```

`--checkpoint` commits only the generated jbox worktree. It never stages or
commits files in the normal host checkout. It refuses a detached worktree or
an unresolved merge/rebase rather than forcing a commit. `--stash-host` uses a
named, recoverable Git stash and drops it only after a successful reapply.

### Previewing host-native changes

For the common edit-test-accept loop, use `preview` rather than `overlay`.
`preview` stops the guest and opens a local host shell in a disposable
worktree containing the session's current committed and uncommitted changes.
The original host checkout remains clean.

```bash
# From the original host repository, create a host-native preview and run tests.
jbox preview

# A preview that did not change source files closes, restarts the guest, and
# reconnects to the recorded Jcode conversation automatically. If source files
# changed, exit leaves the preview open for the explicit acceptance below.

# From the original host repository, make preview edits authoritative and
# accept them into the currently checked-out host branch.
jbox accept

# Or discard the old preview and create a fresh one from the retained session.
jbox preview

# Resume the Jcode guest after accepting the preview.
jbox resume
```

When the preview shell exits with no visible source changes, Jbox discards the
pristine preview, restarts the guest, and reopens the recorded Jcode
conversation. This keeps a test-only preview from ending the active Jcode
workflow. When it contains edits or commits, it remains open and the guest
stays stopped so those changes cannot be silently adopted: run
repository-scoped `jbox accept` to checkpoint and adopt its exact state, then
run `jbox resume` to return to Jcode. Running `jbox preview` again asks before
discarding preview edits and rebuilding it. `jbox resume` and `jbox clean`
refuse while a changed preview exists, preventing an unreviewed preview from
being silently lost. Preview checkpoint commits use the configured Jbox Git
author, including any `.jbox.toml` override. `jbox overlay` remains available
for one-off patch testing of the primary checkout.

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
# Prevent the legacy Jbox SSH identity and broad host GH passthrough from
# bypassing this repository-scoped policy.
credentials = "none"

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
jbox overlay
# run native tests against the temporary host overlay
jbox overlay --undo
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
- User-configured extra mounts canonicalize symlinks and reject paths equal to, within, or containing an original repository, protected credential/configuration directory, XDG-derived jbox state/cache, runtime socket, or host system/device tree. This includes `$HOME`, `$HOME/.ssh`, `$HOME/.aws`, the active XDG configuration directory, `/run/user`, SSH-agent paths, Docker/Podman sockets, and `/`. Direct socket/device mounts are rejected. Disjoint artifact directories remain allowed. Jbox's own narrowly scoped worktree and credential mounts are separate from this extra-mount policy.
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

### Repository-scoped GitHub profiles and grants

New configurations can describe the intended GitHub account and the exact
repository operations it may perform with `[[git.credential_profiles]]` and
`[[git.repository_grants]]`. Profile capability fields are an expectation for
the externally-issued credential. A grant is constrained to one exact
`OWNER/REPO`, names its profile, and declares the Git, pull-request, Actions,
and checks capabilities plus permitted operations. Jbox validates that the
declarations are internally consistent, including that a grant cannot ask for
more than its profile declares.

```toml
[git]
network = true
credentials = "none"

[[git.credential_profiles]]
name = "project-maintainer"
provider = "github-cli"
host = "github.com"
account = "example-maintainer"
storage = "jbox-managed"
expected_contents = "write"
expected_pull_requests = "write"
expected_actions = "read"

[[git.repository_grants]]
id = "project"
repository = "example-org/example-project"
remote = "origin"
credential_profile = "project-maintainer"
delivery = "guest-passthrough"
git = "write"
pull_requests = "write"
actions = "read"
checks = "read"
allowed_operations = ["clone", "fetch", "push-branch", "pr-view", "pr-status", "ci-view", "pr-create", "pr-update"]
allowed_push_ref_prefixes = ["refs/heads/jbox/"]
protected_refs = ["refs/heads/main"]
force_push = false
merge = "deny"
```

This is a migration path away from `git.credentials = "github-cli"`, not a
way to downscope its existing host token. `git.credential_profiles` therefore
requires `git.credentials = "none"`: Jbox will neither mount the legacy
dedicated SSH identity nor the complete host `gh` token store beside the scoped
profile.

Authenticate the named account with the host GitHub CLI. On a **new** session,
Jbox reads only that TOML-selected account through `gh`, writes an isolated
single-account profile below Jbox-managed state, and mounts it read-only in the
guest. No manual import command is needed for normal launch:

```bash
jbox .
jbox credentials github status example-maintainer
```

Provisioning asks `gh` for **only** the named account and writes a
single-account, mode-0600 `hosts.yml`. It never modifies the host GH
configuration. Existing valid managed profiles are retained, including over
session resume, so a host credential is never silently refreshed into a
long-lived guest. Use `jbox credentials github import example-maintainer --yes
--replace` only when you deliberately want to refresh the managed copy.

A session may select only one guest-passthrough profile, so project and private
skills grants that need guest access must share a least-privilege account. A
skill declared with `private = true` additionally needs a matching
guest-passthrough grant with `git = "read"` and `allowed_operations` including
`"clone"`. Public skills leave `private` unset or false and need no profile.

A guest-passthrough profile gives guest programs a usable bearer token. Obtain
a new fine-grained GitHub token restricted to the declared account,
repositories, and minimum permissions before using it. TOML declarations do
not reduce the permissions embedded in a token that a guest can read, nor can
they prevent arbitrary guest `gh` commands that the token itself permits.
GitHub's actual fine-grained token permissions, repository access, and branch
protections are the enforcement boundary.

`brokered` delivery is reserved for host-side control-plane work. Brokered
credentials are never mounted into the guest, and Jbox does not yet execute any
remote brokered action. It does provide an auditable, no-network preflight that
checks an exact configured remote, its GitHub `OWNER/REPO` URL, the requested
operation, and any push-ref restrictions against the session's frozen policy:

```bash
jbox policy plan SESSION_ID --repository jbox --remote origin --operation push-branch --ref refs/heads/jbox/example
jbox policy plans SESSION_ID
```

Each allowed or denied dry run is stored in that session's state journal. It
does not read a credential, invoke `gh`, push, create a pull request, or query
GitHub. A future remote brokered merge requires `merge = "user-confirmed"`, a
brokered delivery grant, and pull-request write capability. It must still
obtain explicit user confirmation before merging. Jbox freezes the complete
resolved policy in each session and rejects a resume if `.jbox.toml` changes
it, preventing a retained guest from silently gaining new policy authority.

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
