use crate::state::Session;
use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const CREDENTIAL_FILE_LIMIT: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct LocalCredential {
    pub provider_hint: String,
    pub source: PathBuf,
    pub destination: PathBuf,
}

#[derive(Debug, Default)]
pub struct CredentialImportReport {
    pub imported: Vec<LocalCredential>,
    pub retained: Vec<LocalCredential>,
}

#[derive(Clone)]
pub struct JboxPaths {
    pub data: PathBuf,
    pub cache: PathBuf,
    pub sessions: PathBuf,
    pub credentials: PathBuf,
}
impl JboxPaths {
    pub fn discover() -> Result<Self> {
        let base = BaseDirs::new().context("could not determine XDG directories")?;
        let data = base.data_local_dir().join("jbox");
        let cache = base.cache_dir().join("jbox");
        Ok(Self {
            sessions: data.join("sessions"),
            credentials: data.join("credentials"),
            data,
            cache,
        })
    }
    pub fn ensure(&self) -> Result<()> {
        for path in [&self.data, &self.cache, &self.sessions, &self.credentials] {
            fs::create_dir_all(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    pub fn ensure_credentials(&self) -> Result<()> {
        let jcode = self.credentials.join("jcode");
        let git = self.credentials.join("git");
        for path in [&jcode, &git] {
            fs::create_dir_all(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        let key = git.join("id_ed25519");
        if !key.exists() {
            let status = Command::new("ssh-keygen")
                .args([
                    "-q",
                    "-t",
                    "ed25519",
                    "-N",
                    "",
                    "-C",
                    "jbox git identity",
                    "-f",
                    key.to_str().context("non-UTF8 credential path")?,
                ])
                .status()
                .context("ssh-keygen is required to create jbox Git identity")?;
            if !status.success() {
                bail!("could not generate dedicated jbox Git key");
            }
            println!(
                "Created dedicated jbox Git identity: {}. Register {}.pub with your Git provider to enable Git SSH access from jbox.",
                key.display(),
                key.display()
            );
        }
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
    /// The only credential locations that may be exposed to the guest. They
    /// live below jbox-owned state, never beneath the user's normal home.
    pub fn jcode_credential_mounts(&self) -> Result<Vec<(PathBuf, PathBuf)>> {
        let root = self.credentials.join("jcode").join("home");
        let mounts = [
            (root.join(".jcode"), "/home/jbox/.jcode"),
            (root.join(".config/jcode"), "/home/jbox/.config/jcode"),
            (
                root.join(".config/github-copilot"),
                "/home/jbox/.config/github-copilot",
            ),
            (root.join(".codex"), "/home/jbox/.codex"),
            (root.join(".claude"), "/home/jbox/.claude"),
            (root.join(".gemini"), "/home/jbox/.gemini"),
            (
                root.join(".local/share/opencode"),
                "/home/jbox/.local/share/opencode",
            ),
            (root.join(".pi/agent"), "/home/jbox/.pi/agent"),
            (root.join(".openclaw"), "/home/jbox/.openclaw"),
            (root.join(".hermes"), "/home/jbox/.hermes"),
        ];
        let mut resolved = Vec::new();
        for (source, target) in mounts {
            fs::create_dir_all(&source)?;
            fs::set_permissions(&source, fs::Permissions::from_mode(0o700))?;
            resolved.push((source, PathBuf::from(target)));
        }
        Ok(resolved)
    }

    /// Enumerate a deliberately narrow allowlist of credential files that
    /// Jcode documents as provider authentication stores. This does not read
    /// arbitrary ~/.jcode configuration, SSH material, shells, or keyrings.
    pub fn local_credentials(&self) -> Result<Vec<LocalCredential>> {
        let home = BaseDirs::new()
            .context("could not determine home directory")?
            .home_dir()
            .to_path_buf();
        self.local_credentials_from(&home)
    }

    fn local_credentials_from(&self, home: &Path) -> Result<Vec<LocalCredential>> {
        let root = self.credentials.join("jcode").join("home");
        let mut files = vec![
            ("Claude OAuth", ".jcode/auth.json"),
            ("OpenAI OAuth", ".jcode/openai-auth.json"),
            ("Gemini OAuth", ".jcode/gemini_oauth.json"),
            ("OpenAI Codex OAuth", ".codex/auth.json"),
            ("Claude Code OAuth", ".claude/.credentials.json"),
            ("Gemini CLI OAuth", ".gemini/oauth_creds.json"),
            ("GitHub Copilot", ".config/github-copilot/hosts.json"),
            ("OpenCode", ".local/share/opencode/auth.json"),
            ("pi", ".pi/agent/auth.json"),
            ("OpenClaw", ".openclaw/agent/auth.json"),
            ("OpenClaw", ".openclaw/credentials/oauth.json"),
            ("Hermes", ".hermes/auth.json"),
        ];
        let mut credentials = Vec::new();
        for (provider_hint, relative) in files.drain(..) {
            let source = home.join(relative);
            if is_safe_credential_file(&source)? {
                credentials.push(LocalCredential {
                    provider_hint: provider_hint.to_owned(),
                    source,
                    destination: root.join(relative),
                });
            }
        }
        // Jcode stores API-provider credentials as individual .env files. The
        // extension is the allowlist: config.toml, session history, and other
        // Jcode configuration are intentionally excluded.
        let env_dir = home.join(".config/jcode");
        if let Ok(entries) = fs::read_dir(&env_dir) {
            for entry in entries.flatten() {
                let source = entry.path();
                if source.extension().is_some_and(|ext| ext == "env")
                    && source.file_name().is_none_or(|name| name != "lmstudio.env")
                    && is_safe_credential_file(&source)?
                {
                    let name = source.file_name().context("credential file lacks a name")?;
                    credentials.push(LocalCredential {
                        provider_hint: format!("Jcode API credential ({})", name.to_string_lossy()),
                        destination: root.join(".config/jcode").join(name),
                        source,
                    });
                }
            }
        }
        Ok(credentials)
    }

    /// Copy an explicitly selected local provider-store allowlist into private
    /// jbox state. Existing guest credentials are never replaced unless the
    /// user makes that choice with `--replace`.
    pub fn import_local_credentials(&self, replace: bool) -> Result<CredentialImportReport> {
        let home = BaseDirs::new()
            .context("could not determine home directory")?
            .home_dir()
            .to_path_buf();
        self.import_local_credentials_from(&home, replace)
    }

    fn import_local_credentials_from(
        &self,
        home: &Path,
        replace: bool,
    ) -> Result<CredentialImportReport> {
        let mut report = CredentialImportReport::default();
        for credential in self.local_credentials_from(home)? {
            let destination_parent = credential
                .destination
                .parent()
                .context("credential destination lacks a parent")?;
            fs::create_dir_all(destination_parent)?;
            fs::set_permissions(destination_parent, fs::Permissions::from_mode(0o700))?;
            if credential.destination.exists() {
                if !replace {
                    report.retained.push(credential);
                    continue;
                }
                if fs::symlink_metadata(&credential.destination)?
                    .file_type()
                    .is_symlink()
                {
                    bail!(
                        "refusing to replace symlinked jbox credential {}",
                        credential.destination.display()
                    );
                }
            }
            let temporary = credential.destination.with_extension("jbox-importing");
            let _ = fs::remove_file(&temporary);
            fs::copy(&credential.source, &temporary)?;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
            fs::rename(&temporary, &credential.destination)?;
            report.imported.push(credential);
        }
        Ok(report)
    }
    pub fn session_ssh_dir(&self, id: &str) -> PathBuf {
        self.sessions.join(id).join("ssh")
    }
    pub fn create_session_ssh(&self, dir: &Path) -> Result<()> {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let key = dir.join("id_ed25519");
        let status = Command::new("ssh-keygen")
            .args([
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "jbox session",
                "-f",
                key.to_str().context("non-UTF8 ssh path")?,
            ])
            .status()?;
        if !status.success() {
            bail!("could not generate session SSH key");
        }
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600))?;
        // `with_extension("ed25519.pub")` would turn `id_ed25519` into
        // `id_ed25519.ed25519.pub`. ssh-keygen writes `id_ed25519.pub`.
        let public = fs::read_to_string(format!("{}.pub", key.display()))?;
        fs::write(dir.join("authorized_keys"), public)?;
        fs::set_permissions(
            dir.join("authorized_keys"),
            fs::Permissions::from_mode(0o600),
        )?;
        Ok(())
    }
    pub fn start_session_ssh_agent(&self, dir: &Path) -> Result<u32> {
        let socket = dir.join("agent.sock");
        if socket.exists() {
            fs::remove_file(&socket)?;
        }
        let output = Command::new("ssh-agent")
            .args([
                "-a",
                socket.to_str().context("non-UTF8 SSH socket path")?,
                "-s",
            ])
            .output()
            .context("ssh-agent is required for local jcode SSH connections")?;
        if !output.status.success() {
            bail!("could not start dedicated jbox SSH agent");
        }
        let text = String::from_utf8(output.stdout)?;
        let pid: u32 = text
            .split("SSH_AGENT_PID=")
            .nth(1)
            .and_then(|value| value.split(';').next())
            .context("ssh-agent did not report its process ID")?
            .parse()?;
        let status = Command::new("ssh-add")
            .arg(dir.join("id_ed25519"))
            .env("SSH_AUTH_SOCK", &socket)
            .status()?;
        if !status.success() {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            bail!("could not add the session key to the dedicated jbox SSH agent");
        }
        Ok(pid)
    }
    pub fn stop_session_ssh_agent(&self, pid: u32) {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
    /// Jcode's native SSH bridge uses the normal OpenSSH known-hosts database
    /// and exposes no per-connection known-hosts option. Trust only this
    /// loopback guest key, tag the line, and remove exactly that line at stop.
    /// The guest never receives the host's `.ssh` directory.
    pub fn trust_session_host(&self, host: &str, session_id: &str) -> Result<String> {
        let home = BaseDirs::new().context("could not determine home directory")?;
        let ssh_dir = home.home_dir().join(".ssh");
        fs::create_dir_all(&ssh_dir)?;
        let known_hosts = ssh_dir.join("known_hosts");
        let mut key = None;
        for _ in 0..25 {
            let output = Command::new("ssh-keyscan")
                .args(["-T", "1", "-t", "ed25519", host])
                .output()
                .context("ssh-keyscan is required to trust the loopback jbox guest")?;
            if !output.stdout.is_empty() {
                key = Some(output.stdout);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let key =
            key.context("could not obtain SSH host key for jbox guest within five seconds")?;
        let tag = format!("# jbox:{session_id}");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&known_hosts)?;
        for line in String::from_utf8(key)?.lines() {
            writeln!(file, "{line} {tag}")?;
        }
        Ok(tag)
    }
    pub fn untrust_session_host(&self, tag: &str) -> Result<()> {
        let home = BaseDirs::new().context("could not determine home directory")?;
        let known_hosts = home.home_dir().join(".ssh/known_hosts");
        if !known_hosts.exists() {
            return Ok(());
        }
        let text = fs::read_to_string(&known_hosts)?;
        let retained = text
            .lines()
            .filter(|line| !line.ends_with(tag))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&known_hosts, format!("{retained}\n"))?;
        Ok(())
    }
    pub fn write_ssh_config(&self, session: &Session) -> Result<PathBuf> {
        let dir = self.session_ssh_dir(&session.id);
        let file = dir.join("config");
        fs::write(
            &file,
            format!(
                "Host jbox\n  HostName {}\n  Port {}\n  User jbox\n  IdentityFile {}\n  IdentitiesOnly yes\n  StrictHostKeyChecking accept-new\n  UserKnownHostsFile {}\n  ForwardAgent no\n",
                session.ssh_host,
                session.ssh_port,
                dir.join("id_ed25519").display(),
                dir.join("known_hosts").display()
            ),
        )?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }
    pub fn write_ssh_wrapper(&self, session: &Session) -> Result<PathBuf> {
        let config = self.write_ssh_config(session)?;
        let file = config.parent().unwrap().join("ssh");
        fs::write(
            &file,
            format!(
                "#!/bin/sh\nexec /usr/bin/ssh -F {} \"$@\"\n",
                shell_quote(&config)
            ),
        )?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700))?;
        Ok(file)
    }
}

fn is_safe_credential_file(path: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(metadata.file_type().is_file()
        && !metadata.file_type().is_symlink()
        && metadata.len() <= CREDENTIAL_FILE_LIMIT)
}
fn shell_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}
pub fn safe_target(target: &str) -> Result<PathBuf> {
    let path = Path::new(target);
    if !path.is_absolute() || path == Path::new("/") {
        bail!("guest mount target must be an absolute non-root path");
    }
    for c in path.components() {
        if matches!(
            c,
            Component::ParentDir | Component::CurDir | Component::Prefix(_)
        ) {
            bail!("unsafe guest mount target {target}");
        }
    }
    Ok(path.to_path_buf())
}
pub fn validate_extra_mount(source: &Path, repos: &[PathBuf]) -> Result<()> {
    if sensitive(source) {
        bail!("refusing sensitive host mount: {}", source.display());
    }
    for repo in repos {
        if source.starts_with(repo) {
            bail!(
                "refusing extra mount {} because it exposes an original Git checkout {}",
                source.display(),
                repo.display()
            );
        }
    }
    Ok(())
}
fn sensitive(path: &Path) -> bool {
    let home = BaseDirs::new().map(|b| b.home_dir().to_path_buf());
    let blocked = [
        PathBuf::from("/"),
        PathBuf::from("/var/run/docker.sock"),
        PathBuf::from("/run/podman/podman.sock"),
        PathBuf::from("/var/run/podman/podman.sock"),
    ];
    if blocked.iter().any(|p| path == p) {
        return true;
    }
    if let Some(home) = home {
        let exact = [
            home.join(".ssh"),
            home.join(".aws"),
            home.join(".config"),
            home.join(".local/share/jbox"),
        ];
        if exact.iter().any(|p| path.starts_with(p)) {
            return true;
        }
    }
    path.to_string_lossy().contains("/run/user/")
        || path.to_string_lossy().contains("SSH_AUTH_SOCK")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_paths_are_strict() {
        assert!(safe_target("/workspace/project").is_ok());
        assert!(safe_target("/workspace/../etc").is_err());
        assert!(safe_target("relative").is_err());
    }

    #[test]
    fn imports_only_allowlisted_credential_files_without_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("host-home");
        let paths = JboxPaths {
            data: tmp.path().join("data"),
            cache: tmp.path().join("cache"),
            sessions: tmp.path().join("data/sessions"),
            credentials: tmp.path().join("data/credentials"),
        };
        fs::create_dir_all(home.join(".jcode")).unwrap();
        fs::create_dir_all(home.join(".config/jcode")).unwrap();
        fs::write(home.join(".jcode/auth.json"), "oauth-only").unwrap();
        fs::write(home.join(".config/jcode/openrouter.env"), "API_KEY=secret").unwrap();
        fs::write(home.join(".jcode/config.toml"), "unrelated = true").unwrap();

        let report = paths.import_local_credentials_from(&home, false).unwrap();
        assert_eq!(report.imported.len(), 2);
        let destination = paths.credentials.join("jcode/home/.jcode/auth.json");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "oauth-only");
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            !paths
                .credentials
                .join("jcode/home/.jcode/config.toml")
                .exists()
        );

        fs::write(home.join(".jcode/auth.json"), "new-oauth").unwrap();
        let report = paths.import_local_credentials_from(&home, false).unwrap();
        assert_eq!(report.imported.len(), 0);
        assert_eq!(report.retained.len(), 2);
        assert_eq!(fs::read_to_string(destination).unwrap(), "oauth-only");
    }
}
