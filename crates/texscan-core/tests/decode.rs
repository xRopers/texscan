//! Decoding fixtures: every image of a texture must come from the right bytes, and
//! every fixture texture must decode and export.

use texscan_core::{Container, DecodeError, ScanOptions, Subresource, decode, png_images, scan, texture_at, write_pngs};
use texscan_fixtures::{DdsSpec, Pf, Unit};

/// Texture bytes where every image is one solid colour, [id, mip, slice, 255], id being
/// the layer. Images are in DDS order: layer, then mip, then slice.
fn solid_images(spec: &DdsSpec) -> Vec<u8> {
    let mut out = spec.header();
    for layer in 0..spec.faces() * spec.array_size {
        for mip in 0..spec.mips.max(1) {
            let (w, h, d) = ((spec.width >> mip).max(1), (spec.height >> mip).max(1), (spec.depth >> mip).max(1));
            for slice in 0..d {
                for _ in 0..w * h {
                    out.extend_from_slice(&[layer as u8, mip as u8, slice as u8, 255]);
                }
            }
        }
    }
    assert_eq!(out.len(), spec.header_size() + spec.data_size());
    out
}

fn check_every_image(spec: DdsSpec) {
    let bytes = solid_images(&spec);
    let t = texture_at(&bytes, 0, Container::Dds).unwrap();
    for layer in 0..t.info.layers() {
        for mip in 0..t.info.mips {
            let (w, h, depth) = t.info.mip_size(mip);
            for slice in 0..depth {
                let image = decode(&bytes, &t.info, Subresource { layer, mip, slice }).unwrap();
                assert_eq!((image.width, image.height), (w, h));
                let want = [layer as u8, mip as u8, slice as u8, 255];
                assert!(image.rgba.chunks(4).all(|p| p == want), "layer {layer} mip {mip} slice {slice}");
            }
        }
    }
}

#[test]
fn cube_array_images_come_from_the_right_place() {
    let mut spec = DdsSpec::new(16, 8, 5, Pf::Dx10(28), Unit::Bits(32), "R8G8B8A8_UNORM", Some(28));
    spec.cube = true;
    spec.array_size = 2;
    check_every_image(spec);
}

#[test]
fn volume_slices_come_from_the_right_place() {
    let abgr = Pf::Masks { flags: 0x41, bits: 32, masks: [0xff, 0xff00, 0xff_0000, 0xff00_0000] };
    let mut spec = DdsSpec::new(8, 8, 4, abgr, Unit::Bits(32), "A8B8G8R8", Some(28));
    spec.depth = 4;
    check_every_image(spec);
}

#[test]
fn every_fixture_texture_exports_to_png() {
    let dir = tempfile::tempdir().unwrap();
    for f in texscan_fixtures::all() {
        for (t, e) in scan(&f.data, &ScanOptions::default()).textures.into_iter().zip(&f.expected) {
            let bytes = &f.data[t.offset as usize..t.end() as usize];
            let stem = format!("{}_{:x}", f.name, t.offset);
            let result = write_pngs(bytes, &t.info, dir.path(), &stem).unwrap();
            if !e.decodable {
                assert!(matches!(result, Err(DecodeError::Unsupported(_))), "{}: {result:?}", e.spec.name);
                continue;
            }
            let names = result.unwrap();
            assert_eq!(names.len(), png_images(&t.info).len());
            assert_eq!(names.len() as u32, t.info.layers() * t.info.depth);
            for name in names {
                let file = std::fs::File::open(dir.path().join(&name)).unwrap();
                let info = png::Decoder::new(std::io::BufReader::new(file)).read_info().unwrap().info().clone();
                assert_eq!((info.width, info.height), (t.info.width, t.info.height), "{name}");
            }
        }
    }
}

#[test]
fn png_names_say_which_image() {
    let mut spec = DdsSpec::new(4, 4, 1, Pf::Dx10(28), Unit::Bits(32), "", None);
    spec.cube = true;
    spec.array_size = 2;
    let bytes = solid_images(&spec);
    let t = texture_at(&bytes, 0, Container::Dds).unwrap();
    let names: Vec<_> = png_images(&t.info).into_iter().map(|(_, suffix)| suffix).collect();
    assert_eq!(names[..7], ["_a0_px", "_a0_nx", "_a0_py", "_a0_ny", "_a0_pz", "_a0_nz", "_a1_px"]);
}
