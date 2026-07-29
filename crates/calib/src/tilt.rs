//! Height-induced parallax: the view-ray geometry implied by the ① lens fit.
//!
//! The ① camera-lens calibration is a *plane* measurement. Every `px → mm`
//! read it produces is the point where that pixel's view ray crosses the plane
//! the paper grid lay on. The operator's camera does not look straight down, so
//! a feature that actually sits `h` mm ABOVE that plane is read at the wrong
//! place: its ray keeps going and meets the calibration plane further from the
//! camera. Nothing downstream can see this — the fit's residuals are perfect,
//! the answer is just for the wrong surface.
//!
//! Sign convention used throughout: **`h` is positive UPWARD, toward the
//! camera**, measured from the ① calibration plane. `h = 0` is the ① plane
//! itself, so a zero height is exactly the uncorrected behaviour.
//!
//! The geometry is recovered from the lens fit alone. Sampling `px_to_mm` over
//! the region the fit covers and re-fitting a plane homography `mm → px`
//! exposes the whole of it. Written out once, symbolically: put the ① plane at
//! `z = 0`, the projection centre at `(0, 0, H)` — so `H` is its VERTICAL
//! height and the nadir is the origin — and lean the optical axis by `θ` toward
//! `+y`. A plane point `(x, y)` then images at
//!
//! ```text
//!     u = f·x / w      v = f·(y·cosθ − H·sinθ) / w      w = y·sinθ + H·cosθ
//! ```
//!
//! so the `mm → px` homography's perspective row is `(0, sinθ, H·cosθ)` and its
//! `w` is exactly the AXIAL depth of the plane point it is evaluated at:
//! `w = H·cosθ` under the nadir, `w = H/cosθ` where the optical axis lands.
//! Vertical height and axial depth are different lengths, and which one a
//! relation produces is the whole subtlety here.
//!
//! Two readings come out of that homography, both taken at the centre `c` of
//! the covered region (write `w_c` for its axial depth):
//!
//! * the **perspective gradient**, with `w` normalized to 1 at `c`, is
//!   `∇w = (0, sinθ) / w_c`. So `|∇w| = sinθ / w_c` measures the AXIAL depth at
//!   the centre, not the standoff, and points along INCREASING depth (away from
//!   the camera).
//! * the **Jacobian** `d(px)/d(mm) = (f/w²)·[[w, −x·sinθ], [0, H]]`, which on
//!   the `x = 0` line of the tilt is diagonal: `f/w` across the tilt and
//!   `f·H/w²` along it. The anisotropy along the depth axis is therefore
//!   `H / w_c` — below 1 out past the nadir, which is the familiar
//!   foreshortening, but ABOVE 1 between the nadir and the camera's principal
//!   plane, where the `1/w²` term wins instead.
//!
//! That is still one measurement short of the geometry: one plane view of an
//! uncalibrated pinhole leaves a one-parameter family (trade focal length off
//! against tilt), so `θ` has to be pinned by a convention. The convention here
//! is the usual unsigned reading, `cos(tilt) = σ_min / σ_max`, which is exact
//! at both of the places a fit region actually sits — centred on the optical
//! axis (`w_c = H/cosθ`, anisotropy `cosθ`) or centred on the nadir
//! (`w_c = H·cosθ`, anisotropy `1/cosθ`). The SIGNED anisotropy then says which
//! of the two it was, and the rest is exact:
//!
//! ```text
//!     w_c   = sinθ / |∇w|                      axial depth at the centre
//!     H     = w_c · (σ_along / σ_across)       vertical standoff
//!     nadir = c − (w_c − H·cosθ)/sinθ · away   the plumb point
//! ```
//!
//! Together those give tilt and distance separately, which one of them alone
//! cannot. Caveat worth stating plainly: the polynomial lens map also carries
//! genuine barrel/pincushion curvature, and a homography fit to it absorbs some
//! of that curvature into `∇w`. The recovered tilt is therefore a *model of the
//! fitted map*, not an independent metrology of the camera mount — which is why
//! the console reports the derived numbers rather than silently trusting them.

use nalgebra::Point2;
use vision::{LensMap, fit_homography};

/// Samples per axis used to re-fit the plane homography over the lens map.
const SAMPLES: usize = 7;
/// Below this depth gradient (per mm) the fitted map has no usable perspective
/// at all — an orthographic or purely affine fit. Reported as "no tilt model".
const MIN_DEPTH_GRADIENT: f64 = 1e-6;
/// Tilts outside this band are not a camera looking at a bed; they are a fit
/// artifact. Refuse rather than emit a confident nonsense correction.
const MIN_TILT_RAD: f64 = 0.5_f64 * std::f64::consts::PI / 180.0;
const MAX_TILT_RAD: f64 = 75.0_f64 * std::f64::consts::PI / 180.0;
/// Plausible perpendicular standoff of a bench camera above the bed, mm.
const MIN_HEIGHT_MM: f64 = 10.0;
const MAX_HEIGHT_MM: f64 = 5_000.0;

/// The camera's view-ray geometry above the ① calibration plane, recovered
/// from a lens fit by [`camera_tilt_from_lens`].
///
/// All lengths and coordinates are in the ① map's own output frame ("paper
/// mm") — the frame `LensMap::px_to_mm` emits, BEFORE the burned-grid rigid
/// alignment into machine mm. Correcting there keeps the correction a pure
/// plane-change: the frame is a rigid transform, so applying it afterward
/// rotates the shift without changing its length.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraTilt {
    /// Angle between the camera's optical axis and the plane normal, radians.
    /// Zero would be a perfectly perpendicular view.
    pub tilt_rad: f64,
    /// Axial depth of [`CameraTilt::center_mm`], mm: how far the centre of the
    /// covered region lies from the projection centre measured ALONG the
    /// optical axis. Shorter than the straight-line distance to that point, and
    /// unequal to `height_mm` except at zero tilt.
    pub working_distance_mm: f64,
    /// VERTICAL standoff of the projection centre above the ① plane, mm — the
    /// plumb-line length, not an axial one. That is the length the parallax
    /// divides by: a point `h` mm (also vertical) above the plane is read
    /// `h / height_mm` of its distance-from-nadir too far out.
    pub height_mm: f64,
    /// Where the camera's plumb line meets the ① plane, paper mm. Parallax is
    /// purely radial about this point.
    pub nadir_mm: (f64, f64),
    /// Unit direction of increasing depth in the ① plane — the foreshortened
    /// axis, pointing AWAY from the camera. Paper mm.
    pub away_dir: (f64, f64),
    /// Centre of the region the lens fit covers, paper mm. The console quotes
    /// the applied correction here.
    pub center_mm: (f64, f64),
}

impl CameraTilt {
    /// Where a point that truly sits `h_mm` above the ① plane is READ by a
    /// `px → mm` lookup, which assumes everything lies on that plane.
    ///
    /// Exactly, the ray from the projection centre `(nadir, H)` through
    /// `(p, h)` meets `z = 0` at `nadir + (p − nadir)·H/(H − h)`: purely radial
    /// about the nadir, and scaled by two VERTICAL lengths — which is why
    /// `height_mm` has to be the plumb standoff and `h_mm` a plumb height.
    /// Taken to first order in `h / height_mm` the factor is `1 + h/height_mm`.
    pub fn reading_of(&self, true_mm: (f64, f64), h_mm: f64) -> (f64, f64) {
        let k = 1.0 + h_mm / self.height_mm;
        (
            self.nadir_mm.0 + (true_mm.0 - self.nadir_mm.0) * k,
            self.nadir_mm.1 + (true_mm.1 - self.nadir_mm.1) * k,
        )
    }

    /// Inverse of [`CameraTilt::reading_of`]: the true position of a feature
    /// known to sit `h_mm` above the ① plane, from its plane reading.
    pub fn true_at(&self, reading_mm: (f64, f64), h_mm: f64) -> Option<(f64, f64)> {
        self.restate(reading_mm, h_mm, 0.0)
    }

    /// Restate a ①-plane reading: given that the feature really sits `from_mm`
    /// above the plane, return the reading the SAME feature would have produced
    /// had it sat at `to_mm` instead.
    ///
    /// This is the whole correction. Every map downstream of the lens (the
    /// burned-grid frame, the ③ field polynomial) was built from readings of
    /// features at the ③ burn plane, so it is keyed on readings at that height;
    /// a mark on a surface at another height has to be restated into that
    /// convention before it is handed over. Equal heights — and in particular
    /// the shipped default of `0` for both — return the reading unchanged.
    pub fn restate(&self, reading_mm: (f64, f64), from_mm: f64, to_mm: f64) -> Option<(f64, f64)> {
        // Short-circuit rather than scale by a computed 1.0: `nadir + (p −
        // nadir)·1.0` is not bit-for-bit `p`, and "the defaults change nothing"
        // has to mean nothing, not a rounding step.
        if from_mm == to_mm {
            return Some(reading_mm);
        }
        let from_k = 1.0 + from_mm / self.height_mm;
        let to_k = 1.0 + to_mm / self.height_mm;
        // `from_k` is the perspective foreshortening the reading already
        // carries; at the camera's own height it collapses to zero and the
        // reading carries no position at all.
        if from_k.abs() < 1e-6 {
            return None;
        }
        let k = to_k / from_k;
        let p = (
            self.nadir_mm.0 + (reading_mm.0 - self.nadir_mm.0) * k,
            self.nadir_mm.1 + (reading_mm.1 - self.nadir_mm.1) * k,
        );
        (p.0.is_finite() && p.1.is_finite()).then_some(p)
    }

    /// Bearing of the foreshortened axis in a frame reached by `rotate`
    /// (radians, applied to [`CameraTilt::away_dir`]), degrees CCW from +X.
    /// The console passes the paper→machine rotation so the operator reads the
    /// direction in machine axes.
    pub fn bearing_deg(&self, rotate: impl Fn((f64, f64)) -> (f64, f64)) -> f64 {
        let d = rotate(self.away_dir);
        d.1.atan2(d.0).to_degrees()
    }
}

/// Recover the view-ray geometry from a ① lens fit, or `None` when the fit
/// carries no usable perspective (see the module docs for what is checked).
pub fn camera_tilt_from_lens(lens: &LensMap) -> Option<CameraTilt> {
    let [x0, y0, x1, y1] = fit_px_box(lens)?;
    let mut pairs = Vec::with_capacity(SAMPLES * SAMPLES);
    let step = (SAMPLES - 1) as f64;
    for i in 0..SAMPLES {
        for j in 0..SAMPLES {
            let px = x0 + (x1 - x0) * i as f64 / step;
            let py = y0 + (y1 - y0) * j as f64 / step;
            let mm = lens.px_to_mm.apply(px, py);
            if !mm.0.is_finite() || !mm.1.is_finite() {
                return None;
            }
            pairs.push((Point2::new(mm.0, mm.1), Point2::new(px, py)));
        }
    }
    let h = fit_homography(&pairs).ok()?;
    let m = &h.matrix;
    // Everything is evaluated at the centre of the covered region: the
    // perspective row is exact everywhere, but the anisotropy and the "one
    // tilt" abstraction are a linearization, and the centre is where it is
    // least wrong.
    let center = lens.px_to_mm.apply((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    if !center.0.is_finite() || !center.1.is_finite() {
        return None;
    }
    let w = m[(2, 0)] * center.0 + m[(2, 1)] * center.1 + m[(2, 2)];
    if !w.is_finite() || w.abs() < 1e-12 {
        return None;
    }
    // ∇w with depth normalized to 1 at the centre, i.e. per-mm fractional
    // depth change: sin(tilt) / axial depth of the centre.
    let grad = (m[(2, 0)] / w, m[(2, 1)] / w);
    let grad_mag = grad.0.hypot(grad.1);
    if !grad_mag.is_finite() || grad_mag < MIN_DEPTH_GRADIENT {
        return None;
    }
    let away_dir = (grad.0 / grad_mag, grad.1 / grad_mag);

    // Jacobian of the projective map at the centre, d(px)/d(mm).
    let px_c = (m[(0, 0)] * center.0 + m[(0, 1)] * center.1 + m[(0, 2)]) / w;
    let py_c = (m[(1, 0)] * center.0 + m[(1, 1)] * center.1 + m[(1, 2)]) / w;
    let j = [
        (m[(0, 0)] - px_c * m[(2, 0)]) / w,
        (m[(0, 1)] - px_c * m[(2, 1)]) / w,
        (m[(1, 0)] - py_c * m[(2, 0)]) / w,
        (m[(1, 1)] - py_c * m[(2, 1)]) / w,
    ];
    let (s_max, s_min) = singular_values_2x2(j)?;
    if s_max <= 0.0 {
        return None;
    }
    let cos_tilt = (s_min / s_max).clamp(0.0, 1.0);
    let tilt_rad = cos_tilt.acos();
    if !(MIN_TILT_RAD..=MAX_TILT_RAD).contains(&tilt_rad) {
        return None;
    }
    // `|∇w|` measures the AXIAL depth of the centre; the vertical standoff is
    // that times the anisotropy read ALONG the depth axis, `H / w_c`. Unsigned,
    // `σ_min/σ_max` is that ratio only when the centre lies past the nadir and
    // the depth axis is the foreshortened one; centred nearer the nadir the
    // same axis is MAGNIFIED and the ratio is its reciprocal. Taking the
    // foreshortened case for granted is what leaves a nadir-centred fit short
    // by cos²(tilt).
    let working_distance_mm = tilt_rad.sin() / grad_mag;
    let anisotropy = if along_is_magnified(j, away_dir) {
        1.0 / cos_tilt
    } else {
        cos_tilt
    };
    let height_mm = working_distance_mm * anisotropy;
    if !height_mm.is_finite() || !(MIN_HEIGHT_MM..=MAX_HEIGHT_MM).contains(&height_mm) {
        return None;
    }
    // Both depths are now known, and `w_c = offset·sin(tilt) + H·cos(tilt)`
    // inverts to the centre's distance from the nadir along the depth axis —
    // `H·tan(tilt)` for a fit centred on the optical axis, zero for one centred
    // on the nadir itself.
    let offset = (working_distance_mm - height_mm * cos_tilt) / tilt_rad.sin();
    let nadir_mm = (
        center.0 - offset * away_dir.0,
        center.1 - offset * away_dir.1,
    );
    Some(CameraTilt {
        tilt_rad,
        working_distance_mm,
        height_mm,
        nadir_mm,
        away_dir,
        center_mm: center,
    })
}

/// The pixel box the lens fit actually covers: the recorded input bounds when
/// the map carries them, otherwise the normalization window every `Poly2`
/// stores (`center ± 1/scale`), which is the same region by construction.
fn fit_px_box(lens: &LensMap) -> Option<[f64; 4]> {
    if let Some(b @ [x0, y0, x1, y1]) = lens.calib_px_bounds
        && b.iter().all(|v| v.is_finite())
        && x1 - x0 > 1.0
        && y1 - y0 > 1.0
    {
        return Some(b);
    }
    let c = lens.px_to_mm.to_coeffs();
    let (cx, cy, scale) = (c[20], c[21], c[22]);
    if !cx.is_finite() || !cy.is_finite() || !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let half = 1.0 / scale;
    Some([cx - half, cy - half, cx + half, cy + half])
}

/// Whether the Jacobian `j` stretches the depth axis `away` MORE than the axis
/// across it. The two singular values say by how much; this says which way
/// round, which is what separates a fit centred on the nadir from one centred
/// on the optical axis.
fn along_is_magnified(j: [f64; 4], away: (f64, f64)) -> bool {
    let scale = |d: (f64, f64)| (j[0] * d.0 + j[1] * d.1).hypot(j[2] * d.0 + j[3] * d.1);
    scale(away) > scale((-away.1, away.0))
}

/// Singular values of a row-major 2×2, largest first. Closed form (the
/// standard half-sum/half-difference decomposition) — a 2×2 does not need an
/// iterative SVD, and this keeps the derivation readable next to the geometry
/// it feeds.
fn singular_values_2x2(m: [f64; 4]) -> Option<(f64, f64)> {
    if !m.iter().all(|v| v.is_finite()) {
        return None;
    }
    let [a, b, c, d] = m;
    let e = (a + d) / 2.0;
    let f = (a - d) / 2.0;
    let g = (b + c) / 2.0;
    let h = (b - c) / 2.0;
    let q = e.hypot(h);
    let r = f.hypot(g);
    Some((q + r, (q - r).abs()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vision::fit_lens;

    /// A synthetic pinhole viewing the bed at a known tilt, with no lens
    /// curvature: the recovered geometry must be the geometry we built.
    ///
    /// The camera sits `height` above the plane, its optical axis leaning by
    /// `tilt` toward +y, so +y is the direction of increasing depth. The nadir
    /// is the origin and the 60 mm patch the fit covers is centred `patch_y` up
    /// the depth axis from it — `0` for a camera looking at its own plumb
    /// point, `height · tan(tilt)` for one whose patch is centred on the
    /// optical axis, which is where a real bench camera's frame sits.
    fn tilted_lens_at(patch_y: f64, height_mm: f64, tilt_rad: f64, focal_px: f64) -> LensMap {
        let nadir = (0.0, 0.0);
        let pairs: Vec<_> = (0..7)
            .flat_map(|i| (0..7).map(move |j| (i, j)))
            .map(|(i, j)| {
                let x = -30.0 + 10.0 * i as f64;
                let y = patch_y - 30.0 + 10.0 * j as f64;
                // Camera frame: rotate the ray about the x axis by `tilt`.
                let (dx, dy, dz) = (x - nadir.0, y - nadir.1, height_mm);
                let (s, c) = tilt_rad.sin_cos();
                let cam_y = c * dy - s * dz;
                let cam_z = s * dy + c * dz;
                (
                    Point2::new(focal_px * dx / cam_z, focal_px * cam_y / cam_z),
                    Point2::new(x, y),
                )
            })
            .collect();
        fit_lens(&pairs).unwrap()
    }

    /// The nadir-centred patch, which is what most of these tests use.
    fn tilted_lens(height_mm: f64, tilt_rad: f64, focal_px: f64) -> LensMap {
        tilted_lens_at(0.0, height_mm, tilt_rad, focal_px)
    }

    /// The patch centred where the optical axis lands, as a bench camera's
    /// frame is. Same geometry, opposite sign of the Jacobian anisotropy.
    fn axis_centred_lens(height_mm: f64, tilt_rad: f64, focal_px: f64) -> LensMap {
        tilted_lens_at(height_mm * tilt_rad.tan(), height_mm, tilt_rad, focal_px)
    }

    /// Several standoffs and tilts, both patch placements: the height has to
    /// come back right for all of them, so a `cos(tilt)` too many or too few —
    /// which cancels itself at one configuration — cannot pass.
    #[test]
    fn recovers_a_known_tilt_and_standoff() {
        for (deg, height) in [(25.0_f64, 300.0_f64), (15.0, 400.0), (35.0, 200.0)] {
            let tilt = deg.to_radians();
            for (what, lens) in [
                ("nadir-centred", tilted_lens(height, tilt, 1200.0)),
                ("axis-centred", axis_centred_lens(height, tilt, 1200.0)),
            ] {
                let t = camera_tilt_from_lens(&lens).expect("tilt model");
                assert!(
                    (t.tilt_rad.to_degrees() - deg).abs() < 1.0,
                    "{what} {deg} deg: tilt {} deg",
                    t.tilt_rad.to_degrees()
                );
                assert!(
                    (t.height_mm - height).abs() < 0.05 * height,
                    "{what} {deg} deg: height {} mm vs {height}",
                    t.height_mm
                );
                // The camera leans toward +y, so depth increases toward +y.
                assert!(
                    t.away_dir.1 > 0.9 && t.away_dir.0.abs() < 0.2,
                    "{what} {deg} deg: away {:?}",
                    t.away_dir
                );
                // And the plumb point is where we put it, whichever patch the
                // fit covered.
                assert!(
                    t.nadir_mm.0.abs() < 0.03 * height && t.nadir_mm.1.abs() < 0.03 * height,
                    "{what} {deg} deg: nadir {:?}",
                    t.nadir_mm
                );
            }
        }
    }

    /// A perpendicular camera has no parallax to model, and says so rather
    /// than returning a tiny made-up tilt.
    #[test]
    fn a_perpendicular_camera_has_no_tilt_model() {
        let lens = tilted_lens(300.0, 0.0, 1200.0);
        assert!(camera_tilt_from_lens(&lens).is_none());
    }

    #[test]
    fn restating_between_equal_heights_is_the_identity() {
        let lens = tilted_lens(300.0, 25.0_f64.to_radians(), 1200.0);
        let t = camera_tilt_from_lens(&lens).unwrap();
        let p = (12.0, -7.0);
        let out = t.restate(p, 3.0, 3.0).unwrap();
        assert!((out.0 - p.0).abs() < 1e-12 && (out.1 - p.1).abs() < 1e-12);
    }

    /// The headline number: for a camera whose frame is centred on its optical
    /// axis — the bench case, and the one the console's own tilt tests build —
    /// a surface 1.6 mm above the calibration plane reads `h · tan(tilt)` too
    /// far out at the field centre, along the foreshortened axis. The centre
    /// sits `H · tan(tilt)` from the nadir and the parallax there is
    /// `h/H` of that, so the standoff cancels and the tangent is left.
    #[test]
    fn a_raised_surface_reads_h_tan_tilt_too_far_from_the_nadir() {
        let tilt = 25.0_f64.to_radians();
        let lens = axis_centred_lens(300.0, tilt, 1200.0);
        let t = camera_tilt_from_lens(&lens).unwrap();
        let reading = t.center_mm;
        let truth = t.true_at(reading, 1.6).unwrap();
        let shift = (truth.0 - reading.0, truth.1 - reading.1);
        let expected = 1.6 * tilt.tan();
        assert!(
            (shift.1.hypot(shift.0) - expected).abs() < 0.05,
            "shift {shift:?} vs {expected}"
        );
        // Toward the camera, i.e. against the increasing-depth direction.
        assert!(shift.1 < 0.0, "shift {shift:?} should point back at -y");
    }
}
