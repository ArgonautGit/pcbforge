use super::*;

impl ConsoleApp {
    /// The persisted input fields, in a fixed key order.
    pub(super) fn settings_blob(&self) -> String {
        crate::settings::blob(&[
            ("kicad_project", self.job.kicad_project.clone()),
            ("copper", self.job.emit_copper.clone()),
            ("outline", self.job.emit_outline.clone()),
            ("lbrn2", self.job.emit_lbrn2.clone()),
            ("offset_mm", self.job.offset_mm.to_string()),
            ("back_copper", self.job.back_copper.clone()),
            ("back_outline", self.job.back_outline.clone()),
            ("thickness_mm", self.job.board_thickness_mm.to_string()),
            ("focal_mm", self.job.focal_mm.to_string()),
            ("place_frame", self.placement.frame.clone()),
            ("place_lbrn2", self.placement.lbrn2.clone()),
            ("place_px_per_mm", self.placement.px_per_mm.to_string()),
            ("fid_frame", self.fiducials.frame.clone()),
            ("fid_layout", self.fiducials.layout.clone()),
            ("fid_px_per_mm", self.fiducials.px_per_mm.to_string()),
            ("cam_file", self.camera.file.clone()),
            (
                "cam_orientation",
                self.camera.orientation.token().to_string(),
            ),
            ("calib_n", self.calibration.n.to_string()),
            ("calib_pitch_mm", self.calibration.pitch_mm.to_string()),
            ("calib_dot_mm", self.calibration.dot_mm.to_string()),
            (
                "calib_dot_kind",
                match self.calibration.dot_kind {
                    crate::calib::DotKind::Dark => "dark".to_string(),
                    crate::calib::DotKind::Bright => "bright".to_string(),
                },
            ),
            (
                "calib_grid_origin_x",
                self.calibration.grid_origin_mm.0.to_string(),
            ),
            (
                "calib_grid_origin_y",
                self.calibration.grid_origin_mm.1.to_string(),
            ),
            ("calib_grid_out", self.calibration.grid_out.clone()),
            ("cam_show_bed", self.camera.show_bed.to_string()),
            (
                "place_field_correct",
                self.placement.field_correct.to_string(),
            ),
            ("field_mm", self.camera.field_mm.to_string()),
            (
                "field_center_auto",
                self.camera.field_center_auto.to_string(),
            ),
            ("field_cx_mm", self.camera.field_cx_mm.to_string()),
            ("field_cy_mm", self.camera.field_cy_mm.to_string()),
            // The calibration matrix (px→mm, row-major) — the operator's grid
            // is taped to the bed and persists, so we keep the fit as a
            // re-anchor seed across restarts (Re-anchor re-locks it).
            (
                "calib_matrix",
                self.calibration
                    .anchor
                    .as_ref()
                    .map(|c| {
                        let m = &c.px_to_mm.matrix;
                        (0..9)
                            .map(|i| m[(i / 3, i % 3)].to_string())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            ),
            (
                "calib_saved_at",
                self.calibration
                    .saved_at
                    .map(|t| t.to_string())
                    .unwrap_or_default(),
            ),
        ])
    }

    /// Overlay any saved input fields from the settings file onto the defaults.
    pub(super) fn load_settings(&mut self) {
        let m = crate::settings::load(&self.runtime.settings_path);
        let str_field =
            |m: &std::collections::BTreeMap<String, String>, k: &str, dst: &mut String| {
                if let Some(v) = m.get(k) {
                    *dst = v.trim().to_string();
                }
            };
        str_field(&m, "kicad_project", &mut self.job.kicad_project);
        str_field(&m, "copper", &mut self.job.emit_copper);
        str_field(&m, "outline", &mut self.job.emit_outline);
        str_field(&m, "lbrn2", &mut self.job.emit_lbrn2);
        str_field(&m, "back_copper", &mut self.job.back_copper);
        str_field(&m, "back_outline", &mut self.job.back_outline);
        str_field(&m, "place_frame", &mut self.placement.frame);
        str_field(&m, "place_lbrn2", &mut self.placement.lbrn2);
        str_field(&m, "fid_frame", &mut self.fiducials.frame);
        str_field(&m, "fid_layout", &mut self.fiducials.layout);
        str_field(&m, "cam_file", &mut self.camera.file);
        let f64_field = |m: &std::collections::BTreeMap<String, String>, k: &str, dst: &mut f64| {
            if let Some(v) = m
                .get(k)
                .and_then(|s| s.trim().parse().ok())
                .filter(|v: &f64| v.is_finite())
            {
                *dst = v;
            }
        };
        f64_field(&m, "offset_mm", &mut self.job.offset_mm);
        f64_field(&m, "thickness_mm", &mut self.job.board_thickness_mm);
        f64_field(&m, "focal_mm", &mut self.job.focal_mm);
        f64_field(&m, "place_px_per_mm", &mut self.placement.px_per_mm);
        f64_field(&m, "fid_px_per_mm", &mut self.fiducials.px_per_mm);
        f64_field(&m, "calib_pitch_mm", &mut self.calibration.pitch_mm);
        f64_field(
            &m,
            "calib_grid_origin_x",
            &mut self.calibration.grid_origin_mm.0,
        );
        f64_field(
            &m,
            "calib_grid_origin_y",
            &mut self.calibration.grid_origin_mm.1,
        );
        f64_field(&m, "calib_dot_mm", &mut self.calibration.dot_mm);
        if let Some(v) = m.get("calib_dot_kind") {
            self.calibration.dot_kind = match v.trim() {
                "bright" => crate::calib::DotKind::Bright,
                _ => crate::calib::DotKind::Dark,
            };
        }
        str_field(&m, "calib_grid_out", &mut self.calibration.grid_out);
        let f32_field = |m: &std::collections::BTreeMap<String, String>, k: &str, dst: &mut f32| {
            if let Some(v) = m
                .get(k)
                .and_then(|s| s.trim().parse().ok())
                .filter(|v: &f32| v.is_finite())
            {
                *dst = v;
            }
        };
        f32_field(&m, "field_mm", &mut self.camera.field_mm);
        f32_field(&m, "field_cx_mm", &mut self.camera.field_cx_mm);
        f32_field(&m, "field_cy_mm", &mut self.camera.field_cy_mm);
        if let Some(v) = m
            .get("field_center_auto")
            .and_then(|s| s.trim().parse().ok())
        {
            self.camera.field_center_auto = v;
        } else if m.contains_key("field_mm")
            || m.contains_key("field_cx_mm")
            || m.contains_key("field_cy_mm")
        {
            // Migrate the former built-in default. Any other legacy tuple was
            // explicitly operator-set, so preserve it as a manual centre.
            let was_old_default = (self.camera.field_mm - 140.0).abs() < f32::EPSILON
                && self.camera.field_cx_mm.abs() < f32::EPSILON
                && self.camera.field_cy_mm.abs() < f32::EPSILON;
            if was_old_default {
                self.camera.field_mm = 70.0;
                self.camera.field_center_auto = true;
            } else {
                self.camera.field_center_auto = false;
            }
        }
        self.sync_auto_field_center();
        if let Some(v) = m.get("cam_show_bed").and_then(|s| s.trim().parse().ok()) {
            self.camera.show_bed = v;
        }
        // A persisted field-correction preference is only honored once a field
        // cal exists this session (the placement frame needs it), so this just
        // restores the operator's intent; calibrate_fit re-enables it on a fit.
        if let Some(v) = m
            .get("place_field_correct")
            .and_then(|s| s.trim().parse().ok())
        {
            self.placement.field_correct = v;
        }
        if let Some(o) = m
            .get("cam_orientation")
            .and_then(|s| Orientation::from_token(s.trim()))
        {
            self.camera.orientation = o;
        }
        if let Some(v) = m
            .get("calib_n")
            .and_then(|s| s.trim().parse::<usize>().ok())
        {
            self.calibration.n = v.clamp(2, 15);
        }
        // The taped grid persists on the bed, so restore the last calibration
        // as a re-anchor seed (found=0 ⇒ "loaded, unconfirmed" until the
        // operator Re-anchors to the paper).
        if let Some(vals) = m.get("calib_matrix").map(|s| {
            s.split_whitespace()
                .filter_map(|t| t.parse::<f64>().ok())
                .collect::<Vec<_>>()
        }) && vals.len() == 9
        {
            let mat = nalgebra::Matrix3::new(
                vals[0], vals[1], vals[2], vals[3], vals[4], vals[5], vals[6], vals[7], vals[8],
            );
            let Ok(px_to_mm) = vision::Homography::from_matrix(mat) else {
                self.calibration.note = "ignored an invalid saved calibration matrix".into();
                return;
            };
            self.calibration.anchor = Some(crate::calib::Calibration {
                px_to_mm,
                rms_um: 0.0,
                found: 0,
                total: self.calibration.n * self.calibration.n,
                dots: Vec::new(),
            });
            self.calibration.saved_at = m.get("calib_saved_at").and_then(|s| s.trim().parse().ok());
            let age = match self.calibration.saved_at {
                Some(t) => human_age(now_unix().saturating_sub(t)),
                None => "age unknown".into(),
            };
            self.calibration.note = format!(
                "loaded a saved calibration ({age}) — click ⟳ Re-anchor to re-lock to the taped grid"
            );
        }
    }

    /// Persist the input fields if they changed since the last save. Cheap to
    /// call every frame — it only touches the disk on an actual edit.
    pub(super) fn save_settings_if_changed(&mut self) {
        // The live anchor rewrites the calib matrix every frame; don't churn the
        // settings file to disk on every one. It's flushed once live stops (the
        // matrix is only a re-anchor seed anyway) (LR-46).
        if self.calibration.live {
            return;
        }
        let blob = self.settings_blob();
        if blob != self.runtime.last_settings {
            match crate::settings::save(&self.runtime.settings_path, &blob) {
                Ok(()) => {
                    self.runtime.last_settings = blob;
                    self.runtime.settings_error = None;
                }
                Err(err) => {
                    let msg = format!("settings save failed: {err}");
                    if self.runtime.settings_error.as_deref() != Some(&msg) {
                        self.runtime.log.push(LogLine {
                            text: msg.clone(),
                            err: true,
                        });
                    }
                    self.runtime.settings_error = Some(msg);
                }
            }
        }
    }
}
