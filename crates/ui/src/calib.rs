//! Camera→laser calibration: learn where the laser's *commanded* coordinates
//! land in the camera image, so a placement in the camera view can be turned
//! into machine coordinates the laser actually burns at.
//!
//! Fiducials tie the design to the board; they do NOT tie the camera to the
//! laser. That second link is what makes "place it here → burn it here" true,
//! and this module measures it: the operator burns a grid of dots at known
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
    BedMap, FiducialProfile, FieldMap, FieldVerdict, Homography, LensMap, Poly2,
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
        match self {
            DotKind::Dark => FiducialProfile::DarkDot {
                diameter_mm: dot_mm,
            },
            DotKind::Bright => FiducialProfile::Backlit {
                diameter_mm: dot_mm,
            },
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
pub(crate) fn refit_anchor_dots(dots: &[AnchorDot], total: usize) -> Result<Calibration, String> {
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
    /// Field error at this dot: `|physical − commanded|`, µm. This is what the
    /// pre-distortion cancels.
    pub field_um: f64,
    /// Post-fit residual of the field map at this dot, µm.
    pub resid_um: f64,
}

/// A fitted laser field-distortion calibration: the `physical ↔ commanded`
/// pre-distortion map plus per-dot feedback.
#[derive(Debug, Clone)]
pub struct FieldCal {
    /// Physical machine mm → commanded mm (apply to emitted geometry).
    pub field: FieldMap,
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
}

/// Map a camera pixel into the laser's **commanded** coordinate frame by
/// composing the metric camera calibration with the measured laser field.
/// Returns `None` instead of allowing a non-finite calibration value to reach
/// placement or an operator overlay.
pub fn camera_px_to_commanded(
    lens: &LensMap,
    field: &FieldMap,
    px: (f64, f64),
) -> Option<(f64, f64)> {
    finite_input(px)?;
    let physical = finite_output(lens.px_to_mm.apply(px.0, px.1))?;
    finite_output(field.to_commanded.apply(physical.0, physical.1))
}

/// Project a laser **commanded** coordinate to the camera by first predicting
/// where the field optics physically land it, then imaging that physical point
/// through the camera lens calibration.
pub fn commanded_to_camera_px(
    lens: &LensMap,
    field: &FieldMap,
    commanded: (f64, f64),
) -> Option<(f64, f64)> {
    finite_input(commanded)?;
    let physical = invert_poly(&field.to_commanded, &field.to_physical, commanded)?;
    physical_to_camera_px(lens, physical)
}

/// Camera pixel to desired physical bed millimeters. Field-corrected placement
/// uses this direction: the emit path applies `FieldMap::to_commanded` later,
/// exactly once.
pub fn camera_px_to_physical(lens: &LensMap, px: (f64, f64)) -> Option<(f64, f64)> {
    finite_input(px)?;
    finite_output(lens.px_to_mm.apply(px.0, px.1))
}

/// Desired physical bed millimeters to camera pixels. This is the display half
/// of field-corrected placement; composing the field map here as well would
/// double-compensate the job.
pub fn physical_to_camera_px(lens: &LensMap, physical: (f64, f64)) -> Option<(f64, f64)> {
    finite_input(physical)?;
    invert_poly(&lens.px_to_mm, &lens.mm_to_px, physical)
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
pub fn field_live_acceptance(cal: &FieldCal, grid: &GridSpec) -> Result<(), String> {
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
        || cal.field.rms_um > 50.0
        || cal.field.max_um > 100.0
    {
        return Err(format!(
            "fit residual RMS {:.0} µm, worst {:.0} µm; limits are 50/100 µm",
            cal.field.rms_um, cal.field.max_um
        ));
    }
    Ok(())
}

/// Fit the laser field pre-distortion from a frame of the **burned** grid and
/// the four hand-marked corner-dot pixels. `lens` is the camera lens map (from
/// [`fit_camera_lens`]) that turns the camera into a metric ruler: each burned
/// dot's detected pixel is mapped to its true physical mm, paired with the
/// commanded coordinate it was burned at, and a `physical → commanded`
/// polynomial is fit. Emitting geometry through it cancels the field distortion.
///
/// The camera must not have moved between the lens fit and this frame — both
/// share the metric frame the lens map defines.
pub fn fit_laser_field(
    frame: &GrayImage,
    corners_px: [(f64, f64); 4],
    grid: &GridSpec,
    dot_mm: f64,
    kind: DotKind,
    lens: &LensMap,
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
    // Detected px → true physical mm (camera-lens metric), paired with the
    // commanded coordinate each dot was burned at.
    let field_pairs: Vec<(Point2<f64>, Point2<f64>)> = pairs
        .iter()
        .map(|(fpx, cmd)| {
            let (phx, phy) = lens.px_to_mm.apply(fpx.x, fpx.y);
            (Point2::new(phx, phy), Point2::new(cmd.x, cmd.y))
        })
        .collect();
    let field = fit_field(&field_pairs).map_err(|e| format!("field fit: {e}"))?;
    // A linear physical-mm → px map for the Place overlay: fit a homography
    // from each dot's physical position to its detected pixel.
    let to_px_pairs: Vec<(Point2<f64>, Point2<f64>)> = pairs
        .iter()
        .zip(&field_pairs)
        .map(|((fpx, _), (phys, _))| (*phys, *fpx))
        .collect();
    let to_px = fit_homography(&to_px_pairs).map_err(|e| format!("overlay fit: {e}"))?;

    let field_verdict = classify_field_error(
        &field_pairs
            .iter()
            .map(|(phys, cmd)| (*cmd, Vector2::new(phys.x - cmd.x, phys.y - cmd.y)))
            .collect::<Vec<_>>(),
    );

    let dots: Vec<FieldDot> = pairs
        .iter()
        .zip(&field_pairs)
        .map(|((fpx, cmd), (phys, _))| {
            let field_um = ((phys.x - cmd.x).powi(2) + (phys.y - cmd.y).powi(2)).sqrt() * 1000.0;
            let (gx, gy) = field.precompensate(phys.x, phys.y);
            let resid_um = ((gx - cmd.x).powi(2) + (gy - cmd.y).powi(2)).sqrt() * 1000.0;
            FieldDot {
                px: (fpx.x, fpx.y),
                physical_mm: (phys.x, phys.y),
                commanded_mm: (cmd.x, cmd.y),
                field_um,
                resid_um,
            }
        })
        .collect();
    Ok(FieldCal {
        field,
        to_px,
        dots,
        found,
        total,
        field_verdict,
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
        vision::Poly2::from_coeffs(&c)
    }

    fn affine_maps() -> (LensMap, FieldMap) {
        // Camera: physical x=(px-20)/10, y=(800-py)/10.
        let lens = LensMap {
            px_to_mm: affine_poly([0.1, 0.0, -2.0], [0.0, -0.1, 80.0]),
            mm_to_px: affine_poly([10.0, 0.0, 20.0], [0.0, -10.0, 800.0]),
            rms_um: 0.0,
            max_um: 0.0,
            residuals: vec![],
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
        let px = commanded_to_camera_px(&lens, &field, commanded).expect("finite projection");
        let got = camera_px_to_commanded(&lens, &field, px).expect("finite inverse");
        assert!((got.0 - commanded.0).abs() < 1e-9);
        assert!((got.1 - commanded.1).abs() < 1e-9);

        let physical = camera_px_to_physical(&lens, px).expect("metric camera");
        let px2 = physical_to_camera_px(&lens, physical).expect("camera projection");
        assert!((px2.0 - px.0).abs() < 1e-9 && (px2.1 - px.1).abs() < 1e-9);
    }

    #[test]
    fn composed_camera_command_mapping_rejects_non_finite_values() {
        let (lens, mut field) = affine_maps();
        let mut coeffs = field.to_physical.to_coeffs();
        coeffs[0] = f64::NAN;
        field.to_physical = vision::Poly2::from_coeffs(&coeffs);
        assert!(!composed_projection_is_finite(&lens, &field));
        assert!(commanded_to_camera_px(&lens, &field, (10.0, 10.0)).is_none());
        assert!(camera_px_to_commanded(&lens, &field, (f64::INFINITY, 2.0)).is_none());
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

        let cal = fit_laser_field(&img, corners_px, &grid, dot_mm, DotKind::Dark, &lens)
            .expect("field fit");
        assert_eq!(cal.found, 49, "all dots detected");
        assert!(cal.field.rms_um < 60.0, "field RMS {} µm", cal.field.rms_um);
        field_live_acceptance(&cal, &grid).expect("well-covered field fit is accepted");

        let mut missing_corner = cal.clone();
        missing_corner
            .dots
            .retain(|d| d.commanded_mm != grid.corners_mm()[0]);
        missing_corner.found = missing_corner.dots.len();
        assert!(
            field_live_acceptance(&missing_corner, &grid)
                .unwrap_err()
                .contains("boundary corners"),
            "a center-heavy fit cannot activate correction"
        );

        let mut inaccurate = cal.clone();
        inaccurate.field.max_um = 101.0;
        assert!(
            field_live_acceptance(&inaccurate, &grid)
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

        let cal = fit_laser_field(&img, corners_px, &grid, dot_mm, DotKind::Dark, &lens)
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

            let cp = commanded_to_camera_px(&cal.lens, &field, (x, y)).unwrap();
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
