use crate::{
    config::ResolvedGitAuthor,
    state::{BeadsBaselineFile, RepoState},
};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

pub struct Git;
const BEADS_EXPORT_LIMIT: u64 = 8 * 1024 * 1024;
pub struct WorktreeCreated {
    pub branch: String,
    pub commit: String,
}

/// Whether a retained worktree needs preservation. `Accepted` means its
/// committed snapshot is already reachable from a different local host branch,
/// so deleting the worktree does not discard those commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeChangeState {
    Clean,
    Accepted,
    Changes,
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

    /// Snapshot the source repository's current Beads issue export into the
    /// generated worktree. `.beads/issues.jsonl` is the portable issue
    /// representation, unlike the live Dolt database and its locks, sockets,
    /// credentials, and process state. Jbox intentionally copies this one
    /// project file rather than mounting or cloning the host Beads database.
    ///
    /// This is a deliberate narrow exception to the normal no-uncommitted-host
    /// files rule: task context needs to follow an agent into its disposable
    /// workspace. The copy is session-local and is hydrated into a guest-local
    /// database during guest startup.
    pub fn snapshot_beads_export(&self, source: &Path, worktree: &Path) -> Result<Option<String>> {
        let source = source.canonicalize().with_context(|| {
            format!(
                "cannot resolve Beads source repository {}",
                source.display()
            )
        })?;
        let export = source.join(".beads/issues.jsonl");
        if !export.exists() {
            return Ok(None);
        }
        let export = export
            .canonicalize()
            .with_context(|| format!("cannot resolve Beads export {}", export.display()))?;
        if !export.starts_with(&source) {
            bail!(
                "refusing Beads export outside source repository {}",
                source.display()
            );
        }
        let export_metadata = fs::metadata(&export)?;
        if !export_metadata.is_file() {
            bail!("Beads export {} is not a regular file", export.display());
        }
        if export_metadata.len() > BEADS_EXPORT_LIMIT {
            bail!(
                "Beads export {} exceeds the {} MiB snapshot limit",
                export.display(),
                BEADS_EXPORT_LIMIT / (1024 * 1024)
            );
        }
        let content = fs::read(&export)?;
        if content.len() as u64 > BEADS_EXPORT_LIMIT {
            bail!(
                "Beads export {} exceeds the {} MiB snapshot limit",
                export.display(),
                BEADS_EXPORT_LIMIT / (1024 * 1024)
            );
        }

        let beads_dir = worktree.join(".beads");
        if let Ok(metadata) = fs::symlink_metadata(&beads_dir)
            && metadata.file_type().is_symlink()
        {
            bail!(
                "refusing to write Beads state through symlink {}",
                beads_dir.display()
            );
        }
        fs::create_dir_all(&beads_dir)?;
        fs::set_permissions(&beads_dir, fs::Permissions::from_mode(0o700))?;

        let destination = beads_dir.join("issues.jsonl");
        if let Ok(metadata) = fs::symlink_metadata(&destination)
            && metadata.file_type().is_symlink()
        {
            bail!(
                "refusing to write Beads export through symlink {}",
                destination.display()
            );
        }
        let temporary = beads_dir.join(".issues.jsonl.jbox-importing");
        let mut temporary_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("cannot stage Beads export at {}", temporary.display()))?;
        temporary_file.write_all(&content)?;
        temporary_file.sync_all()?;
        drop(temporary_file);
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, &destination).with_context(|| {
            format!(
                "cannot atomically snapshot Beads export from {} to {}",
                export.display(),
                destination.display()
            )
        })?;
        Ok(Some(format!("{:x}", Sha256::digest(&content))))
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

    /// Explicitly checkpoint agent work in a generated worktree before it is
    /// accepted. This never touches the original host checkout. An unresolved
    /// merge/rebase cannot be safely committed and receives actionable output
    /// instead of a lossy force operation.
    /// Return whether a generated worktree has checkpointable agent changes.
    /// This is deliberately separate from `checkpoint_session_changes` so a
    /// multi-repository accept can validate every worktree before committing
    /// any one of them.
    pub fn validate_session_checkpoint(&self, repo: &RepoState) -> Result<bool> {
        let conflicts = Self::run(&repo.worktree, &["diff", "--name-only", "--diff-filter=U"])?;
        if !conflicts.is_empty() {
            bail!(
                "cannot checkpoint {}: generated worktree {} has unresolved conflicts:\n{}\nResolve them there, then run `git add <paths>` and complete or abort the Git operation before accepting or resuming. From the host repository, `jbox resolve` opens a local shell in the retained worktree.",
                repo.name,
                repo.worktree.display(),
                conflicts
            );
        }
        let status = self.status(repo)?;
        if status.lines().all(|line| line.starts_with("##")) {
            return Ok(false);
        }
        let branch = Self::run(&repo.worktree, &["branch", "--show-current"])?;
        if branch.is_empty() {
            bail!(
                "cannot checkpoint {}: generated worktree {} is detached, usually because a rebase or merge is in progress; resolve it and run `git rebase --continue` or `git merge --continue`",
                repo.name,
                repo.worktree.display()
            );
        }
        if branch != repo.branch {
            bail!(
                "cannot checkpoint {}: generated worktree is on `{branch}`, expected session branch `{}`",
                repo.name,
                repo.branch
            );
        }
        Ok(true)
    }

    pub fn checkpoint_session_changes(&self, repo: &RepoState, session_id: &str) -> Result<bool> {
        self.checkpoint_session_changes_for(repo, session_id, "accept")
    }

    pub fn checkpoint_session_changes_for_resume(
        &self,
        repo: &RepoState,
        session_id: &str,
    ) -> Result<bool> {
        self.checkpoint_session_changes_for(repo, session_id, "resume")
    }

    fn checkpoint_session_changes_for(
        &self,
        repo: &RepoState,
        session_id: &str,
        operation: &str,
    ) -> Result<bool> {
        if !self.validate_session_checkpoint(repo)? {
            return Ok(false);
        }
        Self::run(&repo.worktree, &["add", "--all"])?;
        // Keep jbox-created Beads bootstrap files out of an agent checkpoint.
        for path in self.ignored_jbox_beads_paths(repo) {
            let _ = Self::run(&repo.worktree, &["reset", "--", &path]);
        }
        Self::run(
            &repo.worktree,
            &[
                "commit",
                "-m",
                &format!("jbox: checkpoint {session_id} before {operation}"),
            ],
        )?;
        Ok(true)
    }

    /// Move uncommitted host files out of the way for a fast-forward accept.
    /// The returned stash object ID remains recoverable if restoration later
    /// conflicts, rather than silently discarding host work.
    pub fn stash_host_changes(&self, repo: &RepoState, session_id: &str) -> Result<Option<String>> {
        if Self::run(&repo.source, &["status", "--porcelain"])?.is_empty() {
            return Ok(None);
        }
        Self::run(
            &repo.source,
            &[
                "stash",
                "push",
                "--include-untracked",
                "-m",
                &format!("jbox {session_id} before accept"),
            ],
        )?;
        Ok(Some(Self::run(
            &repo.source,
            &["rev-parse", "--verify", "refs/stash"],
        )?))
    }

    pub fn restore_host_stash(&self, repo: &RepoState, stash: &str) -> Result<()> {
        Self::run(&repo.source, &["stash", "apply", "--index", stash]).with_context(|| {
            format!(
                "accepted {} but could not reapply preserved host work; stash {stash} remains available in {}",
                repo.name,
                repo.source.display()
            )
        })?;
        // `git stash drop` accepts a reflog name, not a raw object ID. Find
        // the still-present entry by the object ID captured before acceptance,
        // so several repositories may safely hold their own temporary stash.
        let position = Self::run(&repo.source, &["stash", "list", "--format=%H"])?
            .lines()
            .position(|candidate| candidate == stash)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "reapplied preserved host work for {} but could not find temporary stash {stash} to remove; inspect `git stash list` in {}",
                    repo.name,
                    repo.source.display()
                )
            })?;
        let reference = format!("stash@{{{position}}}");
        Self::run(&repo.source, &["stash", "drop", &reference])?;
        Ok(())
    }

    /// Import commits made using the guest metadata, then restore the normal
    /// linked-worktree git file so the branch remains a standard host branch.
    pub fn restore_and_import_guest_metadata(&self, repo: &RepoState) -> Result<()> {
        self.import_guest_commits(repo)?;
        self.restore_guest_metadata(repo)
    }

    /// Recreate the isolated Git directory for a retained, stopped worktree.
    /// Refuse dirty files rather than allowing the required hard reset to lose
    /// uncommitted work. Committed changes already live on the session branch.
    pub fn prepare_retained_worktree_for_guest(
        &self,
        repo: &RepoState,
        author: &ResolvedGitAuthor,
    ) -> Result<()> {
        if self.has_uncommitted_changes(repo)? {
            bail!(
                "cannot resume {}: retained worktree {} has uncommitted changes; commit or stash them before resuming",
                repo.name,
                repo.worktree.display()
            );
        }
        let commit = Self::run(&repo.source, &["rev-parse", "--verify", &repo.branch])?;
        self.isolate_guest_metadata(
            &repo.source,
            &repo.worktree,
            &repo.branch,
            &commit,
            &repo.host_gitfile,
            author,
        )
    }

    /// Verify that the host checkout is clean and currently on `target`, then
    /// fast-forward it to the imported guest session branch. The caller imports
    /// all session branches before calling this method, avoiding a live guest
    /// metadata mutation during the merge itself.
    pub fn preflight_accept_snapshot(&self, repo: &RepoState, target: &str) -> Result<()> {
        self.preflight_accept_target(repo, target)?;
        if !self.host_is_clean(repo)? {
            bail!(
                "cannot accept {}: host repository {} has uncommitted changes; use `jbox accept --stash-host` to preserve them around a fast-forward",
                repo.name,
                repo.source.display()
            );
        }
        if !self.snapshot_can_fast_forward(repo, target)? {
            bail!(
                "cannot accept {}: `{target}` has diverged from guest branch `{}`; use `jbox accept --merge` to create an explicit merge, or rebase the retained worktree",
                repo.name,
                repo.branch
            );
        }
        Ok(())
    }

    /// Test whether an imported guest branch can advance `target` without a
    /// merge commit. The caller performs target and cleanliness checks suited
    /// to its operation before acting on this relationship.
    pub fn snapshot_can_fast_forward(&self, repo: &RepoState, target: &str) -> Result<bool> {
        let target = Self::run(&repo.source, &["check-ref-format", "--branch", target])?;
        Ok(Command::new("git")
            .arg("-C")
            .arg(&repo.source)
            .args(["merge-base", "--is-ancestor", &target, &repo.branch])
            .status()
            .context("could not verify whether the guest snapshot can fast-forward the host")?
            .success())
    }

    pub fn host_is_clean(&self, repo: &RepoState) -> Result<bool> {
        Ok(Self::run(&repo.source, &["status", "--porcelain"])?.is_empty())
    }

    /// Host branch and cleanliness checks shared by fast-forward and explicit
    /// merge acceptance. This intentionally does not require an ancestry
    /// relationship, because `--merge` is the explicit opt-in for divergence.
    pub fn preflight_merge_snapshot(&self, repo: &RepoState, target: &str) -> Result<()> {
        self.preflight_accept_target(repo, target)?;
        if !Self::run(&repo.source, &["status", "--porcelain"])?.is_empty() {
            bail!(
                "cannot merge accept {}: host repository {} has uncommitted changes; use `jbox accept --stash-host --merge` to preserve them around the merge",
                repo.name,
                repo.source.display()
            );
        }
        Ok(())
    }

    /// Validate the selected host branch without requiring a clean checkout.
    /// `accept --stash-host` uses this before creating a temporary stash.
    pub fn preflight_accept_target(&self, repo: &RepoState, target: &str) -> Result<()> {
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
        let target_ref = format!("refs/heads/{target}");
        Self::run(&repo.source, &["rev-parse", "--verify", &target_ref])?;
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

    /// Merge a guest session branch into the host target only when the caller
    /// explicitly opted in to non-fast-forward acceptance.
    pub fn merge_snapshot(&self, repo: &RepoState, target: &str) -> Result<()> {
        let target = Self::run(&repo.source, &["check-ref-format", "--branch", target])?;
        let current = Self::run(&repo.source, &["branch", "--show-current"])?;
        if current != target {
            bail!(
                "cannot merge accept {}: host branch changed from `{target}` before it could be merged",
                repo.name
            );
        }
        match Self::run(&repo.source, &["merge", "--no-edit", &repo.branch]) {
            Ok(_) => Ok(()),
            Err(error) => {
                let conflicts =
                    Self::run(&repo.source, &["diff", "--name-only", "--diff-filter=U"])
                        .unwrap_or_default();
                if conflicts.is_empty() {
                    return Err(error).with_context(|| {
                        format!(
                            "merge acceptance for {} failed in {} while merging session branch {}",
                            repo.name,
                            repo.source.display(),
                            repo.branch
                        )
                    });
                }
                bail!(
                    "acceptance is paused: merging session {} into {} created host conflicts in {}:\n{}\nThe jbox session branch is retained. Resolve and stage the files, then run `jbox accept --continue`. To abandon only this host-side merge and keep the jbox session unchanged, run `jbox accept --abort`",
                    repo.branch,
                    target,
                    repo.source.display(),
                    conflicts,
                );
            }
        }
    }

    /// `Some(paths)` indicates an in-progress host merge. An empty string
    /// means the merge has no remaining unmerged paths and is ready to commit.
    pub fn host_merge_conflicts(&self, repo: &RepoState) -> Result<Option<String>> {
        if self.host_merge_head(&repo.source)?.is_none() {
            return Ok(None);
        }
        Ok(Some(Self::run(
            &repo.source,
            &["diff", "--name-only", "--diff-filter=U"],
        )?))
    }

    /// Return the exact guest commit Git is currently trying to merge into a
    /// host checkout. It lets the accept UI select the originating jbox
    /// session instead of presenting unrelated retained worktrees.
    pub fn host_merge_head(&self, repository: &Path) -> Result<Option<String>> {
        let merge_head = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(["rev-parse", "--verify", "-q", "MERGE_HEAD"])
            .output()
            .context("could not inspect host merge state")?;
        if !merge_head.status.success() {
            return Ok(None);
        }
        Ok(Some(
            String::from_utf8(merge_head.stdout)?.trim().to_owned(),
        ))
    }

    /// Whether the retained session branch contains the commit which Git is
    /// currently merging. A guest may have made newer commits after a paused
    /// host merge, hence ancestry is more robust than exact tip equality.
    pub fn branch_contains_commit(&self, repo: &RepoState, commit: &str) -> Result<bool> {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo.source)
            .args(["merge-base", "--is-ancestor", commit, &repo.branch])
            .status()
            .context("could not match a paused host merge to its jbox session")?;
        Ok(status.success())
    }

    /// Complete an in-progress host merge after the user resolved and staged
    /// its conflicts. This intentionally does not stage files on the user's
    /// behalf: `jbox accept --continue` is a clear confirmation that the
    /// caller reviewed their resolution.
    pub fn continue_host_merge(&self, repo: &RepoState) -> Result<()> {
        let Some(conflicts) = self.host_merge_conflicts(repo)? else {
            bail!(
                "cannot continue acceptance for {}: no host merge is in progress in {}",
                repo.name,
                repo.source.display()
            );
        };
        if !conflicts.is_empty() {
            bail!(
                "acceptance remains paused for {}. Resolve and stage these host files in {} before `jbox accept --continue`:\n{}\nUse `jbox accept --abort` to abandon only the host merge",
                repo.name,
                repo.source.display(),
                conflicts
            );
        }
        Self::run(&repo.source, &["commit", "--no-edit"]).with_context(|| {
            format!(
                "could not complete host merge for {}; stage the reviewed resolution in {} before retrying `jbox accept --continue`",
                repo.name,
                repo.source.display()
            )
        })?;
        Ok(())
    }

    /// Abandon an in-progress host merge without changing the retained jbox
    /// session branch or worktree.
    pub fn abort_host_merge(&self, repo: &RepoState) -> Result<()> {
        if self.host_merge_conflicts(repo)?.is_none() {
            bail!(
                "cannot abort acceptance for {}: no host merge is in progress in {}",
                repo.name,
                repo.source.display()
            );
        }
        Self::run(&repo.source, &["merge", "--abort"])?;
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
    pub fn status(&self, repo: &RepoState) -> Result<String> {
        let status = Self::run(&repo.worktree, &["status", "--short", "--branch"])?;
        Ok(self.without_jbox_beads_snapshot(repo, &status))
    }
    pub fn current_branch(&self, repository: &Path) -> Result<String> {
        Self::run(repository, &["branch", "--show-current"])
    }
    pub fn rebase_worktree(&self, repo: &RepoState, onto: &str) -> Result<()> {
        self.preflight_rebase_worktree(repo, onto)?;
        Self::run(&repo.worktree, &["rebase", onto]).with_context(|| {
            format!(
                "rebase stopped for {}; resolve conflicts in {}, then run `git rebase --continue` there",
                repo.name,
                repo.worktree.display()
            )
        })?;
        Ok(())
    }

    /// Validate a worktree can enter a rebase without mutating it. Session-wide
    /// callers run this for every repository before stopping a shared guest.
    pub fn preflight_rebase_worktree(&self, repo: &RepoState, onto: &str) -> Result<()> {
        if self.has_uncommitted_changes(repo)? {
            bail!(
                "cannot rebase {}: worktree {} has uncommitted changes; commit or stash them first",
                repo.name,
                repo.worktree.display()
            );
        }
        let onto = Self::run(&repo.source, &["check-ref-format", "--branch", onto])?;
        Self::run(
            &repo.source,
            &["rev-parse", "--verify", &format!("refs/heads/{onto}")],
        )?;
        Ok(())
    }
    pub fn diff_stat(&self, repo: &RepoState) -> Result<String> {
        let uncommitted = self.filtered_diff_stat(repo, false)?;
        let staged = self.filtered_diff_stat(repo, true)?;
        let range = format!("{}..HEAD", repo.base_commit);
        let commits = Self::run(&repo.worktree, &["log", "--oneline", &range])?;
        Ok(format!(
            "Uncommitted:\n{uncommitted}\nStaged:\n{staged}\nUnique commits:\n{commits}\n"
        ))
    }
    pub fn change_state(&self, repo: &RepoState) -> Result<WorktreeChangeState> {
        if self.has_uncommitted_changes(repo)? {
            return Ok(WorktreeChangeState::Changes);
        }
        let range = format!("{}..HEAD", repo.base_commit);
        if Self::run(&repo.worktree, &["rev-list", "--count", &range])?.eq("0") {
            return Ok(WorktreeChangeState::Clean);
        }
        let session_ref = format!("refs/heads/{}", repo.branch);
        let containing = Self::run(
            &repo.source,
            &[
                "for-each-ref",
                "--format=%(refname:short)",
                "--contains",
                &session_ref,
                "refs/heads",
            ],
        )?;
        if containing.lines().any(|branch| branch != repo.branch) {
            Ok(WorktreeChangeState::Accepted)
        } else {
            Ok(WorktreeChangeState::Changes)
        }
    }

    pub fn has_changes_or_unique_commits(&self, repo: &RepoState) -> Result<bool> {
        Ok(self.change_state(repo)? != WorktreeChangeState::Clean)
    }

    pub fn has_uncommitted_or_unmerged_changes(&self, repo: &RepoState) -> Result<bool> {
        Ok(self.change_state(repo)? == WorktreeChangeState::Changes)
    }

    /// Capture only the small, known set of project files that `bd init`
    /// rewrites. This runs on the host after the guest has become ready and
    /// before jbox returns control to a client or agent.
    pub fn capture_beads_bootstrap(&self, worktree: &Path) -> Vec<BeadsBaselineFile> {
        [
            ".beads/.gitignore",
            ".beads/config.yaml",
            ".beads/metadata.json",
            ".beads/issues.jsonl",
            ".gitignore",
        ]
        .into_iter()
        .filter_map(|path| {
            let file = worktree.join(path);
            let metadata = fs::symlink_metadata(&file).ok()?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return None;
            }
            let bytes = fs::read(file).ok()?;
            Some(BeadsBaselineFile {
                path: path.into(),
                digest: format!("{:x}", Sha256::digest(bytes)),
            })
        })
        .collect()
    }

    /// An unchanged snapshot is jbox-created task context. It must not block
    /// cleanup or resume, while every agent edit remains ordinary user work.
    fn has_uncommitted_changes(&self, repo: &RepoState) -> Result<bool> {
        let status = Self::run(&repo.worktree, &["status", "--porcelain"])?;
        Ok(!self
            .without_jbox_beads_snapshot(repo, &status)
            .trim()
            .is_empty())
    }

    fn without_jbox_beads_snapshot(&self, repo: &RepoState, status: &str) -> String {
        let snapshot_is_unchanged = self.is_unchanged_beads_snapshot(repo);
        if !snapshot_is_unchanged && repo.beads_bootstrap.is_empty() {
            return status.into();
        }
        status
            .lines()
            .filter(|line| {
                !((snapshot_is_unchanged && line.ends_with(".beads/issues.jsonl"))
                    || self.is_unchanged_bootstrap_line(repo, line)
                    || self.is_transient_beads_gate_lock(repo, line))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn is_unchanged_beads_snapshot(&self, repo: &RepoState) -> bool {
        let Some(expected) = &repo.beads_snapshot else {
            return false;
        };
        let Ok(content) = fs::read(repo.worktree.join(".beads/issues.jsonl")) else {
            return false;
        };
        if format!("{:x}", Sha256::digest(&content)) != *expected {
            return false;
        }
        // Staging a version is an explicit user action, even if the current
        // worktree content happens to equal the original snapshot again.
        Self::run(
            &repo.worktree,
            &["diff", "--cached", "--quiet", "--", ".beads/issues.jsonl"],
        )
        .is_ok()
    }

    fn is_unchanged_bootstrap_line(&self, repo: &RepoState, line: &str) -> bool {
        let Some(path) = repo
            .beads_bootstrap
            .iter()
            .find(|baseline| line.ends_with(&baseline.path))
        else {
            return false;
        };
        self.is_unchanged_bootstrap_file(repo, path)
    }

    /// `bd` leaves this zero-length coordination lock at the workspace root.
    /// It is runtime state, not an agent edit, and must not prevent a retained
    /// jbox worktree from being resumed after its guest stops.
    fn is_transient_beads_gate_lock(&self, repo: &RepoState, line: &str) -> bool {
        (repo.beads_snapshot.is_some() || !repo.beads_bootstrap.is_empty())
            && line.ends_with(".beads.gate.lock")
    }

    fn is_unchanged_bootstrap_file(&self, repo: &RepoState, path: &BeadsBaselineFile) -> bool {
        let Ok(content) = fs::read(repo.worktree.join(&path.path)) else {
            return false;
        };
        format!("{:x}", Sha256::digest(content)) == path.digest
            && Self::run(
                &repo.worktree,
                &["diff", "--cached", "--quiet", "--", &path.path],
            )
            .is_ok()
    }

    fn ignored_jbox_beads_paths(&self, repo: &RepoState) -> Vec<String> {
        let mut paths = Vec::new();
        if self.is_unchanged_beads_snapshot(repo) {
            paths.push(".beads/issues.jsonl".to_owned());
        }
        paths.extend(
            repo.beads_bootstrap
                .iter()
                .filter(|path| self.is_unchanged_bootstrap_file(repo, path))
                .map(|path| path.path.clone()),
        );
        if repo.beads_snapshot.is_some() || !repo.beads_bootstrap.is_empty() {
            paths.push(".beads.gate.lock".to_owned());
        }
        paths.sort();
        paths.dedup();
        paths
    }

    fn filtered_diff_stat(&self, repo: &RepoState, cached: bool) -> Result<String> {
        let mut args = vec!["diff".to_owned(), "--stat".to_owned()];
        if cached {
            args.push("--cached".to_owned());
        }
        args.push("--".to_owned());
        args.push(".".to_owned());
        args.extend(
            self.ignored_jbox_beads_paths(repo)
                .into_iter()
                .map(|path| format!(":(exclude){path}")),
        );
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        Self::run(&repo.worktree, &refs)
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
    fn snapshots_current_beads_export_without_copying_live_database_state() {
        let source = tempdir().unwrap();
        let worktree = tempdir().unwrap();
        fs::create_dir_all(source.path().join(".beads/dolt")).unwrap();
        fs::write(
            source.path().join(".beads/issues.jsonl"),
            "{\"id\":\"task-1\"}\n",
        )
        .unwrap();
        fs::write(source.path().join(".beads/dolt/LOCK"), "live host database").unwrap();
        fs::create_dir_all(worktree.path().join(".beads")).unwrap();
        fs::write(worktree.path().join(".beads/issues.jsonl"), "stale\n").unwrap();

        assert!(
            Git.snapshot_beads_export(source.path(), worktree.path())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            fs::read_to_string(worktree.path().join(".beads/issues.jsonl")).unwrap(),
            "{\"id\":\"task-1\"}\n"
        );
        assert!(!worktree.path().join(".beads/dolt/LOCK").exists());
        assert_eq!(
            fs::metadata(worktree.path().join(".beads/issues.jsonl"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn rejects_beads_export_symlink_escaping_source_repository() {
        let source = tempdir().unwrap();
        let worktree = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir_all(source.path().join(".beads")).unwrap();
        fs::write(outside.path().join("issues.jsonl"), "not project state").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("issues.jsonl"),
            source.path().join(".beads/issues.jsonl"),
        )
        .unwrap();

        assert!(
            Git.snapshot_beads_export(source.path(), worktree.path())
                .unwrap_err()
                .to_string()
                .contains("outside source repository")
        );
    }

    #[test]
    fn unchanged_jbox_beads_snapshot_is_clean_but_an_edit_is_preserved() {
        let source = tempdir().unwrap();
        git(source.path(), &["init"]);
        git(source.path(), &["config", "user.email", "test@example.com"]);
        git(source.path(), &["config", "user.name", "Test"]);
        fs::create_dir_all(source.path().join(".beads")).unwrap();
        fs::write(source.path().join(".beads/issues.jsonl"), "base\n").unwrap();
        git(source.path(), &["add", "."]);
        git(source.path(), &["commit", "-m", "base"]);
        // This models a current host task export that is newer than HEAD.
        fs::write(source.path().join(".beads/issues.jsonl"), "current\n").unwrap();

        let session = tempdir().unwrap();
        let worktree = session.path().join("worktree");
        let made = Git
            .add_worktree(source.path(), &worktree, "bright-fox-123", "repo")
            .unwrap();
        let host_gitfile = source.path().join("host-gitfile");
        let author = ResolvedGitAuthor {
            name: Some("Guest".into()),
            email: Some("guest@example.com".into()),
        };
        Git.isolate_guest_metadata(
            source.path(),
            &worktree,
            &made.branch,
            &made.commit,
            &host_gitfile,
            &author,
        )
        .unwrap();
        let snapshot = Git.snapshot_beads_export(source.path(), &worktree).unwrap();
        assert_eq!(
            fs::read_to_string(worktree.join(".beads/issues.jsonl")).unwrap(),
            "current\n"
        );
        // Model the files `bd init` creates or rewrites before the guest is
        // handed to an agent. They are untracked/modified from Git's point of
        // view, but are Jbox bootstrap state rather than user work.
        fs::write(worktree.join(".beads/.gitignore"), "embeddeddolt/\n").unwrap();
        fs::write(worktree.join(".beads/config.yaml"), "dolt: embedded\n").unwrap();
        fs::write(
            worktree.join(".beads/metadata.json"),
            "{\"dolt_mode\":\"embedded\"}\n",
        )
        .unwrap();
        fs::write(worktree.join(".gitignore"), ".beads/embeddeddolt/\n").unwrap();
        let beads_bootstrap = Git.capture_beads_bootstrap(&worktree);
        assert_eq!(beads_bootstrap.len(), 5);
        fs::write(worktree.join(".beads.gate.lock"), "").unwrap();
        let mut repo = RepoState {
            name: "repo".into(),
            source: source.path().into(),
            worktree: worktree.clone(),
            mount: "/workspace/repo".into(),
            branch: made.branch,
            base_commit: made.commit,
            host_gitfile,
            beads_snapshot: snapshot,
            beads_bootstrap,
        };

        assert_eq!(Git.change_state(&repo).unwrap(), WorktreeChangeState::Clean);
        assert!(Git.status(&repo).unwrap().contains("## jbox/"));
        assert!(!Git.status(&repo).unwrap().contains("issues.jsonl"));
        assert!(!Git.status(&repo).unwrap().contains("config.yaml"));
        assert!(!Git.status(&repo).unwrap().contains("metadata.json"));
        assert!(!Git.status(&repo).unwrap().contains(".beads.gate.lock"));
        assert!(!Git.diff_stat(&repo).unwrap().contains(".beads/"));
        Git.restore_guest_metadata(&repo).unwrap();
        Git.prepare_retained_worktree_for_guest(&repo, &author)
            .unwrap();
        Git.restore_guest_metadata(&repo).unwrap();

        fs::write(worktree.join(".beads/config.yaml"), "agent changed it\n").unwrap();
        git(&worktree, &["add", ".beads/config.yaml"]);
        assert_eq!(
            Git.change_state(&repo).unwrap(),
            WorktreeChangeState::Changes
        );
        assert!(Git.has_uncommitted_or_unmerged_changes(&repo).unwrap());
        // A changed bootstrap file stays visible, just like a changed task
        // export, so clean/resume cannot discard agent work.
        assert!(Git.status(&repo).unwrap().contains("config.yaml"));
        assert!(Git.diff_stat(&repo).unwrap().contains("config.yaml"));
        repo.beads_bootstrap.clear();
        fs::write(worktree.join(".beads/issues.jsonl"), "agent edit\n").unwrap();
        Git.remove_worktree(source.path(), &worktree, true).unwrap();
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
            beads_snapshot: None,
            beads_bootstrap: Vec::new(),
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
    fn prepares_clean_retained_worktree_for_a_second_guest_lifetime() {
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
        let author = ResolvedGitAuthor {
            name: Some("Guest".into()),
            email: Some("guest@example.com".into()),
        };
        Git.isolate_guest_metadata(
            tmp.path(),
            &wt,
            &made.branch,
            &made.commit,
            &host_gitfile,
            &author,
        )
        .unwrap();
        std::fs::write(wt.join("a"), "guest change").unwrap();
        git(&wt, &["add", "a"]);
        git(&wt, &["commit", "-m", "guest"]);
        let repo = RepoState {
            name: "repo".into(),
            source: tmp.path().into(),
            worktree: wt.clone(),
            mount: "/workspace/repo".into(),
            branch: made.branch,
            base_commit: made.commit,
            host_gitfile,
            beads_snapshot: None,
            beads_bootstrap: Vec::new(),
        };
        Git.restore_and_import_guest_metadata(&repo).unwrap();
        assert!(wt.join(".git").is_file());
        Git.prepare_retained_worktree_for_guest(&repo, &author)
            .unwrap();
        assert!(wt.join(".git").is_dir());
        assert_eq!(
            std::fs::read_to_string(wt.join("a")).unwrap(),
            "guest change"
        );
        std::fs::write(wt.join("uncommitted"), "do not lose me").unwrap();
        assert!(
            Git.prepare_retained_worktree_for_guest(&repo, &author)
                .unwrap_err()
                .to_string()
                .contains("uncommitted changes")
        );
        Git.restore_and_import_guest_metadata(&repo).unwrap();
        Git.remove_worktree(tmp.path(), &wt, true).unwrap();
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
            beads_snapshot: None,
            beads_bootstrap: Vec::new(),
        };

        std::fs::write(wt.join("a"), "first accepted snapshot").unwrap();
        git(&wt, &["add", "a"]);
        git(&wt, &["commit", "-m", "first guest change"]);
        Git.import_guest_commits(&repo).unwrap();
        assert_eq!(
            Git.change_state(&repo).unwrap(),
            WorktreeChangeState::Changes
        );
        Git.accept_snapshot(&repo, &target).unwrap();
        assert_eq!(
            Git.change_state(&repo).unwrap(),
            WorktreeChangeState::Accepted
        );
        assert!(!Git.has_uncommitted_or_unmerged_changes(&repo).unwrap());
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

    #[test]
    fn checkpoints_generated_changes_and_restores_a_host_stash() {
        let host = tempdir().unwrap();
        git(host.path(), &["init"]);
        git(host.path(), &["config", "user.email", "host@example.com"]);
        git(host.path(), &["config", "user.name", "Host"]);
        std::fs::write(host.path().join("tracked"), "base\n").unwrap();
        git(host.path(), &["add", "."]);
        git(host.path(), &["commit", "-m", "base"]);

        let session = tempdir().unwrap();
        let worktree = session.path().join("worktree");
        let made = Git
            .add_worktree(host.path(), &worktree, "calm-fox-123", "repo")
            .unwrap();
        let repo = RepoState {
            name: "repo".into(),
            source: host.path().into(),
            worktree: worktree.clone(),
            mount: "/workspace/repo".into(),
            branch: made.branch,
            base_commit: made.commit,
            host_gitfile: session.path().join("host-gitfile"),
            beads_snapshot: None,
            beads_bootstrap: Vec::new(),
        };

        std::fs::write(worktree.join("agent-file"), "agent work\n").unwrap();
        assert!(Git.validate_session_checkpoint(&repo).unwrap());
        assert!(
            Git.checkpoint_session_changes(&repo, "calm-fox-123")
                .unwrap()
        );
        assert!(
            Git::run(&worktree, &["log", "-1", "--format=%s"])
                .unwrap()
                .contains("checkpoint calm-fox-123")
        );

        std::fs::write(host.path().join("tracked"), "host edit\n").unwrap();
        std::fs::write(host.path().join("untracked"), "preserve me\n").unwrap();
        let stash = Git
            .stash_host_changes(&repo, "calm-fox-123")
            .unwrap()
            .expect("host work should be stashed");
        assert!(
            Git::run(host.path(), &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
        Git.restore_host_stash(&repo, &stash).unwrap();
        assert_eq!(
            std::fs::read_to_string(host.path().join("tracked")).unwrap(),
            "host edit\n"
        );
        assert_eq!(
            std::fs::read_to_string(host.path().join("untracked")).unwrap(),
            "preserve me\n"
        );
        Git.remove_worktree(host.path(), &worktree, true).unwrap();
    }

    #[test]
    fn merge_acceptance_reports_conflicted_host_files_and_recovery() {
        let host = tempdir().unwrap();
        git(host.path(), &["init"]);
        git(host.path(), &["config", "user.email", "host@example.com"]);
        git(host.path(), &["config", "user.name", "Host"]);
        let target = Git::run(host.path(), &["branch", "--show-current"]).unwrap();
        std::fs::write(host.path().join("conflicted"), "base\n").unwrap();
        git(host.path(), &["add", "."]);
        git(host.path(), &["commit", "-m", "base"]);

        let session = tempdir().unwrap();
        let worktree = session.path().join("worktree");
        let made = Git
            .add_worktree(host.path(), &worktree, "calm-fox-123", "repo")
            .unwrap();
        std::fs::write(worktree.join("conflicted"), "guest\n").unwrap();
        git(&worktree, &["add", "conflicted"]);
        git(&worktree, &["commit", "-m", "guest"]);
        std::fs::write(host.path().join("conflicted"), "host\n").unwrap();
        git(host.path(), &["add", "conflicted"]);
        git(host.path(), &["commit", "-m", "host"]);

        let repo = RepoState {
            name: "repo".into(),
            source: host.path().into(),
            worktree: worktree.clone(),
            mount: "/workspace/repo".into(),
            branch: made.branch,
            base_commit: made.commit,
            host_gitfile: session.path().join("host-gitfile"),
            beads_snapshot: None,
            beads_bootstrap: Vec::new(),
        };
        let error = Git.merge_snapshot(&repo, &target).unwrap_err().to_string();
        assert!(error.contains("acceptance is paused"), "{error}");
        assert!(error.contains("conflicted"), "{error}");
        assert!(error.contains("jbox accept --continue"), "{error}");
        assert!(error.contains("jbox accept --abort"), "{error}");
        Git.abort_host_merge(&repo).unwrap();
        assert!(Git.host_merge_conflicts(&repo).unwrap().is_none());

        // Recreate the same conflict, then use the integrated continuation
        // after the user has deliberately resolved and staged the file.
        assert!(Git.merge_snapshot(&repo, &target).is_err());
        std::fs::write(host.path().join("conflicted"), "resolved\n").unwrap();
        git(host.path(), &["add", "conflicted"]);
        assert_eq!(
            Git.host_merge_conflicts(&repo).unwrap(),
            Some(String::new())
        );
        Git.continue_host_merge(&repo).unwrap();
        assert!(Git.host_merge_conflicts(&repo).unwrap().is_none());
        assert_eq!(
            std::fs::read_to_string(host.path().join("conflicted")).unwrap(),
            "resolved\n"
        );
        Git.remove_worktree(host.path(), &worktree, true).unwrap();
    }
}
