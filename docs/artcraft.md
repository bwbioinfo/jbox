# ArtCraft headless tools in Jbox

The default x86_64 Debian guest includes all 13 ArtCraft command-line tools.
The suite was reviewed against the official [ArtCraft apps](https://getartcraft.com/apps)
and [storytold repositories](https://github.com/storytold) on 2026-10-24.
GitHub release tags, not the website's occasionally older download labels, are
the source of the pinned versions below.

| Command | Version | Main workflows |
| --- | --- | --- |
| `photocraft-cli` | 0.15.1 | Raster images, layered documents, conversion and batch actions |
| `vectorcraft-cli` | 0.16.0 | Vector illustration and SVG, PDF and raster export |
| `filmcraft-cli` | 0.12.0 | Video timelines, media inspection, rendering and encoding |
| `lightcraft-cli` | 0.13.0 | Photo libraries, RAW development and export |
| `pdfcraft-cli` | 0.13.0 | PDF inspection, text, editing, rendering and OCR |
| `effectcraft-cli` | 0.16.0 | Compositing, animation, effects and frame/video rendering |
| `designcraft-cli` | 0.12.0 | Page layout, document scripting and publishing |
| `figcraft-cli` | 0.10.0 | Interface design documents, conversion and frame export |
| `wordcraft-cli` | 0.10.0 | Word-processing documents, conversion and rendering |
| `soundcraft-cli` | 0.12.1 | Audio files, session editing and offline bounce |
| `deckcraft-cli` | 0.10.0 | Slide decks, rendering and conversion |
| `cadcraft-cli` | 0.12.0 | CAD drawings, inspection and interchange |
| `gridcraft-cli` | 0.11.1 | Spreadsheets, formulas and workbook interchange |

## Installation and dependencies

`templates/artcraft-releases.json` pins every official Linux x86_64 release
archive by version, URL and SHA-256. `templates/install-artcraft.py` verifies the
checksum before safe extraction under `/opt/artcraft/<app>`. It preserves each
release's relative resource and license layout, omits its desktop executable,
and publishes only the CLI through a symlink in `/usr/local/bin`. PdfCraft's
bundled models remain under its release's `share/pdfcraft/models`, relative to
the real executable. Both model files must be nonempty, and startup checks
require `pdfcraft-cli run ocr_status` to report OCR available. The CLI-only PDF
archive omits those models, so Jbox deliberately uses the full distribution but
does not install its desktop executable. See the README's default-tooling
section for image size and the additional language toolchains.

The CLI binaries require glibc and `libgcc_s`, supplied by the Debian-based
Python runtime image and its native build dependencies. The installer uses
Python's standard library to download, hash and extract pinned archives. It
rejects traversal, symlinks, special files, duplicate regular files and oversized
archives before publishing a CLI. No ArtCraft source compilation, desktop
environment, X server, audio service, GPU device, FFmpeg, Tesseract or external
PDF renderer is required for the reviewed core CPU file workflows. Fonts and
codec implementations are bundled in the upstream binaries, but arbitrary
imported documents can still need additional fonts for faithful substitution.
The image also includes Fontconfig, DejaVu and Liberation fonts. Preserve all
upstream application, model, font and third-party license notices under each
release tree.

These conclusions follow the pinned release packaging scripts and real CLI
startup checks, not the desktop packages' broader dependency lists. Useful
upstream references:

- [Shared release workflow](https://github.com/storytold/craft-common-actions/blob/main/.github/workflows/release.yml)
- [PdfCraft 0.8.0 Linux packaging and OCR asset staging](https://github.com/storytold/pdfcraft/blob/v0.8.0/packaging/linux/package.sh)
- [FilmCraft 0.8.0 documentation](https://github.com/storytold/filmcraft/tree/v0.8.0)

Build-time startup checks fail closed on a bad checksum, missing CLI, missing
model directory or broken binary. Every CLI is checked with `--version` and
`--help`, except DeckCraft and CadCraft, whose `--help` exits 1 and is checked
with `commands` instead. Checks are bounded to 30 seconds per invocation and
repeated as the final non-root guest user with `--verify-only`. The version
manifest is installed at `/opt/artcraft/releases.json` for auditing.

Installer and manifest content participate in the base-image cache fingerprint,
so changing either triggers a new image. Existing running or retained guests
keep their original image. Rebuild the Jbox executable and start a new session
to receive the updated default environment. Project Dockerfiles using
`ARG JBOX_BASE_IMAGE` and `FROM ${JBOX_BASE_IMAGE}` inherit these tools.

## Headless use and isolation

Commands, flags and scripting schemas differ between apps. Most offer an opt-in
stdio MCP server through their `mcp` subcommand. Jbox installs the executables
only: it does not register MCP servers, open extra network listeners or connect
to a host desktop. Follow each tool's documented automation read/write-root
options when launching an agent-facing server.

Desktop bridge commands such as `app`, `ui` and `--connect` need a separately
running app and are not provided by this headless image. GPU rendering,
real-time audio, physical printing, external plugins and integrations such as
Zotero are not configured. LightCraft AI denoise additionally needs separately
installed model weights, which are not downloaded into the default image.
Offline rendering/export uses the CPU paths and
requires no relaxed sandbox privileges. Use writable workspace or temporary
paths for outputs rather than the read-only image filesystem.

For example, inside a newly created guest:

```bash
gridcraft-cli eval 'SUM(1,2,3)' --json
```

## Updating releases

Review official GitHub releases and packaging changes for each app. Update the
version and the exact Linux x86_64 tarball digest from upstream `SHA256SUMS.txt`
in `templates/artcraft-releases.json`, then update this version table. Do not
replace pinned URLs with `latest` or download and execute an unverified installer.
Run the repository quality gates plus a real installation into a disposable
prefix, inspect dynamic-library dependencies, and exercise representative
headless workflows, including PdfCraft OCR asset discovery. A complete Docker
build additionally verifies installation against a clean Debian base.

## Release refresh on 2026-10-24

All 13 applications had newer stable releases, published on 2026-10-22 or
2026-10-23. Each pinned Linux x86_64 archive's checksum was checked against both
the upstream `SHA256SUMS.txt` and GitHub's release asset digest. The default
package definitions in `templates/base.Dockerfile` were also reviewed for the
suite's runtime requirements. No additional Debian packages or changes to the
other toolchain pins were needed for this ArtCraft refresh.

A real installation into a disposable prefix on Debian 12 passed checksum
verification, safe extraction and bounded startup checks for every application.
The installed suite also passed `--verify-only` as the non-root guest user.
All ELF dependencies resolved to glibc and `libgcc_s`, and PdfCraft reported its
bundled OCR models available. GridCraft evaluated `SUM(1,2,3)` as `6.0`.
VectorCraft converted a two-path SVG to PDF and PNG without warnings, then
reopened the PDF with both paths and its 128-by-96 artboard intact. The installed
headless payload is approximately 826 MiB before image compression.

Repository formatting, Clippy with warnings denied, all 187 Rust tests, CLI
help, and all 30 offline Python installer regressions passed with the pinned
Rust 1.98.1 toolchain.

These are startup and representative workflow checks, not a new full-workflow
test for every app. Docker is unavailable in this isolated guest, so a clean
image build and fresh Kata session still require host-side acceptance for these
new pins, tracked in `jbox-mau`. Existing guests and their installed applications
are unchanged.

### Branch reconciliation on 2026-10-17

The reconciled implementation uses the JSON manifest and hardened Python
installer from `main`, with VectorCraft 0.12.0 and the bounded startup/OCR checks
from the transport branch. The former shell installer and text manifest were
consolidated into this single installation path. Offline regression tests cover
extraction safety, resource preservation, manifest/documentation consistency,
startup failures and OCR readiness. The historical real-install checks below
predate this consolidation. Clean Docker and fresh Kata acceptance subsequently
passed on this host, as recorded in [the acceptance report](acceptance-2026-10-17.md).
All 13 CLIs passed verification inside the new guest, along with spreadsheet
evaluation, PDF OCR readiness and VectorCraft SVG-to-PDF/PNG conversion. This
closes `jbox-10a` without claiming a new full-workflow test for every application.

## Previous validation on 2026-10-17

The latest-release review found only VectorCraft needed updating, from 0.11.0
to 0.12.0. Its Linux x86_64 tarball matched upstream `SHA256SUMS.txt` and the
GitHub release asset digest. The real installer completed in a disposable
prefix, all ELF dependencies resolved (`libc`, `libm` and `libgcc_s`), and
headless SVG-to-PDF and SVG-to-PNG conversion passed. Reopening the exported
PDF preserved the 128-by-96 artboard and both paths, with no import warnings.
The other 12 release pins remain unchanged. Existing guests are not modified.

### Previous full-suite validation on 2026-10-15

The real checksum-verified installer completed for all 13 pinned releases on
Debian 12 with no display. All binaries' ELF dependencies resolved to glibc and
`libgcc_s`. Representative headless workflows passed for every app: image,
vector and document conversion, PDF rendering and OCR, slide and CAD rendering,
video/composition frame rendering, audio bounce and spreadsheet evaluation.
Core workflow checks also ran with an empty executable search path to detect
hidden external-tool dependencies. PdfCraft discovered its installed models
and recognized text in an image-only PDF. Repository formatting, Clippy,
180 tests and the CLI help smoke check passed.

A clean Docker build and new Kata guest could not be exercised inside the
isolated development guest because Docker is unavailable. This host-only
acceptance check is tracked as Beads issue `jbox-10a` (replacing the unavailable
`jbox-wnm` record). No host runtime socket, device or credential mount was added
to work around that boundary.
