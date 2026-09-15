use crate::{config::Config, paths::JboxPaths};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ImageManager<'a> {
    paths: &'a JboxPaths,
}
impl<'a> ImageManager<'a> {
    pub fn new(paths: &'a JboxPaths) -> Self {
        Self { paths }
    }
    pub fn ensure(&self, config: &Config, project: &Path) -> Result<String> {
        match &config.image.dockerfile {
            Some(dockerfile) => self.project_image(project, dockerfile),
            None => self.base_image(),
        }
    }
    fn exists(&self, tag: &str) -> bool {
        Command::new("docker")
            .args(["image", "inspect", tag])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    fn project_image(&self, project: &Path, raw: &str) -> Result<String> {
        let dockerfile = project
            .join(raw)
            .canonicalize()
            .with_context(|| format!("cannot resolve image.dockerfile {raw}"))?;
        if !dockerfile.starts_with(project) {
            bail!("image.dockerfile must be inside the primary repository");
        }
        let digest = hash_file(&dockerfile)?;
        let tag = format!("jbox/project:{}", &digest[..16]);
        if !self.exists(&tag) {
            run_build(project, &dockerfile, &tag, &[])?;
        }
        Ok(tag)
    }
    fn base_image(&self) -> Result<String> {
        let (uid, gid) = current_user_ids()?;
        let tag = format!("jbox/jcode:local-v4-{uid}-{gid}");
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
        std::fs::write(
            context.join("Dockerfile"),
            "FROM debian:bookworm-slim\nARG JBOX_UID=1000\nARG JBOX_GID=1000\nRUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends bash ca-certificates git openssh-client openssh-server && rm -rf /var/lib/apt/lists/* && groupadd --gid \"$JBOX_GID\" jbox && useradd --uid \"$JBOX_UID\" --gid \"$JBOX_GID\" -m -s /bin/bash jbox && mkdir -p /run/sshd /home/jbox/.ssh && chown -R jbox:jbox /home/jbox\nCOPY jcode /usr/local/bin/jcode\nCOPY jcode-linux-x86_64.bin /usr/local/bin/jcode-linux-x86_64.bin\nCOPY jbox-entrypoint /usr/local/bin/jbox-entrypoint\nRUN chmod 0755 /usr/local/bin/jcode /usr/local/bin/jcode-linux-x86_64.bin /usr/local/bin/jbox-entrypoint && printf '%s\\n' 'Port 2222' 'PasswordAuthentication no' 'PermitRootLogin no' 'AllowUsers jbox' 'AuthorizedKeysFile .ssh/authorized_keys' > /etc/ssh/sshd_config.d/jbox.conf\nEXPOSE 2222\n",
        )?;
        std::fs::write(
            context.join("jbox-entrypoint"),
            "#!/bin/sh\nset -eu\nmkdir -p /run/sshd\nsu -s /bin/sh jbox -c 'mkdir -p /home/jbox/.ssh /home/jbox/.local/share/jcode && jcode serve --server-name jbox --socket /home/jbox/.local/share/jcode/jbox.sock >/tmp/jcode-serve.log 2>&1 &'\nexec /usr/sbin/sshd -D -e\n",
        )?;
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
fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(std::fs::read(path)?);
    Ok(format!("{:x}", hasher.finalize()))
}
