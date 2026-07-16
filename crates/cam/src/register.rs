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
        let x = p.x as f64 / NM_PER_MM as f64;
        let y = p.y as f64 / NM_PER_MM as f64;
        let xp = self.m[0] * x + self.m[1] * y + self.m[2];
        let yp = self.m[3] * x + self.m[4] * y + self.m[5];
        P::new(
            (xp * NM_PER_MM as f64).round() as i64,
            (yp * NM_PER_MM as f64).round() as i64,
        )
    }
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
pub fn transform_shapes_field<F>(
    shapes: &[Poly],
    a: &Affine2,
    max_seg_mm: f64,
    warp: F,
) -> Vec<Poly>
where
    F: Fn(f64, f64) -> (f64, f64),
{
    let seg_nm = (max_seg_mm.max(1e-3) * NM_PER_MM as f64).max(1.0);
    let warp_pt = |p: P| -> P {
        let (cx, cy) = warp(p.x as f64 / NM_PER_MM as f64, p.y as f64 / NM_PER_MM as f64);
        P::new(
            (cx * NM_PER_MM as f64).round() as i64,
            (cy * NM_PER_MM as f64).round() as i64,
        )
    };
    // Densify a closed ring in the physical frame, then warp every point.
    let ring = |r: &[P]| -> Vec<P> {
        if r.len() < 2 {
            return r.iter().map(|&p| warp_pt(a.apply(p))).collect();
        }
        let placed: Vec<P> = r.iter().map(|&p| a.apply(p)).collect();
        let mut out = Vec::with_capacity(placed.len());
        for i in 0..placed.len() {
            let s = placed[i];
            let e = placed[(i + 1) % placed.len()]; // closed: last → first
            out.push(warp_pt(s));
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
                )));
            }
        }
        out
    };
    shapes
        .iter()
        .map(|poly| Poly {
            outer: ring(&poly.outer),
            holes: poly.holes.iter().map(|h| ring(h)).collect(),
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
        let out = transform_shapes_field(&[big_sq()], &Affine2::identity(), 10.0, pincushion);
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
        let out = transform_shapes_field(&[big_sq()], &Affine2::identity(), 10.0, |x, y| (x, y));
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
        let out = transform_shapes_field(&[poly], &Affine2::identity(), 10.0, pincushion);
        assert_eq!(out[0].holes.len(), 1, "hole survives the warp");
        assert!(out[0].holes[0].len() > 4, "hole edges densified too");
    }
}
