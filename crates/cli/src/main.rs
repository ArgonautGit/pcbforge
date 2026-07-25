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

mod drillguide;

use orchestra::db::Db;
use orchestra::engine::{
    self, BoardDefaults, EnvPalletSource, ExecutorRegistry, RecoveryAction, StepReport,
};
use orchestra::stages::StageGraph;

use cam::lbrn2::{self, EmitLayer};
use pcb_core::{AblationParams, CutOpts, Machine, NM_PER_MM, Nm};

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
        /// Use the non-hardware bring-up executors.
        #[arg(long)]
        bringup_stubs: bool,
        /// Admit a new board after the previous board is cleanly complete.
        #[arg(long)]
        new_board: bool,
    },
    /// Reconcile an interrupted stage after physical inspection.
    Recover {
        #[arg(long)]
        board_id: i64,
        #[arg(long, conflicts_with = "mark_done")]
        retry: bool,
        #[arg(long, conflicts_with = "retry")]
        mark_done: bool,
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

        /// Ablate NonConductor copper (no-net pours, logos) instead of
        /// keeping it. By default all copper in the Gerber is kept — an
        /// isolated ground pour is still real copper.
        #[arg(long)]
        clear_nonconductor: bool,

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
    /// Generate the board-outline through-cut (depaneling) job from Edge.Cuts:
    /// kerf-compensated, tabbed cut geometry plus a focus-step schedule that
    /// lowers the focal plane as the cut deepens (CAM-10).
    Cut {
        /// KiCad board (.kicad_pcb): Edge.Cuts is exported via kicad-cli.
        #[arg(long)]
        board: Option<PathBuf>,

        /// Board outline Gerber (Edge.Cuts) — alternative to --board.
        #[arg(long)]
        outline: Option<PathBuf>,

        /// Finished board thickness, mm. Required with --outline; with --board
        /// it overrides any thickness found in the .gbrjob.
        #[arg(long)]
        thickness_mm: Option<f64>,

        /// Output directory for per-step SVG/DXF files and cut-schedule.txt.
        #[arg(long)]
        out: PathBuf,

        /// Measured beam kerf width, mm. The cut centerline is offset onto the
        /// waste side by kerf/2. Leave unset to use the (un-calibrated) default.
        #[arg(long)]
        kerf_mm: Option<f64>,

        /// Holding tabs left per closed ring.
        #[arg(long, default_value_t = 4)]
        tabs: u32,

        /// Width of solid material each tab leaves standing, mm.
        #[arg(long, default_value_t = 0.5)]
        tab_mm: f64,

        /// Measured FR4 depth removed per pass at cut params, mm. Leave unset
        /// to use the (un-calibrated) default.
        #[arg(long)]
        mm_per_pass: Option<f64>,

        /// Maximum focal-plane drop per step, mm (≤ the lens depth of focus).
        /// Leave unset to use the (un-calibrated) default.
        #[arg(long)]
        z_step_mm: Option<f64>,

        /// Extra commanded depth past the far face, mm.
        #[arg(long, default_value_t = 0.1)]
        overcut_mm: f64,

        /// Cutting machine: "fiber" (FR4 bulk) or "uv".
        #[arg(long, default_value = "fiber")]
        machine: String,
    },
    /// Emit a LightBurn `.lbrn2` directly from a KiCad copper Gerber: invert to
    /// non-copper regions and write them as a Fill layer with the given
    /// process recipe — no SVG/DXF import step (EMIT-3).
    Emit {
        /// Copper layer Gerber (e.g. F_Cu.gbr).
        #[arg(long)]
        copper: PathBuf,

        /// Board outline Gerber (Edge.Cuts); without it, copper bbox + margin.
        #[arg(long)]
        outline: Option<PathBuf>,

        /// Output `.lbrn2` path.
        #[arg(long)]
        lbrn2: PathBuf,

        /// Also write a color preview SVG (board / kept-copper / to-ablate) so
        /// you can eyeball exactly what will burn before importing to LightBurn.
        #[arg(long)]
        preview: Option<PathBuf>,

        /// Beam-compensation clearance around copper, mm (0 = exact inverse).
        #[arg(long, default_value_t = 0.0)]
        offset_mm: f64,

        /// Ablate NonConductor copper (no-net pours, logos) instead of
        /// keeping it. By default all copper in the Gerber is kept — an
        /// isolated ground pour is still real copper.
        #[arg(long)]
        clear_nonconductor: bool,

        /// Bounding-box margin when no --outline is given, mm.
        #[arg(long, default_value_t = 1.0)]
        margin_mm: f64,

        /// LightBurn device name (must match a configured device).
        #[arg(long, default_value = lbrn2::DEFAULT_DEVICE)]
        device: String,

        // --- process recipe (see docs/lbrn2-schema.md) ---
        /// Max power %.
        #[arg(long, default_value_t = 20.0)]
        power_pct: f64,
        /// Scan speed, mm/s.
        #[arg(long, default_value_t = 1000.0)]
        speed_mm_s: f64,
        /// Frequency, kHz (written to the file in Hz).
        #[arg(long, default_value_t = 30.0)]
        frequency_khz: f64,
        /// MOPA Q-pulse width, ns (a fluence knob; 0 = source default).
        #[arg(long, default_value_t = 1)]
        pulse_ns: u32,
        /// Fill passes.
        #[arg(long, default_value_t = 1)]
        passes: u32,
        /// Fill line interval, mm.
        #[arg(long, default_value_t = 0.03)]
        interval_mm: f64,
        /// Fill scan angle, deg.
        #[arg(long, default_value_t = 0.0)]
        angle_deg: f64,
        /// Per-pass hatch-angle increment, deg (`anglePerPass`): rotate the
        /// fill lines by this much each pass (needs --passes > 1). 0 = off.
        #[arg(long, default_value_t = 0.0)]
        angle_step_deg: f64,
        /// Enable wobble (spiral the beam along the scan to widen the
        /// effective line). Off by default: the file says `wobbleEnable=0`
        /// explicitly so the device profile can't re-enable it.
        #[arg(long)]
        wobble: bool,
        /// Wobble step along the path, mm (with --wobble; 0 = device default).
        #[arg(long, default_value_t = 0.0)]
        wobble_step_mm: f64,
        /// Wobble diameter, mm (with --wobble; 0 = device default).
        #[arg(long, default_value_t = 0.0)]
        wobble_size_mm: f64,

        // --- placement (FLD-6) ---
        /// Target x for the job, mm. By default the job's lower-left corner
        /// lands here (0 = workspace origin); with --center the job's center
        /// lands here.
        #[arg(long, default_value_t = 0.0)]
        origin_x: f64,
        /// Target y for the job, mm. See --origin-x.
        #[arg(long, default_value_t = 0.0)]
        origin_y: f64,
        /// Interpret --origin-x/-y as the job's center rather than its
        /// lower-left corner.
        #[arg(long)]
        center: bool,

        /// Mirror the design in X for the back side of a double-sided board
        /// (KiCad exports B.Cu in top-view coords; flipping the board
        /// left-right needs the design mirrored to match). Winding is preserved.
        #[arg(long)]
        mirror_x: bool,

        /// Laser-field calibration map. When given, every production edge is
        /// densified and pre-warped from desired physical mm to commanded mm
        /// before writing the LightBurn job. When omitted, the geometry is
        /// emitted UNWARPED (a warning is printed) — field accuracy is then
        /// whatever the machine's own correction provides.
        #[arg(long)]
        field_map: Option<PathBuf>,
        /// Maximum physical edge segment before field pre-warping, mm.
        #[arg(long, default_value_t = 0.25)]
        field_seg_mm: f64,
    },
    /// Fiducial-registered emit (VIS-6, host side): fit a design→machine affine
    /// from fiducial correspondences and bake it into the emitted `.lbrn2`, so
    /// the job burns where the physical board actually sits. Supply the
    /// correspondences explicitly (`--fiducials`) or detect them on a camera
    /// frame (`--frame` + `--layout` + `--px-per-mm`).
    ///
    /// FRAME CONTRACT: the "design" side of each correspondence must be in the
    /// same coordinate frame as the copper Gerber (the fit is applied to the
    /// Gerber-frame geometry, with no origin normalization). Export the Gerber
    /// with KiCad's drill/place-file (aux) origin so Gerber coordinates equal
    /// your board coordinates — then a fiducial drilled at board (10,10) is
    /// simply `10,10` on the design side.
    Register {
        /// Copper layer Gerber.
        #[arg(long)]
        copper: PathBuf,
        /// Board outline Gerber (Edge.Cuts); without it, copper bbox + margin.
        #[arg(long)]
        outline: Option<PathBuf>,
        /// Output registered `.lbrn2` path.
        #[arg(long)]
        lbrn2: PathBuf,

        /// Explicit correspondences `dx,dy=tx,ty; …` (≥3): design mm = machine
        /// mm for each fiducial. Mutually exclusive with --frame.
        #[arg(long)]
        fiducials: Option<String>,

        /// Camera frame (PNG/JPEG) to detect fiducials in. Needs --layout and
        /// --px-per-mm. The design positions are --layout; the detected
        /// positions are the machine targets.
        #[arg(long)]
        frame: Option<PathBuf>,
        /// Design fiducial layout `x,y; …` mm (with --frame).
        #[arg(long)]
        layout: Option<String>,
        /// Bed scale for --frame detection, px/mm (uniform until VIS-3).
        #[arg(long)]
        px_per_mm: Option<f64>,
        /// Fiducial hole diameter for --frame detection, mm.
        #[arg(long, default_value_t = 1.0)]
        diameter_mm: f64,

        /// Beam-compensation clearance around copper, mm.
        #[arg(long, default_value_t = 0.0)]
        offset_mm: f64,
        /// Ablate NonConductor copper instead of keeping it.
        #[arg(long)]
        clear_nonconductor: bool,
        /// Bounding-box margin when no --outline is given, mm.
        #[arg(long, default_value_t = 1.0)]
        margin_mm: f64,
        /// LightBurn device name.
        #[arg(long, default_value = lbrn2::DEFAULT_DEVICE)]
        device: String,
        /// Max residual RMS before the fit is rejected, mm.
        #[arg(long, default_value_t = 0.05)]
        max_rms_mm: f64,

        // --- process recipe (see docs/lbrn2-schema.md) ---
        /// Max power %.
        #[arg(long, default_value_t = 20.0)]
        power_pct: f64,
        /// Scan speed, mm/s.
        #[arg(long, default_value_t = 1000.0)]
        speed_mm_s: f64,
        /// Frequency, kHz (written to the file in Hz).
        #[arg(long, default_value_t = 30.0)]
        frequency_khz: f64,
        /// MOPA Q-pulse width, ns (a fluence knob; 0 = source default).
        #[arg(long, default_value_t = 1)]
        pulse_ns: u32,
        /// Fill passes.
        #[arg(long, default_value_t = 1)]
        passes: u32,
        /// Fill line interval, mm.
        #[arg(long, default_value_t = 0.03)]
        interval_mm: f64,
        /// Enable wobble (spiral the beam along the scan to widen the
        /// effective line). Off by default: the file says `wobbleEnable=0`
        /// explicitly so the device profile can't re-enable it.
        #[arg(long)]
        wobble: bool,
        /// Wobble step along the path, mm (with --wobble; 0 = device default).
        #[arg(long, default_value_t = 0.0)]
        wobble_step_mm: f64,
        /// Wobble diameter, mm (with --wobble; 0 = device default).
        #[arg(long, default_value_t = 0.0)]
        wobble_size_mm: f64,

        /// Laser field-distortion correction file (from the console's
        /// step-3 Laser-field calibration). When given, every emitted vertex is
        /// pre-distorted physical→commanded so the beam cancels the galvo/
        /// f-theta field error; edges are densified so the pre-curvature is
        /// preserved. When omitted, the affine-registered geometry is emitted
        /// UNWARPED (a warning is printed).
        #[arg(long)]
        field_map: Option<PathBuf>,
        /// Edge densification for --field-map, mm (smaller = finer curve).
        #[arg(long, default_value_t = 0.5)]
        field_seg_mm: f64,
    },
    /// Emit a calibration dot grid `.lbrn2` for camera→laser calibration: an
    /// n×n lattice of small filled squares at known commanded coordinates.
    /// Burn it, image it with the PCBForge camera, and the console fits the
    /// camera↔laser transform so a placement burns where you put it.
    CalibGrid {
        /// Output `.lbrn2` path.
        #[arg(long)]
        out: PathBuf,
        /// Dots per side (n×n).
        #[arg(long, default_value_t = 7)]
        n: usize,
        /// Grid pitch, mm.
        #[arg(long, default_value_t = 10.0)]
        pitch_mm: f64,
        /// Lower-left dot's commanded position, mm (as "x,y"). Accepts negative
        /// values (a centre-origin galvo puts the field centre at 0,0), e.g.
        /// `--origin -30,-30`.
        #[arg(long, default_value = "0,0", allow_hyphen_values = true)]
        origin: String,
        /// Dot side length, mm.
        #[arg(long, default_value_t = 0.4)]
        dot_mm: f64,
        /// LightBurn device name.
        #[arg(long, default_value = lbrn2::DEFAULT_DEVICE)]
        device: String,
    },
    /// Emit a PRINT-READY camera-lens calibration dot grid as an A4 SVG (metric
    /// true: 1 user unit = 1 mm). Print it at 100% (never fit-to-page), tape the
    /// sheet to the bed, image it with the PCBForge camera, and the console's
    /// step-1 Camera-lens fit turns the camera into a metric px→mm ruler. Unlike
    /// the burned `calib-grid`, dots are printed larger (a diameter, not a laser
    /// spot) so the detector locks reliably on paper.
    PaperGrid {
        /// Output SVG path.
        #[arg(long)]
        out: PathBuf,
        /// Dots per side (n×n).
        #[arg(long, default_value_t = 9)]
        n: usize,
        /// Nominal printed grid pitch, mm (measure the print with calipers — the
        /// step-1 fit wants the MEASURED pitch, since printers scale).
        #[arg(long, default_value_t = 10.0)]
        pitch_mm: f64,
        /// Dot DIAMETER, mm (printed dots are larger than burned ones so the
        /// camera detector finds them reliably).
        #[arg(long, default_value_t = 2.0)]
        dot_mm: f64,
    },
    /// Emit a LightBurn job that burns fiducial holes at the given positions.
    FidHoles {
        /// Output `.lbrn2` path.
        #[arg(long)]
        out: PathBuf,
        /// Fiducial positions in machine mm, "x,y; x,y; ..." (semicolons
        /// separate points; whitespace is tolerated) — the same format the
        /// console's Fiducials tab uses.
        #[arg(long, allow_hyphen_values = true)]
        layout: String,
        /// Hole shape: "circle" or "rect".
        #[arg(long, default_value = "circle")]
        shape: String,
        /// Circle diameter, or rect width, mm.
        #[arg(long, default_value_t = 1.0)]
        w_mm: f64,
        /// Rect height, mm; 0 means "same as --w-mm" (a square). Circles take
        /// only --w-mm.
        #[arg(long, default_value_t = 0.0)]
        h_mm: f64,
        /// LightBurn device name.
        #[arg(long, default_value = lbrn2::DEFAULT_DEVICE)]
        device: String,

        /// Laser-field calibration map. When given, every production edge is
        /// densified and pre-warped from desired physical mm to commanded mm
        /// before writing the LightBurn job. When omitted, the geometry is
        /// emitted UNWARPED (a warning is printed) — field accuracy is then
        /// whatever the machine's own correction provides.
        #[arg(long)]
        field_map: Option<PathBuf>,
        /// Maximum physical edge segment before field pre-warping, mm.
        #[arg(long, default_value_t = 0.25)]
        field_seg_mm: f64,
    },
    /// Camera capture (VIS-1): list devices or grab a single frame. The webcam
    /// backend needs the `camera` feature; `--file` re-reads an image path and
    /// works everywhere (any capture app that writes a frame to disk).
    Cam {
        /// Enumerate available camera devices as `index: name` and exit.
        #[arg(long)]
        list: bool,

        /// Grab one frame and write it (PNG, grayscale) to this path.
        #[arg(long)]
        grab: Option<PathBuf>,

        /// Grab from camera device at this index (needs the `camera` feature).
        #[arg(long)]
        device: Option<u32>,

        /// Grab by re-reading this image file instead of a device (default when
        /// no --device is given).
        #[arg(long)]
        file: Option<String>,
    },
    /// Guided hand-drilling (ORC-7): step through every Excellon hole largest
    /// bit first. Each invocation confirms the previously-prompted hole on a
    /// camera frame (a dark hole within --tol-um of target) before advancing,
    /// and writes an overlay PNG mapping confirmed/current/remaining holes.
    DrillGuide {
        /// Excellon drill file (.drl).
        #[arg(long)]
        drills: PathBuf,

        /// Registered camera frame (PNG/JPEG) to confirm the pending hole on
        /// and to draw the overlay over. Optional on the first invocation.
        #[arg(long)]
        frame: Option<PathBuf>,

        /// Progress state file (created on first run; delete to restart).
        #[arg(long, default_value = "drill-guide-state.txt")]
        state: PathBuf,

        /// Overlay PNG output path.
        #[arg(long, default_value = "drill-guide.png")]
        overlay: PathBuf,

        /// Uniform frame scale, px per mm (pre-VIS-3 contract, as in the
        /// fiducial check).
        #[arg(long)]
        px_per_mm: f64,

        /// Confirmation gate: detected hole center must sit within this many
        /// µm of the target.
        #[arg(long, default_value_t = 150.0)]
        tol_um: f64,

        /// Detector search radius around the target, mm.
        #[arg(long, default_value_t = 1.0)]
        search_mm: f64,

        /// Force-confirm the pending hole without the detector gate — for a
        /// correctly-drilled hole the detector can't confirm (e.g. a merged
        /// slot). Advances past it. Mutually exclusive with --skip.
        #[arg(long, conflicts_with = "skip")]
        accept: bool,

        /// Advance past the pending hole WITHOUT confirming it (leaves it
        /// unverified). Mutually exclusive with --accept.
        #[arg(long)]
        skip: bool,
    },
    /// Export the copper + outline Gerbers a job needs from a KiCad project,
    /// via kicad-cli. Point it at a `.kicad_pcb` (or a project directory with
    /// one) and it writes `copper.gbr` + `outline.gbr` into `--out`.
    Gerbers {
        /// KiCad board (`.kicad_pcb`) or a project directory containing one.
        #[arg(long)]
        project: PathBuf,

        /// Output directory for `copper.gbr` + `outline.gbr`.
        #[arg(long, default_value = ".")]
        out: PathBuf,

        /// Copper layer to export as the conductor.
        #[arg(long, default_value = "F.Cu")]
        copper_layer: String,

        /// Outline layer to export as the board edge.
        #[arg(long, default_value = "Edge.Cuts")]
        outline_layer: String,
    },
    /// Export the Excellon drill files a job needs from a KiCad project, via
    /// kicad-cli — the drill counterpart of `gerbers`. Writes stable names
    /// into `--out`: `pth.drl` (plated) + `npth.drl` (non-plated; a valid
    /// empty file when the board has none), ready for `drill-emit --drills`.
    Drills {
        /// KiCad board (`.kicad_pcb`) or a project directory containing one.
        #[arg(long)]
        project: PathBuf,

        /// Output directory for `pth.drl` + `npth.drl`.
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// Extract pure drill-hole geometry (round holes + G85 slots) from
    /// Excellon drill files — or straight from a KiCad board via kicad-cli —
    /// and emit it as a LightBurn `.lbrn2` job of hole outlines.
    DrillEmit {
        /// Excellon drill file (.drl); repeat for several (KiCad exports PTH
        /// and NPTH holes as two separate files — pass both to get every
        /// hole). Alternative to --board.
        #[arg(long, conflicts_with = "board")]
        drills: Vec<PathBuf>,

        /// KiCad board (`.kicad_pcb`) or a project directory: exports the
        /// drill file(s) via kicad-cli and uses them all (PTH + NPTH).
        #[arg(long)]
        board: Option<PathBuf>,

        /// Output `.lbrn2` path.
        #[arg(long)]
        out: PathBuf,

        /// Board outline Gerber (Edge.Cuts) pinning the workspace frame: the
        /// board outline's corner (not the drill pattern's) lands on the
        /// origin — the same corner `emit` normalizes to — so the drill job
        /// stays co-registered with the copper job from the same board.
        /// Export the Gerber and the drill file with the same origin
        /// convention. Without it the drill pattern's own corner lands on
        /// the origin.
        #[arg(long)]
        outline: Option<PathBuf>,

        /// Hole rendering: "fill" ablates each hole as a filled disc, "line"
        /// traces each hole outline as a vector cut.
        #[arg(long, default_value = "fill")]
        mode: String,

        /// LightBurn device name (must match a configured device).
        #[arg(long, default_value = lbrn2::DEFAULT_DEVICE)]
        device: String,

        // --- process recipe (see docs/lbrn2-schema.md) ---
        /// Max power %.
        #[arg(long, default_value_t = 20.0)]
        power_pct: f64,
        /// Scan speed, mm/s.
        #[arg(long, default_value_t = 1000.0)]
        speed_mm_s: f64,
        /// Frequency, kHz (written to the file in Hz).
        #[arg(long, default_value_t = 30.0)]
        frequency_khz: f64,
        /// MOPA Q-pulse width, ns (a fluence knob; 0 = source default).
        #[arg(long, default_value_t = 1)]
        pulse_ns: u32,
        /// Passes.
        #[arg(long, default_value_t = 1)]
        passes: u32,
        /// Fill line interval, mm (fill mode only).
        #[arg(long, default_value_t = 0.03)]
        interval_mm: f64,

        // --- placement ---
        /// Target x for the job's anchor, mm. The anchor is the board
        /// outline's corner/center when --outline is given, else the drill
        /// pattern's own.
        #[arg(long, default_value_t = 0.0)]
        origin_x: f64,
        /// Target y for the job's anchor, mm. See --origin-x.
        #[arg(long, default_value_t = 0.0)]
        origin_y: f64,
        /// Anchor the bounding-box center rather than the lower-left corner.
        #[arg(long)]
        center: bool,

        /// Laser-field calibration map (see `emit --field-map`); without it
        /// the holes are emitted unwarped (a warning is printed).
        #[arg(long)]
        field_map: Option<PathBuf>,
        /// Maximum physical edge segment before field pre-warping, mm.
        #[arg(long, default_value_t = 0.25)]
        field_seg_mm: f64,
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
        Command::Next {
            design,
            bringup_stubs,
            new_board,
        } => next(&cli.db, design.as_deref(), *bringup_stubs, *new_board),
        Command::Recover {
            board_id,
            retry,
            mark_done,
        } => recover(&cli.db, *board_id, *retry, *mark_done),
        Command::Noncopper {
            copper,
            outline,
            offset_mm,
            clear_nonconductor,
            margin_mm,
            dxf,
            svg,
            preview,
        } => noncopper_cmd(
            copper,
            outline.as_deref(),
            *offset_mm,
            *clear_nonconductor,
            *margin_mm,
            dxf.as_deref(),
            svg.as_deref(),
            preview.as_deref(),
        ),
        Command::Cut {
            board,
            outline,
            thickness_mm,
            out,
            kerf_mm,
            tabs,
            tab_mm,
            mm_per_pass,
            z_step_mm,
            overcut_mm,
            machine,
        } => cut_cmd(CutArgs {
            board: board.as_deref(),
            outline: outline.as_deref(),
            thickness_mm: *thickness_mm,
            out,
            kerf_mm: *kerf_mm,
            tabs: *tabs,
            tab_mm: *tab_mm,
            mm_per_pass: *mm_per_pass,
            z_step_mm: *z_step_mm,
            overcut_mm: *overcut_mm,
            machine,
        }),
        Command::Emit {
            copper,
            outline,
            lbrn2,
            preview,
            offset_mm,
            clear_nonconductor,
            margin_mm,
            device,
            power_pct,
            speed_mm_s,
            frequency_khz,
            pulse_ns,
            passes,
            interval_mm,
            angle_deg,
            angle_step_deg,
            wobble,
            wobble_step_mm,
            wobble_size_mm,
            origin_x,
            origin_y,
            center,
            mirror_x,
            field_map,
            field_seg_mm,
        } => emit_cmd(EmitArgs {
            copper,
            outline: outline.as_deref(),
            lbrn2,
            preview: preview.as_deref(),
            offset_mm: *offset_mm,
            clear_nonconductor: *clear_nonconductor,
            margin_mm: *margin_mm,
            device,
            params: AblationParams {
                power_pct: *power_pct,
                speed_mm_s: *speed_mm_s,
                frequency_khz: *frequency_khz,
                pulse_ns: *pulse_ns,
                passes: *passes,
            },
            interval_mm: *interval_mm,
            angle_deg: *angle_deg,
            angle_step_deg: *angle_step_deg,
            wobble: *wobble,
            wobble_step_mm: *wobble_step_mm,
            wobble_size_mm: *wobble_size_mm,
            origin_x: *origin_x,
            origin_y: *origin_y,
            center: *center,
            mirror_x: *mirror_x,
            field_map: field_map.as_deref(),
            field_seg_mm: *field_seg_mm,
        }),
        Command::Register {
            copper,
            outline,
            lbrn2,
            fiducials,
            frame,
            layout,
            px_per_mm,
            diameter_mm,
            offset_mm,
            clear_nonconductor,
            margin_mm,
            device,
            max_rms_mm,
            power_pct,
            speed_mm_s,
            frequency_khz,
            pulse_ns,
            passes,
            interval_mm,
            wobble,
            wobble_step_mm,
            wobble_size_mm,
            field_map,
            field_seg_mm,
        } => register_cmd(RegisterArgs {
            copper,
            outline: outline.as_deref(),
            lbrn2,
            fiducials: fiducials.as_deref(),
            frame: frame.as_deref(),
            layout: layout.as_deref(),
            px_per_mm: *px_per_mm,
            diameter_mm: *diameter_mm,
            offset_mm: *offset_mm,
            clear_nonconductor: *clear_nonconductor,
            margin_mm: *margin_mm,
            device,
            max_rms_mm: *max_rms_mm,
            params: AblationParams {
                power_pct: *power_pct,
                speed_mm_s: *speed_mm_s,
                frequency_khz: *frequency_khz,
                pulse_ns: *pulse_ns,
                passes: *passes,
            },
            interval_mm: *interval_mm,
            wobble: *wobble,
            wobble_step_mm: *wobble_step_mm,
            wobble_size_mm: *wobble_size_mm,
            field_map: field_map.as_deref(),
            field_seg_mm: *field_seg_mm,
        }),
        Command::CalibGrid {
            out,
            n,
            pitch_mm,
            origin,
            dot_mm,
            device,
        } => calib_grid_cmd(out, *n, *pitch_mm, origin, *dot_mm, device),
        Command::PaperGrid {
            out,
            n,
            pitch_mm,
            dot_mm,
        } => paper_grid_cmd(out, *n, *pitch_mm, *dot_mm),
        Command::FidHoles {
            out,
            layout,
            shape,
            w_mm,
            h_mm,
            device,
            field_map,
            field_seg_mm,
        } => fid_holes_cmd(
            out,
            layout,
            shape,
            *w_mm,
            *h_mm,
            device,
            field_map.as_deref(),
            *field_seg_mm,
        ),
        Command::Cam {
            list,
            grab,
            device,
            file,
        } => cam_cmd(*list, grab.as_deref(), *device, file.as_deref()),
        Command::DrillGuide {
            drills,
            frame,
            state,
            overlay,
            px_per_mm,
            tol_um,
            search_mm,
            accept,
            skip,
        } => {
            let lines = drillguide::step(
                drills,
                frame.as_deref(),
                state,
                overlay,
                *px_per_mm,
                *tol_um,
                *search_mm,
                *accept,
                *skip,
            )?;
            for l in lines {
                println!("{l}");
            }
            Ok(())
        }
        Command::Gerbers {
            project,
            out,
            copper_layer,
            outline_layer,
        } => gerbers_cmd(project, out, copper_layer, outline_layer),
        Command::Drills { project, out } => drills_cmd(project, out),
        Command::DrillEmit {
            drills,
            board,
            out,
            outline,
            mode,
            device,
            power_pct,
            speed_mm_s,
            frequency_khz,
            pulse_ns,
            passes,
            interval_mm,
            origin_x,
            origin_y,
            center,
            field_map,
            field_seg_mm,
        } => drill_emit_cmd(DrillEmitArgs {
            drills,
            board: board.as_deref(),
            out,
            outline: outline.as_deref(),
            mode,
            device,
            params: AblationParams {
                power_pct: *power_pct,
                speed_mm_s: *speed_mm_s,
                frequency_khz: *frequency_khz,
                pulse_ns: *pulse_ns,
                passes: *passes,
            },
            interval_mm: *interval_mm,
            origin_x: *origin_x,
            origin_y: *origin_y,
            center: *center,
            field_map: field_map.as_deref(),
            field_seg_mm: *field_seg_mm,
        }),
    }
}

/// `pcbforge cam` — VIS-1 capture surface over the shared `capture` crate.
/// `pcbforge calib-grid` — emit an n×n grid of dots at commanded coords.
/// `pcbforge gerbers` — point at a KiCad project and export the copper +
/// outline Gerbers the rest of the pipeline consumes.
fn gerbers_cmd(
    project: &std::path::Path,
    out: &std::path::Path,
    copper_layer: &str,
    outline_layer: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use ingest::kicad_cli::{KicadCli, resolve_board};
    let cli = KicadCli::discover()?;
    let board = resolve_board(project)?;
    let (copper, outline) = cli.export_job_gerbers(&board, out, copper_layer, outline_layer)?;
    // Print in a parseable `key: path` form so the console (or a script) can
    // pick the two Gerbers up and feed them to `emit`/`noncopper`.
    println!("board: {}", board.display());
    println!("copper: {}", copper.display());
    println!("outline: {}", outline.display());
    Ok(())
}

/// `pcbforge drills` — point at a KiCad project and export the Excellon drill
/// files under stable names (`pth.drl`, `npth.drl`), the drill counterpart of
/// `gerbers`.
fn drills_cmd(
    project: &std::path::Path,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use ingest::kicad_cli::{KicadCli, resolve_board};
    let cli = KicadCli::discover()?;
    let board = resolve_board(project)?;
    let (pth, npth) = cli.export_job_drills(&board, out)?;
    // Print in a parseable `key: path` form so the console (or a script) can
    // pick the two drill files up and feed them to `drill-emit --drills`.
    println!("board: {}", board.display());
    println!("pth: {}", pth.display());
    println!("npth: {}", npth.display());
    Ok(())
}

/// The calibration burn grid as squares: the n×n commanded lattice plus two
/// off-lattice orientation markers (a lone dot diagonally outside the
/// lower-left corner, and one below the bottom-edge midpoint) that break the
/// lattice's mirror symmetry so the burned grid's orientation is unambiguous.
fn calib_grid_dots(n: usize, pitch_mm: f64, ox: f64, oy: f64, dot_mm: f64) -> Vec<pcb_core::Poly> {
    let mm = |v: f64| (v * NM_PER_MM as f64).round() as Nm;
    let half = mm(dot_mm / 2.0);
    let square = |cx: Nm, cy: Nm| pcb_core::Poly {
        outer: vec![
            pcb_core::P::new(cx - half, cy - half),
            pcb_core::P::new(cx + half, cy - half),
            pcb_core::P::new(cx + half, cy + half),
            pcb_core::P::new(cx - half, cy + half),
        ],
        holes: vec![],
    };

    let mut dots: Vec<pcb_core::Poly> = Vec::with_capacity(n * n + 2);
    for row in 0..n {
        for col in 0..n {
            dots.push(square(
                mm(ox + col as f64 * pitch_mm),
                mm(oy + row as f64 * pitch_mm),
            ));
        }
    }
    // Two off-lattice orientation markers (nearest lattice site is ≥0.5·pitch
    // away, so per-site detector windows never lock onto them): one diagonally
    // outside the lower-left corner, one below the bottom-edge midpoint.
    dots.push(square(mm(ox - pitch_mm * 0.5), mm(oy - pitch_mm * 0.5)));
    dots.push(square(
        mm(ox + (n - 1) as f64 * pitch_mm / 2.0),
        mm(oy - pitch_mm * 0.5),
    ));
    dots
}

fn calib_grid_cmd(
    out: &std::path::Path,
    n: usize,
    pitch_mm: f64,
    origin: &str,
    dot_mm: f64,
    device: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if n < 2 {
        return Err("--n must be at least 2".into());
    }
    if pitch_mm <= 0.0 || dot_mm <= 0.0 {
        return Err("--pitch-mm and --dot-mm must be positive".into());
    }
    let (ox, oy) = origin
        .split_once(',')
        .and_then(|(a, b)| Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?)))
        .ok_or_else(|| format!("--origin must be \"x,y\", got {origin:?}"))?;

    let dots = calib_grid_dots(n, pitch_mm, ox, oy, dot_mm);
    // A modest fill recipe; the operator tunes power for a clean dark dot.
    let params = AblationParams {
        power_pct: 20.0,
        speed_mm_s: 1000.0,
        frequency_khz: 30.0,
        pulse_ns: 1,
        passes: 1,
    };
    let layer = EmitLayer::fill("CAL", params, lbrn2::polys_to_elems(&dots));
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).ok();
    }
    lbrn2::write_lbrn2(device, &[layer], out)?;
    let span = (n - 1) as f64 * pitch_mm;
    eprintln!(
        "calib grid: {n}×{n} dots, {pitch_mm} mm pitch, from ({ox}, {oy}) over {span}×{span} mm"
    );
    // Print the absolute path so it's findable regardless of the working dir.
    let abs = std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf());
    println!("wrote {}", abs.display());
    Ok(())
}

/// Largest printed span (outermost dot-centre to outermost dot-centre) that
/// still fits the printable width once centred on the A4 sheet — chosen so a
/// US-Letter printer (narrower than A4) reproduces it too.
const PAPER_MAX_SPAN_MM: f64 = 190.0;

/// Build a print-ready camera-lens calibration grid as an A4 SVG string
/// (portrait, 1 user unit = 1 mm). Returns the SVG text, or an error when the
/// parameters are invalid or the lattice can't fit the printable width. Pure
/// (no I/O) so the geometry is unit-testable.
fn paper_grid_svg(n: usize, pitch_mm: f64, dot_mm: f64) -> Result<String, String> {
    if n < 2 {
        return Err("--n must be at least 2".into());
    }
    if pitch_mm <= 0.0 || dot_mm <= 0.0 {
        return Err("--pitch-mm and --dot-mm must be positive".into());
    }
    let span = (n - 1) as f64 * pitch_mm;
    if span > PAPER_MAX_SPAN_MM {
        return Err(format!(
            "requested span (n−1)·pitch = {span} mm exceeds the {PAPER_MAX_SPAN_MM} mm printable \
             width — reduce --n or --pitch-mm"
        ));
    }

    // A4 portrait, mm-true. Everything is kept inside a 190×250 mm box centred
    // on the sheet so a US-Letter printer fits it as well.
    const PAGE_W: f64 = 210.0;
    const CX: f64 = PAGE_W / 2.0; // horizontal page centre (105 mm)
    let r = dot_mm / 2.0;

    // Lattice: n×n dots, centred horizontally, block in the upper/middle area.
    let grid_top = 60.0; // first-row dot-centre y
    let x0 = CX - span / 2.0; // leftmost dot-centre x
    let grid_bottom = grid_top + span + r; // lowest painted extent

    // Caliper-check bar: a horizontal line with two vertical end ticks whose
    // CENTRES are exactly 100 mm apart, centred on the page and kept well clear
    // of the lattice below it.
    const CALIPER_MM: f64 = 100.0;
    let tick_lx = CX - CALIPER_MM / 2.0; // 55 mm
    let tick_rx = CX + CALIPER_MM / 2.0; // 155 mm
    let bar_y = grid_bottom + 20.0;
    let tick_h = 4.0; // half-height of each vertical tick

    let mut s = String::new();
    s.push_str(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"210mm\" height=\"297mm\" \
         viewBox=\"0 0 210 297\">\n",
    );
    // White background so the sheet prints clean even in a dark viewer.
    s.push_str("<rect x=\"0\" y=\"0\" width=\"210\" height=\"297\" fill=\"#fff\"/>\n");

    // Header + instructions (small, outside the lattice).
    s.push_str(&format!(
        "<text x=\"{CX}\" y=\"30\" text-anchor=\"middle\" font-family=\"sans-serif\" \
         font-size=\"4.5\" fill=\"#000\">PCBForge camera-lens paper grid</text>\n"
    ));
    s.push_str(&format!(
        "<text x=\"{CX}\" y=\"38\" text-anchor=\"middle\" font-family=\"sans-serif\" \
         font-size=\"3.2\" fill=\"#000\">print at 100% / Actual size — never fit-to-page</text>\n"
    ));
    s.push_str(&format!(
        "<text x=\"{CX}\" y=\"44\" text-anchor=\"middle\" font-family=\"sans-serif\" \
         font-size=\"3.2\" fill=\"#000\">measured pitch = outermost dot-centre span ÷ (n−1) — \
         enter this as step-1 measured pitch</text>\n"
    ));
    s.push_str(&format!(
        "<text x=\"{CX}\" y=\"50\" text-anchor=\"middle\" font-family=\"sans-serif\" \
         font-size=\"3.0\" fill=\"#000\">{n}×{n} dots · nominal pitch {pitch_mm} mm · dot ⌀ \
         {dot_mm} mm</text>\n"
    ));

    // The dot lattice — the ONLY <circle> elements in the document, so a test
    // can count them as n×n.
    s.push_str("<g fill=\"#000\">\n");
    for row in 0..n {
        for col in 0..n {
            let cx = x0 + col as f64 * pitch_mm;
            let cy = grid_top + row as f64 * pitch_mm;
            s.push_str(&format!(
                "<circle cx=\"{cx:.4}\" cy=\"{cy:.4}\" r=\"{r:.4}\"/>\n"
            ));
        }
    }
    s.push_str("</g>\n");

    // Caliper bar: connecting line + two class-tagged vertical end ticks.
    s.push_str(&format!(
        "<line x1=\"{tick_lx:.4}\" y1=\"{bar_y:.4}\" x2=\"{tick_rx:.4}\" y2=\"{bar_y:.4}\" \
         stroke=\"#000\" stroke-width=\"0.3\"/>\n"
    ));
    for tx in [tick_lx, tick_rx] {
        s.push_str(&format!(
            "<line class=\"caliper-tick\" x1=\"{tx:.4}\" y1=\"{:.4}\" x2=\"{tx:.4}\" \
             y2=\"{:.4}\" stroke=\"#000\" stroke-width=\"0.3\"/>\n",
            bar_y - tick_h,
            bar_y + tick_h
        ));
    }
    s.push_str(&format!(
        "<text x=\"{CX}\" y=\"{:.4}\" text-anchor=\"middle\" font-family=\"sans-serif\" \
         font-size=\"3.2\" fill=\"#000\">tick centres = 100.00 mm — verify the print is at true \
         scale</text>\n",
        bar_y + 8.0
    ));

    s.push_str("</svg>\n");
    Ok(s)
}

/// `pcbforge paper-grid` — write a print-ready camera-lens dot grid SVG.
fn paper_grid_cmd(
    out: &std::path::Path,
    n: usize,
    pitch_mm: f64,
    dot_mm: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    let svg = paper_grid_svg(n, pitch_mm, dot_mm)?;
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(out, svg)?;
    let span = (n - 1) as f64 * pitch_mm;
    eprintln!(
        "paper grid: {n}×{n} dots, {pitch_mm} mm nominal pitch, dot ⌀ {dot_mm} mm, \
         outermost span {span} mm — print at 100%"
    );
    // Print the absolute path so it's findable regardless of the working dir.
    let abs = std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf());
    println!("wrote {}", abs.display());
    Ok(())
}

/// Parse `"10,10; 60.5,-10"` into fiducial positions (machine mm). Trims
/// whitespace, skips an empty trailing token, and names the offending token
/// on failure. Mirrors `ui::fiducial::parse_layout`'s contract but is
/// implemented locally so `cli` doesn't depend on `ui`.
fn parse_fid_layout(s: &str) -> Result<Vec<(f64, f64)>, String> {
    let mut out = Vec::new();
    for tok in s.split(';').map(str::trim).filter(|t| !t.is_empty()) {
        let (xs, ys) = tok
            .split_once(',')
            .ok_or_else(|| format!("--layout: bad point {tok:?}, expected \"x,y\""))?;
        let x: f64 = xs
            .trim()
            .parse()
            .map_err(|_| format!("--layout: bad x in {tok:?}"))?;
        let y: f64 = ys
            .trim()
            .parse()
            .map_err(|_| format!("--layout: bad y in {tok:?}"))?;
        out.push((x, y));
    }
    if out.is_empty() {
        return Err("--layout is empty — expected e.g. \"10,10; 60,10; 10,60\"".into());
    }
    Ok(out)
}

/// Minimum segment count for the circle-hole polygon approximation, chosen so
/// even the smallest holes still look round.
const FID_CIRCLE_MIN_SEGMENTS: usize = 16;

/// Max allowed chord (sagitta) error for the circle approximation, mm.
const FID_CIRCLE_CHORD_ERR_MM: f64 = 0.002;

/// Build the fiducial-hole polygons: one ring per position, either an
/// axis-aligned rect (w×h) or a regular-polygon circle approximation
/// (diameter `w_mm`). `shape` must already be validated to "circle" or
/// "rect"; anything else is treated as "circle".
fn fid_holes_polys(
    shape: &str,
    w_mm: f64,
    h_mm: f64,
    positions: &[(f64, f64)],
) -> Vec<pcb_core::Poly> {
    let mm = |v: f64| (v * NM_PER_MM as f64).round() as Nm;

    if shape == "rect" {
        let hw = w_mm / 2.0;
        let hh = h_mm / 2.0;
        positions
            .iter()
            .map(|&(x, y)| pcb_core::Poly {
                outer: vec![
                    pcb_core::P::new(mm(x - hw), mm(y - hh)),
                    pcb_core::P::new(mm(x + hw), mm(y - hh)),
                    pcb_core::P::new(mm(x + hw), mm(y + hh)),
                    pcb_core::P::new(mm(x - hw), mm(y + hh)),
                ],
                holes: vec![],
            })
            .collect()
    } else {
        let r = w_mm / 2.0;
        let ratio = (1.0 - FID_CIRCLE_CHORD_ERR_MM / r).clamp(-1.0, 1.0);
        let n_seg = (std::f64::consts::PI / ratio.acos())
            .ceil()
            .max(FID_CIRCLE_MIN_SEGMENTS as f64) as usize;
        positions
            .iter()
            .map(|&(x, y)| {
                let outer = (0..n_seg)
                    .map(|i| {
                        let theta = 2.0 * std::f64::consts::PI * i as f64 / n_seg as f64;
                        pcb_core::P::new(mm(x + r * theta.cos()), mm(y + r * theta.sin()))
                    })
                    .collect();
                pcb_core::Poly {
                    outer,
                    holes: vec![],
                }
            })
            .collect()
    }
}

/// Apply the laser-field pre-distortion to hole polys, mirroring the `emit`
/// verb: a supplied `--field-map` densifies and pre-warps every edge
/// physical→commanded so the beam lands on the intended geometry; its absence
/// emits the holes unwarped (with a warning). `tag` prefixes the log lines.
fn warp_polys(
    tag: &str,
    polys: Vec<pcb_core::Poly>,
    field_map: Option<&std::path::Path>,
    field_seg_mm: f64,
) -> Result<Vec<pcb_core::Poly>, Box<dyn std::error::Error>> {
    Ok(match field_map {
        Some(path) => {
            let field = load_field_map(path)?;
            validate_field_segment(field_seg_mm)?;
            eprintln!(
                "{tag}: field warp on (fit RMS {:.1} µm, worst {:.1} µm), edges ≤{:.2} mm",
                field.rms_um, field.max_um, field_seg_mm
            );
            cam::register::transform_shapes_field(
                &polys,
                &cam::register::Affine2::identity(),
                field_seg_mm,
                |x, y| field.precompensate(x, y),
            )
        }
        None => {
            eprintln!(
                "{tag}: WARNING — no --field-map: hole geometry is NOT \
                 field-warped; the burned holes will not be corrected for lens \
                 distortion (positional accuracy depends on the machine's own \
                 correction)"
            );
            polys
        }
    })
}

/// `pcbforge fid-holes` — burn fiducial holes at operator-supplied positions.
fn fid_holes_cmd(
    out: &std::path::Path,
    layout: &str,
    shape: &str,
    w_mm: f64,
    h_mm: f64,
    device: &str,
    field_map: Option<&std::path::Path>,
    field_seg_mm: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    if shape != "circle" && shape != "rect" {
        return Err(format!("--shape must be \"circle\" or \"rect\", got {shape:?}").into());
    }
    if w_mm <= 0.0 {
        return Err("--w-mm must be positive".into());
    }
    if shape == "circle" && h_mm != 0.0 && h_mm != w_mm {
        return Err("--h-mm is ignored for --shape circle (circles take only --w-mm); \
                     pass 0 or omit it, or match --w-mm"
            .into());
    }
    let resolved_h = if h_mm == 0.0 { w_mm } else { h_mm };
    if resolved_h <= 0.0 {
        return Err("--h-mm must be positive".into());
    }
    if layout.trim().is_empty() {
        return Err("--layout must not be empty".into());
    }
    let positions = parse_fid_layout(layout)?;

    let polys = fid_holes_polys(shape, w_mm, resolved_h, &positions);
    let polys = warp_polys("fid holes", polys, field_map, field_seg_mm)?;
    let params = AblationParams {
        power_pct: 20.0,
        speed_mm_s: 1000.0,
        frequency_khz: 30.0,
        pulse_ns: 1,
        passes: 1,
    };
    let layer = EmitLayer::fill("FID", params, lbrn2::polys_to_elems(&polys));
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).ok();
    }
    lbrn2::write_lbrn2(device, &[layer], out)?;
    eprintln!(
        "fid holes: {} {shape} hole(s), {w_mm}×{resolved_h} mm",
        positions.len()
    );
    // Print the absolute path so it's findable regardless of the working dir.
    let abs = std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf());
    println!("wrote {}", abs.display());
    Ok(())
}

struct DrillEmitArgs<'a> {
    drills: &'a [PathBuf],
    board: Option<&'a std::path::Path>,
    out: &'a std::path::Path,
    outline: Option<&'a std::path::Path>,
    mode: &'a str,
    device: &'a str,
    params: AblationParams,
    interval_mm: f64,
    origin_x: f64,
    origin_y: f64,
    center: bool,
    field_map: Option<&'a std::path::Path>,
    field_seg_mm: f64,
}

/// Bounding box over every vertex of `polys` (outer rings and holes), nm.
fn polys_bbox(polys: &[pcb_core::Poly]) -> Option<((Nm, Nm), (Nm, Nm))> {
    let mut pts = polys
        .iter()
        .flat_map(|p| p.outer.iter().chain(p.holes.iter().flatten()));
    let first = pts.next()?;
    let (mut min, mut max) = ((first.x, first.y), (first.x, first.y));
    for p in pts {
        min = (min.0.min(p.x), min.1.min(p.y));
        max = (max.0.max(p.x), max.1.max(p.y));
    }
    Some((min, max))
}

/// Rigidly translate every ring of `polys` by `(dx, dy)` nm.
fn translate_polys(polys: &[pcb_core::Poly], dx: Nm, dy: Nm) -> Vec<pcb_core::Poly> {
    let map = |p: &pcb_core::P| pcb_core::P::new(p.x + dx, p.y + dy);
    polys
        .iter()
        .map(|p| pcb_core::Poly {
            outer: p.outer.iter().map(map).collect(),
            holes: p
                .holes
                .iter()
                .map(|h| h.iter().map(map).collect())
                .collect(),
        })
        .collect()
}

/// `pcbforge drill-emit` — pure drill-hole geometry (Excellon, or a KiCad
/// board via kicad-cli) as a LightBurn `.lbrn2` job: one closed outline per
/// round hole (circle) or G85 slot (capsule).
fn drill_emit_cmd(a: DrillEmitArgs) -> Result<(), Box<dyn std::error::Error>> {
    if a.mode != "fill" && a.mode != "line" {
        return Err(format!("--mode must be \"fill\" or \"line\", got {:?}", a.mode).into());
    }
    let files: Vec<PathBuf> = if !a.drills.is_empty() {
        a.drills.to_vec()
    } else if let Some(board) = a.board {
        let kicad = ingest::kicad_cli::KicadCli::discover()
            .map_err(|e| format!("--board needs kicad-cli to export the drill files: {e}"))?;
        let board = ingest::kicad_cli::resolve_board(board)?;
        let tmp = std::env::temp_dir().join(format!("pcbforge-drill-{}", std::process::id()));
        std::fs::create_dir_all(&tmp)?;
        kicad.export_drill(&board, &tmp)?
    } else {
        return Err(
            "supply --drills <file.drl> (repeatable — KiCad splits PTH/NPTH \
             into two files) or --board <board.kicad_pcb>"
                .into(),
        );
    };

    // Every hole from every file, slots kept lossless (KiCad's PTH/NPTH split
    // is two files; both end up here).
    let mut entries: Vec<cam::process::DrillEntry> = Vec::new();
    let (mut holes, mut slots) = (0usize, 0usize);
    for path in &files {
        let ops = ingest::excellon::load_excellon_full(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let start = entries.len();
        for op in &ops {
            entries.push(match *op {
                ingest::excellon::DrillOp::Hole {
                    center,
                    diameter_nm,
                } => {
                    holes += 1;
                    cam::process::DrillEntry {
                        x_nm: center.x,
                        y_nm: center.y,
                        diameter_nm,
                        slot_end: None,
                    }
                }
                ingest::excellon::DrillOp::Slot {
                    a: s,
                    b,
                    diameter_nm,
                } => {
                    slots += 1;
                    cam::process::DrillEntry {
                        x_nm: s.x,
                        y_nm: s.y,
                        diameter_nm,
                        slot_end: Some((b.x, b.y)),
                    }
                }
            });
        }
        eprintln!("drill: {}: {} op(s)", path.display(), entries.len() - start);
    }
    if entries.is_empty() {
        return Err("the drill file(s) contain no holes".into());
    }

    let polys = cam::drill::drill_polys(&entries);
    if polys.is_empty() {
        return Err("the drill file(s) contain no drillable holes (zero diameters?)".into());
    }

    // Frame: with --outline, pin to the board outline's corner (the corner
    // `emit` normalizes to) so both jobs land identically; else normalize to
    // the drill pattern itself. Placement then moves the chosen anchor to
    // --origin-x/-y — a pure translation, like `emit` (no flip: the drill
    // frame is the Gerber frame, y-up but offset negative).
    let target = (mm_to_nm(a.origin_x), mm_to_nm(a.origin_y));
    let polys = match a.outline {
        Some(p) => {
            let region =
                cam::noncopper::board_region_from_outline(&ingest::gerber::load_gerber(p)?.polys);
            let Some((min, max)) = polys_bbox(&region) else {
                return Err(format!("outline {} encloses no area", p.display()).into());
            };
            // Anchor on the board region, not the drill bbox, so placement
            // matches an `emit` of the same board with the same flags.
            let anchor = if a.center {
                (min.0 + (max.0 - min.0) / 2, min.1 + (max.1 - min.1) / 2)
            } else {
                min
            };
            translate_polys(&polys, target.0 - anchor.0, target.1 - anchor.1)
        }
        None => {
            let polys = lbrn2::normalize_frame(&polys);
            if a.origin_x != 0.0 || a.origin_y != 0.0 || a.center {
                lbrn2::place_frame(&polys, target.0, target.1, a.center)
            } else {
                polys
            }
        }
    };
    let polys = warp_polys("drill", polys, a.field_map, a.field_seg_mm)?;

    let elems = lbrn2::polys_to_elems(&polys);
    let layer = if a.mode == "fill" {
        let mut layer = EmitLayer::fill("DRILL", a.params, elems);
        layer.interval_mm = a.interval_mm;
        layer
    } else {
        EmitLayer::line("DRILL", a.params, elems)
    };
    if let Some(dir) = a.out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).ok();
    }
    lbrn2::write_lbrn2(a.device, &[layer], a.out)?;
    eprintln!(
        "drill: {holes} hole(s) + {slots} slot(s) -> {} layer",
        if a.mode == "fill" { "Fill" } else { "Line" }
    );
    let abs = std::path::absolute(a.out).unwrap_or_else(|_| a.out.to_path_buf());
    println!("wrote {}", abs.display());
    Ok(())
}

fn cam_cmd(
    list: bool,
    grab: Option<&std::path::Path>,
    device: Option<u32>,
    file: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if list {
        let devices = capture::list_devices();
        if devices.is_empty() {
            println!(
                "no camera devices (build with `--features camera` for webcam \
                 enumeration, or use `--file <path>`)"
            );
        } else {
            for (idx, name) in devices {
                println!("{idx}: {name}");
            }
        }
        // Fall through to also grab when both are asked for, rather than
        // silently dropping --grab (LR-39).
        if grab.is_none() {
            return Ok(());
        }
    }

    let Some(out) = grab else {
        return Err("nothing to do — pass --list or --grab <out.png>".into());
    };
    let source = match (device, file) {
        (Some(_), Some(_)) => return Err("pass --device OR --file, not both".into()),
        (Some(i), None) => capture::Source::Device(i),
        (None, Some(f)) => capture::Source::File(f.to_string()),
        (None, None) => return Err("supply --device <index> or --file <path> to --grab".into()),
    };
    let frame = capture::grab(&source)?;
    frame
        .save(out)
        .map_err(|e| format!("save {}: {e}", out.display()))?;
    let (w, h) = frame.dimensions();
    println!("grabbed {w}×{h} → {}", out.display());
    Ok(())
}

struct EmitArgs<'a> {
    copper: &'a std::path::Path,
    outline: Option<&'a std::path::Path>,
    lbrn2: &'a std::path::Path,
    preview: Option<&'a std::path::Path>,
    offset_mm: f64,
    clear_nonconductor: bool,
    margin_mm: f64,
    device: &'a str,
    params: AblationParams,
    interval_mm: f64,
    angle_deg: f64,
    angle_step_deg: f64,
    wobble: bool,
    wobble_step_mm: f64,
    wobble_size_mm: f64,
    origin_x: f64,
    origin_y: f64,
    center: bool,
    mirror_x: bool,
    field_map: Option<&'a std::path::Path>,
    field_seg_mm: f64,
}

/// The inverted job geometry plus the inputs a preview needs.
struct BuiltJob {
    board: Vec<pcb_core::Poly>,
    copper: Vec<pcb_core::Poly>,
    /// Non-copper (to-ablate) regions in the original Gerber frame.
    shapes: Vec<pcb_core::Poly>,
}

fn load_field_map(path: &std::path::Path) -> Result<vision::FieldMap, Box<dyn std::error::Error>> {
    let field = vision::FieldMap::parse(&std::fs::read_to_string(path)?)
        .map_err(|e| format!("field map {}: {e}", path.display()))?;
    let coefficients_are_finite = field
        .to_commanded
        .to_coeffs()
        .into_iter()
        .chain(field.to_physical.to_coeffs())
        .all(f64::is_finite);
    if !coefficients_are_finite || !field.rms_um.is_finite() || !field.max_um.is_finite() {
        return Err(format!(
            "field map {} contains non-finite calibration values",
            path.display()
        )
        .into());
    }
    Ok(field)
}

fn validate_field_segment(field_seg_mm: f64) -> Result<(), Box<dyn std::error::Error>> {
    if !field_seg_mm.is_finite() || field_seg_mm <= 0.0 {
        return Err(
            format!("--field-seg-mm must be finite and positive, got {field_seg_mm}").into(),
        );
    }
    Ok(())
}

/// Load copper + board region and invert to the non-copper regions — the shared
/// front half of `emit` and `register`.
fn build_job(
    copper_path: &std::path::Path,
    outline: Option<&std::path::Path>,
    offset_mm: f64,
    clear_nonconductor: bool,
    margin_mm: f64,
) -> Result<BuiltJob, Box<dyn std::error::Error>> {
    if !(0.0..10.0).contains(&offset_mm) {
        return Err(format!("--offset-mm {offset_mm} out of range [0, 10)").into());
    }
    let copper = load_copper(copper_path, clear_nonconductor)?;
    let board = match outline {
        Some(p) => {
            let region =
                cam::noncopper::board_region_from_outline(&ingest::gerber::load_gerber(p)?.polys);
            if region.is_empty() {
                return Err(format!("outline {} encloses no area", p.display()).into());
            }
            region
        }
        None => {
            let margin_nm = (margin_mm * NM_PER_MM as f64).round() as Nm;
            cam::noncopper::board_region_bbox(&copper.polys, margin_nm)
        }
    };
    if board.is_empty() {
        return Err("empty board region (no copper and no outline)".into());
    }
    let offset_nm = (offset_mm * NM_PER_MM as f64).round() as Nm;
    let shapes = cam::noncopper::noncopper(&board, &copper.polys, offset_nm);
    if shapes.is_empty() {
        return Err("inversion produced no shapes (offset too large?)".into());
    }
    Ok(BuiltJob {
        board,
        copper: copper.polys,
        shapes,
    })
}

/// Copper Gerber → non-copper regions → LightBurn Fill layer `.lbrn2`. The
/// FlatCAM-replacement inversion (like `noncopper`) piped straight into a
/// press-play LightBurn file.
fn emit_cmd(a: EmitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let job = build_job(
        a.copper,
        a.outline,
        a.offset_mm,
        a.clear_nonconductor,
        a.margin_mm,
    )?;
    let (mut board, mut shapes) = (job.board, job.shapes);
    let mut copper = job.copper;
    // Back side: mirror the design in X (about x=0; normalize_frame re-corners
    // it below, so the axis constant is immaterial). Winding is preserved.
    if a.mirror_x {
        let axis = cam::flip::MirrorAxis::VerticalX { x_mm: 0.0 };
        board = cam::flip::mirror_job(&board, &axis);
        copper = cam::flip::mirror_job(&copper, &axis);
        shapes = cam::flip::mirror_job(&shapes, &axis);
        eprintln!("mirror: design mirrored in X for the back side");
    }
    // Preview before the workspace transform: board / kept-copper / to-ablate
    // in the original Gerber frame — the geometry relationship the operator
    // eyeballs (placement is a LightBurn concern, handled below for the job).
    if let Some(p) = a.preview {
        cam::export::write_preview_svg(&board, &copper, &shapes, p)?;
        eprintln!("preview: wrote {}", p.display());
    }
    // KiCad plots Gerbers y-up but offset into negative y (sheet position);
    // translate the job's corner to the origin so it lands on LightBurn's
    // workspace. Translation only — a flip would introduce a mirror.
    let shapes = cam::lbrn2::normalize_frame(&shapes);
    // Placement: land the job's corner (or center) on the requested point.
    let shapes = if a.origin_x != 0.0 || a.origin_y != 0.0 || a.center {
        let tx = (a.origin_x * NM_PER_MM as f64).round() as Nm;
        let ty = (a.origin_y * NM_PER_MM as f64).round() as Nm;
        cam::lbrn2::place_frame(&shapes, tx, ty, a.center)
    } else {
        shapes
    };
    let shapes = match a.field_map {
        Some(path) => {
            let field = load_field_map(path)?;
            validate_field_segment(a.field_seg_mm)?;
            eprintln!(
                "emit: field warp on (fit RMS {:.1} µm, worst {:.1} µm), edges ≤{:.2} mm",
                field.rms_um, field.max_um, a.field_seg_mm
            );
            cam::register::transform_shapes_field(
                &shapes,
                &cam::register::Affine2::identity(),
                a.field_seg_mm,
                |x, y| field.precompensate(x, y),
            )
        }
        None => {
            eprintln!(
                "emit: WARNING — no --field-map: geometry is NOT field-warped; \
                 positional accuracy depends on the machine's own correction"
            );
            shapes
        }
    };

    let mut layer = EmitLayer::fill("C00", a.params, cam::lbrn2::polys_to_elems(&shapes));
    layer.interval_mm = a.interval_mm;
    layer.angle_deg = a.angle_deg;
    layer.fill_angle_step_deg = a.angle_step_deg;
    layer.wobble = a.wobble;
    layer.wobble_step_mm = a.wobble_step_mm;
    layer.wobble_size_mm = a.wobble_size_mm;
    cam::lbrn2::write_lbrn2(a.device, &[layer], a.lbrn2)?;
    let rings: usize = shapes.iter().map(|p| 1 + p.holes.len()).sum();
    eprintln!(
        "non-copper: {} shape(s), {} ring(s) -> Fill layer",
        shapes.len(),
        rings
    );
    println!("wrote {}", a.lbrn2.display());
    Ok(())
}

struct RegisterArgs<'a> {
    copper: &'a std::path::Path,
    outline: Option<&'a std::path::Path>,
    lbrn2: &'a std::path::Path,
    fiducials: Option<&'a str>,
    frame: Option<&'a std::path::Path>,
    layout: Option<&'a str>,
    px_per_mm: Option<f64>,
    diameter_mm: f64,
    offset_mm: f64,
    clear_nonconductor: bool,
    margin_mm: f64,
    device: &'a str,
    max_rms_mm: f64,
    params: AblationParams,
    interval_mm: f64,
    wobble: bool,
    wobble_step_mm: f64,
    wobble_size_mm: f64,
    field_map: Option<&'a std::path::Path>,
    field_seg_mm: f64,
}

/// Fiducial-registered emit: fit design→machine affine, bake it into the job
/// geometry, emit at absolute machine coordinates (no origin normalization —
/// the fit *is* the placement).
fn register_cmd(a: RegisterArgs) -> Result<(), Box<dyn std::error::Error>> {
    use nalgebra::Point2;

    // (design_mm, machine_mm) correspondences.
    let pairs: Vec<(Point2<f64>, Point2<f64>)> = match (a.fiducials, a.frame) {
        (Some(_), Some(_)) => return Err("pass --fiducials OR --frame, not both".into()),
        (Some(spec), None) => parse_correspondences(spec)?,
        (None, Some(frame)) => detect_correspondences(
            frame,
            a.layout
                .ok_or("--frame needs --layout (design fiducial positions)")?,
            a.px_per_mm.ok_or("--frame needs --px-per-mm")?,
            a.diameter_mm,
        )?,
        (None, None) => return Err("supply --fiducials or --frame to register".into()),
    };
    if pairs.len() < 3 {
        return Err(format!("need ≥3 fiducial correspondences, got {}", pairs.len()).into());
    }

    let fit = vision::fit_affine(&pairs).map_err(|e| e.to_string())?;
    eprintln!(
        "register: fit {} fiducials, residual RMS {:.1} µm",
        pairs.len(),
        fit.rms * 1000.0
    );
    if fit.rms > a.max_rms_mm {
        return Err(format!(
            "fit RMS {:.1} µm exceeds --max-rms-mm {:.0} µm — bad correspondences or a mis-detection",
            fit.rms * 1000.0,
            a.max_rms_mm * 1000.0
        )
        .into());
    }
    let t = &fit.transform;
    let affine = cam::register::Affine2 {
        m: [
            t[(0, 0)],
            t[(0, 1)],
            t[(0, 2)],
            t[(1, 0)],
            t[(1, 1)],
            t[(1, 2)],
        ],
    };
    if affine.determinant() <= 0.0 {
        return Err(
            "fitted transform reflects (negative determinant) — check fiducial order".into(),
        );
    }

    let job = build_job(
        a.copper,
        a.outline,
        a.offset_mm,
        a.clear_nonconductor,
        a.margin_mm,
    )?;
    // Apply the fit to the design-frame geometry → machine frame. No
    // normalize_frame: registration places the job in absolute machine mm.
    // With a field map, every vertex is pre-distorted physical→commanded so
    // the beam cancels the laser's field distortion; without one the affine
    // placement is emitted unwarped (warned).
    let (placed, warped) = match a.field_map {
        Some(path) => {
            let field = load_field_map(path)?;
            validate_field_segment(a.field_seg_mm)?;
            eprintln!(
                "register: field warp on (fit RMS {:.1} µm, worst {:.1} µm), edges ≤{:.2} mm",
                field.rms_um, field.max_um, a.field_seg_mm
            );
            (
                cam::register::transform_shapes_field(
                    &job.shapes,
                    &affine,
                    a.field_seg_mm,
                    |x, y| field.precompensate(x, y),
                ),
                true,
            )
        }
        None => {
            eprintln!(
                "register: WARNING — no --field-map: geometry is NOT field-warped; \
                 positional accuracy depends on the machine's own correction"
            );
            (cam::register::transform_shapes(&job.shapes, &affine), false)
        }
    };

    let mut layer = EmitLayer::fill("C00", a.params, cam::lbrn2::polys_to_elems(&placed));
    layer.interval_mm = a.interval_mm;
    layer.wobble = a.wobble;
    layer.wobble_step_mm = a.wobble_step_mm;
    layer.wobble_size_mm = a.wobble_size_mm;
    cam::lbrn2::write_lbrn2(a.device, &[layer], a.lbrn2)?;
    let rings: usize = placed.iter().map(|p| 1 + p.holes.len()).sum();
    // Report where the job actually landed (bbox extent + center, machine mm)
    // so the operator can confirm it matches the placement they intended —
    // unlike `emit`, register does NOT normalize to the origin.
    let mm = |v: pcb_core::Nm| v as f64 / NM_PER_MM as f64;
    let mut bb: Option<(pcb_core::Nm, pcb_core::Nm, pcb_core::Nm, pcb_core::Nm)> = None;
    for p in &placed {
        for pt in p.outer.iter().chain(p.holes.iter().flatten()) {
            bb = Some(match bb {
                None => (pt.x, pt.y, pt.x, pt.y),
                Some((x0, y0, x1, y1)) => (x0.min(pt.x), y0.min(pt.y), x1.max(pt.x), y1.max(pt.y)),
            });
        }
    }
    eprintln!(
        "registered: {} shape(s), {rings} ring(s) in {} coordinates",
        placed.len(),
        if warped {
            "field-warped commanded"
        } else {
            "unwarped machine"
        }
    );
    if let Some((x0, y0, x1, y1)) = bb {
        eprintln!(
            "commanded bounds ({:.2}, {:.2})..({:.2}, {:.2}) mm, center ({:.2}, {:.2}) mm",
            mm(x0),
            mm(y0),
            mm(x1),
            mm(y1),
            mm(x0 + x1) / 2.0,
            mm(y0 + y1) / 2.0
        );
    }
    println!("wrote {}", a.lbrn2.display());
    Ok(())
}

/// A (design, machine) fiducial correspondence, both in mm.
type Corr = (nalgebra::Point2<f64>, nalgebra::Point2<f64>);

/// Parse `"dx,dy=tx,ty; …"` into (design, machine) point pairs.
fn parse_correspondences(spec: &str) -> Result<Vec<Corr>, Box<dyn std::error::Error>> {
    use nalgebra::Point2;
    let mut out = Vec::new();
    for (i, entry) in spec
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        let (d, t) = entry
            .split_once('=')
            .ok_or_else(|| format!("fiducial {}: expected `dx,dy=tx,ty`, got {entry:?}", i + 1))?;
        let p = |s: &str| -> Result<Point2<f64>, String> {
            let (x, y) = s
                .trim()
                .split_once(',')
                .ok_or_else(|| format!("fiducial {}: bad point {s:?}", i + 1))?;
            Ok(Point2::new(
                x.trim().parse().map_err(|_| format!("bad x in {s:?}"))?,
                y.trim().parse().map_err(|_| format!("bad y in {s:?}"))?,
            ))
        };
        out.push((p(d)?, p(t)?));
    }
    Ok(out)
}

/// Detect fiducials on a frame and pair each with its design position. Skips
/// misses (the fit tolerates fewer, as long as ≥3 remain).
fn detect_correspondences(
    frame_path: &std::path::Path,
    layout: &str,
    px_per_mm: f64,
    diameter_mm: f64,
) -> Result<Vec<Corr>, Box<dyn std::error::Error>> {
    use nalgebra::Point2;
    if px_per_mm <= 0.0 {
        return Err("--px-per-mm must be positive".into());
    }
    let design: Vec<Point2<f64>> = layout
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let (x, y) = s
                .split_once(',')
                .ok_or_else(|| format!("bad layout point {s:?}"))?;
            Ok::<_, String>(Point2::new(
                x.trim().parse().map_err(|_| format!("bad x in {s:?}"))?,
                y.trim().parse().map_err(|_| format!("bad y in {s:?}"))?,
            ))
        })
        .collect::<Result<_, _>>()?;

    let frame = image::open(frame_path)?.to_luma8();
    // The given px_per_mm is only a SEED to place the search windows; the true
    // scale is measured from the detected fiducial spacing below, so the
    // registration is anchored to the fiducials — not to a guessed number.
    // y-flipped: bed (0,0) = frame bottom-left, bed y up (machine frame) —
    // mapping image rows to bed y directly would make the design→machine fit
    // a reflection, which the negative-determinant gate rejects.
    let bed = vision::BedMap::uniform_scale_y_flip(px_per_mm, frame.height() as f64);
    let profile = vision::FiducialProfile::DarkDot {
        shape: vision::FidShape::Circle { diameter_mm },
    };
    let results = vision::find_fiducials(&frame, &design, 2.0, &profile, &bed);

    // Collect (design mm, detected px) for every hit.
    let mut hits: Vec<(Point2<f64>, (f64, f64))> = Vec::new();
    for (d, res) in design.iter().zip(&results) {
        match res {
            Ok(f) => hits.push((*d, (f.found_px.x, f.found_px.y))),
            Err(m) => eprintln!("register: fiducial at {d:?} not detected ({m:?}) — skipping"),
        }
    }

    // Measured px/mm = mean over detected pairs of (pixel dist / design dist).
    let (mut acc, mut n) = (0.0, 0u32);
    for i in 0..hits.len() {
        for j in (i + 1)..hits.len() {
            let dmm = (hits[i].0 - hits[j].0).norm();
            let dpx =
                ((hits[i].1.0 - hits[j].1.0).powi(2) + (hits[i].1.1 - hits[j].1.1).powi(2)).sqrt();
            if dmm > 1e-6 {
                acc += dpx / dmm;
                n += 1;
            }
        }
    }
    let ppm = if n > 0 { acc / n as f64 } else { px_per_mm };
    eprintln!(
        "register: measured {ppm:.2} px/mm from {} fiducial(s) (seed was {px_per_mm})",
        hits.len()
    );

    // Machine mm = detected px / measured px/mm — with y flipped against the
    // frame height (machine y up, image rows down) — so the target spacing
    // equals the design spacing (unit scale) and the fit is a pure rigid
    // placement, not a reflection.
    let frame_h = frame.height() as f64;
    let pairs = hits
        .into_iter()
        .map(|(d, (px, py))| (d, Point2::new(px / ppm, (frame_h - py) / ppm)))
        .collect();
    Ok(pairs)
}

struct CutArgs<'a> {
    board: Option<&'a std::path::Path>,
    outline: Option<&'a std::path::Path>,
    thickness_mm: Option<f64>,
    out: &'a std::path::Path,
    kerf_mm: Option<f64>,
    tabs: u32,
    tab_mm: f64,
    mm_per_pass: Option<f64>,
    z_step_mm: Option<f64>,
    overcut_mm: f64,
    machine: &'a str,
}

/// The board-outline through-cut pipeline: Edge.Cuts → board region →
/// kerf-compensated tabbed cut geometry + focus schedule → per-step SVG/DXF
/// and a human-readable cut-schedule.txt.
fn cut_cmd(a: CutArgs) -> Result<(), Box<dyn std::error::Error>> {
    let defaults = CutOpts::default();
    let machine = match a.machine.to_ascii_lowercase().as_str() {
        "fiber" => Machine::Fiber,
        "uv" => Machine::Uv,
        other => return Err(format!("unknown --machine '{other}' (want fiber or uv)").into()),
    };
    // kerf / mm-per-pass / z-step are machine facts; unset means un-calibrated.
    let uncalibrated = a.kerf_mm.is_none() || a.mm_per_pass.is_none() || a.z_step_mm.is_none();
    let opts = CutOpts {
        kerf_mm: a.kerf_mm.unwrap_or(defaults.kerf_mm),
        tab_count: a.tabs,
        tab_mm: a.tab_mm,
        mm_per_pass: a.mm_per_pass.unwrap_or(defaults.mm_per_pass),
        z_step_mm: a.z_step_mm.unwrap_or(defaults.z_step_mm),
        overcut_mm: a.overcut_mm,
        machine,
    };

    // Board region + thickness, from a .kicad_pcb (via kicad-cli) or a Gerber.
    let (board_region, thickness_nm) = load_board_and_thickness(&a)?;
    if board_region.is_empty() {
        return Err("outline encloses no board area".into());
    }

    let sched = cam::cut::schedule(&opts, thickness_nm)?;
    let paths = cam::cut::cut_paths(&board_region, &opts);
    if paths.elems.is_empty() {
        return Err("no cut geometry produced (kerf too large for the board?)".into());
    }
    std::fs::create_dir_all(a.out)?;
    // v1: every focus step traces the same geometry; the per-step files exist
    // so the operator runs exactly `passes` of each and the stopping points
    // are files, not counted passes.
    let mut step_files = Vec::new();
    for i in 0..sched.steps.len() {
        let stem = format!("cut-step-{:02}", i + 1);
        let svg = a.out.join(format!("{stem}.svg"));
        let dxf = a.out.join(format!("{stem}.dxf"));
        cam::export::write_paths_svg(&paths, &svg)?;
        cam::export::write_paths_dxf(&paths, "CUT", &dxf)?;
        step_files.push(stem);
    }

    let ring_groups = paths.elems.len();
    let cutouts: Vec<&pcb_core::Ring> = board_region.iter().flat_map(|p| p.holes.iter()).collect();
    let schedule_txt = render_cut_schedule(
        &opts,
        &sched,
        thickness_nm,
        ring_groups,
        &cutouts,
        &step_files,
        uncalibrated,
    );
    let sched_path = a.out.join("cut-schedule.txt");
    std::fs::write(&sched_path, schedule_txt)?;

    println!(
        "wrote {} step file pair(s) + {}",
        sched.steps.len(),
        sched_path.display()
    );
    if uncalibrated {
        eprintln!(
            "pcbforge: WARNING — kerf/mm-per-pass/z-step are placeholder defaults; \
             run the scrap-FR4 ladder and pass measured values before cutting"
        );
    }
    Ok(())
}

/// Resolve the board region (filled outline with cutouts open) and the board
/// thickness in nm from the CLI arguments.
fn load_board_and_thickness(
    a: &CutArgs,
) -> Result<(Vec<pcb_core::Poly>, Nm), Box<dyn std::error::Error>> {
    match (a.outline, a.board) {
        // Both given would silently ignore --board (and its .gbrjob thickness);
        // reject rather than pick one arbitrarily (LR-38).
        (Some(_), Some(_)) => Err("pass only one of --board or --outline, not both".into()),
        (Some(outline), None) => {
            let thickness_mm = a
                .thickness_mm
                .ok_or("--thickness-mm is required with --outline")?;
            let layer = ingest::gerber::load_gerber(outline)?;
            let region = cam::noncopper::board_region_from_outline(&layer.polys);
            Ok((region, mm_to_nm(thickness_mm)))
        }
        (None, Some(board)) => {
            let cli = ingest::kicad_cli::KicadCli::discover()
                .map_err(|e| format!("--board needs kicad-cli to export Edge.Cuts: {e}"))?;
            let tmp = std::env::temp_dir().join(format!("pcbforge-cut-{}", std::process::id()));
            std::fs::create_dir_all(&tmp)?;
            let gerbers = cli.export_gerbers(board, &["Edge.Cuts"], &tmp)?;
            let layer = ingest::gerber::load_gerber(&gerbers[0])?;
            let region = cam::noncopper::board_region_from_outline(&layer.polys);
            // Thickness: CLI override, else a .gbrjob if kicad wrote one.
            let thickness_nm = match a.thickness_mm {
                Some(mm) => mm_to_nm(mm),
                None => find_gbrjob_thickness(&tmp)
                    .ok_or("board thickness unknown: pass --thickness-mm (no .gbrjob found)")?,
            };
            Ok((region, thickness_nm))
        }
        (None, None) => Err("pass one of --board or --outline".into()),
    }
}

fn find_gbrjob_thickness(dir: &std::path::Path) -> Option<Nm> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("gbrjob")
            && let Ok(meta) = ingest::gbrjob::load_gbrjob(&p)
        {
            return Some(meta.thickness_nm);
        }
    }
    None
}

fn mm_to_nm(mm: f64) -> Nm {
    (mm * NM_PER_MM as f64).round() as Nm
}

/// Render the operator-facing cut-schedule.txt.
#[allow(clippy::too_many_arguments)]
fn render_cut_schedule(
    opts: &CutOpts,
    sched: &pcb_core::CutSchedule,
    thickness_nm: Nm,
    ring_groups: usize,
    cutouts: &[&pcb_core::Ring],
    step_files: &[String],
    uncalibrated: bool,
) -> String {
    let mut s = String::new();
    let machine = match opts.machine {
        Machine::Fiber => "fiber",
        Machine::Uv => "UV",
    };
    let thickness_mm = thickness_nm as f64 / NM_PER_MM as f64;
    s.push_str("PCBForge board-outline cut schedule\n");
    s.push_str("===================================\n\n");
    s.push_str(&format!("Machine:        {machine}\n"));
    s.push_str(&format!(
        "Kerf:           {:.3} mm  (centerline offset {:.3} mm onto the waste side)\n",
        opts.kerf_mm,
        opts.kerf_mm / 2.0
    ));
    s.push_str(&format!(
        "Tabs:           {} per ring, {:.2} mm solid each\n",
        opts.tab_count, opts.tab_mm
    ));
    s.push_str(&format!(
        "Depth:          {:.3} mm board + {:.3} mm overcut = {:.3} mm total\n",
        thickness_mm, opts.overcut_mm, sched.total_depth_mm
    ));
    s.push_str(&format!(
        "Per pass:       {:.3} mm removed; focus drops in steps of <= {:.3} mm\n",
        opts.mm_per_pass, opts.z_step_mm
    ));
    s.push_str(&format!(
        "Cut contours:   {ring_groups} segment(s), {} interior cutout(s) cut before the perimeter\n\n",
        cutouts.len()
    ));

    if uncalibrated {
        s.push_str(
            "!! UN-CALIBRATED: kerf / mm-per-pass / z-step are placeholder defaults.\n\
             \x20  Run the scrap-FR4 ladder (docs/plans/cam-10-board-cut.md): burn cut lines\n\
             \x20  at 5/10/15... passes at fixed focus, measure depth for mm-per-pass and the\n\
             \x20  depth where it flattens for the depth of focus, and measure kerf width.\n\
             \x20  Pass the measured values before cutting a real board.\n\n",
        );
    }
    let mut warned = false;
    for hole in cutouts {
        let d = cam::cut::ring_min_dim_mm(hole);
        if d < cam::cut::SLUG_WARN_MM {
            let (cx, cy) = cam::cut::ring_centroid_mm(hole);
            s.push_str(&format!(
                "!! Small cutout ~{d:.1} mm near ({cx:.1}, {cy:.1}) mm: the slug may jam the\n\
                 \x20  kerf — plan to hold or remove it by hand.\n",
            ));
            warned = true;
        }
    }
    if warned {
        s.push('\n');
    }

    s.push_str("Focus steps (run each file's passes, then adjust focus as noted):\n");
    for (i, step) in sched.steps.iter().enumerate() {
        let file = &step_files[i];
        if step.focus_drop_mm > 0.0 {
            s.push_str(&format!(
                "  Step {:02}: {file}  ->  {} pass(es), then LOWER THE HEAD by {:.3} mm\n",
                i + 1,
                step.passes,
                step.focus_drop_mm
            ));
        } else {
            s.push_str(&format!(
                "  Step {:02}: {file}  ->  {} pass(es)  (final step — the cut is through)\n",
                i + 1,
                step.passes
            ));
        }
    }
    s.push_str(
        "\nSequencing: interior cutouts are cut before the perimeter, and this whole\n\
         cut job runs LAST — after all ablation/mask/legend/drill — because a\n\
         through-cut removes registration and rigidity. \"Lower the head\" means move\n\
         the focal plane down into the material by the stated amount (raise the bed\n\
         if your Z is the bed), keeping the lens-to-cut-floor distance at focus.\n",
    );
    s
}

/// Load a copper Gerber for inversion. All copper in the Gerber is kept by
/// default — a no-net (NonConductor) pour is still real copper on the board
/// (the operator's isolated ground pour, mistaken for clearable dead copper
/// on the third live burn — see decisions.md). `clear_nonconductor` opts
/// into rubbing that copper out instead. The choice is reported so a kept or
/// cleared region is explainable from the console.
fn load_copper(
    path: &std::path::Path,
    clear_nonconductor: bool,
) -> Result<pcb_core::Layer, Box<dyn std::error::Error>> {
    let att = ingest::gerber::load_gerber_x2(path)?;
    let dead = att
        .objects()
        .iter()
        .filter(|o| o.aper_function.as_deref() == Some("NonConductor"))
        .count();
    if clear_nonconductor && dead > 0 {
        eprintln!("clearing {dead} NonConductor region(s) (--clear-nonconductor)");
        Ok(att.layer_without_nonconductor())
    } else {
        if dead > 0 {
            eprintln!(
                "keeping {dead} NonConductor region(s) (no-net pour) as copper; \
                 pass --clear-nonconductor to rub them out"
            );
        }
        Ok(att.layer().clone())
    }
}

/// The FlatCAM-replacement pipeline: Gerber → copper polys → board region →
/// inverted fillable shapes → DXF/SVG.
#[allow(clippy::too_many_arguments)]
fn noncopper_cmd(
    copper_path: &std::path::Path,
    outline_path: Option<&std::path::Path>,
    offset_mm: f64,
    clear_nonconductor: bool,
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

    let copper = load_copper(copper_path, clear_nonconductor)?;
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

fn next(
    db_path: &str,
    design: Option<&str>,
    bringup_stubs: bool,
    new_board: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !bringup_stubs {
        return Err(
            "no production executors are configured; pass --bringup-stubs for simulation".into(),
        );
    }
    let db = Db::open(db_path)?;
    let graph = StageGraph::load()?;
    let registry = ExecutorRegistry::bringup_stubs();
    // Pallet id comes from VIS-11 (camera/AprilTag) later; for now the stub
    // source reads PCBFORGE_PALLET_TAG or a fixed default.
    let pallet = EnvPalletSource::default();

    let mut defaults = BoardDefaults::default();
    if let Some(d) = design {
        defaults.design_path = d.to_owned();
    }

    if new_board {
        let board = engine::start_new_board(&db, &graph, &pallet, &defaults)?;
        println!("started board {} at {}", board.id, board.stage);
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
        StepReport::NeedsRecovery {
            board_id,
            stage,
            attempt,
            phase,
        } => {
            println!(
                "board {board_id}: {stage} attempt {attempt} is {phase}; inspect then use `recover`"
            );
        }
    }
    Ok(())
}

fn recover(
    db_path: &str,
    board_id: i64,
    retry: bool,
    mark_done: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let action = match (retry, mark_done) {
        (true, false) => RecoveryAction::Retry,
        (false, true) => RecoveryAction::MarkDone,
        _ => return Err("choose exactly one of --retry or --mark-done".into()),
    };
    let db = Db::open(db_path)?;
    let graph = StageGraph::load()?;
    let board = engine::recover_board(&db, &graph, board_id, action)?;
    println!(
        "board {} recovered at {} ({})",
        board.id, board.stage, board.stage_phase
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the `f64` value of the first `key="..."` attribute in `seg`.
    fn attr(seg: &str, key: &str) -> f64 {
        let start = seg.find(key).expect("attribute present") + key.len();
        let rest = &seg[start..];
        let end = rest.find('"').expect("attribute closes");
        rest[..end].parse().expect("attribute is a number")
    }

    /// The `cx` (or `cy`) of every dot circle, in document order.
    fn circle_axis(svg: &str, axis: &str) -> Vec<f64> {
        svg.match_indices("<circle")
            .map(|(i, _)| attr(&svg[i..], &format!("{axis}=\"")))
            .collect()
    }

    #[test]
    fn paper_grid_has_n_squared_dots() {
        for n in [2usize, 5, 9] {
            let svg = paper_grid_svg(n, 10.0, 2.0).unwrap();
            assert_eq!(
                svg.matches("<circle").count(),
                n * n,
                "n={n} yields n×n dots"
            );
        }
    }

    #[test]
    fn paper_grid_outermost_span_is_n_minus_one_times_pitch() {
        let (n, pitch) = (9usize, 10.0);
        let svg = paper_grid_svg(n, pitch, 2.0).unwrap();
        let expected = (n - 1) as f64 * pitch;
        for axis in ["cx", "cy"] {
            let vals = circle_axis(&svg, axis);
            let min = vals.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            assert!(
                (max - min - expected).abs() < 1e-3,
                "{axis} span {} should equal (n−1)·pitch {expected}",
                max - min
            );
        }
    }

    #[test]
    fn paper_grid_caliper_ticks_are_100mm_apart() {
        let svg = paper_grid_svg(9, 10.0, 2.0).unwrap();
        let xs: Vec<f64> = svg
            .match_indices("caliper-tick")
            .map(|(i, _)| attr(&svg[i..], "x1=\""))
            .collect();
        assert_eq!(xs.len(), 2, "exactly two caliper ticks");
        assert!(
            ((xs[0] - xs[1]).abs() - 100.0).abs() < 1e-6,
            "tick centres {} and {} are 100 mm apart",
            xs[0],
            xs[1]
        );
        // The scale bar advertises the reference length to the operator.
        assert!(svg.contains("tick centres = 100.00 mm"));
    }

    #[test]
    fn paper_grid_rejects_an_oversize_span() {
        // 30×30 at 10 mm → 290 mm span, past the 190 mm printable width.
        let err = paper_grid_svg(30, 10.0, 2.0).unwrap_err();
        assert!(err.contains("exceeds"), "oversize error: {err}");
        // And the basic validation still fires.
        assert!(paper_grid_svg(1, 10.0, 2.0).is_err());
        assert!(paper_grid_svg(9, 0.0, 2.0).is_err());
        assert!(paper_grid_svg(9, 10.0, 0.0).is_err());
    }

    /// Convert mm to nm exactly as `calib_grid_dots` does, for expectation math.
    fn nm(v: f64) -> Nm {
        (v * NM_PER_MM as f64).round() as Nm
    }

    /// The centre of a square Poly = the average of its 4 outer vertices.
    fn poly_center(p: &pcb_core::Poly) -> (Nm, Nm) {
        let sx: i128 = p.outer.iter().map(|v| v.x as i128).sum();
        let sy: i128 = p.outer.iter().map(|v| v.y as i128).sum();
        ((sx / 4) as Nm, (sy / 4) as Nm)
    }

    #[test]
    fn calib_grid_dots_has_n_squared_plus_two_squares() {
        for n in [2usize, 5, 7] {
            let dots = calib_grid_dots(n, 10.0, 0.0, 0.0, 0.4);
            assert_eq!(
                dots.len(),
                n * n + 2,
                "n={n} yields n×n lattice + 2 markers"
            );
            for d in &dots {
                assert_eq!(d.outer.len(), 4, "every dot is a square");
            }
        }
    }

    #[test]
    fn calib_grid_dots_markers_at_specified_off_lattice_positions() {
        for (ox, oy) in [(0.0, 0.0), (-30.0, -30.0)] {
            let (n, pitch, dot) = (7usize, 10.0, 0.4);
            let dots = calib_grid_dots(n, pitch, ox, oy, dot);
            let diag = poly_center(&dots[n * n]);
            let mid = poly_center(&dots[n * n + 1]);

            let diag_exp = (nm(ox - pitch * 0.5), nm(oy - pitch * 0.5));
            let mid_exp = (nm(ox + (n - 1) as f64 * pitch / 2.0), nm(oy - pitch * 0.5));
            assert!(
                (diag.0 - diag_exp.0).abs() <= 1 && (diag.1 - diag_exp.1).abs() <= 1,
                "diagonal marker at {diag:?}, expected {diag_exp:?} (ox={ox}, oy={oy})"
            );
            assert!(
                (mid.0 - mid_exp.0).abs() <= 1 && (mid.1 - mid_exp.1).abs() <= 1,
                "bottom-mid marker at {mid:?}, expected {mid_exp:?} (ox={ox}, oy={oy})"
            );
        }
    }

    #[test]
    fn calib_grid_dots_markers_are_off_lattice() {
        let (n, pitch, ox, oy, dot) = (7usize, 10.0, 0.0, 0.0, 0.4);
        let dots = calib_grid_dots(n, pitch, ox, oy, dot);
        let sites: Vec<(Nm, Nm)> = (0..n)
            .flat_map(|row| {
                (0..n).map(move |col| (nm(ox + col as f64 * pitch), nm(oy + row as f64 * pitch)))
            })
            .collect();
        let half_pitch_nm = (pitch * 0.5 * NM_PER_MM as f64) as f64;

        for marker in [poly_center(&dots[n * n]), poly_center(&dots[n * n + 1])] {
            let nearest = sites
                .iter()
                .map(|s| {
                    let dx = (s.0 - marker.0) as f64;
                    let dy = (s.1 - marker.1) as f64;
                    (dx * dx + dy * dy).sqrt()
                })
                .fold(f64::INFINITY, f64::min);
            assert!(
                !sites.contains(&marker),
                "marker {marker:?} coincides with a lattice site"
            );
            assert!(
                nearest >= half_pitch_nm - 1.0,
                "marker {marker:?} is {nearest} nm from nearest site, expected ≥ {half_pitch_nm}"
            );
        }
    }

    #[test]
    fn fid_holes_polys_rect_has_correct_extents() {
        let positions = [(10.0, 20.0), (-5.0, 60.5)];
        let (w, h) = (2.0, 3.0);
        let polys = fid_holes_polys("rect", w, h, &positions);
        assert_eq!(polys.len(), positions.len());
        for (poly, &(x, y)) in polys.iter().zip(positions.iter()) {
            assert_eq!(poly.outer.len(), 4, "rect hole is a 4-vertex ring");
            let cx = nm(x);
            let cy = nm(y);
            let hw = nm(w / 2.0);
            let hh = nm(h / 2.0);
            let expect = [
                (cx - hw, cy - hh),
                (cx + hw, cy - hh),
                (cx + hw, cy + hh),
                (cx - hw, cy + hh),
            ];
            for (v, e) in poly.outer.iter().zip(expect.iter()) {
                assert!(
                    (v.x - e.0).abs() <= 1 && (v.y - e.1).abs() <= 1,
                    "vertex {v:?}, expected {e:?}"
                );
            }
        }
    }

    #[test]
    fn fid_holes_polys_circle_has_enough_segments_and_correct_radius() {
        let positions = [(0.0, 0.0), (12.3, -4.5)];
        let w = 1.0; // diameter
        let r_nm = nm(w / 2.0) as f64;
        let polys = fid_holes_polys("circle", w, w, &positions);
        assert_eq!(polys.len(), positions.len());
        for (poly, &(x, y)) in polys.iter().zip(positions.iter()) {
            assert!(
                poly.outer.len() >= 16,
                "circle hole has {} vertices, expected >= 16",
                poly.outer.len()
            );
            let cx = nm(x) as f64;
            let cy = nm(y) as f64;
            for v in &poly.outer {
                let dx = v.x as f64 - cx;
                let dy = v.y as f64 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                assert!(
                    (dist - r_nm).abs() <= 1.0,
                    "vertex at radius {dist}, expected {r_nm}"
                );
            }
        }
    }

    /// Bounding-box center of a Poly's outer ring, in mm — robust to the extra
    /// vertices `transform_shapes_field` adds when it densifies edges.
    fn bbox_center_mm(p: &pcb_core::Poly) -> (f64, f64) {
        let (mut minx, mut maxx) = (Nm::MAX, Nm::MIN);
        let (mut miny, mut maxy) = (Nm::MAX, Nm::MIN);
        for v in &p.outer {
            minx = minx.min(v.x);
            maxx = maxx.max(v.x);
            miny = miny.min(v.y);
            maxy = maxy.max(v.y);
        }
        (
            (minx + maxx) as f64 / 2.0 / NM_PER_MM as f64,
            (miny + maxy) as f64 / 2.0 / NM_PER_MM as f64,
        )
    }

    #[test]
    fn fid_holes_field_warp_shifts_centers_by_the_precompensation() {
        use nalgebra::Point2;
        // A pure +1 mm x translation in commanded space lies inside the bicubic
        // fit's span, so precompensate(x, y) = (x + 1, y): every warped vertex
        // (and thus each hole's center) moves +1 mm in x, nothing in y.
        let pairs: Vec<_> = (0..5)
            .flat_map(|row| {
                (0..5).map(move |col| {
                    let phys = Point2::new(col as f64 * 20.0, row as f64 * 20.0);
                    (phys, Point2::new(phys.x + 1.0, phys.y))
                })
            })
            .collect();
        let field = vision::fit_field(&pairs).expect("translation field fits");

        let dir = std::env::temp_dir().join(format!("pcbforge-fidwarp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("field.txt");
        std::fs::write(&path, field.serialize()).unwrap();

        // Hole centers stay inside the fitted 0..80 mm grid (no extrapolation).
        let positions = [(10.0, 20.0), (30.0, 60.0)];
        let raw = fid_holes_polys("rect", 2.0, 3.0, &positions);
        let warped = warp_polys("fid holes", raw.clone(), Some(path.as_path()), 0.25)
            .expect("warp with a valid field map succeeds");

        assert_eq!(warped.len(), positions.len());
        for (w, &(x, y)) in warped.iter().zip(positions.iter()) {
            let (wx, wy) = bbox_center_mm(w);
            assert!(
                (wx - (x + 1.0)).abs() < 1e-3,
                "center x {wx}, want {} (layout x + 1 mm)",
                x + 1.0
            );
            assert!((wy - y).abs() < 1e-3, "center y {wy}, want {y} (unchanged)");
        }
    }

    #[test]
    fn fid_holes_without_field_map_is_unwarped() {
        // No --field-map: the geometry is passed through untouched, so the holes
        // sit exactly at their layout coordinates (0.25 mm segment is inert here).
        let positions = [(10.0, 20.0), (-5.0, 60.5)];
        let polys = fid_holes_polys("rect", 2.0, 3.0, &positions);
        let out = warp_polys("fid holes", polys.clone(), None, 0.25).expect("no field map is fine");
        assert_eq!(out, polys, "without --field-map the geometry is unchanged");
    }

    #[test]
    fn parse_fid_layout_accepts_valid_and_rejects_malformed() {
        let ok = parse_fid_layout("10,10; 60.5,-10").expect("valid layout should parse");
        assert_eq!(ok, vec![(10.0, 10.0), (60.5, -10.0)]);

        assert!(
            parse_fid_layout("10;10").is_err(),
            "missing comma should be rejected"
        );
    }
}
