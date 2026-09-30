//! Everything the GUI knows, without any drawing code: the open file, what the scan found,
//! and the log.
//!
//! Slow work (opening and scanning, extracting) is done by the free functions here, on a
//! worker thread started by [`crate::jobs`]; their results are applied to the
//! [`Session`] on the UI thread. Tests drive the same functions directly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result};
use texscan_core::input::{self, Input};
use texscan_core::{
    ExtractOptions, ExtractedFile, Image, Manifest, Rejected, ScanOptions, SourceInfo, Subresource, TextureEntry,
    TextureInfo, decode, encode_png, extract_all, scan, texture_bytes,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: Level,
    pub text: String,
}

/// A file open for reading (memory-mapped; never written to).
pub struct OpenFile {
    pub path: PathBuf,
    pub data: Arc<Input>,
}

impl OpenFile {
    pub fn len(&self) -> u64 {
        self.data.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// What a scan found.
pub struct Scanned {
    pub manifest: Manifest,
    /// The parsed header of each texture, in manifest order, for decoding.
    pub infos: Vec<TextureInfo>,
    pub rejected: Vec<Rejected>,
    pub seconds: f64,
}

#[derive(Default)]
pub struct Session {
    pub file: Option<OpenFile>,
    pub scanned: Option<Scanned>,
    pub scan_options: ScanOptions,
    pub log: Vec<LogLine>,
    /// A texture the UI should select and scroll to.
    pub select: Option<u32>,
    /// Bumped whenever the file or scan changes, so views can drop caches.
    pub generation: u64,
}

impl Session {
    pub fn info(&mut self, text: impl Into<String>) {
        self.log.push(LogLine { level: Level::Info, text: text.into() });
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.log.push(LogLine { level: Level::Warn, text: text.into() });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.log.push(LogLine { level: Level::Error, text: text.into() });
    }

    pub fn textures(&self) -> &[TextureEntry] {
        self.scanned.as_ref().map_or(&[], |s| &s.manifest.textures)
    }

    /// A texture's manifest entry and parsed header, by id.
    pub fn texture(&self, id: u32) -> Option<(&TextureEntry, &TextureInfo)> {
        let s = self.scanned.as_ref()?;
        let i = s.manifest.textures.iter().position(|t| t.id == id)?;
        Some((&s.manifest.textures[i], &s.infos[i]))
    }

    pub fn rejected(&self) -> &[Rejected] {
        self.scanned.as_ref().map_or(&[], |s| &s.rejected)
    }

    pub fn set_opened(&mut self, (file, scanned): (OpenFile, Scanned)) {
        self.info(format!(
            "{}: {} texture(s) in {:.2} s{}",
            file.path.display(),
            scanned.manifest.textures.len(),
            scanned.seconds,
            match scanned.rejected.len() {
                0 => String::new(),
                n => format!(", {n} header(s) that look like textures but can't be used"),
            }
        ));
        self.file = Some(file);
        self.scanned = Some(scanned);
        self.generation += 1;
    }

    pub fn set_scanned(&mut self, scanned: Scanned) {
        self.info(format!("scan: {} texture(s) in {:.2} s", scanned.manifest.textures.len(), scanned.seconds));
        self.scanned = Some(scanned);
        self.generation += 1;
    }

    pub fn close(&mut self) {
        self.file = None;
        self.scanned = None;
        self.generation += 1;
    }
}

pub fn open_file(path: &Path) -> Result<OpenFile> {
    let data = input::open(path).with_context(|| format!("opening {}", path.display()))?;
    Ok(OpenFile { path: path.to_path_buf(), data: Arc::new(data) })
}

pub fn run_scan(file: &OpenFile, opts: &ScanOptions) -> Scanned {
    let started = Instant::now();
    let report = scan(&file.data, opts);
    let manifest = Manifest::new(SourceInfo::describe(&file.path, &file.data), opts.clone(), &report.textures);
    let infos = report.textures.into_iter().map(|t| t.info).collect();
    Scanned { manifest, infos, rejected: report.rejected, seconds: started.elapsed().as_secs_f64() }
}

pub fn open_and_scan(path: &Path, opts: &ScanOptions) -> Result<(OpenFile, Scanned)> {
    let file = open_file(path)?;
    let scanned = run_scan(&file, opts);
    Ok((file, scanned))
}

/// Decode one image of a texture.
pub fn decode_image(data: &[u8], entry: &TextureEntry, info: &TextureInfo, sub: Subresource) -> Result<Image> {
    let bytes = texture_bytes(data, entry)?;
    Ok(decode(bytes, info, sub)?)
}

/// Save a texture exactly as it is in the file (a standalone `.dds`).
pub fn save_texture(data: &[u8], entry: &TextureEntry, path: &Path) -> Result<()> {
    let bytes = texture_bytes(data, entry)?;
    std::fs::write(path, bytes).with_context(|| path.display().to_string())
}

pub fn save_png(image: &Image, path: &Path) -> Result<()> {
    std::fs::write(path, encode_png(image)).with_context(|| path.display().to_string())
}

pub fn extract(data: &[u8], manifest: &Manifest, dir: &Path, png: bool) -> Result<Vec<ExtractedFile>> {
    Ok(extract_all(data, manifest, dir, &ExtractOptions { verify_source: true, png })?)
}

/// `1.5 MiB` and so on.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} bytes") } else { format!("{v:.1} {}", UNITS[unit]) }
}
