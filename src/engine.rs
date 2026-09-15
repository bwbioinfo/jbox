use crate::config::Network;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub mounts: Vec<(PathBuf, PathBuf, bool)>,
    pub cpus: u16,
    pub memory: String,
    pub network: Network,
}

/// Narrow OCI engine contract. The Kata runtime stays behind this interface.
pub trait Engine {
    fn check(&self) -> Result<()>;
    fn start(&self, spec: &ContainerSpec) -> Result<u16>;
    fn stop(&self, name: &str) -> Result<()>;
    fn is_running(&self, name: &str) -> Result<bool>;
}

/// Docker is selected for the MVP because Kata on current Arch-based hosts is a
/// rootful deployment. Podman's stronger rootless network defaults cannot be
/// combined with Kata there. The sandbox never receives Docker's socket.
pub struct DockerEngine;
impl DockerEngine {
    fn command() -> Command {
        Command::new("docker")
    }
    fn run_checked(mut command: Command) -> Result<String> {
        let output = command.output().context("could not execute docker")?;
        if !output.status.success() {
            bail!(
                "docker failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8(output.stdout)?.trim().into())
    }
}

impl Engine for DockerEngine {
    fn check(&self) -> Result<()> {
        let output = Self::command()
            .args(["info", "--format", "{{json .Runtimes}}"])
            .output()
            .context("Docker is required. Install Kata and register its runtime with Docker.")?;
        if !output.status.success() {
            bail!(
                "Docker is unusable: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let runtimes: serde_json::Value = serde_json::from_slice(&output.stdout)
            .context("Docker returned invalid runtime metadata")?;
        if runtimes.get("kata").is_none() {
            bail!(
                "Docker has no runtime named `kata`. Register Kata in /etc/docker/daemon.json, restart Docker, and verify `docker info` reports it."
            );
        }
        Ok(())
    }

    fn start(&self, spec: &ContainerSpec) -> Result<u16> {
        let cpus = spec.cpus.to_string();
        let mut command = Self::command();
        command.args([
            "run",
            "-d",
            "--name",
            &spec.name,
            "--runtime",
            "kata",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--read-only",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,nodev",
            "--tmpfs",
            "/run:rw,nosuid,nodev",
            "--tmpfs",
            "/home/jbox:rw,nosuid,nodev,uid=1000,gid=1000,mode=0700",
            "--pids-limit",
            "4096",
            "--cpus",
            &cpus,
            "--memory",
            &spec.memory,
            "-p",
            "127.0.0.1::2222",
        ]);
        if !spec.network.internet {
            command.args(["--network", "none"]);
        }
        // Docker bridge networking provides Internet access but cannot, by itself,
        // guarantee host/LAN denial. This limitation is reported by `jbox doctor`.
        for (source, target, writable) in &spec.mounts {
            let mut mount = format!(
                "type=bind,src={},dst={}",
                source.display(),
                target.display()
            );
            if !writable {
                mount.push_str(",readonly");
            }
            command.args(["--mount", &mount]);
        }
        command
            .arg(&spec.image)
            .arg("/usr/local/bin/jbox-entrypoint");
        Self::run_checked(command).context("could not start Kata session. Confirm Docker's `kata` runtime and /dev/kvm are available")?;

        let mut port_command = Self::command();
        port_command.args(["port", &spec.name, "2222/tcp"]);
        let port = Self::run_checked(port_command)?;
        port.rsplit(':')
            .next()
            .context("Docker did not report SSH port")?
            .trim()
            .parse()
            .context("invalid SSH port from Docker")
    }

    fn stop(&self, name: &str) -> Result<()> {
        let mut command = Self::command();
        command.args(["rm", "-f", name]);
        Self::run_checked(command)?;
        Ok(())
    }

    fn is_running(&self, name: &str) -> Result<bool> {
        let out = Self::command()
            .args(["inspect", "--format", "{{.State.Running}}", name])
            .output()?;
        Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true")
    }
}
