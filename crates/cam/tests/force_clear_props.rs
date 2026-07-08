//! CAM-2 property tests: random dumbbells — two axis-aligned rectangles
//! joined by a random-width neck. Every neck thinner than `min_feature`
//! must get a centerline polyline fully inside it; necks comfortably wider
//! (w > 1.2 * min_feature, margin against boundary flakiness) must produce
//! no polylines at all. Widths in the (min, 1.2 * min] gray zone are not
//! asserted either way.

use cam::{force_clear::force_clear, geom};
use pcb_core::{NM_PER_MM, NM_PER_UM, Nm, P, Poly};
use proptest::prelude::*;

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

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    #[test]
    fn necks_thinner_than_min_feature_get_inside_passes_and_none_elsewhere(
        wa in 2.0..20.0f64,       // rectangle A width, mm
        ha in 4.0..20.0f64,       // rectangle A height, mm
        wb in 2.0..20.0f64,       // rectangle B width, mm
        hb in 4.0..20.0f64,       // rectangle B height, mm
        sep in 1.0..5.0f64,       // horizontal separation, mm
        w in 0.02..2.0f64,        // neck width, mm
        min_feature in 0.05..0.5f64, // mm
    ) {
        // Neck centered on the shorter rectangle's mid-height. Heights start
        // at 4 mm so the neck (<= 2 mm) always keeps > min_feature clearance
        // from the rectangles' top/bottom corners.
        let yc = ha.min(hb) / 2.0;
        prop_assert!(yc - w / 2.0 > min_feature && yc + w / 2.0 < ha.min(hb) - min_feature);

        let a = rect_mm(0.0, 0.0, wa, ha);
        let b = rect_mm(wa + sep, 0.0, wa + sep + wb, hb);
        // Overlap the neck 0.5 mm into each rectangle: the union is then
        // unambiguous, and the exposed neck is exactly the gap span.
        let neck = rect_mm(wa - 0.5, yc - w / 2.0, wa + sep + 0.5, yc + w / 2.0);
        let region = geom::union(&geom::union(&[a], &[b]), &[neck]);
        prop_assert_eq!(region.len(), 1, "dumbbell must be one component");

        let passes = force_clear(&region, min_feature);

        if w < min_feature {
            // Thin neck: at least one pass, and every vertex of every pass
            // inside the exposed neck rectangle (1 um slack).
            prop_assert!(!passes.is_empty(), "thin neck (w = {} mm, min = {} mm) got no pass", w, min_feature);
            let slack: Nm = NM_PER_UM;
            let x0 = (wa * NM_PER_MM as f64).round() as Nm - slack;
            let x1 = ((wa + sep) * NM_PER_MM as f64).round() as Nm + slack;
            let y0 = ((yc - w / 2.0) * NM_PER_MM as f64).round() as Nm - slack;
            let y1 = ((yc + w / 2.0) * NM_PER_MM as f64).round() as Nm + slack;
            for pl in &passes {
                prop_assert!(pl.pts.len() >= 2);
                for p in &pl.pts {
                    prop_assert!(
                        p.x >= x0 && p.x <= x1 && p.y >= y0 && p.y <= y1,
                        "vertex ({}, {}) outside neck box [{}, {}] x [{}, {}]",
                        p.x, p.y, x0, x1, y0, y1
                    );
                }
            }
        } else if w > 1.2 * min_feature {
            // Comfortably wide neck: nothing anywhere (in particular the
            // rectangles' corner roundings must not leak through).
            prop_assert!(
                passes.is_empty(),
                "wide neck (w = {} mm, min = {} mm) produced {} pass(es)",
                w, min_feature, passes.len()
            );
        }
    }
}
