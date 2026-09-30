//! Writes every fixture to a directory (default `tests/fixtures`) as `<name>.bin`
//! plus `<name>.expected.json` listing the textures the scanner should report.

use std::fs;
use std::path::PathBuf;

use serde_json::json;

fn main() -> std::io::Result<()> {
    let dir = std::env::args().nth(1).map_or_else(|| PathBuf::from("tests/fixtures"), PathBuf::from);
    fs::create_dir_all(&dir)?;
    for fixture in texscan_fixtures::all() {
        let textures: Vec<_> = fixture
            .expected
            .iter()
            .map(|e| {
                let s = &e.spec;
                json!({
                    "offset": e.offset,
                    "format": "dds",
                    "size": e.size,
                    "width": s.width,
                    "height": s.height,
                    "depth": s.depth,
                    "mips": s.mips.max(1),
                    "array_size": s.array_size,
                    "faces": s.faces(),
                    "pixel_format": s.name,
                    "dxgi_format": s.dxgi,
                    "crc32": format!("{:08x}", e.crc32),
                })
            })
            .collect();
        let rejected: Vec<_> =
            fixture.rejected.iter().map(|r| json!({ "offset": r.offset, "reason": r.reason_prefix })).collect();
        let expected = json!({
            "description": fixture.description,
            "size": fixture.data.len(),
            "textures": textures,
            "rejected": rejected,
        });
        fs::write(dir.join(format!("{}.bin", fixture.name)), &fixture.data)?;
        fs::write(
            dir.join(format!("{}.expected.json", fixture.name)),
            serde_json::to_string_pretty(&expected).unwrap() + "\n",
        )?;
        println!("{:<12} {:>8} bytes, {} textures", fixture.name, fixture.data.len(), fixture.expected.len());
    }
    Ok(())
}
