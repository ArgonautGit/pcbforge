//! The operator console (UI-1): an egui app with a board/stage status panel, an
//! actions panel that **shells the existing `pcbforge` CLI verbs** (the CLI
//! stays the API — the console never re-implements engine logic), a rasterized
//! job-preview panel, a log pane, and a stubbed camera panel (pending VIS-1).
//!
//! The whole UI is egui-only so it computes frames headlessly and is testable
//! without a display; the `eframe` window is a thin feature-gated wrapper
//! (`src/main.rs`, `--features native`).

mod calibration_ui;
mod camera_ui;
mod commands;
mod fiducial_ui;
mod image_ui;
mod job_ui;
mod lightburn_run;
mod placement_ui;
mod projection;
mod settings_io;
mod state;
#[cfg(test)]
mod tests;

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command as StdCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions};
use pcb_core::{NM_PER_MM, Nm};

use crate::fiducial::{self, FidKind, FidRow};
use crate::preview::{self, Layer};
use crate::status::{self, StatusSnapshot};
#[cfg(test)]
use camera_ui::{CAM_VIEW_MAX, downscale_view};
#[cfg(test)]
use commands::spawn_verb;
use commands::{JobShapes, VerbJob};
pub use commands::{job_shapes, preview_image, run_capture};
use job_ui::status_color;
use lightburn_run::{LightburnRun, spawn_lightburn_run};
use projection::CameraProjection;
use state::*;

/// Shared hint for image panels that support pan/zoom navigation.
const NAV_HINT: &str =
    "Navigate: Ctrl+drag to pan · Ctrl+wheel to zoom · Ctrl+double-click to reset.";

/// Current wall-clock as Unix seconds (0 if the clock is before the epoch).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Human "how long ago" for an age in seconds.
fn human_age(secs: u64) -> String {
    match secs {
        0..=89 => "just now".into(),
        90..=5399 => format!("{} min ago", secs / 60),
        5400..=86399 => format!("{} h ago", secs / 3600),
        _ => {
            let days = secs / 86400;
            format!("{days} day{} ago", if days == 1 { "" } else { "s" })
        }
    }
}

/// One line of shelled-command output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub text: String,
    /// stderr / failure line (rendered in a warning color).
    pub err: bool,
}

/// How to invoke the `pcbforge` CLI: `program` + fixed prefix args, before the
/// verb's own args. Defaults to `cargo run -q --bin pcbforge --` so the console
/// works from a repo checkout with nothing on PATH ([`default_cli_cmd`]).
pub fn default_cli_cmd() -> Vec<String> {
    ["cargo", "run", "-q", "--bin", "pcbforge", "--"]
        .into_iter()
        .map(String::from)
        .collect()
}

/// The console application state.
pub struct ConsoleApp {
    /// Path to the orchestra SQLite DB (`--db`).
    pub db_path: PathBuf,
    /// The CLI invocation: program + fixed prefix args (e.g. `cargo run … --`).
    pub cli_cmd: Vec<String>,
    runtime: RuntimeState,
    job: JobState,
    camera: CameraState,
    calibration: CalibrationState,
    fiducials: FiducialState,
    placement: PlacementState,
    ar: ArState,
    views: ViewState,
}

impl ConsoleApp {
    /// New console over `db_path`, invoking the CLI via `cli_cmd` (program +
    /// prefix args; see [`default_cli_cmd`]). Reads an initial status snapshot.
    pub fn new(db_path: impl Into<PathBuf>, cli_cmd: Vec<String>) -> Self {
        let db_path = db_path.into();
        let status = status::snapshot(&db_path);
        let settings_path = crate::settings::path_for_db(&db_path);
        let mut app = Self {
            db_path,
            cli_cmd,
            runtime: RuntimeState {
                settings_path,
                last_settings: String::new(),
                settings_error: None,
                status,
                log: Vec::new(),
                tab: CentralTab::Job,
                verb_job: None,
                pending_lightburn: None,
                lightburn_run: None,
            },
            job: JobState {
                kicad_project: String::new(),
                emit_copper: String::new(),
                emit_outline: String::new(),
                emit_lbrn2: "job.lbrn2".into(),
                offset_mm: 0.0,
                side: Side::Front,
                back_copper: String::new(),
                back_outline: String::new(),
                board_thickness_mm: 1.6,
                focal_mm: 70.0,
                scan_center_auto: true,
                scan_center_mm: (35.0, 35.0),
                speed_mm_s: 1000.0,
                frequency_khz: 30.0,
                pulse_ns: 1,
                interval_mm: 0.03,
                passes: 1,
                preview_tex: None,
                preview_note: "Set a copper Gerber and click “Render preview”.".into(),
            },
            camera: CameraState {
                use_device: false,
                device: 0,
                file: String::new(),
                orientation: Orientation::Normal,
                live: false,
                tex: None,
                note: "Pick a source and press Live. Snapshot feeds the Fiducial/Place tabs.".into(),
                last: None,
                view_scale: 1.0,
                devices: crate::camera::list_devices(),
                show_bed: true,
                field_mm: 70.0,
                field_center_auto: true,
                field_cx_mm: 35.0,
                field_cy_mm: 35.0,
                capture: None,
                capture_src: None,
            },
            calibration: CalibrationState {
                anchor: None,
                saved_at: None,
                paper: GridParams {
                    n: 7,
                    pitch_mm: 10.0,
                    dot_mm: 0.4,
                    dot_kind: crate::calib::DotKind::Dark,
                },
                burn: GridParams {
                    n: 7,
                    pitch_mm: 10.0,
                    dot_mm: 0.4,
                    dot_kind: crate::calib::DotKind::Dark,
                },
                grid_origin_mm: (0.0, 0.0),
                grid_out: "calib-grid.lbrn2".into(),
                paper_out: "paper-grid.svg".into(),
                frame: String::new(),
                frame_img: None,
                frame_tex: None,
                corners: Vec::new(),
                mode: CalibMode::CameraLens,
                lens: None,
                lens_frame_signature: None,
                field: None,
                field_accepted: false,
                accept_rms_um: 100.0,
                accept_worst_um: 250.0,
                allow_machine_scale: false,
                lens_arrow_scale: 20.0,
                anchor_resid_scale: 30.0,
                edit_anchor_dots: false,
                show_fit_feedback: true,
                live: false,
                capture: None,
                capture_src: None,
                note: "Generate a grid, burn it, image it, click the 4 corner dots (LL, LR, UR, UL), then Fit.".into(),
            },
            fiducials: FiducialState {
                frame: String::new(),
                layout: "10,10; 60,10; 10,60; 60,60".into(),
                px_per_mm: 10.0,
                shape: crate::fiducial::ShapeKind::Circle,
                diameter_mm: 1.0,
                height_mm: 1.0,
                search_mm: 2.0,
                profile: crate::fiducial::ProfileKind::DarkDot,
                out: "fid-holes.lbrn2".into(),
                board_w_mm: 70.0,
                board_h_mm: 50.0,
                margin_mm: 5.0,
                click_place: false,
                note: "Load a frame, drag each marker near its hole, then Check.".into(),
                rows: Vec::new(),
                measured_ppm: None,
                frame_img: None,
                frame_tex: None,
                search: Vec::new(),
                found: Vec::new(),
                drag: None,
                marking: None,
                last_placed: false,
                homography: None,
                pose: None,
                live: false,
                capture: None,
                capture_src: None,
            },
            placement: PlacementState {
                frame: String::new(),
                lbrn2: "placed.lbrn2".into(),
                px_per_mm: 10.0,
                tx_mm: 0.0,
                ty_mm: 0.0,
                rot_deg: 0.0,
                auto_pose: false,
                job: Vec::new(),
                frame_img: None,
                base_rgba: None,
                pivot: (0.0, 0.0),
                tex: None,
                note: "Load a frame + job, then drag / rotate to place it on the board.".into(),
                field_correct: false,
                lightburn_device: cam::lbrn2::DEFAULT_DEVICE.to_string(),
            },
            ar: ArState {
                overlay: false,
                show_board: false,
                show_copper: true,
                show_ablate: true,
                board: Vec::new(),
                copper: Vec::new(),
                ablate: Vec::new(),
                note: "Load the Job-tab Gerbers, detect fiducials, then AR overlays the registered design on the feed.".into(),
            },
            views: ViewState {
                images: std::collections::HashMap::new(),
            },
        };
        app.load_settings();
        app.runtime.last_settings = app.settings_blob();
        app
    }

    /// Draw one frame. Kept separate from the `eframe::App` impl so it runs
    /// under a bare `egui::Context` in tests.
    pub fn ui(&mut self, ctx: &Context) {
        self.pump_verb(ctx);
        self.pump_lightburn(ctx);
        // Pump the live-capture loops regardless of the visible tab, or a
        // tab-switch during ● Live leaves the device held with no consumer and
        // stop-requests unexecuted until the tab is revisited (LR-45). Each is
        // a cheap no-op (and drops its capture) when its Live toggle is off.
        self.pump_calib_live(ctx);
        self.pump_fid_live(ctx);
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("PCBForge console");
                ui.separator();
                if ui.button("⟳ Refresh").clicked() {
                    self.refresh();
                }
                ui.separator();
                ui.label("DB:");
                ui.monospace(self.db_path.display().to_string());
                if self.runtime.verb_job.is_some() {
                    ui.separator();
                    ui.spinner();
                    ui.label("running…");
                }
            });
        });

        egui::SidePanel::left("status")
            .resizable(true)
            .default_width(260.0)
            .show(ctx, |ui| self.status_panel(ui));

        egui::SidePanel::right("actions")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| self.actions_panel(ui));

        egui::TopBottomPanel::bottom("log")
            .resizable(true)
            .default_height(160.0)
            .show(ctx, |ui| self.log_panel(ui));

        egui::CentralPanel::default().show(ctx, |ui| self.preview_panel(ui));

        // Persist the input fields after the frame's edits (no-op unless one
        // actually changed), so the Gerber paths survive a restart.
        self.save_settings_if_changed();
    }

    /// A concise snapshot of the debuggable app state, for the headless
    /// `debug_driver`'s `state` command (see AGENT_DEBUGGING.md). Curated rather
    /// than a full `Debug` — textures and channels aren't useful to print.
    pub fn debug_summary(&self) -> String {
        let calib = match &self.calibration.anchor {
            Some(c) if c.found == 0 => {
                let age = match self.calibration.saved_at {
                    Some(t) => human_age(now_unix().saturating_sub(t)),
                    None => "age unknown".into(),
                };
                format!("saved ({age}), unconfirmed")
            }
            Some(c) => format!(
                "this session, {}/{} dots, RMS {:.0}µm",
                c.found, c.total, c.rms_um
            ),
            None => "none".into(),
        };
        let lens = match &self.calibration.lens {
            Some(c) => format!("{} dots, RMS {:.0}µm", c.found, c.lens.rms_um),
            None => "none".into(),
        };
        let field = match &self.calibration.field {
            Some(c) => format!(
                "{} dots, RMS/worst {:.0}/{:.0}µm, verdict={}, scale={:+.1}%, mirrored={}, extrapolated={}, {}",
                c.found,
                c.field.rms_um,
                c.field.max_um,
                field_verdict_token(&c.field_verdict),
                (c.scale - 1.0) * 100.0,
                if c.paper_to_machine.flip_x {
                    "yes"
                } else {
                    "no"
                },
                c.extrapolated,
                if self.calibration.field_accepted {
                    "accepted"
                } else {
                    "rejected"
                }
            ),
            None => "none".into(),
        };
        let projection = match self.camera.last.as_ref().map(|f| f.dimensions()) {
            Some(dimensions) => match self.camera_projection(dimensions) {
                Ok(Some(CameraProjection::CommandedField { .. })) => "field-warped-commanded",
                Ok(Some(CameraProjection::PhysicalLens { .. })) => "physical",
                Ok(Some(CameraProjection::Homography { .. })) => "homography",
                Ok(None) => "none",
                Err(_) => "invalid",
            },
            None => "no-frame",
        };
        let cam = match &self.camera.last {
            Some(g) => format!(
                "{}×{} (view ×{:.3})",
                g.width(),
                g.height(),
                self.camera.view_scale
            ),
            None => "none".into(),
        };
        let calib_frame = match &self.calibration.frame_img {
            Some(g) => format!("{}×{}", g.width(), g.height()),
            None => "none".into(),
        };
        let base = |s: &str| {
            std::path::Path::new(s)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unset".into())
        };
        let gerbers = format!(
            "copper={} outline={} speed={} freq_khz={} pulse_ns={} interval={} passes={}",
            if self.job.emit_copper.trim().is_empty() {
                "unset".into()
            } else {
                base(&self.job.emit_copper)
            },
            if self.job.emit_outline.trim().is_empty() {
                "unset".into()
            } else {
                base(&self.job.emit_outline)
            },
            self.job.speed_mm_s,
            self.job.frequency_khz,
            self.job.pulse_ns,
            self.job.interval_mm,
            self.job.passes,
        );
        // ④ auto fiducial-hole layout, resolved against the effective field
        // centre (kept in sync with the auto toggle by `sync_auto_field_center`).
        let fid_layout = fiducial::format_layout(&fiducial::board_fid_layout(
            self.camera.field_cx_mm as f64,
            self.camera.field_cy_mm as f64,
            self.fiducials.board_w_mm,
            self.fiducials.board_h_mm,
            self.fiducials.margin_mm,
        ));
        let fid_pose = match &self.fiducials.pose {
            Some(p) => format!(
                "rot={:+.2} tx={:.2} ty={:.2} rms={:.3} flipped={} used={}",
                p.rot_deg, p.tx_mm, p.ty_mm, p.rms_mm, p.flipped, p.used
            ),
            None => "none".into(),
        };
        format!(
            "tab={:?} side={:?} calib_mode={:?}\n\
             gerbers: {gerbers}\n\
             calib_anchor: {calib}\n\
             camera_lens: {lens}\n\
             laser_field: {field} field_correct={} scale_comp={} limits={:.0}/{:.0}µm\n\
             camera_projection: {projection}\n\
             camera_frame: {cam}\n\
             calib_frame: {calib_frame}\n\
             bed_overlay: show={} field={:.0}mm center=({:.1},{:.1}) auto={}\n\
             place: x={:.2} y={:.2} rot={:.1}° auto_pose={} frame={} lightburn={} device={} note={:?}\n\
             calib_paper: n={} pitch={:.2}mm dot={:.2}mm contrast={} out={}\n\
             calib_burn: n={} pitch={:.2}mm dot={:.2}mm contrast={} corners_marked={} edit_anchor_dots={} feedback={} origin=({:.1},{:.1})\n\
             fiducials: {} markers shape={} w={} h={} profile={} search={} out={} marking={}\n\
             fid_board: w={} h={} margin={} layout={}\n\
             fid_pose: {}\n\
             settings: {}",
            self.runtime.tab,
            self.job.side,
            self.calibration.mode,
            self.placement.field_correct,
            if self.calibration.allow_machine_scale {
                "on"
            } else {
                "off"
            },
            self.calibration.accept_rms_um,
            self.calibration.accept_worst_um,
            self.camera.show_bed,
            self.camera.field_mm,
            self.camera.field_cx_mm,
            self.camera.field_cy_mm,
            self.camera.field_center_auto,
            self.placement.tx_mm,
            self.placement.ty_mm,
            self.placement.rot_deg,
            self.placement.auto_pose,
            match (&self.placement.frame_img, &self.placement.tex) {
                (Some(f), Some(_)) => format!("{}x{}", f.width(), f.height()),
                (Some(_), None) => "loaded-no-tex".into(),
                (None, _) => "none".into(),
            },
            self.lightburn_token(),
            self.placement.lightburn_device,
            self.placement.note,
            self.calibration.paper.n,
            self.calibration.paper.pitch_mm,
            self.calibration.paper.dot_mm,
            self.calibration.paper.dot_kind.label(),
            base(&self.calibration.paper_out),
            self.calibration.burn.n,
            self.calibration.burn.pitch_mm,
            self.calibration.burn.dot_mm,
            self.calibration.burn.dot_kind.label(),
            self.calibration.corners.len(),
            self.calibration.edit_anchor_dots,
            if self.calibration.show_fit_feedback {
                "on"
            } else {
                "off"
            },
            self.calibration.grid_origin_mm.0,
            self.calibration.grid_origin_mm.1,
            self.fiducials.search.len(),
            self.fiducials.shape.token(),
            self.fiducials.diameter_mm,
            self.fiducials.height_mm,
            self.fiducials.profile.token(),
            self.fiducials.search_mm,
            base(&self.fiducials.out),
            match self.fiducials.marking {
                Some(k) => k.to_string(),
                None => "-".to_string(),
            },
            self.fiducials.board_w_mm,
            self.fiducials.board_h_mm,
            self.fiducials.margin_mm,
            fid_layout,
            fid_pose,
            self.runtime.settings_error.as_deref().unwrap_or("saved"),
        )
    }
}

/// Operator-facing phrase for a laser-field pincushion-vs-noise verdict.
/// `scale` is the measured/commanded uniform scale from the fit (see
/// `FieldCal::scale`); a notable deviation is appended so the operator sees
/// the actual percentage, not just "uniform scale error".
fn field_verdict_phrase(v: &vision::FieldVerdict, scale: f64) -> String {
    use vision::{FieldPattern, InconclusiveReason};
    let phrase = match v.pattern {
        FieldPattern::Systematic { pincushion } => format!(
            "{} detected ({:.1}× noise floor, {:.0} µm signal vs {:.0} µm scatter) — \
             correction should help",
            if pincushion { "pincushion" } else { "barrel" },
            v.ratio,
            v.systematic_um,
            v.noise_um
        ),
        FieldPattern::NonRadial => format!(
            "systematic error, but non-radial — looks like a rotation/misalignment, not lens \
             curvature ({:.1}× noise floor, {:.0} µm tangential vs {:.0} µm scatter). Correction \
             still fixes it; also check the galvo/camera alignment",
            v.ratio, v.tangential_um, v.noise_um
        ),
        FieldPattern::UniformScale => format!(
            "looks like a uniform scale error, not curvature ({:.0} µm signal vs {:.0} µm \
             scatter) — a LightBurn/EZCAD origin & scale recal, or a mis-scaled reference/print, \
             is the likelier root cause; correction still works",
            v.systematic_um, v.noise_um
        ),
        FieldPattern::Borderline => format!(
            "borderline — {:.0} µm signal vs {:.0} µm scatter isn't conclusive yet — burn a \
             wider/denser grid before trusting this correction",
            v.systematic_um, v.noise_um
        ),
        FieldPattern::Noise => format!(
            "no systematic pattern above the noise floor ({:.0} µm signal vs {:.0} µm scatter) \
             — the field is likely already tight; don't enable correction here (it would just \
             fit noise)",
            v.systematic_um, v.noise_um
        ),
        FieldPattern::Inconclusive(reason) => format!(
            "can't tell pattern from noise yet ({}) — burn a wider/denser grid",
            match reason {
                InconclusiveReason::TooFewDots => "too few dots",
                InconclusiveReason::TooFewOffCenter => "too few dots away from the field center",
                InconclusiveReason::SpanTooSmall => "dots too clustered",
                InconclusiveReason::SpanTooThin => "dots too nearly collinear",
                InconclusiveReason::NonFinite => "bad (non-finite) sample data",
            }
        ),
    };
    if (scale - 1.0).abs() > crate::calib::FIELD_SCALE_NOTE_FRAC {
        format!(
            "{phrase}; burn reads {:.1}% {} than commanded — check the machine's field-size \
             setting",
            (scale - 1.0).abs() * 100.0,
            if scale > 1.0 { "larger" } else { "smaller" }
        )
    } else {
        phrase
    }
}

/// Short machine-greppable token for `debug_summary()` / headless tests.
fn field_verdict_token(v: &vision::FieldVerdict) -> String {
    use vision::{FieldPattern, InconclusiveReason};
    match v.pattern {
        FieldPattern::Systematic { pincushion: true } => {
            format!("pincushion(ratio={:.1})", v.ratio)
        }
        FieldPattern::Systematic { pincushion: false } => format!("barrel(ratio={:.1})", v.ratio),
        FieldPattern::NonRadial => format!("non_radial(ratio={:.1})", v.ratio),
        FieldPattern::UniformScale => format!("uniform_scale(ratio={:.1})", v.ratio),
        FieldPattern::Borderline => format!("borderline(ratio={:.1})", v.ratio),
        FieldPattern::Noise => format!("noise(ratio={:.1})", v.ratio),
        FieldPattern::Inconclusive(reason) => format!(
            "inconclusive({})",
            match reason {
                InconclusiveReason::TooFewDots => "too_few_dots",
                InconclusiveReason::TooFewOffCenter => "too_few_offcenter",
                InconclusiveReason::SpanTooSmall => "span_too_small",
                InconclusiveReason::SpanTooThin => "span_too_thin",
                InconclusiveReason::NonFinite => "non_finite",
            }
        ),
    }
}

fn short_path(p: &str) -> String {
    std::path::Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

#[cfg(feature = "native")]
impl eframe::App for ConsoleApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }
}
