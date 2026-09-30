//! The window itself, driven with egui_kittest (no GPU): clicks go through the real
//! widgets and are checked through the accessibility tree.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use texscan_gui::App;
use texscan_gui::app::{Action, View};

fn harness() -> Harness<'static, App> {
    Harness::builder().with_size([1400.0, 900.0]).build_ui_state(|ui, app: &mut App| app.show(ui), App::new())
}

/// Finish the running job (if any) and let the window catch up.
fn settle(h: &mut Harness<'static, App>) {
    let app = h.state_mut();
    app.jobs.wait(&mut app.session);
    h.run_steps(3);
}

/// Step until `done` holds (thumbnails and previews decode on other threads).
fn wait_until(h: &mut Harness<'static, App>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(h.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
        h.step();
    }
}

struct Input {
    _dir: tempfile::TempDir,
    path: PathBuf,
    fixture: texscan_fixtures::Fixture,
}

fn input() -> Input {
    let fixture = texscan_fixtures::dds_archive();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.bin");
    std::fs::write(&path, &fixture.data).unwrap();
    Input { _dir: dir, path, fixture }
}

fn open(h: &mut Harness<'static, App>, input: &Input) {
    h.state_mut().request(Action::OpenFile(input.path.clone()));
    settle(h);
}

#[test]
fn welcome_screen() {
    let mut h = harness();
    h.run_steps(2);
    h.get_by_label("Open a file…");
    h.get_by_label("or drop a file on this window");
}

#[test]
fn open_shows_every_texture_with_a_thumbnail() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let n = input.fixture.expected.len();
    assert_eq!(h.state().session.textures().len(), n);
    h.get_by_label(&format!("{n} textures"));
    h.get_by_label("2 rejected");
    // Every tile is on screen at this size, and every fixture texture decodes.
    wait_until(&mut h, "thumbnails", |app| !app.thumbnail_counts().2 && app.thumbnail_counts().0 > 0);
    assert_eq!(h.state().thumbnail_counts(), (n, 0, false));
}

#[test]
fn click_a_tile_to_preview_it_and_pick_a_mip() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let first = &input.fixture.expected[0];
    h.get_by_label(&format!("256×128 DXT1 at {:#x}", first.offset)).click();
    h.run_steps(2);
    assert_eq!(h.state().selected, Some(0));
    h.get_by_label("Texture 0");
    wait_until(&mut h, "the preview", |app| app.shown_image() == Some((256, 128)));

    // Mip 3 is 32x16.
    h.state_mut().sub.mip = 3;
    h.run_steps(1);
    wait_until(&mut h, "mip 3", |app| app.shown_image() == Some((32, 16)));
}

#[test]
fn cube_faces_and_array_elements() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let cube = input.fixture.expected.iter().position(|e| e.spec.cube && e.spec.array_size == 1).unwrap() as u32;
    h.state_mut().select(cube);
    h.run_steps(2);
    h.get_by_label("Face");
    h.state_mut().sub.layer = 5;
    h.run_steps(1);
    wait_until(&mut h, "face -Z", |app| app.shown_image() == Some((32, 32)));
    assert_eq!(h.state().sub.layer, 5);

    let array = input.fixture.expected.iter().position(|e| e.spec.array_size == 3).unwrap() as u32;
    h.state_mut().select(array);
    h.run_steps(2);
    h.get_by_label("Element");
    assert_eq!(h.state().sub.layer, 0, "a new selection starts at the first image");
}

#[test]
fn table_view_sorts_filters_and_selects() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    h.get_by_label("Table").click();
    h.run_steps(2);
    assert_eq!(h.state().view, View::Table);
    h.get_by_label("Pixel format");
    h.get_by_label("BC7_UNORM_SRGB").click();
    h.run_steps(2);
    assert_eq!(h.state().selected, Some(3));

    // Filtering by "cube" leaves the two cube maps.
    h.state_mut().filter = "cube".into();
    h.run_steps(2);
    assert!(h.query_by_label("16×16 cube").is_some());
    assert!(h.query_by_label("256×128").is_none());
}

#[test]
fn rejected_headers_are_listed() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    h.get_by_label("2 rejected").click();
    h.run_steps(2);
    assert!(h.state().show_rejected);
    h.get_by_label_contains("unknown FourCC");
    h.get_by_label_contains("truncated");
}

#[test]
fn a_file_without_textures() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.bin");
    std::fs::write(&path, b"nothing to see here, just some DDS text").unwrap();
    let mut h = harness();
    h.state_mut().request(Action::OpenFile(path));
    settle(&mut h);
    h.get_by_label("No textures found.");
}

/// A white PNG of `w`x`h` in `dir`.
fn white_png(dir: &std::path::Path, w: u32, h: u32) -> PathBuf {
    let path = dir.join(format!("white_{w}x{h}.png"));
    let image = texscan_core::Image { width: w, height: h, rgba: vec![255; (w * h * 4) as usize] };
    std::fs::write(&path, texscan_core::encode_png(&image)).unwrap();
    path
}

#[test]
fn replace_an_image_preview_the_edit_and_revert() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let e = &input.fixture.expected[2];
    assert_eq!((e.spec.name, e.spec.width, e.spec.height), ("A8R8G8B8", 32, 16));
    h.state_mut().select(2);
    h.run_steps(2);
    let png = white_png(input._dir.path(), 32, 16);
    h.state_mut().session.set_image_edit(2, 0, 0, png);
    h.run_steps(2);

    // The tile says it's edited, and the preview shows the edit as pack would write it.
    h.get_by_label(&format!("32×16 A8R8G8B8 at {:#x}, edited", e.offset));
    h.get_by_label("Pack (1 edited)…");
    wait_until(&mut h, "the edited preview", |app| app.shows_edit());
    assert!(h.state().shown().unwrap().rgba.iter().all(|&b| b == 255));

    // "Original" shows the texture as it is in the file.
    h.get_by_label("Original").click();
    h.run_steps(2);
    wait_until(&mut h, "the original preview", |app| app.shown().is_some() && !app.shows_edit());
    assert!(!h.state().shown().unwrap().rgba.iter().all(|&b| b == 255));

    h.get_by_label("Revert").click();
    h.run_steps(2);
    assert!(h.state().session.edits.is_empty());
    assert!(h.query_by_label(&format!("32×16 A8R8G8B8 at {:#x}, edited", e.offset)).is_none());
}

#[test]
fn pack_window_dry_run_then_write() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let e = input.fixture.expected[2].clone();
    let png = white_png(input._dir.path(), 32, 16);
    h.state_mut().session.set_image_edit(2, 0, 0, png);
    h.run_steps(2);
    h.get_by_label("Pack (1 edited)…").click();
    h.run_steps(2);
    assert!(h.state().show_pack);
    h.get_by_label("Dry run").click();
    h.run_steps(1);
    settle(&mut h);
    h.get_by_label("Dry run: 1 texture(s) would change.");
    h.get_by_label("1 image(s) encoded, 3 mip(s) each");

    let out = input._dir.path().join("archive.packed.bin");
    h.state_mut().start_pack(Some(out.clone()));
    settle(&mut h);
    h.get_by_label_contains("Written and verified:");
    let packed = std::fs::read(&out).unwrap();
    assert!(packed[e.offset + 128..e.offset + 128 + 32 * 16 * 4].iter().all(|&b| b == 255));
    assert_eq!(packed[..e.offset], input.fixture.data[..e.offset]);

    // Writing over the input is refused.
    h.state_mut().start_pack(Some(input.path.clone()));
    h.run_steps(1);
    assert!(h.state().session.log.last().unwrap().text.contains("overwrite the input"));
}

#[test]
fn closing_with_edits_asks_first() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let png = white_png(input._dir.path(), 32, 16);
    h.state_mut().session.set_image_edit(2, 0, 0, png);
    h.state_mut().request(Action::Close);
    h.run_steps(2);
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().session.file.is_some());
    h.state_mut().request(Action::Close);
    h.run_steps(2);
    h.get_by_label("Discard edits").click();
    h.run_steps(2);
    assert!(h.state().session.file.is_none());
    assert!(h.state().session.edits.is_empty());
}

#[test]
fn import_edits_from_an_extract_folder() {
    let input = input();
    let file = texscan_gui::session::open_file(&input.path).unwrap();
    let scanned = texscan_gui::session::run_scan(&file, &Default::default());
    let dir = input._dir.path().join("out");
    texscan_gui::session::extract(&file.data, &scanned.manifest, &dir, true).unwrap();
    let e = &input.fixture.expected[2];
    let png = dir.join(format!("{:08x}.png", e.offset));
    let image = texscan_core::Image { width: 32, height: 16, rgba: vec![255; 32 * 16 * 4] };
    std::fs::write(&png, texscan_core::encode_png(&image)).unwrap();
    let (edits, unchanged) = texscan_gui::session::import_edits(&file.data, &scanned.manifest, &dir).unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[&2], texscan_gui::session::EditSource::Images([((0, 0), png)].into()));
    assert!(unchanged > 10);
}
