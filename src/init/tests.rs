use super::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use tempfile::tempdir;

const PROJECT: &str = r#"# Keep this project's choices and formatting.
version = 1
[runtime]
backend = 'kata'
[resources]
cpus = 3 # tuned
memory = '6G'
disk = '12G'
ttl = '2h'
[workspace]
mount = '/workspace/science'
[[repos]]
path = '../data'
mount = '/workspace/data'
[image]
dockerfile = 'images/science.Dockerfile'
[network]
internet = true
host = false
lan = false
[jcode]
persistent_credentials = false
default_provider = 'openai' # selected account
default_model = 'gpt-6-sol' # only this version may change
openai_reasoning_effort = 'high'
openai_service_tier = 'priority'
[[jcode.skills]]
repository = 'bwbioinfo/skills'
private = true
managed = true
pin = 'reviewed-release' # do not unpin
[[jcode.skills]]
repository = 'example/scientific-skills'
skill = 'scanpy'
pin = 'v1.2.3'
allow_hidden_dirs = true
[jcode.agent.jbox]
instructions = 'Old managed prompt'
[jcode.agent.project]
# This is author-owned.
instructions = '''Use the scientific acceptance workflow.
Keep the project's custom instructions verbatim.'''
[git]
network = true
credentials = 'github-cli'
[git.author]
inherit_host = false
name = 'Project Author'
email = 'author@example.org'
[[mounts]]
source = 'artifacts'
target = '/artifacts'
writable = false
"#;

#[test]
fn refresh_changes_only_managed_values_and_is_idempotent() {
    let updated = refresh(PROJECT).unwrap();
    let before = toml::from_str::<toml::Value>(PROJECT).unwrap();
    let after = toml::from_str::<toml::Value>(&updated).unwrap();
    for key in [
        "version",
        "runtime",
        "resources",
        "workspace",
        "repos",
        "image",
        "network",
        "git",
        "mounts",
    ] {
        assert_eq!(before[key], after[key], "project section {key}");
    }
    for key in [
        "persistent_credentials",
        "default_provider",
        "openai_reasoning_effort",
        "openai_service_tier",
        "skills",
    ] {
        assert_eq!(
            before["jcode"][key], after["jcode"][key],
            "project Jcode choice {key}"
        );
    }
    assert_eq!(
        before["jcode"]["agent"]["project"],
        after["jcode"]["agent"]["project"]
    );
    assert_eq!(
        after["jcode"]["default_model"].as_str(),
        Some("gpt-6.1-sol")
    );
    assert_eq!(
        after["jcode"]["agent"]["jbox"]["instructions"].as_str(),
        Some(JBOX_PROMPT)
    );
    for fragment in [
        "cpus = 3 # tuned",
        "default_provider = 'openai' # selected account",
        "pin = 'reviewed-release' # do not unpin",
        "# This is author-owned.\ninstructions = '''Use the scientific acceptance workflow.\nKeep the project's custom instructions verbatim.'''",
        "[git]\nnetwork = true\ncredentials = 'github-cli'",
    ] {
        assert!(updated.contains(fragment), "preserve {fragment}");
    }
    assert_eq!(refresh(&updated).unwrap(), updated);
}

#[test]
fn templates_and_runtime_prompt_stay_in_sync() {
    let generated = default_config().unwrap();
    let config = Config::from_toml(&generated).unwrap();
    assert_eq!(
        config.jcode.default_model.as_deref(),
        current_model(Some("openai"), "gpt-6-sol")
    );
    assert!(config.jcode.skills[0].managed);
    assert!(config.jcode.agent.instructions.is_none());
    assert_eq!(
        config.jcode.agent.jbox.instructions.as_deref(),
        Some(JBOX_PROMPT)
    );
    assert!(config.jcode.agent.project.instructions.is_none());
    assert_eq!(refresh(&generated).unwrap(), generated);
}

#[test]
fn known_whole_legacy_prompts_migrate_without_touching_project_text() {
    for prompt in [
        LEGACY_PROMPT,
        LEGACY_PROMPT
            .split("\nWhen presenting a code or command block")
            .next()
            .unwrap(),
        JBOX_PROMPT,
        JBOX_PROMPT
            .split("\nWhen presenting a code or command block")
            .next()
            .unwrap(),
    ] {
        let original = format!(
            "version=1\n[jcode.agent]\ninstructions={}\n[jcode.agent.project]\ninstructions='Project guidance'\n",
            value(prompt)
        );
        let updated = refresh(&original).unwrap();
        let config = Config::from_toml(&updated).unwrap();
        assert!(config.jcode.agent.instructions.is_none());
        assert_eq!(
            config.jcode.agent.project.instructions.as_deref(),
            Some("Project guidance")
        );
        assert_eq!(
            config.jcode.agent.jbox.instructions.as_deref(),
            Some(JBOX_PROMPT)
        );
    }
}

#[test]
fn mixed_legacy_prompt_and_invalid_configuration_leave_files_untouched() {
    let temp = tempdir().unwrap();
    for original in [
        "version=1\n[jcode.agent]\ninstructions='Custom project policy'\n",
        "version=2\n",
        "version=1\n[jcode.agent.project]\ninstructions=123\n",
        "version=1\ninvalid toml",
    ] {
        let path = temp.path().join(".jbox.toml");
        fs::write(&path, original).unwrap();
        assert!(update(temp.path()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(!temp.path().join(".jbox").exists());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}

#[test]
fn skill_customization_and_explicit_opt_out_never_get_bypassed() {
    for options in [
        "managed = false",
        "pin = 'v1.2.3'",
        "skill = 'scanpy'\npin = 'v1.2.3'\nmanaged = true",
        "allow_hidden_dirs = true",
    ] {
        let original = format!(
            "version=1\n[git]\ncredentials='github-cli'\n[[jcode.skills]]\nrepository='bwbioinfo/skills'\nprivate=true\n{options}\n"
        );
        let updated = refresh(&original).unwrap();
        let before = toml::from_str::<toml::Value>(&original).unwrap();
        let after = toml::from_str::<toml::Value>(&updated).unwrap();
        assert_eq!(
            before["jcode"]["skills"], after["jcode"]["skills"],
            "{options}"
        );
        assert_eq!(Config::from_toml(&updated).unwrap().jcode.skills.len(), 1);
    }
    for setting in [
        "[jcode]\nskills = []\n",
        "[jcode]\nskills = [{repository='custom/skills', pin='v1'}]\n",
        "",
    ] {
        let original = format!("version=1\n{setting}");
        let updated = refresh(&original).unwrap();
        let config = Config::from_toml(&updated).unwrap();
        assert_eq!(
            config.jcode.skills.len(),
            usize::from(setting.contains("custom/skills"))
        );
        assert!(updated.contains(setting));
    }
}

#[test]
fn exact_legacy_skill_source_is_adopted_but_duplicates_are_not_guessed() {
    for setting in [
        "[[jcode.skills]]\nrepository='bwbioinfo/skills'\nprivate=true\n",
        "[jcode.skills]\nrepository='bwbioinfo/skills'\nprivate=true\n",
        "[jcode]\nskills=[{repository='bwbioinfo/skills',private=true}]\n",
    ] {
        let original = format!("version=1\n[git]\ncredentials='github-cli'\n{setting}");
        let updated = refresh(&original).unwrap();
        let config = Config::from_toml(&updated).unwrap();
        assert_eq!(config.jcode.skills.len(), 1);
        assert!(config.jcode.skills[0].managed);
        assert_eq!(refresh(&updated).unwrap(), updated);
    }
    let duplicate = "version=1\n[git]\ncredentials='github-cli'\n[[jcode.skills]]\nrepository='bwbioinfo/skills'\nprivate=true\n[[jcode.skills]]\nrepository='bwbioinfo/skills'\nprivate=true\n";
    let duplicate = refresh(duplicate).unwrap();
    let sources = Config::from_toml(&duplicate).unwrap().jcode.skills;
    assert_eq!(sources.len(), 2);
    assert!(sources.iter().all(|source| !source.managed));
}

#[test]
fn managed_single_inline_source_merges_without_losing_project_prompt() {
    let original = "version=1\njcode={skills={repository='owner/custom', managed=true, pin='v1'}, agent={project={instructions='keep'}}}\n[git]\ncredentials='github-cli'\n";
    let updated = refresh(original).unwrap();
    let config = Config::from_toml(&updated).unwrap();
    assert_eq!(
        config.jcode.agent.project.instructions.as_deref(),
        Some("keep")
    );
    assert_eq!(config.jcode.skills.len(), 2);
    assert_eq!(config.jcode.skills[0].repository, "owner/custom");
    assert_eq!(config.jcode.skills[0].pin.as_deref(), Some("v1"));
    assert_eq!(refresh(&updated).unwrap(), updated);
}

#[test]
fn upgrades_stay_in_family_and_preserve_newer_pinned_and_custom_models() {
    for (provider, old, new) in [
        ("openai", "gpt-5.6-sol", "gpt-6.1-sol"),
        ("claude", "claude-sonnet-5", "claude-sonnet-5-5"),
        ("claude", "claude-opus-4-6", "claude-opus-5-5"),
        ("openai-compatible", "Qwen3.7-27B", "Qwen3.8-27B"),
        (
            "openai-compatible",
            "North-Mini-Code-0.9",
            "North-Mini-Code-1.0",
        ),
    ] {
        assert_eq!(current_model(Some(provider), old), Some(new));
    }
    for model in [
        "gpt-7-sol",
        "gpt-6.2-sol",
        "gpt-5.6-terra",
        "claude-sonnet-4-5-20251101",
        "claude-opus-4-6[1m]",
        "Qwen4-27B",
        "Qwen3.7-72B",
        "custom",
    ] {
        assert!(current_model(None, model).is_none(), "preserve {model}");
    }
    assert!(current_model(Some("custom"), "gpt-6-sol").is_none());
    for effort in ["none", "minimal"] {
        let original = format!(
            "version=1\n[jcode]\ndefault_model='gpt-6-sol'\nopenai_reasoning_effort='{effort}'\n"
        );
        let config = Config::from_toml(&refresh(&original).unwrap()).unwrap();
        assert_eq!(config.jcode.default_model.as_deref(), Some("gpt-6-sol"));
        assert_eq!(
            config.jcode.openai_reasoning_effort.as_deref(),
            Some(effort)
        );
    }
    let inherited = Config::from_toml(&refresh("version=1\n").unwrap()).unwrap();
    assert!(inherited.jcode.default_model.is_none());
    assert!(inherited.jcode.default_provider.is_none());
}

#[test]
fn crlf_and_absent_final_newlines_are_preserved_and_repeatable() {
    for original in [
        "version = 1\r\n[jcode]\r\nskills = []\r\n",
        "version = 1\r\n[jcode]\r\nskills = []",
        "version = 1\n[jcode]\nskills = []",
    ] {
        let updated = refresh(original).unwrap();
        assert_eq!(updated.ends_with('\n'), original.ends_with('\n'));
        assert!(!updated.ends_with('\r'));
        if original.contains("\r\n") {
            assert!(!updated.replace("\r\n", "").contains('\n'));
            assert!(updated.contains("version = 1\r\n[jcode]\r\nskills = []"));
        }
        assert_eq!(refresh(&updated).unwrap(), updated);
    }
}

#[test]
fn atomic_update_preserves_permissions_and_noop_identity() {
    let temp = tempdir().unwrap();
    let path = temp.path().join(".jbox.toml");
    fs::write(&path, "version=1\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(update(temp.path()).unwrap());
    let before = fs::metadata(&path).unwrap();
    assert_eq!(before.mode() & 0o777, 0o640);
    assert!(!update(temp.path()).unwrap());
    let after = fs::metadata(&path).unwrap();
    assert_eq!(after.ino(), before.ino());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn candidate_validation_and_preservation_guard_prevent_partial_updates() {
    let original = "version=1\n[jcode.skills]\nrepository='owner/public'\nmanaged=true\n[git]\ncredentials='none'\n";
    assert!(Config::from_toml(original).is_ok());
    let temp = tempdir().unwrap();
    let path = temp.path().join(".jbox.toml");
    fs::write(&path, original).unwrap();
    assert!(update(temp.path()).is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), original);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    let updated = PROJECT.replace("cpus = 3", "cpus = 4");
    assert!(
        ensure_project_preserved(
            PROJECT,
            &updated,
            &Config::from_toml(PROJECT).unwrap(),
            &Config::from_toml(&updated).unwrap()
        )
        .is_err()
    );
}

#[test]
fn unsafe_missing_locked_or_changed_sources_are_rejected_without_side_effects() {
    let temp = tempdir().unwrap();
    assert!(update(temp.path()).is_err());
    let target = temp.path().join("other.toml");
    let path = temp.path().join(".jbox.toml");
    fs::write(&target, "version=1\n").unwrap();
    symlink(&target, &path).unwrap();
    assert!(update(temp.path()).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "version=1\n");
    fs::remove_file(&path).unwrap();
    fs::hard_link(&target, &path).unwrap();
    assert!(update(temp.path()).is_err());
    fs::remove_file(&path).unwrap();
    fs::write(&path, "version=1\n").unwrap();
    let directory = File::open(temp.path()).unwrap();
    directory.try_lock().unwrap();
    assert!(update(temp.path()).is_err());
    directory.unlock().unwrap();
    let (original, metadata) = read_config(&path).unwrap();
    fs::write(&path, "version=1\n# concurrent edit\n").unwrap();
    assert!(ensure_unchanged(&path, &original, &metadata).is_err());
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("# concurrent edit")
    );
}
