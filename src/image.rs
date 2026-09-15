use crate::{config::Config, paths::JboxPaths};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ImageManager<'a> {
    paths: &'a JboxPaths,
}

const JBOX_ENTRYPOINT: &str = r#"#!/bin/sh
set -eu
mkdir -p /run/sshd
if [ -n "${JBOX_SKILLS_REPOSITORY:-}" ]; then
    # The clone is owned by jbox. With CAP_DAC_OVERRIDE intentionally absent,
    # root must not try to remove its contents during a subsequent bootstrap.
    su -s /bin/sh jbox -c 'rm -rf /tmp/jbox-skills'
    mkdir -p /tmp/jbox-skills
    chown jbox:jbox /tmp/jbox-skills
    # The guest root process intentionally lacks DAC_OVERRIDE. The isolated
    # home tmpfs belongs to jbox, so every operation below /home/jbox must run
    # as that unprivileged user rather than failing during guest bootstrap.
    su -s /bin/sh jbox -c 'mkdir -p /home/jbox/.agents/skills && HOME=/home/jbox git clone --depth 1 --no-tags "$JBOX_SKILLS_REPOSITORY" /tmp/jbox-skills/repository'
    su -s /bin/sh jbox -c 'cp -a "/tmp/jbox-skills/repository/${JBOX_SKILLS_PATH}/." /home/jbox/.agents/skills/'
    su -s /bin/sh jbox -c 'rm -rf /tmp/jbox-skills'
fi
if [ "${JBOX_GITHUB_CLI_CREDENTIALS:-}" = "1" ]; then
    su -s /bin/sh jbox -c 'mkdir -p /home/jbox/.config/gh && HOME=/home/jbox GH_CONFIG_DIR=/home/jbox/.config/gh gh auth setup-git'
fi
su -s /bin/sh jbox -c 'mkdir -p /home/jbox/.ssh /home/jbox/.local/share/jcode && jcode serve --server-name jbox --socket /home/jbox/.local/share/jcode/jbox.sock >/tmp/jcode-serve.log 2>&1 &'
exec /usr/sbin/sshd -D -e
"#;

const BASE_DOCKERFILE: &str = "FROM debian:bookworm-slim\nARG JBOX_UID=1000\nARG JBOX_GID=1000\nRUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends bash ca-certificates curl git gzip openssh-client openssh-server tar && rm -rf /var/lib/apt/lists/*\nRUN mkdir -p -m 0755 /etc/apt/keyrings && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg -o /etc/apt/keyrings/githubcli-archive-keyring.gpg && chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg && echo 'deb [arch=amd64 signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main' > /etc/apt/sources.list.d/github-cli.list && apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gh && rm -rf /var/lib/apt/lists/*\nRUN curl -fsSL https://raw.githubusercontent.com/gastownhall/beads/main/scripts/install.sh | bash && bd version\nRUN groupadd --gid \"$JBOX_GID\" jbox && useradd --uid \"$JBOX_UID\" --gid \"$JBOX_GID\" -m -s /bin/bash jbox && mkdir -p /run/sshd /home/jbox/.jcode /home/jbox/.ssh && chown -R jbox:jbox /home/jbox\nCOPY jcode /usr/local/bin/jcode\nCOPY jcode-linux-x86_64.bin /usr/local/bin/jcode-linux-x86_64.bin\nCOPY jbox-entrypoint /usr/local/bin/jbox-entrypoint\nRUN chmod 0755 /usr/local/bin/jcode /usr/local/bin/jcode-linux-x86_64.bin /usr/local/bin/jbox-entrypoint && printf '%s\\n' 'Port 2222' 'PasswordAuthentication no' 'PermitRootLogin no' 'AllowUsers jbox' 'AuthorizedKeysFile .ssh/authorized_keys' > /etc/ssh/sshd_config.d/jbox.conf\nEXPOSE 2222\n";
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
        run_build(
            &context,
            &context.join("Dockerfile"),
            &tag,
            &[format!("JBOX_UID={uid}"), format!("JBOX_GID={gid}")],
        )?;
        Ok(tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_entrypoint_imports_skills_before_starting_jcode() {
        assert!(JBOX_ENTRYPOINT.contains("JBOX_SKILLS_REPOSITORY"));
        assert!(JBOX_ENTRYPOINT.contains("/home/jbox/.agents/skills"));
        assert!(JBOX_ENTRYPOINT.contains("git clone --depth 1 --no-tags"));
        assert!(
            JBOX_ENTRYPOINT.find("git clone").unwrap()
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
    fn base_image_configures_github_cli_for_https_credentials() {
        assert!(BASE_DOCKERFILE.contains("githubcli-archive-keyring.gpg"));
        assert!(BASE_DOCKERFILE.contains("install -y --no-install-recommends gh"));
        assert!(JBOX_ENTRYPOINT.contains("JBOX_GITHUB_CLI_CREDENTIALS"));
        assert!(JBOX_ENTRYPOINT.contains("gh auth setup-git"));
    }

    #[test]
    fn base_image_creates_jcode_config_mount_parent() {
        assert!(BASE_DOCKERFILE.contains("/home/jbox/.jcode"));
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
    command.args(["build", "--tag", tag, "--file"]);
    command.arg(dockerfile);
    for build_arg in build_args {
        command.args(["--build-arg", build_arg]);
    }
    let output = command
        .arg(context)
        .output()
        .context("could not execute docker build")?;
    if !output.status.success() {
        bail!(
            "image build failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
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
