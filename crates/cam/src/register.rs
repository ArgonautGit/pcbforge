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
}
