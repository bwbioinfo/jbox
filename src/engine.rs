use crate::config::Network;
use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Per-invocation wall-clock limit for any single Docker CLI call.
const CMD_TIMEOUT: Duration = Duration::from_secs(10);
const RUN_TIMEOUT: Duration = Duration::from_secs(120);
const STOP_TIMEOUT: Duration = Duration::from_secs(20);

/// Wall-clock budget for the container to publish its SSH port after `docker run -d`.
const PORT_DEADLINE: Duration = Duration::from_secs(30);

/// How long to sleep between consecutive port-readiness polls.
const PORT_POLL: Duration = Duration::from_millis(200);

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

/// Execute `cmd` with a hard wall-clock `timeout`.
///
/// On timeout the child is killed and reaped before returning.  The label
/// appears in every error message so callers get actionable diagnostics.
///
/// Returns `(success, stdout, stderr)` without bailing on a non-zero exit so
/// callers that treat a failed exit as a data signal (e.g. `docker port` when
/// the port is not yet published) can inspect the result themselves.
fn run_timed_raw(
    mut cmd: Command,
    timeout: Duration,
    label: &str,
) -> Result<(bool, String, String)> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not launch `{label}`"))?;

    // Drain both pipes in background threads to prevent buffer-full deadlock.
    let mut raw_out = child.stdout.take().expect("stdout was piped");
    let mut raw_err = child.stderr.take().expect("stderr was piped");
    let (tx_out, rx_out) = mpsc::channel::<Vec<u8>>();
    let (tx_err, rx_err) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut v = Vec::new();
        let _ = raw_out.read_to_end(&mut v);
        let _ = tx_out.send(v);
    });
    thread::spawn(move || {
        let mut v = Vec::new();
        let _ = raw_err.read_to_end(&mut v);
        let _ = tx_err.send(v);
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child
            .try_wait()
            .with_context(|| format!("could not poll `{label}`"))?
        {
            Some(status) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let out = rx_out
                    .recv_timeout(remaining.max(Duration::from_millis(500)))
                    .with_context(|| format!("`{label}` timed out reading stdout"))?;
                let remaining = deadline.saturating_duration_since(Instant::now());
                let err = rx_err
                    .recv_timeout(remaining.max(Duration::from_millis(500)))
                    .with_context(|| format!("`{label}` timed out reading stderr"))?;
                let stdout = String::from_utf8_lossy(&out).trim().to_owned();
                let stderr = String::from_utf8_lossy(&err).trim().to_owned();
                return Ok((status.success(), stdout, stderr));
            }
            None => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    let _ = child.kill();
                    let _ = child.wait(); // reap zombie so no fd/pid leak
                    bail!(
                        "`{label}` timed out after {}s; is the Docker daemon responsive? \
                         Run `docker info` to check.",
                        timeout.as_secs()
                    );
                }
                thread::sleep(Duration::from_millis(50).min(remaining));
            }
        }
    }
}

/// Convenience wrapper: like `run_timed_raw` but bails on a non-zero exit.
fn run_timed(cmd: Command, timeout: Duration, label: &str) -> Result<String> {
    let (ok, stdout, stderr) = run_timed_raw(cmd, timeout, label)?;
    if !ok {
        bail!("`{label}` failed: {stderr}");
    }
    Ok(stdout)
}

/// Docker is selected for the MVP because Kata on current Arch-based hosts is a
/// rootful deployment. Podman's stronger rootless network defaults cannot be
/// combined with Kata there. The sandbox never receives Docker's socket.
pub struct DockerEngine;
impl DockerEngine {
    fn command() -> Command {
        Command::new("docker")
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
        let mut cmd = Self::command();
        cmd.args(["info", "--format", "{{json .Runtimes}}"]);
        let (ok, stdout, stderr) = run_timed_raw(cmd, CMD_TIMEOUT, "docker info")
            .context("Docker is required. Install Kata and register its runtime with Docker.")?;
        if !ok {
            bail!(
                "Docker is unusable: {}. Verify Docker is running with `docker info`.",
                stderr
            );
        }
        let runtimes: serde_json::Value =
            serde_json::from_str(&stdout).context("Docker returned invalid runtime metadata")?;
        if runtimes.get("kata").is_none() {
            bail!(
                "Docker has no runtime named `kata`. Register Kata in /etc/docker/daemon.json, \
                 restart Docker, and verify `docker info` reports it."
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
        eprintln!("Starting Kata guest {} (up to 120s)...", spec.name);
        run_timed(command, RUN_TIMEOUT, "docker run")
            .context("could not start Jbox Kata session")?;

        // A successful `docker run -d` merely means the runtime accepted the
        // process. Poll until the container publishes its SSH port or the
        // wall-clock deadline expires -- whichever comes first.
        eprintln!(
            "Waiting for Kata guest {} SSH port (up to 30s)...",
            spec.name
        );
        let port_deadline = Instant::now() + PORT_DEADLINE;
        loop {
            let remaining = port_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = self.stop(&spec.name);
                bail!("Kata session did not publish its SSH port within 30 seconds");
            }
            let mut inspect = Self::command();
            inspect.args(["inspect", "--format", "{{.State.Running}}", &spec.name]);
            let (inspected, running, _) =
                run_timed_raw(inspect, CMD_TIMEOUT.min(remaining), "docker inspect")?;
            if !inspected || running != "true" {
                // Container exited during startup: grab logs and report them.
                let logs = self
                    .startup_logs(&spec.name)
                    .unwrap_or_else(|error| format!("could not retrieve guest logs: {error:#}"));
                let _ = self.stop(&spec.name);
                bail!(
                    "Kata session exited during startup.\nContainer logs:\n{logs}\n\
                     Check that the guest image has a valid entrypoint."
                );
            }

            let mut port_cmd = Self::command();
            port_cmd.args(["port", &spec.name, "2222/tcp"]);
            let remaining = port_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = self.stop(&spec.name);
                bail!("Kata session did not publish its SSH port within 30 seconds");
            }
            let (ok, port_out, _) =
                run_timed_raw(port_cmd, CMD_TIMEOUT.min(remaining), "docker port")?;
            if ok && !port_out.is_empty() {
                return port_out
                    .rsplit(':')
                    .next()
                    .context("Docker did not report an SSH port")?
                    .trim()
                    .parse()
                    .context("Docker reported an invalid SSH port number");
            }

            let remaining = port_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = self.stop(&spec.name);
                bail!(
                    "Kata session did not publish its SSH port within {}s. \
                     Check that the guest image entrypoint starts sshd on port 2222 \
                     and that the kata runtime is installed correctly.",
                    PORT_DEADLINE.as_secs()
                );
            }
            thread::sleep(PORT_POLL.min(remaining));
        }
    }

    fn stop(&self, name: &str) -> Result<()> {
        let mut cmd = Self::command();
        cmd.args(["rm", "-f", name]);
        match run_timed(cmd, STOP_TIMEOUT, "docker rm") {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = e.to_string();
                // Docker may report this while another jbox invocation is already
                // stopping the same disposable machine. Both desired end states are
                // equivalent, so cleanup remains idempotent.
                if msg.contains("No such container") || msg.contains("removal of container") {
                    return Ok(());
                }
                Err(e).with_context(|| {
                    format!(
                        "could not remove container '{name}'; \
                         try `docker rm -f {name}` to remove it manually"
                    )
                })
            }
        }
    }

    fn is_running(&self, name: &str) -> Result<bool> {
        let mut cmd = Self::command();
        cmd.args(["inspect", "--format", "{{.State.Running}}", name]);
        let (ok, stdout, _) = run_timed_raw(cmd, CMD_TIMEOUT, "docker inspect")?;
        Ok(ok && stdout == "true")
    }
}

impl DockerEngine {
    /// Retrieve the most recent container log lines for post-mortem diagnostics.
    pub fn startup_logs(&self, name: &str) -> Result<String> {
        let mut cmd = Self::command();
        cmd.args(["logs", "--tail", "50", "--", name]);
        let (ok, stdout, stderr) = run_timed_raw(cmd, CMD_TIMEOUT, "docker logs")?;
        if !ok {
            bail!("docker logs failed: {stderr}");
        }
        let combined = format!("{stdout}\n{stderr}");
        Ok(combined.trim().to_owned())
    }
}

fn current_id(flag: &str) -> Result<String> {
    let mut cmd = Command::new("id");
    cmd.arg(flag);
    // `id(1)` is a local process; 5 s is generous but bounded.
    run_timed(cmd, Duration::from_secs(5), &format!("id {flag}"))
        .context("could not determine the current user identity")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    // --- run_timed_raw -------------------------------------------------------

    #[test]
    fn raw_success_captures_stdout_and_stderr() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out; echo err >&2"]);
        let (ok, stdout, stderr) = run_timed_raw(cmd, Duration::from_secs(5), "sh").unwrap();
        assert!(ok);
        assert_eq!(stdout, "out");
        assert_eq!(stderr, "err");
    }

    #[test]
    fn raw_non_zero_exit_does_not_bail() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo body; exit 2"]);
        let (ok, stdout, _stderr) = run_timed_raw(cmd, Duration::from_secs(5), "sh-fail").unwrap();
        assert!(!ok);
        assert_eq!(stdout, "body");
    }

    #[test]
    fn raw_timeout_kills_and_returns_error() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let timeout = Duration::from_millis(300);
        let start = Instant::now();
        let err = run_timed_raw(cmd, timeout, "sleep-long").unwrap_err();
        let elapsed = start.elapsed();
        // Should return well under 2 s; the 60-second sleep must be killed.
        assert!(
            elapsed < Duration::from_secs(2),
            "timeout not enforced; elapsed {elapsed:?}"
        );
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn raw_timeout_message_contains_label() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let err = run_timed_raw(cmd, Duration::from_millis(200), "MY_LABEL").unwrap_err();
        assert!(
            err.to_string().contains("MY_LABEL"),
            "label missing from error: {err}"
        );
    }

    // --- run_timed -----------------------------------------------------------

    #[test]
    fn timed_success_returns_trimmed_stdout() {
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        let out = run_timed(cmd, Duration::from_secs(5), "echo").unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn timed_non_zero_exit_bails() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo problem >&2; exit 1"]);
        let err = run_timed(cmd, Duration::from_secs(5), "sh-err").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("problem"), "stderr missing from error: {msg}");
    }

    #[test]
    fn timed_multiline_output_trimmed() {
        let mut cmd = Command::new("printf");
        cmd.arg("line1\nline2");
        let out = run_timed(cmd, Duration::from_secs(5), "printf").unwrap();
        assert_eq!(out, "line1\nline2");
    }

    #[test]
    fn timed_timeout_kills_process() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let timeout = Duration::from_millis(300);
        let start = Instant::now();
        let err = run_timed(cmd, timeout, "sleep-timed").unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "kill too slow: {:?}",
            start.elapsed()
        );
        assert!(err.to_string().contains("timed out"), "unexpected: {err}");
    }
}
