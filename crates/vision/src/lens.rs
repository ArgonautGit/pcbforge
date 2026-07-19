//! Camera lens-distortion model: a bi-cubic 2-D polynomial mapping camera
//! pixels to true bed millimeters (and back), fit from an imaged grid of known
//! geometry (e.g. a printed reference grid of known pitch).
//!
//! A homography models only a flat plane under a tilted camera (perspective) —
//! it cannot represent the lens's barrel/pincushion *curvature*. A degree-3
//! polynomial does, so once fit the camera becomes a metric ruler across the
//! whole field: straight → curved and back is captured, and residuals report
//! how well (µm). The map is fit both directions (`px→mm` for measurement,
//! `mm→px` for drawing the corrected grid back onto the image).

use nalgebra::{DMatrix, DVector, Point2, Vector2};

/// Full 4×4 tensor-product bicubic basis of normalized coordinates. The first
/// ten entries retain the legacy total-degree-cubic order; the final six add
/// the cross terms needed to model perspective plus radial curvature together.
fn basis(u: f64, v: f64) -> [f64; 16] {
    [
        1.0,
        u,
        v,
        u * u,
        u * v,
        v * v,
        u * u * u,
        u * u * v,
        u * v * v,
        v * v * v,
        u * u * u * v,
        u * v * v * v,
        u * u * v * v,
        u * u * u * v * v,
        u * u * v * v * v,
        u * u * u * v * v * v,
    ]
}

/// A bi-cubic 2-D polynomial mapping one plane to another, with input
/// normalization for numerical conditioning.
#[derive(Debug, Clone, PartialEq)]
pub struct Poly2 {
    cx: [f64; 16],
    cy: [f64; 16],
    /// Input is normalized as `n = (raw − center) · scale` before the basis.
    center: (f64, f64),
    scale: f64,
}

impl Poly2 {
    /// Map an input point through the polynomial.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let u = (x - self.center.0) * self.scale;
        let v = (y - self.center.1) * self.scale;
        let b = basis(u, v);
        let mut ox = 0.0;
        let mut oy = 0.0;
        for ((cx, cy), bj) in self.cx.iter().zip(&self.cy).zip(&b) {
            ox += cx * bj;
            oy += cy * bj;
        }
        (ox, oy)
    }

    /// The 35 numbers that define this map: 16 `cx`, 16 `cy`, then `center.0`,
    /// `center.1`, `scale`. Round-trips through [`Poly2::from_coeffs`] — the
    /// serialization the field-correction file and the UI persist.
    pub fn to_coeffs(&self) -> [f64; 35] {
        let mut v = [0.0; 35];
        v[..16].copy_from_slice(&self.cx);
        v[16..32].copy_from_slice(&self.cy);
        v[32] = self.center.0;
        v[33] = self.center.1;
        v[34] = self.scale;
        v
    }

    /// Rebuild a map from current 35-value coefficients, or promote the legacy
    /// 23-value total-degree cubic by zero-filling the six new cross terms.
    pub fn from_coeffs(v: &[f64]) -> Poly2 {
        let mut cx = [0.0; 16];
        let mut cy = [0.0; 16];
        let (center, scale) = match v.len() {
            35 => {
                cx.copy_from_slice(&v[..16]);
                cy.copy_from_slice(&v[16..32]);
                ((v[32], v[33]), v[34])
            }
            23 => {
                cx[..10].copy_from_slice(&v[..10]);
                cy[..10].copy_from_slice(&v[10..20]);
                ((v[20], v[21]), v[22])
            }
            n => panic!("Poly2 coefficients must contain 23 or 35 values, got {n}"),
        };
        Poly2 {
            cx,
            cy,
            center,
            scale,
        }
    }

    /// Fit `src → dst` (both `(x, y)`) by least squares. Needs ≥ 16 points.
    fn fit(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Result<Poly2, String> {
        let n = src.len();
        if n < 16 {
            return Err(format!("bicubic fit needs ≥16 points, got {n}"));
        }
        // Normalize the input to ~[-1, 1] so the design matrix is well
        // conditioned (raw pixels or mm span hundreds).
        let (mut cx, mut cy) = (0.0, 0.0);
        for &(x, y) in src {
            cx += x;
            cy += y;
        }
        cx /= n as f64;
        cy /= n as f64;
        let mut half = 1e-9_f64;
        for &(x, y) in src {
            half = half.max((x - cx).abs()).max((y - cy).abs());
        }
        let scale = 1.0 / half;

        let mut a = DMatrix::<f64>::zeros(n, 16);
        for (i, &(x, y)) in src.iter().enumerate() {
            let b = basis((x - cx) * scale, (y - cy) * scale);
            for (j, bj) in b.iter().enumerate() {
                a[(i, j)] = *bj;
            }
        }
        let bx = DVector::from_iterator(n, dst.iter().map(|&(x, _)| x));
        let by = DVector::from_iterator(n, dst.iter().map(|&(_, y)| y));
        let svd = a.svd(true, true);
        // Reject a rank-deficient design (near-collinear or under-spread grid
        // points): nalgebra's `solve` happily pseudo-inverts it and returns a
        // confident minimum-norm answer that is garbage off the sampled line.
        // Rank relative to σ_max, matching `fit_affine`'s degeneracy gate.
        let smax = svd.singular_values.iter().cloned().fold(0.0_f64, f64::max);
        let rank = svd
            .singular_values
            .iter()
            .filter(|&&s| s > smax * 1e-9)
            .count();
        if rank < 16 {
            return Err(format!(
                "bicubic fit is rank-deficient (rank {rank}/16): the points are \
                 near-collinear or under-spread — image a full 2-D grid"
            ));
        }
        let sol_x = svd.solve(&bx, 1e-12).map_err(|e| format!("x fit: {e}"))?;
        let sol_y = svd.solve(&by, 1e-12).map_err(|e| format!("y fit: {e}"))?;
        let mut coeff_x = [0.0; 16];
        let mut coeff_y = [0.0; 16];
        for (i, (cx, cy)) in coeff_x.iter_mut().zip(coeff_y.iter_mut()).enumerate() {
            *cx = sol_x[i];
            *cy = sol_y[i];
        }
        Ok(Poly2 {
            cx: coeff_x,
            cy: coeff_y,
            center: (cx, cy),
            scale,
        })
    }
}

/// A fitted lens model: camera pixels ↔ true bed millimeters, with residuals.
#[derive(Debug, Clone)]
pub struct LensMap {
    /// Camera pixel → true bed mm (the metric ruler).
    pub px_to_mm: Poly2,
    /// True bed mm → camera pixel (to draw the corrected grid on the frame).
    pub mm_to_px: Poly2,
    /// RMS of the `px→mm` reprojection over the fit points, µm.
    pub rms_um: f64,
    /// Worst single-point residual, µm.
    pub max_um: f64,
    /// Per fit point: `(px_x, px_y, residual_µm)` — for the heat-map / vectors.
    pub residuals: Vec<(f64, f64, f64)>,
}

#[cfg(test)]
mod rank_tests {
    use super::*;

    /// Collinear inputs used to "fit" with ~0 RMS garbage (LR-18): nalgebra's
    /// `solve` pseudo-inverts the rank-deficient design. The rank gate now
    /// rejects them instead of returning a confident wrong map.
    #[test]
    fn rank_deficient_collinear_fit_is_rejected() {
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = (0..20)
            .map(|i| {
                let t = i as f64;
                // All points on the line y = x in both frames.
                (Point2::new(t, t), Point2::new(2.0 * t + 1.0, 2.0 * t + 1.0))
            })
            .collect();
        let err = fit_lens(&pairs).unwrap_err();
        assert!(err.contains("rank-deficient"), "got: {err}");
    }
}

/// Fit a lens model from `(pixel, true_mm)` correspondences (the imaged known
/// grid). Needs ≥ 16 non-degenerate points.
pub fn fit_lens(pairs: &[(Point2<f64>, Point2<f64>)]) -> Result<LensMap, String> {
    let px: Vec<(f64, f64)> = pairs.iter().map(|(p, _)| (p.x, p.y)).collect();
    let mm: Vec<(f64, f64)> = pairs.iter().map(|(_, m)| (m.x, m.y)).collect();
    let px_to_mm = Poly2::fit(&px, &mm)?;
    let mm_to_px = Poly2::fit(&mm, &px)?;

    let mut residuals = Vec::with_capacity(pairs.len());
    let mut sumsq = 0.0;
    let mut max = 0.0_f64;
    for (p, m) in pairs {
        let (ex, ey) = px_to_mm.apply(p.x, p.y);
        let d_um = ((ex - m.x).powi(2) + (ey - m.y).powi(2)).sqrt() * 1000.0;
        residuals.push((p.x, p.y, d_um));
        sumsq += d_um * d_um;
        max = max.max(d_um);
    }
    let rms_um = (sumsq / pairs.len() as f64).sqrt();
    Ok(LensMap {
        px_to_mm,
        mm_to_px,
        rms_um,
        max_um: max,
        residuals,
    })
}

/// A fitted **laser field** pre-distortion: the same bi-cubic machinery as
/// [`LensMap`], but between the physical machine frame and the laser's
/// *commanded* frame. Emitting `to_commanded(design)` cancels the galvo/
/// f-theta field distortion, so the beam lands on the intended geometry.
#[derive(Debug, Clone)]
pub struct FieldMap {
    /// Physical machine mm → commanded mm. Apply this to emitted geometry.
    pub to_commanded: Poly2,
    /// Commanded mm → physical machine mm (the field distortion itself), for
    /// simulation, overlays, and the round-trip check.
    pub to_physical: Poly2,
    /// RMS of the `physical→commanded` fit over the burned dots, µm.
    pub rms_um: f64,
    /// Worst single-dot residual, µm.
    pub max_um: f64,
}

impl FieldMap {
    /// Pre-distort a physical machine-mm point to the commanded mm the laser
    /// must be told so the beam lands there.
    pub fn precompensate(&self, x_mm: f64, y_mm: f64) -> (f64, f64) {
        self.to_commanded.apply(x_mm, y_mm)
    }

    /// Serialize to the field-correction file format (whitespace-separated,
    /// one directive per line) that `pcbforge register --field-map` reads.
    pub fn serialize(&self) -> String {
        let row = |c: [f64; 35]| {
            c.iter()
                .map(|v| format!("{v:.10}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        format!(
            "pcbforge-field 2\nto_commanded {}\nto_physical {}\nrms_um {:.6}\nmax_um {:.6}\n",
            row(self.to_commanded.to_coeffs()),
            row(self.to_physical.to_coeffs()),
            self.rms_um,
            self.max_um,
        )
    }

    /// Parse the field-correction file format written by [`FieldMap::serialize`].
    pub fn parse(text: &str) -> Result<FieldMap, String> {
        let mut version = None;
        let mut to_commanded: Option<Vec<f64>> = None;
        let mut to_physical: Option<Vec<f64>> = None;
        let (mut rms_um, mut max_um) = (0.0, 0.0);
        let coeffs = |rest: &str| -> Result<Vec<f64>, String> {
            rest.split_whitespace()
                .map(|s| s.parse::<f64>().map_err(|e| e.to_string()))
                .collect::<Result<_, _>>()
        };
        for line in text.lines() {
            let line = line.trim();
            let Some((key, rest)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            match key {
                "pcbforge-field" => {
                    let parsed = rest
                        .trim()
                        .parse::<u32>()
                        .map_err(|_| format!("bad field-map version {rest:?}"))?;
                    if !matches!(parsed, 1 | 2) {
                        return Err(format!("unsupported field-map version {rest:?}"));
                    }
                    version = Some(parsed);
                }
                "to_commanded" => to_commanded = Some(coeffs(rest)?),
                "to_physical" => to_physical = Some(coeffs(rest)?),
                "rms_um" => rms_um = rest.trim().parse().map_err(|_| "bad rms_um")?,
                "max_um" => max_um = rest.trim().parse().map_err(|_| "bad max_um")?,
                _ => {}
            }
        }
        let version = version.ok_or("missing pcbforge-field version")?;
        let expected = if version == 1 { 23 } else { 35 };
        let to_commanded = to_commanded.ok_or("missing to_commanded")?;
        let to_physical = to_physical.ok_or("missing to_physical")?;
        if to_commanded.len() != expected || to_physical.len() != expected {
            return Err(format!(
                "field-map v{version} expects {expected} coefficients per polynomial"
            ));
        }
        Ok(FieldMap {
            to_commanded: Poly2::from_coeffs(&to_commanded),
            to_physical: Poly2::from_coeffs(&to_physical),
            rms_um,
            max_um,
        })
    }
}

/// Fit a laser-field pre-distortion from `(physical_mm, commanded_mm)`
/// correspondences: the physical position each burned dot actually landed at
/// (read through the metric camera-lens map) paired with the commanded
/// coordinate it was burned at. Needs ≥ 16 non-degenerate points.
pub fn fit_field(pairs: &[(Point2<f64>, Point2<f64>)]) -> Result<FieldMap, String> {
    let phys: Vec<(f64, f64)> = pairs.iter().map(|(p, _)| (p.x, p.y)).collect();
    let cmd: Vec<(f64, f64)> = pairs.iter().map(|(_, c)| (c.x, c.y)).collect();
    let to_commanded = Poly2::fit(&phys, &cmd)?;
    let to_physical = Poly2::fit(&cmd, &phys)?;
    let mut sumsq = 0.0;
    let mut max = 0.0_f64;
    for (p, c) in pairs {
        let (ex, ey) = to_commanded.apply(p.x, p.y);
        let d_um = ((ex - c.x).powi(2) + (ey - c.y).powi(2)).sqrt() * 1000.0;
        sumsq += d_um * d_um;
        max = max.max(d_um);
    }
    Ok(FieldMap {
        to_commanded,
        to_physical,
        rms_um: (sumsq / pairs.len() as f64).sqrt(),
        max_um: max,
    })
}

// ---- pincushion-vs-noise diagnostic ---------------------------------------
//
// Classifies a measured laser-field displacement (physical − commanded, per
// dot) as genuine radial field distortion — pincushion/barrel curvature the
// bi-cubic pre-distortion above will fix, or a uniform scale/pitch error a
// LightBurn/EZCAD recal will fix — vs. it being measurement scatter that
// neither would help with.

/// Below `MIN_DOTS` samples, the `k1·r + k3·r³` fit has too few residual
/// degrees of freedom for a fixed `RATIO_THRESHOLD` to hold: Monte-Carlo
/// stress testing pure isotropic noise (no true field distortion) against
/// `classify_field_error` shows `n = 6` (the naive "2-parameter fit needs
/// ≥3 points" floor) lets scatter cross `ratio ≥ RATIO_THRESHOLD` and read as
/// genuine `Systematic` pincushion/barrel in ~1-in-5,000–10,000 trials at
/// realistic (10–30 µm) noise — a false "correction will help" call. `n = 9`
/// already showed zero false `Systematic` reads (20,000 trials) but still a
/// non-trivial `Borderline` rate; `n ≥ 16` (the field grid the UI's
/// `fit_laser_field` actually gates, ≥4×4) showed zero of either across
/// 20,000+ trials. `10` sits above the observed danger zone with margin,
/// while staying below every real call site's grid size. (The later-added
/// tangential linear term is one extra fitted parameter; the boundary test
/// `classify_pure_noise_at_min_dots_boundary...` re-verifies that pure noise
/// still never reads as correctable — systematic *or* non-radial — at n=10.)
const MIN_DOTS: usize = 10;
/// Need at least this many samples outside the center-exclusion radius to
/// trust the radial slope and still have residual degrees of freedom.
const MIN_OFFCENTER: usize = 4;
/// Sample positions must span at least this far end-to-end, mm. Smaller than
/// any real grid pitch used elsewhere in this codebase (10–20 mm in
/// tests/fixtures), so a normal grid always clears it; a clustered or
/// duplicated set doesn't.
const MIN_SPAN_MM: f64 = 3.0;
/// Bounding-box `min(width,height)/max(width,height)` of sample positions
/// must clear this. Below it the samples are nearly collinear and a 2-D
/// radial model isn't identifiable.
const MIN_ASPECT: f64 = 0.15;
/// `systematic_um` must clear this before it's trusted at all, independent
/// of `ratio`. Below the typical clean-fit RMS this codebase already reports
/// (`FieldMap::rms_um`/`LensMap::rms_um`, commonly 20–60 µm in fixtures) but
/// above plausible pure-detection noise.
const ABS_FLOOR_UM: f64 = 15.0;
/// Floor for a `Borderline` call — half of `ABS_FLOOR_UM`.
const BORDERLINE_FLOOR_UM: f64 = ABS_FLOOR_UM / 2.0;
/// `systematic_um` must be at least this many times `noise_um` for a firm
/// `Systematic`/`UniformScale` verdict.
const RATIO_THRESHOLD: f64 = 2.0;
/// Below `RATIO_THRESHOLD` but at/above this (and past `BORDERLINE_FLOOR_UM`)
/// reads as `Borderline` rather than flat `Noise`.
const RATIO_BORDERLINE: f64 = 1.3;
/// Points within `max(CENTER_EXCLUDE_MM, CENTER_EXCLUDE_FRAC·r_max)` of the
/// center are excluded from the `MIN_OFFCENTER` *count* only — not from the
/// fit itself, where small `r` already down-weights them naturally.
const CENTER_EXCLUDE_MM: f64 = 0.5;
const CENTER_EXCLUDE_FRAC: f64 = 0.05;
/// Below this radius a sample's direction is undefined; its whole
/// displacement is folded into `noise_um`, none into the radial fit.
const EPS_MM: f64 = 1e-6;

/// Why `classify_field_error` couldn't reach a
/// `Systematic`/`UniformScale`/`Borderline`/`Noise` verdict — the sample set
/// itself doesn't constrain the radial fit enough to trust any answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InconclusiveReason {
    /// Fewer than `MIN_DOTS` samples total.
    TooFewDots,
    /// Fewer than `MIN_OFFCENTER` samples fall outside the center-exclusion
    /// radius, so the radial slope is unconstrained.
    TooFewOffCenter,
    /// Sample positions reach less than `MIN_SPAN_MM` from their centroid.
    SpanTooSmall,
    /// Sample positions are nearly collinear (their spread is one-dimensional:
    /// the smaller principal spread is below `MIN_ASPECT` of the larger), so a
    /// 2-D radial model isn't identifiable.
    SpanTooThin,
    /// A sample position or displacement was non-finite (NaN/∞) — fail closed
    /// rather than let it poison the sums into a spurious verdict.
    NonFinite,
}

/// What a measured laser-field displacement sample set looks like, per
/// [`classify_field_error`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldPattern {
    /// A radial cubic (curvature) term dominates the systematic signal and it
    /// clears the noise floor: genuine pincushion/barrel field curvature — the
    /// bi-cubic pre-distortion ([`fit_field`]) will measurably fix this.
    Systematic { pincushion: bool },
    /// The systematic signal clears the noise floor but is chiefly
    /// **tangential** (perpendicular to the radius) — a rotation/skew/
    /// misalignment, not lens curvature. The bi-cubic still fixes it, so
    /// correction helps; it just isn't a pincushion.
    NonRadial,
    /// Signal clears the noise floor, but it's a near-pure *radial linear*
    /// term: a uniform radial scale/pitch error, not curvature. Correction
    /// works, but a LightBurn/EZCAD origin & scale recal (or a mis-scaled
    /// reference target) is the likelier root cause.
    UniformScale,
    /// Some systematic trend above the noise floor, but not enough to commit
    /// to a verdict above — a wider/denser grid would sharpen the call.
    Borderline,
    /// No systematic trend (radial or tangential) distinguishable from
    /// scatter: correcting won't help (and risks overfitting) — the field is
    /// likely already tight, or the residual is genuine measurement noise.
    Noise,
    /// Not enough / not well-distributed data to tell either way.
    Inconclusive(InconclusiveReason),
}

/// Classification of a measured laser-field error against a radial
/// (pincushion/barrel/uniform-scale) model, vs. it being measurement
/// scatter. Returned by [`classify_field_error`].
#[derive(Debug, Clone, PartialEq)]
pub struct FieldVerdict {
    pub pattern: FieldPattern,
    /// Distortion-center estimate: the centroid of `position_mm` across all
    /// samples, machine mm. `None` only when `samples` is empty.
    pub center_mm: Option<Point2<f64>>,
    /// Fitted linear radial coefficient from `rad(r) ≈ k1·r + k3·r³`, µm per
    /// mm of radius — a uniform scale/pitch error, NOT curvature. `0.0`
    /// whenever `pattern` is `Inconclusive` (no fit attempted).
    pub k1_um_per_mm: f64,
    /// Fitted cubic radial coefficient, µm per mm³ of radius. Positive =
    /// pincushion (grows outward), negative = barrel. `0.0` whenever
    /// `pattern` is `Inconclusive`.
    pub k3_um_per_mm3: f64,
    /// RMS of the full fitted systematic model — radial (`k1·r + k3·r³`) and
    /// tangential (`t1·r`) combined — over all samples, µm; the "systematic
    /// signal" that is removed to isolate `noise_um`.
    pub systematic_um: f64,
    /// RMS of the radial cubic term alone over all samples, µm — used to
    /// decide pincushion/barrel vs a uniform scale.
    pub curvature_um: f64,
    /// RMS of the radial linear term alone over all samples, µm.
    pub linear_um: f64,
    /// RMS of the fitted **tangential** model (rotation/skew, perpendicular to
    /// the radius) over all samples, µm. Large relative to the radial terms ⇒
    /// a non-radial misalignment rather than lens curvature.
    pub tangential_um: f64,
    /// RMS of what's left after subtracting BOTH the radial and tangential
    /// fitted models over all samples, µm — the true "noise floor". In an
    /// `Inconclusive` verdict this is just the RMS of the raw displacement
    /// magnitudes (no fit attempted).
    pub noise_um: f64,
    /// `systematic_um / noise_um`. `0.0` whenever `pattern` is
    /// `Inconclusive`. Effectively unbounded (divides by `f64::EPSILON`)
    /// when `noise_um` is exactly `0.0`.
    pub ratio: f64,
    /// Farthest sample from `center_mm`, mm. `0.0` when `samples` is empty.
    pub edge_radius_mm: f64,
    /// Sample count passed in.
    pub n: usize,
}

/// Classify a set of `(position_mm, displacement_mm)` laser-field samples —
/// `displacement_mm = physical_mm − commanded_mm` for each burned dot,
/// `position_mm` the dot's **commanded** (exact, not camera-measured)
/// coordinate — as genuine systematic field distortion (radial pincushion/
/// barrel curvature, a uniform scale error, or a non-radial rotation/skew) vs.
/// measurement scatter.
///
/// `position_mm` is deliberately the commanded coordinate, not `physical_mm`:
/// it's exact by construction (it's what the grid generator was told), so it
/// doesn't drag camera/lens-fit noise into the radius estimate the way
/// `physical_mm` would.
///
/// ASSUMES the burned grid is roughly **centered on the scan field**: the
/// distortion center is taken as the centroid of `position_mm`, not fitted.
/// PCBForge's grid generator centers the grid on the operator-set work area,
/// so this holds in normal use. A grid burned far off the scan-lens axis makes
/// genuine curvature partly present as a radial *linear* term about the wrong
/// center, which can read as `UniformScale` — burn the calibration grid
/// centered on the field.
///
/// Both a radial (`k1·r + k3·r³`) and a tangential (rotation/skew) model are
/// fit; the noise floor is what remains after removing *both*, so a pure
/// rotation is recognized as systematic (`NonRadial`), not mistaken for noise.
///
/// Never fails: degenerate or non-finite input comes back as
/// `FieldPattern::Inconclusive`/`Noise`, not `Err`.
pub fn classify_field_error(samples: &[(Point2<f64>, Vector2<f64>)]) -> FieldVerdict {
    let n = samples.len();
    let empty = |pattern, center_mm, noise_um, edge_radius_mm| FieldVerdict {
        pattern,
        center_mm,
        k1_um_per_mm: 0.0,
        k3_um_per_mm3: 0.0,
        systematic_um: 0.0,
        curvature_um: 0.0,
        linear_um: 0.0,
        tangential_um: 0.0,
        noise_um,
        ratio: 0.0,
        edge_radius_mm,
        n,
    };
    if n == 0 {
        return empty(
            FieldPattern::Inconclusive(InconclusiveReason::TooFewDots),
            None,
            0.0,
            0.0,
        );
    }
    // Fail closed on non-finite input: a single NaN/∞ poisons every sum, and
    // all the `>=` gates below would then be false, silently falling through
    // to a confident `Noise` ("field is fine") for garbage.
    if samples
        .iter()
        .any(|(p, d)| !(p.x.is_finite() && p.y.is_finite() && d.x.is_finite() && d.y.is_finite()))
    {
        return empty(
            FieldPattern::Inconclusive(InconclusiveReason::NonFinite),
            None,
            0.0,
            0.0,
        );
    }

    // Center: centroid of commanded positions. No nonlinear fit — see doc
    // comment above (assumes a field-centered grid).
    let mut center = Point2::new(0.0, 0.0);
    for (p, _) in samples {
        center.x += p.x;
        center.y += p.y;
    }
    center.x /= n as f64;
    center.y /= n as f64;

    // Per-sample radius, plus the position covariance (for the collinearity
    // gate: the eigenvalues of the 2×2 second-moment matrix are the principal
    // spreads, so a diagonal line of dots is caught where an axis-aligned
    // bounding box would not).
    let mut r = vec![0.0_f64; n];
    let mut r_max = 0.0_f64;
    let (mut cxx, mut cxy, mut cyy) = (0.0, 0.0, 0.0);
    for (i, (p, _)) in samples.iter().enumerate() {
        let (dx, dy) = (p.x - center.x, p.y - center.y);
        r[i] = (dx * dx + dy * dy).sqrt();
        r_max = r_max.max(r[i]);
        cxx += dx * dx;
        cxy += dx * dy;
        cyy += dy * dy;
    }
    cxx /= n as f64;
    cxy /= n as f64;
    cyy /= n as f64;
    // Eigenvalues of [[cxx,cxy],[cxy,cyy]] (symmetric ⇒ real, ≥0). Their ratio
    // (√λmin/√λmax) is the position aspect; a near-collinear set → ~0.
    let tr = cxx + cyy;
    let disc = ((cxx - cyy).powi(2) + 4.0 * cxy * cxy).max(0.0).sqrt();
    let lam_max = ((tr + disc) / 2.0).max(0.0);
    let lam_min = ((tr - disc) / 2.0).max(0.0);
    let aspect = (lam_min / lam_max.max(1e-18)).sqrt();

    // RMS displacement magnitude over all samples — the fallback `noise_um`
    // used by every early-exit (Inconclusive) branch below, no fit attempted.
    let raw_noise_um = {
        let sumsq: f64 = samples
            .iter()
            .map(|(_, d)| (d.x * 1000.0).powi(2) + (d.y * 1000.0).powi(2))
            .sum();
        (sumsq / n as f64).sqrt()
    };
    let inconclusive = |reason: InconclusiveReason| {
        empty(
            FieldPattern::Inconclusive(reason),
            Some(center),
            raw_noise_um,
            r_max,
        )
    };

    if n < MIN_DOTS {
        return inconclusive(InconclusiveReason::TooFewDots);
    }
    if r_max < MIN_SPAN_MM {
        return inconclusive(InconclusiveReason::SpanTooSmall);
    }
    if aspect < MIN_ASPECT {
        return inconclusive(InconclusiveReason::SpanTooThin);
    }

    let r_min = CENTER_EXCLUDE_MM.max(CENTER_EXCLUDE_FRAC * r_max);
    let offcenter = r.iter().filter(|&&ri| ri >= r_min).count();
    if offcenter < MIN_OFFCENTER {
        return inconclusive(InconclusiveReason::TooFewOffCenter);
    }

    // Signed radial and tangential components per sample (µm). At r≈0 the
    // direction is undefined, so that displacement is left out of both fits and
    // folded whole into the noise residual below.
    let mut rad = vec![0.0_f64; n];
    let mut tan = vec![0.0_f64; n];
    for (i, (p, d)) in samples.iter().enumerate() {
        if r[i] > EPS_MM {
            let ux = (p.x - center.x) / r[i];
            let uy = (p.y - center.y) / r[i];
            let (dx, dy) = (d.x * 1000.0, d.y * 1000.0);
            rad[i] = dx * ux + dy * uy; // outward
            tan[i] = -dx * uy + dy * ux; // 90° CCW from outward
        }
    }

    // Closed-form fit `comp(r) ≈ a1·r + a3·r³` — 2×2 normal equations (no SVD
    // for two basis columns). Used for both the radial and tangential
    // components; the r=0 samples drop out (their basis is zero).
    let fit_profile = |comp: &[f64]| -> (f64, f64) {
        let (mut s11, mut s13, mut s33, mut b1, mut b3) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for i in 0..n {
            let ri = r[i];
            if ri <= EPS_MM {
                continue;
            }
            let r3 = ri * ri * ri;
            s11 += ri * ri;
            s13 += ri * r3;
            s33 += r3 * r3;
            b1 += ri * comp[i];
            b3 += r3 * comp[i];
        }
        let det = s11 * s33 - s13 * s13;
        if det.abs() > 1e-9 {
            ((b1 * s33 - b3 * s13) / det, (s11 * b3 - s13 * b1) / det)
        } else {
            // Degenerate design (e.g. all off-center points equidistant from
            // the centroid) — no reliable slope; treat as no signal.
            (0.0, 0.0)
        }
    };
    let (k1, k3) = fit_profile(&rad); // radial: k1·r + k3·r³
    // Tangential: t1·r only. A rigid rotation is exactly θ·r tangential, so one
    // parameter captures the physical non-radial case; a higher-order term
    // would mostly soak up noise and raise false-positive risk.
    let t1 = {
        let (mut srr, mut srt) = (0.0, 0.0);
        for i in 0..n {
            let ri = r[i];
            if ri > EPS_MM {
                srr += ri * ri;
                srt += ri * tan[i];
            }
        }
        if srr > 1e-9 { srt / srr } else { 0.0 }
    };

    // Split each measured displacement into the fitted systematic vector
    // (radial + tangential predicted) and the residual. Radial and tangential
    // are orthogonal per sample, so their energies add and the residual is
    // exactly what neither model explains — the true noise floor.
    let (mut sum_rad2, mut sum_tan2, mut sum_curv2, mut sum_lin2, mut sum_resid2) =
        (0.0, 0.0, 0.0, 0.0, 0.0);
    for (i, (p, d)) in samples.iter().enumerate() {
        let ri = r[i];
        let lin = k1 * ri;
        let curv = k3 * ri * ri * ri;
        let rad_pred = lin + curv;
        let tan_pred = t1 * ri;
        sum_rad2 += rad_pred * rad_pred;
        sum_tan2 += tan_pred * tan_pred;
        sum_curv2 += curv * curv;
        sum_lin2 += lin * lin;
        let (dx_um, dy_um) = (d.x * 1000.0, d.y * 1000.0);
        let (rx, ry) = if ri > EPS_MM {
            let ux = (p.x - center.x) / ri;
            let uy = (p.y - center.y) / ri;
            // predicted vector = rad_pred·û_r + tan_pred·û_t, û_t = (−uy, ux).
            let px = rad_pred * ux - tan_pred * uy;
            let py = rad_pred * uy + tan_pred * ux;
            (dx_um - px, dy_um - py)
        } else {
            (dx_um, dy_um)
        };
        sum_resid2 += rx * rx + ry * ry;
    }
    let radial_um = (sum_rad2 / n as f64).sqrt();
    let tangential_um = (sum_tan2 / n as f64).sqrt();
    let systematic_um = ((sum_rad2 + sum_tan2) / n as f64).sqrt();
    let curvature_um = (sum_curv2 / n as f64).sqrt();
    let linear_um = (sum_lin2 / n as f64).sqrt();
    let noise_um = (sum_resid2 / n as f64).sqrt();
    let ratio = systematic_um / noise_um.max(f64::EPSILON);

    let pattern = if systematic_um >= ABS_FLOOR_UM && ratio >= RATIO_THRESHOLD {
        if tangential_um > radial_um {
            // Rotation/skew dominates — not lens curvature, but the bi-cubic
            // still fixes it.
            FieldPattern::NonRadial
        } else if curvature_um >= linear_um {
            FieldPattern::Systematic {
                pincushion: k3 > 0.0,
            }
        } else {
            FieldPattern::UniformScale
        }
    } else if systematic_um >= BORDERLINE_FLOOR_UM && ratio >= RATIO_BORDERLINE {
        FieldPattern::Borderline
    } else {
        FieldPattern::Noise
    };

    FieldVerdict {
        pattern,
        center_mm: Some(center),
        k1_um_per_mm: k1,
        k3_um_per_mm3: k3,
        systematic_um,
        curvature_um,
        linear_um,
        tangential_um,
        noise_um,
        ratio,
        edge_radius_mm: r_max,
        n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 7×7 grid of true-mm points at 10 mm pitch.
    fn grid_mm() -> Vec<(f64, f64)> {
        let mut v = Vec::new();
        for r in 0..7 {
            for c in 0..7 {
                v.push((c as f64 * 10.0, r as f64 * 10.0));
            }
        }
        v
    }

    /// Camera model for the test: perspective (a mild keystone) plus **radial
    /// barrel distortion** about the image center — exactly the curvature a
    /// homography cannot represent.
    fn image(mm: (f64, f64)) -> (f64, f64) {
        // Perspective-ish linear part: ~9 px/mm with a slight shear.
        let (x, y) = mm;
        let ideal_u = 60.0 + 9.0 * x + 0.03 * y;
        let ideal_v = 60.0 + 9.0 * y - 0.02 * x;
        // Barrel distortion about the image center.
        let (cx, cy) = (330.0, 330.0);
        let (du, dv) = (ideal_u - cx, ideal_v - cy);
        let r2 = (du * du + dv * dv) / (330.0 * 330.0);
        let k = 0.04; // ~4% barrel at the corner — a realistic machine-vision lens
        (cx + du * (1.0 + k * r2), cy + dv * (1.0 + k * r2))
    }

    #[test]
    fn polynomial_captures_lens_curvature_a_homography_cannot() {
        let mm = grid_mm();
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = mm
            .iter()
            .map(|&(x, y)| {
                let (u, v) = image((x, y));
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();

        // The polynomial lens model fits the distorted grid tightly.
        let lens = fit_lens(&pairs).expect("fit");
        assert!(lens.rms_um < 30.0, "lens RMS {} µm too high", lens.rms_um);
        assert!(lens.max_um < 80.0, "lens max {} µm", lens.max_um);
        assert_eq!(lens.residuals.len(), 49);

        // A homography over the SAME points leaves a large structured residual
        // (the barrel it can't model) — proving the polynomial was needed.
        let h = crate::fit_homography(&pairs).expect("homography");
        let mut hmax = 0.0_f64;
        for (p, m) in &pairs {
            let e = h.apply(*p);
            hmax = hmax.max(((e.x - m.x).powi(2) + (e.y - m.y).powi(2)).sqrt() * 1000.0);
        }
        assert!(
            hmax > 300.0,
            "homography should be visibly worse ({hmax} µm) than the polynomial"
        );
    }

    #[test]
    fn px_to_mm_and_mm_to_px_round_trip() {
        let mm = grid_mm();
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = mm
            .iter()
            .map(|&(x, y)| {
                let (u, v) = image((x, y));
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();
        let lens = fit_lens(&pairs).unwrap();

        // A pixel we didn't fit (image of (25,25) mm) maps to ~(25,25),
        // and mm→px returns near the original pixel.
        let (u, v) = image((25.0, 25.0));
        let (x, y) = lens.px_to_mm.apply(u, v);
        assert!(
            (x - 25.0).abs() < 0.1 && (y - 25.0).abs() < 0.1,
            "px→mm ({x:.3},{y:.3})"
        );
        let (u2, v2) = lens.mm_to_px.apply(x, y);
        assert!(
            (u2 - u).abs() < 1.5 && (v2 - v).abs() < 1.5,
            "round trip px ({u2:.1},{v2:.1})"
        );
    }

    #[test]
    fn too_few_points_errors() {
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = (0..6)
            .map(|i| (Point2::new(i as f64, 0.0), Point2::new(i as f64, 0.0)))
            .collect();
        assert!(fit_lens(&pairs).is_err());
    }

    /// The laser's field distortion: a commanded coordinate physically lands
    /// pincushioned outward about the field center (70,70) — up to ~6% at the
    /// edge. This is the map the pre-distortion must cancel.
    fn laser_field(cmd: (f64, f64)) -> (f64, f64) {
        let (du, dv) = (cmd.0 - 70.0, cmd.1 - 70.0);
        let r2 = (du * du + dv * dv) / (70.0 * 70.0);
        let f = 1.0 + 0.03 * r2; // ~3% pincushion — a realistic residual field
        (70.0 + du * f, 70.0 + dv * f)
    }

    #[test]
    fn precompensation_cancels_the_field_distortion() {
        // Calibration: we command a grid and observe where it physically lands.
        let mut pairs = Vec::new();
        for r in 0..7 {
            for c in 0..7 {
                let cmd = (c as f64 * 20.0, r as f64 * 20.0);
                let phys = laser_field(cmd); // where the beam actually went
                pairs.push((Point2::new(phys.0, phys.1), Point2::new(cmd.0, cmd.1)));
            }
        }
        let field = fit_field(&pairs).expect("fit");
        assert!(field.rms_um < 60.0, "field fit RMS {} µm", field.rms_um);

        // A desired physical target we did NOT calibrate on: pre-distort it,
        // then push the commanded value through the real field — it lands back
        // on target far tighter than the raw command would.
        for &(tx, ty) in &[(35.0, 35.0), (10.0, 110.0), (120.0, 15.0)] {
            let (cx, cy) = field.precompensate(tx, ty);
            let (lx, ly) = laser_field((cx, cy));
            let err = ((lx - tx).powi(2) + (ly - ty).powi(2)).sqrt() * 1000.0;

            // Without pre-distortion the same command lands visibly off — the
            // error the correction removes.
            let raw = laser_field((tx, ty));
            let raw_err = ((raw.0 - tx).powi(2) + (raw.1 - ty).powi(2)).sqrt() * 1000.0;
            assert!(
                err < raw_err / 10.0,
                "pre-distortion cuts error ≥10×: {err:.0} µm vs raw {raw_err:.0} µm at ({tx},{ty})"
            );
            assert!(err < 80.0, "target ({tx},{ty}) lands off by {err:.1} µm");
        }
    }

    #[test]
    fn field_map_serialize_round_trips() {
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = (0..7)
            .flat_map(|r| {
                (0..7).map(move |c| {
                    let cmd = (c as f64 * 20.0, r as f64 * 20.0);
                    let phys = laser_field(cmd);
                    (Point2::new(phys.0, phys.1), Point2::new(cmd.0, cmd.1))
                })
            })
            .collect();
        let field = fit_field(&pairs).unwrap();
        let serialized = field.serialize();
        assert!(serialized.starts_with("pcbforge-field 2\n"));
        let restored = FieldMap::parse(&serialized).expect("parse");
        // The de/serialized map precompensates identically.
        let (a, b) = field.precompensate(33.0, 47.0);
        let (c, d) = restored.precompensate(33.0, 47.0);
        assert!(
            (a - c).abs() < 1e-6 && (b - d).abs() < 1e-6,
            "round-trip precompensation: ({a},{b}) vs ({c},{d})"
        );
        assert!((restored.rms_um - field.rms_um).abs() < 1e-3);
    }

    #[test]
    fn legacy_v1_field_map_still_parses() {
        let mut coefficients = [0.0; 23];
        coefficients[1] = 1.0;
        coefficients[12] = 1.0;
        coefficients[22] = 1.0;
        let row = coefficients
            .iter()
            .map(f64::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        let text = format!(
            "pcbforge-field 1\nto_commanded {row}\nto_physical {row}\nrms_um 0\nmax_um 0\n"
        );
        let restored = FieldMap::parse(&text).expect("legacy field map");
        assert_eq!(restored.precompensate(2.0, 3.0), (2.0, 3.0));
    }

    #[test]
    fn classify_pincushion_reads_systematic() {
        let mut samples = Vec::new();
        for r in 0..7 {
            for c in 0..7 {
                let cmd = (c as f64 * 20.0, r as f64 * 20.0);
                let phys = laser_field(cmd);
                samples.push((
                    Point2::new(cmd.0, cmd.1),
                    Vector2::new(phys.0 - cmd.0, phys.1 - cmd.1),
                ));
            }
        }
        let v = classify_field_error(&samples);
        assert!(
            matches!(v.pattern, FieldPattern::Systematic { pincushion: true }),
            "{:?}",
            v.pattern
        );
        assert!(v.ratio >= 2.0);
    }

    #[test]
    fn classify_barrel_sign_flips() {
        // Same shape as laser_field but k = -0.03 (barrel).
        let barrel = |cmd: (f64, f64)| {
            let (du, dv) = (cmd.0 - 70.0, cmd.1 - 70.0);
            let r2 = (du * du + dv * dv) / (70.0 * 70.0);
            let f = 1.0 - 0.03 * r2;
            (70.0 + du * f, 70.0 + dv * f)
        };
        let mut samples = Vec::new();
        for r in 0..7 {
            for c in 0..7 {
                let cmd = (c as f64 * 20.0, r as f64 * 20.0);
                let phys = barrel(cmd);
                samples.push((
                    Point2::new(cmd.0, cmd.1),
                    Vector2::new(phys.0 - cmd.0, phys.1 - cmd.1),
                ));
            }
        }
        let v = classify_field_error(&samples);
        assert!(matches!(
            v.pattern,
            FieldPattern::Systematic { pincushion: false }
        ));
    }

    #[test]
    fn classify_uniform_scale_is_not_pincushion() {
        // Pure 5% scale error, no r² term.
        let mut samples = Vec::new();
        for r in 0..7 {
            for c in 0..7 {
                let cmd = (c as f64 * 20.0, r as f64 * 20.0);
                let (du, dv) = (cmd.0 - 70.0, cmd.1 - 70.0);
                let phys = (70.0 + du * 1.05, 70.0 + dv * 1.05);
                samples.push((
                    Point2::new(cmd.0, cmd.1),
                    Vector2::new(phys.0 - cmd.0, phys.1 - cmd.1),
                ));
            }
        }
        let v = classify_field_error(&samples);
        assert_eq!(v.pattern, FieldPattern::UniformScale);
    }

    #[test]
    fn classify_random_scatter_reads_noise() {
        // Deterministic pseudo-noise (no RNG dep): hash-based jitter, no radial
        // structure, amplitude comparable to real camera/detection noise.
        fn jitter(seed: u64) -> f64 {
            let x = seed.wrapping_mul(2654435761) ^ (seed >> 13);
            ((x % 1000) as f64 / 1000.0 - 0.5) * 0.02 // ±10 µm
        }
        let mut samples = Vec::new();
        let mut seed = 1u64;
        for r in 0..7 {
            for c in 0..7 {
                let cmd = (c as f64 * 20.0, r as f64 * 20.0);
                seed = seed.wrapping_add(1);
                let dx = jitter(seed);
                seed = seed.wrapping_add(1);
                let dy = jitter(seed);
                samples.push((Point2::new(cmd.0, cmd.1), Vector2::new(dx, dy)));
            }
        }
        let v = classify_field_error(&samples);
        assert_eq!(v.pattern, FieldPattern::Noise, "{v:?}");
    }

    #[test]
    fn classify_too_few_dots_is_inconclusive() {
        let samples = vec![
            (Point2::new(0.0, 0.0), Vector2::new(0.01, 0.0)),
            (Point2::new(10.0, 0.0), Vector2::new(0.02, 0.0)),
        ];
        let v = classify_field_error(&samples);
        assert_eq!(
            v.pattern,
            FieldPattern::Inconclusive(InconclusiveReason::TooFewDots)
        );
    }

    /// Deterministic xorshift64* PRNG + a 12-uniform CLT approximation to
    /// ~N(0,1) — matches the pattern `fiducial::tests` already uses for
    /// noise, no `rand` dependency.
    struct Rng(u64);
    impl Rng {
        fn next_f64(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
        }
        /// Approximately `N(0, sigma²)`.
        fn gauss(&mut self, sigma: f64) -> f64 {
            let mut s = 0.0;
            for _ in 0..12 {
                s += self.next_f64();
            }
            (s - 6.0) * sigma
        }
    }

    /// Adversarial check (a): pure isotropic noise, no true field distortion,
    /// must never read as `Systematic` — at ANY sample count `classify_field_error`
    /// accepts, not just the well-populated grids the other tests use.
    ///
    /// Monte-Carlo stress testing (20,000+ trials/cell, not committed here —
    /// see `docs/decisions.md`) found the old `MIN_DOTS = 6` let noise cross
    /// `ratio ≥ RATIO_THRESHOLD` in ~1-in-5,000–10,000 trials at the boundary
    /// `n`; `MIN_DOTS` was raised to 10 to clear that. This test locks the
    /// fix in cheaply: many seeds at exactly the (now-safe) `MIN_DOTS`
    /// boundary, asserting `Systematic` never fires on pure scatter.
    #[test]
    fn classify_pure_noise_at_min_dots_boundary_never_reads_systematic() {
        // n = MIN_DOTS = 10, a 2×5 lattice — clears span/aspect/off-center
        // gates but has minimal residual degrees of freedom for the radial
        // fit, the regime where a fixed ratio threshold is most exposed.
        let base: Vec<(f64, f64)> = (0..2)
            .flat_map(|r| (0..5).map(move |c| (c as f64 * 20.0, r as f64 * 20.0)))
            .collect();
        assert_eq!(base.len(), MIN_DOTS);
        for sigma_um in [5.0_f64, 10.0, 15.0, 20.0, 30.0] {
            for trial in 0..500u64 {
                let mut rng = Rng(0xD00D_u64
                    ^ trial.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    ^ (sigma_um as u64) << 32);
                let samples: Vec<_> = base
                    .iter()
                    .map(|&(x, y)| {
                        let dx = rng.gauss(sigma_um) / 1000.0;
                        let dy = rng.gauss(sigma_um) / 1000.0;
                        (Point2::new(x, y), Vector2::new(dx, dy))
                    })
                    .collect();
                let v = classify_field_error(&samples);
                assert!(
                    !matches!(
                        v.pattern,
                        FieldPattern::Systematic { .. } | FieldPattern::NonRadial
                    ),
                    "pure noise misread as correctable (systematic/non-radial) at \
                     sigma={sigma_um}µm trial={trial}: {v:?}"
                );
            }
        }
    }

    /// Adversarial check (b): a genuine ~2–4% pincushion PLUS realistic
    /// per-dot detection noise (not the noise-free fixture the other
    /// pincushion tests use) must still read as `Systematic { pincushion:
    /// true }` with a comfortable ratio — proving the noise floor doesn't
    /// eat the real signal. Covers both a normal 7×7 field grid and the
    /// smallest grid the UI actually allows (4×4, `fit_laser_field`'s floor).
    #[test]
    fn classify_pincushion_survives_realistic_measurement_noise() {
        let pincushion = |pct: f64, half_span: f64, cmd: (f64, f64)| {
            let (du, dv) = (cmd.0 - half_span, cmd.1 - half_span);
            let r2 = (du * du + dv * dv) / (half_span * half_span);
            let f = 1.0 + pct * r2;
            (half_span + du * f, half_span + dv * f)
        };
        for &side in &[4usize, 7] {
            let half_span = (side - 1) as f64 * 20.0 / 2.0;
            for &pct in &[0.02_f64, 0.03, 0.04] {
                for &sigma_um in &[5.0_f64, 10.0, 15.0] {
                    for trial in 0..20u64 {
                        let mut rng = Rng(0xF00D_u64
                            ^ trial.wrapping_mul(0x2545_F491_4F6C_DD1D)
                            ^ (side as u64) << 48
                            ^ ((pct * 1000.0) as u64) << 8
                            ^ (sigma_um as u64) << 24);
                        let mut samples = Vec::new();
                        for r in 0..side {
                            for c in 0..side {
                                let cmd = (c as f64 * 20.0, r as f64 * 20.0);
                                let phys = pincushion(pct, half_span, cmd);
                                let dx = phys.0 - cmd.0 + rng.gauss(sigma_um) / 1000.0;
                                let dy = phys.1 - cmd.1 + rng.gauss(sigma_um) / 1000.0;
                                samples.push((Point2::new(cmd.0, cmd.1), Vector2::new(dx, dy)));
                            }
                        }
                        let v = classify_field_error(&samples);
                        assert!(
                            matches!(v.pattern, FieldPattern::Systematic { pincushion: true }),
                            "side={side} pct={pct} sigma={sigma_um}µm trial={trial}: \
                             expected pincushion, got {:?} (ratio={:.2})",
                            v.pattern,
                            v.ratio
                        );
                        assert!(
                            v.ratio >= RATIO_THRESHOLD,
                            "side={side} pct={pct} sigma={sigma_um}µm trial={trial}: \
                             ratio {:.2} below threshold",
                            v.ratio
                        );
                    }
                }
            }
        }
    }

    /// A pure galvo rotation is entirely tangential — zero radial signal — so
    /// the radial-only classifier would have called it `Noise` ("field is
    /// fine, don't correct"). It must instead read `NonRadial`: real systematic
    /// error the bi-cubic fixes. This is the fable-review case.
    #[test]
    fn classify_rotation_reads_non_radial_not_noise() {
        // 7×7 grid centered on the origin; rotate every point ~0.4° about the
        // center → tangential displacement growing with radius (~500 µm at the
        // 42 mm corner), plus mild noise.
        let theta = 0.007_f64; // rad, ~0.4°
        let mut rng = Rng(0x51D_u64);
        let mut samples = Vec::new();
        for r in 0..7 {
            for c in 0..7 {
                let (x, y) = (c as f64 * 14.0 - 42.0, r as f64 * 14.0 - 42.0);
                // Rotation displacement d = θ · (−y, x).
                let dx = -theta * y + rng.gauss(8.0) / 1000.0;
                let dy = theta * x + rng.gauss(8.0) / 1000.0;
                samples.push((Point2::new(x, y), Vector2::new(dx, dy)));
            }
        }
        let v = classify_field_error(&samples);
        assert!(
            matches!(v.pattern, FieldPattern::NonRadial),
            "rotation should read NonRadial, got {:?} (rad={:.0} tan={:.0} noise={:.0})",
            v.pattern,
            v.systematic_um,
            v.tangential_um,
            v.noise_um
        );
        assert!(
            v.tangential_um > v.linear_um && v.tangential_um > v.curvature_um,
            "tangential dominates: tan={:.0} lin={:.0} curv={:.0}",
            v.tangential_um,
            v.linear_um,
            v.curvature_um
        );
    }

    /// Non-finite input fails closed (Inconclusive), never falls through to a
    /// confident `Noise` ("field is fine") on garbage.
    #[test]
    fn classify_non_finite_fails_closed() {
        let mut samples: Vec<_> = (0..7)
            .flat_map(|r| {
                (0..7).map(move |c| {
                    (
                        Point2::new(c as f64 * 14.0, r as f64 * 14.0),
                        Vector2::new(0.01, 0.01),
                    )
                })
            })
            .collect();
        samples[10].1.x = f64::NAN;
        assert!(matches!(
            classify_field_error(&samples).pattern,
            FieldPattern::Inconclusive(InconclusiveReason::NonFinite)
        ));
    }

    /// A diagonal line of dots is one-dimensional but has a square axis-aligned
    /// bounding box — the second-moment (PCA) gate catches it where a bbox
    /// aspect ratio would not, so a degenerate partial detection can't be fit.
    #[test]
    fn classify_diagonal_collinear_is_span_too_thin() {
        let samples: Vec<_> = (0..12)
            .map(|i| {
                let t = i as f64 * 6.0;
                (Point2::new(t, t), Vector2::new(0.02, -0.01))
            })
            .collect();
        assert!(matches!(
            classify_field_error(&samples).pattern,
            FieldPattern::Inconclusive(InconclusiveReason::SpanTooThin)
        ));
    }
}
