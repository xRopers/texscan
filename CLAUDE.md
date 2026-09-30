# texscan — texture scanner, extractor and packer

## Goal
Find standard textures (DDS first, then KTX, PNG and others) inside arbitrary binary files — game archives, extracted blobs, memory dumps — extract them, and reinject edited versions, the way packzip does for zlib. One fast CLI, manifest-driven, with verification; a GUI later on the same core library. A sibling of zscan (`E:\zscan`, github.com/xRopers/zscan): same structure, conventions and style.

## Language and key crates
- Rust workspace, edition 2024, MSRV 1.89, license GPL-2.0-or-later (like zscan)
- `memchr` (magic search), `memmap2` (large inputs), `rayon`, `clap`, `serde` + `serde_json` (manifest), `crc32fast`, `thiserror`/`anyhow`
- Later: a BC1–7 / ETC / ASTC decoder for previews and PNG export, and a BC encoder for importing edits. Check licenses against GPL-2.0-or-later before adding

## Layout
```
texscan/
  crates/
    texscan-core/       # library: formats, scan, manifest, extract (later pack)
      src/formats/      # one file per container format: dds.rs, (ktx.rs, png.rs ...)
      src/pixel.rs      # pixel formats (DXGI table + legacy D3D9) and their byte layouts
    texscan-cli/        # clap CLI, binary `texscan`
    texscan-fixtures/   # deterministic fixtures; `gen-fixtures` writes tests/fixtures/
  tests/fixtures/       # generated .bin + .expected.json, checked in
```
The core must never depend on the CLI or GUI.

## CLI
```
texscan scan    <file> [-o manifest.json] [--show-rejected] [--formats dds]
texscan extract <file> [-m manifest.json] -d out/ [--force]
```
All commands support `--json`. Planned: `pack`, `try --at OFF --format F` (with `--width/--height/--mips/--pixel-format` for headerless textures), `fields`, `info`.

## How it differs from zscan
- Textures aren't compressed streams: a header gives dimensions, mips, faces, array size and pixel format, and the size follows from those. Nothing is decoded to find the end.
- Unedited textures are copied byte for byte, so there's no parameter matching. The hard part is importing edits: a PNG re-encoded to the original pixel format and mip count comes out exactly the original size, so it always fits.
- Many engines (Unity Texture2D, Unreal `.uasset`/`.ubulk`) store raw pixel data with no DDS header. Magic scanning can't find those; they need `try` with user-given dimensions and format, and perhaps a heuristic scanner later (like zscan's Oodle: lean on user-supplied info).

## Build order
1. Workspace, `TextureFormat` trait, DDS scan + manifest + extract, fixtures. **(done)**
2. Decode to RGBA (BCn and uncompressed) for PNG export and previews.
3. Pack: import a PNG, encode to the original format with mips, same size, verify; or reinject an edited `.dds` of the same size and format. Temp file, verify, rename (as zscan's `output.rs`).
4. More formats (KTX1/2, PNG, JPEG, BMP, PVR3, ASTC, VTF, WebP; TGA opt-in, no magic), length fields and relocation (port zscan's `fields.rs`), parallel chunked scanning for multi-GB files.
5. Headerless textures: `try` with explicit layout, then heuristics.
6. GUI (egui, like zscan-gui): thumbnail grid, table, before/after preview, pack window.
7. Optionally scan inside compressed streams by depending on `zscan-core`.

## Status
- Stage 1 done: `texscan scan | extract`.
- `TextureFormat` (`format.rs`): `magic()` + `parse(data from offset to EOF) -> Result<TextureInfo, Reject>`. `Reject::NoMatch` is silent (magic bytes in text); `Reject::Bad(reason)` is reported (`ScanReport::rejected`, `--show-rejected`), e.g. unknown FourCC or truncated. A texture is accepted only if it fits in the file.
- Scan (`scan.rs`): `memmem` for each format's magic, parse candidates in parallel, then keep them in file order, skipping candidates inside a found texture. 1 GiB in 0.87 s, mostly the whole-file CRC.
- DDS (`formats/dds.rs`): recognised by the header size 124 and pixel-format size 32. Size = header (+20 DX10, + palette for P4/P8) + every mip × faces × array, as DirectXTex computes it; pitch/linear size and most flags are ignored, and a mip count of 0 means 1. Legacy files keep FourCC/D3D9 names (`DXT1`, `A8R8G8B8`) with the matching DXGI number; DX10 files use DXGI names. Planar DXGI formats (NV12...) are rejected for now.
- Checked against all 123 real `.dds` files on this machine (Qt's DDS test images cover most legacy formats, plus Visual Studio and UE samples): every one parses to exactly its file length. These files aren't in the repo.
- Manifest v1 (`manifest.rs`): per texture id, offset, size, format, header_size, dimensions, mips, array_size/faces (omitted when 1), pixel_format, dxgi_format, crc32 of the whole texture, file (`{offset:08x}.dds`). Extract checks the input's size and CRC (`--force` skips that) and each texture's CRC.
- Fixtures: `texscan-fixtures` writes DDS headers and computes sizes independently of the core. `dds_archive` covers legacy/DX10, mips, mip count 0, cube (DX10 and legacy), volume, array, non-power-of-two, back-to-back, a valid DDS inside another's pixel data (must be skipped), and traps (text, wrong pixel-format size, unknown FourCC, truncated). `checked_in_fixtures_are_current` fails if `tests/fixtures` is stale: `cargo run -p texscan-fixtures --bin gen-fixtures`.
- cargo is at `%USERPROFILE%\.cargo\bin`, not on the Git Bash PATH; use `~/.cargo/bin/cargo` or PowerShell. `cargo test --workspace`; CI runs clippy with `-D warnings`.

## Working notes
- Prefer direct, plain explanations with concrete next steps.
- Write tests alongside each stage; fixtures should cover each format, false-positive traps, and a pack round trip.
