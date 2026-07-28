use super::*;

/// Which view the central panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CentralTab {
    Job,
    Camera,
    Calibrate,
    Fiducials,
}

/// Which face of a (possibly double-sided) board the operator is working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Front,
    Back,
}

/// Which calibration the operator is doing on the Calibrate tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CalibMode {
    /// Printed reference grid → lens-distortion map (camera becomes a ruler).
    CameraLens,
    /// Burned grid → the laser anchor (camera-px → commanded machine mm).
    LaserAnchor,
    /// Burned grid through the metric camera → the laser field pre-distortion.
    LaserField,
    /// Auto-lay four fiducial holes for a board centred on the field, generated
    /// with the laser-field pre-distortion so spacing/size burn true.
    FidHoles,
}

/// Correction for how the camera is physically mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Orientation {
    Normal,
    FlipH,
    FlipV,
    Rotate180,
}

impl Orientation {
    pub(super) const ALL: [Orientation; 4] = [
        Orientation::Normal,
        Orientation::FlipH,
        Orientation::FlipV,
        Orientation::Rotate180,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Orientation::Normal => "Normal",
            Orientation::FlipH => "Flip ↔ (mirror)",
            Orientation::FlipV => "Flip ↕",
            Orientation::Rotate180 => "Rotate 180° (upside down)",
        }
    }

    pub(super) fn token(self) -> &'static str {
        match self {
            Orientation::Normal => "normal",
            Orientation::FlipH => "flip_h",
            Orientation::FlipV => "flip_v",
            Orientation::Rotate180 => "rotate180",
        }
    }

    pub(super) fn from_token(s: &str) -> Option<Orientation> {
        Orientation::ALL.into_iter().find(|o| o.token() == s)
    }

    pub(super) fn apply(self, img: image::GrayImage) -> image::GrayImage {
        use image::imageops;
        match self {
            Orientation::Normal => img,
            Orientation::FlipH => imageops::flip_horizontal(&img),
            Orientation::FlipV => imageops::flip_vertical(&img),
            Orientation::Rotate180 => imageops::rotate180(&img),
        }
    }
}

/// The ③ scale-handling choices, in the order they are offered. `calib` owns
/// the enum (it is a fit parameter), so the label/token pattern the other
/// persisted enums carry as inherent methods lives here as free functions.
pub(super) const FIELD_SCALE_ALL: [calib::FieldScale; 3] = [
    calib::FieldScale::Refuse,
    calib::FieldScale::Compensate,
    calib::FieldScale::DistortionOnly,
];

pub(super) fn field_scale_label(s: calib::FieldScale) -> &'static str {
    match s {
        calib::FieldScale::Refuse => "refuse a machine scale error",
        calib::FieldScale::Compensate => "compensate machine scale",
        calib::FieldScale::DistortionOnly => "correct distortion only (keep 1:1 work area)",
    }
}

pub(super) fn field_scale_token(s: calib::FieldScale) -> &'static str {
    match s {
        calib::FieldScale::Refuse => "refuse",
        calib::FieldScale::Compensate => "compensate",
        calib::FieldScale::DistortionOnly => "distortion_only",
    }
}

pub(super) fn field_scale_from_token(s: &str) -> Option<calib::FieldScale> {
    FIELD_SCALE_ALL
        .into_iter()
        .find(|m| field_scale_token(*m) == s)
}

pub(super) struct RuntimeState {
    pub(super) settings_path: PathBuf,
    /// The durable diagnostic log beside the settings blob (`<db>.console-log`).
    /// Written on state changes and operator actions only — never per frame.
    pub(super) diag: crate::diag::Diag,
    /// Correlation id for one fiducial check and everything that follows from
    /// it. The check record, the overlay bbox and both halves of an export all
    /// carry `check=N`, because the records they need to be read against each
    /// other are produced by different code paths, frames apart — the export
    /// readback lands only when the CLI child exits. `grep check=7` is what
    /// makes them adjacent.
    pub(super) diag_check_seq: u64,
    /// How many entries of `log` have been mirrored into the diagnostic file.
    /// Kept in step with the 500-line trim in `pump_verb`.
    pub(super) diag_mirrored: usize,
    /// A diagnostic write failure has already been reported to the operator.
    /// Latched, so a broken sink can't push an error line that fails to mirror
    /// and pushes another.
    pub(super) diag_failure_reported: bool,
    /// A `.lbrn2` an in-flight export verb is writing, to be re-read and its
    /// geometry bbox logged once the verb reports success (see `pump_verb`).
    /// Armed only when the verb actually started, alongside `pending_lightburn`.
    pub(super) diag_readback: Option<DiagReadback>,
    /// The placement the overlay bbox was last computed for — `[tx, ty, rot,
    /// scale, pivot_x, pivot_y, job_len]`. The overlay redraws every frame, so
    /// this guards the recompute (a pass over every design vertex) as well as
    /// the record.
    pub(super) diag_overlay_key: Option<[f64; 7]>,
    /// The last LOGGED overlay bbox in machine mm, `[x0, y0, x1, y1]`. A new one
    /// is only recorded once it moves by more than `OVERLAY_EPS_MM`, so a drag
    /// writes a handful of records instead of one per frame.
    pub(super) diag_overlay_bbox: Option<[f64; 4]>,
    pub(super) last_settings: String,
    pub(super) settings_error: Option<String>,
    pub(super) status: StatusSnapshot,
    pub(super) log: Vec<LogLine>,
    pub(super) tab: CentralTab,
    pub(super) verb_job: Option<VerbJob>,
    /// A LightBurn hand-off queued to fire when `verb_job` finishes. Cleared
    /// once consumed or skipped.
    pub(super) pending_lightburn: Option<PendingLightburn>,
    /// The active (or most recent) LightBurn run — kept after it finishes so the
    /// terminal state stays visible; a new run replaces it.
    pub(super) lightburn_run: Option<LightburnRun>,
    /// The console's ONE capture thread, shared by every tab that wants frames.
    ///
    /// There is one camera, so there can only be one open device: the Camera,
    /// Calibration and Fiducial tabs used to each own a `Capture`, and two Live
    /// toggles at once fought over the device. Worse, every one-shot grab opened
    /// and closed the device itself — ~2.1 s of pure init (open 280 ms,
    /// first-frame warm-up ~1.2 s, close 630 ms) on the UI thread, every time.
    /// Keeping the thread alive between grabs turns each later grab into a slot
    /// read; [`should_release_capture`](super::camera_ui::should_release_capture)
    /// hands the device back once no tab needs it (the CLI can't open a busy
    /// camera).
    pub(super) camera_capture: Option<crate::camera::Capture>,
    /// The source `camera_capture` was started for, so a source change restarts
    /// it instead of streaming the wrong camera.
    pub(super) camera_capture_src: Option<crate::camera::Source>,
    /// When the shared capture was last started or read, for the idle release.
    pub(super) camera_last_used: Option<std::time::Instant>,
}

/// An export whose written file should be re-read and measured once the verb
/// that writes it finishes — the "what the machine will actually do" half of an
/// export record, which cannot be known at argv time.
pub(super) struct DiagReadback {
    pub(super) path: PathBuf,
    /// Which export wrote it (`etch` / `fid-holes`), for the record's label.
    pub(super) kind: &'static str,
    /// The `check=N` the export was made under.
    pub(super) check: u64,
    /// The export applied a laser-field map, so the file's coordinates are
    /// COMMANDED mm rather than physical mm.
    pub(super) field_warped: bool,
}

/// A LightBurn hand-off waiting on the export verb that writes its file.
pub(super) struct PendingLightburn {
    /// The ABSOLUTE .lbrn2 the export is writing (not canonicalized — `\\?\`
    /// prefixes upset LightBurn's FORCELOAD).
    pub(super) path: PathBuf,
    /// Send START once the file is loaded. `false` is a **load-only** hand-off:
    /// the job opens in LightBurn and the operator presses play — the contract
    /// the drill and fiducial-hole exports hold, since neither click may fire
    /// the laser.
    pub(super) start: bool,
}

pub(super) struct JobState {
    pub(super) kicad_project: String,
    pub(super) emit_copper: String,
    pub(super) emit_outline: String,
    pub(super) emit_lbrn2: String,
    pub(super) offset_mm: f64,
    pub(super) side: Side,
    pub(super) back_copper: String,
    pub(super) back_outline: String,
    /// The back side's `.lbrn2` output — its own file, so emitting the back
    /// leaves the front job intact.
    pub(super) back_lbrn2: String,
    pub(super) board_thickness_mm: f64,
    pub(super) focal_mm: f64,
    pub(super) scan_center_auto: bool,
    pub(super) scan_center_mm: (f64, f64),
    pub(super) speed_mm_s: f64,
    pub(super) frequency_khz: f64,
    pub(super) pulse_ns: u32,
    pub(super) interval_mm: f64,
    pub(super) passes: u32,
    /// Wobble opt-in for the exports (emit + register). Off by default — the
    /// emitted file says `wobbleEnable=0` explicitly so the device profile
    /// can't re-enable it.
    pub(super) wobble: bool,
    /// Wobble step along the path, mm (0 = the device profile's value).
    pub(super) wobble_step_mm: f64,
    /// Wobble diameter, mm (0 = the device profile's value).
    pub(super) wobble_size_mm: f64,
    pub(super) preview_tex: Option<TextureHandle>,
    pub(super) preview_note: String,
}

pub(super) struct CameraState {
    pub(super) use_device: bool,
    pub(super) device: u32,
    pub(super) file: String,
    pub(super) orientation: Orientation,
    pub(super) live: bool,
    pub(super) tex: Option<TextureHandle>,
    pub(super) note: String,
    pub(super) last: Option<image::GrayImage>,
    pub(super) view_scale: f64,
    pub(super) devices: Vec<(u32, String)>,
    pub(super) show_bed: bool,
    pub(super) field_mm: f32,
    pub(super) field_center_auto: bool,
    pub(super) field_cx_mm: f32,
    pub(super) field_cy_mm: f32,
}

/// One grid-parameter set (dots per side, pitch, dot size, contrast). The ①
/// printed paper and the ②③ burned grid each get their own — sharing one set
/// across the steps let a burned-grid pitch silently mis-scale the ① metric
/// ruler (and vice versa).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GridParams {
    pub(super) n: usize,
    pub(super) pitch_mm: f64,
    pub(super) dot_mm: f64,
    pub(super) dot_kind: calib::DotKind,
}

pub(super) struct CalibrationState {
    pub(super) anchor: Option<calib::Calibration>,
    pub(super) saved_at: Option<u64>,
    /// ① printed paper: the MEASURED pitch (calipers — printers scale).
    pub(super) paper: GridParams,
    /// ②③ burned grid: the COMMANDED pitch.
    pub(super) burn: GridParams,
    pub(super) grid_origin_mm: (f64, f64),
    pub(super) grid_out: String,
    /// ① printed-paper grid SVG output path (`paper-grid` verb).
    pub(super) paper_out: String,
    pub(super) frame: String,
    pub(super) frame_img: Option<image::GrayImage>,
    pub(super) frame_tex: Option<TextureHandle>,
    pub(super) corners: Vec<(f64, f64)>,
    pub(super) mode: CalibMode,
    pub(super) lens: Option<calib::CameraCal>,
    pub(super) lens_frame_signature: Option<((u32, u32), Orientation)>,
    pub(super) field: Option<calib::FieldCal>,
    pub(super) field_accepted: bool,
    /// Operator-configurable step 3 laser-field acceptance limits (µm): a fit
    /// whose residual RMS / worst per-dot error exceeds these is rejected. The
    /// defaults accept the rig's demonstrated measurement floor.
    pub(super) accept_rms_um: f64,
    pub(super) accept_worst_um: f64,
    /// What the ③ fit does about a large uniform burn-vs-paper scale. Defaults
    /// to `Refuse` — a gross scale is usually a setup error, and fixing the
    /// field size in LightBurn is the cleaner fix when it isn't.
    pub(super) field_scale: calib::FieldScale,
    /// The mode that produced the ACTIVE `field` — which is not `field_scale`
    /// once the operator changes the control without re-fitting, nor after a
    /// restore from disk. The ③ status line reads this, so it describes the
    /// calibration in force rather than the pending choice.
    pub(super) field_scale_used: calib::FieldScale,
    pub(super) lens_arrow_scale: f32,
    pub(super) anchor_resid_scale: f32,
    pub(super) edit_anchor_dots: bool,
    /// Per-session view toggle: when off, the post-fit feedback overlays (lens
    /// arrows, anchor mesh/residuals, field lattice + REJECTED banner) are
    /// hidden so the operator can see the bare dots to re-click the 4 corners.
    /// Not persisted — like `corners`, it resets with each session/frame.
    pub(super) show_fit_feedback: bool,
    pub(super) live: bool,
    pub(super) note: String,
}

impl CalibrationState {
    /// The grid-parameter set the active step edits and fits with: ① Camera
    /// lens uses the paper set, ②③ use the burned-grid set.
    pub(super) fn active_params_mut(&mut self) -> &mut GridParams {
        match self.mode {
            CalibMode::CameraLens => &mut self.paper,
            _ => &mut self.burn,
        }
    }
}

pub(super) struct FiducialState {
    pub(super) frame: String,
    pub(super) layout: String,
    pub(super) px_per_mm: f64,
    pub(super) shape: crate::fiducial::ShapeKind,
    /// Circle diameter, or rectangle width (`shape == Rect`).
    pub(super) diameter_mm: f64,
    /// Rectangle height (ignored when `shape == Circle`).
    pub(super) height_mm: f64,
    pub(super) search_mm: f64,
    pub(super) profile: crate::fiducial::ProfileKind,
    /// Output path for the generated fiducial-holes .lbrn2 (`fid-holes` verb).
    pub(super) out: String,
    /// Width of the fiducial rectangle — the x span between hole CENTRES. The
    /// rectangle is centred in the work area, so this plus [`rect_h_mm`] fixes
    /// all four positions without typing coordinates.
    ///
    /// [`rect_h_mm`]: Self::rect_h_mm
    pub(super) rect_w_mm: f64,
    /// Height of the fiducial rectangle — the y span between hole CENTRES,
    /// centred in the work area (mm).
    pub(super) rect_h_mm: f64,
    /// How often (seconds) Live re-runs the detection ladder's stage 3 — the
    /// whole-frame rectangle match — while the holes are lost. The operator's
    /// dial on the trade between following a board that keeps moving and the
    /// ~180 ms hitch each attempt costs; a failed attempt waits 4× this. Clamped
    /// to 0.1..=10.0 both in the DragValue and on load, because the Duration is
    /// built with `from_secs_f64`, which panics on a negative or NaN value.
    pub(super) live_recover_s: f64,
    pub(super) click_place: bool,
    /// Draw the placed job over the fiducial frame, so a lock can be judged
    /// against the holes it was fitted to without leaving the tab.
    pub(super) show_placement: bool,
    /// The most recent detection in MACHINE mm (`place_projection`-mapped),
    /// aligned with the layout. This is the frame `fit_board_pose` fits in, so
    /// it is what "⌖ layout from detection" writes back as the new nominal.
    pub(super) detected_mm: Vec<Option<(f64, f64)>>,
    pub(super) note: String,
    pub(super) rows: Vec<FidRow>,
    pub(super) measured_ppm: Option<f64>,
    pub(super) frame_img: Option<image::GrayImage>,
    pub(super) frame_tex: Option<TextureHandle>,
    pub(super) search: Vec<(f64, f64)>,
    pub(super) found: Vec<Option<(f64, f64)>>,
    /// An active click-the-fiducials-in-order marking round: `Some(k)` means the
    /// next primary canvas click drops search marker `k` (layout order). `None`
    /// when no round is active — the next plain click implicitly starts one at
    /// marker 0 (Check is always available regardless).
    pub(super) marking: Option<usize>,
    /// Whether the MOST RECENT detection actually wrote the Place placement.
    /// Distinct from `pose`/`placement.auto_pose`, which a rejected fit leaves
    /// at their last-good value — this resets to false on every detection and
    /// only goes true on a successful apply, so the verdict never shows a stale
    /// "placement updated" after a Check that was gated out.
    pub(super) last_placed: bool,
    pub(super) homography: Option<vision::Homography>,
    /// The most recently APPLIED board pose (only cached on a successful,
    /// side-matching, in-tolerance fit that wrote the placement); a rejected
    /// fit leaves this unchanged and the note carries the reason.
    pub(super) pose: Option<crate::fiducial::BoardPose>,
    /// The nominal-layout → measured-bed fit of the most recent APPLIED Check.
    /// It is the reference frame the operator's manual placement offset is
    /// measured in: mapping the current placement back through it says where
    /// the design sits RELATIVE TO THE BOARD, which the next Check re-applies
    /// under its own fit so the adjustment travels with the board instead of
    /// being overwritten. `None` (no applied Check yet, or the layout changed
    /// under it) means there is no offset to carry and the design re-centres.
    pub(super) last_fit: Option<calib::Similarity2>,
    /// The operator has explicitly ARMED "drag the design to move the job".
    ///
    /// Without this, any plain drag that started inside the design's screen
    /// bbox re-placed the job — including a pan attempt, since navigation is
    /// Ctrl-only (`imgview::is_navigating`). That is how a registered board
    /// silently acquired a 17.7 mm offset and 5° of rotation between the lock
    /// and the burn.
    ///
    /// One-shot: cleared when the drag it authorised finishes, and on a new
    /// frame or a side switch, so it can never be left armed across scenes. A
    /// gesture-level intent, not a setting — never persisted, and it must come
    /// up OFF after a restart.
    pub(super) move_job: bool,
    /// Latched on `drag_started` when the pointer went down INSIDE the drawn
    /// design AND `move_job` was armed: for the rest of that drag the gesture
    /// moves/rotates the job and must not also drop a ✛ or add a click-placed
    /// fiducial. A per-frame local can't hold it — the overlay function re-runs
    /// every frame — and it is a gesture, not a setting, so it is never
    /// persisted.
    pub(super) design_drag: bool,
    /// Latched on `drag_started` when the pointer went down on an existing ✛:
    /// for the rest of that drag the gesture moves THAT marker (index into
    /// `search`) and must not mark, add or remove one. Re-picking the nearest
    /// marker every frame instead would let a fast drag hop between markers.
    /// Beats `design_drag` — the design's hit test is a coarse bbox that
    /// usually contains the markers. A gesture, not a setting: never persisted.
    pub(super) marker_drag: Option<usize>,
    /// What the in-flight canvas gesture grabbed and where it started, kept so
    /// the `drag_stopped` record can state a delta rather than a position.
    /// `Some` only between `drag_started` and `drag_stopped`.
    pub(super) drag_origin: Option<DragOrigin>,
    pub(super) live: bool,
    /// When the detection ladder's stage 3 (the whole-frame rectangle match)
    /// last ran ON THE LIVE FEED, and whether that run recovered any holes.
    /// Stage 3 is a whole-frame scan on the UI thread, so under a live feed it
    /// has to be throttled rather than run per short frame; see
    /// `should_global_recover`.
    ///
    /// The OUTCOME is stored, not the window it earned: the window is derived
    /// from the current `live_recover_s` at compare time, so turning the dial
    /// down takes effect on the next frame instead of after the snapshotted one
    /// expires. Manual Checks never stamp this — they are not on the feed's
    /// budget. Runtime timing, not a setting: never persisted.
    pub(super) last_global_recover: Option<(std::time::Instant, bool)>,
}

/// One canvas gesture on the fiducial frame, as the diagnostic log describes
/// it. The console recorded no pointer events at all, so attributing a job that
/// moved between the lock and the burn meant fingerprinting placement affines
/// after the fact; a started/stopped pair per gesture makes it a grep.
#[derive(Debug, Clone)]
pub(super) struct DragOrigin {
    /// `marker` / `design` / `none` — which of the overlay's grab targets the
    /// press latched onto, in the priority order `fid_frame_overlay` decides.
    pub(super) target: &'static str,
    /// The ✛ index, when `target` is `marker`.
    pub(super) marker: Option<usize>,
    /// Modifiers held at press, `+`-joined, or `none`. Ctrl means the gesture
    /// was navigation and nothing was grabbed — the distinction that took
    /// forensics to establish.
    pub(super) modifiers: String,
    /// Whether "move job" was armed at press. An unarmed press inside the
    /// design is the pan attempt that used to re-place the job.
    pub(super) armed: bool,
    /// Native frame pixels, not screen: comparable across pan/zoom.
    pub(super) start_px: (f64, f64),
    /// Placement at press — `(tx_mm, ty_mm, rot_deg)`, so the stop record can
    /// state what the gesture actually did to the job.
    pub(super) start_place: (f64, f64, f64),
}

pub(super) struct PlacementState {
    pub(super) frame: String,
    pub(super) lbrn2: String,
    pub(super) px_per_mm: f64,
    pub(super) tx_mm: f64,
    pub(super) ty_mm: f64,
    pub(super) rot_deg: f64,
    /// The fiducial-fitted uniform scale about the pivot (`fit_board_pose`).
    /// This RESIZES THE EMITTED JOB — at 1.038 the burn comes out 3.8% larger
    /// than the design — so it travels into the `.lbrn2` via the placement
    /// affine, not just the on-screen overlay. Nominal is 1.0; there is no
    /// manual control, only a reset.
    pub(super) scale: f64,
    /// The placement was set from detected fiducials (see `fit_board_pose`).
    /// `load_place` must not recenter/zero over an auto-fitted pose.
    pub(super) auto_pose: bool,
    pub(super) job: Vec<pcb_core::Poly>,
    pub(super) pivot: (f64, f64),
    pub(super) note: String,
    pub(super) field_correct: bool,
    /// LightBurn device name for the one-click "Etch + run in LightBurn".
    pub(super) lightburn_device: String,
    /// Excellon drill file path(s) for "Emit drill holes" — `;`-separated
    /// (KiCad exports PTH and NPTH holes as two files).
    pub(super) drills: String,
    /// Output `.lbrn2` path for "Emit drill holes" — separate from the etch
    /// output so the two exports never overwrite each other.
    pub(super) drill_lbrn2: String,
    /// An etch click that was REFUSED because the placement sits far from the
    /// fiducial-derived pose, holding the deviation it reported and which
    /// button armed it. The next click on that same button emits; anything
    /// that moves the placement in between re-arms with the new numbers, so a
    /// confirmation can only ever confirm what the operator was shown.
    pub(super) etch_confirm: Option<EtchConfirm>,
}

/// A pending "click again to etch at this offset" confirmation — see
/// [`PlacementState::etch_confirm`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct EtchConfirm {
    /// How far the placement is from the fiducial pose, bed mm / degrees, as
    /// the refused click reported them.
    pub(super) dev_mm: f64,
    pub(super) dev_deg: f64,
    /// `true` when the refused click was "🔥 Etch + Run"; the plain "▶ Etch
    /// here" cannot confirm it, or a mis-click on the other button would start
    /// a burn the operator never armed.
    pub(super) run_after: bool,
}

pub(super) struct ArState {
    pub(super) overlay: bool,
    pub(super) show_board: bool,
    pub(super) show_copper: bool,
    pub(super) show_ablate: bool,
    pub(super) board: Vec<pcb_core::Poly>,
    pub(super) copper: Vec<pcb_core::Poly>,
    pub(super) ablate: Vec<pcb_core::Poly>,
    pub(super) note: String,
}

pub(super) struct ViewState {
    pub(super) images: std::collections::HashMap<&'static str, crate::imgview::ImageView>,
}
