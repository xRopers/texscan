//! Encoding RGBA images back into a texture's pixel format, and making mips: the reverse
//! of [`crate::decode`], used by pack.
//!
//! Every format that decodes can be encoded, except signed BC4/BC5/BC6H, the 4:2:2 and
//! planar video formats, and R11G11B10/R9G9B9E5. Block formats use `block_compression`
//! (a CPU port of Intel's ISPC compressor), in parallel over rows of blocks. A palette
//! format can only be written if every colour is already in its palette.
//!
//! Values are written as they come, the way decode reads them: an 8-bit image going
//! into a float format gives values in 0–1, so HDR range is lost (edit the `.dds` for
//! that). Formats with one red channel take it from red; signed channels map 128 to 0.

use block_compression::encode::{compress_rgba8, compress_rgba16};
use block_compression::half::f16;
use block_compression::{BC6HSettings, BC7Settings, CompressionVariant};
use rayon::prelude::*;

use crate::decode::{Channel, Image, Kind, parse_channels};
use crate::pixel::{Decode, Layout, PixelFormat};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("writing {0} isn't supported yet")]
    Unsupported(&'static str),
    #[error("{0}")]
    Palette(String),
}

/// Encode one image (one mip of one layer or slice) in `pf`. `palette` is the texture's
/// palette for palette formats: four-byte RGBA entries.
pub fn encode(image: &Image, pf: &PixelFormat, palette: Option<&[u8]>) -> Result<Vec<u8>, EncodeError> {
    let unsupported = || EncodeError::Unsupported(pf.name);
    match pf.decode {
        Decode::Dxgi => encode_dxgi(image, pf.dxgi.ok_or_else(unsupported)?).ok_or_else(unsupported),
        Decode::Rxgb => {
            // Red goes in alpha; the colour block's red is unused.
            let mut moved = image.clone();
            for px in moved.rgba.as_chunks_mut::<4>().0 {
                *px = [0, px[1], px[2], px[0]];
            }
            Ok(blocks(&moved, CompressionVariant::BC3))
        }
        Decode::Masks { r, g, b, a, luminance, signed } => {
            let Layout::Linear { bits } = pf.layout else { return Err(unsupported()) };
            Ok(masks(image, bits, [r, g, b, a], luminance, signed))
        }
        Decode::Palette { .. } => {
            let Layout::Linear { bits } = pf.layout else { return Err(unsupported()) };
            palettized(image, bits, palette.ok_or_else(unsupported)?)
        }
        Decode::Uyvy | Decode::None => Err(unsupported()),
    }
}

fn encode_dxgi(image: &Image, id: u32) -> Option<Vec<u8>> {
    let opaque = image.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
    let bc7 = if opaque { BC7Settings::opaque_basic() } else { BC7Settings::alpha_basic() };
    Some(match id {
        70..=72 => blocks(image, CompressionVariant::BC1),
        73..=75 => blocks(image, CompressionVariant::BC2),
        76..=78 => blocks(image, CompressionVariant::BC3),
        79 | 80 => blocks(image, CompressionVariant::BC4),
        82 | 83 => blocks(image, CompressionVariant::BC5),
        94 | 95 => blocks(image, CompressionVariant::BC6H(BC6HSettings::basic())),
        97..=99 => blocks(image, CompressionVariant::BC7(bc7)),
        _ => by_channels(image, &parse_channels(crate::pixel::dxgi(id).ok()?.name)?),
    })
}

/// Block-compress, padding the image to whole blocks by repeating its edge pixels.
fn blocks(image: &Image, variant: CompressionVariant) -> Vec<u8> {
    let (w, h) = (image.width.div_ceil(4) * 4, image.height.div_ceil(4) * 4);
    let mut padded = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        let sy = y.min(image.height - 1);
        for x in 0..w {
            let (sx, d) = (x.min(image.width - 1), ((y * w + x) * 4) as usize);
            let s = ((sy * image.width + sx) * 4) as usize;
            padded[d..d + 4].copy_from_slice(&image.rgba[s..s + 4]);
        }
    }
    let row_bytes = variant.bytes_per_row(w) as usize;
    let mut out = vec![0u8; variant.blocks_byte_size(w, h)];
    let hdr = matches!(variant, CompressionVariant::BC6H(_));
    let halves: Vec<f16> = if hdr { padded.iter().map(|&v| f16::from_f32(f32::from(v) / 255.0)).collect() } else { Vec::new() };
    // One row of blocks at a time, in parallel.
    out.par_chunks_mut(row_bytes).enumerate().for_each(|(by, dst)| {
        let start = by * 4 * w as usize * 4;
        let end = start + 4 * w as usize * 4;
        if hdr {
            compress_rgba16(variant, &halves[start..end], dst, w, 4, w * 4);
        } else {
            compress_rgba8(variant, &padded[start..end], dst, w, 4, w * 4);
        }
    });
    out
}

fn from_u8(v: u8, c: Channel) -> u64 {
    let f = f32::from(v) / 255.0;
    match c.kind {
        Kind::Unorm => ((f64::from(v) * ((1u64 << c.bits) - 1) as f64 / 255.0).round()) as u64,
        Kind::Snorm => {
            let max = ((1i64 << (c.bits - 1)) - 1) as f32;
            let s = ((f * 2.0 - 1.0) * max).round() as i64;
            (s as u64) & ((1u64 << c.bits) - 1)
        }
        Kind::Uint | Kind::Sint => u64::from(v),
        Kind::Float if c.bits == 16 => u64::from(f16::from_f32(f).to_bits()),
        Kind::Float => u64::from(f.to_bits()),
    }
}

fn by_channels(image: &Image, channels: &[Channel]) -> Vec<u8> {
    let bytes = (channels.iter().map(|c| c.bits).sum::<u32>() / 8) as usize;
    let mut out = Vec::with_capacity(image.rgba.len() / 4 * bytes);
    for p in image.rgba.as_chunks::<4>().0 {
        let (mut px, mut shift) = (0u128, 0);
        for &c in channels {
            if let Some(slot) = c.slot {
                px |= u128::from(from_u8(p[slot], c)) << shift;
            }
            shift += c.bits;
        }
        out.extend_from_slice(&px.to_le_bytes()[..bytes]);
    }
    out
}

fn masks(image: &Image, bits: u32, masks: [u32; 4], luminance: bool, signed: u8) -> Vec<u8> {
    let bytes = (bits / 8) as usize;
    let mut out = Vec::with_capacity(image.rgba.len() / 4 * bytes);
    for p in image.rgba.as_chunks::<4>().0 {
        let mut px = 0u32;
        for (i, &mask) in masks.iter().enumerate() {
            if mask == 0 || (luminance && (i == 1 || i == 2)) {
                continue;
            }
            let shift = mask.trailing_zeros();
            let width = (mask >> shift).count_ones();
            let kind = if signed >> i & 1 != 0 { Kind::Snorm } else { Kind::Unorm };
            px |= (from_u8(p[i], Channel { slot: None, bits: width, kind }) as u32) << shift & mask;
        }
        out.extend_from_slice(&px.to_le_bytes()[..bytes]);
    }
    out
}

/// Look each colour up in the palette; 4-bit indices put the first pixel in the low nibble.
fn palettized(image: &Image, bits: u32, palette: &[u8]) -> Result<Vec<u8>, EncodeError> {
    let entries = palette.as_chunks::<4>().0;
    let index_of = |p: &[u8]| {
        entries.iter().position(|e| e[..] == *p).ok_or_else(|| {
            EncodeError::Palette(format!("colour {p:?} isn't in the texture's palette (a palette format can't take new colours)"))
        })
    };
    let (w, h) = (image.width as usize, image.height as usize);
    let row_bytes = (w * bits as usize).div_ceil(8);
    let mut out = vec![0u8; row_bytes * h];
    for y in 0..h {
        for x in 0..w {
            let index = index_of(&image.rgba[(y * w + x) * 4..][..4])? as u8;
            match bits {
                8 => out[y * row_bytes + x] = index,
                _ => out[y * row_bytes + x / 2] |= (index & 0xf) << (4 * (x % 2)),
            }
        }
    }
    Ok(out)
}

/// Resize by averaging the source pixels under each output pixel (a box filter).
pub fn resize_box(image: &Image, width: u32, height: u32) -> Image {
    let (w, h) = (image.width, image.height);
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let (y0, y1) = (y * h / height, ((y + 1) * h / height).max(y * h / height + 1));
        for x in 0..width {
            let (x0, x1) = (x * w / width, ((x + 1) * w / width).max(x * w / width + 1));
            let mut sum = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = ((sy * w + sx) * 4) as usize;
                    for (s, &v) in sum.iter_mut().zip(&image.rgba[p..p + 4]) {
                        *s += u32::from(v);
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            rgba.extend(sum.iter().map(|&s| ((s + n / 2) / n) as u8));
        }
    }
    Image { width, height, rgba }
}

/// Every mip of a 2D image: the image itself, then each level halved (at least 1×1),
/// `count` in all.
pub fn mip_chain(top: Image, count: u32) -> Vec<Image> {
    let mut mips = vec![top];
    while (mips.len() as u32) < count {
        let last = mips.last().unwrap();
        let next = resize_box(last, (last.width / 2).max(1), (last.height / 2).max(1));
        mips.push(next);
    }
    mips
}

/// Every mip of a volume given its top-level slices: each level halves width, height
/// and depth, averaging pairs of slices. Returns, per mip, its slices.
pub fn volume_mip_chain(top: Vec<Image>, count: u32) -> Vec<Vec<Image>> {
    let mut levels = vec![top];
    while (levels.len() as u32) < count {
        let last = levels.last().unwrap();
        let (w, h) = ((last[0].width / 2).max(1), (last[0].height / 2).max(1));
        let depth = (last.len() / 2).max(1);
        let next = (0..depth)
            .map(|z| {
                let a = resize_box(&last[2 * z], w, h);
                match last.get(2 * z + 1) {
                    Some(b) => {
                        let b = resize_box(b, w, h);
                        let rgba = a.rgba.iter().zip(&b.rgba).map(|(&x, &y)| ((u16::from(x) + u16::from(y)).div_ceil(2)) as u8).collect();
                        Image { width: w, height: h, rgba }
                    }
                    None => a,
                }
            })
            .collect();
        levels.push(next);
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{Subresource, decode};
    use crate::format::TextureInfo;
    use crate::pixel;

    fn info(width: u32, height: u32, pf: PixelFormat) -> TextureInfo {
        TextureInfo {
            size: pf.layout.image_size(width, height),
            header_size: 0,
            width,
            height,
            depth: 1,
            mips: 1,
            array_size: 1,
            faces: 1,
            pixel_format: pf,
        }
    }

    /// Encode then decode.
    fn round_trip(image: &Image, pf: PixelFormat) -> Image {
        let bytes = encode(image, &pf, None).unwrap();
        assert_eq!(bytes.len() as u64, pf.layout.image_size(image.width, image.height), "{}", pf.name);
        decode(&bytes, &info(image.width, image.height, pf), Subresource::default()).unwrap()
    }

    fn gradient(w: u32, h: u32) -> Image {
        let rgba = (0..w * h).flat_map(|i| {
            let (x, y) = (i % w, i / w);
            [(x * 255 / w.max(2).saturating_sub(1).max(1)) as u8, (y * 255 / h.max(2).saturating_sub(1).max(1)) as u8, 128, 255]
        });
        Image { width: w, height: h, rgba: rgba.collect() }
    }

    #[test]
    fn lossless_formats_round_trip_exactly() {
        let image = gradient(7, 5);
        for id in [28, 29, 87, 88, 91] {
            let pf = pixel::dxgi(id).unwrap();
            let back = round_trip(&image, pf);
            let expect: Vec<u8> = if id == 88 {
                image.rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
            } else {
                image.rgba.clone()
            };
            assert_eq!(back.rgba, expect, "{}", pf.name);
        }
        // Legacy A8R8G8B8 by masks.
        let argb = PixelFormat::new("A8R8G8B8", Some(87), Layout::Linear { bits: 32 })
            .with_decode(Decode::Masks { r: 0xff_0000, g: 0xff00, b: 0xff, a: 0xff00_0000, luminance: false, signed: 0 });
        assert_eq!(round_trip(&image, argb).rgba, image.rgba);
    }

    #[test]
    fn narrow_and_float_formats_round_trip_closely() {
        let image = gradient(16, 16);
        for id in [85, 86, 115, 24, 10, 2, 11, 49, 61, 31] {
            let pf = pixel::dxgi(id).unwrap();
            let back = round_trip(&image, pf);
            let decoded_again = round_trip(&back, pf);
            // A second trip changes nothing: the first already snapped to the format.
            assert_eq!(decoded_again.rgba, back.rgba, "{}", pf.name);
        }
    }

    #[test]
    fn block_formats_are_close() {
        // Smooth enough that one 4x4 block barely changes, as in real textures (BC1 can
        // only hold colours along one line per block). Odd sizes pad the edge blocks.
        let image = gradient(125, 93);
        for id in [71, 74, 77, 80, 83, 95, 98] {
            let pf = pixel::dxgi(id).unwrap();
            let back = round_trip(&image, pf);
            assert_eq!((back.width, back.height), (125, 93));
            let channels: &[usize] = match id {
                80 => &[0],
                83 => &[0, 1],
                _ => &[0, 1, 2],
            };
            let worst = back
                .rgba
                .chunks(4)
                .zip(image.rgba.chunks(4))
                .flat_map(|(a, b)| channels.iter().map(move |&c| (i32::from(a[c]) - i32::from(b[c])).abs()))
                .max()
                .unwrap();
            // BC6H stores floats, so 0-1 values are coarser than in 8-bit formats.
            let limit = if id == 95 { 20 } else { 12 };
            assert!(worst <= limit, "{}: off by {worst}", pf.name);
        }
    }

    #[test]
    fn unsupported_and_palette() {
        let image = gradient(4, 4);
        assert_eq!(encode(&image, &pixel::dxgi(81).unwrap(), None), Err(EncodeError::Unsupported("BC4_SNORM")));
        let p8 = PixelFormat::new("P8", Some(113), Layout::Linear { bits: 8 }).with_decode(Decode::Palette { offset: 0 });
        let palette = [[0, 0, 0, 255], [255, 255, 255, 255]].concat();
        let bw = Image { width: 2, height: 1, rgba: [[255, 255, 255, 255], [0, 0, 0, 255]].concat() };
        assert_eq!(encode(&bw, &p8, Some(&palette)).unwrap(), [1, 0]);
        assert!(matches!(encode(&image, &p8, Some(&palette)), Err(EncodeError::Palette(_))));
    }

    #[test]
    fn mips_halve_down_to_one_pixel() {
        let mips = mip_chain(gradient(8, 2), 4);
        let sizes: Vec<_> = mips.iter().map(|m| (m.width, m.height)).collect();
        assert_eq!(sizes, [(8, 2), (4, 1), (2, 1), (1, 1)]);
        let slices = (0..4).map(|_| gradient(4, 4)).collect();
        let levels = volume_mip_chain(slices, 3);
        assert_eq!(levels.iter().map(Vec::len).collect::<Vec<_>>(), [4, 2, 1]);
        assert_eq!((levels[2][0].width, levels[2][0].height), (1, 1));
    }
}
