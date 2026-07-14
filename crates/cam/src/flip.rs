//! Double-sided support (ORC-6 groundwork): mirror a job for the back side and
//! model the beam-angle parallax that shifts a through-hole's *exit* opening
//! relative to its *entry*.
//!
//! Two independent pieces, both pure geometry so they test headlessly:
//!
//! * [`MirrorAxis`] + [`mirror_job`] — reflect the design across a line. KiCad
//!   exports `B.Cu` in top-view coordinates, so to burn the back the operator
//!   flips the board (left-right, about a vertical axis) and the design must be
//!   mirrored in X to match. Reflection reverses orientation, so every ring is
//!   re-wound to keep the `outer` CCW / holes CW convention.
//!
//! * [`FieldParams`] + [`entry_to_exit_mm`] — a non-telecentric f-theta lens
//!   sends the beam to a field point at an angle `θ` with `tan θ ≈ r / f` (r =
//!   distance from the scan center, f = focal length). Drilling a hole of depth
//!   `t`, the beam continues outward, so the **exit** opening sits at radius
//!   `r · (1 + t/f)` — a radial magnification about the scan center. At the
//!   Omni X glass lens (~70 mm) through ~1.6 mm FR4, that is ~0.8 mm at a 35 mm
//!   field radius, far too large to ignore when registering the flipped board
//!   against the same drilled holes.
//!
//! [`back_expected_fiducial_mm`] composes them: a through-hole drilled at the
//! design position exits at `entry_to_exit_mm`, then the physical flip mirrors
//! it — that is where the detector should expect the hole when imaging the back.

use pcb_core::{P, Poly, Ring};

/// The line a board is reflected across when flipped for the back side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MirrorAxis {
    /// Reflect X about the vertical line `x = x_mm` (a left-right flip).
    VerticalX { x_mm: f64 },
    /// Reflect Y about the horizontal line `y = y_mm` (a top-bottom flip).
    HorizontalY { y_mm: f64 },
}

impl MirrorAxis {
    /// Reflect an mm point across this axis.
    pub fn reflect_mm(&self, x: f64, y: f64) -> (f64, f64) {
        match *self {
            MirrorAxis::VerticalX { x_mm } => (2.0 * x_mm - x, y),
            MirrorAxis::HorizontalY { y_mm } => (x, 2.0 * y_mm - y),
        }
    }

    /// Reflect a nanometer point across this axis.
    pub fn reflect(&self, p: P) -> P {
        let (x, y) = self.reflect_mm(p.x_mm(), p.y_mm());
        P::from_mm(x, y)
    }
}

/// Reflect one ring and reverse its winding (reflection flips orientation, so
/// reversing restores the original CCW/CW sense).
fn mirror_ring(ring: &Ring, axis: &MirrorAxis) -> Ring {
    ring.iter().rev().map(|&p| axis.reflect(p)).collect()
}

/// Mirror one polygon (outer + holes) across `axis`, preserving the winding
/// convention.
pub fn mirror_poly(poly: &Poly, axis: &MirrorAxis) -> Poly {
    Poly {
        outer: mirror_ring(&poly.outer, axis),
        holes: poly.holes.iter().map(|h| mirror_ring(h, axis)).collect(),
    }
}

/// Mirror a whole job (e.g. the back copper's to-ablate regions) across `axis`.
pub fn mirror_job(shapes: &[Poly], axis: &MirrorAxis) -> Vec<Poly> {
    shapes.iter().map(|p| mirror_poly(p, axis)).collect()
}

/// Optics of the marking field for the beam-angle parallax.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldParams {
    /// Scan/field center in the design mm frame (the on-axis point of the lens).
    pub scan_center_mm: (f64, f64),
    /// Board thickness the beam drills through, mm.
    pub thickness_mm: f64,
    /// f-theta lens focal length, mm.
    pub focal_mm: f64,
}

impl FieldParams {
    /// Radial magnification of the exit opening about the scan center.
    /// `1` when thickness is 0 (no drill depth) or focal length is unset.
    pub fn exit_magnification(&self) -> f64 {
        if self.focal_mm <= 0.0 {
            1.0
        } else {
            1.0 + self.thickness_mm / self.focal_mm
        }
    }
}

/// Where a through-hole entered at design `(x, y)` mm **exits** the far face,
/// given the field optics — a radial scale about the scan center. A point on
/// the axis is unmoved; the shift grows linearly with field radius.
pub fn entry_to_exit_mm(x: f64, y: f64, field: &FieldParams) -> (f64, f64) {
    let (cx, cy) = field.scan_center_mm;
    let m = field.exit_magnification();
    (cx + (x - cx) * m, cy + (y - cy) * m)
}

/// Where the detector should expect a drilled through-hole when imaging the
/// **back** of the board: the beam exits the far face at [`entry_to_exit_mm`],
/// then the physical flip mirrors that position across `axis`.
pub fn back_expected_fiducial_mm(
    design_x: f64,
    design_y: f64,
    axis: &MirrorAxis,
    field: &FieldParams,
) -> (f64, f64) {
    let (ex, ey) = entry_to_exit_mm(design_x, design_y, field);
    axis.reflect_mm(ex, ey)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::NM_PER_MM;

    const MM: i64 = NM_PER_MM;

    fn tri(a: (i64, i64), b: (i64, i64), c: (i64, i64)) -> Poly {
        Poly {
            outer: vec![
                P::new(a.0 * MM, a.1 * MM),
                P::new(b.0 * MM, b.1 * MM),
                P::new(c.0 * MM, c.1 * MM),
            ],
            holes: vec![],
        }
    }

    /// Signed area (shoelace) in mm² — sign is the winding.
    fn signed_area(ring: &Ring) -> f64 {
        let n = ring.len();
        let mut a = 0.0;
        for i in 0..n {
            let p = ring[i];
            let q = ring[(i + 1) % n];
            a += p.x_mm() * q.y_mm() - q.x_mm() * p.y_mm();
        }
        a / 2.0
    }

    #[test]
    fn mirror_x_reflects_and_preserves_winding() {
        // CCW triangle (positive area).
        let t = tri((0, 0), (10, 0), (0, 10));
        assert!(signed_area(&t.outer) > 0.0);
        let axis = MirrorAxis::VerticalX { x_mm: 0.0 };
        let m = mirror_poly(&t, &axis);
        // X negated, Y kept.
        assert_eq!(m.outer[2].x, -0); // first original vertex, now last after rev
        // Winding preserved (still CCW / positive) despite the reflection.
        assert!(
            signed_area(&m.outer) > 0.0,
            "winding restored after reflection"
        );
        // The reflected point set matches {(0,0),(-10,0),(0,10)}.
        let xs: Vec<i64> = m.outer.iter().map(|p| p.x).collect();
        assert!(xs.contains(&(-10 * MM)) && xs.contains(&0));
    }

    #[test]
    fn mirror_is_an_involution() {
        let axis = MirrorAxis::VerticalX { x_mm: 30.0 };
        let job = vec![tri((5, 5), (25, 5), (5, 25))];
        let back = mirror_job(&job, &axis);
        let there_and_back = mirror_job(&back, &axis);
        assert_eq!(there_and_back, job, "mirroring twice is the identity");
    }

    #[test]
    fn mirror_about_nonzero_axis_reflects_about_that_line() {
        let axis = MirrorAxis::VerticalX { x_mm: 30.0 };
        // x=10 → 2*30-10 = 50; x=30 stays.
        assert_eq!(axis.reflect_mm(10.0, 7.0), (50.0, 7.0));
        assert_eq!(axis.reflect_mm(30.0, 7.0), (30.0, 7.0));
    }

    #[test]
    fn exit_offset_is_radial_and_zero_on_axis() {
        let field = FieldParams {
            scan_center_mm: (0.0, 0.0),
            thickness_mm: 1.6,
            focal_mm: 70.0,
        };
        // On the axis: no shift.
        assert_eq!(entry_to_exit_mm(0.0, 0.0, &field), (0.0, 0.0));
        // At radius 35 mm: exit radius = 35 * (1 + 1.6/70) = 35.8 mm → ~0.8 mm.
        let (ex, ey) = entry_to_exit_mm(35.0, 0.0, &field);
        assert!((ey).abs() < 1e-9);
        assert!((ex - 35.0 * (1.0 + 1.6 / 70.0)).abs() < 1e-9);
        assert!(
            (ex - 35.0 - 0.8).abs() < 0.02,
            "≈0.8 mm shift at the field edge, got {}",
            ex - 35.0
        );
    }

    #[test]
    fn exit_offset_is_identity_without_thickness_or_focal() {
        let no_t = FieldParams {
            scan_center_mm: (5.0, 5.0),
            thickness_mm: 0.0,
            focal_mm: 70.0,
        };
        assert_eq!(entry_to_exit_mm(40.0, 20.0, &no_t), (40.0, 20.0));
        let no_f = FieldParams {
            scan_center_mm: (5.0, 5.0),
            thickness_mm: 1.6,
            focal_mm: 0.0,
        };
        assert_eq!(entry_to_exit_mm(40.0, 20.0, &no_f), (40.0, 20.0));
    }

    #[test]
    fn back_expected_composes_exit_then_mirror() {
        let axis = MirrorAxis::VerticalX { x_mm: 0.0 };
        let field = FieldParams {
            scan_center_mm: (0.0, 0.0),
            thickness_mm: 1.6,
            focal_mm: 70.0,
        };
        let (bx, by) = back_expected_fiducial_mm(35.0, 0.0, &axis, &field);
        // exit at (35.8, 0) then mirror about x=0 → (-35.8, 0).
        assert!((bx + 35.0 * (1.0 + 1.6 / 70.0)).abs() < 1e-9);
        assert!(by.abs() < 1e-9);
    }
}
