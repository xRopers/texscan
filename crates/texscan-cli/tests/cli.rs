//! Run the real binary on the fixtures.

use std::fs;
use std::process::Command;

use serde_json::Value;

fn texscan() -> Command {
    Command::new(env!("CARGO_BIN_EXE_texscan"))
}

fn write_fixture(dir: &std::path::Path, f: &texscan_fixtures::Fixture) -> std::path::PathBuf {
    let path = dir.join(format!("{}.bin", f.name));
    fs::write(&path, &f.data).unwrap();
    path
}

#[test]
fn scan_json_lists_textures_and_rejected_headers() {
    let dir = tempfile::tempdir().unwrap();
    let f = texscan_fixtures::dds_archive();
    let input = write_fixture(dir.path(), &f);
    let out = texscan().args(["scan", "--json"]).arg(&input).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let offsets: Vec<_> = json["textures"].as_array().unwrap().iter().map(|t| t["offset"].as_u64().unwrap()).collect();
    let expected: Vec<_> = f.expected.iter().map(|e| e.offset as u64).collect();
    assert_eq!(offsets, expected);
    assert_eq!(json["rejected"].as_array().unwrap().len(), f.rejected.len());
    assert_eq!(json["textures"][3]["pixel_format"], "BC7_UNORM_SRGB");
}

#[test]
fn scan_text_then_extract_with_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let f = texscan_fixtures::dds_archive();
    let input = write_fixture(dir.path(), &f);
    let manifest = dir.path().join("m.json");
    let out = texscan().arg("scan").arg(&input).arg("-o").arg(&manifest).arg("--show-rejected").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&format!("{} texture(s) found", f.expected.len())), "{text}");
    assert!(text.contains("unknown FourCC \"ZZZZ\""), "{text}");
    assert!(text.contains("32x32 cube"), "{text}");

    let out_dir = dir.path().join("out");
    let out = texscan().arg("extract").arg(&input).arg("-m").arg(&manifest).arg("-d").arg(&out_dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    for e in &f.expected {
        let bytes = fs::read(out_dir.join(format!("{:08x}.dds", e.offset))).unwrap();
        assert_eq!(bytes, &f.data[e.offset..e.offset + e.size]);
    }
}

#[test]
fn extract_refuses_a_changed_input_unless_forced() {
    let dir = tempfile::tempdir().unwrap();
    let f = texscan_fixtures::dds_archive();
    let input = write_fixture(dir.path(), &f);
    let manifest = dir.path().join("m.json");
    assert!(texscan().arg("scan").arg(&input).arg("-o").arg(&manifest).output().unwrap().status.success());
    // Change a byte outside every texture.
    let mut data = f.data.clone();
    data[10] ^= 0xff;
    fs::write(&input, &data).unwrap();
    let out_dir = dir.path().join("out");
    let run = |force: bool| {
        let mut cmd = texscan();
        cmd.arg("extract").arg(&input).arg("-m").arg(&manifest).arg("-d").arg(&out_dir);
        if force {
            cmd.arg("--force");
        }
        cmd.output().unwrap()
    };
    let out = run(false);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not match the manifest"));
    assert!(run(true).status.success());
}
