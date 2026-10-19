//! Refuse an incompatible client before it sends native protocol requests.
//!
//! Guest executables are immutable for a running VM, but the host client can
//! upgrade independently. Retained guests keep their pinned image until the
//! user explicitly stops and refreshes it.

use crate::{engine::run_timed_raw, paths::JboxPaths, state::Session};
use anyhow::{Result, bail};
use std::{process::Command, time::Duration};

const VERSION_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) fn ensure_compatible(paths: &JboxPaths, session: &Session) -> Result<()> {
    let mut local = Command::new("jcode");
    local.args(["--no-update", "--version"]);
    let local = version(local, "local Jcode version", VERSION_TIMEOUT)?;
    let config = paths.write_ssh_config(session)?;
    let mut remote = Command::new("ssh");
    remote
        .arg("-F")
        .arg(config)
        .args(["-T", "jbox", "/usr/local/bin/jcode --no-update --version"]);
    let remote = version(remote, "guest Jcode version", VERSION_TIMEOUT)?;
    require_matching_versions(&local, &remote, &session.id)
}

fn version(command: Command, label: &str, timeout: Duration) -> Result<String> {
    let (ok, stdout, stderr) = run_timed_raw(command, timeout, label)?;
    if !ok {
        bail!("could not query {label}: {stderr}");
    }
    // Fail closed on missing or malformed output, rather than treating two
    // empty responses as a match. Build hashes matter as well as release tags.
    if !stdout.starts_with("jcode v")
        || stdout.len() <= "jcode v".len()
        || stdout.len() > 256
        || stdout.chars().any(char::is_control)
    {
        bail!("could not query {label}: expected one `jcode v...` version line");
    }
    Ok(stdout)
}

fn require_matching_versions(local: &str, remote: &str, session_id: &str) -> Result<()> {
    if local != remote {
        bail!(
            "Jcode client/server version mismatch for jbox session {session_id}: local client is {local}, guest server is {remote}. No client was attached and the running guest was left unchanged. Refresh explicitly with `jbox stop {session_id}` then `jbox resume {session_id} --refresh-image`. This retains worktrees; saved conversation continuity requires jcode.persistent_credentials=true"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn matching_versions_include_build_identity() {
        require_matching_versions("jcode v0.99.0 (abc)", "jcode v0.99.0 (abc)", "calm-fox-123")
            .unwrap();
        for remote in ["jcode v0.98.1 (abc)", "jcode v0.99.0 (def)"] {
            let error = require_matching_versions("jcode v0.99.0 (abc)", remote, "calm-fox-123")
                .unwrap_err()
                .to_string();
            assert!(error.contains(remote));
            assert!(error.contains("jcode v0.99.0 (abc)"));
            assert!(error.contains("jbox stop calm-fox-123"));
            assert!(error.contains("jbox resume calm-fox-123 --refresh-image"));
        }
    }

    #[test]
    fn version_query_rejects_failed_empty_or_multiline_responses() {
        for script in [
            "printf 'jcode v0.99.0'; exit 1",
            "printf ''",
            "printf 'not-jcode v0.99.0'",
            "printf 'jcode v'",
            "printf 'jcode v0.99.0\\nunexpected'",
            "printf 'jcode v0.99.0\\033[31m'",
        ] {
            assert!(version(shell(script), "test version", VERSION_TIMEOUT).is_err());
        }
        assert_eq!(
            version(
                shell("printf 'jcode v0.99.0 (abc)\\n'"),
                "test version",
                VERSION_TIMEOUT
            )
            .unwrap(),
            "jcode v0.99.0 (abc)"
        );
    }

    #[test]
    fn stalled_version_query_is_bounded() {
        let started = std::time::Instant::now();
        let error = version(
            shell("exec sleep 10"),
            "test version",
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.to_string().contains("test version"));
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
