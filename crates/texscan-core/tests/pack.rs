//! Edit fixture textures through an extract folder, pack, and check the output: edited
//! textures decode to the new images, and every other byte of the file is unchanged.

use std::fs;
use std::path::Path;

use texscan_core::encode::resize_box;
use texscan_core::{
    Container, Error, ExtractOptions, Image, Manifest, Outcome, PackOptions, ScanOptions, SourceInfo, Subresource,
    TextureEdit, decode, encode_png, extract_all, load_edits, pack, scan, texture_at,
};
use texscan_fixtures::{DdsSpec, Pf, Rng, Unit};

struct Setup {
    data: Vec<u8>,
    manifest: Manifest,
    dir: tempfile::TempDir,
}

/// The fixture archive, scanned and extracted with PNGs.
fn setup() -> Setup {
    let data = texscan_fixtures::dds_archive().data;
    let found = scan(&data, &ScanOptions::default()).textures;
    let manifest = Manifest::new(SourceInfo::describe(Path::new("archive.bin"), &data), ScanOptions::default(), &found);
    let dir = tempfile::tempdir().unwrap();
    extract_all(&data, &manifest, dir.path(), &ExtractOptions { png: true, ..Default::default() }).unwrap();
    Setup { data, manifest, dir }
}

fn smooth(w: u32, h: u32) -> Image {
    let rgba = (0..w * h)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            [(x * 200 / w) as u8 + 20, (y * 200 / h) as u8 + 30, 90, 255]
        })
        .collect();
    Image { width: w, height: h, rgba }
}

/// Pack the edits found in the folder, write the output and read it back.
fn pack_folder(s: &Setup) -> (texscan_core::PackResult, Vec<u8>) {
    let found = load_edits(&s.data, &s.manifest, s.dir.path()).unwrap();
    let result = pack(&s.data, &s.manifest, &found.edits, &PackOptions::default()).unwrap();
    let out = s.dir.path().join("packed.bin");
    result.write_file(&s.data, &out).unwrap();
    (result, fs::read(&out).unwrap())
}

/// Bytes outside `except` (offset, size) must be unchanged.
fn assert_only_changed(before: &[u8], after: &[u8], except: (u64, u64)) {
    assert_eq!(before.len(), after.len());
    let (start, end) = (except.0 as usize, (except.0 + except.1) as usize);
    assert!(before[..start] == after[..start], "bytes before the texture changed");
    assert!(before[end..] == after[end..], "bytes after the texture changed");
}

fn image_at(data: &[u8], offset: u64, sub: Subresource) -> Image {
    let t = texture_at(data, offset, Container::Dds).unwrap();
    decode(&data[offset as usize..t.end() as usize], &t.info, sub).unwrap()
}

#[test]
fn an_untouched_export_has_no_edits() {
    let s = setup();
    let found = load_edits(&s.data, &s.manifest, s.dir.path()).unwrap();
    assert!(found.edits.is_empty());
    // Every .dds, and a PNG for every image of every texture.
    let pngs: u32 = s.manifest.textures.iter().map(|t| t.array_size * t.faces * t.depth).sum();
    assert_eq!(found.unchanged, s.manifest.textures.len() + pngs as usize);
}

#[test]
fn png_edit_of_an_uncompressed_texture_is_exact_with_new_mips() {
    let s = setup();
    let t = &s.manifest.textures[2];
    assert_eq!((t.pixel_format.as_str(), t.width, t.height, t.mips), ("A8R8G8B8", 32, 16, 3));
    let new = smooth(32, 16);
    fs::write(s.dir.path().join(format!("{:08x}.png", t.offset)), encode_png(&new)).unwrap();

    let (result, packed) = pack_folder(&s);
    assert_eq!(result.textures.len(), 1);
    assert_eq!(result.textures[0].outcome, Outcome::Reencoded { images: 1, mips: 3 });
    assert_only_changed(&s.data, &packed, (t.offset, t.size));
    assert_eq!(packed[t.offset as usize..][..128], s.data[t.offset as usize..][..128], "header kept");
    assert_eq!(image_at(&packed, t.offset, Subresource::default()), new);
    let mip1 = image_at(&packed, t.offset, Subresource { mip: 1, ..Default::default() });
    assert_eq!(mip1, resize_box(&new, 16, 8));
    // The packed file scans the same as the original.
    let before: Vec<_> = scan(&s.data, &ScanOptions::default()).textures.iter().map(|t| (t.offset, t.info.clone())).collect();
    let after: Vec<_> = scan(&packed, &ScanOptions::default()).textures.iter().map(|t| (t.offset, t.info.clone())).collect();
    assert_eq!(before, after);
}

#[test]
fn png_edit_of_a_block_texture_is_close() {
    let s = setup();
    let t = &s.manifest.textures[0];
    assert_eq!((t.pixel_format.as_str(), t.width, t.height), ("DXT1", 256, 128));
    let new = smooth(256, 128);
    fs::write(s.dir.path().join(format!("{:08x}.png", t.offset)), encode_png(&new)).unwrap();
    let (_, packed) = pack_folder(&s);
    assert_only_changed(&s.data, &packed, (t.offset, t.size));
    let back = image_at(&packed, t.offset, Subresource::default());
    let worst = back.rgba.iter().zip(&new.rgba).map(|(&a, &b)| (i32::from(a) - i32::from(b)).abs()).max().unwrap();
    assert!(worst <= 12, "off by {worst}");
}

#[test]
fn a_cube_face_edit_changes_only_that_face() {
    let s = setup();
    let t = s.manifest.textures.iter().find(|t| t.faces == 6 && t.pixel_format == "BC1_UNORM").unwrap().clone();
    fs::write(s.dir.path().join(format!("{:08x}_nz.png", t.offset)), encode_png(&smooth(t.width, t.height))).unwrap();
    let (result, packed) = pack_folder(&s);
    assert_eq!(result.textures[0].outcome, Outcome::Reencoded { images: 1, mips: t.mips });
    let info = texture_at(&s.data, t.offset, Container::Dds).unwrap().info;
    for layer in 0..6 {
        for mip in 0..info.mips {
            let range = info.subresource_range(Subresource { layer, mip, slice: 0 }).unwrap();
            let (a, b) = (&s.data[t.offset as usize..][range.clone()], &packed[t.offset as usize..][range]);
            assert_eq!(a == b, layer != 5, "layer {layer} mip {mip}");
        }
    }
}

#[test]
fn a_volume_slice_edit_rebuilds_the_mips() {
    let s = setup();
    let t = s.manifest.textures.iter().find(|t| t.depth > 1).unwrap().clone();
    let new = smooth(t.width, t.height);
    fs::write(s.dir.path().join(format!("{:08x}_z3.png", t.offset)), encode_png(&new)).unwrap();
    let (_, packed) = pack_folder(&s);
    for z in 0..t.depth {
        let sub = Subresource { slice: z, ..Default::default() };
        let (before, after) = (image_at(&s.data, t.offset, sub), image_at(&packed, t.offset, sub));
        if z == 3 {
            // L8 keeps one channel.
            assert_eq!(after.rgba.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), new.rgba.chunks(4).map(|p| p[0]).collect::<Vec<_>>());
        } else {
            assert_eq!(before, after, "slice {z}");
        }
    }
    let last = Subresource { mip: t.mips - 1, ..Default::default() };
    assert_ne!(image_at(&s.data, t.offset, last), image_at(&packed, t.offset, last), "the smallest mip mixes in the new slice");
}

#[test]
fn a_replacement_dds_with_the_same_layout() {
    let s = setup();
    let t = &s.manifest.textures[1];
    assert_eq!((t.pixel_format.as_str(), t.width, t.mips), ("DXT5", 64, 1));
    // Written with a DX10 header this time: BC3_UNORM is the same format.
    let new = DdsSpec::new(64, 64, 1, Pf::Dx10(77), Unit::Block(16), "", None).build(&mut Rng::new(99));
    fs::write(s.dir.path().join(&t.file), &new).unwrap();
    let (result, packed) = pack_folder(&s);
    assert_eq!(result.textures[0].outcome, Outcome::Replaced);
    assert_only_changed(&s.data, &packed, (t.offset, t.size));
    let at = t.offset as usize;
    assert_eq!(packed[at..at + 128], s.data[at..at + 128], "the original header is kept");
    assert_eq!(packed[at + 128..at + t.size as usize], new[148..], "the new pixel data is used");
}

#[test]
fn a_replacement_that_does_not_fit_is_refused() {
    let s = setup();
    let t = &s.manifest.textures[1];
    let new = DdsSpec::new(32, 32, 1, Pf::Dx10(77), Unit::Block(16), "", None).build(&mut Rng::new(1));
    let edits = [(t.id, TextureEdit::Texture(new))].into();
    let err = pack(&s.data, &s.manifest, &edits, &PackOptions::default()).unwrap_err();
    assert!(err.to_string().contains("32x32x1, 1 mips, 1 layer(s), BC3_UNORM, but the texture is 64x64x1"), "{err}");
}

#[test]
fn a_png_of_the_wrong_size_is_refused() {
    let s = setup();
    let t = &s.manifest.textures[2];
    fs::write(s.dir.path().join(format!("{:08x}.png", t.offset)), encode_png(&smooth(8, 8))).unwrap();
    let err = load_edits(&s.data, &s.manifest, s.dir.path()).unwrap_err();
    assert!(matches!(err, Error::Edit { id: 2, .. }), "{err}");
    assert!(err.to_string().contains("is 8x8, but the texture is 32x16"), "{err}");
}

#[test]
fn pack_refuses_a_different_input() {
    let s = setup();
    let mut changed = s.data.clone();
    changed[10] ^= 1;
    let err = pack(&changed, &s.manifest, &Default::default(), &PackOptions::default()).unwrap_err();
    assert!(matches!(err, Error::SourceMismatch(_)), "{err}");
}
