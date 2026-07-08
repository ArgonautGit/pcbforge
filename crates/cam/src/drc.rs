//! CAM-7 — design-rule checker.
//!
//! [`drc`] flags two classes of hard errors against the active machine's
//! process floor:
//!
//! * **`GapBelowFloor`** — two copper features approach closer than the
//!   floor. Detected by morphological *closing*: dilate the copper by
//!   `floor/2`, erode back, and anything the closing gained relative to the
//!   original copper is a region bridging a sub-floor gap.
//! * **`TraceWidthBelowFloor`** — copper narrower than the floor. Detected by
//!   morphological *opening*: erode by `floor/2`, dilate back, and anything
//!   the original copper has that the opening lost is copper thinner than the
//!   floor.
//!
//! Both use [`crate::geom::offset`] (round joins) and the exact integer
//! boolean kernel.
//!
//! # Asymmetric radii (false-positive suppression)
//!
//! A *symmetric* closing/opening at radius `r = floor/2` produces geometric
//! artifacts that are not violations:
//!
//! * closing fills a radius-`r` fillet at every **reentrant corner** of the
//!   copper (area `(1 − π/4)·r²`, far above any reasonable crumb filter);
//! * opening rounds off every **convex corner** the same way.
//!
//! Both are corner effects, not sub-floor features. They are eliminated by
//! making the *second* offset of each composite slightly deeper than the
//! first: the erode of the closing and the dilate-back of the opening use
//! radius `r·√2 + 1 µm` instead of `r`. For a right-angle corner the offset
//! vertex sits exactly `r·√2` from the original vertex, so this covers the
//! fillet/rounding exactly, with 1 µm of margin absorbing arc-flattening
//! chord sag (≤ `r/1000`). Corners sharper than 90° could still leak
//! artifacts (vertex distance `r/sin(θ/2)` grows unboundedly); the crumb
//! filter below is the backstop for those.
//!
//! The deeper second offset does **not** reduce detection sensitivity for the
//! checks themselves: a trace thinner than the floor is erased entirely by
//! the first erode (so no dilate-back can resurrect it), and a bridged gap
//! channel survives interior to the closing regardless of how much its mouths
//! are pushed back. The cost is only that the *reported region* is trimmed by
//! ~`0.2·floor` where it meets compliant copper, and point-like proximities
//! shorter than ~`floor` along the gap may go unreported.
//!
//! # Crumb filter
//!
//! Residue components with area below `(floor/20)²` are discarded as numeric
//! crumbs (arc-flattening slivers, boolean-op dust, acute-corner leakage).
//!
//! # Reported measurement
//!
//! `measured_mm` is twice the violation region's inradius (found by bisecting
//! erosion radii until the region vanishes, to 0.25 µm). For a uniform-width
//! gap channel or trace this *is* the gap/trace width; for irregular regions
//! it is the width of the widest inscribed disk — honest evidence that the
//! feature is below the floor, not a guarantee of the minimum width.
//! `location` is the component's area centroid, which for a concave component
//! (e.g. a whole L-shaped trace flagged as too thin) may lie outside the
//! copper itself.

use std::f64::consts::SQRT_2;

use pcb_core::{Layer, NM_PER_MM, Nm, P, Poly, Ring};

use crate::geom::{difference, offset, poly_area, union};

/// What a [`Violation`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViolationKind {
    /// Copper-to-copper spacing below the machine floor.
    GapBelowFloor,
    /// Copper feature narrower than the machine floor.
    TraceWidthBelowFloor,
}

/// One hard DRC error with its evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub kind: ViolationKind,
    /// Area centroid of the offending region (may lie outside the copper for
    /// concave regions).
    pub location: P,
    /// Twice the offending region's inradius, mm (see module docs). `None`
    /// only if the region unexpectedly survives erosion by the full floor.
    pub measured_mm: Option<f64>,
}

/// Margin added to the deeper second offset, absorbing arc-flattening chord
/// sag at composite-offset corners. 1 µm ≫ the `r/1000` flattening tolerance
/// and ≪ any process floor.
const CORNER_MARGIN_NM: Nm = 1_000;

/// Bisection tolerance for the inradius measurement, nm (0.25 µm).
const MEASURE_TOL_NM: Nm = 250;

/// Check `layer` against `machine_floor_mm`: minimum copper-to-copper gap and
/// minimum trace width below the floor are reported as hard errors with
/// locations. A non-positive or sub-nanometer floor yields no violations.
pub fn drc(layer: &Layer, machine_floor_mm: f64) -> Vec<Violation> {
    let mut out = Vec::new();
    if machine_floor_mm.is_nan() || machine_floor_mm <= 0.0 {
        return out;
    }
    let r_nm = mm_to_nm(machine_floor_mm / 2.0);
    if r_nm == 0 {
        return out;
    }
    let copper = union(&layer.polys, &[]);
    if copper.is_empty() {
        return out;
    }
    let deep_nm = (r_nm as f64 * SQRT_2).ceil() as Nm + CORNER_MARGIN_NM;
    let floor_nm = mm_to_nm(machine_floor_mm);
    let crumb_nm2 = (floor_nm as f64 / 20.0).powi(2);

    // Gap check: closing residue = regions bridging sub-floor gaps.
    let closing = offset(&offset(&copper, r_nm), -deep_nm);
    let gaps = difference(&closing, &copper);
    collect(
        &gaps,
        ViolationKind::GapBelowFloor,
        crumb_nm2,
        floor_nm,
        &mut out,
    );

    // Trace-width check: opening residue = copper thinner than the floor.
    let opening = offset(&offset(&copper, -r_nm), deep_nm);
    let thin = difference(&copper, &opening);
    collect(
        &thin,
        ViolationKind::TraceWidthBelowFloor,
        crumb_nm2,
        floor_nm,
        &mut out,
    );
    out
}

/// Turn each above-crumb residue component into a [`Violation`].
fn collect(
    residue: &[Poly],
    kind: ViolationKind,
    crumb_nm2: f64,
    floor_nm: Nm,
    out: &mut Vec<Violation>,
) {
    for comp in residue {
        if poly_area(comp) < crumb_nm2 {
            continue;
        }
        out.push(Violation {
            kind,
            location: centroid(comp),
            measured_mm: measured_width_mm(comp, floor_nm),
        });
    }
}

/// Twice the component's inradius in mm, by bisecting the erosion radius at
/// which the component vanishes. Returns `None` if the component survives
/// erosion by the full floor (it is then not usefully "below floor"-sized;
/// never expected for genuine residues, whose width is < floor by
/// construction).
fn measured_width_mm(comp: &Poly, floor_nm: Nm) -> Option<f64> {
    let region = std::slice::from_ref(comp);
    if !offset(region, -floor_nm).is_empty() {
        return None;
    }
    let (mut lo, mut hi) = (0 as Nm, floor_nm);
    while hi - lo > MEASURE_TOL_NM {
        let mid = lo + (hi - lo) / 2;
        if offset(region, -mid).is_empty() {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some((lo + hi) as f64 / NM_PER_MM as f64)
}

/// Area centroid of a polygon with holes (shoelace moments, exact `i128`
/// accumulation). Falls back to the first outer vertex for a degenerate
/// (zero-area) polygon.
fn centroid(p: &Poly) -> P {
    let mut a2: i128 = 0; // twice the signed area
    let mut mx: i128 = 0; // 6 × (area-weighted x moment)
    let mut my: i128 = 0;
    ring_moments(&p.outer, &mut a2, &mut mx, &mut my);
    for hole in &p.holes {
        ring_moments(hole, &mut a2, &mut mx, &mut my);
    }
    if a2 == 0 {
        return p.outer.first().copied().unwrap_or_default();
    }
    let scale = 3.0 * a2 as f64;
    P::new(
        (mx as f64 / scale).round() as Nm,
        (my as f64 / scale).round() as Nm,
    )
}

fn ring_moments(ring: &Ring, a2: &mut i128, mx: &mut i128, my: &mut i128) {
    for (i, a) in ring.iter().enumerate() {
        let b = &ring[(i + 1) % ring.len()];
        let cross = a.x as i128 * b.y as i128 - b.x as i128 * a.y as i128;
        *a2 += cross;
        *mx += (a.x as i128 + b.x as i128) * cross;
        *my += (a.y as i128 + b.y as i128) * cross;
    }
}

#[inline]
fn mm_to_nm(v_mm: f64) -> Nm {
    (v_mm * NM_PER_MM as f64).round() as Nm
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::NM_PER_MM as MM;

    fn rect_mm(x0: f64, y0: f64, x1: f64, y1: f64) -> Poly {
        Poly {
            outer: vec![
                P::from_mm(x0, y0),
                P::from_mm(x1, y0),
                P::from_mm(x1, y1),
                P::from_mm(x0, y1),
            ],
            holes: vec![],
        }
    }

    #[test]
    fn empty_layer_has_no_violations() {
        assert!(drc(&Layer::default(), 0.15).is_empty());
    }

    #[test]
    fn nonpositive_floor_yields_nothing() {
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 1.0, 1.0)],
        };
        assert!(drc(&layer, 0.0).is_empty());
        assert!(drc(&layer, -1.0).is_empty());
        assert!(drc(&layer, f64::NAN).is_empty());
    }

    #[test]
    fn sub_floor_gap_is_flagged_and_measured() {
        // Two 2 mm squares, 0.1 mm apart.
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 2.0, 2.0), rect_mm(2.1, 0.0, 4.1, 2.0)],
        };
        let v = drc(&layer, 0.15);
        assert!(
            v.iter().any(|v| v.kind == ViolationKind::GapBelowFloor),
            "0.1 mm gap must be flagged at floor 0.15: {v:?}"
        );
        assert!(
            v.iter().all(|v| v.kind == ViolationKind::GapBelowFloor),
            "fat squares must not raise width violations: {v:?}"
        );
        let gap = &v[0];
        let w = gap.measured_mm.expect("gap should be measurable");
        assert!((w - 0.1).abs() < 0.002, "measured {w} mm, expected ~0.1");
        assert!(drc(&layer, 0.08).is_empty());
    }

    #[test]
    fn centroid_of_rect_with_hole_is_its_center() {
        let outer = rect_mm(0.0, 0.0, 4.0, 2.0).outer;
        let mut hole = rect_mm(1.0, 0.5, 2.0, 1.5).outer;
        hole.reverse();
        // Hole is symmetric about x = 1.5 only; weight the moment check
        // against the exact analytic centroid instead.
        let p = Poly {
            outer,
            holes: vec![hole],
        };
        let c = centroid(&p);
        // Areas: outer 8, hole -1; outer centroid (2,1), hole centroid (1.5,1).
        // Combined x = (8*2 - 1*1.5)/7 = 14.5/7; y = 1.
        let expect_x = 14.5 / 7.0 * MM as f64;
        assert!((c.x as f64 - expect_x).abs() < 2.0);
        assert_eq!(c.y, MM);
    }
}
