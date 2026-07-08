//! CAM-5 — dual-machine splitter: fiber (bulk removal) vs UV (finishing).
//!
//! The dual-laser workflow clears copper clearance with two machines sharing
//! one board frame: a fast **fiber** laser rips out the bulk of the removal
//! region, and a fine **UV** laser does the edge-critical finishing (the
//! isolation contours that kiss the copper, the force-clear passes through
//! sub-min-feature slivers, and a single boundary contour traced at the exact
//! design copper edge). To keep the coarse fiber beam from ever encroaching on
//! a copper edge the fiber territory is pulled back from the removal region by
//! a `guard_mm` margin; the UV machine owns everything inside that guard band.
//!
//! # What each machine gets
//!
//! * **Removal region** — reused verbatim from CAM-1 as
//!   [`ablation::rubout_band`](crate::ablation::rubout_band): the annular strip
//!   between `clearance_mm` and `clearance_mm + band_mm` outside the copper,
//!   i.e. the copper-clearance area that must be ablated.
//!
//! * **Fiber** — the removal region eroded inward by `guard_mm`
//!   ([`geom::offset`](crate::geom::offset) by `-guard`). Its inner boundary
//!   (the one nearest the copper) therefore sits at `clearance_mm + guard_mm`
//!   from the copper edge, so the fiber territory is at least `guard_mm` clear
//!   of any copper boundary by construction.
//!
//!   *Representation:* the fiber machine clears the bulk with its own downstream
//!   fill/hatch pattern, so this stage emits only the **eroded region's ring
//!   polylines** — every outer and hole ring as a closed
//!   [`PathKind::Rubout`]`(0)` element. These rings are the region outline the
//!   fiber must cover, not a finished tool path.
//!
//! * **UV** — the guard band's finishing work, three tagged sets:
//!   1. the final **isolation** contours (the CAM-1 isolation set,
//!      [`PathKind::Isolation`]), which hug the copper edge;
//!   2. all **force-clear** centerlines
//!      ([`force_clear::force_clear`](crate::force_clear::force_clear)) over the
//!      removal region ([`PathKind::ForceClear`]);
//!   3. one **boundary** contour per copper ring traced at offset 0 — the exact
//!      design copper edge ([`PathKind::Boundary`]).
//!
//! # Guard invariant
//!
//! Every fiber element stays at least `guard_mm` from any copper boundary.
//! Because the fiber region is the removal band (inner edge at `clearance_mm`)
//! eroded by `guard_mm`, its nearest approach to copper is `clearance_mm +
//! guard_mm` — comfortably beyond the `guard_mm` floor. The property test in
//! `tests/split_props.rs` verifies the segment-to-segment distance from every
//! fiber element to every copper-boundary edge is `>= guard_mm - `
//! [`GUARD_TOLERANCE_NM`]. The tolerance (2 µm) absorbs arc-flattening chord
//! error on round offset joins plus nm-grid integer rounding; it is far below
//! the ~`clearance_mm` of real slack the construction leaves.

use std::io::Write as _;
use std::path::Path;

use pcb_core::{CamOpts, Layer, NM_PER_MM, Nm, P, PathElem, PathKind, Paths, Poly, Ring};

use crate::ablation::{ablation_paths, rubout_band};
use crate::force_clear::force_clear;
use crate::geom;

/// Slack allowed on the guard invariant, in nm (2 µm): arc-flattening chord
/// error on round offset joins plus nm-grid rounding. Well below the
/// `clearance_mm`-scale margin the construction actually leaves.
pub const GUARD_TOLERANCE_NM: Nm = 2 * pcb_core::NM_PER_UM;

/// The two machine jobs produced by [`split`], both in the shared board frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SplitJobs {
    /// Bulk-removal region outline for the fiber laser: closed
    /// [`PathKind::Rubout`]`(0)` rings of the guard-eroded removal region.
    pub fiber: Paths,
    /// Edge-critical finishing for the UV laser: isolation contours,
    /// force-clear passes, and the exact design-edge boundary.
    pub uv: Paths,
}

/// Split one copper layer's clearance work between the fiber and UV machines.
///
/// See the module docs for the removal region, the fiber representation, the
/// three UV sets, and the guard invariant.
pub fn split(layer: &Layer, opts: &CamOpts) -> SplitJobs {
    let guard_nm = mm_to_nm(opts.guard_mm);

    // Removal region = the CAM-1 rub-out band (copper-clearance area to ablate).
    let removal = rubout_band(layer, opts);

    // ---- Fiber: removal region eroded inward by guard, rings as Rubout(0). --
    let mut fiber = Vec::new();
    for poly in geom::offset(&removal, -guard_nm) {
        push_closed(&mut fiber, PathKind::Rubout(0), poly.outer);
        for hole in poly.holes {
            push_closed(&mut fiber, PathKind::Rubout(0), hole);
        }
    }

    // ---- UV: isolation contours + force-clear + design-edge boundary. -------
    let mut uv = Vec::new();

    // (1) Final isolation contour set (hatch_sets = 0 -> isolation only).
    uv.extend(ablation_paths(layer, opts, 0).elems);

    // (2) All force-clear centerlines over the removal region.
    for pl in force_clear(&removal, opts.min_feature_mm) {
        if pl.pts.len() >= 2 {
            uv.push(PathElem {
                kind: PathKind::ForceClear,
                pts: pl.pts,
                closed: false,
            });
        }
    }

    // (3) One boundary contour per copper ring at the exact design edge
    // (offset 0 normalizes the copper without moving any edge).
    for poly in geom::offset(&layer.polys, 0) {
        push_closed(&mut uv, PathKind::Boundary, poly.outer);
        for hole in poly.holes {
            push_closed(&mut uv, PathKind::Boundary, hole);
        }
    }

    SplitJobs {
        fiber: Paths { elems: fiber },
        uv: Paths { elems: uv },
    }
}

// ---------------------------------------------------------------------------
// Distance helper (used by the guard-invariant property test)
// ---------------------------------------------------------------------------

/// Minimum distance in nm from the polyline `pts` (segments between
/// consecutive vertices, plus the closing segment when `closed`) to any ring
/// edge — outer or hole — of `polys`. Segment-to-segment distance in f64
/// (exact for PCB-scale coordinates). Returns [`f64::INFINITY`] when either
/// side has no segments.
pub fn min_dist_to_polys_nm(pts: &[P], closed: bool, polys: &[Poly]) -> f64 {
    let mut best = f64::INFINITY;
    for (a, b) in polyline_segments(pts, closed) {
        for poly in polys {
            for ring in std::iter::once(&poly.outer).chain(poly.holes.iter()) {
                for (c, d) in ring_edges(ring) {
                    best = best.min(seg_seg_dist2(a, b, c, d));
                }
            }
        }
    }
    best.sqrt()
}

/// Squared distance between segments `(a,b)` and `(c,d)` in nm² (f64). Zero if
/// they intersect; otherwise the least of the four endpoint-to-segment
/// distances.
fn seg_seg_dist2(a: P, b: P, c: P, d: P) -> f64 {
    if segs_intersect(a, b, c, d) {
        return 0.0;
    }
    let mut m = point_seg_dist2(a, c, d);
    m = m.min(point_seg_dist2(b, c, d));
    m = m.min(point_seg_dist2(c, a, b));
    m.min(point_seg_dist2(d, a, b))
}

/// Do segments `(a,b)` and `(c,d)` intersect? Exact `i128` orientation tests
/// (handles the collinear-overlap case too).
fn segs_intersect(a: P, b: P, c: P, d: P) -> bool {
    let o1 = orient(a, b, c);
    let o2 = orient(a, b, d);
    let o3 = orient(c, d, a);
    let o4 = orient(c, d, b);
    if (o1 > 0) != (o2 > 0) && o1 != 0 && o2 != 0 && (o3 > 0) != (o4 > 0) && o3 != 0 && o4 != 0 {
        return true;
    }
    (o1 == 0 && on_seg(a, b, c))
        || (o2 == 0 && on_seg(a, b, d))
        || (o3 == 0 && on_seg(c, d, a))
        || (o4 == 0 && on_seg(c, d, b))
}

/// Sign of the cross product `(b-a) × (c-a)`, exact in `i128`.
fn orient(a: P, b: P, c: P) -> i128 {
    let v = (b.x - a.x) as i128 * (c.y - a.y) as i128 - (b.y - a.y) as i128 * (c.x - a.x) as i128;
    v.signum()
}

/// Is collinear point `p` within the bounding box of segment `(a,b)`?
fn on_seg(a: P, b: P, p: P) -> bool {
    p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x) && p.y >= a.y.min(b.y) && p.y <= a.y.max(b.y)
}

/// Squared distance from point `p` to segment `(a,b)` in nm² (f64).
fn point_seg_dist2(p: P, a: P, b: P) -> f64 {
    let (abx, aby) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let (apx, apy) = ((p.x - a.x) as f64, (p.y - a.y) as f64);
    let len2 = abx * abx + aby * aby;
    let t = if len2 > 0.0 {
        ((apx * abx + apy * aby) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (dx, dy) = (apx - t * abx, apy - t * aby);
    dx * dx + dy * dy
}

fn polyline_segments(pts: &[P], closed: bool) -> impl Iterator<Item = (P, P)> + '_ {
    let n = pts.len();
    let last = if closed { n } else { n.saturating_sub(1) };
    (0..last).map(move |i| (pts[i], pts[(i + 1) % n]))
}

fn ring_edges(ring: &Ring) -> impl Iterator<Item = (P, P)> + '_ {
    ring.iter()
        .enumerate()
        .map(|(i, &a)| (a, ring[(i + 1) % ring.len()]))
}

// ---------------------------------------------------------------------------
// SVG debug dump (dependency-free)
// ---------------------------------------------------------------------------

/// Write a debug SVG of `jobs`: fiber rings in red, UV isolation contours in
/// blue, UV force-clear passes in orange, and the design-edge boundary in
/// green (dashed). Coordinates are emitted in µm with the y-axis flipped so
/// the picture reads the same way up as the board. Fully self-contained.
pub fn debug_svg(jobs: &SplitJobs, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Bounds over every vertex in both sets (fall back to a unit box).
    let all = jobs.fiber.elems.iter().chain(jobs.uv.elems.iter());
    let mut min_x = Nm::MAX;
    let mut min_y = Nm::MAX;
    let mut max_x = Nm::MIN;
    let mut max_y = Nm::MIN;
    for e in all {
        for p in &e.pts {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
    }
    if min_x > max_x {
        min_x = 0;
        min_y = 0;
        max_x = NM_PER_MM;
        max_y = NM_PER_MM;
    }
    let pad = NM_PER_MM / 2; // 0.5 mm frame
    min_x -= pad;
    min_y -= pad;
    max_x += pad;
    max_y += pad;

    // nm -> µm, y flipped into SVG's top-left origin.
    let to_um = |v: Nm| v as f64 / 1_000.0;
    let w = to_um(max_x - min_x);
    let h = to_um(max_y - min_y);
    let sx = move |x: Nm| to_um(x - min_x);
    let sy = move |y: Nm| to_um(max_y - y);
    let stroke = to_um(NM_PER_MM / 40); // ~25 µm strokes

    let mut s = String::new();
    s.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w:.1} {h:.1}\" \
         width=\"{w:.1}\" height=\"{h:.1}\">\n"
    ));
    s.push_str("<rect x=\"0\" y=\"0\" width=\"100%\" height=\"100%\" fill=\"#ffffff\"/>\n");

    for e in &jobs.fiber.elems {
        push_elem_svg(&mut s, e, &sx, &sy, "#d62728", stroke, false);
    }
    for e in &jobs.uv.elems {
        let (color, dashed) = match e.kind {
            PathKind::Boundary => ("#2ca02c", true),
            PathKind::ForceClear => ("#ff7f0e", false),
            _ => ("#1f77b4", false),
        };
        push_elem_svg(&mut s, e, &sx, &sy, color, stroke, dashed);
    }

    // Legend.
    s.push_str(&format!(
        "<g font-family=\"sans-serif\" font-size=\"{fs:.1}\">\n\
         <text x=\"2\" y=\"{y0:.1}\" fill=\"#d62728\">fiber (Rubout)</text>\n\
         <text x=\"2\" y=\"{y1:.1}\" fill=\"#1f77b4\">uv isolation</text>\n\
         <text x=\"2\" y=\"{y2:.1}\" fill=\"#ff7f0e\">uv force-clear</text>\n\
         <text x=\"2\" y=\"{y3:.1}\" fill=\"#2ca02c\">uv boundary</text>\n</g>\n",
        fs = h * 0.03,
        y0 = h * 0.04,
        y1 = h * 0.08,
        y2 = h * 0.12,
        y3 = h * 0.16,
    ));
    s.push_str("</svg>\n");

    let mut f = std::fs::File::create(path)?;
    f.write_all(s.as_bytes())
}

fn push_elem_svg(
    s: &mut String,
    e: &PathElem,
    sx: &impl Fn(Nm) -> f64,
    sy: &impl Fn(Nm) -> f64,
    color: &str,
    stroke: f64,
    dashed: bool,
) {
    if e.pts.len() < 2 {
        return;
    }
    let mut pts = String::new();
    for p in &e.pts {
        pts.push_str(&format!("{:.1},{:.1} ", sx(p.x), sy(p.y)));
    }
    let tag = if e.closed { "polygon" } else { "polyline" };
    let dash = if dashed {
        " stroke-dasharray=\"40,20\""
    } else {
        ""
    };
    s.push_str(&format!(
        "<{tag} points=\"{}\" fill=\"none\" stroke=\"{color}\" \
         stroke-width=\"{stroke:.2}\"{dash}/>\n",
        pts.trim_end()
    ));
}

// ---------------------------------------------------------------------------

fn push_closed(elems: &mut Vec<PathElem>, kind: PathKind, pts: Ring) {
    if pts.len() >= 3 {
        elems.push(PathElem {
            kind,
            pts,
            closed: true,
        });
    }
}

#[inline]
fn mm_to_nm(v: f64) -> Nm {
    (v * NM_PER_MM as f64).round() as Nm
}

#[cfg(test)]
mod tests {
    use super::*;

    const MM: Nm = NM_PER_MM;

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

    fn copper_boundary(layer: &Layer) -> Vec<Poly> {
        geom::offset(&layer.polys, 0)
    }

    #[test]
    fn square_split_populates_both_machines() {
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 10.0, 10.0)],
        };
        let jobs = split(&layer, &CamOpts::default());

        // Fiber: closed Rubout(0) rings of the eroded band (outer + hole).
        assert!(!jobs.fiber.elems.is_empty());
        for e in &jobs.fiber.elems {
            assert_eq!(e.kind, PathKind::Rubout(0));
            assert!(e.closed);
            assert!(e.pts.len() >= 3);
        }

        // UV: default has 2 isolation contours + 1 boundary contour, no necks.
        assert!(
            jobs.uv
                .elems
                .iter()
                .any(|e| matches!(e.kind, PathKind::Isolation(_)))
        );
        assert!(jobs.uv.elems.iter().any(|e| e.kind == PathKind::Boundary));
    }

    #[test]
    fn boundary_contour_traces_the_exact_copper_edge() {
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 10.0, 10.0)],
        };
        let jobs = split(&layer, &CamOpts::default());
        let boundary: Vec<&PathElem> = jobs
            .uv
            .elems
            .iter()
            .filter(|e| e.kind == PathKind::Boundary)
            .collect();
        assert_eq!(boundary.len(), 1, "one boundary ring for a plain square");
        // Its vertices must sit on the copper edge (distance 0).
        let d = min_dist_to_polys_nm(&boundary[0].pts, true, &layer.polys);
        assert!(d < 1.0, "boundary distance {d} nm should be ~0");
    }

    #[test]
    fn fiber_stays_clear_of_copper_by_the_guard() {
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 8.0, 5.0)],
        };
        let opts = CamOpts::default();
        let jobs = split(&layer, &opts);
        let copper = copper_boundary(&layer);
        let guard_nm = mm_to_nm(opts.guard_mm) as f64;
        for e in &jobs.fiber.elems {
            let d = min_dist_to_polys_nm(&e.pts, e.closed, &copper);
            assert!(
                d >= guard_nm - GUARD_TOLERANCE_NM as f64,
                "fiber element {d} nm < guard {guard_nm} nm"
            );
        }
    }

    #[test]
    fn empty_layer_yields_empty_jobs() {
        let jobs = split(&Layer::default(), &CamOpts::default());
        assert!(jobs.fiber.elems.is_empty());
        assert!(jobs.uv.elems.is_empty());
    }

    #[test]
    fn seg_seg_distance_matches_hand_computation() {
        // Two parallel horizontal unit segments 3 mm apart.
        let d2 = seg_seg_dist2(
            P::from_mm(0.0, 0.0),
            P::from_mm(1.0, 0.0),
            P::from_mm(0.0, 3.0),
            P::from_mm(1.0, 3.0),
        );
        assert!((d2.sqrt() - 3.0 * MM as f64).abs() < 1.0);
        // Crossing segments -> zero.
        let z = seg_seg_dist2(
            P::from_mm(0.0, 0.0),
            P::from_mm(2.0, 2.0),
            P::from_mm(0.0, 2.0),
            P::from_mm(2.0, 0.0),
        );
        assert_eq!(z, 0.0);
    }

    #[test]
    fn debug_svg_writes_a_file() {
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 6.0, 4.0)],
        };
        let jobs = split(&layer, &CamOpts::default());
        let dir = std::env::temp_dir().join("cam_split_svg_test");
        let path = dir.join("split-debug.svg");
        debug_svg(&jobs, &path).expect("write svg");
        let body = std::fs::read_to_string(&path).expect("read svg");
        assert!(body.starts_with("<svg"));
        assert!(body.contains("</svg>"));
    }
}
