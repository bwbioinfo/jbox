use crate::paths::JboxPaths;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use rand::prelude::IndexedRandom;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const ADJECTIVES: &[&str] = &[
    "agile", "amber", "brisk", "bright", "calm", "clever", "cobalt", "cosmic", "crisp", "daring",
    "dawn", "eager", "ember", "fierce", "gentle", "golden", "grand", "harbor", "hidden", "indigo",
    "jade", "keen", "kind", "lively", "lunar", "mellow", "misty", "nimble", "nova", "oaken",
    "peaceful", "plucky", "proud", "quick", "quiet", "rapid", "ruby", "sage", "silver", "solar",
    "steady", "swift", "tidy", "vivid", "warm", "wild", "wise", "zesty",
];

const ANIMALS: &[&str] = &[
    "alpaca", "badger", "beaver", "bison", "caribou", "cat", "crane", "dolphin", "falcon",
    "ferret", "fox", "gecko", "hare", "hawk", "hedgehog", "heron", "ibis", "jaguar", "kite",
    "koala", "lemur", "leopard", "lizard", "lynx", "marten", "mink", "otter", "owl", "panda",
    "penguin", "puma", "quail", "raccoon", "raven", "seal", "shark", "sloth", "sparrow", "stoat",
    "swan", "tiger", "toucan", "turtle", "viper", "walrus", "weasel", "wolf", "wombat", "yak",
    "zebra",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionState {
    Running,
    Stopped,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoState {
    pub name: String,
    pub source: PathBuf,
    pub worktree: PathBuf,
    pub mount: String,
    pub branch: String,
    pub base_commit: String,
    pub host_gitfile: PathBuf,
    /// Digest of the portable Beads JSONL snapshot that jbox itself placed in
    /// this worktree. This lets lifecycle commands distinguish task context
    /// seeded by jbox from an agent's later edit to that export.
    #[serde(default)]
    pub beads_snapshot: Option<String>,
    /// Exact files that the Beads bootstrap changed before jbox exposed the
    /// guest. This is host-recorded after readiness, never guest-provided.
    #[serde(default)]
    pub beads_bootstrap: Vec<BeadsBaselineFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeadsBaselineFile {
    pub path: String,
    pub digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub id: String,
    pub state: SessionState,
    pub container_name: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub ssh_agent_pid: u32,
    pub known_hosts_tag: String,
    pub created_at: DateTime<Utc>,
    pub last_activity_at: DateTime<Utc>,
    pub ttl_seconds: i64,
    pub config_path: PathBuf,
    pub image: String,
    pub repos: Vec<RepoState>,
    #[serde(default)]
    pub jcode_default_provider: Option<String>,
    #[serde(default)]
    pub jcode_default_model: Option<String>,
}
impl Session {
    pub fn expired(&self) -> bool {
        Utc::now() > self.last_activity_at + Duration::seconds(self.ttl_seconds)
    }
}

#[derive(Clone)]
pub struct StateStore {
    paths: JboxPaths,
}
impl StateStore {
    pub fn new(paths: JboxPaths) -> Self {
        Self { paths }
    }
    fn file(&self, id: &str) -> PathBuf {
        self.paths.sessions.join(id).join("session.json")
    }
    pub fn save(&self, session: &Session) -> Result<()> {
        let file = self.file(&session.id);
        std::fs::create_dir_all(file.parent().unwrap())?;
        let text = serde_json::to_string_pretty(session)?;
        std::fs::write(&file, text).with_context(|| format!("cannot write {}", file.display()))?;
        Ok(())
    }
    pub fn load(&self, id: &str) -> Result<Session> {
        let file = self.file(id);
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("unknown jbox session `{id}`"))?;
        serde_json::from_str(&text).context("invalid jbox session state")
    }
    pub fn list(&self) -> Result<Vec<Session>> {
        let mut sessions: Vec<Session> = Vec::new();
        for entry in std::fs::read_dir(&self.paths.sessions)? {
            let entry = entry?;
            let file = entry.path().join("session.json");
            if file.exists() {
                let text = std::fs::read_to_string(&file)?;
                sessions.push(
                    serde_json::from_str(&text)
                        .with_context(|| format!("invalid {}", file.display()))?,
                );
            }
        }
        sessions.sort_by_key(|session| std::cmp::Reverse(session.last_activity_at));
        Ok(sessions)
    }
    pub fn remove(&self, id: &str) -> Result<()> {
        let file = self.file(id);
        if file.exists() {
            std::fs::remove_file(file)?;
        }
        Ok(())
    }
}

pub fn new_session_id() -> String {
    let mut rng = rand::rng();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!(
        "{}-{}-{}",
        ADJECTIVES.choose(&mut rng).unwrap(),
        ANIMALS.choose(&mut rng).unwrap(),
        &suffix[..6]
    )
}

#[cfg(test)]
mod tests {
    use super::{ADJECTIVES, ANIMALS, new_session_id};

    #[test]
    fn session_name_vocabulary_is_large_and_slug_safe() {
        assert!(ADJECTIVES.len() >= 48);
        assert!(ANIMALS.len() >= 48);
        assert!(
            ADJECTIVES
                .iter()
                .chain(ANIMALS)
                .all(|word| word.chars().all(|character| character.is_ascii_lowercase()))
        );
    }

    #[test]
    fn generated_session_ids_use_the_word_lists_and_short_hex_suffixes() {
        for _ in 0..32 {
            let id = new_session_id();
            let parts = id.split('-').collect::<Vec<_>>();
            assert_eq!(parts.len(), 3, "unexpected session id: {id}");
            assert!(ADJECTIVES.contains(&parts[0]), "unexpected adjective: {id}");
            assert!(ANIMALS.contains(&parts[1]), "unexpected animal: {id}");
            assert_eq!(parts[2].len(), 6, "unexpected suffix: {id}");
            assert!(
                parts[2]
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
            );
        }
    }
}
