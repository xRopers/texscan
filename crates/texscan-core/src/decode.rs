//! Decoding one image of a texture to 8-bit RGBA, for previews and PNG export.
//!
//! Block-compressed formats (BC1–BC7) go through `bcdec_rs`, which follows the D3D spec
//! on the edge cases (BC2/BC3 colour is always four-colour, reserved BC6H/BC7 modes
//! decode to black). Uncompressed DXGI
//! formats are decoded from their names, which list channels from the lowest bits up
//! (`B5G6R5` has blue in bits 0–4). Legacy DDS formats are decoded by their bit masks.
//!
//! Values are converted as stored: sRGB data stays sRGB, and float formats are clamped
//! to 0–1 (no tone mapping). A format with only a red channel (R8, R16F, BC4...) is shown
//! as grey. Premultiplied DXT2/DXT4 are not un-premultiplied.

use std::borrow::Cow;
use std::io::Read;
use std::ops::Range;

use crate::format::{Orientation, Storage, TextureInfo};
use crate::pixel::{Decode, Etc, Layout};

/// An 8-bit RGBA image, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One 2D image within a texture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Subresource {
    /// Array element × faces + face. Cube faces are +X, −X, +Y, −Y, +Z, −Z.
    pub layer: u32,
    pub mip: u32,
    /// Depth slice of a volume texture.
    pub slice: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("{0}")]
    OutOfRange(String),
    #[error("decoding {0} isn't supported yet")]
    Unsupported(&'static str),
    #[error("texture data is shorter than its header says")]
    Short,
    #[error("{0} can be read but not written yet")]
    Supercompressed(&'static str),
    #[error("decompressing mip {mip}: {reason}")]
    Decompress { mip: u32, reason: String },
    #[error("the {0} data is invalid")]
    Corrupt(&'static str),
}

/// What a KTX2 supercompression scheme is called.
fn scheme_name(scheme: u32) -> &'static str {
    match scheme {
        1 => "BasisLZ-supercompressed KTX2",
        2 => "Zstandard-supercompressed KTX2",
        3 => "zlib-supercompressed KTX2",
        _ => "KTX2 with this supercompression scheme",
    }
}

impl TextureInfo {
    pub fn layers(&self) -> u32 {
        self.array_size * self.faces
    }

    /// Width, height and depth of a mip level.
    pub fn mip_size(&self, mip: u32) -> (u32, u32, u32) {
        ((self.width >> mip).max(1), (self.height >> mip).max(1), (self.depth >> mip).max(1))
    }

    /// Where an image's bytes are, relative to the start of the texture (see [`Storage`]).
    /// Supercompressed KTX2 images have no such place (see [`image_bytes`]).
    pub fn subresource_range(&self, sub: Subresource) -> Result<Range<usize>, DecodeError> {
        if let Storage::Ktx2 { supercompression, .. } = &self.storage
            && *supercompression != 0
        {
            return Err(DecodeError::Supercompressed(scheme_name(*supercompression)));
        }
        self.range_in_level_or_texture(sub)
    }

    /// For DDS and plain KTX2: the image's range in the texture. For supercompressed
    /// KTX2: its range within its level once decompressed.
    fn range_in_level_or_texture(&self, sub: Subresource) -> Result<Range<usize>, DecodeError> {
        let out = |what: &str, value: u32, count: u32| {
            DecodeError::OutOfRange(format!("{what} {value} doesn't exist (the texture has {count})"))
        };
        if sub.layer >= self.layers() {
            return Err(out("layer", sub.layer, self.layers()));
        }
        if sub.mip >= self.mips {
            return Err(out("mip", sub.mip, self.mips));
        }
        let (_, _, depth) = self.mip_size(sub.mip);
        if sub.slice >= depth {
            return Err(out("slice", sub.slice, depth));
        }
        let layout = self.pixel_format.layout;
        let image = |mip| {
            let (w, h, _) = self.mip_size(mip);
            layout.image_size(w, h)
        };
        if let Storage::Ktx2 { levels, supercompression } = &self.storage {
            let level = levels[sub.mip as usize];
            // Plain levels are addressed in the texture, decompressed ones from 0.
            let base = if *supercompression == 0 { level.offset } else { 0 };
            let start = base + (u64::from(sub.layer) * u64::from(depth) + u64::from(sub.slice)) * image(sub.mip);
            let end = start + image(sub.mip);
            if end > base + level.uncompressed {
                return Err(DecodeError::Short);
            }
            return Ok(start as usize..end as usize);
        }
        let per_layer: u64 = (0..self.mips).map(|m| image(m) * u64::from(self.mip_size(m).2)).sum();
        let before_mip: u64 = (0..sub.mip).map(|m| image(m) * u64::from(self.mip_size(m).2)).sum();
        let start =
            self.header_size + u64::from(sub.layer) * per_layer + before_mip + u64::from(sub.slice) * image(sub.mip);
        let end = start + image(sub.mip);
        Ok(start as usize..end as usize)
    }
}

/// One image's stored bytes (still block-compressed or whatever the pixel format is), with
/// KTX2 Zstandard or zlib supercompression undone. `texture` starts at the texture's
/// first header byte.
pub fn image_bytes<'a>(texture: &'a [u8], info: &TextureInfo, sub: Subresource) -> Result<Cow<'a, [u8]>, DecodeError> {
    let Storage::Ktx2 { levels, supercompression } = &info.storage else {
        return Ok(Cow::Borrowed(texture.get(info.subresource_range(sub)?).ok_or(DecodeError::Short)?));
    };
    if *supercompression == 0 {
        return Ok(Cow::Borrowed(texture.get(info.subresource_range(sub)?).ok_or(DecodeError::Short)?));
    }
    let range = info.range_in_level_or_texture(sub)?;
    let level = levels[sub.mip as usize];
    let stored = texture.get(level.offset as usize..(level.offset + level.length) as usize).ok_or(DecodeError::Short)?;
    let fail = |reason: String| DecodeError::Decompress { mip: sub.mip, reason };
    let limit = level.uncompressed as usize;
    let whole = match supercompression {
        2 => {
            let mut out = Vec::with_capacity(limit);
            let decoder = ruzstd::decoding::StreamingDecoder::new(stored).map_err(|e| fail(e.to_string()))?;
            decoder.take(limit as u64 + 1).read_to_end(&mut out).map_err(|e| fail(e.to_string()))?;
            out
        }
        3 => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(stored, limit + 1).map_err(|e| fail(format!("{e:?}")))?,
        other => return Err(DecodeError::Unsupported(scheme_name(*other))),
    };
    if whole.len() != limit {
        return Err(fail(format!("{} bytes, the level index says {limit}", whole.len())));
    }
    Ok(Cow::Owned(whole[range].to_vec()))
}

/// Decode one image. `texture` starts at the texture's first header byte.
pub fn decode(texture: &[u8], info: &TextureInfo, sub: Subresource) -> Result<Image, DecodeError> {
    let bytes = image_bytes(texture, info, sub)?;
    let data = &bytes[..];
    let (width, height, _) = info.mip_size(sub.mip);
    let pf = info.pixel_format;
    let unsupported = || DecodeError::Unsupported(pf.name);
    let (w, h) = (width as usize, height as usize);
    let rgba = match pf.decode {
        Decode::Dxgi => decode_dxgi(pf.dxgi.ok_or_else(unsupported)?, data, w, h)?,
        Decode::Uyvy => pairs(data, w, h, Pairs::Yuv { y: [1, 3], u: 0, v: 2, sample: 1 }),
        Decode::Channels(spec) => by_channels(data, w * h, &parse_channels(spec).ok_or_else(unsupported)?),
        Decode::Etc(kind) => {
            use texture2ddecoder as t;
            let decoder: T2dDecoder = match kind {
                Etc::Rgb => t::decode_etc2_rgb,
                Etc::Rgba1 => t::decode_etc2_rgba1,
                Etc::Rgba8 => t::decode_etc2_rgba8,
                Etc::R11 => t::decode_eacr,
                Etc::R11Signed => t::decode_eacr_signed,
                Etc::Rg11 => t::decode_eacrg,
                Etc::Rg11Signed => t::decode_eacrg_signed,
            };
            let rgba = t2d("ETC2/EAC", |px| decoder(data, w, h, px), w * h)?;
            match kind {
                // One channel shows as grey; two leave blue empty (mid-grey when signed).
                Etc::R11 | Etc::R11Signed => grey_from_red(rgba),
                Etc::Rg11Signed => with_blue(rgba, 128),
                _ => rgba,
            }
        }
        Decode::Astc => {
            let Layout::Tiles { width: bw, height: bh, .. } = pf.layout else { return Err(unsupported()) };
            t2d("ASTC", |px| texture2ddecoder::decode_astc(data, w, h, bw as usize, bh as usize, px), w * h)?
        }
        Decode::Rxgb => {
            let mut rgba = blocks(data, w, h, 16, Block::Rgba(bcdec_rs::bc3));
            for px in rgba.as_chunks_mut::<4>().0 {
                px[0] = px[3];
                px[3] = 255;
            }
            rgba
        }
        Decode::Masks { r, g, b, a, luminance, signed } => {
            let Layout::Linear { bits } = pf.layout else { return Err(unsupported()) };
            masks(data, w * h, bits, [r, g, b, a], luminance, signed)
        }
        Decode::Palette { offset } => {
            let Layout::Linear { bits } = pf.layout else { return Err(unsupported()) };
            let start = offset as usize;
            let palette = texture.get(start..start + (4 << bits)).ok_or(DecodeError::Short)?;
            palettized(data, w, h, bits, palette)
        }
        Decode::None => return Err(unsupported()),
    };
    Ok(Image { width, height, rgba }.oriented(info.orientation))
}

impl Image {
    /// Mirrored as `orientation` says: from stored to upright, or back (it's its own
    /// inverse).
    pub fn oriented(mut self, orientation: Orientation) -> Image {
        let (w, h) = (self.width as usize, self.height as usize);
        if orientation.flip_y {
            let row = w * 4;
            for y in 0..h / 2 {
                let (top, bottom) = self.rgba.split_at_mut((h - 1 - y) * row);
                top[y * row..(y + 1) * row].swap_with_slice(&mut bottom[..row]);
            }
        }
        if orientation.flip_x {
            for row in self.rgba.chunks_exact_mut(w * 4) {
                let pixels = row.as_chunks_mut::<4>().0;
                pixels.reverse();
            }
        }
        self
    }
}

type T2dDecoder = fn(&[u8], usize, usize, &mut [u32]) -> Result<(), &'static str>;

/// Run a `texture2ddecoder` decoder over `count` pixels. It can panic on invalid block
/// data (its ASTC bit reader indexes out of range), so a panic becomes an error rather
/// than taking down the caller, which may be a thread pool.
fn t2d(
    what: &'static str,
    decode: impl FnOnce(&mut [u32]) -> Result<(), &'static str>,
    count: usize,
) -> Result<Vec<u8>, DecodeError> {
    let mut pixels = vec![0u32; count];
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(&mut pixels))) {
        Ok(Ok(())) => Ok(bgra_to_rgba(&pixels)),
        Ok(Err(_)) => Err(DecodeError::Short),
        Err(_) => Err(DecodeError::Corrupt(what)),
    }
}

/// `texture2ddecoder` writes BGRA words.
fn bgra_to_rgba(pixels: &[u32]) -> Vec<u8> {
    pixels
        .iter()
        .flat_map(|p| {
            let [b, g, r, a] = p.to_le_bytes();
            [r, g, b, a]
        })
        .collect()
}

fn grey_from_red(mut rgba: Vec<u8>) -> Vec<u8> {
    for px in rgba.as_chunks_mut::<4>().0 {
        px[1] = px[0];
        px[2] = px[0];
    }
    rgba
}

fn with_blue(mut rgba: Vec<u8>, blue: u8) -> Vec<u8> {
    for px in rgba.as_chunks_mut::<4>().0 {
        px[2] = blue;
    }
    rgba
}

/// A `bcdec_rs` block decoder and what it writes.
#[derive(Clone, Copy)]
enum Block {
    /// RGBA8.
    Rgba(fn(&[u8], &mut [u8], usize)),
    /// One (BC4) or two (BC5) channels as floats, 0–1, or −1–1 when `signed`.
    Channels { count: usize, signed: bool },
    /// BC6H: RGB floats.
    Hdr { signed: bool },
}

/// Decode 4×4 blocks of `block_bytes` each into an RGBA image, dropping the pixels of
/// edge blocks that fall outside it. The caller has checked the data length.
fn blocks(data: &[u8], w: usize, h: usize, block_bytes: usize, kind: Block) -> Vec<u8> {
    let across = w.div_ceil(4);
    let mut out = vec![0u8; w * h * 4];
    let mut rgba = [0u8; 64];
    let mut floats = [0f32; 48];
    for (i, block) in data.chunks_exact(block_bytes).take(across * h.div_ceil(4)).enumerate() {
        let (bx, by) = (i % across * 4, i / across * 4);
        match kind {
            Block::Rgba(f) => f(block, &mut rgba, 16),
            Block::Channels { count, signed } => {
                match count {
                    1 => bcdec_rs::bc4_float(block, &mut floats, 4, signed),
                    _ => bcdec_rs::bc5_float(block, &mut floats, 8, signed),
                }
                let to_u8 = |f: f32| if signed { float_to_u8(f * 0.5 + 0.5) } else { float_to_u8(f) };
                for p in 0..16 {
                    let r = to_u8(floats[p * count]);
                    // BC5 has no blue; show it as zero, which is mid-grey when signed.
                    let blue = if signed { 128 } else { 0 };
                    let px = if count == 1 { [r, r, r, 255] } else { [r, to_u8(floats[p * 2 + 1]), blue, 255] };
                    rgba[p * 4..p * 4 + 4].copy_from_slice(&px);
                }
            }
            Block::Hdr { signed } => {
                bcdec_rs::bc6h_float(block, &mut floats, 12, signed);
                for p in 0..16 {
                    let c = &floats[p * 3..p * 3 + 3];
                    rgba[p * 4..p * 4 + 4].copy_from_slice(&[float_to_u8(c[0]), float_to_u8(c[1]), float_to_u8(c[2]), 255]);
                }
            }
        }
        let cols = 4.min(w - bx);
        for y in 0..4.min(h - by) {
            let dst = ((by + y) * w + bx) * 4;
            out[dst..dst + cols * 4].copy_from_slice(&rgba[y * 16..y * 16 + cols * 4]);
        }
    }
    out
}

/// Decode as DXGI format `id`. Legacy formats that map to one come here too, so the
/// channel layout comes from the DXGI name, not the format's own.
fn decode_dxgi(id: u32, data: &[u8], w: usize, h: usize) -> Result<Vec<u8>, DecodeError> {
    let name = crate::pixel::dxgi(id).map_err(|_| DecodeError::Unsupported("an unknown DXGI format"))?.name;
    let rgba = match id {
        70..=72 => blocks(data, w, h, 8, Block::Rgba(bcdec_rs::bc1)),
        73..=75 => blocks(data, w, h, 16, Block::Rgba(bcdec_rs::bc2)),
        76..=78 => blocks(data, w, h, 16, Block::Rgba(bcdec_rs::bc3)),
        79 | 80 => blocks(data, w, h, 8, Block::Channels { count: 1, signed: false }),
        81 => blocks(data, w, h, 8, Block::Channels { count: 1, signed: true }),
        82 | 83 => blocks(data, w, h, 16, Block::Channels { count: 2, signed: false }),
        84 => blocks(data, w, h, 16, Block::Channels { count: 2, signed: true }),
        94 | 95 => blocks(data, w, h, 16, Block::Hdr { signed: false }),
        96 => blocks(data, w, h, 16, Block::Hdr { signed: true }),
        97..=99 => blocks(data, w, h, 16, Block::Rgba(bcdec_rs::bc7)),
        26 => packed(data, w * h, r11g11b10),
        67 => packed(data, w * h, r9g9b9e5),
        68 => pairs(data, w, h, Pairs::Rgb { r: 0, g: [1, 3], b: 2 }),
        69 => pairs(data, w, h, Pairs::Rgb { r: 1, g: [0, 2], b: 3 }),
        107 => pairs(data, w, h, Pairs::Yuv { y: [0, 2], u: 1, v: 3, sample: 1 }),
        108 | 109 => pairs(data, w, h, Pairs::Yuv { y: [0, 2], u: 1, v: 3, sample: 2 }),
        _ => {
            let channels = parse_channels(name).ok_or(DecodeError::Unsupported(name))?;
            by_channels(data, w * h, &channels)
        }
    };
    Ok(rgba)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Unorm,
    Snorm,
    Uint,
    Sint,
    Float,
}

/// A channel of an uncompressed format: which RGBA slot it fills (`None` for X) and how
/// many bits it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Channel {
    pub(crate) slot: Option<usize>,
    pub(crate) bits: u32,
    pub(crate) kind: Kind,
}

/// `R16G16B16A16_FLOAT` → four 16-bit float channels. `None` for names that don't
/// follow the pattern (typeless formats other than 8-bit, depth-stencil, video...).
pub(crate) fn parse_channels(name: &str) -> Option<Vec<Channel>> {
    let (components, suffix) = name.split_once('_')?;
    let mut channels = Vec::new();
    let mut chars = components.chars().peekable();
    while let Some(c) = chars.next() {
        let slot = match c {
            'R' | 'D' => Some(0),
            'G' => Some(1),
            'B' => Some(2),
            'A' => Some(3),
            'X' => None,
            _ => return None,
        };
        let mut digits = String::new();
        while let Some(d) = chars.next_if(char::is_ascii_digit) {
            digits.push(d);
        }
        channels.push(Channel { slot, bits: digits.parse().ok()?, kind: Kind::Unorm });
    }
    let kind = match suffix {
        "UNORM" | "UNORM_SRGB" => Kind::Unorm,
        "TYPELESS" if channels.iter().all(|c| c.bits == 8) => Kind::Unorm,
        "SNORM" => Kind::Snorm,
        "UINT" => Kind::Uint,
        "SINT" => Kind::Sint,
        "FLOAT" if channels.iter().all(|c| matches!(c.bits, 16 | 32)) => Kind::Float,
        _ => return None,
    };
    let total: u32 = channels.iter().map(|c| c.bits).sum();
    if !total.is_multiple_of(8) || total > 128 || channels.iter().any(|c| c.bits == 0 || c.bits > 32) {
        return None;
    }
    channels.iter_mut().for_each(|c| c.kind = kind);
    Some(channels)
}

/// Little-endian pixels of `bytes` bytes each.
fn pixels(data: &[u8], count: usize, bytes: usize) -> impl Iterator<Item = u128> + '_ {
    data.chunks_exact(bytes).take(count).map(|px| px.iter().rev().fold(0u128, |acc, &b| acc << 8 | u128::from(b)))
}

fn unorm(v: u64, bits: u32) -> u8 {
    let max = (1u64 << bits) - 1;
    ((v * 255 + max / 2) / max) as u8
}

fn float_to_u8(f: f32) -> u8 {
    (f.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let mant = f32::from(h & 0x3ff);
    sign * match exp {
        0 => mant * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + mant / 1024.0) * 2f32.powi(exp - 15),
    }
}

fn channel_to_u8(v: u64, c: Channel) -> u8 {
    let sign_extend = |v: u64| ((v << (64 - c.bits)) as i64) >> (64 - c.bits);
    match c.kind {
        Kind::Unorm => unorm(v, c.bits),
        Kind::Snorm => {
            let max = ((1i64 << (c.bits - 1)) - 1) as f32;
            float_to_u8(((sign_extend(v) as f32 / max).max(-1.0)) * 0.5 + 0.5)
        }
        Kind::Uint => v.min(255) as u8,
        Kind::Sint => sign_extend(v).clamp(0, 255) as u8,
        Kind::Float if c.bits == 16 => float_to_u8(half_to_f32(v as u16)),
        Kind::Float => float_to_u8(f32::from_bits(v as u32)),
    }
}

fn by_channels(data: &[u8], count: usize, channels: &[Channel]) -> Vec<u8> {
    let bytes = (channels.iter().map(|c| c.bits).sum::<u32>() / 8) as usize;
    let red_only = channels.iter().all(|c| matches!(c.slot, Some(0) | None));
    let mut out = Vec::with_capacity(count * 4);
    for px in pixels(data, count, bytes) {
        let mut rgba = [0, 0, 0, 255];
        let mut shift = 0;
        for &c in channels {
            if let Some(slot) = c.slot {
                let v = (px >> shift) as u64 & ((1u64 << c.bits) - 1);
                rgba[slot] = channel_to_u8(v, c);
            }
            shift += c.bits;
        }
        if red_only {
            rgba[1] = rgba[0];
            rgba[2] = rgba[0];
        }
        out.extend_from_slice(&rgba);
    }
    out
}

/// A 32-bit packed format decoded by a function.
fn packed(data: &[u8], count: usize, f: fn(u32) -> [f32; 3]) -> Vec<u8> {
    pixels(data, count, 4)
        .flat_map(|px| {
            let [r, g, b] = f(px as u32);
            [float_to_u8(r), float_to_u8(g), float_to_u8(b), 255]
        })
        .collect()
}

/// Unsigned float with a 5-bit exponent and `mant_bits` of mantissa.
fn small_float(v: u32, mant_bits: u32) -> f32 {
    let exp = (v >> mant_bits) as i32;
    let mant = (v & ((1 << mant_bits) - 1)) as f32 / (1 << mant_bits) as f32;
    match exp {
        0 => mant * 2f32.powi(-14),
        31 => f32::INFINITY,
        _ => (1.0 + mant) * 2f32.powi(exp - 15),
    }
}

fn r11g11b10(px: u32) -> [f32; 3] {
    [small_float(px & 0x7ff, 6), small_float((px >> 11) & 0x7ff, 6), small_float(px >> 22, 5)]
}

fn r9g9b9e5(px: u32) -> [f32; 3] {
    let scale = 2f32.powi((px >> 27) as i32 - 15 - 9);
    [(px & 0x1ff) as f32 * scale, ((px >> 9) & 0x1ff) as f32 * scale, ((px >> 18) & 0x1ff) as f32 * scale]
}

fn masks(data: &[u8], count: usize, bits: u32, masks: [u32; 4], luminance: bool, signed: u8) -> Vec<u8> {
    let channel = |px: u32, i: usize, default: u8| {
        let mask = masks[i];
        if mask == 0 {
            return default;
        }
        let shift = mask.trailing_zeros();
        let bits = (mask >> shift).count_ones();
        let kind = if signed >> i & 1 != 0 { Kind::Snorm } else { Kind::Unorm };
        channel_to_u8(u64::from((px & mask) >> shift), Channel { slot: None, bits, kind })
    };
    pixels(data, count, (bits / 8) as usize)
        .flat_map(|px| {
            let px = px as u32;
            let red = channel(px, 0, 0);
            let (green, blue) = if luminance { (red, red) } else { (channel(px, 1, 0), channel(px, 2, 0)) };
            [red, green, blue, channel(px, 3, 255)]
        })
        .collect()
}

/// How a 4:2:2 format stores a pair of pixels: sample positions within the pair.
#[derive(Debug, Clone, Copy)]
enum Pairs {
    /// Shared red and blue, a green each (R8G8_B8G8, G8R8_G8B8).
    Rgb { r: usize, g: [usize; 2], b: usize },
    /// A luma each, shared chroma, samples of `sample` bytes of which the top byte is used
    /// (YUY2, UYVY, Y210, Y216).
    Yuv { y: [usize; 2], u: usize, v: usize, sample: usize },
}

fn pairs(data: &[u8], w: usize, h: usize, order: Pairs) -> Vec<u8> {
    let bytes = match order {
        Pairs::Rgb { .. } => 4,
        Pairs::Yuv { sample, .. } => 4 * sample,
    };
    let row_bytes = w.div_ceil(2) * bytes;
    let mut out = Vec::with_capacity(w * h * 4);
    for row in data.chunks_exact(row_bytes).take(h) {
        for x in 0..w {
            let pair = &row[x / 2 * bytes..][..bytes];
            let px = match order {
                Pairs::Rgb { r, g, b } => [pair[r], pair[g[x % 2]], pair[b], 255],
                Pairs::Yuv { y, u, v, sample } => {
                    let at = |i: usize| i32::from(pair[i * sample + sample - 1]);
                    yuv_to_rgb(at(y[x % 2]), at(u), at(v))
                }
            };
            out.extend_from_slice(&px);
        }
    }
    out
}

/// BT.601, studio range, as Direct3D converts YUY2.
fn yuv_to_rgb(y: i32, u: i32, v: i32) -> [u8; 4] {
    let (c, d, e) = (y - 16, u - 128, v - 128);
    let clip = |x: i32| ((x + 128) >> 8).clamp(0, 255) as u8;
    [clip(298 * c + 409 * e), clip(298 * c - 100 * d - 208 * e), clip(298 * c + 516 * d), 255]
}

/// 8- or 4-bit indices; with 4 bits the first pixel is in the low nibble.
fn palettized(data: &[u8], w: usize, h: usize, bits: u32, palette: &[u8]) -> Vec<u8> {
    let row_bytes = (w * bits as usize).div_ceil(8);
    let mut out = Vec::with_capacity(w * h * 4);
    for row in data.chunks_exact(row_bytes).take(h) {
        for x in 0..w {
            let index = match bits {
                8 => row[x] as usize,
                _ => (row[x / 2] >> (4 * (x % 2)) & 0xf) as usize,
            };
            out.extend_from_slice(&palette[index * 4..index * 4 + 4]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pixel::{self, PixelFormat};

    fn info(width: u32, height: u32, pf: PixelFormat) -> TextureInfo {
        let size = pf.layout.image_size(width, height);
        TextureInfo {
            size,
            header_size: 0,
            width,
            height,
            depth: 1,
            mips: 1,
            array_size: 1,
            faces: 1,
            pixel_format: pf,
            storage: crate::format::Storage::Dds,
            orientation: Default::default(),
        }
    }

    fn one(data: &[u8], pf: PixelFormat) -> Vec<u8> {
        decode(data, &info(1, 1, pf), Subresource::default()).unwrap().rgba
    }

    #[test]
    fn orientation_flips_rows_and_columns() {
        let image = Image { width: 2, height: 2, rgba: (0..16).collect() };
        let up = image.clone().oriented(Orientation { flip_x: false, flip_y: true });
        assert_eq!(up.rgba, [8, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3, 4, 5, 6, 7]);
        let left = image.clone().oriented(Orientation { flip_x: true, flip_y: false });
        assert_eq!(left.rgba, [4, 5, 6, 7, 0, 1, 2, 3, 12, 13, 14, 15, 8, 9, 10, 11]);
        assert_eq!(up.oriented(Orientation { flip_x: false, flip_y: true }), image);
    }

    #[test]
    fn channel_names() {
        let c = parse_channels("B5G6R5_UNORM").unwrap();
        assert_eq!(c.iter().map(|c| (c.slot, c.bits)).collect::<Vec<_>>(), [(Some(2), 5), (Some(1), 6), (Some(0), 5)]);
        assert!(parse_channels("R16_TYPELESS").is_none());
        assert!(parse_channels("D24_UNORM_S8_UINT").is_none());
        assert!(parse_channels("R8G8_B8G8_UNORM").is_none());
        assert!(parse_channels("R1_UNORM").is_none());
        assert!(parse_channels("AYUV").is_none());
    }

    #[test]
    fn uncompressed_dxgi() {
        let dxgi = |id| pixel::dxgi(id).unwrap();
        assert_eq!(one(&[1, 2, 3, 4], dxgi(28)), [1, 2, 3, 4]);
        assert_eq!(one(&[1, 2, 3, 4], dxgi(87)), [3, 2, 1, 4]);
        // R5G6B5 pure red, stored as B5G6R5.
        assert_eq!(one(&0xf800u16.to_le_bytes(), dxgi(85)), [255, 0, 0, 255]);
        // Half-float 1.0, 0.5, 2.0 (clamped), -1 (clamped).
        let halves: Vec<u8> = [0x3c00u16, 0x3800, 0x4000, 0xbc00].iter().flat_map(|h| h.to_le_bytes()).collect();
        assert_eq!(one(&halves, dxgi(10)), [255, 128, 255, 0]);
        // One red channel shows as grey.
        assert_eq!(one(&[200], dxgi(61)), [200, 200, 200, 255]);
        assert_eq!(one(&0.5f32.to_le_bytes(), dxgi(41)), [128, 128, 128, 255]);
        // SNORM: -1 → 0, 0 → 128ish, 1 → 255.
        assert_eq!(one(&[0x81, 0, 0x7f, 0], dxgi(31)), [0, 128, 255, 128]);
        // R11G11B10: 1.0 in each (exponent 15, mantissa 0).
        let px = (15u32 << 6) | ((15u32 << 6) << 11) | ((15u32 << 5) << 22);
        assert_eq!(one(&px.to_le_bytes(), dxgi(26)), [255, 255, 255, 255]);
        assert!(matches!(decode(&[0; 4], &info(1, 1, dxgi(45)), Subresource::default()), Err(DecodeError::Unsupported(_))));
    }

    #[test]
    fn packed_pairs() {
        let rgbg = pixel::dxgi(68).unwrap();
        let out = decode(&[10, 20, 30, 40], &info(2, 1, rgbg), Subresource::default()).unwrap();
        assert_eq!(out.rgba, [10, 20, 30, 255, 10, 40, 30, 255]);
        // Studio-range black and white, no chroma.
        let yuy2 = pixel::dxgi(107).unwrap();
        let out = decode(&[16, 128, 235, 128], &info(2, 1, yuy2), Subresource::default()).unwrap();
        assert_eq!(out.rgba, [0, 0, 0, 255, 255, 255, 255, 255]);
        // An odd width still stores whole pairs.
        let out = decode(&[16, 128, 235, 128, 235, 128, 16, 128], &info(3, 1, yuy2), Subresource::default());
        assert_eq!(out.unwrap().rgba.len(), 12);
    }

    #[test]
    fn legacy_float_fourcc_decodes_by_its_dxgi_layout() {
        // R32F is D3D9's name for R32_FLOAT.
        let r32f = PixelFormat::new("R32F", Some(41), Layout::Linear { bits: 32 });
        assert_eq!(one(&1.0f32.to_le_bytes(), r32f), [255, 255, 255, 255]);
    }

    #[test]
    fn invalid_astc_is_an_error_not_a_crash() {
        let astc = PixelFormat::new("ASTC_6x6_UNORM_BLOCK", None, Layout::Tiles { width: 6, height: 6, bytes: 16 })
            .with_decode(Decode::Astc);
        // Random blocks, some of which make texture2ddecoder's bit reader overrun.
        let mut x = 0x1234_5678_9abc_def1u64;
        let mut failures = 0;
        for _ in 0..64 {
            let block: Vec<u8> = (0..16)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect();
            match decode(&block, &info(6, 6, astc), Subresource::default()) {
                Ok(image) => assert_eq!(image.rgba.len(), 6 * 6 * 4),
                Err(e) => {
                    assert_eq!(e, DecodeError::Corrupt("ASTC"));
                    failures += 1;
                }
            }
        }
        assert!(failures > 0, "the random blocks should include some the decoder can't handle");
    }

    #[test]
    fn astc_void_extent_block() {
        // A solid-colour ("void-extent") block: RGBA as 16-bit values.
        let mut block = vec![0xFC, 0xFD, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        for v in [0xFFFFu16, 0x8000, 0x0000, 0xFFFF] {
            block.extend_from_slice(&v.to_le_bytes());
        }
        let astc = PixelFormat::new("ASTC_4x4_UNORM_BLOCK", None, Layout::Tiles { width: 4, height: 4, bytes: 16 })
            .with_decode(Decode::Astc);
        let out = decode(&block, &info(4, 4, astc), Subresource::default()).unwrap();
        assert!(out.rgba.chunks(4).all(|p| p == [255, 128, 0, 255]), "{:?}", &out.rgba[..4]);
    }

    #[test]
    fn bc3_colour_is_always_four_colour() {
        // Colour 0 < colour 1 would mean three-colour mode in BC1, where index 3 is
        // transparent black; BC3's colour block always interpolates. Opaque alpha first.
        let block = [255, 255, 0, 0, 0, 0, 0, 0, 0x1f, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff, 0xff];
        let out = decode(&block, &info(4, 4, pixel::dxgi(77).unwrap()), Subresource::default()).unwrap();
        assert!(out.rgba.chunks(4).all(|p| p[3] == 255 && p[..3] != [0, 0, 0]), "{:?}", &out.rgba[..4]);
        let out = decode(&block[8..], &info(4, 4, pixel::dxgi(71).unwrap()), Subresource::default()).unwrap();
        assert!(out.rgba.chunks(4).all(|p| p == [0, 0, 0, 0]));
    }

    #[test]
    fn bc4_signed() {
        // Endpoints -127 and 127, every index 0: -1, shown as black.
        let block = [0x81, 0x7f, 0, 0, 0, 0, 0, 0];
        let out = decode(&block, &info(4, 4, pixel::dxgi(81).unwrap()), Subresource::default()).unwrap();
        assert!(out.rgba.chunks(4).all(|p| p == [0, 0, 0, 255]));
    }

    #[test]
    fn edge_blocks_are_cropped() {
        // 5x3 BC1: two blocks across, one down; the right one is green.
        let red = [0x00, 0xf8, 0, 0, 0, 0, 0, 0];
        let green = [0xe0, 0x07, 0, 0, 0, 0, 0, 0];
        let data = [red, green].concat();
        let out = decode(&data, &info(5, 3, pixel::dxgi(71).unwrap()), Subresource::default()).unwrap();
        assert_eq!(out.rgba.len(), 5 * 3 * 4);
        assert_eq!(&out.rgba[12..20], [255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn bc1_solid_block() {
        // Colour 0 red, colour 1 blue, every index 0.
        let block = [0x00, 0xf8, 0x1f, 0x00, 0, 0, 0, 0];
        let out = decode(&block, &info(4, 4, pixel::dxgi(71).unwrap()), Subresource::default()).unwrap();
        assert_eq!(out.rgba.len(), 64);
        assert!(out.rgba.chunks(4).all(|p| p == [255, 0, 0, 255]));
        // A 2x2 mip still takes a whole block, and decodes to 2x2.
        let small = decode(&block, &info(2, 2, pixel::dxgi(71).unwrap()), Subresource::default()).unwrap();
        assert_eq!((small.width, small.height, small.rgba.len()), (2, 2, 16));
    }

    #[test]
    fn legacy_masks_and_palette() {
        let a8l8 = PixelFormat::new("A8L8", Some(49), Layout::Linear { bits: 16 })
            .with_decode(Decode::Masks { r: 0xff, g: 0, b: 0, a: 0xff00, luminance: true, signed: 0 });
        assert_eq!(one(&[90, 7], a8l8), [90, 90, 90, 7]);
        let x1r5g5b5 = PixelFormat::new("X1R5G5B5", None, Layout::Linear { bits: 16 })
            .with_decode(Decode::Masks { r: 0x7c00, g: 0x3e0, b: 0x1f, a: 0, luminance: false, signed: 0 });
        assert_eq!(one(&0x7c1fu16.to_le_bytes(), x1r5g5b5), [255, 0, 255, 255]);
        // V8U8: signed, so -127 → 0, 0 → mid-grey.
        let v8u8 = PixelFormat::new("V8U8", Some(51), Layout::Linear { bits: 16 })
            .with_decode(Decode::Masks { r: 0xff, g: 0xff00, b: 0, a: 0, luminance: false, signed: 0b0111 });
        assert_eq!(one(&[0x81, 0], v8u8), [0, 128, 0, 255]);

        // P4: a 4-byte palette of 16 entries, then two pixels in one byte.
        let mut tex = vec![0u8; 64];
        tex[4..8].copy_from_slice(&[1, 2, 3, 4]);
        tex[8..12].copy_from_slice(&[5, 6, 7, 8]);
        tex.push(0x21);
        let p4 = PixelFormat::new("P4", None, Layout::Linear { bits: 4 }).with_decode(Decode::Palette { offset: 0 });
        let mut i = info(2, 1, p4);
        i.header_size = 64;
        assert_eq!(decode(&tex, &i, Subresource::default()).unwrap().rgba, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn subresource_ranges() {
        // 8x8 RGBA8 cube with 4 mips: 256 + 64 + 16 + 4 bytes per face.
        let mut i = info(8, 8, pixel::dxgi(28).unwrap());
        i.header_size = 148;
        i.mips = 4;
        i.faces = 6;
        let range = |layer, mip| i.subresource_range(Subresource { layer, mip, slice: 0 }).unwrap();
        assert_eq!(range(0, 0), 148..404);
        assert_eq!(range(0, 3), 148 + 336..148 + 340);
        assert_eq!(range(2, 1), 148 + 2 * 340 + 256..148 + 2 * 340 + 320);
        assert!(i.subresource_range(Subresource { layer: 6, mip: 0, slice: 0 }).is_err());
        assert!(i.subresource_range(Subresource { layer: 0, mip: 4, slice: 0 }).is_err());
        // Volume: slices halve with each mip.
        let mut v = info(4, 4, pixel::dxgi(61).unwrap());
        (v.depth, v.mips) = (4, 3);
        let range = |mip, slice| v.subresource_range(Subresource { layer: 0, mip, slice }).unwrap();
        assert_eq!(range(0, 3), 48..64);
        assert_eq!(range(1, 1), 64 + 4..64 + 8);
        assert_eq!(range(2, 0), 72..73);
        assert!(v.subresource_range(Subresource { layer: 0, mip: 1, slice: 2 }).is_err());
    }
}
