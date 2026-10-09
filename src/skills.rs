//! Host-side staging of configured GitHub skills.
//!
//! Skills are installed with the host `gh skill install` command into a
//! temporary directory beneath the parent of the destination, validated, and
//! only then published by an atomic rename. The host GitHub CLI authenticates
//! on its own. This module never reads, copies, or logs credentials, and it
//! discards the command's stdout and stderr so tokens cannot reach errors.

use crate::config::SkillSource;
use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_DEPTH: usize = 32;
const MAX_ENTRIES: usize = 100_000;

/// Stage all configured skill sources into `agents_dir/skills`.
///
/// `agents_dir` must not already exist. It is created only if every source
/// installs and validates. With no sources this is a no-op.
pub fn stage(sources: &[SkillSource], agents_dir: &Path) -> Result<()> {
    stage_with(sources, agents_dir, Path::new("gh"), INSTALL_TIMEOUT)
}

fn stage_with(
    sources: &[SkillSource],
    agents_dir: &Path,
    program: &Path,
    timeout: Duration,
) -> Result<()> {
    if sources.is_empty() {
        return Ok(());
    }
    if fs::symlink_metadata(agents_dir).is_ok() {
        bail!("agents directory already exists; refusing to overwrite it");
    }
    let parent = agents_dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).context("failed to create agents parent directory")?;
    let temp = tempfile::Builder::new()
        .prefix(".jbox-skills-")
        .tempdir_in(parent)
        .context("failed to create temporary skills directory")?;
    let skills = temp.path().join("skills");
    let work = temp.path().join("work");
    fs::create_dir(&skills)?;
    fs::create_dir(&work)?;

    let mut lock = serde_json::Map::new();
    for (index, source) in sources.iter().enumerate() {
        let base = work.join(index.to_string());
        let dir = base.join("skills");
        fs::create_dir_all(&dir)?;
        install(source, &dir, program, timeout).with_context(|| {
            format!(
                "failed to install skills from {}. Check `gh auth status` on the host, \
                 access to the repository, the pin, network connectivity, and GitHub rate limits",
                source.repository
            )
        })?;
        // Validate the whole per-source tree: gh writes its lock file beside --dir.
        let count = validate_tree(&base)
            .with_context(|| format!("unsafe skill content from {}", source.repository))?;
        let skill_count = validate_tree(&dir)?;
        if count == 0 || skill_count == 0 {
            bail!("{} installed no SKILL.md files", source.repository);
        }
        merge(&dir, &skills)
            .with_context(|| format!("failed to merge skills from {}", source.repository))?;
        merge_lock(&base.join(".skill-lock.json"), &mut lock)
            .with_context(|| format!("invalid skill lock metadata from {}", source.repository))?;
    }
    if !lock.is_empty() {
        let text = serde_json::to_vec_pretty(&serde_json::Value::Object(lock))?;
        fs::write(temp.path().join(".skill-lock.json"), text)?;
    }
    fs::remove_dir_all(&work)?;
    // Publish: rename the staging root, then keep it from being cleaned up.
    let staged = temp.keep();
    if let Err(err) = fs::rename(&staged, agents_dir) {
        let _ = fs::remove_dir_all(&staged);
        return Err(err).context("failed to publish agents directory");
    }
    Ok(())
}

fn install_args(source: &SkillSource, dir: &Path) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["skill".into(), "install".into()];
    args.push(source.repository.as_str().into());
    match &source.skill {
        Some(name) => args.push(name.as_str().into()),
        None => args.push("--all".into()),
    }
    args.push("--dir".into());
    args.push(dir.as_os_str().to_owned());
    if let Some(pin) = &source.pin {
        args.push("--pin".into());
        args.push(pin.as_str().into());
    }
    if source.allow_hidden_dirs {
        args.push("--allow-hidden-dirs".into());
    }
    args
}

fn install(source: &SkillSource, dir: &Path, program: &Path, timeout: Duration) -> Result<()> {
    let mut child = Command::new(program)
        .args(install_args(source, dir))
        .env("GH_PROMPT_DISABLED", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("could not start host `gh`")?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Descendants must not outlive the command.
                kill_group(&mut child, false);
                if status.success() {
                    return Ok(());
                }
                bail!("`gh skill install` failed with {status}");
            }
            Ok(None) if Instant::now() >= deadline => {
                kill_group(&mut child, true);
                bail!("`gh skill install` timed out after {}s", timeout.as_secs());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => {
                kill_group(&mut child, true);
                return Err(err).context("failed to wait for `gh skill install`");
            }
        }
    }
}

/// SIGKILL the child's whole process group, then reap the direct child.
fn kill_group(child: &mut Child, reap: bool) {
    // The child leads its own group (process_group(0)), so its pid is the pgid.
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    if reap {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Reject symlinks and special files; return the number of SKILL.md files.
fn validate_tree(root: &Path) -> Result<usize> {
    fn walk(dir: &Path, depth: usize, seen: &mut usize) -> Result<usize> {
        if depth > MAX_DEPTH {
            bail!("skill tree is too deep");
        }
        let mut skills = 0;
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            *seen += 1;
            if *seen > MAX_ENTRIES {
                bail!("skill tree has too many entries");
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                bail!(
                    "skill tree contains a symlink: {}",
                    entry.file_name().display()
                );
            } else if kind.is_dir() {
                skills += walk(&entry.path(), depth + 1, seen)?;
            } else if kind.is_file() {
                if entry.file_name() == "SKILL.md" {
                    skills += 1;
                }
            } else {
                bail!("skill tree contains a special file");
            }
        }
        Ok(skills)
    }
    walk(root, 0, &mut 0)
}

/// Merge one gh lock file into `acc` (`version` must agree, skill names must
/// be unique). Errors never include file content.
fn merge_lock(path: &Path, acc: &mut serde_json::Map<String, serde_json::Value>) -> Result<()> {
    use serde_json::Value;
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => bail!("lock metadata is not a regular file"),
        Err(_) => return Ok(()),
    }
    if fs::metadata(path)?.len() > 16 * 1024 * 1024 {
        bail!("lock metadata is too large");
    }
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&fs::read(path)?) else {
        bail!("lock metadata is not a JSON object");
    };
    for (key, value) in map {
        match (key.as_str(), value) {
            ("skills", Value::Object(new)) => {
                let Value::Object(cur) = acc
                    .entry("skills")
                    .or_insert_with(|| Value::Object(Default::default()))
                else {
                    bail!("lock metadata conflict");
                };
                for (name, entry) in new {
                    if cur.insert(name, entry).is_some() {
                        bail!("lock metadata has a duplicate skill entry");
                    }
                }
            }
            ("skills", _) => bail!("lock metadata skills is not an object"),
            (_, value) => {
                if acc.get(&key).is_some_and(|old| *old != value) {
                    bail!("lock metadata conflict");
                }
                acc.insert(key, value);
            }
        }
    }
    Ok(())
}

fn merge(from: &Path, to: &Path) -> Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if fs::symlink_metadata(&dest).is_ok() {
            bail!("duplicate skill entry {}", entry.file_name().display());
        }
        fs::rename(entry.path(), dest)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn source(repo: &str, skill: Option<&str>, pin: Option<&str>, hidden: bool) -> SkillSource {
        SkillSource {
            repository: repo.into(),
            managed: false,
            skill: skill.map(Into::into),
            pin: pin.map(Into::into),
            allow_hidden_dirs: hidden,
            private: false,
        }
    }

    /// Writes a fake `gh` that logs its args and runs `body` with `$DIR` set.
    fn fake(root: &Path, log: &Path, body: &str) -> PathBuf {
        let path = root.join("fake-gh");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
             while [ $# -gt 0 ]; do [ \"$1\" = --dir ] && DIR=$2; shift; done\n{body}\n",
            log.display()
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    const OK: &str = "mkdir -p \"$DIR/demo\" && echo hi > \"$DIR/demo/SKILL.md\" && printf '{\"version\":1,\"skills\":{\"%s\":{}}}' \"$$\" > \"$DIR/../.skill-lock.json\"";

    fn run(sources: &[SkillSource], body: &str) -> (tempfile::TempDir, PathBuf, Result<()>) {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("log");
        let prog = fake(tmp.path(), &log, body);
        let agents = tmp.path().join("out").join("agents");
        let result = stage_with(sources, &agents, &prog, Duration::from_secs(10));
        (tmp, agents, result)
    }

    fn log_of(tmp: &tempfile::TempDir) -> String {
        fs::read_to_string(tmp.path().join("log")).unwrap_or_default()
    }

    #[test]
    fn publishes_skills_and_passes_args() {
        let sources = [source("o/r", Some("demo"), Some("v1"), true)];
        let (tmp, agents, result) = run(&sources, OK);
        result.unwrap();
        assert!(agents.join("skills/demo/SKILL.md").is_file());
        let lock: serde_json::Value =
            serde_json::from_slice(&fs::read(agents.join(".skill-lock.json")).unwrap()).unwrap();
        assert_eq!(lock["version"], 1);
        assert_eq!(lock["skills"].as_object().unwrap().len(), 1);
        let log = log_of(&tmp);
        assert!(log.starts_with("skill install o/r demo --dir "), "{log}");
        assert!(log.contains("--pin v1 --allow-hidden-dirs"), "{log}");
        assert!(!log.contains(" --all "));
        let leftovers: Vec<_> = fs::read_dir(agents.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "temp dir leaked");
    }

    #[test]
    fn installs_all_without_skill_name() {
        let (tmp, _, result) = run(&[source("o/r", None, None, false)], OK);
        result.unwrap();
        let log = log_of(&tmp);
        assert!(log.starts_with("skill install o/r --all --dir "), "{log}");
        assert!(!log.contains("--pin") && !log.contains("--allow-hidden-dirs"));
    }

    #[test]
    fn failed_install_does_not_publish_or_leak_output() {
        let body = "echo ghp_SECRETTOKEN; echo ghp_SECRETTOKEN >&2; exit 3";
        let (tmp, agents, result) = run(&[source("o/r", None, None, false)], body);
        let msg = format!("{:#}", result.unwrap_err());
        assert!(!msg.contains("SECRET"), "{msg}");
        assert!(msg.contains("failed"));
        assert!(!agents.exists());
        assert_eq!(fs::read_dir(agents.parent().unwrap()).unwrap().count(), 0);
        drop(tmp);
    }

    #[test]
    fn second_source_failure_prevents_partial_publish() {
        let body = "case \"$*\" in *bad/repo*) exit 1;; esac; ";
        let (_tmp, agents, result) = run(
            &[
                source("o/r", None, None, false),
                source("bad/repo", None, None, false),
            ],
            &format!("{body}{OK}"),
        );
        assert!(result.is_err());
        assert!(!agents.exists());
    }

    #[test]
    fn empty_install_is_failure() {
        let (_tmp, agents, result) = run(&[source("o/r", None, None, false)], "true");
        assert!(format!("{:#}", result.unwrap_err()).contains("no SKILL.md"));
        assert!(!agents.exists());
    }

    #[test]
    fn symlinks_are_rejected() {
        let body = format!("{OK}; ln -s /etc \"$DIR/demo/escape\"");
        let (_tmp, agents, result) = run(&[source("o/r", None, None, false)], &body);
        assert!(format!("{:#}", result.unwrap_err()).contains("symlink"));
        assert!(!agents.exists());
    }

    #[test]
    fn duplicate_skill_names_fail() {
        let sources = [
            source("o/a", None, None, false),
            source("o/b", None, None, false),
        ];
        let (_tmp, agents, result) = run(&sources, OK);
        assert!(format!("{:#}", result.unwrap_err()).contains("duplicate"));
        assert!(!agents.exists());
    }

    #[test]
    fn timeout_kills_and_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("log");
        let prog = fake(tmp.path(), &log, "sleep 30");
        let agents = tmp.path().join("agents");
        let started = Instant::now();
        let err = stage_with(
            &[source("o/r", None, None, false)],
            &agents,
            &prog,
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!agents.exists());
    }

    #[test]
    fn empty_sources_is_noop_and_existing_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        stage(&[], &agents).unwrap();
        assert!(!agents.exists());
        fs::create_dir(&agents).unwrap();
        let log = tmp.path().join("log");
        let prog = fake(tmp.path(), &log, OK);
        let err = stage_with(
            &[source("o/r", None, None, false)],
            &agents,
            &prog,
            Duration::from_secs(5),
        );
        assert!(err.is_err());
        assert!(!log.exists(), "gh must not run when destination exists");
    }

    #[test]
    fn later_empty_source_fails_after_earlier_success() {
        let body = "case \"$DIR\" in */1/skills) exit 0;; esac; mkdir -p \"$DIR/demo\" && echo hi > \"$DIR/demo/SKILL.md\"";
        let sources = [
            source("o/r", None, None, false),
            source("o/empty", None, None, false),
        ];
        let (tmp, agents, result) = run(&sources, body);
        assert!(format!("{:#}", result.unwrap_err()).contains("no SKILL.md"));
        assert!(!agents.exists());
        assert_eq!(fs::read_dir(agents.parent().unwrap()).unwrap().count(), 0);
        drop(tmp);
    }

    #[test]
    fn symlink_destination_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        std::os::unix::fs::symlink("/nonexistent", &agents).unwrap();
        let prog = fake(tmp.path(), &tmp.path().join("log"), OK);
        let r = stage_with(
            &[source("o/r", None, None, false)],
            &agents,
            &prog,
            Duration::from_secs(5),
        );
        assert!(r.is_err());
    }

    #[test]
    fn lock_files_merge_and_conflicts_fail_without_leaking() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.json");
        let b = dir.path().join("b.json");
        fs::write(&a, r#"{"version":1,"skills":{"x":{}}}"#).unwrap();
        fs::write(&b, r#"{"version":1,"skills":{"y":{}}}"#).unwrap();
        let mut acc = serde_json::Map::new();
        merge_lock(&a, &mut acc).unwrap();
        merge_lock(&b, &mut acc).unwrap();
        assert_eq!(acc["skills"].as_object().unwrap().len(), 2);
        fs::write(&b, r#"{"version":2,"skills":{"z":{"t":"ghp_SECRET"}}}"#).unwrap();
        let err = format!("{:#}", merge_lock(&b, &mut acc).unwrap_err());
        assert!(err.contains("conflict") && !err.contains("SECRET"));
        fs::write(&b, "ghp_SECRET not json").unwrap();
        let err = format!("{:#}", merge_lock(&b, &mut acc).unwrap_err());
        assert!(!err.contains("SECRET"));
    }

    #[test]
    fn timeout_kills_descendants() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("late");
        let body = format!("(sleep 1; echo x > '{}') &\nsleep 30", sentinel.display());
        let prog = fake(tmp.path(), &tmp.path().join("log"), &body);
        let agents = tmp.path().join("agents");
        let err = stage_with(
            &[source("o/r", None, None, false)],
            &agents,
            &prog,
            Duration::from_millis(300),
        );
        assert!(err.is_err());
        std::thread::sleep(Duration::from_millis(1600));
        assert!(!sentinel.exists(), "descendant survived timeout");
    }

    #[test]
    fn descendants_are_killed_after_gh_exits() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("late");
        let body = format!("(sleep 1; echo x > '{}') &\n{OK}", sentinel.display());
        let prog = fake(tmp.path(), &tmp.path().join("log"), &body);
        let agents = tmp.path().join("agents");
        stage_with(
            &[source("o/r", None, None, false)],
            &agents,
            &prog,
            Duration::from_secs(10),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(1600));
        assert!(!sentinel.exists(), "descendant survived gh exit");
    }

    #[test]
    fn stdin_is_null() {
        let body = format!("read x && exit 9; {OK}");
        let (_tmp, _agents, result) = run(&[source("o/r", None, None, false)], &body);
        result.unwrap();
    }
}
