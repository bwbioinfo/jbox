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
            run_build(project, &dockerfile, &tag)?;
        }
        Ok(tag)
    }
    fn base_image(&self) -> Result<String> {
        let tag = "jbox/jcode:local-v2";
        if self.exists(tag) {
            return Ok(tag.into());
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
        let jcode = PathBuf::from(String::from_utf8(jcode.stdout)?.trim());
        std::fs::copy(&jcode, context.join("jcode"))?;
        std::fs::write(
            context.join("Dockerfile"),
            "FROM debian:bookworm-slim\nRUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends bash ca-certificates git openssh-client openssh-server && rm -rf /var/lib/apt/lists/* && useradd -m -s /bin/bash jbox && mkdir -p /run/sshd /home/jbox/.ssh && chown -R jbox:jbox /home/jbox\nCOPY jcode /usr/local/bin/jcode\nCOPY jbox-entrypoint /usr/local/bin/jbox-entrypoint\nRUN chmod 0755 /usr/local/bin/jcode /usr/local/bin/jbox-entrypoint && printf '%s\\n' 'Port 2222' 'PasswordAuthentication no' 'PermitRootLogin no' 'AllowUsers jbox' 'AuthorizedKeysFile .ssh/authorized_keys' > /etc/ssh/sshd_config.d/jbox.conf\nEXPOSE 2222\n",
        )?;
        std::fs::write(
            context.join("jbox-entrypoint"),
            "#!/bin/sh\nset -eu\nmkdir -p /run/sshd\nsu -s /bin/sh jbox -c 'mkdir -p /home/jbox/.ssh /home/jbox/.local/share/jcode && jcode serve --server-name jbox --socket /home/jbox/.local/share/jcode/jbox.sock >/tmp/jcode-serve.log 2>&1 &'\nexec /usr/sbin/sshd -D -e\n",
        )?;
        run_build(&context, &context.join("Dockerfile"), tag)?;
        Ok(tag.into())
    }
}
fn run_build(context: &Path, dockerfile: &Path, tag: &str) -> Result<()> {
    let output = Command::new("docker")
        .args([
            "build",
            "--tag",
            tag,
            "--file",
            &dockerfile.to_string_lossy(),
            &context.to_string_lossy(),
        ])
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
