use std::time::{Duration, Instant};

use super::*;

/// Longest-side cap for the live camera *view* texture. The full-resolution
/// frame is always kept for calibration/detection; only the on-screen preview
/// is downscaled to this, so streaming a 2K/4K sensor stays cheap.
pub(super) const CAM_VIEW_MAX: usize = 1280;

/// How long the shared capture may sit unused before the console hands the
/// device back. Opening device N keeps every other program off it — a live
/// `pcbforge cam --grab --device 1` fails device-busy — so the fast path is
/// only worth holding while the operator is plausibly still using it. Ten
/// seconds outlasts the pause between two clicks of “grab once” (which is what
/// makes the second one cheap) without locking the CLI out for a shift.
pub(super) const CAMERA_IDLE_RELEASE: Duration = Duration::from_secs(10);

/// How long a one-shot grab waits for the first frame off a freshly opened
/// device. The measured warm-up is ~1.2–1.5 s (device init, not bandwidth —
/// it is ~1.04 s even at 1280×720), so 4 s leaves headroom for a slow device
/// while still failing with a message rather than hanging the console.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(4);

/// Poll interval while waiting for that first frame.
const FIRST_FRAME_POLL: Duration = Duration::from_millis(20);

/// Whether the shared capture should be dropped: nothing is streaming from it
/// and it has gone idle. Free-standing so the release rule is testable without
/// a camera or a running app.
pub(super) fn should_release_capture(
    any_live: bool,
    last_used: Option<Instant>,
    now: Instant,
    idle: Duration,
) -> bool {
    // No timestamp means nobody ever read it — release rather than hold the
    // device on a capture whose owner vanished.
    !any_live && last_used.is_none_or(|t| now.duration_since(t) >= idle)
}

/// Nearest-neighbour downscale of a display image so its longest side is
/// ≤ `max_dim`. Returns the (possibly unchanged) image and the applied ratio
/// (`new ÷ old`, `1.0` if untouched). View-only — never used on frame data.
pub(super) fn downscale_view(img: ColorImage, max_dim: usize) -> (ColorImage, f64) {
    let (w, h) = (img.size[0], img.size[1]);
    let longest = w.max(h);
    if longest <= max_dim || longest == 0 {
        return (img, 1.0);
    }
    let scale = max_dim as f64 / longest as f64;
    let nw = ((w as f64 * scale).round() as usize).max(1);
    let nh = ((h as f64 * scale).round() as usize).max(1);
    let mut pixels = Vec::with_capacity(nw * nh);
    for y in 0..nh {
        let sy = (((y as f64 + 0.5) / scale) as usize).min(h - 1);
        for x in 0..nw {
            let sx = (((x as f64 + 0.5) / scale) as usize).min(w - 1);
            pixels.push(img.pixels[sy * w + sx]);
        }
    }
    (
        ColorImage {
            size: [nw, nh],
            pixels,
        },
        nw as f64 / w as f64,
    )
}

impl ConsoleApp {
    /// The current camera source (device or file).
    pub(super) fn cam_source(&self) -> crate::camera::Source {
        if self.camera.use_device {
            crate::camera::Source::Device(self.camera.device)
        } else {
            crate::camera::Source::File(self.camera.file.clone())
        }
    }

    /// Start the shared capture for the current source, or restart it if the
    /// operator switched source, and mark it as wanted right now.
    pub(super) fn ensure_capture(&mut self) {
        let src = self.cam_source();
        if self.runtime.camera_capture.is_none()
            || self.runtime.camera_capture_src.as_ref() != Some(&src)
        {
            // Drop the old one first: its thread must release the device before
            // the new thread tries to open it.
            self.runtime.camera_capture = None;
            self.runtime.camera_capture = Some(crate::camera::Capture::start(src.clone()));
            self.runtime.camera_capture_src = Some(src);
        }
        // Stamp on creation too, not only on a successful read — a capture whose
        // first frame never arrives must still age out rather than linger.
        self.runtime.camera_last_used = Some(Instant::now());
    }

    /// The newest frame from the shared capture, non-blocking. This is what the
    /// tabs' ● Live pumps poll; none of them owns a `Capture` of its own.
    pub(super) fn capture_latest(&mut self) -> Option<Result<image::GrayImage, String>> {
        self.ensure_capture();
        self.runtime
            .camera_capture
            .as_ref()
            .and_then(|c| c.latest())
    }

    /// Grab one frame for the synchronous "grab once" paths, reusing the shared
    /// capture so the device is opened at most once instead of per grab.
    pub(super) fn grab_shared(&mut self) -> Result<image::GrayImage, String> {
        let src = self.cam_source();
        // A file grab is cheap and needs no device; routing it through a capture
        // thread would buy nothing and would swap a guaranteed-current read for
        // whatever frame the thread happened to have on hand.
        if matches!(src, crate::camera::Source::File(_)) {
            return crate::camera::grab(&src);
        }
        self.ensure_capture();
        // Fast path: the thread is already streaming, so the frame is waiting.
        // Slow path — only the FIRST grab after the device opens. That is the
        // ~1.2–1.5 s warm-up the device charges no matter who pays it; every
        // later grab takes the fast path and costs a slot read.
        let deadline = Instant::now() + FIRST_FRAME_TIMEOUT;
        let mut result = self
            .runtime
            .camera_capture
            .as_ref()
            .and_then(|c| c.latest());
        while result.is_none() && Instant::now() < deadline {
            std::thread::sleep(FIRST_FRAME_POLL);
            result = self
                .runtime
                .camera_capture
                .as_ref()
                .and_then(|c| c.latest());
        }
        match result {
            Some(Ok(frame)) => {
                self.runtime.camera_last_used = Some(Instant::now());
                Ok(frame)
            }
            other => {
                // Don't cache a failure for the whole idle window. A capture
                // whose device failed to open has already exited its thread, so
                // reusing it would starve every later grab into the timeout
                // below and bury the real error. Drop it and let the next grab
                // reopen and report what actually went wrong.
                self.runtime.camera_capture = None;
                self.runtime.camera_capture_src = None;
                self.runtime.camera_last_used = None;
                other.unwrap_or_else(|| {
                    Err(format!(
                        "no frame from the camera within {}s — check the device is \
                         connected and not in use by another program",
                        FIRST_FRAME_TIMEOUT.as_secs()
                    ))
                })
            }
        }
    }

    /// Hand the camera back when no tab is streaming and the shared capture has
    /// gone idle. Called every frame from [`ConsoleApp::ui`].
    pub(super) fn release_idle_capture(&mut self) {
        if self.runtime.camera_capture.is_none() {
            return;
        }
        let any_live = self.camera.live || self.calibration.live || self.fiducials.live;
        if should_release_capture(
            any_live,
            self.runtime.camera_last_used,
            Instant::now(),
            CAMERA_IDLE_RELEASE,
        ) {
            self.runtime.camera_capture = None; // stops + joins the thread
            self.runtime.camera_capture_src = None;
            self.runtime.camera_last_used = None;
        }
    }

    /// Store a grabbed frame into the preview texture + cache. When the AR
    /// overlay (UI-2) is on, the registered design layers are blended over it.
    pub(super) fn set_camera_frame(&mut self, ctx: &Context, gray: image::GrayImage) {
        let (w, h) = (gray.width() as usize, gray.height() as usize);
        // Build the display image at full resolution (the AR overlay composites
        // at full res so it stays accurate), then downscale the *view* only.
        let full = if self.ar.overlay {
            self.compose_ar(&gray)
        } else {
            ColorImage {
                size: [w, h],
                pixels: gray.pixels().map(|p| Color32::from_gray(p[0])).collect(),
            }
        };
        let (view, scale) = downscale_view(full, CAM_VIEW_MAX);
        self.camera.view_scale = scale;
        self.camera.note = if scale < 1.0 {
            format!("{w}×{h} (view {}×{})", view.size[0], view.size[1])
        } else {
            format!("{w}×{h}")
        };
        self.camera.tex = Some(ctx.load_texture("camera", view, TextureOptions::LINEAR));
        // Keep the full-resolution frame for calibration, detection, snapshot.
        self.camera.last = Some(gray);
    }

    /// Load the Job-tab Gerbers into the AR layer caches (board / copper /
    /// ablate), so the overlay can be re-blended every frame without re-parsing.
    pub(super) fn load_ar_design(&mut self) {
        match self.active_job() {
            Ok((board, copper, ablate)) => {
                let side = match self.job.side {
                    Side::Front => "front",
                    Side::Back => "back (mirrored)",
                };
                self.ar.note = format!(
                    "{side} design: {} board, {} copper, {} ablate region(s)",
                    board.len(),
                    copper.len(),
                    ablate.len()
                );
                self.ar.board = board;
                self.ar.copper = copper;
                self.ar.ablate = ablate;
            }
            Err(e) => {
                self.ar.board.clear();
                self.ar.copper.clear();
                self.ar.ablate.clear();
                self.ar.note = format!("design: {e}");
            }
        }
    }

    /// Blend the enabled design layers over `gray`, mapping design-mm → pixels
    /// through the fiducial homography (registered AR) when one has been
    /// fitted, else a uniform `fid_px_per_mm` scale (a rough, unregistered
    /// overlay). The design is placed with an identity placement, so its Gerber
    /// coordinates go straight through the map — the same frame contract as
    /// `register --frame`.
    pub(super) fn compose_ar(&self, gray: &image::GrayImage) -> ColorImage {
        let (w, h) = (gray.width() as usize, gray.height() as usize);
        let mut img = ColorImage {
            size: [w, h],
            pixels: gray.pixels().map(|p| Color32::from_gray(p[0])).collect(),
        };
        let ident = crate::place::Placement {
            tx_mm: 0.0,
            ty_mm: 0.0,
            rot_deg: 0.0,
            scale: 1.0,
            pivot_mm: (0.0, 0.0),
        };
        let hgt = self.fiducials.homography.as_ref();
        let mut layer = |shapes: &[pcb_core::Poly], on: bool, color: [u8; 3], alpha: f64| {
            if on && !shapes.is_empty() {
                crate::place::composite_over(
                    &mut img,
                    shapes,
                    &ident,
                    self.fiducials.px_per_mm,
                    hgt,
                    color,
                    alpha,
                );
            }
        };
        layer(&self.ar.board, self.ar.show_board, [0x30, 0x60, 0xa0], 0.30);
        layer(
            &self.ar.copper,
            self.ar.show_copper,
            [0xd0, 0xa0, 0x30],
            0.45,
        );
        layer(
            &self.ar.ablate,
            self.ar.show_ablate,
            [0xf0, 0x50, 0x30],
            0.45,
        );
        img
    }

    /// Grab one frame synchronously (the "grab once" button), off the shared
    /// capture. For Live, the same thread is polled non-blocking instead.
    pub fn grab_camera(&mut self, ctx: &Context) {
        match self.grab_shared() {
            Ok(gray) => {
                let gray = self.camera.orientation.apply(gray);
                self.set_camera_frame(ctx, gray);
            }
            Err(e) => self.camera.note = e,
        }
    }

    /// Pull the newest frame from the shared capture into the camera view
    /// (non-blocking). Live-off just stops asking: the capture belongs to the
    /// console, not this tab, and only the idle rule in [`ConsoleApp::ui`] may
    /// drop it — otherwise turning this tab's Live off would kill a Calibrate
    /// or Fiducial feed running at the same time.
    ///
    /// Unlike the other two pumps this one runs from `camera_view`, i.e. only
    /// while the Camera tab is visible. With a shared capture that is now
    /// harmless: another tab's Live keeps the device open, and the idle rule
    /// releases it if none does.
    pub(super) fn pump_camera(&mut self, ctx: &Context) {
        if !self.camera.live {
            return;
        }
        if let Some(res) = self.capture_latest() {
            match res {
                Ok(gray) => {
                    let gray = self.camera.orientation.apply(gray);
                    self.set_camera_frame(ctx, gray);
                }
                Err(e) => self.camera.note = e,
            }
        }
        ctx.request_repaint(); // keep the loop alive
    }

    /// Save the last grabbed frame to a PNG and point the Fiducial-check tab
    /// at it — the bridge from live view into detection / placement.
    pub(super) fn snapshot_to_tabs(&mut self) {
        let Some(frame) = &self.camera.last else {
            self.camera.note = "grab a frame first".into();
            return;
        };
        let path = std::env::temp_dir().join("pcbforge-snapshot.png");
        match frame.save(&path) {
            Ok(()) => {
                let p = path.to_string_lossy().into_owned();
                self.fiducials.frame = p.clone();
                self.placement.frame = p;
                self.camera.note = format!("snapshot → Fiducial-check tab ({})", path.display());
            }
            Err(e) => self.camera.note = format!("save: {e}"),
        }
    }

    pub(super) fn camera_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.camera.use_device, false, "File");
            ui.selectable_value(&mut self.camera.use_device, true, "Device");
            if self.camera.use_device && ui.button("↻ devices").clicked() {
                self.camera.devices = crate::camera::list_devices();
            }
            ui.separator();
            ui.label("orient");
            egui::ComboBox::from_id_salt("cam-orient")
                .selected_text(self.camera.orientation.label())
                .show_ui(ui, |ui| {
                    for o in Orientation::ALL {
                        ui.selectable_value(&mut self.camera.orientation, o, o.label());
                    }
                })
                .response
                .on_hover_text(
                    "Correct how the camera is mounted (e.g. Rotate 180° if it's \
                     installed upside down). Applied to every camera frame before \
                     detection and registration.",
                );
        });
        if self.camera.use_device {
            if self.camera.devices.is_empty() {
                ui.weak(
                    "No devices (build with --features native,camera for a webcam, or use File).",
                );
                ui.add(
                    egui::DragValue::new(&mut self.camera.device)
                        .range(0..=15)
                        .prefix("index "),
                );
            } else {
                egui::ComboBox::from_label("device")
                    .selected_text(
                        self.camera
                            .devices
                            .iter()
                            .find(|(i, _)| *i == self.camera.device)
                            .map(|(_, n)| n.clone())
                            .unwrap_or_else(|| format!("index {}", self.camera.device)),
                    )
                    .show_ui(ui, |ui| {
                        for (i, name) in &self.camera.devices {
                            ui.selectable_value(
                                &mut self.camera.device,
                                *i,
                                format!("{i}: {name}"),
                            );
                        }
                    });
            }
        } else {
            ui.horizontal(|ui| {
                let lbl = ui.label("frame file");
                ui.add(egui::TextEdit::singleline(&mut self.camera.file).desired_width(240.0))
                    .labelled_by(lbl.id);
            });
            ui.weak("Any capture app that writes a frame to disk drives the live preview.");
        }

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.camera.live, "● Live");
            if ui.button("grab once").clicked() {
                let ctx = ui.ctx().clone();
                self.grab_camera(&ctx);
            }
            if ui.button("📸 Snapshot → Fiducial/Place").clicked() {
                self.snapshot_to_tabs();
            }
            ui.label(egui::RichText::new(&self.camera.note).weak());
        });
        ui.separator();

        // AR overlay (UI-2): the registered design projected over the feed.
        let mut ar_changed = false;
        ui.horizontal_wrapped(|ui| {
            ar_changed |= ui
                .checkbox(&mut self.ar.overlay, "🔲 AR overlay")
                .on_hover_text(
                    "Project the registered design over the camera frame using \
                     the fiducial homography (detect fiducials first).",
                )
                .changed();
            if ui.button("⤵ Load design").clicked() {
                self.load_ar_design();
                ar_changed = true;
            }
            if self.ar.overlay {
                ar_changed |= ui.checkbox(&mut self.ar.show_board, "board").changed();
                ar_changed |= ui.checkbox(&mut self.ar.show_copper, "copper").changed();
                ar_changed |= ui.checkbox(&mut self.ar.show_ablate, "ablate").changed();
            }
        });
        if self.ar.overlay {
            let reg = if self.fiducials.homography.is_some() {
                "registered (perspective)"
            } else {
                "unregistered — detect ≥4 fiducials to register"
            };
            ui.label(egui::RichText::new(format!("{}  ·  {reg}", self.ar.note)).weak());
        }
        // Re-blend a still frame when a toggle changes (live frames re-blend as
        // they arrive).
        if ar_changed
            && !self.camera.live
            && let Some(gray) = self.camera.last.take()
        {
            let ctx = ui.ctx().clone();
            self.set_camera_frame(&ctx, gray);
        }
        ui.separator();

        // Bed overlay: nonlinear once ①+③ are accepted, otherwise the
        // approximate ② homography remains available as a fallback.
        if self.calibration.anchor.is_some() || self.calibration.field_accepted {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.camera.show_bed, "⧉ Work area + 50 mm scale")
                    .on_hover_text(
                        "Project the laser's work area and a 50 mm ruler onto the bed. \
                         Uses nonlinear camera + laser-field compensation after steps 1+3; \
                         otherwise uses the approximate step-2 homography anchor.",
                    );
                if self.camera.show_bed {
                    let size_label = ui.label("field mm");
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
                    let cx_label = ui.label("center x");
                    ui.add_enabled(
                        !self.camera.field_center_auto,
                        egui::DragValue::new(&mut self.camera.field_cx_mm).speed(0.5),
                    )
                    .labelled_by(cx_label.id);
                    let cy_label = ui.label("y");
                    ui.add_enabled(
                        !self.camera.field_center_auto,
                        egui::DragValue::new(&mut self.camera.field_cy_mm).speed(0.5),
                    )
                    .labelled_by(cy_label.id);
                }
            });
            if self.camera.show_bed {
                let (text, ok) = match self.camera.last.as_ref().map(|f| f.dimensions()) {
                    Some(dimensions) => match self.camera_projection(dimensions) {
                        Ok(Some(CameraProjection::CommandedField { .. })) => (
                            "● commanded→physical→camera projection · field warp active".to_string(),
                            true,
                        ),
                        Ok(Some(CameraProjection::PhysicalLens { .. })) => {
                            ("● physical camera projection".to_string(), true)
                        }
                        Ok(Some(CameraProjection::Homography { .. })) => (
                            "◐ approximate homography anchor — run step 1 (Camera lens) + step 3 (Laser field) for distortion compensation".to_string(),
                            false,
                        ),
                        Ok(_) => ("○ no camera-to-machine projection".to_string(), false),
                        Err(e) => (format!("⚠ nonlinear projection unavailable: {e}"), false),
                    },
                    None => ("○ grab a frame to validate the active projection".to_string(), false),
                };
                ui.colored_label(status_color(ok), text);
            }
            ui.separator();
        }

        // Live frames come from the background capture thread (non-blocking).
        let ctx = ui.ctx().clone();
        self.pump_camera(&ctx);

        if let Some(tex) = self.camera.tex.clone() {
            let (xf, _) = self.show_image(ui, "camera", &tex);
            if self.camera.show_bed
                && let Some(dimensions) = self.camera.last.as_ref().map(|f| f.dimensions())
            {
                match self.camera_projection(dimensions) {
                    Ok(Some(projection)) => {
                        let unconfirmed = matches!(projection, CameraProjection::Homography { .. })
                            && self
                                .calibration
                                .anchor
                                .as_ref()
                                .is_some_and(|c| c.found == 0);
                        self.draw_bed_overlay(ui, xf, &projection, unconfirmed);
                    }
                    Err(e) => self.draw_projection_warning(ui, xf, &e),
                    Ok(None) => {}
                }
            }
        } else {
            ui.weak("(no frame yet)");
        }
    }

    /// Draw the laser work area and a 50 mm scale onto the camera feed, both
    /// projected through the active commanded-mm → physical-mm →
    /// camera-px mapping and then the pan/zoom transform, so the overlay shows
    /// where machine coordinates really land and stays glued to the bed.
    pub(super) fn draw_bed_overlay(
        &self,
        ui: &egui::Ui,
        xf: crate::imgview::ImageXform,
        projection: &CameraProjection,
        unconfirmed: bool,
    ) {
        let painter = ui.painter_at(xf.panel);
        // The anchor is in full-resolution camera px, but the view texture is
        // downscaled — scale full-res px → view px before the pan/zoom xform.
        let vs = self.camera.view_scale;
        let proj = |mx: f64, my: f64| {
            projection
                .to_px((mx, my))
                .map(|p| xf.to_screen(p.0 * vs, p.1 * vs))
        };
        // A laser-field map bends straight commanded edges in physical space.
        // Project intermediate points too; joining only the four transformed
        // corners would flatten the correction back into straight chords.
        let projected_segment = |start: (f64, f64), end: (f64, f64)| {
            let length = ((end.0 - start.0).powi(2) + (end.1 - start.1).powi(2)).sqrt();
            let steps = (length / 1.0).ceil().max(1.0) as usize;
            (0..=steps)
                .map(|index| {
                    let t = index as f64 / steps as f64;
                    proj(
                        start.0 + (end.0 - start.0) * t,
                        start.1 + (end.1 - start.1) * t,
                    )
                })
                .collect::<Option<Vec<_>>>()
        };
        let yellow = Color32::from_rgb(0xf0, 0xd0, 0x40);
        let green = Color32::from_rgb(0x30, 0xd0, 0x80);
        let white = Color32::WHITE;

        // Work-area square, centred on (field_cx, field_cy).
        let f = self.camera.field_mm as f64;
        let (cx, cy) = (
            self.camera.field_cx_mm as f64,
            self.camera.field_cy_mm as f64,
        );
        let h = f / 2.0;
        let sq = [
            (cx - h, cy - h),
            (cx + h, cy - h),
            (cx + h, cy + h),
            (cx - h, cy + h),
        ];
        let Some(work_edges): Option<Vec<Vec<egui::Pos2>>> = (0..4)
            .map(|index| projected_segment(sq[index], sq[(index + 1) % 4]))
            .collect()
        else {
            self.draw_projection_warning(ui, xf, "projection returned a non-finite work area");
            return;
        };
        let base = (cx - h, cy - h);
        let Some(scale_x) = projected_segment(base, (base.0 + 50.0, base.1)) else {
            self.draw_projection_warning(ui, xf, "projection returned a non-finite X scale");
            return;
        };
        let Some(scale_y) = projected_segment(base, (base.0, base.1 + 50.0)) else {
            self.draw_projection_warning(ui, xf, "projection returned a non-finite Y scale");
            return;
        };
        let (Some(axis_x), Some(axis_y)) = (
            projected_segment((0.0, 0.0), (20.0, 0.0)),
            projected_segment((0.0, 0.0), (0.0, 20.0)),
        ) else {
            self.draw_projection_warning(ui, xf, "projection returned non-finite machine axes");
            return;
        };
        let b = work_edges[0][0];
        let o = axis_x[0];
        for edge in &work_edges {
            painter.add(egui::Shape::line(
                edge.clone(),
                egui::Stroke::new(2.0, yellow),
            ));
        }
        let topmid = work_edges[2][work_edges[2].len() / 2];
        painter.text(
            topmid,
            egui::Align2::CENTER_BOTTOM,
            format!("work area {f:.0} mm"),
            egui::FontId::proportional(13.0),
            yellow,
        );

        // 50 mm scale as an L at the work-area's lower-left corner: one arm along
        // machine +X, one along +Y, each capped and labelled — perspective-
        // correct at that spot because it goes through the same homography.
        for (path, label, up) in [(scale_x, "50 mm", false), (scale_y, "50 mm", true)] {
            let end = *path.last().expect("sampled scale has endpoints");
            painter.add(egui::Shape::line(path, egui::Stroke::new(2.5, white)));
            // End caps, perpendicular to the arm in screen space.
            let d = end - b;
            let len = d.length().max(1.0);
            let perp = egui::vec2(-d.y, d.x) / len * 5.0;
            painter.line_segment([b - perp, b + perp], (2.0, white));
            painter.line_segment([end - perp, end + perp], (2.0, white));
            let mid = egui::pos2((b.x + end.x) * 0.5, (b.y + end.y) * 0.5);
            let align = if up {
                egui::Align2::RIGHT_CENTER
            } else {
                egui::Align2::CENTER_TOP
            };
            painter.text(mid, align, label, egui::FontId::proportional(12.0), white);
        }

        // Machine origin + axes (clipped to the panel if off to the side).
        painter.circle_filled(o, 3.0, green);
        for (path, label, align) in [
            (axis_x, "+X", egui::Align2::LEFT_CENTER),
            (axis_y, "+Y", egui::Align2::CENTER_BOTTOM),
        ] {
            let end = *path.last().expect("sampled axis has endpoints");
            painter.add(egui::Shape::line(path, egui::Stroke::new(2.0, green)));
            painter.circle_filled(end, 2.0, green);
            painter.text(
                egui::pos2(end.x + 2.0, end.y),
                align,
                label,
                egui::FontId::proportional(12.0),
                green,
            );
        }
        painter.text(
            egui::pos2(o.x + 5.0, o.y + 4.0),
            egui::Align2::LEFT_TOP,
            "0,0",
            egui::FontId::proportional(11.0),
            green,
        );

        if unconfirmed {
            painter.text(
                egui::pos2(xf.panel.min.x + 6.0, xf.panel.max.y - 6.0),
                egui::Align2::LEFT_BOTTOM,
                "⚠ calibration from last session — re-anchor to confirm the scale",
                egui::FontId::proportional(12.0),
                yellow,
            );
        }
    }

    pub(super) fn draw_projection_warning(
        &self,
        ui: &egui::Ui,
        xf: crate::imgview::ImageXform,
        reason: &str,
    ) {
        let painter = ui.painter_at(xf.panel);
        let text = format!("⚠ corrected projection unavailable: {reason}");
        let pos = egui::pos2(xf.panel.min.x + 8.0, xf.panel.min.y + 8.0);
        painter.rect_filled(
            egui::Rect::from_min_size(pos, egui::vec2(8.0 * text.len() as f32, 22.0)),
            3.0_f32,
            Color32::from_black_alpha(190),
        );
        painter.text(
            egui::pos2(pos.x + 4.0, pos.y + 3.0),
            egui::Align2::LEFT_TOP,
            text,
            egui::FontId::proportional(12.0),
            Color32::from_rgb(0xf0, 0x70, 0x50),
        );
    }
}
