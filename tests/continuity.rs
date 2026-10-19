use chrono::Utc;
use jbox::paths::JboxPaths;
use jbox::state::{RepoState, Session, SessionState, StateStore};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn reserved_guest_home_mounts_fail_before_creating_a_workspace_or_guest() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("project");
    fs::create_dir(&repository).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success()
    );
    let secondary = temp.path().join("secondary");
    fs::create_dir(&secondary).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&secondary)
            .status()
            .unwrap()
            .success()
    );
    let metadata = fs::metadata(&repository).unwrap();
    for target in ["/home", "/home/jbox", "/home//jbox/./"] {
        for section in [
            format!("[workspace]\nmount = '{target}'\n"),
            format!("[[repos]]\npath = '../secondary'\nmount = '{target}'\n"),
            format!("[[mounts]]\nsource = '.'\ntarget = '{target}'\nwritable = true\n"),
        ] {
            fs::write(
                repository.join(".jbox.toml"),
                format!("version = 1\n{section}"),
            )
            .unwrap();
            let output = Command::new(env!("CARGO_BIN_EXE_jbox"))
                .args(["run", "--new", "--no-attach", "."])
                .current_dir(&repository)
                .env("XDG_DATA_HOME", temp.path().join("data"))
                .env("XDG_CACHE_HOME", temp.path().join("cache"))
                .env("XDG_CONFIG_HOME", temp.path().join("config"))
                .output()
                .unwrap();
            assert!(!output.status.success(), "{section}");
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(
                error.contains("mount target overlaps the reserved guest home root"),
                "{section}: {error}"
            );
        }
    }
    let sessions = temp.path().join("data/jbox/sessions");
    assert!(!sessions.exists() || fs::read_dir(sessions).unwrap().next().is_none());
    let branches = Command::new("git")
        .args(["branch", "--list", "jbox/*"])
        .current_dir(&repository)
        .output()
        .unwrap();
    assert!(branches.status.success());
    assert!(branches.stdout.is_empty());
    use std::os::unix::fs::MetadataExt;
    let after = fs::metadata(&repository).unwrap();
    assert_eq!(metadata.uid(), after.uid());
    assert_eq!(metadata.gid(), after.gid());
    assert_eq!(metadata.mode(), after.mode());
}

#[test]
fn offline_network_fails_before_creating_a_workspace_or_guest() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("project");
    fs::create_dir(&repository).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repository)
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        repository.join(".jbox.toml"),
        "version = 1\n[network]\ninternet = false\n[git]\nnetwork = false\ncredentials = 'none'\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_jbox"))
        .args(["run", "--new", "--no-attach", "."])
        .current_dir(&repository)
        .env("XDG_DATA_HOME", temp.path().join("data"))
        .env("XDG_CACHE_HOME", temp.path().join("cache"))
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("Docker --network none discards published ports"),
        "{error}"
    );
    assert!(error.contains("No guest was started"), "{error}");
    let sessions = temp.path().join("data/jbox/sessions");
    assert!(
        !sessions.exists() || fs::read_dir(&sessions).unwrap().next().is_none(),
        "no guest session should be recorded"
    );
    let branches = Command::new("git")
        .args(["branch", "--list", "jbox/*"])
        .current_dir(&repository)
        .output()
        .unwrap();
    assert!(branches.status.success());
    assert!(
        branches.stdout.is_empty(),
        "no sandbox branch should be created"
    );
}

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
        "#!/bin/sh\n[ \"$1\" = --no-update ] || exit 88\nif [ \"$2\" = --version ]; then printf 'jcode v0.99.0 (abc)\\n'; exit 0; fi\nprintf '%s\\n' \"$*\" > \"$JBOX_TEST_JCODE_LOG\"\n",
    )
    .unwrap();
    fs::set_permissions(&jcode, fs::Permissions::from_mode(0o700)).unwrap();
    let ssh = fake_bin.join("ssh");
    fs::write(
        &ssh,
        "#!/bin/sh\n[ \"$1\" = -F ] && [ \"$3\" = -T ] && [ \"$4\" = jbox ] && [ \"$5\" = '/usr/local/bin/jcode --no-update --version' ] || exit 87\nprintf '%s\\n' \"${JBOX_TEST_REMOTE_VERSION-jcode v0.99.0 (abc)}\"\nexit \"${JBOX_TEST_REMOTE_EXIT:-0}\"\n",
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir_all(paths.session_ssh_dir(&session.id)).unwrap();
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

    // Failed restarts must not launch Jcode. An already-running session should
    // attach whether selected by ID or by the invoking repository.
    fs::remove_file(&jcode_log).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_jbox"))
        .args(["resume", &session.id])
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
    assert!(!output.status.success());
    assert!(!jcode_log.exists(), "a failed restart attached to Jcode");

    stopped.state = SessionState::Running;
    store.save(&stopped).unwrap();
    for args in [vec!["resume", &session.id], vec!["resume"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_jbox"))
            .args(&args)
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
            "jbox resume failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            fs::read_to_string(&jcode_log)
                .unwrap()
                .contains("--remote-working-dir /workspace/project")
        );
        fs::remove_file(&jcode_log).unwrap();
    }

    // A host upgrade must not launch the TUI against a pinned old guest, nor
    // silently stop/rebuild the guest or alter its saved conversation marker.
    paths
        .save_last_jcode_session_id(&session.id, &session.repos[0].mount, "session_saved_123")
        .unwrap();
    let before = store.load(&session.id).unwrap();
    let docker_before = fs::read_to_string(&docker_log).unwrap();
    for (remote, exit, expected) in [
        ("jcode v0.98.1 (old)", "0", "version mismatch"),
        ("jcode v0.99.0 (different-build)", "0", "version mismatch"),
        ("", "0", "expected one `jcode v...` version line"),
        (
            "jcode v0.99.0 (abc)",
            "1",
            "could not query guest Jcode version",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_jbox"))
            .args(["attach", &session.id])
            .current_dir(&repository)
            .env("XDG_DATA_HOME", temp.path().join("data"))
            .env("XDG_CACHE_HOME", temp.path().join("cache"))
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("JBOX_TEST_DOCKER_LOG", &docker_log)
            .env("JBOX_TEST_JCODE_LOG", &jcode_log)
            .env("JBOX_TEST_REMOTE_VERSION", remote)
            .env("JBOX_TEST_REMOTE_EXIT", exit)
            .env(
                "PATH",
                format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(expected), "{error}");
        if expected == "version mismatch" {
            assert!(error.contains("jbox stop calm-fox-123"), "{error}");
            assert!(
                error.contains("jbox resume calm-fox-123 --refresh-image"),
                "{error}"
            );
        }
        assert!(!jcode_log.exists(), "a failed preflight launched the TUI");
        let after = store.load(&session.id).unwrap();
        assert_eq!(after.state, before.state);
        assert_eq!(after.image, before.image);
        assert_eq!(after.last_activity_at, before.last_activity_at);
        assert_eq!(
            paths.last_jcode_session_id(&session.id, &session.repos[0].mount),
            Some("session_saved_123".into())
        );
    }
    let docker_after = fs::read_to_string(&docker_log).unwrap();
    assert!(docker_after.starts_with(&docker_before));
    assert!(
        docker_after[docker_before.len()..]
            .lines()
            .all(|line| line.starts_with("inspect "))
    );
}
