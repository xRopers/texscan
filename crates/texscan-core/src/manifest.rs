//! The manifest: what a scan found, and the contract that extract (and later pack) work
//! from.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io_err};
use crate::format::Container;
use crate::scan::{FoundTexture, ScanOptions};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub source: SourceInfo,
    pub scan_options: ScanOptions,
    pub textures: Vec<TextureEntry>,
}

/// Identifies the input file, so a manifest isn't applied to the wrong file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub path: String,
    pub size: u64,
    #[serde(with = "hex_u32")]
    pub crc32: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextureEntry {
    pub id: u32,
    pub offset: u64,
    /// The whole texture, header included.
    pub size: u64,
    pub format: Container,
    pub header_size: u64,
    pub width: u32,
    pub height: u32,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub depth: u32,
    pub mips: u32,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub array_size: u32,
    /// 6 for a cube map (fewer for a partial legacy one).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub faces: u32,
    /// As the file names it: a DXGI name, or a FourCC or D3D9 name in older DDS files.
    pub pixel_format: String,
    /// The matching DXGI format number, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dxgi_format: Option<u32>,
    /// CRC-32 of the whole texture, header included.
    #[serde(with = "hex_u32")]
    pub crc32: u32,
    /// Extracted file name, relative to the extract directory.
    pub file: String,
}

fn one() -> u32 {
    1
}

fn is_one(v: &u32) -> bool {
    *v == 1
}

impl SourceInfo {
    pub fn describe(path: &Path, data: &[u8]) -> Self {
        Self { path: path.display().to_string(), size: data.len() as u64, crc32: crc32fast::hash(data) }
    }

    /// Error if `data` is not the file this manifest was made from.
    pub fn check(&self, data: &[u8]) -> Result<()> {
        if data.len() as u64 != self.size {
            return Err(Error::SourceMismatch(format!("input is {} bytes, manifest expects {}", data.len(), self.size)));
        }
        let actual = crc32fast::hash(data);
        if actual != self.crc32 {
            return Err(Error::SourceMismatch(format!(
                "input CRC-32 is {actual:08x}, manifest expects {:08x}",
                self.crc32
            )));
        }
        Ok(())
    }
}

impl TextureEntry {
    pub fn from_found(id: u32, t: &FoundTexture) -> Self {
        let i = &t.info;
        Self {
            id,
            offset: t.offset,
            size: i.size,
            format: t.container,
            header_size: i.header_size,
            width: i.width,
            height: i.height,
            depth: i.depth,
            mips: i.mips,
            array_size: i.array_size,
            faces: i.faces,
            pixel_format: i.pixel_format.name.to_string(),
            dxgi_format: i.pixel_format.dxgi,
            crc32: t.crc32,
            file: texture_filename(t.offset, t.container),
        }
    }

    pub fn end(&self) -> u64 {
        self.offset + self.size
    }
}

impl Manifest {
    pub fn new(source: SourceInfo, scan_options: ScanOptions, found: &[FoundTexture]) -> Self {
        let textures = found.iter().enumerate().map(|(i, t)| TextureEntry::from_found(i as u32, t)).collect();
        Self { version: MANIFEST_VERSION, source, scan_options, textures }
    }

    pub fn from_json(text: &str) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        match value["version"].as_u64() {
            Some(v) if v == u64::from(MANIFEST_VERSION) => Ok(serde_json::from_value(value)?),
            other => {
                let found = other.and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
                Err(Error::ManifestVersion { found, expected: MANIFEST_VERSION })
            }
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("manifest serialization cannot fail")
    }

    pub fn load(path: &Path) -> Result<Self> {
        Self::from_json(&fs::read_to_string(path).map_err(io_err(path))?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fs::write(path, self.to_json() + "\n").map_err(io_err(path))
    }
}

/// `0001f400.dds`: the offset in hex, so files sort in file order.
pub fn texture_filename(offset: u64, container: Container) -> String {
    format!("{offset:08x}.{}", container.extension())
}

pub(crate) mod hex_u32 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &u32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{value:08x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
        let text = String::deserialize(d)?;
        let digits = text.strip_prefix("0x").unwrap_or(&text);
        u32::from_str_radix(digits, 16).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames() {
        assert_eq!(texture_filename(0x1234, Container::Dds), "00001234.dds");
        assert_eq!(texture_filename(0x1_0000_0000, Container::Dds), "100000000.dds");
    }

    #[test]
    fn rejects_other_versions() {
        let err = Manifest::from_json(r#"{"version": 7}"#).unwrap_err();
        assert!(matches!(err, Error::ManifestVersion { found: 7, expected: 1 }));
    }
}
