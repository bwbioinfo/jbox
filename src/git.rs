use crate::state::RepoState;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::process::Command;

pub struct Git;
pub struct WorktreeCreated {
    pub branch: String,
    pub commit: String,
}
impl Git {
    fn run(repo: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .context("could not execute git")?;
        if !output.status.success() {
            bail!(
                "git {} failed in {}: {}",
                args.join(" "),
                repo.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8(output.stdout)?.trim().into())
    }
    pub fn add_worktree(
        &self,
        repo: &Path,
        worktree: &Path,
        session: &str,
        name: &str,
    ) -> Result<WorktreeCreated> {
        let commit = Self::run(repo, &["rev-parse", "HEAD"])?;
        let branch = format!("jbox/{session}/{name}");
        Self::run(
            repo,
            &[
                "worktree",
                "add",
                "--no-checkout",
                "-b",
                &branch,
                &worktree.to_string_lossy(),
                &commit,
            ],
        )?;
        Self::run(worktree, &["reset", "--hard", &commit])?;
        let _ = Self::run(worktree, &["submodule", "update", "--init", "--recursive"]);
        Ok(WorktreeCreated { branch, commit })
    }
    pub fn remove_worktree(&self, repo: &Path, worktree: &Path, force: bool) -> Result<()> {
        if !worktree.exists() {
            return Ok(());
        }
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        let worktree_path = worktree.to_string_lossy();
        args.push(&worktree_path);
        Self::run(repo, &args)?;
        Ok(())
    }
    /// Replace a linked worktree's host-facing `.git` file with a self-contained
    /// metadata copy for the guest. The original link is retained outside the
    /// mount and restored when the session stops. This keeps the host `.git`
    /// directory out of the microVM while allowing normal Git operations.
    pub fn isolate_guest_metadata(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        commit: &str,
        backup_gitfile: &Path,
    ) -> Result<()> {
        let gitfile = worktree.join(".git");
        let original = fs::read_to_string(&gitfile)
            .with_context(|| format!("cannot read linked git file at {}", gitfile.display()))?;
        if !original.starts_with("gitdir: ") {
            bail!(
                "expected {} to be a linked worktree git file",
                gitfile.display()
            );
        }
        fs::create_dir_all(
            backup_gitfile
                .parent()
                .context("gitfile backup must have a parent directory")?,
        )?;
        fs::write(backup_gitfile, original)?;

        let proxy = worktree.join(".jbox-git-proxy");
        let _ = fs::remove_dir_all(&proxy);
        let output = Command::new("git")
            .args(["clone", "--quiet", "--bare", "--no-local"])
            .arg(repo)
            .arg(&proxy)
            .output()
            .context("could not create isolated Git metadata")?;
        if !output.status.success() {
            bail!(
                "could not create isolated Git metadata: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Self::run_git_dir(&proxy, &["config", "core.bare", "false"])?;
        // Do not leave the canonical host repository path as the guest's
        // remote. It is not mounted and would disclose an unnecessary path.
        let _ = Self::run_git_dir(&proxy, &["remote", "remove", "origin"]);
        if let Ok(remote) = Self::run(repo, &["remote", "get-url", "origin"])
            && remote_url_is_safe(&remote)
        {
            Self::run_git_dir(&proxy, &["remote", "add", "origin", &remote])?;
        }
        Self::run_git_dir(
            &proxy,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        )?;

        fs::remove_file(&gitfile)?;
        fs::rename(&proxy, &gitfile)?;
        Self::run(worktree, &["reset", "--hard", commit])?;
        Ok(())
    }

    /// Import commits made using the guest metadata, then restore the normal
    /// linked-worktree git file so the branch remains a standard host branch.
    pub fn restore_and_import_guest_metadata(&self, repo: &RepoState) -> Result<()> {
        let gitfile = repo.worktree.join(".git");
        if !gitfile.is_dir() {
            return Ok(());
        }
        Self::run(
            &repo.source,
            &["fetch", "--quiet", &gitfile.to_string_lossy(), &repo.branch],
        )?;
        Self::run(
            &repo.source,
            &[
                "update-ref",
                &format!("refs/heads/{}", repo.branch),
                "FETCH_HEAD",
            ],
        )?;
        self.restore_guest_metadata(repo)
    }

    pub fn restore_guest_metadata(&self, repo: &RepoState) -> Result<()> {
        let gitfile = repo.worktree.join(".git");
        if !gitfile.is_dir() {
            return Ok(());
        }
        fs::remove_dir_all(&gitfile)?;
        fs::copy(&repo.host_gitfile, &gitfile).with_context(|| {
            format!(
                "cannot restore host worktree git link for {}",
                repo.worktree.display()
            )
        })?;
        Self::run(&repo.worktree, &["reset", "--mixed", &repo.branch])?;
        Ok(())
    }
    pub fn status(&self, worktree: &Path) -> Result<String> {
        Self::run(worktree, &["status", "--short", "--branch"])
    }
    pub fn diff_stat(&self, repo: &RepoState) -> Result<String> {
        let uncommitted = Self::run(&repo.worktree, &["diff", "--stat"])?;
        let staged = Self::run(&repo.worktree, &["diff", "--cached", "--stat"])?;
        let range = format!("{}..HEAD", repo.base_commit);
        let commits = Self::run(&repo.worktree, &["log", "--oneline", &range])?;
        Ok(format!(
            "Uncommitted:\n{uncommitted}\nStaged:\n{staged}\nUnique commits:\n{commits}\n"
        ))
    }
    pub fn has_changes_or_unique_commits(&self, repo: &RepoState) -> Result<bool> {
        if !Self::run(&repo.worktree, &["status", "--porcelain"])?.is_empty() {
            return Ok(true);
        }
        let range = format!("{}..HEAD", repo.base_commit);
        Ok(!Self::run(&repo.worktree, &["rev-list", "--count", &range])?.eq("0"))
    }

    fn run_git_dir(git_dir: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .arg(format!("--git-dir={}", git_dir.display()))
            .args(args)
            .output()?;
        if !output.status.success() {
            bail!(
                "git {} failed in {}: {}",
                args.join(" "),
                git_dir.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8(output.stdout)?.trim().into())
    }
}

fn remote_url_is_safe(url: &str) -> bool {
    !url.split_once("://")
        .is_some_and(|(_, authority_and_path)| {
            authority_and_path
                .split('/')
                .next()
                .is_some_and(|authority| authority.contains('@'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    #[test]
    fn creates_isolated_worktree_at_head() {
        let tmp = tempdir().unwrap();
        git(tmp.path(), &["init"]);
        git(tmp.path(), &["config", "user.email", "test@example.com"]);
        git(tmp.path(), &["config", "user.name", "Test"]);
        std::fs::write(tmp.path().join("a"), "base").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-m", "base"]);
        std::fs::write(tmp.path().join("a"), "dirty host").unwrap();
        let wt = tmp.path().join("worktree");
        let made = Git
            .add_worktree(tmp.path(), &wt, "bright-fox-123", "repo")
            .unwrap();
        assert!(made.branch.starts_with("jbox/"));
        assert_eq!(std::fs::read_to_string(wt.join("a")).unwrap(), "base");
        assert!(Git.remove_worktree(tmp.path(), &wt, false).is_ok());
    }

    #[test]
    fn imports_guest_commit_without_mounting_host_git_dir() {
        let tmp = tempdir().unwrap();
        git(tmp.path(), &["init"]);
        git(tmp.path(), &["config", "user.email", "test@example.com"]);
        git(tmp.path(), &["config", "user.name", "Test"]);
        std::fs::write(tmp.path().join("a"), "base").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-m", "base"]);
        let wt = tmp.path().join("worktree");
        let made = Git
            .add_worktree(tmp.path(), &wt, "bright-fox-123", "repo")
            .unwrap();
        let host_gitfile = tmp.path().join("host-gitfile");
        Git.isolate_guest_metadata(tmp.path(), &wt, &made.branch, &made.commit, &host_gitfile)
            .unwrap();
        assert!(wt.join(".git").is_dir());
        git(&wt, &["config", "user.email", "test@example.com"]);
        git(&wt, &["config", "user.name", "Test"]);
        std::fs::write(wt.join("a"), "guest change").unwrap();
        git(&wt, &["add", "a"]);
        git(&wt, &["commit", "-m", "guest"]);
        let repo = RepoState {
            name: "repo".into(),
            source: tmp.path().into(),
            worktree: wt.clone(),
            mount: "/workspace/repo".into(),
            branch: made.branch.clone(),
            base_commit: made.commit,
            host_gitfile,
        };
        Git.restore_and_import_guest_metadata(&repo).unwrap();
        assert!(wt.join(".git").is_file());
        assert_eq!(
            Git::run(tmp.path(), &["rev-parse", &made.branch]).unwrap(),
            Git::run(&wt, &["rev-parse", "HEAD"]).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("a")).unwrap(),
            "guest change"
        );
        assert!(Git.remove_worktree(tmp.path(), &wt, false).is_ok());
    }
}
