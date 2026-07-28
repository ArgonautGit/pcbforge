//! Host-side registration: apply a 2D affine to job geometry so an emitted
//! `.lbrn2` burns where the physical board actually sits (VIS-6, software half).
//!
//! The affine maps the **design frame → the machine (target) frame**, fitted
//! from fiducial correspondences (`vision::fit_affine`, done in the CLI, which
//! owns the nalgebra dependency). This module just *applies* the six
//! coefficients to geometry — kept dependency-free so `cam` stays lean.
//!
//! Composition note: a complete registration is `board_affine ∘ galvo_affine`
//! (design → bed → galvo). The galvo half needs a burned calibration grid
//! (hardware; VIS-6's `calib grid`). Until it exists, the caller supplies
//! correspondences already in the target frame (e.g. the operator jogs the
//! pointer to each fiducial and reads machine mm, or a camera frame calibrated
//! to the workspace), and this applies that single affine. Both compose
//! trivially — multiply the matrices — when the galvo affine lands.

use pcb_core::{NM_PER_MM, P, Poly};

/// A 2D affine in **millimeters**, row-major `[a, b, c, d, e, f]`:
///
/// ```text
/// x' = a·x + b·y + c
/// y' = d·x + e·y + f
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2 {
    pub m: [f64; 6],
}

impl Affine2 {
    /// The identity transform.
    pub fn identity() -> Self {
        Affine2 {
            m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        }
    }

    /// Determinant of the linear part (`a·e − b·d`). Negative ⇒ the transform
    /// reflects (flips winding); a fiducial fit should never produce this.
    pub fn determinant(&self) -> f64 {
        self.m[0] * self.m[4] - self.m[1] * self.m[3]
    }

    /// Apply to an integer-nm point: nm → mm → affine → mm → nm (rounded).
    pub fn apply(&self, p: P) -> P {
        let (xp, yp) = self.apply_mm(nm_to_mm(p.x), nm_to_mm(p.y));
        P::new(
            (xp * NM_PER_MM as f64).round() as i64,
            (yp * NM_PER_MM as f64).round() as i64,
        )
    }

    /// Apply in millimeters, without rounding to the nm lattice.
    fn apply_mm(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.m[0] * x + self.m[1] * y + self.m[2],
            self.m[3] * x + self.m[4] * y + self.m[5],
        )
    }
}

/// Sanity envelope for a placed or warped coordinate, mm. Beyond this a value
/// is a broken fit rather than a machine move — far outside any bed in this
/// class, and far inside what the nm `i64` lattice represents.
const MAX_COORD_MM: f64 = 10_000.0;

fn nm_to_mm(v: pcb_core::Nm) -> f64 {
    v as f64 / NM_PER_MM as f64
}

/// Why a field-warped transform refused to produce geometry.
///
/// Refusing is the point: `f64 as i64` **saturates**, so a NaN becomes `0` —
/// the machine origin — and `±∞` becomes `±i64::MAX`. Either one turns a broken
/// fit into a beam move across the board, so a bad vertex fails the whole job
/// instead of being clamped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WarpError {
    /// The transform returned NaN or ∞ for a vertex.
    NonFinite {
        /// The input vertex, mm.
        src_mm: (f64, f64),
    },
    /// The transform returned a finite value outside `±MAX_COORD_MM`. Finite is
    /// not enough: `1e300` mm still saturates the nm cast to `i64::MAX`.
    OutOfEnvelope {
        /// The input vertex, mm.
        src_mm: (f64, f64),
        /// The transformed vertex, mm.
        out_mm: (f64, f64),
    },
    /// The vertex landed outside the region the field map was fit over, where
    /// the pre-distortion polynomial is extrapolating rather than measuring.
    /// Refused for the same reason the others are: an extrapolated correction
    /// is a beam move nobody measured, and it grows without bound away from the
    /// fitted dots.
    OutsideFieldCalibration {
        /// The physical vertex the map was asked to evaluate, mm.
        src_mm: (f64, f64),
        /// The fitted region, `[x0, y0, x1, y1]` physical mm.
        calib_mm: [f64; 4],
    },
}

impl std::fmt::Display for WarpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFinite { src_mm } => write!(
                f,
                "transform returned a non-finite (NaN/∞) coordinate for the \
                 vertex at ({:.3}, {:.3}) mm",
                src_mm.0, src_mm.1
            ),
            Self::OutOfEnvelope { src_mm, out_mm } => write!(
                f,
                "transform moved the vertex at ({:.3}, {:.3}) mm to \
                 ({:.3}, {:.3}) mm, outside the ±{MAX_COORD_MM:.0} mm sanity \
                 envelope",
                src_mm.0, src_mm.1, out_mm.0, out_mm.1
            ),
            Self::OutsideFieldCalibration { src_mm, calib_mm } => write!(
                f,
                "the vertex at ({:.3}, {:.3}) mm lies outside the field map's \
                 calibrated region ({:.3}..{:.3}, {:.3}..{:.3}) mm (+{:.0}% \
                 margin) — the field correction is not measured there; place the \
                 job inside the calibrated box or re-run the laser-field \
                 calibration over the area you need",
                src_mm.0,
                src_mm.1,
                calib_mm[0],
                calib_mm[2],
                calib_mm[1],
                calib_mm[3],
                FIELD_BOUNDS_MARGIN_FRAC * 100.0
            ),
        }
    }
}

impl std::error::Error for WarpError {}

/// How far outside the field map's fitted box a vertex may sit and still be
/// warped, as a fraction of the box's own span on each side. Same 5% the lens
/// map's extrapolation check uses, for the same reason: right at the edge the
/// polynomial is still interpolating between the outermost dots in one axis, so
/// a hairline overhang is not extrapolation worth refusing a job over.
const FIELD_BOUNDS_MARGIN_FRAC: f64 = 0.05;

/// Whether `p` (physical mm) sits inside `calib_mm` grown by
/// [`FIELD_BOUNDS_MARGIN_FRAC`] of its own span on each side.
fn within_field_bounds(p: (f64, f64), calib_mm: [f64; 4]) -> bool {
    let [x0, y0, x1, y1] = calib_mm;
    let mx = (x1 - x0) * FIELD_BOUNDS_MARGIN_FRAC;
    let my = (y1 - y0) * FIELD_BOUNDS_MARGIN_FRAC;
    p.0 >= x0 - mx && p.0 <= x1 + mx && p.1 >= y0 - my && p.1 <= y1 + my
}

/// Round a transformed millimeter coordinate onto the nm lattice, refusing
/// anything the `i64` cast would saturate. Checked in **mm, before** scaling by
/// `NM_PER_MM`: a finite-but-huge mm value overflows the nm range silently.
fn checked_nm(src_mm: (f64, f64), out_mm: (f64, f64)) -> Result<P, WarpError> {
    let (x, y) = out_mm;
    if !x.is_finite() || !y.is_finite() {
        return Err(WarpError::NonFinite { src_mm });
    }
    if x.abs() > MAX_COORD_MM || y.abs() > MAX_COORD_MM {
        return Err(WarpError::OutOfEnvelope { src_mm, out_mm });
    }
    Ok(P::new(
        (x * NM_PER_MM as f64).round() as i64,
        (y * NM_PER_MM as f64).round() as i64,
    ))
}

/// Apply `a` to every vertex of `shapes` (outer rings and holes), returning
/// geometry in the target frame. Winding is preserved for a proper (positive-
/// determinant) affine — the only kind a fiducial fit yields.
pub fn transform_shapes(shapes: &[Poly], a: &Affine2) -> Vec<Poly> {
    shapes
        .iter()
        .map(|poly| Poly {
            outer: poly.outer.iter().map(|&p| a.apply(p)).collect(),
            holes: poly
                .holes
                .iter()
                .map(|h| h.iter().map(|&p| a.apply(p)).collect())
                .collect(),
        })
        .collect()
}

/// Like [`transform_shapes`], but after the affine places the job in the
/// physical machine frame, each vertex is pushed through `warp` — a
/// **physical-mm → commanded-mm** pre-distortion — so the beam, which bends
/// commanded coordinates back through the galvo/f-theta field distortion,
/// lands on the intended physical geometry.
///
/// A straight design edge maps to a *curved* path in commanded space, so every
/// edge is first densified to segments no longer than `max_seg_mm` (in the
/// physical frame) and each intermediate point is warped — a warp applied only
/// to the endpoints would leave the mid-edge pre-curvature out and the burn
/// would bow. `warp` takes `(x_mm, y_mm)` physical and returns `(x_mm, y_mm)`
/// commanded.
///
/// `calib_mm` is the region `warp` was FIT over, `[x0, y0, x1, y1]` physical mm
/// (`vision::FieldMap::calib_mm_bounds`). Every point handed to `warp` —
/// including the subdivision points — must land inside it (plus
/// [`FIELD_BOUNDS_MARGIN_FRAC`]) or the whole job is refused: outside the fitted
/// dots the pre-distortion is an extrapolating cubic, which on a real bench map
/// reaches millimetres of "correction" in the uncalibrated outer ring. `None`
/// means the map does not know what it was fit over (a pre-bounds file), and the
/// warp proceeds ungated as it always did.
///
/// # Errors
///
/// - [`WarpError::NonFinite`] if the affine or `warp` yields NaN/∞ for a vertex.
/// - [`WarpError::OutOfEnvelope`] if a transformed vertex leaves the sanity
///   envelope.
/// - [`WarpError::OutsideFieldCalibration`] if a vertex leaves `calib_mm`.
///
/// Any of them refuses the whole job. Clamping would be worse: it would silently
/// move a vertex the operator believes the calibration placed.
pub fn transform_shapes_field<F>(
    shapes: &[Poly],
    a: &Affine2,
    max_seg_mm: f64,
    calib_mm: Option<[f64; 4]>,
    warp: F,
) -> Result<Vec<Poly>, WarpError>
where
    F: Fn(f64, f64) -> (f64, f64),
{
    let seg_nm = (max_seg_mm.max(1e-3) * NM_PER_MM as f64).max(1.0);
    let place_pt = |p: P| -> Result<P, WarpError> {
        let src = (nm_to_mm(p.x), nm_to_mm(p.y));
        checked_nm(src, a.apply_mm(src.0, src.1))
    };
    let warp_pt = |p: P| -> Result<P, WarpError> {
        let src = (nm_to_mm(p.x), nm_to_mm(p.y));
        // Gate BEFORE evaluating: an extrapolated value can be perfectly finite
        // and well inside the envelope, so nothing downstream would catch it.
        if let Some(calib_mm) = calib_mm
            && !within_field_bounds(src, calib_mm)
        {
            return Err(WarpError::OutsideFieldCalibration {
                src_mm: src,
                calib_mm,
            });
        }
        checked_nm(src, warp(src.0, src.1))
    };
    // Densify a closed ring in the physical frame, then warp every point.
    let ring = |r: &[P]| -> Result<Vec<P>, WarpError> {
        if r.len() < 2 {
            return r.iter().map(|&p| warp_pt(place_pt(p)?)).collect();
        }
        let placed: Vec<P> = r.iter().map(|&p| place_pt(p)).collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(placed.len());
        for i in 0..placed.len() {
            let s = placed[i];
            let e = placed[(i + 1) % placed.len()]; // closed: last → first
            out.push(warp_pt(s)?);
            // Interior subdivision points (exclusive of both ends; the next
            // edge contributes its own start).
            let (dx, dy) = ((e.x - s.x) as f64, (e.y - s.y) as f64);
            let len = (dx * dx + dy * dy).sqrt();
            let steps = (len / seg_nm).floor() as i64;
            for k in 1..=steps {
                let t = k as f64 / (steps as f64 + 1.0);
                out.push(warp_pt(P::new(
                    (s.x as f64 + dx * t).round() as i64,
                    (s.y as f64 + dy * t).round() as i64,
                ))?);
            }
        }
        Ok(out)
    };
    shapes
        .iter()
        .map(|poly| {
            Ok(Poly {
                outer: ring(&poly.outer)?,
                holes: poly
                    .holes
                    .iter()
                    .map(|h| ring(h))
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MM: i64 = NM_PER_MM;

    fn sq() -> Poly {
        Poly {
            outer: vec![
                P::new(0, 0),
                P::new(10 * MM, 0),
                P::new(10 * MM, 10 * MM),
                P::new(0, 10 * MM),
            ],
            holes: vec![],
        }
    }

    #[test]
    fn identity_is_a_noop() {
        let out = transform_shapes(std::slice::from_ref(&sq()), &Affine2::identity());
        assert_eq!(out[0], sq());
    }

    #[test]
    fn pure_translation_shifts_every_vertex() {
        // +25 mm x, -4 mm y.
        let a = Affine2 {
            m: [1.0, 0.0, 25.0, 0.0, 1.0, -4.0],
        };
        let out = transform_shapes(std::slice::from_ref(&sq()), &a);
        assert_eq!(out[0].outer[0], P::new(25 * MM, -4 * MM));
        assert_eq!(out[0].outer[2], P::new(35 * MM, 6 * MM));
    }

    #[test]
    fn rotation_90_deg_about_origin() {
        // 90° CCW: (x,y) -> (-y, x).
        let a = Affine2 {
            m: [0.0, -1.0, 0.0, 1.0, 0.0, 0.0],
        };
        let out = transform_shapes(std::slice::from_ref(&sq()), &a);
        // (10,0) -> (0,10)
        assert_eq!(out[0].outer[1], P::new(0, 10 * MM));
        // (10,10) -> (-10,10)
        assert_eq!(out[0].outer[2], P::new(-10 * MM, 10 * MM));
        assert!(a.determinant() > 0.0, "rotation preserves winding");
    }

    #[test]
    fn holes_are_transformed_too() {
        let mut hole = vec![
            P::new(2 * MM, 2 * MM),
            P::new(4 * MM, 2 * MM),
            P::new(4 * MM, 4 * MM),
            P::new(2 * MM, 4 * MM),
        ];
        hole.reverse();
        let poly = Poly {
            outer: sq().outer,
            holes: vec![hole],
        };
        let a = Affine2 {
            m: [1.0, 0.0, 100.0, 0.0, 1.0, 0.0],
        };
        let out = transform_shapes(std::slice::from_ref(&poly), &a);
        // hole reversed to CW: first vertex is (2,4); +100 mm x.
        assert_eq!(out[0].holes[0][0], P::new(102 * MM, 4 * MM));
    }

    #[test]
    fn reflection_is_flagged_by_negative_determinant() {
        let a = Affine2 {
            m: [1.0, 0.0, 0.0, 0.0, -1.0, 0.0], // y flip
        };
        assert!(a.determinant() < 0.0);
    }

    /// A 100 mm square at the origin (physical/design frame).
    fn big_sq() -> Poly {
        Poly {
            outer: vec![
                P::new(0, 0),
                P::new(100 * MM, 0),
                P::new(100 * MM, 100 * MM),
                P::new(0, 100 * MM),
            ],
            holes: vec![],
        }
    }

    /// A pincushion pre-distortion about (50,50) mm: radial expansion by up to
    /// 10% at the corner — the shape of an f-theta field error.
    fn pincushion(x: f64, y: f64) -> (f64, f64) {
        let (du, dv) = (x - 50.0, y - 50.0);
        let r2 = (du * du + dv * dv) / (50.0 * 50.0);
        let f = 1.0 + 0.1 * r2;
        (50.0 + du * f, 50.0 + dv * f)
    }

    #[test]
    fn field_warp_subdivides_edges_and_warps_interior_points() {
        // 10 mm segments over a 100 mm edge → 10 interior points per edge.
        let out = transform_shapes_field(&[big_sq()], &Affine2::identity(), 10.0, None, pincushion)
            .expect("finite warp");
        let ring = &out[0].outer;
        // 4 corners + 10 interior each = 44 points (subdivision happened).
        assert_eq!(ring.len(), 44, "each 100 mm edge densified to 10 mm steps");

        let mm = |p: P| (p.x as f64 / MM as f64, p.y as f64 / MM as f64);
        // The first vertex is the warped bottom-left corner.
        let (x0, y0) = mm(ring[0]);
        assert!(
            (x0 - -10.0).abs() < 0.05 && (y0 - -10.0).abs() < 0.05,
            "corner warped: ({x0:.3},{y0:.3})"
        );
        // ring[1] is the first interior point of the bottom edge, at physical
        // (100/11, 0) mm — it must equal pincushion(that point), NOT a point on
        // the straight chord between the warped corners (which sits at y=-10).
        let phys = (100.0 / 11.0, 0.0);
        let (ex, ey) = pincushion(phys.0, phys.1);
        let (ix, iy) = mm(ring[1]);
        assert!(
            (ix - ex).abs() < 0.05 && (iy - ey).abs() < 0.05,
            "interior point warped by its own physical position: got ({ix:.3},{iy:.3}), want ({ex:.3},{ey:.3})"
        );
        // Its y (~-8.35) is well above the chord (-10): the pre-curvature a
        // warp-endpoints-only emit would have missed.
        assert!(iy > -10.0 + 1.0, "interior carries curvature: y={iy:.3}");
    }

    #[test]
    fn field_warp_identity_keeps_edges_straight() {
        // Identity warp: subdivided points stay collinear on the original edge.
        let out =
            transform_shapes_field(&[big_sq()], &Affine2::identity(), 10.0, None, |x, y| (x, y))
                .expect("finite warp");
        let ring = &out[0].outer;
        assert_eq!(ring.len(), 44);
        // Every bottom-edge interior point (indices 1..=10) has y = 0.
        for p in &ring[1..=10] {
            assert_eq!(p.y, 0, "identity warp keeps the bottom edge on y=0");
        }
    }

    #[test]
    fn field_warp_transforms_holes_too() {
        let mut hole = vec![
            P::new(40 * MM, 40 * MM),
            P::new(60 * MM, 40 * MM),
            P::new(60 * MM, 60 * MM),
            P::new(40 * MM, 60 * MM),
        ];
        hole.reverse();
        let poly = Poly {
            outer: big_sq().outer,
            holes: vec![hole],
        };
        let out = transform_shapes_field(&[poly], &Affine2::identity(), 10.0, None, pincushion)
            .expect("finite warp");
        assert_eq!(out[0].holes.len(), 1, "hole survives the warp");
        assert!(out[0].holes[0].len() > 4, "hole edges densified too");
    }

    #[test]
    fn non_finite_warp_result_refuses_the_job() {
        // NaN would cast to 0 nm — the machine origin — and the beam would draw
        // a line across the board to reach it.
        let out = transform_shapes_field(&[big_sq()], &Affine2::identity(), 10.0, None, |x, y| {
            if x > 50.0 { (f64::NAN, y) } else { (x, y) }
        });
        assert!(matches!(out, Err(WarpError::NonFinite { .. })), "{out:?}");

        let inf = transform_shapes_field(&[big_sq()], &Affine2::identity(), 10.0, None, |x, y| {
            if y > 50.0 { (x, f64::INFINITY) } else { (x, y) }
        });
        assert!(matches!(inf, Err(WarpError::NonFinite { .. })), "{inf:?}");
    }

    #[test]
    fn finite_but_absurd_warp_result_refuses_the_job() {
        // 1e300 mm is finite, so a finiteness-only guard would pass it — and the
        // nm cast saturates it to i64::MAX all the same.
        let out = transform_shapes_field(&[big_sq()], &Affine2::identity(), 10.0, None, |x, y| {
            if x > 50.0 { (1e300, y) } else { (x, y) }
        });
        match out {
            Err(WarpError::OutOfEnvelope { out_mm, .. }) => assert_eq!(out_mm.0, 1e300),
            other => panic!("expected OutOfEnvelope, got {other:?}"),
        }
    }

    #[test]
    fn non_finite_affine_refuses_before_the_warp_runs() {
        let a = Affine2 {
            m: [1.0, 0.0, f64::NAN, 0.0, 1.0, 0.0],
        };
        let out = transform_shapes_field(&[big_sq()], &a, 10.0, None, |x, y| (x, y));
        assert!(matches!(out, Err(WarpError::NonFinite { .. })), "{out:?}");
    }

    #[test]
    fn warp_error_message_names_the_offending_vertex() {
        let err = transform_shapes_field(&[big_sq()], &Affine2::identity(), 10.0, None, |_, _| {
            (f64::NAN, 0.0)
        })
        .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("non-finite"), "{text}");
        assert!(text.contains("0.000, 0.000"), "{text}");
    }

    /// The bench case: a field fit over dots spanning 15–75 mm. A job that lives
    /// inside that box warps exactly as it did before the gate existed.
    #[test]
    fn field_bounds_pass_geometry_inside_the_calibrated_box() {
        let bounds = Some([15.0, 15.0, 75.0, 75.0]);
        let job = Poly {
            outer: vec![
                P::new(20 * MM, 20 * MM),
                P::new(70 * MM, 20 * MM),
                P::new(70 * MM, 70 * MM),
                P::new(20 * MM, 70 * MM),
            ],
            holes: vec![],
        };
        let gated = transform_shapes_field(
            std::slice::from_ref(&job),
            &Affine2::identity(),
            10.0,
            bounds,
            pincushion,
        )
        .expect("job inside the calibrated box still warps");
        let ungated =
            transform_shapes_field(&[job], &Affine2::identity(), 10.0, None, pincushion).unwrap();
        assert_eq!(gated, ungated, "the gate changes no geometry it admits");
    }

    /// Just outside: within the 5% margin the polynomial is still effectively
    /// interpolating, so a hairline overhang is admitted rather than refused.
    #[test]
    fn field_bounds_admit_the_margin_and_refuse_beyond_it() {
        // 60 mm span → 3 mm margin each side, so 77.9 is in and 78.1 is out.
        let bounds = Some([15.0, 15.0, 75.0, 75.0]);
        let at = |x_mm: f64| Poly {
            outer: vec![
                P::new((x_mm * MM as f64) as i64, 40 * MM),
                P::new((x_mm * MM as f64) as i64 + MM, 40 * MM),
                P::new((x_mm * MM as f64) as i64 + MM, 41 * MM),
            ],
            holes: vec![],
        };
        assert!(
            transform_shapes_field(&[at(76.0)], &Affine2::identity(), 10.0, bounds, pincushion)
                .is_ok(),
            "inside the margin"
        );
        let err =
            transform_shapes_field(&[at(79.0)], &Affine2::identity(), 10.0, bounds, pincushion)
                .unwrap_err();
        match err {
            WarpError::OutsideFieldCalibration { src_mm, calib_mm } => {
                assert_eq!(calib_mm, [15.0, 15.0, 75.0, 75.0]);
                assert!(src_mm.0 >= 79.0, "{src_mm:?}");
            }
            other => panic!("{other:?}"),
        }
        let text = err.to_string();
        assert!(
            text.contains("15.000..75.000"),
            "names the calibrated box: {text}"
        );
        assert!(
            text.contains("79.000"),
            "names the offending vertex: {text}"
        );
    }

    /// The subdivision points are gated too: an edge whose ENDS sit inside the
    /// box can still cross the uncalibrated ring on its way, and every point in
    /// between is warped.
    #[test]
    fn field_bounds_gate_the_subdivision_points_not_only_the_vertices() {
        // A tall thin triangle straddling y: ends at y=20 and y=70 (inside),
        // but the edge runs out to x=200 in between.
        let bounds = Some([15.0, 15.0, 75.0, 75.0]);
        let poly = Poly {
            outer: vec![
                P::new(20 * MM, 20 * MM),
                P::new(200 * MM, 45 * MM),
                P::new(20 * MM, 70 * MM),
            ],
            holes: vec![],
        };
        let err = transform_shapes_field(&[poly], &Affine2::identity(), 1.0, bounds, pincushion)
            .unwrap_err();
        assert!(
            matches!(err, WarpError::OutsideFieldCalibration { .. }),
            "{err:?}"
        );
    }

    /// A map that does not know what it was fit over cannot be gated — it warps
    /// as it always did rather than refusing every job.
    #[test]
    fn field_map_without_bounds_is_ungated() {
        let far = Poly {
            outer: vec![
                P::new(500 * MM, 500 * MM),
                P::new(501 * MM, 500 * MM),
                P::new(501 * MM, 501 * MM),
            ],
            holes: vec![],
        };
        assert!(
            transform_shapes_field(&[far], &Affine2::identity(), 10.0, None, pincushion).is_ok()
        );
    }
}
