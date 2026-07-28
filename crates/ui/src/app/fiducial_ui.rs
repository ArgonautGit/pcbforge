use std::time::{Duration, Instant};

use super::*;

/// Reject an auto-placement whose fiducial fit residual (RMS over the detected
/// holes) exceeds this — a loose fit means the pairing or the layout is wrong,
/// and silently moving the job would be worse than leaving it.
pub(super) const POSE_MAX_RMS_MM: f64 = 0.5;

/// Band the fiducial fit's uniform scale must land in for the placement to be
/// applied. The fit is a similarity, so it ABSORBS a spacing mismatch instead
/// of leaving it in the residual — which means [`POSE_MAX_RMS_MM`] no longer
/// catches it: a self-consistent misdetection at 1.5× now fits with an RMS of
/// nearly zero and would sail straight through. This band is the only guard
/// left. A few percent is a plausible machine/calibration scale error; ±10% is
/// not a real board, it is the wrong holes or a layout that does not describe
/// this board, and resizing the job by it would be worse than not placing it.
const POSE_SCALE_MIN: f64 = 0.90;
const POSE_SCALE_MAX: f64 = 1.10;

/// Below this, a fitted scale is not worth reporting as a resize — 0.1% on a
/// 40 mm board is 40 µm, inside the detector's own noise.
pub(super) const POSE_SCALE_QUIET: f64 = 0.001;

/// Smallest exit magnification that can act as a face tell.
///
/// On a mirror-symmetric layout the fit's own mirror flag cannot tell the two
/// faces apart (see `fiducial::layout_is_mirror_symmetric`); what remains is
/// that a wrong-face fit carries one whole factor of the hole-exit
/// magnification `m = 1 + thickness/focal`, while a right-face fit lands at 1.0.
/// At the operator's 1.6 mm board and 70 mm lens that is 2.3% — a clear
/// separation. Below half a percent it is no longer distinguishable from a
/// genuine machine-scale error, so the tell is not used and the ambiguity is
/// only reported.
const MIRROR_TELL_MIN_M: f64 = 1.005;

/// How close the fitted scale must sit to the wrong-face signature before the
/// tell is allowed to refuse a placement, in scale units.
///
/// Nearest-signature is necessary but not sufficient. The two signatures are
/// only ~2.3% apart at the reference optics, so "nearer the wrong one" covers
/// everything past a 1.1% boundary — including scales that are neither, which
/// is precisely what a wrong baseline produces. A board that really is on the
/// wrong face lands on its signature to within the fit's own residual (the
/// swapped correspondence reduces to an exact magnification about the
/// centroid), so 0.4% is generous for the case being caught while leaving the
/// ambiguous middle to the warning path.
const MIRROR_TELL_MAX_DEV: f64 = 0.004;

/// Plausibility band for a field-calibration scale used as the mirror tell's
/// baseline — the same band the fitted pose scale itself has to land in. A
/// baseline outside it is not a machine, it is a bad calibration, and a tell
/// built on it would be worse than the `1.0` assumption it replaces.
const MIRROR_BASELINE_MIN: f64 = POSE_SCALE_MIN;
const MIRROR_BASELINE_MAX: f64 = POSE_SCALE_MAX;

/// Bounds for the whole-frame recovery ([`ConsoleApp::locate_fid_markers_globally`]).
///
/// The board sits right-side-up, so its POSE is only a few degrees off — but
/// the gate has to cover perspective as well as pose. The camera looks at the
/// plate off-axis, which stretches and shears the quad, and a single edge of
/// the bench frame reads up to 16° of apparent rotation on a board that is
/// nearly square to the machine. Sized from that, with margin.
const AUTO_MAX_ROT_DEG: f64 = 20.0;
/// Candidate budget handed to the O(n²) matcher — generous on purpose (see
/// `vision::find_fiducial_candidates`: the list is meant to over-admit and let
/// the arrangement match do the rejecting).
const AUTO_MAX_CANDIDATES: usize = 96;
/// Detections below which a Check is a failure worth a whole-frame rescan.
/// Also `fit_board_pose`'s floor, so fewer than this places nothing anyway.
const AUTO_RECOVER_BELOW: usize = 3;

/// How much further Live backs off after a whole-frame scan that found NOTHING
/// than after one that recovered holes. Preserves the 1 s : 4 s ratio the
/// throttle shipped with, now that the base interval is
/// [`FiducialState::live_recover_s`] rather than a constant.
///
/// [`FiducialState::live_recover_s`]: super::state::FiducialState::live_recover_s
pub(super) const RECOVER_BACKOFF_FACTOR: u32 = 4;

/// Clamp on the operator's re-acquire interval, in seconds. The floor is what
/// bounds the damage a very low setting does to the failure backoff (see the
/// stamp sites in `detect_fiducials`) — and `Duration::from_secs_f64` panics on
/// a negative or NaN value, so the same bounds are enforced on load.
pub(super) const LIVE_RECOVER_MIN_S: f64 = 0.1;
pub(super) const LIVE_RECOVER_MAX_S: f64 = 10.0;

/// The wait a stage-3 scan earns: the operator's re-acquire interval after one
/// that recovered holes, [`RECOVER_BACKOFF_FACTOR`]× that after one that found
/// nothing.
///
/// Derived from `live_recover_s` at COMPARE time rather than snapshotted when
/// the scan ran, so turning the dial down takes effect on the next frame — the
/// hover text promises exactly that, and a snapshotted 10 s window would
/// otherwise ignore the change for another 40 s.
///
/// Re-clamped here rather than trusted: the DragValue range and the settings
/// load clamp both hold it, but `from_secs_f64` PANICS on a negative or NaN,
/// and a live console is the wrong place to find that out. NaN is handled ahead
/// of the clamp, which propagates it rather than bounding it.
pub(super) fn recover_window(live_recover_s: f64, recovered: bool) -> Duration {
    let secs = if live_recover_s.is_nan() {
        LIVE_RECOVER_MIN_S
    } else {
        live_recover_s.clamp(LIVE_RECOVER_MIN_S, LIVE_RECOVER_MAX_S)
    };
    let base = Duration::from_secs_f64(secs);
    if recovered {
        base
    } else {
        base * RECOVER_BACKOFF_FACTOR
    }
}

/// Whether the detection ladder should reach for its third stage, the
/// whole-frame rectangle match. Free-standing so the throttle rule is testable
/// without a frame, a camera or a running app.
///
/// An explicit action ignores the cooldown entirely — even while Live runs: it
/// is one deliberate press and the operator is waiting for the answer. Only
/// STREAMED frames are throttled — `last` carries when the scan last ran on the
/// feed and whether it recovered anything — because the feed would otherwise
/// fire the scan on every frame that comes up short.
pub(super) fn should_global_recover(
    streamed: bool,
    hits: usize,
    last: Option<(Instant, bool)>,
    now: Instant,
    live_recover_s: f64,
) -> bool {
    // Enough holes: the earlier stages did the job, nothing to recover.
    if hits >= AUTO_RECOVER_BELOW {
        return false;
    }
    // An explicit action (Check, load, grab, a marker drop) always scans:
    // it is one deliberate press the operator is waiting on — even while the
    // Live feed happens to be running. Only the feed's own frames are
    // throttled, or the scan would fire on every frame that comes up short.
    if !streamed {
        return true;
    }
    // Never run yet — the first short frame under Live scans.
    last.is_none_or(|(t, recovered)| {
        now.duration_since(t) >= recover_window(live_recover_s, recovered)
    })
}

/// How close (screen px) a press has to land to an existing ✛ to grab it for a
/// drag. Sized to the drawn marker — the cross arms reach 9 px and the ring
/// 11 px — plus a little slack, so anything the operator can see as "on the
/// marker" grabs it. Bigger would start swallowing presses meant for the
/// design underneath; smaller would demand pixel-accurate aim at a marker whose
/// whole point is that it is currently in the wrong place. The same radius the
/// click-to-place hit tests use, so one ✛ has one grab area for every gesture.
pub(super) const MARKER_GRAB_PX: f32 = 18.0;

/// The placed design as it appears on this frame: the outline polylines to
/// stroke, the screen-space bounding box that acts as its drag handle, and
/// where the placement pivot landed (the centre a Shift-drag rotates about).
struct PlacedDesign {
    rings: Vec<Vec<egui::Pos2>>,
    bbox: egui::Rect,
    pivot: egui::Pos2,
}

/// Machine-frame rotation (degrees) for sweeping the pointer from `prev` to
/// `curr` about `pivot`, all in SCREEN coordinates.
///
/// `Placement::affine` builds `[cos, −sin; sin, cos]`, so `rot_deg` is
/// counter-clockwise in the y-**up** machine frame. Screen rows grow
/// **downward**, so the view is a mirror of that frame: a sweep that looks
/// counter-clockwise on screen is clockwise on the bed. `atan2` over raw screen
/// coordinates therefore measures angles of the opposite sense, and the machine
/// delta is the NEGATED screen delta — `angle(prev) − angle(curr)`, not the
/// other way round. (The camera projection is not a pure y-flip, but it is
/// orientation-reversing everywhere and locally near-conformal, which is all
/// the sign depends on.)
pub(super) fn rot_delta_deg(pivot: egui::Pos2, prev: egui::Pos2, curr: egui::Pos2) -> f64 {
    let angle = |p: egui::Pos2| ((p.y - pivot.y) as f64).atan2((p.x - pivot.x) as f64);
    wrap_deg((angle(prev) - angle(curr)).to_degrees())
}

/// Fold an angle into (−180, 180] so a sweep across the seam nudges the job
/// instead of spinning it a full turn.
fn wrap_deg(mut deg: f64) -> f64 {
    while deg > 180.0 {
        deg -= 360.0;
    }
    while deg <= -180.0 {
        deg += 360.0;
    }
    deg
}

/// Whether a press should latch a job MOVE, given what it landed on.
///
/// A free function so the rule is testable: the overlay it is called from is a
/// canvas, and canvas gestures are the one part of this console that cannot be
/// driven through the accessibility tree.
///
/// The precedence it encodes, highest first: navigation (Ctrl) grabs nothing; a
/// ✛ under the cursor beats the design's coarse bbox; and the design moves only
/// when the operator armed it.
pub(super) fn design_drag_latches(
    on_design: bool,
    navigating: bool,
    grabbed_marker: bool,
    move_job_armed: bool,
) -> bool {
    on_design && !navigating && !grabbed_marker && move_job_armed
}

/// The modifier keys held right now, `+`-joined, for the drag records — `none`
/// when the gesture is bare. Ctrl/Cmd is the whole difference between a
/// navigation gesture and one that grabs something, so it has to be on the
/// record rather than inferred from what the gesture went on to do.
fn modifier_token(ui: &egui::Ui) -> String {
    let m = ui.input(|i| i.modifiers);
    let held: Vec<&str> = [
        (m.ctrl, "ctrl"),
        (m.command, "cmd"),
        (m.shift, "shift"),
        (m.alt, "alt"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect();
    if held.is_empty() {
        "none".into()
    } else {
        held.join("+")
    }
}

impl ConsoleApp {
    /// Load the fiducial frame into memory + a texture and seed the search
    /// markers from the design layout (so they start near nominal, ready to
    /// click onto the real holes).
    pub fn load_fid_frame(&mut self, ctx: &Context) {
        let img = match image::open(crate::clean_path(&self.fiducials.frame)) {
            Ok(i) => i.to_luma8(),
            Err(e) => {
                self.fiducials.note = format!("frame: {e}");
                return;
            }
        };
        if !self.set_fid_frame(ctx, img) {
            return;
        }
        // With a calibration the markers can be put on the holes outright, so
        // check immediately. Without one a file load has no way to guess where
        // the holes image, so open the click-in-order marking round: the
        // operator marks each fiducial, and the final click runs the check.
        match self.seed_fid_markers_from_projection() {
            Ok(()) => {
                self.fiducials.marking = None;
                self.detect_fiducials(false);
            }
            Err(e) => {
                self.start_fid_marking();
                // After the round's prompt, which rewrites the note.
                self.fiducials.note.push_str(&format!("  ·  {e}"));
            }
        }
    }

    /// Grab one frame from the camera (the source picked in the Camera tab —
    /// device or file), install it as the fiducial-check frame, and detect
    /// immediately. The one-click camera path for the fiducial check; ● Live
    /// does the same continuously.
    pub fn grab_fid_frame(&mut self, ctx: &Context) {
        match self.grab_shared() {
            Ok(img) => {
                let img = self.camera.orientation.apply(img);
                self.set_fid_frame(ctx, img);
                // Put the markers on the holes through the calibration when
                // there is one; without it, detect at the raw layout seeds as
                // this path always has.
                let fallback = self.seed_fid_markers_from_projection().err();
                if fallback.is_none() {
                    self.fiducials.marking = None;
                }
                self.detect_fiducials(false);
                if let Some(e) = fallback {
                    // Appended after detection, which rewrites the note.
                    self.fiducials.note.push_str(&format!("  ·  {e}"));
                }
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
        // A new frame is a new scene: whatever the arm was for, it was for the
        // photo that just went away. (The Live path deliberately does NOT
        // disarm — it replaces the frame every tick, and doing so there would
        // make the arm unusable under a live feed.)
        self.fiducials.move_job = false;
        self.fiducials.note =
            "click each ✛ onto its hole in layout order — the last click checks (or 🎯 Check as-is)"
                .into();
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
        self.fiducials.note = format!("click fiducial 1 of {n} (layout order)");
    }

    /// Reseed the ✛ set from the layout (the current frame stays) and reopen the
    /// click-in-order marking round — the ↺ reset-markers action.
    pub(super) fn reset_fid_markers(&mut self) {
        // Deliberately does NOT rebuild a ✕-cleared layout: clear means gone,
        // or an overgrown click-placed set has no one-step exit again. ⟳ layout
        // from W×H is the explicit way back.
        self.fiducials.search.clear();
        self.fiducials.found.clear();
        self.sync_fid_markers();
        self.start_fid_marking();
    }

    /// Remove EVERY expected fiducial — the ✕ clear-markers action. The layout
    /// string is the source of truth (`sync_fid_markers` reseeds the ✛ set from
    /// it every frame), so it must empty too or the markers come straight back.
    /// The placement/pose is left alone: clearing markers must not move the job.
    pub(super) fn clear_fid_markers(&mut self) {
        self.fiducials.layout.clear();
        self.fiducials.search.clear();
        self.fiducials.found.clear();
        self.fiducials.rows.clear();
        self.fiducials.marking = None;
        self.fiducials.measured_ppm = None;
        self.fiducials.homography = None;
        self.fiducials.last_placed = false;
        // The layout is gone, so its centroid — the origin the carried
        // placement offset is measured from — means nothing any more.
        self.fiducials.last_fit = None;
        // Stale measurements must not be adoptable as a layout they no longer
        // correspond to.
        self.fiducials.detected_mm.clear();
        self.fiducials.note = "markers cleared — ⟳ layout from W×H rebuilds the four corners, or \
             ✚ click-to-place new ones"
            .into();
    }

    /// Apply one placement click: drop the next search marker (layout order) at
    /// bed `mm`, clear its now-stale detection, and advance. With no round
    /// active (`marking == None`) the click implicitly opens one at marker 0.
    /// The final marker's click closes the round and runs detection (which
    /// auto-updates the placement). Factored out of the canvas handler so tests
    /// can drive it directly.
    pub(super) fn fid_mark_click(&mut self, mm: (f64, f64)) {
        // No active round: a plain click implicitly opens one at marker 0.
        let k = self.fiducials.marking.unwrap_or(0);
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
            self.detect_fiducials(false); // final click → detect + auto-place
        } else {
            self.fiducials.marking = Some(next);
            self.fiducials.note = format!("click fiducial {} of {n}", next + 1);
        }
    }

    /// Whether a canvas click may mark, add or remove a fiducial right now —
    /// not while a drag has grabbed the design or one of the ✛s. Both
    /// latches are read through this so the marking, click-to-place and
    /// right-click-remove paths cannot drift apart.
    pub(super) fn fid_marking_allowed(&self) -> bool {
        !self.fiducials.design_drag && self.fiducials.marker_drag.is_none()
    }

    /// Nudge search marker `i` by `(dx, dy)` mm — one frame of a marker drag.
    ///
    /// Moves ONLY `fiducials.search`. The layout is deliberately untouched:
    /// it is the DESIGN nominal that `fit_board_pose` fits against and that
    /// `scale_from_design` measures the true px/mm from, so writing a dragged
    /// position into it would turn a 1 mm correction over a 50 mm baseline into
    /// a ~2% scale error in everything downstream (LR-17). A search marker says
    /// "look here"; the layout says "this is where the hole is by design", and
    /// dragging must only ever change the former.
    pub(super) fn fid_drag_marker(&mut self, i: usize, (dx, dy): (f64, f64)) {
        // The layout can shrink mid-drag (a typed edit re-runs `sync_fid_markers`
        // every frame), and the latched index outlives the marker it named —
        // same guard, same reason, as `fid_mark_click`'s.
        let Some(m) = self.fiducials.search.get_mut(i) else {
            self.fiducials.marker_drag = None;
            return;
        };
        *m = (m.0 + dx, m.1 + dy);
        if let Some(f) = self.fiducials.found.get_mut(i) {
            *f = None; // this marker's old detection is stale now
        }
    }

    /// End a marker drag: drop the latch and re-check, so the operator sees at
    /// once whether the nudged marker locks onto its hole.
    ///
    /// Once, on RELEASE — never per frame. The detection ladder's third stage is
    /// a whole-frame candidate scan, far too heavy to run while the pointer
    /// moves. The ladder tries the operator's markers FIRST and only reaches for
    /// the projection seed or the rectangle match when they come up short of
    /// [`AUTO_RECOVER_BELOW`] hits, so a manual placement that finds its holes
    /// is kept rather than seeded over.
    pub(super) fn fid_marker_drag_release(&mut self) {
        if self.fiducials.marker_drag.take().is_some() {
            self.detect_fiducials(false);
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
        // A new point moves the layout centroid, which is the origin the
        // carried placement offset is measured from — keeping the old fit would
        // displace the design by the centroid shift on the next Check.
        self.fiducials.last_fit = None;
        // Stale measurements must not be adoptable as a layout they no longer
        // correspond to.
        self.fiducials.detected_mm.clear();
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
        // Dropping a point moves the layout centroid — same reason
        // `add_expected_fiducial` drops the fit: the carried offset is measured
        // from that centroid.
        self.fiducials.last_fit = None;
        // Stale measurements must not be adoptable as a layout they no longer
        // correspond to.
        self.fiducials.detected_mm.clear();
        // Lengths already match; sync is a no-op reconcile (and re-seeds only if
        // the layout still parses).
        self.sync_fid_markers();
        self.fiducials.note = format!("removed fiducial #{i}  ·  {} left", kept.len());
    }

    /// What a RIGHT-face fiducial fit is expected to land at on this machine —
    /// the baseline both wrong-face signatures are measured from.
    ///
    /// `1.0` is only correct on a machine whose field is the size it claims.
    /// The ③ laser-field calibration measures exactly that discrepancy
    /// (`FieldCal::scale`: burned size / commanded size), and the fiducial holes
    /// were drilled by this machine — so when the error is left in the geometry,
    /// the holes are that much oversized and every honest fit lands there, not
    /// at 1.0.
    ///
    /// Which makes the scale mode decisive rather than incidental:
    /// [`Compensate`](calib::FieldScale::Compensate) pre-divides the error out
    /// of what is emitted, so those holes burn true and the baseline goes back
    /// to 1.0. `Refuse` and `DistortionOnly` both leave it in.
    ///
    /// Acceptance is deliberately NOT required. The scale error is a physical
    /// property of the machine that the holes carry whether or not the operator
    /// accepted the fit that measured it; gating on acceptance would restore
    /// the 1.0 assumption for exactly the operator who has already seen the
    /// warning and kept working.
    pub(super) fn mirror_scale_baseline(&self) -> f64 {
        let Some(field) = self.calibration.field.as_ref() else {
            return 1.0;
        };
        if self.calibration.field_scale_used == calib::FieldScale::Compensate {
            return 1.0;
        }
        if !field.scale.is_finite()
            || !(MIRROR_BASELINE_MIN..=MIRROR_BASELINE_MAX).contains(&field.scale)
        {
            return 1.0;
        }
        field.scale
    }

    /// Where the last applied Check WOULD put the design with no manual offset
    /// carried — `(tx_mm, ty_mm, rot_deg, scale)` in bed mm. This is the
    /// "fiducial pose": the reference every manual nudge is measured against.
    ///
    /// `None` when there is no fit to measure against (no applied Check yet, a
    /// side switch, a layout edit) or the layout no longer parses.
    pub(super) fn fiducial_pose(&self) -> Option<(f64, f64, f64, f64)> {
        let fit = self.fiducials.last_fit?;
        let layout = fiducial::parse_layout(&self.fiducials.layout).ok()?;
        if layout.is_empty() {
            return None;
        }
        let n = layout.len() as f64;
        let b0 = (
            layout.iter().map(|p| p.0).sum::<f64>() / n,
            layout.iter().map(|p| p.1).sum::<f64>() / n,
        );
        let (tx, ty) = fit.apply(b0);
        Some((tx, ty, fit.angle_deg(), fit.scale))
    }

    /// How far the CURRENT placement sits from the fiducial pose:
    /// `(translation mm, rotation degrees)`, the rotation wrapped to ±180 so a
    /// nudge past the wrap point still reads as a small number.
    ///
    /// `None` means there is no fiducial reference at all — a purely manual
    /// placement, which is a different statement from "zero offset" and is
    /// reported as such rather than as 0.00 mm.
    pub(super) fn placement_deviation(&self) -> Option<(f64, f64)> {
        let (tx, ty, rot, _) = self.fiducial_pose()?;
        Some((
            (self.placement.tx_mm - tx).hypot(self.placement.ty_mm - ty),
            wrap_deg(self.placement.rot_deg - rot),
        ))
    }

    /// Drop the carried manual offset: put the design back exactly where a
    /// fresh, un-nudged Check would put it — on the fiducial-layout centroid,
    /// at the fitted rotation and scale.
    ///
    /// The offset carry ([`update_placement_from_fiducials`]) deliberately
    /// preserves a drag across re-Checks, which is right when the nudge was
    /// deliberate and a trap when it was not: an accidental drag becomes
    /// permanent, because nothing else clears it. `⤵ Load design` re-centres
    /// only while `auto_pose` is false, and a lock sets it true — so without
    /// this the only escapes were switching side or restarting the console.
    ///
    /// Zeroing the offset is exactly reproducing the `offset = (0,0)` branch of
    /// the apply, so it is written that way rather than by clearing `last_fit`
    /// and asking the operator to Check again.
    ///
    /// [`update_placement_from_fiducials`]: Self::update_placement_from_fiducials
    pub(super) fn recentre_on_fiducials(&mut self) {
        if self.fiducials.last_fit.is_none() {
            self.fiducials.note =
                "nothing to recentre against — check fiducials first, so there is a fit to \
                 centre the design on"
                    .into();
            return;
        }
        let Some((tx, ty, rot, scale)) = self.fiducial_pose() else {
            self.fiducials.note = "recentre needs a valid layout".into();
            return;
        };
        let (moved, turned) = self.placement_deviation().unwrap_or((0.0, 0.0));
        self.placement.tx_mm = tx;
        self.placement.ty_mm = ty;
        self.placement.rot_deg = rot;
        self.placement.scale = scale;
        // Still an auto pose — this IS the fitted placement, so a later Load
        // must not recentre over it either.
        self.placement.auto_pose = true;
        // The offset is gone, so a confirmation armed against it describes a
        // placement that no longer exists — and the next etch click is once
        // again an ordinary one.
        self.placement.etch_confirm = None;
        // Undoing an accidental drag must not leave the tab ready to make
        // another one.
        self.fiducials.move_job = false;
        self.fiducials.note = format!(
            "recentred on the fiducials — dropped a {moved:.2} mm / {turned:+.2}° manual offset; \
             design is back at the layout centroid (rot {:+.2}°, scale {:.4})",
            self.placement.rot_deg, self.placement.scale
        );
    }

    /// Replace the layout with the positions the last Check actually measured,
    /// making the CURRENT board pose the nominal one.
    ///
    /// This is the way out of a layout that never described the real holes.
    /// `fit_board_pose` fits layout → measured as a rigid transform, so any
    /// error in the layout's SHAPE (a hand-clicked quadrilateral standing in
    /// for a rectangle) or in the camera calibration's scale/skew is
    /// irreducible: no rotation and translation can absorb it, it lands in the
    /// RMS, and the gate refuses — correctly, since a job placed from that fit
    /// would burn in the wrong place.
    ///
    /// Adopting the measurement makes that residual zero by construction. From
    /// then on the fit measures only what changed SINCE — which is exactly the
    /// operator-placement error this whole path exists to compensate for. Teach
    /// it once with the board where you want it; every later Check moves the job
    /// by however far the board has drifted.
    ///
    /// Deliberately a button, never automatic: silently redefining the nominal
    /// would turn a genuine misdetection into the new truth.
    pub(super) fn adopt_measured_layout(&mut self) {
        // Back-side detections are compared against MIRRORED, beam-offset
        // expected positions (`expected_points`), so writing them into the
        // layout — which is the un-mirrored design frame — would bake the flip
        // in twice. Front only until that inverse is worked out.
        if self.job.side == Side::Back {
            self.fiducials.note =
                "⌖ layout from detection is front-side only (the back's expected \
                 positions are mirrored and beam-offset)"
                    .into();
            return;
        }
        let measured = &self.fiducials.detected_mm;
        if measured.is_empty() || measured.iter().any(Option::is_none) {
            self.fiducials.note = format!(
                "⌖ layout from detection needs EVERY fiducial found — have {}/{}. \
                 Check again first.",
                measured.iter().filter(|m| m.is_some()).count(),
                measured.len().max(self.fiducials.search.len())
            );
            return;
        }
        let pts: Vec<(f64, f64)> = measured.iter().flatten().copied().collect();
        self.fiducials.layout = fiducial::format_layout(&pts);
        // The nominal frame just moved, so anything measured against the old one
        // is void: the carried offset's origin (the layout centroid) has changed,
        // and the cached pose describes a fit that no longer applies.
        self.fiducials.last_fit = None;
        // Stale measurements must not be adoptable as a layout they no longer
        // correspond to.
        self.fiducials.detected_mm.clear();
        self.fiducials.pose = None;
        self.fiducials.last_placed = false;
        // The scan's Live backoff was earned against the OLD layout, which is
        // exactly the arrangement that was not matching. Adopting a new one
        // changes what stage 3 looks for, so let the next frame try it.
        self.fiducials.last_global_recover = None;
        self.sync_fid_markers();
        self.fiducials.note = format!(
            "layout set from the measured holes ({} points) — this board pose is now \
             nominal. Check again: the fit should be near-identity, and from here a \
             Check measures only how far the board has moved.",
            pts.len()
        );
    }

    /// The layout string for the current fiducial rectangle, centred in the
    /// work area. Resolves the effective field centre first, so the Camera
    /// tab's auto-centre toggle is honoured.
    pub(super) fn fid_rect_layout(&mut self) -> String {
        self.sync_auto_field_center();
        crate::fiducial::format_layout(&crate::fiducial::centered_fid_layout(
            self.camera.field_cx_mm as f64,
            self.camera.field_cy_mm as f64,
            self.fiducials.rect_w_mm,
            self.fiducials.rect_h_mm,
        ))
    }

    /// Rewrite the layout from the rectangle W/H — the response to an edit of
    /// either span. The ✛ set is cleared before the resync so EVERY marker
    /// reseeds at the new corners (`sync_fid_markers` only seeds ones it adds,
    /// which would otherwise leave the crosses sitting at the old rectangle),
    /// and the stale detections/rows go with them.
    pub(super) fn apply_fid_rect(&mut self) {
        self.fiducials.layout = self.fid_rect_layout();
        self.fiducials.search.clear();
        self.fiducials.found.clear();
        self.fiducials.rows.clear();
        // Everything fitted against the OLD corner positions goes stale with
        // them — a surviving homography would keep skewing the Place overlay
        // from a rectangle that no longer exists.
        self.fiducials.measured_ppm = None;
        self.fiducials.homography = None;
        // The rectangle moved the layout centroid, so an offset measured from
        // the old one would displace the design by the difference.
        self.fiducials.last_fit = None;
        // Stale measurements must not be adoptable as a layout they no longer
        // correspond to.
        self.fiducials.detected_mm.clear();
        // Click-placed extras may have made the layout longer than four, so an
        // in-flight marking index can outlive the marker it named.
        self.fiducials.marking = None;
        // A resized rectangle is a different arrangement for stage 3 to match,
        // so the backoff the old one earned should not hold off the first scan
        // against the new one.
        self.fiducials.last_global_recover = None;
        self.sync_fid_markers();
    }

    /// Resize the marker set to match the design layout, preserving existing
    /// (clicked) positions and seeding any new ones from the layout — so adding
    /// a 4th coordinate makes a 4th ✛ appear without a manual reset.
    ///
    /// Deliberately does NOT reseed through the calibrated projection
    /// ([`seed_fid_markers_from_projection`]): this runs every frame from the
    /// overlay, so projection-seeding here would drag the operator's markers
    /// back off their holes as fast as they could place them. Auto-seeding
    /// belongs to the explicit load / grab / Check actions only.
    ///
    /// [`seed_fid_markers_from_projection`]: Self::seed_fid_markers_from_projection
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

    /// Seed the ✛ set at the layout's PROJECTED pixels instead of its raw mm.
    ///
    /// This tab works in the uniform seeded-px/mm frame, which is only ever as
    /// good as the typed px/mm guess — so the crosses land near the holes only
    /// by luck, and the operator has to click each one onto its hole before the
    /// local search can find anything. The calibrated projection already knows
    /// where a machine-mm point images, and the holes were burned at known
    /// machine coordinates, so pushing the expected positions through it puts
    /// every marker inside its own search window and removes the click round.
    ///
    /// Returns `Err` with the reason whenever there is no usable projection (or
    /// it cannot map some point), so the caller can fall back to clicking and
    /// say why.
    fn seed_fid_markers_from_projection(&mut self) -> Result<(), String> {
        let (w, h) = self
            .fiducials
            .frame_img
            .as_ref()
            .map(|f| f.dimensions())
            .ok_or("no frame to seed against")?;
        let projection = self.place_projection(w, h)?;
        let expected = self.expected_points();
        if expected.is_empty() {
            return Err("no expected fiducials to seed".into());
        }
        let ppm = self.fiducials.px_per_mm;
        if ppm <= 0.0 {
            return Err(format!("seed scale {ppm} px/mm is not positive"));
        }
        // Projected px → the tab's uniform frame (x = px/ppm, y measured up
        // from the bottom row), which is what `check_frame` searches in.
        let mut seeds = Vec::with_capacity(expected.len());
        for (i, &p) in expected.iter().enumerate() {
            let (px, py) = projection.to_px(p).ok_or_else(|| {
                format!(
                    "fiducial {} at ({:.1}, {:.1}) mm does not project into this frame",
                    i + 1,
                    p.0,
                    p.1
                )
            })?;
            seeds.push((px / ppm, (f64::from(h) - py) / ppm));
        }
        self.fiducials.search = seeds;
        self.fiducials
            .found
            .resize(self.fiducials.search.len(), None);
        Ok(())
    }

    /// Locate the fiducials by their RECTANGLE GEOMETRY anywhere in the frame
    /// and move the ✛ set onto them — the recovery for when the markers are not
    /// near the real holes at all (bad calibration, board moved, no projection),
    /// so every local search misses and the operator gets nothing back.
    ///
    /// Whole-frame blob scan first, then the layout's own arrangement picks the
    /// true four out of the blobs. Unmatched layout points keep their existing
    /// seed rather than being dragged somewhere arbitrary. Returns the operator
    /// summary — candidate count, how many matched, and the fit RMS — which is
    /// the primary bench diagnostic when this comes up empty.
    fn locate_fid_markers_globally(&mut self) -> Result<String, String> {
        let ppm = self.fiducials.px_per_mm;
        if ppm <= 0.0 {
            return Err(format!(
                "rectangle match: scale {ppm} px/mm is not positive"
            ));
        }
        let layout = fiducial::parse_layout(&self.fiducials.layout)
            .map_err(|e| format!("rectangle match: layout: {e}"))?;
        if layout.len() < AUTO_RECOVER_BELOW {
            return Err(format!(
                "rectangle match: need ≥{AUTO_RECOVER_BELOW} layout points to match a shape, \
                 have {}",
                layout.len()
            ));
        }
        // Match against what THIS FACE actually shows, not the raw design
        // layout. On the back the holes are physically mirrored (and
        // exit-magnified), while the matcher enumerates proper similarities
        // only, within ±`AUTO_MAX_ROT_DEG` — so the raw layout can never match
        // an asymmetric back-face arrangement (recovery dead exactly when the
        // board has just been flipped), and matches a symmetric one with the
        // correspondence swapped. `expected_points` is the same source of truth
        // the projection seeding uses and stays index-aligned with the layout,
        // so `matched_px` still indexes layout order downstream.
        let expected = self.expected_points();
        let profile = self.fiducials.profile.to_profile(
            self.fiducials
                .shape
                .to_fid_shape(self.fiducials.diameter_mm, self.fiducials.height_mm),
        );
        let frame = self
            .fiducials
            .frame_img
            .as_ref()
            .ok_or("rectangle match: no frame")?;
        let h = f64::from(frame.height());
        let cands = vision::find_fiducial_candidates(frame, &profile, ppm, AUTO_MAX_CANDIDATES);
        let n_cand = cands.len();
        let pts: Vec<(f64, f64)> = cands.iter().map(|c| (c.px.x, c.px.y)).collect();
        let m = fiducial::match_layout_to_candidates(
            &expected,
            h,
            ppm,
            &pts,
            AUTO_MAX_ROT_DEG,
            fiducial::match_tol_px(&expected, ppm),
        )
        .ok_or_else(|| format!("rectangle match: {n_cand} candidates, none form the layout"))?;

        // Back into the tab's uniform frame — the exact inverse of the
        // projection seeding's `(px / ppm, (h − py) / ppm)`, so the two paths
        // write the same coordinates.
        self.sync_fid_markers();
        for (slot, found) in self.fiducials.search.iter_mut().zip(&m.matched_px) {
            if let &Some((px, py)) = found {
                *slot = (px / ppm, (h - py) / ppm);
            }
        }
        Ok(format!(
            "{n_cand} candidates, {}/{}, {:.1} px RMS",
            m.matched,
            layout.len(),
            m.rms_px
        ))
    }

    /// Detect around the current search markers and record the found positions,
    /// summary rows, and measured scale.
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
            // The load finished the job: it either seeded through the
            // calibration and checked, or opened the click round whose last
            // click checks. Falling through would detect a second time and
            // bury the round's "click fiducial 1 of N" prompt.
            return;
        }
        if self.fiducials.search.is_empty() {
            self.fiducials.note = "load a frame first".into();
            return;
        }
        // Detection owns the choice of where to look. It tries the operator's
        // markers first, then the calibrated projection, then the whole-frame
        // rectangle match, and keeps the first that works.
        //
        // Deliberately does NOT re-seed through the calibration up front any
        // more. That threw away markers the operator had already clicked onto
        // the holes, and a layout whose coordinates are themselves click-derived
        // (rather than true machine coordinates) does not project back onto
        // them — so a Check that used to find all four found none.
        self.detect_fiducials(false);
    }

    /// Live fiducial tracking: pull frames from the (camera-tab) source and
    /// re-detect each one, so the rings track the holes as the board moves.
    /// Uses `cam_source`, so pick the device/file in the Camera tab.
    pub(super) fn pump_fid_live(&mut self, ctx: &Context) {
        // Live-off only means this tab stops asking for frames — the capture is
        // the console's, shared with the Camera and Calibrate tabs, and is
        // released by the idle rule in `ui()` once no tab wants it.
        if !self.fiducials.live {
            return;
        }
        if let Some(res) = self.capture_latest() {
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
                        self.detect_fiducials(true);
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
    /// fid-holes`, then LOAD the result in LightBurn without starting it: once
    /// the export finishes the queued load-only hand-off FORCELOADs the file
    /// (see [`chain_lightburn_after_verb`](Self::chain_lightburn_after_verb))
    /// and stops there — START stays with the operator, the same contract the
    /// drill emit holds. Uses the same layout string the check drives from, so
    /// the holes land exactly where detection looks for them, and the same
    /// process recipe the drill emit uses (a Line layer at the Job-tab params)
    /// — these are drilled holes, not engraved ones.
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
        // The drill recipe, matching `emit_drill_at_placement`: a Line layer
        // (trace the hole outline, don't scan it in) at the Job-tab process
        // params over the verb's default power.
        let mut args: Vec<String> = vec![
            "fid-holes".into(),
            "--out".into(),
            out.clone(),
            "--layout".into(),
            self.fiducials.layout.clone(),
            "--shape".into(),
            self.fiducials.shape.token().into(),
            "--w-mm".into(),
            format!("{}", self.fiducials.diameter_mm),
            "--h-mm".into(),
            format!("{h_mm}"),
            "--mode".into(),
            "line".into(),
            "--power-pct".into(),
            "20".into(),
            "--speed-mm-s".into(),
            format!("{}", self.job.speed_mm_s),
            "--frequency-khz".into(),
            format!("{}", self.job.frequency_khz),
            "--pulse-ns".into(),
            format!("{}", self.job.pulse_ns),
            "--passes".into(),
            format!("{}", self.job.passes),
            "--interval-mm".into(),
            format!("{}", self.job.interval_mm),
        ];
        // Pre-distort with the laser-field map when a usable calibration + map
        // file exist; otherwise burn uncorrected with a warning (mirrors the
        // job emit path).
        let field_path = self.field_map_path();
        let use_field = self.has_usable_field_cal() && field_path.exists();
        if use_field {
            args.push("--field-map".into());
            args.push(field_path.to_string_lossy().into_owned());
        } else {
            self.runtime.log.push(LogLine {
                text: "fid-holes: no accepted step 1 (Camera lens) + step 3 (Laser field) \
                     calibration — holes will burn without lens correction (accept a laser field \
                     fit first)"
                    .into(),
                err: true,
            });
        }
        // Record 4a. The holes are generated from the layout, not the
        // placement, but the placement snapshot still goes in: "the holes were
        // cut for THIS layout while the design sat there" is the pairing a
        // later registration failure has to be read against.
        let out_path = PathBuf::from(&out);
        self.diag_export("fid-holes", &args, &out_path, use_field);
        let started = self.run_verb(&args);
        if started {
            self.diag_arm_readback("fid-holes", out_path, use_field);
        }
        // Queue a LOAD-ONLY hand-off — the file opens in LightBurn once the
        // export finishes, and START is never sent. Only when the export
        // actually launched: a refused click (a job already running) must not
        // arm the chain against a file this click never wrote. Absolute but
        // NOT canonicalized: the file doesn't exist yet, and \\?\ prefixes
        // upset LightBurn's FORCELOAD (same rule as the placement export).
        if started {
            match std::path::absolute(std::path::Path::new(&out)) {
                Ok(abs) => {
                    self.runtime.pending_lightburn = Some(PendingLightburn {
                        path: abs,
                        start: false,
                    });
                    self.runtime.log.push(LogLine {
                        text: "queued: load the holes in LightBurn once the export finishes \
                               — NOT starting it (press ▶ there to burn)"
                            .into(),
                        err: false,
                    });
                    self.fiducials.note = format!(
                        "writing fiducial holes to {out} — see Log for progress. It loads in \
                         LightBurn but never starts: press play there yourself."
                    );
                }
                // Nothing queued, so don't promise a load the operator won't get.
                Err(e) => {
                    self.runtime.log.push(LogLine {
                        text: format!(
                            "fid-holes: couldn't resolve an absolute path for the LightBurn load \
                             ({e}) — the holes file will still be written"
                        ),
                        err: true,
                    });
                    self.fiducials.note = format!(
                        "writing fiducial holes to {out} — see Log for progress; open the file \
                         in LightBurn yourself."
                    );
                }
            }
        }
    }

    /// How many fiducials a detection result actually located.
    fn fid_hits(r: &fiducial::FidResult) -> usize {
        r.found_px.iter().filter(|f| f.is_some()).count()
    }

    /// One detection attempt at `seeds`, including the local refine pass.
    /// Read-only: the caller decides whether this attempt's seeds are the ones
    /// worth keeping (see [`detect_fiducials`](Self::detect_fiducials)).
    ///
    /// The refine pass: a small uniform placement error (a board nudged between
    /// the burn and the check, or a slightly-off px/mm) moves EVERY hole the
    /// same way, so the fiducials that were found say where the missed ones
    /// went — shift the misses by the mean hit displacement and look again.
    /// Needs ≥2 hits so one bad detection can't drag the misses off on its own,
    /// and runs at most once; the second pass' result is final whatever it says.
    ///
    /// Only the MISSED seeds move, so the hits search unchanged windows and
    /// cannot regress. The refined seeds stay local: they are a better guess at
    /// the holes, not the operator's marker set, and writing them back would
    /// make the ✛ walk on every Check (and every frame under Live). One
    /// consequence — a refined row's `off` reads detector-vs-refined-seed,
    /// which is the meaningful number for the retry.
    fn detect_pass(
        &self,
        seeds: &[(f64, f64)],
        profile: &vision::FiducialProfile,
        ppm: f64,
        search_mm: f64,
    ) -> fiducial::FidResult {
        let frame = self
            .fiducials
            .frame_img
            .as_ref()
            .expect("caller checked a frame is loaded");
        let h = f64::from(frame.height());
        let r = fiducial::check_frame(frame, seeds, ppm, profile, search_mm);

        let hits = Self::fid_hits(&r);
        if hits < 2 || hits >= seeds.len() {
            return r;
        }
        let (mut dx, mut dy) = (0.0, 0.0);
        for (seed, found) in seeds.iter().zip(&r.found_px) {
            if let &Some((px, py)) = found {
                dx += px / ppm - seed.0;
                dy += (h - py) / ppm - seed.1;
            }
        }
        let (dx, dy) = (dx / hits as f64, dy / hits as f64);
        let refined: Vec<(f64, f64)> = seeds
            .iter()
            .zip(&r.found_px)
            .map(|(&s, found)| {
                if found.is_none() {
                    (s.0 + dx, s.1 + dy)
                } else {
                    s
                }
            })
            .collect();
        fiducial::check_frame(frame, &refined, ppm, profile, search_mm)
    }

    /// Run detection on the current in-memory frame around the search markers,
    /// updating rows/found/measured/homography. Shared by the static Check and
    /// the live-tracking loop (FLD-11).
    ///
    /// `streamed` is true only from the Live pump. The `fiducials.live` flag
    /// cannot stand in for it: a Check pressed WHILE Live is on is still one
    /// deliberate press the operator is waiting on, and throttling it (or
    /// letting its failure suppress the feed's next scans) reads as a button
    /// that sometimes does nothing. Only the feed's own frames are on the
    /// feed's budget. `pub(super)` so the throttle test can drive a streamed
    /// frame directly — every non-test caller is in this file.
    pub(super) fn detect_fiducials(&mut self, streamed: bool) {
        if self.fiducials.frame_img.is_none() {
            return;
        }
        let profile = self.fiducials.profile.to_profile(
            self.fiducials
                .shape
                .to_fid_shape(self.fiducials.diameter_mm, self.fiducials.height_mm),
        );
        let ppm = self.fiducials.px_per_mm;
        let search_mm = self.fiducials.search_mm;

        // TRY IN ORDER, KEEP THE FIRST THAT WORKS. Each source of marker
        // positions is attempted at most once, best result wins, and the
        // operator's own markers are the FIRST thing tried — re-seeding
        // unconditionally breaks the flow where the operator has already
        // clicked each ✛ onto its hole, because a layout whose coordinates were
        // themselves click-derived does not project onto the holes.
        let original = self.fiducials.search.clone();
        let mut best = self.detect_pass(&original, &profile, ppm, search_mm);
        let mut best_hits = Self::fid_hits(&best);
        let mut best_seeds = original.clone();
        let mut via = "markers".to_string();
        // Why a stage was skipped or came up short — appended after the tally.
        let mut why: Vec<String> = Vec::new();

        // Stage 2: the calibrated projection knows where a machine-mm point
        // images, which beats a stale marker whenever the layout really is in
        // machine coordinates.
        if best_hits < AUTO_RECOVER_BELOW {
            match self.seed_fid_markers_from_projection() {
                Ok(()) => {
                    let seeds = self.fiducials.search.clone();
                    let r = self.detect_pass(&seeds, &profile, ppm, search_mm);
                    let hits = Self::fid_hits(&r);
                    if hits > best_hits {
                        (best, best_hits, best_seeds) = (r, hits, seeds);
                        via = "projection seed".into();
                    }
                }
                Err(e) => why.push(e),
            }
        }

        // Stage 3: give up on knowing where the holes are and find them by
        // their own geometry — a whole-frame blob scan matched against the
        // layout's ARRANGEMENT. Written as a third inline attempt (like the
        // refine pass inside each attempt) rather than by re-entering this
        // method, so it cannot recurse.
        //
        // THROTTLED under Live, not skipped. The operator needs a moved board
        // re-acquired without touching the console, but the scan runs on the UI
        // thread and costs one visible hitch — ~180 ms in release, 1.3–4.5 s in
        // a dev build — so it fires at most once per cooldown rather than on
        // every short frame. Not threaded on purpose: the frame, the ladder's
        // state and egui's context would all have to cross the boundary, which
        // is out of proportion to a sub-200 ms hitch at the operator's cadence.
        let now = Instant::now();
        if should_global_recover(
            streamed,
            best_hits,
            self.fiducials.last_global_recover,
            now,
            self.fiducials.live_recover_s,
        ) {
            // The scan measures 171–190 ms in release on the 2592×1944 bench
            // frames, against a ~200 ms live iteration (device ~8.7 fps), so at
            // the 0.5 s default it costs about a third of live time while it is
            // re-acquiring — the price of following a board that just moved.
            // Running it per short frame instead would swamp the feed, which is
            // why it used to be skipped under Live outright.
            //
            // A fruitless scan earns `RECOVER_BACKOFF_FACTOR`× that wait
            // instead. A hopeless scene — board removed, lens cap on, wrong
            // layout — would otherwise burn 180 ms every interval forever for a
            // result that cannot change. That backoff is also what keeps a DEV
            // build alive, where the same scan costs 1.3–4.5 s: one scan per few
            // seconds is a painful feed but still a moving one, whereas
            // per-frame would wedge the console. (No build-profile switch — one
            // rule, tuned for the release console the operator actually runs.) A
            // very low interval weakens that protection, and LIVE_RECOVER_MIN_S
            // is what bounds it: 0.4 s of backoff at the floor.
            //
            // Stamp on ATTEMPT, before the outcome is known — including the
            // cheap Err exits inside `locate_fid_markers_globally` (bad scale,
            // too few layout points, no frame) that never reach the scan. Those
            // will not change frame to frame either, so backing off on them is
            // right; stamping only on success would leave a hopeless scene
            // scanning every frame, which is exactly what this guards against.
            //
            // ONLY the feed's own frames stamp. A manual Check runs the scan
            // regardless of the cooldown, and if it also stamped, one press
            // would silence the feed's next scans for up to the full backoff —
            // the opposite of what pressing Check means.
            if streamed {
                self.fiducials.last_global_recover = Some((now, false));
            }
            // Unmatched layout points keep THEIR seed, so start from the
            // operator's markers rather than stage 2's projection guesses.
            self.fiducials.search = original.clone();
            match self.locate_fid_markers_globally() {
                Ok(summary) => {
                    let seeds = self.fiducials.search.clone();
                    let r = self.detect_pass(&seeds, &profile, ppm, search_mm);
                    let hits = Self::fid_hits(&r);
                    if hits > best_hits {
                        (best, best_seeds) = (r, seeds);
                        via = format!("rectangle match ({summary})");
                        // It worked: re-try sooner, so a board that keeps
                        // drifting is followed rather than sitting out the
                        // full backoff.
                        if streamed {
                            self.fiducials.last_global_recover = Some((now, true));
                        }
                    } else {
                        why.push(format!("rectangle match ({summary}) found no more holes"));
                    }
                }
                Err(e) => why.push(e),
            }
        } else if best_hits < AUTO_RECOVER_BELOW {
            // Suppressed by the cooldown — say so, or a Check pressed during
            // Live reads as a ladder that silently stopped at stage 2.
            why.push("rectangle match throttled under Live".into());
        }

        // Install the winning stage's markers. When nothing beat the operator's
        // own, that IS `original` — a failed Check must never park the ✛ set
        // wherever the last failed attempt happened to leave it.
        self.fiducials.search = best_seeds;
        self.fiducials
            .found
            .resize(self.fiducials.search.len(), None);
        if via != "markers" {
            // The markers moved, so the click-in-order round no longer names
            // the positions the operator was working through.
            self.fiducials.marking = None;
        }
        let r = best;

        let (s, w, m) = r.tally;
        self.fiducials.rows = r.rows;
        self.fiducials.found = r.found_px;

        // Measure the camera scale from KNOWN design spacing paired with the
        // detected pixels — not the search-marker spacing check_frame uses
        // internally, which a small marker offset turns into a scale error (LR-17).
        let design = fiducial::parse_layout(&self.fiducials.layout).unwrap_or_default();
        self.fiducials.measured_ppm = fiducial::scale_from_design(&design, &self.fiducials.found);
        let scale = match self.fiducials.measured_ppm {
            Some(p) => format!("  ·  measured {p:.2} px/mm"),
            None => String::new(),
        };
        self.fiducials.note = format!("{s} strong, {w} weak, {m} missed{scale}");
        // After the note is rewritten, never before — everything from here down
        // appends. WHICH stage located the holes is the operator's main
        // diagnostic: "markers" means their clicks were right, "projection
        // seed" means the calibration was, "rectangle match" means neither was
        // and the geometry had to do it. The failed stages' reasons follow, so
        // "the scan found nothing" reads differently from "the scan found blobs
        // but none form the layout" — those need opposite fixes.
        self.fiducials
            .note
            .push_str(&format!("  ·  located via {via}"));
        for reason in &why {
            self.fiducials.note.push_str(&format!("  ·  {reason}"));
        }

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

        self.update_placement_from_fiducials();
    }

    /// Fit the board's actual pose from the just-detected holes and write it
    /// into the placement (rotation + translation, mirror-aware), so a Check
    /// re-registers the job without a manual drag. The outcome — success or the
    /// specific reason it was skipped — is appended to the fiducial note; only
    /// an applied pose sets `placement.auto_pose` and caches `fiducials.pose`.
    pub(super) fn update_placement_from_fiducials(&mut self) {
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
        // Say why. The entry paths that go through `set_fid_frame` do report a
        // bad layout, but detection can also be reached with markers already in
        // place and the layout since edited to something unparseable — and then
        // this returned silently, leaving the verdict line asserting "placement
        // not updated" with no reason anywhere on screen.
        let layout = match fiducial::parse_layout(&self.fiducials.layout) {
            Ok(l) => l,
            Err(e) => {
                self.fiducials
                    .note
                    .push_str(&format!("  ·  placement not updated — layout: {e}"));
                // Still opens a `check=N`: a check that never got past its own
                // layout field is exactly as worth recording as one that did.
                let layout = self.fiducials.layout.clone();
                self.diag_check_begin(&layout, (w, h), Err("not reached"), &[]);
                self.diag_check_outcome(&format!("refused-layout ({e})"));
                return;
            }
        };
        // place_projection is the ONLY correct camera-px → machine-mm source
        // here (the design→px homography above would just recover the layout).
        // Surface the projection's OWN error. It fails for two very different
        // reasons — nothing calibrated at all, versus a calibration this frame
        // doesn't match (resolution/crop/orientation) — and a generic "no
        // calibration" reads as flatly untrue in the second case, which is the
        // one an operator can actually act on.
        let projection = match self.place_projection(w, h) {
            Ok(p) => p,
            Err(e) => {
                self.fiducials.note.push_str(&format!(
                    "  ·  placement not updated — no camera→machine projection: {e}"
                ));
                let layout = self.fiducials.layout.clone();
                self.diag_check_begin(&layout, (w, h), Err(e.as_str()), &[]);
                self.diag_check_outcome("refused-projection");
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
        // Record 2a. WHICH projection variant produced these millimetres is
        // half the answer when the overlay and the export disagree, so it is
        // logged next to the numbers it produced rather than inferred later.
        let layout_text = self.fiducials.layout.clone();
        self.diag_check_begin(&layout_text, (w, h), Ok(&projection), &detected);
        // Cache before the gates below: a REFUSED fit is exactly when the
        // operator needs these, because "⌖ layout from detection" is how a
        // layout that never matched the real holes gets replaced by one that
        // does.
        self.fiducials.detected_mm = detected.clone();
        // Back side: the camera sees the drilled holes' EXIT openings, so fit
        // against the exit-magnified nominal positions.
        let exit = self.back_field_params();
        // …and that model needs real optics to be a model at all.
        // `FieldParams::exit_magnification` degrades to 1.0 when the focal
        // length or the thickness is unset, which does NOT mean "no parallax" —
        // the holes still exit ~2.3% further out, and with the model flat the
        // whole of it lands in the fitted scale. That sits comfortably inside
        // the 0.90–1.10 band, so the back job is emitted 2.3% oversized with
        // nothing on screen to say so. Fail closed, like every other gate here.
        if exit.is_some() && self.exit_magnification() <= 1.0 {
            self.fiducials.note.push_str(
                "  ·  placement not updated — set the board thickness and the focal length \
                 on the Job tab before registering the back face: without them the \
                 hole-exit parallax is fitted as job scale instead",
            );
            self.diag_check_outcome("refused-optics");
            return;
        }
        let fiducial::PoseFit {
            pose,
            residuals_mm,
            fit,
            layout_centroid: b0,
        } = match fiducial::fit_board_pose(&layout, &detected, exit.as_ref()) {
            Ok(fit) => fit,
            Err(e) => {
                self.fiducials
                    .note
                    .push_str(&format!("  ·  placement not updated: {e}"));
                self.diag_check_outcome(&format!("refused-fit ({e})"));
                return;
            }
        };
        // Label each row with its residual from THIS fit — done before the
        // gates below, because a REJECTED fit is exactly when the operator
        // needs to see which fiducial is the outlier. The row's own `off` is
        // detector-vs-marker in the seeded uniform-scale frame and stays small
        // whenever detection worked, so it cannot answer "why is the RMS big".
        for (row, residual) in self.fiducials.rows.iter_mut().zip(&residuals_mm) {
            if let Some(r) = residual {
                row.text.push_str(&format!("   fit {r:.2} mm"));
            }
        }
        // Record 2b, before the gates: a REFUSED fit is exactly the one whose
        // numbers need to survive to the log.
        let centroid = {
            let hits: Vec<(f64, f64)> = detected.iter().flatten().copied().collect();
            (!hits.is_empty()).then(|| {
                let n = hits.len() as f64;
                (
                    hits.iter().map(|p| p.0).sum::<f64>() / n,
                    hits.iter().map(|p| p.1).sum::<f64>() / n,
                )
            })
        };
        self.diag_check_fit(b0, &pose, centroid);
        // The fit's mirror must match the working face, or the board is on the
        // wrong side (or not flipped): refuse rather than place a mirrored job.
        let flipped_expected = self.job.side == Side::Back;
        if pose.flipped != flipped_expected {
            self.fiducials.note.push_str(if flipped_expected {
                "  ·  fiducial pattern does not look flipped — still on the front face?"
            } else {
                "  ·  detected a mirrored fiducial pattern — is the board flipped? switch side to Back"
            });
            self.diag_check_outcome("refused-mirror");
            return;
        }
        if pose.rms_mm > POSE_MAX_RMS_MM {
            self.fiducials.note.push_str(&format!(
                "  ·  fiducial fit RMS {:.2} mm too loose — placement not updated",
                pose.rms_mm
            ));
            self.diag_check_outcome("refused-rms");
            return;
        }
        // The mirror gate above is DEGENERATE on a mirror-symmetric layout —
        // which the default `10,10; 60,10; 10,60; 60,60` is, and so is every
        // rectangle the fid-holes generator makes. There, mirroring the BOARD
        // and permuting the CORRESPONDENCE produce the same detections: each ✛
        // finds its mirror partner inside the search window, the fit comes back
        // proper (or improper) to match whichever side is selected, and the
        // residual is essentially zero. A board on the wrong face then locks
        // cleanly, which is the worst possible outcome for a double-sided job.
        //
        // What the swap cannot hide is the SCALE. The camera images the drilled
        // holes' EXIT openings, magnified by m = 1 + thickness/focal about the
        // scan center; the back fit sources already carry that factor, so a
        // RIGHT-face fit lands at 1.0 on both faces. A wrong-face fit leaves
        // exactly one factor in: m when Front is selected and the board is
        // physically flipped, 1/m when Back is selected and it never was.
        // Nearest-signature wins, so a genuine machine-scale error of a few
        // tenths of a percent is still read as a legitimate fit.
        //
        // What "right-face lands at 1.0" quietly assumed is a machine whose
        // field is the size it says it is. It shipped that way and immediately
        // cost a bench: on a machine 3.58% oversized with the scale error left
        // uncompensated, the drilled holes come out 3.58% oversized too, so a
        // perfectly legitimate front fit lands near the FIELD scale, not near
        // 1.0 — far enough past the half-way boundary that the tell fired on
        // every honest Check (twelve consecutive `refused-mirror-scale`), and
        // registration on that machine was simply unavailable. The two
        // signatures are `b` and `b·m` (or `b/m`), where `b` is what a right-
        // face fit is expected to land at; the old code was the `b = 1` case.
        let symmetric = fiducial::layout_is_mirror_symmetric(&layout);
        let mag = self.exit_magnification();
        let base = self.mirror_scale_baseline();
        if symmetric && mag >= MIRROR_TELL_MIN_M {
            let tell = if flipped_expected {
                base / mag
            } else {
                base * mag
            };
            let to_tell = (pose.scale - tell).abs();
            let to_base = (pose.scale - base).abs();
            // Nearest-signature ALONE would call anything past the midpoint a
            // wrong face, including a scale that is nowhere near either — which
            // is how a bad baseline turns into a refusal the operator cannot
            // argue with. Requiring the fit to actually sit ON the wrong-face
            // signature keeps the accusation falsifiable; a genuine flip lands
            // there within the fit's own noise, since the swapped
            // correspondence leaves a near-exact magnification behind.
            if to_tell < to_base && to_tell <= MIRROR_TELL_MAX_DEV {
                let baseline_note = if (base - 1.0).abs() > POSE_SCALE_QUIET {
                    format!(
                        " (a right-face fit on this machine is expected at {base:.4}, from the \
                         laser-field calibration's measured {:+.2}% scale error)",
                        (base - 1.0) * 100.0
                    )
                } else {
                    String::new()
                };
                self.fiducials.note.push_str(&format!(
                    "  ·  fiducial fit scale {:.4} matches the OTHER face ({tell:.4}), not \
                     {base:.4}{baseline_note} — this layout is mirror-symmetric, so the holes fit \
                     either way round and the mirror check cannot see it. Check which face is up \
                     and which side is selected. Placement not updated.",
                    pose.scale
                ));
                self.diag_check_outcome(&format!(
                    "refused-mirror-scale baseline={base:.6} tell={tell:.6} scale={:.6}",
                    pose.scale
                ));
                return;
            }
            if to_tell < to_base {
                // Leaning towards the wrong-face signature but not sitting on
                // it: either the board really is flipped AND something else is
                // off, or the baseline is wrong. Neither is a fact worth
                // refusing a placement over, so this degrades to the same
                // warning the no-tell case gets — the operator keeps working
                // and is told what the fit looks like.
                self.fiducials.note.push_str(&format!(
                    "  ·  fiducial fit scale {:.4} sits between a right-face fit ({base:.4}) and \
                     the OTHER face's signature ({tell:.4}) — too far from either to call, and \
                     this layout is mirror-symmetric so the mirror check cannot help. Placement \
                     applied; confirm which face is up.",
                    pose.scale
                ));
                self.diag_check_outcome(&format!(
                    "warned-mirror-scale-ambiguous baseline={base:.6} tell={tell:.6} scale={:.6}",
                    pose.scale
                ));
            }
        } else if symmetric {
            // No usable tell: the two faces really are indistinguishable from
            // this frame. Refusing here would block ordinary front work on the
            // default layout, so say it instead — and note that the back half of
            // the hazard is already closed, since back registration without
            // optics is refused outright above.
            self.fiducials.note.push_str(
                "  ·  mirror-symmetric layout with no board thickness/focal length — a flipped \
                 board cannot be told from an unflipped one here; set them on the Job tab",
            );
        }
        // Scale gate — see POSE_SCALE_MIN. Applying the fit resizes the burn,
        // so an implausible scale has to stop the placement even though the
        // residual looks perfect.
        if !(POSE_SCALE_MIN..=POSE_SCALE_MAX).contains(&pose.scale) {
            self.fiducials.note.push_str(&format!(
                "  ·  fiducial fit scale {:.4} ({:+.1}%) is outside \
                 {POSE_SCALE_MIN:.2}–{POSE_SCALE_MAX:.2} — wrong holes or wrong \
                 layout? placement not updated",
                pose.scale,
                (pose.scale - 1.0) * 100.0
            ));
            self.diag_check_outcome("refused-scale");
            return;
        }
        // Where the design sits RELATIVE TO THE BOARD, in the nominal layout
        // frame: map the placement the operator is looking at back through the
        // fit it was written under, and measure it from the layout centroid
        // `b0` (the point a fresh Check would land the design on). Re-applying
        // that offset under the NEW fit carries a manual nudge along as the
        // board moves, instead of every Check throwing it away. Must be read
        // BEFORE the placement is overwritten below.
        //
        // Without a previous fit — the first Check, or a layout edit that made
        // the old offset meaningless — the offset is zero, i.e. exactly the
        // "centre the design on the fiducials" behaviour Check has always had.
        let (offset, rot_offset) = match (&self.fiducials.last_fit, self.placement.auto_pose) {
            (Some(prev), true) => {
                let n = prev.inverse_apply((self.placement.tx_mm, self.placement.ty_mm));
                (
                    (n.0 - b0.0, n.1 - b0.1),
                    self.placement.rot_deg - prev.angle_deg(),
                )
            }
            _ => ((0.0, 0.0), 0.0),
        };
        let (tx_mm, ty_mm) = fit.apply((b0.0 + offset.0, b0.1 + offset.1));
        self.placement.tx_mm = tx_mm;
        self.placement.ty_mm = ty_mm;
        self.placement.rot_deg = fit.angle_deg() + rot_offset;
        // No manual scale control exists, so there is nothing to carry: the
        // fit's scale simply becomes the placement's, resizing the emitted job.
        self.placement.scale = fit.scale;
        self.placement.auto_pose = true;
        self.fiducials.last_placed = true;
        self.fiducials.pose = Some(pose);
        self.fiducials.last_fit = Some(fit);
        // Record 2c: the placement this check actually wrote, with the manual
        // nudge it carried across.
        self.diag_check_outcome(&format!(
            "applied carried_offset_mm={:.3},{:.3} carried_rot_deg={rot_offset:+.4}",
            offset.0, offset.1
        ));
        // Only mention a carried offset once it is big enough to be a real
        // adjustment — sub-10-µm noise from the round trip is not news.
        let carried = offset.0.hypot(offset.1);
        // Layout-frame mm, not bed mm — under a fitted scale the two differ by
        // that factor. It is a "did my nudge survive" indicator, not a metric.
        let carried = if carried > 0.01 {
            format!(", carried offset {carried:.2} mm")
        } else {
            String::new()
        };
        // The resize must never be silent — it changes the burned dimensions,
        // so it is spelled out in percent next to the scale itself.
        let resize = if (pose.scale - 1.0).abs() > POSE_SCALE_QUIET {
            format!(" → job resized {:+.2}%", (pose.scale - 1.0) * 100.0)
        } else {
            String::new()
        };
        self.fiducials.note.push_str(&format!(
            "  ·  placement set from fiducials (rot {:+.2}°, scale {:.4}{resize},              RMS {:.2} mm{carried})",
            pose.rot_deg, pose.scale, pose.rms_mm
        ));
        self.ensure_placement_job();
    }

    /// Make sure the just-locked placement has geometry to draw.
    ///
    /// A lock is only worth anything if the operator can see where the job
    /// landed, and this tab's outline draws nothing until the Gerbers are
    /// loaded — so a good registration would sit invisible behind a ⤵ Load
    /// design click. Called only after a fit has actually been APPLIED, so a
    /// gated-out Check never draws a placement that was refused.
    ///
    /// The pose is also meaningless without the job: `pivot` is the design
    /// point the fit lands on the fiducial centroid, and it stays (0,0) until
    /// the geometry is loaded — which would put the raw GERBER ORIGIN on the
    /// fiducials instead of the design's centre. Parsed once (the job is then
    /// non-empty), so a Live re-lock never re-reads the Gerbers.
    fn ensure_placement_job(&mut self) {
        if !self.placement.job.is_empty() {
            return;
        }
        match self.active_job() {
            Ok((_, _, ablate)) => {
                self.placement.pivot = crate::place::bbox_center_mm(&ablate);
                self.placement.job = ablate;
            }
            Err(e) => self
                .fiducials
                .note
                .push_str(&format!("  ·  design not drawn: {e}")),
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
                ui.label("fiducial rect W mm");
                let mut rect_edited = ui
                    .add(
                        egui::DragValue::new(&mut self.fiducials.rect_w_mm)
                            .speed(0.5)
                            .range(5.0..=500.0),
                    )
                    .on_hover_text(
                        "Centre-to-centre x span of the four fiducials. The \
                         rectangle sits in the middle of the work area (equal \
                         gaps left/right), so no coordinates are needed.",
                    )
                    .changed();
                ui.end_row();
                ui.label("fiducial rect H mm");
                rect_edited |= ui
                    .add(
                        egui::DragValue::new(&mut self.fiducials.rect_h_mm)
                            .speed(0.5)
                            .range(5.0..=500.0),
                    )
                    .on_hover_text(
                        "Centre-to-centre y span of the four fiducials, centred \
                         in the work area (equal gaps top/bottom).",
                    )
                    .changed();
                ui.end_row();
                if rect_edited {
                    self.apply_fid_rect();
                }
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
                ui.add(egui::TextEdit::singleline(&mut self.fiducials.out).desired_width(240.0))
                    .labelled_by(lbl.id)
                    .on_hover_text(
                        "Where the generated fiducial-holes .lbrn2 is written (⚙ Generate holes).",
                    );
                ui.end_row();
            });
        // Wrapped: this row has grown to a dozen widgets, and a plain
        // `horizontal` runs them off the right edge with no scrollbar — the
        // last buttons simply become unreachable at any sane window width.
        ui.horizontal_wrapped(|ui| {
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
            let recover_label = ui.label("re-acquire s");
            ui.add(
                egui::DragValue::new(&mut self.fiducials.live_recover_s)
                    .speed(0.1)
                    .range(LIVE_RECOVER_MIN_S..=LIVE_RECOVER_MAX_S),
            )
            .labelled_by(recover_label.id)
            .on_hover_text(
                "How often Live re-runs the whole-frame rectangle match while \
                 the holes are lost. Each attempt costs a visible hitch of \
                 ~180 ms, so a shorter interval follows a moving board more \
                 closely at the price of a slower feed. After a scan that found \
                 nothing it waits 4× this instead.",
            );
            ui.checkbox(&mut self.fiducials.show_placement, "▦ show placement")
                .on_hover_text(
                    "Outline the placed job over this frame, at the Actions-panel pose \
                     (needs the Job-tab Gerbers and a camera calibration). A fiducial \
                     lock loads the job automatically.",
                );
            // Armed explicitly, never implicitly: an unarmed drag across the
            // design is a pan attempt, not a re-registration.
            ui.checkbox(&mut self.fiducials.move_job, "✋ move job (drag)")
                .on_hover_text(
                    "Arm dragging the placed job: while this is ticked, a drag that \
                     starts on the design MOVES it (Shift+drag rotates about its \
                     pivot). It clears itself as soon as one drag finishes, and on a \
                     new frame or a side switch — so a stray drag can never re-place \
                     a registered job. Pan/zoom stays on Ctrl either way.",
                );
            ui.checkbox(&mut self.fiducials.click_place, "✚ click-to-place")
                .on_hover_text(
                    "Left-click an empty spot to add an expected fiducial; \
                     right-click a ✛ to remove it. \
                     Adding/removing here cancels an active marking round.",
                );
            if ui
                .button("↺ reset markers")
                .on_hover_text(
                    "Reseed the ✛s at the expected layout positions and restart \
                     the click-in-order round (the layout itself is kept).",
                )
                .clicked()
            {
                self.reset_fid_markers();
            }
            if ui
                .button("✕ clear markers")
                .on_hover_text(
                    "Remove ALL expected fiducials — empties the layout, so \
                     neither the per-frame sync nor ↺ can reseed them. \
                     ⟳ layout from W×H is the way back.",
                )
                .clicked()
            {
                self.clear_fid_markers();
            }
            // The offset the button undoes, stated right beside it: the only
            // place it used to appear was a clause deep in the note line, and
            // then only while it happened to be the newest thing there.
            let (on_pose, offset_text) = self.placement_offset_text();
            ui.colored_label(status_color(on_pose), offset_text);
            if ui
                .add_enabled(
                    self.fiducials.last_fit.is_some(),
                    egui::Button::new("⊕ recentre on fiducials"),
                )
                .on_hover_text(
                    "Drop any manual offset and put the design back on the fiducial \
                     centroid at the fitted rotation and scale — and disarm ✋ move job. \
                     Dragging the design is carried across re-Checks on purpose; this is \
                     how you undo one. Disabled until a Check has produced a fit to \
                     centre on.",
                )
                .clicked()
            {
                self.recentre_on_fiducials();
            }
            if ui
                .add_enabled(
                    self.fiducials.detected_mm.iter().all(Option::is_some)
                        && !self.fiducials.detected_mm.is_empty(),
                    egui::Button::new("⌖ layout from detection"),
                )
                .on_hover_text(
                    "Make the CURRENT board pose nominal: replace the layout with the \
                     positions just measured, so the fit residual goes to zero and every \
                     later Check measures only how far the board has moved since. Use \
                     this when the fit is refused as too loose but all the holes were \
                     found — it means the layout never described them.",
                )
                .clicked()
            {
                self.adopt_measured_layout();
            }
            if ui
                .button("⟳ layout from W×H")
                .on_hover_text(
                    "Rebuild the layout as the four corners of the fiducial rectangle \
                     above, centred in the work area — discarding any click-placed \
                     edits. The same thing editing W or H does, for when the value you \
                     want is the one already in the field.",
                )
                .clicked()
            {
                self.apply_fid_rect();
            }
            if ui
                .add_enabled(
                    !self.lightburn_busy(),
                    egui::Button::new("⚙ Generate holes → LightBurn (no burn)"),
                )
                .on_hover_text(
                    "Write a .lbrn2 with a hole at each expected position above (the same \
                     layout the check uses), as a Line layer at the Job-tab drill settings, \
                     then LOAD it in LightBurn (FORCELOAD) without pressing start — you burn \
                     it from LightBurn yourself.",
                )
                .clicked()
            {
                self.fiducial_generate_holes();
            }
            if let Some(ppm) = self.fiducials.measured_ppm
                && ui
                    .button(format!("↧ use measured {ppm:.2} px/mm"))
                    .on_hover_text(
                        "Adopt the fiducial-measured scale for this tab and the placement.",
                    )
                    .clicked()
            {
                self.fiducials.px_per_mm = ppm;
                self.placement.px_per_mm = ppm;
            }
        });
        ui.label(egui::RichText::new(&self.fiducials.note).weak());
        // The resolved positions, so the derived layout stays inspectable —
        // and so click-to-place edits (which write the layout directly, not the
        // W/H above) are visible rather than silent.
        ui.weak(format!("expected: {}", self.fiducials.layout));
        ui.weak("⚙ Generate holes writes a .lbrn2 at the expected positions above — same layout the check uses — and loads it in LightBurn, never pressing start.");
        ui.weak("Click each ✛ onto its hole in layout order; the detector searches locally around it. The typed px/mm only seeds the search — registration is anchored to the measured scale.");
        ui.weak("Drag a ✛ onto its hole to fix one bad marker — the check re-runs when you let go, and only that marker moves (the expected layout is left alone). A ✛ takes the drag first, so Shift+drag ON one moves the marker, not the design.");
        ui.weak("Drag ON the outlined design — away from any ✛ — to move it; Shift+drag to rotate it about its centre. A drag that starts anywhere else marks fiducials as usual.");
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
            let (rms, rot, scale) = self
                .fiducials
                .pose
                .as_ref()
                .map_or((0.0, 0.0, 1.0), |p| (p.rms_mm, p.rot_deg, p.scale));
            ui.colored_label(
                status_color(true),
                format!(
                    "● {s}/{n} fiducials, RMS {rms:.2} mm — placement updated                      (rot {rot:+.2}°, scale {scale:.4})"
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

    /// The placed design projected onto this frame's screen coordinates.
    ///
    /// This is the point of a lock: seeing where the job will actually burn
    /// relative to the holes that were just detected. Vector outlines rather
    /// than a composited overlay — the fiducial texture is re-uploaded whole on
    /// every Live frame already, and blending a bench-resolution image on top
    /// of that would double the per-frame cost for a filled region the operator
    /// does not need in order to judge alignment.
    ///
    /// Design (Gerber mm) → machine mm through the placement affine, then
    /// machine mm → pixels through the SAME projection the fit used, so the
    /// outline lands where the export will.
    fn project_placed_design(
        &self,
        xf: &crate::imgview::ImageXform,
        width: u32,
        height: u32,
    ) -> Result<PlacedDesign, String> {
        let projection = self.place_projection(width, height)?;
        let a = self.placement().affine();
        let to_screen = |mm: (f64, f64)| projection.to_px(mm).map(|(px, py)| xf.to_screen(px, py));
        let mut rings: Vec<Vec<egui::Pos2>> = Vec::new();
        let mut bbox: Option<egui::Rect> = None;
        for poly in &self.placement.job {
            for ring in std::iter::once(&poly.outer).chain(poly.holes.iter()) {
                let pts: Vec<egui::Pos2> = ring
                    .iter()
                    .filter_map(|p| {
                        let nm = NM_PER_MM as f64;
                        let (gx, gy) = (p.x as f64 / nm, p.y as f64 / nm);
                        to_screen((a[0] * gx + a[1] * gy + a[2], a[3] * gx + a[4] * gy + a[5]))
                    })
                    .collect();
                // A ring that partly failed to project would draw a chord
                // across the gap; skip it rather than lie.
                if pts.len() != ring.len() || pts.len() < 2 {
                    continue;
                }
                for &p in &pts {
                    bbox = Some(match bbox {
                        None => egui::Rect::from_min_max(p, p),
                        Some(b) => b.union(egui::Rect::from_min_max(p, p)),
                    });
                }
                rings.push(pts);
            }
        }
        let pivot = to_screen((self.placement.tx_mm, self.placement.ty_mm))
            .ok_or("active camera projection returned a non-finite pivot")?;
        Ok(PlacedDesign {
            rings,
            // The grab handle is the outline's BOUNDING BOX, deliberately
            // coarser than the outline itself: a point-in-polygon test over
            // every copper ring, every frame, to decide whether a press starts
            // a move would cost far more than it buys — and a handle that is
            // slightly generous is easier to hit than one that is exact.
            bbox: bbox.unwrap_or_else(|| egui::Rect::from_min_max(pivot, pivot)),
            pivot,
        })
    }

    /// The frame with clickable search markers (✛) and detected rings drawn on
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
        let (tw, thp) = (tex.size()[0] as u32, tex.size()[1] as u32);

        // Screen positions of the ✛ set, for every hit test below (grab a marker
        // to drag, right-click one to remove, refuse to stack a new one on it).
        // Materialized (not a closure) so the `&self` borrow is released before
        // the `&mut self` calls, and computed once rather than per gesture.
        let marker_px: Vec<(f32, f32)> = self
            .fiducials
            .search
            .iter()
            .map(|&(x, y)| {
                let s = to_screen(x, y);
                (s.x, s.y)
            })
            .collect();

        // Project the design FIRST: its screen-space bounding box is the grab
        // handle the drag below hit-tests against, and the input handlers all
        // run before anything is painted.
        let mut design = (self.fiducials.show_placement && !self.placement.job.is_empty())
            .then(|| self.project_placed_design(&xf, tw, thp));

        // What a drag grabs, in strict priority order — decided once at
        // drag_started and latched for the whole gesture, so the pointer
        // wandering mid-drag neither stops the move nor lets the release drop
        // a ✛:
        //   1. Ctrl (pan/zoom) — navigation always wins, nothing is grabbed.
        //   2. A ✛ within MARKER_GRAB_PX — dragged onto its hole.
        //   3. The design's bounding box, but ONLY while ✋ move job is armed —
        //      moved (Shift rotates).
        //   4. Nothing: the release marks/places fiducials as before.
        // The marker is tested FIRST on purpose. The design's handle is a coarse
        // screen-space bbox that almost always contains the markers, so testing
        // it first would make an existing ✛ ungrabbable whenever the outline is
        // shown. One consequence, since the marker wins outright: a Shift+drag
        // that starts on a ✛ drags the marker instead of rotating the design.
        //
        // Step 3's arming is what closes the incident this gate exists for.
        // Navigation here is Ctrl-only, so an operator who drags to pan without
        // holding it lands in the design's bbox — which used to re-place a
        // registered job silently, and with Shift to rotate it too. Unarmed, a
        // bare drag now falls through to case 4 and does NOTHING to the job:
        // deliberately not "pans instead", because pan/zoom on every canvas in
        // this console is Ctrl, and making one surface pan bare would make the
        // convention the operator relies on conditional.
        if resp.drag_started() {
            let grabbed = resp
                .interact_pointer_pos()
                .filter(|_| !nav)
                .and_then(|p| fiducial::nearest_marker(&marker_px, (p.x, p.y), MARKER_GRAB_PX));
            let on_design = match &design {
                Some(Ok(d)) => resp
                    .interact_pointer_pos()
                    .is_some_and(|p| d.bbox.contains(p)),
                _ => false,
            };
            self.fiducials.marker_drag = grabbed;
            self.fiducials.design_drag =
                design_drag_latches(on_design, nav, grabbed.is_some(), self.fiducials.move_job);
            let target = if grabbed.is_some() {
                "marker"
            } else if self.fiducials.design_drag {
                "design"
            } else {
                "none"
            };
            let origin = DragOrigin {
                target,
                marker: grabbed,
                modifiers: modifier_token(ui),
                armed: self.fiducials.move_job,
                start_px: resp
                    .interact_pointer_pos()
                    .map(|p| xf.to_native(p))
                    .unwrap_or((f64::NAN, f64::NAN)),
                start_place: (
                    self.placement.tx_mm,
                    self.placement.ty_mm,
                    self.placement.rot_deg,
                ),
            };
            self.diag_drag_started(&origin);
            self.fiducials.drag_origin = Some(origin);
        }
        // Move the grabbed ✛ with the cursor. The screen delta goes through the
        // SAME `to_mm` the click handler uses — as the difference of the two
        // endpoints, so there is one conversion from screen to the tab's uniform
        // frame, not two. Detection is deliberately NOT re-run here; it runs
        // once on release (`fid_marker_drag_release`).
        if let Some(i) = self.fiducials.marker_drag
            && !nav
            && resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let delta = resp.drag_delta();
            let (x0, y0) = to_mm(pos - delta);
            let (x1, y1) = to_mm(pos);
            self.fid_drag_marker(i, (x1 - x0, y1 - y0));
        }
        let mut moved = false;
        if self.fiducials.design_drag
            && !nav
            && resp.dragged()
            && let Some(Ok(d)) = &design
        {
            moved = true;
            let delta = resp.drag_delta();
            if ui.input(|i| i.modifiers.shift) {
                // Rotate about the design's own pivot, wherever it landed on
                // screen. Manual adjustment does NOT clear `auto_pose`: the
                // next Check carries the nudge across through `last_fit`.
                if let Some(pos) = resp.interact_pointer_pos() {
                    let step = rot_delta_deg(d.pivot, pos - delta, pos);
                    self.placement.rot_deg = wrap_deg(self.placement.rot_deg + step);
                }
            } else {
                // Screen delta → native frame pixels (divide by the display
                // scale), then applied in PIXEL space (see `drag_place_px`) so
                // the outline tracks the cursor even under a perspective
                // homography. Derived from the pivot each frame rather than
                // accumulated, so the rounding never drifts.
                let scale = xf.scale.max(1e-3) as f64;
                if let Err(e) =
                    self.drag_place_px(tw, thp, delta.x as f64 / scale, delta.y as f64 / scale)
                {
                    self.placement.note = format!("placement projection unavailable: {e}");
                }
            }
        }
        // Re-project after a move so the outline painted below is this frame's
        // pose, not the one the drag started from — a one-frame lag reads as
        // the design sticking to the cursor.
        if moved {
            design = Some(self.project_placed_design(&xf, tw, thp));
        }
        // Record 3. This is the one diagnostic reached from a per-frame path, so
        // it is guarded three ways: not during a drag (an in-progress gesture is
        // not a state change — the settled position lands on the frame after the
        // release), and then inside `diag_overlay` by the placement snapshot and
        // by an epsilon on the resulting machine-mm box.
        if !self.fiducials.design_drag {
            let drawn_center_px = match &design {
                Some(Ok(d)) => Some(xf.to_native(d.bbox.center())),
                _ => None,
            };
            self.diag_overlay(drawn_center_px, (tw, thp));
        }
        // A gesture that grabbed the design OR a marker must not also place
        // fiducials. Read BEFORE the latches are released, so the release frame
        // of either drag can't slip a marker through.
        let marking_allowed = self.fid_marking_allowed();
        if resp.drag_stopped() {
            // The arm is one-shot: it authorises THIS gesture and no more, so
            // it can never be left standing across a re-Check, a new frame or a
            // walk away from the bench.
            if self.fiducials.design_drag {
                self.fiducials.move_job = false;
            }
            self.fiducials.design_drag = false;
            // Takes the marker latch and re-checks if one was held.
            self.fid_marker_drag_release();
            // After the release handlers, so the record carries the placement
            // the gesture finally left behind.
            if let Some(origin) = self.fiducials.drag_origin.take() {
                let end_px = resp.interact_pointer_pos().map(|p| xf.to_native(p));
                self.diag_drag_stopped(&origin, end_px);
            }
        }

        // Click-to-place (FLD-12): hit-test add (empty spot) vs. remove
        // (right-click on a ✛). Suppressed while navigating.
        if self.fiducials.click_place && !nav && marking_allowed {
            // Right-click a marker → remove it, so the set shrinks (fixes the
            // add-only pile-up).
            if resp.secondary_clicked()
                && let Some(pos) = resp.interact_pointer_pos()
                && let Some(i) =
                    fiducial::nearest_marker(&marker_px, (pos.x, pos.y), MARKER_GRAB_PX)
            {
                self.remove_expected_fiducial(i);
            }
            // Left-click on empty frame → append an expected fiducial there (not
            // when a marker is under the pointer, so an existing ✛ isn't stacked on).
            else if resp.clicked()
                && let Some(pos) = resp.interact_pointer_pos()
                && fiducial::nearest_marker(&marker_px, (pos.x, pos.y), MARKER_GRAB_PX).is_none()
            {
                let (mx, my) = to_mm(pos);
                self.add_expected_fiducial(mx, my);
            }
        }

        // Placement: outside click-to-place, a primary click drops the next
        // marker in layout order — implicitly opening a round when none is
        // active, and closing it + detecting on the final marker's click.
        // Suppressed while navigating.
        if !self.fiducials.click_place
            && !nav
            && marking_allowed
            && resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let mm = to_mm(pos);
            self.fid_mark_click(mm);
        }

        // Paint markers + detected rings.
        let painter = ui.painter_at(rect);

        // The placed job, drawn UNDER the markers so a ✛ is never hidden by it.
        match design {
            Some(Ok(ref d)) => {
                let stroke = egui::Stroke::new(1.0_f32, Color32::from_rgb(0xf0, 0x50, 0x30));
                for ring in &d.rings {
                    painter.add(egui::Shape::closed_line(ring.clone(), stroke));
                }
            }
            Some(Err(ref e)) => {
                painter.text(
                    rect.left_top() + egui::vec2(6.0, 6.0),
                    egui::Align2::LEFT_TOP,
                    format!("placement not drawn: {e}"),
                    egui::FontId::proportional(12.0),
                    Color32::from_rgb(0xd0, 0x50, 0x50),
                );
            }
            None => {}
        }

        let cyan = Color32::from_rgb(0x22, 0xcc, 0xdd);
        // While a marking round is active, ghost the markers not yet placed
        // (index ≥ the one being marked) so the next target reads at a glance.
        let ghost = Color32::from_rgba_unmultiplied(0x22, 0xcc, 0xdd, 90);
        // A grab has to be discoverable, so the ✛ the pointer is close enough to
        // take — and the one a drag is holding — draw white and heavier. Reuses
        // the hit test the drag itself runs, so what lights up is exactly what
        // would be grabbed — and nothing lights up mid design-drag, where the
        // pointer sits over the widget all the way and a ✛ passing under it
        // would otherwise read as the thing being moved.
        let hot = self.fiducials.marker_drag.or_else(|| {
            (!nav && !self.fiducials.design_drag)
                .then(|| resp.hover_pos())
                .flatten()
                .and_then(|p| fiducial::nearest_marker(&marker_px, (p.x, p.y), MARKER_GRAB_PX))
        });
        let ring_r = (self.fiducials.diameter_mm as f32 * ppm * 0.5 * xf.scale).max(5.0);
        for (i, &(mx, my)) in self.fiducials.search.iter().enumerate() {
            let c = to_screen(mx, my);
            let grab = hot == Some(i);
            let mcol = match self.fiducials.marking {
                _ if grab => Color32::WHITE,
                Some(k) if i >= k => ghost,
                _ => cyan,
            };
            let w = if grab { 2.5_f32 } else { 1.5 };
            painter.line_segment(
                [egui::pos2(c.x - 9.0, c.y), egui::pos2(c.x + 9.0, c.y)],
                (w, mcol),
            );
            painter.line_segment(
                [egui::pos2(c.x, c.y - 9.0), egui::pos2(c.x, c.y + 9.0)],
                (w, mcol),
            );
            painter.circle_stroke(
                c,
                11.0,
                egui::Stroke::new(if grab { 2.0 } else { 1.0 }, mcol),
            );
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
                        let hw =
                            (self.fiducials.diameter_mm as f32 * ppm * 0.5 * xf.scale).max(3.0);
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
