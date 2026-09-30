//! Saving decoded images as PNG.

use std::fs;
use std::path::Path;

use crate::decode::{DecodeError, Image, Subresource, decode};
use crate::error::{Result, io_err};
use crate::format::TextureInfo;

const CUBE_FACES: [&str; 6] = ["px", "nx", "py", "ny", "pz", "nz"];

/// An 8-bit RGBA PNG of `image`.
pub fn encode_png(image: &Image) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("writing to a Vec cannot fail");
    writer.write_image_data(&image.rgba).expect("image size matches its dimensions");
    writer.finish().expect("writing to a Vec cannot fail");
    out
}

/// Read a PNG of any colour type and bit depth as 8-bit RGBA.
pub fn decode_png(bytes: &[u8]) -> std::result::Result<Image, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let frame = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(frame.buffer_size());
    let rgba: Vec<u8> = match frame.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("indexed PNG wasn't expanded".into()),
    };
    Ok(Image { width: frame.width, height: frame.height, rgba })
}

/// The images a PNG export writes: the top mip of every layer and slice, each with a
/// file name suffix. A plain 2D texture gives one image with no suffix; otherwise
/// `_a{n}` for the array element, `_px`/`_nx`/... for the cube face and `_z{n}` for the
/// depth slice.
pub fn png_images(info: &TextureInfo) -> Vec<(Subresource, String)> {
    let mut out = Vec::new();
    for layer in 0..info.layers() {
        for slice in 0..info.depth {
            let mut suffix = String::new();
            if info.array_size > 1 {
                suffix += &format!("_a{}", layer / info.faces);
            }
            if info.is_cube() {
                suffix += "_";
                suffix += CUBE_FACES.get((layer % info.faces) as usize).unwrap_or(&"f");
            }
            if info.depth > 1 {
                suffix += &format!("_z{slice}");
            }
            out.push((Subresource { layer, mip: 0, slice }, suffix));
        }
    }
    out
}

/// Write `{stem}{suffix}.png` for each of [`png_images`] into `dir`. Returns the file
/// names written, or why the texture can't be decoded (nothing is written then).
pub fn write_pngs(
    texture: &[u8],
    info: &TextureInfo,
    dir: &Path,
    stem: &str,
) -> Result<std::result::Result<Vec<String>, DecodeError>> {
    let images = png_images(info);
    let mut decoded = Vec::with_capacity(images.len());
    for (sub, suffix) in images {
        match decode(texture, info, sub) {
            Ok(image) => decoded.push((image, suffix)),
            Err(e) => return Ok(Err(e)),
        }
    }
    let mut names = Vec::with_capacity(decoded.len());
    for (image, suffix) in decoded {
        let name = format!("{stem}{suffix}.png");
        let path = dir.join(&name);
        fs::write(&path, encode_png(&image)).map_err(io_err(&path))?;
        names.push(name);
    }
    Ok(Ok(names))
}
