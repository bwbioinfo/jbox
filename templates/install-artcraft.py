#!/usr/bin/env python3
"""Install checksum-pinned, official ArtCraft CLI releases into a guest image."""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import tarfile
import tempfile
from urllib.parse import urlparse
from urllib.request import Request, urlopen


APPS = (
    "photocraft", "vectorcraft", "filmcraft", "lightcraft", "pdfcraft",
    "effectcraft", "designcraft", "figcraft", "wordcraft", "soundcraft",
    "deckcraft", "cadcraft", "gridcraft",
)
MAX_DOWNLOAD_BYTES = 512 * 1024 * 1024
MAX_EXTRACT_BYTES = 2 * 1024 * 1024 * 1024
MAX_MEMBERS = 10_000


def validate_release(release):
    app = release["app"]
    if app not in APPS or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", release["version"]):
        raise ValueError("invalid ArtCraft application or version")
    if not re.fullmatch(r"[0-9a-f]{64}", release["sha256"]):
        raise ValueError(f"{app}: a pinned SHA-256 checksum is required")
    url = urlparse(release["url"])
    if (url.scheme != "https" or url.netloc != "github.com"
            or not url.path.startswith(f"/storytold/{app}/releases/download/")
            or not url.path.endswith(".tar.gz") or url.query or url.fragment):
        raise ValueError(f"{app}: expected an official GitHub release tarball")
    binary = PurePosixPath(release["binary"])
    if (binary.is_absolute() or ".." in binary.parts
            or binary.parts != (f"{app}-{release['version']}-linux-x86_64", "bin", f"{app}-cli")):
        raise ValueError(f"{app}: invalid CLI path")


def load_releases(manifest):
    releases = json.loads(manifest.read_text())["releases"]
    for release in releases:
        validate_release(release)
    if sorted(release["app"] for release in releases) != sorted(APPS):
        raise ValueError("ArtCraft manifest must contain each of the 13 applications exactly once")
    return releases


def download(url, destination):
    request = Request(url, headers={"User-Agent": "jbox-image-builder"})
    with urlopen(request, timeout=60) as source, destination.open("wb") as output:
        total = 0
        while chunk := source.read(1024 * 1024):
            total += len(chunk)
            if total > MAX_DOWNLOAD_BYTES:
                raise ValueError("ArtCraft download exceeds size limit")
            output.write(chunk)


def extract_cli(archive, destination, desktop):
    # Do not use extractall: older supported host Pythons lack extraction
    # filters. Official release archives need only regular files/directories.
    # Reject links and special files rather than recreating filesystem aliases.
    total = 0
    for count, member in enumerate(archive, start=1):
        path = PurePosixPath(member.name)
        if (path.is_absolute() or ".." in path.parts or not path.parts
                or path.parts[0] != desktop.parts[0]):
            raise ValueError("unsafe archive path")
        if not member.isfile() and not member.isdir():
            raise ValueError("archive links and special files are not allowed")
        total += member.size
        if count > MAX_MEMBERS or member.size < 0 or total > MAX_EXTRACT_BYTES:
            raise ValueError("ArtCraft archive exceeds extraction limits")
        if path == desktop:
            continue
        target = destination / path
        if member.isdir():
            target.mkdir(parents=True, exist_ok=True)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            with archive.extractfile(member) as source, target.open("xb") as output:
                shutil.copyfileobj(source, output)


def install_release(release, prefix, bin_dir):
    validate_release(release)
    app = release["app"]
    destination = prefix / app
    link = bin_dir / f"{app}-cli"
    if destination.exists() or destination.is_symlink() or link.exists() or link.is_symlink():
        raise FileExistsError(f"{app}: refusing to overwrite an existing installation")
    print(f"Installing {app} {release['version']}", flush=True)
    prefix.mkdir(parents=True, exist_ok=True)
    bin_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".download-", dir=prefix) as temporary:
        archive = Path(temporary) / "release.tar.gz"
        download(release["url"], archive)
        with archive.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        if digest != release["sha256"]:
            raise ValueError(f"{app}: SHA-256 mismatch, refusing to extract")
        # Preserve relative resources, shared libraries, fonts and license notices.
        # Omit the desktop executable, but do not relocate the headless CLI.
        desktop = PurePosixPath(release["binary"]).with_name(app)

        destination.mkdir()
        with tarfile.open(archive, "r:gz") as source:
            extract_cli(source, destination, desktop)
    binary = destination / release["binary"]
    if not binary.is_file() or binary.is_symlink():
        raise ValueError(f"{app}: archive does not contain the expected regular CLI binary")
    binary.chmod(0o755)
    link.symlink_to(binary)
    return binary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--prefix", type=Path, default=Path("/opt/artcraft"))
    parser.add_argument("--bin-dir", type=Path, default=Path("/usr/local/bin"))
    args = parser.parse_args()
    releases = load_releases(args.manifest)
    for release in releases:
        install_release(release, args.prefix.resolve(), args.bin_dir.resolve())
    # Record the exact installed versions and checksums for later inspection.
    shutil.copyfile(args.manifest, args.prefix / "releases.json")
    print("Installed all 13 ArtCraft CLIs", flush=True)


if __name__ == "__main__":
    main()
