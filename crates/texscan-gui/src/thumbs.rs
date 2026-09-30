//! Thumbnails for the grid, decoded on rayon's pool and uploaded on the UI thread. Only
//! the tiles on screen ask for one, and each is made from the smallest mip that is still
//! at least [`THUMB`] pixels across, so even huge textures are quick.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use egui::{ColorImage, TextureHandle, TextureOptions};
use texscan_core::input::Input;
use texscan_core::{Image, Subresource, TextureEntry, TextureInfo};

use crate::session::decode_image;

/// Largest thumbnail side, in pixels.
pub const THUMB: u32 = 160;

type Done = (u64, u32, Result<ColorImage, String>);

pub struct Thumbnails {
    generation: u64,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    requested: HashSet<u32>,
    done: HashMap<u32, Result<TextureHandle, String>>,
}

impl Default for Thumbnails {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { generation: 0, tx, rx, requested: HashSet::new(), done: HashMap::new() }
    }
}

impl Thumbnails {
    /// Forget everything if the session changed since the thumbnails were made.
    pub fn sync(&mut self, generation: u64) {
        if generation != self.generation {
            self.generation = generation;
            self.requested.clear();
            self.done.clear();
        }
    }

    pub fn get(&self, id: u32) -> Option<&Result<TextureHandle, String>> {
        self.done.get(&id)
    }

    /// Thumbnails made: (decoded, failed).
    pub fn counts(&self) -> (usize, usize) {
        let failed = self.done.values().filter(|r| r.is_err()).count();
        (self.done.len() - failed, failed)
    }

    /// Whether some thumbnails are still being made.
    pub fn pending(&self) -> bool {
        self.requested.len() > self.done.len()
    }

    pub fn request(
        &mut self,
        data: &Arc<Input>,
        entry: &TextureEntry,
        info: &TextureInfo,
        wake: impl Fn() + Send + 'static,
    ) {
        if !self.requested.insert(entry.id) {
            return;
        }
        let (data, entry, info, tx, generation) = (data.clone(), entry.clone(), info.clone(), self.tx.clone(), self.generation);
        rayon::spawn(move || {
            let _ = tx.send((generation, entry.id, thumbnail(&data, &entry, &info)));
            wake();
        });
    }

    /// Upload the thumbnails that have finished.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((generation, id, result)) = self.rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            let handle = result.map(|image| ctx.load_texture(format!("thumb-{id}"), image, TextureOptions::LINEAR));
            self.done.insert(id, handle);
        }
    }
}

/// The mip to make a thumbnail from: the smallest one at least `THUMB` across.
pub fn thumbnail_mip(info: &TextureInfo) -> u32 {
    (0..info.mips).rev().find(|&m| info.mip_size(m).0.max(info.mip_size(m).1) >= THUMB).unwrap_or(0)
}

fn thumbnail(data: &[u8], entry: &TextureEntry, info: &TextureInfo) -> Result<ColorImage, String> {
    let sub = Subresource { layer: 0, mip: thumbnail_mip(info), slice: 0 };
    let image = decode_image(data, entry, info, sub).map_err(|e| format!("{e:#}"))?;
    let small = shrink(&image, THUMB);
    Ok(ColorImage::from_rgba_unmultiplied([small.width as usize, small.height as usize], &small.rgba))
}

/// Scale down to fit in `max` × `max` by averaging the source pixels under each output
/// pixel; smaller images are returned as they are.
pub fn shrink(image: &Image, max: u32) -> Image {
    let (w, h) = (image.width, image.height);
    if w <= max && h <= max {
        return image.clone();
    }
    let scale = f64::from(max) / f64::from(w.max(h));
    let (nw, nh) = (((f64::from(w) * scale).round() as u32).max(1), ((f64::from(h) * scale).round() as u32).max(1));
    let mut rgba = Vec::with_capacity((nw * nh * 4) as usize);
    for y in 0..nh {
        let (y0, y1) = (y * h / nh, ((y + 1) * h / nh).max(y * h / nh + 1));
        for x in 0..nw {
            let (x0, x1) = (x * w / nw, ((x + 1) * w / nw).max(x * w / nw + 1));
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
    Image { width: nw, height: nh, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrink_averages_and_keeps_aspect() {
        let image = Image { width: 4, height: 2, rgba: [[0u8, 0, 0, 255], [255, 255, 255, 255]].repeat(4).concat() };
        let small = shrink(&image, 2);
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(&small.rgba[..4], [128, 128, 128, 255]);
        assert_eq!(shrink(&image, 8), image);
    }
}
