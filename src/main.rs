use anyhow::Result;
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "jbox",
    version,
    about = "Disposable Kata microVM workspaces for jcode"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Start the guest from a non-mutating snapshot of staged, unstaged, and nonignored untracked host files.
        #[arg(long, requires = "new")]
        include_host_changes: bool,
        /// Create a fresh workspace instead of reconnecting the latest retained session for this repository.
        #[arg(long)]
        new: bool,
        #[arg(long)]
        no_attach: bool,
    },
    /// Create a project-local jbox configuration and container image template.
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Debian package to install in the generated container image. Repeat for more packages.
        #[arg(long = "tool", value_name = "APT_PACKAGE")]
        tools: Vec<String>,
    },
    Ls {
        /// List every jbox session across all repositories.
        #[arg(long)]
        all: bool,
    },
    Attach {
        /// Session to attach. From a participating repository, opens its workspace; otherwise
        /// opens the primary workspace. Omit it to select a running workspace for the current repository.
        session: Option<String>,
    },
    Shell {
        /// Session to open. Omit it to select a running workspace for the current repository.
        session: Option<String>,
    },
    Status {
        /// Session to inspect. Omit it to select a worktree for the current repository.
        session: Option<String>,
    },
    Diff {
        /// Session to inspect. Omit it to select a worktree for the current repository.
        session: Option<String>,
    },
    Stop {
        /// Session to stop. Omit it to select a workspace for the current repository.
        session: Option<String>,
    },
    /// Temporarily apply a session's uncommitted changes to the current host repository, or undo that overlay.
    Overlay {
        /// Session that owns the generated worktree.
        session: Option<String>,
        /// Remove the previously applied overlay without touching unrelated test output.
        #[arg(long)]
        undo: bool,
    },
    /// Open a disposable host-native preview of a session's current changes.
    Preview {
        /// Session to preview. Omit it to select a workspace for the current repository.
        session: Option<String>,
    },
    /// Fast-forward a host branch to a session's committed snapshot without stopping it.
    Accept {
        /// Session to accept. Omit it to select a session for the current repository.
        #[arg(conflicts_with = "all")]
        session: Option<String>,
        /// Existing local branch checked out in each host repository. With --all,
        /// omit this to use each repository's currently checked-out branch.
        #[arg(long)]
        into: Option<String>,
        /// Select a session and accept every repository worktree in that session.
        #[arg(long)]
        all: bool,
        /// Commit visible changes in jbox-generated worktrees before accepting them.
        #[arg(long)]
        checkpoint: bool,
        /// Preserve dirty host files in a temporary stash and reapply them afterward.
        #[arg(long)]
        stash_host: bool,
        /// Explicitly create a Git merge when the guest and host branches diverge.
        #[arg(long)]
        merge: bool,
        /// Commit a reviewed, staged host merge that was paused by `accept --merge`.
        #[arg(
            long = "continue",
            conflicts_with_all = ["session", "into", "all", "checkpoint", "stash_host", "merge", "abort"]
        )]
        continue_accept: bool,
        /// Abort a host merge paused by `accept --merge`, retaining its jbox session.
        #[arg(
            long,
            conflicts_with_all = ["session", "into", "all", "checkpoint", "stash_host", "merge", "continue_accept"]
        )]
        abort: bool,
    },
    /// Preflight, stop, and rebase every worktree in the selected session onto host branches.
    Rebase {
        /// Branch to rebase every session worktree onto. Defaults to each host repository's current branch.
        #[arg(long)]
        onto: Option<String>,
    },
    /// Synchronize every repository in a selected project session with its configured upstream.
    Sync {
        /// Report every repository's synchronization plan without changing branches or remotes.
        #[arg(long)]
        dry_run: bool,
        /// Commit visible generated-worktree changes before rebasing and accepting them.
        #[arg(long)]
        checkpoint: bool,
        /// Confirm local rebases, acceptance, and every non-force push without prompts.
        #[arg(long, conflicts_with = "dry_run")]
        yes: bool,
        /// Resume a retained synchronization after resolving its reported Git operation.
        #[arg(long = "continue", conflicts_with_all = ["dry_run", "checkpoint", "abort"])]
        continue_sync: bool,
        /// Abort a currently paused host or guest rebase, retaining all completed sync stages.
        #[arg(long, conflicts_with_all = ["dry_run", "checkpoint", "yes", "continue_sync"])]
        abort: bool,
    },
    /// Open a local shell in a retained worktree to resolve a Git operation before resuming.
    Resolve {
        /// Session whose worktree to open. Omit it to select from the current repository.
        session: Option<String>,
    },
    /// Restart a retained stopped session, or attach when it is already running. Omit the session to select one for the current repository.
    Resume {
        session: Option<String>,
    },
    Clean {
        /// Session to clean. Omit it to select a workspace for the current repository.
        session: Option<String>,
        #[arg(long)]
        force: bool,
        /// Consider every session, rather than selecting one belonging to the current repository.
        #[arg(long, conflicts_with = "session")]
        all: bool,
    },
    Credentials {
        #[command(subcommand)]
        command: CredentialCommand,
    },
    /// Inspect non-mutating brokered GitHub action plans against frozen session policy.
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// Load Kata VSOCK and guest-networking modules for the current boot.
    Prime,
    Doctor,
    Expire,
    #[command(hide = true)]
    WatchExpiry,
}

#[derive(Subcommand)]
enum CredentialCommand {
    /// Explicitly copy supported provider stores to jbox-managed credential state.
    Import {
        /// Select every supported local credential store.
        #[arg(long)]
        all: bool,
        /// Confirm copying provider credentials into jbox-managed state.
        #[arg(long)]
        yes: bool,
        /// Replace existing jbox-managed copies. Never affects host source files.
        #[arg(long)]
        replace: bool,
    },
    /// Manage isolated, per-account GitHub CLI profiles for repository-scoped grants.
    Github {
        #[command(subcommand)]
        command: GithubCredentialCommand,
    },
}

#[derive(Subcommand)]
enum GithubCredentialCommand {
    /// Copy exactly one authenticated GitHub CLI account into Jbox-managed state.
    Import {
        /// GitHub login selected from the host GitHub CLI store.
        account: String,
        /// Confirm extracting this account's token into Jbox-managed state.
        #[arg(long)]
        yes: bool,
        /// Replace an existing Jbox-managed profile for this account.
        #[arg(long)]
        replace: bool,
    },
    /// Validate an existing Jbox-managed profile without contacting GitHub.
    Status {
        /// GitHub login whose isolated profile should be checked.
        account: String,
    },
}

#[derive(Subcommand)]
enum PolicyCommand {
    /// Evaluate and durably record a brokered action dry-run. No credential is read and no network request is made.
    Plan {
        /// Retained Jbox session whose frozen policy will be checked.
        session: String,
        /// Jbox repository name from `jbox status <session>`.
        #[arg(long)]
        repository: String,
        /// Local Git remote name, for example `origin`.
        #[arg(long)]
        remote: String,
        /// Proposed operation: clone, fetch, push-branch, pr-view, pr-status, ci-view, pr-create, or pr-update.
        #[arg(long)]
        operation: String,
        /// Required only for push-branch. Must be a full refs/heads/<branch> ref.
        #[arg(long = "ref")]
        reference: Option<String>,
    },
    /// Print durable brokered action dry-runs for a retained session.
    Plans { session: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse_from(normalized_args());
    let app = jbox::App::open()?;
    match cli.command {
        Command::Run {
            path,
            include_host_changes,
            new,
            no_attach,
        } => {
            if !new && app.reconnect_latest_from_repository(&path, !no_attach)? {
                return Ok(());
            }
            let launch_directory = std::env::current_dir()?;
            let id = app.create(&path, include_host_changes, &launch_directory)?;
            jbox::start_expiry_watch()?;
            if !no_attach {
                app.attach(&id)?;
            }
            println!(
                "Use `jbox attach {id}` to reconnect, or `jbox status {id}` to inspect changes."
            );
        }
        Command::Init { path, tools } => app.init(&path, &tools)?,
        Command::Ls { all } => {
            if all {
                app.list_all()?
            } else {
                app.list_from_repository(&std::env::current_dir()?)?
            }
        }
        Command::Attach { session } => match session {
            Some(session) => app.attach(&session)?,
            None => app.attach_from_repository(&std::env::current_dir()?)?,
        },
        Command::Shell { session } => match session {
            Some(session) => app.shell(&session)?,
            None => app.shell_from_repository(&std::env::current_dir()?)?,
        },
        Command::Status { session } => match session {
            Some(session) => app.status(&session, false)?,
            None => app.status_from_repository(&std::env::current_dir()?, false)?,
        },
        Command::Diff { session } => match session {
            Some(session) => app.status(&session, true)?,
            None => app.status_from_repository(&std::env::current_dir()?, true)?,
        },
        Command::Stop { session } => match session {
            Some(session) => app.stop(&session)?,
            None => app.stop_from_repository(&std::env::current_dir()?)?,
        },
        Command::Overlay { session, undo } => match session {
            Some(session) => app.overlay(&session, &std::env::current_dir()?, undo)?,
            None => app.overlay_from_repository(&std::env::current_dir()?, undo)?,
        },
        Command::Preview { session } => match session {
            Some(session) => app.preview(&session, &std::env::current_dir()?)?,
            None => app.preview_from_repository(&std::env::current_dir()?)?,
        },
        Command::Accept {
            session,
            into,
            all,
            checkpoint,
            stash_host,
            merge,
            continue_accept,
            abort,
        } => {
            if continue_accept {
                app.continue_accept_from_repository(&std::env::current_dir()?)?;
                return Ok(());
            }
            if abort {
                app.abort_accept_from_repository(&std::env::current_dir()?)?;
                return Ok(());
            }
            let options = jbox::AcceptOptions {
                checkpoint,
                stash_host,
                merge,
            };
            match (session, all) {
                (Some(session), false) => app.accept(
                    &session,
                    into.as_deref().ok_or_else(|| {
                        anyhow::anyhow!("`jbox accept <session>` requires --into <branch>")
                    })?,
                    options,
                )?,
                (None, true) => app.accept_all_from_repository(
                    &std::env::current_dir()?,
                    into.as_deref(),
                    options,
                )?,
                (None, false) => {
                    app.accept_from_repository(&std::env::current_dir()?, into.as_deref(), options)?
                }
                (Some(_), true) => unreachable!("Clap rejects --all with an explicit session"),
            }
        }
        Command::Rebase { onto } => {
            app.rebase_from_repository(&std::env::current_dir()?, onto.as_deref())?
        }
        Command::Sync {
            dry_run,
            checkpoint,
            yes,
            continue_sync,
            abort,
        } => app.sync_from_repository(
            &std::env::current_dir()?,
            jbox::SyncOptions {
                dry_run,
                checkpoint,
                yes,
                continue_sync,
                abort,
            },
        )?,
        Command::Resolve { session } => match session {
            Some(session) => app.resolve(&session, &std::env::current_dir()?)?,
            None => app.resolve_from_repository(&std::env::current_dir()?)?,
        },
        Command::Resume { session } => match session {
            Some(session) => app.resume(&session)?,
            None => app.resume_from_repository(&std::env::current_dir()?)?,
        },
        Command::Clean {
            session,
            force,
            all,
        } => match (session, all) {
            (Some(session), _) => app.clean(Some(&session), force)?,
            (None, true) => app.clean(None, force)?,
            (None, false) => app.clean_from_repository(&std::env::current_dir()?, force)?,
        },
        Command::Credentials { command } => match command {
            CredentialCommand::Import { all, yes, replace } => {
                app.import_credentials(all, yes, replace)?
            }
            CredentialCommand::Github { command } => match command {
                GithubCredentialCommand::Import {
                    account,
                    yes,
                    replace,
                } => app.import_github_cli_profile(&account, yes, replace)?,
                GithubCredentialCommand::Status { account } => {
                    app.github_cli_profile_status(&account)?
                }
            },
        },
        Command::Policy { command } => match command {
            PolicyCommand::Plan {
                session,
                repository,
                remote,
                operation,
                reference,
            } => app.plan_brokered_action(
                &session,
                &repository,
                &remote,
                &operation,
                reference.as_deref(),
            )?,
            PolicyCommand::Plans { session } => app.list_brokered_plans(&session)?,
        },
        Command::Prime => app.prime()?,
        Command::Doctor => app.doctor()?,
        Command::Expire => app.expire()?,
        Command::WatchExpiry => loop {
            app.expire()?;
            std::thread::sleep(std::time::Duration::from_secs(60));
        },
    }
    Ok(())
}

/// `jbox .` is the primary UX. Known command words still use Clap subcommands.
fn normalized_args() -> Vec<OsString> {
    let mut args = std::env::args_os().collect::<Vec<_>>();
    let known = [
        "run",
        "init",
        "ls",
        "attach",
        "shell",
        "status",
        "diff",
        "stop",
        "overlay",
        "preview",
        "accept",
        "rebase",
        "sync",
        "resolve",
        "resume",
        "clean",
        "credentials",
        "policy",
        "prime",
        "doctor",
        "expire",
        "watch-expiry",
        "--help",
        "-h",
        "--version",
        "-V",
    ];
    let is_command = args
        .get(1)
        .and_then(|value| value.to_str())
        .is_some_and(|word| known.contains(&word));
    if !is_command {
        args.insert(1, OsString::from("run"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bare_path_becomes_run() {
        let args = [OsString::from("jbox"), OsString::from(".")];
        // Keep the parsing behavior covered without altering the real process args.
        assert_eq!(args[1], ".");
    }

    #[test]
    fn run_reconnects_by_default_and_requires_new_for_host_snapshot() {
        let cli = Cli::try_parse_from(["jbox", "run", "some-repo", "--no-attach"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Run {
                path,
                new: false,
                no_attach: true,
                include_host_changes: false,
            } if path == std::path::Path::new("some-repo")
        ));
        assert!(Cli::try_parse_from(["jbox", "run", "--include-host-changes"]).is_err());
        let cli = Cli::try_parse_from([
            "jbox",
            "run",
            "--new",
            "--include-host-changes",
            "some-repo",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Run {
                new: true,
                include_host_changes: true,
                ..
            }
        ));
    }

    #[test]
    fn overlay_accepts_repository_scoped_invocation_without_a_session_id() {
        let cli = Cli::try_parse_from(["jbox", "overlay"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Overlay {
                session: None,
                undo: false
            }
        ));
    }

    #[test]
    fn sync_uses_project_scope_and_standard_continue_flag() {
        let cli = Cli::try_parse_from(["jbox", "sync", "--continue"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Sync {
                dry_run: false,
                checkpoint: false,
                yes: false,
                continue_sync: true,
                abort: false,
            }
        ));
    }

    #[test]
    fn github_profile_import_requires_an_explicit_account_and_confirmation_flag() {
        let cli = Cli::try_parse_from([
            "jbox",
            "credentials",
            "github",
            "import",
            "jbox-project-bot",
            "--yes",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Credentials {
                command: CredentialCommand::Github {
                    command: GithubCredentialCommand::Import {
                        account,
                        yes: true,
                        replace: false,
                    },
                },
            } if account == "jbox-project-bot"
        ));
    }

    #[test]
    fn policy_plan_requires_explicit_session_repository_remote_and_operation() {
        let cli = Cli::try_parse_from([
            "jbox",
            "policy",
            "plan",
            "calm-otter-123",
            "--repository",
            "jbox",
            "--remote",
            "origin",
            "--operation",
            "push-branch",
            "--ref",
            "refs/heads/jbox/policy",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Policy {
                command: PolicyCommand::Plan {
                    session,
                    repository,
                    remote,
                    operation,
                    reference: Some(reference),
                },
            } if session == "calm-otter-123"
                && repository == "jbox"
                && remote == "origin"
                && operation == "push-branch"
                && reference == "refs/heads/jbox/policy"
        ));
    }
}
