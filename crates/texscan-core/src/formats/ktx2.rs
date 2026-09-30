//! KTX 2.0 (`.ktx2`), Khronos's container for Vulkan-era textures: a 12-byte identifier,
//! a header, a level index giving each mip's offset and length, then the data format
//! descriptor, key/value data, supercompression data and the mip levels (usually
//! smallest first). Within a level, images go layer by layer, face by face, slice by
//! slice.
//!
//! Pixel formats are Vulkan `VkFormat` numbers, named here without the `VK_FORMAT_`
//! prefix and mapped to the matching DXGI format where there is one, so BC and the common
//! uncompressed formats decode and pack like DDS. ETC2, EAC and ASTC are sized and found
//! but not decoded yet; neither are supercompressed levels (BasisLZ, Zstandard, zlib, or
//! a newer scheme such as Basis Universal's UASTC HDR 6x6 intermediate), whose size
//! comes from the level index alone.
//!
//! Checked against the Khronos KTX-Software test files: every one parses to exactly its
//! own length.

use crate::format::{Container, Orientation, Reject, Storage, TextureFormat, TextureInfo, u32_le, u64_le};
use crate::pixel::{self, Decode, Layout, PixelFormat};

pub struct Ktx2;

pub const MAGIC: &[u8; 12] = b"\xABKTX 20\xBB\r\n\x1A\n";
const HEADER: usize = 80;
const LEVEL_ENTRY: usize = 24;
const MAX_DIMENSION: u32 = 65536;
const MAX_LAYERS: u32 = 2048;

impl TextureFormat for Ktx2 {
    fn container(&self) -> Container {
        Container::Ktx2
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
    if !data.starts_with(MAGIC) {
        return Err(Reject::NoMatch);
    }
    if data.len() < HEADER {
        return Err(bad(format!("header truncated: {} of {HEADER} bytes", data.len())));
    }
    let field = |pos: usize| u32_le(data, pos);
    let vk_format = field(12);
    let width = field(20);
    let height = field(24).max(1);
    let depth = field(28).max(1);
    let array_size = field(32).max(1);
    let faces = field(36);
    let mips = field(40).max(1);
    let supercompression = field(44);

    if width == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION || depth > MAX_DIMENSION {
        return Err(bad(format!("size {width}x{height}x{depth}")));
    }
    if faces != 1 && faces != 6 {
        return Err(bad(format!("{faces} faces (must be 1 or 6)")));
    }
    if faces == 6 && (depth > 1 || width != height) {
        return Err(bad(format!("a cube map must be square and 2D, not {width}x{height}x{depth}")));
    }
    if array_size > MAX_LAYERS {
        return Err(bad(format!("{array_size} layers")));
    }
    let max_mips = 32 - width.max(height).max(depth).leading_zeros();
    if mips > max_mips {
        return Err(bad(format!("{mips} mips, but {width}x{height}x{depth} has at most {max_mips}")));
    }

    let index_end = HEADER + mips as usize * LEVEL_ENTRY;
    if data.len() < index_end {
        return Err(bad("level index truncated"));
    }
    let mut end = index_end as u64;
    let mut extend = |what: &str, offset: u64, length: u64| -> Result<(), Reject> {
        if length == 0 {
            return Ok(());
        }
        let stop = offset.checked_add(length).ok_or_else(|| bad(format!("{what} overflows")))?;
        if stop > data.len() as u64 {
            return Err(bad(format!("truncated: {what} ends at {stop}, {} bytes left in the file", data.len())));
        }
        end = end.max(stop);
        Ok(())
    };
    extend("data format descriptor", u64::from(field(48)), u64::from(field(52)))?;
    extend("key/value data", u64::from(field(56)), u64::from(field(60)))?;
    extend("supercompression data", u64_le(data, 64), u64_le(data, 72))?;

    let pixel_format = vk_format_info(vk_format, supercompression);
    let mut levels = Vec::with_capacity(mips as usize);
    for m in 0..mips {
        let at = HEADER + m as usize * LEVEL_ENTRY;
        let (offset, length) = (u64_le(data, at), u64_le(data, at + 8));
        if offset < index_end as u64 {
            return Err(bad(format!("mip {m} starts at {offset}, inside the header")));
        }
        extend(&format!("mip {m}"), offset, length)?;
        if supercompression == 0 && !matches!(pixel_format.layout, Layout::Unknown) {
            let (w, h, d) = ((width >> m).max(1), (height >> m).max(1), (depth >> m).max(1));
            let expected = pixel_format.layout.image_size(w, h) * u64::from(d) * u64::from(array_size * faces);
            if length != expected {
                return Err(bad(format!("mip {m} holds {length} bytes, but {} needs {expected}", pixel_format.name)));
            }
        }
        levels.push((offset, length));
    }
    let header_size = levels.iter().filter(|l| l.1 > 0).map(|l| l.0).min().unwrap_or(index_end as u64);
    let (kvd_at, kvd_len) = (field(56) as usize, field(60) as usize);
    let orientation = orientation(&data[kvd_at..kvd_at + kvd_len]);
    Ok(TextureInfo {
        size: end,
        header_size,
        width,
        height,
        depth,
        mips,
        array_size,
        faces,
        pixel_format,
        storage: Storage::Ktx2 { levels, supercompression },
        orientation,
    })
}

/// The `KTXorientation` entry of the key/value data: `r` or `l` for x, `d` or `u` for y
/// (then `o` or `i` for z, which isn't used). Missing means `rd`, the usual way.
fn orientation(kvd: &[u8]) -> Orientation {
    let mut rest = kvd;
    while rest.len() >= 4 {
        let len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        let Some(pair) = rest.get(4..4 + len) else { break };
        if let Some(value) = pair.strip_prefix(b"KTXorientation\0") {
            let value = value.split(|&b| b == 0).next().unwrap_or(&[]);
            return Orientation { flip_x: value.first() == Some(&b'l'), flip_y: value.get(1) == Some(&b'u') };
        }
        rest = rest.get((4 + len).next_multiple_of(4)..).unwrap_or(&[]);
    }
    Orientation::default()
}

/// ASTC HDR (`VK_EXT_texture_compression_astc_hdr`): 1000066000 + i for the i-th size.
const ASTC_HDR: [&str; 14] = [
    "ASTC_4x4_SFLOAT_BLOCK",
    "ASTC_5x4_SFLOAT_BLOCK",
    "ASTC_5x5_SFLOAT_BLOCK",
    "ASTC_6x5_SFLOAT_BLOCK",
    "ASTC_6x6_SFLOAT_BLOCK",
    "ASTC_8x5_SFLOAT_BLOCK",
    "ASTC_8x6_SFLOAT_BLOCK",
    "ASTC_8x8_SFLOAT_BLOCK",
    "ASTC_10x5_SFLOAT_BLOCK",
    "ASTC_10x6_SFLOAT_BLOCK",
    "ASTC_10x8_SFLOAT_BLOCK",
    "ASTC_10x10_SFLOAT_BLOCK",
    "ASTC_12x10_SFLOAT_BLOCK",
    "ASTC_12x12_SFLOAT_BLOCK",
];

const ASTC: [(u32, u32, &str, &str); 14] = [
    (4, 4, "ASTC_4x4_UNORM_BLOCK", "ASTC_4x4_SRGB_BLOCK"),
    (5, 4, "ASTC_5x4_UNORM_BLOCK", "ASTC_5x4_SRGB_BLOCK"),
    (5, 5, "ASTC_5x5_UNORM_BLOCK", "ASTC_5x5_SRGB_BLOCK"),
    (6, 5, "ASTC_6x5_UNORM_BLOCK", "ASTC_6x5_SRGB_BLOCK"),
    (6, 6, "ASTC_6x6_UNORM_BLOCK", "ASTC_6x6_SRGB_BLOCK"),
    (8, 5, "ASTC_8x5_UNORM_BLOCK", "ASTC_8x5_SRGB_BLOCK"),
    (8, 6, "ASTC_8x6_UNORM_BLOCK", "ASTC_8x6_SRGB_BLOCK"),
    (8, 8, "ASTC_8x8_UNORM_BLOCK", "ASTC_8x8_SRGB_BLOCK"),
    (10, 5, "ASTC_10x5_UNORM_BLOCK", "ASTC_10x5_SRGB_BLOCK"),
    (10, 6, "ASTC_10x6_UNORM_BLOCK", "ASTC_10x6_SRGB_BLOCK"),
    (10, 8, "ASTC_10x8_UNORM_BLOCK", "ASTC_10x8_SRGB_BLOCK"),
    (10, 10, "ASTC_10x10_UNORM_BLOCK", "ASTC_10x10_SRGB_BLOCK"),
    (12, 10, "ASTC_12x10_UNORM_BLOCK", "ASTC_12x10_SRGB_BLOCK"),
    (12, 12, "ASTC_12x12_UNORM_BLOCK", "ASTC_12x12_SRGB_BLOCK"),
];

/// The pixel format for a `VkFormat`. Unknown formats (and `UNDEFINED`, used by Basis
/// Universal) get an unknown layout: the texture is still found, by its level index, but
/// can't be decoded or checked.
pub fn vk_format_info(vk: u32, supercompression: u32) -> PixelFormat {
    use Layout::*;
    let dxgi = |name: &'static str, id: u32| {
        let layout = pixel::dxgi(id).expect("mapped DXGI formats exist").layout;
        PixelFormat::new(name, Some(id), layout)
    };
    let masks = |name: &'static str, bits: u32, [r, g, b, a]: [u32; 4]| {
        PixelFormat::new(name, None, Linear { bits }).with_decode(Decode::Masks { r, g, b, a, luminance: false, signed: 0 })
    };
    let opaque = |name: &'static str, layout: Layout| PixelFormat::new(name, None, layout);
    let channels = |name: &'static str, bits: u32, spec: &'static str| {
        PixelFormat::new(name, None, Linear { bits }).with_decode(Decode::Channels(spec))
    };
    match vk {
        0 if supercompression == 1 => opaque("UNDEFINED (Basis Universal ETC1S)", Unknown),
        0 => opaque("UNDEFINED (see the data format descriptor)", Unknown),
        2 => masks("R4G4B4A4_UNORM_PACK16", 16, [0xf000, 0x0f00, 0x00f0, 0x000f]),
        3 => masks("B4G4R4A4_UNORM_PACK16", 16, [0x00f0, 0x0f00, 0xf000, 0x000f]),
        4 => dxgi("R5G6B5_UNORM_PACK16", 85),
        5 => masks("B5G6R5_UNORM_PACK16", 16, [0x001f, 0x07e0, 0xf800, 0]),
        6 => masks("R5G5B5A1_UNORM_PACK16", 16, [0xf800, 0x07c0, 0x003e, 0x0001]),
        7 => masks("B5G5R5A1_UNORM_PACK16", 16, [0x003e, 0x07c0, 0xf800, 0x0001]),
        8 => dxgi("A1R5G5B5_UNORM_PACK16", 86),
        9 => dxgi("R8_UNORM", 61),
        10 => dxgi("R8_SNORM", 63),
        13 => dxgi("R8_UINT", 62),
        14 => dxgi("R8_SINT", 64),
        15 => PixelFormat::new("R8_SRGB", None, Linear { bits: 8 })
            .with_decode(Decode::Masks { r: 0xff, g: 0, b: 0, a: 0, luminance: true, signed: 0 }),
        16 => dxgi("R8G8_UNORM", 49),
        17 => dxgi("R8G8_SNORM", 51),
        20 => dxgi("R8G8_UINT", 50),
        21 => dxgi("R8G8_SINT", 52),
        22 => masks("R8G8_SRGB", 16, [0xff, 0xff00, 0, 0]),
        23 => masks("R8G8B8_UNORM", 24, [0xff, 0xff00, 0xff_0000, 0]),
        29 => masks("R8G8B8_SRGB", 24, [0xff, 0xff00, 0xff_0000, 0]),
        30 => masks("B8G8R8_UNORM", 24, [0xff_0000, 0xff00, 0xff, 0]),
        36 => masks("B8G8R8_SRGB", 24, [0xff_0000, 0xff00, 0xff, 0]),
        37 => dxgi("R8G8B8A8_UNORM", 28),
        38 => dxgi("R8G8B8A8_SNORM", 31),
        41 => dxgi("R8G8B8A8_UINT", 30),
        42 => dxgi("R8G8B8A8_SINT", 32),
        43 => dxgi("R8G8B8A8_SRGB", 29),
        44 => dxgi("B8G8R8A8_UNORM", 87),
        50 => dxgi("B8G8R8A8_SRGB", 91),
        51 => dxgi("A8B8G8R8_UNORM_PACK32", 28),
        52 => dxgi("A8B8G8R8_SNORM_PACK32", 31),
        55 => dxgi("A8B8G8R8_UINT_PACK32", 30),
        56 => dxgi("A8B8G8R8_SINT_PACK32", 32),
        57 => dxgi("A8B8G8R8_SRGB_PACK32", 29),
        58 => masks("A2R10G10B10_UNORM_PACK32", 32, [0x3ff0_0000, 0x000f_fc00, 0x0000_03ff, 0xc000_0000]),
        64 => dxgi("A2B10G10R10_UNORM_PACK32", 24),
        68 => dxgi("A2B10G10R10_UINT_PACK32", 25),
        70 => dxgi("R16_UNORM", 56),
        71 => dxgi("R16_SNORM", 58),
        74 => dxgi("R16_UINT", 57),
        75 => dxgi("R16_SINT", 59),
        76 => dxgi("R16_SFLOAT", 54),
        77 => dxgi("R16G16_UNORM", 35),
        78 => dxgi("R16G16_SNORM", 37),
        81 => dxgi("R16G16_UINT", 36),
        82 => dxgi("R16G16_SINT", 38),
        83 => dxgi("R16G16_SFLOAT", 34),
        84 => channels("R16G16B16_UNORM", 48, "R16G16B16_UNORM"),
        85 => channels("R16G16B16_SNORM", 48, "R16G16B16_SNORM"),
        88 => channels("R16G16B16_UINT", 48, "R16G16B16_UINT"),
        89 => channels("R16G16B16_SINT", 48, "R16G16B16_SINT"),
        90 => channels("R16G16B16_SFLOAT", 48, "R16G16B16_FLOAT"),
        91 => dxgi("R16G16B16A16_UNORM", 11),
        92 => dxgi("R16G16B16A16_SNORM", 13),
        95 => dxgi("R16G16B16A16_UINT", 12),
        96 => dxgi("R16G16B16A16_SINT", 14),
        97 => dxgi("R16G16B16A16_SFLOAT", 10),
        98 => dxgi("R32_UINT", 42),
        99 => dxgi("R32_SINT", 43),
        100 => dxgi("R32_SFLOAT", 41),
        101 => dxgi("R32G32_UINT", 17),
        102 => dxgi("R32G32_SINT", 18),
        103 => dxgi("R32G32_SFLOAT", 16),
        104 => dxgi("R32G32B32_UINT", 7),
        105 => dxgi("R32G32B32_SINT", 8),
        106 => dxgi("R32G32B32_SFLOAT", 6),
        107 => dxgi("R32G32B32A32_UINT", 3),
        108 => dxgi("R32G32B32A32_SINT", 4),
        109 => dxgi("R32G32B32A32_SFLOAT", 2),
        122 => dxgi("B10G11R11_UFLOAT_PACK32", 26),
        123 => dxgi("E5B9G9R9_UFLOAT_PACK32", 67),
        124 => dxgi("D16_UNORM", 55),
        126 => dxgi("D32_SFLOAT", 40),
        131 => dxgi("BC1_RGB_UNORM_BLOCK", 71),
        132 => dxgi("BC1_RGB_SRGB_BLOCK", 72),
        133 => dxgi("BC1_RGBA_UNORM_BLOCK", 71),
        134 => dxgi("BC1_RGBA_SRGB_BLOCK", 72),
        135 => dxgi("BC2_UNORM_BLOCK", 74),
        136 => dxgi("BC2_SRGB_BLOCK", 75),
        137 => dxgi("BC3_UNORM_BLOCK", 77),
        138 => dxgi("BC3_SRGB_BLOCK", 78),
        139 => dxgi("BC4_UNORM_BLOCK", 80),
        140 => dxgi("BC4_SNORM_BLOCK", 81),
        141 => dxgi("BC5_UNORM_BLOCK", 83),
        142 => dxgi("BC5_SNORM_BLOCK", 84),
        143 => dxgi("BC6H_UFLOAT_BLOCK", 95),
        144 => dxgi("BC6H_SFLOAT_BLOCK", 96),
        145 => dxgi("BC7_UNORM_BLOCK", 98),
        146 => dxgi("BC7_SRGB_BLOCK", 99),
        147 => opaque("ETC2_R8G8B8_UNORM_BLOCK", Block { bytes: 8 }),
        148 => opaque("ETC2_R8G8B8_SRGB_BLOCK", Block { bytes: 8 }),
        149 => opaque("ETC2_R8G8B8A1_UNORM_BLOCK", Block { bytes: 8 }),
        150 => opaque("ETC2_R8G8B8A1_SRGB_BLOCK", Block { bytes: 8 }),
        151 => opaque("ETC2_R8G8B8A8_UNORM_BLOCK", Block { bytes: 16 }),
        152 => opaque("ETC2_R8G8B8A8_SRGB_BLOCK", Block { bytes: 16 }),
        153 => opaque("EAC_R11_UNORM_BLOCK", Block { bytes: 8 }),
        154 => opaque("EAC_R11_SNORM_BLOCK", Block { bytes: 8 }),
        155 => opaque("EAC_R11G11_UNORM_BLOCK", Block { bytes: 16 }),
        156 => opaque("EAC_R11G11_SNORM_BLOCK", Block { bytes: 16 }),
        157..=184 => {
            let (w, h, unorm, srgb) = ASTC[(vk - 157) as usize / 2];
            opaque(if vk % 2 == 1 { unorm } else { srgb }, Tiles { width: w, height: h, bytes: 16 })
        }
        1_000_066_000..=1_000_066_013 => {
            let i = (vk - 1_000_066_000) as usize;
            let (w, h, _, _) = ASTC[i];
            opaque(ASTC_HDR[i], Tiles { width: w, height: h, bytes: 16 })
        }
        _ => opaque("unknown VkFormat", Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A KTX2 texture: header, level index (levels stored smallest first), a dummy data
    /// format descriptor, then the level data given per mip.
    fn build(vk: u32, width: u32, height: u32, faces: u32, levels: &[Vec<u8>], scheme: u32) -> Vec<u8> {
        let n = levels.len();
        let dfd_at = HEADER + n * LEVEL_ENTRY;
        let mut out = MAGIC.to_vec();
        for v in [vk, 1, width, height, 0, 0, faces, n as u32, scheme, dfd_at as u32, 44, 0, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&[0u8; 16]); // sgd offset and length
        let mut data_at = dfd_at + 44;
        let mut entries = vec![(0u64, 0u64); n];
        for m in (0..n).rev() {
            data_at = data_at.next_multiple_of(16);
            entries[m] = (data_at as u64, levels[m].len() as u64);
            data_at += levels[m].len();
        }
        for (offset, length) in &entries {
            for v in [*offset, *length, *length] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out.resize(data_at, 0);
        out[dfd_at..dfd_at + 4].copy_from_slice(&44u32.to_le_bytes());
        for m in 0..n {
            let at = entries[m].0 as usize;
            out[at..at + levels[m].len()].copy_from_slice(&levels[m]);
        }
        out
    }

    #[test]
    fn bc7_with_mips_smallest_first() {
        // 8x8: 4 blocks, then 1, then 1.
        let levels = vec![vec![1u8; 64], vec![2u8; 16], vec![3u8; 16]];
        let mut file = build(145, 8, 8, 1, &levels, 0);
        let info = parse(&file).unwrap();
        assert_eq!((info.width, info.height, info.mips, info.pixel_format.name), (8, 8, 3, "BC7_UNORM_BLOCK"));
        assert_eq!(info.pixel_format.dxgi, Some(98));
        assert_eq!(info.size, file.len() as u64);
        let Storage::Ktx2 { levels: index, .. } = &info.storage else { panic!() };
        assert!(index[0].0 > index[2].0, "mip 0 is stored last");
        // Trailing bytes aren't part of it.
        file.extend_from_slice(b"next");
        assert_eq!(parse(&file).unwrap().size, info.size);
    }

    #[test]
    fn broken_headers() {
        let good = build(37, 4, 4, 1, &[vec![0; 64]], 0);
        assert_eq!(parse(b"\xABKTX 11\xBB\r\n\x1A\n rest"), Err(Reject::NoMatch));
        assert!(matches!(parse(&good[..good.len() - 1]), Err(Reject::Bad(r)) if r.starts_with("truncated")));
        let wrong_size = build(37, 4, 4, 1, &[vec![0; 60]], 0);
        assert!(matches!(parse(&wrong_size), Err(Reject::Bad(r)) if r.contains("holds 60 bytes")));
        let three_faces = build(37, 4, 4, 3, &[vec![0; 192]], 0);
        assert!(matches!(parse(&three_faces), Err(Reject::Bad(r)) if r.contains("3 faces")));
        // Supercompressed levels aren't checked against the format: their size is theirs.
        let zstd = build(145, 64, 64, 1, &[vec![9; 100]], 2);
        assert_eq!(parse(&zstd).unwrap().size, zstd.len() as u64);
        // Newer schemes (4 is Basis Universal's UASTC HDR 6x6 intermediate) too.
        let scheme4 = build(0, 64, 64, 1, &[vec![9; 100]], 4);
        assert_eq!(parse(&scheme4).unwrap().size, scheme4.len() as u64);
    }

    #[test]
    fn orientation_metadata() {
        let entry = |text: &[u8]| {
            let mut kv = (text.len() as u32).to_le_bytes().to_vec();
            kv.extend_from_slice(text);
            kv.resize(kv.len().next_multiple_of(4), 0);
            kv
        };
        let kvd = [entry(b"KTXwriter\0test\0"), entry(b"KTXorientation\0ru\0")].concat();
        assert_eq!(orientation(&kvd), Orientation { flip_x: false, flip_y: true });
        assert_eq!(orientation(&entry(b"KTXorientation\0ld\0")), Orientation { flip_x: true, flip_y: false });
        assert_eq!(orientation(&entry(b"KTXwriter\0x\0")), Orientation::default());
        assert_eq!(orientation(&[1, 2]), Orientation::default());
    }

    #[test]
    fn vulkan_formats() {
        assert_eq!(vk_format_info(43, 0).dxgi, Some(29));
        assert_eq!(vk_format_info(167, 0).layout, Layout::Tiles { width: 8, height: 5, bytes: 16 });
        assert_eq!(vk_format_info(168, 0).name, "ASTC_8x5_SRGB_BLOCK");
        assert_eq!(vk_format_info(184, 0).name, "ASTC_12x12_SRGB_BLOCK");
        assert_eq!(vk_format_info(0, 1).layout, Layout::Unknown);
        assert_eq!(vk_format_info(1_000_066_004, 0).name, "ASTC_6x6_SFLOAT_BLOCK");
        assert_eq!(vk_format_info(90, 0).layout, Layout::Linear { bits: 48 });
        assert_eq!(Layout::Tiles { width: 6, height: 6, bytes: 16 }.image_size(13, 7), 3 * 2 * 16);
    }
}
