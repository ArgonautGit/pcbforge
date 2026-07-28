//! Fiducial-check view: run `vision::find_fiducials` (VIS-4) on a frame and
//! draw the result over it, so the operator confirms the detector locked onto
//! the real drilled holes (not honeycomb-bed decoys) before any registration is
//! trusted.
//!
//! Overlays, painted into an [`egui::ColorImage`] (verifiable headless, shown
//! via the console's texture path):
//! * **cyan crosshair** at each expected fiducial;
//! * **ring + center dot** at each detected center — green if the confidence
//!   score clears [`SCORE_OK`], amber if weak;
//! * **red ✕** at the expected spot for any miss.
//!
//! The frame is a file for now (a saved camera grab or a phone photo); it
//! becomes the live VIS-1 feed later with no change to this code. Until VIS-3
//! provides the real bed homography, the px↔mm map is a uniform scale with
//! the **y axis flipped**: bed (0,0) is the frame's bottom-left and bed y
//! grows upward, matching the machine/Gerber y-up frame (image rows grow
//! downward — mapping them to bed y directly would mirror every position
//! handed to `register`, which is what the machine burns).

use egui::{Color32, ColorImage};
use image::GrayImage;
use nalgebra::Point2;
use vision::{BedMap, FidShape, FiducialProfile, Miss, find_fiducials};

/// Confidence score at or above which a detection is drawn as "strong" (green).
pub const SCORE_OK: f64 = 0.25;

/// Which [`FiducialProfile`] the operator selected, kept as a plain `Copy`
/// enum for the UI's combo box (the vision profile carries a diameter; this
/// pairs with the separate diameter field to build one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    /// Dark drilled hole / burned dot on a bright field (the operator default).
    DarkDot,
    /// Bright ablated disc inside an untouched ring (burned annulus).
    Annulus,
    /// Bright blob on a dark field (hole lit from below).
    Backlit,
}

impl ProfileKind {
    /// Build the vision [`FiducialProfile`] for this kind with `shape`.
    pub fn to_profile(self, shape: FidShape) -> FiducialProfile {
        match self {
            ProfileKind::DarkDot => FiducialProfile::DarkDot { shape },
            ProfileKind::Annulus => FiducialProfile::Annulus { shape },
            ProfileKind::Backlit => FiducialProfile::Backlit { shape },
        }
    }

    /// Short label for the combo box.
    pub fn label(self) -> &'static str {
        match self {
            ProfileKind::DarkDot => "Dark dot (drilled hole)",
            ProfileKind::Annulus => "Annulus (burned ring)",
            ProfileKind::Backlit => "Backlit (lit from below)",
        }
    }

    /// Stable token for persistence.
    pub fn token(self) -> &'static str {
        match self {
            ProfileKind::DarkDot => "dark_dot",
            ProfileKind::Annulus => "annulus",
            ProfileKind::Backlit => "backlit",
        }
    }

    /// Parse a [`token`](Self::token) back to a kind.
    pub fn from_token(s: &str) -> Option<ProfileKind> {
        ProfileKind::ALL.into_iter().find(|k| k.token() == s)
    }

    /// The three kinds, for populating the combo box.
    pub const ALL: [ProfileKind; 3] = [
        ProfileKind::DarkDot,
        ProfileKind::Annulus,
        ProfileKind::Backlit,
    ];
}

/// Which [`FidShape`] footprint the operator selected, kept as a plain `Copy`
/// enum for the UI's combo box (the vision shape carries the actual mm sizes;
/// this pairs with the diameter/width + height fields to build one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShapeKind {
    /// Round hole / dot — sized by a single diameter.
    Circle,
    /// Axis-aligned rectangle — sized by width and height.
    Rect,
}

impl ShapeKind {
    /// Short label for the combo box.
    pub fn label(self) -> &'static str {
        match self {
            ShapeKind::Circle => "Circle ⌀",
            ShapeKind::Rect => "Rectangle",
        }
    }

    /// Stable token for persistence and the CLI `--shape` flag.
    pub fn token(self) -> &'static str {
        match self {
            ShapeKind::Circle => "circle",
            ShapeKind::Rect => "rect",
        }
    }

    /// Parse a [`token`](Self::token) back to a kind.
    pub fn from_token(s: &str) -> Option<ShapeKind> {
        ShapeKind::ALL.into_iter().find(|k| k.token() == s)
    }

    /// Build the vision [`FidShape`]: a circle uses `diameter_mm`; a rectangle
    /// uses `diameter_mm` as its width and `height_mm` as its height.
    pub fn to_fid_shape(self, diameter_mm: f64, height_mm: f64) -> FidShape {
        match self {
            ShapeKind::Circle => FidShape::Circle { diameter_mm },
            ShapeKind::Rect => FidShape::Rect {
                w_mm: diameter_mm,
                h_mm: height_mm,
            },
        }
    }

    /// The two kinds, for populating the combo box.
    pub const ALL: [ShapeKind; 2] = [ShapeKind::Circle, ShapeKind::Rect];
}

const CYAN: Color32 = Color32::from_rgb(0x22, 0xcc, 0xdd);
const GREEN: Color32 = Color32::from_rgb(0x40, 0xc0, 0x50);
const AMBER: Color32 = Color32::from_rgb(0xe0, 0x90, 0x20);
const RED: Color32 = Color32::from_rgb(0xd0, 0x40, 0x40);

/// Per-fiducial outcome for the summary list, tinted in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FidKind {
    FoundStrong,
    FoundWeak,
    Miss,
}

/// One summary row (text + how to tint it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FidRow {
    pub text: String,
    pub kind: FidKind,
}

/// The overlay image plus the per-fiducial summary.
pub struct FidResult {
    pub overlay: ColorImage,
    pub rows: Vec<FidRow>,
    /// Detected center in frame pixels per input fiducial (`None` = miss),
    /// aligned with the input order — for drawing rings over the live frame.
    pub found_px: Vec<Option<(f64, f64)>>,
    /// Counts for the header: (found_strong, found_weak, misses).
    pub tally: (usize, usize, usize),
    /// px/mm **measured** from the detected fiducial spacing vs their known
    /// design spacing — the true scale, independent of the seed. `None` with
    /// fewer than two detections.
    pub measured_px_per_mm: Option<f64>,
}

/// Index of the marker nearest `p`, within `max` distance — the hit-test for
/// dragging a search marker onto its hole. Screen-space units.
pub fn nearest_marker(markers: &[(f32, f32)], p: (f32, f32), max: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, &(mx, my)) in markers.iter().enumerate() {
        let d = ((mx - p.0).powi(2) + (my - p.1).powi(2)).sqrt();
        if d <= max && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// A detected fiducial as (design mm, found px).
type DesignPx = ((f64, f64), (f64, f64));

/// Mean px/mm over every pair of detected fiducials: pixel distance / design
/// distance. The fiducials' design spacing is their real physical spacing, so
/// this is the true camera scale.
fn measure_scale(found: &[DesignPx]) -> Option<f64> {
    let mut acc = 0.0;
    let mut n = 0;
    for i in 0..found.len() {
        for j in (i + 1)..found.len() {
            let (di, pi) = found[i];
            let (dj, pj) = found[j];
            let dmm = ((di.0 - dj.0).powi(2) + (di.1 - dj.1).powi(2)).sqrt();
            let dpx = ((pi.0 - pj.0).powi(2) + (pi.1 - pj.1).powi(2)).sqrt();
            if dmm > 1e-6 {
                acc += dpx / dmm;
                n += 1;
            }
        }
    }
    (n > 0).then(|| acc / n as f64)
}

/// Measured px/mm from KNOWN design spacing paired (by index) with detected
/// pixel positions — the true camera scale, independent of where the operator
/// dragged the search markers. Using the dragged spacing instead turns a 1 mm
/// drag over a 50 mm baseline into ~2% scale error (LR-17).
pub fn scale_from_design(design: &[(f64, f64)], found_px: &[Option<(f64, f64)>]) -> Option<f64> {
    let pairs: Vec<DesignPx> = design
        .iter()
        .zip(found_px)
        .filter_map(|(&d, f)| f.map(|p| (d, p)))
        .collect();
    measure_scale(&pairs)
}

/// Run detection on an in-memory frame and build the overlay + summary.
/// `expected_mm` is the nominal fiducial layout in bed mm; `px_per_mm` is the
/// (uniform, pre-VIS-3) bed scale; `profile` is the operator-selected fiducial
/// appearance (its diameter also sizes the overlay rings).
pub fn check_frame(
    frame: &GrayImage,
    expected_mm: &[(f64, f64)],
    px_per_mm: f64,
    profile: &FiducialProfile,
    search_mm: f64,
) -> FidResult {
    // y-flipped: bed (0,0) = frame bottom-left, bed y up (machine frame).
    let bed = BedMap::uniform_scale_y_flip(px_per_mm, frame.height() as f64);
    let expected: Vec<Point2<f64>> = expected_mm
        .iter()
        .map(|&(x, y)| Point2::new(x, y))
        .collect();
    let shape = profile.shape();
    let results = find_fiducials(frame, &expected, search_mm, profile, &bed);

    let overlay = render_overlay(frame, &expected, &results, &bed, shape, px_per_mm);
    let (rows, tally) = summarize(expected_mm, &results);
    // Measured scale from the detected fiducials' spacing.
    let found: Vec<DesignPx> = expected_mm
        .iter()
        .zip(&results)
        .filter_map(|(&d, r)| r.as_ref().ok().map(|f| (d, (f.found_px.x, f.found_px.y))))
        .collect();
    let found_px: Vec<Option<(f64, f64)>> = results
        .iter()
        .map(|r| r.as_ref().ok().map(|f| (f.found_px.x, f.found_px.y)))
        .collect();
    FidResult {
        overlay,
        rows,
        found_px,
        tally,
        measured_px_per_mm: measure_scale(&found),
    }
}

/// Load a frame from `path` (PNG/JPEG) and run [`check_frame`].
pub fn check(
    path: &str,
    expected_mm: &[(f64, f64)],
    px_per_mm: f64,
    profile: &FiducialProfile,
    search_mm: f64,
) -> Result<FidResult, String> {
    let path = crate::clean_path(path);
    if path.is_empty() {
        return Err("set a frame image path (a saved camera grab or a photo)".into());
    }
    if px_per_mm <= 0.0 {
        return Err("px per mm must be positive".into());
    }
    let frame = image::open(&path)
        .map_err(|e| format!("open {path}: {e}"))?
        .to_luma8();
    Ok(check_frame(
        &frame,
        expected_mm,
        px_per_mm,
        profile,
        search_mm,
    ))
}

/// Parse `"10,10; 60,10; 10,60"` into fiducial points. Whitespace-tolerant;
/// returns an error naming the offending token.
pub fn parse_layout(s: &str) -> Result<Vec<(f64, f64)>, String> {
    let mut out = Vec::new();
    for (i, pair) in s
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .enumerate()
    {
        let mut it = pair.split(',').map(str::trim);
        let x = it
            .next()
            .and_then(|v| v.parse::<f64>().ok())
            .ok_or_else(|| format!("point {}: bad x in {pair:?}", i + 1))?;
        let y = it
            .next()
            .and_then(|v| v.parse::<f64>().ok())
            .ok_or_else(|| format!("point {}: bad y in {pair:?}", i + 1))?;
        out.push((x, y));
    }
    if out.is_empty() {
        return Err("no fiducials — expected e.g. `10,10; 60,10; 10,60`".into());
    }
    Ok(out)
}

/// The four fiducial-hole centres for a `w`×`h` rectangle centred at
/// `(cx, cy)` — `w`/`h` are the CENTRE-to-CENTRE spans, not a board outline,
/// so the corners land exactly on them. Order: (x0,y0), (x1,y0), (x0,y1),
/// (x1,y1) — the same LL, LR, UL, UR ordering the check drives from.
///
/// Centring is what lets the operator skip coordinates entirely: with the
/// rectangle centred in the work area, the two numbers fix all four positions.
pub fn centered_fid_layout(cx: f64, cy: f64, w: f64, h: f64) -> [(f64, f64); 4] {
    let x0 = cx - w / 2.0;
    let x1 = cx + w / 2.0;
    let y0 = cy - h / 2.0;
    let y1 = cy + h / 2.0;
    [(x0, y0), (x1, y0), (x0, y1), (x1, y1)]
}

/// Format points as the `"x,y; x,y; …"` layout string [`parse_layout`] reads,
/// with 2-decimal coordinates.
pub fn format_layout(pts: &[(f64, f64)]) -> String {
    pts.iter()
        .map(|(x, y)| format!("{x:.2},{y:.2}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Scale band a pair hypothesis must land in. This is the ONLY separation test
/// — an absolute `|d_cand − d_layout|` gate cannot be used, because the layout
/// is measured in px through the operator's typed px/mm, which is a guess. A
/// 20% wrong guess offsets every layout distance by 20% of the span, dwarfing
/// any sane pixel tolerance, so the gate has to be on the RATIO. ±30% covers
/// that guess plus the per-edge stretch perspective adds (measured up to 6.5%
/// on the bench frame), and the arrangement itself does the real pruning: all
/// of ≥3 points must land, not just one pair at a plausible spacing.
const MATCH_SCALE_MIN: f64 = 0.7;
const MATCH_SCALE_MAX: f64 = 1.3;
/// Hard cap on the candidate list before the O(n²) pair loop. The hypothesis
/// count is `layout² × candidates²` (4 points × 96 candidates ≈ 110 k pair
/// tests, of which only the in-band ones are scored), so this is the compute
/// budget. Applied here as well as by the caller: this is `pub`, and a caller
/// handing over 500 blobs must not wedge the console.
const MATCH_MAX_CANDIDATES: usize = 96;

/// Match tolerance as a fraction of the layout's own span. Sized from the
/// bench frame (`samples/fiducial/bench-plate-4holes.png`), where the plate is
/// tilted enough that its diagonals differ by 9% and the best similarity still
/// leaves ~5% of the span on the worst corner. Tightening this below the
/// perspective error rejects the real board; loosening it much past 10% starts
/// letting honeycomb holes stand in for a corner.
const MATCH_TOL_SPAN_FRAC: f64 = 0.09;

/// The match tolerance callers should pass to [`match_layout_to_candidates`]:
/// [`MATCH_TOL_SPAN_FRAC`] of the layout's largest point separation, in pixels.
/// Span-relative rather than absolute so it means the same thing whether the
/// camera is 100 mm or 400 mm from the bed.
pub fn match_tol_px(layout_mm: &[(f64, f64)], px_per_mm: f64) -> f64 {
    let mut span: f64 = 0.0;
    for (i, a) in layout_mm.iter().enumerate() {
        for b in &layout_mm[i + 1..] {
            span = span.max((a.0 - b.0).hypot(a.1 - b.1));
        }
    }
    MATCH_TOL_SPAN_FRAC * span * px_per_mm
}

/// Result of matching the known layout onto whole-frame candidates.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutMatch {
    /// Chosen candidate pixel per layout index (`None` = no candidate matched).
    pub matched_px: Vec<Option<(f64, f64)>>,
    pub matched: usize,
    /// RMS of the matched points against the hypothesised transform, px.
    pub rms_px: f64,
}

/// Find the subset of `candidates_px` whose ARRANGEMENT matches `layout_mm` —
/// the recovery for when the search markers are nowhere near the real holes and
/// every local search misses.
///
/// The discriminator is the layout's rigid geometry, not proximity to a guess:
/// a hypothesis is built from each pair of layout points against each pair of
/// candidates, and only the arrangement that claims the most candidates
/// survives. That is what rejects honeycomb-bed decoys here — they are the
/// right size (so `vision::find_fiducial_candidates` admits them) but they do
/// not sit on the rectangle.
///
/// `candidates_px` MUST be ordered best-first, as
/// [`vision::find_fiducial_candidates`] returns it: the position in the list is
/// the only quality signal the matcher has, and it is what breaks ties between
/// arrangements that all fit (see the ranking comment below).
///
/// `tol_px` is how far a projected layout point may sit from the candidate it
/// claims, in layout pixels (it is multiplied by the hypothesis' scale before
/// use, so it means the same fraction of the observed pattern whatever the
/// px/mm guess). It must be SPAN-RELATIVE, not a fixed pixel count: the camera
/// sees the plate at a tilt, and a similarity cannot fit a perspective-warped
/// rectangle — on the bench frame the best similarity still leaves ~5-9% of the
/// span on some corners. See [`match_tol_px`], which is what callers should use.
///
/// Returns `None` unless at least 3 layout points matched, the floor
/// [`fit_board_pose`] needs — a 2-point "match" is two decoys at the right
/// spacing, which would place the job from noise.
pub fn match_layout_to_candidates(
    layout_mm: &[(f64, f64)],
    frame_h_px: f64,
    px_per_mm: f64,
    candidates_px: &[(f64, f64)],
    max_rot_deg: f64,
    tol_px: f64,
) -> Option<LayoutMatch> {
    if layout_mm.len() < 3 || !(px_per_mm.is_finite() && px_per_mm > 0.0) || !tol_px.is_finite() {
        return None;
    }
    // Convert the layout out of bed mm (y-UP, origin bottom-left) into the
    // frame's pixel convention (y-DOWN, origin top-left) FIRST. After this the
    // hypothesised transform is a plain similarity with a small rotation;
    // carrying the y-flip through the matcher instead would make every
    // hypothesis improper, and "small rotation" would stop meaning anything.
    let layout_px: Vec<(f64, f64)> = layout_mm
        .iter()
        .map(|&(x, y)| (x * px_per_mm, frame_h_px - y * px_per_mm))
        .collect();
    let cands = &candidates_px[..candidates_px.len().min(MATCH_MAX_CANDIDATES)];
    if cands.len() < 3 {
        return None;
    }
    let max_rot = max_rot_deg.to_radians();
    let (n, m) = (layout_px.len(), cands.len());

    let mut best: Option<LayoutMatch> = None;
    // Summed candidate index behind `best` — its quality rank (see below).
    let mut best_rank = usize::MAX;
    for i in 0..n {
        for j in 0..n {
            if i == j {
                continue;
            }
            let lv = (
                layout_px[j].0 - layout_px[i].0,
                layout_px[j].1 - layout_px[i].1,
            );
            let ld = lv.0.hypot(lv.1);
            if ld < 1e-6 {
                continue;
            }
            let la = lv.1.atan2(lv.0);
            for a in 0..m {
                for b in 0..m {
                    if a == b {
                        continue;
                    }
                    let cv = (cands[b].0 - cands[a].0, cands[b].1 - cands[a].1);
                    let cd = cv.0.hypot(cv.1);
                    let s = cd / ld;
                    if !(MATCH_SCALE_MIN..=MATCH_SCALE_MAX).contains(&s) {
                        continue;
                    }
                    // Tolerance travels with the hypothesis' scale, so it stays
                    // the same fraction of the OBSERVED pattern even when the
                    // typed px/mm is well off.
                    let tol = tol_px * s;
                    // Rotation from the two direction vectors, wrapped to
                    // (-π, π]. The board is right-side-up on the bed, so
                    // anything past a few degrees is a coincidence, not a pose.
                    let mut th = cv.1.atan2(cv.0) - la;
                    th = th.rem_euclid(std::f64::consts::TAU);
                    if th > std::f64::consts::PI {
                        th -= std::f64::consts::TAU;
                    }
                    if th.abs() > max_rot {
                        continue;
                    }
                    let (sin, cos) = th.sin_cos();
                    // p ↦ cand[a] + s·R(θ)·(p − layout_px[i]).
                    let project = |p: (f64, f64)| {
                        let (dx, dy) = (p.0 - layout_px[i].0, p.1 - layout_px[i].1);
                        (
                            cands[a].0 + s * (cos * dx - sin * dy),
                            cands[a].1 + s * (sin * dx + cos * dy),
                        )
                    };
                    // Greedy claim, closest pair first, so a candidate sitting
                    // between two projected points goes to the one it fits
                    // better. One candidate may not serve two layout points.
                    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
                    for (li, &p) in layout_px.iter().enumerate() {
                        let q = project(p);
                        for (ci, &c) in cands.iter().enumerate() {
                            let d = (c.0 - q.0).hypot(c.1 - q.1);
                            if d <= tol {
                                pairs.push((d, li, ci));
                            }
                        }
                    }
                    pairs.sort_by(|x, y| x.0.total_cmp(&y.0));
                    let mut matched_px = vec![None; n];
                    let mut taken = vec![false; m];
                    let (mut count, mut sse, mut rank_sum) = (0usize, 0.0, 0usize);
                    for (d, li, ci) in pairs {
                        if matched_px[li].is_some() || taken[ci] {
                            continue;
                        }
                        matched_px[li] = Some(cands[ci]);
                        taken[ci] = true;
                        count += 1;
                        sse += d * d;
                        rank_sum += ci;
                    }
                    if count < 3 {
                        continue;
                    }
                    let cand = LayoutMatch {
                        matched_px,
                        matched: count,
                        rms_px: (sse / count as f64).sqrt(),
                    };
                    // Ranking, in order: most points matched; then the most
                    // fiducial-LOOKING points (lowest summed candidate index —
                    // `candidates_px` arrives best-first); then the tighter fit.
                    //
                    // Quality has to outrank RMS, and that is the difference
                    // between working and not on the bench frame. The plate is
                    // tilted, so the true quad is perspective-warped and the
                    // best similarity leaves several px of residual, while a
                    // honeycomb bed is a REGULAR GRID — among a hundred bed
                    // holes some four sit on a near-perfect scaled copy of the
                    // layout and fit tighter than the real thing. Residual
                    // cannot separate them; how much each blob looks like a
                    // drilled fiducial can, and does (the four plate holes rank
                    // 0/2/7/11 of 161 there).
                    let better = match &best {
                        None => true,
                        Some(b) if cand.matched != b.matched => cand.matched > b.matched,
                        Some(_) if rank_sum != best_rank => rank_sum < best_rank,
                        Some(b) => cand.rms_px < b.rms_px,
                    };
                    if better {
                        best = Some(cand);
                        best_rank = rank_sum;
                    }
                }
            }
        }
    }
    best
}

/// The board's fitted pose, distilled to what the placement needs: the rigid
/// (optionally mirrored) transform carrying the design/layout frame onto the
/// machine bed, plus the fit quality.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoardPose {
    pub tx_mm: f64,
    pub ty_mm: f64,
    pub rot_deg: f64,
    /// Uniform scale of the fit: measured fiducial spacing / nominal layout
    /// spacing. `1.038` means the holes read 3.8% further apart than the design
    /// says. The caller APPLIES this to the job (see `place::Placement::scale`),
    /// so it must be gated before use — see `POSE_SCALE_MIN`/`POSE_SCALE_MAX`.
    pub scale: f64,
    pub rms_mm: f64,
    /// The fit chose the x-mirrored variant (`Rigid2::flip_x`). Whether that
    /// matches the working face (front vs. back) is the caller's decision.
    pub flipped: bool,
    pub used: usize,
}

/// A fitted board pose plus the per-fiducial residuals behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct PoseFit {
    pub pose: BoardPose,
    /// Distance from each fitted position to the detected one, mm, aligned with
    /// the layout (`None` = that fiducial was not detected). These are exactly
    /// what [`BoardPose::rms_mm`] averages, so one large entry names the odd
    /// fiducial out.
    ///
    /// The summary rows' own `off` cannot do this job: it is measured in the
    /// seeded uniform-scale bed frame against the SEARCH MARKER, so it reports
    /// how far the detector moved from where you clicked. It stays small
    /// whenever detection worked, even when the fit is metres out.
    pub residuals_mm: Vec<Option<f64>>,
    /// The raw nominal-bed → measured-bed correction the pose was written from.
    /// A caller that wants to keep something else fixed relative to the board
    /// (an operator's manual placement offset) needs the transform itself, not
    /// just the one point [`BoardPose`] carries through it.
    pub fit: calib::Similarity2,
    /// `b0`: the FULL layout's centroid in the raw design frame — the point
    /// `fit` is applied to for the pose, and so the origin any board-relative
    /// offset is measured from.
    pub layout_centroid: (f64, f64),
}

/// Fit the board's pose from detected fiducials paired with the nominal layout.
///
/// `detected_mm[i]` is the machine-mm position measured for layout point `i`
/// (`None` = not found). `exit = Some(params)` is the BACK face: the camera
/// sees each drilled through-hole's EXIT opening, so the fit sources are the
/// exit-magnified nominal positions ([`cam::flip::entry_to_exit_mm`]); `None`
/// is the front. The translation lands the FULL layout's centroid `b0` (so the
/// pose is independent of which subset detected) under the fit, which centers
/// the design on the fiducial-layout centroid. Back exactness holds because the
/// design mirror about x=0 (applied to the job elsewhere) and the fitted
/// physical flip compose to a proper placement.
///
/// The fit is a SIMILARITY (uniform scale + rotation + translation + mirror),
/// so a board whose real hole spacing differs from the nominal layout registers
/// instead of being refused by the residual gate — at the cost of the residual
/// no longer revealing a scale error. `BoardPose::scale` is where that error
/// now shows, and the caller must gate on it.
pub fn fit_board_pose(
    layout_mm: &[(f64, f64)],
    detected_mm: &[Option<(f64, f64)>],
    exit: Option<&cam::flip::FieldParams>,
) -> Result<PoseFit, String> {
    if layout_mm.is_empty() {
        return Err("no layout points".into());
    }
    // On the back face this is itself a magnification, and the fit below now
    // has a scale term to absorb error in it. So a back-face `pose.scale` is
    // machine scale × exit-model error, not pure machine scale — compare front
    // and back before believing either as a machine constant.
    let src = |p: (f64, f64)| match exit {
        Some(field) => cam::flip::entry_to_exit_mm(p.0, p.1, field),
        None => p,
    };
    let pairs: Vec<(Point2<f64>, Point2<f64>)> = layout_mm
        .iter()
        .zip(detected_mm)
        .filter_map(|(&l, d)| {
            d.map(|(dx, dy)| {
                let (sx, sy) = src(l);
                (Point2::new(sx, sy), Point2::new(dx, dy))
            })
        })
        .collect();
    if pairs.len() < 3 {
        return Err(format!("need ≥3 detected fiducials, have {}", pairs.len()));
    }
    // Similarity, not rigid: the operator's measured fiducial spacing differs
    // from the nominal layout by a real percentage (machine/calibration scale),
    // and a rigid fit can only dump that into the residual — where it trips the
    // RMS gate and no placement ever happens. Fitting the scale collapses the
    // residual to the true registration error, and the scale is carried into
    // the emitted job. `fit_similarity` is reflection-aware like `fit_rigid`,
    // so the front/back mirror check below is unchanged.
    let fit = calib::fit_similarity(&pairs)?;
    // b0 = centroid of the FULL layout (not just the detected subset), in the
    // RAW design frame — the pose is written as fit.apply(b0).
    let n = layout_mm.len() as f64;
    let b0 = (
        layout_mm.iter().map(|p| p.0).sum::<f64>() / n,
        layout_mm.iter().map(|p| p.1).sum::<f64>() / n,
    );
    // Per-point residuals, aligned with the FULL layout so a caller can label
    // rows by index; `rms_mm` is derived from these so the two can't drift.
    let residuals_mm: Vec<Option<f64>> = layout_mm
        .iter()
        .zip(detected_mm)
        .map(|(&l, d)| {
            d.map(|(dx, dy)| {
                let (x, y) = fit.apply(src(l));
                ((x - dx).powi(2) + (y - dy).powi(2)).sqrt()
            })
        })
        .collect();
    let sse: f64 = residuals_mm.iter().flatten().map(|r| r * r).sum();
    let rms_mm = (sse / pairs.len() as f64).sqrt();
    let (tx_mm, ty_mm) = fit.apply(b0);
    Ok(PoseFit {
        pose: BoardPose {
            tx_mm,
            ty_mm,
            rot_deg: fit.angle_deg(),
            scale: fit.scale,
            rms_mm,
            flipped: fit.rigid.flip_x,
            used: pairs.len(),
        },
        residuals_mm,
        fit,
        layout_centroid: b0,
    })
}

fn summarize(
    expected_mm: &[(f64, f64)],
    results: &[Result<vision::Fiducial, Miss>],
) -> (Vec<FidRow>, (usize, usize, usize)) {
    let mut rows = Vec::new();
    let (mut strong, mut weak, mut miss) = (0, 0, 0);
    for (i, (exp, res)) in expected_mm.iter().zip(results).enumerate() {
        match res {
            Ok(f) => {
                let off_um = ((f.found_mm.x - exp.0).powi(2) + (f.found_mm.y - exp.1).powi(2))
                    .sqrt()
                    * 1000.0;
                let score = f.confidence.score;
                let kind = if score >= SCORE_OK {
                    strong += 1;
                    FidKind::FoundStrong
                } else {
                    weak += 1;
                    FidKind::FoundWeak
                };
                rows.push(FidRow {
                    text: format!(
                        "#{i}  ({:.1},{:.1})→({:.2},{:.2}) mm   off {off_um:.0} µm   score {score:.2}",
                        exp.0, exp.1, f.found_mm.x, f.found_mm.y
                    ),
                    kind,
                });
            }
            Err(m) => {
                miss += 1;
                let why = match m {
                    Miss::LowContrast { snr } => format!("low contrast (snr {snr:.1})"),
                    Miss::NoCandidate { snr } => format!("no candidate (snr {snr:.1})"),
                    Miss::OutsideFrame => "search window outside frame".to_string(),
                    Miss::DotTooSmall { dot_px } => {
                        format!(
                            "dot too small ({dot_px:.1} px) — move closer or fix diameter/scale"
                        )
                    }
                };
                rows.push(FidRow {
                    text: format!("#{i}  ({:.1},{:.1}) mm   MISS: {why}", exp.0, exp.1),
                    kind: FidKind::Miss,
                });
            }
        }
    }
    (rows, (strong, weak, miss))
}

fn render_overlay(
    frame: &GrayImage,
    expected: &[Point2<f64>],
    results: &[Result<vision::Fiducial, Miss>],
    bed: &BedMap,
    shape: FidShape,
    px_per_mm: f64,
) -> ColorImage {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let mut px: Vec<Color32> = Vec::with_capacity(w * h);
    for p in frame.pixels() {
        px.push(Color32::from_gray(p[0]));
    }
    let mut ov = Overlay { px, w, h };

    // Footprint mm extents; a circle is square. The crosshair arms are sized
    // off the largest extent so a rectangle's arms still clear its outline.
    let (w_mm, h_mm) = match shape {
        FidShape::Circle { diameter_mm } => (diameter_mm, diameter_mm),
        FidShape::Rect { w_mm, h_mm } => (w_mm, h_mm),
    };
    let maxdim = w_mm.max(h_mm);
    let r = (maxdim * px_per_mm * 0.5).max(3.0) as i32;
    let arm = (maxdim * px_per_mm * 0.9).max(5.0) as i32;
    // Rectangle half-extents in pixels (only used for the Rect footprint).
    let half_w = (w_mm * px_per_mm * 0.5).max(2.0) as i32;
    let half_h = (h_mm * px_per_mm * 0.5).max(2.0) as i32;

    // Expected crosshairs first (under the detections).
    for e in expected {
        let p = bed.mm_to_px(*e);
        ov.cross(p.x as i32, p.y as i32, arm, CYAN);
    }
    // Detections / misses on top.
    for (e, res) in expected.iter().zip(results) {
        match res {
            Ok(f) => {
                let c = if f.confidence.score >= SCORE_OK {
                    GREEN
                } else {
                    AMBER
                };
                let (cx, cy) = (f.found_px.x as i32, f.found_px.y as i32);
                // A circle draws its ring; a rectangle draws its axis-aligned
                // outline (w×h), both centered on the detected point.
                match shape {
                    FidShape::Circle { .. } => ov.ring(cx, cy, r, c),
                    FidShape::Rect { .. } => ov.rect(cx, cy, half_w, half_h, c),
                }
                ov.disk(cx, cy, 2, c);
            }
            Err(_) => {
                let p = bed.mm_to_px(*e);
                ov.ex(p.x as i32, p.y as i32, arm, RED);
            }
        }
    }
    ColorImage {
        size: [w, h],
        pixels: ov.px,
    }
}

/// A pixel target with clamped plotting primitives.
struct Overlay {
    px: Vec<Color32>,
    w: usize,
    h: usize,
}

impl Overlay {
    fn put(&mut self, x: i32, y: i32, c: Color32) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = c;
        }
    }
    /// A `+` crosshair, arms of half-length `arm`, thickened one pixel.
    fn cross(&mut self, cx: i32, cy: i32, arm: i32, c: Color32) {
        for d in -arm..=arm {
            self.put(cx + d, cy, c);
            self.put(cx + d, cy + 1, c);
            self.put(cx, cy + d, c);
            self.put(cx + 1, cy + d, c);
        }
    }
    /// A `✕`, arms of half-length `arm`.
    fn ex(&mut self, cx: i32, cy: i32, arm: i32, c: Color32) {
        for d in -arm..=arm {
            self.put(cx + d, cy + d, c);
            self.put(cx + d + 1, cy + d, c);
            self.put(cx + d, cy - d, c);
            self.put(cx + d + 1, cy - d, c);
        }
    }
    /// A circle outline of radius `r` (midpoint), thickened by also drawing
    /// `r-1` so it reads at small sizes.
    fn ring(&mut self, cx: i32, cy: i32, r: i32, c: Color32) {
        for rr in [r, r - 1].into_iter().filter(|v| *v > 0) {
            let (mut x, mut y, mut d) = (rr, 0, 1 - rr);
            while x >= y {
                for (px, py) in [
                    (cx + x, cy + y),
                    (cx + y, cy + x),
                    (cx - y, cy + x),
                    (cx - x, cy + y),
                    (cx - x, cy - y),
                    (cx - y, cy - x),
                    (cx + y, cy - x),
                    (cx + x, cy - y),
                ] {
                    self.put(px, py, c);
                }
                y += 1;
                if d < 0 {
                    d += 2 * y + 1;
                } else {
                    x -= 1;
                    d += 2 * (y - x) + 1;
                }
            }
        }
    }
    /// An axis-aligned rectangle outline, half-extents `hw`×`hh`, thickened
    /// one pixel inward so it reads at small sizes (mirrors [`ring`]).
    fn rect(&mut self, cx: i32, cy: i32, hw: i32, hh: i32, c: Color32) {
        for t in 0..2 {
            let (l, r, top, bot) = (cx - hw + t, cx + hw - t, cy - hh + t, cy + hh - t);
            if l > r || top > bot {
                continue;
            }
            for x in l..=r {
                self.put(x, top, c);
                self.put(x, bot, c);
            }
            for y in top..=bot {
                self.put(l, y, c);
                self.put(r, y, c);
            }
        }
    }
    /// A small filled disk of radius `r`.
    fn disk(&mut self, cx: i32, cy: i32, r: i32, c: Color32) {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    self.put(cx + dx, cy + dy, c);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Front: a known PROPER rigid maps the layout to the detected holes, and
    /// the fit recovers it exactly — rotation, the layout-centroid translation,
    /// and flipped=false.
    #[test]
    fn fit_board_pose_recovers_a_proper_front_pose() {
        let layout = [(10.0, 10.0), (80.0, 10.0), (10.0, 80.0), (80.0, 80.0)];
        let (s, c) = 5.0_f64.to_radians().sin_cos();
        let t = calib::Rigid2 {
            cos: c,
            sin: s,
            tx: 3.0,
            ty: -2.0,
            flip_x: false,
        };
        let detected: Vec<Option<(f64, f64)>> = layout.iter().map(|&p| Some(t.apply(p))).collect();

        let fit = fit_board_pose(&layout, &detected, None).unwrap();
        let pose = fit.pose;
        assert!(
            fit.residuals_mm.iter().all(|r| r.is_some_and(|v| v < 1e-9)),
            "every point sits on the fit: {:?}",
            fit.residuals_mm
        );
        assert!(!pose.flipped, "a proper pattern is not flagged flipped");
        assert!((pose.rot_deg - 5.0).abs() < 1e-6, "rot {}", pose.rot_deg);
        assert!(pose.rms_mm < 1e-9, "exact fit, rms {}", pose.rms_mm);
        assert_eq!(pose.used, 4);
        let b0 = (
            layout.iter().map(|p| p.0).sum::<f64>() / 4.0,
            layout.iter().map(|p| p.1).sum::<f64>() / 4.0,
        );
        let (ex, ey) = t.apply(b0);
        assert!(
            (pose.tx_mm - ex).abs() < 1e-9 && (pose.ty_mm - ey).abs() < 1e-9,
            "translation lands the layout centroid under the fit: ({}, {}) vs ({ex}, {ey})",
            pose.tx_mm,
            pose.ty_mm
        );
    }

    /// The bench case this fit exists for: the holes are really 4% further
    /// apart than the layout says. A rigid fit would leave that as ~1 mm of
    /// residual and the caller's RMS gate would refuse the placement; the
    /// similarity fit recovers the scale exactly and the residual collapses.
    #[test]
    fn fit_board_pose_recovers_a_uniform_scale() {
        let layout = [(10.0, 10.0), (80.0, 10.0), (10.0, 80.0), (80.0, 80.0)];
        let (s, c) = 5.0_f64.to_radians().sin_cos();
        let truth = calib::Similarity2 {
            scale: 1.04,
            rigid: calib::Rigid2 {
                cos: c,
                sin: s,
                tx: 3.0,
                ty: -2.0,
                flip_x: false,
            },
        };
        let detected: Vec<Option<(f64, f64)>> =
            layout.iter().map(|&p| Some(truth.apply(p))).collect();

        let fit = fit_board_pose(&layout, &detected, None).unwrap();
        let pose = fit.pose;
        assert!((pose.scale - 1.04).abs() < 1e-12, "scale {}", pose.scale);
        assert!((pose.rot_deg - 5.0).abs() < 1e-6, "rot {}", pose.rot_deg);
        assert!(!pose.flipped);
        assert!(pose.rms_mm < 1e-9, "scale absorbed, rms {}", pose.rms_mm);
        assert!(
            fit.residuals_mm.iter().all(|r| r.is_some_and(|v| v < 1e-9)),
            "every point sits on the fit: {:?}",
            fit.residuals_mm
        );
        let b0 = (
            layout.iter().map(|p| p.0).sum::<f64>() / 4.0,
            layout.iter().map(|p| p.1).sum::<f64>() / 4.0,
        );
        let (ex, ey) = truth.apply(b0);
        assert!((pose.tx_mm - ex).abs() < 1e-9 && (pose.ty_mm - ey).abs() < 1e-9);

        // The same data under a rigid fit is exactly the failure being fixed:
        // >0.5 mm RMS over this 70 mm square, i.e. gated out.
        let pairs: Vec<_> = layout
            .iter()
            .zip(&detected)
            .map(|(&l, d)| {
                let d = d.unwrap();
                (Point2::new(l.0, l.1), Point2::new(d.0, d.1))
            })
            .collect();
        let rigid = calib::fit_rigid(&pairs).unwrap();
        let sse: f64 = pairs
            .iter()
            .map(|(a, b)| {
                let (x, y) = rigid.apply((a.x, a.y));
                (x - b.x).powi(2) + (y - b.y).powi(2)
            })
            .sum();
        assert!(
            (sse / 4.0).sqrt() > 0.5,
            "a rigid fit cannot absorb it: rms {}",
            (sse / 4.0).sqrt()
        );
    }

    /// Back (load-bearing): the camera sees each hole's EXIT opening after a
    /// physical flip. Fit against the exit-magnified layout, then verify the
    /// resulting back Placement carries the x=0-mirrored COPPER design through
    /// the SAME physical flip — with NO exit magnification (copper is a surface
    /// mark, only the drilled fiducials carry the parallax). The load-bearing
    /// identity, derived from the recipe: Placement.apply(mirror_x0(g)) ==
    /// flip.apply(g − pivot_front + b0).
    #[test]
    fn fit_board_pose_back_places_mirrored_copper_through_the_physical_flip() {
        let layout = [(10.0, 10.0), (80.0, 10.0), (10.0, 80.0), (80.0, 80.0)];
        let b0 = (
            layout.iter().map(|p| p.0).sum::<f64>() / 4.0,
            layout.iter().map(|p| p.1).sum::<f64>() / 4.0,
        ); // (45, 45)
        let params = cam::flip::FieldParams {
            scan_center_mm: b0,
            thickness_mm: 1.6,
            focal_mm: 70.0,
        };
        // The physical flip: a reflection (map = R·F_neg) — R = +3°, chosen tx/ty.
        let (s, c) = 3.0_f64.to_radians().sin_cos();
        let flip = calib::Rigid2 {
            cos: c,
            sin: s,
            tx: 90.0,
            ty: 5.0,
            flip_x: true,
        };
        // detected = flip(exit_magnify(L_i)).
        let detected: Vec<Option<(f64, f64)>> = layout
            .iter()
            .map(|&p| Some(flip.apply(cam::flip::entry_to_exit_mm(p.0, p.1, &params))))
            .collect();

        let pose = fit_board_pose(&layout, &detected, Some(&params))
            .unwrap()
            .pose;
        assert!(pose.flipped, "a mirrored pattern is flagged flipped");
        assert!(pose.rms_mm < 1e-9, "exact fit, rms {}", pose.rms_mm);

        // Sample copper points distinct from the fiducials, so pivot_front ≠ b0
        // and the identity is non-trivial. active_job mirrors the back job about
        // design x=0, so the placed design point is mirror_x0(g).
        let copper = [(20.0, 30.0), (55.0, 15.0)];
        let (fx0, fx1, fy0, fy1) = (20.0_f64, 55.0_f64, 15.0_f64, 30.0_f64);
        let pivot_front = ((fx0 + fx1) / 2.0, (fy0 + fy1) / 2.0);
        // bbox center of the x=0-mirrored copper == mirror_x0(pivot_front).
        let pivot_back = (-pivot_front.0, pivot_front.1);
        let placement = crate::place::Placement {
            tx_mm: pose.tx_mm,
            ty_mm: pose.ty_mm,
            rot_deg: pose.rot_deg,
            scale: pose.scale,
            pivot_mm: pivot_back,
        };
        let a = placement.affine();
        let apply_place = |g: (f64, f64)| {
            (
                a[0] * g.0 + a[1] * g.1 + a[2],
                a[3] * g.0 + a[4] * g.1 + a[5],
            )
        };
        for &g in &copper {
            let g_back = (-g.0, g.1);
            let bed_front = (g.0 - pivot_front.0 + b0.0, g.1 - pivot_front.1 + b0.1);
            let want = flip.apply(bed_front);
            let got = apply_place(g_back);
            assert!(
                (got.0 - want.0).abs() < 1e-6 && (got.1 - want.1).abs() < 1e-6,
                "g {g:?}: placement {got:?} vs flip(bed_front) {want:?}"
            );
        }
    }

    /// A front-side layout whose detections are actually MIRRORED (the board
    /// was flipped without switching to Back) is flagged `flipped` — the signal
    /// the caller keys on to refuse the update.
    #[test]
    fn fit_board_pose_flags_a_mirrored_pattern() {
        let layout = [(10.0, 10.0), (80.0, 10.0), (10.0, 80.0), (80.0, 80.0)];
        let (s, c) = 4.0_f64.to_radians().sin_cos();
        let mirrored = calib::Rigid2 {
            cos: c,
            sin: s,
            tx: 7.0,
            ty: -3.0,
            flip_x: true,
        };
        let detected: Vec<Option<(f64, f64)>> =
            layout.iter().map(|&p| Some(mirrored.apply(p))).collect();
        let pose = fit_board_pose(&layout, &detected, None).unwrap().pose;
        assert!(pose.flipped, "the mirror is detected");
        assert!(pose.rms_mm < 1e-9, "still an exact fit");
    }

    /// Gates: too few detections, an empty layout, and a degenerate target
    /// (all detections collapsed to one point) each return Err.
    #[test]
    fn fit_board_pose_rejects_too_few_or_degenerate() {
        let layout = [(10.0, 10.0), (80.0, 10.0), (10.0, 80.0), (80.0, 80.0)];
        let two = [Some((10.0, 10.0)), Some((80.0, 10.0)), None, None];
        assert!(fit_board_pose(&layout, &two, None).is_err(), "need ≥3");
        assert!(fit_board_pose(&[], &[], None).is_err(), "empty layout");
        let collapsed = [Some((5.0, 5.0)), Some((5.0, 5.0)), Some((5.0, 5.0)), None];
        assert!(
            fit_board_pose(&layout, &collapsed, None).is_err(),
            "no target spread is degenerate"
        );
    }

    /// xorshift for deterministic noise (no rand dep), matching VIS-4's test.
    struct Rng(u64);
    impl Rng {
        fn noise(&mut self, a: f64) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            let u = (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64;
            (u * 2.0 - 1.0) * a
        }
    }

    /// Frame with anti-aliased dark holes on a glary copper field + optional
    /// decoys — the operator's honeycomb-bed hazard.
    fn frame(w: u32, h: u32, dots: &[(f64, f64, f64)], depth: f64, noise: f64) -> GrayImage {
        let mut rng = Rng(1);
        GrayImage::from_fn(w, h, |x, y| {
            let bg = 140.0 + 70.0 * (x as f64 + y as f64) / (w + h) as f64;
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if dots.iter().any(|&(cx, cy, d)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < d / 2.0
                    }) {
                        cover += 1.0_f64 / 16.0;
                    }
                }
            }
            image::Luma([(bg - depth * cover + rng.noise(noise)).clamp(0.0, 255.0) as u8])
        })
    }

    const PPM: f64 = 10.0;

    /// Bed mm → image pixel row under the y-flipped uniform map (bed origin
    /// at the frame's bottom-left, bed y up — the machine/Gerber frame).
    fn py(frame_h: u32, y_mm: f64) -> f64 {
        frame_h as f64 - y_mm * PPM
    }

    /// The operator's default profile (1 mm drilled hole → dark dot).
    fn dark() -> FiducialProfile {
        FiducialProfile::DarkDot {
            shape: FidShape::Circle { diameter_mm: 1.0 },
        }
    }

    /// The operator's L-layout: all three holes found, small offsets, strong
    /// scores, and the overlay marks green at each detected center.
    #[test]
    fn operator_layout_all_found_and_marked_green() {
        let expected = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
        // Board nudged ~0.4 mm off nominal (bed mm, y-up).
        let (dx, dy) = (0.4, -0.3);
        let dots: Vec<_> = expected
            .iter()
            .map(|(ex, ey)| ((ex + dx) * PPM, py(700, ey + dy), 1.0 * PPM))
            .collect();
        let img = frame(700, 700, &dots, 85.0, 5.0);

        let r = check_frame(&img, &expected, PPM, &dark(), 2.0);
        assert_eq!(r.tally, (3, 0, 0), "all three strong: {:?}", r.rows);
        // The measured scale recovers the true px/mm from the hole spacing,
        // regardless of the seed passed in.
        let measured = r.measured_px_per_mm.expect("scale measured");
        assert!((measured - PPM).abs() < 0.1, "measured {measured} vs {PPM}");

        // Overlay carries the green found-color near each detected center.
        let [w, h] = r.overlay.size;
        let green = Color32::from_rgb(0x40, 0xc0, 0x50);
        for (ex, ey) in expected {
            let cx = ((ex + dx) * PPM) as usize;
            let cy = py(700, ey + dy) as usize;
            let hit = (cy.saturating_sub(8)..(cy + 8).min(h)).any(|y| {
                (cx.saturating_sub(8)..(cx + 8).min(w))
                    .any(|x| r.overlay.pixels[y * w + x] == green)
            });
            assert!(hit, "expected green mark near ({ex},{ey})");
        }
    }

    /// The measured scale recovers the true px/mm even when the seed is off:
    /// the seed only has to be close enough to place the search windows (a wide
    /// window here absorbs a 5% seed error), and the reported scale then comes
    /// from the fiducial spacing, not the seed.
    #[test]
    fn measured_scale_recovers_truth_from_an_off_seed() {
        let expected = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
        let dots: Vec<_> = expected
            .iter()
            .map(|(ex, ey)| (ex * PPM, py(700, *ey), 1.0 * PPM))
            .collect();
        let img = frame(700, 700, &dots, 85.0, 5.0);
        // Seed 9.5 px/mm (5% low) with a generous 5 mm search window.
        let r = check_frame(&img, &expected, 9.5, &dark(), 5.0);
        assert_eq!(r.tally.0, 3, "all found with the off seed: {:?}", r.rows);
        let measured = r.measured_px_per_mm.unwrap();
        assert!(
            (measured - PPM).abs() < 0.1,
            "measured {measured}, truth {PPM}"
        );
    }

    #[test]
    fn scale_from_design_uses_design_spacing() {
        // Detected 500 px apart; the DESIGN says 50 mm apart → 10 px/mm,
        // independent of where the search markers were dragged (LR-17).
        let design = vec![(0.0, 0.0), (50.0, 0.0)];
        let found = vec![Some((100.0, 100.0)), Some((600.0, 100.0))];
        assert!((scale_from_design(&design, &found).unwrap() - 10.0).abs() < 1e-9);
        // A missing detection drops that point from the pairing.
        assert!(scale_from_design(&design, &[Some((100.0, 100.0)), None]).is_none());
    }

    /// A low-contrast frame produces a MISS row that names the SNR reason —
    /// a lighting problem surfaced, not a silent bad lock.
    ///
    /// The rendered depth dropped from 6 to 0.6 when `vision` moved to a
    /// matched filter (2026-07-26). Averaging over a dot-sized region cuts
    /// uniform pixel noise by roughly √(area), so a 6-level dot under ±6 noise
    /// is now genuinely recoverable and no longer models "too dim to see".
    #[test]
    fn low_contrast_reports_a_miss_with_reason() {
        let img = frame(200, 200, &[(100.0, 100.0, 10.0)], 0.6, 6.0);
        let r = check_frame(&img, &[(10.0, 10.0)], PPM, &dark(), 2.0);
        assert_eq!(r.tally, (0, 0, 1));
        assert_eq!(r.rows[0].kind, FidKind::Miss);
        assert!(
            r.rows[0].text.contains("MISS") && r.rows[0].text.contains("snr"),
            "miss row names the SNR: {:?}",
            r.rows[0].text
        );
    }

    /// A same-size decoy 2.5 mm off the expected spot must not be marked as the
    /// fiducial — the detection stays on the true hole (VIS-4's local search).
    #[test]
    fn decoy_is_not_marked_as_the_fiducial() {
        // Expected bed (10,10) mm → image (100, py(220,10)=120) px.
        let true_c = (100.0, py(220, 10.0));
        let img = frame(
            220,
            220,
            &[(true_c.0, true_c.1, 10.0), (125.0, true_c.1, 10.0)],
            85.0,
            5.0,
        );
        let r = check_frame(&img, &[(10.0, 10.0)], PPM, &dark(), 3.0);
        assert_eq!(r.tally.0, 1, "true hole found");
        // The found row's offset must be small (locked on the true hole, not
        // the decoy 2.5 mm away).
        assert!(
            r.rows[0].text.contains("off ") && !r.rows[0].text.contains("MISS"),
            "{:?}",
            r.rows[0].text
        );
    }

    #[test]
    fn nearest_marker_picks_the_closest_within_range() {
        let markers = [(10.0, 10.0), (100.0, 100.0), (12.0, 9.0)];
        // Closest to (11,10) is index 2 (dist ~1.4) over index 0 (dist ~1.0)?
        // index 0 is (10,10) dist 1.0; index 2 is (12,9) dist ~1.4 → index 0.
        assert_eq!(nearest_marker(&markers, (11.0, 10.0), 30.0), Some(0));
        // Nothing within range.
        assert_eq!(nearest_marker(&markers, (500.0, 500.0), 30.0), None);
        assert_eq!(nearest_marker(&[], (0.0, 0.0), 30.0), None);
    }

    #[test]
    fn parse_layout_reads_the_operator_default() {
        assert_eq!(
            parse_layout("10,10; 60,10; 10,60").unwrap(),
            vec![(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)]
        );
        assert!(parse_layout("garbage").is_err());
        assert!(parse_layout("").is_err());
    }

    #[test]
    fn centered_fid_layout_spans_the_rectangle_about_the_centre() {
        // A 60×40 rectangle centred at (45,45) → x 15..75, y 25..65.
        let pts = centered_fid_layout(45.0, 45.0, 60.0, 40.0);
        assert_eq!(
            pts,
            [(15.0, 25.0), (75.0, 25.0), (15.0, 65.0), (75.0, 65.0)]
        );
    }

    #[test]
    fn format_layout_round_trips_through_parse_layout() {
        let pts = centered_fid_layout(45.0, 45.0, 60.0, 40.0);
        let s = format_layout(&pts);
        assert_eq!(s, "15.00,25.00; 75.00,25.00; 15.00,65.00; 75.00,65.00");
        assert_eq!(parse_layout(&s).unwrap(), pts.to_vec());
    }

    /// The four-corner layout the recovery tests drive from (45 × 35 mm), and
    /// the frame it is rendered into.
    const REC_LAYOUT: [(f64, f64); 4] = [(15.0, 25.0), (60.0, 25.0), (15.0, 60.0), (60.0, 60.0)];
    const REC_FRAME: u32 = 800;
    /// The console's shipped rotation gate, so these exercise the real config.
    const REC_MAX_ROT: f64 = 20.0;

    /// True hole positions as `frame`-renderable dots: the layout displaced by
    /// `shift` bed mm, then rotated `rot_deg` about its own centre in the pixel
    /// frame (a board skewed on the bed).
    fn hole_dots(shift: (f64, f64), rot_deg: f64) -> Vec<(f64, f64, f64)> {
        let pts: Vec<(f64, f64)> = REC_LAYOUT
            .iter()
            .map(|&(x, y)| ((x + shift.0) * PPM, py(REC_FRAME, y + shift.1)))
            .collect();
        let n = pts.len() as f64;
        let cx = pts.iter().map(|p| p.0).sum::<f64>() / n;
        let cy = pts.iter().map(|p| p.1).sum::<f64>() / n;
        let (s, c) = rot_deg.to_radians().sin_cos();
        pts.iter()
            .map(|&(x, y)| {
                let (dx, dy) = (x - cx, y - cy);
                (cx + c * dx - s * dy, cy + s * dx + c * dy, 1.0 * PPM)
            })
            .collect()
    }

    /// The console's recovery path end to end: whole-frame candidate scan, then
    /// arrangement match. Returns the candidate pixels and the match.
    fn recover(img: &GrayImage, max_rot_deg: f64) -> (Vec<(f64, f64)>, Option<LayoutMatch>) {
        let cands = vision::find_fiducial_candidates(img, &dark(), PPM, 96);
        let pts: Vec<(f64, f64)> = cands.iter().map(|c| (c.px.x, c.px.y)).collect();
        let m = match_layout_to_candidates(
            &REC_LAYOUT,
            f64::from(REC_FRAME),
            PPM,
            &pts,
            max_rot_deg,
            match_tol_px(&REC_LAYOUT, PPM),
        );
        (pts, m)
    }

    /// Assert every layout point matched the hole rendered for it.
    fn assert_matched_on(m: &LayoutMatch, dots: &[(f64, f64, f64)]) {
        assert_eq!(m.matched, 4, "matched {}/4", m.matched);
        for (i, (got, &(tx, ty, _))) in m.matched_px.iter().zip(dots).enumerate() {
            let (gx, gy) = got.unwrap_or_else(|| panic!("layout point {i} unmatched"));
            let d = (gx - tx).hypot(gy - ty);
            assert!(d < 1.0, "point {i} matched {d:.2} px from its hole");
        }
    }

    /// Sanity on the frame conversion before any detector noise is involved:
    /// feed the layout's own pixel positions in as candidates and the matcher
    /// must return them exactly. An inverted y-flip fails here and nowhere
    /// else — every later test would just look like a tuning problem.
    #[test]
    fn match_layout_round_trips_exact_pixels() {
        let exact: Vec<(f64, f64)> = REC_LAYOUT
            .iter()
            .map(|&(x, y)| (x * PPM, py(REC_FRAME, y)))
            .collect();
        let m = match_layout_to_candidates(
            &REC_LAYOUT,
            f64::from(REC_FRAME),
            PPM,
            &exact,
            8.0,
            match_tol_px(&REC_LAYOUT, PPM),
        )
        .expect("an exact layout matches itself");
        assert_eq!(m.matched, 4);
        assert!(m.rms_px < 1e-9, "rms {}", m.rms_px);
        for (got, &want) in m.matched_px.iter().zip(&exact) {
            assert_eq!(got.unwrap(), want);
        }
    }

    /// Four holes on the glare gradient: the whole-frame scan sees all four and
    /// the arrangement match claims all four, tightly.
    #[test]
    fn whole_frame_recovery_matches_a_nominal_board() {
        let dots = hole_dots((0.4, -0.3), 0.0);
        let img = frame(REC_FRAME, REC_FRAME, &dots, 85.0, 5.0);
        let (cands, m) = recover(&img, REC_MAX_ROT);
        for &(tx, ty, _) in &dots {
            assert!(
                cands.iter().any(|&(x, y)| (x - tx).hypot(y - ty) < 1.0),
                "no candidate at ({tx:.1}, {ty:.1}) among {}",
                cands.len()
            );
        }
        let m = m.expect("the rectangle is matched");
        assert_matched_on(&m, &dots);
        assert!(m.rms_px < 2.0, "rms {:.2} px", m.rms_px);
    }

    /// THE BUG: the board sits 15 mm from where the layout says, far beyond any
    /// sane `search_mm`, so every local window misses. The whole-frame recovery
    /// still finds all four — it matches the holes' ARRANGEMENT, not their
    /// proximity to a guess.
    #[test]
    fn whole_frame_recovery_finds_a_board_far_from_nominal() {
        let dots = hole_dots((15.0, -12.0), 0.0);
        let img = frame(REC_FRAME, REC_FRAME, &dots, 85.0, 5.0);

        // The local search, given the nominal layout, gets nothing: this is
        // exactly the state the operator is stuck in.
        let local = check_frame(&img, &REC_LAYOUT, PPM, &dark(), 2.0);
        assert_eq!(local.tally.0 + local.tally.1, 0, "local search must miss");

        let m = recover(&img, REC_MAX_ROT)
            .1
            .expect("recovered despite the 15 mm offset");
        assert_matched_on(&m, &dots);
    }

    /// The honeycomb-bed hazard: same-size decoy dots scattered across the
    /// frame. They pass every per-blob gate — only the arrangement rejects
    /// them, which is the discrimination the local search used to provide.
    #[test]
    fn whole_frame_recovery_rejects_scattered_decoys() {
        let mut dots = hole_dots((15.0, -12.0), 0.0);
        let truth = dots.clone();
        for &(dx, dy) in &[
            (180.0, 180.0),
            (420.0, 640.0),
            (700.0, 220.0),
            (250.0, 470.0),
            (620.0, 700.0),
        ] {
            dots.push((dx, dy, 1.0 * PPM));
        }
        let img = frame(REC_FRAME, REC_FRAME, &dots, 85.0, 5.0);
        let (cands, m) = recover(&img, REC_MAX_ROT);
        assert!(
            cands.len() >= 9,
            "decoys are candidates too: {}",
            cands.len()
        );
        assert_matched_on(&m.expect("the true rectangle is still matched"), &truth);
    }

    /// Two real holes cannot make a board pose. The matcher returns `None`
    /// rather than a two-point "match" — with or without a stray decoy to lift
    /// the candidate count past its own ≥3 minimum.
    #[test]
    fn whole_frame_recovery_refuses_below_three_points() {
        let two: Vec<_> = hole_dots((15.0, -12.0), 0.0).into_iter().take(2).collect();
        let img = frame(REC_FRAME, REC_FRAME, &two, 85.0, 5.0);
        assert!(
            recover(&img, REC_MAX_ROT).1.is_none(),
            "two holes place nothing"
        );

        let mut plus_decoy = two;
        plus_decoy.push((200.0, 640.0, 1.0 * PPM));
        let img = frame(REC_FRAME, REC_FRAME, &plus_decoy, 85.0, 5.0);
        let (cands, m) = recover(&img, REC_MAX_ROT);
        assert!(cands.len() >= 3, "candidate floor is not the gate here");
        assert!(
            m.is_none(),
            "a decoy must not complete the rectangle: {m:?}"
        );
    }

    /// A few degrees of skew is a real board on a bed and is matched; 45° is
    /// not, and `max_rot_deg` rejects it. Same frame recipe both times, so the
    /// only difference is the angle.
    #[test]
    fn whole_frame_recovery_accepts_small_rotation_and_rejects_large() {
        let skewed = hole_dots((2.0, -1.0), 2.5);
        let img = frame(REC_FRAME, REC_FRAME, &skewed, 85.0, 5.0);
        let m = recover(&img, REC_MAX_ROT)
            .1
            .expect("2.5° is a plausible board pose");
        assert_matched_on(&m, &skewed);

        let spun = hole_dots((2.0, -1.0), 45.0);
        let img = frame(REC_FRAME, REC_FRAME, &spun, 85.0, 5.0);
        let (cands, m) = recover(&img, REC_MAX_ROT);
        assert!(cands.len() >= 4, "the four dots are still found as blobs");
        assert!(m.is_none(), "45° is outside max_rot_deg: {m:?}");
    }

    /// THE ACCEPTANCE TEST — a real bench photo, not a render.
    /// `samples/fiducial/bench-plate-4holes.png`: the operator's laser-parameter
    /// test plate on the honeycomb bed, lit from one side and photographed at a
    /// tilt. The left third of the frame is bed: dozens of dark round holes the
    /// same apparent size as the plate's four, which is precisely the decoy
    /// population the arrangement match exists to reject. The plate's diagonals
    /// differ by 9%, so there is real perspective here and no similarity fits
    /// the quad exactly — that is what the span-relative tolerance buys.
    #[test]
    fn bench_frame_recovers_the_four_plate_holes() {
        let img = image::open(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../samples/fiducial/bench-plate-4holes.png"
        ))
        .expect("bench frame fixture")
        .to_luma8();
        // The operator's own layout string, verbatim — a hand-clicked quad, not
        // a generated rectangle, so its corners are already a few mm out of
        // square before the camera adds any perspective.
        let layout = parse_layout("81.7,20.2; 128.4,27.9; 125.3,71.6; 76.3,64.8").unwrap();
        // The plate's mounting holes image ~30 px across at this resolution.
        let profile = FiducialProfile::DarkDot {
            shape: FidShape::Circle { diameter_mm: 3.0 },
        };
        // Measured hole centres, original-image px, in layout order: the layout
        // runs bottom-left, bottom-right, top-right, top-left in bed mm (y-up),
        // which is BL, BR, TR, TL down the image.
        let truth = [
            (1275.0, 650.0),
            (1760.0, 703.0),
            (1830.0, 243.0),
            (1360.0, 213.0),
        ];
        // The typed px/mm is a guess, so ±20% of it must still recover the
        // plate — otherwise the recovery only works once the operator has
        // already got the scale right, which is the thing they cannot do while
        // detection is returning nothing.
        for scale in [0.8, 1.0, 1.2] {
            let ppm = 9.97 * scale;
            let cands = vision::find_fiducial_candidates(&img, &profile, ppm, 96);
            let pts: Vec<(f64, f64)> = cands.iter().map(|c| (c.px.x, c.px.y)).collect();
            let m = match_layout_to_candidates(
                &layout,
                f64::from(img.height()),
                ppm,
                &pts,
                20.0,
                match_tol_px(&layout, ppm),
            )
            .unwrap_or_else(|| {
                panic!("no match at {ppm:.2} px/mm from {} candidates", cands.len())
            });
            assert_eq!(m.matched, 4, "at {ppm:.2} px/mm: matched {}", m.matched);
            for (i, (got, &(tx, ty))) in m.matched_px.iter().zip(&truth).enumerate() {
                let (gx, gy) = got.unwrap_or_else(|| panic!("plate hole {i} unmatched"));
                let d = (gx - tx).hypot(gy - ty);
                // 15 px against centres estimated by eye to ±5 px. A decoy
                // would be hundreds of px out — this cannot pass on one.
                assert!(d < 15.0, "at {ppm:.2} px/mm hole {i} is {d:.0} px out");
            }
            // The residual is the perspective the similarity cannot model
            // (~3% of the span), not a bad lock — but it must stay bounded.
            assert!(m.rms_px < 30.0, "at {ppm:.2} px/mm rms {:.1} px", m.rms_px);
        }
    }

    #[test]
    fn check_rejects_missing_path_and_bad_scale() {
        assert!(check("", &[(1.0, 1.0)], 10.0, &dark(), 2.0).is_err());
        assert!(check("x.png", &[(1.0, 1.0)], 0.0, &dark(), 2.0).is_err());
    }
}
