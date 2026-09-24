use chrono::Utc;
use jbox::paths::JboxPaths;
use jbox::state::{RepoState, Session, SessionState, StateStore};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn repeated_run_reuses_the_recorded_workspace_without_starting_another_vm() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("project");
    fs::create_dir_all(repository.join("nested")).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success()
    );
    let paths = JboxPaths {
        data: temp.path().join("data/jbox"),
        cache: temp.path().join("cache/jbox"),
        sessions: temp.path().join("data/jbox/sessions"),
        credentials: temp.path().join("data/jbox/credentials"),
    };
    let store = StateStore::new(paths.clone());
    let now = Utc::now();
    let session = Session {
        version: 1,
        id: "calm-fox-123".into(),
        state: SessionState::Running,
        container_name: "jbox-calm-fox-123".into(),
        ssh_host: "127.0.0.2".into(),
        ssh_port: 22,
        ssh_agent_pid: 0,
        known_hosts_tag: "# jbox:calm-fox-123".into(),
        created_at: now,
        last_activity_at: now,
        ttl_seconds: 86_400,
        config_path: repository.join(".jbox.toml"),
        launch_directory: Some(repository.clone()),
        image: "unused".into(),
        repos: vec![RepoState {
            name: "project".into(),
            source: repository.clone(),
            worktree: temp.path().join("retained-worktree"),
            mount: "/workspace/project".into(),
            branch: "jbox/calm-fox-123/project".into(),
            base_commit: "deadbeef".into(),
            host_gitfile: temp.path().join("host-gitfile"),
            beads_snapshot: None,
            beads_bootstrap: Vec::new(),
        }],
        jcode_default_provider: None,
        jcode_default_model: None,
        git_access_policy: None,
        brokered_plans: Vec::new(),
        sync: None,
    };
    store.save(&session).unwrap();
    paths
        .save_last_jcode_session_id(&session.id, &session.repos[0].mount, "session_saved_123")
        .unwrap();

    let fake_bin = temp.path().join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let docker = fake_bin.join("docker");
    fs::write(
        &docker,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$JBOX_TEST_DOCKER_LOG\"\nif [ \"$1\" = inspect ]; then printf 'true\\n'; exit 0; fi\nexit 86\n",
    )
    .unwrap();
    fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
    let docker_log = temp.path().join("docker.log");
    let jcode = fake_bin.join("jcode");
    fs::write(
        &jcode,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$JBOX_TEST_JCODE_LOG\"\n",
    )
    .unwrap();
    fs::set_permissions(&jcode, fs::Permissions::from_mode(0o700)).unwrap();
    let jcode_log = temp.path().join("jcode.log");
    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_jbox"))
            .args(["run", "--no-attach", "nested"])
            .current_dir(&repository)
            .env("XDG_DATA_HOME", temp.path().join("data"))
            .env("XDG_CACHE_HOME", temp.path().join("cache"))
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("JBOX_TEST_DOCKER_LOG", &docker_log)
            .env("JBOX_TEST_JCODE_LOG", &jcode_log)
            .env(
                "PATH",
                format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "jbox failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("Reusing running jbox session calm-fox-123")
        );
        assert!(!jcode_log.exists(), "--no-attach launched Jcode");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_jbox"))
        .args(["run", "nested"])
        .current_dir(&repository)
        .env("XDG_DATA_HOME", temp.path().join("data"))
        .env("XDG_CACHE_HOME", temp.path().join("cache"))
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("JBOX_TEST_DOCKER_LOG", &docker_log)
        .env("JBOX_TEST_JCODE_LOG", &jcode_log)
        .env(
            "PATH",
            format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let jcode_args = fs::read_to_string(&jcode_log).unwrap();
    assert!(jcode_args.contains("--remote-working-dir /workspace/project"));
    assert!(jcode_args.contains("--resume session_saved_123"));
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(
        paths.last_jcode_session_id(&session.id, &session.repos[0].mount),
        Some("session_saved_123".into())
    );
    let runtime = paths.sessions.join(&session.id).join("runtime/jcode");
    fs::remove_file(runtime.join(JboxPaths::jcode_session_marker_name(
        &session.repos[0].mount,
    )))
    .unwrap();
    fs::write(runtime.join("record-session"), "#!/bin/sh\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_jbox"))
        .args(["run", "nested"])
        .current_dir(&repository)
        .env("XDG_DATA_HOME", temp.path().join("data"))
        .env("XDG_CACHE_HOME", temp.path().join("cache"))
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("JBOX_TEST_DOCKER_LOG", &docker_log)
        .env("JBOX_TEST_JCODE_LOG", &jcode_log)
        .env(
            "PATH",
            format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fs::read_to_string(&jcode_log).unwrap().contains("--resume"));
    let commands = fs::read_to_string(&docker_log).unwrap();
    assert_eq!(commands.lines().count(), 6);
    assert!(commands.lines().all(|line| line.starts_with("inspect ")));
    assert!(!temp.path().join("retained-worktree").exists());

    // A stopped retained session must take the restart path rather than create
    // another VM. This fixture deliberately lacks a resumable configuration,
    // so verify a safe error and unchanged retained state without starting Kata.
    let mut stopped = store.load(&session.id).unwrap();
    stopped.state = SessionState::Stopped;
    store.save(&stopped).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_jbox"))
        .args(["run", "--no-attach", "nested"])
        .current_dir(&repository)
        .env("XDG_DATA_HOME", temp.path().join("data"))
        .env("XDG_CACHE_HOME", temp.path().join("cache"))
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("JBOX_TEST_DOCKER_LOG", &docker_log)
        .env(
            "PATH",
            format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Restarting retained jbox session calm-fox-123")
    );
    assert_eq!(store.list().unwrap().len(), 1);
    assert_eq!(
        store.load(&session.id).unwrap().state,
        SessionState::Stopped
    );
    let stopped_commands = fs::read_to_string(&docker_log).unwrap();
    assert_eq!(
        stopped_commands.lines().count(),
        commands.lines().count() + 1
    );
    assert!(
        stopped_commands
            .lines()
            .all(|line| line.starts_with("inspect "))
    );
}
