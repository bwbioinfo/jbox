# Default-tool review on 2026-10-24

This review covers the non-ArtCraft defaults in `templates/base.Dockerfile`.
The [ArtCraft review](artcraft.md) records the creative suite separately.
Keep the Debian Bookworm base aligned across the Rust, Node and Python stages.
Do not move to another distribution or a prerelease just to get a larger
version number.

## Updated pins

| Tool | Previous default | Reviewed default | Reason and official source |
| --- | --- | --- | --- |
| Rust | 1.99.0 | 1.99.1 | Stable regression fixes, including x86_64 FFI `bool` returns. [Release announcement](https://blog.rust-lang.org/2026/10/15/Rust-1.99.1/) |
| Node.js | 26.12.0 | 26.12.1 | Security patch on the existing Current line. [Official release index](https://nodejs.org/dist/index.json) |
| Python | 3.15.0 | 3.15.1 | Expedited security maintenance release, including pip 26.2.2 and its updated certificate bundle. [Release notes](https://www.python.org/downloads/release/python-3151/) |
| uv / uvx | 0.13.1 | 0.13.5 | Certificate-chain security fixes, updated managed Python builds and resolver fixes. [Release notes](https://github.com/astral-sh/uv/releases/tag/0.13.5) |
| npm / npx | Bundled 11.20.0 | Explicitly pinned 11.21.1 | Latest stable release on the existing npm 11 line, with dependency and installation fixes. [Release notes](https://github.com/npm/cli/releases/tag/v11.21.1) |
| pnpm / pnpx | 12.11.2 | 12.15.0 | Stable update within pnpm 12, including registry metadata validation and installation fixes. [Release notes](https://github.com/pnpm/pnpm/releases/tag/v12.15.0) |
| Beads | Floating installer from `main` | 1.3.2 | Latest stable release, pinned with SHA-256 before extraction. [Release notes](https://github.com/gastownhall/beads/releases/tag/v1.3.2) |

The [official Rust](https://github.com/docker-library/official-images/blob/master/library/rust),
[Node](https://github.com/docker-library/official-images/blob/master/library/node)
and [Python](https://github.com/docker-library/official-images/blob/master/library/python)
image catalogs list the selected Bookworm tags for amd64. uv retains its
version-pinned official `ghcr.io/astral-sh/uv` image. npm and pnpm are installed
with explicit versions in the Node stage. pnpm's lifecycle scripts remain
enabled with package-scoped `--allow-scripts=pnpm` because its native executable
needs them. No global allow-all script policy is installed.

Beads previously selected a release at build time through an unpinned shell
installer, with fallback compilation paths. It now downloads only the official
Linux amd64 1.3.2 archive and verifies
`c6ea3281f6ff82628351b2d0967c9eb85862c8a0ec0ebc24e63e645c8b02462e`
before extraction. This digest matches both upstream
[checksums.txt](https://github.com/gastownhall/beads/releases/download/v1.3.2/checksums.txt)
and GitHub's release asset digest. Its binary, license, README and changelog
remain together under `/opt/beads`, with only `bd` linked onto PATH. Image
builds also run `bd version` as the final non-root guest user. Changing this
pin now changes the image cache fingerprint rather than relying on whichever
version a floating installer happens to select.

## Reviewed and retained

- **Rclone 1.75.2** remains the latest stable
  [official release](https://github.com/rclone/rclone/releases/tag/v1.75.2).
- **Quarto 1.10.19** remains the latest stable
  [official release](https://github.com/quarto-dev/quarto-cli/releases/tag/v1.10.19).
  Its Linux amd64 checksum still matches the existing pin. Do not substitute
  a Quarto prerelease or independently replace its bundled Pandoc and Deno.
- **npm 12.2.1** is the overall latest npm release, but this review deliberately
  retains Node's existing npm 11 major and updates it to 11.21.1 instead.
  This avoids introducing a package-manager major-version change alongside
  a Node security patch. The selected npm version supports Node 26.
- **GitHub CLI** continues to use GitHub's official APT repository. Its amd64
  [package index](https://cli.github.com/packages/dists/stable/main/binary-amd64/Packages)
  offered 2.103.2 during review, matching the latest
  [official release](https://github.com/cli/cli/releases/tag/v2.103.2).
  New image builds select the available repository version. Existing cached
  images are not updated automatically.
- **Native builds, shell, inspection, archives, Git, OpenSSH, SQLite, rsync,
  fonts and CA certificates** remain Debian Bookworm packages. Keep Debian's
  maintained versions and security backports instead of installing unrelated
  upstream binaries or mixing Debian suites. Their exact versions are selected
  from the configured APT repositories at image-build time, not pinned to the
  latest upstream major versions. This is not a full vulnerability audit.
- **Cargo, rustfmt, Clippy and rust-analyzer** follow the pinned Rust image.
  **pip and venv** follow the Python image. Python 3.15.1 bundles pip 26.2.2.
- **Jcode** remains the host-selected launcher and executable copied into the
  guest build. Its contents already participate in the cache key. A default
  package review does not download a different Jcode or change host credentials.
- The repository's own **Rust 1.98.1 development pin** is independent of the
  guest's default Rust compiler and remains unchanged.

## Validation and remaining acceptance

Real release checks ran in disposable scratch directories as the non-root
Debian 12 guest user, without modifying existing installed tools or project
Beads data:

- Node 26.12.1's official Linux x64 archive matched `SHASUMS256.txt`, then
  started successfully with working crypto, npm and npx.
- Installing npm 11.21.1 and pnpm 12.15.0 with npm's lifecycle scripts
  succeeded, including an npm self-upgrade into the same prefix as Node's
  bundled npm. npm, npx, pnpm and pnpx started successfully.
- uv 0.13.5's official Linux x86_64 archive matched its published checksum
  and GitHub asset digest. uv and uvx started successfully.
- uv installed its managed Python 3.15.1 build and created a virtual
  environment. SSL, SQLite, venv and bundled pip 26.2.2 checks passed.
  This checks a managed standalone Python build, not the Docker Python image.
- Rust 1.99.1, Cargo, rustfmt, Clippy and rust-analyzer started successfully,
  and a program compiled with that compiler ran successfully.
- The checksum-verified Beads 1.3.2 archive started on Debian 12 with its libc
  dependency resolved. Real embedded-Dolt initialization with Jbox's guest
  flags imported a synthetic JSONL issue. Re-importing preserved both the
  original issue and a database-only issue, and bootstrap accepted the
  retained database.

The complete repository gate passed with its unchanged Rust 1.98.1 pin:
formatting, Clippy with warnings denied, all 187 Rust tests, CLI help and all
30 offline Python installer regressions. Updated image regressions check the
new versions, Beads checksum-before-extraction and license preservation,
package-scoped pnpm lifecycle approval, and non-root Beads startup. Quarto's
checksum-order assertion is scoped to its own download rather than whichever
archive happens to be downloaded first.

Docker is unavailable in this isolated guest. The exact new image tags,
cross-stage copies, clean APT installation and fresh Kata startup still need
host-side acceptance, tracked in `jbox-mau`. No host runtime socket, device or
credential mount was added to bypass that boundary. New defaults apply only
after rebuilding Jbox and starting a fresh session. Existing guests are not
modified.
