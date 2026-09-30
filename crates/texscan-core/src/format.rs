//! The texture-format trait: each container format (DDS, and later KTX, PNG...) is one
//! file under `formats/` that recognises its header and works out the texture's size.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::pixel::PixelFormat;

/// A texture file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Container {
    Dds,
}

impl Container {
    pub const ALL: [Container; 1] = [Container::Dds];

    pub fn name(self) -> &'static str {
        match self {
            Container::Dds => "dds",
        }
    }

    /// File extension for extracted textures, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Container::Dds => "dds",
        }
    }
}

impl fmt::Display for Container {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

impl FromStr for Container {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "dds" => Ok(Container::Dds),
            _ => Err(format!("unknown texture format {s:?} (known: dds)")),
        }
    }
}

/// What a texture's header says, and so how big it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureInfo {
    /// The whole texture, header included.
    pub size: u64,
    pub header_size: u64,
    pub width: u32,
    pub height: u32,
    /// 1 unless it's a volume texture.
    pub depth: u32,
    /// Mip levels, at least 1.
    pub mips: u32,
    /// Array elements, at least 1. A cube map array of N cubes has N here.
    pub array_size: u32,
    /// Faces per array element: 6 for a cube map (fewer for a partial legacy one), else 1.
    pub faces: u32,
    pub pixel_format: PixelFormat,
}

impl TextureInfo {
    pub fn is_cube(&self) -> bool {
        self.faces > 1
    }

    /// Like `1024x512`, `64x64x16` or `256x256 cube`.
    pub fn dimensions(&self) -> String {
        let mut s = format!("{}x{}", self.width, self.height);
        if self.depth > 1 {
            s += &format!("x{}", self.depth);
        }
        if self.is_cube() {
            s += " cube";
        }
        if self.array_size > 1 {
            s += &format!(" [{}]", self.array_size);
        }
        s
    }
}

/// Why [`TextureFormat::parse`] didn't accept a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// Not this format at all, such as the magic bytes inside some text. Not reported.
    NoMatch,
    /// The header is clearly this format but can't be used (unknown pixel format,
    /// truncated data...). Reported, since the user may want to know.
    Bad(String),
}

pub trait TextureFormat: Sync {
    fn container(&self) -> Container;

    /// Bytes every texture in this format starts with.
    fn magic(&self) -> &'static [u8];

    /// Read the header of the texture at `data[0]`; `data` runs to the end of the file.
    /// Accepts it only if the whole texture fits.
    fn parse(&self, data: &[u8]) -> Result<TextureInfo, Reject>;
}

pub fn format_for(container: Container) -> &'static dyn TextureFormat {
    match container {
        Container::Dds => &crate::formats::dds::Dds,
    }
}

/// Little-endian u32 at `pos`; the caller has checked the length.
pub(crate) fn u32_le(data: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap())
}
