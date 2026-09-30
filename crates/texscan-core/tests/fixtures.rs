//! Scan and extract every fixture; textures must be reported at exactly the known offsets
//! with the sizes the fixture builder worked out independently.

use std::fs;
use std::path::Path;

use texscan_core::{Container, Error, ExtractOptions, Manifest, ScanOptions, SourceInfo, extract_all, scan, texture_at};
use texscan_fixtures::Fixture;

type Row = (u64, u64, u32, u32, u32, u32, u32, u32, String, Option<u32>, u32);

fn expected_rows(f: &Fixture) -> Vec<Row> {
    f.expected
        .iter()
        .map(|e| {
            let s = &e.spec;
            let (offset, size) = (e.offset as u64, e.size as u64);
            (offset, size, s.width, s.height, s.depth, s.mips.max(1), s.array_size, s.faces(), s.name.into(), s.dxgi, e.crc32)
        })
        .collect()
}

fn found_rows(data: &[u8]) -> Vec<Row> {
    scan(data, &ScanOptions::default())
        .textures
        .iter()
        .map(|t| {
            let i = &t.info;
            let name = i.pixel_format.name.to_string();
            (t.offset, i.size, i.width, i.height, i.depth, i.mips, i.array_size, i.faces, name, i.pixel_format.dxgi, t.crc32)
        })
        .collect()
}

#[test]
fn every_fixture_scans_exactly() {
    for f in texscan_fixtures::all() {
        assert_eq!(found_rows(&f.data), expected_rows(&f), "fixture {}", f.name);
    }
}

#[test]
fn unusable_headers_are_reported_with_a_reason() {
    for f in texscan_fixtures::all() {
        let report = scan(&f.data, &ScanOptions::default());
        let found: Vec<_> = report.rejected.iter().map(|r| r.offset).collect();
        let expected: Vec<_> = f.rejected.iter().map(|r| r.offset as u64).collect();
        assert_eq!(found, expected, "fixture {}", f.name);
        for (r, e) in report.rejected.iter().zip(&f.rejected) {
            assert!(r.reason.starts_with(e.reason_prefix), "fixture {}: {:?}", f.name, r.reason);
        }
    }
}

#[test]
fn texture_at_an_exact_offset() {
    let f = texscan_fixtures::dds_archive();
    let e = &f.expected[3];
    let t = texture_at(&f.data, e.offset as u64, Container::Dds).unwrap();
    assert_eq!((t.info.size, t.crc32), (e.size as u64, e.crc32));
    assert!(texture_at(&f.data, e.offset as u64 + 1, Container::Dds).is_err());
    assert!(texture_at(&f.data, u64::MAX, Container::Dds).is_err());
}

#[test]
fn extract_writes_each_texture_verbatim() {
    let f = texscan_fixtures::dds_archive();
    let found = scan(&f.data, &ScanOptions::default()).textures;
    let manifest = Manifest::new(SourceInfo::describe(Path::new(f.name), &f.data), ScanOptions::default(), &found);
    let manifest = Manifest::from_json(&manifest.to_json()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let files = extract_all(&f.data, &manifest, dir.path(), &ExtractOptions::default()).unwrap();
    assert_eq!(files.len(), f.expected.len());
    for (file, e) in files.iter().zip(&f.expected) {
        let bytes = fs::read(&file.path).unwrap();
        assert_eq!(bytes, &f.data[e.offset..e.offset + e.size]);
        // An extracted texture is a valid .dds file on its own.
        let alone = scan(&bytes, &ScanOptions::default()).textures;
        assert_eq!((alone.len(), alone[0].offset, alone[0].info.size), (1, 0, e.size as u64));
    }
}

#[test]
fn extract_refuses_a_different_input() {
    let f = texscan_fixtures::dds_file();
    let found = scan(&f.data, &ScanOptions::default()).textures;
    let manifest = Manifest::new(SourceInfo::describe(Path::new(f.name), &f.data), ScanOptions::default(), &found);
    let mut changed = f.data.clone();
    changed[500] ^= 1;
    let dir = tempfile::tempdir().unwrap();
    let err = extract_all(&changed, &manifest, dir.path(), &ExtractOptions::default()).unwrap_err();
    assert!(matches!(err, Error::SourceMismatch(_)), "{err}");
    // Forcing past the file check still catches the changed texture.
    let err = extract_all(&changed, &manifest, dir.path(), &ExtractOptions { verify_source: false, ..Default::default() }).unwrap_err();
    assert!(matches!(err, Error::Texture { id: 0, .. }), "{err}");
}

/// The generated files in `tests/fixtures` must match what the builder makes now.
#[test]
fn checked_in_fixtures_are_current() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    for f in texscan_fixtures::all() {
        let on_disk = fs::read(dir.join(format!("{}.bin", f.name))).unwrap();
        assert!(on_disk == f.data, "{}.bin is stale: run `cargo run -p texscan-fixtures --bin gen-fixtures`", f.name);
    }
}
