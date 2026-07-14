//! The operator console (UI-1): an egui app with a board/stage status panel, an
//! actions panel that **shells the existing `pcbforge` CLI verbs** (the CLI
//! stays the API — the console never re-implements engine logic), a rasterized
//! job-preview panel, a log pane, and a stubbed camera panel (pending VIS-1).
//!
//! The whole UI is egui-only so it computes frames headlessly and is testable
//! without a display; the `eframe` window is a thin feature-gated wrapper
//! (`src/main.rs`, `--features native`).

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

/// One line of shelled-command output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub text: String,
    /// stderr / failure line (rendered in a warning color).
    pub err: bool,
}

/// Which view the central panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CentralTab {
    Job,
    Camera,
    Fiducials,
    Place,
}

/// Which face of a (possibly double-sided) board the operator is working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Front,
    Back,
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
    status: StatusSnapshot,
    log: Vec<LogLine>,
    tab: CentralTab,
    verb_job: Option<VerbJob>,

    // emit form
    emit_copper: String,
    emit_outline: String,
    emit_lbrn2: String,
    offset_mm: f64,

    // double-sided (ORC-6): back-side gerbers + flip/beam-offset params. When
    // `side` is Back, the job is mirrored in X and the fiducial expectations
    // carry the f-theta entry→exit parallax.
    side: Side,
    back_copper: String,
    back_outline: String,
    board_thickness_mm: f64,
    focal_mm: f64,
    /// Derive the scan center from the fiducial-layout centroid (default) —
    /// the un-calibrated assumption pending VIS-3.
    scan_center_auto: bool,
    /// Explicit scan/field center in design mm, used when `scan_center_auto`
    /// is off (the operator measures where the lens axis actually is).
    scan_center_mm: (f64, f64),

    // job preview
    preview_tex: Option<TextureHandle>,
    preview_note: String,

    // fiducial check
    fid_frame: String,
    fid_layout: String,
    fid_px_per_mm: f64,
    fid_diameter_mm: f64,
    fid_search_mm: f64,
    /// Which fiducial appearance the detector matches (FLD-12).
    fid_profile: crate::fiducial::ProfileKind,
    /// Click-to-place mode: a click on empty frame appends an expected
    /// fiducial there (FLD-12), instead of only dragging existing markers.
    fid_click_place: bool,
    fid_note: String,
    fid_rows: Vec<FidRow>,
    fid_measured_ppm: Option<f64>,
    // draggable search markers over the live frame
    fid_frame_img: Option<image::GrayImage>,
    fid_frame_tex: Option<TextureHandle>,
    fid_search: Vec<(f64, f64)>,
    fid_found: Vec<Option<(f64, f64)>>,
    fid_drag: Option<usize>,
    /// design/bed-mm → pixel perspective fit (≥4 fiducials); shared with Place.
    fid_homography: Option<vision::Homography>,
    // live fiducial tracking (FLD-11): pull from the camera source, re-detect
    // each frame.
    fid_live: bool,
    fid_capture: Option<crate::camera::Capture>,
    fid_capture_src: Option<crate::camera::Source>,

    // drag-to-place
    place_frame: String,
    place_px_per_mm: f64,
    place_tx_mm: f64,
    place_ty_mm: f64,
    place_rot_deg: f64,
    place_job: Vec<pcb_core::Poly>,
    place_frame_img: Option<image::GrayImage>,
    place_pivot: (f64, f64),
    place_tex: Option<TextureHandle>,
    place_note: String,

    // live camera
    cam_use_device: bool,
    cam_device: u32,
    cam_file: String,
    cam_live: bool,
    cam_tex: Option<TextureHandle>,
    cam_note: String,
    cam_last: Option<image::GrayImage>,
    cam_devices: Vec<(u32, String)>,
    cam_capture: Option<crate::camera::Capture>,
    cam_capture_src: Option<crate::camera::Source>,

    // AR overlay (UI-2): project the registered design over the camera frame
    // through the fiducial homography, with per-layer toggles.
    ar_overlay: bool,
    ar_show_board: bool,
    ar_show_copper: bool,
    ar_show_ablate: bool,
    ar_board: Vec<pcb_core::Poly>,
    ar_copper: Vec<pcb_core::Poly>,
    ar_ablate: Vec<pcb_core::Poly>,
    ar_note: String,
}

impl ConsoleApp {
    /// New console over `db_path`, invoking the CLI via `cli_cmd` (program +
    /// prefix args; see [`default_cli_cmd`]). Reads an initial status snapshot.
    pub fn new(db_path: impl Into<PathBuf>, cli_cmd: Vec<String>) -> Self {
        let db_path = db_path.into();
        let status = status::snapshot(&db_path);
        Self {
            db_path,
            cli_cmd,
            status,
            log: Vec::new(),
            tab: CentralTab::Job,
            verb_job: None,
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
            preview_tex: None,
            preview_note: "Set a copper Gerber and click “Render preview”.".into(),
            fid_frame: String::new(),
            // Four fiducials (a rectangle) so a perspective homography is
            // determinable — 3 can only fix an affine. The 4th at (60,60)
            // completes the operator's L into a square.
            fid_layout: "10,10; 60,10; 10,60; 60,60".into(),
            fid_px_per_mm: 10.0,
            fid_diameter_mm: 1.0,
            fid_search_mm: 2.0,
            fid_profile: crate::fiducial::ProfileKind::DarkDot,
            fid_click_place: false,
            fid_note: "Load a frame, drag each marker near its hole, then Check.".into(),
            fid_rows: Vec::new(),
            fid_measured_ppm: None,
            fid_frame_img: None,
            fid_frame_tex: None,
            fid_search: Vec::new(),
            fid_found: Vec::new(),
            fid_drag: None,
            fid_homography: None,
            fid_live: false,
            fid_capture: None,
            fid_capture_src: None,
            place_frame: String::new(),
            place_px_per_mm: 10.0,
            place_tx_mm: 0.0,
            place_ty_mm: 0.0,
            place_rot_deg: 0.0,
            place_job: Vec::new(),
            place_frame_img: None,
            place_pivot: (0.0, 0.0),
            place_tex: None,
            place_note: "Load a frame + job, then drag / rotate to place it on the board.".into(),
            cam_use_device: false,
            cam_device: 0,
            cam_file: String::new(),
            cam_live: false,
            cam_tex: None,
            cam_note: "Pick a source and press Live. Snapshot feeds the Fiducial/Place tabs."
                .into(),
            cam_last: None,
            cam_devices: crate::camera::list_devices(),
            cam_capture: None,
            cam_capture_src: None,
            ar_overlay: false,
            ar_show_board: false,
            ar_show_copper: true,
            ar_show_ablate: true,
            ar_board: Vec::new(),
            ar_copper: Vec::new(),
            ar_ablate: Vec::new(),
            ar_note: "Load the Job-tab Gerbers, detect fiducials, then AR overlays the registered design on the feed.".into(),
        }
    }

    /// Re-read the status snapshot from the DB.
    pub fn refresh(&mut self) {
        self.status = status::snapshot(&self.db_path);
    }

    /// Start `pcbforge <args>` on a background thread; its output streams into
    /// the log via [`pump_verb`](Self::pump_verb). Non-blocking — the GUI stays
    /// responsive. One verb at a time; a second is refused while one runs.
    pub fn run_verb(&mut self, args: &[String]) {
        if self.verb_job.as_ref().is_some_and(|j| !j.finished()) {
            self.log.push(LogLine {
                text: "a job is already running — wait for it to finish".into(),
                err: true,
            });
            return;
        }
        self.verb_job = Some(spawn_verb(&self.cli_cmd, args));
    }

    /// Drain any streamed verb output into the log; on completion, refresh the
    /// status snapshot. Called every frame.
    fn pump_verb(&mut self, ctx: &Context) {
        let Some(job) = &self.verb_job else {
            return;
        };
        let (mut lines, finished) = (job.drain(), job.finished());
        if finished {
            lines.extend(job.drain()); // catch any stragglers after the flag
        }
        for l in lines {
            self.log.push(l);
        }
        if self.log.len() > 500 {
            let drop = self.log.len() - 500;
            self.log.drain(0..drop);
        }
        if finished {
            self.verb_job = None;
            self.refresh();
        } else {
            ctx.request_repaint();
        }
    }

    /// (Re)build the preview texture from the active side's Gerbers (the back
    /// side is shown mirrored, exactly as it will burn).
    pub fn render_preview(&mut self, ctx: &Context) {
        match self.active_job() {
            Ok((board, copper, ablate)) => {
                let img = preview::rasterize(
                    &[
                        preview::Layer {
                            polys: &board,
                            color: preview::BOARD,
                        },
                        preview::Layer {
                            polys: &ablate,
                            color: preview::ABLATE,
                        },
                        preview::Layer {
                            polys: &copper,
                            color: preview::COPPER,
                        },
                    ],
                    preview::BOARD,
                    40.0,
                    900,
                );
                let side = match self.side {
                    Side::Front => "front",
                    Side::Back => "back (mirrored)",
                };
                self.preview_note = format!(
                    "{side}: {} copper region(s), {} to-ablate region(s), offset {} mm",
                    copper.len(),
                    ablate.len(),
                    self.offset_mm
                );
                self.preview_tex =
                    Some(ctx.load_texture("job-preview", img, TextureOptions::NEAREST));
            }
            Err(e) => {
                self.preview_tex = None;
                self.preview_note = e;
            }
        }
    }

    /// Load the fiducial frame into memory + a texture and seed the search
    /// markers from the design layout (so they start near nominal, ready to
    /// drag onto the real holes).
    pub fn load_fid_frame(&mut self, ctx: &Context) {
        let img = match image::open(crate::clean_path(&self.fid_frame)) {
            Ok(i) => i.to_luma8(),
            Err(e) => {
                self.fid_note = format!("frame: {e}");
                return;
            }
        };
        let design = match fiducial::parse_layout(&self.fid_layout) {
            Ok(d) => d,
            Err(e) => {
                self.fid_note = format!("layout: {e}");
                return;
            }
        };
        let _ = design;
        self.sync_fid_markers();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let color = ColorImage {
            size: [w, h],
            pixels: img.pixels().map(|p| Color32::from_gray(p[0])).collect(),
        };
        self.fid_frame_tex = Some(ctx.load_texture("fid-frame", color, TextureOptions::NEAREST));
        self.fid_frame_img = Some(img);
        self.fid_note = "drag each ✛ near its hole, then Check".into();
    }

    /// Append an expected fiducial at bed `(mx, my)` mm to the layout and sync
    /// the markers (FLD-12 click-to-place). The layout string stays the source
    /// of truth, so the new ✛ appears and feeds the homography correspondences.
    fn add_expected_fiducial(&mut self, mx: f64, my: f64) {
        let base = self.fid_layout.trim().trim_end_matches(';').trim();
        let sep = if base.is_empty() { "" } else { "; " };
        self.fid_layout = format!("{base}{sep}{mx:.1},{my:.1}");
        self.sync_fid_markers();
        let n = self.fid_search.len();
        self.fid_note = format!(
            "added fiducial at ({mx:.1}, {my:.1}) mm  ·  {n} total (right-click a ✛ to remove)"
        );
    }

    /// Remove expected fiducial `i` (FLD-12 click-to-place). Drops the matching
    /// layout token — keeping the others' exact text — and the aligned search /
    /// found entries, so the ✛ set shrinks instead of only ever growing.
    fn remove_expected_fiducial(&mut self, i: usize) {
        let tokens: Vec<String> = self
            .fid_layout
            .split(';')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        if i >= tokens.len() {
            return;
        }
        let kept: Vec<String> = tokens
            .into_iter()
            .enumerate()
            .filter(|(k, _)| *k != i)
            .map(|(_, t)| t)
            .collect();
        self.fid_layout = kept.join("; ");
        if i < self.fid_search.len() {
            self.fid_search.remove(i);
        }
        if i < self.fid_found.len() {
            self.fid_found.remove(i);
        }
        // Lengths already match; sync is a no-op reconcile (and re-seeds only if
        // the layout still parses).
        self.sync_fid_markers();
        self.fid_note = format!("removed fiducial #{i}  ·  {} left", kept.len());
    }

    /// Resize the draggable markers to match the design layout, preserving
    /// existing (dragged) positions and seeding any new ones from the layout —
    /// so adding a 4th coordinate makes a 4th ✛ appear without a manual reset.
    fn sync_fid_markers(&mut self) {
        if fiducial::parse_layout(&self.fid_layout).is_err() {
            return;
        }
        // Seed from the side-aware expected positions (design on the front;
        // mirrored + beam-offset on the back).
        let expected = self.expected_points();
        let old = self.fid_search.len();
        self.fid_search.resize(expected.len(), (0.0, 0.0));
        for (i, d) in expected.iter().enumerate().skip(old) {
            self.fid_search[i] = *d;
        }
        self.fid_found.resize(self.fid_search.len(), None);
    }

    /// Detect around the current (draggable) search markers and record the
    /// found positions, summary rows, and measured scale.
    pub fn render_fiducials(&mut self, ctx: &Context) {
        self.sync_fid_markers();
        if self.fid_frame_img.is_none() {
            self.load_fid_frame(ctx);
        }
        if self.fid_frame_img.is_none() {
            return;
        }
        if self.fid_search.is_empty() {
            self.fid_note = "load a frame first".into();
            return;
        }
        self.detect_fiducials();
    }

    /// Live fiducial tracking: pull frames from the (camera-tab) source and
    /// re-detect each one, so the rings track the holes as the board moves.
    /// Uses `cam_source`, so pick the device/file in the Camera tab.
    fn pump_fid_live(&mut self, ctx: &Context) {
        if !self.fid_live {
            if self.fid_capture.is_some() {
                self.fid_capture = None;
                self.fid_capture_src = None;
            }
            return;
        }
        let src = self.cam_source();
        if self.fid_capture.is_none() || self.fid_capture_src.as_ref() != Some(&src) {
            self.fid_capture = None;
            self.fid_capture = Some(crate::camera::Capture::start(src.clone()));
            self.fid_capture_src = Some(src);
        }
        let latest = self.fid_capture.as_ref().and_then(|c| c.latest());
        if let Some(res) = latest {
            match res {
                Ok(gray) => {
                    let (w, h) = (gray.width() as usize, gray.height() as usize);
                    let color = ColorImage {
                        size: [w, h],
                        pixels: gray.pixels().map(|p| Color32::from_gray(p[0])).collect(),
                    };
                    self.fid_frame_tex =
                        Some(ctx.load_texture("fid-frame", color, TextureOptions::NEAREST));
                    self.fid_frame_img = Some(gray);
                    self.sync_fid_markers();
                    if !self.fid_search.is_empty() {
                        self.detect_fiducials();
                    }
                }
                Err(e) => {
                    self.fid_note = e;
                    self.fid_live = false;
                }
            }
        }
        ctx.request_repaint();
    }

    /// Run detection on the current in-memory frame around the search markers,
    /// updating rows/found/measured/homography. Shared by the static Check and
    /// the live-tracking loop (FLD-11).
    fn detect_fiducials(&mut self) {
        let Some(frame) = &self.fid_frame_img else {
            return;
        };
        let profile = self.fid_profile.to_profile(self.fid_diameter_mm);
        let r = fiducial::check_frame(
            frame,
            &self.fid_search,
            self.fid_px_per_mm,
            &profile,
            self.fid_search_mm,
        );
        let (s, w, m) = r.tally;
        self.fid_measured_ppm = r.measured_px_per_mm;
        let scale = match r.measured_px_per_mm {
            Some(p) => format!("  ·  measured {p:.2} px/mm"),
            None => String::new(),
        };
        self.fid_note = format!("{s} strong, {w} weak, {m} missed{scale}");
        self.fid_rows = r.rows;
        self.fid_found = r.found_px;

        // Perspective: with ≥4 detected fiducials, fit the design→pixel
        // homography (a tilted camera keystones the flat board). It corrects
        // the Place overlay and any downstream mapping; <4 can only be affine.
        let design = fiducial::parse_layout(&self.fid_layout).unwrap_or_default();
        let corr: Vec<_> = design
            .iter()
            .zip(&self.fid_found)
            .filter_map(|(&(dx, dy), f)| {
                f.map(|(px, py)| (nalgebra::Point2::new(dx, dy), nalgebra::Point2::new(px, py)))
            })
            .collect();
        self.fid_homography = if corr.len() >= 4 {
            match vision::fit_homography(&corr) {
                Ok(hgt) => {
                    self.fid_note
                        .push_str(&format!("  ·  perspective fit (reproj {:.2} px)", hgt.rms));
                    Some(hgt)
                }
                Err(e) => {
                    self.fid_note.push_str(&format!("  ·  perspective: {e}"));
                    None
                }
            }
        } else {
            if !corr.is_empty() {
                self.fid_note
                    .push_str("  ·  add a 4th fiducial for perspective");
            }
            None
        };
    }

    /// Move the placement so its pivot's **pixel** position shifts by
    /// `(dpx, dpy)` frame pixels. Dragging felt wrong under perspective because
    /// the old code added a uniform mm delta — a uniform mm step is *not* a
    /// uniform pixel step on a tilted plane, so the overlay slid along the plane
    /// instead of following the cursor. Here we map the pivot to pixels through
    /// the same homography the composite uses, shift in pixels, and invert back
    /// to bed-mm — so the geometry tracks where the mouse moves over the image.
    fn drag_place_px(&mut self, dpx: f64, dpy: f64) {
        let ppm = self.place_px_per_mm;
        let inv = self.fid_homography.as_ref().and_then(|h| h.try_inverse());
        // Forward: pivot bed-mm → pixel (perspective only if it's invertible,
        // so the forward/back maps always agree).
        let (px, py) = match self.fid_homography.as_ref() {
            Some(h) if inv.is_some() => {
                let p = h.apply(nalgebra::Point2::new(self.place_tx_mm, self.place_ty_mm));
                (p.x, p.y)
            }
            _ => (self.place_tx_mm * ppm, self.place_ty_mm * ppm),
        };
        let (nx, ny) = (px + dpx, py + dpy);
        let (tx, ty) = match &inv {
            Some(i) => {
                let p = i.apply(nalgebra::Point2::new(nx, ny));
                (p.x, p.y)
            }
            None => (nx / ppm, ny / ppm),
        };
        self.place_tx_mm = tx;
        self.place_ty_mm = ty;
    }

    /// The copper/outline Gerber paths for the active side.
    fn active_gerbers(&self) -> (&str, &str) {
        match self.side {
            Side::Front => (&self.emit_copper, &self.emit_outline),
            Side::Back => (&self.back_copper, &self.back_outline),
        }
    }

    /// The active side's (board, copper, ablate) job, mirrored in X when it's
    /// the back side (KiCad B.Cu is top-view, so a left-right flip mirrors it).
    fn active_job(&self) -> Result<JobShapes, String> {
        let (copper, outline) = self.active_gerbers();
        let (board, cu, ablate) = job_shapes(copper, outline, self.offset_mm)?;
        Ok(match self.side {
            Side::Front => (board, cu, ablate),
            Side::Back => {
                let axis = cam::flip::MirrorAxis::VerticalX { x_mm: 0.0 };
                (
                    cam::flip::mirror_job(&board, &axis),
                    cam::flip::mirror_job(&cu, &axis),
                    cam::flip::mirror_job(&ablate, &axis),
                )
            }
        })
    }

    /// The flip axis + field optics for the back side. The mirror is about the
    /// fiducial layout's vertical centerline (a display choice — it keeps the
    /// flipped markers on-screen). The beam parallax scales about the **scan
    /// center**: the layout centroid by default (`scan_center_auto`, the
    /// un-calibrated assumption pending VIS-3) or the operator-entered field
    /// center when they've measured where the lens axis really is. `None` on
    /// the front.
    fn back_field(&self) -> Option<(cam::flip::MirrorAxis, cam::flip::FieldParams)> {
        if self.side != Side::Back {
            return None;
        }
        let pts = fiducial::parse_layout(&self.fid_layout).ok()?;
        let n = pts.len() as f64;
        let cx = pts.iter().map(|p| p.0).sum::<f64>() / n;
        let cy = pts.iter().map(|p| p.1).sum::<f64>() / n;
        let scan_center = if self.scan_center_auto {
            (cx, cy)
        } else {
            self.scan_center_mm
        };
        Some((
            cam::flip::MirrorAxis::VerticalX { x_mm: cx },
            cam::flip::FieldParams {
                scan_center_mm: scan_center,
                thickness_mm: self.board_thickness_mm,
                focal_mm: self.focal_mm,
            },
        ))
    }

    /// The expected fiducial positions to display/detect, in bed mm: the raw
    /// design layout on the front, or the mirrored + beam-offset positions on
    /// the back (where the drilled through-holes actually appear when flipped).
    fn expected_points(&self) -> Vec<(f64, f64)> {
        let design = fiducial::parse_layout(&self.fid_layout).unwrap_or_default();
        match self.back_field() {
            None => design,
            Some((axis, field)) => design
                .iter()
                .map(|&(x, y)| cam::flip::back_expected_fiducial_mm(x, y, &axis, &field))
                .collect(),
        }
    }

    /// Switch the working side, clearing the per-side caches so the fiducial
    /// markers, AR design, and Place job all recompute for the new face.
    fn set_side(&mut self, side: Side) {
        if self.side == side {
            return;
        }
        self.side = side;
        self.fid_search.clear();
        self.fid_found.clear();
        self.fid_homography = None;
        self.ar_board.clear();
        self.ar_copper.clear();
        self.ar_ablate.clear();
        self.place_job.clear();
    }

    /// Current manual placement.
    fn placement(&self) -> crate::place::Placement {
        crate::place::Placement {
            tx_mm: self.place_tx_mm,
            ty_mm: self.place_ty_mm,
            rot_deg: self.place_rot_deg,
            pivot_mm: self.place_pivot,
        }
    }

    /// Load the bed frame + job geometry into the place cache and center the
    /// job on the frame. Uses the Job-tab Gerber paths for the geometry.
    pub fn load_place(&mut self, ctx: &Context) {
        let img = match image::open(crate::clean_path(&self.place_frame)) {
            Ok(i) => i.to_luma8(),
            Err(e) => {
                self.place_note = format!("frame: {e}");
                return;
            }
        };
        let (_, _, ablate) = match self.active_job() {
            Ok(t) => t,
            Err(e) => {
                self.place_note = format!("job: {e}");
                return;
            }
        };
        self.place_pivot = crate::place::bbox_center_mm(&ablate);
        // Start centered on the frame.
        self.place_tx_mm = img.width() as f64 / 2.0 / self.place_px_per_mm;
        self.place_ty_mm = img.height() as f64 / 2.0 / self.place_px_per_mm;
        self.place_rot_deg = 0.0;
        self.place_job = ablate;
        self.place_frame_img = Some(img);
        self.recompose(ctx);
    }

    /// Re-blend the placed job over the cached frame into the display texture.
    fn recompose(&mut self, ctx: &Context) {
        let Some(frame) = &self.place_frame_img else {
            return;
        };
        if self.place_job.is_empty() {
            return;
        }
        let img = crate::place::composite(
            frame,
            &self.place_job,
            &self.placement(),
            self.place_px_per_mm,
            self.fid_homography.as_ref(),
            [0xf0, 0x50, 0x30],
            0.55,
        );
        let persp = if self.fid_homography.is_some() {
            " · perspective"
        } else {
            ""
        };
        self.place_note = format!(
            "placed at ({:.1}, {:.1}) mm, {:.0}°{persp}",
            self.place_tx_mm, self.place_ty_mm, self.place_rot_deg
        );
        self.place_tex = Some(ctx.load_texture("place", img, TextureOptions::NEAREST));
    }

    /// Emit the job registered to the current manual placement by encoding it
    /// as fiducial correspondences and shelling `pcbforge register`.
    fn emit_at_placement(&mut self) {
        if self.place_job.is_empty() {
            self.log.push(LogLine {
                text: "place: load a frame + job first".into(),
                err: true,
            });
            return;
        }
        if self.emit_copper.trim().is_empty() {
            self.log.push(LogLine {
                text: "place: set a copper Gerber (Job tab) first".into(),
                err: true,
            });
            return;
        }
        let mut args: Vec<String> = vec![
            "register".into(),
            "--copper".into(),
            crate::clean_path(&self.emit_copper),
            "--lbrn2".into(),
            crate::clean_path(&self.emit_lbrn2),
            "--fiducials".into(),
            self.placement().correspondences(),
        ];
        if !crate::clean_path(&self.emit_outline).is_empty() {
            args.push("--outline".into());
            args.push(crate::clean_path(&self.emit_outline));
        }
        self.run_verb(&args);
    }

    /// The current camera source (device or file).
    fn cam_source(&self) -> crate::camera::Source {
        if self.cam_use_device {
            crate::camera::Source::Device(self.cam_device)
        } else {
            crate::camera::Source::File(self.cam_file.clone())
        }
    }

    /// Store a grabbed frame into the preview texture + cache. When the AR
    /// overlay (UI-2) is on, the registered design layers are blended over it.
    fn set_camera_frame(&mut self, ctx: &Context, gray: image::GrayImage) {
        let (w, h) = (gray.width() as usize, gray.height() as usize);
        let img = if self.ar_overlay {
            self.compose_ar(&gray)
        } else {
            ColorImage {
                size: [w, h],
                pixels: gray.pixels().map(|p| Color32::from_gray(p[0])).collect(),
            }
        };
        self.cam_tex = Some(ctx.load_texture("camera", img, TextureOptions::NEAREST));
        self.cam_note = format!("{w}×{h}");
        self.cam_last = Some(gray);
    }

    /// Load the Job-tab Gerbers into the AR layer caches (board / copper /
    /// ablate), so the overlay can be re-blended every frame without re-parsing.
    fn load_ar_design(&mut self) {
        match self.active_job() {
            Ok((board, copper, ablate)) => {
                let side = match self.side {
                    Side::Front => "front",
                    Side::Back => "back (mirrored)",
                };
                self.ar_note = format!(
                    "{side} design: {} board, {} copper, {} ablate region(s)",
                    board.len(),
                    copper.len(),
                    ablate.len()
                );
                self.ar_board = board;
                self.ar_copper = copper;
                self.ar_ablate = ablate;
            }
            Err(e) => {
                self.ar_board.clear();
                self.ar_copper.clear();
                self.ar_ablate.clear();
                self.ar_note = format!("design: {e}");
            }
        }
    }

    /// Blend the enabled design layers over `gray`, mapping design-mm → pixels
    /// through the fiducial homography (registered AR) when one has been
    /// fitted, else a uniform `fid_px_per_mm` scale (a rough, unregistered
    /// overlay). The design is placed with an identity placement, so its Gerber
    /// coordinates go straight through the map — the same frame contract as
    /// `register --frame`.
    fn compose_ar(&self, gray: &image::GrayImage) -> ColorImage {
        let (w, h) = (gray.width() as usize, gray.height() as usize);
        let mut img = ColorImage {
            size: [w, h],
            pixels: gray.pixels().map(|p| Color32::from_gray(p[0])).collect(),
        };
        let ident = crate::place::Placement {
            tx_mm: 0.0,
            ty_mm: 0.0,
            rot_deg: 0.0,
            pivot_mm: (0.0, 0.0),
        };
        let hgt = self.fid_homography.as_ref();
        let mut layer = |shapes: &[pcb_core::Poly], on: bool, color: [u8; 3], alpha: f64| {
            if on && !shapes.is_empty() {
                crate::place::composite_over(
                    &mut img,
                    shapes,
                    &ident,
                    self.fid_px_per_mm,
                    hgt,
                    color,
                    alpha,
                );
            }
        };
        layer(&self.ar_board, self.ar_show_board, [0x30, 0x60, 0xa0], 0.30);
        layer(
            &self.ar_copper,
            self.ar_show_copper,
            [0xd0, 0xa0, 0x30],
            0.45,
        );
        layer(
            &self.ar_ablate,
            self.ar_show_ablate,
            [0xf0, 0x50, 0x30],
            0.45,
        );
        img
    }

    /// Grab one frame synchronously (the "grab once" button). For Live, the
    /// background [`Capture`](crate::camera::Capture) thread is used instead so
    /// I/O never blocks the GUI.
    pub fn grab_camera(&mut self, ctx: &Context) {
        match crate::camera::grab(&self.cam_source()) {
            Ok(gray) => self.set_camera_frame(ctx, gray),
            Err(e) => self.cam_note = e,
        }
    }

    /// Ensure the background capture matches Live state + the current source,
    /// and pull the newest frame from it (non-blocking).
    fn pump_camera(&mut self, ctx: &Context) {
        if self.cam_live {
            let src = self.cam_source();
            let restart = self.cam_capture.is_none() || self.cam_capture_src.as_ref() != Some(&src);
            if restart {
                // Dropping the old Capture stops its thread before the new one.
                self.cam_capture = None;
                self.cam_capture = Some(crate::camera::Capture::start(src.clone()));
                self.cam_capture_src = Some(src);
            }
            let latest = self.cam_capture.as_ref().and_then(|c| c.latest());
            if let Some(res) = latest {
                match res {
                    Ok(gray) => self.set_camera_frame(ctx, gray),
                    Err(e) => self.cam_note = e,
                }
            }
            ctx.request_repaint(); // keep the loop alive
        } else if self.cam_capture.is_some() {
            self.cam_capture = None; // stop the thread
            self.cam_capture_src = None;
        }
    }

    /// Save the last grabbed frame to a PNG and point the Fiducial + Place tabs
    /// at it — the bridge from live view into detection / placement.
    fn snapshot_to_tabs(&mut self) {
        let Some(frame) = &self.cam_last else {
            self.cam_note = "grab a frame first".into();
            return;
        };
        let path = std::env::temp_dir().join("pcbforge-snapshot.png");
        match frame.save(&path) {
            Ok(()) => {
                let p = path.to_string_lossy().into_owned();
                self.fid_frame = p.clone();
                self.place_frame = p;
                self.cam_note = format!("snapshot → Fiducial + Place tabs ({})", path.display());
            }
            Err(e) => self.cam_note = format!("save: {e}"),
        }
    }

    /// Draw one frame. Kept separate from the `eframe::App` impl so it runs
    /// under a bare `egui::Context` in tests.
    pub fn ui(&mut self, ctx: &Context) {
        self.pump_verb(ctx);
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
                if self.verb_job.is_some() {
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
    }

    fn status_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Status");
        if let Some(err) = &self.status.error {
            ui.colored_label(Color32::from_rgb(0xd0, 0x60, 0x50), err);
        }
        ui.separator();
        ui.label(egui::RichText::new("Stages").strong());
        for s in &self.status.stages {
            let here = self.status.boards.iter().any(|b| &b.stage == s);
            if here {
                ui.colored_label(Color32::from_rgb(0x60, 0xb0, 0x70), format!("▶ {s}"));
            } else {
                ui.label(format!("   {s}"));
            }
        }
        ui.separator();
        ui.label(egui::RichText::new("Boards").strong());
        if self.status.boards.is_empty() {
            ui.weak("none — run `next` to admit a board");
        }
        for b in &self.status.boards {
            let reg = if b.registered {
                "✔ registered"
            } else {
                "unregistered"
            };
            ui.monospace(format!("#{} [{}] {}", b.id, b.stage, reg));
            ui.weak(short_path(&b.design));
        }
    }

    fn actions_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Actions");
        ui.label(egui::RichText::new("These shell the `pcbforge` CLI.").weak());
        ui.separator();

        egui::Grid::new("emit-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("copper .gbr");
                ui.add(egui::TextEdit::singleline(&mut self.emit_copper).desired_width(180.0));
                ui.end_row();
                ui.label("outline .gbr");
                ui.add(egui::TextEdit::singleline(&mut self.emit_outline).desired_width(180.0));
                ui.end_row();
                ui.label("out .lbrn2");
                ui.add(egui::TextEdit::singleline(&mut self.emit_lbrn2).desired_width(180.0));
                ui.end_row();
                ui.label("offset mm");
                ui.add(
                    egui::DragValue::new(&mut self.offset_mm)
                        .speed(0.005)
                        .range(0.0..=10.0),
                );
                ui.end_row();
            });

        // Double-sided (ORC-6): side selector + back-side inputs.
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("side");
            let mut side = self.side;
            ui.selectable_value(&mut side, Side::Front, "Front");
            ui.selectable_value(&mut side, Side::Back, "Back");
            self.set_side(side);
        });
        if self.side == Side::Back {
            egui::Grid::new("back-form")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("back copper .gbr");
                    ui.add(egui::TextEdit::singleline(&mut self.back_copper).desired_width(180.0));
                    ui.end_row();
                    ui.label("back outline .gbr");
                    ui.add(egui::TextEdit::singleline(&mut self.back_outline).desired_width(180.0));
                    ui.end_row();
                    ui.label("thickness mm");
                    ui.add(
                        egui::DragValue::new(&mut self.board_thickness_mm)
                            .speed(0.05)
                            .range(0.0..=10.0),
                    );
                    ui.end_row();
                    ui.label("focal mm");
                    ui.add(
                        egui::DragValue::new(&mut self.focal_mm)
                            .speed(1.0)
                            .range(1.0..=1000.0),
                    );
                    ui.end_row();
                    ui.label("scan center");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.scan_center_auto, "auto")
                            .on_hover_text(
                                "Use the fiducial-layout centroid as the lens axis. \
                                 Uncheck and enter the measured field center once known \
                                 (VIS-3 will calibrate it).",
                            );
                        if !self.scan_center_auto {
                            ui.add(
                                egui::DragValue::new(&mut self.scan_center_mm.0)
                                    .speed(0.5)
                                    .prefix("x "),
                            );
                            ui.add(
                                egui::DragValue::new(&mut self.scan_center_mm.1)
                                    .speed(0.5)
                                    .prefix("y "),
                            );
                        }
                    });
                    ui.end_row();
                });
            ui.weak(
                "Back mirrors the design in X; fiducial markers carry the beam \
                 entry→exit offset (thickness/focal, about the scan center) so \
                 they land on the flipped holes.",
            );
        }

        ui.horizontal(|ui| {
            if ui.button("🖼 Render preview").clicked() {
                let ctx = ui.ctx().clone();
                self.render_preview(&ctx);
            }
            if ui.button("▶ Emit .lbrn2").clicked() {
                self.emit_clicked();
            }
        });

        ui.separator();
        if ui.button("⏭ Next stage (pcbforge next)").clicked() {
            self.run_verb(&["next".into()]);
        }
        ui.separator();
        ui.weak("Live camera → the “📷 Camera” tab.");
    }

    fn emit_clicked(&mut self) {
        let (copper, outline) = self.active_gerbers();
        let (copper, outline) = (crate::clean_path(copper), crate::clean_path(outline));
        if copper.is_empty() {
            let which = match self.side {
                Side::Front => "copper Gerber",
                Side::Back => "back copper Gerber",
            };
            self.log.push(LogLine {
                text: format!("emit: set a {which} first"),
                err: true,
            });
            return;
        }
        let mut args: Vec<String> = vec![
            "emit".into(),
            "--copper".into(),
            copper,
            "--lbrn2".into(),
            crate::clean_path(&self.emit_lbrn2),
            "--offset-mm".into(),
            format!("{}", self.offset_mm),
        ];
        if !outline.is_empty() {
            args.push("--outline".into());
            args.push(outline);
        }
        // Back side: mirror the design in X to match the flipped board.
        if self.side == Side::Back {
            args.push("--mirror-x".into());
        }
        self.run_verb(&args);
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, CentralTab::Job, "🖼 Job preview");
            ui.selectable_value(&mut self.tab, CentralTab::Camera, "📷 Camera");
            ui.selectable_value(&mut self.tab, CentralTab::Fiducials, "🎯 Fiducial check");
            ui.selectable_value(&mut self.tab, CentralTab::Place, "✋ Place on board");
        });
        ui.separator();
        match self.tab {
            CentralTab::Job => self.job_view(ui),
            CentralTab::Camera => self.camera_view(ui),
            CentralTab::Fiducials => self.fiducial_view(ui),
            CentralTab::Place => self.place_view(ui),
        }
    }

    fn camera_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.cam_use_device, false, "File");
            ui.selectable_value(&mut self.cam_use_device, true, "Device");
            if self.cam_use_device && ui.button("↻ devices").clicked() {
                self.cam_devices = crate::camera::list_devices();
            }
        });
        if self.cam_use_device {
            if self.cam_devices.is_empty() {
                ui.weak(
                    "No devices (build with --features native,camera for a webcam, or use File).",
                );
                ui.add(
                    egui::DragValue::new(&mut self.cam_device)
                        .range(0..=15)
                        .prefix("index "),
                );
            } else {
                egui::ComboBox::from_label("device")
                    .selected_text(
                        self.cam_devices
                            .iter()
                            .find(|(i, _)| *i == self.cam_device)
                            .map(|(_, n)| n.clone())
                            .unwrap_or_else(|| format!("index {}", self.cam_device)),
                    )
                    .show_ui(ui, |ui| {
                        for (i, name) in &self.cam_devices {
                            ui.selectable_value(&mut self.cam_device, *i, format!("{i}: {name}"));
                        }
                    });
            }
        } else {
            ui.horizontal(|ui| {
                ui.label("frame file");
                ui.add(egui::TextEdit::singleline(&mut self.cam_file).desired_width(240.0));
            });
            ui.weak("Any capture app that writes a frame to disk drives the live preview.");
        }

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.cam_live, "● Live");
            if ui.button("grab once").clicked() {
                let ctx = ui.ctx().clone();
                self.grab_camera(&ctx);
            }
            if ui.button("📸 Snapshot → Fiducial/Place").clicked() {
                self.snapshot_to_tabs();
            }
            ui.label(egui::RichText::new(&self.cam_note).weak());
        });
        ui.separator();

        // AR overlay (UI-2): the registered design projected over the feed.
        let mut ar_changed = false;
        ui.horizontal(|ui| {
            ar_changed |= ui
                .checkbox(&mut self.ar_overlay, "🔲 AR overlay")
                .on_hover_text(
                    "Project the registered design over the camera frame using \
                     the fiducial homography (detect fiducials first).",
                )
                .changed();
            if ui.button("⤵ Load design").clicked() {
                self.load_ar_design();
                ar_changed = true;
            }
            if self.ar_overlay {
                ar_changed |= ui.checkbox(&mut self.ar_show_board, "board").changed();
                ar_changed |= ui.checkbox(&mut self.ar_show_copper, "copper").changed();
                ar_changed |= ui.checkbox(&mut self.ar_show_ablate, "ablate").changed();
            }
        });
        if self.ar_overlay {
            let reg = if self.fid_homography.is_some() {
                "registered (perspective)"
            } else {
                "unregistered — detect ≥4 fiducials to register"
            };
            ui.label(egui::RichText::new(format!("{}  ·  {reg}", self.ar_note)).weak());
        }
        // Re-blend a still frame when a toggle changes (live frames re-blend as
        // they arrive).
        if ar_changed
            && !self.cam_live
            && let Some(gray) = self.cam_last.take()
        {
            let ctx = ui.ctx().clone();
            self.set_camera_frame(&ctx, gray);
        }
        ui.separator();

        // Live frames come from the background capture thread (non-blocking).
        let ctx = ui.ctx().clone();
        self.pump_camera(&ctx);

        if let Some(tex) = &self.cam_tex {
            ui.add(egui::Image::from_texture((tex.id(), tex.size_vec2())).shrink_to_fit());
        } else {
            ui.weak("(no frame yet)");
        }
    }

    fn place_view(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        egui::Grid::new("place-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("bed frame");
                ui.add(egui::TextEdit::singleline(&mut self.place_frame).desired_width(240.0));
                ui.end_row();
                ui.label("px per mm");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut self.place_px_per_mm)
                            .speed(0.1)
                            .range(0.1..=1000.0),
                    )
                    .changed();
                ui.end_row();
            });
        ui.horizontal(|ui| {
            if ui.button("⤵ Load frame + job").clicked() {
                let ctx = ui.ctx().clone();
                self.load_place(&ctx);
            }
            if ui.button("▶ Etch here (register)").clicked() {
                self.emit_at_placement();
            }
        });
        ui.horizontal(|ui| {
            ui.label("x mm");
            changed |= ui
                .add(egui::DragValue::new(&mut self.place_tx_mm).speed(0.1))
                .changed();
            ui.label("y mm");
            changed |= ui
                .add(egui::DragValue::new(&mut self.place_ty_mm).speed(0.1))
                .changed();
            ui.label("rot°");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut self.place_rot_deg)
                        .speed(0.5)
                        .range(-180.0..=180.0),
                )
                .changed();
        });
        ui.label(egui::RichText::new(&self.place_note).weak());
        ui.weak("Uses the Job-tab Gerbers. Drag the overlay to position; “Etch here” bakes it in via register.");
        ui.separator();

        if let Some(tex) = &self.place_tex {
            let native_w = tex.size()[0] as f32;
            let img = egui::Image::from_texture((tex.id(), tex.size_vec2()))
                .shrink_to_fit()
                .sense(egui::Sense::drag());
            let resp = ui.add(img);
            if resp.dragged() {
                let d = resp.drag_delta();
                // The frame is scaled to fit, so convert the screen-point drag
                // back to frame pixels (native_w / displayed_w). The move is
                // then applied in pixel space (see drag_place_px) so the overlay
                // tracks the cursor even when a perspective homography is active.
                let scale = (native_w / resp.rect.width().max(1.0)) as f64;
                self.drag_place_px(d.x as f64 * scale, d.y as f64 * scale);
                changed = true;
            }
        } else {
            ui.weak("(load a frame + job to place)");
        }

        if changed {
            let ctx = ui.ctx().clone();
            self.recompose(&ctx);
        }
    }

    fn job_view(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(&self.preview_note).weak());
        if let Some(tex) = &self.preview_tex {
            ui.add(egui::Image::from_texture((tex.id(), tex.size_vec2())).shrink_to_fit());
        } else {
            ui.weak("(no preview rendered — see the Actions panel)");
        }
    }

    fn fiducial_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.pump_fid_live(&ctx);
        egui::Grid::new("fid-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("frame image");
                ui.add(egui::TextEdit::singleline(&mut self.fid_frame).desired_width(240.0));
                ui.end_row();
                ui.label("expected (x,y mm; …)");
                ui.add(egui::TextEdit::singleline(&mut self.fid_layout).desired_width(240.0));
                ui.end_row();
                ui.label("px/mm (seed)");
                ui.add(
                    egui::DragValue::new(&mut self.fid_px_per_mm)
                        .speed(0.1)
                        .range(0.1..=1000.0),
                )
                .on_hover_text(
                    "Rough scale, only used to place the search windows. The true \
                     px/mm is measured from the fiducial spacing after detection.",
                );
                ui.end_row();
                ui.label("profile");
                egui::ComboBox::from_id_salt("fid-profile")
                    .selected_text(self.fid_profile.label())
                    .show_ui(ui, |ui| {
                        for k in crate::fiducial::ProfileKind::ALL {
                            ui.selectable_value(&mut self.fid_profile, k, k.label());
                        }
                    });
                ui.end_row();
                ui.label("hole ⌀ mm");
                ui.add(
                    egui::DragValue::new(&mut self.fid_diameter_mm)
                        .speed(0.05)
                        .range(0.05..=20.0),
                );
                ui.end_row();
                ui.label("search mm");
                ui.add(
                    egui::DragValue::new(&mut self.fid_search_mm)
                        .speed(0.1)
                        .range(0.1..=20.0),
                );
                ui.end_row();
            });
        ui.horizontal(|ui| {
            if ui.button("⤵ Load frame").clicked() {
                let ctx = ui.ctx().clone();
                self.load_fid_frame(&ctx);
            }
            if ui.button("🎯 Check fiducials").clicked() {
                let ctx = ui.ctx().clone();
                self.render_fiducials(&ctx);
            }
            ui.checkbox(&mut self.fid_live, "● Live").on_hover_text(
                "Track fiducials on the live camera feed (source from the Camera tab).",
            );
            ui.checkbox(&mut self.fid_click_place, "✚ click-to-place")
                .on_hover_text(
                    "Left-click an empty spot to add an expected fiducial; \
                     right-click a ✛ to remove it; drag markers to fine-tune.",
                );
            if ui.button("↺ reset markers").clicked() {
                self.fid_search.clear(); // reseeded from layout on next load/check
                let ctx = ui.ctx().clone();
                self.load_fid_frame(&ctx);
            }
            if let Some(ppm) = self.fid_measured_ppm
                && ui
                    .button(format!("↧ use measured {ppm:.2} px/mm"))
                    .on_hover_text("Adopt the fiducial-measured scale for this and the Place tab.")
                    .clicked()
            {
                self.fid_px_per_mm = ppm;
                self.place_px_per_mm = ppm;
            }
        });
        ui.label(egui::RichText::new(&self.fid_note).weak());
        ui.weak("Drag each ✛ near its hole; the detector searches locally around it. The typed px/mm only seeds the search — registration is anchored to the measured scale.");
        ui.separator();

        for row in &self.fid_rows {
            let color = match row.kind {
                FidKind::FoundStrong => Color32::from_rgb(0x50, 0xb0, 0x60),
                FidKind::FoundWeak => Color32::from_rgb(0xe0, 0x90, 0x20),
                FidKind::Miss => Color32::from_rgb(0xd0, 0x50, 0x50),
            };
            ui.colored_label(color, &row.text);
        }
        ui.separator();
        self.fid_frame_overlay(ui);
    }

    /// The frame with draggable search markers (✛) and detected rings drawn on
    /// top via the painter — so markers move without re-rasterizing the image.
    fn fid_frame_overlay(&mut self, ui: &mut egui::Ui) {
        // Keep the marker count in step with the layout field (live), so
        // adding/removing a coordinate adds/removes a ✛.
        self.sync_fid_markers();
        let Some(tex) = &self.fid_frame_tex else {
            ui.weak("(load a frame to place markers)");
            return;
        };
        let (tw, th) = (tex.size()[0] as f32, tex.size()[1] as f32);
        let resp = ui.add(
            egui::Image::from_texture((tex.id(), egui::vec2(tw, th)))
                .shrink_to_fit()
                .sense(egui::Sense::click_and_drag()),
        );
        let rect = resp.rect;
        let ppm = self.fid_px_per_mm as f32;
        // bed-mm ↔ screen (via the image rect + native texture size).
        let to_screen = |mmx: f64, mmy: f64| {
            egui::pos2(
                rect.min.x + (mmx as f32 * ppm) / tw * rect.width(),
                rect.min.y + (mmy as f32 * ppm) / th * rect.height(),
            )
        };
        let px_to_screen = |px: f64, py: f64| {
            egui::pos2(
                rect.min.x + (px as f32) / tw * rect.width(),
                rect.min.y + (py as f32) / th * rect.height(),
            )
        };
        let ppm_f = self.fid_px_per_mm; // a local copy so the closure below
        // doesn't borrow `self`, leaving `sync_fid_markers` callable.
        let to_mm = |p: egui::Pos2| {
            let ix = (p.x - rect.min.x) / rect.width() * tw;
            let iy = (p.y - rect.min.y) / rect.height() * th;
            (ix as f64 / ppm_f, iy as f64 / ppm_f)
        };

        // Click-to-place (FLD-12): screen positions of the current markers, for
        // hit-testing add (empty spot) vs. remove (right-click on a ✛).
        // Materialized (not a closure) so the `&self` borrow is released before
        // the `&mut self` add/remove calls below.
        if self.fid_click_place {
            let marker_px: Vec<(f32, f32)> = self
                .fid_search
                .iter()
                .map(|&(x, y)| {
                    let s = to_screen(x, y);
                    (s.x, s.y)
                })
                .collect();
            // Right-click a marker → remove it, so the set shrinks (fixes the
            // add-only pile-up).
            if resp.secondary_clicked()
                && let Some(pos) = resp.interact_pointer_pos()
                && let Some(i) = fiducial::nearest_marker(&marker_px, (pos.x, pos.y), 20.0)
            {
                self.remove_expected_fiducial(i);
            }
            // Left-click on empty frame → append an expected fiducial there (not
            // when a marker is under the pointer, so dragging isn't hijacked).
            else if resp.clicked()
                && let Some(pos) = resp.interact_pointer_pos()
                && fiducial::nearest_marker(&marker_px, (pos.x, pos.y), 20.0).is_none()
            {
                let (mx, my) = to_mm(pos);
                self.add_expected_fiducial(mx, my);
            }
        }

        // Drag: pick the nearest marker on press, move it while dragging.
        if resp.drag_started()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let markers: Vec<(f32, f32)> = self
                .fid_search
                .iter()
                .map(|&(x, y)| {
                    let s = to_screen(x, y);
                    (s.x, s.y)
                })
                .collect();
            self.fid_drag = fiducial::nearest_marker(&markers, (pos.x, pos.y), 30.0);
        }
        if resp.dragged()
            && let (Some(i), Some(pos)) = (self.fid_drag, resp.interact_pointer_pos())
            && i < self.fid_search.len()
        {
            self.fid_search[i] = to_mm(pos);
        }
        if resp.drag_stopped() {
            self.fid_drag = None;
        }

        // Paint markers + detected rings.
        let painter = ui.painter_at(rect);
        let cyan = Color32::from_rgb(0x22, 0xcc, 0xdd);
        let ring_r = (self.fid_diameter_mm as f32 * ppm * 0.5).max(5.0);
        for (i, &(mx, my)) in self.fid_search.iter().enumerate() {
            let c = to_screen(mx, my);
            painter.line_segment(
                [egui::pos2(c.x - 9.0, c.y), egui::pos2(c.x + 9.0, c.y)],
                (1.5, cyan),
            );
            painter.line_segment(
                [egui::pos2(c.x, c.y - 9.0), egui::pos2(c.x, c.y + 9.0)],
                (1.5, cyan),
            );
            painter.circle_stroke(c, 11.0, egui::Stroke::new(1.0, cyan));
            if let Some(Some((fx, fy))) = self.fid_found.get(i) {
                let col = match self.fid_rows.get(i).map(|r| &r.kind) {
                    Some(FidKind::FoundStrong) => Color32::from_rgb(0x40, 0xc0, 0x50),
                    _ => Color32::from_rgb(0xe0, 0x90, 0x20),
                };
                let fc = px_to_screen(*fx, *fy);
                painter.circle_stroke(fc, ring_r, egui::Stroke::new(2.0, col));
                painter.circle_filled(fc, 2.0, col);
            }
        }
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Log");
            if ui.button("clear").clicked() {
                self.log.clear();
            }
        });
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.log {
                    if line.err {
                        ui.colored_label(Color32::from_rgb(0xd0, 0x80, 0x60), &line.text);
                    } else {
                        ui.monospace(&line.text);
                    }
                }
            });
    }
}

/// Shell `cmd[0] cmd[1..] args`, capturing stdout (info) and stderr (warn) as
/// log lines plus a header and an exit-status footer. A spawn failure — or an
/// empty command — is one error line.
pub fn run_capture(cmd: &[String], args: &[String]) -> Vec<LogLine> {
    let Some((program, prefix)) = cmd.split_first() else {
        return vec![LogLine {
            text: "no CLI command configured".into(),
            err: true,
        }];
    };
    let mut out = vec![LogLine {
        text: format!("$ {} {}", cmd.join(" "), args.join(" ")),
        err: false,
    }];
    match std::process::Command::new(program)
        .args(prefix)
        .args(args)
        .output()
    {
        Ok(o) => {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                out.push(LogLine {
                    text: line.to_string(),
                    err: false,
                });
            }
            for line in String::from_utf8_lossy(&o.stderr).lines() {
                out.push(LogLine {
                    text: line.to_string(),
                    err: true,
                });
            }
            out.push(LogLine {
                text: format!("[exit {}]", o.status.code().unwrap_or(-1)),
                err: !o.status.success(),
            });
        }
        Err(e) => out.push(LogLine {
            text: format!("failed to run `{program}`: {e}"),
            err: true,
        }),
    }
    out
}

/// A CLI verb running on a background thread. Its stdout/stderr stream over the
/// channel line-by-line so the GUI never blocks; `done` flips when the process
/// exits. Dropping the job detaches the reader threads (they end when the
/// child's pipes close).
pub struct VerbJob {
    rx: Receiver<LogLine>,
    done: Arc<AtomicBool>,
}

impl VerbJob {
    /// Take all output lines available since the last poll (non-blocking).
    fn drain(&self) -> Vec<LogLine> {
        let mut v = Vec::new();
        while let Ok(l) = self.rx.try_recv() {
            v.push(l);
        }
        v
    }
    fn finished(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

/// Spawn `cmd[0] cmd[1..] args`, streaming stdout (info) and stderr (warn)
/// lines over the returned job — without blocking the caller (FLD-9).
pub fn spawn_verb(cmd: &[String], args: &[String]) -> VerbJob {
    let (tx, rx) = mpsc::channel::<LogLine>();
    let done = Arc::new(AtomicBool::new(false));
    let done_t = done.clone();
    let cmd = cmd.to_vec();
    let args = args.to_vec();
    thread::spawn(move || {
        let _ = tx.send(LogLine {
            text: format!("$ {} {}", cmd.join(" "), args.join(" ")),
            err: false,
        });
        let Some((program, prefix)) = cmd.split_first() else {
            let _ = tx.send(LogLine {
                text: "no CLI command configured".into(),
                err: true,
            });
            done_t.store(true, Ordering::Relaxed);
            return;
        };
        let spawned = StdCommand::new(program)
            .args(prefix)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(LogLine {
                    text: format!("failed to run `{program}`: {e}"),
                    err: true,
                });
                done_t.store(true, Ordering::Relaxed);
                return;
            }
        };
        // Read stdout and stderr concurrently so a full pipe can't deadlock.
        let txo = tx.clone();
        let ho = child.stdout.take().map(|o| {
            thread::spawn(move || {
                for line in BufReader::new(o).lines().map_while(Result::ok) {
                    let _ = txo.send(LogLine {
                        text: line,
                        err: false,
                    });
                }
            })
        });
        let txe = tx.clone();
        let he = child.stderr.take().map(|e| {
            thread::spawn(move || {
                for line in BufReader::new(e).lines().map_while(Result::ok) {
                    let _ = txe.send(LogLine {
                        text: line,
                        err: true,
                    });
                }
            })
        });
        if let Some(h) = ho {
            let _ = h.join();
        }
        if let Some(h) = he {
            let _ = h.join();
        }
        let (code, ok) = match child.wait() {
            Ok(s) => (s.code().unwrap_or(-1), s.success()),
            Err(_) => (-1, false),
        };
        let _ = tx.send(LogLine {
            text: format!("[exit {code}]"),
            err: !ok,
        });
        done_t.store(true, Ordering::Relaxed);
    });
    VerbJob { rx, done }
}

/// (board, kept-copper, to-ablate) region sets in the Gerber frame.
pub type JobShapes = (
    Vec<pcb_core::Poly>,
    Vec<pcb_core::Poly>,
    Vec<pcb_core::Poly>,
);

/// The job's board, kept-copper, and to-ablate regions in the Gerber frame —
/// the shared geometry behind the preview and the drag-to-place overlay. A
/// *view* computation (pure geometry via `cam::noncopper`), not engine logic;
/// the actual job is still produced by shelling `pcbforge`.
pub fn job_shapes(
    copper_path: &str,
    outline_path: &str,
    offset_mm: f64,
) -> Result<JobShapes, String> {
    let copper_path = crate::clean_path(copper_path);
    let outline_path = crate::clean_path(outline_path);
    if copper_path.is_empty() {
        return Err("set a copper Gerber path first".into());
    }
    let copper = ingest::gerber::load_gerber(std::path::Path::new(&copper_path))
        .map_err(|e| format!("copper: {}", e.msg))?
        .polys;
    let board = if outline_path.is_empty() {
        cam::noncopper::board_region_bbox(&copper, NM_PER_MM) // 1 mm margin
    } else {
        let o = ingest::gerber::load_gerber(std::path::Path::new(&outline_path))
            .map_err(|e| format!("outline: {}", e.msg))?
            .polys;
        cam::noncopper::board_region_from_outline(&o)
    };
    if board.is_empty() {
        return Err("empty board region".into());
    }
    let offset_nm = (offset_mm * NM_PER_MM as f64).round() as Nm;
    let ablate = cam::noncopper::noncopper(&board, &copper, offset_nm);
    Ok((board, copper, ablate))
}

/// Build a preview image from Gerber paths: invert copper → non-copper (the
/// same geometry `emit` burns) and rasterize board/copper/ablate. Returns the
/// image and a caption.
pub fn preview_image(
    copper_path: &str,
    outline_path: &str,
    offset_mm: f64,
) -> Result<(ColorImage, String), String> {
    let (board, copper, ablate) = job_shapes(copper_path, outline_path, offset_mm)?;
    let img = preview::rasterize(
        &[
            Layer {
                polys: &board,
                color: preview::BOARD,
            },
            Layer {
                polys: &ablate,
                color: preview::ABLATE,
            },
            Layer {
                polys: &copper,
                color: preview::COPPER,
            },
        ],
        preview::BOARD,
        40.0,
        900,
    );
    let note = format!(
        "{} copper region(s), {} to-ablate region(s), offset {offset_mm} mm",
        copper.len(),
        ablate.len(),
    );
    Ok((img, note))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ui-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("t.sqlite")
    }

    #[test]
    fn run_capture_captures_stdout_and_exit() {
        let out = run_capture(&["echo".into()], &["hello".into()]);
        assert!(out.iter().any(|l| l.text == "hello" && !l.err));
        assert!(out.iter().any(|l| l.text.starts_with("[exit 0]")));
    }

    #[test]
    fn run_capture_reports_spawn_failure() {
        let out = run_capture(&["definitely-not-a-real-binary-xyz".into()], &[]);
        assert!(
            out.iter()
                .any(|l| l.err && l.text.contains("failed to run"))
        );
    }

    #[test]
    fn build_preview_rejects_empty_copper() {
        assert!(preview_image("", "", 0.0).is_err());
    }

    /// Headless frame: the whole console lays out under a bare egui context
    /// with no display and no panic, and produces render output.
    #[test]
    fn app_lays_out_one_frame_headless() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        let ctx = Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            app.ui(ctx);
        });
        // egui produced a tessellated frame (at least the panels' shapes).
        assert!(
            !out.shapes.is_empty(),
            "the console must render some shapes"
        );
    }

    /// The Fiducial-check tab lays out headless (form + summary + image slot).
    #[test]
    fn fiducial_tab_lays_out_headless() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.tab = CentralTab::Fiducials;
        let ctx = Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
        assert!(!out.shapes.is_empty(), "fiducial tab must render");
    }

    /// The Place-on-board tab lays out headless (form + placement controls).
    #[test]
    fn place_tab_lays_out_headless() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.tab = CentralTab::Place;
        let ctx = Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
        assert!(!out.shapes.is_empty(), "place tab must render");
    }

    /// FLD-11: live tracking pulls frames from the camera source and re-detects
    /// each one — the found rings and the perspective fit update without a
    /// manual Check. Verified with a File source of 4 holes.
    #[test]
    fn live_fiducial_tracking_detects_on_the_feed() {
        let dir = std::env::temp_dir().join(format!("ui-fidlive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bed4.png");
        let ppm = 10.0;
        let holes = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0), (60.0, 60.0)];
        let img = image::GrayImage::from_fn(700, 700, |x, y| {
            let mut v = 150.0;
            for (mx, my) in holes {
                let (cx, cy) = (mx * ppm, my * ppm);
                if (((x as f64) - cx).powi(2) + ((y as f64) - cy).powi(2)).sqrt() < 0.5 * ppm {
                    v -= 90.0;
                }
            }
            image::Luma([v as u8])
        });
        img.save(&path).unwrap();

        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.tab = CentralTab::Fiducials;
        app.cam_use_device = false;
        app.cam_file = path.to_string_lossy().into();
        app.fid_layout = "10,10; 60,10; 10,60; 60,60".into();
        app.fid_px_per_mm = ppm;
        app.fid_live = true;
        let ctx = Context::default();
        for _ in 0..500 {
            app.pump_fid_live(&ctx);
            if app.fid_found.iter().filter(|f| f.is_some()).count() >= 4 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        assert!(
            app.fid_found.iter().filter(|f| f.is_some()).count() >= 4,
            "live tracking detected the four holes: {:?}",
            app.fid_rows
        );
        assert!(
            app.fid_homography.is_some(),
            "perspective fitted from 4 live fiducials"
        );

        app.fid_live = false;
        app.pump_fid_live(&ctx);
        assert!(app.fid_capture.is_none(), "capture stops when Live is off");
    }

    /// FLD-9: a verb runs on a background thread (run_verb returns at once),
    /// streams its output, and completing clears the job + refreshes status.
    #[test]
    fn run_verb_is_nonblocking_and_streams() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["echo".into()]);
        app.run_verb(&["streamed".into()]);
        assert!(app.verb_job.is_some(), "run_verb returned immediately");
        let ctx = Context::default();
        for _ in 0..500 {
            app.pump_verb(&ctx);
            if app.verb_job.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        assert!(app.verb_job.is_none(), "job completed and cleared");
        assert!(
            app.log.iter().any(|l| l.text == "streamed"),
            "stdout streamed"
        );
        assert!(
            app.log.iter().any(|l| l.text.starts_with("[exit 0]")),
            "exit footer logged"
        );
    }

    #[test]
    fn spawn_verb_reports_stderr_and_exit() {
        // `sh -c 'echo out; echo err 1>&2; exit 3'` exercises both streams.
        let job = spawn_verb(
            &["sh".into()],
            &["-c".into(), "echo out; echo err 1>&2; exit 3".into()],
        );
        let mut lines = Vec::new();
        for _ in 0..500 {
            lines.extend(job.drain());
            if job.finished() {
                lines.extend(job.drain());
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        assert!(lines.iter().any(|l| l.text == "out" && !l.err));
        assert!(lines.iter().any(|l| l.text == "err" && l.err));
        assert!(lines.iter().any(|l| l.text.contains("[exit 3]") && l.err));
    }

    /// The marker set tracks the layout field: adding a coordinate adds a
    /// marker (seeded from the layout), removing one drops it, and existing
    /// dragged positions are preserved.
    #[test]
    fn markers_follow_the_layout_field() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.fid_layout = "10,10; 60,10; 10,60".into();
        app.sync_fid_markers();
        assert_eq!(app.fid_search.len(), 3);

        app.fid_search[0] = (11.5, 9.0); // drag marker 0
        app.fid_layout = "10,10; 60,10; 10,60; 60,60".into();
        app.sync_fid_markers();
        assert_eq!(app.fid_search.len(), 4, "4th marker appears");
        assert_eq!(app.fid_search[0], (11.5, 9.0), "dragged position kept");
        assert_eq!(app.fid_search[3], (60.0, 60.0), "4th seeded from layout");

        app.fid_layout = "10,10; 60,10".into();
        app.sync_fid_markers();
        assert_eq!(app.fid_search.len(), 2, "removing coords drops markers");
    }

    /// Dragging a search marker onto an off-nominal hole makes detection find
    /// it: at the nominal design position the hole is out of the search window
    /// (miss); after moving the marker onto the hole, it's found.
    #[test]
    fn dragging_marker_lets_detection_find_offset_hole() {
        let dir = std::env::temp_dir().join(format!("ui-drag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hole.png");
        // One dark hole at bed (13,10) mm → px (130,100) at 10 px/mm.
        let ppm = 10.0;
        let (hx, hy) = (13.0 * ppm, 10.0 * ppm);
        let img = image::GrayImage::from_fn(220, 160, |x, y| {
            let bg = 150.0;
            let d = (((x as f64) - hx).powi(2) + ((y as f64) - hy).powi(2)).sqrt();
            let v = if d < 0.5 * ppm { bg - 90.0 } else { bg };
            image::Luma([v as u8])
        });
        img.save(&path).unwrap();

        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.tab = CentralTab::Fiducials;
        app.fid_frame = path.to_string_lossy().into();
        app.fid_layout = "10,10".into(); // design nominal, 3 mm from the hole
        app.fid_px_per_mm = 10.0;
        app.fid_diameter_mm = 1.0;
        app.fid_search_mm = 2.0;
        let ctx = Context::default();

        app.load_fid_frame(&ctx);
        assert_eq!(
            app.fid_search,
            vec![(10.0, 10.0)],
            "markers seed from design"
        );

        app.render_fiducials(&ctx);
        assert!(
            app.fid_found[0].is_none(),
            "misses at nominal (hole is 3 mm off)"
        );

        // Drag the marker onto the hole.
        app.fid_search[0] = (13.0, 10.0);
        app.render_fiducials(&ctx);
        assert!(
            app.fid_found[0].is_some(),
            "found after dragging the marker onto the hole"
        );
    }

    /// The Camera tab lays out headless, a File-source grab loads a texture,
    /// and Snapshot points the Fiducial + Place tabs at the saved frame.
    #[test]
    fn camera_grab_and_snapshot_flow() {
        let dir = std::env::temp_dir().join(format!("ui-camflow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let frame = dir.join("live.png");
        image::GrayImage::from_pixel(48, 32, image::Luma([90]))
            .save(&frame)
            .unwrap();

        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.tab = CentralTab::Camera;
        app.cam_use_device = false;
        app.cam_file = format!("\"{}\"", frame.display()); // quoted on purpose
        let ctx = Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));

        app.grab_camera(&ctx);
        assert!(app.cam_tex.is_some(), "grab loaded a texture");
        assert_eq!(app.cam_last.as_ref().unwrap().dimensions(), (48, 32));

        app.snapshot_to_tabs();
        assert!(app.fid_frame.ends_with("pcbforge-snapshot.png"));
        assert_eq!(app.fid_frame, app.place_frame);
        assert!(std::path::Path::new(&app.fid_frame).is_file());
    }

    /// FLD-12: click-to-place appends an expected fiducial to the layout and a
    /// matching search marker, keeping the layout string as source of truth.
    #[test]
    fn click_to_place_appends_an_expected_fiducial() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.fid_layout = "10,10; 60,10".into();
        app.sync_fid_markers();
        assert_eq!(app.fid_search.len(), 2);

        app.add_expected_fiducial(60.0, 60.0);
        assert_eq!(app.fid_search.len(), 3, "a 3rd marker appeared");
        assert_eq!(app.fid_search[2], (60.0, 60.0), "seeded at the click");
        assert!(
            app.fid_layout.contains("60.0,60.0"),
            "layout carries the added point: {}",
            app.fid_layout
        );

        // Removal shrinks the set (fixes the add-only pile-up): drop the middle
        // marker and the aligned layout token + search/found entries go with it.
        app.fid_layout = "10,10; 60,10; 60,60".into();
        app.sync_fid_markers();
        // Fine-tune the 3rd marker via drag, so we can prove removal keeps the
        // *other* markers' dragged positions aligned by index.
        app.fid_search[2] = (61.5, 59.0);
        app.remove_expected_fiducial(1); // remove the (60,10) middle one
        assert_eq!(app.fid_search.len(), 2, "one fewer marker");
        assert!(
            !app.fid_layout.contains("60,10"),
            "removed token is gone: {}",
            app.fid_layout
        );
        assert!(app.fid_layout.contains("10,10") && app.fid_layout.contains("60,60"));
        assert_eq!(
            app.fid_search[1],
            (61.5, 59.0),
            "the survivor's dragged position stayed aligned to its token"
        );

        // Appending onto an empty layout doesn't produce a leading separator.
        app.fid_layout = String::new();
        app.fid_search.clear();
        app.fid_found.clear();
        app.add_expected_fiducial(5.0, 7.0);
        assert_eq!(app.fid_layout.trim_start(), "5.0,7.0");
    }

    /// FLD-12: the selected profile flows into detection. A backlit frame
    /// (bright dots on a dark field) is found with the Backlit profile but the
    /// dark-dot matcher does not lock onto it — proving the selector is wired.
    #[test]
    fn profile_selector_changes_detection_polarity() {
        let ppm = 10.0;
        let holes = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
        // Bright blobs on a dark field: inverted polarity vs a drilled hole.
        let img = image::GrayImage::from_fn(700, 700, |x, y| {
            let mut v = 40.0;
            for (mx, my) in holes {
                let (cx, cy) = (mx * ppm, my * ppm);
                if (((x as f64) - cx).powi(2) + ((y as f64) - cy).powi(2)).sqrt() < 0.5 * ppm {
                    v += 170.0;
                }
            }
            image::Luma([v as u8])
        });

        let backlit = fiducial::check_frame(
            &img,
            &holes,
            ppm,
            &crate::fiducial::ProfileKind::Backlit.to_profile(1.0),
            2.0,
        );
        assert_eq!(backlit.tally.0, 3, "backlit finds the bright blobs");

        let darkdot = fiducial::check_frame(
            &img,
            &holes,
            ppm,
            &crate::fiducial::ProfileKind::DarkDot.to_profile(1.0),
            2.0,
        );
        assert!(
            darkdot.tally.0 < 3,
            "dark-dot matcher does not strongly lock the bright blobs: {:?}",
            darkdot.rows
        );
    }

    /// UI-2: the AR overlay blends the registered design over a frame. With a
    /// homography mapping design-mm → px, a copper region lands (tinted) at the
    /// mapped pixel; with the overlay off, the frame stays untouched gray.
    #[test]
    fn ar_overlay_projects_design_through_the_homography() {
        use nalgebra::Matrix3;
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        // A single 4 mm copper square centered at design (10,10) mm.
        let mm = pcb_core::NM_PER_MM;
        let sq = pcb_core::Poly {
            outer: vec![
                pcb_core::P::new(8 * mm, 8 * mm),
                pcb_core::P::new(12 * mm, 8 * mm),
                pcb_core::P::new(12 * mm, 12 * mm),
                pcb_core::P::new(8 * mm, 12 * mm),
            ],
            holes: vec![],
        };
        app.ar_copper = vec![sq];
        app.ar_show_copper = true;
        app.ar_show_board = false;
        app.ar_show_ablate = false;
        // Pure 5 px/mm scale: design (10,10) mm → px (50,50).
        app.fid_homography = Some(vision::Homography {
            matrix: Matrix3::new(5.0, 0.0, 0.0, 0.0, 5.0, 0.0, 0.0, 0.0, 1.0),
            residuals: vec![],
            rms: 0.0,
        });

        let gray = image::GrayImage::from_pixel(200, 200, image::Luma([120]));
        let over = app.compose_ar(&gray);
        let at = |x: usize, y: usize| over.pixels[y * 200 + x];
        // The copper square maps to px (40,40)..(60,60); its outline (left edge
        // x=40) is crisply tinted, and the interior is at least softly filled.
        assert!(at(40, 50).r() > 150, "copper outline at the mapped edge");
        assert!(at(50, 50).r() > 120, "copper interior softly filled");
        assert_eq!(
            at(150, 150),
            Color32::from_gray(120),
            "far corner untouched"
        );

        // A disabled layer leaves the frame gray.
        app.ar_show_copper = false;
        let plain = app.compose_ar(&gray);
        assert_eq!(plain.pixels[50 * 200 + 50], Color32::from_gray(120));
    }

    /// Place drag tracks the cursor in pixel space, even under perspective: a
    /// drag of (dpx, dpy) frame pixels shifts the pivot's *projected pixel* by
    /// exactly that — so the overlay follows the mouse over the image instead of
    /// sliding along the tilted plane.
    #[test]
    fn place_drag_tracks_cursor_under_perspective() {
        use nalgebra::Point2;
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        // A keystone homography (bed-mm → px): top edge narrower than bottom.
        let corr = [
            (Point2::new(0.0, 0.0), Point2::new(180.0, 110.0)),
            (Point2::new(60.0, 0.0), Point2::new(460.0, 110.0)),
            (Point2::new(60.0, 50.0), Point2::new(520.0, 380.0)),
            (Point2::new(0.0, 50.0), Point2::new(120.0, 380.0)),
        ];
        app.fid_homography = Some(vision::fit_homography(&corr).unwrap());
        app.place_px_per_mm = 8.0;
        app.place_tx_mm = 30.0;
        app.place_ty_mm = 25.0;

        let pivot = |a: &ConsoleApp| {
            a.fid_homography
                .as_ref()
                .unwrap()
                .apply(Point2::new(a.place_tx_mm, a.place_ty_mm))
        };
        let before = pivot(&app);
        app.drag_place_px(12.0, -7.0);
        let after = pivot(&app);
        assert!(
            (after.x - (before.x + 12.0)).abs() < 1e-6,
            "x pixel tracked: {} vs {}",
            after.x,
            before.x + 12.0
        );
        assert!(
            (after.y - (before.y - 7.0)).abs() < 1e-6,
            "y pixel tracked: {} vs {}",
            after.y,
            before.y - 7.0
        );
    }

    /// Without a homography the drag is the plain uniform-scale move (pixel
    /// delta ÷ px-per-mm added to the bed-mm translation).
    #[test]
    fn place_drag_uniform_without_homography() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.fid_homography = None;
        app.place_px_per_mm = 10.0;
        app.place_tx_mm = 5.0;
        app.place_ty_mm = 5.0;
        app.drag_place_px(20.0, -30.0);
        assert!((app.place_tx_mm - (5.0 + 2.0)).abs() < 1e-9);
        assert!((app.place_ty_mm - (5.0 - 3.0)).abs() < 1e-9);
    }

    /// Double-sided: on the back, the expected fiducial positions are the
    /// design layout mirrored about its centerline with the beam entry→exit
    /// offset applied — matching the kernel `back_expected_fiducial_mm`.
    #[test]
    fn back_side_expected_points_mirror_and_offset() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.fid_layout = "10,10; 60,10; 10,60; 60,60".into();
        app.board_thickness_mm = 1.6;
        app.focal_mm = 70.0;

        // Front: expected == design.
        assert_eq!(app.side, Side::Front);
        assert_eq!(
            app.expected_points(),
            vec![(10.0, 10.0), (60.0, 10.0), (10.0, 60.0), (60.0, 60.0)]
        );

        // Back: mirror about the layout centroid (x=35) + f-theta offset.
        app.set_side(Side::Back);
        let axis = cam::flip::MirrorAxis::VerticalX { x_mm: 35.0 };
        let field = cam::flip::FieldParams {
            scan_center_mm: (35.0, 35.0),
            thickness_mm: 1.6,
            focal_mm: 70.0,
        };
        let want: Vec<(f64, f64)> = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0), (60.0, 60.0)]
            .into_iter()
            .map(|(x, y)| cam::flip::back_expected_fiducial_mm(x, y, &axis, &field))
            .collect();
        let got = app.expected_points();
        assert_eq!(got.len(), 4);
        for (g, w) in got.iter().zip(&want) {
            assert!(
                (g.0 - w.0).abs() < 1e-9 && (g.1 - w.1).abs() < 1e-9,
                "{g:?} vs {w:?}"
            );
        }
        // The left/right holes swapped sides (mirror), so hole #0 (was x=10) now
        // sits right of center.
        assert!(
            got[0].0 > 35.0,
            "left hole mirrored to the right: {:?}",
            got[0]
        );
    }

    /// An explicit scan-center override changes the back-side parallax: with
    /// the lens axis at a fiducial, that hole stops shifting while the others
    /// shift more — matching the physics (no parallax on the optical axis).
    #[test]
    fn scan_center_override_moves_the_parallax_origin() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.fid_layout = "10,10; 60,10; 10,60; 60,60".into();
        app.set_side(Side::Back);

        // Auto (centroid 35,35): every hole shifts off its plain mirror image.
        let auto_pts = app.expected_points();

        // Override: lens axis exactly on the first hole (10,10) → that hole's
        // exit == entry, so its expected back position is the *pure* mirror.
        app.scan_center_auto = false;
        app.scan_center_mm = (10.0, 10.0);
        let over_pts = app.expected_points();
        let mirror_only = |x: f64, y: f64| (2.0 * 35.0 - x, y); // axis stays at centroid
        let (mx, my) = mirror_only(10.0, 10.0);
        assert!(
            (over_pts[0].0 - mx).abs() < 1e-9 && (over_pts[0].1 - my).abs() < 1e-9,
            "on-axis hole has no parallax: {:?} vs ({mx},{my})",
            over_pts[0]
        );
        assert_ne!(
            auto_pts[0], over_pts[0],
            "moving the scan center changes the expectation"
        );
    }

    /// Switching side clears the per-side caches so nothing from the front
    /// bleeds into the back view.
    #[test]
    fn set_side_resets_per_side_caches() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.fid_layout = "10,10; 60,10".into();
        app.sync_fid_markers();
        app.ar_copper = vec![pcb_core::Poly::default()];
        assert!(!app.fid_search.is_empty());
        app.set_side(Side::Back);
        assert!(app.fid_search.is_empty(), "markers cleared on side switch");
        assert!(app.ar_copper.is_empty(), "AR design cleared on side switch");
        assert_eq!(app.side, Side::Back);
    }

    /// The Job tab lays out with the Back side selected (the back form renders).
    #[test]
    fn back_side_job_tab_lays_out() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.set_side(Side::Back);
        app.tab = CentralTab::Job;
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
        assert!(!out.shapes.is_empty(), "back-side job tab produced shapes");
    }

    /// A second frame after a status refresh still lays out (state survives).
    #[test]
    fn app_survives_refresh_and_relayout() {
        let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
        app.refresh();
        let ctx = Context::default();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
        }
    }
}
