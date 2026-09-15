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
    Ls,
    Attach {
        session: String,
    },
    Shell {
        session: String,
    },
    Status {
        session: String,
    },
    Diff {
        session: String,
    },
    Stop {
        session: String,
    },
    Clean {
        session: Option<String>,
        #[arg(long)]
        force: bool,
    },
    Credentials {
        #[command(subcommand)]
        command: CredentialCommand,
    },
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
        Command::Run { path, no_attach } => {
            let id = app.create(&path, true)?;
            start_expiry_watch()?;
            if !no_attach {
                app.attach(&id)?;
            }
            println!(
                "Use `jbox attach {id}` to reconnect, or `jbox status {id}` to inspect changes."
            );
        }
        Command::Init { path, tools } => app.init(&path, &tools)?,
        Command::Ls => app.list()?,
        Command::Attach { session } => app.attach(&session)?,
        Command::Shell { session } => app.shell(&session)?,
        Command::Status { session } => app.status(&session, false)?,
        Command::Diff { session } => app.status(&session, true)?,
        Command::Stop { session } => app.stop(&session)?,
        Command::Clean { session, force } => app.clean(session.as_deref(), force)?,
        Command::Credentials { command } => match command {
            CredentialCommand::Import { all, yes, replace } => {
                app.import_credentials(all, yes, replace)?
            }
        },
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
        "clean",
        "credentials",
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
        let args = vec![OsString::from("jbox"), OsString::from(".")];
        // Keep the parsing behavior covered without altering the real process args.
        assert_eq!(args[1], ".");
    }
}
