use super::*;

/// Inset per side applied when deriving the fiducial rectangle from a measured
/// board bbox: the bbox is the board OUTLINE, the rectangle is a hole-CENTRE
/// span, so each side moves this far in from the edge.
const FID_EDGE_INSET_MM: f64 = 5.0;

impl ConsoleApp {
    pub(super) fn calib_grid(&self) -> calib::GridSpec {
        calib::GridSpec {
            // The origin the grid was generated (burned) at — NOT (0,0). The
            // burned-grid fit and bed overlay must match where the dots
            // actually landed (LR-02).
            origin_mm: self.calibration.grid_origin_mm,
            pitch_mm: self.calibration.burn.pitch_mm,
            n: self.calibration.burn.n,
        }
    }

    /// Emit the calibration dot grid by shelling `pcbforge calib-grid` — the
    /// operator burns it, then images it back.
    pub(super) fn calibrate_generate_grid(&mut self) {
        let out = crate::clean_path(&self.calibration.grid_out);
        if out.is_empty() {
            self.calibration.note = "set an output path for the grid".into();
            return;
        }
        // Centre the grid on the machine field (Camera-tab work area), so it
        // lands inside the addressable area — a centre-origin galvo has (0,0)
        // at the field centre, not a corner. origin = field centre − span/2.
        let span =
            (self.calibration.burn.n.saturating_sub(1)) as f64 * self.calibration.burn.pitch_mm;
        let ox = self.camera.field_cx_mm as f64 - span / 2.0;
        let oy = self.camera.field_cy_mm as f64 - span / 2.0;
        // Remember the exact origin the grid is burned at, so the later fit
        // labels the lower-left dot with the machine mm it was actually
        // commanded to — not (0,0) (LR-02).
        self.calibration.grid_origin_mm = (ox, oy);
        self.run_verb(&[
            "calib-grid".into(),
            "--out".into(),
            out,
            "--n".into(),
            self.calibration.burn.n.to_string(),
            "--pitch-mm".into(),
            format!("{}", self.calibration.burn.pitch_mm),
            "--dot-mm".into(),
            format!("{}", self.calibration.burn.dot_mm),
            "--origin".into(),
            format!("{ox},{oy}"),
        ]);
        let fits = span <= self.camera.field_mm as f64 + 1e-6;
        let warn = if fits {
            ""
        } else {
            " ⚠ grid is BIGGER than the work area — lower pitch or dots per side"
        };
        self.calibration.note = format!(
            "generating {n}×{n} grid centred on work area ({cx:.0},{cy:.0}) size {sz:.0} → \
             spans ({ox:.0},{oy:.0})…({x1:.0},{y1:.0}) mm — see Log for the file path{warn}",
            n = self.calibration.burn.n,
            cx = self.camera.field_cx_mm,
            cy = self.camera.field_cy_mm,
            sz = self.camera.field_mm,
            x1 = ox + span,
            y1 = oy + span,
        );
    }

    /// Emit the ① printed-paper calibration grid by shelling `pcbforge
    /// paper-grid` — the operator prints it at 100%, calipers the printed pitch,
    /// enters that as the step-1 measured pitch, then images it.
    pub(super) fn calibrate_generate_paper_grid(&mut self) {
        let out = crate::clean_path(&self.calibration.paper_out);
        if out.is_empty() {
            self.calibration.note = "set an output path for the paper grid".into();
            return;
        }
        let params = self.calibration.paper;
        self.run_verb(&[
            "paper-grid".into(),
            "--out".into(),
            out,
            "--n".into(),
            params.n.to_string(),
            "--pitch-mm".into(),
            format!("{}", params.pitch_mm),
            "--dot-mm".into(),
            format!("{}", params.dot_mm),
        ]);
        // Mirror generate-grid: warn but still shell, so the CLI's own
        // span-too-large error surfaces in the Log.
        let span = (params.n.saturating_sub(1)) as f64 * params.pitch_mm;
        let warn = if span > 190.0 {
            " ⚠ span exceeds the 190 mm printable width — lower pitch or dots per side"
        } else {
            ""
        };
        self.calibration.note = format!(
            "generating {n}×{n} paper grid, nominal pitch {p} mm — print at 100%, then CALIPER \
             the printed pitch and enter it above before fitting — see Log for the file path{warn}",
            n = params.n,
            p = params.pitch_mm,
        );
    }

    /// Load the burned-grid frame — from the camera (source picked in the
    /// Camera tab) when the frame path is empty, else from that file — and
    /// clear any prior corner clicks.
    pub(super) fn calibrate_load_frame(&mut self, ctx: &Context) {
        let img = if crate::clean_path(&self.calibration.frame).is_empty() {
            match self.grab_shared() {
                Ok(g) => self.camera.orientation.apply(g),
                Err(e) => {
                    self.calibration.note = format!("camera: {e}");
                    return;
                }
            }
        } else {
            match image::open(crate::clean_path(&self.calibration.frame)) {
                Ok(i) => i.to_luma8(),
                Err(e) => {
                    self.calibration.note = format!("frame: {e}");
                    return;
                }
            }
        };
        let (w, h) = (img.width() as usize, img.height() as usize);
        let color = ColorImage {
            size: [w, h],
            pixels: img.pixels().map(|p| Color32::from_gray(p[0])).collect(),
        };
        self.calibration.frame_tex =
            Some(ctx.load_texture("calib-frame", color, TextureOptions::NEAREST));
        self.calibration.frame_img = Some(img);
        self.calibration.corners.clear();
        // Fresh frame → hide any stale fit overlay so the bare dots are visible
        // to re-click the corners.
        self.calibration.show_fit_feedback = false;
        self.calibration.note = format!("click the 4 corner dots: {}", self.corner_click_order());
    }

    /// The corner-click order instruction, mode-aware. The step-1 printed paper
    /// has an arbitrary pose by design, so its corners are named by visual
    /// position. The burned laser grid (steps 2/3) carries orientation markers
    /// that break its mirror symmetry, so its corners are named by those markers
    /// — this is what stops a mirrored-X machine from being silently absorbed
    /// into the labelling (the flip must reach the fit as a real reflection).
    pub(super) fn corner_click_order(&self) -> &'static str {
        if self.calibration.mode == CalibMode::CameraLens {
            "lower-left, lower-right, upper-right, upper-left"
        } else {
            "LL = the corner nearest the lone diagonal marker; LR = the far corner of the edge \
             with the midpoint marker; then UR, UL"
        }
    }

    /// Fit the camera→laser calibration from the 4 clicked corners.
    pub(super) fn calibrate_fit(&mut self) {
        let Some(frame) = &self.calibration.frame_img else {
            self.calibration.note = "load the grid frame first".into();
            return;
        };
        if self.calibration.corners.len() != 4 {
            self.calibration.note = format!(
                "click all 4 corner dots (have {}) — {}",
                self.calibration.corners.len(),
                self.corner_click_order()
            );
            return;
        }
        let corners: [(f64, f64); 4] = [
            self.calibration.corners[0],
            self.calibration.corners[1],
            self.calibration.corners[2],
            self.calibration.corners[3],
        ];
        match self.calibration.mode {
            CalibMode::CameraLens => {
                // The camera-lens fit is a metric ruler over the PRINTED grid:
                // its own parameter set (measured pitch), and an arbitrary
                // origin pinned to (0,0) rather than inheriting a burn origin
                // left over from a generated laser grid (LR-02).
                let p = self.calibration.paper;
                let paper = calib::GridSpec {
                    origin_mm: (0.0, 0.0),
                    pitch_mm: p.pitch_mm,
                    n: p.n,
                };
                match calib::fit_camera_lens(frame, corners, &paper, p.dot_mm, p.dot_kind) {
                    Ok(cal) => {
                        self.calibration.note = format!(
                            "lens fit: {}/{} dots, RMS {:.0} µm, worst {:.0} µm — camera is now a metric ruler; run step 3 again before using field correction",
                            cal.found, cal.total, cal.lens.rms_um, cal.lens.max_um
                        );
                        self.calibration.lens_frame_signature =
                            Some((frame.dimensions(), self.camera.orientation));
                        self.calibration.show_fit_feedback = true;
                        self.calibration.lens = Some(cal);
                        // A field fit is expressed in the old lens ruler's
                        // physical frame. Re-fitting the lens invalidates it.
                        self.calibration.field = None;
                        self.calibration.field_accepted = false;
                    }
                    Err(e) => {
                        // Keep any previous lens calibration — a bad fit
                        // (wrong corners/polarity) must not erase the working
                        // one the operator depends on (LR-16).
                        self.calibration.note = format!("lens fit failed (kept previous): {e}");
                    }
                }
            }
            CalibMode::LaserAnchor => {
                let grid = self.calib_grid();
                let b = self.calibration.burn;
                match calib::fit_camera_to_machine(frame, corners, &grid, b.dot_mm, b.dot_kind) {
                    Ok(cal) => {
                        self.calibration.note = format!(
                            "anchor: {}/{} dots, RMS {:.0} µm — Place now burns in machine coordinates",
                            cal.found, cal.total, cal.rms_um
                        );
                        self.calibration.show_fit_feedback = true;
                        self.calibration.anchor = Some(cal);
                        self.calibration.saved_at = Some(now_unix());
                    }
                    Err(e) => {
                        // Preserve the taped-grid calibration on a failed
                        // re-fit (matches re_anchor's behavior) (LR-16).
                        self.calibration.note = format!("anchor fit failed (kept previous): {e}");
                    }
                }
            }
            CalibMode::LaserField => {
                let grid = self.calib_grid();
                let b = self.calibration.burn;
                let Some(lens) = self.calibration.lens.as_ref().map(|c| c.lens.clone()) else {
                    self.calibration.note =
                        "laser-field needs a camera-lens calibration first (step 1, Camera lens) — \
                         it's the metric ruler that measures where burns land"
                            .into();
                    return;
                };
                let signature = (frame.dimensions(), self.camera.orientation);
                if self.calibration.lens_frame_signature != Some(signature) {
                    self.calibration.note = format!(
                        "laser-field refused: camera lens was calibrated for {:?}, but this frame is {:?}; keep the same resolution/crop/orientation and do not move the camera",
                        self.calibration.lens_frame_signature, signature
                    );
                    self.calibration.field_accepted = false;
                    return;
                }
                match calib::fit_laser_field(
                    frame,
                    corners,
                    &grid,
                    b.dot_mm,
                    b.dot_kind,
                    &lens,
                    self.calibration.field_scale,
                ) {
                    Ok(cal) => {
                        // A successful fit produced fresh feedback worth showing,
                        // regardless of whether it met the acceptance limits.
                        self.calibration.show_fit_feedback = true;
                        let worst = cal.dots.iter().map(|d| d.field_um).fold(0.0_f64, f64::max);
                        let acceptance = calib::field_live_acceptance(
                            &cal,
                            &grid,
                            self.calibration.accept_rms_um,
                            self.calibration.accept_worst_um,
                        );
                        self.calibration.field_accepted = acceptance.is_ok();
                        // classify_field_error assumes the grid is centred on
                        // the scan field: off-axis, genuine curvature reads as
                        // a uniform scale. Warn when the burn is well off the
                        // configured field centre.
                        const OFF_CENTER_FRAC: f64 = 0.25;
                        let span = (grid.n.saturating_sub(1)) as f64 * grid.pitch_mm;
                        let (cx, cy) =
                            (grid.origin_mm.0 + span / 2.0, grid.origin_mm.1 + span / 2.0);
                        let off = (cx - self.camera.field_cx_mm as f64)
                            .hypot(cy - self.camera.field_cy_mm as f64);
                        let off_center_note = if off > OFF_CENTER_FRAC * self.camera.field_mm as f64
                        {
                            format!(
                                " note: grid centre ({cx:.0},{cy:.0}) is {off:.0} mm off the \
                                 configured field centre — an off-axis grid makes curvature \
                                 read as uniform scale; centre the grid (or fix the work-area \
                                 centre)"
                            )
                        } else {
                            String::new()
                        };
                        // Burned dots outside the region the step-1 lens fit
                        // covered read through an extrapolating ruler, so their
                        // error hides in the scatter floor instead of surfacing.
                        let extrapolated_note = if cal.extrapolated > 0 {
                            format!(
                                " note: {}/{} dots lie outside the region the step-1 lens \
                                 calibration covered — the ruler extrapolates there and the \
                                 error reads as scatter; print/calibrate a larger paper grid \
                                 or shrink the burn grid",
                                cal.extrapolated, cal.found
                            )
                        } else {
                            String::new()
                        };
                        // A fit that passes only because a dot was thrown away
                        // must not read the same as one that passes outright,
                        // so this rides on BOTH the accepted and rejected note.
                        let rejection_note = if cal.rejection_note.is_empty() {
                            String::new()
                        } else {
                            format!(" note: {}", cal.rejection_note)
                        };
                        self.calibration.note = match acceptance {
                            Ok(()) => {
                                let path = self.field_map_path();
                                match std::fs::write(&path, cal.field.serialize()) {
                                    Ok(()) => {
                                        // When the operator opted to absorb a
                                        // large machine scale, say so loudly: the
                                        // polynomial makes shapes true, but the
                                        // machine's physical speeds and hatch
                                        // spacing stay in its oversized units, so
                                        // energy density differs by this factor.
                                        let scale_absorbed = match self.calibration.field_scale {
                                            calib::FieldScale::Compensate
                                                if (cal.scale - 1.0).abs()
                                                    > calib::FIELD_SCALE_FAIL_FRAC =>
                                            {
                                                format!(
                                                    "machine scale {:+.1}% ABSORBED in software — physical speeds/hatch density differ from commanded by this factor; ",
                                                    (cal.scale - 1.0) * 100.0
                                                )
                                            }
                                            // Distortion-only never hides the
                                            // mis-size: the correction is 1:1,
                                            // so the machine still burns this
                                            // much off size until it is fixed.
                                            calib::FieldScale::DistortionOnly => format!(
                                                "distortion only — machine scale {:+.1}% measured and NOT corrected (burns stay this much off size); the correction is 1:1 and covers the whole work area; ",
                                                (cal.scale - 1.0) * 100.0
                                            ),
                                            _ => String::new(),
                                        };
                                        // A fitted mirror means the machine's X
                                        // axis runs backwards vs commanded
                                        // coordinates (a LightBurn axis-negate /
                                        // galvo mapping). The correction accounts
                                        // for it, but say so loudly so the
                                        // operator can undo it at the source.
                                        let mirror_note = if cal.paper_to_machine.flip_x {
                                            "machine X axis is MIRRORED relative to commanded coordinates — the correction accounts for it; clearing the axis negate in LightBurn and recalibrating removes this; "
                                        } else {
                                            ""
                                        };
                                        format!(
                                            "field accepted: {}/{} dots, raw worst {:.0} µm, fit RMS/worst {:.0}/{:.0} µm — {mirror_note}{scale_absorbed}{}{rejection_note}{off_center_note}{extrapolated_note}",
                                            cal.found,
                                            cal.total,
                                            worst,
                                            cal.field.rms_um,
                                            cal.field.max_um,
                                            field_verdict_phrase(&cal.field_verdict, cal.scale)
                                        )
                                    }
                                    Err(e) => {
                                        self.calibration.field_accepted = false;
                                        format!(
                                            "field fit met quality limits ({}/{}, RMS/worst {:.0}/{:.0} µm) but saving {} failed: {e}; correction remains disabled",
                                            cal.found,
                                            cal.total,
                                            cal.field.rms_um,
                                            cal.field.max_um,
                                            path.display()
                                        )
                                    }
                                }
                            }
                            Err(reason) => format!(
                                "field rejected: {}/{} dots, raw worst {:.0} µm, fit RMS/worst {:.0}/{:.0} µm — {reason}; correction remains disabled{rejection_note}{off_center_note}{extrapolated_note}",
                                cal.found, cal.total, worst, cal.field.rms_um, cal.field.max_um
                            ),
                        };
                        self.calibration.field = Some(cal);
                        self.calibration.field_scale_used = self.calibration.field_scale;
                    }
                    Err(e) => {
                        // Keep any previous field calibration on a failed fit
                        // (LR-16). For the scale setup-error gate, append the
                        // two entered pitches so a ①/③ pitch mix-up is
                        // checkable at a glance.
                        let pitch_context = if e.contains(calib::FIELD_SCALE_ERR_MARKER) {
                            let paper = self.calibration.paper.pitch_mm;
                            let burn = self.calibration.burn.pitch_mm;
                            format!(
                                " (step-1 paper pitch entered: {paper} mm; step-3 commanded pitch: \
                                 {burn} mm — ratio {:.3})",
                                burn / paper
                            )
                        } else {
                            String::new()
                        };
                        self.calibration.note =
                            format!("laser-field fit failed (kept previous): {e}{pitch_context}");
                    }
                }
            }
            // ④ Fiducial holes never marks corners or fits — no-op.
            CalibMode::FidHoles => {}
        }
    }

    /// An accepted, finite ① lens + ③ field calibration exists — the emit
    /// paths field-warp when this holds and fall back to unwarped (warned)
    /// output when it doesn't.
    pub(super) fn has_usable_field_cal(&self) -> bool {
        self.calibration.field_accepted
            && self
                .calibration
                .lens
                .as_ref()
                .zip(self.calibration.field.as_ref())
                .is_some_and(|(lens, field)| {
                    calib::composed_projection_is_finite(&lens.lens, &field.field)
                })
    }

    /// Where the laser field-correction file is written for `register
    /// --field-map` — beside the settings blob so it survives with the session.
    pub(super) fn field_map_path(&self) -> PathBuf {
        self.runtime
            .settings_path
            .with_file_name("pcbforge-field-map.txt")
    }

    /// Re-anchor the existing calibration to a fresh camera frame — no corner
    /// clicks. Absorbs camera drift as long as the burned grid is still in view
    /// and the move was small; a big jump fails and needs a fresh corner fit.
    pub(super) fn calibrate_re_anchor(&mut self) {
        let Some(prev) = self.calibration.anchor.clone() else {
            self.calibration.note = "no calibration yet — do a full Fit first".into();
            return;
        };
        let grid = self.calib_grid();
        let dot = self.calibration.burn.dot_mm;
        let kind = self.calibration.burn.dot_kind;
        match self.grab_shared() {
            Ok(g) => {
                let frame = self.camera.orientation.apply(g);
                match calib::re_anchor(&frame, &prev, &grid, dot, kind) {
                    Ok(cal) => {
                        self.calibration.note = format!(
                            "re-anchored: {}/{} dots, RMS {:.0} µm",
                            cal.found, cal.total, cal.rms_um
                        );
                        self.calibration.anchor = Some(cal);
                        self.calibration.saved_at = Some(now_unix());
                    }
                    Err(e) => {
                        self.calibration.note =
                            format!("re-anchor failed: {e} — re-Fit the corners")
                    }
                }
            }
            Err(e) => self.calibration.note = format!("camera: {e}"),
        }
    }

    /// Continuous re-anchoring: while ● Live anchor is on, re-fit the
    /// calibration from the camera every frame so the mapping tracks the camera
    /// as it moves (the burned grid must stay in view). Stops the capture when
    /// off.
    pub(super) fn pump_calib_live(&mut self, ctx: &Context) {
        // Live-off only means this tab stops asking for frames — the capture is
        // shared with the Camera and Fiducial tabs, and only the idle rule in
        // `ui()` may drop it.
        if !self.calibration.live {
            return;
        }
        let Some(prev) = self.calibration.anchor.clone() else {
            self.calibration.live = false;
            self.calibration.note = "calibrate once (Fit) before live anchoring".into();
            return;
        };
        if let Some(Ok(g)) = self.capture_latest() {
            let frame = self.camera.orientation.apply(g);
            let (w, h) = (frame.width() as usize, frame.height() as usize);
            let color = ColorImage {
                size: [w, h],
                pixels: frame.pixels().map(|p| Color32::from_gray(p[0])).collect(),
            };
            self.calibration.frame_tex =
                Some(ctx.load_texture("calib-frame", color, TextureOptions::NEAREST));
            match calib::re_anchor(
                &frame,
                &prev,
                &self.calib_grid(),
                self.calibration.burn.dot_mm,
                self.calibration.burn.dot_kind,
            ) {
                Ok(cal) => {
                    self.calibration.note = format!(
                        "live: {}/{} dots, RMS {:.0} µm",
                        cal.found, cal.total, cal.rms_um
                    );
                    self.calibration.anchor = Some(cal);
                    self.calibration.saved_at = Some(now_unix());
                }
                Err(e) => self.calibration.note = format!("live anchor lost the grid: {e}"),
            }
            self.calibration.frame_img = Some(frame);
        }
        ctx.request_repaint();
    }
}

impl ConsoleApp {
    pub(super) fn calibrate_edit_anchor_dot(&mut self, native_px: (f64, f64), remove: bool) {
        let Some(current) = self.calibration.anchor.clone() else {
            self.calibration.note = "fit the anchor before correcting dots".into();
            return;
        };
        let Some(mm_to_px) = current.px_to_mm.try_inverse() else {
            self.calibration.note = "cannot edit dots: the anchor is singular".into();
            return;
        };
        let grid = self.calib_grid();
        let native = nalgebra::Point2::new(native_px.0, native_px.1);
        let (ox, oy) = grid.origin_mm;
        let origin = mm_to_px.apply(nalgebra::Point2::new(ox, oy));
        let pitch_x = mm_to_px.apply(nalgebra::Point2::new(ox + grid.pitch_mm, oy)) - origin;
        let pitch_y = mm_to_px.apply(nalgebra::Point2::new(ox, oy + grid.pitch_mm)) - origin;
        let pitch_px = (0.5 * (pitch_x.norm() + pitch_y.norm())).max(8.0);
        let mut dots = current.dots.clone();

        let action = if remove {
            let Some((index, distance)) = dots
                .iter()
                .enumerate()
                .map(|(index, dot)| {
                    (
                        index,
                        (nalgebra::Point2::new(dot.px.0, dot.px.1) - native).norm(),
                    )
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
            else {
                self.calibration.note = "there are no detected dots to remove".into();
                return;
            };
            if distance > pitch_px * 0.30 {
                self.calibration.note = "right-click closer to a detected dot to remove it".into();
                return;
            }
            let removed = dots.remove(index);
            format!(
                "removed dot at ({:.0}, {:.0}) mm",
                removed.mm.0, removed.mm.1
            )
        } else {
            let Some((mm, distance)) = grid
                .points()
                .into_iter()
                .map(|mm| {
                    let expected = mm_to_px.apply(nalgebra::Point2::new(mm.0, mm.1));
                    (mm, (expected - native).norm())
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
            else {
                self.calibration.note = "the calibration grid has no sites".into();
                return;
            };
            if distance > pitch_px * 0.48 {
                self.calibration.note =
                    "left-click closer to the square you want to correct".into();
                return;
            }
            if let Some(dot) = dots
                .iter_mut()
                .find(|dot| (dot.mm.0 - mm.0).abs() < 1e-6 && (dot.mm.1 - mm.1).abs() < 1e-6)
            {
                dot.px = native_px;
            } else {
                dots.push(calib::AnchorDot {
                    px: native_px,
                    mm,
                    resid_um: 0.0,
                });
            }
            format!("corrected dot at ({:.0}, {:.0}) mm", mm.0, mm.1)
        };

        match calib::refit_anchor_dots(&dots, current.total) {
            Ok(calibration) => {
                self.calibration.note = format!(
                    "{action}; re-fit {}/{} dots, RMS {:.0} µm",
                    calibration.found, calibration.total, calibration.rms_um
                );
                self.calibration.anchor = Some(calibration);
                self.calibration.saved_at = Some(now_unix());
            }
            Err(error) => self.calibration.note = format!("dot correction rejected: {error}"),
        }
    }

    pub(super) fn calib_frame_overlay(&mut self, ui: &mut egui::Ui) {
        // ④ Fiducial holes has no frame/corner interaction — nothing to draw.
        if self.calibration.mode == CalibMode::FidHoles {
            return;
        }
        let Some(tex) = self.calibration.frame_tex.clone() else {
            ui.weak("(load a grid frame to mark corners)");
            return;
        };
        let (xf, resp) = self.show_image(ui, "calib", &tex);
        let rect = xf.panel;
        let to_screen = |px: f64, py: f64| xf.to_screen(px, py);
        // A click adds the next corner (up to 4) — unless navigating (Ctrl).
        if !crate::imgview::is_navigating(ui)
            && let Some(pos) = resp.interact_pointer_pos()
        {
            if self.calibration.edit_anchor_dots && self.calibration.mode == CalibMode::LaserAnchor
            {
                if resp.clicked_by(egui::PointerButton::Primary) {
                    self.calibrate_edit_anchor_dot(xf.to_native(pos), false);
                } else if resp.clicked_by(egui::PointerButton::Secondary) {
                    self.calibrate_edit_anchor_dot(xf.to_native(pos), true);
                }
            } else if resp.clicked_by(egui::PointerButton::Primary)
                && self.calibration.corners.len() < 4
            {
                self.calibration.corners.push(xf.to_native(pos));
            }
        }
        let painter = ui.painter_at(rect);
        let cyan = Color32::from_rgb(0x22, 0xcc, 0xdd);

        // Lens feedback (camera mode, after a fit): a magenta arrow per dot
        // showing the distortion it exhibited (exaggerated ×scale), and a dot
        // colored by how well the polynomial corrected it.
        if self.calibration.show_fit_feedback
            && self.calibration.mode == CalibMode::CameraLens
            && let Some(cal) = &self.calibration.lens
        {
            let s = xf.scale;
            let scale = self.calibration.lens_arrow_scale;
            let magenta = Color32::from_rgb(0xd0, 0x50, 0xd0);
            for d in &cal.dots {
                let base = to_screen(d.px.0, d.px.1);
                let tip = egui::pos2(
                    base.x + (d.distort_px.0 as f32) * scale * s,
                    base.y + (d.distort_px.1 as f32) * scale * s,
                );
                painter.line_segment([base, tip], (1.5, magenta));
                painter.circle_filled(tip, 2.0, magenta);
                // Correction quality: green < 30 µm, amber < 100, red beyond.
                let col = if d.resid_um < 30.0 {
                    Color32::from_rgb(0x40, 0xc0, 0x50)
                } else if d.resid_um < 100.0 {
                    Color32::from_rgb(0xe0, 0x90, 0x20)
                } else {
                    Color32::from_rgb(0xd0, 0x40, 0x40)
                };
                painter.circle_stroke(base, 4.0, egui::Stroke::new(1.5_f32, col));
            }
        }

        // Laser-anchor feedback (after a fit): the machine coordinate grid the
        // camera reconstructs, drawn as a blue mesh over the burned dots, with
        // the origin + axes, per-dot residual vectors (× exaggerated), and any
        // dots that failed to lock. This makes the abstract homography visible:
        // the operator sees exactly where the laser thinks its grid is.
        if self.calibration.show_fit_feedback
            && self.calibration.mode == CalibMode::LaserAnchor
            && let Some(cal) = &self.calibration.anchor
            && cal.found > 0
            && let Some(mm_to_px) = cal.px_to_mm.try_inverse()
        {
            let grid = self.calib_grid();
            let exagg = self.calibration.anchor_resid_scale;
            let proj = |mx: f64, my: f64| {
                let p = mm_to_px.apply(nalgebra::Point2::new(mx, my));
                to_screen(p.x, p.y)
            };
            let green = Color32::from_rgb(0x40, 0xc0, 0x50);
            let amber = Color32::from_rgb(0xe0, 0x90, 0x20);
            let red = Color32::from_rgb(0xd0, 0x40, 0x40);
            let mesh = Color32::from_rgb(0x35, 0x70, 0xb0);
            let axis = Color32::from_rgb(0x30, 0xd0, 0x80);
            let orange = Color32::from_rgb(0xf0, 0x90, 0x30);

            // The full commanded lattice projected into the frame, as a mesh.
            let n = grid.n;
            let pts = grid.points();
            let nodes: Vec<egui::Pos2> = pts.iter().map(|&(mx, my)| proj(mx, my)).collect();
            let node = |r: usize, c: usize| nodes[r * n + c];
            for r in 0..n {
                for c in 0..n {
                    if c + 1 < n {
                        painter.line_segment([node(r, c), node(r, c + 1)], (1.0, mesh));
                    }
                    if r + 1 < n {
                        painter.line_segment([node(r, c), node(r + 1, c)], (1.0, mesh));
                    }
                }
            }

            // Dots that never locked: a red ✕ at the commanded lattice site.
            for &(mx, my) in &pts {
                let detected = cal
                    .dots
                    .iter()
                    .any(|d| (d.mm.0 - mx).abs() < 1e-6 && (d.mm.1 - my).abs() < 1e-6);
                if !detected {
                    let s = proj(mx, my);
                    painter.line_segment(
                        [
                            egui::pos2(s.x - 5.0, s.y - 5.0),
                            egui::pos2(s.x + 5.0, s.y + 5.0),
                        ],
                        (1.5, red),
                    );
                    painter.line_segment(
                        [
                            egui::pos2(s.x - 5.0, s.y + 5.0),
                            egui::pos2(s.x + 5.0, s.y - 5.0),
                        ],
                        (1.5, red),
                    );
                }
            }

            // Per detected dot: quality-colored ring + an exaggerated residual
            // vector from the commanded (predicted) site to where it was found.
            for d in &cal.dots {
                let det = to_screen(d.px.0, d.px.1);
                let cmd = proj(d.mm.0, d.mm.1);
                let tip = egui::pos2(
                    cmd.x + (det.x - cmd.x) * exagg,
                    cmd.y + (det.y - cmd.y) * exagg,
                );
                painter.line_segment([cmd, tip], (1.2, orange));
                let col = if d.resid_um < 50.0 {
                    green
                } else if d.resid_um < 200.0 {
                    amber
                } else {
                    red
                };
                painter.circle_stroke(det, 4.0, egui::Stroke::new(1.5_f32, col));
            }

            // Origin + machine axes: the laser's (0,0) and +X/+Y directions.
            let (ox, oy) = grid.origin_mm;
            let o = proj(ox, oy);
            painter.circle_filled(o, 3.5, axis);
            for (tip, label) in [
                (proj(ox + grid.pitch_mm, oy), "+X"),
                (proj(ox, oy + grid.pitch_mm), "+Y"),
            ] {
                painter.line_segment([o, tip], (2.0, axis));
                painter.circle_filled(tip, 2.5, axis);
                painter.text(
                    egui::pos2(tip.x + 4.0, tip.y - 4.0),
                    egui::Align2::LEFT_BOTTOM,
                    label,
                    egui::FontId::proportional(12.0),
                    axis,
                );
            }
            painter.text(
                egui::pos2(o.x + 6.0, o.y + 6.0),
                egui::Align2::LEFT_TOP,
                format!("{ox:.0},{oy:.0} mm"),
                egui::FontId::proportional(11.0),
                axis,
            );

            // Readout, top-left of the frame.
            let worst = cal.dots.iter().map(|d| d.resid_um).fold(0.0_f64, f64::max);
            let txt = format!(
                "anchor {}/{} · RMS {:.0} µm · worst {:.0} µm",
                cal.found, cal.total, cal.rms_um, worst
            );
            let pos = egui::pos2(rect.min.x + 6.0, rect.min.y + 6.0);
            painter.rect_filled(
                egui::Rect::from_min_size(pos, egui::vec2(9.0 * txt.len() as f32, 18.0)),
                3.0_f32,
                Color32::from_black_alpha(150),
            );
            painter.text(
                egui::pos2(pos.x + 3.0, pos.y + 2.0),
                egui::Align2::LEFT_TOP,
                txt,
                egui::FontId::monospace(12.0),
                Color32::WHITE,
            );
        }

        // Laser-field feedback: the nonlinear commanded→physical→camera
        // lattice should pass through the detected burns. Orange vectors show
        // the raw field error from the desired physical coordinate to where the
        // command actually landed; ring color is the post-fit residual.
        if self.calibration.show_fit_feedback
            && self.calibration.mode == CalibMode::LaserField
            && let (Some(cal), Some(lens)) = (&self.calibration.field, &self.calibration.lens)
        {
            let grid = self.calib_grid();
            let pts = grid.points();
            let nonlinear: Option<Vec<egui::Pos2>> = pts
                .iter()
                .map(|&mm| {
                    calib::commanded_to_camera_px(&lens.lens, &cal.paper_to_machine, &cal.field, mm)
                        .map(|p| to_screen(p.0, p.1))
                })
                .collect();
            if let Some(nodes) = nonlinear {
                let n = grid.n;
                let node = |r: usize, c: usize| nodes[r * n + c];
                let mesh = Color32::from_rgb(0x35, 0x70, 0xb0);
                for r in 0..n {
                    for c in 0..n {
                        if c + 1 < n {
                            painter.line_segment([node(r, c), node(r, c + 1)], (1.2, mesh));
                        }
                        if r + 1 < n {
                            painter.line_segment([node(r, c), node(r + 1, c)], (1.2, mesh));
                        }
                    }
                }

                let orange = Color32::from_rgb(0xf0, 0x90, 0x30);
                let green = Color32::from_rgb(0x40, 0xc0, 0x50);
                let amber = Color32::from_rgb(0xe0, 0x90, 0x20);
                let red = Color32::from_rgb(0xd0, 0x40, 0x40);
                let magenta = Color32::from_rgb(0xe0, 0x40, 0xd0);
                for d in &cal.dots {
                    let det = to_screen(d.px.0, d.px.1);
                    if let Some(desired) = calib::physical_to_camera_px(
                        &lens.lens,
                        &cal.paper_to_machine,
                        d.commanded_mm,
                    ) {
                        painter.line_segment([to_screen(desired.0, desired.1), det], (1.2, orange));
                    }
                    // An excluded dot did not shape the map, so its residual
                    // colour would be misleading: mark it as struck out instead
                    // — a magenta ring with an ✕ through it, at double radius so
                    // it is findable on the sheet without hunting.
                    if d.rejected {
                        let r = 8.0_f32;
                        let stroke = egui::Stroke::new(2.0_f32, magenta);
                        painter.circle_stroke(det, r, stroke);
                        let d1 = r * std::f32::consts::FRAC_1_SQRT_2;
                        painter.line_segment(
                            [det + egui::vec2(-d1, -d1), det + egui::vec2(d1, d1)],
                            stroke,
                        );
                        painter.line_segment(
                            [det + egui::vec2(-d1, d1), det + egui::vec2(d1, -d1)],
                            stroke,
                        );
                        continue;
                    }
                    let col = if d.resid_um < 50.0 {
                        green
                    } else if d.resid_um < 100.0 {
                        amber
                    } else {
                        red
                    };
                    painter.circle_stroke(det, 4.0, egui::Stroke::new(1.5_f32, col));
                }

                let raw_worst = cal.dots.iter().map(|d| d.field_um).fold(0.0_f64, f64::max);
                let excluded = if cal.rejected > 0 {
                    format!(" · {} ✕ EXCLUDED", cal.rejected)
                } else {
                    String::new()
                };
                let txt = format!(
                    "field {} {}/{}{excluded} · raw worst {:.0} µm · fit RMS/worst {:.0}/{:.0} µm",
                    if self.calibration.field_accepted {
                        "accepted"
                    } else {
                        "REJECTED"
                    },
                    cal.found,
                    cal.total,
                    raw_worst,
                    cal.field.rms_um,
                    cal.field.max_um
                );
                let pos = egui::pos2(rect.min.x + 6.0, rect.min.y + 6.0);
                painter.rect_filled(
                    egui::Rect::from_min_size(pos, egui::vec2(8.0 * txt.len() as f32, 18.0)),
                    3.0_f32,
                    Color32::from_black_alpha(170),
                );
                painter.text(
                    egui::pos2(pos.x + 3.0, pos.y + 2.0),
                    egui::Align2::LEFT_TOP,
                    txt,
                    egui::FontId::monospace(12.0),
                    if self.calibration.field_accepted {
                        Color32::WHITE
                    } else {
                        red
                    },
                );
            } else {
                self.draw_projection_warning(ui, xf, "laser-field map returned a non-finite grid");
            }
        }

        for (i, &(px, py)) in self.calibration.corners.iter().enumerate() {
            let c = to_screen(px, py);
            painter.circle_stroke(c, 9.0, egui::Stroke::new(2.0_f32, cyan));
            painter.line_segment(
                [egui::pos2(c.x - 12.0, c.y), egui::pos2(c.x + 12.0, c.y)],
                (1.0, cyan),
            );
            painter.line_segment(
                [egui::pos2(c.x, c.y - 12.0), egui::pos2(c.x, c.y + 12.0)],
                (1.0, cyan),
            );
            painter.text(
                egui::pos2(c.x + 10.0, c.y - 10.0),
                egui::Align2::LEFT_BOTTOM,
                ["LL", "LR", "UR", "UL"][i],
                egui::FontId::proportional(13.0),
                cyan,
            );
        }
    }
}

impl ConsoleApp {
    pub(super) fn calibrate_view(&mut self, ui: &mut egui::Ui) {
        // Live capture is pumped from ui() regardless of tab (LR-45).
        //
        // The controls (step selector, description, form grid, buttons and the
        // multi-line fit notes) had grown tall enough to squeeze the camera
        // image into a sliver. Put them in a resizable, scrollable top panel so
        // the operator can drag the split and the image below gets real room.
        egui::TopBottomPanel::top("calib-controls")
            .resizable(true)
            .default_height(300.0)
            .min_height(100.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("calib-controls-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.calibrate_controls(ui));
            });
        self.calib_frame_overlay(ui);
    }

    /// The Calibrate tab's control block (everything above the camera image):
    /// step selector, description, form grid, button rows, and the fit notes.
    /// Lives in its own resizable/scrollable panel so it can't crowd out the
    /// image (see `calibrate_view`).
    fn calibrate_controls(&mut self, ui: &mut egui::Ui) {
        // Four steps no longer fit one row at narrow panel widths — wrap.
        ui.horizontal_wrapped(|ui| {
            ui.label("step");
            ui.selectable_value(
                &mut self.calibration.mode,
                CalibMode::CameraLens,
                "1) Camera lens (printed grid)",
            );
            ui.selectable_value(
                &mut self.calibration.mode,
                CalibMode::LaserAnchor,
                "2) Laser anchor (approximate)",
            );
            ui.selectable_value(
                &mut self.calibration.mode,
                CalibMode::LaserField,
                "3) Laser field (burned grid)",
            );
            ui.selectable_value(
                &mut self.calibration.mode,
                CalibMode::FidHoles,
                "4) Fiducial holes (board)",
            );
        });
        match self.calibration.mode {
            CalibMode::CameraLens => ui.label(egui::RichText::new(
                "Correct the camera lens: print a dot grid, tape it to the bed, image it, mark the \
                 4 corners, and Fit. Enter the MEASURED printed pitch (calipers) so distances are true. \
                 The arrows show the lens distortion; the color shows how well it was corrected.",
            ).weak()),
            CalibMode::LaserAnchor => ui.label(egui::RichText::new(
                "Approximate homography anchor: burn a dot grid at known coordinates, leave it taped down, image it, \
                 mark the 4 corners, and Fit. Re-anchor / ● Live re-lock to that fixed grid as the camera \
                 moves. It cannot model lens/field curvature; steps 1+3 provide the corrected nonlinear projection.",
            ).weak()),
            CalibMode::LaserField => ui.label(egui::RichText::new(
                "Correct the laser field: needs step 1 (Camera lens) first (the metric ruler), at the same camera \
                 pose, resolution, crop, and orientation. Burn a dot grid at \
                 known coordinates, image it, mark the 4 corners, and Fit — this measures where each command \
                 physically lands and fits a pre-distortion. Accepted fits are mandatory for every production \
                 export; geometry is always field-warped so shapes burn dimensionally true.",
            ).weak()),
            CalibMode::FidHoles => ui.label(egui::RichText::new(
                "Auto-lay four fiducial holes for a board centred on the laser field centre. Enter the \
                 board size and an edge margin; the holes sit that margin in from each board edge, and \
                 the layout feeds the fiducial check automatically. Generated with lens pre-distortion \
                 so hole spacing and size burn true.",
            ).weak()),
        };
        // ④ Fiducial holes has its own self-contained form — no grid params,
        // corner marking, or fit controls apply.
        if self.calibration.mode == CalibMode::FidHoles {
            self.fid_holes_controls(ui);
            return;
        }
        let is_paper = self.calibration.mode == CalibMode::CameraLens;
        ui.label(
            egui::RichText::new(if is_paper {
                "Step 1 has its own printed-paper parameters — edits here don't touch the steps 2/3 \
                 burned-grid set."
            } else {
                "Steps 2/3 share the burned-grid parameters — the step-1 printed-paper set is separate."
            })
            .weak(),
        );
        egui::Grid::new("calib-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("dots per side");
                ui.add(
                    egui::DragValue::new(&mut self.calibration.active_params_mut().n).range(2..=15),
                );
                ui.end_row();
                ui.label(if is_paper {
                    "measured pitch mm (paper)"
                } else {
                    "commanded pitch mm"
                });
                ui.add(
                    egui::DragValue::new(&mut self.calibration.active_params_mut().pitch_mm)
                        .speed(0.1)
                        .range(1.0..=50.0),
                )
                .on_hover_text(
                    "Camera mode: the pitch you MEASURED on the printed sheet with calipers — \
                     printers scale, so measure it. Laser mode: the commanded pitch you burned.",
                );
                ui.end_row();
                ui.label("dot ⌀ mm");
                ui.add(
                    egui::DragValue::new(&mut self.calibration.active_params_mut().dot_mm)
                        .speed(0.05)
                        .range(0.05..=5.0),
                );
                ui.end_row();
                ui.label("dot contrast").on_hover_text(
                    "How the dots read against their background. A printed grid or a \
                     burn that darkens the plate is dark-on-light; an ablated mark that \
                     brightens a dark surface (or a backlit hole) is bright-on-dark. If \
                     the fit finds 0 dots, this is usually the wrong setting.",
                );
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.calibration.active_params_mut().dot_kind,
                        calib::DotKind::Dark,
                        "◉ dark-on-light",
                    )
                    .on_hover_text("Printed grid or dark-anodized burn.");
                    ui.selectable_value(
                        &mut self.calibration.active_params_mut().dot_kind,
                        calib::DotKind::Bright,
                        "◎ bright-on-dark",
                    )
                    .on_hover_text("Ablated mark on a dark plate, or a backlit hole.");
                });
                ui.end_row();
                if self.calibration.mode == CalibMode::CameraLens {
                    let lbl = ui.label("paper out .svg");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.calibration.paper_out)
                            .desired_width(240.0),
                    )
                    .labelled_by(lbl.id)
                    .on_hover_text(
                        "Where the printable A4 dot-grid SVG is written. Print it at 100%.",
                    );
                    ui.end_row();
                    // Recorded here, with the fit it describes: the lens map
                    // reads on whatever plane the sheet lay on, and that is
                    // only known while the sheet is on the bed.
                    let height_label = ui.label("paper grid height mm");
                    ui.add(
                        egui::DragValue::new(&mut self.calibration.paper_height_mm)
                            .speed(0.1)
                            .range(-PLANE_HEIGHT_MAX_MM..=PLANE_HEIGHT_MAX_MM),
                    )
                    .labelled_by(height_label.id)
                    .on_hover_text(
                        "Height above the bed surface, positive up, that this printed \
                         paper grid sat at — the sheet plus whatever it lay on, mm. \
                         The step-1 lens map reads on that plane, so it is what the \
                         other two heights are measured against; only DIFFERENCES \
                         between the three heights affect anything.",
                    );
                    ui.end_row();
                }
                if self.calibration.mode != CalibMode::CameraLens {
                    ui.label("grid out .lbrn2");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.calibration.grid_out)
                            .desired_width(240.0),
                    );
                    ui.end_row();
                    ui.label("work area (mm)").on_hover_text(
                        "Your laser's addressable area as LightBurn shows it: the \
                             work-area square's centre and side length. Generate centres \
                             the grid here so it lands inside the field. (Same values as \
                             the Camera-tab overlay.)",
                    );
                    ui.horizontal(|ui| {
                        let size_label = ui.label("size");
                        ui.add(
                            egui::DragValue::new(&mut self.camera.field_mm)
                                .speed(1.0)
                                .range(10.0..=400.0),
                        )
                        .labelled_by(size_label.id);
                        ui.checkbox(&mut self.camera.field_center_auto, "auto center")
                            .on_hover_text(
                                "Keep the centre at half the field size (for example, \
                                 70 mm → 35,35). Turn this off if LightBurn shows an \
                                 offset or centre-origin field.",
                            );
                        self.sync_auto_field_center();
                        let cx_label = ui.label("cx");
                        ui.add_enabled(
                            !self.camera.field_center_auto,
                            egui::DragValue::new(&mut self.camera.field_cx_mm).speed(0.5),
                        )
                        .labelled_by(cx_label.id);
                        let cy_label = ui.label("cy");
                        ui.add_enabled(
                            !self.camera.field_center_auto,
                            egui::DragValue::new(&mut self.camera.field_cy_mm).speed(0.5),
                        )
                        .labelled_by(cy_label.id);
                    });
                    ui.end_row();
                }
                if self.calibration.mode == CalibMode::LaserField {
                    // Same reasoning as the ① paper height: the plane the grid
                    // was burned at is a fact about this fit.
                    let height_label = ui.label("laser grid height mm");
                    ui.add(
                        egui::DragValue::new(&mut self.calibration.laser_height_mm)
                            .speed(0.1)
                            .range(-PLANE_HEIGHT_MAX_MM..=PLANE_HEIGHT_MAX_MM),
                    )
                    .labelled_by(height_label.id)
                    .on_hover_text(
                        "Height above the bed surface, positive up, that this field \
                         grid was BURNED at — the plate's top face, mm. The grid was \
                         measured through the camera at that height, so the machine \
                         frame and field polynomial it anchors are keyed on readings \
                         of features there; only DIFFERENCES between the three \
                         heights affect anything.",
                    );
                    ui.end_row();
                    let rms_label = ui.label("accept RMS µm");
                    ui.add(
                        egui::DragValue::new(&mut self.calibration.accept_rms_um)
                            .speed(1.0)
                            .range(10.0..=1000.0),
                    )
                    .labelled_by(rms_label.id)
                    .on_hover_text(
                        "Reject the step 3 field fit when its residual RMS exceeds this. \
                         Raise it to accept a fit as good as the camera/optics can measure.",
                    );
                    ui.end_row();
                    let worst_label = ui.label("accept worst µm");
                    ui.add(
                        egui::DragValue::new(&mut self.calibration.accept_worst_um)
                            .speed(1.0)
                            .range(10.0..=1000.0),
                    )
                    .labelled_by(worst_label.id)
                    .on_hover_text(
                        "Reject the step 3 field fit when its worst per-dot residual exceeds this.",
                    );
                    ui.end_row();
                }
                let lbl = ui.label("grid frame (optional)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.calibration.frame).desired_width(240.0),
                )
                .labelled_by(lbl.id)
                .on_hover_text("Empty = grab from the camera; a path checks a saved grid image.");
                ui.end_row();
            });
        ui.horizontal(|ui| {
            if self.calibration.mode == CalibMode::CameraLens
                && ui
                    .button("Generate paper grid")
                    .on_hover_text("Emit a print-ready A4 SVG dot grid — print it at 100%.")
                    .clicked()
            {
                self.calibrate_generate_paper_grid();
            }
            if self.calibration.mode != CalibMode::CameraLens
                && ui
                    .button("Generate grid")
                    .on_hover_text("Emit the dot grid to burn.")
                    .clicked()
            {
                self.calibrate_generate_grid();
            }
            if ui.button("⤵ Load grid frame").clicked() {
                let ctx = ui.ctx().clone();
                self.calibrate_load_frame(&ctx);
            }
            if ui.button("🎯 Fit").clicked() {
                self.calibrate_fit();
            }
            if ui.button("↺ clear corners").clicked() {
                self.calibration.corners.clear();
            }
            ui.checkbox(&mut self.calibration.show_fit_feedback, "show fit feedback")
                .on_hover_text(
                    "Draw the post-fit overlay (step 1 lens arrows, step 2 anchor mesh, \
                 step 3 field lattice and rejection banner). Turn it off to see the bare \
                 dots when re-clicking the 4 corners. Loading a fresh frame hides it; a \
                 successful Fit shows it.",
                );
        });

        // ③ only: what the fit does about a large machine-scale error, next to
        // the Fit control that uses it. Radio buttons rather than a combo so
        // each choice is its own labelled, clickable node.
        if self.calibration.mode == CalibMode::LaserField {
            ui.label("Machine scale error:");
            for mode in FIELD_SCALE_ALL {
                let hint = match mode {
                    calib::FieldScale::Refuse => {
                        "Refuse the fit when the burn reads more than 5% off the paper ruler. A \
                         gross mismatch is usually a setup error (wrong pitch entered, camera \
                         moved, paper out of the burn plane), not a real field — fix it at the \
                         source."
                    }
                    calib::FieldScale::Compensate => {
                        "Absorb the scale error into the field correction. Shapes burn \
                         dimensionally true, but command space stretches by the same factor, so \
                         the usable work area shrinks — and the machine's speeds and hatch \
                         spacing stay in its own units, so energy density changes; re-tune \
                         power/speed after enabling."
                    }
                    calib::FieldScale::DistortionOnly => {
                        "Extrapolate the radial distortion to the work area without shrinking it. \
                         The uniform scale is measured and reported but divided out, so the \
                         correction is 1:1 — commanding 90 mm still asks for 90 mm — and it \
                         applies beyond the burned grid's span. Burns stay off size by the \
                         reported scale until the machine's field size is fixed."
                    }
                };
                ui.radio_value(
                    &mut self.calibration.field_scale,
                    mode,
                    field_scale_label(mode),
                )
                .on_hover_text(hint);
            }
        }

        // Mode-specific status + controls.
        match self.calibration.mode {
            CalibMode::CameraLens => {
                let (status, ok) = match &self.calibration.lens {
                    Some(c) => (
                        format!(
                            "● lens calibrated ({} dots, RMS {:.0} µm, worst {:.0} µm)",
                            c.found, c.lens.rms_um, c.lens.max_um
                        ),
                        true,
                    ),
                    None => (
                        "○ lens not calibrated — the camera isn't a metric ruler yet".to_string(),
                        false,
                    ),
                };
                ui.colored_label(status_color(ok), status);
                if self.calibration.lens.is_some() {
                    ui.add(
                        egui::Slider::new(&mut self.calibration.lens_arrow_scale, 1.0..=100.0)
                            .text("distortion arrow ×"),
                    );
                }
            }
            CalibMode::LaserAnchor => {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            self.calibration.anchor.is_some(),
                            egui::Button::new("⟳ Re-anchor"),
                        )
                        .on_hover_text(
                            "Re-fit from a fresh camera frame without re-clicking corners — \
                             corrects for the camera having moved (grid must still be in view).",
                        )
                        .clicked()
                    {
                        self.calibrate_re_anchor();
                    }
                    ui.add_enabled_ui(self.calibration.anchor.is_some(), |ui| {
                        ui.checkbox(&mut self.calibration.live, "● Live anchor")
                            .on_hover_text(
                                "Continuously re-anchor every frame so the calibration tracks the \
                             camera as it moves. Keep the burned grid in view.",
                            );
                    });
                    let can_edit = self
                        .calibration
                        .anchor
                        .as_ref()
                        .is_some_and(|anchor| !anchor.dots.is_empty());
                    let edit = ui
                        .add_enabled(
                            can_edit,
                            egui::Checkbox::new(
                                &mut self.calibration.edit_anchor_dots,
                                "Correct detected dots",
                            ),
                        )
                        .on_hover_text(
                            "Review the fitted squares on the image. Left-click a burn to add or move its center; right-click a detected center to remove it.",
                        );
                    if edit.changed() && self.calibration.edit_anchor_dots {
                        self.calibration.live = false;
                        self.calibration.note =
                            "dot correction active: left-click a square; right-click removes a dot"
                                .into();
                    }
                    if self.calibration.live {
                        ui.spinner();
                    }
                });
                let (status, ok) = match &self.calibration.anchor {
                    // Restored from disk (not re-confirmed this session): report
                    // its age so "old" is distinguishable from "just now".
                    Some(c) if c.found == 0 => {
                        let age = match self.calibration.saved_at {
                            Some(t) => human_age(now_unix().saturating_sub(t)),
                            None => "age unknown".into(),
                        };
                        (
                            format!("◐ saved calibration, {age} — ⟳ Re-anchor to confirm"),
                            false,
                        )
                    }
                    Some(c) => {
                        let worst = c.dots.iter().map(|d| d.resid_um).fold(0.0_f64, f64::max);
                        (
                            format!(
                                "● anchored this session ({}/{} dots, RMS {:.0} µm, worst {:.0} µm)",
                                c.found, c.total, c.rms_um, worst
                            ),
                            true,
                        )
                    }
                    None => (
                        "○ no camera calibration — never anchored (Place uses the design frame only)"
                            .to_string(),
                        false,
                    ),
                };
                ui.colored_label(status_color(ok), status);
                if self
                    .calibration
                    .anchor
                    .as_ref()
                    .is_some_and(|c| !c.dots.is_empty())
                {
                    if self.calibration.edit_anchor_dots {
                        ui.colored_label(
                            Color32::from_rgb(0xf0, 0xc0, 0x40),
                            "Correction active — left-click a square center; right-click removes a detection.",
                        );
                    }
                    ui.add(
                        egui::Slider::new(&mut self.calibration.anchor_resid_scale, 1.0..=100.0)
                            .text("residual ×"),
                    );
                    ui.weak(
                        "Blue mesh = the laser's coordinate grid seen by the camera. \
                         Dots: green = tight, amber/red = loose; orange vectors (× exaggerated) \
                         point commanded→detected. Hollow red squares = dots that didn't lock.",
                    );
                }
            }
            CalibMode::LaserField => {
                if self.calibration.lens.is_none() {
                    ui.colored_label(
                        status_color(false),
                        "○ needs step 1 (Camera lens) first — that metric ruler measures where burns land",
                    );
                }
                let (status, ok) = match &self.calibration.field {
                    Some(c) => {
                        let worst = c.dots.iter().map(|d| d.field_um).fold(0.0_f64, f64::max);
                        // Distortion-only leaves the machine's mis-size in
                        // place, so the mode and the measured scale belong in
                        // the standing status, not just the one-shot fit note:
                        // otherwise a real mis-size reads as "accepted" and
                        // disappears.
                        let mode = if self.calibration.field_scale_used
                            == calib::FieldScale::DistortionOnly
                        {
                            format!(
                                ", distortion only — machine scale {:+.1}% measured, NOT corrected",
                                (c.scale - 1.0) * 100.0
                            )
                        } else {
                            String::new()
                        };
                        // The residuals are over the SURVIVORS, so an accepted
                        // fit that only passed once a dot was excluded has to
                        // keep saying so for as long as it is the active one.
                        let excluded = if c.rejected > 0 {
                            format!(", {} EXCLUDED as outliers", c.rejected)
                        } else {
                            String::new()
                        };
                        (
                            format!(
                                "{} field fit ({}/{} dots{excluded}, raw worst {:.0} µm, fit RMS/worst {:.0}/{:.0} µm{mode})",
                                if self.calibration.field_accepted {
                                    "● accepted"
                                } else {
                                    "⚠ rejected"
                                },
                                c.found,
                                c.total,
                                worst,
                                c.field.rms_um,
                                c.field.max_um
                            ),
                            self.calibration.field_accepted,
                        )
                    }
                    None => (
                        "○ laser field not calibrated — Etch here emits uncorrected geometry"
                            .to_string(),
                        false,
                    ),
                };
                ui.colored_label(status_color(ok), status);
                if let Some(c) = &self.calibration.field {
                    use vision::FieldPattern;
                    // `ok` (green) marks a CONCLUSIVE read either way — genuine
                    // distortion to correct, or confirmed clean (nothing to
                    // correct). Amber is reserved for actual uncertainty: not
                    // enough/well-distributed data to tell signal from scatter.
                    let (glyph, verdict_ok) = match c.field_verdict.pattern {
                        FieldPattern::Systematic { .. }
                        | FieldPattern::NonRadial
                        | FieldPattern::UniformScale => ("⬤", true),
                        FieldPattern::Noise => ("○", true),
                        FieldPattern::Borderline | FieldPattern::Inconclusive(_) => ("?", false),
                    };
                    ui.colored_label(
                        status_color(verdict_ok),
                        format!(
                            "{glyph} {}",
                            field_verdict_phrase(&c.field_verdict, c.scale)
                        ),
                    );
                    // Outlier rejection is never allowed to be a silent step:
                    // the same sentence stands here for as long as the fit is
                    // active, in the ✕ colour the overlay strikes those dots
                    // out with. Also carries the "nothing was excluded, and
                    // here is why" cases, so `rejected == 0` is not the test.
                    if !c.rejection_note.is_empty() {
                        ui.colored_label(
                            Color32::from_rgb(0xe0, 0x40, 0xd0),
                            format!("✕ {}", c.rejection_note),
                        );
                    }
                    let hint = if !self.calibration.field_accepted {
                        format!(
                            "This fit did not meet 80% + four-corner + {:.0}/{:.0} µm acceptance; recapture before use.",
                            self.calibration.accept_rms_um, self.calibration.accept_worst_um
                        )
                    } else {
                        "Field warping is mandatory and active for every production console export."
                            .to_string()
                    };
                    if self.calibration.field_accepted {
                        ui.weak(format!(
                            "Correction file: {}. {hint}",
                            self.field_map_path().display()
                        ));
                    } else {
                        ui.colored_label(status_color(false), hint);
                    }
                }
            }
            // ④ returns early above; this arm only satisfies the match.
            CalibMode::FidHoles => {}
        }
        ui.label(egui::RichText::new(&self.calibration.note).weak());
        ui.weak(format!(
            "Click the 4 corner dots in order: {}.",
            self.corner_click_order()
        ));
        ui.weak(NAV_HINT);
    }

    /// ④ Fiducial holes: enter the fiducial rectangle's W/H, preview the
    /// computed layout + effective field centre, and generate the holes .lbrn2
    /// (writing the layout into the fiducial check first).
    fn fid_holes_controls(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("fid-holes-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("fiducial rect W mm");
                ui.add(
                    egui::DragValue::new(&mut self.fiducials.rect_w_mm)
                        .speed(0.5)
                        .range(5.0..=500.0),
                )
                .on_hover_text(
                    "Centre-to-centre x span of the four holes. The rectangle is \
                     centred in the work area, so no coordinates are needed.",
                );
                ui.end_row();
                ui.label("fiducial rect H mm");
                ui.add(
                    egui::DragValue::new(&mut self.fiducials.rect_h_mm)
                        .speed(0.5)
                        .range(5.0..=500.0),
                )
                .on_hover_text("Centre-to-centre y span of the four holes.");
                ui.end_row();
                ui.label("shape");
                egui::ComboBox::from_id_salt("fid-holes-shape")
                    .selected_text(self.fiducials.shape.label())
                    .show_ui(ui, |ui| {
                        for k in crate::fiducial::ShapeKind::ALL {
                            ui.selectable_value(&mut self.fiducials.shape, k, k.label());
                        }
                    });
                ui.end_row();
                match self.fiducials.shape {
                    crate::fiducial::ShapeKind::Circle => {
                        ui.label("hole ⌀ mm");
                        ui.add(
                            egui::DragValue::new(&mut self.fiducials.diameter_mm)
                                .speed(0.05)
                                .range(0.05..=20.0),
                        );
                        ui.end_row();
                    }
                    crate::fiducial::ShapeKind::Rect => {
                        ui.label("width mm");
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::DragValue::new(&mut self.fiducials.diameter_mm)
                                    .speed(0.05)
                                    .range(0.05..=20.0),
                            );
                            let hl = ui.label("height mm");
                            ui.add(
                                egui::DragValue::new(&mut self.fiducials.height_mm)
                                    .speed(0.05)
                                    .range(0.05..=20.0),
                            )
                            .labelled_by(hl.id);
                        });
                        ui.end_row();
                    }
                }
                let lbl = ui.label("holes out");
                ui.add(egui::TextEdit::singleline(&mut self.fiducials.out).desired_width(240.0))
                    .labelled_by(lbl.id)
                    .on_hover_text("Where the generated fiducial-holes .lbrn2 is written.");
                ui.end_row();
            });

        // Read the (now-edited) rectangle spans and the effective field centre
        // after the form closure so the preview + validation reflect this
        // frame's edits, not the last one.
        self.sync_auto_field_center();
        let (cx, cy) = (
            self.camera.field_cx_mm as f64,
            self.camera.field_cy_mm as f64,
        );
        let field = self.camera.field_mm as f64;
        let (w, h) = (self.fiducials.rect_w_mm, self.fiducials.rect_h_mm);
        let pts = crate::fiducial::centered_fid_layout(cx, cy, w, h);
        let layout = crate::fiducial::format_layout(&pts);
        ui.label(format!(
            "layout: {layout}   (rectangle centred at {cx:.1},{cy:.1})"
        ));

        // Validation: any hole outside the addressable field disables
        // generation. Centred, that is just a span wider than the field.
        let out_of_field = pts
            .iter()
            .any(|&(x, y)| x < 0.0 || y < 0.0 || x > field || y > field);
        if out_of_field {
            ui.colored_label(
                status_color(false),
                format!(
                    "holes fall outside the {field:.0} mm laser field — shrink the rectangle or \
                     recentre the field"
                ),
            );
        }

        ui.horizontal_wrapped(|ui| {
            if ui
                .button("⤵ rect from job board size")
                .on_hover_text(format!(
                    "Measure the board bbox from the active side's Gerbers and fill W/H, \
                     inset {FID_EDGE_INSET_MM:.0} mm per side so the holes land inside the board \
                     edge.",
                ))
                .clicked()
            {
                self.fid_holes_board_size_from_job();
            }
            if ui
                .add_enabled(
                    !out_of_field && !self.lightburn_busy(),
                    egui::Button::new("⚙ Generate fiducial holes → LightBurn (no burn)"),
                )
                .on_hover_text(
                    "Write this layout into the fiducial check and emit the holes .lbrn2 with \
                     laser-field pre-distortion, as a Line layer at the Job-tab drill settings, \
                     then LOAD it in LightBurn (FORCELOAD) without pressing start — you burn it \
                     from LightBurn yourself.",
                )
                .clicked()
            {
                self.fid_holes_generate();
            }
        });
        ui.label(egui::RichText::new(&self.fiducials.note).weak());
        ui.weak(NAV_HINT);
    }

    /// Measure the active side's board bbox from its Gerbers and fill the
    /// fiducial rectangle W/H (rounded to 0.01 mm, clamped to the field
    /// ranges). The bbox is the board OUTLINE, so it is inset
    /// [`FID_EDGE_INSET_MM`] per side to become a hole-centre span.
    fn fid_holes_board_size_from_job(&mut self) {
        let (copper, outline) = self.active_gerbers();
        let (copper, outline) = (copper.to_string(), outline.to_string());
        match job_shapes(&copper, &outline, self.job.offset_mm) {
            Ok((board, _cu, _ablate)) => match crate::place::bbox_size_mm(&board) {
                Some((w, h)) => {
                    let round2 = |v: f64| (v * 100.0).round() / 100.0;
                    let inset = |v: f64| round2(v - 2.0 * FID_EDGE_INSET_MM).clamp(5.0, 500.0);
                    let (bw, bh) = (round2(w), round2(h));
                    let (w, h) = (inset(w), inset(h));
                    self.fiducials.rect_w_mm = w;
                    self.fiducials.rect_h_mm = h;
                    self.runtime.log.push(LogLine {
                        text: format!(
                            "fiducial rect from job: board {bw}×{bh} mm (Gerber bbox) inset \
                             {FID_EDGE_INSET_MM} mm per side → {w}×{h} mm"
                        ),
                        err: false,
                    });
                }
                None => self.runtime.log.push(LogLine {
                    text: "fiducial rect from job: empty board region".into(),
                    err: true,
                }),
            },
            Err(e) => self.runtime.log.push(LogLine {
                text: format!("fiducial rect from job: {e}"),
                err: true,
            }),
        }
    }

    /// Compute the layout from the current rectangle + field centre, write it
    /// into the fiducial check's layout string, and generate the holes.
    fn fid_holes_generate(&mut self) {
        let (w, h) = (self.fiducials.rect_w_mm, self.fiducials.rect_h_mm);
        self.apply_fid_rect();
        let layout = self.fiducials.layout.clone();
        self.runtime.log.push(LogLine {
            text: format!("fiducial layout updated from a {w}×{h} mm centred rectangle → {layout}"),
            err: false,
        });
        self.fiducial_generate_holes();
    }
}
