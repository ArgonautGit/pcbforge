use super::*;

/// Reject an auto-placement whose fiducial fit residual (RMS over the detected
/// holes) exceeds this — a loose fit means the pairing or the layout is wrong,
/// and silently moving the job would be worse than leaving it.
const POSE_MAX_RMS_MM: f64 = 0.5;

impl ConsoleApp {
    /// Load the fiducial frame into memory + a texture and seed the search
    /// markers from the design layout (so they start near nominal, ready to
    /// drag onto the real holes).
    pub fn load_fid_frame(&mut self, ctx: &Context) {
        let img = match image::open(crate::clean_path(&self.fiducials.frame)) {
            Ok(i) => i.to_luma8(),
            Err(e) => {
                self.fiducials.note = format!("frame: {e}");
                return;
            }
        };
        // A file load has no auto-detect, so open the click-in-order marking
        // round: the operator marks each fiducial, and the final click runs the
        // check. (Grab/live auto-detect at the seeded positions instead.)
        if self.set_fid_frame(ctx, img) {
            self.start_fid_marking();
        }
    }

    /// Grab one frame from the camera (the source picked in the Camera tab —
    /// device or file), install it as the fiducial-check frame, and detect
    /// immediately. The one-click camera path for the fiducial check; ● Live
    /// does the same continuously.
    pub fn grab_fid_frame(&mut self, ctx: &Context) {
        match crate::camera::grab(&self.cam_source()) {
            Ok(img) => {
                let img = self.camera.orientation.apply(img);
                self.set_fid_frame(ctx, img);
                self.detect_fiducials(ctx);
            }
            Err(e) => self.fiducials.note = format!("camera: {e}"),
        }
    }

    /// Install `img` as the fiducial-check frame (texture + cache) and sync
    /// the markers, reporting a bad layout instead of silently proceeding.
    /// Returns whether the frame was installed (false = bad layout).
    fn set_fid_frame(&mut self, ctx: &Context, img: image::GrayImage) -> bool {
        if let Err(e) = fiducial::parse_layout(&self.fiducials.layout) {
            self.fiducials.note = format!("layout: {e}");
            return false;
        }
        self.sync_fid_markers();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let color = ColorImage {
            size: [w, h],
            pixels: img.pixels().map(|p| Color32::from_gray(p[0])).collect(),
        };
        self.fiducials.frame_tex =
            Some(ctx.load_texture("fid-frame", color, TextureOptions::NEAREST));
        self.fiducials.frame_img = Some(img);
        self.fiducials.note = "drag each ✛ near its hole, then Check".into();
        true
    }

    /// Open a click-the-fiducials-in-order round: the next primary canvas click
    /// drops marker 0, then 1, … and the final click runs the check. A no-op
    /// (round stays closed) when there are no markers to place.
    fn start_fid_marking(&mut self) {
        let n = self.fiducials.search.len();
        if n == 0 {
            self.fiducials.marking = None;
            return;
        }
        self.fiducials.marking = Some(0);
        self.fiducials.note =
            format!("click fiducial 1 of {n} (layout order) — or drag each ✛ and Check");
    }

    /// Reseed the ✛ set from the layout (the current frame stays) and reopen the
    /// click-in-order marking round — the ↺ reset-markers action.
    pub(super) fn reset_fid_markers(&mut self) {
        self.fiducials.search.clear();
        self.fiducials.found.clear();
        self.sync_fid_markers();
        self.start_fid_marking();
    }

    /// Apply one marking-round click: drop search marker `marking` at bed `mm`,
    /// clear its now-stale detection, and advance. The final marker's click
    /// closes the round and runs detection (which auto-updates the placement).
    /// Factored out of the canvas handler so tests can drive it directly.
    pub(super) fn fid_mark_click(&mut self, mm: (f64, f64), ctx: &Context) {
        let Some(k) = self.fiducials.marking else {
            return;
        };
        // The layout shrank under an active round (a typed edit): cancel rather
        // than index out of bounds.
        if k >= self.fiducials.search.len() {
            self.fiducials.marking = None;
            return;
        }
        self.fiducials.search[k] = mm;
        if let Some(f) = self.fiducials.found.get_mut(k) {
            *f = None; // this marker's old detection is stale now
        }
        let n = self.fiducials.search.len();
        let next = k + 1;
        if next >= n {
            self.fiducials.marking = None;
            self.detect_fiducials(ctx); // final click → detect + auto-place
        } else {
            self.fiducials.marking = Some(next);
            self.fiducials.note = format!("click fiducial {} of {n}", next + 1);
        }
    }

    /// Append an expected fiducial at bed `(mx, my)` mm to the layout and sync
    /// the markers (FLD-12 click-to-place). The layout string stays the source
    /// of truth, so the new ✛ appears and feeds the homography correspondences.
    pub(super) fn add_expected_fiducial(&mut self, mx: f64, my: f64) {
        let base = self.fiducials.layout.trim().trim_end_matches(';').trim();
        let sep = if base.is_empty() { "" } else { "; " };
        self.fiducials.layout = format!("{base}{sep}{mx:.1},{my:.1}");
        // Editing the marker set invalidates the "click N of M" ordering, so
        // cancel any active marking round (documented in the checkbox hover).
        self.fiducials.marking = None;
        self.sync_fid_markers();
        let n = self.fiducials.search.len();
        self.fiducials.note = format!(
            "added fiducial at ({mx:.1}, {my:.1}) mm  ·  {n} total (right-click a ✛ to remove)"
        );
    }

    /// Remove expected fiducial `i` (FLD-12 click-to-place). Drops the matching
    /// layout token — keeping the others' exact text — and the aligned search /
    /// found entries, so the ✛ set shrinks instead of only ever growing.
    pub(super) fn remove_expected_fiducial(&mut self, i: usize) {
        let tokens: Vec<String> = self
            .fiducials
            .layout
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
        self.fiducials.layout = kept.join("; ");
        if i < self.fiducials.search.len() {
            self.fiducials.search.remove(i);
        }
        if i < self.fiducials.found.len() {
            self.fiducials.found.remove(i);
        }
        // Keep the summary/ring rows aligned too, or colours and rows index
        // stale markers until the next Check (LR-40).
        if i < self.fiducials.rows.len() {
            self.fiducials.rows.remove(i);
        }
        // Editing the marker set invalidates the "click N of M" ordering, so
        // cancel any active marking round (documented in the checkbox hover).
        self.fiducials.marking = None;
        // Lengths already match; sync is a no-op reconcile (and re-seeds only if
        // the layout still parses).
        self.sync_fid_markers();
        self.fiducials.note = format!("removed fiducial #{i}  ·  {} left", kept.len());
    }

    /// Resize the draggable markers to match the design layout, preserving
    /// existing (dragged) positions and seeding any new ones from the layout —
    /// so adding a 4th coordinate makes a 4th ✛ appear without a manual reset.
    pub(super) fn sync_fid_markers(&mut self) {
        if fiducial::parse_layout(&self.fiducials.layout).is_err() {
            return;
        }
        // Seed from the side-aware expected positions (design on the front;
        // mirrored + beam-offset on the back).
        let expected = self.expected_points();
        let old = self.fiducials.search.len();
        self.fiducials.search.resize(expected.len(), (0.0, 0.0));
        for (i, d) in expected.iter().enumerate().skip(old) {
            self.fiducials.search[i] = *d;
        }
        self.fiducials
            .found
            .resize(self.fiducials.search.len(), None);
    }

    /// Detect around the current (draggable) search markers and record the
    /// found positions, summary rows, and measured scale.
    pub fn render_fiducials(&mut self, ctx: &Context) {
        self.sync_fid_markers();
        // No frame yet: pull one from the file path if set, else grab from
        // the camera — so Check works camera-first without a file.
        if self.fiducials.frame_img.is_none() {
            if crate::clean_path(&self.fiducials.frame).is_empty() {
                self.grab_fid_frame(ctx);
                return; // grab already detects (or reported the error)
            }
            self.load_fid_frame(ctx);
        }
        if self.fiducials.frame_img.is_none() {
            return;
        }
        if self.fiducials.search.is_empty() {
            self.fiducials.note = "load a frame first".into();
            return;
        }
        self.detect_fiducials(ctx);
    }

    /// Live fiducial tracking: pull frames from the (camera-tab) source and
    /// re-detect each one, so the rings track the holes as the board moves.
    /// Uses `cam_source`, so pick the device/file in the Camera tab.
    pub(super) fn pump_fid_live(&mut self, ctx: &Context) {
        if !self.fiducials.live {
            if self.fiducials.capture.is_some() {
                self.fiducials.capture = None;
                self.fiducials.capture_src = None;
            }
            return;
        }
        let src = self.cam_source();
        if self.fiducials.capture.is_none() || self.fiducials.capture_src.as_ref() != Some(&src) {
            self.fiducials.capture = None;
            self.fiducials.capture = Some(crate::camera::Capture::start(src.clone()));
            self.fiducials.capture_src = Some(src);
        }
        let latest = self.fiducials.capture.as_ref().and_then(|c| c.latest());
        if let Some(res) = latest {
            match res {
                Ok(gray) => {
                    let gray = self.camera.orientation.apply(gray);
                    let (w, h) = (gray.width() as usize, gray.height() as usize);
                    let color = ColorImage {
                        size: [w, h],
                        pixels: gray.pixels().map(|p| Color32::from_gray(p[0])).collect(),
                    };
                    self.fiducials.frame_tex =
                        Some(ctx.load_texture("fid-frame", color, TextureOptions::NEAREST));
                    self.fiducials.frame_img = Some(gray);
                    self.sync_fid_markers();
                    if !self.fiducials.search.is_empty() {
                        self.detect_fiducials(ctx);
                    }
                }
                Err(e) => {
                    self.fiducials.note = e;
                    self.fiducials.live = false;
                }
            }
        }
        ctx.request_repaint();
    }

    /// Emit fiducial holes at the expected positions by shelling `pcbforge
    /// fid-holes` — the operator burns them, then images them back for the
    /// check. Uses the same layout string the check drives from, so the burned
    /// holes land exactly where detection looks for them.
    pub(super) fn fiducial_generate_holes(&mut self) {
        if let Err(e) = crate::fiducial::parse_layout(&self.fiducials.layout) {
            self.fiducials.note = format!("layout: {e}");
            return;
        }
        let out = crate::clean_path(&self.fiducials.out);
        if out.is_empty() {
            self.fiducials.note = "set an output path for the fiducial holes".into();
            return;
        }
        // Circles pass --h-mm 0 (the CLI reads the diameter as the square
        // side); rectangles pass their real height.
        let h_mm = match self.fiducials.shape {
            crate::fiducial::ShapeKind::Circle => 0.0,
            crate::fiducial::ShapeKind::Rect => self.fiducials.height_mm,
        };
        let mut args: Vec<String> = vec![
            "fid-holes".into(),
            "--out".into(),
            out,
            "--layout".into(),
            self.fiducials.layout.clone(),
            "--shape".into(),
            self.fiducials.shape.token().into(),
            "--w-mm".into(),
            format!("{}", self.fiducials.diameter_mm),
            "--h-mm".into(),
            format!("{h_mm}"),
        ];
        // Pre-distort with the laser-field map when a usable calibration + map
        // file exist; otherwise burn uncorrected with a warning (mirrors the
        // job emit path).
        let field_path = self.field_map_path();
        if self.has_usable_field_cal() && field_path.exists() {
            args.push("--field-map".into());
            args.push(field_path.to_string_lossy().into_owned());
        } else {
            self.runtime.log.push(LogLine {
                text:
                    "fid-holes: no accepted step 1 (Camera lens) + step 3 (Laser field) \
                     calibration — holes will burn without lens correction (accept a laser field \
                     fit first)"
                        .into(),
                err: true,
            });
        }
        self.run_verb(&args);
        self.fiducials.note =
            "generating fiducial holes at the expected positions — see Log for the file path".into();
    }

    /// Run detection on the current in-memory frame around the search markers,
    /// updating rows/found/measured/homography. Shared by the static Check and
    /// the live-tracking loop (FLD-11).
    fn detect_fiducials(&mut self, ctx: &Context) {
        let Some(frame) = &self.fiducials.frame_img else {
            return;
        };
        let profile = self.fiducials.profile.to_profile(
            self.fiducials
                .shape
                .to_fid_shape(self.fiducials.diameter_mm, self.fiducials.height_mm),
        );
        let r = fiducial::check_frame(
            frame,
            &self.fiducials.search,
            self.fiducials.px_per_mm,
            &profile,
            self.fiducials.search_mm,
        );
        let (s, w, m) = r.tally;
        self.fiducials.rows = r.rows;
        self.fiducials.found = r.found_px;

        // Measure the camera scale from KNOWN design spacing paired with the
        // detected pixels — not the dragged search-marker spacing check_frame
        // uses internally, which a small drag turns into a scale error (LR-17).
        let design = fiducial::parse_layout(&self.fiducials.layout).unwrap_or_default();
        self.fiducials.measured_ppm = fiducial::scale_from_design(&design, &self.fiducials.found);
        let scale = match self.fiducials.measured_ppm {
            Some(p) => format!("  ·  measured {p:.2} px/mm"),
            None => String::new(),
        };
        self.fiducials.note = format!("{s} strong, {w} weak, {m} missed{scale}");

        // Perspective: with ≥4 detected fiducials, fit the design→pixel
        // homography (a tilted camera keystones the flat board). It corrects
        // the Place overlay and any downstream mapping; <4 can only be affine.
        let corr: Vec<_> = design
            .iter()
            .zip(&self.fiducials.found)
            .filter_map(|(&(dx, dy), f)| {
                f.map(|(px, py)| (nalgebra::Point2::new(dx, dy), nalgebra::Point2::new(px, py)))
            })
            .collect();
        self.fiducials.homography = if corr.len() >= 4 {
            match vision::fit_homography(&corr) {
                Ok(hgt) => {
                    self.fiducials
                        .note
                        .push_str(&format!("  ·  perspective fit (reproj {:.2} px)", hgt.rms));
                    Some(hgt)
                }
                Err(e) => {
                    self.fiducials
                        .note
                        .push_str(&format!("  ·  perspective: {e}"));
                    None
                }
            }
        } else {
            if !corr.is_empty() {
                self.fiducials
                    .note
                    .push_str("  ·  add a 4th fiducial for perspective");
            }
            None
        };

        self.update_placement_from_fiducials(ctx);
    }

    /// Fit the board's actual pose from the just-detected holes and write it
    /// into the Place tab (rotation + translation, mirror-aware), so a Check
    /// re-registers the job without a manual drag. The outcome — success or the
    /// specific reason it was skipped — is appended to the fiducial note; only
    /// an applied pose sets `placement.auto_pose` and caches `fiducials.pose`.
    fn update_placement_from_fiducials(&mut self, ctx: &Context) {
        // This detection's placement outcome: reset up front so every early
        // return below leaves it false, and only the successful apply sets it
        // true. The verdict line reads this so it never shows a stale
        // "placement updated" after a gated-out Check (e.g. a dropped fiducial
        // under Live), while `pose`/`auto_pose` keep their last-good value.
        self.fiducials.last_placed = false;
        // Capture the frame size up front so no frame borrow straddles the
        // later placement writes.
        let Some((w, h)) = self.fiducials.frame_img.as_ref().map(|f| f.dimensions()) else {
            return;
        };
        let Ok(layout) = fiducial::parse_layout(&self.fiducials.layout) else {
            return; // a bad layout was already surfaced above
        };
        // place_projection is the ONLY correct camera-px → machine-mm source
        // here (the design→px homography above would just recover the layout).
        let projection = match self.place_projection(w, h) {
            Ok(p) => p,
            Err(_) => {
                self.fiducials
                    .note
                    .push_str("  ·  no camera→machine calibration — placement not updated");
                return;
            }
        };
        // Detected machine mm, index-aligned with the layout; a px→mm failure
        // for one point drops just that point.
        let detected: Vec<Option<(f64, f64)>> = self
            .fiducials
            .found
            .iter()
            .map(|&f| f.and_then(|px| projection.from_px(px)))
            .collect();
        // Back side: the camera sees the drilled holes' EXIT openings, so fit
        // against the exit-magnified nominal positions.
        let exit = self.back_field_params();
        let pose = match fiducial::fit_board_pose(&layout, &detected, exit.as_ref()) {
            Ok(pose) => pose,
            Err(e) => {
                self.fiducials
                    .note
                    .push_str(&format!("  ·  placement not updated: {e}"));
                return;
            }
        };
        // The fit's mirror must match the working face, or the board is on the
        // wrong side (or not flipped): refuse rather than place a mirrored job.
        let flipped_expected = self.job.side == Side::Back;
        if pose.flipped != flipped_expected {
            self.fiducials.note.push_str(if flipped_expected {
                "  ·  fiducial pattern does not look flipped — still on the front face?"
            } else {
                "  ·  detected a mirrored fiducial pattern — is the board flipped? switch side to Back"
            });
            return;
        }
        if pose.rms_mm > POSE_MAX_RMS_MM {
            self.fiducials.note.push_str(&format!(
                "  ·  fiducial fit RMS {:.2} mm too loose — placement not updated",
                pose.rms_mm
            ));
            return;
        }
        self.placement.tx_mm = pose.tx_mm;
        self.placement.ty_mm = pose.ty_mm;
        self.placement.rot_deg = pose.rot_deg;
        self.placement.auto_pose = true;
        self.fiducials.last_placed = true;
        self.fiducials.pose = Some(pose);
        self.fiducials.note.push_str(&format!(
            "  ·  placement set from fiducials (rot {:+.2}°, RMS {:.2} mm)",
            pose.rot_deg, pose.rms_mm
        ));
        // Rebuild the Place overlay only if a frame is already loaded there.
        if self.placement.frame_img.is_some() {
            self.recompose(ctx);
        }
    }

    pub(super) fn fiducial_view(&mut self, ui: &mut egui::Ui) {
        // Live capture is pumped from ui() regardless of tab (LR-45).
        //
        // Match the Calibrate tab: the form + buttons + notes + summary had
        // grown tall enough to squeeze the frame into a sliver, so put them in a
        // resizable, scrollable top panel and let the image below take the rest.
        egui::TopBottomPanel::top("fid-controls")
            .resizable(true)
            .default_height(300.0)
            .min_height(100.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("fid-controls-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.fiducial_controls(ui));
            });
        self.fid_frame_overlay(ui);
    }

    /// The Fiducial-check tab's control block (everything above the frame
    /// image): form grid, button row, notes/hints, the colored verdict line and
    /// the per-fiducial summary rows. Lives in its own resizable/scrollable
    /// panel so it can't crowd out the image (see `fiducial_view`).
    fn fiducial_controls(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("fid-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                let lbl = ui.label("frame file (optional)");
                ui.add(egui::TextEdit::singleline(&mut self.fiducials.frame).desired_width(240.0))
                    .labelled_by(lbl.id)
                    .on_hover_text(
                        "Leave empty to use the camera (source picked in the \
                         Camera tab); set a path to check a saved image instead.",
                    );
                ui.end_row();
                let lbl = ui.label("expected (x,y mm; …)");
                ui.add(egui::TextEdit::singleline(&mut self.fiducials.layout).desired_width(240.0))
                    .labelled_by(lbl.id)
                    .on_hover_text(
                        "Fiducial positions in board/machine mm (Gerber frame, \
                         y up). On the un-calibrated uniform scale, bed (0,0) \
                         is the bottom-left of the camera frame.",
                    );
                ui.end_row();
                ui.label("px/mm (seed)");
                ui.add(
                    egui::DragValue::new(&mut self.fiducials.px_per_mm)
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
                    .selected_text(self.fiducials.profile.label())
                    .show_ui(ui, |ui| {
                        for k in crate::fiducial::ProfileKind::ALL {
                            ui.selectable_value(&mut self.fiducials.profile, k, k.label());
                        }
                    });
                ui.end_row();
                ui.label("shape");
                egui::ComboBox::from_id_salt("fid-shape")
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
                ui.label("search mm");
                ui.add(
                    egui::DragValue::new(&mut self.fiducials.search_mm)
                        .speed(0.1)
                        .range(0.1..=20.0),
                );
                ui.end_row();
                let lbl = ui.label("holes out");
                ui.add(
                    egui::TextEdit::singleline(&mut self.fiducials.out).desired_width(240.0),
                )
                .labelled_by(lbl.id)
                .on_hover_text(
                    "Where the generated fiducial-holes .lbrn2 is written (⚙ Generate holes).",
                );
                ui.end_row();
            });
        ui.horizontal(|ui| {
            if ui
                .button("📷 Grab & check")
                .on_hover_text(
                    "Grab one frame from the camera (source picked in the \
                     Camera tab) and run detection on it.",
                )
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.grab_fid_frame(&ctx);
            }
            if ui
                .button("⤵ Load frame")
                .on_hover_text("Load the frame file above instead of the camera.")
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.load_fid_frame(&ctx);
            }
            if ui.button("🎯 Check fiducials").clicked() {
                let ctx = ui.ctx().clone();
                self.render_fiducials(&ctx);
            }
            ui.checkbox(&mut self.fiducials.live, "● Live")
                .on_hover_text(
                    "Track fiducials on the live camera feed (source from the Camera tab).",
                );
            ui.checkbox(&mut self.fiducials.click_place, "✚ click-to-place")
                .on_hover_text(
                    "Left-click an empty spot to add an expected fiducial; \
                     right-click a ✛ to remove it; drag markers to fine-tune. \
                     Adding/removing here cancels an active marking round.",
                );
            if ui.button("↺ reset markers").clicked() {
                self.reset_fid_markers();
            }
            if ui
                .button("⚙ Generate holes")
                .on_hover_text(
                    "Burn a .lbrn2 with a hole at each expected position above — the same \
                     layout the check uses.",
                )
                .clicked()
            {
                self.fiducial_generate_holes();
            }
            if let Some(ppm) = self.fiducials.measured_ppm
                && ui
                    .button(format!("↧ use measured {ppm:.2} px/mm"))
                    .on_hover_text("Adopt the fiducial-measured scale for this and the Place tab.")
                    .clicked()
            {
                self.fiducials.px_per_mm = ppm;
                self.placement.px_per_mm = ppm;
            }
        });
        ui.label(egui::RichText::new(&self.fiducials.note).weak());
        ui.weak("⚙ Generate holes burns holes at the expected positions above — same layout the check uses.");
        ui.weak("Drag each ✛ near its hole; the detector searches locally around it. The typed px/mm only seeds the search — registration is anchored to the measured scale.");
        ui.weak(NAV_HINT);
        ui.separator();

        self.fid_verdict(ui);
        for row in &self.fiducials.rows {
            let color = match row.kind {
                FidKind::FoundStrong => Color32::from_rgb(0x50, 0xb0, 0x60),
                FidKind::FoundWeak => Color32::from_rgb(0xe0, 0x90, 0x20),
                FidKind::Miss => Color32::from_rgb(0xd0, 0x50, 0x50),
            };
            ui.colored_label(color, &row.text);
        }
    }

    /// One colored status line (Calibrate-style) summarizing the last check and
    /// whether it moved the Place placement. Derived from the cached
    /// tally/rows/pose — it never re-runs detection.
    fn fid_verdict(&mut self, ui: &mut egui::Ui) {
        let n = self.fiducials.found.len();
        let s = self.fiducials.found.iter().filter(|f| f.is_some()).count();
        if self.fiducials.frame_img.is_none() {
            ui.weak("○ no frame loaded");
        } else if self.fiducials.rows.is_empty() {
            ui.weak("○ not checked");
        } else if self.fiducials.last_placed {
            // last_placed ⇒ the pose was just written, so it's the fresh fit.
            let (rms, rot) = self
                .fiducials
                .pose
                .as_ref()
                .map_or((0.0, 0.0), |p| (p.rms_mm, p.rot_deg));
            ui.colored_label(
                status_color(true),
                format!(
                    "● {s}/{n} fiducials, RMS {rms:.2} mm — placement updated (rot {rot:+.2}°)"
                ),
            );
        } else {
            // Detection ran but the fit was gated out — the note carries why.
            ui.colored_label(
                status_color(false),
                format!("◐ {s}/{n} fiducials — placement not updated"),
            );
        }
    }

    /// The frame with draggable search markers (✛) and detected rings drawn on
    /// top via the painter — so markers move without re-rasterizing the image.
    fn fid_frame_overlay(&mut self, ui: &mut egui::Ui) {
        // Keep the marker count in step with the layout field (live), so
        // adding/removing a coordinate adds/removes a ✛.
        self.sync_fid_markers();
        let Some(tex) = self.fiducials.frame_tex.clone() else {
            ui.weak("(load a frame to place markers)");
            return;
        };
        let (xf, resp) = self.show_image(ui, "fiducial", &tex);
        let rect = xf.panel;
        let th = tex.size()[1] as f64;
        let ppm = self.fiducials.px_per_mm as f32;
        let ppm_f = self.fiducials.px_per_mm;
        let nav = crate::imgview::is_navigating(ui);
        // bed-mm ↔ screen: bed mm is y-up with its origin at the frame's
        // bottom-left (machine / Gerber frame) while image rows grow downward,
        // so flip against the native texture height, then map native px →
        // screen through the pan/zoom transform.
        let to_screen = |mmx: f64, mmy: f64| xf.to_screen(mmx * ppm_f, th - mmy * ppm_f);
        let px_to_screen = |px: f64, py: f64| xf.to_screen(px, py);
        let to_mm = |p: egui::Pos2| {
            let (ix, iy) = xf.to_native(p);
            (ix / ppm_f, (th - iy) / ppm_f)
        };

        // Click-to-place (FLD-12): screen positions of the current markers, for
        // hit-testing add (empty spot) vs. remove (right-click on a ✛).
        // Materialized (not a closure) so the `&self` borrow is released before
        // the `&mut self` add/remove calls below. Suppressed while navigating.
        if self.fiducials.click_place && !nav {
            let marker_px: Vec<(f32, f32)> = self
                .fiducials
                .search
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

        // Marking round: outside click-to-place, a primary click drops the next
        // marker in layout order (the final click closes the round + detects).
        // Suppressed while navigating.
        if self.fiducials.marking.is_some()
            && !self.fiducials.click_place
            && !nav
            && resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let mm = to_mm(pos);
            let ctx = ui.ctx().clone();
            self.fid_mark_click(mm, &ctx);
        }

        // Drag: pick the nearest marker on press, move it while dragging.
        // Suppressed while navigating (Ctrl+drag pans instead).
        if !nav
            && resp.drag_started()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let markers: Vec<(f32, f32)> = self
                .fiducials
                .search
                .iter()
                .map(|&(x, y)| {
                    let s = to_screen(x, y);
                    (s.x, s.y)
                })
                .collect();
            self.fiducials.drag = fiducial::nearest_marker(&markers, (pos.x, pos.y), 30.0);
        }
        if !nav
            && resp.dragged()
            && let (Some(i), Some(pos)) = (self.fiducials.drag, resp.interact_pointer_pos())
            && i < self.fiducials.search.len()
        {
            self.fiducials.search[i] = to_mm(pos);
        }
        if resp.drag_stopped() {
            self.fiducials.drag = None;
        }

        // Paint markers + detected rings.
        let painter = ui.painter_at(rect);
        let cyan = Color32::from_rgb(0x22, 0xcc, 0xdd);
        // While a marking round is active, ghost the markers not yet placed
        // (index ≥ the one being marked) so the next target reads at a glance.
        let ghost = Color32::from_rgba_unmultiplied(0x22, 0xcc, 0xdd, 90);
        let ring_r = (self.fiducials.diameter_mm as f32 * ppm * 0.5 * xf.scale).max(5.0);
        for (i, &(mx, my)) in self.fiducials.search.iter().enumerate() {
            let c = to_screen(mx, my);
            let mcol = match self.fiducials.marking {
                Some(k) if i >= k => ghost,
                _ => cyan,
            };
            painter.line_segment(
                [egui::pos2(c.x - 9.0, c.y), egui::pos2(c.x + 9.0, c.y)],
                (1.5, mcol),
            );
            painter.line_segment(
                [egui::pos2(c.x, c.y - 9.0), egui::pos2(c.x, c.y + 9.0)],
                (1.5, mcol),
            );
            painter.circle_stroke(c, 11.0, egui::Stroke::new(1.0_f32, mcol));
            // 1-based index label next to each ✛ (Calibrate's corner-label style).
            painter.text(
                egui::pos2(c.x + 12.0, c.y - 12.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{}", i + 1),
                egui::FontId::proportional(13.0),
                mcol,
            );
            if let Some(Some((fx, fy))) = self.fiducials.found.get(i) {
                let col = match self.fiducials.rows.get(i).map(|r| &r.kind) {
                    Some(FidKind::FoundStrong) => Color32::from_rgb(0x40, 0xc0, 0x50),
                    _ => Color32::from_rgb(0xe0, 0x90, 0x20),
                };
                let fc = px_to_screen(*fx, *fy);
                let stroke = egui::Stroke::new(2.0_f32, col);
                // A circle draws its ring; a rectangle draws its axis-aligned
                // outline (width×height) centered on the detected point.
                match self.fiducials.shape {
                    crate::fiducial::ShapeKind::Circle => {
                        painter.circle_stroke(fc, ring_r, stroke);
                    }
                    crate::fiducial::ShapeKind::Rect => {
                        let hw = (self.fiducials.diameter_mm as f32 * ppm * 0.5 * xf.scale).max(3.0);
                        let hh = (self.fiducials.height_mm as f32 * ppm * 0.5 * xf.scale).max(3.0);
                        let (l, r, t, b) = (fc.x - hw, fc.x + hw, fc.y - hh, fc.y + hh);
                        painter.line_segment([egui::pos2(l, t), egui::pos2(r, t)], stroke);
                        painter.line_segment([egui::pos2(r, t), egui::pos2(r, b)], stroke);
                        painter.line_segment([egui::pos2(r, b), egui::pos2(l, b)], stroke);
                        painter.line_segment([egui::pos2(l, b), egui::pos2(l, t)], stroke);
                    }
                }
                painter.circle_filled(fc, 2.0, col);
            }
        }
    }
}
