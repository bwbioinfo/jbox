use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
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
    #[serde(default, deserialize_with = "deserialize_skill_sources")]
    pub skills: Vec<SkillSource>,
    #[serde(default)]
    pub agent: Agent,
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
            skills: Vec::new(),
            agent: Agent::default(),
            default_provider: None,
            default_model: None,
            openai_reasoning_effort: None,
            openai_service_tier: None,
        }
    }
}

/// Session-wide guidance rendered to the guest's global `~/AGENTS.md`.
/// Jcode loads this after the project AGENTS.md, without changing a worktree.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillSource {
    pub repository: String,
    /// When omitted, install every discoverable skill from the repository.
    pub skill: Option<String>,
    pub pin: Option<String>,
    #[serde(default)]
    pub allow_hidden_dirs: bool,
    /// Private skill sources must have an explicit legacy GitHub CLI mode or a
    /// scoped guest-passthrough clone grant. Public sources need no token.
    #[serde(default)]
    pub private: bool,
}

fn deserialize_skill_sources<'de, D>(deserializer: D) -> Result<Vec<SkillSource>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Sources {
        One(SkillSource),
        Many(Vec<SkillSource>),
    }

    Ok(match Sources::deserialize(deserializer)? {
        Sources::One(source) => vec![source],
        Sources::Many(sources) => sources,
    })
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
    /// Named GitHub identities and their expected, externally-enforced
    /// capabilities. Credentials are imported into Jbox-owned state in a later
    /// lifecycle step. Keeping these declarations separate from grants prevents
    /// a repository from silently selecting an arbitrary host account.
    #[serde(default)]
    pub credential_profiles: Vec<GitCredentialProfile>,
    /// Exact GitHub repository grants. A grant describes Jbox policy, not a
    /// substitute for the permissions embedded in the credential itself.
    #[serde(default)]
    pub repository_grants: Vec<GitRepositoryGrant>,
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
            credential_profiles: Vec::new(),
            repository_grants: Vec::new(),
        }
    }
}

/// Ordered so a grant can be checked against its credential profile's declared
/// minimum capability without stringly typed comparisons.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum GitCapability {
    #[default]
    None,
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GitCredentialDelivery {
    /// The credential remains on the host and future Jbox control-plane
    /// commands mediate all use of it.
    #[default]
    Brokered,
    /// An explicitly selected profile is available to guest programs. The
    /// actual token must be least-privilege because configuration alone cannot
    /// downscope a token a guest can read.
    GuestPassthrough,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GitMergePolicy {
    #[default]
    Deny,
    UserConfirmed,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum GitRepositoryOperation {
    Clone,
    Fetch,
    PushBranch,
    PrView,
    PrStatus,
    CiView,
    PrCreate,
    PrUpdate,
}

impl GitRepositoryOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clone => "clone",
            Self::Fetch => "fetch",
            Self::PushBranch => "push-branch",
            Self::PrView => "pr-view",
            Self::PrStatus => "pr-status",
            Self::CiView => "ci-view",
            Self::PrCreate => "pr-create",
            Self::PrUpdate => "pr-update",
        }
    }

    pub fn parse_name(value: &str) -> Result<Self> {
        Ok(match value {
            "clone" => Self::Clone,
            "fetch" => Self::Fetch,
            "push-branch" => Self::PushBranch,
            "pr-view" => Self::PrView,
            "pr-status" => Self::PrStatus,
            "ci-view" => Self::CiView,
            "pr-create" => Self::PrCreate,
            "pr-update" => Self::PrUpdate,
            _ => bail!(
                "unknown Git repository operation `{value}`; use clone, fetch, push-branch, pr-view, pr-status, ci-view, pr-create, or pr-update"
            ),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitCredentialProfile {
    pub name: String,
    #[serde(default = "github_cli_provider")]
    pub provider: String,
    #[serde(default = "github_com_host")]
    pub host: String,
    pub account: String,
    #[serde(default = "jbox_managed_storage")]
    pub storage: String,
    #[serde(default)]
    pub expected_contents: GitCapability,
    #[serde(default)]
    pub expected_pull_requests: GitCapability,
    #[serde(default)]
    pub expected_actions: GitCapability,
}

fn github_cli_provider() -> String {
    "github-cli".into()
}

fn github_com_host() -> String {
    "github.com".into()
}

fn jbox_managed_storage() -> String {
    "jbox-managed".into()
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitRepositoryGrant {
    pub id: String,
    /// An exact GitHub OWNER/REPO identifier. The associated profile supplies
    /// the host, avoiding URL parsing and credential selection based on a
    /// guest-controlled remote string.
    pub repository: String,
    /// A local Git remote, when this grant is used by a checked-out repository.
    /// Skills grants deliberately omit it because they clone by repository ID.
    pub remote: Option<String>,
    pub credential_profile: String,
    #[serde(default)]
    pub delivery: GitCredentialDelivery,
    #[serde(default)]
    pub git: GitCapability,
    #[serde(default)]
    pub pull_requests: GitCapability,
    #[serde(default)]
    pub actions: GitCapability,
    #[serde(default)]
    pub checks: GitCapability,
    pub allowed_operations: Vec<GitRepositoryOperation>,
    #[serde(default)]
    pub allowed_push_ref_prefixes: Vec<String>,
    #[serde(default)]
    pub protected_refs: Vec<String>,
    #[serde(default)]
    pub force_push: bool,
    #[serde(default)]
    pub merge: GitMergePolicy,
}

/// The canonical repository-access policy captured at session creation. It is
/// intentionally made from declarative data only, never credentials. A resume
/// compares this policy with the current configuration rather than allowing a
/// guest-edited `.jbox.toml` to broaden a retained session's authority.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ResolvedGitAccessPolicy {
    pub version: u32,
    pub digest: String,
    pub credential_profiles: Vec<GitCredentialProfile>,
    pub repository_grants: Vec<GitRepositoryGrant>,
}

impl Git {
    pub fn resolved_access_policy(&self) -> Result<Option<ResolvedGitAccessPolicy>> {
        validate_git_access(self)?;
        if self.credential_profiles.is_empty() {
            return Ok(None);
        }
        let mut credential_profiles = self.credential_profiles.clone();
        credential_profiles.sort_by(|left, right| left.name.cmp(&right.name));
        let mut repository_grants = self.repository_grants.clone();
        repository_grants.sort_by(|left, right| left.id.cmp(&right.id));
        let canonical = serde_json::to_vec(&(&credential_profiles, &repository_grants))
            .context("could not serialize Git access policy")?;
        let digest = format!("{:x}", Sha256::digest(canonical));
        Ok(Some(ResolvedGitAccessPolicy {
            version: 1,
            digest,
            credential_profiles,
            repository_grants,
        }))
    }

    /// Return the one GitHub CLI profile that may enter the guest. A guest can
    /// use every capability embedded in a mounted token, so the configuration
    /// validator permits only one explicitly selected profile per session.
    pub fn guest_passthrough_profile(&self) -> Result<Option<&GitCredentialProfile>> {
        validate_git_access(self)?;
        let Some(profile_name) = self
            .repository_grants
            .iter()
            .find(|grant| grant.delivery == GitCredentialDelivery::GuestPassthrough)
            .map(|grant| grant.credential_profile.as_str())
        else {
            return Ok(None);
        };
        Ok(self
            .credential_profiles
            .iter()
            .find(|profile| profile.name == profile_name))
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
        if !matches!(
            config.git.credentials.as_str(),
            "jbox" | "github-cli" | "none"
        ) {
            bail!("git.credentials must be `jbox`, `github-cli`, or `none`");
        }
        if config.git.credentials == "github-cli" && !config.git.network {
            bail!("git.credentials=`github-cli` requires git.network=true");
        }
        validate_git_access(&config.git)?;
        if !config.jcode.skills.is_empty() {
            validate_skills(&config.jcode.skills, &config.network, &config.git)?;
        }
        if let Some(instructions) = config.jcode.agent.instructions.as_deref()
            && (instructions.trim().is_empty()
                || instructions.contains('\0')
                || instructions.len() > 65_536)
        {
            bail!("jcode.agent.instructions must be non-empty, NUL-free, and at most 65536 bytes");
        }
        for (name, value) in [
            (
                "jcode.default_provider",
                config.jcode.default_provider.as_deref(),
            ),
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
fn validate_skills(skills: &[SkillSource], network: &Network, git: &Git) -> Result<()> {
    if !network.internet {
        bail!("jcode.skills requires network.internet=true for `gh skill install`");
    }
    for source in skills {
        if !valid_github_repository(&source.repository) {
            bail!(
                "jcode.skills.repository must be a GitHub OWNER/REPO identifier: {}",
                source.repository
            );
        }
        for (field, value) in [
            ("skill", source.skill.as_deref()),
            ("pin", source.pin.as_deref()),
        ] {
            if let Some(value) = value
                && (value.trim().is_empty()
                    || value.starts_with('-')
                    || value.contains('\0')
                    || value.contains('\n')
                    || value.contains('\r'))
            {
                bail!(
                    "jcode.skills.{field} must be a non-empty, non-option argument without line breaks"
                );
            }
        }
        if source.private {
            if !git.network {
                bail!(
                    "private jcode.skills.repository `{}` requires git.network=true",
                    source.repository
                );
            }
            if git.credential_profiles.is_empty() {
                if git.credentials != "github-cli" {
                    bail!(
                        "private jcode.skills.repository `{}` requires git.credentials=`github-cli`, or a repository-scoped guest-passthrough grant",
                        source.repository
                    );
                }
            } else if !git.repository_grants.iter().any(|grant| {
                grant.repository == source.repository
                    && grant.delivery == GitCredentialDelivery::GuestPassthrough
                    && grant.git >= GitCapability::Read
                    && grant
                        .allowed_operations
                        .contains(&GitRepositoryOperation::Clone)
            }) {
                bail!(
                    "private jcode.skills.repository `{}` requires a guest-passthrough repository grant with git = `read` and allowed_operations including `clone`",
                    source.repository
                );
            }
        }
    }
    Ok(())
}

/// Validate the declarative GitHub policy before any credential is imported or
/// mounted. This deliberately checks only configuration consistency. The
/// credential import and broker layers must separately verify the account and
/// the actual privileges granted by GitHub.
fn validate_git_access(git: &Git) -> Result<()> {
    if git.credential_profiles.is_empty() {
        if !git.repository_grants.is_empty() {
            bail!("git.repository_grants requires at least one git.credential_profiles entry");
        }
        return Ok(());
    }
    if git.credentials != "none" {
        bail!(
            "git.credential_profiles requires git.credentials = `none` so legacy host Git credentials cannot bypass repository-scoped policy"
        );
    }

    let mut profiles = HashMap::new();
    for profile in &git.credential_profiles {
        if !valid_config_identifier(&profile.name) {
            bail!(
                "git.credential_profiles.name must contain only letters, digits, `_`, `-`, or `.`: {}",
                profile.name
            );
        }
        if profiles.insert(profile.name.as_str(), profile).is_some() {
            bail!("duplicate git credential profile `{}`", profile.name);
        }
        if profile.provider != "github-cli" {
            bail!(
                "git credential profile `{}` has unsupported provider `{}`; only `github-cli` is currently supported",
                profile.name,
                profile.provider
            );
        }
        if profile.host != "github.com" {
            bail!(
                "git credential profile `{}` currently supports only host `github.com`, not `{}`",
                profile.name,
                profile.host
            );
        }
        if !valid_github_account(&profile.account) {
            bail!(
                "git credential profile `{}` has invalid GitHub account `{}`",
                profile.name,
                profile.account
            );
        }
        if profile.storage != "jbox-managed" {
            bail!(
                "git credential profile `{}` must use storage = `jbox-managed`",
                profile.name
            );
        }
    }

    let mut grant_ids = HashSet::new();
    let mut guest_profiles = HashSet::new();
    for grant in &git.repository_grants {
        if !valid_config_identifier(&grant.id) {
            bail!(
                "git repository grant id must contain only letters, digits, `_`, `-`, or `.`: {}",
                grant.id
            );
        }
        if !grant_ids.insert(&grant.id) {
            bail!("duplicate git repository grant `{}`", grant.id);
        }
        if !valid_github_repository(&grant.repository) {
            bail!(
                "git repository grant `{}` must use an exact GitHub OWNER/REPO identifier: {}",
                grant.id,
                grant.repository
            );
        }
        if let Some(remote) = &grant.remote
            && !valid_config_identifier(remote)
        {
            bail!(
                "git repository grant `{}` has invalid remote `{remote}`",
                grant.id
            );
        }
        let profile = profiles
            .get(grant.credential_profile.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "git repository grant `{}` refers to unknown credential profile `{}`",
                    grant.id,
                    grant.credential_profile
                )
            })?;
        if grant.delivery == GitCredentialDelivery::GuestPassthrough {
            guest_profiles.insert(profile.name.as_str());
        }
        if grant.git > profile.expected_contents {
            bail!(
                "git repository grant `{}` requires {} Git contents access, but profile `{}` declares {}",
                grant.id,
                capability_name(grant.git),
                profile.name,
                capability_name(profile.expected_contents)
            );
        }
        if grant.pull_requests > profile.expected_pull_requests {
            bail!(
                "git repository grant `{}` requires {} pull-request access, but profile `{}` declares {}",
                grant.id,
                capability_name(grant.pull_requests),
                profile.name,
                capability_name(profile.expected_pull_requests)
            );
        }
        if grant.actions > profile.expected_actions || grant.checks > profile.expected_actions {
            bail!(
                "git repository grant `{}` requires Actions or checks access beyond profile `{}`'s declared Actions capability",
                grant.id,
                profile.name
            );
        }
        validate_grant_operations(grant)?;
        if grant.merge == GitMergePolicy::UserConfirmed {
            if grant.delivery != GitCredentialDelivery::Brokered {
                bail!(
                    "git repository grant `{}` may allow merge only with delivery = `brokered`",
                    grant.id
                );
            }
            if grant.pull_requests != GitCapability::Write {
                bail!(
                    "git repository grant `{}` requires pull_requests = `write` for user-confirmed merge",
                    grant.id
                );
            }
        }
    }
    if guest_profiles.len() > 1 {
        bail!(
            "only one credential profile may use delivery = `guest-passthrough` in a session; use one least-privilege account or keep additional grants brokered"
        );
    }
    Ok(())
}

fn validate_grant_operations(grant: &GitRepositoryGrant) -> Result<()> {
    if grant.allowed_operations.is_empty() {
        bail!(
            "git repository grant `{}` must declare at least one allowed operation",
            grant.id
        );
    }
    let mut operations = HashSet::new();
    for operation in &grant.allowed_operations {
        if !operations.insert(*operation) {
            bail!(
                "git repository grant `{}` repeats operation `{}`",
                grant.id,
                operation_name(*operation)
            );
        }
        match operation {
            GitRepositoryOperation::Clone | GitRepositoryOperation::Fetch
                if grant.git < GitCapability::Read =>
            {
                bail!(
                    "git repository grant `{}` requires git = `read` for `{}`",
                    grant.id,
                    operation_name(*operation)
                );
            }
            GitRepositoryOperation::PushBranch if grant.git != GitCapability::Write => {
                bail!(
                    "git repository grant `{}` requires git = `write` for `push-branch`",
                    grant.id
                );
            }
            GitRepositoryOperation::PrView | GitRepositoryOperation::PrStatus
                if grant.pull_requests < GitCapability::Read =>
            {
                bail!(
                    "git repository grant `{}` requires pull_requests = `read` for `{}`",
                    grant.id,
                    operation_name(*operation)
                );
            }
            GitRepositoryOperation::CiView
                if grant.actions < GitCapability::Read && grant.checks < GitCapability::Read =>
            {
                bail!(
                    "git repository grant `{}` requires actions or checks = `read` for `ci-view`",
                    grant.id
                );
            }
            GitRepositoryOperation::PrCreate | GitRepositoryOperation::PrUpdate
                if grant.pull_requests != GitCapability::Write =>
            {
                bail!(
                    "git repository grant `{}` requires pull_requests = `write` for `{}`",
                    grant.id,
                    operation_name(*operation)
                );
            }
            _ => {}
        }
    }

    let allows_push = operations.contains(&GitRepositoryOperation::PushBranch);
    if allows_push && grant.allowed_push_ref_prefixes.is_empty() {
        bail!(
            "git repository grant `{}` must restrict push-branch with allowed_push_ref_prefixes",
            grant.id
        );
    }
    if grant.force_push && !allows_push {
        bail!(
            "git repository grant `{}` cannot allow force_push without `push-branch`",
            grant.id
        );
    }
    for reference in grant
        .allowed_push_ref_prefixes
        .iter()
        .chain(grant.protected_refs.iter())
    {
        if !valid_ref_rule(reference) {
            bail!(
                "git repository grant `{}` has invalid reference rule `{reference}`",
                grant.id
            );
        }
    }
    Ok(())
}

fn valid_config_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_github_account(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_ref_rule(value: &str) -> bool {
    value.starts_with("refs/")
        && !value.contains('\0')
        && !value.contains("..")
        && !value.contains("//")
        && !value.contains("@{")
        && !value.ends_with('.')
        && !value.ends_with(".lock")
        && value.bytes().all(|byte| {
            !byte.is_ascii_whitespace()
                && !matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
}

fn capability_name(capability: GitCapability) -> &'static str {
    match capability {
        GitCapability::None => "none",
        GitCapability::Read => "read",
        GitCapability::Write => "write",
    }
}

fn operation_name(operation: GitRepositoryOperation) -> &'static str {
    operation.as_str()
}

fn valid_github_repository(repository: &str) -> bool {
    let mut parts = repository.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(name) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !owner.is_empty()
        && !name.is_empty()
        && [owner, name].into_iter().all(|part| {
            part.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
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
    fn validates_multiple_github_skill_sources() {
        let skills = vec![
            SkillSource {
                repository: "bwbioinfo/skills".into(),
                skill: None,
                pin: None,
                allow_hidden_dirs: false,
                private: true,
            },
            SkillSource {
                repository: "K-Dense-AI/scientific-agent-skills".into(),
                skill: Some("scanpy".into()),
                pin: Some("v1.2.3".into()),
                allow_hidden_dirs: true,
                private: false,
            },
        ];
        let network = Network::default();
        let git = Git {
            credentials: "github-cli".into(),
            ..Git::default()
        };
        assert!(validate_skills(&skills, &network, &git).is_ok());
        assert!(
            validate_skills(
                &skills,
                &Network {
                    internet: false,
                    ..Network::default()
                },
                &git
            )
            .is_err()
        );
        assert!(
            validate_skills(
                &[SkillSource {
                    repository: "file:///tmp/skills".into(),
                    skill: None,
                    pin: None,
                    allow_hidden_dirs: false,
                    private: false,
                }],
                &network,
                &git,
            )
            .is_err()
        );
        assert!(
            validate_skills(
                &[SkillSource {
                    repository: "bwbioinfo/skills".into(),
                    skill: Some("--all".into()),
                    pin: None,
                    allow_hidden_dirs: false,
                    private: true,
                }],
                &network,
                &git,
            )
            .is_err()
        );
        assert!(validate_skills(&skills, &network, &Git::default()).is_err());
    }

    #[test]
    fn parses_single_and_multiple_skills_blocks() {
        let single: Jcode = toml::from_str(
            "persistent_credentials = true\n[skills]\nrepository = 'bwbioinfo/skills'\nskill = 'scanpy'\n",
        )
        .unwrap();
        assert_eq!(single.skills.len(), 1);
        assert_eq!(single.skills[0].skill.as_deref(), Some("scanpy"));

        let multiple: Jcode = toml::from_str(
            "persistent_credentials = true\n[[skills]]\nrepository = 'bwbioinfo/skills'\n[[skills]]\nrepository = 'K-Dense-AI/scientific-agent-skills'\nskill = 'scanpy'\n",
        )
        .unwrap();
        assert_eq!(multiple.skills.len(), 2);
    }

    #[test]
    fn github_cli_credentials_require_git_networking() {
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

    fn profile(
        name: &str,
        contents: GitCapability,
        pull_requests: GitCapability,
    ) -> GitCredentialProfile {
        GitCredentialProfile {
            name: name.into(),
            provider: "github-cli".into(),
            host: "github.com".into(),
            account: "jbox-project-bot".into(),
            storage: "jbox-managed".into(),
            expected_contents: contents,
            expected_pull_requests: pull_requests,
            expected_actions: GitCapability::Read,
        }
    }

    fn grant(profile: &str) -> GitRepositoryGrant {
        GitRepositoryGrant {
            id: "project-maintainer".into(),
            repository: "bwbioinfo/jbox".into(),
            remote: Some("origin".into()),
            credential_profile: profile.into(),
            delivery: GitCredentialDelivery::Brokered,
            git: GitCapability::Write,
            pull_requests: GitCapability::Write,
            actions: GitCapability::Read,
            checks: GitCapability::Read,
            allowed_operations: vec![
                GitRepositoryOperation::Fetch,
                GitRepositoryOperation::PushBranch,
                GitRepositoryOperation::PrCreate,
                GitRepositoryOperation::PrUpdate,
                GitRepositoryOperation::CiView,
            ],
            allowed_push_ref_prefixes: vec!["refs/heads/jbox/".into()],
            protected_refs: vec!["refs/heads/main".into()],
            force_push: false,
            merge: GitMergePolicy::UserConfirmed,
        }
    }

    #[test]
    fn validates_typed_repository_scoped_git_access() {
        let git = Git {
            credentials: "none".into(),
            credential_profiles: vec![profile(
                "project-maintainer",
                GitCapability::Write,
                GitCapability::Write,
            )],
            repository_grants: vec![grant("project-maintainer")],
            ..Git::default()
        };
        assert!(validate_git_access(&git).is_ok());

        let parsed: Git = toml::from_str(
            r#"
network = true
credentials = "none"

[[credential_profiles]]
name = "skills-reader"
account = "jbox-skills-bot"
expected_contents = "read"

[[repository_grants]]
id = "skills-fetch"
repository = "bwbioinfo/skills"
credential_profile = "skills-reader"
delivery = "guest-passthrough"
git = "read"
allowed_operations = ["clone", "fetch"]
"#,
        )
        .unwrap();
        assert!(validate_git_access(&parsed).is_ok());
        assert_eq!(
            parsed.repository_grants[0].delivery,
            GitCredentialDelivery::GuestPassthrough
        );
        assert_eq!(
            parsed.guest_passthrough_profile().unwrap().unwrap().account,
            "jbox-skills-bot"
        );
    }

    #[test]
    fn scoped_skills_require_a_guest_clone_grant() {
        let skills = vec![SkillSource {
            repository: "bwbioinfo/skills".into(),
            skill: None,
            pin: None,
            allow_hidden_dirs: false,
            private: true,
        }];
        let mut skills_grant = grant("skills-reader");
        skills_grant.id = "skills-fetch".into();
        skills_grant.repository = "bwbioinfo/skills".into();
        skills_grant.remote = None;
        skills_grant.delivery = GitCredentialDelivery::GuestPassthrough;
        skills_grant.git = GitCapability::Read;
        skills_grant.pull_requests = GitCapability::None;
        skills_grant.actions = GitCapability::None;
        skills_grant.checks = GitCapability::None;
        skills_grant.allowed_operations = vec![GitRepositoryOperation::Clone];
        skills_grant.allowed_push_ref_prefixes.clear();
        skills_grant.protected_refs.clear();
        skills_grant.merge = GitMergePolicy::Deny;
        let git = Git {
            credentials: "none".into(),
            credential_profiles: vec![profile(
                "skills-reader",
                GitCapability::Read,
                GitCapability::None,
            )],
            repository_grants: vec![skills_grant],
            ..Git::default()
        };
        assert!(validate_skills(&skills, &Network::default(), &git).is_ok());

        let mut missing_clone = git.clone();
        missing_clone.repository_grants[0].allowed_operations = vec![GitRepositoryOperation::Fetch];
        assert!(validate_skills(&skills, &Network::default(), &missing_clone).is_err());
    }

    #[test]
    fn public_skills_do_not_require_guest_git_credentials() {
        let public_skill = SkillSource {
            repository: "K-Dense-AI/scientific-agent-skills".into(),
            skill: Some("scanpy".into()),
            pin: None,
            allow_hidden_dirs: false,
            private: false,
        };
        let git = Git {
            network: false,
            credentials: "none".into(),
            ..Git::default()
        };
        assert!(validate_skills(&[public_skill], &Network::default(), &git).is_ok());
    }

    #[test]
    fn rejects_more_than_one_guest_passthrough_profile() {
        let mut first = grant("one");
        first.delivery = GitCredentialDelivery::GuestPassthrough;
        first.merge = GitMergePolicy::Deny;
        let mut second = grant("two");
        second.id = "second".into();
        second.repository = "bwbioinfo/skills".into();
        second.remote = None;
        second.delivery = GitCredentialDelivery::GuestPassthrough;
        second.merge = GitMergePolicy::Deny;
        let git = Git {
            credentials: "none".into(),
            credential_profiles: vec![
                profile("one", GitCapability::Write, GitCapability::Write),
                profile("two", GitCapability::Write, GitCapability::Write),
            ],
            repository_grants: vec![first, second],
            ..Git::default()
        };
        assert!(validate_git_access(&git).is_err());
    }

    #[test]
    fn rejects_inconsistent_repository_scoped_git_access() {
        let read_only = Git {
            credentials: "none".into(),
            credential_profiles: vec![profile(
                "observer",
                GitCapability::Read,
                GitCapability::Read,
            )],
            repository_grants: vec![grant("observer")],
            ..Git::default()
        };
        assert!(validate_git_access(&read_only).is_err());

        let mut missing_push_rule = grant("writer");
        missing_push_rule.allowed_push_ref_prefixes.clear();
        let missing_push_rule = Git {
            credentials: "none".into(),
            credential_profiles: vec![profile(
                "writer",
                GitCapability::Write,
                GitCapability::Write,
            )],
            repository_grants: vec![missing_push_rule],
            ..Git::default()
        };
        assert!(validate_git_access(&missing_push_rule).is_err());

        let mut guest_merge = grant("writer");
        guest_merge.delivery = GitCredentialDelivery::GuestPassthrough;
        let guest_merge = Git {
            credentials: "none".into(),
            credential_profiles: vec![profile(
                "writer",
                GitCapability::Write,
                GitCapability::Write,
            )],
            repository_grants: vec![guest_merge],
            ..Git::default()
        };
        assert!(validate_git_access(&guest_merge).is_err());
    }

    #[test]
    fn resolved_git_access_policy_is_canonical_and_detects_changes() {
        let git = Git {
            credentials: "none".into(),
            credential_profiles: vec![profile(
                "project-maintainer",
                GitCapability::Write,
                GitCapability::Write,
            )],
            repository_grants: vec![grant("project-maintainer")],
            ..Git::default()
        };
        let first = git.resolved_access_policy().unwrap().unwrap();
        let second = git.resolved_access_policy().unwrap().unwrap();
        assert_eq!(first, second);
        assert_eq!(first.digest.len(), 64);

        let mut changed = git.clone();
        changed.repository_grants[0].force_push = true;
        let changed = changed.resolved_access_policy().unwrap().unwrap();
        assert_ne!(first.digest, changed.digest);
        assert!(Git::default().resolved_access_policy().unwrap().is_none());
    }

    #[test]
    fn repository_scoped_git_access_rejects_unknown_toml_fields() {
        let parsed = toml::from_str::<Git>(
            r#"
network = true
credentials = "jbox"

[[credential_profiles]]
name = "skills-reader"
account = "jbox-skills-bot"
unexpected = true
"#,
        );
        assert!(parsed.is_err());
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

    fn init_test_repository(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q", path.to_str().unwrap()])
                .status()
                .unwrap()
                .success()
        );
    }

    fn create_primary_and_original_repositories(temp: &tempfile::TempDir) -> PathBuf {
        let primary = temp.path().join("primary");
        init_test_repository(&primary);
        init_test_repository(&temp.path().join("original"));
        primary
    }

    fn write_config_with_declared_original_repository(
        primary: &Path,
        mount_source: &str,
        writable: bool,
    ) {
        std::fs::write(
            primary.join(".jbox.toml"),
            format!(
                "version = 1\n[[repos]]\npath = '../original'\nmount = '/workspace/original'\n[[mounts]]\nsource = '{mount_source}'\ntarget = '/data'\nwritable = {writable}\n"
            ),
        )
        .unwrap();
    }

    fn resolved_repositories_with_primary(
        config: &Config,
        primary: &Path,
    ) -> Vec<ResolvedRepository> {
        let mut repos = vec![ResolvedRepository {
            source: primary.to_path_buf(),
            mount: "/workspace/project".into(),
            name: "primary".into(),
        }];
        repos.extend(config.resolve_repositories(primary).unwrap());
        repos
    }

    #[test]
    fn rejects_extra_mount_ancestor_of_declared_original_repository() {
        let temp = tempfile::tempdir_in("/var/tmp").unwrap();
        let primary = create_primary_and_original_repositories(&temp);
        for writable in [false, true] {
            write_config_with_declared_original_repository(&primary, "..", writable);
            let (config, resolved_primary) = Config::load(&primary).unwrap();
            let repos = resolved_repositories_with_primary(&config, &resolved_primary);

            assert!(config.validate_repositories(&repos).is_err());
        }
    }

    #[test]
    fn rejects_symlink_alias_of_extra_mount_ancestor_of_declared_original_repository() {
        let temp = tempfile::tempdir_in("/var/tmp").unwrap();
        let primary = create_primary_and_original_repositories(&temp);
        std::os::unix::fs::symlink("..", primary.join("ancestor-alias")).unwrap();
        write_config_with_declared_original_repository(&primary, "ancestor-alias", false);
        let (config, primary) = Config::load(&primary).unwrap();
        let repos = resolved_repositories_with_primary(&config, &primary);

        assert!(config.validate_repositories(&repos).is_err());
    }

    #[test]
    fn allows_disjoint_extra_mount_alongside_declared_original_repository() {
        let temp = tempfile::tempdir_in("/var/tmp").unwrap();
        let primary = create_primary_and_original_repositories(&temp);
        std::fs::create_dir_all(temp.path().join("safe")).unwrap();
        write_config_with_declared_original_repository(&primary, "../safe", false);
        let (config, primary) = Config::load(&primary).unwrap();
        let repos = resolved_repositories_with_primary(&config, &primary);

        assert!(config.validate_repositories(&repos).is_ok());
    }
}
