use super::*;

/// Max physical edge segment before field pre-warping drill geometry, mm —
/// matches the CLI's `--field-seg-mm` default the etch path inherits.
const DRILL_FIELD_SEG_MM: f64 = 0.25;

impl ConsoleApp {
    /// Move the placement so its pivot's **pixel** position shifts by
    /// `(dpx, dpy)` frame pixels. Dragging felt wrong under perspective because
    /// the old code added a uniform mm delta — a uniform mm step is *not* a
    /// uniform pixel step on a tilted plane, so the overlay slid along the plane
    /// instead of following the cursor. Here we map the pivot to pixels through
    /// the same homography the composite uses, shift in pixels, and invert back
    /// to bed-mm — so the geometry tracks where the mouse moves over the image.
    pub(super) fn drag_place_px(&mut self, dpx: f64, dpy: f64) -> Result<(), String> {
        let dimensions = self
            .placement
            .frame_img
            .as_ref()
            .map(image::GenericImageView::dimensions)
            .ok_or("load a placement frame first")?;
        let projection = self.place_projection(dimensions.0, dimensions.1)?;
        let (px, py) = projection
            .to_px((self.placement.tx_mm, self.placement.ty_mm))
            .ok_or("active camera projection returned a non-finite placement")?;
        let (nx, ny) = (px + dpx, py + dpy);
        let (tx, ty) = projection
            .from_px((nx, ny))
            .ok_or("active camera projection returned a non-finite drag result")?;
        self.placement.tx_mm = tx;
        self.placement.ty_mm = ty;
        Ok(())
    }

    /// The copper/outline Gerber paths for the active side.
    pub(super) fn active_gerbers(&self) -> (&str, &str) {
        match self.job.side {
            Side::Front => (&self.job.emit_copper, &self.job.emit_outline),
            Side::Back => (&self.job.back_copper, &self.job.back_outline),
        }
    }

    /// The active side's (board, copper, ablate) job, mirrored in X when it's
    /// the back side (KiCad B.Cu is top-view, so a left-right flip mirrors it).
    pub(super) fn active_job(&self) -> Result<JobShapes, String> {
        let (copper, outline) = self.active_gerbers();
        let (board, cu, ablate) = job_shapes(copper, outline, self.job.offset_mm)?;
        Ok(match self.job.side {
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
        let params = self.back_field_params()?;
        let pts = fiducial::parse_layout(&self.fiducials.layout).ok()?;
        let n = pts.len() as f64;
        let cx = pts.iter().map(|p| p.0).sum::<f64>() / n;
        Some((cam::flip::MirrorAxis::VerticalX { x_mm: cx }, params))
    }

    /// The f-theta field optics for the back side's exit-parallax model — the
    /// scan center is the fiducial layout centroid by default
    /// (`scan_center_auto`) or the operator-entered field center. `None` on the
    /// front (no parallax to model) or when the layout doesn't parse. Shared by
    /// the display mirror ([`back_field`]) and the fiducial pose fit.
    pub(super) fn back_field_params(&self) -> Option<cam::flip::FieldParams> {
        if self.job.side != Side::Back {
            return None;
        }
        let pts = fiducial::parse_layout(&self.fiducials.layout).ok()?;
        let n = pts.len() as f64;
        let cx = pts.iter().map(|p| p.0).sum::<f64>() / n;
        let cy = pts.iter().map(|p| p.1).sum::<f64>() / n;
        let scan_center = if self.job.scan_center_auto {
            (cx, cy)
        } else {
            self.job.scan_center_mm
        };
        Some(cam::flip::FieldParams {
            scan_center_mm: scan_center,
            thickness_mm: self.job.board_thickness_mm,
            focal_mm: self.job.focal_mm,
        })
    }

    /// The expected fiducial positions to display/detect, in bed mm: the raw
    /// design layout on the front, or the mirrored + beam-offset positions on
    /// the back (where the drilled through-holes actually appear when flipped).
    pub(super) fn expected_points(&self) -> Vec<(f64, f64)> {
        let design = fiducial::parse_layout(&self.fiducials.layout).unwrap_or_default();
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
    pub(super) fn set_side(&mut self, side: Side) {
        if self.job.side == side {
            return;
        }
        self.job.side = side;
        self.fiducials.search.clear();
        self.fiducials.found.clear();
        self.fiducials.rows.clear();
        self.fiducials.marking = None;
        self.fiducials.last_placed = false;
        self.fiducials.homography = None;
        self.fiducials.pose = None;
        self.ar.board.clear();
        self.ar.copper.clear();
        self.ar.ablate.clear();
        self.placement.job.clear();
        // The fitted pose was for the old face's detection — drop it so the
        // next Load recenters normally until a fresh Check re-fits.
        self.placement.auto_pose = false;
        // Also drop the cached frame/textures, or both tabs keep painting the
        // other side's image until a new frame is loaded (LR-41).
        self.placement.frame_img = None;
        self.placement.tex = None;
        self.job.preview_tex = None;
    }

    /// Current manual placement.
    fn placement(&self) -> crate::place::Placement {
        crate::place::Placement {
            tx_mm: self.placement.tx_mm,
            ty_mm: self.placement.ty_mm,
            rot_deg: self.placement.rot_deg,
            pivot_mm: self.placement.pivot,
        }
    }

    /// Load the bed frame + job geometry into the place cache and center the
    /// job on the frame. Uses the Job-tab Gerber paths for the geometry.
    pub fn load_place(&mut self, ctx: &Context) {
        // ALWAYS grab a fresh frame from the camera source first, so Load
        // reflects the board as it sits now (the bed-frame path is persisted —
        // preferring it would silently keep re-loading a stale file). The file
        // is the fallback when no camera source is reachable.
        let img = match crate::camera::grab(&self.cam_source()) {
            Ok(img) => self.camera.orientation.apply(img),
            Err(cam_err) => {
                let path = crate::clean_path(&self.placement.frame);
                if path.trim().is_empty() {
                    self.placement.note = format!("camera: {cam_err}");
                    return;
                }
                match image::open(path) {
                    Ok(i) => {
                        self.placement.note =
                            format!("camera unavailable ({cam_err}) — loaded the bed-frame file");
                        i.to_luma8()
                    }
                    Err(e) => {
                        self.placement.note = format!("camera: {cam_err}; frame file: {e}");
                        return;
                    }
                }
            }
        };
        let (_, _, ablate) = match self.active_job() {
            Ok(t) => t,
            Err(e) => {
                self.placement.note = format!("job: {e}");
                return;
            }
        };
        self.placement.pivot = crate::place::bbox_center_mm(&ablate);
        // Convert the frame to RGBA once; recompose clones this per drag step.
        let base = ColorImage {
            size: [img.width() as usize, img.height() as usize],
            pixels: img.pixels().map(|p| Color32::from_gray(p[0])).collect(),
        };
        // Start centered on the frame — in the SAME frame the overlay draws in.
        // Under an active homography, uniform px/mm would land the job far
        // off-centre until dragged (LR-42). Skip when the pose was fitted from
        // fiducials: Load must not recenter/zero over an auto-placement.
        if !self.placement.auto_pose {
            let (cx, cy) = match self.initial_center_mm(img.width() as f64, img.height() as f64) {
                Ok(center) => center,
                Err(e) => {
                    // No projection at all — still show the bare frame so the
                    // operator sees what loaded; the note says what's missing.
                    self.placement.note =
                        format!("frame loaded, but placement needs calibration: {e}");
                    self.placement.job.clear();
                    self.placement.frame_img = Some(img);
                    self.set_place_tex(ctx, base.clone());
                    self.placement.base_rgba = Some(base);
                    return;
                }
            };
            self.placement.tx_mm = cx;
            self.placement.ty_mm = cy;
            self.placement.rot_deg = 0.0;
        }
        self.placement.job = ablate;
        self.placement.frame_img = Some(img);
        self.placement.base_rgba = Some(base);
        self.recompose(ctx);
    }

    /// Re-blend the placed job over the cached frame into the display texture.
    pub(super) fn recompose(&mut self, ctx: &Context) {
        let Some(frame) = &self.placement.frame_img else {
            return;
        };
        if self.placement.job.is_empty() {
            return;
        }
        let projection = match self.place_projection(frame.width(), frame.height()) {
            Ok(p) => p,
            Err(e) => {
                self.placement.note = format!("placement projection unavailable: {e}");
                self.placement.tex = None;
                return;
            }
        };
        // Blend over a clone of the cached RGBA base (built once at load);
        // rebuild the cache here only for state set up outside load_place.
        let mut img = match &self.placement.base_rgba {
            Some(base) if base.size == [frame.width() as usize, frame.height() as usize] => {
                base.clone()
            }
            _ => {
                let base = ColorImage {
                    size: [frame.width() as usize, frame.height() as usize],
                    pixels: frame.pixels().map(|p| Color32::from_gray(p[0])).collect(),
                };
                self.placement.base_rgba = Some(base.clone());
                base
            }
        };
        if let Err(e) = crate::place::composite_over_projected(
            &mut img,
            &self.placement.job,
            &self.placement(),
            &|x, y| projection.to_px((x, y)),
            [0xf0, 0x50, 0x30],
            0.55,
        ) {
            self.placement.note = format!("placement projection unavailable: {e}");
            self.placement.tex = None;
            return;
        }
        let frame_note = projection.label();
        self.placement.note = format!(
            "placed at ({:.1}, {:.1}) mm, {:.0}° · {frame_note}",
            self.placement.tx_mm, self.placement.ty_mm, self.placement.rot_deg
        );
        self.set_place_tex(ctx, img);
    }

    /// Upload the composed image, reusing the existing GPU texture when one is
    /// live (a fresh `load_texture` per drag step allocates a new texture).
    fn set_place_tex(&mut self, ctx: &Context, img: ColorImage) {
        match &mut self.placement.tex {
            Some(tex) => tex.set(img, TextureOptions::NEAREST),
            None => {
                self.placement.tex = Some(ctx.load_texture("place", img, TextureOptions::NEAREST));
            }
        }
    }

    /// Emit the job registered to the current manual placement by encoding it
    /// as fiducial correspondences and shelling `pcbforge register`. When
    /// `run_after` is set, queue a LightBurn "load + run" of the exported file
    /// to fire once the export succeeds (see [`pump_verb`](Self::pump_verb)).
    pub(super) fn emit_at_placement(&mut self, run_after: bool) {
        // Back-side etch is not wired: this path hardcodes the FRONT Gerbers and
        // shells `register`, which has no mirror pass — it would emit the front
        // copper's ablate set, unmirrored, translated by the mirrored job's
        // pivot: wrong pattern, wrong chirality, wrong position, silently.
        // Refuse until register grows `--mirror-x` (IMP-05) (LR-03).
        if self.job.side == Side::Back {
            self.runtime.log.push(LogLine {
                text: "place: back-side \"Etch here\" isn't supported yet — register can't \
                       mirror, so it would burn the FRONT copper unmirrored at the wrong \
                       spot. Switch to the front side to etch."
                    .into(),
                err: true,
            });
            return;
        }
        if self.placement.job.is_empty() {
            self.runtime.log.push(LogLine {
                text: "place: load a frame + job first".into(),
                err: true,
            });
            return;
        }
        if self.job.emit_copper.trim().is_empty() {
            self.runtime.log.push(LogLine {
                text: "place: set a copper Gerber (Job tab) first".into(),
                err: true,
            });
            return;
        }
        let Some(dimensions) = self
            .placement
            .frame_img
            .as_ref()
            .map(image::GenericImageView::dimensions)
        else {
            self.runtime.log.push(LogLine {
                text: "place: load the current camera frame before export".into(),
                err: true,
            });
            return;
        };
        // Field-warp when a valid calibration for THIS frame + the map file
        // exist; otherwise export unwarped with a warning (operator's call).
        let field_path = self.field_map_path();
        let use_field = match self.nonlinear_maps_for_frame(dimensions) {
            Ok(Some(_)) => field_path.exists(),
            Ok(None) => false,
            Err(error) => {
                self.runtime.log.push(LogLine {
                    text: format!(
                        "place: field-warp calibration is stale or invalid ({error}) — \
                         exporting UNWARPED geometry"
                    ),
                    err: true,
                });
                false
            }
        };
        self.placement.field_correct = use_field;
        // Resolve the output to an ABSOLUTE path so the operator knows exactly
        // which file to open. A bare filename (e.g. "placed.lbrn2") lands next
        // to the copper Gerber — beside their inputs — not in the console's
        // launch directory, which is otherwise a mystery on a GUI.
        let copper = crate::clean_path(&self.job.emit_copper);
        let out_path = self.resolve_place_output(&copper);
        let out = out_path.to_string_lossy().into_owned();
        let mut args: Vec<String> = vec![
            "register".into(),
            "--copper".into(),
            copper,
            "--lbrn2".into(),
            out.clone(),
            "--fiducials".into(),
            self.placement().correspondences(),
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
        if !crate::clean_path(&self.job.emit_outline).is_empty() {
            args.push("--outline".into());
            args.push(crate::clean_path(&self.job.emit_outline));
        }
        let field_note = if use_field {
            args.push("--field-map".into());
            args.push(field_path.to_string_lossy().into_owned());
            " · field-warped geometry"
        } else {
            self.runtime.log.push(LogLine {
                text:
                    "place: no accepted step 1 (Camera lens) + step 3 (Laser field) calibration — \
                       exporting UNWARPED geometry"
                        .into(),
                err: true,
            });
            " · UNWARPED geometry (no field calibration)"
        };
        // Make the placement + the exact output path explicit in the log — the
        // register output is its own file (not the Job-tab emit), and this is
        // the position it bakes in.
        self.runtime.log.push(LogLine {
            text: format!(
                "Etch here → {out}\n  job placed at ({:.2}, {:.2}) mm, {:.1}°{field_note} — OPEN THIS FILE (not the Job-tab emit output)",
                self.placement.tx_mm, self.placement.ty_mm, self.placement.rot_deg
            ),
            err: false,
        });
        let started = self.run_verb(&args);
        // Queue the LightBurn run only when the export actually launched — a
        // refused click (a job already running) must not arm the chain against
        // a file this click never wrote. Resolve to an ABSOLUTE path without
        // canonicalizing: the file may not exist yet, and \\?\ prefixes upset
        // LightBurn's FORCELOAD.
        if run_after && started {
            match std::path::absolute(&out_path) {
                Ok(abs) => {
                    self.runtime.pending_lightburn = Some(abs);
                    self.runtime.log.push(LogLine {
                        text: "queued: load + run in LightBurn once the export finishes".into(),
                        err: false,
                    });
                }
                Err(e) => self.runtime.log.push(LogLine {
                    text: format!(
                        "place: couldn't resolve an absolute path for the LightBurn run ({e}) — \
                         the export will still be written"
                    ),
                    err: true,
                }),
            }
        }
    }

    /// Absolute output path for "Etch here". An absolute `place_lbrn2` is used
    /// as-is; a bare filename lands next to the copper Gerber (beside the
    /// operator's inputs) so the written file is easy to find.
    pub(super) fn resolve_place_output(&self, copper: &str) -> PathBuf {
        let raw = PathBuf::from(crate::clean_path(&self.placement.lbrn2));
        if raw.is_absolute() {
            return raw;
        }
        match PathBuf::from(copper).parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.join(&raw),
            _ => raw,
        }
    }

    /// Absolute output path for "Emit drill holes": an absolute `drill_lbrn2`
    /// as-is, a bare filename next to the (first) drill file — beside the
    /// operator's inputs, like [`Self::resolve_place_output`].
    fn resolve_drill_output(&self, first_drill: &str) -> PathBuf {
        let raw = PathBuf::from(crate::clean_path(&self.placement.drill_lbrn2));
        if raw.is_absolute() {
            return raw;
        }
        match PathBuf::from(first_drill).parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.join(&raw),
            _ => raw,
        }
    }

    /// Fill the drill-file field from the Actions-panel KiCad project and
    /// shell `pcbforge drills` in the **background** — the drill counterpart
    /// of the Job tab's "⚙ Gerbers from KiCad". The output paths are
    /// deterministic (`<board dir>/pcbforge-gerbers/{pth,npth}.drl` — the
    /// same directory the Gerbers land in), so the field is filled
    /// immediately; the files appear when the background export finishes,
    /// whose progress/errors stream to the Log.
    pub(super) fn drills_from_kicad(&mut self) {
        let proj = crate::clean_path(&self.job.kicad_project);
        if proj.trim().is_empty() {
            self.runtime.log.push(LogLine {
                text: "drills: set a KiCad project path (Actions panel) first".into(),
                err: true,
            });
            return;
        }
        // Resolve the board just to place the output dir; the CLI re-resolves it.
        let board = match ingest::kicad_cli::resolve_board(std::path::Path::new(&proj)) {
            Ok(b) => b,
            Err(e) => {
                self.runtime.log.push(LogLine {
                    text: format!("drills: {e}"),
                    err: true,
                });
                return;
            }
        };
        let out_dir = board
            .parent()
            .map(|p| p.join("pcbforge-gerbers"))
            .unwrap_or_else(|| PathBuf::from("pcbforge-gerbers"));
        self.placement.drills = format!(
            "{};{}",
            out_dir.join("pth.drl").display(),
            out_dir.join("npth.drl").display()
        );
        self.run_verb(&[
            "drills".into(),
            "--project".into(),
            proj,
            "--out".into(),
            out_dir.display().to_string(),
        ]);
        self.placement.note = "exporting drill files (PTH + NPTH) from KiCad… (see Log)".into();
    }

    /// Emit ONLY the drill-hole geometry (round holes + G85 slots) at the
    /// current manual placement as a `.lbrn2` — and never queue a LightBurn
    /// run: the file is written for the operator to open and start themselves
    /// ([`Self::emit_at_placement`] owns the etch/run chain; this path never
    /// touches `pending_lightburn`).
    ///
    /// Runs in-process (Excellon → `cam::drill::drill_polys` → the placement
    /// affine → `cam::lbrn2`) rather than shelling `drill-emit`: the CLI verb
    /// only takes a translation origin, so a rotated placement is not
    /// expressible through it.
    pub(super) fn emit_drill_at_placement(&mut self) {
        // Same back-side refusal as "Etch here": this path applies the FRONT
        // placement affine with no mirror pass, so it would burn the hole
        // pattern with the wrong chirality at the wrong spot, silently.
        if self.job.side == Side::Back {
            self.runtime.log.push(LogLine {
                text: "place: back-side drill emit isn't supported yet — the placement \
                       can't mirror, so it would burn the hole pattern unmirrored at \
                       the wrong spot. Switch to the front side."
                    .into(),
                err: true,
            });
            return;
        }
        // The pose (pivot, tx/ty/rot) only means something once a job is
        // loaded and placed — same precondition as the etch buttons.
        if self.placement.job.is_empty() {
            self.runtime.log.push(LogLine {
                text: "place: load a frame + job first".into(),
                err: true,
            });
            return;
        }
        let drill_paths: Vec<String> = self
            .placement
            .drills
            .split(';')
            .map(crate::clean_path)
            .filter(|p| !p.trim().is_empty())
            .collect();
        if drill_paths.is_empty() {
            self.runtime.log.push(LogLine {
                text: "place: set a drill file (.drl) first — KiCad exports PTH and \
                       NPTH holes as two files; list both separated by ;"
                    .into(),
                err: true,
            });
            return;
        }
        let Some(dimensions) = self
            .placement
            .frame_img
            .as_ref()
            .map(image::GenericImageView::dimensions)
        else {
            self.runtime.log.push(LogLine {
                text: "place: load the current camera frame before export".into(),
                err: true,
            });
            return;
        };
        // Every hole from every file, slots kept lossless.
        let mut entries: Vec<cam::process::DrillEntry> = Vec::new();
        let (mut holes, mut slots) = (0usize, 0usize);
        for path in &drill_paths {
            let ops = match ingest::excellon::load_excellon_full(std::path::Path::new(path)) {
                Ok(ops) => ops,
                Err(e) => {
                    self.placement.note = format!("drill file {path}: {e}");
                    self.runtime.log.push(LogLine {
                        text: format!("place: drill file {path}: {e}"),
                        err: true,
                    });
                    return;
                }
            };
            for op in &ops {
                entries.push(match *op {
                    ingest::excellon::DrillOp::Hole {
                        center,
                        diameter_nm,
                    } => {
                        holes += 1;
                        cam::process::DrillEntry {
                            x_nm: center.x,
                            y_nm: center.y,
                            diameter_nm,
                            slot_end: None,
                        }
                    }
                    ingest::excellon::DrillOp::Slot { a, b, diameter_nm } => {
                        slots += 1;
                        cam::process::DrillEntry {
                            x_nm: a.x,
                            y_nm: a.y,
                            diameter_nm,
                            slot_end: Some((b.x, b.y)),
                        }
                    }
                });
            }
        }
        let hole_polys = cam::drill::drill_polys(&entries);
        if hole_polys.is_empty() {
            self.runtime.log.push(LogLine {
                text: "place: the drill file(s) contain no holes".into(),
                err: true,
            });
            return;
        }
        // Field-warp under exactly the conditions "Etch here" would (a valid
        // calibration for THIS frame + the map file), so the two exports land
        // on the same physical geometry.
        let field_path = self.field_map_path();
        let use_field = match self.nonlinear_maps_for_frame(dimensions) {
            Ok(Some(_)) => field_path.exists(),
            Ok(None) => false,
            Err(error) => {
                self.runtime.log.push(LogLine {
                    text: format!(
                        "place: field-warp calibration is stale or invalid ({error}) — \
                         exporting UNWARPED drill geometry"
                    ),
                    err: true,
                });
                false
            }
        };
        self.placement.field_correct = use_field;
        // Drill files share the Gerber frame, so the copper job's placement
        // affine positions the holes on the physical board directly.
        let affine = cam::register::Affine2 {
            m: self.placement().affine(),
        };
        let placed = if use_field {
            let field = match std::fs::read_to_string(&field_path)
                .map_err(|e| e.to_string())
                .and_then(|s| vision::FieldMap::parse(&s).map_err(|e| e.to_string()))
            {
                Ok(f) => f,
                Err(e) => {
                    // Refuse rather than silently falling back: the operator
                    // believes exports are warped while this file is broken.
                    self.runtime.log.push(LogLine {
                        text: format!(
                            "place: field map {} is unreadable ({e}) — drill emit \
                             refused; fix or remove the file",
                            field_path.display()
                        ),
                        err: true,
                    });
                    return;
                }
            };
            match cam::register::transform_shapes_field(
                &hole_polys,
                &affine,
                DRILL_FIELD_SEG_MM,
                |x, y| field.precompensate(x, y),
            ) {
                Ok(warped) => warped,
                Err(e) => {
                    // Refuse rather than emit: a saturated vertex would send the
                    // beam to the machine origin on its way to the hole.
                    self.runtime.log.push(LogLine {
                        text: format!(
                            "place: field warp refused ({e}) — drill emit refused; \
                             re-run the laser-field calibration"
                        ),
                        err: true,
                    });
                    return;
                }
            }
        } else {
            self.runtime.log.push(LogLine {
    text: format!(
        "place: drill emit without field-warp (need accepted step 1 (Camera lens) + step 3 (Laser field) calibration and a readable {}) — exporting UNWARPED drill geometry",
        field_path.display()
    ),
    err: true,
});
            cam::register::transform_shapes(&hole_polys, &affine)
        };
        // Same recipe the etch path bakes in: the Job-tab process params over
        // the register verb's default power.
        let params = pcb_core::AblationParams {
            power_pct: 20.0,
            speed_mm_s: self.job.speed_mm_s,
            frequency_khz: self.job.frequency_khz,
            pulse_ns: self.job.pulse_ns,
            passes: self.job.passes,
        };
        let mut layer =
            cam::lbrn2::EmitLayer::fill("DRILL", params, cam::lbrn2::polys_to_elems(&placed));
        layer.interval_mm = self.job.interval_mm;
        let out_path = self.resolve_drill_output(&drill_paths[0]);
        let out = out_path.to_string_lossy().into_owned();
        if let Err(e) =
            cam::lbrn2::write_lbrn2(&self.placement.lightburn_device, &[layer], &out_path)
        {
            self.placement.note = format!("drill emit: {e}");
            self.runtime.log.push(LogLine {
                text: format!("place: drill emit {out}: {e}"),
                err: true,
            });
            return;
        }
        let field_note = if use_field {
            " · field-warped geometry"
        } else {
            " · UNWARPED geometry (no field calibration)"
        };
        self.runtime.log.push(LogLine {
            text: format!(
                "Drill holes → {out}\n  {holes} hole(s) + {slots} slot(s) placed at \
                 ({:.2}, {:.2}) mm, {:.1}°{field_note} — loading in LightBurn, NOT \
                 starting it (press ▶ there to burn)",
                self.placement.tx_mm, self.placement.ty_mm, self.placement.rot_deg
            ),
            err: false,
        });
        // Load (never start) the written file in LightBurn: a load-only run —
        // START stays with the operator, and `pending_lightburn` (the etch
        // path's export→run chain) is never touched. A run already in flight
        // is left alone; the file is written either way.
        let lb_busy = self
            .runtime
            .lightburn_run
            .as_ref()
            .is_some_and(|r| !r.finished());
        if lb_busy {
            self.placement.note = format!(
                "drill holes → {out} · {holes} hole(s) + {slots} slot(s) — written; \
                 LightBurn is busy, open the file there yourself (no burn)"
            );
            self.runtime.log.push(LogLine {
                text: "place: a LightBurn run is in flight — skipped the drill-file \
                       load; open it in LightBurn yourself"
                    .into(),
                err: true,
            });
            return;
        }
        // Absolute path without canonicalizing: the file exists, but \\?\
        // prefixes upset LightBurn's FORCELOAD (same rule as the etch chain).
        match std::path::absolute(&out_path) {
            Ok(abs) => {
                self.runtime.lightburn_run = Some(spawn_lightburn_load(
                    abs,
                    self.placement.lightburn_device.clone(),
                ));
                self.placement.note = format!(
                    "drill holes → {out} · {holes} hole(s) + {slots} slot(s) — \
                     loading in LightBurn (no burn started)"
                );
            }
            Err(e) => {
                self.placement.note = format!(
                    "drill holes → {out} · {holes} hole(s) + {slots} slot(s) — \
                     written; couldn't load it in LightBurn ({e})"
                );
                self.runtime.log.push(LogLine {
                    text: format!(
                        "place: couldn't resolve an absolute path for the LightBurn \
                         load ({e}) — the file is written; open it manually"
                    ),
                    err: true,
                });
            }
        }
    }

    pub(super) fn place_view(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        egui::Grid::new("place-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                let lbl = ui.label("bed frame");
                ui.add(egui::TextEdit::singleline(&mut self.placement.frame).desired_width(240.0))
                    .labelled_by(lbl.id)
                    .on_hover_text(
                        "Optional fallback image: \"Load frame + job\" always grabs a \
                         fresh frame from the camera source picked in the Camera tab, \
                         and loads this file only when that grab fails (offline work).",
                    );
                ui.end_row();
                ui.label("out .lbrn2");
                ui.add(egui::TextEdit::singleline(&mut self.placement.lbrn2).desired_width(240.0))
                    .on_hover_text(
                        "Where \"Etch here\" writes the registered job — separate \
                         from the Job tab's emit output so they don't overwrite \
                         each other. A bare filename lands next to the copper \
                         Gerber; the log prints the full path it wrote.",
                    );
                ui.end_row();
                let dev = ui.label("LightBurn device");
                ui.add(
                    egui::TextEdit::singleline(&mut self.placement.lightburn_device)
                        .desired_width(240.0),
                )
                .labelled_by(dev.id)
                .on_hover_text(
                    "Device name for \"Etch + run in LightBurn\" — must match a \
                     configured LightBurn device (the LASER: automation command \
                     selects it before loading the file).",
                );
                ui.end_row();
                let drl = ui.label("drill .drl");
                ui.add(egui::TextEdit::singleline(&mut self.placement.drills).desired_width(240.0))
                    .labelled_by(drl.id)
                    .on_hover_text(
                        "Excellon drill file(s) for \"Emit drill holes\" — KiCad \
                         exports PTH and NPTH holes as two files; list both \
                         separated by ; to get every hole. \"⚙ Drills from \
                         KiCad\" fills this from the Actions-panel project.",
                    );
                ui.end_row();
                let drl_out = ui.label("drill out .lbrn2");
                ui.add(
                    egui::TextEdit::singleline(&mut self.placement.drill_lbrn2)
                        .desired_width(240.0),
                )
                .labelled_by(drl_out.id)
                .on_hover_text(
                    "Where \"Emit drill holes\" writes the hole-geometry job — \
                     separate from the etch output so they never overwrite each \
                     other. A bare filename lands next to the drill file; the log \
                     prints the full path it wrote.",
                );
                ui.end_row();
            });
        ui.horizontal(|ui| {
            if ui
                .button("⤵ Load frame + job")
                .on_hover_text(
                    "Grabs a fresh frame from the camera source picked in the Camera \
                     tab and loads the Job-tab Gerbers onto it; the bed-frame file is \
                     used only when the camera grab fails.",
                )
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.load_place(&ctx);
            }
            if ui.button("▶ Etch here (register)").clicked() {
                self.emit_at_placement(false);
            }
            // One-click: export, then load + run it in LightBurn. Disabled while
            // a run is in flight so a second click can't stack a second job.
            if ui
                .add_enabled(
                    !self.lightburn_busy(),
                    egui::Button::new("▶ Etch + run in LightBurn"),
                )
                .on_hover_text(
                    "Runs the register export, then drives LightBurn over its UDP \
                     automation interface to load the file and START the job — \
                     reporting progress in the log. LightBurn must be open with \
                     the device configured.",
                )
                .clicked()
            {
                self.emit_at_placement(true);
            }
            let has_field = self.has_usable_field_cal();
            self.placement.field_correct = has_field;
            ui.colored_label(
                status_color(has_field),
                if has_field {
                    "● field-warped export active"
                } else {
                    "⚠ UNWARPED export (no step 1 Camera lens + step 3 Laser field)"
                },
            )
            .on_hover_text(
                "With an accepted laser-field map, geometry is densified and pre-warped \
                 physical→commanded. Without one, \"Etch here\" exports the placement \
                 unwarped — field accuracy is then the machine's own correction.",
            );
        });
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
        if self.calibration.field.is_some() && !self.calibration.field_accepted {
            ui.colored_label(
                status_color(false),
                "⚠ latest step-3 field fit was rejected; nonlinear placement and correction are disabled",
            );
        }
        ui.horizontal(|ui| {
            ui.label("x mm");
            changed |= ui
                .add(egui::DragValue::new(&mut self.placement.tx_mm).speed(0.1))
                .changed();
            ui.label("y mm");
            changed |= ui
                .add(egui::DragValue::new(&mut self.placement.ty_mm).speed(0.1))
                .changed();
            ui.label("rot°");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut self.placement.rot_deg)
                        .speed(0.5)
                        .range(-180.0..=180.0),
                )
                .changed();
        });
        ui.label(egui::RichText::new(&self.placement.note).weak());
        ui.weak("Uses the Job-tab Gerbers. Drag to place; “Etch here” field-warps every edge when a field calibration is accepted, else exports unwarped.");
        ui.weak(NAV_HINT);
        ui.separator();

        if let Some(tex) = self.placement.tex.clone() {
            let (xf, resp) = self.show_image(ui, "place", &tex);
            // Plain drag repositions the job; Ctrl+drag pans the view instead.
            if !crate::imgview::is_navigating(ui) && resp.dragged() {
                let d = resp.drag_delta();
                // Convert the screen-space drag back to native frame pixels
                // (divide by the display scale). The move is applied in pixel
                // space (see drag_place_px) so the overlay tracks the cursor
                // even when a perspective homography is active.
                let s = xf.scale.max(1e-3) as f64;
                match self.drag_place_px(d.x as f64 / s, d.y as f64 / s) {
                    Ok(()) => changed = true,
                    Err(e) => {
                        self.placement.note = format!("placement projection unavailable: {e}")
                    }
                }
            }
        } else {
            ui.weak("(load a frame + job to place)");
        }

        if changed {
            let ctx = ui.ctx().clone();
            self.recompose(&ctx);
        }
    }
}
