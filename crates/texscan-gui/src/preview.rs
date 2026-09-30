//! Decoding the selected texture's current image for the preview pane, off the UI
//! thread. A newer request replaces an older one; a stale result is dropped.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use egui::{ColorImage, TextureHandle, TextureOptions};
use texscan_core::input::Input;
use texscan_core::{Image, Subresource, TextureEntry, TextureInfo};

use crate::session::decode_image;

/// Largest image decoded for display, in pixels.
const MAX_PIXELS: u64 = 64 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub id: u32,
    pub sub: Subresource,
    /// Session generation, so a rescan reloads.
    pub generation: u64,
}

/// Which channels to show. One colour channel on its own is shown as grey, as is alpha
/// on its own; with alpha off, the image is opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channels {
    pub r: bool,
    pub g: bool,
    pub b: bool,
    pub a: bool,
}

impl Default for Channels {
    fn default() -> Self {
        Self { r: true, g: true, b: true, a: true }
    }
}

impl Channels {
    pub fn apply(self, image: &Image) -> ColorImage {
        let colours = [self.r, self.g, self.b];
        let single = (colours.iter().filter(|&&c| c).count() == 1).then(|| colours.iter().position(|&c| c).unwrap());
        let alpha_only = colours == [false; 3] && self.a;
        let rgba: Vec<u8> = image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let a = if self.a { p[3] } else { 255 };
                match single {
                    _ if alpha_only => [p[3], p[3], p[3], 255],
                    Some(c) => [p[c], p[c], p[c], a],
                    None => [if self.r { p[0] } else { 0 }, if self.g { p[1] } else { 0 }, if self.b { p[2] } else { 0 }, a],
                }
            })
            .collect();
        ColorImage::from_rgba_unmultiplied([image.width as usize, image.height as usize], &rgba)
    }
}

#[derive(Default)]
pub struct Previewer {
    pending: Option<(Key, Receiver<Result<Image, String>>)>,
    pub key: Option<Key>,
    pub current: Option<Result<Image, String>>,
    texture: Option<(Channels, TextureHandle)>,
}

impl Previewer {
    /// Decode `key`'s image unless it is already shown or being decoded.
    pub fn request(
        &mut self,
        key: Key,
        data: Arc<Input>,
        entry: TextureEntry,
        info: TextureInfo,
        wake: impl Fn() + Send + 'static,
    ) {
        if self.key == Some(key) || self.pending.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (w, h, _) = info.mip_size(key.sub.mip);
            let result = if u64::from(w) * u64::from(h) > MAX_PIXELS {
                Err(format!("{w}×{h} is too large to preview; pick a smaller mip"))
            } else {
                decode_image(&data, &entry, &info, key.sub).map_err(|e| format!("{e:#}"))
            };
            let _ = tx.send(result);
            wake();
        });
        self.pending = Some((key, rx));
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }

    /// Take a finished decode, if there is one.
    pub fn poll(&mut self) {
        let Some((_, rx)) = &self.pending else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("decoding failed unexpectedly".into()),
        };
        let (key, _) = self.pending.take().unwrap();
        self.key = Some(key);
        self.current = Some(result);
        self.texture = None;
    }

    /// The current image as a texture with `channels` applied, uploading it if needed.
    pub fn texture(&mut self, ctx: &egui::Context, channels: Channels) -> Option<&TextureHandle> {
        let Some(Ok(image)) = &self.current else { return None };
        if self.texture.as_ref().is_none_or(|(c, _)| *c != channels) {
            let handle = ctx.load_texture("preview", channels.apply(image), TextureOptions::NEAREST);
            self.texture = Some((channels, handle));
        }
        self.texture.as_ref().map(|(_, t)| t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_views() {
        let image = Image { width: 1, height: 1, rgba: vec![10, 20, 30, 40] };
        let show = |r, g, b, a| Channels { r, g, b, a }.apply(&image).pixels[0].to_srgba_unmultiplied();
        assert_eq!(show(true, true, true, false), [10, 20, 30, 255]);
        assert_eq!(show(false, true, false, false), [20, 20, 20, 255]);
        assert_eq!(show(false, false, false, true), [40, 40, 40, 255]);
        assert_eq!(show(true, false, true, false), [10, 0, 30, 255]);
    }
}
