//! `pcbforge` — the operator CLI.
//!
//! * `next` (ORC-2): advance the board on the bed by one stage. Each
//!   invocation opens the DB, reads the pallet tag, runs the current stage's
//!   executor, advances, and persists.
//! * `noncopper`: the FlatCAM-replacement inversion — read a KiCad copper
//!   Gerber (plus optionally the Edge.Cuts Gerber), compute the non-copper
//!   regions as contiguous closed shapes, and export SVG/DXF for the
//!   fill-and-ablate workflow in LightBurn or EZCAD.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use orchestra::db::Db;
use orchestra::engine::{self, BoardDefaults, EnvPalletSource, ExecutorRegistry, StepReport};
use orchestra::stages::StageGraph;
use pcb_core::NM_PER_MM;

/// Default database path when `--db` is not given.
const DEFAULT_DB: &str = "pcbforge.sqlite";

#[derive(Parser)]
#[command(name = "pcbforge", version, about = "PCBForge orchestration CLI")]
struct Cli {
    /// Path to the SQLite database (created if absent).
    #[arg(long, global = true, default_value = DEFAULT_DB)]
    db: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Advance the board on the bed by one stage.
    Next {
        /// Placeholder design path used when a pallet is first seen.
        #[arg(long)]
        design: Option<String>,
    },
    /// Invert a KiCad copper Gerber into fillable non-copper shapes (SVG/DXF
    /// for LightBurn or EZCAD) — replaces the FlatCAM step.
    Noncopper {
        /// Copper layer Gerber (e.g. F_Cu.gbr from `kicad-cli pcb export gerbers`).
        #[arg(long)]
        copper: PathBuf,

        /// Board outline Gerber (Edge.Cuts). Without it the board region is
        /// the copper bounding box grown by --margin-mm.
        #[arg(long)]
        outline: Option<PathBuf>,

        /// Beam-compensation clearance kept around every copper edge, mm
        /// (typically half the effective spot diameter). 0 = exact inverse.
        #[arg(long, default_value_t = 0.0)]
        offset_mm: f64,

        /// Bounding-box margin when no --outline is given, mm.
        #[arg(long, default_value_t = 1.0)]
        margin_mm: f64,

        /// Write the shapes as DXF R12 (EZCAD; also fine in LightBurn with
        /// DXF import units set to mm).
        #[arg(long)]
        dxf: Option<PathBuf>,

        /// Write the shapes as an SVG (black, even-odd fill, mm units) —
        /// the preferred LightBurn import: set the layer to Fill and burn.
        #[arg(long)]
        svg: Option<PathBuf>,

        /// Write a color preview SVG (board / copper / to-ablate) for eyeballing.
        #[arg(long)]
        preview: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pcbforge: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    match &cli.command {
        Command::Next { design } => next(&cli.db, design.as_deref()),
        Command::Noncopper {
            copper,
            outline,
            offset_mm,
            margin_mm,
            dxf,
            svg,
            preview,
        } => noncopper_cmd(
            copper,
            outline.as_deref(),
            *offset_mm,
            *margin_mm,
            dxf.as_deref(),
            svg.as_deref(),
            preview.as_deref(),
        ),
    }
}

/// The FlatCAM-replacement pipeline: Gerber → copper polys → board region →
/// inverted fillable shapes → DXF/SVG.
fn noncopper_cmd(
    copper_path: &std::path::Path,
    outline_path: Option<&std::path::Path>,
    offset_mm: f64,
    margin_mm: f64,
    dxf: Option<&std::path::Path>,
    svg: Option<&std::path::Path>,
    preview: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    if dxf.is_none() && svg.is_none() && preview.is_none() {
        return Err("nothing to do: pass at least one of --dxf, --svg, --preview".into());
    }
    if !(0.0..10.0).contains(&offset_mm) {
        return Err(format!("--offset-mm {offset_mm} out of range [0, 10)").into());
    }

    let copper = ingest::gerber::load_gerber(copper_path)?;
    eprintln!(
        "copper: {} shape(s) from {}",
        copper.polys.len(),
        copper_path.display()
    );

    let board = match outline_path {
        Some(p) => {
            let outline = ingest::gerber::load_gerber(p)?;
            let region = cam::noncopper::board_region_from_outline(&outline.polys);
            if region.is_empty() {
                return Err(format!("outline {} encloses no area", p.display()).into());
            }
            eprintln!("board: outline region from {}", p.display());
            region
        }
        None => {
            let margin_nm = (margin_mm * NM_PER_MM as f64).round() as i64;
            eprintln!("board: copper bounding box + {margin_mm} mm margin (no --outline)");
            cam::noncopper::board_region_bbox(&copper.polys, margin_nm)
        }
    };
    if board.is_empty() {
        return Err("empty board region (no copper and no outline)".into());
    }

    let offset_nm = (offset_mm * NM_PER_MM as f64).round() as i64;
    let shapes = cam::noncopper::noncopper(&board, &copper.polys, offset_nm);
    let rings: usize = shapes.iter().map(|p| 1 + p.holes.len()).sum();
    eprintln!(
        "non-copper: {} contiguous shape(s), {} ring(s), offset {offset_mm} mm",
        shapes.len(),
        rings
    );
    if shapes.is_empty() {
        return Err("inversion produced no shapes (offset too large?)".into());
    }

    if let Some(p) = dxf {
        cam::export::write_dxf(&shapes, p)?;
        println!("wrote {}", p.display());
    }
    if let Some(p) = svg {
        cam::export::write_svg(&shapes, p)?;
        println!("wrote {}", p.display());
    }
    if let Some(p) = preview {
        cam::export::write_preview_svg(&board, &copper.polys, &shapes, p)?;
        println!("wrote {}", p.display());
    }
    Ok(())
}

fn next(db_path: &str, design: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let db = Db::open(db_path)?;
    let graph = StageGraph::load()?;
    let registry = ExecutorRegistry::with_defaults();
    // Pallet id comes from VIS-11 (camera/AprilTag) later; for now the stub
    // source reads PCBFORGE_PALLET_TAG or a fixed default.
    let pallet = EnvPalletSource::default();

    let mut defaults = BoardDefaults::default();
    if let Some(d) = design {
        defaults.design_path = d.to_owned();
    }

    let report = engine::step(&db, &graph, &registry, &pallet, &defaults)?;
    match report {
        StepReport::Advanced {
            board_id,
            stage,
            to,
        } => {
            println!("board {board_id}: {stage} -> {to}");
        }
        StepReport::Stayed { board_id, stage } => {
            println!("board {board_id}: staying at {stage} (needs another pass)");
        }
        StepReport::Halted { board_id, stage } => {
            println!("board {board_id}: halted at {stage} (complete or escalated)");
        }
    }
    Ok(())
}
