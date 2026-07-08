//! Property tests for `cam::geom` (GEO-1 acceptance criteria).
//!
//! Random convex polygons are built as points on a circle at sorted random
//! angles (guaranteed convex, CCW). Sizes span 1 mm – 100 mm diameter, and a
//! center-coordinate strategy also places them near ±1 m (±1e9 nm) to
//! exercise i64/f64 headroom.

use std::slice::from_ref;

use cam::geom::{area_nm2, difference, intersect, offset, union, xor};
use pcb_core::{NM_PER_MM, Nm, P, Poly, Ring};
use proptest::prelude::*;

/// Twice the signed shoelace area, exact in i128 (test-local copy).
fn ring_doubled_area(ring: &Ring) -> i128 {
    let mut sum: i128 = 0;
    for (i, a) in ring.iter().enumerate() {
        let b = &ring[(i + 1) % ring.len()];
        sum += a.x as i128 * b.y as i128 - b.x as i128 * a.y as i128;
    }
    sum
}

/// Assert the output orientation convention: outer CCW, holes CW.
fn assert_convention(polys: &[Poly]) {
    for p in polys {
        assert!(p.outer.len() >= 3, "degenerate outer ring in output");
        assert!(ring_doubled_area(&p.outer) > 0, "outer ring must be CCW");
        for h in &p.holes {
            assert!(h.len() >= 3, "degenerate hole ring in output");
            assert!(ring_doubled_area(h) < 0, "hole ring must be CW");
        }
    }
}

fn relative_close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * a.abs().max(b.abs())
}

/// Coordinates near the origin or near ±1 m (in nm), i64 headroom check.
fn center_coord() -> impl Strategy<Value = Nm> {
    prop_oneof![
        Just(0),
        -1_000_000_000i64..=-999_000_000,
        999_000_000i64..=1_000_000_000,
    ]
}

/// Random convex CCW polygon: points on a circle of radius 0.5–50 mm at
/// sorted random angles with a bounded-below angular gap (no slivers).
fn convex_poly() -> impl Strategy<Value = Poly> {
    (
        center_coord(),
        center_coord(),
        0.5f64..50.0,
        prop::collection::vec(0.3f64..1.0, 3..=10),
        0.0f64..std::f64::consts::TAU,
    )
        .prop_map(|(cx, cy, r_mm, gaps, phase)| {
            let total: f64 = gaps.iter().sum();
            let r_nm = r_mm * NM_PER_MM as f64;
            let mut acc = 0.0;
            let mut ring: Ring = Vec::with_capacity(gaps.len());
            for g in &gaps {
                let th = phase + std::f64::consts::TAU * acc / total;
                ring.push(P::new(
                    cx + (r_nm * th.cos()).round() as Nm,
                    cy + (r_nm * th.sin()).round() as Nm,
                ));
                acc += g;
            }
            Poly {
                outer: ring,
                holes: vec![],
            }
        })
}

proptest! {
    #[test]
    fn union_is_idempotent(p in convex_poly()) {
        let u = union(from_ref(&p), &[]);
        prop_assert_eq!(u.len(), 1);
        assert_convention(&u);
        let a_in = area_nm2(&[p]);
        let a_u = area_nm2(&u);
        prop_assert!(relative_close(a_in, a_u, 1e-9), "normalize changed area: {a_in} vs {a_u}");

        let uu = union(&u, &u);
        assert_convention(&uu);
        prop_assert_eq!(uu.len(), u.len());
        let a_uu = area_nm2(&uu);
        prop_assert!(relative_close(a_u, a_uu, 1e-9), "union(u,u) changed area: {a_u} vs {a_uu}");
    }

    #[test]
    fn difference_with_self_is_empty(p in convex_poly()) {
        let d = difference(from_ref(&p), from_ref(&p));
        prop_assert!(d.is_empty(), "p \\ p left {} polys, area {}", d.len(), area_nm2(&d));

        // Companion self-op invariants (same exact-i64 machinery).
        prop_assert!(xor(from_ref(&p), from_ref(&p)).is_empty());
        let i = intersect(from_ref(&p), from_ref(&p));
        assert_convention(&i);
        prop_assert!(relative_close(area_nm2(&i), area_nm2(&[p]), 1e-9));
    }

    #[test]
    fn offset_round_trip_preserves_area(p in convex_poly(), frac in 0.05f64..0.4) {
        let a0 = area_nm2(from_ref(&p));
        prop_assert!(a0 > 0.0);
        // Offset distance proportional to the polygon's effective radius.
        let r_eff = (a0 / std::f64::consts::PI).sqrt();
        let d = (frac * r_eff).round() as Nm;
        prop_assume!(d > 0);

        // Note: poly *count* is deliberately not asserted. Near degenerate
        // configurations cavalier_contours can emit a tiny spurious loop
        // alongside the real result; the acceptance criterion is area-based
        // (0.5 %), which also bounds any such junk.
        let grown = offset(&[p], d);
        prop_assert!(!grown.is_empty());
        assert_convention(&grown);
        prop_assert!(area_nm2(&grown) > a0, "positive offset must dilate");

        let back = offset(&grown, -d);
        prop_assert!(!back.is_empty());
        assert_convention(&back);
        let a1 = area_nm2(&back);
        prop_assert!(
            relative_close(a0, a1, 0.005),
            "round-trip area off by {:.4}%: {a0} -> {a1} (d = {d} nm)",
            100.0 * (a1 - a0).abs() / a0
        );
    }
}
