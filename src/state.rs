use crate::paths::JboxPaths;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use rand::prelude::IndexedRandom;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    let adjectives = ["bright", "calm", "clever", "swift", "quiet", "bold"];
    let animals = ["fox", "otter", "raven", "lynx", "badger", "kite"];
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!(
        "{}-{}-{}",
        adjectives.choose(&mut rng).unwrap(),
        animals.choose(&mut rng).unwrap(),
        &suffix[..6]
    )
}
