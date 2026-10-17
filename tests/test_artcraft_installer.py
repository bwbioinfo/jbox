"""Offline installer regression tests. Run: python3 -m unittest discover -s tests -p 'test_*.py'"""

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("install_artcraft", ROOT / "templates/install-artcraft.py")
INSTALLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INSTALLER)


class ArtcraftInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.prefix = self.root / "opt/artcraft"
        self.bin_dir = self.root / "bin"
        self.release = {
            "app": "vectorcraft",
            "version": "0.11.0",
            "url": "https://github.com/storytold/vectorcraft/releases/download/v0.11.0/vectorcraft-0.11.0-linux-x86_64.tar.gz",
            "sha256": "0" * 64,
            "binary": "vectorcraft-0.11.0-linux-x86_64/bin/vectorcraft-cli",
        }

    def archive(self, entries, symlinks=()):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            items = entries.items() if isinstance(entries, dict) else entries
            for name, contents in items:
                member = tarfile.TarInfo(name)
                member.size = len(contents)
                member.mode = 0o644
                archive.addfile(member, io.BytesIO(contents))
            for name, target in symlinks:
                member = tarfile.TarInfo(name)
                member.type = tarfile.SYMTYPE
                member.linkname = target
                archive.addfile(member)
        data = output.getvalue()
        self.release["sha256"] = hashlib.sha256(data).hexdigest()
        return data

    def install(self, data):
        def download(_url, destination):
            destination.write_bytes(data)
        with patch.object(INSTALLER, "download", side_effect=download):
            return INSTALLER.install_release(self.release, self.prefix, self.bin_dir)

    def select_app(self, app):
        self.release = {
            key: value.replace("vectorcraft", app)
            for key, value in self.release.items()
        }

    def cli_fixture(self, app="vectorcraft", script="#!/bin/sh\nexit 0\n"):
        binary = self.root / app / "bin" / f"{app}-cli"
        binary.parent.mkdir(parents=True)
        binary.write_text(script)
        binary.chmod(0o755)
        if app == "pdfcraft":
            models = binary.parent.parent / "share/pdfcraft/models"
            models.mkdir(parents=True)
            for name in INSTALLER.PDFCRAFT_MODELS:
                (models / name).write_bytes(b"model")
        return binary

    def pdf_entries(self, status='{"available": true}'):
        self.select_app("pdfcraft")
        top = str(Path(self.release["binary"]).parent.parent)
        return {
            self.release["binary"]: (
                "#!/bin/sh\ncase \"$1\" in\n"
                "--version|--help) exit 0;;\n"
                f"run) printf '%s\\n' '{status}';;\n"
                "*) exit 7;;\nesac\n"
            ).encode(),
            f"{top}/share/pdfcraft/models/text-detection.rten": b"detection",
            f"{top}/share/pdfcraft/models/text-recognition.rten": b"recognition",
            f"{top}/share/doc/pdfcraft/MODELS-LICENSE": b"CC-BY-SA-4.0",
        }

    def test_bundled_manifest_pins_all_thirteen_official_releases(self):
        releases = INSTALLER.load_releases(ROOT / "templates/artcraft-releases.json")
        self.assertEqual(len(releases), 13)
        self.assertEqual({release["app"] for release in releases}, set(INSTALLER.APPS))

    def test_documented_versions_match_the_canonical_manifest(self):
        documentation = (ROOT / "docs/artcraft.md").read_text()
        for release in INSTALLER.load_releases(ROOT / "templates/artcraft-releases.json"):
            self.assertIn(f"| `{release['app']}-cli` | {release['version']} |", documentation)

    def test_installs_cli_without_desktop_and_retains_resources_and_licenses(self):
        top = "vectorcraft-0.11.0-linux-x86_64"
        entries = {
            self.release["binary"]: b"#!/bin/sh\nexit 0\n",
            f"{top}/bin/vectorcraft": b"unneeded desktop executable",
            f"{top}/share/doc/vectorcraft/LICENSE-MIT": b"license",
            f"{top}/share/fonts/OFL.txt": b"font license",
            f"{top}/share/models/model.rten": b"model data",
        }
        binary = self.install(self.archive(entries))
        self.assertTrue(binary.stat().st_mode & 0o111)
        self.assertEqual((self.bin_dir / "vectorcraft-cli").resolve(), binary)
        self.assertFalse((binary.parent / "vectorcraft").exists())
        for path, contents in entries.items():
            if not path.endswith("/bin/vectorcraft"):
                self.assertEqual((self.prefix / "vectorcraft" / path).read_bytes(), contents)
        self.assertFalse(list(self.prefix.glob(".download-*")))

    def test_checksum_mismatch_fails_before_extracting(self):
        data = self.archive({self.release["binary"]: b"binary"})
        self.release["sha256"] = "f" * 64
        with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
            self.install(data)
        self.assertFalse((self.prefix / "vectorcraft").exists())
        self.assertFalse((self.bin_dir / "vectorcraft-cli").exists())

    def test_archive_traversal_is_rejected(self):
        data = self.archive({"../../escaped": b"bad", self.release["binary"]: b"binary"})
        with self.assertRaisesRegex(ValueError, "unsafe archive path"):
            self.install(data)
        self.assertFalse((self.root / "escaped").exists())

    def test_archive_external_symlinks_are_rejected(self):
        data = self.archive({}, [(self.release["binary"], "/etc/passwd")])
        with self.assertRaisesRegex(ValueError, "links and special files"):
            self.install(data)
        self.assertFalse((self.bin_dir / "vectorcraft-cli").exists())

    def test_archive_size_limit_is_enforced(self):
        data = self.archive({self.release["binary"]: b"binary"})
        with patch.object(INSTALLER, "MAX_EXTRACT_BYTES", 2):
            with self.assertRaisesRegex(ValueError, "extraction limits"):
                self.install(data)

    def test_archive_member_limit_is_enforced(self):
        top = "vectorcraft-0.11.0-linux-x86_64"
        data = self.archive({self.release["binary"]: b"binary", f"{top}/LICENSE-MIT": b"license"})
        with patch.object(INSTALLER, "MAX_MEMBERS", 1):
            with self.assertRaisesRegex(ValueError, "extraction limits"):
                self.install(data)

    def test_duplicate_regular_files_are_rejected(self):
        data = self.archive([(self.release["binary"], b"first"), (self.release["binary"], b"second")])
        with self.assertRaises(FileExistsError):
            self.install(data)

    def test_archive_wrong_top_level_is_rejected(self):
        data = self.archive({"unexpected/bin/vectorcraft-cli": b"binary"})
        with self.assertRaisesRegex(ValueError, "unsafe archive path"):
            self.install(data)

    def test_download_size_limit_is_enforced(self):
        with patch.object(INSTALLER, "urlopen", return_value=io.BytesIO(b"oversized")):
            with patch.object(INSTALLER, "MAX_DOWNLOAD_BYTES", 2):
                with self.assertRaisesRegex(ValueError, "download exceeds size limit"):
                    INSTALLER.download(self.release["url"], self.root / "archive")

    def test_dockerfile_run_instructions_have_valid_posix_shell_syntax(self):
        # Join only continuation lines into complete Docker instructions.
        logical = (ROOT / "templates/base.Dockerfile").read_text().replace("\\\n", " ")
        self.assertIn("RUN ", logical)
        for instruction in logical.splitlines():
            if instruction.startswith("RUN "):
                result = subprocess.run(["sh", "-n"], input=instruction[4:], text=True,
                                        capture_output=True, check=False)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_cli_fails_without_publishing_a_link(self):
        data = self.archive({"vectorcraft-0.11.0-linux-x86_64/share/doc/vectorcraft/LICENSE-MIT": b"license"})
        with self.assertRaisesRegex(ValueError, "expected regular CLI"):
            self.install(data)
        self.assertFalse((self.bin_dir / "vectorcraft-cli").exists())

    def test_existing_installation_and_dangling_links_are_preserved(self):
        self.bin_dir.mkdir()
        link = self.bin_dir / "vectorcraft-cli"
        link.symlink_to(self.root / "missing")
        with self.assertRaises(FileExistsError):
            self.install(b"must not download")
        self.assertTrue(link.is_symlink())

    def test_verification_runs_before_publishing_the_link(self):
        data = self.archive({self.release["binary"]: b"#!/bin/sh\nexit 0\n"})

        def verify(binary, app):
            self.assertEqual(app, "vectorcraft")
            self.assertTrue(binary.is_file())
            self.assertFalse((self.bin_dir / "vectorcraft-cli").is_symlink())
            raise ValueError("verification failed")

        with patch.object(INSTALLER, "verify_cli", side_effect=verify):
            with self.assertRaisesRegex(ValueError, "verification failed"):
                self.install(data)
        self.assertFalse((self.bin_dir / "vectorcraft-cli").is_symlink())

    def test_installation_rejects_failed_version_and_help_without_publishing_link(self):
        for failing in ("--version", "--help"):
            with self.subTest(failing=failing):
                script = f'#!/bin/sh\n[ "$1" != "{failing}" ] || exit 7\nexit 0\n'
                data = self.archive({self.release["binary"]: script.encode()})
                with self.assertRaises(subprocess.CalledProcessError):
                    self.install(data)
                self.assertFalse((self.bin_dir / "vectorcraft-cli").is_symlink())
                # A failed installation intentionally remains unpublished. Use a
                # different prefix for the next independent installation attempt.
                self.prefix = self.root / f"next-{failing}"

    def test_verification_is_headless_and_each_command_is_time_bounded(self):
        binary = self.cli_fixture()
        result = subprocess.CompletedProcess([], 0, stdout="version or help")
        with patch.dict(os.environ, {"DISPLAY": ":99", "WAYLAND_DISPLAY": "test"}):
            with patch.object(INSTALLER.subprocess, "run", return_value=result) as run:
                INSTALLER.verify_cli(binary, "vectorcraft")
        self.assertEqual([call.args[0] for call in run.call_args_list], [
            [str(binary), "--version"], [str(binary), "--help"],
        ])
        for call in run.call_args_list:
            self.assertEqual(call.kwargs["timeout"], 30)
            self.assertTrue(call.kwargs["check"])
            self.assertNotIn("DISPLAY", call.kwargs["env"])
            self.assertNotIn("WAYLAND_DISPLAY", call.kwargs["env"])

    def test_deckcraft_and_cadcraft_check_commands_instead_of_help(self):
        for app in ("deckcraft", "cadcraft"):
            binary = self.cli_fixture(app, '#!/bin/sh\ncase "$1" in\n'
                                      '--version|commands) exit 0;;\n*) exit 1;;\nesac\n')
            INSTALLER.verify_cli(binary, app)

    def test_registry_failures_are_not_ignored(self):
        for app in ("deckcraft", "cadcraft"):
            binary = self.cli_fixture(app, '#!/bin/sh\n[ "$1" != commands ] || exit 7\n')
            with self.assertRaises(subprocess.CalledProcessError):
                INSTALLER.verify_cli(binary, app)

    def test_verification_propagates_timeout_for_every_command(self):
        binary = self.cli_fixture("pdfcraft")
        success = subprocess.CompletedProcess([], 0, stdout="ok")
        for command_index in range(3):
            with self.subTest(command_index=command_index):
                results = [success] * command_index + [subprocess.TimeoutExpired("cli", 30)]
                with patch.object(INSTALLER.subprocess, "run", side_effect=results):
                    with self.assertRaises(subprocess.TimeoutExpired):
                        INSTALLER.verify_cli(binary, "pdfcraft")

    def test_a_hung_cli_is_actually_terminated(self):
        binary = self.cli_fixture(script="#!/bin/sh\nexec sleep 10\n")
        with patch.object(INSTALLER, "CLI_TIMEOUT_SECONDS", 0.01):
            with self.assertRaises(subprocess.TimeoutExpired):
                INSTALLER.verify_cli(binary, "vectorcraft")

    def test_pdfcraft_installation_preserves_models_and_licenses(self):
        entries = self.pdf_entries()
        binary = self.install(self.archive(entries))
        for path, contents in entries.items():
            self.assertEqual((self.prefix / "pdfcraft" / path).read_bytes(), contents)
        # Resolve from the final public symlink, not its unrelated bin directory.
        INSTALLER.verify_cli(self.bin_dir / "pdfcraft-cli", "pdfcraft")
        self.assertEqual((self.bin_dir / "pdfcraft-cli").resolve(), binary)

    def test_pdfcraft_missing_and_empty_models_fail_before_publishing(self):
        for index, name in enumerate(INSTALLER.PDFCRAFT_MODELS):
            for missing in (True, False):
                with self.subTest(model=name, missing=missing):
                    self.prefix = self.root / f"pdf-{index}-{missing}"
                    entries = self.pdf_entries()
                    model_path = next(path for path in entries if path.endswith(name))
                    if missing:
                        del entries[model_path]
                    else:
                        entries[model_path] = b""
                    with self.assertRaisesRegex(ValueError, "missing or empty OCR model"):
                        self.install(self.archive(entries))
                    self.assertFalse((self.bin_dir / "pdfcraft-cli").is_symlink())

    def test_pdfcraft_status_requires_json_object_with_boolean_true(self):
        for index, status in enumerate((
            '{"available": false}', '{"available": 1}', '{"available": "true"}',
            '{}', '[]', 'null', '{"nested": {"available": true}}', 'not JSON',
        )):
            with self.subTest(status=status):
                self.prefix = self.root / f"status-{index}"
                with self.assertRaises(ValueError):
                    self.install(self.archive(self.pdf_entries(status)))
                self.assertFalse((self.bin_dir / "pdfcraft-cli").is_symlink())

    def test_pdfcraft_failed_ocr_status_is_rejected(self):
        binary = self.cli_fixture("pdfcraft", '#!/bin/sh\n[ "$1" != run ] || exit 7\n')
        with self.assertRaises(subprocess.CalledProcessError):
            INSTALLER.verify_cli(binary, "pdfcraft")

    def test_verify_only_does_not_download_or_write_metadata(self):
        releases = INSTALLER.load_releases(ROOT / "templates/artcraft-releases.json")
        for release in releases:
            binary = self.prefix / release["app"] / release["binary"]
            binary.parent.mkdir(parents=True)
            binary.write_text("placeholder")
            self.bin_dir.mkdir(exist_ok=True)
            (self.bin_dir / f"{release['app']}-cli").symlink_to(binary)
        arguments = ["install-artcraft.py", "--manifest", str(ROOT / "templates/artcraft-releases.json"),
                     "--prefix", str(self.prefix), "--bin-dir", str(self.bin_dir), "--verify-only"]
        with patch.object(sys, "argv", arguments):
            with patch.object(INSTALLER, "verify_cli") as verify:
                with patch.object(INSTALLER, "install_release") as install:
                    with patch.object(INSTALLER.shutil, "copyfile") as copy:
                        INSTALLER.main()
        self.assertEqual(verify.call_count, 13)
        install.assert_not_called()
        copy.assert_not_called()
        self.assertFalse((self.prefix / "releases.json").exists())

    def test_verify_only_rejects_mismatched_installed_link(self):
        releases = INSTALLER.load_releases(ROOT / "templates/artcraft-releases.json")
        expected = self.prefix / releases[0]["app"] / releases[0]["binary"]
        expected.parent.mkdir(parents=True)
        expected.write_text("expected binary")
        other = self.cli_fixture("photocraft")
        self.bin_dir.mkdir()
        (self.bin_dir / "photocraft-cli").symlink_to(other)
        arguments = ["install-artcraft.py", "--manifest", str(ROOT / "templates/artcraft-releases.json"),
                     "--prefix", str(self.prefix), "--bin-dir", str(self.bin_dir), "--verify-only"]
        with patch.object(sys, "argv", arguments):
            with patch.object(INSTALLER, "load_releases", return_value=releases[:1]):
                with self.assertRaisesRegex(ValueError, "does not match manifest"):
                    INSTALLER.main()

    def test_verify_only_cli_checks_all_thirteen_installed_tools(self):
        releases = INSTALLER.load_releases(ROOT / "templates/artcraft-releases.json")
        self.bin_dir.mkdir()
        for release in releases:
            binary = self.prefix / release["app"] / release["binary"]
            binary.parent.mkdir(parents=True)
            registry = "commands" if release["app"] in ("deckcraft", "cadcraft") else "--help"
            binary.write_text(
                f'#!/bin/sh\ncase "$1" in\n--version|{registry}) exit 0;;\n'
                'run) printf \'%s\\n\' \'{"available": true}\';;\n*) exit 7;;\nesac\n'
            )
            binary.chmod(0o755)
            (self.bin_dir / f"{release['app']}-cli").symlink_to(binary)
            if release["app"] == "pdfcraft":
                models = binary.parent.parent / "share/pdfcraft/models"
                models.mkdir(parents=True)
                for name in INSTALLER.PDFCRAFT_MODELS:
                    (models / name).write_bytes(b"model")
        command = [sys.executable, str(ROOT / "templates/install-artcraft.py"),
                   "--manifest", str(ROOT / "templates/artcraft-releases.json"),
                   "--prefix", str(self.prefix), "--bin-dir", str(self.bin_dir), "--verify-only"]
        before = {path: (path.stat().st_mode, path.stat().st_mtime_ns, path.read_bytes())
                  for path in self.prefix.rglob("*") if path.is_file()}
        result = subprocess.run(command, capture_output=True, text=True, timeout=10, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Verified all 13 ArtCraft CLIs", result.stdout)
        after = {path: (path.stat().st_mode, path.stat().st_mtime_ns, path.read_bytes())
                 for path in self.prefix.rglob("*") if path.is_file()}
        self.assertEqual(before, after)
        self.assertFalse((self.prefix / "releases.json").exists())
        # The same public CLI must reject a subsequently broken installed tool.
        broken = self.prefix / releases[-1]["app"] / releases[-1]["binary"]
        broken.write_text("#!/bin/sh\nexit 7\n")
        result = subprocess.run(command, capture_output=True, text=True, timeout=10, check=False)
        self.assertNotEqual(result.returncode, 0)

    def test_invalid_binary_url_and_checksum_are_rejected(self):
        for field, value in [
            ("binary", "../../vectorcraft-cli"),
            ("binary", "/vectorcraft-cli"),
            ("binary", "bin/vectorcraft"),
            ("url", self.release["url"].replace("github.com", "example.com")),
            ("url", self.release["url"].replace("https:", "http:")),
            ("url", self.release["url"].replace("/storytold/", "/other/")),
            ("sha256", "not-pinned"),
        ]:
            with self.subTest(field=field, value=value):
                with self.assertRaises(ValueError):
                    INSTALLER.validate_release({**self.release, field: value})

    def test_manifest_omissions_and_duplicates_are_rejected(self):
        raw = json.loads((ROOT / "templates/artcraft-releases.json").read_text())
        manifest = self.root / "manifest.json"
        for releases in [raw["releases"][:-1], raw["releases"] + [raw["releases"][0]]]:
            manifest.write_text(json.dumps({"releases": releases}))
            with self.assertRaisesRegex(ValueError, "exactly once"):
                INSTALLER.load_releases(manifest)


if __name__ == "__main__":
    unittest.main()
