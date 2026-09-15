use crate::{config::ResolvedGitAuthor, state::RepoState};
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
        author: &ResolvedGitAuthor,
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
        if let Some(name) = &author.name {
            Self::run_git_dir(&proxy, &["config", "user.name", name])?;
        }
        if let Some(email) = &author.email {
            Self::run_git_dir(&proxy, &["config", "user.email", email])?;
        }

        fs::remove_file(&gitfile)?;
        fs::rename(&proxy, &gitfile)?;
        Self::run(worktree, &["reset", "--hard", commit])?;
        Ok(())
    }

    /// Import the guest's latest committed snapshot while leaving its isolated
    /// Git directory in place. This is safe to use for a running session: any
    /// later commits remain on the same guest branch and can be imported again.
    pub fn import_guest_commits(&self, repo: &RepoState) -> Result<()> {
        let gitfile = repo.worktree.join(".git");
        if !gitfile.is_dir() {
            return Ok(());
        }
        Self::run(
            &repo.source,
            &["fetch", "--quiet", &gitfile.to_string_lossy(), &repo.branch],
        )?;
        let session_ref = format!("refs/heads/{}", repo.branch);
        let is_fast_forward = Command::new("git")
            .arg("-C")
            .arg(&repo.source)
            .args(["merge-base", "--is-ancestor", &session_ref, "FETCH_HEAD"])
            .status()
            .context("could not verify imported guest history")?;
        if !is_fast_forward.success() {
            bail!(
                "guest branch `{}` rewrote history; refusing to replace its host snapshot",
                repo.branch
            );
        }
        Self::run(&repo.source, &["update-ref", &session_ref, "FETCH_HEAD"])?;
        Ok(())
    }

    /// Import commits made using the guest metadata, then restore the normal
    /// linked-worktree git file so the branch remains a standard host branch.
    pub fn restore_and_import_guest_metadata(&self, repo: &RepoState) -> Result<()> {
        self.import_guest_commits(repo)?;
        self.restore_guest_metadata(repo)
    }

    /// Verify that the host checkout is clean and currently on `target`, then
    /// fast-forward it to the imported guest session branch. The caller imports
    /// all session branches before calling this method, avoiding a live guest
    /// metadata mutation during the merge itself.
    pub fn preflight_accept_snapshot(&self, repo: &RepoState, target: &str) -> Result<()> {
        let target = Self::run(&repo.source, &["check-ref-format", "--branch", target])?;
        let current = Self::run(&repo.source, &["branch", "--show-current"])?;
        if current.is_empty() {
            bail!(
                "cannot accept {}: host repository {} is detached; check out `{target}` first",
                repo.name,
                repo.source.display()
            );
        }
        if current != target {
            bail!(
                "cannot accept {}: host repository {} is on `{current}`, not `{target}`; check out the target branch first",
                repo.name,
                repo.source.display()
            );
        }
        if !Self::run(&repo.source, &["status", "--porcelain"])?.is_empty() {
            bail!(
                "cannot accept {}: host repository {} has uncommitted changes",
                repo.name,
                repo.source.display()
            );
        }
        let target_ref = format!("refs/heads/{target}");
        Self::run(&repo.source, &["rev-parse", "--verify", &target_ref])?;
        let ancestor = Command::new("git")
            .arg("-C")
            .arg(&repo.source)
            .args(["merge-base", "--is-ancestor", &target, &repo.branch])
            .status()
            .context("could not verify whether the guest snapshot can fast-forward the host")?;
        if !ancestor.success() {
            bail!(
                "cannot accept {}: `{target}` has diverged from guest branch `{}`; merge or rebase it manually",
                repo.name,
                repo.branch
            );
        }
        Ok(())
    }

    /// Fast-forward a host branch after `preflight_accept_snapshot` succeeded.
    /// `git merge` still protects the checkout if another process changes it
    /// between the preflight and this final operation.
    pub fn fast_forward_snapshot(&self, repo: &RepoState, target: &str) -> Result<()> {
        let target = Self::run(&repo.source, &["check-ref-format", "--branch", target])?;
        let current = Self::run(&repo.source, &["branch", "--show-current"])?;
        if current != target {
            bail!(
                "cannot accept {}: host branch changed from `{target}` before it could be merged",
                repo.name
            );
        }
        Self::run(&repo.source, &["merge", "--ff-only", &repo.branch])?;
        Ok(())
    }

    pub fn accept_snapshot(&self, repo: &RepoState, target: &str) -> Result<()> {
        self.preflight_accept_snapshot(repo, target)?;
        self.fast_forward_snapshot(repo, target)
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
    pub fn current_branch(&self, repository: &Path) -> Result<String> {
        Self::run(repository, &["branch", "--show-current"])
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
        let guest_author = ResolvedGitAuthor {
            name: Some("Guest Author".into()),
            email: Some("guest@example.com".into()),
        };
        Git.isolate_guest_metadata(
            tmp.path(),
            &wt,
            &made.branch,
            &made.commit,
            &host_gitfile,
            &guest_author,
        )
        .unwrap();
        assert!(wt.join(".git").is_dir());
        assert_eq!(
            Git::run(&wt, &["config", "user.name"]).unwrap(),
            "Guest Author"
        );
        assert_eq!(
            Git::run(&wt, &["config", "user.email"]).unwrap(),
            "guest@example.com"
        );
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

    #[test]
    fn accepts_committed_snapshots_without_stopping_the_guest_worktree() {
        let tmp = tempdir().unwrap();
        git(tmp.path(), &["init"]);
        git(tmp.path(), &["config", "user.email", "host@example.com"]);
        git(tmp.path(), &["config", "user.name", "Host"]);
        let target = Git::run(tmp.path(), &["branch", "--show-current"]).unwrap();
        std::fs::write(tmp.path().join("a"), "base").unwrap();
        git(tmp.path(), &["add", "."]);
        git(tmp.path(), &["commit", "-m", "base"]);

        // Real jbox worktrees are outside the host checkout. Keep the test
        // layout faithful so the host cleanliness gate can be exercised.
        let session = tempdir().unwrap();
        let wt = session.path().join("worktree");
        let made = Git
            .add_worktree(tmp.path(), &wt, "bright-fox-123", "repo")
            .unwrap();
        let host_gitfile = session.path().join("host-gitfile");
        let guest_author = ResolvedGitAuthor {
            name: Some("Guest".into()),
            email: Some("guest@example.com".into()),
        };
        Git.isolate_guest_metadata(
            tmp.path(),
            &wt,
            &made.branch,
            &made.commit,
            &host_gitfile,
            &guest_author,
        )
        .unwrap();
        let repo = RepoState {
            name: "repo".into(),
            source: tmp.path().into(),
            worktree: wt.clone(),
            mount: "/workspace/repo".into(),
            branch: made.branch,
            base_commit: made.commit,
            host_gitfile,
        };

        std::fs::write(wt.join("a"), "first accepted snapshot").unwrap();
        git(&wt, &["add", "a"]);
        git(&wt, &["commit", "-m", "first guest change"]);
        Git.import_guest_commits(&repo).unwrap();
        Git.accept_snapshot(&repo, &target).unwrap();
        assert!(wt.join(".git").is_dir());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a")).unwrap(),
            "first accepted snapshot"
        );

        std::fs::write(wt.join("a"), "second accepted snapshot").unwrap();
        git(&wt, &["add", "a"]);
        git(&wt, &["commit", "-m", "second guest change"]);
        Git.import_guest_commits(&repo).unwrap();
        Git.accept_snapshot(&repo, &target).unwrap();
        assert!(wt.join(".git").is_dir());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a")).unwrap(),
            "second accepted snapshot"
        );

        Git.restore_and_import_guest_metadata(&repo).unwrap();
        assert!(Git.remove_worktree(tmp.path(), &wt, false).is_ok());
    }
}
