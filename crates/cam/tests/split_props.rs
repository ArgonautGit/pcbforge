//! Property tests for CAM-5 dual-machine split.
//!
//! Core invariant: no fiber element ever comes within `guard_mm` of a copper
//! boundary. Plus a deterministic fixture that writes an SVG debug dump for an
//! eyeball check.

use std::path::PathBuf;

use cam::split::{GUARD_TOLERANCE_NM, debug_svg, min_dist_to_polys_nm, split};
use pcb_core::{CamOpts, Layer, NM_PER_MM, P, Poly};
use proptest::prelude::*;

/// A copper rectangle on a 10 µm grid, anywhere in a ±5 mm window, 0.5–4 mm
/// on a side.
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

/// A copper disc (regular 64-gon), 0.5–3 mm radius, anywhere in ±5 mm.
fn disc_strategy() -> impl Strategy<Value = Poly> {
    (-500i64..500, -500i64..500, 50i64..300).prop_map(|(cx, cy, r)| {
        let (cx, cy, r) = (cx as f64 * 1e4, cy as f64 * 1e4, r as f64 * 1e4);
        let outer = (0..64)
            .map(|i| {
                let t = 2.0 * std::f64::consts::PI * i as f64 / 64.0;
                P::new(
                    (cx + r * t.cos()).round() as i64,
                    (cy + r * t.sin()).round() as i64,
                )
            })
            .collect();
        Poly {
            outer,
            holes: vec![],
        }
    })
}

fn layer_strategy() -> impl Strategy<Value = Layer> {
    prop::collection::vec(prop_oneof![rect_strategy(), disc_strategy()], 1..4)
        .prop_map(|polys| Layer { polys })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// The guard invariant: every fiber element stays at least `guard_mm`
    /// (minus the documented arc/rounding tolerance) from every copper edge.
    /// Copper boundary = the design polys normalized at offset 0.
    #[test]
    fn fiber_never_within_guard_of_copper(layer in layer_strategy()) {
        let opts = CamOpts::default();
        let jobs = split(&layer, &opts);
        let copper = cam::geom::offset(&layer.polys, 0);
        let guard_nm = (opts.guard_mm * NM_PER_MM as f64).round();
        for e in &jobs.fiber.elems {
            let d = min_dist_to_polys_nm(&e.pts, e.closed, &copper);
            prop_assert!(
                d >= guard_nm - GUARD_TOLERANCE_NM as f64,
                "fiber element only {} nm from copper (guard {} nm, tol {} nm)",
                d, guard_nm, GUARD_TOLERANCE_NM
            );
        }
    }
}

/// Deterministic visual fixture: a couple of copper features, split, dumped to
/// `target/test-artifacts/split-debug.svg` for an eyeball check.
#[test]
fn split_debug_svg_fixture() {
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
    let disc = {
        let (cx, cy, r) = (14.0, 4.0, 2.5);
        let outer = (0..96)
            .map(|i| {
                let t = 2.0 * std::f64::consts::PI * i as f64 / 96.0;
                P::from_mm(cx + r * t.cos(), cy + r * t.sin())
            })
            .collect();
        Poly {
            outer,
            holes: vec![],
        }
    };
    let layer = Layer {
        polys: vec![rect_mm(0.0, 0.0, 6.0, 8.0), disc],
    };
    let jobs = split(&layer, &CamOpts::default());

    // Workspace target dir: <manifest>/../../target/test-artifacts.
    let out: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "target",
        "test-artifacts",
        "split-debug.svg",
    ]
    .iter()
    .collect();
    debug_svg(&jobs, &out).expect("write split-debug.svg");
    let body = std::fs::read_to_string(&out).expect("read split-debug.svg");
    assert!(body.starts_with("<svg") && body.contains("</svg>"));
}
