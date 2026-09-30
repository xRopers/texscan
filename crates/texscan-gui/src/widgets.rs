//! Custom widgets: the file strip showing where textures are, thumbnail tiles, and the
//! checkerboard drawn behind transparent images.

use egui::{Color32, ColorImage, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2};
use texscan_core::{Rejected, TextureEntry};

/// Colour for a texture by its kind of pixel format.
pub fn texture_color(entry: &TextureEntry) -> Color32 {
    let f = entry.pixel_format.as_str();
    let block = ["BC", "DXT", "ATI", "ETC", "EAC", "ASTC"].iter().any(|p| f.starts_with(p)) || f == "RXGB";
    if block {
        Color32::from_rgb(90, 160, 255)
    } else if f.contains("FLOAT") || f.ends_with('F') || f.contains("16") || f.contains("32") {
        Color32::from_rgb(230, 150, 70)
    } else {
        Color32::from_rgb(110, 200, 120)
    }
}

pub const REJECTED_COLOR: Color32 = Color32::from_rgb(220, 70, 60);

/// The file at a glance: one lane with every texture (and rejected headers in red).
/// Returns the texture clicked, if any.
pub fn file_strip(
    ui: &mut Ui,
    file_len: u64,
    textures: &[TextureEntry],
    rejected: &[Rejected],
    selected: Option<u32>,
) -> Option<u32> {
    const H: f32 = 18.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), H), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    if file_len == 0 {
        return None;
    }
    let width = rect.width();
    let x_of = |offset: u64| rect.left() + (offset as f64 / file_len as f64 * f64::from(width)) as f32;
    for t in textures {
        let (x0, x1) = (x_of(t.offset), x_of(t.end()));
        let r = Rect::from_min_max(Pos2::new(x0, rect.top() + 2.0), Pos2::new(x1.max(x0 + 1.0), rect.bottom() - 2.0));
        painter.rect_filled(r, 0.0, texture_color(t));
        if selected == Some(t.id) {
            painter.rect_stroke(r.expand(1.0), 0.0, Stroke::new(2.0, ui.visuals().strong_text_color()), StrokeKind::Outside);
        }
    }
    for r in rejected {
        let x = x_of(r.offset);
        painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.5, REJECTED_COLOR));
    }
    // The texture under the pointer, or the nearest one within a few pixels.
    let hit = |pos: Pos2| {
        textures
            .iter()
            .map(|t| {
                let (x0, x1) = (x_of(t.offset), x_of(t.end()).max(x_of(t.offset) + 1.0));
                (t, if pos.x < x0 { x0 - pos.x } else if pos.x > x1 { pos.x - x1 } else { 0.0 })
            })
            .filter(|(_, d)| *d <= 4.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(t, _)| t)
    };
    let hovered = response.hover_pos().and_then(hit);
    let response = match hovered {
        Some(t) => response.on_hover_text_at_pointer(format!(
            "{:#x}: {}x{} {}",
            t.offset, t.width, t.height, t.pixel_format
        )),
        None => response,
    };
    if response.clicked() { response.interact_pointer_pos().and_then(hit).map(|t| t.id) } else { None }
}

/// A small checkerboard texture, drawn repeated behind images so transparency shows.
pub fn checker_texture(ctx: &egui::Context) -> TextureHandle {
    let (light, dark) = ([150, 150, 150, 255], [105, 105, 105, 255]);
    let rgba = [light, dark, dark, light].concat();
    let image = ColorImage::from_rgba_unmultiplied([2, 2], &rgba);
    let options = TextureOptions { wrap_mode: egui::TextureWrapMode::Repeat, ..TextureOptions::NEAREST };
    ctx.load_texture("checker", image, options)
}

/// Fill `rect` with the checkerboard, in squares of `square` points.
pub fn paint_checker(ui: &Ui, checker: &TextureHandle, rect: Rect, square: f32) {
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(rect.width() / square / 2.0, rect.height() / square / 2.0));
    ui.painter().image(checker.id(), rect, uv, Color32::WHITE);
}

/// `rect` shrunk to the largest centred rectangle with `size`'s aspect ratio, never
/// scaled up past `max_scale`.
pub fn fit(rect: Rect, size: Vec2, max_scale: f32) -> Rect {
    let scale = (rect.width() / size.x).min(rect.height() / size.y).min(max_scale);
    Rect::from_center_size(rect.center(), size * scale)
}
