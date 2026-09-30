use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use texscan_core::{
    Container, ExtractOptions, FoundTexture, Manifest, Rejected, ScanOptions, SourceInfo, TextureEntry, extract_all, input,
    scan,
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
}

#[derive(Args)]
struct ScanArgs {
    /// Formats to look for, comma separated [default: all (dds)]
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
    }
    Ok(())
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
