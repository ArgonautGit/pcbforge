//! Camera→laser calibration: learn where the laser's *commanded* coordinates
//! land in the camera image, so a placement in the camera view can be turned
//! into machine coordinates the laser actually burns at.
//!
//! This is the *workflow* layer above [`vision`]: `vision` owns the primitives
//! (blob detection, homography/affine/lens fits), and this crate composes them
//! into the multi-step bench procedure — print a paper grid, fit the camera
//! lens, anchor the laser to the camera, burn a grid and fit the laser field —
//! plus the acceptance gating that decides whether a fit is good enough to cut
//! with. It contains no egui: it lived in `crates/ui` and so was reachable only
//! from the console, which left the whole fitting pipeline invisible to the CLI.
//!
//! Fiducials tie the design to the board; they do NOT tie the camera to the
//! laser. That second link is what makes "place it here → burn it here" true,
//! and this crate measures it: the operator burns a grid of dots at known
//! commanded coordinates, images it, and we fit a **camera-px → commanded-mm**
//! homography (perspective, so a tilted camera is absorbed).
//!
//! Flow: an initial homography from the four hand-marked corner dots predicts
//! every dot's pixel position; [`vision::find_fiducials`] refines each locally;
//! the full set is re-fit for the final, accurate transform. Because the
//! operator's camera moves between sessions, the fit is cheap to redo and the
//! console flags a stale calibration.

use image::GrayImage;
use nalgebra::{Matrix3, Point2, Vector2};
use vision::{
    BedMap, FidShape, FiducialProfile, FieldMap, FieldVerdict, Homography, LensMap, Poly2,
    classify_field_error, find_fiducials, fit_field, fit_homography, fit_lens,
};

mod square_grid;

/// How the grid dots read against their background. A **printed** reference
/// grid and a burn that darkens the surface (dark-anodized plate) are
/// dark-on-light; an **ablated** mark that brightens a dark surface — or a
/// backlit hole — is bright-on-dark. The operator's ComMarker burns on the
/// dark plate came out light-on-dark, so the anchor step needs `Bright`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DotKind {
    /// Dark dot on a bright field (printed grid, dark-anodized burn).
    #[default]
    Dark,
    /// Bright dot on a dark field (ablated mark, backlit hole).
    Bright,
}

impl DotKind {
    /// The detector profile for this polarity at `dot_mm` diameter.
    fn profile(self, dot_mm: f64) -> FiducialProfile {
        let shape = FidShape::Circle {
            diameter_mm: dot_mm,
        };
        match self {
            DotKind::Dark => FiducialProfile::DarkDot { shape },
            DotKind::Bright => FiducialProfile::Backlit { shape },
        }
    }

    /// A short human label for status lines.
    pub fn label(self) -> &'static str {
        match self {
            DotKind::Dark => "dark-on-light",
            DotKind::Bright => "bright-on-dark",
        }
    }
}

/// The commanded dot grid the operator burns: an `n×n` lattice at `pitch_mm`
/// starting from `origin_mm` (the lower-left dot), in machine mm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridSpec {
    pub origin_mm: (f64, f64),
    pub pitch_mm: f64,
    pub n: usize,
}

impl GridSpec {
    /// All commanded dot centers, row-major (x fastest), machine mm.
    pub fn points(&self) -> Vec<(f64, f64)> {
        let mut v = Vec::with_capacity(self.n * self.n);
        for row in 0..self.n {
            for col in 0..self.n {
                v.push((
                    self.origin_mm.0 + col as f64 * self.pitch_mm,
                    self.origin_mm.1 + row as f64 * self.pitch_mm,
                ));
            }
        }
        v
    }

    /// The four corner dots in commanded mm, ordered lower-left, lower-right,
    /// upper-right, upper-left — the order the operator clicks them.
    pub fn corners_mm(&self) -> [(f64, f64); 4] {
        let m = (self.n.saturating_sub(1)) as f64 * self.pitch_mm;
        let (ox, oy) = self.origin_mm;
        [(ox, oy), (ox + m, oy), (ox + m, oy + m), (ox, oy + m)]
    }
}

/// One detected burned-grid dot's anchor feedback for the overlay: where it was
/// detected in the frame, the commanded machine mm it corresponds to, and how
/// far the fitted anchor lands from that commanded point (µm).
#[derive(Debug, Clone, Copy)]
pub struct AnchorDot {
    /// Where the dot was detected in the camera frame (px).
    pub px: (f64, f64),
    /// The commanded machine coordinate this dot was burned at (mm).
    pub mm: (f64, f64),
    /// Residual of the fitted anchor at this dot: `|px_to_mm(px) − mm|`, µm.
    pub resid_um: f64,
}

/// A fitted camera→laser calibration.
#[derive(Debug, Clone)]
pub struct Calibration {
    /// Camera pixel → commanded machine mm.
    pub px_to_mm: Homography,
    /// Fit residual RMS, µm (in the commanded-mm frame).
    pub rms_um: f64,
    /// Dots detected vs commanded.
    pub found: usize,
    pub total: usize,
    /// Per detected dot, for the overlay (empty for a restored-seed calibration
    /// that hasn't been re-anchored to a live frame yet).
    pub dots: Vec<AnchorDot>,
}

/// A detected dot as `(found_px, grid_mm)`.
type DotPair = (Point2<f64>, Point2<f64>);

/// Detect every grid dot: `mm_to_px_seed` (grid-mm → px) places the local
/// search windows, `find_fiducials` refines each. Returns the pairs for the
/// dots that locked, plus the commanded total.
fn detect_grid_dots(
    frame: &GrayImage,
    mm_to_px_seed: &Homography,
    grid: &GridSpec,
    dot_mm: f64,
    kind: DotKind,
) -> Result<(Vec<DotPair>, usize), String> {
    let mm_from_px: Matrix3<f64> = mm_to_px_seed
        .matrix
        .try_inverse()
        .ok_or("seed homography is singular")?;
    let bed = BedMap::new(mm_from_px).ok_or("seed bed map is singular")?;
    let commanded = grid.points();
    let expected: Vec<Point2<f64>> = commanded.iter().map(|&(x, y)| Point2::new(x, y)).collect();
    let profile = kind.profile(dot_mm);
    // ~0.4·pitch so windows don't overlap, but at least the dot size. This also
    // bounds how far the camera may drift between re-anchors and still lock on.
    let search_mm = (grid.pitch_mm * 0.4).max(dot_mm);
    // Ablated bright burns use the globally constrained square detector first;
    // this is the difficult low-contrast/glare case. Keep the established
    // generic path fast for printed dark/circular fixtures, with the square
    // detector as its fallback when generic coverage is poor.
    if kind == DotKind::Bright {
        let square_pairs = square_grid::detect_square_grid(frame, mm_to_px_seed, grid, dot_mm);
        if square_pairs.len() * 5 >= commanded.len() * 3 {
            return Ok((square_pairs, commanded.len()));
        }
    }
    let results = find_fiducials(frame, &expected, search_mm, &profile, &bed);
    let pairs: Vec<(Point2<f64>, Point2<f64>)> = commanded
        .iter()
        .zip(&results)
        .filter_map(|(&(mx, my), r)| r.as_ref().ok().map(|f| (f.found_px, Point2::new(mx, my))))
        .collect();
    if kind == DotKind::Dark && pairs.len() * 5 < commanded.len() * 3 {
        let square_pairs = square_grid::detect_square_grid(frame, mm_to_px_seed, grid, dot_mm);
        if square_pairs.len() > pairs.len() {
            return Ok((square_pairs, commanded.len()));
        }
    }
    Ok((pairs, commanded.len()))
}

/// Corner homography (grid-mm → px) from the four hand-marked corner dots.
fn corner_seed(corners_px: [(f64, f64); 4], grid: &GridSpec) -> Result<Homography, String> {
    let pairs: Vec<(Point2<f64>, Point2<f64>)> = grid
        .corners_mm()
        .iter()
        .zip(corners_px.iter())
        .map(|(&(mx, my), &(px, py))| (Point2::new(mx, my), Point2::new(px, py)))
        .collect();
    fit_homography(&pairs).map_err(|e| format!("corner fit: {e}"))
}

/// Detect the grid dots via a seed and fit the final camera-px → commanded-mm
/// **homography** (the laser anchor). The seed is the corner homography for a
/// fresh fit, or the previous calibration for a re-anchor.
fn refit(
    frame: &GrayImage,
    mm_to_px_seed: &Homography,
    grid: &GridSpec,
    dot_mm: f64,
    kind: DotKind,
) -> Result<Calibration, String> {
    let (pairs, total) = detect_grid_dots(frame, mm_to_px_seed, grid, dot_mm, kind)?;
    let found = pairs.len();
    if found < 4 {
        return Err(format!(
            "only {found}/{total} grid dots detected — need ≥4 (check the frame, \
             the seed, and the dot size; a big camera move needs a fresh corner fit)"
        ));
    }
    let dots: Vec<AnchorDot> = pairs
        .iter()
        .map(|(fpx, mm)| AnchorDot {
            px: (fpx.x, fpx.y),
            mm: (mm.x, mm.y),
            resid_um: 0.0,
        })
        .collect();
    refit_anchor_dots(&dots, total)
}

/// Refit an anchor after the operator has corrected one or more detected
/// square centers. The grid identity (`mm`) of every point is retained; only
/// its observed pixel center changes.
pub fn refit_anchor_dots(dots: &[AnchorDot], total: usize) -> Result<Calibration, String> {
    if dots.len() < 4 {
        return Err(format!(
            "only {}/{} anchor dots remain — need ≥4",
            dots.len(),
            total
        ));
    }
    if dots.iter().any(|dot| {
        !dot.px.0.is_finite()
            || !dot.px.1.is_finite()
            || !dot.mm.0.is_finite()
            || !dot.mm.1.is_finite()
    }) {
        return Err("anchor correction contains a non-finite coordinate".into());
    }
    let pairs: Vec<DotPair> = dots
        .iter()
        .map(|dot| {
            (
                Point2::new(dot.px.0, dot.px.1),
                Point2::new(dot.mm.0, dot.mm.1),
            )
        })
        .collect();
    let px_to_mm = fit_homography(&pairs).map_err(|e| format!("grid fit: {e}"))?;
    let fitted = dots
        .iter()
        .map(|dot| {
            let got = px_to_mm.apply(Point2::new(dot.px.0, dot.px.1));
            AnchorDot {
                px: dot.px,
                mm: dot.mm,
                resid_um: ((got.x - dot.mm.0).powi(2) + (got.y - dot.mm.1).powi(2)).sqrt() * 1000.0,
            }
        })
        .collect();
    Ok(Calibration {
        rms_um: px_to_mm.rms * 1000.0,
        px_to_mm,
        found: dots.len(),
        total: total.max(dots.len()),
        dots: fitted,
    })
}

// ---- camera lens distortion (printed grid) --------------------------------

/// One dot's calibration feedback for the overlay: where it was detected, the
/// lens **distortion** it exhibits (detected − perspective-predicted, px), and
/// how well the polynomial fit corrected it (µm).
#[derive(Debug, Clone, Copy)]
pub struct LensDot {
    pub px: (f64, f64),
    /// Detected px minus where a pure-perspective (homography) model predicts —
    /// i.e. the lens distortion at this dot, in pixels. Drawn as an arrow.
    pub distort_px: (f64, f64),
    /// Post-fit residual of the polynomial lens map at this dot, µm.
    pub resid_um: f64,
}

/// The camera lens calibration: the metric map plus per-dot feedback.
#[derive(Debug, Clone)]
pub struct CameraCal {
    pub lens: LensMap,
    pub dots: Vec<LensDot>,
    pub found: usize,
    pub total: usize,
}

/// Fit the camera lens-distortion map from a frame of the **printed** reference
/// grid (known `grid` geometry) and the four hand-marked corner dots. Also
/// computes the per-dot distortion field for visual feedback.
pub fn fit_camera_lens(
    frame: &GrayImage,
    corners_px: [(f64, f64); 4],
    grid: &GridSpec,
    dot_mm: f64,
    kind: DotKind,
) -> Result<CameraCal, String> {
    let seed = corner_seed(corners_px, grid)?;
    let (pairs, total) = detect_grid_dots(frame, &seed, grid, dot_mm, kind)?;
    let found = pairs.len();
    if found < 10 {
        return Err(format!(
            "only {found}/{total} grid dots detected — the lens fit needs ≥10 \
             (check the printed grid, the corner clicks, and the dot size)"
        ));
    }
    // Perspective-only model over the same points, to show the distortion the
    // polynomial had to absorb (grid-mm → px).
    let persp = fit_homography(&pairs.iter().map(|&(px, mm)| (mm, px)).collect::<Vec<_>>())
        .map_err(|e| format!("perspective ref: {e}"))?;
    let lens = fit_lens(&pairs).map_err(|e| format!("lens fit: {e}"))?;

    let dots: Vec<LensDot> = pairs
        .iter()
        .zip(&lens.residuals)
        .map(|(&(px, mm), &(_, _, resid_um))| {
            let pred = persp.apply(mm);
            LensDot {
                px: (px.x, px.y),
                distort_px: (px.x - pred.x, px.y - pred.y),
                resid_um,
            }
        })
        .collect();
    Ok(CameraCal {
        lens,
        dots,
        found,
        total,
    })
}

/// Fit the camera→laser homography from a frame of the burned grid and the
/// four hand-marked corner-dot pixel positions (same order as
/// [`GridSpec::corners_mm`]). `dot_mm` sizes the detector and `kind` selects
/// its polarity (an ablated light-on-dark burn needs [`DotKind::Bright`]).
pub fn fit_camera_to_machine(
    frame: &GrayImage,
    corners_px: [(f64, f64); 4],
    grid: &GridSpec,
    dot_mm: f64,
    kind: DotKind,
) -> Result<Calibration, String> {
    if grid.n < 2 {
        return Err("grid must be at least 2×2".into());
    }
    // Initial commanded-mm → px homography from the four corners, then refine
    // every dot and re-fit.
    let corner_pairs: Vec<(Point2<f64>, Point2<f64>)> = grid
        .corners_mm()
        .iter()
        .zip(corners_px.iter())
        .map(|(&(mx, my), &(px, py))| (Point2::new(mx, my), Point2::new(px, py)))
        .collect();
    let seed = fit_homography(&corner_pairs).map_err(|e| format!("corner fit: {e}"))?;
    refit(frame, &seed, grid, dot_mm, kind)
}

/// Re-anchor an existing calibration to a fresh frame — no corner clicks. Uses
/// the previous calibration to place the search windows, so as long as the
/// burned grid is still in view and the camera hasn't jumped more than ~0.4·
/// pitch, it re-locks the dots and re-fits, absorbing camera drift. A bigger
/// move fails and needs a fresh corner fit ([`fit_camera_to_machine`]).
pub fn re_anchor(
    frame: &GrayImage,
    previous: &Calibration,
    grid: &GridSpec,
    dot_mm: f64,
    kind: DotKind,
) -> Result<Calibration, String> {
    let mm_to_px = Homography {
        matrix: previous
            .px_to_mm
            .matrix
            .try_inverse()
            .ok_or("calibration is singular")?,
        residuals: Vec::new(),
        rms: 0.0,
    };
    refit(frame, &mm_to_px, grid, dot_mm, kind)
}

// ---- laser field distortion (burned grid through the metric camera) -------

/// A rigid (rotation + translation, no scale) frame alignment. The ① printed
/// paper grid characterizes the lens *distortion* and the metric *scale*, but
/// its pose in the camera view is arbitrary — it's just taped somewhere on
/// top. The **burned laser grid** is the coordinate reference: this transform
/// carries the paper's metric frame onto the machine's commanded frame, fit
/// from the burned dots. Scale is deliberately excluded so a genuine galvo
/// scale error stays measurable against the paper's printed pitch.
///
/// `flip_x` lets the frame represent a REFLECTION, not just a rotation: some
/// galvo machines mirror the X axis relative to commanded coordinates (a
/// LightBurn axis-negate / galvo mapping), so a burned grid labelled with its
/// TRUE commanded coordinates is the commanded lattice reflected in x. A pure
/// rotation+translation cannot undo that; a reflection can. The convention is
/// `map = R · F`: the reflection `F` (which negates x) is applied FIRST, then
/// the rotation `R`, then the translation. `flip_x == false` is the ordinary
/// proper (det = +1) rigid transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rigid2 {
    pub cos: f64,
    pub sin: f64,
    pub tx: f64,
    pub ty: f64,
    /// When set, negate the source x BEFORE the rotation (`map = R · F`), so the
    /// transform is an improper isometry (a reflection). See the type docs.
    pub flip_x: bool,
}

impl Rigid2 {
    pub const IDENTITY: Rigid2 = Rigid2 {
        cos: 1.0,
        sin: 0.0,
        tx: 0.0,
        ty: 0.0,
        flip_x: false,
    };

    /// Paper mm → machine mm. With `flip_x`, x is negated before the rotation
    /// (`map = R · F`).
    pub fn apply(&self, p: (f64, f64)) -> (f64, f64) {
        let x = if self.flip_x { -p.0 } else { p.0 };
        (
            self.cos * x - self.sin * p.1 + self.tx,
            self.sin * x + self.cos * p.1 + self.ty,
        )
    }

    /// Machine mm → paper mm (rotations invert by transpose). With `flip_x`,
    /// `F` is its own inverse, so un-negate x LAST: `map⁻¹ = F · Rᵀ`.
    pub fn inverse_apply(&self, p: (f64, f64)) -> (f64, f64) {
        let (dx, dy) = (p.0 - self.tx, p.1 - self.ty);
        let rx = self.cos * dx + self.sin * dy;
        let ry = -self.sin * dx + self.cos * dy;
        (if self.flip_x { -rx } else { rx }, ry)
    }

    pub fn is_finite(&self) -> bool {
        [self.cos, self.sin, self.tx, self.ty]
            .iter()
            .all(|v| v.is_finite())
    }

    /// Rotation angle of the `R` factor, degrees — for operator feedback. Note
    /// that with `flip_x` set this is the rotation applied AFTER the x
    /// reflection (`map = R · F`), not the overall det-negative map's "angle".
    pub fn angle_deg(&self) -> f64 {
        self.sin.atan2(self.cos).to_degrees()
    }
}

/// Least-squares PROPER rigid alignment `src → dst` (2-D Kabsch/Procrustes, no
/// scale, det = +1). The reflection-aware [`fit_rigid`] wraps this. Needs ≥2
/// points with non-zero spread.
fn fit_rigid_proper(pairs: &[(Point2<f64>, Point2<f64>)]) -> Result<Rigid2, String> {
    let n = pairs.len();
    if n < 2 {
        return Err(format!("rigid alignment needs ≥2 points, got {n}"));
    }
    let nf = n as f64;
    let (mut ax, mut ay, mut bx, mut by) = (0.0, 0.0, 0.0, 0.0);
    for (a, b) in pairs {
        ax += a.x;
        ay += a.y;
        bx += b.x;
        by += b.y;
    }
    let (ax, ay, bx, by) = (ax / nf, ay / nf, bx / nf, by / nf);
    // θ maximizing Σ (a−a̅)·R(θ)ᵀ(b−b̅): atan2 of the cross/dot accumulators.
    let (mut dot, mut cross, mut spread) = (0.0, 0.0, 0.0_f64);
    for (a, b) in pairs {
        let (ux, uy) = (a.x - ax, a.y - ay);
        let (vx, vy) = (b.x - bx, b.y - by);
        dot += ux * vx + uy * vy;
        cross += ux * vy - uy * vx;
        spread = spread.max(ux.hypot(uy));
    }
    if spread < 1e-9 || (dot == 0.0 && cross == 0.0) {
        return Err("rigid alignment is degenerate: the points have no spread".into());
    }
    let theta = cross.atan2(dot);
    let (sin, cos) = theta.sin_cos();
    let rigid = Rigid2 {
        cos,
        sin,
        tx: bx - (cos * ax - sin * ay),
        ty: by - (sin * ax + cos * ay),
        flip_x: false,
    };
    rigid
        .is_finite()
        .then_some(rigid)
        .ok_or_else(|| "rigid alignment produced non-finite values".into())
}

/// Sum of squared residuals `Σ |transform(src) − dst|²` for a fitted frame.
fn frame_residual_sq(frame: &Rigid2, pairs: &[(Point2<f64>, Point2<f64>)]) -> f64 {
    pairs
        .iter()
        .map(|(a, b)| {
            let (x, y) = frame.apply((a.x, a.y));
            (x - b.x).powi(2) + (y - b.y).powi(2)
        })
        .sum()
}

/// Least-squares rigid alignment `src → dst` with REFLECTION (2-D Procrustes
/// with reflection). Fits both the proper (det = +1) transform and the
/// x-reflected variant (`map = R · F`, F negating src x) and returns whichever
/// has the lower residual, recording the choice in `flip_x`. This lets the
/// paper→machine frame absorb a machine that mirrors X relative to commanded
/// coordinates. Needs ≥2 points with non-zero spread.
pub fn fit_rigid(pairs: &[(Point2<f64>, Point2<f64>)]) -> Result<Rigid2, String> {
    let proper = fit_rigid_proper(pairs)?;
    // Reflected variant: fit the proper transform on x-negated src, then mark
    // flip_x so `apply` negates x itself (`map = R · F` on the ORIGINAL src).
    let reflected_src: Vec<(Point2<f64>, Point2<f64>)> = pairs
        .iter()
        .map(|(a, b)| (Point2::new(-a.x, a.y), *b))
        .collect();
    match fit_rigid_proper(&reflected_src) {
        Ok(mut reflected) => {
            reflected.flip_x = true;
            if frame_residual_sq(&reflected, pairs) < frame_residual_sq(&proper, pairs) {
                Ok(reflected)
            } else {
                Ok(proper)
            }
        }
        Err(_) => Ok(proper),
    }
}

/// A similarity (uniform scale · rotation + translation) alignment `src → dst`.
///
/// Two callers with opposite intent, so read the doc of the one you mean:
/// the metric **calibration anchor** (`FieldCal::paper_to_machine`) stays rigid
/// ([`fit_rigid`]) so a galvo scale error keeps showing up as residual against
/// the printed pitch — there, a similarity is a *diagnostic* (`FieldCal::scale`)
/// and must never absorb the mismatch. The **fiducial board pose**
/// (`ui::fiducial::fit_board_pose`) deliberately does absorb it: the board's
/// measured hole spacing is the thing being registered to, so scale belongs in
/// the fit and is carried into the emitted job.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity2 {
    /// src units → dst units, > 0.
    pub scale: f64,
    /// Rotation + translation applied after scaling.
    pub rigid: Rigid2,
}

impl Similarity2 {
    pub fn apply(&self, p: (f64, f64)) -> (f64, f64) {
        self.rigid.apply((p.0 * self.scale, p.1 * self.scale))
    }

    /// dst → src: undo the rigid part, then the scale. Every constructor goes
    /// through [`fit_similarity_proper`], which rejects a non-positive scale,
    /// so the division is safe for any `Similarity2` this module hands out.
    pub fn inverse_apply(&self, p: (f64, f64)) -> (f64, f64) {
        let r = self.rigid.inverse_apply(p);
        (r.0 / self.scale, r.1 / self.scale)
    }

    /// Rotation angle of the `R` factor, degrees — see [`Rigid2::angle_deg`].
    pub fn angle_deg(&self) -> f64 {
        self.rigid.angle_deg()
    }
}

/// Least-squares PROPER similarity alignment `src → dst` (2-D Procrustes with
/// uniform scale, det = +1). The reflection-aware [`fit_similarity`] wraps this.
/// Needs ≥2 points with non-zero spread.
fn fit_similarity_proper(pairs: &[(Point2<f64>, Point2<f64>)]) -> Result<Similarity2, String> {
    let n = pairs.len();
    if n < 2 {
        return Err(format!("similarity alignment needs ≥2 points, got {n}"));
    }
    let nf = n as f64;
    let (mut ax, mut ay, mut bx, mut by) = (0.0, 0.0, 0.0, 0.0);
    for (a, b) in pairs {
        ax += a.x;
        ay += a.y;
        bx += b.x;
        by += b.y;
    }
    let (ax, ay, bx, by) = (ax / nf, ay / nf, bx / nf, by / nf);
    let (mut dot, mut cross, mut src_sq, mut spread) = (0.0, 0.0, 0.0, 0.0_f64);
    for (a, b) in pairs {
        let (ux, uy) = (a.x - ax, a.y - ay);
        let (vx, vy) = (b.x - bx, b.y - by);
        dot += ux * vx + uy * vy;
        cross += ux * vy - uy * vx;
        src_sq += ux * ux + uy * uy;
        spread = spread.max(ux.hypot(uy));
    }
    if spread < 1e-9 || (dot == 0.0 && cross == 0.0) {
        return Err("similarity alignment is degenerate: the points have no spread".into());
    }
    let scale = dot.hypot(cross) / src_sq;
    if !(scale.is_finite() && scale > 0.0) {
        return Err("similarity alignment produced a non-positive scale".into());
    }
    let theta = cross.atan2(dot);
    let (sin, cos) = theta.sin_cos();
    let rigid = Rigid2 {
        cos,
        sin,
        tx: bx - scale * (cos * ax - sin * ay),
        ty: by - scale * (sin * ax + cos * ay),
        flip_x: false,
    };
    rigid
        .is_finite()
        .then_some(Similarity2 { scale, rigid })
        .ok_or_else(|| "similarity alignment produced non-finite values".into())
}

/// Sum of squared residuals `Σ |sim(src) − dst|²` for a fitted similarity.
fn similarity_residual_sq(sim: &Similarity2, pairs: &[(Point2<f64>, Point2<f64>)]) -> f64 {
    pairs
        .iter()
        .map(|(a, b)| {
            let (x, y) = sim.apply((a.x, a.y));
            (x - b.x).powi(2) + (y - b.y).powi(2)
        })
        .sum()
}

/// Least-squares similarity alignment `src → dst` with REFLECTION (2-D
/// Procrustes with uniform scale AND reflection). Fits both the proper
/// (det = +1) transform and the x-reflected variant (`map = R · F` on the
/// scaled src) and returns whichever has the lower residual, recording the
/// choice in `rigid.flip_x`. Needs ≥2 points with non-zero spread.
pub fn fit_similarity(pairs: &[(Point2<f64>, Point2<f64>)]) -> Result<Similarity2, String> {
    let proper = fit_similarity_proper(pairs)?;
    // Reflected variant: fit the proper similarity on x-negated src, then mark
    // flip_x so `apply` negates the scaled x itself (`map = R · F`).
    let reflected_src: Vec<(Point2<f64>, Point2<f64>)> = pairs
        .iter()
        .map(|(a, b)| (Point2::new(-a.x, a.y), *b))
        .collect();
    match fit_similarity_proper(&reflected_src) {
        Ok(mut reflected) => {
            reflected.rigid.flip_x = true;
            if similarity_residual_sq(&reflected, pairs) < similarity_residual_sq(&proper, pairs) {
                Ok(reflected)
            } else {
                Ok(proper)
            }
        }
        Err(_) => Ok(proper),
    }
}

/// One burned-grid dot's laser-field feedback for the overlay: where it was
/// detected in the frame, the true physical mm it landed at (read through the
/// camera-lens map), the commanded mm it was burned at, and the field error
/// (physical − commanded) it exhibits.
#[derive(Debug, Clone, Copy)]
pub struct FieldDot {
    /// Detected dot center in the camera frame (px).
    pub px: (f64, f64),
    /// Where it physically landed, true mm (camera-lens metric).
    pub physical_mm: (f64, f64),
    /// The commanded machine coordinate it was burned at (mm).
    pub commanded_mm: (f64, f64),
    /// Field error at this dot: `|physical − commanded|`, µm. The RAW measured
    /// error — under [`FieldScale::DistortionOnly`] the uniform scale is
    /// deliberately NOT cancelled, so this stays much larger than `resid_um`;
    /// that gap is the machine's mis-size, reported rather than hidden.
    pub field_um: f64,
    /// Post-fit residual of the field map at this dot, µm. Measured in the same
    /// frame the map was fit in, so it always agrees with `FieldMap::rms_um`
    /// — for the SURVIVING dots. A rejected dot's residual is measured against
    /// the same final map but did not contribute to it, so it reads large; that
    /// is the point, and the overlay draws it distinctly.
    pub resid_um: f64,
    /// Excluded from the field polynomial as an outlier (see
    /// [`FIELD_OUTLIER_K`]). Still detected, still counted in `found`, still
    /// drawn — rejection is not "not found".
    pub rejected: bool,
}

/// A fitted laser field-distortion calibration: the `physical ↔ commanded`
/// pre-distortion map plus per-dot feedback.
#[derive(Debug, Clone)]
pub struct FieldCal {
    /// Physical machine mm → commanded mm (apply to emitted geometry).
    pub field: FieldMap,
    /// Paper-frame mm (the ① lens map's output) → machine mm, anchored to the
    /// burned grid. Every camera↔machine conversion goes through this; the
    /// paper's pose in the view is arbitrary and carries no meaning.
    pub paper_to_machine: Rigid2,
    /// Physical machine mm → camera px (a linear approximation, for the Place
    /// overlay so it positions in the same metric frame the emit corrects in).
    pub to_px: Homography,
    pub dots: Vec<FieldDot>,
    pub found: usize,
    pub total: usize,
    /// Whether the measured field error is genuine radial distortion the
    /// correction will fix, or scatter it won't help with — see
    /// `vision::classify_field_error`.
    pub field_verdict: FieldVerdict,
    /// Measured burn size / commanded size: the uniform similarity scale
    /// between the burned grid (through the ① paper ruler) and the commanded
    /// lattice. `> 1.0` ⇒ the burn reads larger than commanded. Diagnostic
    /// only — the alignment the map is built on is rigid (no scale).
    pub scale: f64,
    /// How many detected burned-grid dots landed OUTSIDE the pixel region the
    /// step-1 camera-lens calibration was fit over (`LensMap::calib_px_bounds`,
    /// expanded 5% each side). The metric ruler extrapolates there, so their
    /// contribution to the field error reads as scatter rather than real
    /// distortion. `0` when the lens map carries no bounds. Per-fit feedback —
    /// not persisted.
    pub extrapolated: usize,
    /// How many detected dots were excluded from the field polynomial as
    /// outliers. `field.rms_um` / `field.max_um` are over the survivors, so a
    /// non-zero count here is the difference between "this fit passed" and
    /// "this fit passed once a dot was thrown away" — the console must show it.
    /// Per-fit feedback, not persisted.
    pub rejected: usize,
    /// Operator-facing sentence about the outlier rejection: what was excluded
    /// and how far out it was, or why nothing was (the cap fired, a boundary
    /// corner was protected, the refit failed). Empty when there is nothing to
    /// say. Per-fit feedback, not persisted.
    pub rejection_note: String,
}

/// Map a camera pixel into the laser's **commanded** coordinate frame by
/// composing the metric camera calibration with the measured laser field.
/// Returns `None` instead of allowing a non-finite calibration value to reach
/// placement or an operator overlay.
pub fn camera_px_to_commanded(
    lens: &LensMap,
    frame: &Rigid2,
    field: &FieldMap,
    px: (f64, f64),
) -> Option<(f64, f64)> {
    let physical = camera_px_to_physical(lens, frame, px)?;
    finite_output(field.to_commanded.apply(physical.0, physical.1))
}

/// Project a laser **commanded** coordinate to the camera by first predicting
/// where the field optics physically land it, then imaging that physical point
/// through the camera lens calibration.
pub fn commanded_to_camera_px(
    lens: &LensMap,
    frame: &Rigid2,
    field: &FieldMap,
    commanded: (f64, f64),
) -> Option<(f64, f64)> {
    finite_input(commanded)?;
    let physical = invert_poly(&field.to_commanded, &field.to_physical, commanded)?;
    physical_to_camera_px(lens, frame, physical)
}

/// Camera pixel to desired physical machine millimeters: through the lens map
/// into the paper's metric frame, then the burned-grid rigid alignment into
/// the machine frame. Field-corrected placement uses this direction: the emit
/// path applies `FieldMap::to_commanded` later, exactly once.
pub fn camera_px_to_physical(lens: &LensMap, frame: &Rigid2, px: (f64, f64)) -> Option<(f64, f64)> {
    finite_input(px)?;
    let paper = finite_output(lens.px_to_mm.apply(px.0, px.1))?;
    finite_output(frame.apply(paper))
}

/// Desired physical machine millimeters to camera pixels (inverse of
/// [`camera_px_to_physical`]). This is the display half of field-corrected
/// placement; composing the field map here as well would double-compensate
/// the job.
pub fn physical_to_camera_px(
    lens: &LensMap,
    frame: &Rigid2,
    physical: (f64, f64),
) -> Option<(f64, f64)> {
    finite_input(physical)?;
    let paper = finite_output(frame.inverse_apply(physical))?;
    invert_poly(&lens.px_to_mm, &lens.mm_to_px, paper)
}

/// Whether every polynomial coefficient needed by the nonlinear camera ↔
/// commanded projection is finite. Callers use this to disable a bad mapping
/// before an overlay or placement starts, rather than falling back silently.
pub fn composed_projection_is_finite(lens: &LensMap, field: &FieldMap) -> bool {
    [
        &lens.px_to_mm,
        &lens.mm_to_px,
        &field.to_commanded,
        &field.to_physical,
    ]
    .into_iter()
    .all(|p| p.to_coeffs().into_iter().all(f64::is_finite))
}

fn finite_input(point: (f64, f64)) -> Option<()> {
    (point.0.is_finite() && point.1.is_finite()).then_some(())
}

fn finite_output(point: (f64, f64)) -> Option<(f64, f64)> {
    (point.0.is_finite() && point.1.is_finite()).then_some(point)
}

/// Numerically invert `forward` at `target`, starting from the separately fit
/// reverse polynomial. The reverse maps are excellent seeds but are not exact
/// algebraic inverses; a few Newton steps remove their edge round-trip error.
fn invert_poly(forward: &Poly2, reverse_seed: &Poly2, target: (f64, f64)) -> Option<(f64, f64)> {
    let mut p = finite_output(reverse_seed.apply(target.0, target.1))?;
    for _ in 0..8 {
        let got = finite_output(forward.apply(p.0, p.1))?;
        let error = (got.0 - target.0, got.1 - target.1);
        if error.0.hypot(error.1) < 1e-9 {
            return Some(p);
        }

        let h = 1e-5 * p.0.abs().max(p.1.abs()).max(1.0);
        let fx = finite_output(forward.apply(p.0 + h, p.1))?;
        let fy = finite_output(forward.apply(p.0, p.1 + h))?;
        let j00 = (fx.0 - got.0) / h;
        let j10 = (fx.1 - got.1) / h;
        let j01 = (fy.0 - got.0) / h;
        let j11 = (fy.1 - got.1) / h;
        let det = j00 * j11 - j01 * j10;
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let dx = (j11 * error.0 - j01 * error.1) / det;
        let dy = (-j10 * error.0 + j00 * error.1) / det;
        p = finite_output((p.0 - dx, p.1 - dy))?;
    }

    let got = finite_output(forward.apply(p.0, p.1))?;
    ((got.0 - target.0).hypot(got.1 - target.1) < 1e-7).then_some(p)
}

/// Gate a real laser-field fit before it is allowed to drive corrected
/// projection or emission. The four boundary corners keep a deceptively good
/// center-only polynomial from being accepted.
pub fn field_live_acceptance(
    cal: &FieldCal,
    grid: &GridSpec,
    accept_rms_um: f64,
    accept_worst_um: f64,
) -> Result<(), String> {
    if cal.total == 0 || cal.found * 5 < cal.total * 4 {
        let pct = if cal.total == 0 {
            0
        } else {
            cal.found * 100 / cal.total
        };
        return Err(format!(
            "detected {}/{} dots ({pct}%); need at least 80%",
            cal.found, cal.total
        ));
    }

    let missing_corners = grid
        .corners_mm()
        .into_iter()
        .filter(|&(x, y)| {
            !cal.dots
                .iter()
                .any(|d| (d.commanded_mm.0 - x).abs() < 1e-6 && (d.commanded_mm.1 - y).abs() < 1e-6)
        })
        .count();
    if missing_corners != 0 {
        return Err(format!(
            "{missing_corners}/4 boundary corners did not lock; recapture before correcting"
        ));
    }
    if !cal.field.rms_um.is_finite()
        || !cal.field.max_um.is_finite()
        || cal.field.rms_um > accept_rms_um
        || cal.field.max_um > accept_worst_um
    {
        return Err(format!(
            "fit residual RMS {:.0} µm, worst {:.0} µm; limits are {accept_rms_um:.0}/{accept_worst_um:.0} µm",
            cal.field.rms_um, cal.field.max_um
        ));
    }
    Ok(())
}

/// Fit the laser field pre-distortion from a frame of the **burned** grid and
/// the four hand-marked corner-dot pixels. `lens` is the camera lens map (from
/// [`fit_camera_lens`]) used purely as a distortion-corrected metric ruler:
/// each burned dot's detected pixel is mapped into the paper's metric frame,
/// rigidly re-anchored to the machine frame via the burned grid itself
/// ([`fit_rigid`]), paired with the commanded coordinate it was burned at, and
/// a `physical → commanded` polynomial is fit. Emitting geometry through it
/// cancels the field distortion.
///
/// The printed paper's position/rotation in the view is arbitrary — only its
/// pitch (metric scale) and the lens curvature it characterizes matter. The
/// camera must not have moved between the lens fit and this frame.
/// `|scale − 1|` above this is a setup error (wrong pitch entered at ①/③, the
/// camera moved or zoomed since ①, the paper not lying in the burn plane, or a
/// machine field-size misconfiguration) — the fit fails early rather than
/// producing a garbage rejection. Genuine galvo scale errors are ≲1–2%.
pub const FIELD_SCALE_FAIL_FRAC: f64 = 0.05;
/// `|scale − 1|` above this is surfaced to the operator but still fittable
/// (the field polynomial's linear terms absorb it).
pub const FIELD_SCALE_NOTE_FRAC: f64 = 0.01;
/// Stable marker substring in the scale-gate error, so the UI can recognize it
/// without matching prose.
pub const FIELD_SCALE_ERR_MARKER: &str = "burn-vs-paper scale";

/// What the ③ fit does about a large uniform burn-vs-paper scale (a machine
/// whose configured field size is wrong, or a setup mistake that reads like
/// one). The mirror guard is independent of this choice: a scrambled corner
/// order cannot be fixed by scale + rotation + translation + reflection, so it
/// is caught in every mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldScale {
    /// Refuse the fit above [`FIELD_SCALE_FAIL_FRAC`]. A gross scale mismatch is
    /// far more often a setup error than a real field, so this is the default.
    Refuse,
    /// Absorb the scale into the field polynomial's linear terms. Shapes burn
    /// dimensionally true, but the commanded coordinates are stretched by
    /// `1/scale` — a machine reading 32% small needs 132 mm commanded for 90 mm
    /// physical, so the usable work area shrinks by the same factor. The
    /// machine's speeds and hatch spacing also stay in its own units, so energy
    /// density shifts; the UI warns and records the choice.
    Compensate,
    /// Fit and apply the NON-UNIFORM distortion only: the uniform scale is
    /// divided out of the fit targets and merely reported (`FieldCal::scale`),
    /// so the resulting map has unit magnification — commanding X mm asks for
    /// X mm plus the distortion correction, and the whole configured work area
    /// stays addressable. The scale gate never fires in this mode.
    DistortionOnly,
}

/// Outlier cut for the ③ field fit, in robust sigmas above the MEDIAN residual.
///
/// The cut is `median + K·sigma`, not `K·sigma` about zero: `resid_um` is a
/// Euclidean magnitude, so its distribution sits well away from zero and a
/// zero-centred cut is far too aggressive. On the case this was built for (49
/// dots, median residual ≈ 90 µm, MAD-sigma ≈ 40 µm) a zero-centred 3.5-sigma
/// cut lands at ~155 µm and would eat perfectly healthy 160–200 µm dots.
///
/// 3.5 sits in the conventional 3–4 band. Below 3 the ordinary upper tail of a
/// 50-dot grid starts tripping it; above 4 a single 2 mm scuff on a grid that
/// is otherwise loose can survive.
pub const FIELD_OUTLIER_K: f64 = 3.5;

/// Absolute floor on the outlier cut, µm — nothing below this is ever rejected.
///
/// A near-perfect grid has a tiny MAD-sigma, and `median + K·sigma` would then
/// start rejecting healthy dots for being merely ordinary. The floor is tied to
/// the reason rejection exists at all: a dot whose residual is inside the
/// WORST-DOT acceptance limit can never be why a fit was refused, so there is
/// nothing to gain by dropping it. This is the console's default
/// `accept_worst_um` (see [`field_live_acceptance`]). Those limits are
/// operator-configurable and are deliberately NOT threaded into the fit — the
/// coupling is by value, so that lowering the acceptance limits tightens what
/// must be met without also licensing the fit to discard more evidence.
pub const FIELD_OUTLIER_FLOOR_UM: f64 = 250.0;

/// Largest share of the detected dots the fit will ever exclude.
///
/// Above this it is not a scuff on the paper — it is a bad capture, a bad
/// corner-click order, or a field the bi-cubic does not describe. Deleting an
/// eighth of the evidence to make a laser calibration pass is exactly the
/// failure this guard exists to prevent, so when the cut would take more than
/// this share NOTHING is dropped and the existing gates refuse as before. 10%
/// still covers several genuinely bad dots on a realistic 49–81 dot grid.
pub const FIELD_OUTLIER_MAX_FRAC: f64 = 0.10;

/// Reject/refit passes. The cut comes from a robust spread, so the first pass
/// already sees a sigma the outliers have not inflated and the second finds
/// nothing in practice; each further pass only adds a way for the dot set to
/// erode one dot at a time until whatever is left agrees with itself.
const FIELD_OUTLIER_PASSES: usize = 2;

/// Median of `v`, reordering it. Used for both the median residual and the MAD.
fn median_in_place(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Per-pair residual of `map` in µm, in the frame the map was fit in.
fn field_resid_um(map: &FieldMap, pairs: &[(Point2<f64>, Point2<f64>)]) -> Vec<f64> {
    pairs
        .iter()
        .map(|(p, c)| {
            let (ex, ey) = map.to_commanded.apply(p.x, p.y);
            ((ex - c.x).powi(2) + (ey - c.y).powi(2)).sqrt() * 1000.0
        })
        .collect()
}

/// The field map plus which dots it was fit over and what to tell the operator.
struct RobustFieldFit {
    field: FieldMap,
    /// Parallel to the fit pairs: `false` = excluded from `field`.
    keep: Vec<bool>,
    note: String,
}

/// Refit `base` with outlying dots excluded, so one bad dot cannot veto an
/// otherwise-good calibration.
///
/// Spread is the MAD (`1.4826·median|r − median r|`), never the standard
/// deviation — the SD is inflated by the very outliers being looked for, which
/// is how a 2 mm dot hides itself. `is_corner` marks the four boundary corner
/// dots: if any of them is an outlier, rejection is suspended altogether — the
/// corner gate exists to constrain the fit where distortion is largest, and
/// fitting around a bad corner (by deleting it, or by deleting the neighbours
/// that were holding the polynomial away from it) defeats it either way.
///
/// Every path returns a usable fit. A refit that fails, or a cut that would
/// take too many dots, falls back to `base` over all dots and says so — the
/// operator still gets numbers to read, and the existing gates still refuse.
fn reject_field_outliers(
    fit_pairs: &[(Point2<f64>, Point2<f64>)],
    base: FieldMap,
    is_corner: &[bool],
    extrapolated_dot: &[bool],
) -> RobustFieldFit {
    let n = fit_pairs.len();
    let unrejected = |note: String| RobustFieldFit {
        field: base.clone(),
        keep: vec![true; n],
        note,
    };
    let max_drop = (FIELD_OUTLIER_MAX_FRAC * n as f64).floor() as usize;

    let mut keep = vec![true; n];
    let mut field = base.clone();
    let mut dropped: Vec<usize> = Vec::new();
    let mut final_resid = field_resid_um(&field, fit_pairs);

    for _ in 0..FIELD_OUTLIER_PASSES {
        let resid = field_resid_um(&field, fit_pairs);
        let mut surviving: Vec<f64> = (0..n).filter(|&i| keep[i]).map(|i| resid[i]).collect();
        let median = median_in_place(&mut surviving);
        let mut deviations: Vec<f64> = surviving.iter().map(|r| (r - median).abs()).collect();
        let sigma = 1.4826 * median_in_place(&mut deviations);
        let cut = (median + FIELD_OUTLIER_K * sigma).max(FIELD_OUTLIER_FLOOR_UM);

        let mut bad_corners: Vec<f64> = Vec::new();
        let mut flagged = Vec::new();
        for i in 0..n {
            if !keep[i] || resid[i] <= cut {
                continue;
            }
            if is_corner[i] {
                bad_corners.push(resid[i]);
            } else {
                flagged.push(i);
            }
        }
        // An off-lattice boundary corner suspends rejection ENTIRELY — not just
        // for that corner. Dropping its neighbours would remove the constraints
        // holding the polynomial away from it, the fit would bend toward the bad
        // corner, and its residual would fall back under the acceptance limit:
        // the same defeat the corner gate exists to prevent, arriving by
        // deleting the witnesses instead of the corner. Nothing is dropped, and
        // the note names the corner so the accept/reject stays honest.
        if !bad_corners.is_empty() {
            return unrejected(format!(
                "{} of the 4 boundary corner dots are outliers ({}) — outlier rejection is \
                 SUSPENDED and nothing was excluded. The corner gate exists to constrain the fit \
                 where distortion is largest, so a bad corner has to fail the fit rather than be \
                 fitted around; recapture those corners.",
                bad_corners.len(),
                bad_corners
                    .iter()
                    .map(|r| format!("{r:.0} µm"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if flagged.is_empty() {
            break;
        }
        if dropped.len() + flagged.len() > max_drop {
            let total = dropped.len() + flagged.len();
            return unrejected(format!(
                "{total} of {n} fitted dots read as outliers (over the {:.0}% cap) — that is a bad \
                 capture or a model that doesn't describe this field, not stray dots, so NOTHING \
                 was excluded and the residuals below are over all {n}",
                FIELD_OUTLIER_MAX_FRAC * 100.0
            ));
        }

        for &i in &flagged {
            keep[i] = false;
        }
        dropped.extend_from_slice(&flagged);
        let survivors: Vec<(Point2<f64>, Point2<f64>)> =
            (0..n).filter(|&i| keep[i]).map(|i| fit_pairs[i]).collect();
        match fit_field(&survivors) {
            Ok(refit) => {
                field = refit;
                final_resid = field_resid_um(&field, fit_pairs);
            }
            Err(e) => {
                return unrejected(format!(
                    "{} dot(s) read as outliers but the refit without them failed ({e}); nothing \
                     was excluded and the residuals below are over all {n}",
                    dropped.len()
                ));
            }
        }
    }

    if dropped.is_empty() {
        return RobustFieldFit {
            field,
            keep,
            note: String::new(),
        };
    }

    dropped.sort_unstable_by(|&a, &b| final_resid[b].total_cmp(&final_resid[a]));
    let residuals = dropped
        .iter()
        .map(|&i| format!("{:.0} µm", final_resid[i]))
        .collect::<Vec<_>>()
        .join(", ");
    // A dot that is BOTH an outlier and outside the ① lens-calibrated region is
    // the signature of the metric ruler extrapolating, not of the laser field
    // curving — different fix, so name it.
    let extrap = dropped.iter().filter(|&&i| extrapolated_dot[i]).count();
    let extrap_note = if extrap > 0 {
        format!(
            " {extrap} of them also lie OUTSIDE the region the step-1 lens calibration covered, \
             where the metric ruler extrapolates — that combination points at a ruler artefact \
             rather than field distortion: print/calibrate a larger paper grid, or shrink the \
             burn grid."
        )
    } else {
        String::new()
    };
    RobustFieldFit {
        field,
        keep,
        note: format!(
            "{} of {n} dots EXCLUDED from the fit as outliers (residual {residuals}); the RMS/worst \
             above are over the remaining {}.{extrap_note}",
            dropped.len(),
            n - dropped.len()
        ),
    }
}

pub fn fit_laser_field(
    frame: &GrayImage,
    corners_px: [(f64, f64); 4],
    grid: &GridSpec,
    dot_mm: f64,
    kind: DotKind,
    lens: &LensMap,
    field_scale: FieldScale,
) -> Result<FieldCal, String> {
    if grid.n < 4 {
        return Err("laser-field fit needs at least a 4×4 grid".into());
    }
    let seed = corner_seed(corners_px, grid)?;
    let (pairs, total) = detect_grid_dots(frame, &seed, grid, dot_mm, kind)?;
    let found = pairs.len();
    if found < 10 {
        return Err(format!(
            "only {found}/{total} grid dots detected — the field fit needs ≥10 \
             (check the burned grid, the corner clicks, the dot size, and polarity)"
        ));
    }
    // Count detected dots whose pixels fall outside the region the step-1 lens
    // calibration was fit over. Reading a burn through the ruler out there
    // extrapolates the bi-cubic unreliably, and that error lands silently in the
    // field fit's residuals/scatter — so surface it. The box is grown 5% of its
    // own span on each side: extrapolation right at the edge is harmless.
    // Kept per dot, not just counted: an excluded dot that ALSO sits out here is
    // the signature of ruler extrapolation rather than field distortion.
    let extrapolated_dot: Vec<bool> = match lens.calib_px_bounds {
        Some([min_x, min_y, max_x, max_y]) => {
            let mx = (max_x - min_x) * 0.05;
            let my = (max_y - min_y) * 0.05;
            let (lo_x, hi_x) = (min_x - mx, max_x + mx);
            let (lo_y, hi_y) = (min_y - my, max_y + my);
            pairs
                .iter()
                .map(|(fpx, _)| fpx.x < lo_x || fpx.x > hi_x || fpx.y < lo_y || fpx.y > hi_y)
                .collect()
        }
        None => vec![false; pairs.len()],
    };
    let extrapolated = extrapolated_dot.iter().filter(|e| **e).count();
    // Detected px → paper-frame mm through the lens ruler. The printed paper
    // only characterizes distortion + metric scale; its pose in the view is
    // arbitrary (it's taped on top). The BURNED GRID anchors the coordinate
    // frame: fit the rigid paper→machine alignment from the burned dots, so
    // the residual against the commanded lattice is genuine field error, not
    // where the operator happened to tape the paper.
    let paper_pairs: Vec<(Point2<f64>, Point2<f64>)> = pairs
        .iter()
        .map(|(fpx, cmd)| {
            let (phx, phy) = lens.px_to_mm.apply(fpx.x, fpx.y);
            (Point2::new(phx, phy), Point2::new(cmd.x, cmd.y))
        })
        .collect();
    // Scale sanity gate, BEFORE the mirror guard: a large uniform scale mismatch
    // (wrong paper pitch at ①, camera moved since ①, paper out of the burn
    // plane, machine field size) is a setup error unless the operator has picked
    // a mode that handles it — `Compensate` absorbs it in the field polynomial's
    // linear terms, `DistortionOnly` divides it out of the fit targets. `scale`
    // records it in every mode.
    let sim =
        fit_similarity(&paper_pairs).map_err(|e| format!("burned-grid frame alignment: {e}"))?;
    // sim is measured → commanded, so measured/commanded is its inverse.
    let scale = 1.0 / sim.scale;
    if field_scale == FieldScale::Refuse && (scale - 1.0).abs() > FIELD_SCALE_FAIL_FRAC {
        return Err(format!(
            "{FIELD_SCALE_ERR_MARKER} is off by {:+.1}% — a setup error, not field distortion. \
             Likeliest causes, in order: the pitch entered at step 1 wasn't the paper's MEASURED \
             pitch (or step 3's isn't the commanded one); the camera moved or zoomed since step 1; the \
             printed paper wasn't lying in the burn plane; the machine's field-size setting \
             (LightBurn/EZCAD). Fix the setup, re-run step 1, then step 3 (or pick \
             \"compensate machine scale\" to absorb it in software, or \"correct distortion \
             only\" to keep the work area at 1:1 and fix the curvature alone)",
            (scale - 1.0) * 100.0
        ));
    }
    // Correspondence guard on the SIMILARITY residual, in BOTH modes. The
    // similarity fit now absorbs a REFLECTION (`fit_similarity` tries the
    // x-mirrored variant too), so a genuinely mirrored machine — burned grid
    // labelled with its TRUE commanded coordinates — no longer trips this: it's
    // a reflection the fit represents, and `paper_to_machine.flip_x` records it.
    // What still leaves a large residual is a correspondence that is NOT a
    // similarity-with-reflection at all: a corner-click order that scrambles the
    // labels into a non-isometry (or any other mislabelling / shear). Scale +
    // rotation + translation + reflection cannot undo that, so the guard stays
    // as the backstop, even when a genuine scale is legitimately absorbed.
    let sim_rms_mm = (paper_pairs
        .iter()
        .map(|(p, c)| {
            let (sx, sy) = sim.apply((p.x, p.y));
            (sx - c.x).powi(2) + (sy - c.y).powi(2)
        })
        .sum::<f64>()
        / paper_pairs.len() as f64)
        .sqrt();
    if sim_rms_mm > grid.pitch_mm {
        return Err(format!(
            "burned-grid frame alignment left {sim_rms_mm:.1} mm RMS after scale+rotation+reflection \
             (≥ one grid pitch) — the dots don't match the commanded lattice; check the corner \
             click order against the grid's orientation markers (LL is the corner nearest the lone \
             diagonal marker; the edge with the midpoint marker is the bottom)"
        ));
    }
    let paper_to_machine =
        fit_rigid(&paper_pairs).map_err(|e| format!("burned-grid frame alignment: {e}"))?;
    let field_pairs: Vec<(Point2<f64>, Point2<f64>)> = paper_pairs
        .iter()
        .map(|(paper, cmd)| {
            let (mx, my) = paper_to_machine.apply((paper.x, paper.y));
            (Point2::new(mx, my), *cmd)
        })
        .collect();
    // `DistortionOnly`: strip the uniform scale from the FIELD POLYNOMIAL'S FIT
    // TARGETS, and nowhere else. It is deliberately NOT taken out of
    // `paper_to_machine` (that stays `fit_rigid`) because that alignment is also
    // the camera projection's metric anchor — `camera_px_to_physical` reads true
    // mm through it, and absorbing a scale factor there would silently rescale
    // fiducial measurement, placement and the overlay. Removing it here instead
    // leaves the polynomial learning the non-uniform component alone, so the map
    // comes out at unit magnification and the whole work area stays addressable.
    //
    // The scale is divided out about the COMMANDED LATTICE'S CENTROID, which
    // `fit_rigid` maps the measured centroid onto exactly — so the centroid is
    // an exact fixed point and the normalization adds no translation. That
    // assumes the burned grid is centred on the scan field; the console already
    // warns when it is well off the configured field centre.
    let distortion_pairs = (field_scale == FieldScale::DistortionOnly).then(|| {
        let n = field_pairs.len() as f64;
        let cx = field_pairs.iter().map(|(_, c)| c.x).sum::<f64>() / n;
        let cy = field_pairs.iter().map(|(_, c)| c.y).sum::<f64>() / n;
        field_pairs
            .iter()
            .map(|(phys, cmd)| {
                (
                    Point2::new(cx + (phys.x - cx) / scale, cy + (phys.y - cy) / scale),
                    *cmd,
                )
            })
            .collect::<Vec<_>>()
    });
    let fit_pairs = distortion_pairs.as_deref().unwrap_or(&field_pairs);
    let base = fit_field(fit_pairs).map_err(|e| format!("field fit: {e}"))?;
    // Robust refit: one scuff on the paper must not veto a whole calibration.
    // Only the FIELD POLYNOMIAL is refit on the survivors. `paper_to_machine`,
    // `sim` and `scale` are left over all dots on purpose: one outlier perturbs
    // them by a rigid/uniform-scale amount, and the field polynomial absorbs
    // constant and linear terms exactly, so the survivors' residuals are
    // unchanged by it — refitting them would only make the anchor the overlay
    // and placement share disagree with the dots it was measured from.
    let is_corner: Vec<bool> = pairs
        .iter()
        .map(|(_, cmd)| {
            grid.corners_mm()
                .into_iter()
                .any(|(x, y)| (cmd.x - x).abs() < 1e-6 && (cmd.y - y).abs() < 1e-6)
        })
        .collect();
    let robust = reject_field_outliers(fit_pairs, base, &is_corner, &extrapolated_dot);
    let field = robust.field;
    let rejected = robust.keep.iter().filter(|k| !**k).count();
    let survivor = |i: usize| robust.keep[i];
    // A linear physical-mm → px map for the Place overlay: fit a homography
    // from each dot's physical position to its detected pixel. Over the
    // survivors too — an excluded dot is a bad measurement wherever it is read.
    let to_px_pairs: Vec<(Point2<f64>, Point2<f64>)> = pairs
        .iter()
        .zip(&field_pairs)
        .enumerate()
        .filter(|(i, _)| survivor(*i))
        .map(|(_, ((fpx, _), (phys, _)))| (*phys, *fpx))
        .collect();
    let to_px = fit_homography(&to_px_pairs).map_err(|e| format!("overlay fit: {e}"))?;

    // Classified over the survivors as well: a 2 mm outlier reads as scatter and
    // would have the verdict tell the operator "correction won't help" about a
    // fit that was just accepted.
    let field_verdict = classify_field_error(
        &field_pairs
            .iter()
            .enumerate()
            .filter(|(i, _)| survivor(*i))
            .map(|(_, (phys, cmd))| (*cmd, Vector2::new(phys.x - cmd.x, phys.y - cmd.y)))
            .collect::<Vec<_>>(),
    );

    // `physical_mm`/`field_um` report the RAW measurement; `resid_um` reports the
    // fit, so it is evaluated on the same points the fit saw (identical to
    // `field_pairs` outside `DistortionOnly`) and always agrees with `rms_um`.
    let dots: Vec<FieldDot> = pairs
        .iter()
        .zip(&field_pairs)
        .zip(fit_pairs)
        .enumerate()
        .map(|(i, (((fpx, cmd), (phys, _)), (fit_phys, _)))| {
            let field_um = ((phys.x - cmd.x).powi(2) + (phys.y - cmd.y).powi(2)).sqrt() * 1000.0;
            let (gx, gy) = field.precompensate(fit_phys.x, fit_phys.y);
            let resid_um = ((gx - cmd.x).powi(2) + (gy - cmd.y).powi(2)).sqrt() * 1000.0;
            FieldDot {
                px: (fpx.x, fpx.y),
                physical_mm: (phys.x, phys.y),
                commanded_mm: (cmd.x, cmd.y),
                field_um,
                resid_um,
                rejected: !survivor(i),
            }
        })
        .collect();
    Ok(FieldCal {
        field,
        paper_to_machine,
        to_px,
        dots,
        found,
        total,
        field_verdict,
        scale,
        extrapolated,
        rejected,
        rejection_note: robust.note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render the grid as anti-aliased dark discs on a bright field, through
    /// `mm_to_px` (commanded-mm → px) with dots ~`dot_mm`·10 px across.
    fn render_grid(
        grid: &GridSpec,
        mm_to_px: &Homography,
        dot_mm: f64,
        w: u32,
        h: u32,
    ) -> GrayImage {
        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(mx, my)| {
                let p = mm_to_px.apply(Point2::new(mx, my));
                (p.x, p.y, dot_mm * 10.0)
            })
            .collect();
        GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        })
    }

    /// (commanded-mm, pixel) corner pair.
    type MmPx = ((f64, f64), (f64, f64));

    fn homog(pairs: &[MmPx]) -> Homography {
        let p: Vec<_> = pairs
            .iter()
            .map(|&((mx, my), (px, py))| (Point2::new(mx, my), Point2::new(px, py)))
            .collect();
        fit_homography(&p).unwrap()
    }

    fn affine_poly(ax: [f64; 3], ay: [f64; 3]) -> vision::Poly2 {
        let mut c = [0.0; 23];
        c[0] = ax[2];
        c[1] = ax[0];
        c[2] = ax[1];
        c[10] = ay[2];
        c[11] = ay[0];
        c[12] = ay[1];
        c[22] = 1.0;
        vision::Poly2::from_coeffs(&c).expect("test poly has a valid normalization")
    }

    fn affine_maps() -> (LensMap, FieldMap) {
        // Camera: physical x=(px-20)/10, y=(800-py)/10.
        let lens = LensMap {
            px_to_mm: affine_poly([0.1, 0.0, -2.0], [0.0, -0.1, 80.0]),
            mm_to_px: affine_poly([10.0, 0.0, 20.0], [0.0, -10.0, 800.0]),
            rms_um: 0.0,
            max_um: 0.0,
            residuals: vec![],
            calib_px_bounds: None,
        };
        // Field: command (x,y) lands physically at (1.02x+1, 0.98y-2).
        let field = FieldMap {
            to_commanded: affine_poly(
                [1.0 / 1.02, 0.0, -1.0 / 1.02],
                [0.0, 1.0 / 0.98, 2.0 / 0.98],
            ),
            to_physical: affine_poly([1.02, 0.0, 1.0], [0.0, 0.98, -2.0]),
            rms_um: 0.0,
            max_um: 0.0,
        };
        (lens, field)
    }

    #[test]
    fn composed_camera_command_mapping_round_trips() {
        let (lens, field) = affine_maps();
        assert!(composed_projection_is_finite(&lens, &field));
        let commanded = (37.0, 24.0);
        // A non-trivial paper→machine anchor: 90° rotation + offset.
        let frame = Rigid2 {
            cos: 0.0,
            sin: 1.0,
            tx: 5.0,
            ty: -3.0,
            flip_x: false,
        };
        let px =
            commanded_to_camera_px(&lens, &frame, &field, commanded).expect("finite projection");
        let got = camera_px_to_commanded(&lens, &frame, &field, px).expect("finite inverse");
        assert!((got.0 - commanded.0).abs() < 1e-9);
        assert!((got.1 - commanded.1).abs() < 1e-9);

        let physical = camera_px_to_physical(&lens, &frame, px).expect("metric camera");
        let px2 = physical_to_camera_px(&lens, &frame, physical).expect("camera projection");
        assert!((px2.0 - px.0).abs() < 1e-9 && (px2.1 - px.1).abs() < 1e-9);
    }

    /// The rigid fit recovers a known rotation+translation (proper, det = +1)
    /// and refuses degenerate input; apply/inverse_apply round-trip.
    #[test]
    fn rigid_alignment_recovers_pose_and_round_trips() {
        let truth = Rigid2 {
            cos: (0.3_f64).cos(),
            sin: (0.3_f64).sin(),
            tx: 12.5,
            ty: -7.25,
            flip_x: false,
        };
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = (0..5)
            .flat_map(|r| (0..5).map(move |c| (c as f64 * 10.0, r as f64 * 10.0)))
            .map(|p| {
                let q = truth.apply(p);
                (Point2::new(p.0, p.1), Point2::new(q.0, q.1))
            })
            .collect();
        let fit = fit_rigid(&pairs).expect("rigid fit");
        assert!(
            !fit.flip_x,
            "a proper transform is not flagged as reflected"
        );
        assert!((fit.cos - truth.cos).abs() < 1e-12);
        assert!((fit.sin - truth.sin).abs() < 1e-12);
        assert!((fit.tx - truth.tx).abs() < 1e-9);
        assert!((fit.ty - truth.ty).abs() < 1e-9);

        let p = (3.7, -1.2);
        let round = fit.inverse_apply(fit.apply(p));
        assert!((round.0 - p.0).abs() < 1e-12 && (round.1 - p.1).abs() < 1e-12);

        let coincident: Vec<_> = (0..4)
            .map(|_| (Point2::new(1.0, 1.0), Point2::new(2.0, 2.0)))
            .collect();
        assert!(fit_rigid(&coincident).is_err(), "no spread is refused");
    }

    /// The rigid fit recovers a known REFLECTED transform (`map = R · F`, F
    /// negating x): `flip_x` is set and the parameters are recovered, and
    /// apply/inverse_apply still round-trip through the flip.
    #[test]
    fn rigid_alignment_recovers_a_reflection() {
        let truth = Rigid2 {
            cos: (-0.4_f64).cos(),
            sin: (-0.4_f64).sin(),
            tx: -3.5,
            ty: 8.0,
            flip_x: true,
        };
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = (0..5)
            .flat_map(|r| (0..5).map(move |c| (c as f64 * 10.0, r as f64 * 10.0)))
            .map(|p| {
                let q = truth.apply(p);
                (Point2::new(p.0, p.1), Point2::new(q.0, q.1))
            })
            .collect();
        let fit = fit_rigid(&pairs).expect("rigid fit");
        assert!(fit.flip_x, "the reflected variant is chosen");
        assert!((fit.cos - truth.cos).abs() < 1e-12);
        assert!((fit.sin - truth.sin).abs() < 1e-12);
        assert!((fit.tx - truth.tx).abs() < 1e-9);
        assert!((fit.ty - truth.ty).abs() < 1e-9);
        // The fitted reflected frame reproduces the truth on every point.
        for (a, b) in &pairs {
            let (x, y) = fit.apply((a.x, a.y));
            assert!((x - b.x).abs() < 1e-9 && (y - b.y).abs() < 1e-9);
        }
        // inverse_apply is the exact inverse through the flip.
        let p = (3.7, -1.2);
        let round = fit.inverse_apply(fit.apply(p));
        assert!((round.0 - p.0).abs() < 1e-9 && (round.1 - p.1).abs() < 1e-9);
    }

    /// The similarity fit recovers a known scale·rotation+translation and
    /// refuses degenerate input.
    #[test]
    fn similarity_alignment_recovers_scale_rotation_translation() {
        let truth = Similarity2 {
            scale: 1.35,
            rigid: Rigid2 {
                cos: (0.3_f64).cos(),
                sin: (0.3_f64).sin(),
                tx: 12.5,
                ty: -7.25,
                flip_x: false,
            },
        };
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = (0..5)
            .flat_map(|r| (0..5).map(move |c| (c as f64 * 10.0, r as f64 * 10.0)))
            .map(|p| {
                let q = truth.apply(p);
                (Point2::new(p.0, p.1), Point2::new(q.0, q.1))
            })
            .collect();
        let fit = fit_similarity(&pairs).expect("similarity fit");
        assert!(!fit.rigid.flip_x, "a proper similarity is not reflected");
        assert!((fit.scale - truth.scale).abs() < 1e-12);
        assert!((fit.rigid.cos - truth.rigid.cos).abs() < 1e-12);
        assert!((fit.rigid.sin - truth.rigid.sin).abs() < 1e-12);
        assert!((fit.rigid.tx - truth.rigid.tx).abs() < 1e-9);
        assert!((fit.rigid.ty - truth.rigid.ty).abs() < 1e-9);

        assert!(fit_similarity(&pairs[..1]).is_err(), "<2 points refused");
        let coincident: Vec<_> = (0..4)
            .map(|_| (Point2::new(1.0, 1.0), Point2::new(2.0, 2.0)))
            .collect();
        assert!(fit_similarity(&coincident).is_err(), "no spread is refused");
    }

    /// The similarity fit recovers a known REFLECTED scale·rotation+translation
    /// (`map = R · F` on the scaled src): `flip_x` is set and every parameter is
    /// recovered.
    #[test]
    fn similarity_alignment_recovers_a_reflection() {
        let truth = Similarity2 {
            scale: 0.8,
            rigid: Rigid2 {
                cos: (1.1_f64).cos(),
                sin: (1.1_f64).sin(),
                tx: 4.0,
                ty: -2.0,
                flip_x: true,
            },
        };
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = (0..5)
            .flat_map(|r| (0..5).map(move |c| (c as f64 * 10.0, r as f64 * 10.0)))
            .map(|p| {
                let q = truth.apply(p);
                (Point2::new(p.0, p.1), Point2::new(q.0, q.1))
            })
            .collect();
        let fit = fit_similarity(&pairs).expect("similarity fit");
        assert!(fit.rigid.flip_x, "the reflected variant is chosen");
        assert!((fit.scale - truth.scale).abs() < 1e-12);
        assert!((fit.rigid.cos - truth.rigid.cos).abs() < 1e-12);
        assert!((fit.rigid.sin - truth.rigid.sin).abs() < 1e-12);
        assert!((fit.rigid.tx - truth.rigid.tx).abs() < 1e-9);
        assert!((fit.rigid.ty - truth.rigid.ty).abs() < 1e-9);
        for (a, b) in &pairs {
            let (x, y) = fit.apply((a.x, a.y));
            assert!((x - b.x).abs() < 1e-9 && (y - b.y).abs() < 1e-9);
        }
    }

    /// `Similarity2::inverse_apply` is the exact inverse of `apply` for BOTH
    /// reflection states at a non-unit scale — the fiducial placement carry
    /// math maps a bed point back through the previous fit with it, so an
    /// inverse that is only right for `flip_x == false` would silently drift
    /// the operator's manual offset on the back face.
    #[test]
    fn similarity_inverse_round_trips_through_scale_and_reflection() {
        for flip_x in [false, true] {
            let sim = Similarity2 {
                scale: 1.04,
                rigid: Rigid2 {
                    cos: (0.42_f64).cos(),
                    sin: (0.42_f64).sin(),
                    tx: 30.0,
                    ty: -11.5,
                    flip_x,
                },
            };
            for p in [(0.0, 0.0), (17.0, -4.0), (-25.5, 60.25)] {
                let round = sim.inverse_apply(sim.apply(p));
                assert!(
                    (round.0 - p.0).abs() < 1e-9 && (round.1 - p.1).abs() < 1e-9,
                    "flip_x={flip_x}: {p:?} → {round:?}"
                );
                // …and the other way round, so neither direction is privileged.
                let back = sim.apply(sim.inverse_apply(p));
                assert!((back.0 - p.0).abs() < 1e-9 && (back.1 - p.1).abs() < 1e-9);
            }
            // The scale really is undone, not folded into the rigid part.
            let far = sim.apply((10.0, 0.0));
            let near = sim.apply((0.0, 0.0));
            assert!(
                (far.0.hypot(far.1 - 0.0) - near.0.hypot(near.1)).abs() > 0.0,
                "sanity"
            );
            assert!(
                ((far.0 - near.0).hypot(far.1 - near.1) - 10.0 * sim.scale).abs() < 1e-9,
                "a 10 mm src span maps to 10·scale mm"
            );
            assert!((sim.angle_deg() - sim.rigid.angle_deg()).abs() < 1e-12);
        }
    }

    #[test]
    fn composed_camera_command_mapping_rejects_non_finite_values() {
        let (lens, mut field) = affine_maps();
        let mut coeffs = field.to_physical.to_coeffs();
        coeffs[0] = f64::NAN;
        field.to_physical = vision::Poly2::from_coeffs(&coeffs).expect("scale still valid");
        assert!(!composed_projection_is_finite(&lens, &field));
        let frame = Rigid2::IDENTITY;
        assert!(commanded_to_camera_px(&lens, &frame, &field, (10.0, 10.0)).is_none());
        assert!(camera_px_to_commanded(&lens, &frame, &field, (f64::INFINITY, 2.0)).is_none());
    }

    #[test]
    fn grid_points_and_corners() {
        let g = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let pts = g.points();
        assert_eq!(pts.len(), 49);
        assert_eq!(pts[0], (0.0, 0.0));
        assert_eq!(pts[6], (60.0, 0.0)); // end of first row
        assert_eq!(*pts.last().unwrap(), (60.0, 60.0));
        assert_eq!(
            g.corners_mm(),
            [(0.0, 0.0), (60.0, 0.0), (60.0, 60.0), (0.0, 60.0)]
        );
    }

    #[test]
    fn corrected_anchor_dots_are_refit_and_rescored() {
        let dots = [
            AnchorDot {
                px: (20.0, 30.0),
                mm: (0.0, 0.0),
                resid_um: 999.0,
            },
            AnchorDot {
                px: (120.0, 30.0),
                mm: (10.0, 0.0),
                resid_um: 999.0,
            },
            AnchorDot {
                px: (120.0, 130.0),
                mm: (10.0, 10.0),
                resid_um: 999.0,
            },
            AnchorDot {
                px: (20.0, 130.0),
                mm: (0.0, 10.0),
                resid_um: 999.0,
            },
            AnchorDot {
                px: (70.0, 80.0),
                mm: (5.0, 5.0),
                resid_um: 999.0,
            },
        ];
        let calibration = refit_anchor_dots(&dots, 49).expect("manual correction refits");
        assert_eq!((calibration.found, calibration.total), (5, 49));
        assert!(calibration.rms_um < 1e-6);
        assert!(calibration.dots.iter().all(|dot| dot.resid_um < 1e-6));

        let mut invalid = dots;
        invalid[0].px.0 = f64::NAN;
        assert!(
            refit_anchor_dots(&invalid, 49)
                .unwrap_err()
                .contains("non-finite")
        );
    }

    /// Render a grid of dark dots through a known perspective homography
    /// (commanded-mm → px), then confirm the fit recovers commanded coords
    /// from pixels to sub-pixel-equivalent accuracy.
    #[test]
    fn recovers_commanded_coordinates_from_a_burned_grid() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        // A mild keystone: commanded (0..60, 0..60) mm imaged into a ~600px
        // frame, top edge slightly narrower than the bottom (tilted camera).
        let corr = [
            ((0.0, 0.0), (60.0, 540.0)),
            ((60.0, 0.0), (560.0, 540.0)),
            ((60.0, 60.0), (520.0, 60.0)),
            ((0.0, 60.0), (100.0, 60.0)),
        ];
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = corr
            .iter()
            .map(|&((mx, my), (px, py))| (Point2::new(mx, my), Point2::new(px, py)))
            .collect();
        let mm_to_px = fit_homography(&pairs).unwrap();
        let dot_mm = 1.5;

        // Render the 49 dots as anti-aliased dark discs on a bright field.
        let (w, h) = (620u32, 620u32);
        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(mx, my)| {
                let p = mm_to_px.apply(Point2::new(mx, my));
                // dot radius in px ≈ dot_mm * local scale (~10 px/mm here).
                (p.x, p.y, dot_mm * 10.0)
            })
            .collect();
        let img = GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        });

        // Corner clicks = the true corner-dot pixels (what the operator marks).
        let corners_px = grid.corners_mm().map(|(mx, my)| {
            let p = mm_to_px.apply(Point2::new(mx, my));
            (p.x, p.y)
        });

        let cal =
            fit_camera_to_machine(&img, corners_px, &grid, dot_mm, DotKind::Dark).expect("fit");
        assert!(
            cal.found >= 45,
            "detected most dots: {}/{}",
            cal.found,
            cal.total
        );
        assert!(cal.rms_um < 200.0, "tight fit: {} µm", cal.rms_um);

        // Per-dot anchor feedback populates (one entry per detected dot), each
        // carries the commanded mm it maps to, and residuals are small on this
        // clean fit — this is what the overlay draws.
        assert_eq!(cal.dots.len(), cal.found, "one AnchorDot per detected dot");
        let worst = cal.dots.iter().map(|d| d.resid_um).fold(0.0_f64, f64::max);
        assert!(worst < 400.0, "worst dot residual {worst:.0} µm");
        // Every dot's commanded mm sits on the 10 mm lattice.
        assert!(
            cal.dots
                .iter()
                .all(|d| (d.mm.0 % 10.0).abs() < 1e-6 && (d.mm.1 % 10.0).abs() < 1e-6),
            "dot mm are on the commanded lattice"
        );

        // A pixel we didn't feed in: the center dot (commanded (30,30)) maps
        // back to ~(30,30) mm.
        let center_px = mm_to_px.apply(Point2::new(30.0, 30.0));
        let back = cal.px_to_mm.apply(center_px);
        assert!(
            (back.x - 30.0).abs() < 0.2 && (back.y - 30.0).abs() < 0.2,
            "center maps to ~(30,30): ({:.3},{:.3})",
            back.x,
            back.y
        );
    }

    /// Camera drift: fit at pose A, then the camera shifts ~2 mm (the grid
    /// moves in the image); re-anchoring from the pose-A calibration re-locks
    /// the dots and recovers correct commanded coordinates — no corner clicks.
    #[test]
    fn re_anchor_absorbs_a_camera_shift() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        // Pose A: 10 px/mm, grid at image (30..630, 30..630).
        let a = homog(&[
            ((0.0, 0.0), (30.0, 30.0)),
            ((60.0, 0.0), (630.0, 30.0)),
            ((60.0, 60.0), (630.0, 630.0)),
            ((0.0, 60.0), (30.0, 630.0)),
        ]);
        let img_a = render_grid(&grid, &a, dot_mm, 680, 680);
        let corners_a = grid.corners_mm().map(|(mx, my)| {
            (
                a.apply(Point2::new(mx, my)).x,
                a.apply(Point2::new(mx, my)).y,
            )
        });
        let cal_a =
            fit_camera_to_machine(&img_a, corners_a, &grid, dot_mm, DotKind::Dark).expect("fit A");

        // Pose B: the camera shifted, so the grid sits ~20 px (2 mm) right and
        // ~12 px down — well within the ~4 mm search window.
        let b = homog(&[
            ((0.0, 0.0), (50.0, 42.0)),
            ((60.0, 0.0), (650.0, 42.0)),
            ((60.0, 60.0), (650.0, 642.0)),
            ((0.0, 60.0), (50.0, 642.0)),
        ]);
        let img_b = render_grid(&grid, &b, dot_mm, 680, 680);

        // Re-anchor from cal_A onto frame B — no corner clicks.
        let cal_b = re_anchor(&img_b, &cal_a, &grid, dot_mm, DotKind::Dark).expect("re-anchor B");
        assert!(cal_b.found >= 45, "re-locked most dots: {}", cal_b.found);
        assert!(cal_b.rms_um < 200.0, "tight re-fit: {} µm", cal_b.rms_um);

        // The re-anchored map reads the new frame correctly: dot (30,30) mm,
        // now at pose-B pixels, maps back to ~(30,30).
        let c = b.apply(Point2::new(30.0, 30.0));
        let back = cal_b.px_to_mm.apply(c);
        assert!(
            (back.x - 30.0).abs() < 0.3 && (back.y - 30.0).abs() < 0.3,
            "re-anchored center ~(30,30): ({:.3},{:.3})",
            back.x,
            back.y
        );
        // And the STALE pose-A calibration would have been wrong on frame B:
        let stale = cal_a.px_to_mm.apply(c);
        assert!(
            (stale.x - 30.0).abs() > 1.0 || (stale.y - 30.0).abs() > 1.0,
            "stale calibration is visibly off ({:.2},{:.2}) — re-anchor was needed",
            stale.x,
            stale.y
        );
    }

    /// An **ablated** grid images as bright dots on a dark plate. Polarity is
    /// a hint, not a gate: `DotKind::Bright` locks it directly, and the
    /// square-grid fallback rescues a mistaken `DotKind::Dark` selection when
    /// the generic dark detector finds too little — so the burned-grid case
    /// the operator hit (0/49 with the wrong polarity) now locks either way.
    /// Both settings must converge on the same commanded mapping.
    #[test]
    fn bright_on_dark_locks_with_either_polarity() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        let mm_to_px = homog(&[
            ((0.0, 0.0), (40.0, 640.0)),
            ((60.0, 0.0), (640.0, 640.0)),
            ((60.0, 60.0), (640.0, 40.0)),
            ((0.0, 60.0), (40.0, 40.0)),
        ]);
        // Bright ablated discs (~230) on a dark plate (~40) — inverse polarity
        // of `render_grid`.
        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(mx, my)| {
                let p = mm_to_px.apply(Point2::new(mx, my));
                (p.x, p.y, dot_mm * 10.0)
            })
            .collect();
        let img = GrayImage::from_fn(700, 700, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(40.0 + 190.0 * cover) as u8])
        });
        let corners_px = grid.corners_mm().map(|(mx, my)| {
            let p = mm_to_px.apply(Point2::new(mx, my));
            (p.x, p.y)
        });

        let dark = fit_camera_to_machine(&img, corners_px, &grid, dot_mm, DotKind::Dark)
            .expect("square-grid fallback rescues the mistaken dark polarity");
        let bright = fit_camera_to_machine(&img, corners_px, &grid, dot_mm, DotKind::Bright)
            .expect("bright fit");
        for (label, cal) in [("dark-fallback", &dark), ("bright", &bright)] {
            assert!(cal.found >= 45, "{label} locked most dots: {}", cal.found);
            assert!(cal.rms_um < 200.0, "{label} tight fit: {} µm", cal.rms_um);
            let center_px = mm_to_px.apply(Point2::new(30.0, 30.0));
            let back = cal.px_to_mm.apply(center_px);
            assert!(
                (back.x - 30.0).abs() < 0.3 && (back.y - 30.0).abs() < 0.3,
                "{label} center maps to ~(30,30): ({:.3},{:.3})",
                back.x,
                back.y
            );
        }
    }

    /// The camera lens fit locks the printed grid, corrects the barrel
    /// distortion to a low residual, and reports a non-trivial distortion
    /// field (arrows) for the operator to see.
    #[test]
    fn camera_lens_fit_corrects_barrel_and_reports_distortion() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        // Perspective ~10 px/mm plus 4% barrel about the image center.
        let base = homog(&[
            ((0.0, 0.0), (40.0, 40.0)),
            ((60.0, 0.0), (640.0, 40.0)),
            ((60.0, 60.0), (640.0, 640.0)),
            ((0.0, 60.0), (40.0, 640.0)),
        ]);
        let distort = |p: Point2<f64>| {
            let (cx, cy) = (340.0, 340.0);
            let (du, dv) = (p.x - cx, p.y - cy);
            let r2 = (du * du + dv * dv) / (340.0 * 340.0);
            let k = 0.04;
            Point2::new(cx + du * (1.0 + k * r2), cy + dv * (1.0 + k * r2))
        };
        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(mx, my)| {
                let p = distort(base.apply(Point2::new(mx, my)));
                (p.x, p.y, dot_mm * 10.0)
            })
            .collect();
        let (w, h) = (700u32, 700u32);
        let img = GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        });
        let corners_px = grid.corners_mm().map(|(mx, my)| {
            let p = distort(base.apply(Point2::new(mx, my)));
            (p.x, p.y)
        });

        let cal =
            fit_camera_lens(&img, corners_px, &grid, dot_mm, DotKind::Dark).expect("lens fit");
        assert!(cal.found >= 45, "locked most dots: {}", cal.found);
        assert!(
            cal.lens.rms_um < 60.0,
            "corrected RMS {} µm",
            cal.lens.rms_um
        );
        // The distortion field is real: at least one corner dot deviates several
        // px from the perspective model (that's the barrel we corrected).
        let max_distort = cal
            .dots
            .iter()
            .map(|d| (d.distort_px.0.powi(2) + d.distort_px.1.powi(2)).sqrt())
            .fold(0.0, f64::max);
        assert!(
            max_distort > 3.0,
            "shows the barrel field: {max_distort:.1} px"
        );
    }

    /// End-to-end laser-field fit: a burned grid imaged through a metric
    /// camera, where the laser's field distortion put each commanded dot a few
    /// percent off. The fit (via the camera-lens map) recovers a pre-distortion
    /// that cancels it: a physical target maps to a command the field bends
    /// back onto that target.
    #[test]
    fn laser_field_fit_recovers_precompensation() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        // Camera: physical mm → px, a plain 10 px/mm ruler offset by (50,50).
        let cam = |phx: f64, phy: f64| (10.0 * phx + 50.0, 10.0 * phy + 50.0);
        // Laser field: commanded → physical, ~3% pincushion about (30,30).
        let field = |cx: f64, cy: f64| {
            let (du, dv) = (cx - 30.0, cy - 30.0);
            let r2 = (du * du + dv * dv) / (30.0 * 30.0);
            let f = 1.0 + 0.03 * r2;
            (30.0 + du * f, 30.0 + dv * f)
        };

        // Camera-lens map from a printed grid (physical known, imaged by cam).
        let lens_pairs: Vec<(Point2<f64>, Point2<f64>)> = grid
            .points()
            .iter()
            .map(|&(x, y)| {
                let (u, v) = cam(x, y);
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();
        let lens = fit_lens(&lens_pairs).expect("lens");

        // Burned grid: commanded dots land physically distorted, imaged by cam.
        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(cx, cy)| {
                let (px, py) = field(cx, cy);
                let (u, v) = cam(px, py);
                (u, v, dot_mm * 10.0)
            })
            .collect();
        let (w, h) = (720u32, 720u32);
        let img = GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        });
        // Corner clicks: the burned corner dots' pixels.
        let corners_px = grid.corners_mm().map(|(cx, cy)| {
            let (px, py) = field(cx, cy);
            cam(px, py)
        });

        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::Refuse,
        )
        .expect("field fit");
        assert_eq!(cal.found, 49, "all dots detected");
        assert!(cal.field.rms_um < 60.0, "field RMS {} µm", cal.field.rms_um);
        field_live_acceptance(&cal, &grid, 100.0, 250.0)
            .expect("well-covered field fit is accepted");

        let mut missing_corner = cal.clone();
        missing_corner
            .dots
            .retain(|d| d.commanded_mm != grid.corners_mm()[0]);
        missing_corner.found = missing_corner.dots.len();
        assert!(
            field_live_acceptance(&missing_corner, &grid, 100.0, 250.0)
                .unwrap_err()
                .contains("boundary corners"),
            "a center-heavy fit cannot activate correction"
        );

        let mut inaccurate = cal.clone();
        inaccurate.field.max_um = 101.0;
        assert!(
            field_live_acceptance(&inaccurate, &grid, 50.0, 100.0)
                .unwrap_err()
                .contains("limits are 50/100"),
            "a high-residual polynomial cannot activate correction"
        );
        // The distortion is real: corner dots are visibly off their command.
        let worst = cal.dots.iter().map(|d| d.field_um).fold(0.0_f64, f64::max);
        assert!(worst > 300.0, "field error present: worst {worst:.0} µm");

        // Precompensation cancels it: a physical target we didn't fit maps to a
        // command the field bends back onto the target.
        for &(tx, ty) in &[(25.0, 25.0), (5.0, 55.0), (55.0, 5.0)] {
            let (cx, cy) = cal.field.precompensate(tx, ty);
            let (lx, ly) = field(cx, cy);
            let err = ((lx - tx).powi(2) + (ly - ty).powi(2)).sqrt() * 1000.0;
            assert!(err < 80.0, "target ({tx},{ty}) off by {err:.0} µm");
        }

        // The genuine ~3% pincushion is classified as such, not written off as
        // scatter — this is what routes the operator to "correction will help"
        // in the ③ status block.
        assert!(
            matches!(
                cal.field_verdict.pattern,
                vision::FieldPattern::Systematic { pincushion: true }
            ),
            "expected a pincushion verdict, got {:?}",
            cal.field_verdict.pattern
        );
        assert!(cal.field_verdict.ratio >= 2.0);
    }

    /// The operator's real ③ geometry: a 7×7 burned grid (49 dots) with a ~3%
    /// pincushion field, imaged by a plain 10 px/mm camera through a lens ruler
    /// fit on the same points. `nudge_mm` displaces individual burns in the
    /// PHYSICAL frame — that is what a scuffed/spattered dot looks like to the
    /// fit: detected fine, sitting off the lattice.
    fn pincushion_setup(
        nudge_mm: &dyn Fn(f64, f64) -> (f64, f64),
    ) -> (GrayImage, [(f64, f64); 4], GridSpec, f64, LensMap) {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        let cam = |phx: f64, phy: f64| (10.0 * phx + 50.0, 10.0 * phy + 50.0);
        let field = |cx: f64, cy: f64| {
            let (du, dv) = (cx - 30.0, cy - 30.0);
            let r2 = (du * du + dv * dv) / (30.0 * 30.0);
            let f = 1.0 + 0.03 * r2;
            (30.0 + du * f, 30.0 + dv * f)
        };
        // Where the burn for commanded (cx,cy) physically ended up.
        let burned = |cx: f64, cy: f64| {
            let (px, py) = field(cx, cy);
            let (dx, dy) = nudge_mm(cx, cy);
            (px + dx, py + dy)
        };

        let lens_pairs: Vec<(Point2<f64>, Point2<f64>)> = grid
            .points()
            .iter()
            .map(|&(x, y)| {
                let (u, v) = cam(x, y);
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();
        let lens = fit_lens(&lens_pairs).expect("lens");

        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(cx, cy)| {
                let (px, py) = burned(cx, cy);
                let (u, v) = cam(px, py);
                (u, v, dot_mm * 10.0)
            })
            .collect();
        let img = GrayImage::from_fn(720, 720, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        });
        // The operator clicks the corner dots WHERE THEY ARE, nudge included.
        let corners_px = grid.corners_mm().map(|(cx, cy)| {
            let (px, py) = burned(cx, cy);
            cam(px, py)
        });
        (img, corners_px, grid, dot_mm, lens)
    }

    fn fit_pincushion(
        setup: &(GrayImage, [(f64, f64); 4], GridSpec, f64, LensMap),
    ) -> Result<FieldCal, String> {
        let (img, corners_px, grid, dot_mm, lens) = setup;
        fit_laser_field(
            img,
            *corners_px,
            grid,
            *dot_mm,
            DotKind::Dark,
            lens,
            FieldScale::Refuse,
        )
    }

    /// The case this was built for: 49 dots, one interior burn ~2 mm off the
    /// lattice. Including it the fit reads ~300 µm RMS / ~2 mm worst and is
    /// refused; the outlier is excluded, reported, and the survivors land back
    /// on the clean grid's residuals — inside the console's 100/250 µm limits.
    #[test]
    fn one_stray_dot_is_excluded_and_reported() {
        let clean = fit_pincushion(&pincushion_setup(&|_, _| (0.0, 0.0))).expect("clean fit");
        assert_eq!(clean.rejected, 0);
        assert!(clean.rejection_note.is_empty());

        // (30,30) is the grid's interior centre — not a boundary corner.
        let strayed = fit_pincushion(&pincushion_setup(&|cx: f64, cy: f64| {
            if cx == 30.0 && cy == 30.0 {
                (2.0, 0.0)
            } else {
                (0.0, 0.0)
            }
        }))
        .expect("strayed fit");

        assert_eq!(strayed.found, 49, "the stray dot is still DETECTED");
        assert_eq!(strayed.total, 49);
        assert_eq!(strayed.rejected, 1, "note: {}", strayed.rejection_note);
        let stray: Vec<&FieldDot> = strayed.dots.iter().filter(|d| d.rejected).collect();
        assert_eq!(stray[0].commanded_mm, (30.0, 30.0), "the injected dot");
        assert!(
            stray[0].resid_um > 1500.0,
            "rejected dot still reads its true error: {:.0} µm",
            stray[0].resid_um
        );
        assert!(
            strayed.rejection_note.contains("1 of 49 dots EXCLUDED"),
            "note: {}",
            strayed.rejection_note
        );

        // The point of the exercise: the survivors' fit matches the clean grid's
        // and passes the console's default limits, where the all-dots fit didn't.
        assert!(
            (strayed.field.rms_um - clean.field.rms_um).abs() < 10.0,
            "survivor RMS {:.0} µm vs clean {:.0} µm",
            strayed.field.rms_um,
            clean.field.rms_um
        );
        field_live_acceptance(&strayed, &grid_7x7(), 100.0, 250.0)
            .expect("one excluded outlier no longer vetoes the calibration");

        // …and the pre-rejection fit really was refused, so the change matters.
        let worst = strayed.dots.iter().map(|d| d.resid_um).fold(0.0, f64::max);
        assert!(worst > 250.0, "un-excluded worst {worst:.0} µm");
    }

    fn grid_7x7() -> GridSpec {
        GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        }
    }

    /// Six bad dots out of 49 is not a scuff on the paper, it is a bad capture.
    /// The cap (4, at 10% of 49) fires: nothing is dropped, the fit still comes
    /// back so the operator can read it, and the note says the cap is why.
    #[test]
    fn too_many_outliers_drop_nothing() {
        // Six scattered interior dots, all well clear of the boundary corners.
        let bad = [
            (10.0, 10.0),
            (30.0, 10.0),
            (50.0, 10.0),
            (10.0, 30.0),
            (30.0, 30.0),
            (50.0, 30.0),
        ];
        let cal = fit_pincushion(&pincushion_setup(&move |cx: f64, cy: f64| {
            if bad.contains(&(cx, cy)) {
                (2.0, 0.0)
            } else {
                (0.0, 0.0)
            }
        }))
        .expect("fit still returns");
        assert_eq!(cal.rejected, 0, "note: {}", cal.rejection_note);
        assert!(cal.dots.iter().all(|d| !d.rejected));
        assert!(
            cal.rejection_note.contains("NOTHING") && cal.rejection_note.contains("10% cap"),
            "note: {}",
            cal.rejection_note
        );
        // And it is still refused, exactly as it was before rejection existed.
        assert!(field_live_acceptance(&cal, &grid_7x7(), 100.0, 250.0).is_err());
    }

    /// A bad boundary corner must never be fitted around — neither by dropping
    /// the corner nor by dropping the neighbours that hold the polynomial away
    /// from it. A corner sits at the bi-cubic's maximum leverage, so the second
    /// route is the real risk: delete two neighbours and the surface bends
    /// toward the corner until its residual falls back under the limit.
    ///
    /// Swept across the whole range where a corner error is big enough to be
    /// flagged but small enough that the fit might still slip through: nothing
    /// is ever excluded, the note names the corner, and acceptance always fails.
    #[test]
    fn a_corner_outlier_suspends_rejection_and_the_fit_fails() {
        for step in 8..=16 {
            let nudge = step as f64 / 10.0;
            let cal = fit_pincushion(&pincushion_setup(&move |cx: f64, cy: f64| {
                if cx == 0.0 && cy == 0.0 {
                    (nudge, 0.0)
                } else {
                    (0.0, 0.0)
                }
            }))
            .expect("fit");
            assert_eq!(
                cal.rejected, 0,
                "nudge {nudge} mm excluded dots: {}",
                cal.rejection_note
            );
            assert!(cal.dots.iter().all(|d| !d.rejected));
            assert!(
                cal.rejection_note
                    .contains("boundary corner dots are outliers")
                    && cal.rejection_note.contains("SUSPENDED"),
                "nudge {nudge} mm note: {}",
                cal.rejection_note
            );
            // The corner is still PRESENT, so the corner gate is satisfied —
            // what refuses the fit is the residual gate, with the corner's own
            // error still in it.
            let err = field_live_acceptance(&cal, &grid_7x7(), 100.0, 250.0)
                .expect_err("a bad corner must not be fitted around");
            assert!(err.contains("limits are 100/250"), "nudge {nudge}: {err}");
        }
    }

    /// The residual acceptance limits are operator-configurable: a fit sitting
    /// at the rig's measurement floor (RMS 70 µm / worst 180 µm) passes the new
    /// 100/250 defaults but is rejected by the old hardcoded 50/100.
    #[test]
    fn field_acceptance_limits_are_configurable() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 4,
        };
        let (_lens, mut field) = affine_maps();
        field.rms_um = 70.0;
        field.max_um = 180.0;
        // One dot per commanded point, so all four corners lock and coverage is
        // 100% — isolating the residual gate.
        let dots: Vec<FieldDot> = grid
            .points()
            .iter()
            .map(|&(x, y)| FieldDot {
                px: (0.0, 0.0),
                physical_mm: (x, y),
                commanded_mm: (x, y),
                field_um: 0.0,
                resid_um: 0.0,
                rejected: false,
            })
            .collect();
        let total = dots.len();
        let to_px = homog(&[
            ((0.0, 0.0), (0.0, 0.0)),
            ((30.0, 0.0), (300.0, 0.0)),
            ((30.0, 30.0), (300.0, 300.0)),
            ((0.0, 30.0), (0.0, 300.0)),
        ]);
        let cal = FieldCal {
            field,
            paper_to_machine: Rigid2::IDENTITY,
            to_px,
            found: total,
            total,
            dots,
            field_verdict: vision::classify_field_error(&[]),
            scale: 1.0,
            extrapolated: 0,
            rejected: 0,
            rejection_note: String::new(),
        };
        // The new defaults accept the rig's floor.
        assert!(field_live_acceptance(&cal, &grid, 100.0, 250.0).is_ok());
        // The old hardcoded limits reject it, quoting the configured limits.
        let err = field_live_acceptance(&cal, &grid, 50.0, 100.0).unwrap_err();
        assert!(err.contains("limits are 50/100"), "got: {err}");
    }

    /// Build a burned-grid frame with NO field distortion, imaged by a plain
    /// 10 px/mm camera, but a lens ruler whose mm output is scaled by
    /// `ruler_scale` — exactly what fitting ① against the wrong printed pitch
    /// produces. Returns (frame, corners_px, grid, dot_mm, lens).
    fn mis_scaled_ruler_setup(
        ruler_scale: f64,
    ) -> (GrayImage, [(f64, f64); 4], GridSpec, f64, LensMap) {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        let cam = |x: f64, y: f64| (10.0 * x + 50.0, 10.0 * y + 50.0);
        let lens_pairs: Vec<(Point2<f64>, Point2<f64>)> = grid
            .points()
            .iter()
            .map(|&(x, y)| {
                let (u, v) = cam(x, y);
                (
                    Point2::new(u, v),
                    Point2::new(x * ruler_scale, y * ruler_scale),
                )
            })
            .collect();
        let lens = fit_lens(&lens_pairs).expect("lens");
        let mm_to_px = homog(&[
            ((0.0, 0.0), cam(0.0, 0.0)),
            ((60.0, 0.0), cam(60.0, 0.0)),
            ((60.0, 60.0), cam(60.0, 60.0)),
            ((0.0, 60.0), cam(0.0, 60.0)),
        ]);
        let img = render_grid(&grid, &mm_to_px, dot_mm, 720, 720);
        let corners_px = grid.corners_mm().map(|(x, y)| cam(x, y));
        (img, corners_px, grid, dot_mm, lens)
    }

    /// A grossly mis-scaled metric ruler (wrong pitch at ①, a moved camera, a
    /// paper out of the burn plane…) must fail the field fit EARLY as a setup
    /// error with the measured percentage — not fall into the mirrored-view
    /// guard or come back as a garbage "field error".
    #[test]
    fn laser_field_fit_rejects_setup_scale_error() {
        let (img, corners_px, grid, dot_mm, lens) = mis_scaled_ruler_setup(1.35);
        let err = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::Refuse,
        )
        .expect_err("a 35% scale mismatch is a setup error");
        assert!(
            err.contains(FIELD_SCALE_ERR_MARKER),
            "setup-scale message expected, got: {err}"
        );
        assert!(
            err.contains("+35.0%") || err.contains("+34.9%") || err.contains("+35.1%"),
            "measured percentage reported: {err}"
        );
        assert!(
            !err.contains("mirrored"),
            "must not be misdiagnosed as a mirrored view: {err}"
        );
    }

    /// A mild scale deviation (galvo-plausible) still fits; the measured
    /// scale is carried on the calibration for the operator-facing note.
    #[test]
    fn laser_field_fit_mild_scale_passes_with_diagnostic() {
        let (img, corners_px, grid, dot_mm, lens) = mis_scaled_ruler_setup(1.015);
        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::Refuse,
        )
        .expect("a 1.5% scale deviation is fittable");
        assert!(
            (cal.scale - 1.015).abs() < 0.003,
            "measured scale {} ≈ 1.015",
            cal.scale
        );
    }

    /// With `FieldScale::Compensate`, a gross uniform scale (an oversized machine
    /// field the operator chose to compensate in software) is no longer a hard
    /// setup error: the fit succeeds, records the scale, and the field
    /// polynomial genuinely absorbs it — a physical target maps to a command
    /// scaled by `1/scale` relative to the grid, so shapes burn dimensionally
    /// true.
    #[test]
    fn laser_field_fit_absorbs_machine_scale_when_allowed() {
        let (img, corners_px, grid, dot_mm, lens) = mis_scaled_ruler_setup(1.35);
        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::Compensate,
        )
        .expect("a 35% scale is fittable once the operator opts to compensate it");
        assert!(
            (cal.scale - 1.35).abs() < 0.01,
            "measured scale {} ≈ 1.35",
            cal.scale
        );
        // Precompensation carries the scale: the command-space separation of two
        // physical targets is the physical separation shrunk by 1/1.35 (the
        // additive centroid offset cancels in the ratio). Targets sit inside the
        // grid's machine-frame span (~ −10.5..70.5 mm).
        let (a, b) = ((10.0, 10.0), (40.0, 40.0));
        let pa = cal.field.precompensate(a.0, a.1);
        let pb = cal.field.precompensate(b.0, b.1);
        let cmd_sep = ((pa.0 - pb.0).powi(2) + (pa.1 - pb.1).powi(2)).sqrt();
        let phys_sep = ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
        let ratio = cmd_sep / phys_sep;
        assert!(
            (ratio * 1.35 - 1.0).abs() < 0.01,
            "command/physical separation ratio {ratio} ≈ 1/1.35"
        );
    }

    /// The operator's real ③ case: a 7×7 grid burned over commanded 15..75 mm
    /// of a 0..90 mm field, where the laser puts each dot at
    /// `centre + scale·(1 + k·r²)·(cmd − centre)` about the field centre
    /// (45,45) — a known pincushion of strength `k` on top of a known uniform
    /// `scale`. The camera is an exact metric ruler, so everything the fit sees
    /// out of true comes from the synthetic field.
    /// Returns (frame, corners_px, grid, dot_mm, lens).
    fn scaled_pincushion_setup(
        scale: f64,
        k: f64,
    ) -> (GrayImage, [(f64, f64); 4], GridSpec, f64, LensMap) {
        let grid = GridSpec {
            origin_mm: (15.0, 15.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        // Physical mm → px, centred so the shrunken burn stays well inside the
        // frame: 10 px/mm about physical (45,45) ↦ px (360,360).
        let cam = |phx: f64, phy: f64| (10.0 * (phx - 45.0) + 360.0, 10.0 * (phy - 45.0) + 360.0);
        let field = |cx: f64, cy: f64| {
            let (du, dv) = (cx - 45.0, cy - 45.0);
            let f = scale * (1.0 + k * (du * du + dv * dv) / (30.0 * 30.0));
            (45.0 + du * f, 45.0 + dv * f)
        };
        // The ① ruler is fit over the region the burn actually occupies, so no
        // dot reads through an extrapolating lens.
        let lens_pairs: Vec<(Point2<f64>, Point2<f64>)> = (0..=8)
            .flat_map(|r| (0..=8).map(move |c| (r, c)))
            .map(|(r, c)| {
                let (x, y) = (20.0 + c as f64 * 6.25, 20.0 + r as f64 * 6.25);
                let (u, v) = cam(x, y);
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();
        let lens = fit_lens(&lens_pairs).expect("lens");

        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(cx, cy)| {
                let (px, py) = field(cx, cy);
                let (u, v) = cam(px, py);
                (u, v, dot_mm * 10.0)
            })
            .collect();
        let img = GrayImage::from_fn(720, 720, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        });
        let corners_px = grid.corners_mm().map(|(cx, cy)| {
            let (px, py) = field(cx, cy);
            cam(px, py)
        });
        (img, corners_px, grid, dot_mm, lens)
    }

    /// Invert the synthetic field `gain·(1 + k·d²/30²)·d = phys_off` for `d`:
    /// the commanded offset from the field centre that lands `phys_off` away
    /// once the fit has divided the measured uniform scale out. `gain` is the
    /// synthetic scale over the MEASURED one, which is 1 only when there is no
    /// pincushion — see `laser_field_distortion_only_*` on why they differ.
    fn field_inverse(phys_off: f64, k: f64, gain: f64) -> f64 {
        let mut d = phys_off;
        for _ in 0..64 {
            d = phys_off / (gain * (1.0 + k * d * d / 900.0));
        }
        d
    }

    /// `DistortionOnly` on a −32.2% machine: the fit succeeds, REPORTS the
    /// scale, and produces a UNIT-MAGNIFICATION map. The field centre maps to
    /// itself and a mid-field point's correction is the pincushion alone — a
    /// fraction of a millimetre, nowhere near the ~47% stretch that absorbing
    /// the scale would introduce.
    #[test]
    fn laser_field_distortion_only_recovers_pincushion_at_unit_magnification() {
        let (img, corners_px, grid, dot_mm, lens) = scaled_pincushion_setup(0.678, 0.03);
        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::DistortionOnly,
        )
        .expect("distortion-only never refuses on scale magnitude");
        // A least-squares similarity over a pincushioned lattice necessarily
        // lumps the pincushion's MEAN radial magnification (~+3.7% here) into
        // the uniform scale — the split between "scale" and "radial term" is
        // only defined that way. So the reported scale is the synthetic 0.678
        // times that, not 0.678 exactly, and the fit divides out what it
        // measured. The pure-scale test below pins the undisturbed case tight.
        assert!(
            (cal.scale - 0.678).abs() < 0.03,
            "measured scale {} ≈ 0.678 is still reported",
            cal.scale
        );
        let centre = cal.field.precompensate(45.0, 45.0);
        assert!(
            (centre.0 - 45.0).hypot(centre.1 - 45.0) < 0.05,
            "field centre maps to itself, got {centre:?}"
        );
        // 20 mm out along +x: the correction is the radial term alone.
        let gain = 0.678 / cal.scale;
        let want = 45.0 + field_inverse(20.0, 0.03, gain);
        let mid = cal.field.precompensate(65.0, 45.0);
        assert!(
            (mid.0 - want).abs() < 0.1,
            "mid-field command {} ≈ {want} (radial term only, no scale term)",
            mid.0
        );
        // Magnification over a 40 mm span: 1.0 to within the pincushion's own
        // few percent — not the 1/0.678 ≈ 1.475 that absorbing it would give.
        let lo = cal.field.precompensate(25.0, 45.0);
        let mag = (mid.0 - lo.0) / 40.0;
        assert!(
            (mag - 1.0).abs() < 0.05,
            "magnification {mag} ≈ 1.0 (compensating would give 1.475)"
        );
    }

    /// The same burn under `Compensate` keeps behaving as it always has: the
    /// scale is absorbed into the map, so command space is stretched by
    /// `1/scale` — this is exactly the work-area shrink `DistortionOnly` avoids.
    #[test]
    fn laser_field_compensate_still_absorbs_the_same_scale() {
        let (img, corners_px, grid, dot_mm, lens) = scaled_pincushion_setup(0.678, 0.03);
        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::Compensate,
        )
        .expect("compensate fits a gross scale");
        assert!(
            (cal.scale - 0.678).abs() < 0.03,
            "measured scale {} ≈ 0.678",
            cal.scale
        );
        let lo = cal.field.precompensate(35.0, 45.0);
        let hi = cal.field.precompensate(55.0, 45.0);
        let mag = (hi.0 - lo.0) / 20.0;
        assert!(
            (mag * 0.678 - 1.0).abs() < 0.02,
            "magnification {mag} ≈ 1/0.678 — the scale is absorbed"
        );
    }

    /// `Refuse` is untouched by the new mode: the same burn is still rejected
    /// as a setup error before anything is fit.
    #[test]
    fn laser_field_refuse_still_rejects_the_same_scale() {
        let (img, corners_px, grid, dot_mm, lens) = scaled_pincushion_setup(0.678, 0.03);
        let err = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::Refuse,
        )
        .expect_err("a −32% scale is still a setup error by default");
        assert!(
            err.contains(FIELD_SCALE_ERR_MARKER),
            "setup-scale message expected, got: {err}"
        );
    }

    /// The load-bearing one: a PURE uniform scale with no distortion at all
    /// must come back as a near-identity correction under `DistortionOnly`. If
    /// the scale were smeared into the polynomial the map would magnify by
    /// `1/0.678`; if it were divided the wrong way, by `0.678²`. Neither
    /// survives a ±0.1 mm identity check over the whole grid.
    #[test]
    fn laser_field_distortion_only_leaves_a_pure_scale_as_identity() {
        let (img, corners_px, grid, dot_mm, lens) = scaled_pincushion_setup(0.678, 0.0);
        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::DistortionOnly,
        )
        .expect("pure scale is fittable in distortion-only mode");
        assert!(
            (cal.scale - 0.678).abs() < 0.005,
            "measured scale {} ≈ 0.678",
            cal.scale
        );
        for &(x, y) in &grid.points() {
            let (gx, gy) = cal.field.precompensate(x, y);
            assert!(
                (gx - x).hypot(gy - y) < 0.1,
                "({x},{y}) → ({gx},{gy}) should be identity: nothing to correct"
            );
        }
        // Direction check: 0.678² ≈ 0.46 and 1/0.678 ≈ 1.475 both fail this.
        let lo = cal.field.precompensate(25.0, 45.0);
        let hi = cal.field.precompensate(65.0, 45.0);
        let mag = (hi.0 - lo.0) / 40.0;
        assert!((mag - 1.0).abs() < 0.01, "magnification {mag} ≈ 1.0");
    }

    /// `to_commanded` and `to_physical` are separately fit, so the new mode's
    /// pair must still round-trip through the Newton inversion the projection
    /// path uses — inside the burned span, and out at the field corners the
    /// operator will actually address.
    #[test]
    fn laser_field_distortion_only_round_trips_commanded_and_physical() {
        let (img, corners_px, grid, dot_mm, lens) = scaled_pincushion_setup(0.678, 0.03);
        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::DistortionOnly,
        )
        .expect("distortion-only fit");
        for &(x, y) in &[
            (45.0, 45.0),
            (25.0, 35.0),
            (65.0, 70.0),
            (5.0, 5.0),
            (85.0, 85.0),
        ] {
            let phys = invert_poly(&cal.field.to_commanded, &cal.field.to_physical, (x, y))
                .unwrap_or_else(|| panic!("({x},{y}) inverts"));
            let (bx, by) = cal.field.precompensate(phys.0, phys.1);
            assert!(
                (bx - x).hypot(by - y) < 1e-6,
                "({x},{y}) round-trips, got ({bx},{by})"
            );
        }
    }

    /// The whole point of the mode: the map is USED outside the burned grid.
    /// The grid covers commanded 15..75 mm; sweep the full 0..90 mm work area
    /// and require the correction to stay finite and strictly monotonic — the
    /// bi-cubic is extrapolating out there and a fold-over would silently send
    /// two different physical targets to the same command.
    #[test]
    fn laser_field_distortion_only_extrapolates_across_the_work_area() {
        let (img, corners_px, grid, dot_mm, lens) = scaled_pincushion_setup(0.678, 0.03);
        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::DistortionOnly,
        )
        .expect("distortion-only fit");
        let sweep: Vec<f64> = (0..=90).step_by(2).map(|v| v as f64).collect();
        for &fixed in &[5.0, 45.0, 85.0] {
            let (mut prev_x, mut prev_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
            for &v in &sweep {
                let along_x = cal.field.precompensate(v, fixed);
                let along_y = cal.field.precompensate(fixed, v);
                assert!(
                    along_x.0.is_finite()
                        && along_x.1.is_finite()
                        && along_y.0.is_finite()
                        && along_y.1.is_finite(),
                    "({v},{fixed}) stays finite"
                );
                assert!(
                    along_x.0 > prev_x,
                    "x is monotonic across the work area at y={fixed}: {} after {prev_x}",
                    along_x.0
                );
                assert!(
                    along_y.1 > prev_y,
                    "y is monotonic across the work area at x={fixed}: {} after {prev_y}",
                    along_y.1
                );
                prev_x = along_x.0;
                prev_y = along_y.1;
            }
        }
        // Extrapolation must stay a CORRECTION, not run away: 15 mm past the
        // burned grid the command still tracks the true radial inverse to
        // within ~1.5 mm. (The correction itself is ~6 mm out at the work-area
        // corners — that is the synthetic pincushion, not fit divergence.)
        let gain = 0.678 / cal.scale;
        for &(x, y) in &[(0.0, 0.0), (90.0, 0.0), (90.0, 90.0), (0.0, 90.0)] {
            let (du, dv): (f64, f64) = (x - 45.0, y - 45.0);
            let r = du.hypot(dv);
            let d = field_inverse(r, 0.03, gain);
            let (wx, wy) = (45.0 + du / r * d, 45.0 + dv / r * d);
            let (gx, gy) = cal.field.precompensate(x, y);
            assert!(
                (gx - wx).hypot(gy - wy) < 1.5,
                "corner ({x},{y}) → ({gx},{gy}) tracks the true inverse ({wx},{wy})"
            );
        }
    }

    /// A machine that MIRRORS X relative to commanded coordinates is now
    /// absorbed, not rejected. With the burned grid labelled by its TRUE
    /// commanded corners (the operator reads them off the grid's orientation
    /// markers), the paper↔commanded correspondence is a pure reflection: the
    /// dot commanded to LL landed on the mirrored (right) side, so LL's true
    /// pixel is the far-right corner. The reflected similarity/rigid variants
    /// fit it cleanly, `paper_to_machine.flip_x` is set, and the composed
    /// projection round-trips exactly — exercising `Rigid2::inverse_apply` as
    /// the true inverse of `apply` THROUGH the flip.
    ///
    /// Geometrically this is the horizontally-reflected corner order on a
    /// symmetric grid: the dot at physical (cx,cy) carries commanded (60−cx,cy).
    #[test]
    fn laser_field_fit_absorbs_a_mirrored_machine() {
        let (img, corners_px, grid, dot_mm, lens) = mis_scaled_ruler_setup(1.0);
        // TRUE labels for a mirror: LL's dot is the far-right burn, etc.
        let true_corners = [corners_px[1], corners_px[0], corners_px[3], corners_px[2]];
        let cal = fit_laser_field(
            &img,
            true_corners,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::Refuse,
        )
        .expect("a mirrored machine with true corner labels is a reflection the fit absorbs");
        assert!(
            cal.paper_to_machine.flip_x,
            "the mirror is recorded as flip_x, not rejected"
        );
        // The fit is clean: no residual scatter beyond detection noise.
        assert!(
            cal.field.rms_um < 200.0,
            "clean fit RMS {} µm",
            cal.field.rms_um
        );
        // The composed projection round-trips commanded → camera px → commanded
        // to identity, through the flipped frame and the field inverse.
        for &cmd in &[(15.0, 15.0), (5.0, 55.0), (55.0, 5.0), (30.0, 30.0)] {
            let px = commanded_to_camera_px(&lens, &cal.paper_to_machine, &cal.field, cmd)
                .expect("finite projection");
            let back = camera_px_to_commanded(&lens, &cal.paper_to_machine, &cal.field, px)
                .expect("finite inverse");
            assert!(
                (back.0 - cmd.0).abs() < 1e-6 && (back.1 - cmd.1).abs() < 1e-6,
                "round-trip identity through the flip: {cmd:?} → {back:?}"
            );
        }
    }

    /// A genuinely SCRAMBLED corner order (a non-isometry permutation, not a
    /// symmetry of the square) still fails — reflection support only absorbs
    /// reflections, not arbitrary relabellings. An adjacent swap (LL↔LR only)
    /// bowties the seed quad, so the search windows land off the dots and the
    /// fit cannot proceed. The guard against silently accepting a scramble is
    /// intact: the fit returns an error rather than a bogus calibration.
    #[test]
    fn laser_field_fit_rejects_a_scrambled_corner_order() {
        let (img, corners_px, grid, dot_mm, lens) = mis_scaled_ruler_setup(1.0);
        // Swap only LL↔LR (keep UR, UL): NOT a square symmetry, so neither a
        // rotation nor a reflection can align it — a real mislabelling.
        let scrambled = [corners_px[1], corners_px[0], corners_px[2], corners_px[3]];
        let err = fit_laser_field(
            &img,
            scrambled,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::Compensate,
        )
        .expect_err("a scrambled (non-isometry) corner order is not a calibration");
        // It fails through detection (the bowtie seed places windows off the
        // dots) rather than silently producing a flipped fit.
        assert!(
            err.contains("dots") || err.contains("orientation markers"),
            "scramble is rejected (detection or correspondence guard), got: {err}"
        );
    }

    /// The correspondence guard still trips on a burned grid whose true-labelled
    /// dots form a SHEAR relative to the commanded lattice — a correspondence
    /// that is neither a similarity nor a reflection, so scale+rotation+
    /// reflection cannot align it. All dots lock (a shear is affine, so the
    /// corner seed predicts them), but the similarity residual exceeds one grid
    /// pitch and the guard rejects the fit with the orientation-marker message.
    /// Run with `FieldScale::Compensate` so the scale gate is skipped and the
    /// correspondence guard is the path exercised.
    #[test]
    fn laser_field_fit_guard_trips_on_a_sheared_correspondence() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        let cam = |phx: f64, phy: f64| (10.0 * phx + 50.0, 10.0 * phy + 50.0);
        // Camera-lens ruler over a printed grid (physical == commanded).
        let lens_pairs: Vec<(Point2<f64>, Point2<f64>)> = grid
            .points()
            .iter()
            .map(|&(x, y)| {
                let (u, v) = cam(x, y);
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();
        let lens = fit_lens(&lens_pairs).expect("lens");
        // Burned grid sheared in x by y: commanded (cx,cy) lands physically at
        // (cx + cy, cy). A shear is not a similarity; its best similarity fit
        // leaves several mm of anisotropic residual, above the 10 mm pitch.
        let shear = |cx: f64, cy: f64| (cx + cy, cy);
        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(cx, cy)| {
                let (px, py) = shear(cx, cy);
                let (u, v) = cam(px, py);
                (u, v, dot_mm * 10.0)
            })
            .collect();
        let (w, h) = (1320u32, 720u32);
        let img = GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        });
        // TRUE corner labels: the sheared pixel of each commanded corner.
        let corners_px = grid.corners_mm().map(|(cx, cy)| {
            let (px, py) = shear(cx, cy);
            cam(px, py)
        });
        let err = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::Compensate,
        )
        .expect_err("a shear is not a similarity+reflection — the guard must trip");
        assert!(
            err.contains("orientation markers"),
            "the correspondence guard message is expected, got: {err}"
        );
    }

    /// A burned grid with NO field distortion (commanded == physical, up to
    /// camera/detection quantization) must NOT be misread as pincushion — the
    /// diagnostic's whole point is to not flag noise as a real field problem.
    #[test]
    fn laser_field_fit_flat_grid_reads_noise_not_pincushion() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        let cam = |phx: f64, phy: f64| (10.0 * phx + 50.0, 10.0 * phy + 50.0);

        let lens_pairs: Vec<(Point2<f64>, Point2<f64>)> = grid
            .points()
            .iter()
            .map(|&(x, y)| {
                let (u, v) = cam(x, y);
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();
        let lens = fit_lens(&lens_pairs).expect("lens");

        // Burned grid: commanded dots land exactly where commanded (no field
        // distortion), imaged by the same camera map.
        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(cx, cy)| {
                let (u, v) = cam(cx, cy);
                (u, v, dot_mm * 10.0)
            })
            .collect();
        let (w, h) = (720u32, 720u32);
        let img = GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        });
        let corners_px = grid.corners_mm().map(|(cx, cy)| cam(cx, cy));

        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &lens,
            FieldScale::Refuse,
        )
        .expect("field fit");
        assert!(
            !matches!(
                cal.field_verdict.pattern,
                vision::FieldPattern::Systematic { .. }
            ),
            "a flat field must not be flagged as systematic distortion: {:?}",
            cal.field_verdict.pattern
        );
    }

    /// The step-3 field fit flags burned dots that fall outside the pixel
    /// region the step-1 lens calibration was fit over: reading them through
    /// the metric ruler extrapolates the bi-cubic, so their error would
    /// otherwise hide in the scatter floor. A lens fit over a SMALLER pixel
    /// area than the burned grid spans reports `extrapolated > 0`; a lens fit
    /// over the full area reports 0.
    #[test]
    fn field_fit_flags_dots_outside_the_lens_calibration_region() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        let cam = |x: f64, y: f64| (10.0 * x + 50.0, 10.0 * y + 50.0);

        // A flat burned grid (commanded == physical), imaged by `cam`, spanning
        // pixels ~50..650.
        let mm_to_px = homog(&[
            ((0.0, 0.0), cam(0.0, 0.0)),
            ((60.0, 0.0), cam(60.0, 0.0)),
            ((60.0, 60.0), cam(60.0, 60.0)),
            ((0.0, 60.0), cam(0.0, 60.0)),
        ]);
        let img = render_grid(&grid, &mm_to_px, dot_mm, 720, 720);
        let corners_px = grid.corners_mm().map(|(x, y)| cam(x, y));

        // Lens fit over ONLY the inner 4×4 block (commanded 10..40 mm → pixels
        // ~150..450): the burned grid's outer ring lands outside this box.
        let inner: Vec<(Point2<f64>, Point2<f64>)> = (1..5)
            .flat_map(|r| (1..5).map(move |c| (c as f64 * 10.0, r as f64 * 10.0)))
            .map(|(x, y)| {
                let (u, v) = cam(x, y);
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();
        let inner_lens = fit_lens(&inner).expect("inner lens");
        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &inner_lens,
            FieldScale::Refuse,
        )
        .expect("field fit over inner lens");
        assert!(
            cal.extrapolated > 0,
            "outer dots must be flagged, got {}",
            cal.extrapolated
        );
        assert!(
            cal.extrapolated < cal.found,
            "not every dot is outside: {}/{}",
            cal.extrapolated,
            cal.found
        );

        // Lens fit over the FULL grid (pixels ~50..650): every burned dot is
        // inside, so nothing is flagged.
        let full: Vec<(Point2<f64>, Point2<f64>)> = grid
            .points()
            .iter()
            .map(|&(x, y)| {
                let (u, v) = cam(x, y);
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();
        let full_lens = fit_lens(&full).expect("full lens");
        let cal = fit_laser_field(
            &img,
            corners_px,
            &grid,
            dot_mm,
            DotKind::Dark,
            &full_lens,
            FieldScale::Refuse,
        )
        .expect("field fit over full lens");
        assert_eq!(cal.extrapolated, 0, "all dots within the calibrated region");
    }

    #[test]
    fn too_few_dots_is_an_error() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 3,
        };
        // A blank frame → nothing detected.
        let img = GrayImage::from_pixel(200, 200, image::Luma([200]));
        let corners = [(10.0, 10.0), (190.0, 10.0), (190.0, 190.0), (10.0, 190.0)];
        assert!(fit_camera_to_machine(&img, corners, &grid, 1.0, DotKind::Dark).is_err());
    }

    /// End-to-end gate: load the committed distorted-grid fixture (a real PNG
    /// rendered by the `gen_distorted_grid` example — perspective + 5% barrel)
    /// and confirm the camera-lens calibration recovers all 49 dots to a tight
    /// RMS. This proves the calibration works on an on-disk image, not just an
    /// in-memory render.
    #[test]
    fn calibrates_from_the_distorted_grid_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../samples/calibration/grid-7x7-10mm-distorted.png"
        );
        let img = image::open(path)
            .expect("distorted-grid fixture present")
            .to_luma8();
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        // The four corner dots (lower-left, lower-right, upper-right,
        // upper-left) as recorded in the fixture's JSON sidecar.
        let corners = [
            (42.506, 632.64),
            (620.163, 618.878),
            (606.277, 39.892),
            (43.744, 41.83),
        ];
        let cal =
            fit_camera_lens(&img, corners, &grid, 2.0, DotKind::Dark).expect("calibrate fixture");
        assert_eq!(cal.found, 49, "all dots detected");
        assert!(
            cal.lens.rms_um < 60.0,
            "recovered RMS {:.1} µm too high",
            cal.lens.rms_um
        );
        // The raw barrel the fit had to absorb is well above its residual —
        // proving the fixture carries distortion a homography couldn't model.
        let max_distort = cal
            .dots
            .iter()
            .map(|d| (d.distort_px.0.powi(2) + d.distort_px.1.powi(2)).sqrt())
            .fold(0.0_f64, f64::max);
        assert!(
            max_distort > 5.0,
            "fixture should carry real distortion: {max_distort:.1} px"
        );

        // An identity laser field makes the composed commanded→camera map
        // exactly the fitted camera-lens map. Compare it with the best single
        // homography over the same detected dots: the homography still bows at
        // the field edges, while the nonlinear composition stays within the
        // calibration's 60 µm fixture tolerance.
        let mm = grid.points();
        let mm_px: Vec<_> = mm
            .iter()
            .zip(&cal.dots)
            .map(|(&(x, y), d)| (Point2::new(x, y), Point2::new(d.px.0, d.px.1)))
            .collect();
        let hom = fit_homography(&mm_px).expect("perspective reference");
        let identity_pairs: Vec<_> = mm
            .iter()
            .map(|&(x, y)| (Point2::new(x, y), Point2::new(x, y)))
            .collect();
        let field = fit_field(&identity_pairs).expect("identity field");
        let mut hom_worst_um = 0.0_f64;
        let mut composed_worst_um = 0.0_f64;
        for &(x, y) in &mm {
            let hp = hom.apply(Point2::new(x, y));
            let hm = cal.lens.px_to_mm.apply(hp.x, hp.y);
            hom_worst_um =
                hom_worst_um.max(((hm.0 - x).powi(2) + (hm.1 - y).powi(2)).sqrt() * 1000.0);

            let cp = commanded_to_camera_px(&cal.lens, &Rigid2::IDENTITY, &field, (x, y)).unwrap();
            let cm = cal.lens.px_to_mm.apply(cp.0, cp.1);
            composed_worst_um =
                composed_worst_um.max(((cm.0 - x).powi(2) + (cm.1 - y).powi(2)).sqrt() * 1000.0);
        }
        assert!(
            hom_worst_um > 200.0,
            "the fixture's curvature should beat a homography: {hom_worst_um:.0} µm"
        );
        assert!(
            composed_worst_um < 60.0,
            "composed nonlinear projection stays calibrated: {composed_worst_um:.1} µm"
        );
    }
}
