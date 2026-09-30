//! Finding textures: look for each format's magic bytes, then let the format check the
//! header and work out the size. A texture that is found is skipped past, so magic bytes
//! inside its pixel data are ignored.

use memchr::memmem;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::format::{Container, Reject, TextureInfo, format_for};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanOptions {
    /// Formats to look for.
    pub formats: Vec<Container>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { formats: Container::ALL.to_vec() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundTexture {
    pub offset: u64,
    pub container: Container,
    pub info: TextureInfo,
    /// CRC-32 of the whole texture, header included.
    pub crc32: u32,
}

impl FoundTexture {
    pub fn end(&self) -> u64 {
        self.offset + self.info.size
    }
}

/// A header that is clearly a texture but couldn't be used, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rejected {
    pub offset: u64,
    pub container: Container,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// In file order, never overlapping.
    pub textures: Vec<FoundTexture>,
    /// In file order. Candidates inside a found texture aren't listed.
    pub rejected: Vec<Rejected>,
}

pub fn scan(data: &[u8], opts: &ScanOptions) -> ScanReport {
    let mut candidates: Vec<(usize, Container)> = opts
        .formats
        .iter()
        .flat_map(|&c| memmem::find_iter(data, format_for(c).magic()).map(move |pos| (pos, c)))
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    let parsed: Vec<_> =
        candidates.into_par_iter().map(|(pos, c)| (pos, c, format_for(c).parse(&data[pos..]))).collect();

    let mut report = ScanReport::default();
    let mut next = 0;
    for (pos, container, result) in parsed {
        if pos < next {
            continue;
        }
        let offset = pos as u64;
        match result {
            Ok(info) => {
                next = pos + info.size as usize;
                report.textures.push(FoundTexture { offset, container, info, crc32: 0 });
            }
            Err(Reject::Bad(reason)) => report.rejected.push(Rejected { offset, container, reason }),
            Err(Reject::NoMatch) => {}
        }
    }
    report.textures.par_iter_mut().for_each(|t| {
        t.crc32 = crc32fast::hash(&data[t.offset as usize..t.end() as usize]);
    });
    report
}

/// Check for one texture of a given format at an exact offset.
pub fn texture_at(data: &[u8], offset: u64, container: Container) -> Result<FoundTexture, Reject> {
    let pos = usize::try_from(offset).ok().filter(|&p| p <= data.len()).ok_or(Reject::NoMatch)?;
    let info = format_for(container).parse(&data[pos..])?;
    let crc32 = crc32fast::hash(&data[pos..pos + info.size as usize]);
    Ok(FoundTexture { offset, container, info, crc32 })
}
