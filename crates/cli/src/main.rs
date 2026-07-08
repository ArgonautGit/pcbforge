//! `pcbforge` — the operator CLI.
//!
//! ORC-2 ships one verb, `next`: advance the board on the bed by one stage.
//! Each invocation opens the DB, reads the pallet tag, runs the current stage's
//! executor, advances, and persists — so running `pcbforge next` repeatedly
//! walks a board through the graph, one stage per call.

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use orchestra::db::Db;
use orchestra::engine::{self, BoardDefaults, EnvPalletSource, ExecutorRegistry, StepReport};
use orchestra::stages::StageGraph;

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
    }
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
