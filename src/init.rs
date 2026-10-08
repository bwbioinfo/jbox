//! Narrow, formatting-preserving refresh of Jbox-owned configuration.
use crate::config::{Config, SkillSource};
use anyhow::{Context, Result, bail};
use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table, TableLike, Value, value};

const CONFIG_TEMPLATE: &str = include_str!("../templates/default.toml");
pub const JBOX_PROMPT: &str = include_str!("../templates/jbox-prompt.md");
const LEGACY_PROMPT: &str = include_str!("../templates/legacy-prompt-v0.15.md");

struct UpdateLock(File);

impl Drop for UpdateLock {
    fn drop(&mut self) {
        // Parallel subprocess spawning can inherit an open file description
        // briefly before exec. Unlock explicitly rather than relying on the
        // last descriptor closing to release the advisory lock.
        let _ = self.0.unlock();
    }
}

/// Resolve the project without opening Jbox's global state or credentials.
pub fn update_project(input: &Path) -> Result<()> {
    let project = Config::repository_root(input)?;
    if update(&project)? {
        println!(
            "updated managed model versions, skills and Jbox prompt in {}",
            project.join(".jbox.toml").display()
        );
    } else {
        println!(
            "managed configuration is already up to date in {}",
            project.join(".jbox.toml").display()
        );
    }
    println!("Project settings are unchanged. Updated configuration applies to fresh sessions.");
    Ok(())
}

pub fn default_config() -> Result<String> {
    let mut document = CONFIG_TEMPLATE.parse::<DocumentMut>()?;
    let jcode = child(document.as_table_mut(), "jcode")?;
    let agent = child(jcode, "agent")?;
    set_value(child(agent, "jbox")?, "instructions", value(JBOX_PROMPT));
    Ok(document.to_string())
}

/// Refresh an existing file only. No image, credentials, skills installation,
/// runtime state, or other project file is touched by this operation.
pub fn update(project: &Path) -> Result<bool> {
    // Coordinate Jbox updaters without adding a lock file to the project. This
    // advisory directory lock does not lock out unrelated editors, so also
    // recheck the source immediately before its atomic replacement.
    let directory = File::open(project)?;
    directory
        .try_lock()
        .context("another Jbox update is using this repository")?;
    let directory = UpdateLock(directory);
    let path = project.join(".jbox.toml");
    let (original, metadata) = read_config(&path)?;
    let refreshed = refresh(&original)?;
    if original == refreshed {
        return Ok(false);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(project)?;
    let temporary_metadata = temporary.as_file().metadata()?;
    if temporary_metadata.uid() != metadata.uid() || temporary_metadata.gid() != metadata.gid() {
        bail!("cannot preserve .jbox.toml ownership during atomic update; no files changed");
    }
    temporary.write_all(refreshed.as_bytes())?;
    temporary
        .as_file()
        .set_permissions(metadata.permissions())?;
    temporary.as_file().sync_all()?;
    ensure_unchanged(&path, &original, &metadata)?;
    let current_directory = std::fs::metadata(project)?;
    let locked_directory = directory.0.metadata()?;
    if current_directory.dev() != locked_directory.dev()
        || current_directory.ino() != locked_directory.ino()
    {
        bail!("repository directory changed during --update; no update was written");
    }
    temporary
        .persist(&path)
        .map_err(|error| error.error)
        .context("cannot atomically replace .jbox.toml")?;
    Ok(true)
}

fn read_config(path: &Path) -> Result<(String, Metadata)> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| {
            format!(
                "cannot open {}: --update requires an existing regular file, not a symlink; run `jbox init` first if missing",
                path.display()
            )
        })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        bail!(
            "{} must be a regular file with a single hard link",
            path.display()
        );
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok((text, metadata))
}

fn ensure_unchanged(path: &Path, original: &str, metadata: &Metadata) -> Result<()> {
    let (current, current_metadata) = read_config(path)?;
    if current != original
        || current_metadata.dev() != metadata.dev()
        || current_metadata.ino() != metadata.ino()
        || current_metadata.mode() != metadata.mode()
        || current_metadata.uid() != metadata.uid()
        || current_metadata.gid() != metadata.gid()
    {
        bail!(
            ".jbox.toml changed during --update; no update was written, retry after reviewing it"
        );
    }
    Ok(())
}

fn refresh(original: &str) -> Result<String> {
    let existing = Config::from_toml(original)?;
    let mut document = original
        .parse::<DocumentMut>()
        .context("cannot edit .jbox.toml")?;
    let defaults = default_config()?.parse::<DocumentMut>()?;
    let default_config = Config::from_toml(&defaults.to_string())?;

    // A whole known generated prompt may be migrated, but never guess which
    // parts of an edited legacy prompt belong to its project.
    if let Some(legacy) = existing.jcode.agent.instructions.as_deref()
        && !known_legacy_prompt(legacy)
    {
        bail!(
            "edited legacy jcode.agent.instructions cannot be safely refreshed: move project-specific guidance to [jcode.agent.project].instructions, remove the legacy instructions key, then rerun `jbox init --update`; no files changed"
        );
    }
    let jcode = child(document.as_table_mut(), "jcode")?;
    if let Some(model) = existing.jcode.default_model.as_deref()
        && let Some(updated) = current_model(existing.jcode.default_provider.as_deref(), model)
        && !(updated == "gpt-6.1-sol"
            && matches!(
                existing.jcode.openai_reasoning_effort.as_deref(),
                Some("none" | "minimal")
            ))
    {
        set_value(jcode, "default_model", value(updated));
    }
    refresh_skills(
        jcode,
        &existing.jcode.skills,
        &default_config.jcode.skills,
        &defaults,
    )?;
    let agent = child(jcode, "agent")?;
    if existing.jcode.agent.instructions.is_some() {
        agent.remove("instructions");
    }
    set_value(child(agent, "jbox")?, "instructions", value(JBOX_PROMPT));
    // Only add the empty authoring section if it is absent. Never copy a
    // template over a project's existing instructions or comments.
    if !agent.contains_key("project") {
        let mut project = Table::new();
        project.decor_mut().set_prefix("\n# Project-owned session guidance. `jbox init --update` never replaces this section.\n");
        agent.insert("project", Item::Table(project));
    }
    if existing.jcode.agent.instructions.is_some()
        && let Some(agent) = jcode.get_mut("agent").and_then(Item::as_table_mut)
    {
        agent.set_implicit(true);
    }
    let mut result = document.to_string();
    if original.contains("\r\n") && !original.replace("\r\n", "").contains('\n') {
        result = result.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    if !original.ends_with('\n') && result.ends_with('\n') {
        result.pop();
        if result.ends_with('\r') {
            result.pop();
        }
    }
    let refreshed = Config::from_toml(&result)
        .context("refreshed configuration failed validation; no files changed")?;
    ensure_project_preserved(original, &result, &existing, &refreshed)?;
    Ok(result)
}

fn ensure_project_preserved(
    original: &str,
    updated: &str,
    before: &Config,
    after: &Config,
) -> Result<()> {
    let skills_preserved = after.jcode.skills.len() >= before.jcode.skills.len()
        && before
            .jcode
            .skills
            .iter()
            .zip(&after.jcode.skills)
            .all(|(before, after)| {
                before.repository == after.repository
                    && before.skill == after.skill
                    && before.pin == after.pin
                    && before.private == after.private
                    && before.allow_hidden_dirs == after.allow_hidden_dirs
            });
    if !skills_preserved {
        bail!("refresh would change a project's skill selection or settings; no files changed");
    }
    let project_view = |text: &str| -> Result<toml::Value> {
        let mut document = toml::from_str::<toml::Value>(text)?;
        if let Some(jcode) = document
            .get_mut("jcode")
            .and_then(toml::Value::as_table_mut)
        {
            jcode.remove("default_model");
            jcode.remove("skills");
            if let Some(agent) = jcode.get_mut("agent").and_then(toml::Value::as_table_mut) {
                agent.remove("instructions");
                agent.remove("jbox");
                if agent
                    .get("project")
                    .and_then(toml::Value::as_table)
                    .is_some_and(|project| project.is_empty())
                {
                    agent.remove("project");
                }
                if agent.is_empty() {
                    jcode.remove("agent");
                }
            }
            if jcode.is_empty() {
                document.as_table_mut().unwrap().remove("jcode");
            }
        }
        Ok(document)
    };
    if project_view(original)? != project_view(updated)? {
        bail!("refresh would change project-specific configuration; no files changed");
    }
    Ok(())
}

#[cfg(test)]
mod tests;

fn known_legacy_prompt(prompt: &str) -> bool {
    let prompt = prompt.trim_end_matches('\n');
    prompt == LEGACY_PROMPT.trim_end_matches('\n')
        || prompt
            == LEGACY_PROMPT
                .split("\nWhen presenting a code or command block")
                .next()
                .unwrap_or(LEGACY_PROMPT)
                .trim_end_matches('\n')
        || prompt == JBOX_PROMPT.trim_end_matches('\n')
        // The previous project-local policy omitted the copy/paste paragraph.
        || prompt
            == JBOX_PROMPT
                .split("\nWhen presenting a code or command block")
                .next()
                .unwrap_or(JBOX_PROMPT)
                .trim_end_matches('\n')
}

/// Update a known family without changing provider, model tier or effort.
/// Unknown/custom routes and missing settings remain intentional project choices.
fn current_model(provider: Option<&str>, model: &str) -> Option<&'static str> {
    let eligible = |expected| provider.is_none_or(|provider| provider == expected);
    if eligible("openai")
        && model
            .strip_prefix("gpt-")
            .and_then(|model| model.strip_suffix("-sol"))
            .is_some_and(|version| version_at_most(version, '.', &[6, 1]))
    {
        Some("gpt-6.1-sol")
    } else if eligible("claude")
        && model
            .strip_prefix("claude-sonnet-")
            .is_some_and(|version| version_at_most(version, '-', &[5, 5]))
    {
        Some("claude-sonnet-5-5")
    } else if eligible("claude")
        && model
            .strip_prefix("claude-opus-")
            .is_some_and(|version| version_at_most(version, '-', &[5, 5]))
    {
        Some("claude-opus-5-5")
    } else if eligible("openai-compatible")
        && model
            .strip_prefix("North-Mini-Code-")
            .is_some_and(|version| version_at_most(version, '.', &[1, 0]))
    {
        Some("North-Mini-Code-1.0")
    } else if eligible("openai-compatible")
        && model
            .strip_prefix("Qwen")
            .and_then(|model| model.strip_suffix("-27B"))
            .is_some_and(|version| version_at_most(version, '.', &[3, 8]))
    {
        Some("Qwen3.8-27B")
    } else {
        None
    }
}

fn version_at_most(version: &str, separator: char, current: &[u64]) -> bool {
    let Some(mut components) = version
        .split(separator)
        .map(|part| {
            if !part.is_empty() && part.bytes().all(|character| character.is_ascii_digit()) {
                part.parse::<u64>().ok()
            } else {
                None
            }
        })
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    // Dated or context-size variants are deliberate project selections, not
    // ordinary rolling versions. Never downgrade a newer local model either.
    if components.len() > current.len() {
        return false;
    }
    components.resize(current.len(), 0);
    components.as_slice() <= current
}

fn source_identity(source: &SkillSource) -> (&str, Option<&str>) {
    (&source.repository, source.skill.as_deref())
}

fn refresh_skills(
    jcode: &mut dyn TableLike,
    existing: &[SkillSource],
    defaults: &[SkillSource],
    template: &DocumentMut,
) -> Result<()> {
    let Some(sources) = jcode.get_mut("skills") else {
        // Missing and explicitly empty sources are intentional opt-outs.
        return Ok(());
    };
    let adoptable = |source: &SkillSource| {
        !source.managed
            && source.pin.is_none()
            && defaults.iter().any(|default| {
                source_identity(source) == source_identity(default)
                    && source.private == default.private
                    && source.allow_hidden_dirs == default.allow_hidden_dirs
            })
            && existing
                .iter()
                .filter(|other| source_identity(other) == source_identity(source))
                .count()
                == 1
    };
    let ownership_present = if let Some(tables) = sources.as_array_of_tables() {
        tables
            .iter()
            .map(|table| table.contains_key("managed"))
            .collect::<Vec<_>>()
    } else if let Some(array) = sources.as_array() {
        array
            .iter()
            .map(|source| {
                source
                    .as_inline_table()
                    .is_some_and(|table| table.contains_key("managed"))
            })
            .collect()
    } else {
        vec![
            sources
                .as_table_like()
                .is_some_and(|table| table.contains_key("managed")),
        ]
    };
    let managed = existing
        .iter()
        .enumerate()
        .map(|(index, source)| source.managed || (!ownership_present[index] && adoptable(source)))
        .collect::<Vec<_>>();
    if !managed.iter().any(|managed| *managed) {
        return Ok(());
    }
    let mark = |table: &mut dyn TableLike, index: usize| {
        if managed[index] {
            set_value(table, "managed", value(true));
        }
    };
    if let Some(tables) = sources.as_array_of_tables_mut() {
        for (index, table) in tables.iter_mut().enumerate() {
            mark(table, index);
        }
    } else if let Some(array) = sources.as_array_mut() {
        for (index, source) in array.iter_mut().enumerate() {
            mark(
                source
                    .as_inline_table_mut()
                    .context("invalid skill source")?,
                index,
            );
        }
    } else {
        mark(
            sources
                .as_table_like_mut()
                .context("invalid skill source")?,
            0,
        );
    }
    // Merge new default sources only into a collection that opted into managed
    // defaults. Existing pins, selectors and authentication declarations are
    // project-owned even when the source is marked managed.
    for (index, default) in defaults.iter().enumerate() {
        if existing
            .iter()
            // A narrower selector or pinned source already owns this repository.
            // Adding its unpinned whole-repository default could overwrite those
            // skill contents during guest installation, even with fields intact.
            .any(|source| source.repository == default.repository)
        {
            continue;
        }
        let table = template["jcode"]["skills"]
            .as_array_of_tables()
            .and_then(|tables| tables.get(index))
            .context("invalid bundled default skill source")?
            .clone();
        if let Some(tables) = sources.as_array_of_tables_mut() {
            tables.push(table);
        } else if let Some(array) = sources.as_array_mut() {
            array.push(Value::InlineTable(table.into_inline_table()));
        } else {
            let inline = sources.is_value();
            let previous = sources
                .clone()
                .into_table()
                .map_err(|_| anyhow::anyhow!("invalid skill source"))?;
            let mut tables = toml_edit::ArrayOfTables::new();
            tables.push(previous);
            tables.push(table);
            *sources = if inline {
                Item::Value(Value::Array(tables.into_array()))
            } else {
                Item::ArrayOfTables(tables)
            };
        }
    }
    Ok(())
}

fn child<'a>(parent: &'a mut dyn TableLike, key: &str) -> Result<&'a mut dyn TableLike> {
    if !parent.contains_key(key) {
        let mut table = Table::new();
        table.set_implicit(true);
        parent.insert(key, Item::Table(table));
    }
    parent
        .get_mut(key)
        .and_then(Item::as_table_like_mut)
        .with_context(|| format!("{key} must be a TOML table"))
}

fn set_value(table: &mut dyn TableLike, key: &str, mut replacement: Item) {
    if let Some(current) = table.get(key).and_then(Item::as_value) {
        if current.to_string() == replacement.as_value().unwrap().to_string()
            || (current.as_str().is_some() && current.as_str() == replacement.as_str())
            || (current.as_bool().is_some() && current.as_bool() == replacement.as_bool())
        {
            return;
        }
        *replacement.as_value_mut().unwrap().decor_mut() = current.decor().clone();
    }
    table.insert(key, replacement);
}
