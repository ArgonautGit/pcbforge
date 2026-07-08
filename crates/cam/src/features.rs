//! CAM-8 — fiducial / tooling feature injector.
//!
//! [`inject_features`] prepends machine-registration features to a job so
//! they burn *first*, before any design geometry:
//!
//! * **Three fiducial annulus ops** at the standard board-frame positions
//!   `(5, 5)`, `(5, H−5)`, `(W−5, 5)` mm (see [`fiducial_centers_mm`]),
//!   where `W`/`H` are the board's **design-frame dimensions in mm**.
//! * **Optional tooling-hole center marks** at the two Ø3.02 mm hole
//!   positions. Those positions are pallet-specific, so they arrive as an
//!   `Option<[P; 2]>` argument rather than being hardcoded.
//!
//! # Representation decisions
//!
//! All feature elements are tagged [`PathKind::Mark`], matching the CAM-1
//! convention that closed contours are implicit-closure vertex rings
//! (`closed: true`, first vertex not repeated) and strokes are open
//! polylines. Downstream consumers (CAM-6 ordering, VIS-4 preview) consume
//! *positions*, not fill density, so each annulus op is represented
//! geometrically rather than as a dense burn raster:
//!
//! * **1 mm-diameter filled disc**: one closed circle at the disc boundary
//!   (r = [`FIDUCIAL_DISC_R_MM`] = 0.5 mm) plus concentric closed fill
//!   circles at a fixed [`FIDUCIAL_FILL_PITCH_MM`] = 0.05 mm radial pitch
//!   (r = 0.05, 0.10, …, 0.45 mm).
//! * **1 mm cleared ring** (band from r = 0.5 mm to r =
//!   [`FIDUCIAL_RING_OUTER_R_MM`] = 1.5 mm): represented by its two boundary
//!   circles only — the actual clearing hatch is the burner's concern later.
//!   The inner ring boundary coincides with the disc boundary and is emitted
//!   **once** (it serves both roles), so an annulus op is 9 fill circles +
//!   the shared r = 0.5 mm boundary + the r = 1.5 mm outer boundary =
//!   11 closed elements, innermost to outermost.
//! * **Tooling-hole center mark**: a small cross — two open two-point
//!   elements (horizontal then vertical) with [`CROSS_ARM_MM`] = 1 mm arms,
//!   i.e. each stroke spans ±1 mm about the given center.
//!
//! # Circle discretization
//!
//! Circles are regular polygons whose vertices lie *on* the ideal circle
//! (then rounded to the nm grid, ≤ ~0.7 nm). The mid-chord sagitta of an
//! `n`-gon is `s = r·(1 − cos(π/n))`; requiring `s ≤ e` for chord error
//! `e` = [`MAX_CHORD_ERROR_MM`] = 2 µm gives
//!
//! ```text
//! n = max(MIN_CIRCLE_SEGMENTS, ceil(π / acos(1 − e/r)))
//! ```
//!
//! implemented by [`circle_segments`].

use pcb_core::{NM_PER_MM, P, PathElem, PathKind, Paths};

/// Radius of the filled fiducial disc (1 mm diameter), mm.
pub const FIDUCIAL_DISC_R_MM: f64 = 0.5;

/// Outer radius of the 1 mm-wide cleared ring around the disc, mm.
pub const FIDUCIAL_RING_OUTER_R_MM: f64 = 1.5;

/// Radial pitch of the concentric disc-fill circles, mm.
pub const FIDUCIAL_FILL_PITCH_MM: f64 = 0.05;

/// Inset of the fiducial centers from the board-frame edges, mm.
pub const FIDUCIAL_INSET_MM: f64 = 5.0;

/// Arm length of a tooling-hole center cross (each stroke spans ±arm), mm.
pub const CROSS_ARM_MM: f64 = 1.0;

/// Maximum allowed mid-chord sagitta of a discretized circle, mm (2 µm).
pub const MAX_CHORD_ERROR_MM: f64 = 0.002;

/// Floor on the segment count so tiny radii still look round.
const MIN_CIRCLE_SEGMENTS: usize = 8;

/// The three fiducial centers for a `w × h` mm board (design frame):
/// `(5, 5)`, `(5, h−5)`, `(w−5, 5)` — an L pattern that disambiguates
/// orientation.
pub fn fiducial_centers_mm(board_w_mm: f64, board_h_mm: f64) -> [(f64, f64); 3] {
    [
        (FIDUCIAL_INSET_MM, FIDUCIAL_INSET_MM),
        (FIDUCIAL_INSET_MM, board_h_mm - FIDUCIAL_INSET_MM),
        (board_w_mm - FIDUCIAL_INSET_MM, FIDUCIAL_INSET_MM),
    ]
}

/// Return a new [`Paths`] with the standard registration features
/// **prepended** (they burn first) and the original elements after.
///
/// `board_w_mm` / `board_h_mm` are the board's design-frame dimensions in
/// mm. Prepended, in order: the three fiducial annulus ops of
/// [`annulus_op`] at [`fiducial_centers_mm`], then (if `tooling_marks` is
/// `Some`) one [`cross_op`] per given Ø3.02 tooling-hole position.
pub fn inject_features(
    paths: &Paths,
    board_w_mm: f64,
    board_h_mm: f64,
    tooling_marks: Option<[P; 2]>,
) -> Paths {
    let mut elems = Vec::new();
    for (cx, cy) in fiducial_centers_mm(board_w_mm, board_h_mm) {
        elems.extend(annulus_op(cx, cy));
    }
    if let Some(centers) = tooling_marks {
        for c in centers {
            elems.extend(cross_op(c));
        }
    }
    elems.extend(paths.elems.iter().cloned());
    Paths { elems }
}

/// One fiducial annulus op centered at `(cx_mm, cy_mm)`: the concentric
/// fill circles, the shared disc/ring-inner boundary at
/// [`FIDUCIAL_DISC_R_MM`], and the ring outer boundary at
/// [`FIDUCIAL_RING_OUTER_R_MM`] — all closed [`PathKind::Mark`] contours,
/// innermost to outermost. See the module docs for the representation
/// rationale.
pub fn annulus_op(cx_mm: f64, cy_mm: f64) -> Vec<PathElem> {
    // Fill radii k·pitch for k in 1..n where n·pitch == disc radius, then
    // the disc boundary itself, then the ring outer boundary.
    let n_fill = (FIDUCIAL_DISC_R_MM / FIDUCIAL_FILL_PITCH_MM).round() as usize;
    let mut elems = Vec::with_capacity(n_fill + 1);
    for k in 1..n_fill {
        elems.push(circle(cx_mm, cy_mm, k as f64 * FIDUCIAL_FILL_PITCH_MM));
    }
    elems.push(circle(cx_mm, cy_mm, FIDUCIAL_DISC_R_MM));
    elems.push(circle(cx_mm, cy_mm, FIDUCIAL_RING_OUTER_R_MM));
    elems
}

/// A tooling-hole center mark at `c`: two open two-point
/// [`PathKind::Mark`] strokes (horizontal then vertical), each spanning
/// ±[`CROSS_ARM_MM`] about `c`.
pub fn cross_op(c: P) -> Vec<PathElem> {
    let arm = (CROSS_ARM_MM * NM_PER_MM as f64).round() as i64;
    let stroke = |a: P, b: P| PathElem {
        kind: PathKind::Mark,
        pts: vec![a, b],
        closed: false,
    };
    vec![
        stroke(P::new(c.x - arm, c.y), P::new(c.x + arm, c.y)),
        stroke(P::new(c.x, c.y - arm), P::new(c.x, c.y + arm)),
    ]
}

/// Segment count for a radius-`r_mm` circle so the mid-chord sagitta
/// `r·(1 − cos(π/n))` stays ≤ [`MAX_CHORD_ERROR_MM`]:
/// `max(`[`MIN_CIRCLE_SEGMENTS`]`, ceil(π / acos(1 − e/r)))`.
pub fn circle_segments(r_mm: f64) -> usize {
    let cos_half = (1.0 - MAX_CHORD_ERROR_MM / r_mm).clamp(-1.0, 1.0);
    let n = (std::f64::consts::PI / cos_half.acos()).ceil() as usize;
    n.max(MIN_CIRCLE_SEGMENTS)
}

/// A closed [`PathKind::Mark`] circle: a regular [`circle_segments`]-gon
/// with vertices on the ideal circle, rounded to the nm grid.
fn circle(cx_mm: f64, cy_mm: f64, r_mm: f64) -> PathElem {
    let n = circle_segments(r_mm);
    let pts = (0..n)
        .map(|i| {
            let t = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
            P::from_mm(cx_mm + r_mm * t.cos(), cy_mm + r_mm * t.sin())
        })
        .collect();
    PathElem {
        kind: PathKind::Mark,
        pts,
        closed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::Nm;

    const MM: f64 = NM_PER_MM as f64;
    const UM: f64 = MM / 1_000.0;

    fn dummy_original() -> Paths {
        Paths {
            elems: vec![PathElem {
                kind: PathKind::ForceClear,
                pts: vec![P::from_mm(1.0, 1.0), P::from_mm(2.0, 2.0)],
                closed: false,
            }],
        }
    }

    fn centroid(pts: &[P]) -> (f64, f64) {
        let n = pts.len() as f64;
        (
            pts.iter().map(|p| p.x as f64).sum::<f64>() / n,
            pts.iter().map(|p| p.y as f64).sum::<f64>() / n,
        )
    }

    fn mean_radius(e: &PathElem, cx: f64, cy: f64) -> f64 {
        let n = e.pts.len() as f64;
        e.pts
            .iter()
            .map(|p| (p.x as f64 - cx).hypot(p.y as f64 - cy))
            .sum::<f64>()
            / n
    }

    /// Expected per-annulus radii in nm, innermost to outermost.
    fn expected_radii_nm() -> Vec<f64> {
        let mut r: Vec<f64> = (1..10).map(|k| k as f64 * 0.05 * MM).collect();
        r.push(0.5 * MM);
        r.push(1.5 * MM);
        r
    }

    #[test]
    fn annulus_groups_centered_at_board_frame_positions_100x80() {
        let out = inject_features(&dummy_original(), 100.0, 80.0, None);
        let per = annulus_op(0.0, 0.0).len();
        assert_eq!(per, 11, "9 fill + shared 0.5 boundary + 1.5 boundary");
        assert_eq!(out.elems.len(), 3 * per + 1);

        let expected_centers = [(5.0, 5.0), (5.0, 75.0), (95.0, 5.0)];
        assert_eq!(fiducial_centers_mm(100.0, 80.0), expected_centers);

        for (g, &(cx_mm, cy_mm)) in expected_centers.iter().enumerate() {
            let (cx, cy) = (cx_mm * MM, cy_mm * MM);
            for e in &out.elems[g * per..(g + 1) * per] {
                assert_eq!(e.kind, PathKind::Mark);
                assert!(e.closed);
                let (gx, gy) = centroid(&e.pts);
                assert!(
                    (gx - cx).abs() < UM && (gy - cy).abs() < UM,
                    "group {g}: centroid ({gx}, {gy}) not within 1 µm of ({cx}, {cy})"
                );
            }
        }
    }

    #[test]
    fn annulus_radii_are_fill_pitch_disc_and_ring_boundaries() {
        let elems = annulus_op(5.0, 5.0);
        let expected = expected_radii_nm();
        assert_eq!(elems.len(), expected.len());
        for (e, want) in elems.iter().zip(&expected) {
            let r = mean_radius(e, 5.0 * MM, 5.0 * MM);
            // Vertices sit on the ideal circle up to nm rounding.
            assert!(
                (r - want).abs() < 2.0,
                "mean radius {r} nm, expected {want} nm"
            );
        }
    }

    #[test]
    fn tooling_marks_are_crosses_centered_at_given_positions() {
        let holes = [P::from_mm(10.0, 10.0), P::from_mm(90.0, 70.0)];
        let out = inject_features(&dummy_original(), 100.0, 80.0, Some(holes));
        let per = annulus_op(0.0, 0.0).len();
        assert_eq!(out.elems.len(), 3 * per + 4 + 1);

        let arm = NM_PER_MM; // 1 mm arms
        for (i, c) in holes.iter().enumerate() {
            let cross = &out.elems[3 * per + 2 * i..3 * per + 2 * i + 2];
            for e in cross {
                assert_eq!(e.kind, PathKind::Mark);
                assert!(!e.closed);
                assert_eq!(e.pts.len(), 2);
                // Midpoint is exactly the given center.
                assert_eq!((e.pts[0].x + e.pts[1].x) / 2, c.x);
                assert_eq!((e.pts[0].y + e.pts[1].y) / 2, c.y);
            }
            assert_eq!(
                cross[0].pts,
                vec![P::new(c.x - arm, c.y), P::new(c.x + arm, c.y)]
            );
            assert_eq!(
                cross[1].pts,
                vec![P::new(c.x, c.y - arm), P::new(c.x, c.y + arm)]
            );
        }
    }

    #[test]
    fn features_are_prepended_before_original_elements() {
        let original = dummy_original();
        let out = inject_features(
            &original,
            100.0,
            80.0,
            Some([P::from_mm(50.0, 8.0), P::from_mm(50.0, 72.0)]),
        );
        let n_features = out.elems.len() - 1;
        assert!(
            out.elems[..n_features]
                .iter()
                .all(|e| e.kind == PathKind::Mark),
            "all prepended feature elements are Mark"
        );
        assert_eq!(out.elems[n_features], original.elems[0], "original last");
        // The input is not mutated.
        assert_eq!(original.elems.len(), 1);
    }

    #[test]
    fn circle_sagitta_within_two_micron_bound() {
        // Spot-check every circle of one annulus: vertices are ON the ideal
        // circle, so the discretization error is the mid-chord sagitta.
        // Allow 5 nm slack for nm-grid vertex rounding.
        let (cx, cy) = (95.0 * MM, 5.0 * MM);
        for e in annulus_op(95.0, 5.0) {
            let r = mean_radius(&e, cx, cy);
            let n = e.pts.len();
            for i in 0..n {
                let (a, b) = (e.pts[i], e.pts[(i + 1) % n]);
                let mx = (a.x + b.x) as f64 / 2.0 - cx;
                let my = (a.y + b.y) as f64 / 2.0 - cy;
                let sagitta = r - mx.hypot(my);
                assert!(
                    sagitta <= 2.0 * UM + 5.0,
                    "sagitta {sagitta} nm > 2 µm at r = {r} nm ({n} segments)"
                );
            }
        }
    }

    #[test]
    fn circle_segments_formula_matches_sagitta_bound() {
        for r_mm in [0.05, 0.1, 0.5, 1.5, 10.0] {
            let n = circle_segments(r_mm);
            assert!(n >= MIN_CIRCLE_SEGMENTS);
            let sagitta = |k: usize| r_mm * (1.0 - (std::f64::consts::PI / k as f64).cos());
            assert!(sagitta(n) <= MAX_CHORD_ERROR_MM, "r = {r_mm} mm, n = {n}");
            // Minimal (unless clamped): one fewer segment would violate it.
            if n > MIN_CIRCLE_SEGMENTS {
                assert!(
                    sagitta(n - 1) > MAX_CHORD_ERROR_MM,
                    "n not minimal at r = {r_mm}"
                );
            }
        }
    }

    #[test]
    fn empty_job_gets_features_only() {
        let out = inject_features(&Paths::default(), 100.0, 80.0, None);
        let per = annulus_op(0.0, 0.0).len();
        assert_eq!(out.elems.len(), 3 * per);
        assert!(out.elems.iter().all(|e| e.kind == PathKind::Mark));
        let _: Nm = out.elems[0].pts[0].x; // nm-typed geometry
    }
}
