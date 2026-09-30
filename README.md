# texscan

Find textures inside any binary file and extract them.

Many games keep standard DDS textures, headers and all, inside their own archive formats. texscan finds them by their headers, works out each texture's exact size from its dimensions, mip levels and pixel format, and writes them out as ordinary `.dds` files. No engine-specific tool needed.

It's a sibling of [zscan](https://github.com/xRopers/zscan), which does the same for compressed streams, and works the same way: a scan writes a JSON manifest, and later steps work from it.

**Status: early.** Scanning and extracting DDS textures work. Putting edited textures back (like packzip), more formats (KTX, PNG and others) and a desktop app are next.

## Build

Rust 1.89 or later:

```bash
cargo build --release
```

## Usage

```bash
texscan scan game.pak -o manifest.json      # list textures, write a manifest
texscan extract game.pak -m manifest.json -d textures/
```

```
      OFFSET        SIZE  FORMAT  DIMENSIONS          MIPS  PIXEL FORMAT
        0x72       21992  dds     256x128                9  DXT1
      0x567f        4224  dds     64x64                  1  DXT5
      0x66ff        2816  dds     32x16                  3  A8R8G8B8
      0x7204       22020  dds     128x128                8  BC7_UNORM_SRGB
...
```

Every command takes `--json`. The input is never modified. `extract` refuses a file that no longer matches the manifest (`--force` overrides).

`--show-rejected` lists headers that look like DDS but can't be used, and why (for example an unknown pixel format, or a texture cut off by the end of the file).

## DDS support

- Legacy headers: DXT1–5, ATI1/ATI2, BC4/BC5, RXGB, RGB, luminance, alpha, bump-map, palettized (P4/P8) and D3D9 float formats.
- DX10 headers: every DXGI format except planar video formats (NV12 and similar).
- Mipmaps, cube maps, volume textures and texture arrays.

Checked against 123 real DDS files covering most legacy formats: each one's size is worked out exactly.

Textures stored without a header can't be found this way. That includes Unity's Texture2D and Unreal's `.uasset`/`.ubulk`, which keep raw pixel data. A later version will let you describe them by hand.

## License

GPL-2.0-or-later. See [LICENSE](LICENSE).
