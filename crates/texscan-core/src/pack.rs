//! Putting edited textures back into a copy of the file, in place.
//!
//! An edit is either a whole replacement texture (a `.dds` or `.ktx2` with the same
//! dimensions, mips, faces, array size and pixel format, so it fits exactly) or new images for some
//! layers and slices of a texture (from PNGs), which are encoded in the texture's pixel
//! format with their mips regenerated. Either way the texture keeps its size and its
//! original header bytes; only pixel data changes, so nothing else in the file moves.
//!
//! [`pack`] builds and checks the new bytes of every edited texture; [`PackResult::write_file`]
//! writes the output through a temporary file and checks it again before renaming.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::decode::{Image, Subresource, decode};
use crate::encode::{encode, mip_chain, volume_mip_chain};
use crate::error::{Error, Result, io_err};
use crate::export::{decode_png, png_images};
use crate::extract::texture_bytes;
use crate::format::{Container, Reject, TextureInfo, format_for};
use crate::input;
use crate::manifest::{Manifest, TextureEntry};
use crate::output::write_via_temp;
use crate::pixel::{Decode, Layout};

/// A change to one texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextureEdit {
    /// A whole texture file of the same format and layout; its pixel data is used.
    Texture(Vec<u8>),
    /// New top-mip images, by (layer, slice). Mips below them are regenerated.
    Images(BTreeMap<(u32, u32), Image>),
}

/// Edits by texture id.
pub type Edits = BTreeMap<u32, TextureEdit>;

#[derive(Debug, Clone)]
pub struct PackOptions {
    /// Refuse to pack if the input's size or CRC differs from the manifest's.
    pub verify_source: bool,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self { verify_source: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The pixel data was replaced from another texture file.
    Replaced,
    /// `images` images were encoded, each with `mips` mip levels.
    Reencoded { images: usize, mips: u32 },
    /// The edit gives the same bytes as the original.
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TexturePlan {
    pub id: u32,
    pub offset: u64,
    pub size: u64,
    pub outcome: Outcome,
    /// Something worth knowing, such as alpha a format can't keep.
    pub note: Option<String>,
    /// CRC-32 of the new texture.
    pub crc32: u32,
}

#[derive(Debug, Clone)]
pub struct PackResult {
    /// One per edited texture, in file order.
    pub textures: Vec<TexturePlan>,
    /// New bytes for each changed texture, by offset; same length as the original.
    pub patches: Vec<(u64, Vec<u8>)>,
    pub input_len: u64,
}

impl PackResult {
    pub fn changed(&self) -> usize {
        self.patches.len()
    }

    /// Write the input with the patches applied to `path`, via a temporary file that
    /// is read back and checked before it replaces `path`.
    pub fn write_file(&self, data: &[u8], path: &Path) -> Result<()> {
        write_via_temp(path, |tmp| {
            let mut out = std::io::BufWriter::with_capacity(1 << 20, fs::File::create(tmp).map_err(io_err(tmp))?);
            let mut pos = 0usize;
            for (offset, bytes) in &self.patches {
                let offset = *offset as usize;
                out.write_all(&data[pos..offset]).map_err(io_err(tmp))?;
                out.write_all(bytes).map_err(io_err(tmp))?;
                pos = offset + bytes.len();
            }
            out.write_all(&data[pos..]).map_err(io_err(tmp))?;
            out.into_inner().map_err(|e| io_err(tmp)(e.into_error()))?.sync_all().map_err(io_err(tmp))?;
            let written = input::open(tmp)?;
            self.verify(&written)
        })
    }

    /// Check a packed file: the right length, and every changed texture where it
    /// belongs with its new CRC and a header that still parses to the same size.
    pub fn verify(&self, packed: &[u8]) -> Result<()> {
        if packed.len() as u64 != self.input_len {
            return Err(Error::Pack(format!("output is {} bytes, expected {}", packed.len(), self.input_len)));
        }
        for plan in self.textures.iter().filter(|p| p.outcome != Outcome::Unchanged) {
            let fail = |reason: String| Error::Verify { id: plan.id, offset: plan.offset, reason };
            let bytes = &packed[plan.offset as usize..(plan.offset + plan.size) as usize];
            let crc = crc32fast::hash(bytes);
            if crc != plan.crc32 {
                return Err(fail(format!("CRC-32 is {crc:08x}, expected {:08x}", plan.crc32)));
            }
        }
        Ok(())
    }
}

/// Build the new bytes of every edited texture and check them.
pub fn pack(data: &[u8], manifest: &Manifest, edits: &Edits, opts: &PackOptions) -> Result<PackResult> {
    if opts.verify_source {
        manifest.source.check(data)?;
    }
    if let Some(id) = edits.keys().find(|id| !manifest.textures.iter().any(|t| t.id == **id)) {
        return Err(Error::Pack(format!("there is no texture {id} in the manifest")));
    }
    let mut result = PackResult { textures: Vec::new(), patches: Vec::new(), input_len: data.len() as u64 };
    for entry in &manifest.textures {
        let Some(edit) = edits.get(&entry.id) else { continue };
        let (bytes, outcome, note) = pack_texture(texture_bytes(data, entry)?, entry, edit)?;
        let crc32 = crc32fast::hash(&bytes);
        if outcome != Outcome::Unchanged {
            result.patches.push((entry.offset, bytes));
        }
        result.textures.push(TexturePlan { id: entry.id, offset: entry.offset, size: entry.size, outcome, note, crc32 });
    }
    Ok(result)
}

/// The new bytes of one texture: `original` (its current bytes) with `edit` applied.
/// Also what happened, and a note if something was lost. Same length as `original`.
pub fn pack_texture(
    original: &[u8],
    entry: &TextureEntry,
    edit: &TextureEdit,
) -> Result<(Vec<u8>, Outcome, Option<String>)> {
    let fail = |reason: String| Error::Edit { id: entry.id, offset: entry.offset, reason };
    let info = format_for(entry.format).parse(original).map_err(|_| fail("its header no longer parses".into()))?;
    let (bytes, outcome, note) = match edit {
        TextureEdit::Texture(new) => (replace_data(original, &info, new).map_err(fail)?, Outcome::Replaced, None),
        TextureEdit::Images(images) => {
            let (bytes, note) = reencode(original, &info, images).map_err(fail)?;
            (bytes, Outcome::Reencoded { images: images.len(), mips: info.mips }, note)
        }
    };
    // The texture must still describe itself the same way.
    let check = format_for(entry.format).parse(&bytes).map_err(|_| fail("the new texture doesn't parse".into()))?;
    if check != info {
        return Err(fail("the new texture's header differs from the original".into()));
    }
    let outcome = if bytes == original { Outcome::Unchanged } else { outcome };
    Ok((bytes, outcome, note))
}

/// The original header followed by the replacement's pixel data, if the layouts match.
///
/// The replacement can be any container (a DDS for a KTX2 texture, say): each image is
/// copied from where the replacement keeps it to where the texture keeps it.
fn replace_data(original: &[u8], info: &TextureInfo, new: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let container = Container::sniff(new).ok_or("the replacement isn't a DDS or KTX2 texture")?;
    let new_info = format_for(container).parse(new).map_err(|e| match e {
        Reject::Bad(reason) => format!("the replacement {container} can't be used: {reason}"),
        Reject::NoMatch => format!("the replacement isn't a valid {container} texture"),
    })?;
    let describe = |i: &TextureInfo| {
        format!(
            "{}x{}x{}, {} mips, {} layer(s), {}",
            i.width,
            i.height,
            i.depth,
            i.mips,
            i.layers(),
            i.pixel_format.name
        )
    };
    let same_format = new_info.pixel_format.name == info.pixel_format.name
        || (new_info.pixel_format.dxgi.is_some()
            && new_info.pixel_format.dxgi == info.pixel_format.dxgi
            && new_info.pixel_format.decode == info.pixel_format.decode);
    let same_shape = (new_info.width, new_info.height, new_info.depth, new_info.mips, new_info.array_size, new_info.faces)
        == (info.width, info.height, info.depth, info.mips, info.array_size, info.faces);
    if !same_format || !same_shape {
        return Err(format!(
            "the replacement is {}, but the texture is {}; it must match to fit in place",
            describe(&new_info),
            describe(info)
        ));
    }
    if matches!(info.pixel_format.layout, Layout::Unknown) {
        return Err(format!("{} isn't a known pixel format, so its images can't be matched up", info.pixel_format.name));
    }
    let mut out = original.to_vec();
    for layer in 0..info.layers() {
        for mip in 0..info.mips {
            for slice in 0..info.mip_size(mip).2 {
                let sub = Subresource { layer, mip, slice };
                let to = info.subresource_range(sub).map_err(|e| format!("the texture: {e}"))?;
                let from = new_info.subresource_range(sub).map_err(|e| format!("the replacement: {e}"))?;
                out[to].copy_from_slice(&new[from]);
            }
        }
    }
    Ok(out)
}

/// Encode new top-mip images (and their mips) into a copy of the texture.
fn reencode(
    original: &[u8],
    info: &TextureInfo,
    images: &BTreeMap<(u32, u32), Image>,
) -> std::result::Result<(Vec<u8>, Option<String>), String> {
    let (w, h) = (info.width, info.height);
    for (&(layer, slice), image) in images {
        if layer >= info.layers() || slice >= info.depth {
            return Err(format!("it has no layer {layer}, slice {slice}"));
        }
        if (image.width, image.height) != (w, h) {
            return Err(format!("the new image is {}x{}, the texture {w}x{h}", image.width, image.height));
        }
    }
    let pf = info.pixel_format;
    let palette = match pf.decode {
        Decode::Palette { offset } => {
            let Layout::Linear { bits } = pf.layout else { unreachable!("palette formats are linear") };
            Some(&original[offset as usize..offset as usize + (4 << bits)])
        }
        _ => None,
    };
    let mut out = original.to_vec();
    // Images arrive upright (as decode gives them); turn them back to how they're stored.
    let mut put = |sub: Subresource, image: &Image| -> std::result::Result<(), String> {
        let stored = image.clone().oriented(info.orientation);
        let bytes = encode(&stored, &pf, palette).map_err(|e| e.to_string())?;
        let range = info.subresource_range(sub).map_err(|e| e.to_string())?;
        out[range].copy_from_slice(&bytes);
        Ok(())
    };
    if info.depth > 1 {
        // A volume's mips mix its slices, so the whole chain is rebuilt from all of the
        // top-level slices, edited or not.
        let top = (0..info.depth)
            .map(|z| match images.get(&(0, z)) {
                Some(image) => Ok(image.clone()),
                None => decode(original, info, Subresource { layer: 0, mip: 0, slice: z }).map_err(|e| e.to_string()),
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (mip, slices) in volume_mip_chain(top, info.mips).into_iter().enumerate() {
            for (slice, image) in slices.iter().enumerate() {
                put(Subresource { layer: 0, mip: mip as u32, slice: slice as u32 }, image)?;
            }
        }
    } else {
        for (&(layer, _), image) in images {
            for (mip, level) in mip_chain(image.clone(), info.mips).iter().enumerate() {
                put(Subresource { layer, mip: mip as u32, slice: 0 }, level)?;
            }
        }
    }
    let loses_alpha = matches!(pf.dxgi, Some(70..=72)) && images.values().any(|i| i.rgba.as_chunks::<4>().0.iter().any(|p| p[3] < 255));
    let note = loses_alpha.then(|| "BC1 is written opaque: the new image's transparency was dropped".to_string());
    Ok((out, note))
}

/// What [`load_edits`] found in an extract folder.
#[derive(Debug, Default)]
pub struct FoundEdits {
    pub edits: Edits,
    /// Files that were there but match the original (exported and not edited).
    pub unchanged: usize,
}

/// Find edits in a folder written by `extract`: a texture's file (`.dds`, `.ktx2`) whose CRC no longer
/// matches, or PNGs (named as [`png_images`] names them) whose pixels differ from the
/// original. Only one kind per texture.
pub fn load_edits(data: &[u8], manifest: &Manifest, dir: &Path) -> Result<FoundEdits> {
    let mut found = FoundEdits::default();
    for entry in &manifest.textures {
        let fail = |reason: String| Error::Edit { id: entry.id, offset: entry.offset, reason };
        let original = texture_bytes(data, entry)?;
        let info = format_for(entry.format).parse(original).map_err(|_| fail("its header no longer parses".into()))?;

        let dds_path = dir.join(&entry.file);
        let dds = match fs::read(&dds_path) {
            Ok(bytes) if crc32fast::hash(&bytes) != entry.crc32 => Some(bytes),
            Ok(_) => {
                found.unchanged += 1;
                None
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_err(&dds_path)(e)),
        };
        let images = load_pngs(original, entry, &info, dir, &mut found.unchanged)?;
        match (dds, images.is_empty()) {
            (Some(_), false) => {
                return Err(fail(format!(
                    "both {} and its PNGs were edited; keep one kind of edit per texture",
                    entry.file
                )));
            }
            (Some(bytes), true) => {
                found.edits.insert(entry.id, TextureEdit::Texture(bytes));
            }
            (None, false) => {
                found.edits.insert(entry.id, TextureEdit::Images(images));
            }
            (None, true) => {}
        }
    }
    Ok(found)
}

fn load_pngs(
    original: &[u8],
    entry: &TextureEntry,
    info: &TextureInfo,
    dir: &Path,
    unchanged: &mut usize,
) -> Result<BTreeMap<(u32, u32), Image>> {
    let fail = |reason: String| Error::Edit { id: entry.id, offset: entry.offset, reason };
    let stem = entry.file.rsplit_once('.').map_or(entry.file.as_str(), |(s, _)| s);
    let mut images = BTreeMap::new();
    for (sub, suffix) in png_images(info) {
        let name = format!("{stem}{suffix}.png");
        let path = dir.join(&name);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_err(&path)(e)),
        };
        let image = decode_png(&bytes).map_err(|e| fail(format!("{name}: {e}")))?;
        if (image.width, image.height) != (info.width, info.height) {
            return Err(fail(format!(
                "{name} is {}x{}, but the texture is {}x{}",
                image.width, image.height, info.width, info.height
            )));
        }
        // An exported PNG that wasn't edited decodes to exactly the original pixels.
        if decode(original, info, sub).is_ok_and(|o| o == image) {
            *unchanged += 1;
            continue;
        }
        images.insert((sub.layer, sub.slice), image);
    }
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_catches_a_wrong_crc_and_length() {
        let result = PackResult {
            textures: vec![TexturePlan {
                id: 0,
                offset: 2,
                size: 2,
                outcome: Outcome::Replaced,
                note: None,
                crc32: crc32fast::hash(b"ab"),
            }],
            patches: vec![(2, b"ab".to_vec())],
            input_len: 6,
        };
        assert!(result.verify(b"..ab..").is_ok());
        assert!(matches!(result.verify(b"..ax.."), Err(Error::Verify { id: 0, .. })));
        assert!(matches!(result.verify(b"..ab."), Err(Error::Pack(_))));
    }
}
