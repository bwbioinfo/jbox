# ArtCraft refresh acceptance on 2026-11-02

## Scope and provenance

The implementation refreshes all 13 previously bundled tools and adds released
TextureCraft 0.2.0, Craft Anything 0.3.0 and community SolidCraft 0.30.1, for
16 headless CLIs. The exact version, official release URL, archive path and SHA-256
for each artifact are recorded in `templates/artcraft-releases.json` and the
[version matrix](artcraft.md). Upstream releases and published checksums were
reviewed on 2026-11-02. The newer Craft Anything and SolidCraft releases were
confirmed against the live official release API, not cached `releases/latest`
search results.

SolidCraft is released by `tangency-labs/solidcraft`, an application linked from
the official ArtCraft suite catalog. The installer now explicitly trusts that
owner for SolidCraft only. All other applications remain restricted to their
individual `storytold` repositories. Unknown applications and cross-owner URLs
are rejected. Existing archive traversal, link, special-file, checksum, size and
startup safeguards remain in place.

PdfCraft 0.17.0 changes the bundled OCR model filenames from `.rten` to `.onnx`.
The installer and regression fixtures were updated together, and the real CLI
reported OCR available with both nonempty ONNX models in the preserved resource
layout.

## Real release-artifact checks

The production installer downloaded, checksum-verified, safely extracted and
started the released binaries in a disposable writable prefix in this guest.
The original 15 were installed together, then SolidCraft was installed using
the same installer functions after review expanded the trusted-owner map. A final
`--verify-only` against the complete 16-entry manifest passed as the unprivileged
`jbox` account. No installed tool in the running guest's `/usr/local/bin` was
replaced.

- All 16 advertised CLI versions matched the manifest.
- All 16 ELF dependency inspections resolved their libraries. The directly linked
  runtime dependencies were the system C library, math library and `libgcc_s`.
- GridCraft evaluated `SUM(1,2,3)` to `6.0`.
- PdfCraft returned `available: true` from `run ocr_status`. A separate real
  inference check rasterized original text artwork with VectorCraft, embedded
  the bitmap into an image-only PDF, confirmed empty extracted text before OCR,
  and ran the PdfCraft OCR batch step. It recognized three words and the saved
  PDF extracted exactly `ARTCRAFT OCR TEST`. VectorCraft reported a benign
  DejaVu Sans Regular-to-Book font substitution when rasterizing the artwork.
  The ONNX model attribution and full CC-BY-SA-4.0 notices were also inspected.
- TextureCraft created a built-in crate sample with 128-pixel texture sets,
  inspected the saved `.tcraft`, rendered a 128-by-96 PNG with four CPU samples,
  and exported the texture maps. The PNG signature/dimensions were checked and
  the nonblank crate image was visually inspected. This low-sample thumbnail
  checks the CPU path, not production render quality. Texture/export resolution
  must be at least 128 in this release.
- Craft Anything reported clean-room capabilities and created/read a `.cany`
  investigation session using `core-session-show`. No coding-agent backend,
  target execution, device access, external inspection engine installation or
  paid API was enabled. Missing optional engines remain capability limitations.
- The exact new SolidCraft skill example created a 40-by-30-by-20 rectangular
  solid, saved `.sc`, exported STEP and reopened/evaluated the design. It reported
  6 faces, 12 edges, 8 vertices and volume 24,000 cubic millimetres.

The installed payload measured approximately **1,146 MiB before image
compression**, compared with the previous 826 MiB measurement. These measurements
are guest filesystem usage, not Docker layer-transfer sizes.

## Skills integration

The sibling repository `/workspace/skills` adds `texturecraft`, `craft-anything`,
`solidcraft`, `materialcraft` and `craft-launcher`, each with `SKILL.md` and
`agents/openai.yaml`. Its dated `references/artcraft-tools.md` records reviewed
source revisions and authoritative documentation. The collection index was
reconciled to the actual 77 skills.

Both collection lint and the repository validator wrapper passed all 77 skills.
The wrapper's expected upstream OpenAI `quick_validate.py` was absent from the
guest, so an unmodified upstream copy was downloaded to disposable scratch and
selected through `SKILL_VALIDATOR`. Its SHA-256 was
`6cc9dc3199c935916cf6f73fcbbbb0e3bb1b58c8f5109fefa499978908164f51`.
PyYAML 6.0.3 was installed only in an isolated validation virtual environment,
not in the base image or either repository's dependency manifests.

MaterialCraft had source and documented CLI/MCP workflows but no published
release: the official release list was empty and latest returned HTTP 404.
Its skill documents that limitation. A default-container pin is deferred in
`jbox-20n` until a verifiable artifact exists. ArtCraft Launcher and ArtCraft Studio
are distinct GUI applications and are not installed in this headless image.

## Source quality gates and environment

The guest had no Docker or Slurm executable. Its home and temporary mounts are
`noexec`, while the approved workspace mount supports executable build outputs.
The repository-pinned Rust 1.98.1 toolchain was copied from the guest-managed
installation into ignored workspace build output. Executable test fixtures were
placed under that same approved output tree using `TMPDIR`. No mount flags,
privileges, reference data, host devices or credentials were changed.

The first default-parallel Rust test runs passed 199 of 200 library tests but
hit the existing one-second bootstrap mock deadline: the test expected export
exit status 9 and observed timeout 124. The focused test passed. An executable
`TMPDIR` alone did not eliminate the parallel-load failure. This is tracked
separately in `jbox-43w`; final validation uses `RUST_TEST_THREADS=1` rather than
weakening a production timeout or changing unrelated lifecycle code.

Final validation passed `cargo fmt --check`, Clippy for all targets with warnings
denied, all **215 Rust tests** across the library, binary and integration targets,
CLI help, and all **39 offline Python tests**. The full run used pinned Rust
1.98.1, an executable workspace `TMPDIR`, and `RUST_TEST_THREADS=1`. The independent
skills changes were committed as `8d5226a` in `/workspace/skills`. Durable results
and remaining acceptance work are recorded in `jbox-ggb.1` and `jbox-ggb.2`.

## Remaining acceptance boundary

This is real release-artifact and representative workflow validation, not a
clean Docker build or full workflow coverage for every application. A clean
image build and fresh Kata guest still require an authorized host environment
and are tracked in `jbox-ggb.2`. The current guest and its preinstalled suite
remain unchanged. Build the updated Jbox executable and start a fresh session
to receive the refreshed image. Refresh the host-managed skills snapshot through
the normal Jbox workflow to receive the sibling repository's new skills.
