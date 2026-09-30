//! KTX2 specifics: images come from the level index in KTX2 order, edits pack in place,
//! a DDS can replace a KTX2 texture, and what can't be read says so.

use std::path::Path;

use texscan_core::{
    Container, DecodeError, Image, Manifest, Outcome, PackOptions, ScanOptions, SourceInfo, Subresource, TextureEdit,
    decode, pack, scan, texture_at,
};
use texscan_fixtures::{DdsSpec, Pf, Rng, Unit, ktx2};

/// Level `m`'s (offset, length), read straight from the level index.
fn level(bytes: &[u8], m: usize) -> (usize, usize) {
    let at = 80 + m * 24;
    let read = |p: usize| u64::from_le_bytes(bytes[p..p + 8].try_into().unwrap()) as usize;
    (read(at), read(at + 8))
}

/// An RGBA8 KTX2 texture where every image is solid [layer, mip, slice, 255], written in
/// KTX2 order: per level, layer (element × faces + face), then slice.
fn solid_ktx2(spec: &DdsSpec) -> Vec<u8> {
    let mut bytes = ktx2(spec, 37, 0, &mut Rng::new(5));
    for m in 0..spec.mips.max(1) {
        let (offset, length) = level(&bytes, m as usize);
        let (w, h, d) = ((spec.width >> m).max(1), (spec.height >> m).max(1), (spec.depth >> m).max(1));
        let mut data = Vec::new();
        for layer in 0..spec.faces() * spec.array_size {
            for slice in 0..d {
                for _ in 0..w * h {
                    data.extend_from_slice(&[layer as u8, m as u8, slice as u8, 255]);
                }
            }
        }
        assert_eq!(data.len(), length);
        bytes[offset..offset + length].copy_from_slice(&data);
    }
    bytes
}

fn check_every_image(spec: DdsSpec) {
    let bytes = solid_ktx2(&spec);
    let t = texture_at(&bytes, 0, Container::Ktx2).unwrap();
    assert_eq!(t.info.size, bytes.len() as u64);
    for layer in 0..t.info.layers() {
        for mip in 0..t.info.mips {
            for slice in 0..t.info.mip_size(mip).2 {
                let image = decode(&bytes, &t.info, Subresource { layer, mip, slice }).unwrap();
                let want = [layer as u8, mip as u8, slice as u8, 255];
                assert!(image.rgba.chunks(4).all(|p| p == want), "layer {layer} mip {mip} slice {slice}");
            }
        }
    }
}

#[test]
fn cube_array_images_come_from_the_right_place() {
    let mut spec = DdsSpec::new(16, 16, 5, Pf::Dx10(0), Unit::Bits(32), "", None);
    spec.cube = true;
    spec.array_size = 2;
    check_every_image(spec);
}

#[test]
fn volume_slices_come_from_the_right_place() {
    let mut spec = DdsSpec::new(8, 8, 4, Pf::Dx10(0), Unit::Bits(32), "", None);
    spec.depth = 4;
    check_every_image(spec);
}

fn archive() -> (Vec<u8>, Manifest) {
    let data = texscan_fixtures::ktx2_archive().data;
    let found = scan(&data, &ScanOptions::default()).textures;
    let manifest = Manifest::new(SourceInfo::describe(Path::new("k.bin"), &data), ScanOptions::default(), &found);
    (data, manifest)
}

#[test]
fn a_png_edit_of_a_cube_face_packs_in_place() {
    let (data, manifest) = archive();
    let t = manifest.textures.iter().find(|t| t.pixel_format == "R8G8B8A8_SRGB").unwrap();
    let new = Image { width: 32, height: 32, rgba: (0..32 * 32).flat_map(|i| [i as u8, 7, 200, 255]).collect() };
    let edits = [(t.id, TextureEdit::Images([((3, 0), new.clone())].into()))].into();
    let result = pack(&data, &manifest, &edits, &PackOptions::default()).unwrap();
    assert_eq!(result.textures[0].outcome, Outcome::Reencoded { images: 1, mips: 6 });
    let (offset, bytes) = &result.patches[0];
    assert_eq!(*offset, t.offset);
    let info = texture_at(&data, t.offset, Container::Ktx2).unwrap().info;
    // Header, level index, DFD and key/value data are untouched.
    let header = info.header_size as usize;
    assert_eq!(bytes[..header], data[t.offset as usize..][..header]);
    assert_eq!(decode(bytes, &info, Subresource { layer: 3, mip: 0, slice: 0 }).unwrap(), new);
    for layer in [0, 1, 2, 4, 5] {
        let sub = Subresource { layer, mip: 0, slice: 0 };
        assert_eq!(decode(bytes, &info, sub).unwrap(), decode(&data[t.offset as usize..], &info, sub).unwrap());
    }
}

#[test]
fn a_dds_can_replace_a_ktx2_texture() {
    let (data, manifest) = archive();
    let t = manifest.textures.iter().find(|t| t.pixel_format == "BC7_SRGB_BLOCK").unwrap();
    // The same shape and format, as a DX10 DDS: BC7_UNORM_SRGB.
    let dds = DdsSpec::new(128, 64, 8, Pf::Dx10(99), Unit::Block(16), "", None).build(&mut Rng::new(3));
    let edits = [(t.id, TextureEdit::Texture(dds.clone()))].into();
    let result = pack(&data, &manifest, &edits, &PackOptions::default()).unwrap();
    assert_eq!(result.textures[0].outcome, Outcome::Replaced);
    let (_, bytes) = &result.patches[0];
    let info = texture_at(&data, t.offset, Container::Ktx2).unwrap().info;
    let dds_info = texture_at(&dds, 0, Container::Dds).unwrap().info;
    for mip in 0..8 {
        let sub = Subresource { layer: 0, mip, slice: 0 };
        let (to, from) = (info.subresource_range(sub).unwrap(), dds_info.subresource_range(sub).unwrap());
        assert_eq!(bytes[to], dds[from], "mip {mip}");
    }
}

/// The same random level data written plain and supercompressed.
fn plain_and_packed(scheme: u32) -> (Vec<u8>, Vec<u8>) {
    let mut spec = DdsSpec::new(16, 16, 5, Pf::Dx10(0), Unit::Bits(32), "", None);
    spec.cube = true;
    (ktx2(&spec, 37, 0, &mut Rng::new(8)), ktx2(&spec, 37, scheme, &mut Rng::new(8)))
}

#[test]
fn zstd_and_zlib_levels_decode_like_plain_ones() {
    for scheme in [2, 3] {
        let (plain, packed) = plain_and_packed(scheme);
        let a = texture_at(&plain, 0, Container::Ktx2).unwrap().info;
        let b = texture_at(&packed, 0, Container::Ktx2).unwrap().info;
        assert_eq!(b.size, packed.len() as u64);
        for layer in 0..6 {
            for mip in 0..5 {
                let sub = Subresource { layer, mip, slice: 0 };
                assert_eq!(decode(&plain, &a, sub).unwrap(), decode(&packed, &b, sub).unwrap(), "scheme {scheme} {sub:?}");
            }
        }
        // A corrupt level is an error, not a panic. (Damaging the data inside a frame
        // needn't be: these frames have no checksum, so break its header instead.)
        let mut broken = packed.clone();
        let (mip0, _) = level(&broken, 0);
        broken[mip0] ^= 0xff;
        broken[mip0 + 1] ^= 0xff;
        let err = decode(&broken, &b, Subresource::default());
        assert!(matches!(err, Err(DecodeError::Decompress { mip: 0, .. })), "scheme {scheme}: {err:?}");
    }
}

#[test]
fn what_can_be_read_but_not_written_says_so() {
    let (data, manifest) = archive();
    let blank = |t: &texscan_core::TextureEntry| Image { width: t.width, height: t.height, rgba: vec![0; (t.width * t.height * 4) as usize] };
    let find = |name: &str| manifest.textures.iter().find(|t| t.pixel_format == name).unwrap();
    for (name, why) in [
        ("BC1_RGBA_UNORM_BLOCK", "Zstandard-supercompressed KTX2 can be read but not written yet"),
        ("ASTC_6x6_UNORM_BLOCK", "writing ASTC_6x6_UNORM_BLOCK isn't supported yet"),
        ("ETC2_R8G8B8_UNORM_BLOCK", "writing ETC2_R8G8B8_UNORM_BLOCK isn't supported yet"),
    ] {
        let t = find(name);
        let info = texture_at(&data, t.offset, Container::Ktx2).unwrap().info;
        decode(&data[t.offset as usize..], &info, Subresource::default()).unwrap();
        let edits = [(t.id, TextureEdit::Images([((0, 0), blank(t))].into()))].into();
        let err = pack(&data, &manifest, &edits, &PackOptions::default()).unwrap_err();
        assert!(err.to_string().contains(why), "{name}: {err}");
    }
    // Basis Universal can't even be decoded.
    let t = find("UNDEFINED (Basis Universal ETC1S)");
    let info = texture_at(&data, t.offset, Container::Ktx2).unwrap().info;
    let err = decode(&data[t.offset as usize..], &info, Subresource::default()).unwrap_err();
    assert_eq!(err, DecodeError::Unsupported("BasisLZ-supercompressed KTX2"));
}
