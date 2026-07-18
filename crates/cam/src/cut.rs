//! CAM-10 — board-outline through-cut (depaneling) with a lowering focal
//! plane.
//!
//! The rest of the pipeline ablates, drills, masks, and legends on the board
//! *in place*; this stage frees the finished board from the FR4 stock. Two
//! parts:
//!
//! * [`cut_paths`] — the geometry. The board region (from `noncopper`'s
//!   [`board_region_from_outline`](crate::noncopper::board_region_from_outline))
//!   is offset onto the **waste side** by half the kerf (one
//!   [`geom::offset`](crate::geom::offset) of `+kerf/2`: the winding
//!   convention grows the outer perimeter outward and shrinks interior
//!   cutouts inward, both correct in a single call), then each closed ring is
//!   broken by **holding tabs** so nothing shifts or drops mid-cut. Interior
//!   cutout rings are emitted before the perimeter, and the whole job is meant
//!   to run **last** (a cut destroys registration and rigidity). Every element
//!   is [`PathKind::Cut`].
//!
//! * [`schedule`] — the physics. A galvo's F-theta lens has a fixed focal
//!   plane and a depth of focus far shallower than a 1.6 mm board, so a
//!   fixed-focus cut defocuses at the floor and chars instead of deepening.
//!   [`schedule`] groups passes into [`CutStep`]s that each remove at most
//!   `z_step_mm` (within the usable depth of focus) and, between steps, emits
//!   an explicit focal-plane drop so focus tracks the descending cut floor.
//!   Board thickness comes from the `.gbrjob` (ING-5), never assumed.
//!
//! `kerf_mm`, `mm_per_pass`, and `z_step_mm` are machine facts measured on
//! scrap FR4 (see docs/plans/cam-10-board-cut.md); [`CutOpts::default`] ships
//! conservative placeholders that the CLI flags as un-calibrated.

use pcb_core::{
    CutOpts, CutSchedule, CutStep, NM_PER_MM, Nm, P, PathElem, PathKind, Paths, Poly, Ring,
};

use crate::geom;

/// A vertex whose turn angle exceeds this is a "corner"; tabs are nudged so
/// their solid span never straddles one (a tab across a sharp corner registers
/// unpredictably). 30°. A finely-flattened circle turns far less per vertex,
/// so it has no corners and tabs may sit anywhere.
const CORNER_TURN_RAD: f64 = std::f64::consts::PI / 6.0;

/// Cutouts whose smaller bounding-box dimension is below this get a
/// slug-may-jam warning in the schedule. mm.
pub const SLUG_WARN_MM: f64 = 10.0;

/// Kerf compensation offsets the board region through
/// [`geom::offset`](crate::geom::offset), which can leave sub-µm² sliver
/// rings when a boundary is finely tessellated (e.g. a Gerber-polygonized
/// circular cutout — see the winding-artifact note in `cam::geom`). Any ring
/// below this absolute area is such an artifact, not a real cut contour, and
/// is dropped. Far below any cuttable cutout (a Ø0.36 mm hole is ≈ 1e-1 mm²)
/// yet ~500× the observed artifact scale (~2e-7 mm²). mm².
const MIN_RING_AREA_MM2: f64 = 1e-4;

/// Kerf-compensated, tabbed cut geometry for a board region.
///
/// `board` is the filled board region (outer rings CCW, cutouts as CW holes),
/// as produced by
/// [`board_region_from_outline`](crate::noncopper::board_region_from_outline).
/// Returns [`PathKind::Cut`] elements: interior cutout rings first (each broken
/// into `opts.tab_count` open segments by holding tabs), then perimeter rings.
/// A ring too small to host even one tab is emitted as a single closed cut.
pub fn cut_paths(board: &[Poly], opts: &CutOpts) -> Paths {
    let half_kerf_nm = (opts.kerf_mm * 0.5 * NM_PER_MM as f64).round() as Nm;
    let compensated = geom::offset(board, half_kerf_nm);
    let gap_w_nm = ((opts.tab_mm + opts.kerf_mm) * NM_PER_MM as f64).round() as Nm;
    let floor = MIN_RING_AREA_MM2 * NM_PER_MM as f64 * NM_PER_MM as f64;
    let real = |ring: &Ring| ring_abs_area_nm2(ring) >= floor;

    let mut elems = Vec::new();
    // Interior cutouts first (maximum stiffness held as long as possible)...
    for poly in &compensated {
        if !real(&poly.outer) {
            continue; // spurious sliver poly — skip it and its holes
        }
        for hole in &poly.holes {
            if real(hole) {
                elems.extend(tab_ring(hole, gap_w_nm, opts.tab_count));
            }
        }
    }
    // ...perimeter last.
    for poly in &compensated {
        if real(&poly.outer) {
            elems.extend(tab_ring(&poly.outer, gap_w_nm, opts.tab_count));
        }
    }
    Paths { elems }
}

/// Arc-length window over which corner turning is measured, nm (40 µm).
/// `geom::offset` on a finely tessellated boundary (e.g. a Gerber-polygonized
/// circle) returns a ring with sub-µm zigzag noise whose *per-vertex* turns
/// can exceed the corner threshold. Measuring the turn between directions
/// averaged over ±40 µm cancels that noise, so only genuine, sustained corners
/// register — while the emitted cut path stays the exact kerf offset.
const CORNER_WINDOW_NM: f64 = 40_000.0;

/// Break one closed `ring` into `tab_count` open [`PathKind::Cut`] segments
/// separated by holding tabs of arc-length `gap_w_nm` (= `tab_mm + kerf_mm`,
/// leaving ≈ `tab_mm` of solid material once the kerf is accounted for).
///
/// Tabs are spread evenly by arc length and nudged so no tab's span straddles
/// a corner. Returns exactly one closed element (no tabs) when `tab_count` is
/// 0 or the ring is too short to host a single tab.
pub fn tab_ring(ring: &Ring, gap_w_nm: Nm, tab_count: u32) -> Vec<PathElem> {
    if ring.len() < 3 {
        return Vec::new();
    }
    let (cum, total) = ring_cum_lengths(ring);
    let w = gap_w_nm as f64;
    if tab_count == 0 || w <= 0.0 || w >= total {
        return vec![closed_cut(ring)];
    }
    let h = w / 2.0;
    let corners = corner_positions(ring, &cum, total);

    // Evenly spaced tab centers (offset half a slot so we never open on the
    // ring's start seam), each nudged clear of corners.
    let t = tab_count as usize;
    let mut centers: Vec<f64> = Vec::new();
    for k in 0..t {
        let target = (k as f64 + 0.5) * total / t as f64;
        if let Some(c) = nudge(target, h, &corners, total) {
            centers.push(c.rem_euclid(total));
        }
    }
    centers.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // Drop tabs whose windows would overlap a kept one.
    let mut kept: Vec<f64> = Vec::new();
    for &c in &centers {
        if kept.iter().all(|&k| circ_dist(k, c, total) >= w - 1.0) {
            kept.push(c);
        }
    }
    if kept.is_empty() {
        match longest_free_center(&corners, total, w) {
            Some(c) => kept.push(c),
            None => return vec![closed_cut(ring)],
        }
    }
    kept.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // One open segment per gap: the arc from this tab's far edge to the next
    // tab's near edge. m gaps around a closed ring leave m arcs.
    let m = kept.len();
    let mut out = Vec::with_capacity(m);
    for j in 0..m {
        let s0 = kept[j] + h;
        let next = if j + 1 < m {
            kept[j + 1]
        } else {
            kept[0] + total
        };
        let s1 = next - h;
        // Skip a vanishing or inverted gap: at minimum tab spacing `s1` can
        // land at/just below `s0`, and subarc would emit a reversed 2-point
        // sliver "cut" (LR-35). 1 µm is far below any real cut segment.
        if s1 - s0 < 1000.0 {
            continue;
        }
        let pts = subarc(ring, &cum, total, s0, s1);
        if pts.len() >= 2 {
            out.push(PathElem {
                kind: PathKind::Cut,
                pts,
                closed: false,
            });
        }
    }
    out
}

/// The focus schedule for cutting through a board of `thickness_nm`.
///
/// Passes are grouped into [`CutStep`]s each removing at most `z_step_mm`
/// (`passes_per_step = floor(z_step / mm_per_pass)`, min 1); after each step
/// the focal plane drops by exactly what that step removed, so focus follows
/// the cut floor. The final step reaches `thickness + overcut` and its
/// `focus_drop_mm` is 0. Commanded depth Σ(passes·mm_per_pass) is ≥ the target
/// and less than one pass over it.
pub fn schedule(opts: &CutOpts, thickness_nm: Nm) -> CutSchedule {
    let thickness_mm = thickness_nm as f64 / NM_PER_MM as f64;
    let total_depth_mm = thickness_mm + opts.overcut_mm;
    let mpp = opts.mm_per_pass.max(1e-6);
    let passes_per_step = ((opts.z_step_mm / mpp).floor() as i64).max(1) as u32;
    let step_depth = passes_per_step as f64 * mpp;

    let mut steps = Vec::new();
    let mut full = 0u32;
    // Backstop against a pathological (non-positive) step size.
    while (full as usize) < 100_000 {
        // Recompute `done` from the step count each iteration rather than
        // accumulating, so drift never shifts the step boundary.
        let done = full as f64 * step_depth;
        let remaining = total_depth_mm - done;
        if remaining <= 1e-9 {
            break;
        }
        if step_depth >= remaining - 1e-9 {
            // The −1e-6 tolerance keeps fp noise on an exact-multiple depth
            // (e.g. 2.0000000000000018) from rounding up to a spurious pass.
            let passes = ((remaining / mpp) - 1e-6).ceil().max(1.0) as u32;
            steps.push(CutStep {
                passes,
                focus_drop_mm: 0.0,
            });
            break;
        }
        steps.push(CutStep {
            passes: passes_per_step,
            focus_drop_mm: step_depth,
        });
        full += 1;
    }
    if steps.is_empty() {
        steps.push(CutStep {
            passes: 1,
            focus_drop_mm: 0.0,
        });
    }
    CutSchedule {
        steps,
        total_depth_mm,
    }
}

/// Smaller bounding-box dimension of a ring, mm — used to flag cutouts whose
/// slug could jam the kerf.
pub fn ring_min_dim_mm(ring: &Ring) -> f64 {
    if ring.is_empty() {
        return 0.0;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (Nm::MAX, Nm::MAX, Nm::MIN, Nm::MIN);
    for p in ring {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    ((x1 - x0).min(y1 - y0)) as f64 / NM_PER_MM as f64
}

/// Centroid of a ring in mm (vertex average) — for labeling cutout warnings.
pub fn ring_centroid_mm(ring: &Ring) -> (f64, f64) {
    let n = ring.len().max(1) as f64;
    let sx: f64 = ring.iter().map(|p| p.x as f64).sum();
    let sy: f64 = ring.iter().map(|p| p.y as f64).sum();
    (sx / n / NM_PER_MM as f64, sy / n / NM_PER_MM as f64)
}

// ---------------------------------------------------------------------------
// Ring arc-length helpers
// ---------------------------------------------------------------------------

/// Cumulative arc length at each vertex (`cum[0] == 0`) and the total
/// perimeter (including the closing edge back to vertex 0).
fn ring_cum_lengths(ring: &Ring) -> (Vec<f64>, f64) {
    let n = ring.len();
    let mut cum = Vec::with_capacity(n);
    let mut acc = 0.0;
    for i in 0..n {
        cum.push(acc);
        acc += dist(ring[i], ring[(i + 1) % n]);
    }
    (cum, acc)
}

/// Arc positions (nm) of the ring's corner vertices: those whose turn angle,
/// measured between directions averaged over a ±[`CORNER_WINDOW_NM`] arc-length
/// window, exceeds [`CORNER_TURN_RAD`]. The window cancels sub-µm offset noise
/// so only genuine corners register (a smooth circle has none). A real corner
/// flags a short band of nearby vertices, which is exactly the region tabs must
/// avoid.
fn corner_positions(ring: &Ring, cum: &[f64], total: f64) -> Vec<f64> {
    let n = ring.len();
    // Shrink the window on tiny rings so it never spans the whole loop.
    let win = CORNER_WINDOW_NM.min(total / 8.0);
    if win <= 0.0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..n {
        let b = ring[i];
        let behind = point_at(ring, cum, total, (cum[i] - win).rem_euclid(total));
        let ahead = point_at(ring, cum, total, (cum[i] + win).rem_euclid(total));
        let (ux, uy) = ((b.x - behind.x) as f64, (b.y - behind.y) as f64);
        let (vx, vy) = ((ahead.x - b.x) as f64, (ahead.y - b.y) as f64);
        let cross = ux * vy - uy * vx;
        let dot = ux * vx + uy * vy;
        if cross.atan2(dot).abs() > CORNER_TURN_RAD {
            out.push(cum[i]);
        }
    }
    out
}

/// The point on the ring at arc position `s` (clamped into `[0, total]`),
/// rounded to the nm grid.
fn point_at(ring: &Ring, cum: &[f64], total: f64, s: f64) -> P {
    let n = ring.len();
    let s = s.clamp(0.0, total);
    for i in 0..n {
        let start = cum[i];
        let end = if i + 1 < n { cum[i + 1] } else { total };
        if s <= end + 1e-6 {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            let seg = end - start;
            let t = if seg > 1e-9 {
                ((s - start) / seg).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let x = a.x as f64 + (b.x - a.x) as f64 * t;
            let y = a.y as f64 + (b.y - a.y) as f64 * t;
            return P::new(x.round() as Nm, y.round() as Nm);
        }
    }
    ring[0]
}

/// Extract the open sub-arc of the ring from arc position `s0` forward to
/// `s1` (`s1 - s0` in `(0, total]`), with exact interpolated endpoints and the
/// interior vertices in between.
fn subarc(ring: &Ring, cum: &[f64], total: f64, s0: f64, s1: f64) -> Vec<P> {
    let len = s1 - s0;
    let start = point_at(ring, cum, total, s0.rem_euclid(total));
    let end = point_at(ring, cum, total, s1.rem_euclid(total));
    let mut pts = vec![start];
    let mut mids: Vec<(f64, P)> = Vec::new();
    for (i, &c) in cum.iter().enumerate() {
        let rel = (c - s0).rem_euclid(total);
        if rel > 1.0 && rel < len - 1.0 {
            mids.push((rel, ring[i]));
        }
    }
    mids.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    for (_, p) in mids {
        if *pts.last().unwrap() != p {
            pts.push(p);
        }
    }
    if *pts.last().unwrap() != end {
        pts.push(end);
    }
    pts
}

/// Nearest arc position to `target` whose ±`h` window clears every corner by
/// at least `h`. `None` if corners are so dense no such position exists.
fn nudge(target: f64, h: f64, corners: &[f64], total: f64) -> Option<f64> {
    if corners.is_empty() {
        return Some(target);
    }
    let ok = |c: f64| corners.iter().all(|&k| circ_dist(k, c, total) >= h - 1.0);
    if ok(target) {
        return Some(target);
    }
    let mut best: Option<f64> = None;
    let mut best_d = f64::INFINITY;
    for &k in corners {
        for cand in [k + h, k - h] {
            let cc = cand.rem_euclid(total);
            if ok(cc) {
                let d = circ_dist(cc, target, total);
                if d < best_d {
                    best_d = d;
                    best = Some(cc);
                }
            }
        }
    }
    best
}

/// Center of the longest corner-free span, if it can host a full `w`-wide tab.
/// Used only as a last resort when the even placement placed nothing.
fn longest_free_center(corners: &[f64], total: f64, w: f64) -> Option<f64> {
    if corners.is_empty() {
        return Some(0.0);
    }
    let mut sorted = corners.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sorted.len();
    let mut best: Option<(f64, f64)> = None; // (span_len, center)
    for i in 0..n {
        let a = sorted[i];
        let b = if i + 1 < n {
            sorted[i + 1]
        } else {
            sorted[0] + total
        };
        let span = b - a;
        let center = ((a + b) / 2.0).rem_euclid(total);
        if best.is_none_or(|(bl, _)| span > bl) {
            best = Some((span, center));
        }
    }
    best.and_then(|(span, center)| (span >= w).then_some(center))
}

fn closed_cut(ring: &Ring) -> PathElem {
    PathElem {
        kind: PathKind::Cut,
        pts: ring.clone(),
        closed: true,
    }
}

#[inline]
fn dist(a: P, b: P) -> f64 {
    ((b.x - a.x) as f64).hypot((b.y - a.y) as f64)
}

/// Absolute shoelace area of a ring in nm² (exact `i128`).
fn ring_abs_area_nm2(ring: &Ring) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut doubled: i128 = 0;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        doubled += a.x as i128 * b.y as i128 - b.x as i128 * a.y as i128;
    }
    (doubled as f64 / 2.0).abs()
}

/// Shorter of the two arc distances between positions `a` and `b` on a ring of
/// length `total`.
#[inline]
fn circ_dist(a: f64, b: f64, total: f64) -> f64 {
    let d = (a - b).rem_euclid(total);
    d.min(total - d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::{Machine, NM_PER_MM};

    const MM: Nm = NM_PER_MM;

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

    fn board_with_cutout() -> Vec<Poly> {
        // 40 x 30 board with a 8 x 8 square cutout.
        let mut hole = rect(16 * MM, 11 * MM, 24 * MM, 19 * MM).outer;
        hole.reverse(); // CW cutout
        vec![Poly {
            outer: rect(0, 0, 40 * MM, 30 * MM).outer,
            holes: vec![hole],
        }]
    }

    fn ring_len(pts: &[P], closed: bool) -> f64 {
        let n = pts.len();
        let mut s = 0.0;
        for i in 0..n.saturating_sub(1) {
            s += dist(pts[i], pts[i + 1]);
        }
        if closed && n > 1 {
            s += dist(pts[n - 1], pts[0]);
        }
        s
    }

    #[test]
    fn kerf_compensation_offsets_onto_the_waste_side() {
        let board = board_with_cutout();
        let opts = CutOpts::default();
        let paths = cut_paths(&board, &opts);
        assert!(!paths.elems.is_empty());
        let half_kerf = opts.kerf_mm * 0.5 * MM as f64;
        // Every emitted cut vertex sits half a kerf from the board boundary.
        for e in &paths.elems {
            let d = crate::split::min_dist_to_polys_nm(&e.pts, e.closed, &board);
            assert!(
                (d - half_kerf).abs() <= 1_000.0, // 1 µm
                "cut element {d} nm from board boundary, want {half_kerf} nm"
            );
        }
    }

    #[test]
    fn tabs_partition_the_ring_exactly() {
        // A big square ring: four long edges, four 90° corners.
        let ring = rect(0, 0, 40 * MM, 30 * MM).outer;
        let w = (0.5 + 0.05) * MM as f64; // tab_mm + kerf_mm
        let gap_w = w.round() as Nm;
        let tabs = 4;
        let segs = tab_ring(&ring, gap_w, tabs);
        assert_eq!(segs.len(), tabs as usize, "one open segment per tab gap");
        assert!(segs.iter().all(|e| e.kind == PathKind::Cut && !e.closed));

        let total: f64 = {
            let (_, t) = ring_cum_lengths(&ring);
            t
        };
        let cut_len: f64 = segs.iter().map(|e| ring_len(&e.pts, false)).sum();
        let gap_len = total - cut_len;
        // Segments + gaps close to the full perimeter, and the removed length
        // is exactly tab_count * (tab + kerf).
        assert!(
            (gap_len - tabs as f64 * w).abs() <= 4_000.0,
            "gap total {gap_len}"
        );

        // Each individual gap (straight-edge jump between consecutive segments)
        // is one tab width.
        for j in 0..segs.len() {
            let end = *segs[j].pts.last().unwrap();
            let start = segs[(j + 1) % segs.len()].pts[0];
            let g = dist(end, start);
            assert!((g - w).abs() <= 1_000.0, "gap {j} is {g} nm, want {w}");
        }
    }

    #[test]
    fn tabs_avoid_sharp_corners() {
        let ring = rect(0, 0, 40 * MM, 30 * MM).outer;
        let (cum, total) = ring_cum_lengths(&ring);
        let corners = corner_positions(&ring, &cum, total);
        assert_eq!(corners.len(), 4, "a rectangle has four corners");
        let w = (0.5 + 0.05) * MM as f64;
        let gap_w = w.round() as Nm;
        let segs = tab_ring(&ring, gap_w, 4);
        // No segment endpoint (i.e. no tab edge) lands within half a tab of a
        // corner: the tab's solid span never straddles a corner.
        for e in &segs {
            for endpoint in [e.pts[0], *e.pts.last().unwrap()] {
                let s = nearest_arc_pos(&ring, &cum, total, endpoint);
                let near = corners
                    .iter()
                    .map(|&k| circ_dist(k, s, total))
                    .fold(f64::INFINITY, f64::min);
                assert!(
                    near >= w / 2.0 - 2_000.0,
                    "tab edge {near} nm from a corner"
                );
            }
        }
    }

    /// Arc position of the ring point nearest `p` (helper for the corner test).
    fn nearest_arc_pos(ring: &Ring, cum: &[f64], total: f64, p: P) -> f64 {
        let n = ring.len();
        let mut best = (f64::INFINITY, 0.0);
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            let seg = dist(a, b);
            let t = if seg > 0.0 {
                (((p.x - a.x) as f64 * (b.x - a.x) as f64
                    + (p.y - a.y) as f64 * (b.y - a.y) as f64)
                    / (seg * seg))
                    .clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (px, py) = (
                a.x as f64 + (b.x - a.x) as f64 * t,
                a.y as f64 + (b.y - a.y) as f64 * t,
            );
            let d = (px - p.x as f64).hypot(py - p.y as f64);
            if d < best.0 {
                best = (d, (cum[i] + seg * t).rem_euclid(total));
            }
        }
        best.1
    }

    #[test]
    fn circular_cutout_gets_full_tab_count() {
        // Fine polygonal circle (no corners): tabs may sit anywhere.
        let cx = 20.0 * MM as f64;
        let cy = 15.0 * MM as f64;
        let r = 6.0 * MM as f64;
        let n = 200;
        let ring: Ring = (0..n)
            .map(|i| {
                let a = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
                P::new(
                    (cx + r * a.cos()).round() as Nm,
                    (cy + r * a.sin()).round() as Nm,
                )
            })
            .collect();
        let w = (0.5 + 0.05) * MM as f64;
        let segs = tab_ring(&ring, w.round() as Nm, 4);
        assert_eq!(segs.len(), 4, "circle takes all four tabs");
        let (_, total) = ring_cum_lengths(&ring);
        let cut_len: f64 = segs.iter().map(|e| ring_len(&e.pts, false)).sum();
        assert!((total - cut_len - 4.0 * w).abs() <= 8_000.0);
    }

    #[test]
    fn interior_cutouts_are_cut_before_the_perimeter() {
        let board = board_with_cutout();
        let opts = CutOpts::default();
        let paths = cut_paths(&board, &opts);
        // Cutout is centered at (20,15); perimeter spans the whole 40x30. The
        // first tab_count segments must be the cutout (near its centroid).
        let tabs = opts.tab_count as usize;
        let cutout_centroid = (20.0, 15.0);
        for (i, e) in paths.elems.iter().enumerate() {
            let (gx, gy) = ring_centroid_mm(&e.pts);
            let near_cutout = (gx - cutout_centroid.0).hypot(gy - cutout_centroid.1) < 8.0;
            if i < tabs {
                assert!(near_cutout, "element {i} should be the interior cutout");
            } else {
                assert!(!near_cutout, "element {i} should be the perimeter");
            }
        }
        assert!(paths.elems.iter().all(|e| e.kind == PathKind::Cut));
    }

    #[test]
    fn schedule_sums_to_thickness_plus_overcut_with_bounded_drops() {
        let opts = CutOpts::default(); // mpp 0.05, z_step 0.2, overcut 0.1
        let thickness = (1.6 * MM as f64) as Nm;
        let sched = schedule(&opts, thickness);
        assert!((sched.total_depth_mm - 1.7).abs() < 1e-9);

        // 8 full steps of 4 passes (1.6 mm) + a final 2-pass step (0.1 mm).
        assert_eq!(sched.steps.len(), 9);
        assert_eq!(sched.steps[0].passes, 4);
        assert!((sched.steps[0].focus_drop_mm - 0.2).abs() < 1e-9);
        let last = sched.steps.last().unwrap();
        assert_eq!(last.passes, 2);
        assert_eq!(last.focus_drop_mm, 0.0);

        // Commanded depth ≥ target, and under one pass over it.
        let commanded: f64 = sched
            .steps
            .iter()
            .map(|s| s.passes as f64 * opts.mm_per_pass)
            .sum();
        assert!(commanded >= sched.total_depth_mm - 1e-9);
        assert!(commanded < sched.total_depth_mm + opts.mm_per_pass);

        // Every drop within the depth of focus; final drop zero.
        assert!(
            sched
                .steps
                .iter()
                .all(|s| s.focus_drop_mm <= opts.z_step_mm + 1e-9)
        );

        // Σ(drops) == commanded depth − final step's removal.
        let drops: f64 = sched.steps.iter().map(|s| s.focus_drop_mm).sum();
        let last_removal = last.passes as f64 * opts.mm_per_pass;
        assert!((drops - (commanded - last_removal)).abs() < 1e-9);
    }

    #[test]
    fn thin_board_is_a_single_final_step() {
        let opts = CutOpts {
            overcut_mm: 0.0,
            ..CutOpts::default()
        };
        // Thickness under one step's depth (0.2 mm): one final step, no drop.
        let sched = schedule(&opts, (0.1 * MM as f64) as Nm);
        assert_eq!(sched.steps.len(), 1);
        assert_eq!(sched.steps[0].focus_drop_mm, 0.0);
        assert_eq!(sched.steps[0].passes, 2); // ceil(0.1 / 0.05)
    }

    #[test]
    fn uv_machine_selectable() {
        let opts = CutOpts {
            machine: Machine::Uv,
            ..CutOpts::default()
        };
        assert_eq!(opts.machine, Machine::Uv);
        // Geometry does not depend on the machine field.
        let a = cut_paths(&board_with_cutout(), &opts);
        let b = cut_paths(&board_with_cutout(), &CutOpts::default());
        assert_eq!(a.elems.len(), b.elems.len());
    }

    #[test]
    fn small_cutout_is_flagged() {
        let board = board_with_cutout();
        let hole = &board[0].holes[0];
        assert!(ring_min_dim_mm(hole) < SLUG_WARN_MM, "8 mm cutout warns");
        assert!(
            ring_min_dim_mm(&board[0].outer) > SLUG_WARN_MM,
            "40x30 does not"
        );
    }
}
