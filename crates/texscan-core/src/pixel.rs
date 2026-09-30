//! Pixel formats: what they're called and how many bytes an image in each takes.
//!
//! DXGI formats (Direct3D 10+) are the common vocabulary. Older DDS files use Direct3D 9
//! formats, which map to a DXGI format where one matches exactly and otherwise keep
//! their D3D9 name.

/// How pixels are laid out in memory, which is all the size calculation needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// `bits` per pixel, each row padded to a whole byte.
    Linear { bits: u32 },
    /// 4×4 pixel blocks of `bytes` each (BC1–BC7).
    Block { bytes: u32 },
    /// Pairs of pixels sharing `bytes` (4:2:2 formats such as YUY2 and R8G8_B8G8).
    Pair { bytes: u32 },
}

impl Layout {
    /// Bytes in one `width` × `height` image. Width and height are at least 1.
    pub fn image_size(self, width: u32, height: u32) -> u64 {
        let (w, h) = (u64::from(width), u64::from(height));
        match self {
            Layout::Linear { bits } => (w * u64::from(bits)).div_ceil(8) * h,
            Layout::Block { bytes } => w.div_ceil(4) * h.div_ceil(4) * u64::from(bytes),
            Layout::Pair { bytes } => w.div_ceil(2) * h * u64::from(bytes),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat {
    /// DXGI name without the `DXGI_FORMAT_` prefix, or a D3D9 name for older formats.
    pub name: &'static str,
    /// The matching DXGI format, if there is one.
    pub dxgi: Option<u32>,
    pub layout: Layout,
}

impl PixelFormat {
    pub const fn new(name: &'static str, dxgi: Option<u32>, layout: Layout) -> Self {
        Self { name, dxgi, layout }
    }

    pub fn is_block_compressed(&self) -> bool {
        matches!(self.layout, Layout::Block { .. })
    }
}

/// Why a DXGI format can't be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DxgiError {
    Unknown,
    /// Planar video formats (NV12 and friends) aren't supported yet.
    Planar(&'static str),
}

/// The DXGI format with this number.
pub fn dxgi(id: u32) -> Result<PixelFormat, DxgiError> {
    use Layout::*;
    let (name, layout) = match id {
        1 => ("R32G32B32A32_TYPELESS", Linear { bits: 128 }),
        2 => ("R32G32B32A32_FLOAT", Linear { bits: 128 }),
        3 => ("R32G32B32A32_UINT", Linear { bits: 128 }),
        4 => ("R32G32B32A32_SINT", Linear { bits: 128 }),
        5 => ("R32G32B32_TYPELESS", Linear { bits: 96 }),
        6 => ("R32G32B32_FLOAT", Linear { bits: 96 }),
        7 => ("R32G32B32_UINT", Linear { bits: 96 }),
        8 => ("R32G32B32_SINT", Linear { bits: 96 }),
        9 => ("R16G16B16A16_TYPELESS", Linear { bits: 64 }),
        10 => ("R16G16B16A16_FLOAT", Linear { bits: 64 }),
        11 => ("R16G16B16A16_UNORM", Linear { bits: 64 }),
        12 => ("R16G16B16A16_UINT", Linear { bits: 64 }),
        13 => ("R16G16B16A16_SNORM", Linear { bits: 64 }),
        14 => ("R16G16B16A16_SINT", Linear { bits: 64 }),
        15 => ("R32G32_TYPELESS", Linear { bits: 64 }),
        16 => ("R32G32_FLOAT", Linear { bits: 64 }),
        17 => ("R32G32_UINT", Linear { bits: 64 }),
        18 => ("R32G32_SINT", Linear { bits: 64 }),
        19 => ("R32G8X24_TYPELESS", Linear { bits: 64 }),
        20 => ("D32_FLOAT_S8X24_UINT", Linear { bits: 64 }),
        21 => ("R32_FLOAT_X8X24_TYPELESS", Linear { bits: 64 }),
        22 => ("X32_TYPELESS_G8X24_UINT", Linear { bits: 64 }),
        23 => ("R10G10B10A2_TYPELESS", Linear { bits: 32 }),
        24 => ("R10G10B10A2_UNORM", Linear { bits: 32 }),
        25 => ("R10G10B10A2_UINT", Linear { bits: 32 }),
        26 => ("R11G11B10_FLOAT", Linear { bits: 32 }),
        27 => ("R8G8B8A8_TYPELESS", Linear { bits: 32 }),
        28 => ("R8G8B8A8_UNORM", Linear { bits: 32 }),
        29 => ("R8G8B8A8_UNORM_SRGB", Linear { bits: 32 }),
        30 => ("R8G8B8A8_UINT", Linear { bits: 32 }),
        31 => ("R8G8B8A8_SNORM", Linear { bits: 32 }),
        32 => ("R8G8B8A8_SINT", Linear { bits: 32 }),
        33 => ("R16G16_TYPELESS", Linear { bits: 32 }),
        34 => ("R16G16_FLOAT", Linear { bits: 32 }),
        35 => ("R16G16_UNORM", Linear { bits: 32 }),
        36 => ("R16G16_UINT", Linear { bits: 32 }),
        37 => ("R16G16_SNORM", Linear { bits: 32 }),
        38 => ("R16G16_SINT", Linear { bits: 32 }),
        39 => ("R32_TYPELESS", Linear { bits: 32 }),
        40 => ("D32_FLOAT", Linear { bits: 32 }),
        41 => ("R32_FLOAT", Linear { bits: 32 }),
        42 => ("R32_UINT", Linear { bits: 32 }),
        43 => ("R32_SINT", Linear { bits: 32 }),
        44 => ("R24G8_TYPELESS", Linear { bits: 32 }),
        45 => ("D24_UNORM_S8_UINT", Linear { bits: 32 }),
        46 => ("R24_UNORM_X8_TYPELESS", Linear { bits: 32 }),
        47 => ("X24_TYPELESS_G8_UINT", Linear { bits: 32 }),
        48 => ("R8G8_TYPELESS", Linear { bits: 16 }),
        49 => ("R8G8_UNORM", Linear { bits: 16 }),
        50 => ("R8G8_UINT", Linear { bits: 16 }),
        51 => ("R8G8_SNORM", Linear { bits: 16 }),
        52 => ("R8G8_SINT", Linear { bits: 16 }),
        53 => ("R16_TYPELESS", Linear { bits: 16 }),
        54 => ("R16_FLOAT", Linear { bits: 16 }),
        55 => ("D16_UNORM", Linear { bits: 16 }),
        56 => ("R16_UNORM", Linear { bits: 16 }),
        57 => ("R16_UINT", Linear { bits: 16 }),
        58 => ("R16_SNORM", Linear { bits: 16 }),
        59 => ("R16_SINT", Linear { bits: 16 }),
        60 => ("R8_TYPELESS", Linear { bits: 8 }),
        61 => ("R8_UNORM", Linear { bits: 8 }),
        62 => ("R8_UINT", Linear { bits: 8 }),
        63 => ("R8_SNORM", Linear { bits: 8 }),
        64 => ("R8_SINT", Linear { bits: 8 }),
        65 => ("A8_UNORM", Linear { bits: 8 }),
        66 => ("R1_UNORM", Linear { bits: 1 }),
        67 => ("R9G9B9E5_SHAREDEXP", Linear { bits: 32 }),
        68 => ("R8G8_B8G8_UNORM", Pair { bytes: 4 }),
        69 => ("G8R8_G8B8_UNORM", Pair { bytes: 4 }),
        70 => ("BC1_TYPELESS", Block { bytes: 8 }),
        71 => ("BC1_UNORM", Block { bytes: 8 }),
        72 => ("BC1_UNORM_SRGB", Block { bytes: 8 }),
        73 => ("BC2_TYPELESS", Block { bytes: 16 }),
        74 => ("BC2_UNORM", Block { bytes: 16 }),
        75 => ("BC2_UNORM_SRGB", Block { bytes: 16 }),
        76 => ("BC3_TYPELESS", Block { bytes: 16 }),
        77 => ("BC3_UNORM", Block { bytes: 16 }),
        78 => ("BC3_UNORM_SRGB", Block { bytes: 16 }),
        79 => ("BC4_TYPELESS", Block { bytes: 8 }),
        80 => ("BC4_UNORM", Block { bytes: 8 }),
        81 => ("BC4_SNORM", Block { bytes: 8 }),
        82 => ("BC5_TYPELESS", Block { bytes: 16 }),
        83 => ("BC5_UNORM", Block { bytes: 16 }),
        84 => ("BC5_SNORM", Block { bytes: 16 }),
        85 => ("B5G6R5_UNORM", Linear { bits: 16 }),
        86 => ("B5G5R5A1_UNORM", Linear { bits: 16 }),
        87 => ("B8G8R8A8_UNORM", Linear { bits: 32 }),
        88 => ("B8G8R8X8_UNORM", Linear { bits: 32 }),
        89 => ("R10G10B10_XR_BIAS_A2_UNORM", Linear { bits: 32 }),
        90 => ("B8G8R8A8_TYPELESS", Linear { bits: 32 }),
        91 => ("B8G8R8A8_UNORM_SRGB", Linear { bits: 32 }),
        92 => ("B8G8R8X8_TYPELESS", Linear { bits: 32 }),
        93 => ("B8G8R8X8_UNORM_SRGB", Linear { bits: 32 }),
        94 => ("BC6H_TYPELESS", Block { bytes: 16 }),
        95 => ("BC6H_UF16", Block { bytes: 16 }),
        96 => ("BC6H_SF16", Block { bytes: 16 }),
        97 => ("BC7_TYPELESS", Block { bytes: 16 }),
        98 => ("BC7_UNORM", Block { bytes: 16 }),
        99 => ("BC7_UNORM_SRGB", Block { bytes: 16 }),
        100 => ("AYUV", Linear { bits: 32 }),
        101 => ("Y410", Linear { bits: 32 }),
        102 => ("Y416", Linear { bits: 64 }),
        103 => return Err(DxgiError::Planar("NV12")),
        104 => return Err(DxgiError::Planar("P010")),
        105 => return Err(DxgiError::Planar("P016")),
        106 => return Err(DxgiError::Planar("420_OPAQUE")),
        107 => ("YUY2", Pair { bytes: 4 }),
        108 => ("Y210", Pair { bytes: 8 }),
        109 => ("Y216", Pair { bytes: 8 }),
        110 => return Err(DxgiError::Planar("NV11")),
        111 => ("AI44", Linear { bits: 8 }),
        112 => ("IA44", Linear { bits: 8 }),
        113 => ("P8", Linear { bits: 8 }),
        114 => ("A8P8", Linear { bits: 16 }),
        115 => ("B4G4R4A4_UNORM", Linear { bits: 16 }),
        130 => return Err(DxgiError::Planar("P208")),
        131 => return Err(DxgiError::Planar("V208")),
        132 => return Err(DxgiError::Planar("V408")),
        _ => return Err(DxgiError::Unknown),
    };
    Ok(PixelFormat::new(name, Some(id), layout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_sizes() {
        let bc1 = Layout::Block { bytes: 8 };
        assert_eq!(bc1.image_size(256, 256), 32768);
        // Partial blocks round up, and a 1×1 mip is still one whole block.
        assert_eq!(bc1.image_size(5, 3), 16);
        assert_eq!(bc1.image_size(1, 1), 8);
        assert_eq!(Layout::Linear { bits: 24 }.image_size(3, 2), 18);
        assert_eq!(Layout::Linear { bits: 1 }.image_size(9, 2), 4);
        assert_eq!(Layout::Pair { bytes: 4 }.image_size(3, 2), 16);
    }

    #[test]
    fn dxgi_table() {
        assert_eq!(dxgi(98).unwrap().name, "BC7_UNORM");
        assert_eq!(dxgi(98).unwrap().layout, Layout::Block { bytes: 16 });
        assert_eq!(dxgi(103), Err(DxgiError::Planar("NV12")));
        assert_eq!(dxgi(0), Err(DxgiError::Unknown));
        assert_eq!(dxgi(116), Err(DxgiError::Unknown));
        // Every known number maps back to itself.
        for id in 0..200 {
            if let Ok(f) = dxgi(id) {
                assert_eq!(f.dxgi, Some(id));
            }
        }
    }
}
