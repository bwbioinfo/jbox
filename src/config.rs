use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct ResolvedRepository {
    pub source: PathBuf,
    pub mount: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub runtime: Runtime,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub image: Image,
    #[serde(default)]
    pub workspace: Workspace,
    #[serde(default)]
    pub repos: Vec<Repo>,
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub jcode: Jcode,
    #[serde(default)]
    pub git: Git,
    #[serde(default)]
    pub mounts: Vec<Mount>,
    #[serde(skip)]
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    #[serde(default = "kata")]
    pub backend: String,
}
fn kata() -> String {
    "kata".into()
}
impl Default for Runtime {
    fn default() -> Self {
        Self { backend: kata() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    #[serde(default = "cpus")]
    pub cpus: u16,
    #[serde(default = "memory")]
    pub memory: String,
    #[serde(default = "disk")]
    pub disk: String,
    #[serde(default = "ttl")]
    pub ttl: String,
    #[serde(skip)]
    pub ttl_seconds: i64,
}
fn cpus() -> u16 {
    8
}
fn memory() -> String {
    "16G".into()
}
fn disk() -> String {
    "40G".into()
}
fn ttl() -> String {
    "24h".into()
}
impl Default for Resources {
    fn default() -> Self {
        Self {
            cpus: cpus(),
            memory: memory(),
            disk: disk(),
            ttl: ttl(),
            ttl_seconds: 86_400,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub dockerfile: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    #[serde(default = "workspace_mount")]
    pub mount: String,
}
fn workspace_mount() -> String {
    "/workspace/project".into()
}
impl Default for Workspace {
    fn default() -> Self {
        Self {
            mount: workspace_mount(),
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    pub path: String,
    pub mount: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    #[serde(default = "yes")]
    pub internet: bool,
    #[serde(default)]
    pub host: bool,
    #[serde(default)]
    pub lan: bool,
}
fn yes() -> bool {
    true
}
impl Default for Network {
    fn default() -> Self {
        Self {
            internet: true,
            host: false,
            lan: false,
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Jcode {
    #[serde(default = "yes")]
    pub persistent_credentials: bool,
    #[serde(default)]
    pub skills: Option<Skills>,
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub openai_reasoning_effort: Option<String>,
    #[serde(default)]
    pub openai_service_tier: Option<String>,
}
impl Default for Jcode {
    fn default() -> Self {
        Self {
            persistent_credentials: true,
            skills: None,
            default_provider: None,
            default_model: None,
            openai_reasoning_effort: None,
            openai_service_tier: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skills {
    pub repository: String,
    #[serde(default = "default_skills_path")]
    pub path: String,
}

fn default_skills_path() -> String {
    "skills".into()
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Git {
    #[serde(default = "yes")]
    pub network: bool,
    #[serde(default = "git_credentials")]
    pub credentials: String,
    #[serde(default)]
    pub author: GitAuthor,
}
fn git_credentials() -> String {
    "jbox".into()
}
impl Default for Git {
    fn default() -> Self {
        Self {
            network: true,
            credentials: git_credentials(),
            author: GitAuthor::default(),
        }
    }
}

/// The guest's Git commit identity. By default each unset field is read from
/// the host's global Git configuration at session creation, never mounted into
/// the guest. Values in `.jbox.toml` take precedence per field.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitAuthor {
    #[serde(default = "yes")]
    pub inherit_host: bool,
    pub name: Option<String>,
    pub email: Option<String>,
}

impl Default for GitAuthor {
    fn default() -> Self {
        Self {
            inherit_host: true,
            name: None,
            email: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedGitAuthor {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl GitAuthor {
    pub fn resolve(&self) -> Result<ResolvedGitAuthor> {
        Ok(ResolvedGitAuthor {
            name: resolve_git_author_field(&self.name, self.inherit_host, "user.name")?,
            email: resolve_git_author_field(&self.email, self.inherit_host, "user.email")?,
        })
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mount {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub writable: bool,
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

impl Config {
    pub fn load(input: &Path) -> Result<(Self, PathBuf)> {
        let primary = Self::repository_root(input)?;
        let config_path = primary.join(".jbox.toml");
        let mut config = if config_path.exists() {
            let text = std::fs::read_to_string(&config_path).context("cannot read .jbox.toml")?;
            toml::from_str::<Config>(&text).context("invalid .jbox.toml")?
        } else {
            Config {
                version: 1,
                runtime: Runtime::default(),
                resources: Resources::default(),
                image: Image::default(),
                workspace: Workspace::default(),
                repos: Vec::new(),
                network: Network::default(),
                jcode: Jcode::default(),
                git: Git::default(),
                mounts: Vec::new(),
                path: PathBuf::new(),
            }
        };
        if config.version != 1 {
            bail!(
                "unsupported .jbox.toml version {}; only version = 1 is supported",
                config.version
            );
        }
        if config.runtime.backend != "kata" {
            bail!("runtime.backend must be `kata` for this MVP");
        }
        config.resources.ttl_seconds = parse_duration(&config.resources.ttl)?;
        if config.resources.cpus == 0 {
            bail!("resources.cpus must be greater than zero");
        }
        if config.resources.memory.trim().is_empty() || config.resources.disk.trim().is_empty() {
            bail!("resources.memory and resources.disk must be non-empty OCI quantities");
        }
        if !config.network.internet && config.git.network {
            bail!("git.network=true requires network.internet=true");
        }
        if !matches!(config.git.credentials.as_str(), "jbox" | "github-cli") {
            bail!("git.credentials must be `jbox` or `github-cli`");
        }
        if config.git.credentials == "github-cli" && !config.git.network {
            bail!("git.credentials=`github-cli` requires git.network=true");
        }
        if let Some(skills) = &config.jcode.skills {
            validate_skills(skills, config.network.internet)?;
        }
        for (name, value) in [
            ("jcode.default_provider", config.jcode.default_provider.as_deref()),
            ("jcode.default_model", config.jcode.default_model.as_deref()),
            (
                "jcode.openai_reasoning_effort",
                config.jcode.openai_reasoning_effort.as_deref(),
            ),
            (
                "jcode.openai_service_tier",
                config.jcode.openai_service_tier.as_deref(),
            ),
            ("git.author.name", config.git.author.name.as_deref()),
            ("git.author.email", config.git.author.email.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty() || value.contains('\0')) {
                bail!("{name} must be a non-empty string without NUL characters");
            }
        }
        if let Some(effort) = config.jcode.openai_reasoning_effort.as_deref()
            && !matches!(
                effort,
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            )
        {
            bail!(
                "jcode.openai_reasoning_effort must be one of none, minimal, low, medium, high, xhigh, or max"
            );
        }
        if let Some(tier) = config.jcode.openai_service_tier.as_deref()
            && !matches!(tier, "priority" | "flex" | "off")
        {
            bail!("jcode.openai_service_tier must be `priority`, `flex`, or `off`");
        }
        config.path = config_path;
        for mount in &mut config.mounts {
            let path = resolve_under(&primary, &mount.source)?;
            mount.source_path = Some(path);
        }
        Ok((config, primary))
    }

    /// Resolve the Git root that owns a path without requiring a valid jbox
    /// configuration. This is used by `jbox init`, which must work before a
    /// `.jbox.toml` exists.
    pub fn repository_root(input: &Path) -> Result<PathBuf> {
        let input = input
            .canonicalize()
            .with_context(|| format!("cannot resolve {}", input.display()))?;
        let dir = if input.is_dir() {
            input
        } else {
            input
                .parent()
                .context("configuration has no parent")?
                .to_path_buf()
        };
        git_root(&dir)
    }

    pub fn resolve_repositories(&self, primary: &Path) -> Result<Vec<ResolvedRepository>> {
        self.repos
            .iter()
            .map(|r| {
                let source = resolve_under(primary, &r.path)?;
                let name = source
                    .file_name()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .context("repository has no valid name")?
                    .to_owned();
                Ok(ResolvedRepository {
                    source,
                    mount: r.mount.clone(),
                    name,
                })
            })
            .collect()
    }

    pub fn validate_repositories(&self, repos: &[ResolvedRepository]) -> Result<()> {
        let mut sources = HashSet::new();
        let mut mounts = HashSet::new();
        for repo in repos {
            if !is_git_repository(&repo.source) {
                bail!("{} is not a Git repository", repo.source.display());
            }
            if !sources.insert(repo.source.clone()) {
                bail!(
                    "repository {} is declared more than once",
                    repo.source.display()
                );
            }
            validate_guest_path(&repo.mount)?;
            if !mounts.insert(repo.mount.clone()) {
                bail!("guest mount {} is declared more than once", repo.mount);
            }
        }
        let repo_sources = repos
            .iter()
            .map(|repo| repo.source.clone())
            .collect::<Vec<_>>();
        for mount in &self.mounts {
            validate_guest_path(&mount.target)?;
            if !mounts.insert(mount.target.clone()) {
                bail!(
                    "guest mount {} conflicts with a workspace mount",
                    mount.target
                );
            }
            crate::paths::validate_extra_mount(
                mount
                    .source_path
                    .as_deref()
                    .context("configured mount source was not resolved")?,
                &repo_sources,
            )?;
        }
        Ok(())
    }

    pub fn project_name(&self, primary: &Path) -> String {
        primary
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("project")
            .to_owned()
    }
}

fn resolve_under(base: &Path, raw: &str) -> Result<PathBuf> {
    let p = Path::new(raw);
    let candidate = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    candidate
        .canonicalize()
        .with_context(|| format!("cannot resolve configured path {raw}"))
}
fn git_root(dir: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .args([
            "-C",
            dir.to_str().context("non-UTF8 repository path")?,
            "rev-parse",
            "--show-toplevel",
        ])
        .output()
        .context("could not run git")?;
    if !out.status.success() {
        bail!("{} is not inside a Git repository", dir.display());
    }
    PathBuf::from(String::from_utf8(out.stdout)?.trim())
        .canonicalize()
        .context("cannot canonicalize Git root")
}
fn is_git_repository(path: &Path) -> bool {
    Command::new("git")
        .args([
            "-C",
            &path.to_string_lossy(),
            "rev-parse",
            "--is-inside-work-tree",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
fn validate_guest_path(path: &str) -> Result<()> {
    if !path.starts_with('/') || path == "/" || path.contains("..") || path.contains('\0') {
        bail!("mount target must be an absolute, non-root path without `..`: {path}");
    }
    Ok(())
}
fn validate_skills(skills: &Skills, internet: bool) -> Result<()> {
    if !internet {
        bail!("jcode.skills requires network.internet=true to clone its repository");
    }
    if !(skills.repository.starts_with("https://")
        || skills.repository.starts_with("ssh://")
        || skills.repository.starts_with("git@"))
    {
        bail!("jcode.skills.repository must use https://, ssh://, or git@ Git transport");
    }
    let path = Path::new(&skills.path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(component, std::path::Component::ParentDir | std::path::Component::RootDir)
        })
    {
        bail!("jcode.skills.path must be a non-empty relative path without `..`");
    }
    Ok(())
}

fn global_git_config(key: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["config", "--global", "--get", key])
        .output()
        .context("could not read host global Git configuration")?;
    if !output.status.success() {
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        bail!(
            "could not read host global Git configuration for {key}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let value = String::from_utf8(output.stdout)?;
    Ok((!value.trim().is_empty()).then(|| value.trim().to_owned()))
}

fn resolve_git_author_field(
    configured: &Option<String>,
    inherit_host: bool,
    key: &str,
) -> Result<Option<String>> {
    match configured {
        Some(value) => Ok(Some(value.clone())),
        None if inherit_host => global_git_config(key),
        None => Ok(None),
    }
}
fn parse_duration(input: &str) -> Result<i64> {
    let s = input.trim();
    let (n, unit) = s.chars().partition::<String, _>(|c| c.is_ascii_digit());
    let n: i64 = n.parse().context("TTL must start with an integer")?;
    let multiplier = match unit.as_str() {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => bail!("TTL must use s, m, h, or d, for example `24h`"),
    };
    if n <= 0 {
        bail!("TTL must be positive");
    }
    Ok(n * multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("24h").unwrap(), 86400);
        assert!(parse_duration("0h").is_err());
        assert!(parse_duration("24hours").is_err());
    }
    #[test]
    fn rejects_unsafe_targets() {
        assert!(validate_guest_path("relative").is_err());
        assert!(validate_guest_path("/workspace/../home").is_err());
        assert!(validate_guest_path("/workspace/project").is_ok());
    }

    #[test]
    fn validates_skills_repository_and_path() {
        let skills = Skills {
            repository: "https://github.com/example/skills.git".into(),
            path: "skills".into(),
        };
        assert!(validate_skills(&skills, true).is_ok());
        assert!(validate_skills(&skills, false).is_err());
        assert!(validate_skills(
            &Skills {
                repository: "file:///tmp/skills".into(),
                path: "skills".into(),
            },
            true,
        )
        .is_err());
        assert!(validate_skills(
            &Skills {
                repository: "https://github.com/example/skills.git".into(),
                path: "../skills".into(),
            },
            true,
        )
        .is_err());
    }

    #[test]
    fn github_cli_credentials_require_git_networking() {
        let temp = tempdir().unwrap();
        assert!(Command::new("git")
            .arg("init")
            .arg(temp.path())
            .status()
            .unwrap()
            .success());
        std::fs::write(
            temp.path().join(".jbox.toml"),
            "version = 1\n[network]\ninternet = false\n[git]\nnetwork = false\ncredentials = 'github-cli'\n",
        )
        .unwrap();
        assert!(Config::load(temp.path()).is_err());

        std::fs::write(
            temp.path().join(".jbox.toml"),
            "version = 1\n[git]\nnetwork = true\ncredentials = 'github-cli'\n",
        )
        .unwrap();
        assert!(Config::load(temp.path()).is_ok());
    }

    #[test]
    fn explicit_git_author_overrides_do_not_need_host_configuration() {
        let author = GitAuthor {
            inherit_host: false,
            name: Some("Jbox Test".into()),
            email: Some("test@example.com".into()),
        };
        assert_eq!(
            author.resolve().unwrap(),
            ResolvedGitAuthor {
                name: Some("Jbox Test".into()),
                email: Some("test@example.com".into()),
            }
        );
    }

    #[test]
    fn rejects_extra_mount_of_original_checkout() {
        let temp = tempdir().unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .arg(temp.path())
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(
            temp.path().join(".jbox.toml"),
            "version = 1\n[[mounts]]\nsource = '.'\ntarget = '/data'\n",
        )
        .unwrap();
        let (config, primary) = Config::load(temp.path()).unwrap();
        let repos = vec![ResolvedRepository {
            source: primary.clone(),
            mount: "/workspace/project".into(),
            name: "project".into(),
        }];
        assert!(config.validate_repositories(&repos).is_err());
    }
}
