pub mod config;
pub mod engine;
pub mod git;
pub mod image;
pub mod paths;
pub mod state;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use config::{Config, ResolvedRepository};
use engine::{ContainerSpec, DockerEngine, Engine};
use git::Git;
use image::ImageManager;
use paths::{JboxPaths, safe_target};
use state::{RepoState, Session, SessionState, StateStore};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const JCODE_SOCKET: &str = "/home/jbox/.local/share/jcode/jbox.sock";

pub struct App {
    pub paths: JboxPaths,
    pub state: StateStore,
    git: Git,
    engine: DockerEngine,
}

impl App {
    pub fn open() -> Result<Self> {
        let paths = JboxPaths::discover()?;
        paths.ensure()?;
        Ok(Self {
            state: StateStore::new(paths.clone()),
            paths,
            git: Git,
            engine: DockerEngine,
        })
    }

    pub fn doctor(&self) -> Result<()> {
        println!("jbox state: {}", self.paths.data.display());
        println!(
            "KVM: {}",
            if Path::new("/dev/kvm").exists() {
                "available"
            } else {
                "missing"
            }
        );
        println!(
            "VSOCK: {}",
            if Path::new("/dev/vhost-vsock").exists() {
                "available"
            } else {
                "missing: load vhost_vsock"
            }
        );
        let vhost_net = std::fs::read_to_string("/proc/modules")
            .map(|modules| modules.lines().any(|line| line.starts_with("vhost_net ")))
            .unwrap_or(false);
        println!(
            "Kata guest networking: {}",
            if vhost_net {
                "vhost_net loaded"
            } else {
                "vhost_net missing: run `sudo modprobe vhost_net`"
            }
        );
        match self.engine.check() {
            Ok(()) => println!("Docker + Kata runtime: ready"),
            Err(error) => {
                println!("Docker + Kata runtime: unavailable");
                println!("  {error:#}");
                println!(
                    "  Install Kata, register it as Docker's `kata` OCI runtime, then run `docker info`."
                );
            }
        }
        println!(
            "Network: Docker bridge grants Internet access but cannot enforce host/LAN denial by itself. Apply host firewall policy before treating this boundary as strict."
        );
        Ok(())
    }

    pub fn create(&self, input: &Path, no_attach: bool) -> Result<String> {
        let (config, primary) = Config::load(input)?;
        let session_id = state::new_session_id();

        let mut resolved = vec![ResolvedRepository {
            source: primary.clone(),
            mount: config.workspace.mount.clone(),
            name: config.project_name(&primary),
        }];
        resolved.extend(config.resolve_repositories(&primary)?);
        config.validate_repositories(&resolved)?;
        self.engine.check()?;

        if config.git.network {
            self.paths.ensure_credentials()?;
        }
        let image = ImageManager::new(&self.paths).ensure(&config, &primary)?;
        let ssh_host = self.next_ssh_host(&session_id)?;
        let session_dir = self.paths.sessions.join(&session_id);
        let worktrees = session_dir.join("worktrees");
        std::fs::create_dir_all(&worktrees)?;

        let mut repos: Vec<RepoState> = Vec::with_capacity(resolved.len());
        for repo in &resolved {
            let worktree = worktrees.join(&repo.name);
            let created = match self
                .git
                .add_worktree(&repo.source, &worktree, &session_id, &repo.name)
                .with_context(|| format!("could not create worktree for {}", repo.source.display()))
            {
                Ok(created) => created,
                Err(error) => {
                    for created_repo in &repos {
                        let _ = self.git.remove_worktree(
                            &created_repo.source,
                            &created_repo.worktree,
                            false,
                        );
                    }
                    return Err(error);
                }
            };
            println!(
                "{}: sandbox branch {} starts at {}",
                repo.name, created.branch, created.commit
            );
            let host_gitfile = session_dir
                .join("host-gitfiles")
                .join(format!("{}.git", repo.name));
            if let Err(error) = self.git.isolate_guest_metadata(
                &repo.source,
                &worktree,
                &created.branch,
                &created.commit,
                &host_gitfile,
            ) {
                let _ = self.git.remove_worktree(&repo.source, &worktree, false);
                self.cleanup_worktrees(&repos);
                let _ = std::fs::remove_dir_all(&session_dir);
                return Err(error).context("could not prepare isolated guest Git metadata");
            }
            repos.push(RepoState {
                name: repo.name.clone(),
                source: repo.source.clone(),
                worktree,
                mount: repo.mount.clone(),
                branch: created.branch,
                base_commit: created.commit,
                host_gitfile,
            });
        }

        let ssh = self.paths.session_ssh_dir(&session_id);
        if let Err(error) = self.paths.create_session_ssh(&ssh) {
            self.cleanup_worktrees(&repos);
            let _ = std::fs::remove_dir_all(&session_dir);
            return Err(error).context("could not create session SSH credentials");
        }
        let ssh_agent_pid = match self.paths.start_session_ssh_agent(&ssh) {
            Ok(pid) => pid,
            Err(error) => {
                self.cleanup_worktrees(&repos);
                let _ = std::fs::remove_dir_all(&session_dir);
                return Err(error);
            }
        };
        let container_name = format!("jbox-{session_id}");
        let spec = match self.container_spec(
            &config,
            &repos,
            &container_name,
            &ssh,
            image.clone(),
            &ssh_host,
        ) {
            Ok(spec) => spec,
            Err(error) => {
                self.cleanup_worktrees(&repos);
                self.paths.stop_session_ssh_agent(ssh_agent_pid);
                let _ = std::fs::remove_dir_all(&session_dir);
                return Err(error);
            }
        };
        let port = match self.engine.start(&spec) {
            Ok(port) => port,
            Err(error) => {
                self.cleanup_worktrees(&repos);
                self.paths.stop_session_ssh_agent(ssh_agent_pid);
                let _ = std::fs::remove_dir_all(&session_dir);
                return Err(error);
            }
        };
        let known_hosts_tag = match self.paths.trust_session_host(&ssh_host, &session_id) {
            Ok(tag) => tag,
            Err(error) => {
                let _ = self.engine.stop(&container_name);
                self.paths.stop_session_ssh_agent(ssh_agent_pid);
                self.cleanup_worktrees(&repos);
                let _ = std::fs::remove_dir_all(&session_dir);
                return Err(error);
            }
        };

        let now = Utc::now();
        let session = Session {
            version: 1,
            id: session_id.clone(),
            state: SessionState::Running,
            container_name,
            ssh_host: ssh_host.clone(),
            ssh_port: port,
            ssh_agent_pid,
            known_hosts_tag,
            created_at: now,
            last_activity_at: now,
            ttl_seconds: config.resources.ttl_seconds,
            config_path: config.path,
            image,
            repos,
        };
        self.state.save(&session)?;
        println!("jbox session {session_id} is running on {ssh_host}:{port}");
        if !no_attach {
            self.attach(&session_id)?;
        }
        Ok(session_id)
    }

    fn container_spec(
        &self,
        config: &Config,
        repos: &[RepoState],
        name: &str,
        ssh: &Path,
        image: String,
        ssh_host: &str,
    ) -> Result<ContainerSpec> {
        let mut mounts = Vec::new();
        for repo in repos {
            mounts.push((repo.worktree.clone(), safe_target(&repo.mount)?, true));
        }
        for mount in &config.mounts {
            mounts.push((
                mount
                    .source_path
                    .clone()
                    .context("configured mount source was not resolved")?,
                safe_target(&mount.target)?,
                mount.writable,
            ));
        }
        if config.jcode.persistent_credentials {
            for (source, target) in self.paths.jcode_credential_mounts()? {
                mounts.push((source, target, true));
            }
        }
        if config.git.network {
            mounts.push((
                self.paths.credentials.join("git").join("id_ed25519"),
                PathBuf::from("/home/jbox/.ssh/id_ed25519"),
                false,
            ));
        }
        mounts.push((
            ssh.join("authorized_keys"),
            PathBuf::from("/home/jbox/.ssh/authorized_keys"),
            false,
        ));
        Ok(ContainerSpec {
            name: name.into(),
            image,
            mounts,
            cpus: config.resources.cpus,
            memory: config.resources.memory.clone(),
            network: config.network.clone(),
            ssh_host: ssh_host.to_owned(),
        })
    }

    pub fn attach(&self, id: &str) -> Result<()> {
        let mut session = self.state.load(id)?;
        self.require_running(&session)?;
        self.touch(&mut session)?;
        let ssh_socket = self.paths.session_ssh_dir(&session.id).join("agent.sock");
        let status = Command::new("jcode")
            .args([
                "--ssh",
                &format!("jbox@{}", session.ssh_host),
                "--ssh-binary",
                "/usr/local/bin/jcode",
                "--ssh-server-socket",
                JCODE_SOCKET,
                "--remote-working-dir",
                &session.repos[0].mount,
            ])
            .env("SSH_AUTH_SOCK", ssh_socket)
            .status()
            .context("could not launch local jcode")?;
        if !status.success() {
            bail!("local jcode exited with {status}");
        }
        Ok(())
    }

    pub fn shell(&self, id: &str) -> Result<()> {
        let mut session = self.state.load(id)?;
        self.require_running(&session)?;
        self.touch(&mut session)?;
        let config = self.paths.write_ssh_config(&session)?;
        let status = Command::new("ssh")
            .args(["-F", config.to_str().unwrap(), "jbox", "-t", "bash", "-l"])
            .status()?;
        if !status.success() {
            bail!("ssh exited with {status}");
        }
        Ok(())
    }

    pub fn import_credentials(&self, all: bool, confirmed: bool, replace: bool) -> Result<()> {
        if !all {
            bail!(
                "credential import requires `--all`; jbox never guesses which provider credentials to copy"
            );
        }
        let candidates = self.paths.local_credentials()?;
        if candidates.is_empty() {
            println!("No supported local Jcode credential stores were found.");
            return Ok(());
        }
        println!("The following provider credential stores may be copied into jbox-managed state:");
        for credential in &candidates {
            println!(
                "- {} ({})",
                credential.provider_hint,
                credential.source.display()
            );
        }
        if !confirmed {
            println!(
                "Nothing was copied. Re-run with `jbox credentials import --all --yes` to explicitly copy these credentials. This never mounts your host credential directories."
            );
            return Ok(());
        }
        let report = self.paths.import_local_credentials(replace)?;
        for credential in &report.imported {
            println!("imported {}", credential.provider_hint);
        }
        for credential in &report.retained {
            println!(
                "retained existing jbox credential for {}; use --replace to overwrite it",
                credential.provider_hint
            );
        }
        println!(
            "Credentials are stored only under {} and are available to newly created jbox guests.",
            self.paths.credentials.join("jcode").display()
        );
        Ok(())
    }

    pub fn stop(&self, id: &str) -> Result<()> {
        let mut session = self.state.load(id)?;
        if session.state == SessionState::Running {
            self.engine.stop(&session.container_name)?;
            self.paths.stop_session_ssh_agent(session.ssh_agent_pid);
            self.paths.untrust_session_host(&session.known_hosts_tag)?;
            for repo in &session.repos {
                self.git.restore_and_import_guest_metadata(repo)?;
            }
            session.state = SessionState::Stopped;
            self.state.save(&session)?;
        }
        println!("jbox session {id} stopped. Worktrees were retained.");
        Ok(())
    }

    pub fn list(&self) -> Result<()> {
        let sessions = self.state.list()?;
        if sessions.is_empty() {
            println!("No jbox sessions.");
            return Ok(());
        }
        println!("ID\tSTATE\tLAST ACTIVITY\tREPOSITORIES");
        for s in sessions {
            println!(
                "{}\t{:?}\t{}\t{}",
                s.id,
                s.state,
                s.last_activity_at.format("%Y-%m-%d %H:%M UTC"),
                s.repos
                    .iter()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            );
        }
        Ok(())
    }

    pub fn status(&self, id: &str, diff: bool) -> Result<()> {
        let session = self.state.load(id)?;
        println!("{} ({:?})", session.id, session.state);
        for repo in &session.repos {
            let output = if diff {
                self.git.diff_stat(repo)?
            } else {
                self.git.status(&repo.worktree)?
            };
            println!("\n{} [{}]", repo.name, repo.branch);
            print!("{output}");
        }
        Ok(())
    }

    pub fn clean(&self, id: Option<&str>, force: bool) -> Result<()> {
        let targets = match id {
            Some(id) => vec![self.state.load(id)?],
            None => self.state.list()?,
        };
        for mut session in targets {
            let changed = session
                .repos
                .iter()
                .any(|r| self.git.has_changes_or_unique_commits(r).unwrap_or(true));
            if changed && !force {
                println!(
                    "refusing to clean {}: worktrees contain changes or commits. Use `jbox clean {} --force` only after inspecting `jbox status {}`.",
                    session.id, session.id, session.id
                );
                continue;
            }
            if session.state == SessionState::Running {
                self.engine.stop(&session.container_name)?;
                self.paths.stop_session_ssh_agent(session.ssh_agent_pid);
                self.paths.untrust_session_host(&session.known_hosts_tag)?;
                for repo in &session.repos {
                    self.git.restore_and_import_guest_metadata(repo)?;
                }
                session.state = SessionState::Stopped;
            }
            for repo in &session.repos {
                self.git
                    .remove_worktree(&repo.source, &repo.worktree, force)?;
            }
            self.state.remove(&session.id)?;
            let _ = std::fs::remove_dir_all(self.paths.sessions.join(&session.id));
            println!("cleaned {}", session.id);
        }
        Ok(())
    }

    pub fn expire(&self) -> Result<()> {
        for session in self.state.list()? {
            if session.state == SessionState::Running && session.expired() {
                println!("TTL expired: {}", session.id);
                self.stop(&session.id)?;
            }
        }
        Ok(())
    }

    fn require_running(&self, session: &Session) -> Result<()> {
        if session.state != SessionState::Running {
            bail!(
                "session {} is {:?}; start/resume is not implemented yet. Create a new session or inspect its retained worktree.",
                session.id,
                session.state
            );
        }
        if !self.engine.is_running(&session.container_name)? {
            bail!(
                "runtime container for {} is no longer running; worktrees were retained safely",
                session.id
            );
        }
        Ok(())
    }
    fn touch(&self, session: &mut Session) -> Result<()> {
        session.last_activity_at = Utc::now();
        self.state.save(session)
    }

    fn cleanup_worktrees(&self, repos: &[RepoState]) {
        for repo in repos {
            let _ = self.git.restore_guest_metadata(repo);
            let _ = self
                .git
                .remove_worktree(&repo.source, &repo.worktree, false);
        }
    }

    fn next_ssh_host(&self, session_id: &str) -> Result<String> {
        let used = self
            .state
            .list()?
            .into_iter()
            .map(|session| session.ssh_host)
            .collect::<std::collections::HashSet<_>>();
        let start = (session_id.bytes().fold(0u8, u8::wrapping_add) % 253) + 2;
        for offset in 0..253u8 {
            let octet = 2 + ((start - 2 + offset) % 253);
            let candidate = format!("127.0.0.{octet}");
            if !used.contains(&candidate) {
                return Ok(candidate);
            }
        }
        bail!("all dedicated jbox loopback addresses are in use")
    }
}
