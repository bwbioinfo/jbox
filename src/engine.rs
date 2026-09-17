use crate::config::Network;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub mounts: Vec<(PathBuf, PathBuf, bool)>,
    pub cpus: u16,
    pub memory: String,
    pub network: Network,
    pub ssh_host: String,
    pub environment: Vec<(String, String)>,
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

    fn check_host_prerequisites(needs_network: bool) -> Result<()> {
        if !Path::new("/dev/kvm").exists() {
            bail!("Kata requires an accessible /dev/kvm device");
        }
        if !Path::new("/dev/vhost-vsock").exists() {
            bail!("Kata requires /dev/vhost-vsock; load the vhost_vsock kernel module");
        }
        if needs_network && !module_loaded("vhost_net")? {
            bail!(
                "Kata guest networking requires the vhost_net kernel module; run `sudo modprobe vhost_net`"
            );
        }
        Ok(())
    }
}

fn module_loaded(name: &str) -> Result<bool> {
    let modules = std::fs::read_to_string("/proc/modules")
        .context("could not inspect loaded kernel modules")?;
    Ok(modules
        .lines()
        .any(|line| line.split_whitespace().next() == Some(name)))
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
        Self::check_host_prerequisites(spec.network.internet)?;
        let cpus = spec.cpus.to_string();
        let uid = current_id("-u")?;
        let gid = current_id("-g")?;
        let home_tmpfs = format!("/home/jbox:rw,nosuid,nodev,uid={uid},gid={gid},mode=0700");
        let port_mapping = format!("{}:22:2222", spec.ssh_host);
        let mut command = Self::command();
        command.args([
            "run",
            "-d",
            "--name",
            &spec.name,
            "--runtime",
            "kata",
            // Project Dockerfiles commonly finish with `USER jbox`. The
            // guest entrypoint must nevertheless begin as root to create
            // runtime directories and start sshd before dropping Jcode and
            // all interactive sessions to the unprivileged account.
            "--user",
            "root",
            "--cap-drop",
            "ALL",
            // The entrypoint begins as root so sshd can start, then `su` drops
            // to the unprivileged jbox account before launching jcode. sshd
            // also chroots its pre-auth privilege-separation process. A PTY
            // additionally needs CHOWN so sshd can assign /dev/pts ownership
            // to the unprivileged session account. Debian's sshd also writes
            // its audit login record and may signal the UID-switched session
            // during PTY cleanup. These are the only capabilities the
            // entrypoint and SSH service require.
            "--cap-add",
            "SETGID",
            "--cap-add",
            "SETUID",
            "--cap-add",
            "SYS_CHROOT",
            "--cap-add",
            "CHOWN",
            "--cap-add",
            "AUDIT_WRITE",
            "--cap-add",
            "KILL",
            "--security-opt",
            "no-new-privileges",
            "--read-only",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,nodev",
            "--tmpfs",
            "/run:rw,nosuid,nodev",
            "--tmpfs",
            &home_tmpfs,
            "--pids-limit",
            "4096",
            "--cpus",
            &cpus,
            "--memory",
            &spec.memory,
            "-p",
            &port_mapping,
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
        for (key, value) in &spec.environment {
            command.args(["--env", &format!("{key}={value}")]);
        }
        command
            .arg(&spec.image)
            .arg("/usr/local/bin/jbox-entrypoint");
        Self::run_checked(command).context(
            "could not start Kata session. Confirm Docker's `kata` runtime and /dev/kvm are available",
        )?;

        // A successful `docker run -d` merely means the runtime accepted the
        // process. Verify that it survives long enough to expose SSH before
        // publishing session state to the caller.
        for _ in 0..25 {
            if !self.is_running(&spec.name)? {
                let mut logs = Self::command();
                logs.args(["logs", &spec.name]);
                let output = logs.output()?;
                let _ = self.stop(&spec.name);
                bail!(
                    "Kata session exited during startup: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            let mut port_command = Self::command();
            port_command.args(["port", &spec.name, "2222/tcp"]);
            let output = port_command.output()?;
            if output.status.success() && !output.stdout.is_empty() {
                let port = String::from_utf8(output.stdout)?;
                return port
                    .rsplit(':')
                    .next()
                    .context("Docker did not report SSH port")?
                    .trim()
                    .parse()
                    .context("invalid SSH port from Docker");
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let _ = self.stop(&spec.name);
        bail!("Kata session did not publish its SSH port within five seconds")
    }

    fn stop(&self, name: &str) -> Result<()> {
        let mut command = Self::command();
        command.args(["rm", "-f", name]);
        let output = command.output()?;
        if output.status.success() {
            return Ok(());
        }
        let error = String::from_utf8_lossy(&output.stderr);
        // Docker may report this while another jbox invocation is already
        // stopping the same disposable machine. Both desired end states are
        // equivalent, so cleanup remains idempotent.
        if error.contains("No such container") || error.contains("removal of container") {
            return Ok(());
        }
        bail!("docker failed: {}", error.trim());
    }

    fn is_running(&self, name: &str) -> Result<bool> {
        let out = Self::command()
            .args(["inspect", "--format", "{{.State.Running}}", name])
            .output()?;
        Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true")
    }
}

fn current_id(flag: &str) -> Result<String> {
    let output = Command::new("id").arg(flag).output()?;
    if !output.status.success() {
        bail!("could not determine the current user identity")
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
