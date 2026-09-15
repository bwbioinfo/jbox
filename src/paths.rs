use crate::state::Session;
use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

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
        let public = fs::read_to_string(key.with_extension("ed25519.pub"))?;
        fs::write(dir.join("authorized_keys"), public)?;
        fs::set_permissions(
            dir.join("authorized_keys"),
            fs::Permissions::from_mode(0o600),
        )?;
        Ok(())
    }
    pub fn write_ssh_config(&self, session: &Session) -> Result<PathBuf> {
        let dir = self.session_ssh_dir(&session.id);
        let file = dir.join("config");
        fs::write(
            &file,
            format!(
                "Host jbox\n  HostName 127.0.0.1\n  Port {}\n  User jbox\n  IdentityFile {}\n  IdentitiesOnly yes\n  StrictHostKeyChecking accept-new\n  UserKnownHostsFile {}\n  ForwardAgent no\n",
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
}
