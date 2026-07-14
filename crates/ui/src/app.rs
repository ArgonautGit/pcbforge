//! The operator console (UI-1): an egui app with a board/stage status panel, an
//! actions panel that **shells the existing `pcbforge` CLI verbs** (the CLI
//! stays the API — the console never re-implements engine logic), a rasterized
//! job-preview panel, a log pane, and a stubbed camera panel (pending VIS-1).
//!
//! The whole UI is egui-only so it computes frames headlessly and is testable
//! without a display; the `eframe` window is a thin feature-gated wrapper
//! (`src/main.rs`, `--features native`).

use std::path::PathBuf;

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

    // emit form
    emit_copper: String,
    emit_outline: String,
    emit_lbrn2: String,
    offset_mm: f64,

    // job preview
    preview_tex: Option<TextureHandle>,
    preview_note: String,

    // fiducial check
    fid_frame: String,
    fid_layout: String,
    fid_px_per_mm: f64,
    fid_diameter_mm: f64,
    fid_search_mm: f64,
    fid_note: String,
    fid_rows: Vec<FidRow>,
    fid_measured_ppm: Option<f64>,
    // draggable search markers over the live frame
    fid_frame_img: Option<image::GrayImage>,
    fid_frame_tex: Option<TextureHandle>,
    fid_search: Vec<(f64, f64)>,
    fid_found: Vec<Option<(f64, f64)>>,
    fid_drag: Option<usize>,

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
            emit_copper: String::new(),
            emit_outline: String::new(),
            emit_lbrn2: "job.lbrn2".into(),
            offset_mm: 0.0,
            preview_tex: None,
            preview_note: "Set a copper Gerber and click “Render preview”.".into(),
            fid_frame: String::new(),
            // Default to the operator's drilled-hole L-layout (field photo).
            fid_layout: "10,10; 60,10; 10,60".into(),
            fid_px_per_mm: 10.0,
            fid_diameter_mm: 1.0,
            fid_search_mm: 2.0,
            fid_note: "Load a frame, drag each marker near its hole, then Check.".into(),
            fid_rows: Vec::new(),
            fid_measured_ppm: None,
            fid_frame_img: None,
            fid_frame_tex: None,
            fid_search: Vec::new(),
            fid_found: Vec::new(),
            fid_drag: None,
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
        }
    }

    /// Re-read the status snapshot from the DB.
    pub fn refresh(&mut self) {
        self.status = status::snapshot(&self.db_path);
    }

    /// Shell `pcbforge <args>` and fold its output into the log, then refresh
    /// status (a verb may have advanced a stage). Synchronous: the console
    /// blocks for the duration of the verb. Returns the appended lines (for
    /// tests).
    pub fn run_verb(&mut self, args: &[String]) -> Vec<LogLine> {
        let lines = run_capture(&self.cli_cmd, args);
        self.log.extend(lines.iter().cloned());
        // Cap the log so a long session doesn't grow unbounded.
        if self.log.len() > 500 {
            let drop = self.log.len() - 500;
            self.log.drain(0..drop);
        }
        self.refresh();
        lines
    }

    /// (Re)build the preview texture from the emit form's Gerber paths.
    pub fn render_preview(&mut self, ctx: &Context) {
        match preview_image(&self.emit_copper, &self.emit_outline, self.offset_mm) {
            Ok((img, note)) => {
                self.preview_tex =
                    Some(ctx.load_texture("job-preview", img, TextureOptions::NEAREST));
                self.preview_note = note;
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
        // Seed markers from the design positions (bed mm) unless already sized.
        if self.fid_search.len() != design.len() {
            self.fid_search = design;
            self.fid_found = vec![None; self.fid_search.len()];
        }
        let (w, h) = (img.width() as usize, img.height() as usize);
        let color = ColorImage {
            size: [w, h],
            pixels: img.pixels().map(|p| Color32::from_gray(p[0])).collect(),
        };
        self.fid_frame_tex = Some(ctx.load_texture("fid-frame", color, TextureOptions::NEAREST));
        self.fid_frame_img = Some(img);
        self.fid_note = "drag each ✛ near its hole, then Check".into();
    }

    /// Detect around the current (draggable) search markers and record the
    /// found positions, summary rows, and measured scale.
    pub fn render_fiducials(&mut self, ctx: &Context) {
        if self.fid_frame_img.is_none() {
            self.load_fid_frame(ctx);
        }
        let Some(frame) = &self.fid_frame_img else {
            return;
        };
        if self.fid_search.is_empty() {
            self.fid_note = "load a frame first".into();
            return;
        }
        let r = fiducial::check_frame(
            frame,
            &self.fid_search,
            self.fid_px_per_mm,
            self.fid_diameter_mm,
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
        let (_, _, ablate) = match job_shapes(&self.emit_copper, &self.emit_outline, self.offset_mm)
        {
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
            [0xf0, 0x50, 0x30],
            0.55,
        );
        self.place_note = format!(
            "placed at ({:.1}, {:.1}) mm, {:.0}°",
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

    /// Store a grabbed frame into the preview texture + cache.
    fn set_camera_frame(&mut self, ctx: &Context, gray: image::GrayImage) {
        let (w, h) = (gray.width() as usize, gray.height() as usize);
        let img = ColorImage {
            size: [w, h],
            pixels: gray.pixels().map(|p| Color32::from_gray(p[0])).collect(),
        };
        self.cam_tex = Some(ctx.load_texture("camera", img, TextureOptions::NEAREST));
        self.cam_note = format!("{w}×{h}");
        self.cam_last = Some(gray);
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
        if self.emit_copper.trim().is_empty() {
            self.log.push(LogLine {
                text: "emit: set a copper Gerber first".into(),
                err: true,
            });
            return;
        }
        let mut args: Vec<String> = vec![
            "emit".into(),
            "--copper".into(),
            crate::clean_path(&self.emit_copper),
            "--lbrn2".into(),
            crate::clean_path(&self.emit_lbrn2),
            "--offset-mm".into(),
            format!("{}", self.offset_mm),
        ];
        if !crate::clean_path(&self.emit_outline).is_empty() {
            args.push("--outline".into());
            args.push(crate::clean_path(&self.emit_outline));
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

        // Live frames come from the background capture thread (non-blocking).
        let ctx = ui.ctx().clone();
        self.pump_camera(&ctx);

        if let Some(tex) = &self.cam_tex {
            egui::ScrollArea::both().show(ui, |ui| {
                ui.add(
                    egui::Image::from_texture((tex.id(), tex.size_vec2()))
                        .fit_to_original_size(1.0),
                );
            });
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
            let img = egui::Image::from_texture((tex.id(), tex.size_vec2()))
                .fit_to_original_size(1.0)
                .sense(egui::Sense::drag());
            let resp = ui.add(img);
            if resp.dragged() {
                let d = resp.drag_delta();
                // Screen points ≈ frame pixels at native display; px → mm.
                self.place_tx_mm += d.x as f64 / self.place_px_per_mm;
                self.place_ty_mm += d.y as f64 / self.place_px_per_mm;
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
            egui::ScrollArea::both().show(ui, |ui| {
                ui.add(
                    egui::Image::from_texture((tex.id(), tex.size_vec2()))
                        .fit_to_original_size(1.0),
                );
            });
        } else {
            ui.weak("(no preview rendered — see the Actions panel)");
        }
    }

    fn fiducial_view(&mut self, ui: &mut egui::Ui) {
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
        let Some(tex) = &self.fid_frame_tex else {
            ui.weak("(load a frame to place markers)");
            return;
        };
        let (tw, th) = (tex.size()[0] as f32, tex.size()[1] as f32);
        let resp = ui.add(
            egui::Image::from_texture((tex.id(), egui::vec2(tw, th)))
                .fit_to_original_size(1.0)
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
        let to_mm = |p: egui::Pos2| {
            let ix = (p.x - rect.min.x) / rect.width() * tw;
            let iy = (p.y - rect.min.y) / rect.height() * th;
            (
                ix as f64 / self.fid_px_per_mm,
                iy as f64 / self.fid_px_per_mm,
            )
        };

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
