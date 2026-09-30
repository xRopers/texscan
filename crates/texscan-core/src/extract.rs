//! Writing the textures a manifest lists to files, exactly as they are in the input.

use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::error::{Error, Result, io_err};
use crate::manifest::{Manifest, TextureEntry};

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Refuse to extract if the input's size or CRC differs from the manifest's.
    pub verify_source: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self { verify_source: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedFile {
    pub id: u32,
    pub offset: u64,
    pub path: PathBuf,
    pub size: u64,
}

/// The bytes of one texture, checked against the manifest's CRC.
pub fn texture_bytes<'a>(data: &'a [u8], entry: &TextureEntry) -> Result<&'a [u8]> {
    let fail = |reason: String| Error::Texture { id: entry.id, offset: entry.offset, reason };
    let bytes = usize::try_from(entry.end())
        .ok()
        .and_then(|end| data.get(entry.offset as usize..end))
        .ok_or_else(|| fail(format!("runs past the end of the input ({} bytes)", data.len())))?;
    let crc = crc32fast::hash(bytes);
    if crc != entry.crc32 {
        return Err(fail(format!("CRC-32 is {crc:08x}, manifest expects {:08x}", entry.crc32)));
    }
    Ok(bytes)
}

pub fn extract_all(data: &[u8], manifest: &Manifest, out_dir: &Path, opts: &ExtractOptions) -> Result<Vec<ExtractedFile>> {
    if opts.verify_source {
        manifest.source.check(data)?;
    }
    // Validate everything before writing anything.
    if let Some(bad) = manifest.textures.iter().find(|t| !is_safe_filename(&t.file)) {
        return Err(Error::BadFilename(bad.file.clone()));
    }
    let slices = manifest.textures.iter().map(|t| texture_bytes(data, t)).collect::<Result<Vec<_>>>()?;
    fs::create_dir_all(out_dir).map_err(io_err(out_dir))?;
    manifest
        .textures
        .par_iter()
        .zip(slices)
        .map(|(entry, bytes)| {
            let path = out_dir.join(&entry.file);
            fs::write(&path, bytes).map_err(io_err(&path))?;
            Ok(ExtractedFile { id: entry.id, offset: entry.offset, path, size: bytes.len() as u64 })
        })
        .collect()
}

/// A single file name with no directory parts, so a manifest can't write outside the
/// extract directory.
pub fn is_safe_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0'])
        && Path::new(name).file_name().is_some_and(|f| f == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_filenames() {
        assert!(is_safe_filename("00001234.dds"));
        for bad in ["", ".", "..", "../x.dds", "a/b.dds", "a\\b.dds", "C:x.dds"] {
            assert!(!is_safe_filename(bad), "{bad:?}");
        }
    }
}
