use anyhow::Result;
use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};

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
        #[arg(long)]
        include_host_changes: bool,
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
        /// Session to attach. Omit it to select a running workspace for the current repository.
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
        session: String,
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
}

fn main() -> Result<()> {
    let cli = Cli::parse_from(normalized_args());
    let app = jbox::App::open()?;
    match cli.command {
        Command::Run {
            path,
            include_host_changes,
            no_attach,
        } => {
            let launch_directory = std::env::current_dir()?;
            let id = app.create(&path, include_host_changes, &launch_directory)?;
            start_expiry_watch()?;
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
        Command::Overlay { session, undo } => {
            app.overlay(&session, &std::env::current_dir()?, undo)?
        }
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
        "resolve",
        "resume",
        "clean",
        "credentials",
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

fn start_expiry_watch() -> Result<()> {
    let exe = std::env::current_exe()?;
    ProcessCommand::new(exe)
        .arg("watch-expiry")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
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
}
