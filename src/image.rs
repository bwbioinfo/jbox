use crate::{config::Config, paths::JboxPaths};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
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
mkdir -p /run/sshd
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
su -s /bin/sh jbox -c 'mkdir -p /home/jbox/.ssh /home/jbox/.local/share/jcode && jcode serve --server-name jbox --socket /home/jbox/.local/share/jcode/jbox.sock >/tmp/jcode-serve.log 2>&1 &'
exec /usr/sbin/sshd -D -e
"#;

const BASE_DOCKERFILE: &str = "FROM debian:bookworm-slim\nARG JBOX_UID=1000\nARG JBOX_GID=1000\nRUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends bash ca-certificates curl git gzip openssh-client openssh-server rustfmt tar && rm -rf /var/lib/apt/lists/*\nRUN mkdir -p -m 0755 /etc/apt/keyrings && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg && chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg && echo 'deb [arch=amd64 signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main' > /etc/apt/sources.list.d/github-cli.list && apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/*\nRUN curl -fsSL https://raw.githubusercontent.com/gastownhall/beads/main/scripts/install.sh | bash && bd version\n# Never inherit a repository's host-managed Dolt server settings in a jbox guest.\nENV BEADS_DOLT_SERVER_MODE=embedded BEADS_DOLT_AUTO_START=true\nRUN groupadd --gid \"$JBOX_GID\" jbox && useradd --uid \"$JBOX_UID\" --gid \"$JBOX_GID\" -m -s /bin/bash jbox && mkdir -p /run/sshd /home/jbox/.jcode /home/jbox/.ssh && chown -R jbox:jbox /home/jbox\nCOPY jcode /usr/local/bin/jcode\nCOPY jcode-linux-x86_64.bin /usr/local/bin/jcode-linux-x86_64.bin\nCOPY jbox-entrypoint /usr/local/bin/jbox-entrypoint\nCOPY jbox-beads-pre-commit /usr/local/bin/jbox-beads-pre-commit\nRUN chmod 0755 /usr/local/bin/jcode /usr/local/bin/jcode-linux-x86_64.bin /usr/local/bin/jbox-entrypoint /usr/local/bin/jbox-beads-pre-commit && printf '%s\\n' 'Port 2222' 'PasswordAuthentication no' 'PermitRootLogin no' 'AllowUsers jbox' 'AuthorizedKeysFile .ssh/authorized_keys' > /etc/ssh/sshd_config.d/jbox.conf\nEXPOSE 2222\n";
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
        // The base is generated from both strings below. Include their content
        // in the tag so an entrypoint security or startup fix cannot silently
        // reuse a stale local base image, which would also poison project-image
        // caching through its base-image build argument.
        let mut hasher = Sha256::new();
        hasher.update(BASE_DOCKERFILE.as_bytes());
        hasher.update(JBOX_ENTRYPOINT.as_bytes());
        hasher.update(JBOX_BEADS_PRE_COMMIT.as_bytes());
        let fingerprint = format!("{:x}", hasher.finalize());
        let tag = format!("jbox/jcode:local-v10-{uid}-{gid}-{}", &fingerprint[..16]);
        if self.exists(&tag) {
            return Ok(tag);
        }
        let context = self.paths.cache.join("base-image");
        std::fs::create_dir_all(&context)?;
        let jcode = Command::new("sh")
            .args(["-c", "command -v jcode"])
            .output()
            .context("jcode must be installed locally to build the default image")?;
        if !jcode.status.success() {
            bail!("jcode must be installed locally or configure image.dockerfile");
        }
        let jcode = PathBuf::from(String::from_utf8(jcode.stdout)?.trim())
            .canonicalize()
            .context("could not resolve local jcode launcher")?;
        std::fs::copy(&jcode, context.join("jcode"))?;
        let jcode_binary = jcode
            .parent()
            .context("jcode executable has no parent directory")?
            .join("jcode-linux-x86_64.bin");
        if !jcode_binary.is_file() {
            bail!(
                "jcode distribution is incomplete: expected {} next to its launcher",
                jcode_binary.display()
            );
        }
        std::fs::copy(&jcode_binary, context.join("jcode-linux-x86_64.bin"))?;
        std::fs::write(context.join("Dockerfile"), BASE_DOCKERFILE)?;
        std::fs::write(context.join("jbox-entrypoint"), JBOX_ENTRYPOINT)?;
        std::fs::write(context.join("jbox-beads-pre-commit"), JBOX_BEADS_PRE_COMMIT)?;
        run_build(
            &context,
            &context.join("Dockerfile"),
            &tag,
            &[format!("JBOX_UID={uid}"), format!("JBOX_GID={gid}")],
        )?;
        Ok(tag)
    }
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
                < JBOX_ENTRYPOINT.find("jcode serve").unwrap()
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
                < JBOX_ENTRYPOINT.find("jcode serve").unwrap()
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
    fn base_image_installs_rustfmt() {
        assert!(BASE_DOCKERFILE.contains("openssh-server rustfmt tar"));
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
