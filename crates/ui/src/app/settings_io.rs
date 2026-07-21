use super::*;

/// Settings token for a dot polarity (round-trips through the loaders).
fn dot_kind_token(kind: crate::calib::DotKind) -> &'static str {
    match kind {
        crate::calib::DotKind::Dark => "dark",
        crate::calib::DotKind::Bright => "bright",
    }
}

/// Space-joined full-precision coefficient row (round-trips through `parse`).
fn coeff_row(c: &[f64; 23]) -> String {
    c.iter().map(f64::to_string).collect::<Vec<_>>().join(" ")
}

/// Parse a [`coeff_row`] back to the 23 coefficients; `None` on any
/// missing/extra/non-finite value.
fn parse_coeffs(s: &str) -> Option<[f64; 23]> {
    let vals: Vec<f64> = s
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let arr: [f64; 23] = vals.try_into().ok()?;
    arr.iter().all(|v| v.is_finite()).then_some(arr)
}

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
            ("fid_shape", self.fiducials.shape.token().to_string()),
            ("fid_diameter_mm", self.fiducials.diameter_mm.to_string()),
            ("fid_height_mm", self.fiducials.height_mm.to_string()),
            ("fid_search_mm", self.fiducials.search_mm.to_string()),
            ("fid_profile", self.fiducials.profile.token().to_string()),
            ("fid_out", self.fiducials.out.clone()),
            ("cam_file", self.camera.file.clone()),
            (
                "cam_orientation",
                self.camera.orientation.token().to_string(),
            ),
            ("cam_use_device", self.camera.use_device.to_string()),
            ("cam_device", self.camera.device.to_string()),
            // Legacy calib_* names carry the ②③ burned-grid set (their
            // long-standing meaning); the ① printed-paper set gets its own
            // calib_paper_* keys.
            ("calib_n", self.calibration.burn.n.to_string()),
            ("calib_pitch_mm", self.calibration.burn.pitch_mm.to_string()),
            ("calib_dot_mm", self.calibration.burn.dot_mm.to_string()),
            (
                "calib_dot_kind",
                dot_kind_token(self.calibration.burn.dot_kind).to_string(),
            ),
            ("calib_paper_n", self.calibration.paper.n.to_string()),
            (
                "calib_paper_pitch_mm",
                self.calibration.paper.pitch_mm.to_string(),
            ),
            (
                "calib_paper_dot_mm",
                self.calibration.paper.dot_mm.to_string(),
            ),
            (
                "calib_paper_dot_kind",
                dot_kind_token(self.calibration.paper.dot_kind).to_string(),
            ),
            ("calib_paper_out", self.calibration.paper_out.clone()),
            (
                "calib_grid_origin_x",
                self.calibration.grid_origin_mm.0.to_string(),
            ),
            (
                "calib_grid_origin_y",
                self.calibration.grid_origin_mm.1.to_string(),
            ),
            ("calib_grid_out", self.calibration.grid_out.clone()),
            (
                "calib_allow_machine_scale",
                self.calibration.allow_machine_scale.to_string(),
            ),
            (
                "calib_accept_rms_um",
                self.calibration.accept_rms_um.to_string(),
            ),
            (
                "calib_accept_worst_um",
                self.calibration.accept_worst_um.to_string(),
            ),
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
            // The ① camera-lens calibration (bi-cubic px↔mm maps), so the
            // camera distortion survives a restart. The per-dot residual
            // vectors are display-only and are not persisted.
            (
                "lens_px_to_mm",
                self.calibration
                    .lens
                    .as_ref()
                    .map(|c| coeff_row(&c.lens.px_to_mm.to_coeffs()))
                    .unwrap_or_default(),
            ),
            (
                "lens_mm_to_px",
                self.calibration
                    .lens
                    .as_ref()
                    .map(|c| coeff_row(&c.lens.mm_to_px.to_coeffs()))
                    .unwrap_or_default(),
            ),
            (
                "lens_stats",
                self.calibration
                    .lens
                    .as_ref()
                    .map(|c| {
                        format!(
                            "{} {} {} {}",
                            c.lens.rms_um, c.lens.max_um, c.found, c.total
                        )
                    })
                    .unwrap_or_default(),
            ),
            // Pixel bounds the ① lens fit covered, so the ③ field fit's
            // out-of-region extrapolation check survives a restart. Empty when
            // the restored/hand-built map carries no bounds.
            (
                "lens_px_bounds",
                self.calibration
                    .lens
                    .as_ref()
                    .and_then(|c| c.lens.calib_px_bounds)
                    .map(|[min_x, min_y, max_x, max_y]| format!("{min_x} {min_y} {max_x} {max_y}"))
                    .unwrap_or_default(),
            ),
            (
                "lens_frame_sig",
                self.calibration
                    .lens_frame_signature
                    .map(|((w, h), o)| format!("{w} {h} {}", o.token()))
                    .unwrap_or_default(),
            ),
            // The ③ laser-field calibration: the FieldMap itself lives in the
            // field-map file (written on acceptance); persist the rest here.
            (
                "field_accepted",
                self.calibration.field_accepted.to_string(),
            ),
            (
                "field_to_px",
                self.calibration
                    .field
                    .as_ref()
                    .filter(|_| self.calibration.field_accepted)
                    .map(|f| {
                        let m = &f.to_px.matrix;
                        (0..9)
                            .map(|i| m[(i / 3, i % 3)].to_string())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default(),
            ),
            (
                "field_stats",
                self.calibration
                    .field
                    .as_ref()
                    .filter(|_| self.calibration.field_accepted)
                    .map(|f| format!("{} {} {}", f.found, f.total, f.scale))
                    .unwrap_or_default(),
            ),
            // The burned-grid frame anchor (paper mm → machine mm rigid). Five
            // tokens: cos sin tx ty flip, flip as 0/1 (a mirrored machine axis).
            // A legacy four-token save loads as flip=0 (not mirrored).
            (
                "field_frame",
                self.calibration
                    .field
                    .as_ref()
                    .filter(|_| self.calibration.field_accepted)
                    .map(|f| {
                        let r = &f.paper_to_machine;
                        format!(
                            "{} {} {} {} {}",
                            r.cos,
                            r.sin,
                            r.tx,
                            r.ty,
                            if r.flip_x { 1 } else { 0 }
                        )
                    })
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
        str_field(&m, "fid_out", &mut self.fiducials.out);
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
        // Fiducial footprint + search window, clamped to the DragValue ranges
        // (a hand-edited or corrupt blob can't push them out of bounds).
        if let Some(v) = m
            .get("fid_diameter_mm")
            .and_then(|s| s.trim().parse().ok())
            .filter(|v: &f64| v.is_finite())
        {
            self.fiducials.diameter_mm = v.clamp(0.05, 20.0);
        }
        if let Some(v) = m
            .get("fid_height_mm")
            .and_then(|s| s.trim().parse().ok())
            .filter(|v: &f64| v.is_finite())
        {
            self.fiducials.height_mm = v.clamp(0.05, 20.0);
        }
        if let Some(v) = m
            .get("fid_search_mm")
            .and_then(|s| s.trim().parse().ok())
            .filter(|v: &f64| v.is_finite())
        {
            self.fiducials.search_mm = v.clamp(0.1, 20.0);
        }
        if let Some(k) = m
            .get("fid_shape")
            .and_then(|s| crate::fiducial::ShapeKind::from_token(s.trim()))
        {
            self.fiducials.shape = k;
        }
        if let Some(k) = m
            .get("fid_profile")
            .and_then(|s| crate::fiducial::ProfileKind::from_token(s.trim()))
        {
            self.fiducials.profile = k;
        }
        f64_field(&m, "calib_pitch_mm", &mut self.calibration.burn.pitch_mm);
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
        f64_field(&m, "calib_dot_mm", &mut self.calibration.burn.dot_mm);
        if let Some(v) = m.get("calib_dot_kind") {
            self.calibration.burn.dot_kind = match v.trim() {
                "bright" => crate::calib::DotKind::Bright,
                _ => crate::calib::DotKind::Dark,
            };
        }
        str_field(&m, "calib_grid_out", &mut self.calibration.grid_out);
        str_field(&m, "calib_paper_out", &mut self.calibration.paper_out);
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
        if let Some(v) = m
            .get("calib_allow_machine_scale")
            .and_then(|s| s.trim().parse().ok())
        {
            self.calibration.allow_machine_scale = v;
        }
        if let Some(v) = m
            .get("calib_accept_rms_um")
            .and_then(|s| s.trim().parse().ok())
            .filter(|v: &f64| v.is_finite())
        {
            self.calibration.accept_rms_um = v.clamp(10.0, 1000.0);
        }
        if let Some(v) = m
            .get("calib_accept_worst_um")
            .and_then(|s| s.trim().parse().ok())
            .filter(|v: &f64| v.is_finite())
        {
            self.calibration.accept_worst_um = v.clamp(10.0, 1000.0);
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
        if let Some(v) = m.get("cam_use_device").and_then(|s| s.trim().parse().ok()) {
            self.camera.use_device = v;
        }
        if let Some(v) = m
            .get("cam_device")
            .and_then(|s| s.trim().parse::<u32>().ok())
        {
            self.camera.device = v;
        }
        if let Some(v) = m
            .get("calib_n")
            .and_then(|s| s.trim().parse::<usize>().ok())
        {
            self.calibration.burn.n = v.clamp(2, 15);
        }
        // The ① printed-paper parameter set: overlay its own keys when any are
        // present; otherwise seed it from the (already-loaded) burned-grid set
        // — a pre-split save had one shared set, so that's what ① was last
        // fit with.
        let paper_keys = [
            "calib_paper_n",
            "calib_paper_pitch_mm",
            "calib_paper_dot_mm",
            "calib_paper_dot_kind",
        ];
        if paper_keys.iter().any(|k| m.contains_key(*k)) {
            f64_field(
                &m,
                "calib_paper_pitch_mm",
                &mut self.calibration.paper.pitch_mm,
            );
            f64_field(&m, "calib_paper_dot_mm", &mut self.calibration.paper.dot_mm);
            if let Some(v) = m.get("calib_paper_dot_kind") {
                self.calibration.paper.dot_kind = match v.trim() {
                    "bright" => crate::calib::DotKind::Bright,
                    _ => crate::calib::DotKind::Dark,
                };
            }
            if let Some(v) = m
                .get("calib_paper_n")
                .and_then(|s| s.trim().parse::<usize>().ok())
            {
                self.calibration.paper.n = v.clamp(2, 15);
            }
        } else {
            self.calibration.paper = self.calibration.burn;
        }
        // Restore the ① camera-lens calibration so the camera distortion
        // survives a restart. Staleness is guarded at use time: the
        // frame-signature check refuses it if resolution/crop/orientation
        // changed, and a physically moved camera is re-anchored/re-fit anyway.
        if let (Some(px), Some(mm)) = (
            m.get("lens_px_to_mm").and_then(|s| parse_coeffs(s)),
            m.get("lens_mm_to_px").and_then(|s| parse_coeffs(s)),
        ) {
            let mut stats = m
                .get("lens_stats")
                .map(|s| s.split_whitespace())
                .into_iter()
                .flatten();
            self.calibration.lens = Some(crate::calib::CameraCal {
                lens: vision::LensMap {
                    px_to_mm: vision::Poly2::from_coeffs(&px),
                    mm_to_px: vision::Poly2::from_coeffs(&mm),
                    rms_um: stats.next().and_then(|s| s.parse().ok()).unwrap_or(0.0),
                    max_um: stats.next().and_then(|s| s.parse().ok()).unwrap_or(0.0),
                    residuals: Vec::new(),
                    calib_px_bounds: m.get("lens_px_bounds").and_then(|s| {
                        let vals: Vec<f64> = s
                            .split_whitespace()
                            .filter_map(|t| t.parse().ok())
                            .filter(|v: &f64| v.is_finite())
                            .collect();
                        <[f64; 4]>::try_from(vals).ok()
                    }),
                },
                dots: Vec::new(),
                found: stats.next().and_then(|s| s.parse().ok()).unwrap_or(0),
                total: stats.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            });
        }
        if let Some(sig) = m.get("lens_frame_sig") {
            let mut it = sig.split_whitespace();
            if let (Some(w), Some(h), Some(o)) = (
                it.next().and_then(|s| s.parse::<u32>().ok()),
                it.next().and_then(|s| s.parse::<u32>().ok()),
                it.next().and_then(Orientation::from_token),
            ) {
                self.calibration.lens_frame_signature = Some(((w, h), o));
            }
        }
        // Restore the accepted ③ laser field: the FieldMap comes from the
        // field-map file written on acceptance; the linear to_px overlay
        // approximation and counts are persisted here. Restored only when the
        // lens is present too (the field is meaningless without its ruler).
        if self.calibration.lens.is_some()
            && m.get("field_accepted").map(|s| s.trim()) == Some("true")
        {
            let to_px = m.get("field_to_px").and_then(|s| {
                let vals: Vec<f64> = s
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .filter(|v: &f64| v.is_finite())
                    .collect();
                let v: [f64; 9] = vals.try_into().ok()?;
                let mat =
                    nalgebra::Matrix3::new(v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]);
                vision::Homography::from_matrix(mat).ok()
            });
            // The frame anchor is required: a save without it predates the
            // burned-grid frame fix, and its field map is paper-anchored —
            // restoring it would reintroduce the frame-mismatch bug.
            let frame = m.get("field_frame").and_then(|s| {
                let vals: Vec<f64> = s
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .filter(|v: &f64| v.is_finite())
                    .collect();
                // 4 tokens (legacy) → flip_x=false; 5 tokens → last is 0/1.
                let (cos, sin, tx, ty, flip_x) = match *vals.as_slice() {
                    [cos, sin, tx, ty] => (cos, sin, tx, ty, false),
                    [cos, sin, tx, ty, flip] => (cos, sin, tx, ty, flip != 0.0),
                    _ => return None,
                };
                Some(crate::calib::Rigid2 {
                    cos,
                    sin,
                    tx,
                    ty,
                    flip_x,
                })
            });
            let field = std::fs::read_to_string(self.field_map_path())
                .ok()
                .and_then(|s| vision::FieldMap::parse(&s).ok());
            if let (Some(to_px), Some(field), Some(paper_to_machine)) = (to_px, field, frame) {
                let mut stats = m
                    .get("field_stats")
                    .map(|s| s.split_whitespace())
                    .into_iter()
                    .flatten();
                self.calibration.field = Some(crate::calib::FieldCal {
                    field,
                    paper_to_machine,
                    to_px,
                    dots: Vec::new(),
                    found: stats.next().and_then(|s| s.parse().ok()).unwrap_or(0),
                    total: stats.next().and_then(|s| s.parse().ok()).unwrap_or(0),
                    field_verdict: vision::classify_field_error(&[]),
                    // Third token is new; older two-token saves read as 1.0
                    // (pitch-true).
                    scale: stats
                        .next()
                        .and_then(|s| s.parse().ok())
                        .filter(|v: &f64| v.is_finite() && *v > 0.0)
                        .unwrap_or(1.0),
                    // Per-fit feedback like `dots` — not persisted; a restored
                    // field has no fresh detection to count against.
                    extrapolated: 0,
                });
                self.calibration.field_accepted = true;
            }
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
                total: self.calibration.burn.n * self.calibration.burn.n,
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
