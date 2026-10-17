use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

const INSTALLER: &str = include_str!("../templates/install-artcraft.sh");
const RELEASES: &str = include_str!("../templates/artcraft-releases.txt");

struct Fixture {
    temp: tempfile::TempDir,
    archive: PathBuf,
    manifest: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new(cli: Option<&str>) -> Self {
        Self::new_for("photocraft", cli)
    }

    fn new_for(app: &str, cli: Option<&str>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let archive_name = format!("{app}-1.2.3-linux-x86_64");
        let tree = root.join("tree").join(&archive_name);
        fs::create_dir_all(tree.join("bin")).unwrap();
        fs::create_dir_all(tree.join(format!("share/doc/{app}"))).unwrap();
        fs::write(tree.join(format!("share/doc/{app}/LICENSE-MIT")), "license").unwrap();
        fs::write(tree.join(format!("bin/{app}")), "GUI must not be installed").unwrap();
        if app == "pdfcraft" {
            fs::create_dir_all(tree.join("share/pdfcraft/models")).unwrap();
            for model in ["text-detection.rten", "text-recognition.rten"] {
                fs::write(tree.join("share/pdfcraft/models").join(model), "OCR model").unwrap();
            }
        }
        if let Some(cli) = cli {
            fs::write(tree.join(format!("bin/{app}-cli")), cli).unwrap();
        }
        let archive = root.join("release.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(root.join("tree"))
                .arg(&archive_name)
                .status()
                .unwrap()
                .success()
        );
        let digest = format!("{:x}", Sha256::digest(fs::read(&archive).unwrap()));
        let manifest = root.join("releases.txt");
        fs::write(&manifest, format!("# test release\n{app} 1.2.3 {digest}\n")).unwrap();
        let bin = root.join("mock-bin");
        fs::create_dir(&bin).unwrap();
        let curl = bin.join("curl");
        fs::write(
            &curl,
            "#!/bin/sh\nset -eu\nwhile [ \"$#\" -gt 0 ]; do\n if [ \"$1\" = -o ]; then cp -f \"$MOCK_ARCHIVE\" \"$2\"; exit 0; fi\n shift\ndone\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(curl, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.join("installer.sh"), INSTALLER).unwrap();
        Self {
            prefix: root.join("prefix"),
            archive,
            manifest,
            temp,
        }
    }

    fn run(&self) -> Output {
        Command::new("sh")
            .arg(self.temp.path().join("installer.sh"))
            .arg(&self.manifest)
            .arg(&self.prefix)
            .env("MOCK_ARCHIVE", &self.archive)
            .env("TMPDIR", self.temp.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.temp.path().join("mock-bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .unwrap()
    }
}

#[test]
fn releases_pin_all_thirteen_cli_archives() {
    let expected = [
        "photocraft",
        "vectorcraft",
        "filmcraft",
        "lightcraft",
        "pdfcraft",
        "effectcraft",
        "designcraft",
        "figcraft",
        "wordcraft",
        "soundcraft",
        "deckcraft",
        "cadcraft",
        "gridcraft",
    ];
    let entries: Vec<_> = RELEASES
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .collect();
    assert_eq!(entries.len(), expected.len());
    for (entry, app) in entries.iter().zip(expected) {
        assert_eq!(entry.len(), 3);
        assert_eq!(entry[0], app);
        assert_eq!(entry[1].split('.').count(), 3);
        assert!(entry[1].chars().all(|c| c.is_ascii_digit() || c == '.'));
        assert!(
            include_str!("../docs/artcraft.md")
                .contains(&format!("| `{app}-cli` | {} |", entry[1]))
        );
        assert_eq!(entry[2].len(), 64);
        assert!(entry[2].chars().all(|c| c.is_ascii_hexdigit()));
    }
}

#[test]
fn installer_selects_cli_and_licenses_without_gui() {
    let fixture = Fixture::new(Some("#!/bin/sh\necho photocraft-cli 1.2.3\n"));
    let result = fixture.run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let cli = fixture.prefix.join("bin/photocraft-cli");
    assert!(cli.is_file());
    assert_eq!(
        fs::metadata(cli).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(!fixture.prefix.join("bin/photocraft").exists());
    assert!(
        fixture
            .prefix
            .join("share/doc/photocraft/LICENSE-MIT")
            .is_file()
    );
    assert_eq!(
        fs::read(fixture.prefix.join("share/doc/artcraft-releases.txt")).unwrap(),
        fs::read(&fixture.manifest).unwrap()
    );
}

#[test]
fn installer_preserves_pdfcraft_ocr_models() {
    let fixture = Fixture::new_for(
        "pdfcraft",
        Some("#!/bin/sh\necho '{\"available\": true}'\n"),
    );
    let result = fixture.run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read_to_string(
            fixture
                .prefix
                .join("share/pdfcraft/models/text-detection.rten")
        )
        .unwrap(),
        "OCR model"
    );
}

#[test]
fn installer_rejects_unavailable_ocr_models() {
    let fixture = Fixture::new_for(
        "pdfcraft",
        Some("#!/bin/sh\necho '{\"available\": false}'\n"),
    );
    let result = fixture.run();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("OCR models are unavailable"));
}

#[test]
fn installer_checks_registries_when_help_exits_unsuccessfully() {
    for app in ["deckcraft", "cadcraft"] {
        let fixture = Fixture::new_for(
            app,
            Some("#!/bin/sh\ncase \"$1\" in --version|commands) exit 0;; *) exit 1;; esac\n"),
        );
        assert!(fixture.run().status.success());
    }
}

#[test]
fn installer_rejects_bad_checksum_before_installation() {
    let fixture = Fixture::new(Some("#!/bin/sh\nexit 0\n"));
    fs::write(
        &fixture.manifest,
        format!("photocraft 1.2.3 {}\n", "0".repeat(64)),
    )
    .unwrap();
    assert!(!fixture.run().status.success());
    assert!(!fixture.prefix.join("bin/photocraft-cli").exists());
}

#[test]
fn installer_rejects_missing_or_broken_cli() {
    for cli in [None, Some("#!/bin/sh\nexit 7\n")] {
        let fixture = Fixture::new(cli);
        assert!(!fixture.run().status.success());
    }
}

#[test]
fn installer_rejects_unsafe_manifest_entries() {
    for entry in [
        "../photocraft 1.2.3",
        "photocraft ../../escape",
        "photocraft latest",
        "photocraft 1.2.3 extra",
    ] {
        let fixture = Fixture::new(Some("#!/bin/sh\nexit 0\n"));
        fs::write(&fixture.manifest, format!("{entry} {}\n", "0".repeat(64))).unwrap();
        assert!(!fixture.run().status.success());
        assert!(!fixture.prefix.join("bin/photocraft-cli").exists());
    }
}
