# ArtCraft headless tools in Jbox

The default x86_64 Debian guest includes all 13 ArtCraft command-line tools.
The suite was reviewed against the official [ArtCraft apps](https://getartcraft.com/apps)
and [storytold repositories](https://github.com/storytold) on 2026-10-15.
GitHub release tags, not the website's occasionally older download labels, are
the source of the pinned versions below.

| Command | Version | Main workflows |
| --- | --- | --- |
| `photocraft-cli` | 0.9.0 | Raster images, layered documents, conversion and batch actions |
| `vectorcraft-cli` | 0.11.0 | Vector illustration and SVG, PDF and raster export |
| `filmcraft-cli` | 0.8.0 | Video timelines, media inspection, rendering and encoding |
| `lightcraft-cli` | 0.8.0 | Photo libraries, RAW development and export |
| `pdfcraft-cli` | 0.8.0 | PDF inspection, text, editing, rendering and OCR |
| `effectcraft-cli` | 0.10.0 | Compositing, animation, effects and frame/video rendering |
| `designcraft-cli` | 0.8.0 | Page layout, document scripting and publishing |
| `figcraft-cli` | 0.6.0 | Interface design documents, conversion and frame export |
| `wordcraft-cli` | 0.7.0 | Word-processing documents, conversion and rendering |
| `soundcraft-cli` | 0.7.0 | Audio files, session editing and offline bounce |
| `deckcraft-cli` | 0.7.0 | Slide decks, rendering and conversion |
| `cadcraft-cli` | 0.7.0 | CAD drawings, inspection and interchange |
| `gridcraft-cli` | 0.7.0 | Spreadsheets, formulas and workbook interchange |

## Installation and dependencies

`templates/artcraft-releases.txt` pins every official Linux x86_64 release archive
by version and SHA-256. `templates/install-artcraft.sh` verifies the checksum
before extracting the CLI and license documentation into `/usr/local`. It also
copies PdfCraft's bundled OCR models to `/usr/local/share/pdfcraft/models`, the
location resolved relative to `/usr/local/bin/pdfcraft-cli`. Both model files
must be nonempty, and the build checks that `pdfcraft-cli run ocr_status` reports
OCR available. The installed tools, documentation and models occupy about
783 MiB in total for these releases. The CLI-only PDF
archive omits those models, so Jbox deliberately uses the full distribution but
does not install its desktop executable.

The CLI binaries require glibc and `libgcc_s`, supplied explicitly by Debian's
`libc6` and `libgcc-s1` packages. The installer uses the base image's `curl`, CA
certificates, `tar`, `gzip` and standard core utilities. No source compilation,
Node/Python runtime, desktop environment, X server, audio service, GPU device,
FFmpeg, Tesseract or external PDF renderer is required for the reviewed core
CPU file workflows. Fonts and codec implementations are bundled in the upstream
binaries, but arbitrary imported documents can still need additional fonts for
faithful substitution. Preserve
upstream license notices under `/usr/local/share/doc/<app>`.

These conclusions follow the pinned release packaging scripts and real CLI
startup checks, not the desktop packages' broader dependency lists. Useful
upstream references:

- [Shared release workflow](https://github.com/storytold/craft-common-actions/blob/main/.github/workflows/release.yml)
- [PdfCraft 0.8.0 Linux packaging and OCR asset staging](https://github.com/storytold/pdfcraft/blob/v0.8.0/packaging/linux/package.sh)
- [FilmCraft 0.8.0 documentation](https://github.com/storytold/filmcraft/tree/v0.8.0)

Build-time startup checks fail closed on a bad checksum, missing CLI, missing
model directory or broken binary. Every CLI is checked with `--version` and
`--help`, except DeckCraft and CadCraft, whose `--help` exits 1 and is checked
with `commands` instead. The version manifest is also installed at
`/usr/local/share/doc/artcraft-releases.txt` for auditing.

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
in `templates/artcraft-releases.txt`, then update this version table. Do not
replace pinned URLs with `latest` or download and execute an unverified installer.
Run the repository quality gates plus a real installation into a disposable
prefix, inspect dynamic-library dependencies, and exercise representative
headless workflows, including PdfCraft OCR asset discovery. A complete Docker
build additionally verifies installation against a clean Debian base.

## Validation on 2026-10-15

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
acceptance check is tracked as Beads issue `jbox-wnm`. No host runtime socket,
device or credential mount was added to work around that boundary.
