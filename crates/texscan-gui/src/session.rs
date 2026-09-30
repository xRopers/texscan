//! Everything the GUI knows, without any drawing code: the open file, what the scan found,
//! the edits, and the log.
//!
//! Edits are kept as file paths and read when they're previewed or packed, so a PNG can
//! still be changed in an image editor after it was chosen.
//!
//! Slow work (opening and scanning, extracting, packing) is done by the free functions
//! here, on a worker thread started by [`crate::jobs`]; their results are applied to the
//! [`Session`] on the UI thread. Tests drive the same functions directly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result};
use texscan_core::input::{self, Input};
use texscan_core::{
    Edits, ExtractOptions, ExtractedFile, Image, Manifest, Outcome, PackOptions, Rejected, ScanOptions, SourceInfo,
    Subresource, TextureEdit, TextureEntry, TextureInfo, TexturePlan, decode, decode_png, encode_png, extract_all,
    format_for, load_edits, pack, pack_texture, png_images, scan, texture_bytes,
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

/// Where a texture's new contents come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditSource {
    /// A replacement texture file (DDS or KTX2) of the same layout.
    File(PathBuf),
    /// New top-mip images (PNGs) by (layer, slice).
    Images(BTreeMap<(u32, u32), PathBuf>),
}

impl EditSource {
    /// The files, for display.
    pub fn files(&self) -> Vec<&Path> {
        match self {
            EditSource::File(p) => vec![p.as_path()],
            EditSource::Images(m) => m.values().map(PathBuf::as_path).collect(),
        }
    }
}

/// What the last pack did or would do.
#[derive(Debug, Clone)]
pub struct PackReport {
    pub textures: Vec<TexturePlan>,
    /// Where the packed file was written and checked, or `None` for a dry run.
    pub written: Option<PathBuf>,
}

impl PackReport {
    pub fn changed(&self) -> usize {
        self.textures.iter().filter(|t| t.outcome != Outcome::Unchanged).count()
    }
}

#[derive(Default)]
pub struct Session {
    pub file: Option<OpenFile>,
    pub scanned: Option<Scanned>,
    pub scan_options: ScanOptions,
    /// Texture id -> where its new contents come from.
    pub edits: BTreeMap<u32, EditSource>,
    /// Bumped whenever the edits change, so the preview reloads.
    pub edits_generation: u64,
    pub last_pack: Option<PackReport>,
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
        self.clear_edits();
        self.generation += 1;
    }

    pub fn set_scanned(&mut self, scanned: Scanned) {
        self.info(format!("scan: {} texture(s) in {:.2} s", scanned.manifest.textures.len(), scanned.seconds));
        self.scanned = Some(scanned);
        self.clear_edits();
        self.generation += 1;
    }

    pub fn close(&mut self) {
        self.file = None;
        self.scanned = None;
        self.clear_edits();
        self.generation += 1;
    }

    fn clear_edits(&mut self) {
        self.edits.clear();
        self.last_pack = None;
        self.edits_generation += 1;
    }

    fn edits_changed(&mut self) {
        self.last_pack = None;
        self.edits_generation += 1;
    }

    /// Replace a whole texture with a texture file (DDS or KTX2) of the same layout.
    pub fn set_file_edit(&mut self, id: u32, path: PathBuf) {
        self.info(format!("texture {id}: will be replaced by {}", path.display()));
        self.edits.insert(id, EditSource::File(path));
        self.edits_changed();
    }

    /// Replace one image (layer, slice) of a texture with a PNG. A `.dds` edit of the same
    /// texture is dropped.
    pub fn set_image_edit(&mut self, id: u32, layer: u32, slice: u32, path: PathBuf) {
        self.info(format!("texture {id}: image will be replaced by {}", path.display()));
        match self.edits.get_mut(&id) {
            Some(EditSource::Images(images)) => {
                images.insert((layer, slice), path);
            }
            _ => {
                self.edits.insert(id, EditSource::Images([((layer, slice), path)].into()));
            }
        }
        self.edits_changed();
    }

    pub fn revert(&mut self, id: u32) {
        if self.edits.remove(&id).is_some() {
            self.info(format!("texture {id}: edit reverted"));
            self.edits_changed();
        }
    }

    pub fn set_imported_edits(&mut self, dir: &Path, (edits, unchanged): (BTreeMap<u32, EditSource>, usize)) {
        self.info(format!(
            "{} edited texture(s) found in {} ({unchanged} unedited file(s) skipped)",
            edits.len(),
            dir.display()
        ));
        self.edits = edits;
        self.edits_changed();
    }

    pub fn set_pack_report(&mut self, report: PackReport) {
        match &report.written {
            Some(path) => self.info(format!("{} texture(s) packed into {} (verified)", report.changed(), path.display())),
            None => self.info(format!("dry run: {} texture(s) would change", report.changed())),
        }
        for t in &report.textures {
            if let Some(note) = &t.note {
                self.warn(format!("texture {}: {note}", t.id));
            }
        }
        self.last_pack = Some(report);
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

/// Read an edit's files.
pub fn read_edit(source: &EditSource) -> Result<TextureEdit> {
    Ok(match source {
        EditSource::File(path) => TextureEdit::Texture(std::fs::read(path).with_context(|| path.display().to_string())?),
        EditSource::Images(paths) => {
            let mut images = BTreeMap::new();
            for (&key, path) in paths {
                let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
                let image = decode_png(&bytes).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
                images.insert(key, image);
            }
            TextureEdit::Images(images)
        }
    })
}

pub fn read_edits(edits: &BTreeMap<u32, EditSource>) -> Result<Edits> {
    edits.iter().map(|(&id, source)| Ok((id, read_edit(source)?))).collect()
}

/// Decode one image of a texture as pack would write it with `edit`.
pub fn decode_edited(
    data: &[u8],
    entry: &TextureEntry,
    info: &TextureInfo,
    edit: &EditSource,
    sub: Subresource,
) -> Result<Image> {
    let packed = pack_texture(texture_bytes(data, entry)?, entry, &read_edit(edit)?)?.0;
    Ok(decode(&packed, info, sub)?)
}

/// Pack the edits: a dry run, or written to `output` and checked.
pub fn run_pack(data: &[u8], manifest: &Manifest, edits: &Edits, output: Option<&Path>) -> Result<PackReport> {
    let result = pack(data, manifest, edits, &PackOptions::default())?;
    if let Some(out) = output {
        if result.changed() == 0 {
            anyhow::bail!("nothing to write: every edit gives the original bytes");
        }
        result.write_file(data, out)?;
    }
    Ok(PackReport { textures: result.textures, written: output.map(Path::to_path_buf) })
}

/// Edits found in an extract folder (see [`load_edits`]), as file paths, and how many
/// unedited files were skipped.
pub fn import_edits(data: &[u8], manifest: &Manifest, dir: &Path) -> Result<(BTreeMap<u32, EditSource>, usize)> {
    let found = load_edits(data, manifest, dir)?;
    let mut edits = BTreeMap::new();
    for (&id, edit) in &found.edits {
        let entry = manifest.textures.iter().find(|t| t.id == id).expect("edits are of listed textures");
        let source = match edit {
            TextureEdit::Texture(_) => EditSource::File(dir.join(&entry.file)),
            TextureEdit::Images(images) => {
                let info = format_for(entry.format)
                    .parse(texture_bytes(data, entry)?)
                    .map_err(|_| anyhow::anyhow!("texture {id}'s header no longer parses"))?;
                let stem = entry.file.rsplit_once('.').map_or(entry.file.as_str(), |(s, _)| s);
                let names: BTreeMap<_, _> =
                    png_images(&info).into_iter().map(|(sub, suffix)| ((sub.layer, sub.slice), suffix)).collect();
                EditSource::Images(images.keys().map(|k| (*k, dir.join(format!("{stem}{}.png", names[k])))).collect())
            }
        };
        edits.insert(id, source);
    }
    Ok((edits, found.unchanged))
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

/// `game.pak` → `game.packed.pak`.
pub fn default_output_name(input: &Path) -> String {
    let stem = input.file_stem().map_or_else(|| "output".into(), |s| s.to_string_lossy().into_owned());
    match input.extension() {
        Some(ext) => format!("{stem}.packed.{}", ext.to_string_lossy()),
        None => format!("{stem}.packed"),
    }
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
