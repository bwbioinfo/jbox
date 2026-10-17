use crate::{
    config::ResolvedGitAuthor,
    state::{BeadsBaselineFile, RepoState},
};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Seek, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Git;
const BEADS_EXPORT_LIMIT: u64 = 8 * 1024 * 1024;
const BEADS_DOLT_SERVER_CONFIG: &str = ".beads/dolt-server-config.yaml";

/// Keep a newly imported pack alive until its snapshot ref is published. A
/// unique marker prevents removing a keep file owned by another operation.
struct GuestPackKeep {
    path: PathBuf,
    marker: String,
}
impl Drop for GuestPackKeep {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path).is_ok_and(|text| text.trim() == self.marker) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// The exact remote branch configured as a checked-out host branch's upstream.
/// Jbox deliberately never guesses `origin` or assumes the upstream branch has
/// the same name as the local branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub branch: String,
    pub remote: String,
    pub remote_ref: String,
}
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

    /// Create a host-native preview worktree from a stopped session branch.
    /// Unlike an overlay, this never modifies the original host checkout.
    pub fn add_preview_worktree(
        &self,
        repo: &RepoState,
        worktree: &Path,
        branch: &str,
        author: &ResolvedGitAuthor,
    ) -> Result<()> {
        if worktree.exists() {
            bail!(
                "refusing to replace existing preview worktree {}",
                worktree.display()
            );
        }
        let current = Self::run(&repo.source, &["rev-parse", "HEAD"])?;
        let session_head = Self::run(&repo.source, &["rev-parse", "--verify", &repo.branch])?;
        if current != session_head {
            bail!(
                "cannot create preview for {}: host {} is at {}, but session branch {} is at {}. Rebase or accept the session before previewing so native tests use matching history",
                repo.name,
                repo.source.display(),
                current,
                repo.branch,
                session_head
            );
        }
        Self::run(
            &repo.source,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                &worktree.to_string_lossy(),
                &repo.branch,
            ],
        )?;
        if let Some(name) = &author.name {
            Self::run(worktree, &["config", "user.name", name])?;
        }
        if let Some(email) = &author.email {
            Self::run(worktree, &["config", "user.email", email])?;
        }
        Ok(())
    }

    /// Remove a disposable preview worktree and its private branch. Callers
    /// use this only for an explicitly discarded preview or after its content
    /// has been adopted by the session branch.
    pub fn remove_preview_worktree(
        &self,
        repo: &RepoState,
        worktree: &Path,
        branch: &str,
    ) -> Result<()> {
        self.remove_worktree(&repo.source, worktree, true)?;
        if Self::run(&repo.source, &["show-ref", "--verify", "--quiet", branch]).is_ok() {
            Self::run(&repo.source, &["branch", "-D", branch])?;
        }
        Ok(())
    }

    pub fn has_uncommitted_changes_at(&self, worktree: &Path) -> Result<bool> {
        Ok(!Self::run(worktree, &["status", "--porcelain"])?.is_empty())
    }

    /// Inspect a configured remote without invoking any network operation.
    pub fn remote_url(&self, repo: &Path, remote: &str) -> Result<String> {
        if remote.is_empty()
            || remote.starts_with('-')
            || !remote
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            bail!("remote name must contain only letters, digits, `_`, `-`, or `.`");
        }
        Self::run(repo, &["remote", "get-url", remote])
    }

    /// A preview needs confirmation before replacement when it has either
    /// uncommitted edits or commits beyond the retained session branch.
    pub fn preview_has_changes(&self, worktree: &Path, session_branch: &str) -> Result<bool> {
        if self.has_uncommitted_changes_at(worktree)? {
            return Ok(true);
        }
        let preview_head = Self::run(worktree, &["rev-parse", "HEAD"])?;
        let session_head = Self::run(worktree, &["rev-parse", "--verify", session_branch])?;
        Ok(preview_head != session_head)
    }

    /// Checkpoint all visible preview edits. The preview is an explicit,
    /// disposable host-native editing surface, so no Jbox bootstrap files are
    /// present to exclude here.
    pub fn checkpoint_preview_changes(&self, worktree: &Path, session_id: &str) -> Result<bool> {
        self.reject_conflicted_source(worktree)?;
        if !self.has_uncommitted_changes_at(worktree)? {
            return Ok(false);
        }
        Self::run(worktree, &["add", "--all"])?;
        if Self::run(worktree, &["diff", "--cached", "--quiet"]).is_ok() {
            return Ok(false);
        }
        Self::run(
            worktree,
            &[
                "commit",
                "-m",
                &format!("jbox: accept preview {session_id}"),
            ],
        )?;
        Ok(true)
    }

    /// Replace the retained session worktree's dirty state with the committed
    /// preview snapshot. The caller must first verify that the retained
    /// worktree has not changed since the preview was created.
    pub fn adopt_preview_commit(&self, repo: &RepoState, preview_branch: &str) -> Result<()> {
        Self::run(&repo.worktree, &["reset", "--hard", "HEAD"])?;
        Self::run(&repo.worktree, &["clean", "-fd"])?;
        Self::run(&repo.worktree, &["reset", "--hard", preview_branch])?;
        Ok(())
    }

    /// Create a session-private Git patch representing the source checkout's
    /// entire visible working tree, without changing its real index or files.
    /// A temporary alternate index lets Git encode nonignored untracked files
    /// in the ordinary binary patch, including symlinks, executable bits, and
    /// deletions. The guest receives the result as unstaged changes, so a
    /// source's staged/unstaged split is intentionally normalized.
    pub fn snapshot_host_changes(&self, source: &Path, destination: &Path) -> Result<bool> {
        self.snapshot_changes(source, destination, &[])
    }

    /// Snapshot real changes from a generated worktree for a temporary host
    /// overlay. Jbox-created Beads bootstrap state is deliberately excluded.
    pub fn snapshot_session_changes(&self, repo: &RepoState, destination: &Path) -> Result<bool> {
        self.snapshot_changes(
            &repo.worktree,
            destination,
            &self.ignored_jbox_beads_paths(repo),
        )
    }

    fn snapshot_changes(
        &self,
        source: &Path,
        destination: &Path,
        excluded_paths: &[String],
    ) -> Result<bool> {
        self.reject_conflicted_source(source)?;
        let parent = destination
            .parent()
            .context("host-change snapshot needs a parent directory")?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        if destination.exists() {
            bail!(
                "refusing to replace existing host-change snapshot {}",
                destination.display()
            );
        }

        let temporary_index = destination.with_extension("index");
        if temporary_index.exists() {
            bail!(
                "refusing to replace existing temporary Git index {}",
                temporary_index.display()
            );
        }
        let snapshot = (|| {
            Self::run_with_index(source, &temporary_index, &["read-tree", "HEAD"])?;
            Self::run_with_index(source, &temporary_index, &["add", "--all"])?;
            if !excluded_paths.is_empty() {
                let mut reset = vec!["reset", "--"];
                reset.extend(excluded_paths.iter().map(String::as_str));
                Self::run_with_index(source, &temporary_index, &reset)?;
            }
            Self::run_with_index(
                source,
                &temporary_index,
                &["diff", "--cached", "--binary", "--full-index", "HEAD"],
            )
        })();
        let _ = fs::remove_file(&temporary_index);
        // Git may write an optional lock or cache-tree sidecar alongside an
        // alternate index. They are session-local and never host state.
        let _ = fs::remove_file(temporary_index.with_extension("index.lock"));
        let patch = snapshot?;
        if patch.is_empty() {
            return Ok(false);
        }

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .with_context(|| {
                format!(
                    "cannot create host-change snapshot {}",
                    destination.display()
                )
            })?;
        file.write_all(&patch)?;
        file.sync_all()?;
        drop(file);
        fs::set_permissions(destination, fs::Permissions::from_mode(0o600))?;
        Ok(true)
    }

    /// Apply and remove a session-private host-change patch after guest Git
    /// metadata isolation has reset the generated worktree to its base commit.
    /// The patch is never applied to the source repository.
    pub fn apply_host_changes(&self, worktree: &Path, snapshot: &Path) -> Result<()> {
        self.apply_patch(worktree, snapshot, false)?;
        fs::remove_file(snapshot).with_context(|| {
            format!(
                "applied host-change snapshot but could not remove private copy {}",
                snapshot.display()
            )
        })?;
        Ok(())
    }

    /// Apply a stored binary patch. Reverse application is safe for a host
    /// overlay because Git refuses to overwrite a path edited after overlay.
    pub fn apply_patch(&self, worktree: &Path, snapshot: &Path, reverse: bool) -> Result<()> {
        let patch = fs::read(snapshot)
            .with_context(|| format!("cannot read host-change snapshot {}", snapshot.display()))?;
        let mut args = vec!["apply", "--binary", "--whitespace=nowarn"];
        if reverse {
            args.push("--reverse");
        }
        args.push("-");
        let mut child = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(args)
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("could not execute git apply for host-change snapshot")?;
        child
            .stdin
            .as_mut()
            .context("could not open git apply input for host-change snapshot")?
            .write_all(&patch)?;
        let output = child
            .wait_with_output()
            .context("could not wait for git apply host-change snapshot")?;
        if !output.status.success() {
            bail!(
                "could not apply host-change snapshot in {}: {}",
                worktree.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    pub fn head(&self, repo: &Path) -> Result<String> {
        Self::run(repo, &["rev-parse", "HEAD"])
    }

    fn reject_conflicted_source(&self, source: &Path) -> Result<()> {
        let conflicts = Self::run(source, &["diff", "--name-only", "--diff-filter=U"])?;
        if !conflicts.is_empty() {
            bail!(
                "cannot include host changes from {} with unresolved conflicts:\n{}\nResolve or abort the Git operation before running `jbox run --include-host-changes`",
                source.display(),
                conflicts
            );
        }
        Ok(())
    }

    fn run_with_index(repo: &Path, index: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .env("GIT_INDEX_FILE", index)
            .args(args)
            .output()
            .context("could not execute git with temporary host-change index")?;
        if !output.status.success() {
            bail!(
                "git {} failed in {} while preparing host-change snapshot: {}",
                args.join(" "),
                repo.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
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
        // Refuse to snapshot an empty export. An uninitialized or wiped Beads
        // database produces a zero-byte file and must never replace a working
        // guest database with an empty one.
        if !content.iter().any(|byte| !byte.is_ascii_whitespace()) {
            bail!(
                "refusing to snapshot empty Beads export {}; the database may be uninitialized",
                export.display()
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
        if destination.is_file() {
            let previous = fs::read(&destination)?;
            let issue_count = |bytes: &[u8]| {
                bytes
                    .split(|byte| *byte == b'\n')
                    .filter(|line| line.iter().any(|byte| !byte.is_ascii_whitespace()))
                    .filter(|line| {
                        serde_json::from_slice::<serde_json::Value>(line).map_or(true, |record| {
                            record.get("_type").and_then(|value| value.as_str()) != Some("memory")
                        })
                    })
                    .count()
            };
            if issue_count(&previous) > issue_count(&content) {
                bail!(
                    "refusing Beads snapshot from {}: {} issues would replace {} issues in {}",
                    export.display(),
                    issue_count(&content),
                    issue_count(&previous),
                    destination.display()
                );
            }
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
        // Some checkout filters turn Git's normal linked-worktree `.git` file
        // into a symlink. Git rejects that representation when removing the
        // worktree, so normalize Jbox's generated link first.
        let gitfile = worktree.join(".git");
        if fs::symlink_metadata(&gitfile).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            let link = Self::linked_worktree_git_link(&gitfile)?;
            fs::remove_file(&gitfile)?;
            fs::write(&gitfile, link)?;
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

    /// Beads creates this empty root-level coordination lock while a guest is
    /// operating. It is neither source nor task data. Remove it only when it
    /// is exactly the expected ordinary empty file, never when a user has
    /// replaced it with a symlink, directory, or content-bearing file.
    pub fn remove_transient_beads_gate_lock(&self, repo: &RepoState) -> Result<bool> {
        if repo.beads_snapshot.is_none() && repo.beads_bootstrap.is_empty() {
            return Ok(false);
        }
        let path = repo.worktree.join(".beads.gate.lock");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("cannot inspect transient Beads lock {}", path.display())
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != 0 {
            return Ok(false);
        }
        fs::remove_file(&path)
            .with_context(|| format!("cannot remove transient Beads lock {}", path.display()))?;
        Ok(true)
    }
    /// Replace a linked worktree's host-facing `.git` file with a self-contained
    /// metadata copy for the guest. The original link is retained outside the
    /// mount and restored when the session stops. Some checkout filters replace
    /// Git's usual `.git` link file with a symlink to the same linked-worktree
    /// directory, so normalize that representation before saving it. This keeps
    /// the host `.git` directory out of the microVM while allowing normal Git
    /// operations.
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
        let original = if fs::symlink_metadata(&gitfile)
            .with_context(|| format!("cannot inspect linked git path at {}", gitfile.display()))?
            .file_type()
            .is_symlink()
        {
            Self::linked_worktree_git_link(&gitfile)?
        } else {
            fs::read_to_string(&gitfile)
                .with_context(|| format!("cannot read linked git file at {}", gitfile.display()))?
        };
        if !original.starts_with("gitdir: ") {
            bail!(
                "expected {} to be a linked worktree git file or symlink",
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

    /// Convert a linked-worktree `.git` symlink into Git's canonical gitfile
    /// content. The normalized link is safe to persist outside a guest mount.
    fn linked_worktree_git_link(gitfile: &Path) -> Result<String> {
        let gitdir = fs::canonicalize(gitfile).with_context(|| {
            format!("cannot resolve linked git symlink at {}", gitfile.display())
        })?;
        if !gitdir.is_dir() {
            bail!(
                "expected linked git symlink {} to resolve to a directory",
                gitfile.display()
            );
        }
        Ok(format!("gitdir: {}\n", gitdir.display()))
    }

    /// Import the guest's latest committed snapshot while leaving its isolated
    /// Git directory in place. This is safe to use for a running session: any
    /// later commits remain on the same guest branch and can be imported again.
    pub fn import_guest_commits(&self, repo: &RepoState) -> Result<()> {
        let gitfile = repo.worktree.join(".git");
        if !gitfile.is_dir() {
            return Ok(());
        }
        let session_ref = format!("refs/heads/{}", repo.branch);
        let previous = Self::run(
            &repo.source,
            &[
                "rev-parse",
                "--verify",
                &format!("{session_ref}^{{commit}}"),
            ],
        )?;
        let snapshot = Self::run(
            &repo.worktree,
            &[
                "rev-parse",
                "--verify",
                &format!("{session_ref}^{{commit}}"),
            ],
        )?;
        // Pin the snapshot before importing. FETCH_HEAD is shared host state,
        // and neither a later guest commit nor another fetch should change
        // which commit this operation validates and installs.
        let fetched = Self::run(
            &repo.source,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-recurse-submodules",
                &gitfile.to_string_lossy(),
                &snapshot,
            ],
        )
        .and_then(|_| Self::verify_snapshot_objects(&repo.source, &snapshot));
        let _pack_keep = if let Err(import_error) = fetched {
            // Fetch's negotiation/connectivity checks can walk unrelated
            // refs, such as stale refs/codex/turn-diffs entries. Do not delete
            // those refs or bypass preservation during forced cleanup.
            let keep = Self::import_guest_pack(repo, &snapshot).with_context(|| {
                format!(
                    "{import_error:#}; branch-scoped import also failed for {}. Guest worktree {} was retained",
                    repo.name,
                    repo.worktree.display()
                )
            })?;
            Self::verify_snapshot_objects(&repo.source, &snapshot).with_context(|| {
                format!(
                    "guest snapshot verification failed; worktree {} was retained",
                    repo.worktree.display()
                )
            })?;
            Some(keep)
        } else {
            None
        };
        let is_fast_forward = Command::new("git")
            .arg("-C")
            .arg(&repo.source)
            .args(["merge-base", "--is-ancestor", &previous, &snapshot])
            .output()
            .context("could not verify imported guest history")?;
        if is_fast_forward.status.code() == Some(1) {
            bail!(
                "guest branch `{}` rewrote history; refusing to replace its host snapshot",
                repo.branch
            );
        }
        if !is_fast_forward.status.success() {
            bail!(
                "could not verify imported guest history for {}: {}. Guest worktree {} was retained",
                repo.name,
                String::from_utf8_lossy(&is_fast_forward.stderr).trim(),
                repo.worktree.display()
            );
        }
        Self::run(
            &repo.source,
            &["update-ref", &session_ref, &snapshot, &previous],
        )?;
        Ok(())
    }

    fn verify_snapshot_objects(repository: &Path, snapshot: &str) -> Result<()> {
        // A successful/no-op fetch and commit ancestry alone do not guarantee
        // that every required tree and blob is available on the host.
        Self::run(
            repository,
            &[
                "rev-list",
                "--objects",
                "--quiet",
                "--missing=error",
                snapshot,
                "--",
            ],
        )?;
        Ok(())
    }

    /// Transfer a self-contained pack rooted only at the requested snapshot.
    /// Unlike fetch, this does not enumerate either repository's other refs.
    /// Spool to disk rather than buffering an entire repository in memory or
    /// leaving a producer process blocked if the consumer exits early.
    fn import_guest_pack(repo: &RepoState, snapshot: &str) -> Result<GuestPackKeep> {
        // A session can be large, and the system temporary directory may be
        // RAM-backed. Keep anonymous staging files on the session filesystem.
        let staging = repo.worktree.join(".git");
        let mut revisions =
            tempfile::tempfile_in(&staging).context("cannot stage guest pack revisions")?;
        writeln!(revisions, "{snapshot}")?;
        revisions.rewind()?;
        let mut pack =
            tempfile::tempfile_in(&staging).context("cannot stage guest snapshot pack")?;
        let exported = Command::new("git")
            .arg("-C")
            .arg(&repo.worktree)
            .args(["pack-objects", "--stdout", "--revs"])
            .stdin(Stdio::from(revisions))
            .stdout(Stdio::from(pack.try_clone()?))
            .output()
            .context("cannot export guest snapshot objects")?;
        if !exported.status.success() {
            bail!(
                "cannot export guest snapshot {snapshot}: {}",
                String::from_utf8_lossy(&exported.stderr).trim()
            );
        }
        pack.rewind()?;
        let pack_directory = Self::run(&repo.source, &["rev-parse", "--git-path", "objects/pack"])?;
        let marker = format!("jbox guest snapshot {}", uuid::Uuid::new_v4());
        let imported = Command::new("git")
            .arg("-C")
            .arg(&repo.source)
            .args([
                "index-pack",
                "--stdin",
                "--strict",
                &format!("--keep={marker}"),
            ])
            .stdin(Stdio::from(pack))
            .output()
            .context("cannot import guest snapshot objects")?;
        if !imported.status.success() {
            bail!(
                "cannot import guest snapshot {snapshot}: {}",
                String::from_utf8_lossy(&imported.stderr).trim()
            );
        }
        let output = String::from_utf8(imported.stdout)?;
        let hash = output
            .split_whitespace()
            .last()
            .filter(|hash| {
                matches!(hash.len(), 40 | 64) && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .context("index-pack did not report a valid guest pack hash")?;
        Ok(GuestPackKeep {
            path: repo
                .source
                .join(pack_directory)
                .join(format!("pack-{hash}.keep")),
            marker,
        })
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
        if status
            .lines()
            .any(|line| line == format!("!! {BEADS_DOLT_SERVER_CONFIG}"))
        {
            bail!(
                "cannot checkpoint {}: ignored {} is not recognized as generated Beads runtime state; review it and explicitly stage it with `git add -f -- {}` or move it out of the worktree",
                repo.name,
                BEADS_DOLT_SERVER_CONFIG,
                BEADS_DOLT_SERVER_CONFIG
            );
        }
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
        // Determine provenance before staging: an explicit user-staged file
        // must remain project work, while our own `add` must not invalidate
        // the unchanged bootstrap/runtime exclusions.
        let ignored_paths = self.ignored_jbox_beads_paths(repo);
        Self::run(&repo.worktree, &["add", "--all"])?;
        // Keep jbox-created Beads bootstrap files out of an agent checkpoint.
        for path in ignored_paths {
            Self::run(&repo.worktree, &["reset", "--", &path])?;
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
        let status = Self::run(
            &repo.worktree,
            &["status", "--short", "--branch", "--untracked-files=all"],
        )?;
        self.without_jbox_beads_snapshot(repo, &status)
    }
    pub fn current_branch(&self, repository: &Path) -> Result<String> {
        Self::run(repository, &["branch", "--show-current"])
    }

    /// Resolve and validate the exact upstream for the current host branch.
    /// The clean and operation checks happen here, before `pull --rebase` can
    /// modify a checkout.
    pub fn upstream(&self, repository: &Path) -> Result<Upstream> {
        let branch = self.current_branch(repository)?;
        if branch.is_empty() {
            bail!(
                "cannot synchronize host repository {}: it is detached; check out a branch with a configured upstream",
                repository.display()
            );
        }
        if !Self::run(repository, &["status", "--porcelain"])?.is_empty() {
            bail!(
                "cannot synchronize host repository {}: it has uncommitted changes; commit or stash them before `jbox sync`",
                repository.display()
            );
        }
        if let Some(operation) = self.in_progress_operation(repository)? {
            bail!(
                "cannot synchronize host repository {}: a Git {operation} is in progress; finish or abort it before `jbox sync`",
                repository.display()
            );
        }
        let branch_ref = format!("refs/heads/{branch}");
        let remote = Self::run(
            repository,
            &[
                "for-each-ref",
                "--format=%(upstream:remotename)",
                &branch_ref,
            ],
        )?;
        let remote_ref = Self::run(
            repository,
            &[
                "for-each-ref",
                "--format=%(upstream:remoteref)",
                &branch_ref,
            ],
        )?;
        if remote.is_empty() || remote_ref.is_empty() {
            bail!(
                "cannot synchronize `{branch}` in {}: it has no configured upstream; configure one before `jbox sync`",
                repository.display()
            );
        }
        Ok(Upstream {
            branch,
            remote,
            remote_ref,
        })
    }

    /// Synchronize the current clean host branch with its explicit upstream.
    /// Preserve local merge commits when replaying changes onto the upstream.
    /// A plain rebase flattens a previously resolved merge and can replay its
    /// second parent even when the upstream is already the first parent.
    pub fn rebase_host_onto_upstream(&self, repository: &Path, upstream: &Upstream) -> Result<()> {
        let current = self.current_branch(repository)?;
        if current != upstream.branch {
            bail!(
                "cannot synchronize {}: host branch changed from `{}` to `{current}`",
                repository.display(),
                upstream.branch
            );
        }
        Self::run(
            repository,
            &[
                "pull",
                "--rebase=merges",
                "--no-autostash",
                &upstream.remote,
                &upstream.remote_ref,
            ],
        )
        .with_context(|| {
            format!(
                "host rebase stopped in {}; resolve and stage its conflicts, run `git rebase --continue`, then run `jbox sync --continue`",
                repository.display()
            )
        })?;
        Ok(())
    }

    /// Test that the exact upstream can be fetched before Jbox stops a guest or
    /// rewrites any local branch. `--dry-run` contacts the remote but leaves
    /// local tracking refs and the checked-out branch unchanged.
    pub fn preflight_upstream_fetch(&self, repository: &Path, upstream: &Upstream) -> Result<()> {
        Self::run(
            repository,
            &[
                "fetch",
                "--dry-run",
                "--no-tags",
                &upstream.remote,
                &upstream.remote_ref,
            ],
        )
        .with_context(|| {
            format!(
                "cannot fetch configured upstream {}/{} for {}",
                upstream.remote,
                upstream.remote_ref,
                repository.display()
            )
        })?;
        Ok(())
    }

    /// Push the checked-out host branch to precisely the configured upstream.
    /// The explicit refspec avoids `push.default` sending unrelated branches.
    pub fn push_upstream(&self, repository: &Path, upstream: &Upstream) -> Result<()> {
        let current = self.current_branch(repository)?;
        if current != upstream.branch {
            bail!(
                "cannot push {}: host branch changed from `{}` to `{current}`",
                repository.display(),
                upstream.branch
            );
        }
        Self::run(
            repository,
            &[
                "push",
                "--porcelain",
                &upstream.remote,
                &format!("HEAD:{}", upstream.remote_ref),
            ],
        )
        .with_context(|| {
            format!(
                "could not push {} to {}/{}; accepted commits remain local and `jbox sync --continue` can retry after the remote is reconciled",
                repository.display(),
                upstream.remote,
                upstream.remote_ref
            )
        })?;
        Ok(())
    }

    /// Return the active destructive Git operation, if any. Git's operation
    /// directories work for normal and linked worktrees, unlike assuming a
    /// `.git` directory exists below the checkout root.
    pub fn in_progress_operation(&self, repository: &Path) -> Result<Option<&'static str>> {
        let git_dir = PathBuf::from(Self::run(repository, &["rev-parse", "--absolute-git-dir"])?);
        if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
            return Ok(Some("rebase"));
        }
        if git_dir.join("MERGE_HEAD").exists() {
            return Ok(Some("merge"));
        }
        if git_dir.join("CHERRY_PICK_HEAD").exists() {
            return Ok(Some("cherry-pick"));
        }
        Ok(None)
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

    /// Advance a clean, already-accepted session branch to the host branch
    /// after the host itself was rebased for a failed-push retry. This is safe
    /// only after acceptance: no guest-only commits or files may be discarded.
    pub fn align_accepted_worktree(&self, repo: &RepoState, host_branch: &str) -> Result<()> {
        self.preflight_rebase_worktree(repo, host_branch)?;
        Self::run(&repo.worktree, &["reset", "--hard", host_branch]).with_context(|| {
            format!(
                "could not align accepted session worktree {} with host branch `{host_branch}`",
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
        self.preflight_rebase_target(&repo.source, onto)
    }

    /// Validate a host rebase target without inspecting a generated worktree.
    /// Checkpoint-mode synchronization uses this before it commits visible guest
    /// files, then runs the complete worktree preflight after that checkpoint.
    pub fn preflight_rebase_target(&self, source: &Path, onto: &str) -> Result<()> {
        let onto = Self::run(source, &["check-ref-format", "--branch", onto])?;
        Self::run(
            source,
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

    /// Git refuses to remove a worktree with ignored files even though those
    /// files are deliberately outside its tracked project state. The caller
    /// already checked that no tracked or untracked work would be lost, so use
    /// Git's force flag solely to dispose of ignored caches and guest runtime
    /// artifacts.
    pub fn requires_safe_worktree_force(&self, repo: &RepoState) -> Result<bool> {
        if self.has_uncommitted_changes(repo)? {
            return Ok(false);
        }
        let all_status = Self::run(
            &repo.worktree,
            &[
                "status",
                "--porcelain",
                "--ignored",
                "--untracked-files=all",
            ],
        )?;
        Ok(!all_status.trim().is_empty())
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
        let status = Self::run(
            &repo.worktree,
            &["status", "--porcelain", "--untracked-files=all"],
        )?;
        Ok(!self
            .without_jbox_beads_snapshot(repo, &status)?
            .trim()
            .is_empty())
    }

    fn without_jbox_beads_snapshot(&self, repo: &RepoState, status: &str) -> Result<String> {
        let snapshot_is_unchanged = self.is_unchanged_beads_snapshot(repo);
        if repo.beads_snapshot.is_none() && repo.beads_bootstrap.is_empty() {
            return Ok(status.into());
        }
        let runtime_status = Self::run(
            &repo.worktree,
            &[
                "status",
                "--porcelain",
                "-z",
                "--ignored",
                "--untracked-files=all",
                "--",
                BEADS_DOLT_SERVER_CONFIG,
            ],
        )?;
        let generated_runtime = self.is_generated_beads_dolt_config(repo);
        let mut filtered = status
            .lines()
            .filter(|line| {
                !((snapshot_is_unchanged && line.ends_with(".beads/issues.jsonl"))
                    || self.is_unchanged_bootstrap_line(repo, line)
                    || self.is_transient_beads_gate_lock(repo, line)
                    || (generated_runtime
                        && (*line == format!("?? {BEADS_DOLT_SERVER_CONFIG}")
                            || *line == format!("!! {BEADS_DOLT_SERVER_CONFIG}")))
                    || self.is_safe_beads_runtime_line(repo, line))
            })
            .collect::<Vec<_>>()
            .join("\n");
        // A malformed or symlinked config must not become disposable merely
        // because a Beads version added the runtime name to its gitignore.
        let ignored_marker = format!("!! {BEADS_DOLT_SERVER_CONFIG}");
        let ignored_directory = format!("{ignored_marker}/");
        if !generated_runtime
            && runtime_status
                .split('\0')
                .any(|line| line == ignored_marker || line.starts_with(&ignored_directory))
            && !filtered.lines().any(|line| line == ignored_marker)
        {
            if !filtered.is_empty() {
                filtered.push('\n');
            }
            filtered.push_str(&ignored_marker);
        }
        Ok(filtered)
    }

    /// Recognize only the ordinary, untracked localhost config emitted by
    /// Beads. Unknown versions or edits fail closed instead of hiding work.
    fn is_generated_beads_dolt_config(&self, repo: &RepoState) -> bool {
        if repo.beads_snapshot.is_none() && repo.beads_bootstrap.is_empty() {
            return false;
        }
        let beads = repo.worktree.join(".beads");
        if !fs::symlink_metadata(&beads).is_ok_and(|metadata| metadata.is_dir()) {
            return false;
        }
        let path = repo.worktree.join(BEADS_DOLT_SERVER_CONFIG);
        let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        else {
            return false;
        };
        if !file
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= 4096)
        {
            return false;
        }
        // Check HEAD as well as the index. A tracked file staged for deletion
        // and recreated untracked must not have its deliberate deletion reset.
        for args in [
            vec!["ls-files", "--", BEADS_DOLT_SERVER_CONFIG],
            vec![
                "ls-tree",
                "--name-only",
                "HEAD",
                "--",
                BEADS_DOLT_SERVER_CONFIG,
            ],
        ] {
            if !Self::run(&repo.worktree, &args).is_ok_and(|text| text.is_empty()) {
                return false;
            }
        }
        let mut text = String::new();
        if file.take(4097).read_to_string(&mut text).is_err() || text.len() > 4096 {
            return false;
        }
        let prefix = "log_level: warning\nbehavior:\n    auto_gc_behavior:\n        archive_level: 0\nlistener:\n    host: 127.0.0.1\n    port: ";
        let Some((port_text, remainder)) = text
            .strip_prefix(prefix)
            .and_then(|tail| tail.split_once('\n'))
        else {
            return false;
        };
        let Ok(port) = port_text.parse::<u16>() else {
            return false;
        };
        port != 0
            && port.to_string() == port_text
            && remainder
                == format!(
                    "cfg_dir: {}\nuser_session_vars: []\njwks: []\n",
                    Path::new(&repo.mount)
                        .join(".beads/dolt/.doltcfg")
                        .display()
                )
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
            && self.transient_beads_gate_lock_is_safe(repo)
    }

    fn transient_beads_gate_lock_is_safe(&self, repo: &RepoState) -> bool {
        let path = repo.worktree.join(".beads.gate.lock");
        fs::symlink_metadata(path).is_ok_and(|metadata| {
            !metadata.file_type().is_symlink() && metadata.is_file() && metadata.len() == 0
        })
    }

    /// Beads' embedded-Dolt data and coordination files are guest-local
    /// derivations of the portable issues JSONL. Keep the allowlist exact: an
    /// arbitrary ignored directory must remain visible as work that blocks a
    /// non-force cleanup.
    fn is_safe_beads_runtime_line(&self, repo: &RepoState, line: &str) -> bool {
        if repo.beads_snapshot.is_none() && repo.beads_bootstrap.is_empty() {
            return false;
        }
        let Some(path) = line.strip_prefix("!! ") else {
            return false;
        };
        let (path, directory) = match path {
            ".beads/.local_version"
            | ".beads/dolt.gate.lock"
            | ".beads/embeddeddolt.gate.lock"
            | ".beads/last-touched" => (path, false),
            path if path.starts_with(".beads/backup/") => (".beads/backup", true),
            path if path.starts_with(".beads/dolt/") => (".beads/dolt", true),
            path if path.starts_with(".beads/embeddeddolt/") => (".beads/embeddeddolt", true),
            _ => return false,
        };
        let Ok(metadata) = fs::symlink_metadata(repo.worktree.join(path)) else {
            return false;
        };
        !metadata.file_type().is_symlink()
            && if directory {
                metadata.is_dir()
            } else {
                metadata.is_file()
            }
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
        if self.is_generated_beads_dolt_config(repo) {
            paths.push(BEADS_DOLT_SERVER_CONFIG.to_owned());
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
    fn host_sync_keeps_a_resolved_merge_when_upstream_is_its_parent() {
        let root = tempdir().unwrap();
        let remote = root.path().join("remote.git");
        let host = root.path().join("host");
        git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(
            root.path(),
            &["clone", remote.to_str().unwrap(), host.to_str().unwrap()],
        );
        git(&host, &["config", "user.email", "test@example.com"]);
        git(&host, &["config", "user.name", "Test"]);
        git(&host, &["checkout", "-b", "main"]);
        fs::write(host.join("policy"), "base\n").unwrap();
        git(&host, &["add", "policy"]);
        git(&host, &["commit", "-m", "base"]);
        git(&host, &["push", "-u", "origin", "main"]);

        git(&host, &["branch", "topic"]);
        fs::write(host.join("policy"), "upstream\n").unwrap();
        git(&host, &["commit", "-am", "upstream policy"]);
        git(&host, &["push", "origin", "main"]);
        git(&host, &["checkout", "topic"]);
        fs::write(host.join("policy"), "local\n").unwrap();
        git(&host, &["commit", "-am", "local policy"]);
        git(&host, &["checkout", "main"]);
        assert!(Git::run(&host, &["merge", "--no-ff", "topic"]).is_err());
        fs::write(host.join("policy"), "resolved\n").unwrap();
        git(&host, &["add", "policy"]);
        git(&host, &["commit", "-m", "resolve policy merge"]);
        let original_head = Git::run(&host, &["rev-parse", "HEAD"]).unwrap();

        Git.rebase_host_onto_upstream(
            &host,
            &Upstream {
                branch: "main".into(),
                remote: "origin".into(),
                remote_ref: "refs/heads/main".into(),
            },
        )
        .unwrap();

        assert_eq!(
            Git::run(&host, &["rev-parse", "HEAD"]).unwrap(),
            original_head
        );
        assert_eq!(
            fs::read_to_string(host.join("policy")).unwrap(),
            "resolved\n"
        );
        assert!(Git.in_progress_operation(&host).unwrap().is_none());
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
    fn snapshots_staged_unstaged_deleted_and_untracked_host_changes_without_mutation() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        let guest = root.path().join("guest");
        let scratch = tempdir().unwrap();
        fs::create_dir(&source).unwrap();
        git(&source, &["init"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        fs::write(source.join("tracked.txt"), "base\n").unwrap();
        fs::write(source.join("removed.txt"), "remove me\n").unwrap();
        fs::write(source.join(".gitignore"), "ignored.txt\n").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "base"]);

        fs::write(source.join("tracked.txt"), "staged\n").unwrap();
        git(&source, &["add", "tracked.txt"]);
        fs::write(source.join("tracked.txt"), "working tree\n").unwrap();
        fs::remove_file(source.join("removed.txt")).unwrap();
        fs::write(source.join("untracked.bin"), b"\0guest snapshot\xff").unwrap();
        std::os::unix::fs::symlink("tracked.txt", source.join("untracked-link")).unwrap();
        fs::write(source.join("ignored.txt"), "must not copy\n").unwrap();
        let before =
            Git::run(&source, &["status", "--porcelain", "--untracked-files=all"]).unwrap();

        let snapshot = scratch.path().join("host.patch");
        assert!(Git.snapshot_host_changes(&source, &snapshot).unwrap());
        assert_eq!(
            Git::run(&source, &["status", "--porcelain", "--untracked-files=all"]).unwrap(),
            before,
            "capturing must leave the source index and files untouched"
        );

        let made = Git
            .add_worktree(&source, &guest, "bright-fox-123", "repo")
            .unwrap();
        let host_gitfile = scratch.path().join("host-gitfile");
        let author = ResolvedGitAuthor {
            name: Some("Guest".into()),
            email: Some("guest@example.com".into()),
        };
        Git.isolate_guest_metadata(
            &source,
            &guest,
            &made.branch,
            &made.commit,
            &host_gitfile,
            &author,
        )
        .unwrap();
        Git.apply_host_changes(&guest, &snapshot).unwrap();

        assert_eq!(
            fs::read_to_string(guest.join("tracked.txt")).unwrap(),
            "working tree\n"
        );
        assert!(!guest.join("removed.txt").exists());
        assert_eq!(
            fs::read(guest.join("untracked.bin")).unwrap(),
            b"\0guest snapshot\xff"
        );
        assert_eq!(
            fs::read_link(guest.join("untracked-link")).unwrap(),
            std::path::PathBuf::from("tracked.txt")
        );
        assert!(!guest.join("ignored.txt").exists());
        assert!(!snapshot.exists(), "applied snapshots are removed promptly");
        let guest_status =
            Git::run(&guest, &["status", "--porcelain", "--untracked-files=all"]).unwrap();
        assert!(guest_status.contains("tracked.txt"));
        assert!(guest_status.contains("removed.txt"));
        assert!(guest_status.contains("untracked.bin"));
        assert_eq!(
            Git::run(&source, &["status", "--porcelain", "--untracked-files=all"]).unwrap(),
            before,
            "applying in the generated worktree must not affect the source"
        );

        let repo = RepoState {
            name: "repo".into(),
            source: source.clone(),
            worktree: guest.clone(),
            mount: "/workspace/repo".into(),
            branch: made.branch,
            base_commit: made.commit,
            host_gitfile,
            beads_snapshot: None,
            beads_bootstrap: Vec::new(),
        };
        Git.restore_guest_metadata(&repo).unwrap();
        Git.remove_worktree(&source, &guest, true).unwrap();
    }

    #[test]
    fn rejects_snapshotting_unresolved_host_conflicts() {
        let source = tempdir().unwrap();
        let scratch = tempdir().unwrap();
        git(source.path(), &["init"]);
        git(source.path(), &["config", "user.email", "test@example.com"]);
        git(source.path(), &["config", "user.name", "Test"]);
        fs::write(source.path().join("conflicted"), "base\n").unwrap();
        git(source.path(), &["add", "."]);
        git(source.path(), &["commit", "-m", "base"]);
        git(source.path(), &["checkout", "-b", "other"]);
        fs::write(source.path().join("conflicted"), "other\n").unwrap();
        git(source.path(), &["commit", "-am", "other"]);
        git(source.path(), &["checkout", "-"]);
        fs::write(source.path().join("conflicted"), "main\n").unwrap();
        git(source.path(), &["commit", "-am", "main"]);
        let merge = Command::new("git")
            .arg("-C")
            .arg(source.path())
            .args(["merge", "other"])
            .status()
            .unwrap();
        assert!(!merge.success());

        let error = Git
            .snapshot_host_changes(source.path(), &scratch.path().join("host.patch"))
            .unwrap_err();
        assert!(error.to_string().contains("unresolved conflicts"));
        assert!(!scratch.path().join("host.patch").exists());
        git(source.path(), &["merge", "--abort"]);
    }

    #[test]
    fn reverses_guest_overlay_without_deleting_unrelated_host_test_output() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        let guest = root.path().join("guest");
        let scratch = tempdir().unwrap();
        fs::create_dir(&source).unwrap();
        git(&source, &["init"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        fs::write(source.join("tracked"), "base\n").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "base"]);
        let made = Git
            .add_worktree(&source, &guest, "bright-fox-123", "repo")
            .unwrap();
        fs::write(guest.join("tracked"), "guest edit\n").unwrap();
        fs::write(guest.join("guest-only"), "guest file\n").unwrap();
        let repo = RepoState {
            name: "repo".into(),
            source: source.clone(),
            worktree: guest.clone(),
            mount: "/workspace/repo".into(),
            branch: made.branch,
            base_commit: made.commit,
            host_gitfile: scratch.path().join("host-gitfile"),
            beads_snapshot: None,
            beads_bootstrap: Vec::new(),
        };
        let patch = scratch.path().join("overlay.patch");
        assert!(Git.snapshot_session_changes(&repo, &patch).unwrap());
        Git.apply_patch(&source, &patch, false).unwrap();
        assert_eq!(
            fs::read_to_string(source.join("tracked")).unwrap(),
            "guest edit\n"
        );
        assert_eq!(
            fs::read_to_string(source.join("guest-only")).unwrap(),
            "guest file\n"
        );

        fs::write(source.join("native-test-output"), "keep this\n").unwrap();
        Git.apply_patch(&source, &patch, true).unwrap();
        assert_eq!(
            fs::read_to_string(source.join("tracked")).unwrap(),
            "base\n"
        );
        assert!(!source.join("guest-only").exists());
        assert_eq!(
            fs::read_to_string(source.join("native-test-output")).unwrap(),
            "keep this\n"
        );
        fs::remove_file(source.join("native-test-output")).unwrap();
        Git.remove_worktree(&source, &guest, true).unwrap();
    }

    #[test]
    fn preview_worktree_adopts_native_edits_without_touching_host_checkout() {
        let root = tempdir().unwrap();
        let source = root.path().join("source");
        let guest = root.path().join("guest");
        let preview = root.path().join("preview");
        fs::create_dir(&source).unwrap();
        git(&source, &["init"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        fs::write(source.join("tracked"), "base\n").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "base"]);
        let made = Git
            .add_worktree(&source, &guest, "bright-fox-123", "repo")
            .unwrap();
        let repo = RepoState {
            name: "repo".into(),
            source: source.clone(),
            worktree: guest.clone(),
            mount: "/workspace/repo".into(),
            branch: made.branch,
            base_commit: made.commit,
            host_gitfile: root.path().join("host-gitfile"),
            beads_snapshot: None,
            beads_bootstrap: Vec::new(),
        };
        fs::write(guest.join("tracked"), "guest edit\n").unwrap();
        fs::write(guest.join("guest-only"), "guest file\n").unwrap();
        let seed = root.path().join("preview.seed.patch");
        assert!(Git.snapshot_session_changes(&repo, &seed).unwrap());

        let preview_branch = "jbox/bright-fox-123/preview/repo";
        let preview_author = ResolvedGitAuthor {
            name: Some("Preview".into()),
            email: Some("preview@example.com".into()),
        };
        Git.add_preview_worktree(&repo, &preview, preview_branch, &preview_author)
            .unwrap();
        // A pristine preview is safe to discard and reconnect to the guest
        // automatically when its host shell closes.
        assert!(!Git.preview_has_changes(&preview, &repo.branch).unwrap());
        assert_eq!(
            Git::run(&preview, &["config", "user.name"]).unwrap(),
            "Preview"
        );
        Git.apply_patch(&preview, &seed, false).unwrap();
        assert!(Git.preview_has_changes(&preview, &repo.branch).unwrap());
        assert_eq!(
            fs::read_to_string(preview.join("tracked")).unwrap(),
            "guest edit\n"
        );
        assert_eq!(
            fs::read_to_string(preview.join("guest-only")).unwrap(),
            "guest file\n"
        );
        assert_eq!(
            fs::read_to_string(source.join("tracked")).unwrap(),
            "base\n"
        );

        fs::write(preview.join("native-edit"), "tested on host\n").unwrap();
        assert!(
            Git.checkpoint_preview_changes(&preview, "bright-fox-123")
                .unwrap()
        );
        assert!(Git.preview_has_changes(&preview, &repo.branch).unwrap());
        Git.adopt_preview_commit(&repo, preview_branch).unwrap();
        assert!(!Git.preview_has_changes(&preview, &repo.branch).unwrap());
        assert_eq!(
            fs::read_to_string(guest.join("tracked")).unwrap(),
            "guest edit\n"
        );
        assert_eq!(
            fs::read_to_string(guest.join("native-edit")).unwrap(),
            "tested on host\n"
        );
        assert_eq!(
            fs::read_to_string(source.join("tracked")).unwrap(),
            "base\n"
        );

        // A completed accept fast-forwards the original checkout to the
        // session branch. A later preview must use that current session head,
        // rather than the session's initial base commit.
        git(&source, &["merge", "--ff-only", &repo.branch]);
        assert_eq!(
            fs::read_to_string(source.join("native-edit")).unwrap(),
            "tested on host\n"
        );

        Git.remove_preview_worktree(&repo, &preview, preview_branch)
            .unwrap();
        assert!(!preview.exists());
        assert!(
            Git::run(
                &source,
                &["show-ref", "--verify", "--quiet", preview_branch]
            )
            .is_err()
        );

        let later_preview = root.path().join("later-preview");
        let later_branch = "jbox/bright-fox-123/preview-later/repo";
        Git.add_preview_worktree(&repo, &later_preview, later_branch, &preview_author)
            .unwrap();
        Git.remove_preview_worktree(&repo, &later_preview, later_branch)
            .unwrap();
        Git.remove_worktree(&source, &guest, true).unwrap();
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
    fn refuses_empty_beads_export_without_replacing_guest_snapshot() {
        let source = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join(".beads")).unwrap();
        fs::create_dir_all(worktree.path().join(".beads")).unwrap();
        let source_export = source.path().join(".beads/issues.jsonl");
        let guest_export = worktree.path().join(".beads/issues.jsonl");
        fs::write(&guest_export, "{\"id\":\"classy-1\"}\n").unwrap();
        for empty in ["", " \n\r\t"] {
            fs::write(&source_export, empty).unwrap();
            assert!(
                Git.snapshot_beads_export(source.path(), worktree.path())
                    .is_err()
            );
            assert_eq!(
                fs::read_to_string(&guest_export).unwrap(),
                "{\"id\":\"classy-1\"}\n"
            );
        }
    }

    #[test]
    fn refuses_snapshot_that_drops_historical_issues() {
        let source = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join(".beads")).unwrap();
        fs::create_dir_all(worktree.path().join(".beads")).unwrap();
        let source_export = source.path().join(".beads/issues.jsonl");
        let guest_export = worktree.path().join(".beads/issues.jsonl");
        let history = "{\"id\":\"classy-1\"}\n{\"id\":\"classy-2\"}\n{\"_type\":\"memory\",\"id\":\"memory-1\"}\n";
        let newer = "{\"id\":\"classy-new\"}\n{\"_type\":\"memory\",\"id\":\"m2\"}\n{\"_type\":\"memory\",\"id\":\"m3\"}\n{\"_type\":\"memory\",\"id\":\"m4\"}\n";
        fs::write(&source_export, newer).unwrap();
        fs::write(&guest_export, history).unwrap();
        let error = Git
            .snapshot_beads_export(source.path(), worktree.path())
            .unwrap_err();
        assert!(error.to_string().contains("refusing Beads snapshot"));
        assert_eq!(fs::read_to_string(&guest_export).unwrap(), history);
        assert_eq!(fs::read_to_string(&source_export).unwrap(), newer);
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
        fs::create_dir_all(worktree.join(".beads/embeddeddolt")).unwrap();
        fs::write(worktree.join(".beads/embeddeddolt/runtime"), "derived").unwrap();
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
        assert!(!Git.status(&repo).unwrap().contains("embeddeddolt"));
        assert!(!Git.diff_stat(&repo).unwrap().contains(".beads/"));
        assert!(Git.requires_safe_worktree_force(&repo).unwrap());
        assert!(Git.remove_transient_beads_gate_lock(&repo).unwrap());
        assert!(!worktree.join(".beads.gate.lock").exists());

        // An identically named content-bearing file is not Jbox runtime state.
        // It remains visible and the cleanup helper must leave it alone.
        fs::write(worktree.join(".beads.gate.lock"), "agent data\n").unwrap();
        assert_eq!(
            Git.change_state(&repo).unwrap(),
            WorktreeChangeState::Changes
        );
        assert!(!Git.remove_transient_beads_gate_lock(&repo).unwrap());
        assert!(!Git.requires_safe_worktree_force(&repo).unwrap());
        assert!(worktree.join(".beads.gate.lock").is_file());
        fs::remove_file(worktree.join(".beads.gate.lock")).unwrap();
        assert_eq!(Git.change_state(&repo).unwrap(), WorktreeChangeState::Clean);

        // Ignored cache artifacts are not project work. They stay out of
        // status and do not prevent cleanup, but Git needs --force to remove
        // the otherwise clean worktree.
        fs::write(worktree.join(".git/info/exclude"), "private-cache/\n").unwrap();
        fs::create_dir(worktree.join("private-cache")).unwrap();
        fs::write(worktree.join("private-cache/value"), "user cache").unwrap();
        assert_eq!(Git.change_state(&repo).unwrap(), WorktreeChangeState::Clean);
        assert!(!Git.status(&repo).unwrap().contains("private-cache"));
        assert!(Git.requires_safe_worktree_force(&repo).unwrap());
        fs::remove_dir_all(worktree.join("private-cache")).unwrap();
        assert_eq!(Git.change_state(&repo).unwrap(), WorktreeChangeState::Clean);
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

    fn dolt_runtime_fixture() -> (tempfile::TempDir, RepoState) {
        let root = tempdir().unwrap();
        let host = root.path().join("host");
        git(root.path(), &["init", host.to_str().unwrap()]);
        git(&host, &["config", "user.email", "test@example.com"]);
        git(&host, &["config", "user.name", "Test"]);
        fs::write(host.join("tracked"), "base\n").unwrap();
        git(&host, &["add", "."]);
        git(&host, &["commit", "-m", "base"]);
        let worktree = root.path().join("worktree");
        let made = Git
            .add_worktree(&host, &worktree, "calm-fox-123", "repo")
            .unwrap();
        fs::create_dir(worktree.join(".beads")).unwrap();
        fs::write(worktree.join(".beads/config.yaml"), "dolt: server\n").unwrap();
        let beads_bootstrap = Git.capture_beads_bootstrap(&worktree);
        let repo = RepoState {
            name: "repo".into(),
            source: host,
            worktree,
            mount: "/workspace/repo".into(),
            branch: made.branch,
            base_commit: made.commit,
            host_gitfile: root.path().join("host-gitfile"),
            beads_snapshot: None,
            beads_bootstrap,
        };
        fs::write(
            repo.worktree.join(".beads/dolt-server-config.yaml"),
            dolt_runtime_config(34763),
        )
        .unwrap();
        (root, repo)
    }

    fn dolt_runtime_config(port: u16) -> String {
        format!(
            "log_level: warning\nbehavior:\n    auto_gc_behavior:\n        archive_level: 0\nlistener:\n    host: 127.0.0.1\n    port: {port}\ncfg_dir: /workspace/repo/.beads/dolt/.doltcfg\nuser_session_vars: []\njwks: []\n"
        )
    }

    #[test]
    fn generated_dolt_runtime_config_is_clean_without_an_issue_snapshot() {
        let (_root, repo) = dolt_runtime_fixture();
        assert_eq!(Git.change_state(&repo).unwrap(), WorktreeChangeState::Clean);
        assert!(
            !Git.status(&repo)
                .unwrap()
                .contains("dolt-server-config.yaml")
        );
        assert!(Git.requires_safe_worktree_force(&repo).unwrap());
        let host_branch = Git::run(&repo.source, &["branch", "--show-current"]).unwrap();
        Git.preflight_rebase_worktree(&repo, &host_branch).unwrap();
        assert!(!Git.validate_session_checkpoint(&repo).unwrap());
        assert!(
            !Git.checkpoint_session_changes(&repo, "calm-fox-123")
                .unwrap()
        );
        assert_eq!(
            Git::run(&repo.worktree, &["rev-parse", "HEAD"]).unwrap(),
            repo.base_commit
        );
        assert_eq!(
            fs::read_to_string(repo.worktree.join(".beads/dolt-server-config.yaml")).unwrap(),
            dolt_runtime_config(34763)
        );
    }

    #[test]
    fn checkpoint_keeps_generated_dolt_runtime_and_bootstrap_files_out_of_commit() {
        for resume in [false, true] {
            let (_root, repo) = dolt_runtime_fixture();
            fs::write(repo.worktree.join("agent-file"), "real work\n").unwrap();
            if resume {
                assert!(
                    Git.checkpoint_session_changes_for_resume(&repo, "calm-fox-123")
                        .unwrap()
                );
            } else {
                assert!(
                    Git.checkpoint_session_changes(&repo, "calm-fox-123")
                        .unwrap()
                );
            }
            assert_eq!(
                Git::run(
                    &repo.worktree,
                    &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]
                )
                .unwrap(),
                "agent-file"
            );
            assert!(
                Git::run(&repo.worktree, &["ls-files", ".beads"])
                    .unwrap()
                    .is_empty()
            );
            assert!(!Git.validate_session_checkpoint(&repo).unwrap());
            assert!(
                repo.worktree
                    .join(".beads/dolt-server-config.yaml")
                    .is_file()
            );
        }
    }

    #[test]
    fn staged_and_tracked_dolt_configs_remain_user_work() {
        for tracked in [false, true] {
            let (_root, repo) = dolt_runtime_fixture();
            let path = ".beads/dolt-server-config.yaml";
            git(&repo.worktree, &["add", path]);
            if tracked {
                git(&repo.worktree, &["commit", "-m", "user config"]);
                fs::write(repo.worktree.join(path), dolt_runtime_config(34764)).unwrap();
            }
            assert!(Git.status(&repo).unwrap().contains(path));
            assert!(Git.validate_session_checkpoint(&repo).unwrap());
            assert!(
                Git.checkpoint_session_changes(&repo, "calm-fox-123")
                    .unwrap()
            );
            assert_eq!(Git::run(&repo.worktree, &["ls-files", path]).unwrap(), path);
            assert_eq!(
                Git::run(&repo.worktree, &["show", &format!("HEAD:{path}")]).unwrap(),
                dolt_runtime_config(if tracked { 34764 } else { 34763 }).trim()
            );
        }
    }

    #[test]
    fn unrecognized_dolt_configs_and_paths_remain_visible() {
        let (_root, mut repo) = dolt_runtime_fixture();
        let path = ".beads/dolt-server-config.yaml";
        let generated = dolt_runtime_config(34763);
        for content in [
            "agent data\n".to_owned(),
            generated.replace("127.0.0.1", "0.0.0.0"),
            generated.replace("/workspace/repo", "/workspace/other"),
            generated.replace("34763", "0"),
            generated.replace("34763", "65536"),
            generated.replace("34763", "+34763"),
            generated.replace("archive_level: 0", "archive_level: 1"),
            format!("{generated}agent_option: true\n"),
            format!("{generated}---\nlistener: {{host: example.com}}\n"),
            format!("{generated}listener: {{host: example.com}}\n"),
            "x".repeat(4097),
        ] {
            fs::write(repo.worktree.join(path), &content).unwrap();
            assert!(Git.status(&repo).unwrap().contains(path), "{content}");
            assert!(Git.validate_session_checkpoint(&repo).unwrap());
            assert!(
                !Git.ignored_jbox_beads_paths(&repo)
                    .contains(&path.to_owned())
            );
        }
        fs::write(repo.worktree.join(path), &generated).unwrap();
        repo.beads_bootstrap.clear();
        assert!(Git.status(&repo).unwrap().contains(path));
    }

    #[test]
    fn recreated_dolt_config_does_not_hide_a_staged_deletion() {
        let (_root, repo) = dolt_runtime_fixture();
        let path = BEADS_DOLT_SERVER_CONFIG;
        git(&repo.worktree, &["add", path]);
        git(&repo.worktree, &["commit", "-m", "user config"]);
        git(&repo.worktree, &["rm", "--", path]);
        fs::write(repo.worktree.join(path), dolt_runtime_config(34764)).unwrap();
        assert!(!Git.is_generated_beads_dolt_config(&repo));
        assert!(
            !Git.ignored_jbox_beads_paths(&repo)
                .contains(&path.to_owned())
        );
        assert!(Git.status(&repo).unwrap().contains(path));
        assert_eq!(
            Git::run(&repo.worktree, &["diff", "--cached", "--name-status"]).unwrap(),
            format!("D\t{path}")
        );
    }

    #[test]
    fn symlinked_beads_directory_cannot_prove_dolt_runtime_is_disposable() {
        let (_root, repo) = dolt_runtime_fixture();
        let beads = repo.worktree.join(".beads");
        let outside = repo.source.join("external-beads");
        fs::rename(&beads, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &beads).unwrap();
        assert!(!Git.is_generated_beads_dolt_config(&repo));
        assert!(
            !Git.ignored_jbox_beads_paths(&repo)
                .contains(&BEADS_DOLT_SERVER_CONFIG.to_owned())
        );
        assert_eq!(
            Git.change_state(&repo).unwrap(),
            WorktreeChangeState::Changes
        );
        assert!(!Git.requires_safe_worktree_force(&repo).unwrap());
    }

    #[test]
    fn ignored_dolt_runtime_is_safe_but_symlinked_runtime_is_not() {
        let (_root, repo) = dolt_runtime_fixture();
        let path = ".beads/dolt-server-config.yaml";
        fs::write(
            repo.worktree.join(".beads/.gitignore"),
            "dolt-server-config.yaml\n",
        )
        .unwrap();
        let mut repo = repo;
        repo.beads_bootstrap = Git.capture_beads_bootstrap(&repo.worktree);
        let ignored = Git::run(
            &repo.worktree,
            &[
                "status",
                "--porcelain",
                "--ignored",
                "--untracked-files=all",
            ],
        )
        .unwrap();
        assert!(
            !Git.without_jbox_beads_snapshot(&repo, &ignored)
                .unwrap()
                .contains(path)
        );
        assert_eq!(Git.change_state(&repo).unwrap(), WorktreeChangeState::Clean);
        fs::write(repo.worktree.join(path), "custom config\n").unwrap();
        assert!(Git.status(&repo).unwrap().contains(path));
        assert_eq!(
            Git.change_state(&repo).unwrap(),
            WorktreeChangeState::Changes
        );
        assert!(!Git.requires_safe_worktree_force(&repo).unwrap());
        assert!(
            Git.validate_session_checkpoint(&repo)
                .unwrap_err()
                .to_string()
                .contains("git add -f")
        );
        fs::remove_file(repo.worktree.join(path)).unwrap();
        let outside = repo.source.join("runtime.yaml");
        fs::write(&outside, dolt_runtime_config(34763)).unwrap();
        std::os::unix::fs::symlink(&outside, repo.worktree.join(path)).unwrap();
        let ignored = Git::run(
            &repo.worktree,
            &[
                "status",
                "--porcelain",
                "--ignored",
                "--untracked-files=all",
            ],
        )
        .unwrap();
        assert!(
            Git.without_jbox_beads_snapshot(&repo, &ignored)
                .unwrap()
                .contains(path)
        );
        assert!(Git.status(&repo).unwrap().contains(path));
        assert_eq!(
            Git.change_state(&repo).unwrap(),
            WorktreeChangeState::Changes
        );
        assert!(!Git.requires_safe_worktree_force(&repo).unwrap());
        assert!(
            Git.validate_session_checkpoint(&repo)
                .unwrap_err()
                .to_string()
                .contains("git add -f")
        );
        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            dolt_runtime_config(34763)
        );
        fs::remove_file(repo.worktree.join(".beads/.gitignore")).unwrap();
        assert!(Git.status(&repo).unwrap().contains(path));
    }

    #[test]
    fn ignored_dolt_config_directory_remains_protected() {
        for name in [
            "user-data",
            "user\tdata",
            "ümlaut",
            "user\nfile",
            "quoted\"name",
        ] {
            let (_root, mut repo) = dolt_runtime_fixture();
            let path = BEADS_DOLT_SERVER_CONFIG;
            fs::remove_file(repo.worktree.join(path)).unwrap();
            fs::create_dir(repo.worktree.join(path)).unwrap();
            fs::write(repo.worktree.join(path).join(name), "preserve me\n").unwrap();
            fs::write(
                repo.worktree.join(".beads/.gitignore"),
                "dolt-server-config.yaml/\n",
            )
            .unwrap();
            repo.beads_bootstrap = Git.capture_beads_bootstrap(&repo.worktree);
            assert!(
                Git.status(&repo).unwrap().contains(&format!("!! {path}")),
                "{name:?}"
            );
            assert_eq!(
                Git.change_state(&repo).unwrap(),
                WorktreeChangeState::Changes
            );
            assert!(!Git.requires_safe_worktree_force(&repo).unwrap());
            assert!(
                Git.validate_session_checkpoint(&repo)
                    .unwrap_err()
                    .to_string()
                    .contains("git add -f")
            );
            assert_eq!(
                fs::read_to_string(repo.worktree.join(path).join(name)).unwrap(),
                "preserve me\n"
            );
        }
    }

    #[test]
    fn changed_issue_snapshot_does_not_disable_dolt_runtime_classification() {
        let (_root, mut repo) = dolt_runtime_fixture();
        repo.beads_bootstrap.clear();
        repo.beads_snapshot = Some(format!("{:x}", Sha256::digest(b"initial\n")));
        fs::write(repo.worktree.join(".beads/issues.jsonl"), "agent update\n").unwrap();
        let status = Git.status(&repo).unwrap();
        assert!(status.contains("issues.jsonl"));
        assert!(!status.contains(BEADS_DOLT_SERVER_CONFIG));
    }

    #[test]
    fn dolt_runtime_status_query_failure_is_not_treated_as_clean() {
        let (_root, repo) = dolt_runtime_fixture();
        let git_dir = Git::run(&repo.worktree, &["rev-parse", "--git-common-dir"]).unwrap();
        fs::write(Path::new(&git_dir).join("config"), "[invalid\n").unwrap();
        assert!(Git.without_jbox_beads_snapshot(&repo, "").is_err());
        assert!(repo.worktree.join(BEADS_DOLT_SERVER_CONFIG).is_file());
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

    fn isolated_import_fixture() -> (tempfile::TempDir, RepoState) {
        let root = tempdir().unwrap();
        let host = root.path().join("host");
        git(root.path(), &["init", host.to_str().unwrap()]);
        git(&host, &["config", "user.email", "test@example.com"]);
        git(&host, &["config", "user.name", "Test"]);
        fs::write(host.join("a"), "base\n").unwrap();
        git(&host, &["add", "a"]);
        git(&host, &["commit", "-m", "base"]);
        let worktree = root.path().join("worktree");
        let made = Git
            .add_worktree(&host, &worktree, "bright-fox-123", "repo")
            .unwrap();
        let host_gitfile = root.path().join("host-gitfile");
        Git.isolate_guest_metadata(
            &host,
            &worktree,
            &made.branch,
            &made.commit,
            &host_gitfile,
            &ResolvedGitAuthor {
                name: Some("Guest".into()),
                email: Some("guest@example.com".into()),
            },
        )
        .unwrap();
        fs::write(worktree.join("a"), "guest\n").unwrap();
        git(&worktree, &["commit", "-am", "guest"]);
        let repo = RepoState {
            name: "repo".into(),
            source: host,
            worktree,
            mount: "/workspace/repo".into(),
            branch: made.branch,
            base_commit: made.commit,
            host_gitfile,
            beads_snapshot: None,
            beads_bootstrap: Vec::new(),
        };
        (root, repo)
    }

    fn add_broken_import_ref(repository: &Path, packed: bool, oid_length: usize) -> PathBuf {
        let oid = "1".repeat(oid_length);
        let name = "refs/codex/turn-diffs/test/base";
        if packed {
            let path = repository.join(".git/packed-refs");
            let mut text = fs::read_to_string(&path)
                .unwrap_or_else(|_| "# pack-refs with: peeled\n".into())
                .replace(" sorted", "");
            text.push_str(&format!("{oid} {name}\n"));
            fs::write(&path, text).unwrap();
            path
        } else {
            let path = repository.join(".git").join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("{oid}\n")).unwrap();
            path
        }
    }

    #[test]
    fn imports_guest_snapshot_with_unrelated_broken_loose_or_packed_refs() {
        for packed in [false, true] {
            for (broken_host, broken_guest) in [(true, false), (false, true), (true, true)] {
                let (_root, repo) = isolated_import_fixture();
                let snapshot = Git::run(&repo.worktree, &["rev-parse", "HEAD"]).unwrap();
                let mut retained_refs = Vec::new();
                for (repository, broken) in
                    [(&repo.source, broken_host), (&repo.worktree, broken_guest)]
                {
                    if broken {
                        let path = add_broken_import_ref(repository, packed, snapshot.len());
                        let bytes = fs::read(&path).unwrap();
                        retained_refs.push((path, bytes));
                    }
                }
                Git.import_guest_commits(&repo).unwrap();
                Git.import_guest_commits(&repo).unwrap();
                assert!(repo.worktree.join(".git").is_dir());
                assert_eq!(
                    Git::run(&repo.source, &["rev-parse", &repo.branch]).unwrap(),
                    snapshot
                );
                assert_eq!(
                    Git::run(&repo.source, &["show", &format!("{}:a", repo.branch)]).unwrap(),
                    "guest"
                );
                assert_eq!(fs::read_to_string(repo.source.join("a")).unwrap(), "base\n");
                assert_eq!(
                    Git::run(&repo.source, &["rev-parse", "HEAD"]).unwrap(),
                    repo.base_commit
                );
                for (path, bytes) in retained_refs {
                    assert_eq!(fs::read(path).unwrap(), bytes);
                }
                assert!(
                    fs::read_dir(repo.source.join(".git/objects/pack"))
                        .unwrap()
                        .all(|entry| entry
                            .unwrap()
                            .path()
                            .extension()
                            .is_none_or(|ext| ext != "keep"))
                );
                Git.restore_and_import_guest_metadata(&repo).unwrap();
                assert!(repo.worktree.join(".git").is_file());
                Git.remove_worktree(&repo.source, &repo.worktree, true)
                    .unwrap();
            }
        }
    }

    #[test]
    fn failed_guest_snapshot_import_retains_required_objects_and_metadata() {
        for object in ["HEAD:a", "HEAD^{tree}", "HEAD", "HEAD^"] {
            let (_root, repo) = isolated_import_fixture();
            fs::write(repo.worktree.join("second"), "second guest commit\n").unwrap();
            git(&repo.worktree, &["add", "second"]);
            git(&repo.worktree, &["commit", "-m", "second guest commit"]);
            add_broken_import_ref(&repo.source, false, repo.base_commit.len());
            let oid = Git::run(&repo.worktree, &["rev-parse", object]).unwrap();
            fs::remove_file(
                repo.worktree
                    .join(".git/objects")
                    .join(&oid[..2])
                    .join(&oid[2..]),
            )
            .unwrap();
            let error = Git.restore_and_import_guest_metadata(&repo).unwrap_err();
            if object != "HEAD" {
                let message = format!("{error:#}");
                assert!(
                    message.contains("branch-scoped import also failed"),
                    "{message}"
                );
                assert!(
                    message.contains("cannot export guest snapshot"),
                    "{message}"
                );
            }
            assert!(repo.worktree.join(".git").is_dir());
            assert!(repo.host_gitfile.is_file());
            assert_eq!(
                fs::read_to_string(repo.worktree.join("a")).unwrap(),
                "guest\n"
            );
            assert_eq!(
                Git::run(&repo.source, &["rev-parse", &repo.branch]).unwrap(),
                repo.base_commit
            );
        }
    }

    #[test]
    fn guest_import_repairs_missing_host_objects_even_when_tips_match() {
        for object in ["HEAD:a", "HEAD^{tree}"] {
            let (_root, repo) = isolated_import_fixture();
            Git.import_guest_commits(&repo).unwrap();
            let oid = Git::run(&repo.source, &["rev-parse", object]).unwrap();
            fs::remove_file(
                repo.source
                    .join(".git/objects")
                    .join(&oid[..2])
                    .join(&oid[2..]),
            )
            .unwrap();
            let snapshot = Git::run(&repo.worktree, &["rev-parse", "HEAD"]).unwrap();
            assert!(Git::verify_snapshot_objects(&repo.source, &snapshot).is_err());
            Git.import_guest_commits(&repo).unwrap();
            Git::verify_snapshot_objects(&repo.source, &snapshot).unwrap();
            assert!(repo.worktree.join(".git").is_dir());
            assert_eq!(fs::read_to_string(repo.source.join("a")).unwrap(), "base\n");
            assert_eq!(
                Git::run(&repo.source, &["rev-parse", &repo.branch]).unwrap(),
                snapshot
            );
        }
    }

    #[test]
    fn fallback_guest_import_still_rejects_rewritten_history() {
        let (_root, repo) = isolated_import_fixture();
        Git.import_guest_commits(&repo).unwrap();
        let previous = Git::run(&repo.source, &["rev-parse", &repo.branch]).unwrap();
        git(&repo.worktree, &["reset", "--hard", &repo.base_commit]);
        fs::write(repo.worktree.join("a"), "rewritten\n").unwrap();
        git(&repo.worktree, &["commit", "-am", "rewrite"]);
        add_broken_import_ref(&repo.source, false, previous.len());
        let error = Git.restore_and_import_guest_metadata(&repo).unwrap_err();
        assert!(error.to_string().contains("rewrote history"));
        assert!(repo.worktree.join(".git").is_dir());
        assert_eq!(
            Git::run(&repo.source, &["rev-parse", &repo.branch]).unwrap(),
            previous
        );
    }

    #[test]
    fn guest_pack_keep_guard_removes_only_its_own_keep_file() {
        let (_root, repo) = isolated_import_fixture();
        let snapshot = Git::run(&repo.worktree, &["rev-parse", "HEAD"]).unwrap();
        let keep = Git::import_guest_pack(&repo, &snapshot).unwrap();
        let path = keep.path.clone();
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), keep.marker);
        drop(keep);
        assert!(!path.exists());

        fs::write(&path, "another operation\n").unwrap();
        drop(Git::import_guest_pack(&repo, &snapshot).unwrap());
        assert_eq!(fs::read_to_string(path).unwrap(), "another operation\n");
    }

    #[test]
    fn failed_guest_ref_publication_retains_isolated_metadata() {
        for fallback in [false, true] {
            let (_root, repo) = isolated_import_fixture();
            if fallback {
                add_broken_import_ref(&repo.source, false, repo.base_commit.len());
            }
            let lock = repo
                .source
                .join(".git/refs/heads")
                .join(format!("{}.lock", repo.branch));
            fs::write(&lock, "another operation\n").unwrap();
            let error = Git.restore_and_import_guest_metadata(&repo).unwrap_err();
            assert!(error.to_string().contains("update-ref"));
            assert_eq!(fs::read_to_string(lock).unwrap(), "another operation\n");
            assert!(repo.worktree.join(".git").is_dir());
            assert!(repo.host_gitfile.is_file());
            assert_eq!(
                Git::run(&repo.source, &["rev-parse", &repo.branch]).unwrap(),
                repo.base_commit
            );
        }
    }

    #[test]
    fn isolates_worktree_with_symlinked_git_link() {
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
        let gitfile = wt.join(".git");
        let gitdir = std::fs::read_to_string(&gitfile)
            .unwrap()
            .strip_prefix("gitdir: ")
            .unwrap()
            .trim()
            .to_owned();
        std::fs::remove_file(&gitfile).unwrap();
        std::os::unix::fs::symlink(gitdir, &gitfile).unwrap();
        assert!(
            std::fs::symlink_metadata(&gitfile)
                .unwrap()
                .file_type()
                .is_symlink()
        );

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
        assert!(gitfile.is_dir());

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
        Git.restore_guest_metadata(&repo).unwrap();
        assert!(gitfile.is_file());
        assert!(
            !std::fs::symlink_metadata(&gitfile)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&gitfile)
                .unwrap()
                .starts_with("gitdir: ")
        );
        assert!(
            Git::run(&wt, &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
        let restored_gitdir = std::fs::read_to_string(&gitfile)
            .unwrap()
            .strip_prefix("gitdir: ")
            .unwrap()
            .trim()
            .to_owned();
        std::fs::remove_file(&gitfile).unwrap();
        std::os::unix::fs::symlink(restored_gitdir, &gitfile).unwrap();
        Git.remove_worktree(tmp.path(), &wt, false).unwrap();
        assert!(!wt.exists());
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
