use jbox::config::Config;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output};

fn repository(path: &Path) {
    fs::create_dir_all(path.join("nested")).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

fn cli(root: &Path, repository: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jbox"))
        .args(arguments)
        .current_dir(repository)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .output()
        .unwrap()
}

fn no_global_state(root: &Path) {
    for directory in ["data", "cache", "config"] {
        assert!(
            !root.join(directory).exists(),
            "--update must not initialize {directory}"
        );
    }
}

#[test]
fn update_from_nested_directory_preserves_project_and_creates_no_other_files() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    repository(&project);
    let original = "# Scientific project\nversion=1\n[resources]\ncpus=2\nmemory='4G'\n[jcode]\ndefault_provider='openai'\ndefault_model='gpt-6-sol'\nopenai_reasoning_effort='high'\nopenai_service_tier='priority'\nskills=[]\n[jcode.agent.project]\ninstructions='Keep the scientific acceptance workflow'\n";
    let config_path = project.join(".jbox.toml");
    fs::write(&config_path, original).unwrap();
    let output = cli(temp.path(), &project.join("nested"), &["init", "--update"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (config, _) = Config::load(&project).unwrap();
    assert_eq!(config.resources.cpus, 2);
    assert_eq!(config.resources.memory, "4G");
    assert!(config.image.dockerfile.is_none());
    assert_eq!(config.jcode.default_model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(config.jcode.default_provider.as_deref(), Some("openai"));
    assert_eq!(
        config.jcode.openai_reasoning_effort.as_deref(),
        Some("high")
    );
    assert_eq!(
        config.jcode.openai_service_tier.as_deref(),
        Some("priority")
    );
    assert!(config.jcode.skills.is_empty());
    assert_eq!(
        config.jcode.agent.project.instructions.as_deref(),
        Some("Keep the scientific acceptance workflow")
    );
    assert_eq!(
        config.jcode.agent.jbox.instructions.as_deref(),
        Some(jbox::init::JBOX_PROMPT)
    );
    assert!(!project.join(".jbox").exists());
    assert!(!project.join("AGENTS.md").exists());
    no_global_state(temp.path());
    let before = fs::read(&config_path).unwrap();
    let metadata = fs::metadata(&config_path).unwrap();
    let output = cli(temp.path(), &project, &["init", "--update", "nested"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("already up to date"));
    assert_eq!(fs::read(&config_path).unwrap(), before);
    assert_eq!(fs::metadata(&config_path).unwrap().ino(), metadata.ino());
    no_global_state(temp.path());
}

#[test]
fn update_errors_and_tool_conflict_are_non_mutating_and_state_free() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    repository(&project);
    let missing = cli(temp.path(), &project, &["init", "--update"]);
    assert!(!missing.status.success());
    assert!(!project.join(".jbox.toml").exists());
    let path = project.join(".jbox.toml");
    for text in [
        "version=2\n",
        "version=1\n[jcode.agent]\ninstructions='Project text mixed into a legacy prompt'\n",
    ] {
        fs::write(&path, text).unwrap();
        let output = cli(temp.path(), &project, &["init", "--update"]);
        assert!(!output.status.success());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        no_global_state(temp.path());
    }
    let output = cli(temp.path(), &project, &["init", "--update", "--tool", "jq"]);
    assert!(!output.status.success());
    assert!(!project.join(".jbox").exists());
    no_global_state(temp.path());
    fs::remove_file(&path).unwrap();
    let target = temp.path().join("other.toml");
    fs::write(&target, "version=1\n").unwrap();
    symlink(&target, &path).unwrap();
    let output = cli(temp.path(), &project, &["init", "--update"]);
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(&target).unwrap(), "version=1\n");
    no_global_state(temp.path());
}

#[test]
fn ordinary_init_then_update_preserves_image_and_existing_state_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    repository(&project);
    let output = cli(temp.path(), &project, &["init", "--tool", "jq"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dockerfile = project.join(".jbox/Dockerfile");
    let image = fs::read(&dockerfile).unwrap();
    let config = fs::read(project.join(".jbox.toml")).unwrap();
    let state = temp.path().join("data/jbox");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o750)).unwrap();
    let output = cli(temp.path(), &project, &["init", "--update"]);
    assert!(output.status.success());
    assert_eq!(fs::read(&dockerfile).unwrap(), image);
    assert_eq!(fs::read(project.join(".jbox.toml")).unwrap(), config);
    assert_eq!(fs::metadata(&state).unwrap().mode() & 0o777, 0o750);
}
