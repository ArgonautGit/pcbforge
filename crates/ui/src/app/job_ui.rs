use super::*;

impl ConsoleApp {
    pub(super) fn status_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Status");
        if let Some(err) = &self.runtime.status.error {
            ui.colored_label(Color32::from_rgb(0xd0, 0x60, 0x50), err);
        }
        ui.separator();
        ui.label(egui::RichText::new("Stages").strong());
        for s in &self.runtime.status.stages {
            let here = self.runtime.status.boards.iter().any(|b| &b.stage == s);
            if here {
                ui.colored_label(Color32::from_rgb(0x60, 0xb0, 0x70), format!("▶ {s}"));
            } else {
                ui.label(format!("   {s}"));
            }
        }
        ui.separator();
        ui.label(egui::RichText::new("Boards").strong());
        if self.runtime.status.boards.is_empty() {
            ui.weak("none — run `next` to admit a board");
        }
        for b in &self.runtime.status.boards {
            let reg = if b.registered {
                "✔ registered"
            } else {
                "unregistered"
            };
            ui.monospace(format!("#{} [{}] {}", b.id, b.stage, reg));
            ui.weak(short_path(&b.design));
        }
    }

    /// Export the copper + outline Gerbers from the KiCad project by shelling
    /// `pcbforge gerbers` in the **background** (the export can take a second or
    /// two — the window must not freeze). The output paths are deterministic
    /// (`<board dir>/pcbforge-gerbers/{copper,outline}.gbr`), so the fields are
    /// filled immediately; the files appear when the background job finishes,
    /// whose progress/errors stream to the Log.
    ///
    /// The drill export is queued behind it (`pending_drills`) rather than
    /// shelled here: only one verb runs at a time, so a second `run_verb` call
    /// would be refused and leave the drill fields pointing at files nobody
    /// wrote.
    pub(super) fn gerbers_from_kicad(&mut self) {
        let proj = crate::clean_path(&self.job.kicad_project);
        if proj.trim().is_empty() {
            self.runtime.log.push(LogLine {
                text: "gerbers: set a KiCad project path first".into(),
                err: true,
            });
            return;
        }
        let (copper_layer, outline_layer) = match self.job.side {
            Side::Front => ("F.Cu", "Edge.Cuts"),
            Side::Back => ("B.Cu", "Edge.Cuts"),
        };
        // Resolve the board just to place the output dir; the CLI re-resolves it.
        let board = match ingest::kicad_cli::resolve_board(std::path::Path::new(&proj)) {
            Ok(b) => b,
            Err(e) => {
                self.runtime.log.push(LogLine {
                    text: format!("gerbers: {e}"),
                    err: true,
                });
                return;
            }
        };
        let out_dir = board
            .parent()
            .map(|p| p.join("pcbforge-gerbers"))
            .unwrap_or_else(|| PathBuf::from("pcbforge-gerbers"));
        let copper = out_dir.join("copper.gbr").display().to_string();
        let outline = out_dir.join("outline.gbr").display().to_string();
        match self.job.side {
            Side::Front => {
                self.job.emit_copper = copper;
                self.job.emit_outline = outline;
            }
            Side::Back => {
                self.job.back_copper = copper;
                self.job.back_outline = outline;
            }
        }
        let started = self.run_verb(&[
            "gerbers".into(),
            "--project".into(),
            proj.clone(),
            "--out".into(),
            out_dir.display().to_string(),
            "--copper-layer".into(),
            copper_layer.into(),
            "--outline-layer".into(),
            outline_layer.into(),
        ]);
        if started {
            // Same out_dir the Gerbers are going to — never re-resolved later,
            // so the drill paths can't point somewhere else.
            self.runtime.pending_drills = Some((proj, out_dir.display().to_string()));
        }
        self.job.preview_note =
            format!("exporting {copper_layer} + {outline_layer} from KiCad… (see Log)");
    }

    /// The side panel has no scrolling of its own, and the drill + etch recipes
    /// together are taller than a short window — without this the controls at
    /// the bottom are simply unreachable.
    pub(super) fn actions_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("actions-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| self.actions_controls(ui));
    }

    fn actions_controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("Actions");
        ui.label(egui::RichText::new("These shell the `pcbforge` CLI.").weak());
        ui.separator();

        egui::Grid::new("kicad-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                let l = ui.label("KiCad project");
                ui.add(
                    egui::TextEdit::singleline(&mut self.job.kicad_project).desired_width(180.0),
                )
                .labelled_by(l.id)
                .on_hover_text(
                    "A .kicad_pcb file or a project directory containing one. \
                         ⚙ exports its copper + outline Gerbers via kicad-cli.",
                );
                ui.end_row();
            });
        if ui
            .button("⚙ Gerbers from KiCad")
            .on_hover_text(
                "Run kicad-cli to export copper.gbr + outline.gbr and fill the fields below. \
                 Once that finishes it also exports pth.drl + npth.drl into the same \
                 directory and fills the drill .drl field.",
            )
            .clicked()
        {
            self.gerbers_from_kicad();
        }

        // Drilling and etching are two recipes for one board, so they sit side
        // by side under the shared KiCad project above. Collapsible (open by
        // default) so the etch settings are one click away while drilling is
        // not the job at hand.
        ui.separator();
        egui::CollapsingHeader::new("⌀ Drill")
            .default_open(true)
            .show(ui, |ui| self.drill_settings(ui));
        ui.separator();

        ui.label(egui::RichText::new("Etch").strong());
        egui::Grid::new("emit-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                let l = ui.label("copper .gbr");
                ui.add(egui::TextEdit::singleline(&mut self.job.emit_copper).desired_width(180.0))
                    .labelled_by(l.id);
                ui.end_row();
                let l = ui.label("outline .gbr");
                ui.add(egui::TextEdit::singleline(&mut self.job.emit_outline).desired_width(180.0))
                    .labelled_by(l.id);
                ui.end_row();
                ui.label("out .lbrn2");
                ui.add(egui::TextEdit::singleline(&mut self.job.emit_lbrn2).desired_width(180.0));
                ui.end_row();
                ui.label("offset mm");
                ui.add(
                    egui::DragValue::new(&mut self.job.offset_mm)
                        .speed(0.005)
                        .range(0.0..=10.0),
                );
                ui.end_row();
                let l = ui.label("speed mm/s");
                ui.add(
                    egui::DragValue::new(&mut self.job.speed_mm_s)
                        .speed(10.0)
                        .range(1.0..=15000.0),
                )
                .labelled_by(l.id);
                ui.end_row();
                let l = ui.label("frequency kHz");
                ui.add(
                    egui::DragValue::new(&mut self.job.frequency_khz)
                        .speed(1.0)
                        .range(1.0..=4000.0),
                )
                .labelled_by(l.id)
                .on_hover_text("pulse repetition rate");
                ui.end_row();
                let l = ui.label("Q-pulse ns");
                ui.add(
                    egui::DragValue::new(&mut self.job.pulse_ns)
                        .speed(1.0)
                        .range(0..=500),
                )
                .labelled_by(l.id)
                .on_hover_text("MOPA Q-pulse width; 0 = source default (omits QPulseWidth)");
                ui.end_row();
                let l = ui.label("interval mm");
                ui.add(
                    egui::DragValue::new(&mut self.job.interval_mm)
                        .speed(0.001)
                        .range(0.001..=1.0),
                )
                .labelled_by(l.id)
                .on_hover_text("fill line spacing (hatch interval)");
                ui.end_row();
                let l = ui.label("passes");
                ui.add(
                    egui::DragValue::new(&mut self.job.passes)
                        .speed(1.0)
                        .range(1..=1000),
                )
                .labelled_by(l.id);
                ui.end_row();
                ui.checkbox(&mut self.job.wobble, "wobble").on_hover_text(
                    "Spiral the beam along the scan to widen the effective line. \
                     OFF by default — the export writes wobbleEnable=0 explicitly \
                     so the device profile can't re-enable it.",
                );
                ui.horizontal(|ui| {
                    if self.job.wobble {
                        ui.add(
                            egui::DragValue::new(&mut self.job.wobble_step_mm)
                                .speed(0.005)
                                .range(0.0..=2.0)
                                .prefix("step "),
                        )
                        .on_hover_text("wobble step along the path, mm (0 = device default)");
                        ui.add(
                            egui::DragValue::new(&mut self.job.wobble_size_mm)
                                .speed(0.005)
                                .range(0.0..=2.0)
                                .prefix("size "),
                        )
                        .on_hover_text("wobble diameter, mm (0 = device default)");
                    } else {
                        ui.weak("off");
                    }
                });
                ui.end_row();
            });
        ui.weak("Recipe (speed / Q-pulse / interval / passes / wobble) applies to both this Emit and Place's “Etch here”.");

        // Double-sided (ORC-6): side selector + back-side inputs.
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("side");
            let mut side = self.job.side;
            ui.selectable_value(&mut side, Side::Front, "Front");
            ui.selectable_value(&mut side, Side::Back, "Back");
            self.set_side(side);
        });
        if self.job.side == Side::Back {
            egui::Grid::new("back-form")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("back copper .gbr");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.job.back_copper).desired_width(180.0),
                    );
                    ui.end_row();
                    ui.label("back outline .gbr");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.job.back_outline).desired_width(180.0),
                    );
                    ui.end_row();
                    ui.label("thickness mm");
                    ui.add(
                        egui::DragValue::new(&mut self.job.board_thickness_mm)
                            .speed(0.05)
                            .range(0.0..=10.0),
                    );
                    ui.end_row();
                    ui.label("focal mm");
                    ui.add(
                        egui::DragValue::new(&mut self.job.focal_mm)
                            .speed(1.0)
                            .range(1.0..=1000.0),
                    );
                    ui.end_row();
                    ui.label("scan center");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.job.scan_center_auto, "auto")
                            .on_hover_text(
                                "Use the fiducial-layout centroid as the lens axis. \
                                 Uncheck and enter the measured field center once known \
                                 (VIS-3 will calibrate it).",
                            );
                        if !self.job.scan_center_auto {
                            ui.add(
                                egui::DragValue::new(&mut self.job.scan_center_mm.0)
                                    .speed(0.5)
                                    .prefix("x "),
                            );
                            ui.add(
                                egui::DragValue::new(&mut self.job.scan_center_mm.1)
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
            if ui.button("▶ Emit field-warped .lbrn2").clicked() {
                self.emit_clicked();
            }
        });

        ui.separator();
        ui.weak(
            "With an accepted step 1 (Camera lens) + step 3 (Laser field) map the export field-warps every edge; without one it emits unwarped (warned in the log).",
        );
        if ui.button("⏭ Next stage (bring-up)").clicked() {
            self.run_verb(&["next".into(), "--bringup-stubs".into()]);
        }
        ui.separator();
        ui.weak("Live camera → the “📷 Camera” tab.");
    }

    /// The wobble recipe args shared by Emit and Place's "Etch here" (empty
    /// when wobble is off — the CLI default already writes wobbleEnable=0).
    pub(super) fn wobble_args(&self) -> Vec<String> {
        if !self.job.wobble {
            return Vec::new();
        }
        vec![
            "--wobble".into(),
            "--wobble-step-mm".into(),
            format!("{}", self.job.wobble_step_mm),
            "--wobble-size-mm".into(),
            format!("{}", self.job.wobble_size_mm),
        ]
    }

    pub(super) fn emit_clicked(&mut self) {
        let (copper, outline) = self.active_gerbers();
        let (copper, outline) = (crate::clean_path(copper), crate::clean_path(outline));
        if copper.is_empty() {
            let which = match self.job.side {
                Side::Front => "copper Gerber",
                Side::Back => "back copper Gerber",
            };
            self.runtime.log.push(LogLine {
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
            crate::clean_path(&self.job.emit_lbrn2),
            "--offset-mm".into(),
            format!("{}", self.job.offset_mm),
            "--speed-mm-s".into(),
            format!("{}", self.job.speed_mm_s),
            "--frequency-khz".into(),
            format!("{}", self.job.frequency_khz),
            "--pulse-ns".into(),
            format!("{}", self.job.pulse_ns),
            "--interval-mm".into(),
            format!("{}", self.job.interval_mm),
            "--passes".into(),
            format!("{}", self.job.passes),
        ];
        args.extend(self.wobble_args());
        // Field-warp when a usable calibration + map file exist; otherwise
        // emit unwarped with a warning (operator's call — the machine's own
        // correction is then the only field compensation).
        let field_path = self.field_map_path();
        if self.has_usable_field_cal() && field_path.exists() {
            args.push("--field-map".into());
            args.push(field_path.to_string_lossy().into_owned());
        } else {
            self.runtime.log.push(LogLine {
                text:
                    "emit: no accepted step 1 (Camera lens) + step 3 (Laser field) calibration — \
                       emitting UNWARPED geometry"
                        .into(),
                err: true,
            });
        }
        if !outline.is_empty() {
            args.push("--outline".into());
            args.push(outline);
        }
        // Back side: mirror the design in X to match the flipped board.
        if self.job.side == Side::Back {
            args.push("--mirror-x".into());
        }
        self.run_verb(&args);
    }

    pub(super) fn preview_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.runtime.tab, CentralTab::Job, "🖼 Job preview");
            ui.selectable_value(&mut self.runtime.tab, CentralTab::Camera, "📷 Camera");
            ui.selectable_value(&mut self.runtime.tab, CentralTab::Calibrate, "🎯 Calibrate");
            ui.selectable_value(
                &mut self.runtime.tab,
                CentralTab::Fiducials,
                "◎ Fiducial check",
            );
            ui.selectable_value(
                &mut self.runtime.tab,
                CentralTab::Place,
                "✋ Place on board",
            );
        });
        ui.separator();
        match self.runtime.tab {
            CentralTab::Job => self.job_view(ui),
            CentralTab::Camera => self.camera_view(ui),
            CentralTab::Calibrate => self.calibrate_view(ui),
            CentralTab::Fiducials => self.fiducial_view(ui),
            CentralTab::Place => self.place_view(ui),
        }
    }

    pub(super) fn job_view(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new(&self.job.preview_note).weak());
        if let Some(tex) = self.job.preview_tex.clone() {
            self.show_image(ui, "preview", &tex);
        } else {
            ui.weak("(no preview rendered — see the Actions panel)");
        }
    }

    pub(super) fn log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Log");
            if ui.button("clear").clicked() {
                self.runtime.log.clear();
            }
        });
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.runtime.log {
                    if line.err {
                        ui.colored_label(Color32::from_rgb(0xd0, 0x80, 0x60), &line.text);
                    } else {
                        ui.monospace(&line.text);
                    }
                }
            });
    }
}

/// Green when a calibration step is satisfied, amber otherwise.
pub(super) fn status_color(ok: bool) -> Color32 {
    if ok {
        Color32::from_rgb(0x50, 0xb0, 0x60)
    } else {
        Color32::from_rgb(0xe0, 0x90, 0x20)
    }
}
