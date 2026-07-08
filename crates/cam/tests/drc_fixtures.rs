//! CAM-7 fixture tests: known-geometry boards checked against two machine
//! floors (0.15 mm and 0.08 mm).

use cam::drc::{ViolationKind, drc};
use pcb_core::{Layer, P, Poly};

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

/// Fixture 1: two 5 mm squares separated by exactly 0.1 mm (channel
/// x ∈ [5.0, 5.1] mm, y ∈ [0, 5] mm).
fn gap_fixture() -> Layer {
    Layer {
        polys: vec![rect_mm(0.0, 0.0, 5.0, 5.0), rect_mm(5.1, 0.0, 10.1, 5.0)],
    }
}

/// Fixture 2: an L-shaped trace 0.12 mm wide (3 mm horizontal arm, 2 mm
/// vertical arm), as a single CCW polygon.
fn l_trace_fixture() -> Layer {
    Layer {
        polys: vec![Poly {
            outer: vec![
                P::from_mm(0.0, 0.0),
                P::from_mm(3.0, 0.0),
                P::from_mm(3.0, 2.0),
                P::from_mm(2.88, 2.0),
                P::from_mm(2.88, 0.12),
                P::from_mm(0.0, 0.12),
            ],
            holes: vec![],
        }],
    }
}

/// Fixture 3: a clean board — one fat 5 mm square.
fn clean_fixture() -> Layer {
    Layer {
        polys: vec![rect_mm(0.0, 0.0, 5.0, 5.0)],
    }
}

#[test]
fn gap_fixture_flagged_at_floor_0_15() {
    let v = drc(&gap_fixture(), 0.15);
    let gaps: Vec<_> = v
        .iter()
        .filter(|v| v.kind == ViolationKind::GapBelowFloor)
        .collect();
    assert!(!gaps.is_empty(), "0.1 mm gap must be flagged at floor 0.15");
    // Every gap violation must sit inside the 0.1 mm channel.
    for g in &gaps {
        let (x, y) = (g.location.x_mm(), g.location.y_mm());
        assert!(
            (5.0..=5.1).contains(&x) && (0.0..=5.0).contains(&y),
            "gap location ({x}, {y}) mm outside the channel"
        );
        let w = g.measured_mm.expect("gap width should be measurable");
        assert!((w - 0.1).abs() < 0.002, "measured {w} mm, expected ~0.1");
    }
    // Fat squares: no width violations.
    assert!(
        v.iter()
            .all(|v| v.kind != ViolationKind::TraceWidthBelowFloor),
        "unexpected trace-width violations: {v:?}"
    );
}

#[test]
fn gap_fixture_passes_at_floor_0_08() {
    let v = drc(&gap_fixture(), 0.08);
    assert!(v.is_empty(), "0.1 mm gap must pass at floor 0.08: {v:?}");
}

#[test]
fn thin_l_trace_flagged_at_floor_0_15() {
    let v = drc(&l_trace_fixture(), 0.15);
    let widths: Vec<_> = v
        .iter()
        .filter(|v| v.kind == ViolationKind::TraceWidthBelowFloor)
        .collect();
    assert!(
        !widths.is_empty(),
        "0.12 mm trace must be flagged at floor 0.15: {v:?}"
    );
    for w in &widths {
        // measured_mm is twice the region's inradius. For the whole flagged
        // L-trace the widest inscribed disk sits at the elbow, diameter
        // 0.12 * 2*sqrt(2)/(sqrt(2)+1) ~= 0.1406 mm — still below the floor.
        let m = w.measured_mm.expect("trace width should be measurable");
        assert!(
            (0.118..0.15).contains(&m),
            "measured {m} mm, expected in [0.12, 0.15)"
        );
    }
    // A single connected trace has no copper-to-copper gap.
    assert!(
        v.iter().all(|v| v.kind != ViolationKind::GapBelowFloor),
        "reentrant corner must not raise a gap violation: {v:?}"
    );
}

#[test]
fn thin_l_trace_clean_at_floor_0_08() {
    let v = drc(&l_trace_fixture(), 0.08);
    assert!(v.is_empty(), "0.12 mm trace must pass at floor 0.08: {v:?}");
}

#[test]
fn clean_board_is_clean_at_both_floors() {
    assert!(drc(&clean_fixture(), 0.15).is_empty());
    assert!(drc(&clean_fixture(), 0.08).is_empty());
}

/// Nanometer sanity: the channel centroid should land mid-channel.
#[test]
fn gap_location_is_mid_channel() {
    let v = drc(&gap_fixture(), 0.15);
    let g = v
        .iter()
        .find(|v| v.kind == ViolationKind::GapBelowFloor)
        .expect("gap violation present");
    // Mid-channel is (5.05, 2.5) mm; erosion trims the mouths symmetrically.
    assert!((g.location.x - 5_050_000).abs() < 5_000);
    assert!((g.location.y - 2_500_000).abs() < 200_000);
}
