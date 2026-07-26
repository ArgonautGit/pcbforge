use super::*;

impl ConsoleApp {
    pub(super) fn drill_view(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("drill-paths-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                let drl = ui.label("drill .drl");
                ui.add(egui::TextEdit::singleline(&mut self.drill.files).desired_width(240.0))
                    .labelled_by(drl.id)
                    .on_hover_text(
                        "Excellon drill file(s) for \"Emit drill holes\" — KiCad \
                         exports PTH and NPTH holes as two files; list both \
                         separated by ; to get every hole. \"⚙ Drills from \
                         KiCad\" fills this from the Actions-panel project.",
                    );
                ui.end_row();
                let drl_out = ui.label("drill out .lbrn2");
                ui.add(egui::TextEdit::singleline(&mut self.drill.out_lbrn2).desired_width(240.0))
                    .labelled_by(drl_out.id)
                    .on_hover_text(
                        "Where \"Emit drill holes\" writes the hole-geometry job — \
                         separate from the etch output so they never overwrite each \
                         other. A bare filename lands next to the drill file; the log \
                         prints the full path it wrote.",
                    );
                ui.end_row();
            });
        ui.separator();
        egui::Grid::new("drill-recipe-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                let l = ui.label("drill power %");
                ui.add(
                    egui::DragValue::new(&mut self.drill.power_pct)
                        .speed(1.0)
                        .range(0.0..=100.0),
                )
                .labelled_by(l.id)
                .on_hover_text(
                    "Beam power for the hole outlines. AblationParams::validate \
                     rejects 0 power, so a 0 here refuses the emit rather than \
                     writing a file that traces every hole with the beam off.",
                );
                ui.end_row();
                let l = ui.label("drill speed mm/s");
                ui.add(
                    egui::DragValue::new(&mut self.drill.speed_mm_s)
                        .speed(10.0)
                        .range(1.0..=15000.0),
                )
                .labelled_by(l.id);
                ui.end_row();
                let l = ui.label("drill frequency kHz");
                ui.add(
                    egui::DragValue::new(&mut self.drill.frequency_khz)
                        .speed(1.0)
                        .range(1.0..=4000.0),
                )
                .labelled_by(l.id)
                .on_hover_text("pulse repetition rate");
                ui.end_row();
                let l = ui.label("drill Q-pulse ns");
                ui.add(
                    egui::DragValue::new(&mut self.drill.pulse_ns)
                        .speed(1.0)
                        .range(0..=500),
                )
                .labelled_by(l.id)
                .on_hover_text("MOPA Q-pulse width; 0 = source default (omits QPulseWidth)");
                ui.end_row();
                let l = ui.label("drill passes");
                ui.add(
                    egui::DragValue::new(&mut self.drill.passes)
                        .speed(1.0)
                        .range(1..=1000),
                )
                .labelled_by(l.id)
                .on_hover_text("times the beam retraces each hole outline");
                ui.end_row();
                ui.checkbox(&mut self.drill.wobble, "drill wobble")
                    .on_hover_text(
                        "Spiral the beam along the outline to widen the effective \
                         line. OFF by default — the export writes wobbleEnable=0 \
                         explicitly so the device profile can't re-enable it.",
                    );
                ui.horizontal(|ui| {
                    if self.drill.wobble {
                        ui.add(
                            egui::DragValue::new(&mut self.drill.wobble_step_mm)
                                .speed(0.005)
                                .range(0.0..=2.0)
                                .prefix("step "),
                        )
                        .on_hover_text("wobble step along the path, mm (0 = device default)");
                        ui.add(
                            egui::DragValue::new(&mut self.drill.wobble_size_mm)
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
        ui.separator();
        ui.horizontal(|ui| {
            if ui
                .button("⚙ Drills from KiCad")
                .on_hover_text(
                    "Run kicad-cli on the Actions-panel KiCad project to export \
                     pth.drl + npth.drl (next to the Gerbers) and fill the drill \
                     .drl field with both.",
                )
                .clicked()
            {
                self.drills_from_kicad();
            }
            // Disabled while a LightBurn run is in flight, like "Etch + run":
            // the load-only run replaces `lightburn_run`, and stomping a live
            // burn's progress reporting would be rude.
            let lb_running = self
                .runtime
                .lightburn_run
                .as_ref()
                .is_some_and(|r| !r.finished());
            if ui
                .add_enabled(
                    !lb_running,
                    egui::Button::new("⤓ Emit drill holes → LightBurn (no burn)"),
                )
                .on_hover_text(
                    "Writes ONLY the drill-hole geometry (round holes + slots) from \
                     the drill .drl file(s) at this placement to the drill out \
                     .lbrn2, then LOADS the file in LightBurn (FORCELOAD) without \
                     pressing start — you burn it from LightBurn yourself.",
                )
                .clicked()
            {
                self.emit_drill_at_placement();
            }
            ui.weak("hole pattern at the placed pose — loads in LightBurn, never presses start");
        });
        // The emit bakes in the Place tab's pose, which isn't visible from here.
        ui.weak(format!(
            "emits at the Place-tab pose: ({:.2}, {:.2}) mm, {:.1}°",
            self.placement.tx_mm, self.placement.ty_mm, self.placement.rot_deg
        ));
        ui.weak(
            "Emitted as a Line (vector outline) layer: LightBurn traces each hole/slot \
             outline rather than scan-filling it, so there is no fill interval here.",
        );
        ui.weak("This recipe is independent of the Job tab's — nothing here changes the etch.");
    }
}
