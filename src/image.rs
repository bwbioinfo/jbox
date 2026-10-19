use crate::{config::Config, paths::JboxPaths};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::process::Command;

pub struct ImageManager<'a> {
    paths: &'a JboxPaths,
}

const JBOX_BEADS_PRE_COMMIT: &str = r#"#!/bin/sh
set -eu
jsonl=.beads/issues.jsonl
prefix="$(bd config get issue_prefix)"
if [ -z "$prefix" ] || [ ! -s "$jsonl" ]; then
    echo "jbox: refusing Beads hook without issue_prefix or nonempty JSONL" >&2
    exit 1
fi
backup="$(mktemp)"
candidate="$(mktemp)"
expected="$(mktemp)"
actual="$(mktemp)"
trap 'status=$?; if [ "$status" -ne 0 ] && [ -s "$backup" ]; then cp -f "$backup" "$jsonl"; fi; rm -f "$backup" "$candidate" "$expected" "$actual"; exit "$status"' EXIT
cp "$jsonl" "$backup"
ids() {
    awk -F '"' '{ for (i=1; i<NF; i++) if ($i=="id") { print $(i+2); break } }' "$1" | sort -u
}
safe_export() {
    ids "$backup" > "$expected"
    ids "$candidate" > "$actual"
    [ -s "$expected" ] && [ -s "$actual" ] || return 1
    missing="$(comm -23 "$expected" "$actual")"
    [ -z "$missing" ] || return 1
    old_memories="$(grep -c '"_type"[[:space:]]*:[[:space:]]*"memory"' "$backup" || :)"
    new_memories="$(grep -c '"_type"[[:space:]]*:[[:space:]]*"memory"' "$candidate" || :)"
    [ "$new_memories" -ge "$old_memories" ]
}
bd export --include-memories > "$candidate"
if ! safe_export; then
    echo "jbox: refusing Beads hook: database would lose JSONL issues or memories" >&2
    exit 1
fi
export BD_GIT_HOOK=1
bd hooks run pre-commit "$@"
cp "$jsonl" "$candidate"
if ! safe_export; then
    echo "jbox: restored JSONL after unsafe Beads hook export" >&2
    exit 1
fi
"#;

const JBOX_ENTRYPOINT: &str = r#"#!/bin/sh
set -eu
/usr/bin/mkdir -p /run/sshd
# OCI creates ancestors of nested bind mounts as root inside the tmpfs home.
# Repair only these fixed guest directories, never a bind-mounted directory or
# its contents. In particular, .jcode can itself be a persistent credential mount.
guest_home_backing() {
    /usr/bin/awk -v path="$1" '
        length($5) > longest && ($5 == "/" || path == $5 || index(path, $5 "/") == 1) {
            longest = length($5)
            backing = $5
            for (field = 6; field <= NF; field++) {
                if ($field == "-") { kind = $(field + 1); break }
            }
        }
        END { print backing, kind }
    ' /proc/self/mountinfo
}
if [ -L /home/jbox ] || [ "$(guest_home_backing /home/jbox)" != "/home/jbox tmpfs" ]; then
    echo 'jbox: refusing guest home initialization without its dedicated tmpfs' >&2
    exit 1
fi
# Root deliberately lacks DAC_OVERRIDE and cannot traverse the user's 0700
# home. Temporarily own only the dedicated tmpfs root while repairing it, then
# restore its user ownership before starting any unprivileged guest process.
/usr/bin/chown -h root:jbox /home/jbox
trap '/usr/bin/chown -h jbox:jbox /home/jbox' EXIT
prepared=
for directory in /home/jbox/.local /home/jbox/.local/bin /home/jbox/.local/share /home/jbox/.jcode /home/jbox/.config /home/jbox/.config/gh /home/jbox/.pi /home/jbox/.ssh; do
    if [ -L "$directory" ]; then
        echo "jbox: refusing symlinked guest home directory $directory" >&2
        exit 1
    fi
    # Longest-prefix lookup also excludes descendants of a reference mount.
    if [ "$(guest_home_backing "$directory")" = "/home/jbox tmpfs" ]; then
        /usr/bin/mkdir -p "$directory"
        prepared="$directory $prepared"
    fi
done
# Restore children before their parents, which OCI can create with mode 0700.
for directory in $prepared; do
    /usr/bin/chown -h jbox:jbox "$directory"
done
/usr/bin/chown -h jbox:jbox /home/jbox
trap - EXIT
if [ -x /usr/local/bin/jbox-toolchain-init ]; then
    su -s /bin/sh jbox -c /usr/local/bin/jbox-toolchain-init
fi
if [ "${JBOX_GITHUB_CLI_CREDENTIALS:-}" = "1" ]; then
    # `gh auth setup-git` only installs a credential helper. It must never
    # prevent SSH or Jcode from starting when GitHub CLI blocks on a broken or
    # interactive credential store. The mounted hosts.yml remains available to
    # `gh`, and the user receives a warning for Git HTTPS troubleshooting.
    if ! su -s /bin/sh jbox -c 'mkdir -p /home/jbox/.config/gh && HOME=/home/jbox GH_CONFIG_DIR=/home/jbox/.config/gh timeout 15 gh auth setup-git </dev/null'; then
        echo "jbox: GitHub CLI credential-helper setup failed or timed out; gh authentication remains available" >&2
    fi
fi
index=0
while [ "$index" -lt "${JBOX_BEADS_WORKSPACE_COUNT:-0}" ]; do
    workspace="$(printenv "JBOX_BEADS_WORKSPACE_${index}")"
    prefix="$(printenv "JBOX_BEADS_PREFIX_${index}")"
    # Import only the portable JSONL task snapshot staged in the generated
    # worktree. This initializes a guest-local database without mounting the
    # host Beads Dolt database, locks, sockets, or credentials.
    # A stalled Dolt command must not keep SSH unavailable indefinitely. Fail
    # closed on timeout instead of exposing an incompletely hydrated workspace.
    if timeout -k 5s 60s env BEADS_DOLT_SERVER_MODE=embedded BEADS_DOLT_AUTO_START=true JBOX_ONE_BEADS_WORKSPACE="$workspace" JBOX_ONE_BEADS_PREFIX="$prefix" su -s /bin/sh jbox -c '
        set -eu
        if [ -f "$JBOX_ONE_BEADS_WORKSPACE/.beads/issues.jsonl" ]; then
            cd "$JBOX_ONE_BEADS_WORKSPACE"
            if [ ! -s .beads/issues.jsonl ]; then
                echo "jbox: refusing empty Beads JSONL in $JBOX_ONE_BEADS_WORKSPACE" >&2
                exit 1
            fi
            original="$(mktemp)"
            cp .beads/issues.jsonl "$original"
            trap "rm -f \"\$original\"" EXIT
            # A repository can track server-mode Beads metadata for its host.
            # The guest must use only its own embedded database, never start a
            # server against a stale port or another mounted repository.
            if [ -f .beads/metadata.json ] && grep -Eq '"'"'"dolt_mode"[[:space:]]*:[[:space:]]*"server"'"'"' .beads/metadata.json; then
                if [ -L .beads/metadata.json ]; then
                    echo "jbox: refusing symlinked Beads metadata in $JBOX_ONE_BEADS_WORKSPACE" >&2
                    exit 1
                fi
                normalized="$(mktemp)"
                sed -E '"'"'s/("dolt_mode"[[:space:]]*:[[:space:]]*)"server"/\1"embedded"/'"'"' .beads/metadata.json > "$normalized"
                cat "$normalized" > .beads/metadata.json
                rm -f "$normalized"
            fi
            if [ -d .beads/embeddeddolt ]; then
                # A retained guest can contain newer issues absent from the
                # portable snapshot. Import upserts instead of resetting it.
                current_prefix="$(bd config get issue_prefix)"
                if [ -z "$current_prefix" ] || [ "$current_prefix" != "$JBOX_ONE_BEADS_PREFIX" ]; then
                    echo "jbox: refusing Beads database without expected issue_prefix $JBOX_ONE_BEADS_PREFIX" >&2
                    exit 1
                fi
                # `su -c` inherits redirected stdin during non-interactive
                # starts. Beads refuses a default import in that case unless
                # the JSONL path is passed explicitly.
                bd import .beads/issues.jsonl --dry-run >/dev/null
                bd import .beads/issues.jsonl >/dev/null
            else
                # `bd init --stealth` can remove a tracked root .gitignore.
                tracked_gitignore=
                if git ls-files --error-unmatch .gitignore >/dev/null 2>&1; then
                    tracked_gitignore="$(mktemp)"
                    cp .gitignore "$tracked_gitignore"
                fi
                bd init --sandbox --stealth --from-jsonl --prefix "$JBOX_ONE_BEADS_PREFIX" --non-interactive --skip-agents --skip-hooks
                if [ -n "$tracked_gitignore" ]; then
                    cat "$tracked_gitignore" > .gitignore
                    rm -f "$tracked_gitignore"
                fi
            fi
            current_prefix="$(bd config get issue_prefix)"
            if [ -z "$current_prefix" ] || [ "$current_prefix" != "$JBOX_ONE_BEADS_PREFIX" ]; then
                echo "jbox: refusing Beads database without expected issue_prefix $JBOX_ONE_BEADS_PREFIX" >&2
                exit 1
            fi
            # Compare portable record IDs, not just counts. Preserve memories
            # as well as issues and reject any failed or incomplete import.
            exported="$(mktemp)"
            expected="$(mktemp)"
            actual="$(mktemp)"
            trap "rm -f \"\$original\" \"\$exported\" \"\$expected\" \"\$actual\"" EXIT
            bd export --include-memories > "$exported"
            awk -F\" '\''{ for (i=1; i<NF; i++) if ($i=="id") { print $(i+2); break } }'\'' "$original" | sort -u > "$expected"
            awk -F\" '\''{ for (i=1; i<NF; i++) if ($i=="id") { print $(i+2); break } }'\'' "$exported" | sort -u > "$actual"
            missing="$(comm -23 "$expected" "$actual")"
            if [ ! -s "$actual" ] || [ ! -s "$expected" ] || [ -n "$missing" ]; then
                echo "jbox: refusing Beads database missing JSONL records in $JBOX_ONE_BEADS_WORKSPACE" >&2
                exit 1
            fi
            # The isolated Git metadata belongs only to this guest. Never
            # replace an existing custom hook rather than bypassing it.
            if git config --get core.hooksPath >/dev/null 2>&1; then
                echo "jbox: refusing Beads hook setup with custom core.hooksPath" >&2
                exit 1
            fi
            hook="$(git rev-parse --git-path hooks/pre-commit)"
            if [ -L "$hook" ] || { [ -e "$hook" ] && ! cmp -s "$hook" /usr/local/bin/jbox-beads-pre-commit; }; then
                echo "jbox: refusing to replace existing pre-commit hook $hook" >&2
                exit 1
            fi
            cp /usr/local/bin/jbox-beads-pre-commit "$hook"
            chmod 0700 "$hook"
        fi
    '; then
        :
    else
        result=$?
        echo "jbox: Beads bootstrap failed for $workspace (exit $result; 124 means 60-second timeout)" >&2
        exit "$result"
    fi
    index=$((index + 1))
done
su -s /bin/sh jbox -c 'mkdir -p /home/jbox/.ssh /home/jbox/.local/share/jcode && /usr/local/bin/jcode --no-update serve --server-name jbox --socket /home/jbox/.local/share/jcode/jbox.sock >/tmp/jcode-serve.log 2>&1 &'
exec /usr/sbin/sshd -D -e
"#;

const BASE_DOCKERFILE: &str = include_str!("../templates/base.Dockerfile");
const BASE_IMAGE_FILES: &[(&str, &str)] = &[
    ("Dockerfile", BASE_DOCKERFILE),
    ("jbox-entrypoint", JBOX_ENTRYPOINT),
    ("jbox-beads-pre-commit", JBOX_BEADS_PRE_COMMIT),
    (
        "jbox-toolchain-init",
        include_str!("../templates/jbox-toolchain-init.sh"),
    ),
    (
        "install-artcraft.py",
        include_str!("../templates/install-artcraft.py"),
    ),
    (
        "artcraft-releases.json",
        include_str!("../templates/artcraft-releases.json"),
    ),
];
impl<'a> ImageManager<'a> {
    pub fn new(paths: &'a JboxPaths) -> Self {
        Self { paths }
    }
    pub fn ensure(&self, config: &Config, project: &Path) -> Result<String> {
        match &config.image.dockerfile {
            Some(dockerfile) => {
                let base = self.base_image()?;
                self.project_image(project, dockerfile, &base)
            }
            None => self.base_image(),
        }
    }
    fn exists(&self, tag: &str) -> bool {
        Command::new("docker")
            .args(["image", "inspect", tag])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
    fn project_image(&self, project: &Path, raw: &str, base: &str) -> Result<String> {
        let dockerfile = project
            .join(raw)
            .canonicalize()
            .with_context(|| format!("cannot resolve image.dockerfile {raw}"))?;
        if !dockerfile.starts_with(project) {
            bail!("image.dockerfile must be inside the primary repository");
        }
        let digest = hash_file_with_context(&dockerfile, base.as_bytes())?;
        let tag = format!("jbox/project:{}", &digest[..16]);
        if !self.exists(&tag) {
            run_build(
                project,
                &dockerfile,
                &tag,
                &[format!("JBOX_BASE_IMAGE={base}")],
            )?;
        }
        Ok(tag)
    }
    fn base_image(&self) -> Result<String> {
        let (uid, gid) = current_user_ids()?;
        let jcode = Command::new("sh")
            .args(["-c", "command -v jcode"])
            .output()
            .context("jcode must be installed locally to build the default image")?;
        if !jcode.status.success() {
            bail!("jcode must be installed locally or configure image.dockerfile");
        }
        // Snapshot before hashing or consulting Docker. Hash exactly the bytes
        // Docker will consume, not a version string or a mutable host path.
        // Private contexts also prevent concurrent builds overwriting inputs.
        let context = stage_base_image(
            &self.paths.cache,
            Path::new(String::from_utf8(jcode.stdout)?.trim()),
        )?;
        let fingerprint = staged_image_fingerprint(context.path())?;
        let tag = format!("jbox/jcode:local-v11-{uid}-{gid}-{}", &fingerprint[..16]);
        if self.exists(&tag) {
            return Ok(tag);
        }
        run_build(
            context.path(),
            &context.path().join("Dockerfile"),
            &tag,
            &[format!("JBOX_UID={uid}"), format!("JBOX_GID={gid}")],
        )?;
        Ok(tag)
    }
}

fn stage_base_image(cache: &Path, launcher: &Path) -> Result<tempfile::TempDir> {
    let launcher = launcher
        .canonicalize()
        .context("could not resolve local jcode launcher")?;
    let executable = launcher
        .parent()
        .context("jcode executable has no parent directory")?
        .join("jcode-linux-x86_64.bin");
    if !executable.is_file() {
        bail!(
            "jcode distribution is incomplete: expected {} next to its launcher",
            executable.display()
        );
    }
    std::fs::create_dir_all(cache)?;
    let context = tempfile::Builder::new()
        .prefix("base-image-")
        .tempdir_in(cache)?;
    std::fs::copy(launcher, context.path().join("jcode"))?;
    std::fs::copy(executable, context.path().join("jcode-linux-x86_64.bin"))?;
    for (name, contents) in BASE_IMAGE_FILES {
        std::fs::write(context.path().join(name), contents)?;
    }
    Ok(context)
}

fn staged_image_fingerprint(context: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(base_image_fingerprint(BASE_IMAGE_FILES).as_bytes());
    // Stream the executable, which can be hundreds of MB. Include the launcher
    // independently so wrapper-only upgrades invalidate the cache as well.
    for name in ["jcode", "jcode-linux-x86_64.bin"] {
        let mut file = std::fs::File::open(context.join(name))?;
        hasher.update((name.len() as u64).to_be_bytes());
        hasher.update(name.as_bytes());
        hasher.update(file.metadata()?.len().to_be_bytes());
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn base_image_fingerprint(files: &[(&str, &str)]) -> String {
    let mut hasher = Sha256::new();
    for (name, contents) in files {
        hasher.update((name.len() as u64).to_be_bytes());
        hasher.update(name.as_bytes());
        hasher.update((contents.len() as u64).to_be_bytes());
        hasher.update(contents.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn current_user_ids() -> Result<(String, String)> {
    let id = |flag| -> Result<String> {
        let output = Command::new("id").arg(flag).output()?;
        if !output.status.success() {
            bail!("could not determine current user identity for the jbox image");
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    Ok((id("-u")?, id("-g")?))
}
fn run_build(context: &Path, dockerfile: &Path, tag: &str, build_args: &[String]) -> Result<()> {
    let mut command = Command::new("docker");
    // --progress=plain streams each build step incrementally so the user can
    // see where a long build is without waiting for the whole thing to finish.
    // Stdout/stderr are inherited rather than captured so progress flows to the
    // terminal in real time and nothing is buffered in process memory.
    command.args(["build", "--progress=plain", "--tag", tag, "--file"]);
    command.arg(dockerfile);
    for build_arg in build_args {
        command.args(["--build-arg", build_arg]);
    }
    let status = command
        .arg(context)
        .status()
        .context("could not execute docker build")?;
    if !status.success() {
        bail!(
            "image build failed (exit {}); see docker output above",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}
fn hash_file_with_context(path: &Path, context: &[u8]) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(std::fs::read(path)?);
    hasher.update(context);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn guest_entrypoint_does_not_install_skills() {
        // Skills are fetched on the host and snapshotted into the session.
        assert!(!JBOX_ENTRYPOINT.contains("JBOX_SKILL_"));
        assert!(!JBOX_ENTRYPOINT.contains("gh skill install"));
        assert!(!JBOX_ENTRYPOINT.contains("failed to install configured GitHub skill source"));
        assert!(
            JBOX_ENTRYPOINT.find("gh auth setup-git").unwrap()
                < JBOX_ENTRYPOINT
                    .find("/usr/local/bin/jcode --no-update serve")
                    .unwrap()
        );
    }

    #[test]
    fn base_image_installs_beads() {
        assert!(BASE_DOCKERFILE.contains("curl"));
        assert!(BASE_DOCKERFILE.contains("gastownhall/beads/main/scripts/install.sh"));
        assert!(BASE_DOCKERFILE.contains("bd version"));
    }

    #[test]
    fn guest_hydrates_beads_exports_before_starting_jcode() {
        assert!(JBOX_ENTRYPOINT.contains("JBOX_BEADS_WORKSPACE_COUNT"));
        assert!(JBOX_ENTRYPOINT.contains("bd init --sandbox --stealth --from-jsonl"));
        assert!(JBOX_ENTRYPOINT.contains("git ls-files --error-unmatch .gitignore"));
        assert!(JBOX_ENTRYPOINT.contains("cat \"$tracked_gitignore\" > .gitignore"));
        assert!(JBOX_ENTRYPOINT.contains("BEADS_DOLT_SERVER_MODE=embedded"));
        assert!(JBOX_ENTRYPOINT.contains("timeout -k 5s 60s env BEADS_DOLT_SERVER_MODE=embedded"));
        assert!(JBOX_ENTRYPOINT.contains("Beads bootstrap failed for $workspace"));
        assert!(
            JBOX_ENTRYPOINT.find("timeout -k 5s 60s env").unwrap()
                < JBOX_ENTRYPOINT.find("bd init --sandbox").unwrap()
        );
        assert!(
            JBOX_ENTRYPOINT
                .find("Beads bootstrap failed for $workspace")
                .unwrap()
                < JBOX_ENTRYPOINT.find("exec /usr/sbin/sshd").unwrap()
        );
        assert!(BASE_DOCKERFILE.contains("ENV BEADS_DOLT_SERVER_MODE=embedded"));
        assert!(JBOX_ENTRYPOINT.contains(".beads/embeddeddolt"));
        assert!(JBOX_ENTRYPOINT.contains("if [ -d .beads/embeddeddolt ]; then"));
        assert!(!JBOX_ENTRYPOINT.contains("--reinit-local"));
        assert!(
            JBOX_ENTRYPOINT
                .find("bd init --sandbox --stealth --from-jsonl")
                .unwrap()
                < JBOX_ENTRYPOINT
                    .find("/usr/local/bin/jcode --no-update serve")
                    .unwrap()
        );
    }

    #[test]
    fn stalled_guest_bootstrap_fails_before_ssh_starts() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let su = bin.join("su");
        fs::write(&su, "#!/bin/sh\nexec sleep 5\n").unwrap();
        fs::set_permissions(&su, fs::Permissions::from_mode(0o700)).unwrap();

        let start = JBOX_ENTRYPOINT
            .find("    if timeout -k 5s 60s env BEADS_DOLT_SERVER_MODE")
            .unwrap();
        let end = JBOX_ENTRYPOINT[start..]
            .find("    index=$((index + 1))")
            .unwrap()
            + start;
        let script = JBOX_ENTRYPOINT[start..end].replace("timeout -k 5s 60s", "timeout -k 1s 0.1s");
        let started = std::time::Instant::now();
        let output = Command::new("sh")
            .args(["-c", &script])
            .env("workspace", temp.path())
            .env("prefix", "test")
            .env("JBOX_ONE_BEADS_WORKSPACE", temp.path())
            .env("JBOX_ONE_BEADS_PREFIX", "test")
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(124));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Beads bootstrap failed"));
    }

    #[test]
    fn guest_entrypoint_refuses_unreconciled_beads_before_jcode() {
        assert!(
            Command::new("sh")
                .arg("-n")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child
                        .stdin
                        .take()
                        .unwrap()
                        .write_all(JBOX_ENTRYPOINT.as_bytes())?;
                    child.wait()
                })
                .unwrap()
                .success()
        );
        for required in [
            "refusing empty Beads JSONL",
            "bd config get issue_prefix",
            "bd import .beads/issues.jsonl --dry-run",
            "bd import .beads/issues.jsonl >/dev/null",
            "bd export --include-memories",
            "comm -23",
        ] {
            assert!(JBOX_ENTRYPOINT.contains(required), "missing {required}");
        }
        assert!(
            JBOX_ENTRYPOINT
                .find("bd import .beads/issues.jsonl >/dev/null")
                .unwrap()
                < JBOX_ENTRYPOINT
                    .find("bd export --include-memories")
                    .unwrap()
        );
        assert!(BASE_DOCKERFILE.contains("COPY jbox-beads-pre-commit"));
        assert!(JBOX_ENTRYPOINT.contains("git rev-parse --git-path hooks/pre-commit"));
        assert!(JBOX_ENTRYPOINT.contains("refusing to replace existing pre-commit hook"));
        assert!(
            Command::new("sh")
                .arg("-n")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child
                        .stdin
                        .take()
                        .unwrap()
                        .write_all(JBOX_BEADS_PRE_COMMIT.as_bytes())?;
                    child.wait()
                })
                .unwrap()
                .success()
        );
    }

    #[test]
    fn existing_guest_database_imports_historical_records_without_losing_new_ones() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("classy");
        let beads = workspace.join(".beads");
        let bin = temp.path().join("bin");
        fs::create_dir_all(beads.join("embeddeddolt")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let original = "{\"id\":\"classy-old\",\"title\":\"historical\"}\n{\"id\":\"mem-1\",\"_type\":\"memory\"}\n";
        fs::write(beads.join("issues.jsonl"), original).unwrap();
        let db_export = temp.path().join("database.jsonl");
        fs::write(&db_export, "{\"id\":\"classy-new\",\"title\":\"new\"}\n").unwrap();
        let prefix = temp.path().join("prefix");
        fs::write(&prefix, "classy\n").unwrap();
        let su = bin.join("su");
        fs::write(&su, "#!/bin/sh\nshift 4\nexec sh -c \"$1\"\n").unwrap();
        let bd = bin.join("bd");
        fs::write(
            &bd,
            "#!/bin/sh\ncase \"$1\" in\n config) cat \"$MOCK_PREFIX\";;\n import) [ \"${2:-}\" = .beads/issues.jsonl ] || { echo 'explicit JSONL operand required' >&2; exit 2; }; if [ \"${3:-}\" != --dry-run ] && [ \"${MOCK_SKIP_IMPORT:-}\" != 1 ]; then cat \"$2\" >> \"$MOCK_EXPORT\"; fi;;\n export) cat \"$MOCK_EXPORT\";;\n *) exit 1;;\nesac\n",
        )
        .unwrap();
        for executable in [&su, &bd] {
            fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let start = JBOX_ENTRYPOINT
            .find("    if timeout -k 5s 60s env BEADS_DOLT_SERVER_MODE=embedded")
            .unwrap();
        let end = JBOX_ENTRYPOINT[start..]
            .find("            # The isolated Git metadata")
            .unwrap()
            + start;
        let bootstrap = format!(
            "{}\n        fi\n    '; then\n        :\n    else\n        exit 1\n    fi\n",
            &JBOX_ENTRYPOINT[start..end]
        );
        let run = |skip_import: bool| {
            Command::new("sh")
                .arg("-c")
                .arg(&bootstrap)
                .env("workspace", &workspace)
                .env("prefix", "classy")
                .env("JBOX_ONE_BEADS_WORKSPACE", &workspace)
                .env("JBOX_ONE_BEADS_PREFIX", "classy")
                .env("MOCK_PREFIX", &prefix)
                .env("MOCK_EXPORT", &db_export)
                .env("MOCK_SKIP_IMPORT", if skip_import { "1" } else { "0" })
                .stdin(std::process::Stdio::null())
                .env(
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                )
                .output()
                .unwrap()
        };
        let result = run(false);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let exported = fs::read_to_string(&db_export).unwrap();
        for id in ["classy-old", "classy-new", "mem-1"] {
            assert!(exported.contains(id), "missing {id} in {exported:?}");
        }
        assert_eq!(
            fs::read_to_string(beads.join("issues.jsonl")).unwrap(),
            original
        );

        fs::write(&db_export, "").unwrap();
        assert!(
            run(false).status.success(),
            "empty database must import JSONL"
        );
        assert!(
            fs::read_to_string(&db_export)
                .unwrap()
                .contains("classy-old")
        );
        fs::write(&db_export, "").unwrap();
        assert!(
            !run(true).status.success(),
            "failed import must not start Jcode"
        );
        fs::write(&prefix, "\n").unwrap();
        assert!(
            !run(false).status.success(),
            "missing issue_prefix must fail closed"
        );
        fs::write(&prefix, "classy\n").unwrap();
        let metadata = beads.join("metadata.json");
        fs::write(
            &metadata,
            "{\"dolt_mode\":\"server\",\"dolt_database\":\"classy\"}\n",
        )
        .unwrap();
        let result = run(false);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            fs::read_to_string(&metadata)
                .unwrap()
                .contains("\"dolt_mode\":\"embedded\"")
        );
        fs::remove_file(&metadata).unwrap();
        let external = temp.path().join("external-metadata.json");
        fs::write(&external, "{\"dolt_mode\":\"server\"}\n").unwrap();
        std::os::unix::fs::symlink(&external, &metadata).unwrap();
        assert!(
            !run(false).status.success(),
            "symlinked metadata must fail closed"
        );
        assert!(
            fs::read_to_string(&external)
                .unwrap()
                .contains("\"server\"")
        );
        fs::remove_file(&metadata).unwrap();
        fs::write(beads.join("issues.jsonl"), "").unwrap();
        assert!(!run(false).status.success(), "empty JSONL must fail closed");
    }

    #[test]
    fn guarded_git_hook_rejects_empty_db_missing_prefix_and_destructive_export() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("classy");
        let bin = temp.path().join("bin");
        fs::create_dir_all(repo.join(".beads")).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.org"]);
        let hook = repo.join(".git/hooks/pre-commit");
        fs::write(&hook, JBOX_BEADS_PRE_COMMIT).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
        let historical = "{\"id\":\"classy-old\"}\n{\"_type\":\"memory\",\"id\":\"memory-1\"}\n";
        fs::write(repo.join(".beads/issues.jsonl"), historical).unwrap();
        fs::write(repo.join("change"), "new work\n").unwrap();
        git(&["add", "change"]);
        let database = temp.path().join("database.jsonl");
        let hook_output = temp.path().join("hook-output.jsonl");
        let prefix = temp.path().join("prefix");
        let bd = bin.join("bd");
        fs::write(&bd, "#!/bin/sh\ncase \"$1\" in\n config) cat \"$MOCK_PREFIX\";;\n export) cat \"$MOCK_DATABASE\";;\n hooks) cp \"$MOCK_HOOK_OUTPUT\" .beads/issues.jsonl;;\n *) exit 1;;\nesac\n").unwrap();
        fs::set_permissions(&bd, fs::Permissions::from_mode(0o700)).unwrap();
        let commit = || {
            Command::new("git")
                .args(["commit", "-m", "checked"])
                .current_dir(&repo)
                .env("MOCK_DATABASE", &database)
                .env("MOCK_HOOK_OUTPUT", &hook_output)
                .env("MOCK_PREFIX", &prefix)
                .env(
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                )
                .output()
                .unwrap()
        };
        fs::write(&prefix, "classy\n").unwrap();
        fs::write(&database, "").unwrap();
        fs::write(&hook_output, "").unwrap();
        assert!(!commit().status.success());
        assert_eq!(
            fs::read_to_string(repo.join(".beads/issues.jsonl")).unwrap(),
            historical
        );
        fs::write(&database, "{\"id\":\"classy-new\"}\n").unwrap();
        assert!(
            !commit().status.success(),
            "new-only database must not erase history"
        );

        fs::write(&database, historical).unwrap();
        fs::write(&prefix, "\n").unwrap();
        assert!(!commit().status.success());
        fs::write(&prefix, "classy\n").unwrap();
        assert!(
            !commit().status.success(),
            "hook export that erases JSONL must fail"
        );
        assert_eq!(
            fs::read_to_string(repo.join(".beads/issues.jsonl")).unwrap(),
            historical
        );

        fs::write(&hook_output, historical).unwrap();
        assert!(
            commit().status.success(),
            "healthy hook should allow Git commit"
        );
    }

    #[test]
    fn base_image_configures_github_cli_for_https_credentials() {
        assert!(BASE_DOCKERFILE.contains("githubcli-archive-keyring.gpg"));
        assert!(BASE_DOCKERFILE.contains("install -y --no-install-recommends gh"));
        assert!(JBOX_ENTRYPOINT.contains("JBOX_GITHUB_CLI_CREDENTIALS"));
        assert!(JBOX_ENTRYPOINT.contains("gh auth setup-git"));
        assert!(JBOX_ENTRYPOINT.contains("timeout 15 gh auth setup-git </dev/null"));
    }

    #[test]
    fn base_image_installs_pinned_development_toolchains() {
        for expected in [
            "FROM rust:1.99.0-slim-bookworm AS rust",
            "rustup component add rustfmt clippy rust-analyzer",
            "FROM node:26.12.0-bookworm-slim AS node",
            "FROM python:3.15.0-slim-bookworm",
            "build-essential",
            "cmake",
            "ninja-build",
            "pkg-config",
            "libssl-dev",
            "ripgrep",
            "jq",
        ] {
            assert!(BASE_DOCKERFILE.contains(expected), "missing {expected}");
        }
        assert!(!BASE_DOCKERFILE.contains("apt-get install -y rustfmt"));
    }

    #[test]
    fn base_image_installs_package_managers_and_transfer_tools() {
        for expected in [
            "FROM ghcr.io/astral-sh/uv:0.13.1 AS uv",
            "COPY --from=uv /uv /uvx /usr/local/bin/",
            "npm install --global --prefix /usr/local pnpm@12.11.2 --no-audit --no-fund",
            "../lib/node_modules/pnpm/pnpm /usr/local/bin/pnpm",
            "../lib/node_modules/pnpm/pnpx /usr/local/bin/pnpx",
            "FROM rclone/rclone:1.75.2 AS rclone",
            "COPY --from=rclone /usr/local/bin/rclone /usr/local/bin/rclone",
            "        rsync \\",
            "        shellcheck \\",
            "        sqlite3 \\",
            "uv --version; uvx --version; pnpm --version",
            "sqlite3 :memory: \"SELECT 1;\"; rsync --version; shellcheck --version; rclone version",
            "PNPM_HOME=/home/jbox/.local/bin",
            "test -w \"$PNPM_HOME\"",
        ] {
            assert!(BASE_DOCKERFILE.contains(expected), "missing {expected}");
        }
        // Disabling these scripts leaves pnpm 12's native executable uninstalled.
        assert!(!BASE_DOCKERFILE.contains("--ignore-scripts"));
        assert!(!BASE_DOCKERFILE.contains("COPY --from=rclone /config"));
    }

    #[test]
    fn base_image_installs_and_smoke_tests_pinned_quarto() {
        for expected in [
            "https://github.com/quarto-dev/quarto-cli/releases/download/v1.10.19/quarto-1.10.19-linux-amd64.tar.gz",
            "8e38c2b2df6a6dbd369ea16e15acded6894545258d13a02f777f81ad18fd7973  /tmp/quarto.tar.gz",
            "sha256sum -c -",
            "tar -xzf /tmp/quarto.tar.gz -C /opt/quarto --strip-components=1",
            "ln -s /opt/quarto/bin/quarto /usr/local/bin/quarto",
            "rm -f /tmp/quarto.tar.gz",
            "RUN su -s /bin/sh jbox -c 'set -eu; quarto --version;",
            "quarto render \"$work/smoke.qmd\" --to html --no-execute",
            "test -s \"$work/smoke.html\"",
        ] {
            assert!(BASE_DOCKERFILE.contains(expected), "missing {expected}");
        }
        let download = BASE_DOCKERFILE.find("RUN curl -fsSL --retry 3").unwrap();
        let checksum = BASE_DOCKERFILE[download..].find("sha256sum -c -").unwrap();
        let extraction = BASE_DOCKERFILE[download..].find("tar -xzf").unwrap();
        assert!(checksum < extraction, "verify Quarto before extraction");
    }

    #[test]
    fn base_image_preserves_non_root_and_ssh_toolchain_access() {
        for expected in [
            "export RUSTUP_HOME=",
            "exec /usr/local/cargo/bin/$tool",
            "chown -R jbox:jbox /home/jbox",
            "RUSTUP_HOME=/home/jbox/.rustup",
            "COPY jbox-toolchain-init /usr/local/bin/jbox-toolchain-init",
            "prefix=${HOME}/.local",
            "SetEnv PATH=/home/jbox/.cargo/bin:/home/jbox/.local/bin:/usr/local/bin:",
            "RUN su -s /bin/sh jbox -c 'set -eu; jbox-toolchain-init; rustc --version",
            "python -m pip --version",
            "npm --version",
            "test -w \"$RUSTUP_HOME\"",
        ] {
            assert!(BASE_DOCKERFILE.contains(expected), "missing {expected}");
        }
        assert!(
            JBOX_ENTRYPOINT.find("jbox-toolchain-init").unwrap()
                < JBOX_ENTRYPOINT
                    .find("/usr/local/bin/jcode --no-update serve")
                    .unwrap()
        );
    }

    #[test]
    fn guest_prepares_only_unmounted_home_ancestors_before_dropping_privileges() {
        let prepare = JBOX_ENTRYPOINT
            .split("for directory in ")
            .nth(1)
            .unwrap()
            .split("\ndone\n")
            .next()
            .unwrap();
        let directories = prepare.split("; do").next().unwrap();
        assert_eq!(
            directories.split_whitespace().collect::<Vec<_>>(),
            vec![
                "/home/jbox/.local",
                "/home/jbox/.local/bin",
                "/home/jbox/.local/share",
                "/home/jbox/.jcode",
                "/home/jbox/.config",
                "/home/jbox/.config/gh",
                "/home/jbox/.pi",
                "/home/jbox/.ssh",
            ]
        );
        assert!(prepare.contains("if [ -L \"$directory\" ]; then"));
        assert!(JBOX_ENTRYPOINT.contains("/proc/self/mountinfo"));
        assert!(prepare.contains(
            "if [ \"$(guest_home_backing \"$directory\")\" = \"/home/jbox tmpfs\" ]; then"
        ));
        assert!(prepare.contains("prepared=\"$directory $prepared\""));
        assert!(!prepare.contains("chown"));
        assert!(JBOX_ENTRYPOINT.contains(
            "for directory in $prepared; do\n    /usr/bin/chown -h jbox:jbox \"$directory\"\ndone"
        ));
        assert!(JBOX_ENTRYPOINT.contains(
            "/usr/bin/chown -h root:jbox /home/jbox\ntrap '/usr/bin/chown -h jbox:jbox /home/jbox' EXIT"
        ));
        assert!(JBOX_ENTRYPOINT.contains(
            "/usr/bin/chown -h jbox:jbox /home/jbox\ntrap - EXIT\nif [ -x /usr/local/bin/jbox-toolchain-init ]"
        ));
        assert!(!prepare.contains("chown -R"));
        assert!(!prepare.contains("chmod"));
        assert!(
            JBOX_ENTRYPOINT.find("chown -h jbox:jbox").unwrap()
                < JBOX_ENTRYPOINT
                    .find("su -s /bin/sh jbox -c /usr/local/bin/jbox-toolchain-init")
                    .unwrap()
        );
    }

    #[test]
    fn home_backing_lookup_excludes_bind_mount_roots_and_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let mountinfo = temp.path().join("mountinfo");
        fs::write(
            &mountinfo,
            "1 0 0:1 / / rw - overlay overlay rw\n\
             2 1 0:2 / /home/jbox rw - tmpfs tmpfs rw\n\
             3 2 8:1 /reference /home/jbox/.local rw - ext4 /dev/test rw\n\
             4 2 8:1 /credentials /home/jbox/.jcode rw - ext4 /dev/test rw\n\
             5 2 8:1 /config /home/jbox/.config/gh/hosts.yml ro - ext4 /dev/test ro\n\
             6 2 0:3 / /home/jbox/.pi rw - tmpfs tmpfs rw\n",
        )
        .unwrap();
        let function = JBOX_ENTRYPOINT
            .split("guest_home_backing() {")
            .nth(1)
            .unwrap()
            .split("\n}\n")
            .next()
            .unwrap();
        let script = format!("guest_home_backing() {{{function}\n}}\nguest_home_backing \"$1\"")
            .replace("/proc/self/mountinfo", mountinfo.to_str().unwrap());
        for (path, expected) in [
            ("/home/jbox", "/home/jbox tmpfs"),
            ("/home/jbox/.local", "/home/jbox/.local ext4"),
            ("/home/jbox/.local/bin", "/home/jbox/.local ext4"),
            ("/home/jbox/.local/share/new", "/home/jbox/.local ext4"),
            ("/home/jbox/.jcode", "/home/jbox/.jcode ext4"),
            ("/home/jbox/.jcode/new", "/home/jbox/.jcode ext4"),
            ("/home/jbox/.pi/agent", "/home/jbox/.pi tmpfs"),
            ("/home/jbox/.config/gh", "/home/jbox tmpfs"),
            ("/home/jbox/.local-other", "/home/jbox tmpfs"),
            ("/home/other", "/ overlay"),
        ] {
            let output = Command::new("sh")
                .args(["-c", &script, "home-backing-test", path])
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
        }
    }

    #[test]
    fn every_bundled_base_image_input_invalidates_the_cache() {
        let original = base_image_fingerprint(BASE_IMAGE_FILES);
        for index in 0..BASE_IMAGE_FILES.len() {
            let mut changed = BASE_IMAGE_FILES.to_vec();
            changed[index].1 = "changed";
            assert_ne!(original, base_image_fingerprint(&changed));
            let mut renamed = BASE_IMAGE_FILES.to_vec();
            renamed[index].0 = "renamed";
            assert_ne!(original, base_image_fingerprint(&renamed));
        }
        assert_ne!(
            base_image_fingerprint(&[("a", "bc")]),
            base_image_fingerprint(&[("ab", "c")])
        );
    }

    fn fake_distribution(root: &Path, launcher: &str, executable: &[u8]) -> std::path::PathBuf {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("jcode"), launcher).unwrap();
        fs::write(root.join("jcode-linux-x86_64.bin"), executable).unwrap();
        root.join("jcode")
    }

    #[test]
    fn jcode_launcher_and_binary_contents_invalidate_base_and_project_images() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let launcher = fake_distribution(&temp.path().join("dist"), "launcher-a", b"binary-a");
        let original = stage_base_image(&cache, &launcher).unwrap();
        let original_hash = staged_image_fingerprint(original.path()).unwrap();
        let unchanged = stage_base_image(&cache, &launcher).unwrap();
        assert_ne!(original.path(), unchanged.path());
        assert_eq!(
            original_hash,
            staged_image_fingerprint(unchanged.path()).unwrap()
        );

        // Same path, length and version label, but new executable bytes.
        fs::write(
            launcher.with_file_name("jcode-linux-x86_64.bin"),
            b"binary-b",
        )
        .unwrap();
        let upgraded = stage_base_image(&cache, &launcher).unwrap();
        let upgraded_hash = staged_image_fingerprint(upgraded.path()).unwrap();
        assert_ne!(original_hash, upgraded_hash);
        let dockerfile = temp.path().join("project.Dockerfile");
        fs::write(
            &dockerfile,
            "ARG JBOX_BASE_IMAGE\nFROM ${JBOX_BASE_IMAGE}\n",
        )
        .unwrap();
        assert_ne!(
            hash_file_with_context(&dockerfile, original_hash.as_bytes()).unwrap(),
            hash_file_with_context(&dockerfile, upgraded_hash.as_bytes()).unwrap()
        );

        fs::write(&launcher, "launcher-b").unwrap();
        let wrapper_upgrade = stage_base_image(&cache, &launcher).unwrap();
        assert_ne!(
            upgraded_hash,
            staged_image_fingerprint(wrapper_upgrade.path()).unwrap()
        );
        // Neither an upgrade nor a concurrent stage can alter existing inputs.
        assert_eq!(
            original_hash,
            staged_image_fingerprint(original.path()).unwrap()
        );
        assert_eq!(
            fs::read(original.path().join("jcode-linux-x86_64.bin")).unwrap(),
            b"binary-a"
        );
        assert!(BASE_DOCKERFILE.contains("/usr/local/bin/jcode --no-update --version"));
    }

    #[test]
    fn symlinked_jcode_resolves_its_matching_adjacent_executable() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let first = fake_distribution(&temp.path().join("v1"), "launcher", b"binary-1");
        let second = fake_distribution(&temp.path().join("v2"), "launcher", b"binary-2");
        let selected = temp.path().join("jcode");
        std::os::unix::fs::symlink(&first, &selected).unwrap();
        let original = stage_base_image(&cache, &selected).unwrap();
        fs::remove_file(&selected).unwrap();
        std::os::unix::fs::symlink(&second, &selected).unwrap();
        let upgraded = stage_base_image(&cache, &selected).unwrap();
        assert_ne!(
            staged_image_fingerprint(original.path()).unwrap(),
            staged_image_fingerprint(upgraded.path()).unwrap()
        );
        assert_eq!(
            fs::read(upgraded.path().join("jcode-linux-x86_64.bin")).unwrap(),
            b"binary-2"
        );
    }

    #[test]
    fn incomplete_jcode_distribution_fails_before_cache_lookup() {
        let temp = tempfile::tempdir().unwrap();
        let launcher = fake_distribution(&temp.path().join("dist"), "launcher", b"binary");
        let cache = temp.path().join("cache");
        let staged = stage_base_image(&cache, &launcher).unwrap();
        let path = staged.path().to_owned();
        drop(staged);
        assert!(!path.exists(), "temporary image context leaked");
        fs::remove_file(launcher.with_file_name("jcode-linux-x86_64.bin")).unwrap();
        let error = stage_base_image(&cache, &launcher).unwrap_err().to_string();
        assert!(error.contains("distribution is incomplete"), "{error}");
        assert_eq!(fs::read_dir(cache).unwrap().count(), 0);
    }

    #[test]
    fn toolchain_init_populates_a_fresh_home_without_copying_compilers() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("fresh-home");
        let seed = temp.path().join("immutable-seed");
        let name = "1.99.0-x86_64-unknown-linux-gnu";
        fs::create_dir_all(seed.join("toolchains").join(name)).unwrap();
        fs::write(seed.join("settings.toml"), "default_toolchain = '1.99.0'\n").unwrap();
        let init = || {
            Command::new("sh")
                .args([
                    "-c",
                    include_str!("../templates/jbox-toolchain-init.sh"),
                    "jbox-toolchain-init",
                ])
                .arg(&seed)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &home)
                .status()
                .unwrap()
        };
        assert!(init().success());
        assert!(home.join(".cargo/bin").is_dir());
        assert!(home.join(".local/bin").is_dir());
        let settings = home.join(".rustup/settings.toml");
        assert_eq!(
            fs::read(&settings).unwrap(),
            fs::read(seed.join("settings.toml")).unwrap()
        );
        let toolchain = home.join(".rustup/toolchains").join(name);
        assert_eq!(
            fs::read_link(&toolchain).unwrap(),
            seed.join("toolchains").join(name)
        );
        fs::write(&settings, "default_toolchain = 'custom'\n").unwrap();
        let custom = home.join(".rustup/toolchains/custom");
        fs::create_dir(&custom).unwrap();
        assert!(init().success());
        assert_eq!(
            fs::read_to_string(settings).unwrap(),
            "default_toolchain = 'custom'\n"
        );
        assert!(custom.is_dir());
        assert!(toolchain.is_symlink());
    }

    #[test]
    fn artcraft_manifest_pins_the_complete_official_cli_suite() {
        let raw = BASE_IMAGE_FILES
            .iter()
            .find(|(name, _)| *name == "artcraft-releases.json")
            .unwrap()
            .1;
        let manifest: serde_json::Value = serde_json::from_str(raw).unwrap();
        let releases = manifest["releases"].as_array().unwrap();
        let mut apps: Vec<_> = releases
            .iter()
            .map(|release| {
                let app = release["app"].as_str().unwrap();
                let url = release["url"].as_str().unwrap();
                assert!(url.starts_with(&format!(
                    "https://github.com/storytold/{app}/releases/download/"
                )));
                assert!(url.ends_with("-linux-x86_64.tar.gz"));
                let checksum = release["sha256"].as_str().unwrap();
                assert_eq!(checksum.len(), 64);
                assert!(checksum.bytes().all(|byte| byte.is_ascii_hexdigit()));
                assert!(
                    release["binary"]
                        .as_str()
                        .unwrap()
                        .ends_with(&format!("/{app}-cli"))
                );
                app
            })
            .collect();
        apps.sort();
        let mut expected = vec![
            "photocraft",
            "vectorcraft",
            "filmcraft",
            "lightcraft",
            "pdfcraft",
            "effectcraft",
            "designcraft",
            "figcraft",
            "wordcraft",
            "soundcraft",
            "deckcraft",
            "cadcraft",
            "gridcraft",
        ];
        expected.sort();
        assert_eq!(apps, expected);
        assert!(BASE_DOCKERFILE.contains("python /usr/local/lib/jbox/install-artcraft.py"));
        assert!(BASE_DOCKERFILE.contains("--verify-only"));
    }

    #[test]
    fn base_image_creates_jcode_config_mount_parent() {
        assert!(BASE_DOCKERFILE.contains("/home/jbox/.jcode"));
    }

    #[test]
    fn image_build_streams_progress_and_reports_exit_code_on_failure() {
        // run_build must pass --progress=plain so docker emits each build step
        // to the inherited terminal rather than buffering everything silently.
        // The error message must reference the exit code rather than captured
        // stderr, because output is no longer captured.
        let src = std::fs::read_to_string("src/image.rs").unwrap();
        assert!(
            src.contains("--progress=plain"),
            "run_build must pass --progress=plain"
        );
        assert!(
            src.contains(".status()"),
            "run_build must use .status() not .output() to stream progress"
        );
        assert!(
            src.contains("see docker output above"),
            "failure message must direct the user to the streamed docker output"
        );
        assert!(
            !src.contains(".output()\n        .context(\"could not execute docker build\")"),
            "run_build must not use .output() which buffers and hides progress"
        );
    }
}
