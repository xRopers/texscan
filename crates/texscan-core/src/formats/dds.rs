//! DirectDraw Surface (`.dds`): the `DDS ` magic, a 124-byte header, an optional
//! 20-byte DX10 header, then every mip of every face and array element.
//!
//! Recognised by the two fixed size fields (header 124, pixel format 32): with the magic
//! that is 12 exact bytes, so text such as "DDS files" never passes. The size is worked
//! out from dimensions, mips, faces, array size and the pixel format, the way DirectXTex
//! reads it. `dwPitchOrLinearSize` and most flags are ignored, since writers often get
//! them wrong; a mip count of 0 means 1.
//!
//! Legacy (D3D9) pixel formats keep their FourCC or D3D9 name (`DXT1`, `A8R8G8B8`), with
//! the matching DXGI format alongside; DX10-header files use DXGI names (`BC7_UNORM`).
//!
//! Palettized (P4, P8) files have a palette of 2^bits four-byte entries after the header;
//! [`TextureInfo::header_size`] includes it.

use crate::format::{Container, Reject, Storage, TextureFormat, TextureInfo, u32_le};
use crate::pixel::{self, Decode, DxgiError, Layout, PixelFormat};

pub struct Dds;

pub const MAGIC: &[u8; 4] = b"DDS ";
/// Magic plus the main header.
const BASE_SIZE: usize = 4 + 124;
const DX10_SIZE: usize = 20;

// Pixel format flags.
const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_ALPHA: u32 = 0x2;
const DDPF_PALETTEINDEXED4: u32 = 0x8;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_PALETTEINDEXED8: u32 = 0x20;
const DDPF_RGB: u32 = 0x40;
const DDPF_YUV: u32 = 0x200;
const DDPF_LUMINANCE: u32 = 0x2_0000;
const DDPF_BUMPLUMINANCE: u32 = 0x4_0000;
const DDPF_BUMPDUDV: u32 = 0x8_0000;

// dwCaps2.
const DDSCAPS2_CUBEMAP: u32 = 0x200;
const DDSCAPS2_CUBEMAP_FACES: u32 = 0xFC00;
const DDSCAPS2_VOLUME: u32 = 0x20_0000;

// DX10 header.
const DIMENSION_TEXTURE1D: u32 = 2;
const DIMENSION_TEXTURE2D: u32 = 3;
const DIMENSION_TEXTURE3D: u32 = 4;
const MISC_TEXTURECUBE: u32 = 0x4;

/// Direct3D's largest texture is 16384 wide; leave room for tools that go further.
const MAX_DIMENSION: u32 = 65536;
const MAX_ARRAY: u32 = 2048;

impl TextureFormat for Dds {
    fn container(&self) -> Container {
        Container::Dds
    }

    fn magic(&self) -> &'static [u8] {
        MAGIC
    }

    fn parse(&self, data: &[u8]) -> Result<TextureInfo, Reject> {
        parse(data)
    }
}

fn bad(reason: impl Into<String>) -> Reject {
    Reject::Bad(reason.into())
}

pub fn parse(data: &[u8]) -> Result<TextureInfo, Reject> {
    if data.len() < 8 || &data[..4] != MAGIC || u32_le(data, 4) != 124 {
        return Err(Reject::NoMatch);
    }
    if data.len() < BASE_SIZE {
        return Err(bad(format!("header truncated: {} of {BASE_SIZE} bytes", data.len())));
    }
    // Offsets below are from the start of the file (magic included).
    let field = |pos: usize| u32_le(data, pos);
    if field(76) != 32 {
        return Err(Reject::NoMatch);
    }
    let height = field(12);
    let width = field(16);
    let header_depth = field(24);
    let mips = field(28).max(1);
    let pf_flags = field(80);
    let fourcc: [u8; 4] = data[84..88].try_into().unwrap();
    let caps2 = field(112);

    let mut header_size = BASE_SIZE + palette_size(pf_flags);
    let mut depth = 1;
    let mut array_size = 1;
    let mut faces = 1;
    let pixel_format;

    if pf_flags & DDPF_FOURCC != 0 && &fourcc == b"DX10" {
        header_size = BASE_SIZE + DX10_SIZE;
        if data.len() < header_size {
            return Err(bad("DX10 header truncated"));
        }
        let format = field(128);
        pixel_format = match pixel::dxgi(format) {
            Ok(f) => f,
            Err(DxgiError::Unknown) => return Err(bad(format!("unknown DXGI format {format}"))),
            Err(DxgiError::Planar(name)) => return Err(bad(format!("planar format {name} is not supported"))),
        };
        array_size = field(140);
        if array_size == 0 || array_size > MAX_ARRAY {
            return Err(bad(format!("array size {array_size}")));
        }
        match field(132) {
            DIMENSION_TEXTURE1D | DIMENSION_TEXTURE2D => {
                if field(136) & MISC_TEXTURECUBE != 0 {
                    faces = 6;
                }
            }
            DIMENSION_TEXTURE3D => {
                depth = header_depth;
                if array_size != 1 {
                    return Err(bad("a volume texture can't be an array"));
                }
            }
            other => return Err(bad(format!("resource dimension {other}"))),
        }
    } else {
        pixel_format = legacy_format(pf_flags, fourcc, [field(88), field(92), field(96), field(100), field(104)])?;
        if caps2 & DDSCAPS2_VOLUME != 0 {
            depth = header_depth;
        } else if caps2 & DDSCAPS2_CUBEMAP != 0 {
            faces = (caps2 & DDSCAPS2_CUBEMAP_FACES).count_ones();
            if faces == 0 {
                return Err(bad("cube map with no faces"));
            }
        }
    }

    for (name, value) in [("width", width), ("height", height), ("depth", depth)] {
        if value == 0 || value > MAX_DIMENSION {
            return Err(bad(format!("{name} {value}")));
        }
    }
    let max_mips = 32 - width.max(height).max(depth).leading_zeros();
    if mips > max_mips {
        return Err(bad(format!("{mips} mips, but {width}x{height}x{depth} has at most {max_mips}")));
    }

    let data_size = data_size(pixel_format.layout, width, height, depth, mips)
        .checked_mul(u64::from(array_size) * u64::from(faces))
        .ok_or_else(|| bad("size overflows"))?;
    let size = header_size as u64 + data_size;
    if size > data.len() as u64 {
        return Err(bad(format!("truncated: needs {size} bytes, {} left in the file", data.len())));
    }
    Ok(TextureInfo {
        size,
        header_size: header_size as u64,
        width,
        height,
        depth,
        mips,
        array_size,
        faces,
        pixel_format,
        storage: Storage::Dds,
        orientation: Default::default(),
    })
}

/// Palettized formats keep their palette right after the header.
fn palette_size(pf_flags: u32) -> usize {
    if pf_flags & DDPF_FOURCC != 0 {
        0
    } else if pf_flags & DDPF_PALETTEINDEXED8 != 0 {
        256 * 4
    } else if pf_flags & DDPF_PALETTEINDEXED4 != 0 {
        16 * 4
    } else {
        0
    }
}

/// Bytes in one face or array element: every mip, each of `depth` slices, halving
/// each dimension per mip down to 1. Can't overflow: 65536³ × 128 bits is 2^52.
fn data_size(layout: Layout, width: u32, height: u32, depth: u32, mips: u32) -> u64 {
    (0..mips)
        .map(|i| layout.image_size((width >> i).max(1), (height >> i).max(1)) * u64::from((depth >> i).max(1)))
        .sum()
}

/// The pixel format of a file without a DX10 header. `masks` are bit count and the R,
/// G, B and A masks.
fn legacy_format(flags: u32, fourcc: [u8; 4], masks: [u32; 5]) -> Result<PixelFormat, Reject> {
    use Layout::*;
    let pf = |name, dxgi, layout| Ok(PixelFormat::new(name, dxgi, layout));
    if flags & DDPF_FOURCC != 0 {
        return match &fourcc {
            b"DXT1" => pf("DXT1", Some(71), Block { bytes: 8 }),
            b"DXT2" => pf("DXT2", Some(74), Block { bytes: 16 }),
            b"DXT3" => pf("DXT3", Some(74), Block { bytes: 16 }),
            b"DXT4" => pf("DXT4", Some(77), Block { bytes: 16 }),
            b"DXT5" => pf("DXT5", Some(77), Block { bytes: 16 }),
            // DXT5 with red and alpha swapped, from Doom 3's normal maps.
            b"RXGB" => Ok(PixelFormat::new("RXGB", None, Block { bytes: 16 }).with_decode(Decode::Rxgb)),
            b"ATI1" => pf("ATI1", Some(80), Block { bytes: 8 }),
            b"BC4U" => pf("BC4U", Some(80), Block { bytes: 8 }),
            b"BC4S" => pf("BC4S", Some(81), Block { bytes: 8 }),
            b"ATI2" => pf("ATI2", Some(83), Block { bytes: 16 }),
            b"BC5U" => pf("BC5U", Some(83), Block { bytes: 16 }),
            b"BC5S" => pf("BC5S", Some(84), Block { bytes: 16 }),
            // D3D9 names channels from the top bit down and DXGI from the bottom up, so
            // D3DFMT_R8G8_B8G8 is DXGI's G8R8_G8B8 (checked against Qt's test images).
            b"RGBG" => pf("RGBG", Some(69), Pair { bytes: 4 }),
            b"GRGB" => pf("GRGB", Some(68), Pair { bytes: 4 }),
            b"YUY2" => pf("YUY2", Some(107), Pair { bytes: 4 }),
            b"UYVY" => Ok(PixelFormat::new("UYVY", None, Pair { bytes: 4 }).with_decode(Decode::Uyvy)),
            // D3DFORMAT numbers stored as the FourCC.
            _ => match u32::from_le_bytes(fourcc) {
                36 => pf("A16B16G16R16", Some(11), Linear { bits: 64 }),
                110 => pf("Q16W16V16U16", Some(13), Linear { bits: 64 }),
                111 => pf("R16F", Some(54), Linear { bits: 16 }),
                112 => pf("G16R16F", Some(34), Linear { bits: 32 }),
                113 => pf("A16B16G16R16F", Some(10), Linear { bits: 64 }),
                114 => pf("R32F", Some(41), Linear { bits: 32 }),
                115 => pf("G32R32F", Some(16), Linear { bits: 64 }),
                116 => pf("A32B32G32R32F", Some(2), Linear { bits: 128 }),
                117 => pf("CxV8U8", None, Linear { bits: 16 }),
                n => Err(bad(match fourcc.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
                    true => format!("unknown FourCC {:?}", String::from_utf8_lossy(&fourcc)),
                    false => format!("unknown FourCC {n:#x}"),
                })),
            },
        };
    }
    if flags & DDPF_PALETTEINDEXED8 != 0 {
        let palette = Decode::Palette { offset: BASE_SIZE as u32 };
        return Ok(PixelFormat::new("P8", Some(113), Linear { bits: 8 }).with_decode(palette));
    }
    if flags & DDPF_PALETTEINDEXED4 != 0 {
        let palette = Decode::Palette { offset: BASE_SIZE as u32 };
        return Ok(PixelFormat::new("P4", None, Linear { bits: 4 }).with_decode(palette));
    }
    if flags & (DDPF_RGB | DDPF_LUMINANCE | DDPF_ALPHA | DDPF_YUV | DDPF_BUMPDUDV | DDPF_BUMPLUMINANCE) == 0 {
        return Err(bad(format!("no pixel format (flags {flags:#x})")));
    }
    let [bits, r, g, b, a] = masks;
    let a = if flags & (DDPF_ALPHAPIXELS | DDPF_ALPHA) != 0 { a } else { 0 };
    if !matches!(bits, 8 | 16 | 24 | 32) {
        return Err(bad(format!("{bits} bits per pixel")));
    }
    let limit = if bits == 32 { u32::MAX } else { (1 << bits) - 1 };
    let colour = [r, g, b, a];
    let overlap = (0..4).any(|i| (i + 1..4).any(|j| colour[i] & colour[j] != 0));
    if colour.iter().all(|&m| m == 0) || colour.iter().any(|&m| m & !limit != 0) || overlap {
        return Err(bad(format!("bad channel masks {r:#x} {g:#x} {b:#x} {a:#x} for {bits} bits")));
    }
    let layout = Linear { bits };
    let known = if flags & DDPF_BUMPDUDV != 0 {
        match (bits, r, g, b, a) {
            (16, 0xff, 0xff00, 0, 0) => Some(("V8U8", Some(51))),
            (32, 0xffff, 0xffff_0000, 0, 0) => Some(("V16U16", Some(37))),
            (32, 0xff, 0xff00, 0xff_0000, 0xff00_0000) => Some(("Q8W8V8U8", Some(31))),
            (32, 0x3ff0_0000, 0xffc00, 0x3ff, 0xc000_0000) => Some(("A2W10V10U10", None)),
            _ => None,
        }
    } else if flags & DDPF_BUMPLUMINANCE != 0 {
        match (bits, r, g, b) {
            (16, 0x1f, 0x3e0, 0xfc00) => Some(("L6V5U5", None)),
            (32, 0xff, 0xff00, 0xff_0000) => Some(("X8L8V8U8", None)),
            _ => None,
        }
    } else if flags & DDPF_LUMINANCE != 0 {
        match (bits, r, a) {
            (8, 0xff, 0) => Some(("L8", Some(61))),
            (16, 0xffff, 0) => Some(("L16", Some(56))),
            (16, 0xff, 0xff00) => Some(("A8L8", Some(49))),
            (8, 0x0f, 0xf0) => Some(("A4L4", None)),
            _ => None,
        }
    } else if flags & DDPF_RGB != 0 {
        match (bits, r, g, b, a) {
            (32, 0xff_0000, 0xff00, 0xff, 0xff00_0000) => Some(("A8R8G8B8", Some(87))),
            (32, 0xff_0000, 0xff00, 0xff, 0) => Some(("X8R8G8B8", Some(88))),
            (32, 0xff, 0xff00, 0xff_0000, 0xff00_0000) => Some(("A8B8G8R8", Some(28))),
            (32, 0xff, 0xff00, 0xff_0000, 0) => Some(("X8B8G8R8", None)),
            (32, 0x3ff, 0xffc00, 0x3ff0_0000, 0xc000_0000) => Some(("A2B10G10R10", Some(24))),
            (32, 0x3ff0_0000, 0xffc00, 0x3ff, 0xc000_0000) => Some(("A2R10G10B10", None)),
            (32, 0xffff, 0xffff_0000, 0, 0) => Some(("G16R16", Some(35))),
            (24, 0xff_0000, 0xff00, 0xff, 0) => Some(("R8G8B8", None)),
            (16, 0xf800, 0x7e0, 0x1f, 0) => Some(("R5G6B5", Some(85))),
            (16, 0x7c00, 0x3e0, 0x1f, 0x8000) => Some(("A1R5G5B5", Some(86))),
            (16, 0x7c00, 0x3e0, 0x1f, 0) => Some(("X1R5G5B5", None)),
            (16, 0xf00, 0xf0, 0xf, 0xf000) => Some(("A4R4G4B4", Some(115))),
            (16, 0xf00, 0xf0, 0xf, 0) => Some(("X4R4G4B4", None)),
            (16, 0xe0, 0x1c, 0x3, 0xff00) => Some(("A8R3G3B2", None)),
            (8, 0xe0, 0x1c, 0x3, 0) => Some(("R3G3B2", None)),
            _ => None,
        }
    } else if flags & DDPF_ALPHA != 0 && (bits, a) == (8, 0xff) {
        Some(("A8", Some(65)))
    } else {
        None
    };
    let (name, dxgi) = known.unwrap_or(match bits {
        8 => ("8-bit (other masks)", None),
        16 => ("16-bit (other masks)", None),
        24 => ("24-bit (other masks)", None),
        _ => ("32-bit (other masks)", None),
    });
    // Always decoded by mask: the DXGI equivalents are only equivalent in storage.
    let luminance = flags & DDPF_LUMINANCE != 0;
    // D3DX wrote A2W10V10U10's U and W masks swapped (U is the low 10 bits, as the name
    // says); DirectXTex's reference header has the same swap, so undo it for decoding.
    let (r, b) = if name == "A2W10V10U10" { (b, r) } else { (r, b) };
    // Bump maps store signed U, V (and W); bump-luminance keeps its L unsigned.
    let signed = if flags & DDPF_BUMPDUDV != 0 {
        0b0111
    } else if flags & DDPF_BUMPLUMINANCE != 0 {
        0b0011
    } else {
        0
    };
    Ok(PixelFormat::new(name, dxgi, layout).with_decode(Decode::Masks { r, g, b, a, luminance, signed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal legacy header; tests patch fields in place.
    fn header(width: u32, height: u32, mips: u32, fourcc: &[u8; 4]) -> Vec<u8> {
        let mut h = vec![0u8; BASE_SIZE];
        h[..4].copy_from_slice(MAGIC);
        let mut put = |pos: usize, v: u32| h[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
        put(4, 124);
        put(8, 0x1007);
        put(12, height);
        put(16, width);
        put(28, mips);
        put(76, 32);
        put(80, DDPF_FOURCC);
        h[84..88].copy_from_slice(fourcc);
        h
    }

    fn put(h: &mut [u8], pos: usize, v: u32) {
        h[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn dxt1_with_mips() {
        let mut d = header(16, 8, 5, b"DXT1");
        // 16x8, 8x4, 4x2, 2x1, 1x1 → 8 + 2 + 1 + 1 + 1 blocks.
        d.resize(BASE_SIZE + 13 * 8, 0);
        let info = parse(&d).unwrap();
        assert_eq!((info.size, info.width, info.height, info.mips), (d.len() as u64, 16, 8, 5));
        assert_eq!(info.pixel_format.name, "DXT1");
        // Trailing bytes aren't part of it.
        d.extend_from_slice(b"more");
        assert_eq!(parse(&d).unwrap().size, BASE_SIZE as u64 + 104);
    }

    #[test]
    fn truncated_and_wrong_headers() {
        let mut d = header(16, 16, 1, b"DXT5");
        d.resize(BASE_SIZE + 255, 0);
        assert!(matches!(parse(&d), Err(Reject::Bad(r)) if r.starts_with("truncated")));
        assert!(matches!(parse(&d[..60]), Err(Reject::Bad(r)) if r.starts_with("header truncated")));
        assert_eq!(parse(b"DDS files are textures, and this is some text about them"), Err(Reject::NoMatch));
        let mut wrong_pf = d.clone();
        put(&mut wrong_pf, 76, 0);
        assert_eq!(parse(&wrong_pf), Err(Reject::NoMatch));
        assert!(matches!(parse(&header(16, 16, 1, b"ZZZZ")), Err(Reject::Bad(r)) if r.contains("\"ZZZZ\"")));
        assert!(matches!(parse(&header(16, 16, 6, b"DXT1")), Err(Reject::Bad(r)) if r.contains("at most 5")));
        assert!(matches!(parse(&header(0, 16, 1, b"DXT1")), Err(Reject::Bad(r)) if r == "width 0"));
    }

    #[test]
    fn palettized_includes_the_palette() {
        let mut d = header(4, 4, 1, b"\0\0\0\0");
        put(&mut d, 80, DDPF_PALETTEINDEXED8);
        put(&mut d, 88, 8);
        d.resize(BASE_SIZE + 1024 + 16, 0);
        let info = parse(&d).unwrap();
        assert_eq!((info.pixel_format.name, info.header_size, info.size), ("P8", 128 + 1024, d.len() as u64));
    }

    #[test]
    fn dx10_cube_array() {
        let mut d = header(8, 8, 4, b"DX10");
        d.resize(BASE_SIZE + DX10_SIZE, 0);
        put(&mut d, 128, 98); // BC7_UNORM
        put(&mut d, 132, DIMENSION_TEXTURE2D);
        put(&mut d, 136, MISC_TEXTURECUBE);
        put(&mut d, 140, 2);
        // Per face: 4 + 1 + 1 + 1 blocks of 16 bytes; 12 faces.
        d.resize(BASE_SIZE + DX10_SIZE + 7 * 16 * 12, 0);
        let info = parse(&d).unwrap();
        assert_eq!((info.faces, info.array_size, info.header_size), (6, 2, 148));
        assert_eq!(info.size, d.len() as u64);
        assert_eq!(info.pixel_format.name, "BC7_UNORM");
        assert_eq!(info.dimensions(), "8x8 cube [2]");
        put(&mut d, 128, 103);
        assert!(matches!(parse(&d), Err(Reject::Bad(r)) if r.contains("NV12")));
    }

    #[test]
    fn legacy_rgb_volume() {
        let mut d = header(4, 4, 3, b"\0\0\0\0");
        put(&mut d, 80, DDPF_RGB | DDPF_ALPHAPIXELS);
        put(&mut d, 88, 32);
        for (pos, mask) in [(92, 0xff_0000), (96, 0xff00), (100, 0xff), (104, 0xff00_0000)] {
            put(&mut d, pos, mask);
        }
        put(&mut d, 24, 4);
        put(&mut d, 112, DDSCAPS2_VOLUME);
        // 4x4x4, 2x2x2, 1x1x1 at 4 bytes a pixel.
        d.resize(BASE_SIZE + (64 + 8 + 1) * 4, 0);
        let info = parse(&d).unwrap();
        assert_eq!((info.pixel_format.name, info.pixel_format.dxgi, info.depth), ("A8R8G8B8", Some(87), 4));
        assert_eq!(info.size, d.len() as u64);
        // Overlapping masks are rejected.
        put(&mut d, 96, 0xff_ff00);
        assert!(matches!(parse(&d), Err(Reject::Bad(r)) if r.starts_with("bad channel masks")));
    }
}
