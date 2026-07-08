//! Geometry kernel: boolean ops and offsetting over nm [`Poly`] types.
//!
//! # Backends and numeric boundaries
//!
//! * **Boolean ops** (union / intersect / difference / xor) use `i_overlay`
//!   v7's *generic integer* path (`Overlay<i64>` over `IntPoint<i64>`), so nm
//!   coordinates flow through **exactly** — no float conversion at all. The
//!   crate's wide arithmetic for `i64` is `u128`/`i128`, so ±1 m (1e9 nm)
//!   coordinates have ample headroom. Fill rule is `NonZero`, which matches
//!   our orientation convention (outer CCW, holes CW); output uses
//!   `i_overlay`'s default `ContourDirection::CounterClockwise`, i.e. outer
//!   CCW / holes CW — the same convention, so no re-orientation is needed.
//!
//! * **Offsets** use `cavalier_contours`, which is f64-only. Coordinates
//!   cross the boundary as **millimeters**: `mm = nm / 1e6` is correctly
//!   rounded (one f64 division), and the return trip `round(mm * 1e6)`
//!   reproduces the original integer exactly for |coord| ≤ 1 m (relative
//!   error per op ≤ 2⁻⁵³, so the accumulated error at 1e9 nm is ≈ 2e-7 nm ≪
//!   0.5 nm). The round trip is therefore lossless even though a single
//!   nm→mm conversion is only correctly rounded. New vertices *created* by
//!   the offset are rounded to the nm grid (≤ 0.5 nm error, far below any
//!   process tolerance).
//!
//! # Offset semantics
//!
//! `offset(polys, +d)` dilates (region grows by `d` everywhere, round joins
//! at convex corners), `offset(polys, -d)` erodes. `cavalier_contours`
//! offsets step *left* of the tangent for positive amounts; with a CCW loop
//! "left" is the loop interior, so a loop-outward offset by `η` is
//! `parallel_offset(-η)`. Region semantics per ring: an outer ring's loop
//! grows with the region (`η = d`), a hole ring's loop shrinks when the
//! region dilates (`η = -d`). Rings are offset independently (canonicalized
//! to CCW) and the resulting loops recombined in a single `i_overlay` pass
//! (`OverlayRule::Subject` + `FillRule::Positive`, winding-sum > 0), which
//! resolves outer/hole interactions (holes merging with the boundary under
//! erosion, holes vanishing under dilation, islands splitting) and
//! re-normalizes orientation.
//!
//! # Offset robustness: trimmed primary, winding validator
//!
//! `Polyline::parallel_offset` (raw offset → slice validation → stitching)
//! proved unreliable on exactly the inputs this kernel produces. Observed
//! empirically with the pinned 0.7.0:
//!
//! * it can **panic** on inward offsets that collapse a region (upstream
//!   issue #79) — every call here is wrapped in `catch_unwind`;
//! * its result **depends on the starting vertex** of the closed polyline
//!   (eroding one test ring succeeded for only 9 of its 75 rotations —
//!   starting inside a dense fan of arc-flattening chords makes the slice
//!   validation prune every slice);
//! * eroding a just-dilated region by the same `d` (corner arcs of radius
//!   exactly `d`) is fully degenerate and returns **no loops** for *any*
//!   rotation;
//! * worst, it can return a **plausible-looking but over-pruned** result
//!   (one observed erode silently lost 17 % of the region's area).
//!
//! To keep `parallel_offset`'s clean output while defusing all four modes,
//! each ring's offset is computed twice:
//!
//! 1. **Reference (winding)**: the *raw* (untrimmed) offset curve from
//!    `polyline::internal::pline_offset::create_raw_offset_polyline`, arcs
//!    flattened, then the **positive-winding region** extracted with the
//!    exact integer kernel (the Chen–McMains winding-number offset). This
//!    never loses area chunks; its only defect is small **overfill
//!    artifacts** where the offset distance exactly reaches a local feature
//!    radius (measured: attached slivers of ~chord scale, area ≲
//!    `1e-3·η²` per radian of corner turn; a fully collapsed ring can leave
//!    a phantom loop).
//! 2. **Primary (trimmed)**: a retry ladder of `parallel_offset` attempts —
//!    the ring rotated to start at its three longest edges, then two
//!    overshoot-and-compensate attempts (offset by `η·(1+ε)` then back by
//!    `η·ε`, which un-degenerates exact-feature-radius erodes at the cost of
//!    corner rounding at the `ε·|η|` scale).
//!
//! The first trimmed attempt whose total area matches the winding
//! reference within the artifact budget wins. An empty trimmed result
//! validates against a phantom-sized reference too, so genuine collapses
//! return empty. If nothing validates: an all-empty ladder with a
//! collapse-plausible ring (`area ≤ perimeter·|η|`, a sound coarea bound —
//! full collapse forces the whole region within `|η|` of its boundary)
//! returns empty, otherwise the winding reference is used as fallback
//! (complete, possibly with sliver artifacts). Known residual: a ring that
//! is thin relative to `|η|` *and* defeats the whole trimmed ladder may
//! resolve to the winding fallback or empty conservatively; and a phantom
//! from a fully-collapsed *pocket* nested inside a surviving region can
//! survive via the fallback. Both are beyond anything the CAM pipeline
//! generates (offsets ≪ feature sizes).
//!
//! Offsetting produces arc segments (bulges) at joins; these are flattened
//! back to line segments with `arcs_to_approx_lines` using a tolerance of
//! `max(|d| / 1000, 1 nm)` before re-entering the integer world.

use std::panic::{AssertUnwindSafe, catch_unwind};

use cavalier_contours::polyline::internal::pline_offset::create_raw_offset_polyline;
use cavalier_contours::polyline::{PlineSource, PlineSourceMut, Polyline};
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::{Overlay, ShapeType};
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use i_overlay::i_shape::int::shape::{IntContour, IntShapes};
use pcb_core::{NM_PER_MM, Nm, P, Poly, Ring};

/// Union of the regions `a ∪ b`. Output is normalized: outer CCW, holes CW,
/// no self-intersections, degenerate contours removed.
pub fn union(a: &[Poly], b: &[Poly]) -> Vec<Poly> {
    boolean(a, b, OverlayRule::Union)
}

/// Intersection `a ∩ b`. Same normalization as [`union`].
pub fn intersect(a: &[Poly], b: &[Poly]) -> Vec<Poly> {
    boolean(a, b, OverlayRule::Intersect)
}

/// Difference `a \ b`. Same normalization as [`union`].
pub fn difference(a: &[Poly], b: &[Poly]) -> Vec<Poly> {
    boolean(a, b, OverlayRule::Difference)
}

/// Symmetric difference `a ⊕ b`. Same normalization as [`union`].
pub fn xor(a: &[Poly], b: &[Poly]) -> Vec<Poly> {
    boolean(a, b, OverlayRule::Xor)
}

/// Offset (morphological dilate/erode) of the region by `delta_nm`.
///
/// Positive `delta_nm` grows the region outward by `delta_nm` everywhere
/// (round joins at convex corners); negative shrinks it. A region that fully
/// collapses under a negative offset yields an empty vec instead of panicking
/// (see the module docs). `delta_nm == 0` returns the input normalized.
pub fn offset(polys: &[Poly], delta_nm: Nm) -> Vec<Poly> {
    if delta_nm == 0 {
        return boolean(polys, &[], OverlayRule::Subject);
    }
    let delta_mm = delta_nm as f64 / NM_PER_MM as f64;
    // Max chord-to-arc distance when flattening offset arcs, in mm.
    let arc_tol_mm = (delta_mm.abs() * 1e-3).max(1e-6);

    let mut loops: Vec<IntContour<i64>> = Vec::new();
    for poly in polys {
        ring_offset_loops(&poly.outer, delta_mm, arc_tol_mm, &mut loops);
        for hole in &poly.holes {
            ring_offset_loops(hole, delta_mm, arc_tol_mm, &mut loops);
        }
    }
    winding_extract(&loops)
}

/// Total signed area of `polys` in nm². Computed exactly in `i128` (shoelace)
/// and converted to `f64` at the end. Positive for convention-correct input
/// (outer CCW, holes CW subtract).
pub fn area_nm2(polys: &[Poly]) -> f64 {
    polys.iter().map(poly_area).sum()
}

/// Signed area of one polygon in nm²: shoelace of the outer ring plus the
/// (negative, by the CW convention) shoelace of each hole. Exact `i128`
/// arithmetic, converted to `f64` at the end.
pub fn poly_area(p: &Poly) -> f64 {
    let mut doubled = ring_doubled_area(&p.outer);
    for hole in &p.holes {
        doubled += ring_doubled_area(hole);
    }
    doubled as f64 / 2.0
}

/// Twice the signed shoelace area of `ring`, exact in `i128`.
fn ring_doubled_area(ring: &Ring) -> i128 {
    if ring.len() < 3 {
        return 0;
    }
    let mut sum: i128 = 0;
    for (i, a) in ring.iter().enumerate() {
        let b = &ring[(i + 1) % ring.len()];
        sum += a.x as i128 * b.y as i128 - b.x as i128 * a.y as i128;
    }
    sum
}

// ---------------------------------------------------------------------------
// Boolean machinery (exact i64)
// ---------------------------------------------------------------------------

fn boolean(subj: &[Poly], clip: &[Poly], rule: OverlayRule) -> Vec<Poly> {
    let cap = point_count(subj) + point_count(clip);
    if cap == 0 {
        return Vec::new();
    }
    let mut ov = Overlay::<i64>::new(cap);
    for p in subj {
        add_poly(&mut ov, p, ShapeType::Subject);
    }
    for p in clip {
        add_poly(&mut ov, p, ShapeType::Clip);
    }
    shapes_to_polys(ov.overlay(rule, FillRule::NonZero))
}

/// Positive-winding extraction of a soup of (possibly self-intersecting,
/// mixed-orientation) closed loops: keeps the region with winding-sum > 0
/// and normalizes orientation.
fn winding_extract(loops: &[IntContour<i64>]) -> Vec<Poly> {
    if loops.is_empty() {
        return Vec::new();
    }
    let cap = loops.iter().map(Vec::len).sum();
    let mut ov = Overlay::<i64>::new(cap);
    for l in loops {
        ov.add_contour(l, ShapeType::Subject);
    }
    shapes_to_polys(ov.overlay(OverlayRule::Subject, FillRule::Positive))
}

fn point_count(polys: &[Poly]) -> usize {
    polys
        .iter()
        .map(|p| p.outer.len() + p.holes.iter().map(Vec::len).sum::<usize>())
        .sum()
}

fn add_poly(ov: &mut Overlay<i64>, p: &Poly, st: ShapeType) {
    if p.outer.len() < 3 {
        return; // degenerate polygon: no area, nothing to add
    }
    ov.add_contour(&ring_to_contour(&p.outer), st);
    for hole in &p.holes {
        if hole.len() >= 3 {
            ov.add_contour(&ring_to_contour(hole), st);
        }
    }
}

fn ring_to_contour(ring: &Ring) -> IntContour<i64> {
    ring.iter().map(|p| IntPoint::new(p.x, p.y)).collect()
}

fn shapes_to_polys(shapes: IntShapes<i64>) -> Vec<Poly> {
    shapes
        .into_iter()
        .filter_map(|mut shape| {
            if shape.is_empty() {
                return None;
            }
            let holes = shape.split_off(1);
            Some(Poly {
                outer: contour_to_ring(shape.into_iter().next().unwrap()),
                holes: holes.into_iter().map(contour_to_ring).collect(),
            })
        })
        .collect()
}

fn contour_to_ring(contour: IntContour<i64>) -> Ring {
    contour.into_iter().map(|q| P::new(q.x, q.y)).collect()
}

// ---------------------------------------------------------------------------
// Offset machinery (cavalier_contours at the boundary)
// ---------------------------------------------------------------------------

/// cavalier's default fuzzy position epsilon, in mm (10 nm).
const POS_EQUAL_EPS_MM: f64 = 1e-5;

/// Relative overshoots for the un-degenerating retry attempts. Two distinct
/// factors so a panic zone around one does not sink both.
const OVERSHOOTS: [f64; 2] = [0.05, 0.11];

/// How many longest-edge starting rotations to try.
const RETRY_ROTATIONS: usize = 3;

/// Winding-artifact area budget per radian of total boundary turning, as a
/// fraction of `η²`. Artifacts form at high-curvature features, one per
/// corner, each ~`1e-3·η²` per radian of corner turn (measured ~2× below
/// this constant); over-pruned chunks are orders of magnitude larger. For a
/// convex ring (total turning 2π) the budget is ~`0.013·η²`, which stays
/// under 0.1 % of the area for offsets up to ~40 % of the effective radius.
const ARTIFACT_BUDGET_PER_RADIAN: f64 = 2e-3;

/// Offset one ring by `delta_mm` outward-of-the-region and push the
/// resulting loops (oriented to contribute the ring's original winding sign)
/// onto `out`. See the module docs for the trimmed-primary /
/// winding-validator scheme.
fn ring_offset_loops(ring: &Ring, delta_mm: f64, arc_tol_mm: f64, out: &mut Vec<IntContour<i64>>) {
    if ring.len() < 3 {
        return;
    }
    // Canonicalize to a CCW loop; a hole (CW) ring's loop shrinks when the
    // region dilates, so its loop-outward offset has the opposite sign.
    let ccw = ring_doubled_area(ring) > 0;
    let canonical: Ring = if ccw {
        ring.clone()
    } else {
        ring.iter().rev().cloned().collect()
    };
    let eta_mm = if ccw { delta_mm } else { -delta_mm };
    let eta_nm = eta_mm * NM_PER_MM as f64;

    // Reference: positive-winding region of the raw offset curve.
    let reference = winding_reference(&canonical, eta_mm, arc_tol_mm);
    let ref_area = area_nm2(&reference);

    let a_ring = ring_doubled_area(&canonical).unsigned_abs() as f64 / 2.0;
    let perim = ring_perimeter_nm(&canonical);
    let budget = ARTIFACT_BUDGET_PER_RADIAN * eta_nm * eta_nm * total_abs_turning(&canonical)
        + 2.0 * perim
        + 1e6;

    // Primary: trimmed parallel_offset retry ladder, validated against the
    // reference area.
    let mut ladder_all_empty = true;
    let emit = |loops: Vec<IntContour<i64>>, out: &mut Vec<IntContour<i64>>| {
        for mut l in loops {
            if !ccw {
                l.reverse();
            }
            out.push(l);
        }
    };
    for start in longest_edge_starts(&canonical, RETRY_ROTATIONS) {
        let attempt = trimmed_offset(&ring_to_pline(&canonical, start), eta_mm, arc_tol_mm);
        ladder_all_empty &= attempt.is_empty();
        if (loops_signed_area(&attempt) - ref_area).abs() <= budget {
            emit(attempt, out);
            return;
        }
    }
    let pl = ring_to_pline(&canonical, 0);
    for overshoot in OVERSHOOTS {
        let eps = eta_mm * overshoot;
        let deeper = loop_offset(&pl, eta_mm + eps);
        let mut attempt = Vec::new();
        for l in &deeper {
            attempt.extend(trimmed_flatten(loop_offset(l, -eps), arc_tol_mm));
        }
        if deeper.is_empty() {
            attempt.clear();
        }
        ladder_all_empty &= attempt.is_empty();
        if (loops_signed_area(&attempt) - ref_area).abs() <= budget {
            emit(attempt, out);
            return;
        }
    }

    // Nothing validated. An all-empty ladder on a collapse-plausible ring is
    // a genuine collapse (coarea bound: full collapse forces area ≤
    // perimeter·|η|); otherwise fall back to the complete winding reference.
    if ladder_all_empty && eta_nm < 0.0 && a_ring <= perim * -eta_nm + budget {
        return;
    }
    for p in reference {
        let mut outer = ring_to_contour(&p.outer);
        let mut holes: Vec<IntContour<i64>> = p.holes.iter().map(ring_to_contour).collect();
        if !ccw {
            outer.reverse();
            for h in &mut holes {
                h.reverse();
            }
        }
        out.push(outer);
        out.append(&mut holes);
    }
}

/// Positive-winding region of the raw (untrimmed) offset curve of a CCW
/// loop, offset loop-outward by `eta_mm`. Normalized polys, exact topology,
/// possible small overfill artifacts (module docs).
fn winding_reference(canonical: &Ring, eta_mm: f64, arc_tol_mm: f64) -> Vec<Poly> {
    let pl = ring_to_pline(canonical, 0);
    // catch_unwind: defense in depth against upstream panics on collapsing
    // inputs (issue #79 class).
    let raw = catch_unwind(AssertUnwindSafe(|| {
        create_raw_offset_polyline::<_, _, Polyline<f64>>(&pl, -eta_mm, POS_EQUAL_EPS_MM)
    }));
    let Ok(raw) = raw else {
        return Vec::new();
    };
    let Some(contour) = flatten_to_contour(&raw, arc_tol_mm) else {
        return Vec::new();
    };
    winding_extract(&[contour])
}

/// One trimmed `parallel_offset` attempt on a CCW loop, loop-outward by
/// `eta_mm`, flattened to integer contours. Panics are absorbed (empty).
fn trimmed_offset(pl: &Polyline<f64>, eta_mm: f64, arc_tol_mm: f64) -> Vec<IntContour<i64>> {
    trimmed_flatten(loop_offset(pl, eta_mm), arc_tol_mm)
}

/// `parallel_offset` with loop-outward sign semantics and the #79 panic
/// guard (a panic yields no loops).
fn loop_offset(pl: &Polyline<f64>, eta_mm: f64) -> Vec<Polyline<f64>> {
    catch_unwind(AssertUnwindSafe(|| pl.parallel_offset(-eta_mm))).unwrap_or_default()
}

fn trimmed_flatten(plines: Vec<Polyline<f64>>, arc_tol_mm: f64) -> Vec<IntContour<i64>> {
    plines
        .iter()
        .filter_map(|l| flatten_to_contour(l, arc_tol_mm))
        .collect()
}

/// Flatten a cavalier polyline's arcs and convert to an implicitly closed
/// nm contour. cavalier occasionally returns loops not marked closed even
/// though their source was; every loop is treated as implicitly closed.
/// `arcs_to_approx_lines` only fails on numeric-cast breakdown, which flat
/// PCB geometry never hits.
fn flatten_to_contour(pl: &Polyline<f64>, arc_tol_mm: f64) -> Option<IntContour<i64>> {
    if pl.vertex_count() < 2 {
        return None;
    }
    let flat = pl.arcs_to_approx_lines(arc_tol_mm)?;
    let mut contour: IntContour<i64> = Vec::with_capacity(flat.vertex_count());
    for v in flat.iter_vertexes() {
        let q = IntPoint::new(mm_to_nm(v.x), mm_to_nm(v.y));
        if contour.last() != Some(&q) {
            contour.push(q);
        }
    }
    while contour.len() > 1 && contour.first() == contour.last() {
        contour.pop();
    }
    (contour.len() >= 3).then_some(contour)
}

/// Net signed area (nm²) of a set of integer loops (shoelace sum; holes
/// subtract by orientation).
fn loops_signed_area(loops: &[IntContour<i64>]) -> f64 {
    let mut doubled: i128 = 0;
    for c in loops {
        for (i, a) in c.iter().enumerate() {
            let b = &c[(i + 1) % c.len()];
            doubled += a.x as i128 * b.y as i128 - b.x as i128 * a.y as i128;
        }
    }
    doubled as f64 / 2.0
}

/// Build a closed mm polyline from `ring`, starting at vertex `start`.
fn ring_to_pline(ring: &Ring, start: usize) -> Polyline<f64> {
    let mut pl = Polyline::new_closed();
    for i in 0..ring.len() {
        let p = &ring[(start + i) % ring.len()];
        pl.add(nm_to_mm(p.x), nm_to_mm(p.y), 0.0);
    }
    pl
}

/// Indices of the vertices that start the `n` longest edges of `ring`,
/// longest first. Starting a polyline inside a dense fan of short chords
/// breaks cavalier's slice validation, so attempts begin on long edges.
fn longest_edge_starts(ring: &Ring, n: usize) -> Vec<usize> {
    let mut by_len: Vec<(i128, usize)> = (0..ring.len())
        .map(|i| {
            let a = &ring[i];
            let b = &ring[(i + 1) % ring.len()];
            let dx = (b.x - a.x) as i128;
            let dy = (b.y - a.y) as i128;
            (dx * dx + dy * dy, i)
        })
        .collect();
    by_len.sort_by_key(|&(len2, _)| std::cmp::Reverse(len2));
    by_len.into_iter().take(n).map(|(_, i)| i).collect()
}

/// Total absolute turning of `ring` in radians (Σ |exterior angle|); 2π for
/// a convex ring. Scales the winding-artifact budget.
fn total_abs_turning(ring: &Ring) -> f64 {
    let n = ring.len();
    let mut sum = 0.0;
    for i in 0..n {
        let a = &ring[i];
        let b = &ring[(i + 1) % n];
        let c = &ring[(i + 2) % n];
        let (ux, uy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
        let (vx, vy) = ((c.x - b.x) as f64, (c.y - b.y) as f64);
        let cross = ux * vy - uy * vx;
        let dot = ux * vx + uy * vy;
        sum += cross.atan2(dot).abs();
    }
    sum
}

/// Perimeter of `ring` in nm (f64; exact enough for budget terms).
fn ring_perimeter_nm(ring: &Ring) -> f64 {
    let mut sum = 0.0;
    for (i, a) in ring.iter().enumerate() {
        let b = &ring[(i + 1) % ring.len()];
        sum += ((b.x - a.x) as f64).hypot((b.y - a.y) as f64);
    }
    sum
}

#[inline]
fn nm_to_mm(v: Nm) -> f64 {
    v as f64 / NM_PER_MM as f64
}

#[inline]
fn mm_to_nm(v: f64) -> Nm {
    (v * NM_PER_MM as f64).round() as Nm
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: Nm, y0: Nm, x1: Nm, y1: Nm) -> Poly {
        Poly {
            outer: vec![
                P::new(x0, y0),
                P::new(x1, y0),
                P::new(x1, y1),
                P::new(x0, y1),
            ],
            holes: vec![],
        }
    }

    /// Reverse a CCW ring into a CW hole.
    fn as_hole(mut r: Ring) -> Ring {
        r.reverse();
        r
    }

    const MM: Nm = NM_PER_MM;

    #[test]
    fn nm_mm_round_trip_lossless_at_one_meter() {
        for v in [
            0,
            1,
            -1,
            999_999_999,
            1_000_000_000,
            -1_000_000_000,
            123_456_789,
            -987_654_321,
        ] {
            assert_eq!(mm_to_nm(nm_to_mm(v)), v, "round trip failed for {v}");
        }
    }

    #[test]
    fn union_of_overlapping_squares() {
        let a = rect(0, 0, 10 * MM, 10 * MM);
        let b = rect(5 * MM, 0, 15 * MM, 10 * MM);
        let u = union(&[a], &[b]);
        assert_eq!(u.len(), 1);
        let expect = 150.0 * (MM as f64) * (MM as f64);
        assert!((area_nm2(&u) - expect).abs() < 1.0);
    }

    #[test]
    fn difference_carves_a_hole_with_cw_orientation() {
        let outer = rect(0, 0, 10 * MM, 10 * MM);
        let inner = rect(4 * MM, 4 * MM, 6 * MM, 6 * MM);
        let d = difference(&[outer], &[inner]);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].holes.len(), 1);
        assert!(ring_doubled_area(&d[0].outer) > 0, "outer must be CCW");
        assert!(ring_doubled_area(&d[0].holes[0]) < 0, "hole must be CW");
        let expect = 96.0 * (MM as f64) * (MM as f64);
        assert!((area_nm2(&d) - expect).abs() < 1.0);
    }

    #[test]
    fn xor_of_identical_is_empty() {
        let a = rect(0, 0, 10 * MM, 10 * MM);
        let a2 = a.clone();
        assert!(xor(&[a], &[a2]).is_empty());
    }

    #[test]
    fn intersect_of_disjoint_is_empty() {
        let a = rect(0, 0, MM, MM);
        let b = rect(2 * MM, 0, 3 * MM, MM);
        assert!(intersect(&[a], &[b]).is_empty());
    }

    #[test]
    fn dilate_square_area_matches_round_join_formula() {
        // Dilating an s-square by d: area = s^2 + 4 s d + pi d^2 (round joins).
        let s = 10.0 * MM as f64;
        let d = 2 * MM;
        let g = offset(&[rect(0, 0, 10 * MM, 10 * MM)], d);
        assert_eq!(g.len(), 1);
        let df = d as f64;
        let expect = s * s + 4.0 * s * df + std::f64::consts::PI * df * df;
        let got = area_nm2(&g);
        // Arc flattening inscribes chords, so `got` is slightly below `expect`.
        assert!(got <= expect + 1.0);
        assert!(
            (got - expect).abs() / expect < 1e-3,
            "got {got}, expect {expect}"
        );
    }

    #[test]
    fn erode_square_is_exact() {
        let g = offset(&[rect(0, 0, 10 * MM, 10 * MM)], -2 * MM);
        assert_eq!(g.len(), 1);
        let expect = 36.0 * (MM as f64) * (MM as f64);
        assert!(
            (area_nm2(&g) - expect).abs() < 1e3,
            "erode area {} != {expect}",
            area_nm2(&g)
        );
    }

    #[test]
    fn dilate_shrinks_holes_and_erode_grows_them() {
        let p = Poly {
            outer: rect(0, 0, 10 * MM, 10 * MM).outer,
            holes: vec![as_hole(rect(3 * MM, 3 * MM, 7 * MM, 7 * MM).outer)],
        };
        let a0 = area_nm2(std::slice::from_ref(&p));
        let grown = offset(std::slice::from_ref(&p), MM / 2);
        assert_eq!(grown.len(), 1);
        assert_eq!(grown[0].holes.len(), 1, "hole must survive a 0.5 mm dilate");
        assert!(ring_doubled_area(&grown[0].holes[0]) < 0, "hole must be CW");
        assert!(area_nm2(&grown) > a0);
        let eroded = offset(&[p], -MM / 2);
        assert!(area_nm2(&eroded) < a0);
        assert!(!eroded.is_empty());
    }

    #[test]
    fn dilate_swallows_small_hole() {
        let p = Poly {
            outer: rect(0, 0, 10 * MM, 10 * MM).outer,
            holes: vec![as_hole(rect(4 * MM, 4 * MM, 6 * MM, 6 * MM).outer)],
        };
        let g = offset(&[p], 2 * MM); // hole half-width 1 mm < 2 mm -> gone
        assert_eq!(g.len(), 1);
        assert!(g[0].holes.is_empty(), "2 mm dilate must close a 2 mm hole");
    }

    #[test]
    fn erode_to_full_collapse_is_empty_not_panic() {
        let p = rect(0, 0, 10 * MM, 10 * MM);
        assert!(offset(&[p], -6 * MM).is_empty());
    }

    #[test]
    fn erode_just_past_collapse_is_empty() {
        // Inradius of a 10 mm square is 5 mm; -5.1 mm must yield nothing
        // (this is the regime where the winding rule alone overfills).
        let p = rect(0, 0, 10 * MM, 10 * MM);
        assert!(offset(&[p], -5_100_000).is_empty());
    }

    #[test]
    fn offset_zero_is_identity_normalized() {
        let p = rect(0, 0, 10 * MM, 10 * MM);
        let o = offset(std::slice::from_ref(&p), 0);
        assert_eq!(o.len(), 1);
        assert!((area_nm2(&o) - area_nm2(&[p])).abs() < 1.0);
    }

    #[test]
    fn erode_splits_dumbbell_into_two() {
        // Two 10 mm squares joined by a 1 mm-tall bridge; eroding by 1 mm
        // severs the bridge.
        let left = rect(0, 0, 10 * MM, 10 * MM);
        let right = rect(14 * MM, 0, 24 * MM, 10 * MM);
        let bridge = rect(9 * MM, 4 * MM, 15 * MM, 5 * MM);
        let joined = union(&union(&[left], &[right]), &[bridge]);
        assert_eq!(joined.len(), 1);
        let eroded = offset(&joined, -MM);
        assert_eq!(eroded.len(), 2, "1 mm erode must sever a 1 mm bridge");
    }
}
