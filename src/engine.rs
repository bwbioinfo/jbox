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

/// Docker discards published ports on `--network none`. Jbox cannot attach over
/// SSH in that mode, so reject it before any worktree or container is created.
/// Do not silently substitute bridge networking: that would grant egress the
/// project explicitly disabled.
pub fn validate_ssh_network(network: &Network) -> Result<()> {
    if !network.internet {
        bail!(
            "network.internet=false cannot launch a Jbox SSH guest: Docker --network none discards published ports. No guest was started. Use an explicitly permitted network only after reviewing its egress policy; offline SSH transport is not yet supported"
        );
    }
    Ok(())
}

/// Execute `cmd` with a hard wall-clock `timeout`.
///
/// On timeout the child is killed and reaped before returning.  The label
/// appears in every error message so callers get actionable diagnostics.
///
/// Returns `(success, stdout, stderr)` without bailing on a non-zero exit so
/// callers that treat a failed exit as a data signal (e.g. `docker port` when
/// the port is not yet published) can inspect the result themselves.
pub(crate) fn run_timed_raw(
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
                    bail!("`{label}` timed out after {}s", timeout.as_secs());
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
        validate_ssh_network(&spec.network)?;
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
            .map_err(|error| error.context(GuestStartUncertain))
            .context("could not start Jbox Kata session")?;

        // A successful `docker run -d` merely means the runtime accepted the
        // process. Poll until the container publishes its SSH port or the
        // wall-clock deadline expires -- whichever comes first.
        eprintln!(
            "Waiting for Kata guest {} SSH port (up to 30s)...",
            spec.name
        );
        let result = (|| {
            let port_deadline = Instant::now() + PORT_DEADLINE;
            loop {
                let remaining = port_deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    bail!("Kata session did not publish its SSH port within 30 seconds");
                }
                let mut inspect = Self::command();
                inspect.args(["inspect", "--format", "{{.State.Running}}", &spec.name]);
                let (inspected, running, _) =
                    run_timed_raw(inspect, CMD_TIMEOUT.min(remaining), "docker inspect")?;
                if !inspected || running != "true" {
                    bail!(
                        "Kata session exited during startup. \
                     Check that the guest image has a valid entrypoint."
                    );
                }

                let mut port_cmd = Self::command();
                port_cmd.args(["port", &spec.name, "2222/tcp"]);
                let remaining = port_deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
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
                    bail!("Kata session did not publish its SSH port within 30 seconds");
                }
                thread::sleep(PORT_POLL.min(remaining));
            }
        })();
        // Once Docker has accepted our guest, all polling errors must collect
        // evidence and stop it before callers remove its mounted worktrees.
        finish_startup(
            result,
            || self.startup_logs(&spec.name),
            || self.stop(&spec.name),
        )
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
    ///
    /// Docker returns stdout and stderr on separate pipes. Requesting
    /// timestamps lets us restore their chronological interleaving.
    pub fn startup_logs(&self, name: &str) -> Result<String> {
        let mut cmd = Self::command();
        cmd.args(startup_logs_args(name));
        let (ok, stdout, stderr) = run_timed_raw(cmd, CMD_TIMEOUT, "docker logs")?;
        if !ok {
            bail!("docker logs failed: {stderr}");
        }
        Ok(merge_timestamped_logs(&stdout, &stderr, STARTUP_LOG_LINES))
    }
}

/// A failed Docker client may leave a daemon-side create request in flight.
/// Immediate absence is not sufficient evidence to delete its bind sources.
#[derive(Debug)]
pub(crate) struct GuestStartUncertain;

impl std::fmt::Display for GuestStartUncertain {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "Docker run did not confirm completion. Guest creation may still be in flight",
        )
    }
}

impl std::error::Error for GuestStartUncertain {}

fn finish_startup(
    result: Result<u16>,
    logs: impl FnOnce() -> Result<String>,
    stop: impl FnOnce() -> Result<()>,
) -> Result<u16> {
    result.map_err(|error| {
        let logs =
            logs().unwrap_or_else(|error| format!("could not retrieve guest logs: {error:#}"));
        let cleanup = match stop() {
            Ok(()) => String::new(),
            Err(error) => format!("\nGuest cleanup also failed: {error:#}"),
        };
        error.context(format!(
            "guest startup failed. Container logs:\n{logs}{cleanup}"
        ))
    })
}

/// Maximum number of startup log lines requested from Docker and kept after merging.
const STARTUP_LOG_LINES: usize = 200;

fn startup_logs_args(name: &str) -> Vec<String> {
    vec![
        "logs".into(),
        "--timestamps".into(),
        "--tail".into(),
        STARTUP_LOG_LINES.to_string(),
        "--".into(),
        name.into(),
    ]
}

/// Merge Docker `--timestamps` output from stdout and stderr chronologically.
///
/// Docker emits RFC3339Nano UTC timestamps with trailing zeros trimmed, so
/// timestamps are compared by parsed (seconds, nanoseconds) rather than
/// lexically. Ties keep stdout before stderr and preserve per-stream order.
/// Lines without a parseable timestamp inherit their predecessor's key. Only
/// the latest `limit` merged lines are returned.
fn merge_timestamped_logs(stdout: &str, stderr: &str, limit: usize) -> String {
    fn keyed(text: &str, stream: u8) -> Vec<((i64, u32), u8, usize, &str)> {
        let mut last = (i64::MIN, 0u32);
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .enumerate()
            .map(|(index, line)| {
                if let Some(key) = line.split_once(' ').and_then(|(ts, _)| parse_rfc3339(ts)) {
                    last = key;
                }
                (last, stream, index, line)
            })
            .collect()
    }
    let mut lines = keyed(stdout, 0);
    lines.extend(keyed(stderr, 1));
    lines.sort_by_key(|(key, stream, index, _)| (*key, *stream, *index));
    let skip = lines.len().saturating_sub(limit);
    lines
        .into_iter()
        .skip(skip)
        .map(|(_, _, _, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a Docker RFC3339 timestamp into a sortable (seconds, nanos) key.
fn parse_rfc3339(ts: &str) -> Option<(i64, u32)> {
    let parsed = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some((parsed.timestamp(), parsed.timestamp_subsec_nanos()))
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

    #[test]
    fn offline_docker_network_is_rejected_before_launch() {
        let offline = Network {
            internet: false,
            host: false,
            lan: false,
        };
        let error = validate_ssh_network(&offline).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Docker --network none discards published ports")
        );
        assert!(
            validate_ssh_network(&Network {
                internet: true,
                ..offline
            })
            .is_ok()
        );
    }

    // --- startup log merging -------------------------------------------------

    #[test]
    fn uncertain_start_marker_survives_error_context_and_keeps_cause() {
        let error = anyhow::anyhow!("docker run timed out after 120 seconds")
            .context(GuestStartUncertain)
            .context("could not start Jbox Kata session");
        assert!(error.is::<GuestStartUncertain>());
        let message = format!("{error:#}");
        assert!(message.contains("may still be in flight"));
        assert!(message.contains("docker run timed out after 120 seconds"));
        assert!(!anyhow::anyhow!("SSH port unavailable").is::<GuestStartUncertain>());
    }

    #[test]
    fn startup_command_timeout_collects_logs_before_stopping() {
        let events = std::cell::RefCell::new(Vec::new());
        let mut command = Command::new("sleep");
        command.arg("1");
        let result =
            run_timed_raw(command, Duration::from_millis(20), "docker inspect").map(|_| 22);
        let error = finish_startup(
            result,
            || {
                events.borrow_mut().push("logs");
                Ok("phase=init START".into())
            },
            || {
                events.borrow_mut().push("stop");
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(*events.borrow(), ["logs", "stop"]);
        let text = format!("{error:#}");
        assert!(
            text.contains("docker inspect") && text.contains("timed out"),
            "{text}"
        );
        assert!(text.contains("phase=init START"));
    }

    #[test]
    fn malformed_port_keeps_original_error_when_logs_and_cleanup_fail() {
        let result = "not-a-port"
            .parse::<u16>()
            .context("Docker reported an invalid SSH port number");
        let error = finish_startup(
            result,
            || anyhow::bail!("logs unavailable"),
            || anyhow::bail!("remove failed"),
        )
        .unwrap_err();
        let text = format!("{error:#}");
        for expected in ["invalid SSH port", "logs unavailable", "remove failed"] {
            assert!(text.contains(expected), "{text}");
        }
    }

    #[test]
    fn successful_startup_does_not_read_logs_or_stop_guest() {
        assert_eq!(
            finish_startup(
                Ok(22),
                || panic!("unexpected logs"),
                || panic!("unexpected stop")
            )
            .unwrap(),
            22
        );
    }

    #[test]
    fn startup_logs_args_are_bounded_and_timestamped() {
        assert_eq!(
            startup_logs_args("-x"),
            ["logs", "--timestamps", "--tail", "200", "--", "-x"]
        );
    }

    #[test]
    fn merge_interleaves_streams_chronologically() {
        let out = "2026-01-01T00:00:01Z out-a\n2026-01-01T00:00:03.5Z out-b";
        let err = "2026-01-01T00:00:02.000000001Z err-a\n2026-01-01T00:00:03.25Z err-b";
        assert_eq!(
            merge_timestamped_logs(out, err, 200),
            "2026-01-01T00:00:01Z out-a\n2026-01-01T00:00:02.000000001Z err-a\n\
             2026-01-01T00:00:03.25Z err-b\n2026-01-01T00:00:03.5Z out-b"
        );
    }

    #[test]
    fn merge_equal_timestamps_is_stable() {
        let ts = "2026-01-01T00:00:01.1Z";
        let out = format!("{ts} o1\n{ts} o2");
        let err = format!("{ts} e1\n{ts} e2");
        assert_eq!(
            merge_timestamped_logs(&out, &err, 200),
            format!("{ts} o1\n{ts} o2\n{ts} e1\n{ts} e2")
        );
    }

    #[test]
    fn merge_compares_trimmed_fractions_numerically_across_days() {
        // Trimmed fractions (.10 == .1 < .9) and day rollover order by value.
        let out = "2026-02-28T23:59:59.9Z late\n2026-03-01T00:00:00Z next";
        let err = "2026-02-28T23:59:59.10Z early";
        assert_eq!(
            merge_timestamped_logs(out, err, 200),
            "2026-02-28T23:59:59.10Z early\n2026-02-28T23:59:59.9Z late\n2026-03-01T00:00:00Z next"
        );
    }

    #[test]
    fn merge_keeps_only_latest_lines_and_untimestamped_follow_predecessor() {
        let out = "2026-01-01T00:00:01Z a\ncontinuation\n2026-01-01T00:00:05Z c";
        let err = "2026-01-01T00:00:02Z b";
        assert_eq!(
            merge_timestamped_logs(out, err, 2),
            "2026-01-01T00:00:02Z b\n2026-01-01T00:00:05Z c"
        );
        assert_eq!(
            merge_timestamped_logs(out, err, 3),
            "continuation\n2026-01-01T00:00:02Z b\n2026-01-01T00:00:05Z c"
        );
        assert_eq!(merge_timestamped_logs("", "", 200), "");
    }

    #[test]
    fn rfc3339_rejects_malformed() {
        assert!(parse_rfc3339("2026-13-01T00:00:00Z").is_none());
        assert!(parse_rfc3339("2026-01-01T00:00:00").is_none());
        assert!(parse_rfc3339("2026-02-30T00:00:00Z").is_none());
        assert!(parse_rfc3339("99999999999-01-01T00:00:00Z").is_none());
        assert!(parse_rfc3339("2026-01-01T25:00:00Z").is_none());
        assert!(parse_rfc3339("garbage").is_none());
        assert!(parse_rfc3339("1970-01-01T00:00:01.5Z") == Some((1, 500_000_000)));
    }

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
