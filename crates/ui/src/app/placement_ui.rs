use super::*;

/// Max physical edge segment before field pre-warping drill geometry, mm —
/// matches the CLI's `--field-seg-mm` default the etch path inherits.
const DRILL_FIELD_SEG_MM: f64 = 0.25;

/// The emitter needs a `maxPower` and `AblationParams::validate` rejects 0, but
/// power is not an operator concept on this machine: pulse energy is set by the
/// frequency and Q-pulse width. Fixed at the value the drill path has always
/// emitted (and the CLI's own default), so the only drill knobs on screen are
/// the ones that change the burn.
const DRILL_POWER_PCT: f64 = 20.0;

/// How far the placement may sit from the fiducial-derived pose before the
/// offset readout stops calling it "on the pose" and flags it.
///
/// 0.5 mm is not arbitrary: it is [`POSE_MAX_RMS_MM`], the residual above which
/// the console already refuses to trust a fiducial fit at all. An offset the
/// registration itself would call a failure is worth reading twice; anything
/// under it is inside the tolerance the lock was accepted with.
///
/// 0.5° is the rotational match: on a 50 mm board it puts the corners about
/// 0.44 mm out, the same error at the same scale.
///
/// [`POSE_MAX_RMS_MM`]: super::fiducial_ui::POSE_MAX_RMS_MM
const OFFSET_TELL_MM: f64 = 0.5;
const OFFSET_TELL_DEG: f64 = 0.5;

impl ConsoleApp {
    /// The carried manual offset as the operator should read it, plus whether
    /// it is small enough to be uninteresting (drives the status colour).
    ///
    /// One formatter for both places it appears — beside the ⊕ recentre button
    /// that undoes it, and beside the x/y/rot fields the etch burns — so the
    /// two can never quote different numbers for the same state.
    pub(super) fn placement_offset_text(&self) -> (bool, String) {
        match self.placement_deviation() {
            None => (true, "manual placement — no fiducial reference".into()),
            Some((mm, deg)) if mm <= OFFSET_TELL_MM && deg.abs() <= OFFSET_TELL_DEG => (
                true,
                format!("on the fiducial pose ({mm:.2} mm / {deg:+.2}°)"),
            ),
            Some((mm, deg)) => (
                false,
                format!("⚠ {mm:.2} mm / {deg:+.2}° off the fiducial pose"),
            ),
        }
    }

    /// The frame the placement is measured against, in pixels: the fiducial
    /// frame the pose was fitted in, else the last camera grab. Only the
    /// DIMENSIONS matter — they are what `place_projection` validates the lens
    /// calibration against (resolution/crop/orientation must match the fit).
    pub(super) fn place_frame_dims(&self) -> Option<(u32, u32)> {
        self.fiducials
            .frame_img
            .as_ref()
            .or(self.camera.last.as_ref())
            .map(image::GenericImageView::dimensions)
    }

    /// Move the placement so its pivot's **pixel** position shifts by
    /// `(dpx, dpy)` frame pixels of a `width`×`height` frame. Dragging felt
    /// wrong under perspective because the old code added a uniform mm delta —
    /// a uniform mm step is *not* a uniform pixel step on a tilted plane, so
    /// the overlay slid along the plane instead of following the cursor. Here
    /// we map the pivot to pixels through the same projection the outline is
    /// drawn with, shift in pixels, and invert back to bed-mm — so the geometry
    /// tracks where the mouse moves over the image.
    pub(super) fn drag_place_px(
        &mut self,
        width: u32,
        height: u32,
        dpx: f64,
        dpy: f64,
    ) -> Result<(), String> {
        let projection = self.place_projection(width, height)?;
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

    /// The through-hole exit magnification the CURRENT optics imply,
    /// `1 + thickness/focal` — 1.0 when either is unset.
    ///
    /// Read straight off the job fields rather than through
    /// [`back_field_params`](Self::back_field_params), which is `None` on the
    /// front: the FRONT needs this number too. It is the tell that separates a
    /// genuine front fit from a flipped board fitted through a mirror-symmetric
    /// layout, where the mirror flag alone cannot decide.
    pub(super) fn exit_magnification(&self) -> f64 {
        cam::flip::FieldParams {
            scan_center_mm: (0.0, 0.0),
            thickness_mm: self.job.board_thickness_mm,
            focal_mm: self.job.focal_mm,
        }
        .exit_magnification()
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
        // The other face's fit is not a frame this face's placement can be
        // measured against, so there is no offset to carry into its first Check.
        self.fiducials.last_fit = None;
        // The other face's measurements are in a mirrored frame — not a layout
        // this side could adopt.
        self.fiducials.detected_mm.clear();
        // The photo, its texture and the scale measured off it all belong to the
        // face that was showing. Kept, they put the FRONT board on screen under
        // a Back selection — and a Check against that stale image is not merely
        // confusing, it can fit and lock a placement from the wrong face.
        self.fiducials.frame_img = None;
        self.fiducials.frame_tex = None;
        self.fiducials.measured_ppm = None;
        // …and the note describing that detection goes with it. Back to the
        // tab's opening instruction rather than blank: a side switch does a lot,
        // and an empty line reads as nothing having happened.
        self.fiducials.note = "Load a frame, click each marker onto its hole, then Check.".into();
        // A side switch is a new scene. The whole-frame scan's Live backoff was
        // earned against the other face's frame, and up to 40 s of it would
        // suppress exactly the re-acquisition a flip needs.
        self.fiducials.last_global_recover = None;
        self.ar.board.clear();
        self.ar.copper.clear();
        self.ar.ablate.clear();
        self.placement.job.clear();
        // The fitted pose was for the old face's detection — drop it so the
        // next Load recenters normally until a fresh Check re-fits.
        self.placement.auto_pose = false;
        // …and with it the fitted resize: the other face's scale is not this
        // face's, and a stale one would silently resize the next job.
        self.placement.scale = 1.0;
        // Also drop the cached preview, or the Job tab keeps painting the other
        // side's render until a new one is made (LR-41).
        self.job.preview_tex = None;
    }

    /// Current manual placement.
    pub(super) fn placement(&self) -> crate::place::Placement {
        crate::place::Placement {
            tx_mm: self.placement.tx_mm,
            ty_mm: self.placement.ty_mm,
            rot_deg: self.placement.rot_deg,
            scale: self.placement.scale,
            pivot_mm: self.placement.pivot,
        }
    }

    /// Where a freshly loaded design starts, in bed mm, plus how that was
    /// chosen (for the note). Preferred: the centre of the fiducial frame the
    /// operator is looking at, mapped through the SAME projection the outline
    /// is drawn with — under an active homography a uniform px/mm would land
    /// the job far off-centre until dragged (LR-42). With no frame, or no
    /// calibration to map one with, the middle of the work area is still a
    /// position the operator can drag from.
    fn initial_place_center_mm(&self) -> ((f64, f64), &'static str) {
        if let Some((w, h)) = self.fiducials.frame_img.as_ref().map(|f| f.dimensions())
            && let Ok(center) = self.initial_center_mm(w as f64, h as f64)
        {
            return (center, "centred on the fiducial frame");
        }
        (
            (
                self.camera.field_cx_mm as f64,
                self.camera.field_cy_mm as f64,
            ),
            "centred on the work area",
        )
    }

    /// Load the DESIGN — the Job-tab Gerbers for the active side — into the
    /// placement cache and give it a starting position. There is no bed image
    /// to load any more: the Fiducial-check tab draws the job over the frame it
    /// already has, so grabbing a camera frame here would cost a capture and
    /// prove nothing.
    pub fn load_place(&mut self) {
        let (_, _, ablate) = match self.active_job() {
            Ok(t) => t,
            Err(e) => {
                self.placement.note = format!("job: {e}");
                return;
            }
        };
        self.placement.pivot = crate::place::bbox_center_mm(&ablate);
        // Skip when the pose was fitted from fiducials: Load must not
        // recenter/zero over an auto-placement.
        let where_note = if self.placement.auto_pose {
            "kept the fiducial-locked pose"
        } else {
            let ((cx, cy), note) = self.initial_place_center_mm();
            self.placement.tx_mm = cx;
            self.placement.ty_mm = cy;
            self.placement.rot_deg = 0.0;
            // Recentering means "nominal placement" — the job must go back to
            // its design size too, not keep a previous fit's resize.
            self.placement.scale = 1.0;
            note
        };
        self.placement.job = ablate;
        self.placement.note = format!(
            "design loaded, {where_note} — at ({:.1}, {:.1}) mm, {:.0}°",
            self.placement.tx_mm, self.placement.ty_mm, self.placement.rot_deg
        );
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
                text: "place: load the design first (⤵ Load design)".into(),
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
        let Some(dimensions) = self.place_frame_dims() else {
            self.runtime.log.push(LogLine {
                text: "place: check fiducials (or grab a camera frame) before export — \
                       the field warp is only valid for the frame it was calibrated in"
                    .into(),
                err: true,
            });
            return;
        };
        // Measured once, up front: the burn record below names the offset from
        // the fitted pose, and it has to be the offset that was actually
        // exported rather than one re-read after the fact.
        let deviation = self.placement_deviation();
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
        //
        // The offset from the fiducial pose rides on this line UNCONDITIONALLY,
        // including when it is zero. It used to appear only as a clause in the
        // Fiducial-check tab's note, on another tab, and only once it was
        // non-trivial — so the log of the burn itself, the record anyone reads
        // afterwards, never said whether the job went where the holes put it.
        let offset_note = match deviation {
            Some((mm, deg)) => format!(" · {mm:.2} mm / {deg:+.2}° off the fiducial pose"),
            None => " · manual placement — no fiducial reference".into(),
        };
        self.runtime.log.push(LogLine {
            text: format!(
                "Etch here → {out}\n  job placed at ({:.2}, {:.2}) mm, {:.1}°{offset_note}{field_note} — OPEN THIS FILE (not the Job-tab emit output)",
                self.placement.tx_mm, self.placement.ty_mm, self.placement.rot_deg
            ),
            err: false,
        });
        // Record 4a — the placement, its affine, the correspondences string it
        // was encoded as and the argv, before the CLI ever sees them.
        self.diag_export("etch", &args, &out_path, use_field);
        let started = self.run_verb(&args);
        // Measure the written file once the verb reports, but only if it really
        // started: a refused click must not attribute an older file to it.
        if started {
            self.diag_arm_readback("etch", out_path.clone(), use_field);
        }
        // Queue the LightBurn run only when the export actually launched — a
        // refused click (a job already running) must not arm the chain against
        // a file this click never wrote. Resolve to an ABSOLUTE path without
        // canonicalizing: the file may not exist yet, and \\?\ prefixes upset
        // LightBurn's FORCELOAD.
        if run_after && started {
            match std::path::absolute(&out_path) {
                Ok(abs) => {
                    self.runtime.pending_lightburn = Some(PendingLightburn {
                        path: abs,
                        start: true,
                    });
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

    /// The drill files the KiCad export writes for the Job-tab project, at
    /// their deterministic location (`<board dir>/pcbforge-gerbers/{pth,npth}.drl`
    /// — the same pair [`Self::drills_from_kicad`] fills in) — but only the ones
    /// that are actually on disk. `None` when no project is set, the board
    /// can't be resolved, or neither file exists: nothing to emit, so the
    /// caller's "name a drill file" error still stands.
    fn kicad_drill_paths(&self) -> Option<Vec<String>> {
        let proj = crate::clean_path(&self.job.kicad_project);
        if proj.trim().is_empty() {
            return None;
        }
        let board = ingest::kicad_cli::resolve_board(std::path::Path::new(&proj)).ok()?;
        let out_dir = board
            .parent()
            .map(|p| p.join("pcbforge-gerbers"))
            .unwrap_or_else(|| PathBuf::from("pcbforge-gerbers"));
        let found: Vec<String> = ["pth.drl", "npth.drl"]
            .into_iter()
            .map(|name| out_dir.join(name))
            .filter(|p| p.exists())
            .map(|p| p.display().to_string())
            .collect();
        (!found.is_empty()).then_some(found)
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
                text: "place: load the design first (⤵ Load design)".into(),
                err: true,
            });
            return;
        }
        let mut drill_paths: Vec<String> = self
            .placement
            .drills
            .split(';')
            .map(crate::clean_path)
            .filter(|p| !p.trim().is_empty())
            .collect();
        // An empty field is not an error while the Job tab names a KiCad
        // project: the export writes its .drl files at a fixed place, so derive
        // them and fill the field instead of refusing. The files are re-read
        // from disk on every emit, so a fresh kicad-cli export is picked up
        // without re-exporting from here.
        if drill_paths.is_empty() {
            match self.kicad_drill_paths() {
                Some(found) => {
                    self.placement.drills = found.join(";");
                    drill_paths = found;
                }
                None => {
                    self.runtime.log.push(LogLine {
                        text: "place: set a drill file (.drl) first — KiCad exports PTH and \
                               NPTH holes as two files; list both separated by ; (or set the \
                               KiCad project and run ⚙ Drills from KiCad)"
                            .into(),
                        err: true,
                    });
                    return;
                }
            }
        }
        let Some(dimensions) = self.place_frame_dims() else {
            self.runtime.log.push(LogLine {
                text: "place: check fiducials (or grab a camera frame) before export — \
                       the field warp is only valid for the frame it was calibrated in"
                    .into(),
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
        // The drill's own recipe over the register verb's default power —
        // punching a hole is not etching copper, so it doesn't borrow the
        // Job-tab numbers.
        let params = pcb_core::AblationParams {
            power_pct: DRILL_POWER_PCT,
            speed_mm_s: self.drill.speed_mm_s,
            frequency_khz: self.drill.frequency_khz,
            pulse_ns: self.drill.pulse_ns,
            passes: self.drill.passes,
        };
        // A hole is TRACED, not scan-filled: `drill_polys` hands over the
        // outline ring of each hole/slot and LightBurn follows it, so the layer
        // is Line (`type="Cut"`).
        let mut layer =
            cam::lbrn2::EmitLayer::line("DRILL", params, cam::lbrn2::polys_to_elems(&placed));
        layer.interval_mm = self.drill.interval_mm;
        // `line` defaults wobble off; carry the operator's setting through.
        layer.wobble = self.drill.wobble;
        layer.wobble_step_mm = self.drill.wobble_step_mm;
        layer.wobble_size_mm = self.drill.wobble_size_mm;
        let out_path = self.resolve_drill_output(&drill_paths[0]);
        let out = out_path.to_string_lossy().into_owned();
        // Record 4a. There is no argv — this path writes the file itself rather
        // than shelling `register` — so the "argv" is the input list it read.
        let inputs: Vec<String> = drill_paths.clone();
        self.diag_export("drill", &inputs, &out_path, use_field);
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
        // Record 4b, inline: this export is in-process, so the file is already
        // on disk and there is no verb completion to wait for.
        let readback = DiagReadback {
            path: out_path.clone(),
            kind: "drill",
            check: self.runtime.diag_check_seq,
            field_warped: use_field,
        };
        self.diag_export_readback(&readback);
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

    /// The bed/output path fields the Job tab renders — the design's
    /// destinations (etch + drill `.lbrn2`, the LightBurn device, the drill
    /// inputs). They sit beside the Gerbers that feed them rather than next to
    /// the buttons: they are typed once and then left alone.
    pub(super) fn placement_paths_form(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("place-paths-form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                let lbl = ui.label("out .lbrn2");
                ui.add(egui::TextEdit::singleline(&mut self.placement.lbrn2).desired_width(240.0))
                    .labelled_by(lbl.id)
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
                    "Device name for \"🔥 Etch + Run\" — must match a configured \
                     LightBurn device (the LASER: automation command selects it \
                     before loading the file).",
                );
                ui.end_row();
                let drl = ui.label("drill .drl");
                ui.add(egui::TextEdit::singleline(&mut self.placement.drills).desired_width(240.0))
                    .labelled_by(drl.id)
                    .on_hover_text(
                        "Excellon drill file(s) for \"Emit drill holes\" — KiCad \
                         exports PTH and NPTH holes as two files; list both \
                         separated by ; to get every hole. Left empty, the emit \
                         derives them from the Actions-panel KiCad project.",
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
                let l = ui.label("drill interval mm");
                ui.add(
                    egui::DragValue::new(&mut self.drill.interval_mm)
                        .speed(0.001)
                        .range(0.0..=1.0),
                )
                .labelled_by(l.id)
                .on_hover_text(
                    "Line spacing written with the drill layer; 0 leaves the \
                     LightBurn device profile's own value in the file.",
                );
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
                         line. The export writes wobbleEnable explicitly either \
                         way, so the device profile can't override it.",
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
                let bed = ui.label("bed frame");
                ui.add(egui::TextEdit::singleline(&mut self.placement.frame).desired_width(240.0))
                    .labelled_by(bed.id)
                    .on_hover_text(
                        "Where the Camera tab's \"save snapshot\" writes the bed \
                         image. Nothing reads it back — the Fiducial-check tab \
                         works from the frame it grabbed.",
                    );
                ui.end_row();
            });
        ui.weak(
            "The drill recipe is independent of the etch recipe: \"Emit drill holes\" \
             writes a Line layer that traces each hole/slot outline at these settings.",
        );
    }

    /// The placement action block in the right-hand Actions panel: load the
    /// design, export/burn it where it now sits, and the pose readout that says
    /// where that is. The Place tab that used to host these is gone — the job
    /// is placed by dragging it on the Fiducial-check tab, over the very frame
    /// its registration was measured in.
    pub(super) fn placement_actions(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Place on the board").strong());
        // Wrapped throughout this panel: it is a ~300 px side panel and these
        // button labels are long, so a plain `horizontal` pushes the second
        // button off the right edge where it cannot be clicked at all.
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("⤵ Load design")
                .on_hover_text(
                    "Load the Job-tab Gerbers as the placed design and start it in \
                     the middle of the current fiducial frame (or of the work area \
                     when there is none). A fiducial lock is never recentred over.",
                )
                .clicked()
            {
                self.load_place();
            }
            if ui
                .button("▶ Etch here (register)")
                .on_hover_text(
                    "Export the registered job at this placement without touching \
                     LightBurn — open the written file yourself.",
                )
                .clicked()
            {
                self.emit_at_placement(false);
            }
        });
        ui.horizontal_wrapped(|ui| {
            // One-click: export, then load + run it in LightBurn. Disabled while
            // a run is in flight so a second click can't stack a second job.
            if ui
                .add_enabled(!self.lightburn_busy(), egui::Button::new("🔥 Etch + Run"))
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
        });
        // Disabled while a LightBurn run is in flight, like "🔥 Etch + Run": the
        // load-only run replaces `lightburn_run`, and stomping a live burn's
        // progress reporting would be rude.
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
        if self.calibration.field.is_some() && !self.calibration.field_accepted {
            ui.colored_label(
                status_color(false),
                "⚠ latest step-3 field fit was rejected; nonlinear placement and correction are disabled",
            );
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("x mm");
            ui.add(egui::DragValue::new(&mut self.placement.tx_mm).speed(0.1));
            ui.label("y mm");
            ui.add(egui::DragValue::new(&mut self.placement.ty_mm).speed(0.1));
            ui.label("rot°");
            ui.add(
                egui::DragValue::new(&mut self.placement.rot_deg)
                    .speed(0.5)
                    .range(-180.0..=180.0),
            );
        });
        // The fiducial-fitted resize, read-only + a reset. It changes the
        // burned dimensions, so it cannot sit only in the fiducial note on
        // another tab — the operator must see it where they see x/y/rot.
        let off = (self.placement.scale - 1.0).abs() > super::fiducial_ui::POSE_SCALE_QUIET;
        let text = if off {
            format!(
                "scale {:.4} — job burns {:+.2}% (from fiducials)",
                self.placement.scale,
                (self.placement.scale - 1.0) * 100.0
            )
        } else {
            format!(
                "scale {:.4} — job burns at design size",
                self.placement.scale
            )
        };
        ui.colored_label(status_color(!off), text);
        if off && ui.button("reset scale to 1.000").clicked() {
            self.placement.scale = 1.0;
        }
        // How far the job has been nudged off the pose the fiducials fitted —
        // next to the x/y/rot that will be burned, because this is the panel
        // the operator is looking at when they press Etch. The undo for it is
        // ⊕ recentre on fiducials, named here so it can be found from the
        // number rather than only by browsing the other tab's toolbar.
        let (on_pose, offset_text) = self.placement_offset_text();
        ui.colored_label(status_color(on_pose), offset_text)
            .on_hover_text(
                "Distance and rotation between this placement and the pose the last \
                 applied fiducial Check put it at. A manual drag is carried across \
                 later Checks on purpose, so it persists until it is undone: \
                 \"⊕ recentre on fiducials\" on the Fiducial-check tab drops it. \
                 Past 0.5 mm / 0.5° it is flagged; the export log names it either way.",
            );
        ui.label(egui::RichText::new(&self.placement.note).weak());
        ui.weak(
            "Uses the Job-tab Gerbers; drag the design on the Fiducial-check tab. \
             \u{201c}Etch here\u{201d} field-warps every edge when a field calibration is \
             accepted, else exports unwarped.",
        );
    }
}
