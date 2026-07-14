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
            origin_x,
            origin_y,
            center,
            mirror_x,
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
            origin_x: *origin_x,
            origin_y: *origin_y,
            center: *center,
            mirror_x: *mirror_x,
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
        }),
        Command::Cam {
            list,
            grab,
            device,
            file,
        } => cam_cmd(*list, grab.as_deref(), *device, file.as_deref()),
    }
}

/// `pcbforge cam` — VIS-1 capture surface over the shared `capture` crate.
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
        return Ok(());
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
    origin_x: f64,
    origin_y: f64,
    center: bool,
    mirror_x: bool,
}

/// The inverted job geometry plus the inputs a preview needs.
struct BuiltJob {
    board: Vec<pcb_core::Poly>,
    copper: Vec<pcb_core::Poly>,
    /// Non-copper (to-ablate) regions in the original Gerber frame.
    shapes: Vec<pcb_core::Poly>,
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

    let mut layer = EmitLayer::fill("C00", a.params, cam::lbrn2::polys_to_elems(&shapes));
    layer.interval_mm = a.interval_mm;
    layer.angle_deg = a.angle_deg;
    layer.fill_angle_step_deg = a.angle_step_deg;
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
    let placed = cam::register::transform_shapes(&job.shapes, &affine);

    // Same default recipe as `emit`; the operator tunes it in LightBurn or
    // re-runs with a richer flag set later.
    let params = AblationParams {
        power_pct: 20.0,
        speed_mm_s: 1000.0,
        frequency_khz: 30.0,
        pulse_ns: 1,
        passes: 1,
    };
    let layer = EmitLayer::fill("C00", params, cam::lbrn2::polys_to_elems(&placed));
    cam::lbrn2::write_lbrn2(a.device, &[layer], a.lbrn2)?;
    let rings: usize = placed.iter().map(|p| 1 + p.holes.len()).sum();
    eprintln!(
        "registered: {} shape(s), {rings} ring(s) at machine coordinates",
        placed.len()
    );
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
    let bed = vision::BedMap::uniform_scale(px_per_mm);
    let profile = vision::FiducialProfile::DarkDot { diameter_mm };
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

    // Machine mm = detected px / measured px/mm, so the target spacing equals
    // the design spacing (unit scale) and the fit is a pure rigid placement.
    let pairs = hits
        .into_iter()
        .map(|(d, (px, py))| (d, Point2::new(px / ppm, py / ppm)))
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

    let paths = cam::cut::cut_paths(&board_region, &opts);
    if paths.elems.is_empty() {
        return Err("no cut geometry produced (kerf too large for the board?)".into());
    }
    let sched = cam::cut::schedule(&opts, thickness_nm);

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
        (Some(outline), _) => {
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
