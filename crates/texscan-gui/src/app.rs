//! The window: menus, the thumbnail grid or table, and the details pane with the image
//! preview. State lives in [`Session`]; slow work runs through [`Jobs`].

use std::path::PathBuf;

use egui::{Align, Color32, Key, Layout, Modifiers, RichText, Sense, Stroke, StrokeKind, TextureHandle, Ui, Vec2, ViewportCommand};
use egui_extras::{Column, TableBuilder};
use texscan_core::{Image, Outcome, Subresource, TextureEntry, TextureInfo};

use crate::jobs::{Jobs, finish};
use crate::preview::{self, Channels, Previewer};
use crate::session::{self, EditSource, Level, Session, human_size};
use crate::thumbs::Thumbnails;
use crate::widgets::{REJECTED_COLOR, checker_texture, file_strip, fit, paint_checker, texture_color};

const CUBE_FACES: [&str; 6] = ["+X", "−X", "+Y", "−Y", "+Z", "−Z"];
const EDITED_COLOR: Color32 = Color32::from_rgb(240, 160, 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    OpenFile(PathBuf),
    Rescan,
    Close,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Grid,
    Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Id,
    Offset,
    Size,
    Dimensions,
    Mips,
    Layers,
    PixelFormat,
    Edited,
}

/// How the preview is scaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zoom {
    /// Fit the pane (never enlarging past 8×).
    pub fit: bool,
    /// Scale when not fitting.
    pub scale: f32,
}

pub struct App {
    pub session: Session,
    pub jobs: Jobs,
    /// The context of the last frame drawn (for windows, repaints and viewport commands).
    ctx: egui::Context,
    preview: Previewer,
    thumbs: Thumbnails,
    checker: Option<TextureHandle>,
    pub selected: Option<u32>,
    scroll_to_selected: bool,
    pub view: View,
    sort: (SortKey, bool),
    pub filter: String,
    /// Thumbnail tile size, in points.
    tile: f32,
    /// Tiles per grid row in the last frame, for arrow keys.
    per_row: usize,
    /// Which image of the selected texture the preview shows.
    pub sub: Subresource,
    pub channels: Channels,
    pub zoom: Zoom,
    /// Preview the edit rather than the original, for an edited texture.
    pub show_edited: bool,
    show_log: bool,
    pub show_rejected: bool,
    pub show_pack: bool,
    /// An action waiting for "discard edits?" to be answered.
    pub confirm: Option<Action>,
    allow_close: bool,
    title: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            session: Session::default(),
            jobs: Jobs::default(),
            ctx: egui::Context::default(),
            preview: Previewer::default(),
            thumbs: Thumbnails::default(),
            checker: None,
            selected: None,
            scroll_to_selected: false,
            view: View::Grid,
            sort: (SortKey::Offset, true),
            filter: String::new(),
            tile: 128.0,
            per_row: 1,
            sub: Subresource::default(),
            channels: Channels::default(),
            zoom: Zoom { fit: true, scale: 1.0 },
            show_edited: true,
            show_log: false,
            show_rejected: false,
            show_pack: false,
            confirm: None,
            allow_close: false,
            title: String::new(),
        }
    }

    // ----- actions -------------------------------------------------------------------

    /// Run `action`, first asking if it would throw away edits.
    pub fn request(&mut self, action: Action) {
        if self.session.edits.is_empty() {
            self.perform(action);
        } else {
            self.confirm = Some(action);
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::OpenFile(path) => self.open_file(path),
            Action::Rescan => self.rescan(),
            Action::Close => {
                self.session.close();
                self.deselect();
            }
            Action::Exit => {
                self.allow_close = true;
                self.ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        self.deselect();
        let opts = self.session.scan_options.clone();
        self.jobs.start("Opening and scanning", move || {
            finish("Open", session::open_and_scan(&path, &opts), Session::set_opened)
        });
    }

    fn rescan(&mut self) {
        let Some(file) = &self.session.file else { return };
        let file = session::OpenFile { path: file.path.clone(), data: file.data.clone() };
        let opts = self.session.scan_options.clone();
        self.deselect();
        self.jobs.start("Scanning", move || {
            let scanned = session::run_scan(&file, &opts);
            Box::new(move |s: &mut Session| s.set_scanned(scanned))
        });
    }

    pub fn select(&mut self, id: u32) {
        if self.selected != Some(id) {
            self.sub = Subresource::default();
            self.zoom.fit = true;
        }
        self.selected = Some(id);
        self.scroll_to_selected = true;
    }

    /// Replace the image the preview shows (its layer and slice) with a PNG.
    fn replace_with_png(&mut self, id: u32) {
        let Some((_, info)) = self.session.texture(id) else { return };
        let what = image_name(info, self.sub);
        let Some(path) = rfd::FileDialog::new().add_filter("PNG", &["png"]).set_title(format!("New {what} for texture {id}")).pick_file()
        else {
            return;
        };
        self.session.set_image_edit(id, self.sub.layer, self.sub.slice, path);
        self.show_edited = true;
    }

    fn replace_with_file(&mut self, id: u32) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Texture", &["dds", "ktx2"])
            .set_title(format!("Replacement for texture {id}"))
            .pick_file()
        else {
            return;
        };
        self.session.set_file_edit(id, path);
        self.show_edited = true;
    }

    fn import_edits(&mut self) {
        let (Some(file), Some(scanned)) = (&self.session.file, &self.session.scanned) else { return };
        let Some(dir) = rfd::FileDialog::new().set_title("Folder with edited textures (from Extract)").pick_folder() else {
            return;
        };
        let (data, manifest) = (file.data.clone(), scanned.manifest.clone());
        self.jobs.start("Looking for edits", move || {
            finish("Import edits", session::import_edits(&data, &manifest, &dir), move |s, found| {
                s.set_imported_edits(&dir, found)
            })
        });
    }

    /// Pack the edits: a dry run, or written to `output`.
    pub fn start_pack(&mut self, output: Option<PathBuf>) {
        let (Some(file), Some(scanned)) = (&self.session.file, &self.session.scanned) else { return };
        if output.as_deref().is_some_and(|out| same_file(out, &file.path)) {
            self.session.error("Pack: the output would overwrite the input; choose another file");
            return;
        }
        let (data, manifest, edits) = (file.data.clone(), scanned.manifest.clone(), self.session.edits.clone());
        let label = if output.is_some() { "Packing" } else { "Dry run" };
        self.jobs.start(label, move || {
            let result = session::read_edits(&edits).and_then(|edits| session::run_pack(&data, &manifest, &edits, output.as_deref()));
            finish("Pack", result, Session::set_pack_report)
        });
    }

    fn pick_pack_output(&mut self) {
        let Some(file) = &self.session.file else { return };
        let mut dialog = rfd::FileDialog::new().set_title("Write the packed file to").set_file_name(session::default_output_name(&file.path));
        if let Some(dir) = file.path.parent() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(out) = dialog.save_file() {
            self.start_pack(Some(out));
        }
    }

    /// The image the preview shows, once decoded.
    pub fn shown(&self) -> Option<&Image> {
        match (&self.preview.key, &self.preview.current) {
            (Some(k), Some(Ok(image))) if Some(k.id) == self.selected && k.sub == self.sub => Some(image),
            _ => None,
        }
    }

    /// Whether the preview shows the edit (and has decoded it).
    pub fn shows_edit(&self) -> bool {
        self.preview.key.is_some_and(|k| k.edited.is_some()) && self.shown().is_some()
    }

    /// Size of the image the preview shows, once decoded.
    pub fn shown_image(&self) -> Option<(u32, u32)> {
        self.shown().map(|image| (image.width, image.height))
    }

    /// Thumbnails made so far: (decoded, failed), and whether more are coming.
    pub fn thumbnail_counts(&self) -> (usize, usize, bool) {
        let (ok, failed) = self.thumbs.counts();
        (ok, failed, self.thumbs.pending())
    }

    fn deselect(&mut self) {
        self.selected = None;
        self.preview.clear();
    }

    fn pick_and_open(&mut self) {
        if let Some(path) = rfd::FileDialog::new().set_title("Open a file to scan for textures").pick_file() {
            self.request(Action::OpenFile(path));
        }
    }

    fn save_file(&mut self, id: u32) {
        let (Some(file), Some((entry, _))) = (&self.session.file, self.session.texture(id)) else { return };
        let ext = entry.format.extension();
        let Some(path) = rfd::FileDialog::new().add_filter(ext.to_uppercase(), &[ext]).set_file_name(&entry.file).save_file() else {
            return;
        };
        let (data, entry) = (file.data.clone(), entry.clone());
        self.jobs.start("Saving", move || {
            finish("Save", session::save_texture(&data, &entry, &path), move |s, ()| {
                s.info(format!("texture {id} saved to {}", path.display()))
            })
        });
    }

    /// Save one image of a texture as PNG.
    fn save_png(&mut self, id: u32, sub: Subresource) {
        let (Some(file), Some((entry, info))) = (&self.session.file, self.session.texture(id)) else { return };
        let stem = entry.file.rsplit_once('.').map_or(entry.file.as_str(), |(s, _)| s);
        let name = format!("{stem}{}.png", image_suffix(info, sub));
        let Some(path) = rfd::FileDialog::new().add_filter("PNG", &["png"]).set_file_name(name).save_file() else {
            return;
        };
        let (data, entry, info) = (file.data.clone(), entry.clone(), info.clone());
        self.jobs.start("Saving", move || {
            let result = session::decode_image(&data, &entry, &info, sub).and_then(|image| session::save_png(&image, &path));
            finish("Save PNG", result, move |s, ()| s.info(format!("saved {}", path.display())))
        });
    }

    fn extract_all(&mut self, png: bool) {
        let (Some(file), Some(scanned)) = (&self.session.file, &self.session.scanned) else { return };
        let Some(dir) = rfd::FileDialog::new().set_title("Extract every texture to").pick_folder() else { return };
        let (data, manifest) = (file.data.clone(), scanned.manifest.clone());
        self.jobs.start("Extracting", move || {
            finish("Extract", session::extract(&data, &manifest, &dir, png), move |s, files| {
                let pngs: usize = files.iter().map(|f| f.pngs.len()).sum();
                let failed: Vec<_> = files.iter().filter(|f| f.png_error.is_some()).collect();
                let mut text = format!("{} texture(s) extracted to {}", files.len(), dir.display());
                if png {
                    text += &format!(", {pngs} PNG(s)");
                }
                s.info(text);
                for f in failed {
                    s.warn(format!("{}: no PNG: {}", f.path.display(), f.png_error.as_deref().unwrap_or("")));
                }
            })
        });
    }

    // ----- drawing -------------------------------------------------------------------

    /// Draw one frame.
    pub fn show(&mut self, ui: &mut Ui) {
        if self.ctx != *ui.ctx() {
            self.ctx = ui.ctx().clone();
            let waker = self.ctx.clone();
            self.jobs.set_waker(move || waker.request_repaint());
            self.checker = Some(checker_texture(&self.ctx));
        }
        self.preview.poll();
        self.thumbs.sync(self.session.generation);
        self.thumbs.poll(&self.ctx);
        if self.jobs.poll(&mut self.session) {
            self.thumbs.sync(self.session.generation);
        }
        if let Some(id) = self.session.select.take() {
            self.select(id);
        }
        if self.selected.is_some_and(|id| self.session.texture(id).is_none()) {
            self.deselect();
        }
        self.handle_input(ui);
        self.update_title();

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| self.menu_bar(ui));
            ui.add_space(2.0);
            self.toolbar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.session.scanned.is_some() {
            egui::Panel::top("strip").show(ui, |ui| {
                ui.add_space(4.0);
                self.strip(ui);
                ui.add_space(4.0);
            });
        }
        if self.selected.is_some() {
            egui::Panel::right("details").resizable(true).default_size(560.0).min_size(320.0).show(ui, |ui| self.details(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));

        self.log_window();
        self.rejected_window();
        self.pack_window();
        self.confirm_modal();
        if self.jobs.busy() || self.preview.loading() || self.thumbs.pending() {
            self.ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn handle_input(&mut self, ui: &Ui) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && !self.session.edits.is_empty() {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.confirm = Some(Action::Exit);
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()));
        if let Some(path) = dropped
            && !self.jobs.busy()
        {
            // A PNG dropped while a texture is selected replaces the image shown.
            let png = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
            match self.selected {
                Some(id) if png => {
                    self.session.set_image_edit(id, self.sub.layer, self.sub.slice, path);
                    self.show_edited = true;
                }
                _ => self.request(Action::OpenFile(path)),
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) && !self.jobs.busy() {
            self.pick_and_open();
        }
        // Arrow keys move the selection when nothing else has focus.
        if ctx.memory(|m| m.focused().is_none()) {
            let across = if self.view == View::Grid { self.per_row.max(1) as i64 } else { 0 };
            let step = ctx.input(|i| {
                let down = i.key_pressed(Key::ArrowDown) as i64 - i.key_pressed(Key::ArrowUp) as i64;
                let right = i.key_pressed(Key::ArrowRight) as i64 - i.key_pressed(Key::ArrowLeft) as i64;
                if across > 0 { down * across + right } else { down }
            });
            if step != 0 {
                let view = self.view_order();
                let textures = self.session.textures();
                let pos = self.selected.and_then(|id| view.iter().position(|&i| textures[i].id == id));
                let next = match pos {
                    Some(p) => (p as i64 + step).clamp(0, view.len() as i64 - 1) as usize,
                    None => 0,
                };
                if let Some(&i) = view.get(next) {
                    let id = textures[i].id;
                    self.select(id);
                }
            }
        }
    }

    fn update_title(&mut self) {
        let title = match &self.session.file {
            Some(f) => format!("{} - texscan", f.path.file_name().map_or_else(|| f.path.display().to_string(), |n| n.to_string_lossy().into_owned())),
            None => "texscan".to_string(),
        };
        if title != self.title {
            self.ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn menu_bar(&mut self, ui: &mut Ui) {
        let busy = self.jobs.busy();
        let has_file = self.session.file.is_some();
        let has_textures = !self.session.textures().is_empty();
        ui.menu_button("File", |ui| {
            if ui.add_enabled(!busy, egui::Button::new("Open…").shortcut_text("Ctrl+O")).clicked() {
                ui.close();
                self.pick_and_open();
            }
            if ui.add_enabled(has_file && !busy, egui::Button::new("Scan again")).clicked() {
                ui.close();
                self.request(Action::Rescan);
            }
            if ui.add_enabled(has_file && !busy, egui::Button::new("Close")).clicked() {
                ui.close();
                self.request(Action::Close);
            }
            ui.separator();
            if ui.button("Exit").clicked() {
                ui.close();
                self.request(Action::Exit);
            }
        });
        ui.menu_button("Textures", |ui| {
            if ui.add_enabled(has_textures && !busy, egui::Button::new("Extract all…")).clicked() {
                ui.close();
                self.extract_all(false);
            }
            if ui.add_enabled(has_textures && !busy, egui::Button::new("Extract all, with PNGs…")).clicked() {
                ui.close();
                self.extract_all(true);
            }
            if ui.add_enabled(has_textures && !busy, egui::Button::new("Import edits from folder…"))
                .on_hover_text("Find the textures you changed in a folder written by Extract")
                .clicked()
            {
                ui.close();
                self.import_edits();
            }
            if ui.add_enabled(has_textures, egui::Button::new("Pack…")).clicked() {
                ui.close();
                self.show_pack = true;
            }
            ui.separator();
            match self.selected {
                Some(id) => {
                    if ui.add_enabled(!busy, egui::Button::new(format!("Save texture {id}…"))).clicked() {
                        ui.close();
                        self.save_file(id);
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Save shown image as PNG…")).clicked() {
                        ui.close();
                        self.save_png(id, self.sub);
                    }
                    ui.separator();
                    if ui.button("Replace shown image with PNG…").clicked() {
                        ui.close();
                        self.replace_with_png(id);
                    }
                    if ui.button("Replace texture with DDS/KTX2…").clicked() {
                        ui.close();
                        self.replace_with_file(id);
                    }
                    if ui.add_enabled(self.session.edits.contains_key(&id), egui::Button::new("Revert")).clicked() {
                        ui.close();
                        self.session.revert(id);
                    }
                }
                None => {
                    ui.add_enabled(false, egui::Button::new("Select a texture for more"));
                }
            }
        });
        ui.menu_button("View", |ui| {
            if ui.radio(self.view == View::Grid, "Thumbnails").clicked() {
                self.view = View::Grid;
                ui.close();
            }
            if ui.radio(self.view == View::Table, "Table").clicked() {
                self.view = View::Table;
                ui.close();
            }
            ui.separator();
            if ui.button("Rejected headers").clicked() {
                ui.close();
                self.show_rejected = true;
            }
            if ui.button("Log").clicked() {
                ui.close();
                self.show_log = true;
            }
        });
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let busy = self.jobs.busy();
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("Open…")).clicked() {
                self.pick_and_open();
            }
            let n = self.session.edits.len();
            let label = if n > 0 { format!("Pack ({n} edited)…") } else { "Pack…".to_string() };
            if ui.add_enabled(self.session.scanned.is_some(), egui::Button::new(label)).clicked() {
                self.show_pack = true;
            }
            ui.separator();
            ui.selectable_value(&mut self.view, View::Grid, "Thumbnails");
            ui.selectable_value(&mut self.view, View::Table, "Table");
            if self.view == View::Grid {
                ui.add(egui::Slider::new(&mut self.tile, 64.0..=256.0).show_value(false)).on_hover_text("Thumbnail size");
            }
            ui.separator();
            ui.label("Filter:");
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("format, size, offset…").desired_width(180.0));
            if !self.filter.is_empty() && ui.small_button("×").on_hover_text("Clear the filter").clicked() {
                self.filter.clear();
            }
        });
    }

    fn strip(&mut self, ui: &mut Ui) {
        let Some(file) = &self.session.file else { return };
        let clicked = file_strip(ui, file.len(), self.session.textures(), self.session.rejected(), self.selected);
        if let Some(id) = clicked {
            self.select(id);
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(job) = self.jobs.current() {
                ui.spinner();
                ui.label(format!("{}… {:.1} s", job.label, job.started.elapsed().as_secs_f64()));
            } else if let Some(line) = self.session.log.last() {
                let text = RichText::new(&line.text).color(level_color(ui, line.level));
                if ui.add(egui::Label::new(text).sense(Sense::click()).truncate()).on_hover_text("Show the log").clicked() {
                    self.show_log = true;
                }
            } else {
                ui.label("Open a file, or drop one on the window.");
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(file) = &self.session.file {
                    ui.label(human_size(file.len()));
                    if self.session.scanned.is_some() {
                        ui.separator();
                        let rejected = self.session.rejected().len();
                        if rejected > 0
                            && ui.link(RichText::new(format!("{rejected} rejected")).color(REJECTED_COLOR)).clicked()
                        {
                            self.show_rejected = true;
                        }
                        if !self.session.edits.is_empty() {
                            ui.colored_label(EDITED_COLOR, format!("{} edited", self.session.edits.len()));
                        }
                        ui.label(format!("{} textures", self.session.textures().len()));
                    }
                }
            });
        });
    }

    fn central(&mut self, ui: &mut Ui) {
        if self.session.file.is_none() {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 3.0);
                ui.heading("texscan");
                ui.label("Find, preview and extract textures inside binary files.");
                ui.add_space(12.0);
                if ui.add_enabled(!self.jobs.busy(), egui::Button::new("Open a file…")).clicked() {
                    self.pick_and_open();
                }
                ui.add_space(8.0);
                ui.weak("or drop a file on this window");
            });
            return;
        }
        if self.session.textures().is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 3.0);
                if self.jobs.busy() {
                    ui.spinner();
                    return;
                }
                ui.label("No textures found.");
                let rejected = self.session.rejected().len();
                if rejected > 0 && ui.link(format!("{rejected} header(s) look like textures but can't be used")).clicked() {
                    self.show_rejected = true;
                }
            });
            return;
        }
        match self.view {
            View::Grid => self.grid(ui),
            View::Table => self.table(ui),
        }
    }

    /// Indices into the manifest's textures, filtered and sorted.
    fn view_order(&self) -> Vec<usize> {
        let textures = self.session.textures();
        let needle = self.filter.trim().to_lowercase();
        let mut view: Vec<usize> = (0..textures.len())
            .filter(|&i| {
                let t = &textures[i];
                needle.is_empty()
                    || t.pixel_format.to_lowercase().contains(&needle)
                    || format!("{}x{}", t.width, t.height).contains(&needle)
                    || format!("{:#x}", t.offset).contains(&needle)
                    || t.id.to_string() == needle
                    || (needle == "cube" && t.faces > 1)
                    || (needle == "edited" && self.session.edits.contains_key(&t.id))
            })
            .collect();
        let (key, ascending) = self.sort;
        view.sort_by(|&a, &b| {
            let (a, b) = (&textures[a], &textures[b]);
            let ord = match key {
                SortKey::Id => a.id.cmp(&b.id),
                SortKey::Offset => a.offset.cmp(&b.offset),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Dimensions => (u64::from(a.width) * u64::from(a.height)).cmp(&(u64::from(b.width) * u64::from(b.height))),
                SortKey::Mips => a.mips.cmp(&b.mips),
                SortKey::Layers => (a.array_size * a.faces * a.depth).cmp(&(b.array_size * b.faces * b.depth)),
                SortKey::PixelFormat => a.pixel_format.cmp(&b.pixel_format),
                SortKey::Edited => self.session.edits.contains_key(&a.id).cmp(&self.session.edits.contains_key(&b.id)),
            };
            let ord = ord.then(a.offset.cmp(&b.offset));
            if ascending { ord } else { ord.reverse() }
        });
        view
    }

    fn grid(&mut self, ui: &mut Ui) {
        ui.style_mut().interaction.selectable_labels = false;
        let view = self.view_order();
        let label_h = ui.text_style_height(&egui::TextStyle::Small) * 2.0 + 6.0;
        let cell = Vec2::new(self.tile + 10.0, self.tile + label_h + 10.0);
        let per_row = ((ui.available_width() + ui.spacing().item_spacing.x) / (cell.x + ui.spacing().item_spacing.x)).floor().max(1.0) as usize;
        self.per_row = per_row;
        let rows = view.len().div_ceil(per_row);
        let row_h = cell.y + ui.spacing().item_spacing.y;

        let mut area = egui::ScrollArea::vertical().auto_shrink(false);
        if self.scroll_to_selected {
            let textures = self.session.textures();
            if let Some(pos) = self.selected.and_then(|id| view.iter().position(|&i| textures[i].id == id)) {
                let row = (pos / per_row) as f32;
                area = area.vertical_scroll_offset((row * row_h - ui.available_height() / 2.0 + row_h / 2.0).max(0.0));
            }
            self.scroll_to_selected = false;
        }

        let (session, thumbs, checker) = (&self.session, &mut self.thumbs, self.checker.clone());
        let (Some(file), selected, tile) = (&session.file, self.selected, self.tile) else { return };
        let ctx = self.ctx.clone();
        let mut clicked = None;
        let mut action = None;
        area.show_rows(ui, cell.y, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    for &i in view.iter().skip(row * per_row).take(per_row) {
                        let entry = &session.textures()[i];
                        let (_, info) = session.texture(entry.id).expect("listed texture");
                        let waker = ctx.clone();
                        thumbs.request(&file.data, entry, info, move || waker.request_repaint());
                        let edited = session.edits.contains_key(&entry.id);
                        let marks = TileMarks { selected: selected == Some(entry.id), edited };
                        let response = thumb_tile(ui, cell, tile, entry, thumbs.get(entry.id), checker.as_ref(), marks);
                        if response.clicked() {
                            clicked = Some(entry.id);
                        }
                        response.context_menu(|ui| {
                            clicked = Some(entry.id);
                            texture_menu(ui, entry.id, edited, &mut action);
                        });
                    }
                });
            }
        });
        if let Some(id) = clicked {
            self.select(id);
            self.scroll_to_selected = false;
        }
        self.run_menu_action(action);
    }

    fn table(&mut self, ui: &mut Ui) {
        ui.style_mut().interaction.selectable_labels = false;
        let view = self.view_order();
        let textures = self.session.textures();
        let mut clicked = None;
        let mut action = None;
        let mut sort = self.sort;
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::auto().at_least(40.0))
            .column(Column::auto().at_least(90.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::auto().at_least(110.0))
            .column(Column::auto().at_least(40.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::auto().at_least(160.0))
            .column(Column::remainder().at_least(50.0));
        if self.scroll_to_selected {
            if let Some(row) = self.selected.and_then(|id| view.iter().position(|&i| textures[i].id == id)) {
                table = table.scroll_to_row(row, Some(Align::Center));
            }
            self.scroll_to_selected = false;
        }
        let headers = [
            ("#", SortKey::Id),
            ("Offset", SortKey::Offset),
            ("Size", SortKey::Size),
            ("Dimensions", SortKey::Dimensions),
            ("Mips", SortKey::Mips),
            ("Images", SortKey::Layers),
            ("Pixel format", SortKey::PixelFormat),
            ("Edited", SortKey::Edited),
        ];
        table
            .header(22.0, |mut header| {
                for (label, key) in headers {
                    header.col(|ui| {
                        let arrow = match sort {
                            (k, true) if k == key => " ⏶",
                            (k, false) if k == key => " ⏷",
                            _ => "",
                        };
                        if ui.add(egui::Button::new(RichText::new(format!("{label}{arrow}")).strong()).frame(false)).clicked() {
                            sort = if sort.0 == key { (key, !sort.1) } else { (key, true) };
                        }
                    });
                }
            })
            .body(|mut body| {
                let row_height = body.ui_mut().text_style_height(&egui::TextStyle::Body) + 4.0;
                body.rows(row_height, view.len(), |mut row| {
                    let t = &textures[view[row.index()]];
                    row.set_selected(self.selected == Some(t.id));
                    row.col(|ui| {
                        ui.label(t.id.to_string());
                    });
                    row.col(|ui| {
                        ui.monospace(format!("{:#010x}", t.offset));
                    });
                    row.col(|ui| {
                        ui.label(human_size(t.size));
                    });
                    row.col(|ui| {
                        ui.label(dimensions(t));
                    });
                    row.col(|ui| {
                        ui.label(t.mips.to_string());
                    });
                    row.col(|ui| {
                        ui.label((t.array_size * t.faces * t.depth).to_string());
                    });
                    row.col(|ui| {
                        ui.colored_label(texture_color(t), &t.pixel_format);
                    });
                    let edited = self.session.edits.contains_key(&t.id);
                    row.col(|ui| {
                        if edited {
                            ui.colored_label(EDITED_COLOR, "edited");
                        }
                    });
                    let response = row.response();
                    if response.clicked() {
                        clicked = Some(t.id);
                    }
                    response.context_menu(|ui| {
                        clicked = Some(t.id);
                        texture_menu(ui, t.id, edited, &mut action);
                    });
                });
            });
        self.sort = sort;
        if let Some(id) = clicked {
            self.select(id);
            self.scroll_to_selected = false;
        }
        self.run_menu_action(action);
    }

    fn run_menu_action(&mut self, action: Option<(u32, MenuAction)>) {
        match action {
            Some((id, MenuAction::SaveFile)) => self.save_file(id),
            Some((id, MenuAction::SavePng)) => self.save_png(id, Subresource::default()),
            Some((id, MenuAction::ReplacePng)) => {
                self.select(id);
                self.replace_with_png(id);
            }
            Some((id, MenuAction::ReplaceFile)) => self.replace_with_file(id),
            Some((id, MenuAction::Revert)) => self.session.revert(id),
            None => {}
        }
    }

    fn details(&mut self, ui: &mut Ui) {
        let Some(id) = self.selected else { return };
        let (Some((entry, info)), Some(data)) = (self.session.texture(id), self.session.file.as_ref().map(|f| f.data.clone()))
        else {
            return;
        };
        let (entry, info) = (entry.clone(), info.clone());

        ui.horizontal(|ui| {
            ui.heading(format!("Texture {id}"));
            ui.colored_label(texture_color(&entry), &entry.pixel_format);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("×").on_hover_text("Close").clicked() {
                    self.deselect();
                }
            });
        });
        egui::Grid::new("details").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            ui.label("Offset");
            ui.monospace(format!("{:#x} – {:#x}", entry.offset, entry.end()));
            ui.end_row();
            ui.label("Size");
            ui.label(format!("{} ({} bytes, header {})", human_size(entry.size), entry.size, entry.header_size));
            ui.end_row();
            ui.label("Dimensions");
            ui.label(format!("{}, {} mip{}", dimensions(&entry), entry.mips, if entry.mips == 1 { "" } else { "s" }));
            ui.end_row();
            ui.label("Container");
            ui.label(entry.format.name().to_uppercase());
            ui.end_row();
            ui.label("Pixel format");
            ui.label(match entry.dxgi_format {
                Some(n) => format!("{} (DXGI {n})", entry.pixel_format),
                None => entry.pixel_format.clone(),
            });
            ui.end_row();
            ui.label("CRC-32");
            ui.monospace(format!("{:08x}", entry.crc32));
            ui.end_row();
        });
        let edit = self.session.edits.get(&id).cloned();
        ui.horizontal(|ui| {
            let busy = self.jobs.busy();
            if ui.add_enabled(!busy, egui::Button::new(format!("Save as .{}…", entry.format.extension()))).clicked() {
                self.save_file(id);
            }
            if ui.add_enabled(!busy, egui::Button::new("Save image as PNG…")).clicked() {
                self.save_png(id, self.sub);
            }
        });
        ui.horizontal(|ui| {
            let what = image_name(&info, self.sub);
            if ui.button("Replace with PNG…").on_hover_text(format!("Replace the {what} shown below; its mips are rebuilt. Or drop a PNG on the window")).clicked() {
                self.replace_with_png(id);
            }
            if ui.button("Replace with DDS/KTX2…").on_hover_text("A .dds or .ktx2 with the same dimensions, mips and pixel format").clicked() {
                self.replace_with_file(id);
            }
            if ui.add_enabled(edit.is_some(), egui::Button::new("Revert")).clicked() {
                self.session.revert(id);
            }
        });
        if let Some(edit) = &edit {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(EDITED_COLOR, "Edited:");
                let names: Vec<_> = edit.files().iter().map(|p| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())).collect();
                ui.label(names.join(", ")).on_hover_text(edit.files().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n"));
            });
        }
        ui.separator();
        self.image_controls(ui, &info, edit.is_some());

        let edited = edit.is_some() && self.show_edited;
        let key = preview::Key { id, sub: self.sub, generation: self.session.generation, edited: edited.then_some(self.session.edits_generation) };
        let ctx = self.ctx.clone();
        self.preview.request(key, data, entry, info.clone(), edit.filter(|_| edited), move || ctx.request_repaint());
        if self.preview.key != Some(key) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Decoding…");
            });
            return;
        }
        if let Some(Err(e)) = &self.preview.current {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        self.image_view(ui);
    }

    /// Mip, face, element, slice, channel and zoom controls.
    fn image_controls(&mut self, ui: &mut Ui, info: &TextureInfo, edited: bool) {
        if edited {
            ui.horizontal(|ui| {
                ui.label("Show");
                ui.selectable_value(&mut self.show_edited, false, "Original");
                ui.selectable_value(&mut self.show_edited, true, RichText::new("Edited").color(EDITED_COLOR))
                    .on_hover_text("As pack will write it, encoded in the texture's pixel format");
            });
        }
        let sub = &mut self.sub;
        ui.horizontal_wrapped(|ui| {
            if info.mips > 1 {
                ui.label("Mip");
                let (w, h, _) = info.mip_size(sub.mip);
                egui::ComboBox::from_id_salt("mip").selected_text(format!("{}: {w}×{h}", sub.mip)).show_ui(ui, |ui| {
                    for m in 0..info.mips {
                        let (w, h, _) = info.mip_size(m);
                        ui.selectable_value(&mut sub.mip, m, format!("{m}: {w}×{h}"));
                    }
                });
            }
            if info.array_size > 1 {
                let mut element = sub.layer / info.faces;
                ui.label("Element");
                if ui.add(egui::DragValue::new(&mut element).range(0..=info.array_size - 1)).changed() {
                    sub.layer = element * info.faces + sub.layer % info.faces;
                }
            }
            if info.faces > 1 {
                let mut face = sub.layer % info.faces;
                ui.label("Face");
                egui::ComboBox::from_id_salt("face").selected_text(CUBE_FACES.get(face as usize).copied().unwrap_or("?")).show_ui(ui, |ui| {
                    for f in 0..info.faces {
                        ui.selectable_value(&mut face, f, CUBE_FACES.get(f as usize).copied().unwrap_or("?"));
                    }
                });
                sub.layer = sub.layer / info.faces * info.faces + face;
            }
            let depth = info.mip_size(sub.mip).2;
            sub.slice = sub.slice.min(depth - 1);
            if depth > 1 {
                ui.label("Slice");
                ui.add(egui::DragValue::new(&mut sub.slice).range(0..=depth - 1));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Channels");
            let c = &mut self.channels;
            ui.toggle_value(&mut c.r, RichText::new("R").color(Color32::from_rgb(230, 90, 90)));
            ui.toggle_value(&mut c.g, RichText::new("G").color(Color32::from_rgb(90, 200, 90)));
            ui.toggle_value(&mut c.b, RichText::new("B").color(Color32::from_rgb(100, 140, 255)));
            ui.toggle_value(&mut c.a, "A");
            ui.separator();
            ui.toggle_value(&mut self.zoom.fit, "Fit");
            if ui.button("1:1").clicked() {
                self.zoom = Zoom { fit: false, scale: 1.0 };
            }
            if !self.zoom.fit {
                ui.add(egui::Slider::new(&mut self.zoom.scale, 0.0625..=16.0).logarithmic(true).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
            }
        });
    }

    fn image_view(&mut self, ui: &mut Ui) {
        let Some(Ok(image)) = &self.preview.current else { return };
        let (w, h) = (image.width, image.height);
        let size = Vec2::new(w as f32, h as f32);
        let ctx = self.ctx.clone();
        let channels = self.channels;
        let Some(texture) = self.preview.texture(&ctx, channels).cloned() else { return };
        let Some(Ok(image)) = &self.preview.current else { return };
        let mut hover = None;
        let mut zoom = self.zoom;
        let bottom = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        let avail = ui.available_size() - Vec2::new(0.0, bottom);
        if zoom.fit {
            zoom.scale = (avail.x / size.x).min(avail.y / size.y).min(8.0);
        }
        egui::ScrollArea::both().max_height(avail.y).auto_shrink([false, false]).show(ui, |ui| {
            let shown = size * zoom.scale;
            let (rect, response) = ui.allocate_exact_size(shown.max(ui.available_size()), Sense::hover());
            let rect = fit(rect, size, zoom.scale);
            if let Some(checker) = &self.checker {
                paint_checker(ui, checker, rect, 8.0);
            }
            ui.painter().image(texture.id(), rect, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)), Color32::WHITE);
            ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, ui.visuals().weak_text_color()), StrokeKind::Outside);
            if let Some(pos) = response.hover_pos()
                && rect.contains(pos)
            {
                let x = (((pos.x - rect.left()) / rect.width()) * w as f32).clamp(0.0, w as f32 - 1.0) as u32;
                let y = (((pos.y - rect.top()) / rect.height()) * h as f32).clamp(0.0, h as f32 - 1.0) as u32;
                let p = ((y * w + x) * 4) as usize;
                hover = Some((x, y, [image.rgba[p], image.rgba[p + 1], image.rgba[p + 2], image.rgba[p + 3]]));
                let delta = ui.input(|i| i.zoom_delta());
                if delta != 1.0 {
                    zoom = Zoom { fit: false, scale: (zoom.scale * delta).clamp(0.0625, 16.0) };
                }
            }
        });
        self.zoom = zoom;
        ui.horizontal(|ui| {
            ui.label(format!("{w}×{h}"));
            ui.separator();
            ui.label(format!("{:.0}%", zoom.scale * 100.0));
            if let Some((x, y, [r, g, b, a])) = hover {
                ui.separator();
                ui.monospace(format!("({x}, {y})  R {r} G {g} B {b} A {a}"));
            }
        });
    }

    fn pack_window(&mut self) {
        let mut open = self.show_pack;
        let (mut dry_run, mut write, mut open_packed) = (false, false, None);
        egui::Window::new("Pack").open(&mut open).default_size([620.0, 360.0]).show(&self.ctx.clone(), |ui| {
            let busy = self.jobs.busy();
            if self.session.edits.is_empty() {
                ui.label("No edits yet. Replace a texture's image with a PNG, or the whole texture with a DDS or KTX2 file, from the details pane or the Textures menu; or import a folder of edits written by Extract.");
            } else {
                ui.label(format!("{} edited texture(s):", self.session.edits.len()));
                egui::Grid::new("pack-edits").striped(true).num_columns(2).spacing([12.0, 3.0]).show(ui, |ui| {
                    for (id, edit) in &self.session.edits {
                        ui.label(format!("texture {id}"));
                        ui.label(match edit {
                            EditSource::File(p) => format!("texture file {}", p.display()),
                            EditSource::Images(m) => format!("{} image(s): {}", m.len(), m.values().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")),
                        });
                        ui.end_row();
                    }
                });
            }
            ui.add_space(6.0);
            ui.weak("Each texture keeps its size and header, so nothing else in the file moves. The input is never changed: the output is a new file, read back and checked before it's kept.");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let can = !busy && !self.session.edits.is_empty();
                if ui.add_enabled(can, egui::Button::new("Dry run")).clicked() {
                    dry_run = true;
                }
                if ui.add_enabled(can, egui::Button::new("Write packed file…")).clicked() {
                    write = true;
                }
            });
            if let Some(report) = &self.session.last_pack {
                ui.separator();
                egui::Grid::new("pack-report").striped(true).num_columns(4).spacing([12.0, 3.0]).show(ui, |ui| {
                    for h in ["#", "Offset", "Result", "Note"] {
                        ui.strong(h);
                    }
                    ui.end_row();
                    for t in &report.textures {
                        ui.label(t.id.to_string());
                        ui.monospace(format!("{:#x}", t.offset));
                        ui.label(match &t.outcome {
                            Outcome::Replaced => "pixel data replaced from the texture file".to_string(),
                            Outcome::Reencoded { images, mips } => format!("{images} image(s) encoded, {mips} mip(s) each"),
                            Outcome::Unchanged => "unchanged (same bytes as the original)".to_string(),
                        });
                        match &t.note {
                            Some(note) => ui.colored_label(ui.visuals().warn_fg_color, note),
                            None => ui.label(""),
                        };
                        ui.end_row();
                    }
                });
                match &report.written {
                    Some(path) => {
                        ui.horizontal(|ui| {
                            ui.label(format!("Written and verified: {}", path.display()));
                            if ui.button("Open it").clicked() {
                                open_packed = Some(path.clone());
                            }
                        });
                    }
                    None => {
                        ui.label(format!("Dry run: {} texture(s) would change.", report.changed()));
                    }
                }
            }
        });
        self.show_pack = open;
        if dry_run {
            self.start_pack(None);
        }
        if write {
            self.pick_pack_output();
        }
        if let Some(path) = open_packed {
            self.request(Action::OpenFile(path));
        }
    }

    fn confirm_modal(&mut self) {
        let Some(action) = self.confirm.clone() else { return };
        let what = match &action {
            Action::OpenFile(_) => "Open another file",
            Action::Rescan => "Scan again",
            Action::Close => "Close the file",
            Action::Exit => "Exit",
        };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("confirm")).show(&self.ctx.clone(), |ui| {
            ui.set_max_width(380.0);
            ui.heading("Unpacked edits");
            ui.label(format!("{what}? The {} edit(s) haven't been packed and will be forgotten (the edited files stay where they are).", self.session.edits.len()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Discard edits").clicked() {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(false);
                }
            });
        });
        match choice {
            Some(true) => {
                self.confirm = None;
                self.perform(action);
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    fn log_window(&mut self) {
        let mut open = self.show_log;
        egui::Window::new("Log").open(&mut open).default_size([640.0, 320.0]).show(&self.ctx.clone(), |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
                for line in &self.session.log {
                    ui.colored_label(level_color(ui, line.level), &line.text);
                }
            });
        });
        self.show_log = open;
    }

    fn rejected_window(&mut self) {
        let mut open = self.show_rejected;
        egui::Window::new("Rejected headers").open(&mut open).default_size([560.0, 300.0]).show(&self.ctx.clone(), |ui| {
            ui.label("These look like textures but can't be used.");
            ui.add_space(4.0);
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                egui::Grid::new("rejected").striped(true).num_columns(3).spacing([12.0, 3.0]).show(ui, |ui| {
                    for r in self.session.rejected() {
                        ui.monospace(format!("{:#010x}", r.offset));
                        ui.label(r.container.name());
                        ui.label(&r.reason);
                        ui.end_row();
                    }
                });
            });
        });
        self.show_rejected = open;
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

#[derive(Debug, Clone, Copy)]
enum MenuAction {
    SaveFile,
    SavePng,
    ReplacePng,
    ReplaceFile,
    Revert,
}

fn texture_menu(ui: &mut Ui, id: u32, edited: bool, action: &mut Option<(u32, MenuAction)>) {
    let mut item = |ui: &mut Ui, enabled: bool, label: &str, what: MenuAction| {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            *action = Some((id, what));
            ui.close();
        }
    };
    item(ui, true, "Save texture file…", MenuAction::SaveFile);
    item(ui, true, "Save as PNG…", MenuAction::SavePng);
    ui.separator();
    item(ui, true, "Replace with PNG…", MenuAction::ReplacePng);
    item(ui, true, "Replace with DDS/KTX2…", MenuAction::ReplaceFile);
    item(ui, edited, "Revert", MenuAction::Revert);
}

/// How a grid tile is highlighted.
#[derive(Debug, Clone, Copy)]
struct TileMarks {
    selected: bool,
    edited: bool,
}

/// One tile of the grid: thumbnail on a checkerboard, then dimensions and format.
fn thumb_tile(
    ui: &mut Ui,
    cell: Vec2,
    tile: f32,
    entry: &TextureEntry,
    thumb: Option<&Result<TextureHandle, String>>,
    checker: Option<&TextureHandle>,
    TileMarks { selected, edited }: TileMarks,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(cell, Sense::click());
    let mut label = format!("{} {} at {:#x}", dimensions(entry), entry.pixel_format, entry.offset);
    if edited {
        label += ", edited";
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    let visuals = ui.visuals();
    if selected {
        ui.painter().rect_filled(rect, 4.0, visuals.selection.bg_fill);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let image_box = egui::Rect::from_min_size(rect.min + Vec2::new(5.0, 5.0), Vec2::splat(tile));
    match thumb {
        Some(Ok(texture)) => {
            let r = fit(image_box, texture.size_vec2(), f32::INFINITY);
            if let Some(checker) = checker {
                paint_checker(ui, checker, r, 6.0);
            }
            ui.painter().image(texture.id(), r, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        Some(Err(_)) => {
            ui.painter().rect_stroke(image_box, 2.0, Stroke::new(1.0, visuals.weak_text_color()), StrokeKind::Inside);
            ui.painter().text(image_box.center(), egui::Align2::CENTER_CENTER, "no preview", egui::FontId::proportional(12.0), visuals.weak_text_color());
        }
        None => {
            ui.painter().rect_filled(image_box, 2.0, visuals.extreme_bg_color);
        }
    }
    if edited {
        let badge = egui::Rect::from_min_size(egui::Pos2::new(image_box.right() - 46.0, image_box.top() + 3.0), Vec2::new(43.0, 15.0));
        ui.painter().rect_filled(badge, 3.0, EDITED_COLOR);
        ui.painter().text(badge.center(), egui::Align2::CENTER_CENTER, "edited", egui::FontId::proportional(11.0), Color32::BLACK);
    }
    let small = egui::FontId::proportional(ui.text_style_height(&egui::TextStyle::Small));
    let text_top = image_box.bottom() + 3.0;
    let clip = |s: String| {
        let max = (tile / (small.size * 0.55)) as usize;
        if s.chars().count() > max { s.chars().take(max.saturating_sub(1)).collect::<String>() + "…" } else { s }
    };
    ui.painter().text(egui::Pos2::new(rect.center().x, text_top), egui::Align2::CENTER_TOP, clip(dimensions(entry)), small.clone(), visuals.text_color());
    ui.painter().text(
        egui::Pos2::new(rect.center().x, text_top + small.size + 2.0),
        egui::Align2::CENTER_TOP,
        clip(entry.pixel_format.clone()),
        small,
        texture_color(entry),
    );
    match thumb {
        Some(Err(e)) => response.on_hover_text(format!("{label}\n{e}")),
        _ => response.on_hover_text(&label),
    }
}

/// Like `1024×512`, `64×64×16`, `256×256 cube` or `128×128 ×4`.
pub fn dimensions(t: &TextureEntry) -> String {
    let mut s = format!("{}×{}", t.width, t.height);
    if t.depth > 1 {
        s += &format!("×{}", t.depth);
    }
    if t.faces > 1 {
        s += " cube";
    }
    if t.array_size > 1 {
        s += &format!(" [{}]", t.array_size);
    }
    s
}

/// File name suffix for one image, as the CLI's PNG export names them.
fn image_suffix(info: &TextureInfo, sub: Subresource) -> String {
    let mut s = String::new();
    if info.array_size > 1 {
        s += &format!("_a{}", sub.layer / info.faces);
    }
    if info.faces > 1 {
        s += ["_px", "_nx", "_py", "_ny", "_pz", "_nz"].get((sub.layer % info.faces) as usize).copied().unwrap_or("_f");
    }
    if info.depth > 1 {
        s += &format!("_z{}", sub.slice);
    }
    if sub.mip > 0 {
        s += &format!("_mip{}", sub.mip);
    }
    s
}

/// "image", "+Z face", "slice 3", "element 2, −X face" and so on: which image `sub` is.
fn image_name(info: &TextureInfo, sub: Subresource) -> String {
    let mut parts = Vec::new();
    if info.array_size > 1 {
        parts.push(format!("element {}", sub.layer / info.faces));
    }
    if info.faces > 1 {
        parts.push(format!("{} face", CUBE_FACES.get((sub.layer % info.faces) as usize).copied().unwrap_or("?")));
    }
    if info.depth > 1 {
        parts.push(format!("slice {}", sub.slice));
    }
    if parts.is_empty() { "image".to_string() } else { parts.join(", ") }
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn level_color(ui: &Ui, level: Level) -> Color32 {
    match level {
        Level::Info => ui.visuals().text_color(),
        Level::Warn => ui.visuals().warn_fg_color,
        Level::Error => ui.visuals().error_fg_color,
    }
}
