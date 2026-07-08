//! Property tests for CAM-1 ablation path generation.

use cam::ablation::{ablation_paths, hatch_set_angle_deg, point_in_polys, rubout_band};
use pcb_core::{CamOpts, Layer, P, PathKind, Poly};
use proptest::prelude::*;

/// A copper rectangle on a 10 µm grid (coordinates in µm to keep shapes
/// non-degenerate), anywhere in a ±5 mm window, 0.5–4 mm on a side.
fn rect_strategy() -> impl Strategy<Value = Poly> {
    (-500i64..500, -500i64..500, 50i64..400, 50i64..400).prop_map(|(x0, y0, w, h)| {
        let um10 = 10_000; // 10 µm in nm
        let (x0, y0, x1, y1) = (x0 * um10, y0 * um10, (x0 + w) * um10, (y0 + h) * um10);
        Poly {
            outer: vec![
                P::new(x0, y0),
                P::new(x1, y0),
                P::new(x1, y1),
                P::new(x0, y1),
            ],
            holes: vec![],
        }
    })
}

fn layer_strategy() -> impl Strategy<Value = Layer> {
    prop::collection::vec(rect_strategy(), 1..3).prop_map(|polys| Layer { polys })
}

fn opts_strategy() -> impl Strategy<Value = CamOpts> {
    (
        20i64..100, // clearance, hundredths of mm: 0.2–1.0 mm
        30i64..200, // band, hundredths of mm: 0.3–2.0 mm
        3i64..20,   // interval, hundredths of mm: 0.03–0.2 mm
        0i64..1440, // base angle, eighths of a degree: 0–180 deg
        1i64..320,  // angle step, eighths of a degree: 0.125–40 deg
    )
        .prop_map(|(clr, band, ivl, base, step)| CamOpts {
            n_contours: 0,
            clearance_mm: clr as f64 / 100.0,
            band_mm: band as f64 / 100.0,
            interval_mm: ivl as f64 / 100.0,
            base_angle_deg: base as f64 * 0.125,
            fill_angle_step_deg: step as f64 * 0.125,
            ..CamOpts::default()
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// No hatch segment midpoint lies outside the rub-out band (boundary
    /// padded by 10 nm to absorb the rotate-and-round grid error).
    #[test]
    fn hatch_midpoints_never_leave_the_band(
        layer in layer_strategy(),
        opts in opts_strategy(),
        hatch_sets in 1u32..4,
    ) {
        let band = rubout_band(&layer, &opts);
        let paths = ablation_paths(&layer, &opts, hatch_sets);
        for e in &paths.elems {
            prop_assert_eq!(e.pts.len(), 2);
            prop_assert!(!e.closed);
            let mid = P::new((e.pts[0].x + e.pts[1].x) / 2, (e.pts[0].y + e.pts[1].y) / 2);
            prop_assert!(
                point_in_polys(mid, &band, 10),
                "midpoint {:?} of {:?} segment outside band", mid, e.kind
            );
        }
    }

    /// Segments of hatch set k run at base + k*step degrees (mod 180),
    /// measured from the longest segment of the set (angle-of-a-rounded-
    /// segment error shrinks with length, so gate on >= 0.5 mm).
    #[test]
    fn hatch_sets_run_at_their_pass_angle(
        layer in layer_strategy(),
        opts in opts_strategy(),
        hatch_sets in 1u32..4,
    ) {
        let paths = ablation_paths(&layer, &opts, hatch_sets);
        for k in 0..hatch_sets {
            let longest = paths
                .elems
                .iter()
                .filter(|e| e.kind == PathKind::Rubout(k))
                .max_by(|a, b| seg_len_nm(a.pts[0], a.pts[1])
                    .total_cmp(&seg_len_nm(b.pts[0], b.pts[1])));
            let Some(e) = longest else { continue };
            if seg_len_nm(e.pts[0], e.pts[1]) < 500_000.0 {
                continue; // too short to measure an angle meaningfully
            }
            let measured = ((e.pts[1].y - e.pts[0].y) as f64)
                .atan2((e.pts[1].x - e.pts[0].x) as f64)
                .to_degrees();
            let expected = hatch_set_angle_deg(&opts, k);
            let diff = (measured - expected).rem_euclid(180.0);
            let dist = diff.min(180.0 - diff);
            prop_assert!(dist < 1e-3, "set {}: measured {} expected {}", k, measured, expected);
        }
    }

    /// Every pass angle differs from its neighbor by exactly
    /// fill_angle_step_deg (exact f64 equality on a dyadic degree grid).
    #[test]
    fn consecutive_pass_angles_differ_by_exactly_the_step(
        base_eighths in -2880i64..2880,
        step_eighths in 1i64..2880,
        k in 0u32..64,
    ) {
        let opts = CamOpts {
            base_angle_deg: base_eighths as f64 * 0.125,
            fill_angle_step_deg: step_eighths as f64 * 0.125,
            ..CamOpts::default()
        };
        prop_assert_eq!(
            hatch_set_angle_deg(&opts, k + 1) - hatch_set_angle_deg(&opts, k),
            opts.fill_angle_step_deg
        );
    }
}

fn seg_len_nm(a: P, b: P) -> f64 {
    ((b.x - a.x) as f64).hypot((b.y - a.y) as f64)
}
