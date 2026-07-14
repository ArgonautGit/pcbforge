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
    Fiducials,
}

/// The console application state.
pub struct ConsoleApp {
    /// Path to the orchestra SQLite DB (`--db`).
    pub db_path: PathBuf,
    /// The `pcbforge` binary to shell (PATH name or absolute path).
    pub pcbforge_bin: String,
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
    fid_tex: Option<TextureHandle>,
    fid_note: String,
    fid_rows: Vec<FidRow>,
}

impl ConsoleApp {
    /// New console over `db_path`, shelling `pcbforge_bin`. Reads an initial
    /// status snapshot immediately.
    pub fn new(db_path: impl Into<PathBuf>, pcbforge_bin: impl Into<String>) -> Self {
        let db_path = db_path.into();
        let status = status::snapshot(&db_path);
        Self {
            db_path,
            pcbforge_bin: pcbforge_bin.into(),
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
            fid_tex: None,
            fid_note: "Load a frame (camera grab / photo) and click “Check fiducials”.".into(),
            fid_rows: Vec::new(),
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
        let lines = run_capture(&self.pcbforge_bin, args);
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

    /// Run VIS-4 fiducial detection on the loaded frame and build the overlay.
    pub fn render_fiducials(&mut self, ctx: &Context) {
        let expected = match fiducial::parse_layout(&self.fid_layout) {
            Ok(e) => e,
            Err(e) => {
                self.fid_tex = None;
                self.fid_rows.clear();
                self.fid_note = format!("layout: {e}");
                return;
            }
        };
        match fiducial::check(
            &self.fid_frame,
            &expected,
            self.fid_px_per_mm,
            self.fid_diameter_mm,
            self.fid_search_mm,
        ) {
            Ok(r) => {
                let (s, w, m) = r.tally;
                self.fid_note = format!("{s} strong, {w} weak, {m} missed");
                self.fid_rows = r.rows;
                self.fid_tex =
                    Some(ctx.load_texture("fiducials", r.overlay, TextureOptions::NEAREST));
            }
            Err(e) => {
                self.fid_tex = None;
                self.fid_rows.clear();
                self.fid_note = e;
            }
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
        ui.collapsing("📷 Camera", |ui| {
            ui.weak("Live camera pending VIS-1 (capture module).");
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), 120.0),
                egui::Sense::hover(),
            );
            ui.painter().rect_filled(rect, 4.0, Color32::from_gray(30));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "no camera",
                egui::FontId::proportional(14.0),
                Color32::from_gray(120),
            );
        });
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
            self.emit_copper.clone(),
            "--lbrn2".into(),
            self.emit_lbrn2.clone(),
            "--offset-mm".into(),
            format!("{}", self.offset_mm),
        ];
        if !self.emit_outline.trim().is_empty() {
            args.push("--outline".into());
            args.push(self.emit_outline.clone());
        }
        self.run_verb(&args);
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.tab, CentralTab::Job, "🖼 Job preview");
            ui.selectable_value(&mut self.tab, CentralTab::Fiducials, "🎯 Fiducial check");
        });
        ui.separator();
        match self.tab {
            CentralTab::Job => self.job_view(ui),
            CentralTab::Fiducials => self.fiducial_view(ui),
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
                ui.label("px per mm");
                ui.add(
                    egui::DragValue::new(&mut self.fid_px_per_mm)
                        .speed(0.1)
                        .range(0.1..=1000.0),
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
            if ui.button("🎯 Check fiducials").clicked() {
                let ctx = ui.ctx().clone();
                self.render_fiducials(&ctx);
            }
            ui.label(egui::RichText::new(&self.fid_note).weak());
        });
        ui.weak("Frame is a saved camera grab or photo; becomes the live feed with VIS-1.");
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
        if let Some(tex) = &self.fid_tex {
            egui::ScrollArea::both().show(ui, |ui| {
                ui.add(
                    egui::Image::from_texture((tex.id(), tex.size_vec2()))
                        .fit_to_original_size(1.0),
                );
            });
        } else {
            ui.weak("(no frame checked yet)");
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

/// Shell `bin args`, capturing stdout (info) and stderr (warn) as log lines
/// plus a header and an exit-status footer. A spawn failure is one error line.
pub fn run_capture(bin: &str, args: &[String]) -> Vec<LogLine> {
    let mut out = vec![LogLine {
        text: format!("$ {bin} {}", args.join(" ")),
        err: false,
    }];
    match std::process::Command::new(bin).args(args).output() {
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
            text: format!("failed to run `{bin}`: {e}"),
            err: true,
        }),
    }
    out
}

/// Build a preview image from Gerber paths: invert copper → non-copper (the
/// same geometry `emit` burns) and rasterize board/copper/ablate. This is a
/// *view* computation (pure geometry), not engine logic — the actual job is
/// still produced by shelling `pcbforge emit`. Returns the image and a caption.
pub fn preview_image(
    copper_path: &str,
    outline_path: &str,
    offset_mm: f64,
) -> Result<(ColorImage, String), String> {
    if copper_path.trim().is_empty() {
        return Err("set a copper Gerber path first".into());
    }
    let copper = ingest::gerber::load_gerber(std::path::Path::new(copper_path.trim()))
        .map_err(|e| format!("copper: {}", e.msg))?
        .polys;
    let board = if outline_path.trim().is_empty() {
        cam::noncopper::board_region_bbox(&copper, NM_PER_MM) // 1 mm margin
    } else {
        let o = ingest::gerber::load_gerber(std::path::Path::new(outline_path.trim()))
            .map_err(|e| format!("outline: {}", e.msg))?
            .polys;
        cam::noncopper::board_region_from_outline(&o)
    };
    if board.is_empty() {
        return Err("empty board region".into());
    }
    let offset_nm = (offset_mm * NM_PER_MM as f64).round() as Nm;
    let ablate = cam::noncopper::noncopper(&board, &copper, offset_nm);

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
        let out = run_capture("echo", &["hello".into()]);
        assert!(out.iter().any(|l| l.text == "hello" && !l.err));
        assert!(out.iter().any(|l| l.text.starts_with("[exit 0]")));
    }

    #[test]
    fn run_capture_reports_spawn_failure() {
        let out = run_capture("definitely-not-a-real-binary-xyz", &[]);
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
        let mut app = ConsoleApp::new(tmp_db(), "pcbforge");
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
        let mut app = ConsoleApp::new(tmp_db(), "pcbforge");
        app.tab = CentralTab::Fiducials;
        let ctx = Context::default();
        let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
        assert!(!out.shapes.is_empty(), "fiducial tab must render");
    }

    /// A second frame after a status refresh still lays out (state survives).
    #[test]
    fn app_survives_refresh_and_relayout() {
        let mut app = ConsoleApp::new(tmp_db(), "pcbforge");
        app.refresh();
        let ctx = Context::default();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
        }
    }
}
