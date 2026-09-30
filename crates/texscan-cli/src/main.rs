use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use texscan_core::{
    Container, ExtractOptions, FoundTexture, Manifest, Outcome, PackOptions, Rejected, ScanOptions, SourceInfo, TextureEntry,
    extract_all, input, load_edits, pack, scan,
};

#[derive(Parser)]
#[command(name = "texscan", version, about = "Find, extract and reinject textures inside binary files")]
struct Cli {
    /// Print machine-readable JSON on stdout instead of text
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Find textures and optionally write a manifest
    Scan {
        file: PathBuf,
        /// Write the manifest here
        #[arg(short, long, value_name = "MANIFEST")]
        output: Option<PathBuf>,
        /// Also list headers that look like textures but can't be used, and why
        #[arg(long)]
        show_rejected: bool,
        #[command(flatten)]
        filters: ScanArgs,
    },
    /// Extract the textures listed in a manifest (or found by a fresh scan if no manifest
    /// is given), each as a file of its own
    Extract {
        file: PathBuf,
        /// Manifest from `texscan scan -o`
        #[arg(short, long)]
        manifest: Option<PathBuf>,
        /// Output directory
        #[arg(short = 'd', long = "dir", value_name = "DIR")]
        dir: PathBuf,
        /// Extract even if the input's size or CRC no longer matches the manifest
        #[arg(long)]
        force: bool,
        /// Also save each texture as PNG: the top mip, one file per array element, cube
        /// face (_px, _nx, _py, _ny, _pz, _nz) and depth slice
        #[arg(long)]
        png: bool,
        /// Filters for the fresh scan (ignored with --manifest)
        #[command(flatten)]
        filters: ScanArgs,
    },
    /// Put edited textures from an extract folder back into a copy of the input. Edit a
    /// texture's PNG (same size; mips are regenerated) or replace its .dds/.ktx2 with a
    /// DDS or KTX2 file of the same dimensions, mips and pixel format. Textures keep their size, so nothing
    /// else in the file moves
    Pack {
        file: PathBuf,
        /// Manifest from `texscan scan -o`
        #[arg(short, long)]
        manifest: PathBuf,
        /// Folder written by `texscan extract` (with `--png` to edit PNGs)
        #[arg(short = 'd', long = "dir", value_name = "DIR")]
        dir: PathBuf,
        /// Output file [default: <input stem>.packed.<ext> next to the input]
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Show what would change without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Pack even if the input's size or CRC no longer matches the manifest
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct ScanArgs {
    /// Formats to look for, comma separated [default: all: dds, ktx2]
    #[arg(long, value_delimiter = ',', default_values_t = Container::ALL.to_vec(), hide_default_value = true)]
    formats: Vec<Container>,
}

impl ScanArgs {
    fn options(&self) -> ScanOptions {
        ScanOptions { formats: self.formats.clone() }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Scan { file, output, show_rejected, filters } => {
            let data = input::open(&file)?;
            let opts = filters.options();
            let report = scan(&data, &opts);
            let manifest = Manifest::new(SourceInfo::describe(&file, &data), opts, &report.textures);
            if let Some(path) = &output {
                manifest.save(path)?;
            }
            if cli.json {
                print_json(&ScanJson { textures: &manifest.textures, rejected: &report.rejected })?;
            } else {
                print_textures(&report.textures);
                println!("{} texture(s) found", report.textures.len());
                if !report.rejected.is_empty() {
                    if show_rejected {
                        print_rejected(&report.rejected);
                    } else {
                        println!("{} header(s) look like textures but can't be used (--show-rejected lists them)", report.rejected.len());
                    }
                }
                if let Some(path) = &output {
                    println!("manifest written to {}", path.display());
                }
            }
        }
        Command::Extract { file, manifest, dir, force, png, filters } => {
            let data = input::open(&file)?;
            let manifest = match &manifest {
                Some(path) => Manifest::load(path)?,
                None => {
                    let opts = filters.options();
                    let found = scan(&data, &opts).textures;
                    Manifest::new(SourceInfo::describe(&file, &data), opts, &found)
                }
            };
            if manifest.textures.is_empty() {
                bail!("no textures to extract");
            }
            let files = extract_all(&data, &manifest, &dir, &ExtractOptions { verify_source: !force, png })?;
            if cli.json {
                let rows: Vec<_> = files
                    .iter()
                    .map(|f| ExtractJson {
                        id: f.id,
                        offset: f.offset,
                        path: f.path.display().to_string(),
                        size: f.size,
                        pngs: &f.pngs,
                        png_error: f.png_error.as_deref(),
                    })
                    .collect();
                print_json(&rows)?;
            } else {
                for f in &files {
                    if let Some(e) = &f.png_error {
                        println!("{}: no PNG: {e}", f.path.display());
                    }
                }
                println!("{} texture(s) extracted to {}", files.len(), dir.display());
                if png {
                    let failed = files.iter().filter(|f| f.png_error.is_some()).count();
                    let written: usize = files.iter().map(|f| f.pngs.len()).sum();
                    println!("{written} PNG(s) written{}", if failed > 0 { format!(", {failed} texture(s) couldn't be decoded") } else { String::new() });
                }
            }
        }
        Command::Pack { file, manifest, dir, output, dry_run, force } => {
            let data = input::open(&file)?;
            let manifest = Manifest::load(&manifest)?;
            let found = load_edits(&data, &manifest, &dir)?;
            let result = pack(&data, &manifest, &found.edits, &PackOptions { verify_source: !force })?;
            let output = output.unwrap_or_else(|| default_output(&file));
            if !dry_run && result.changed() > 0 {
                if same_file(&output, &file) {
                    bail!("the output would overwrite the input; choose another --output");
                }
                result.write_file(&data, &output)?;
            }
            if cli.json {
                let rows: Vec<_> = result
                    .textures
                    .iter()
                    .map(|t| PackJson {
                        id: t.id,
                        offset: t.offset,
                        outcome: match t.outcome {
                            Outcome::Replaced => "replaced",
                            Outcome::Reencoded { .. } => "reencoded",
                            Outcome::Unchanged => "unchanged",
                        },
                        note: t.note.as_deref(),
                    })
                    .collect();
                let written = (!dry_run && result.changed() > 0).then(|| output.display().to_string());
                print_json(&serde_json::json!({ "textures": rows, "output": written }))?;
            } else {
                for t in &result.textures {
                    let what = match &t.outcome {
                        Outcome::Replaced => "replaced from the texture file".to_string(),
                        Outcome::Reencoded { images, mips } => format!("{images} image(s) encoded, {mips} mip(s) each"),
                        Outcome::Unchanged => "unchanged (the edit gives the same bytes)".to_string(),
                    };
                    println!("texture {:>3} at {:#x}: {what}", t.id, t.offset);
                    if let Some(note) = &t.note {
                        println!("    note: {note}");
                    }
                }
                match (result.changed(), dry_run) {
                    (0, _) => println!("nothing to pack: no edited textures in {}", dir.display()),
                    (n, true) => println!("dry run: {n} texture(s) would change; nothing written"),
                    (n, false) => println!("{n} texture(s) packed into {} (verified)", output.display()),
                }
            }
        }
    }
    Ok(())
}

/// `game.pak` → `game.packed.pak` next to it.
fn default_output(input: &std::path::Path) -> PathBuf {
    let stem = input.file_stem().map_or_else(|| "output".into(), |s| s.to_string_lossy().into_owned());
    let name = match input.extension() {
        Some(ext) => format!("{stem}.packed.{}", ext.to_string_lossy()),
        None => format!("{stem}.packed"),
    };
    input.with_file_name(name)
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[derive(Serialize)]
struct PackJson<'a> {
    id: u32,
    offset: u64,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
}

#[derive(Serialize)]
struct ScanJson<'a> {
    textures: &'a [TextureEntry],
    rejected: &'a [Rejected],
}

#[derive(Serialize)]
struct ExtractJson<'a> {
    id: u32,
    offset: u64,
    path: String,
    size: u64,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    pngs: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    png_error: Option<&'a str>,
}

fn print_json<T: Serialize + ?Sized>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_textures(textures: &[FoundTexture]) {
    if textures.is_empty() {
        return;
    }
    println!("{:>12}  {:>10}  {:<6}  {:<18}  {:>4}  PIXEL FORMAT", "OFFSET", "SIZE", "FORMAT", "DIMENSIONS", "MIPS");
    for t in textures {
        let i = &t.info;
        println!(
            "{:>#12x}  {:>10}  {:<6}  {:<18}  {:>4}  {}",
            t.offset,
            i.size,
            t.container,
            i.dimensions(),
            i.mips,
            i.pixel_format.name
        );
    }
}

fn print_rejected(rejected: &[Rejected]) {
    println!("Not usable:");
    for r in rejected {
        println!("{:>#12x}  {:<6}  {}", r.offset, r.container, r.reason);
    }
}
