"""Offline installer regression tests. Run: python3 -m unittest discover -s tests -p 'test_*.py'"""

import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
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

    def test_bundled_manifest_pins_all_thirteen_official_releases(self):
        releases = INSTALLER.load_releases(ROOT / "templates/artcraft-releases.json")
        self.assertEqual(len(releases), 13)
        self.assertEqual({release["app"] for release in releases}, set(INSTALLER.APPS))

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
