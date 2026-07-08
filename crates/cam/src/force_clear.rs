//! CAM-2 — sliver force-clear.
//!
//! Copper regions can contain *necks*: strips thinner than the machine's
//! minimum reliably-clearable feature (`CamOpts::min_feature_mm`). The rubout
//! hatch cannot be trusted inside them, so each one gets a dedicated
//! centerline pass ([`pcb_core::PathKind::ForceClear`]).
//!
//! # Detection: morphological opening residue
//!
//! The opening of the region by a disk of diameter `min_feature` is
//! `offset(offset(region, -min/2), +min/2)` — exactly the sub-region
//! coverable by a `min_feature`-diameter disk sliding inside the region.
//! [`geom::offset`] wraps `cavalier_contours` with the collapse guard
//! documented in `geom`'s module docs: an erode that fully collapses a
//! feature (any strip thinner than `min_feature`) returns empty instead of
//! panicking, so the opening simply drops such strips. The *residue*
//! `difference(region, opening)` is therefore the union of everything
//! locally thinner than `min_feature`.
//!
//! The residue also contains benign artifacts that must not trigger passes:
//!
//! * **corner roundings** — opening rounds every convex corner (radius
//!   `min/2`), leaving a sliver of diameter `(min/2)·√2 ≈ 0.71·min` and area
//!   `(1 − π/4)·(min/2)² ≈ 0.054·min²` per right-angle corner;
//! * **boolean/flattening crumbs** — nm-scale slivers along edges where the
//!   dilate re-traces the original boundary.
//!
//! Components are kept only if their area is at least `(min/10)²` (drops the
//! crumbs) **and** their diameter (farthest vertex pair) exceeds
//! `min_feature` (drops the corner roundings, whose diameter is `0.71·min`).
//! Documented limitation: a genuine neck *shorter* than `min_feature` is
//! indistinguishable from a corner rounding by this filter and is not
//! flagged — such a feature is smaller than the process can resolve anyway.
//!
//! # Centerline: principal-axis maximal chord
//!
//! For each surviving component the pass is the maximal chord through the
//! component's area centroid along its principal axis (eigenvector of the
//! edge-length-weighted covariance of the boundary), clipped to the
//! component and inset slightly at both ends so the polyline stays strictly
//! inside. For straight necks — by far the dominant real-world case and the
//! shape family the property tests exercise — this *is* the neck midline.
//! Documented limitation: for a strongly curved neck the single straight
//! chord hugs the centroid region rather than following the curve; a
//! skeleton-based centerline would be needed for those.

use pcb_core::{NM_PER_MM, Nm, P, Poly};

use crate::geom;

/// An open polyline in integer nanometers: the laser traces `pts` in order
/// (no implicit closure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolylineNm {
    pub pts: Vec<P>,
}

/// End inset of each centerline chord, as a fraction of `min_feature`.
const END_INSET_FRACTION: f64 = 0.1;

/// Residue components with area below `(min_feature · AREA_FLOOR_FRACTION)²`
/// are boolean/flattening crumbs, not necks.
const AREA_FLOOR_FRACTION: f64 = 0.1;

/// Find every neck of `region` thinner than `min_feature_mm` and return one
/// force-clear centerline pass per neck. See the module docs for the
/// detection scheme, the artifact filters, and the centerline construction.
///
/// Returns an empty vec for an empty region or a non-positive / non-finite
/// `min_feature_mm`.
pub fn force_clear(region: &[Poly], min_feature_mm: f64) -> Vec<PolylineNm> {
    if region.is_empty() || !min_feature_mm.is_finite() || min_feature_mm <= 0.0 {
        return Vec::new();
    }
    let min_nm = min_feature_mm * NM_PER_MM as f64;
    let half = (min_nm / 2.0).round() as Nm;
    if half == 0 {
        return Vec::new(); // sub-nm feature size: nothing is ever a neck
    }

    let opened = geom::offset(&geom::offset(region, -half), half);
    let residue = geom::difference(region, &opened);

    let area_floor = (min_nm * AREA_FLOOR_FRACTION).powi(2);
    let inset_nm = min_nm * END_INSET_FRACTION;

    let mut out = Vec::new();
    for comp in &residue {
        if geom::poly_area(comp) < area_floor {
            continue; // boolean / arc-flattening crumb
        }
        if diameter_nm(&comp.outer) <= min_nm {
            continue; // corner-rounding sliver (diameter ~0.71 * min)
        }
        if let Some(pl) = centerline(comp, inset_nm) {
            out.push(pl);
        }
    }
    out
}

/// Length of the farthest vertex pair of `ring`, in nm. Exact `i128`
/// squared distances, one final sqrt.
fn diameter_nm(ring: &[P]) -> f64 {
    let mut best: i128 = 0;
    for (i, a) in ring.iter().enumerate() {
        for b in &ring[i + 1..] {
            let dx = (b.x - a.x) as i128;
            let dy = (b.y - a.y) as i128;
            best = best.max(dx * dx + dy * dy);
        }
    }
    (best as f64).sqrt()
}

/// Maximal chord of `comp` along its principal axis through its area
/// centroid, inset by `inset_nm` at both ends (clamped so the chord keeps at
/// least half its length). `None` for degenerate components or if the
/// crossing scan finds no interior interval.
fn centerline(comp: &Poly, inset_nm: f64) -> Option<PolylineNm> {
    if comp.outer.len() < 3 {
        return None;
    }
    let c = area_centroid(comp)?;
    let u = principal_axis(&comp.outer)?;

    // Crossings of the line `c + t*u` with every edge of every ring.
    // Half-open edge parameter s in [0, 1) so a vertex hit counts once.
    // Signed perpendicular distance of a point from the ray line through `c`
    // along `u`: zero exactly on the line. An edge crosses the line iff its
    // endpoints have different `perp < 0` classifications, treating `perp == 0`
    // as non-negative. This counts each crossing exactly once even when the
    // ray passes precisely through a vertex — which happens whenever the
    // centroid is vertically (or horizontally) centered on a symmetric neck,
    // the case the edge-parameter `[0,1)` test used to miss entirely.
    let perp = |p: &P| (p.x as f64 - c.0) * u.1 - (p.y as f64 - c.1) * u.0;
    let mut ts: Vec<f64> = Vec::new();
    let mut cross_edges = |ring: &[P]| {
        for (i, a) in ring.iter().enumerate() {
            let b = &ring[(i + 1) % ring.len()];
            let (pa, pb) = (perp(a), perp(b));
            if (pa < 0.0) == (pb < 0.0) {
                continue; // both endpoints on the same side: no crossing
            }
            let s = pa / (pa - pb); // edge param where perp == 0
            let (cx, cy) = (
                a.x as f64 + s * (b.x - a.x) as f64,
                a.y as f64 + s * (b.y - a.y) as f64,
            );
            // Position of the crossing along the unit axis from `c`.
            ts.push((cx - c.0) * u.0 + (cy - c.1) * u.1);
        }
    };
    cross_edges(&comp.outer);
    for hole in &comp.holes {
        cross_edges(hole);
    }
    if ts.len() < 2 {
        return None;
    }
    ts.sort_by(|p, q| p.partial_cmp(q).expect("crossing params are finite"));

    // Consecutive crossing pairs alternate outside/inside starting outside:
    // (ts[0], ts[1]), (ts[2], ts[3]), ... are the interior intervals. Prefer
    // the one containing the centroid (t = 0), else the longest.
    let mut chosen: Option<(f64, f64)> = None;
    for pair in ts.chunks_exact(2) {
        let (t0, t1) = (pair[0], pair[1]);
        if t0 <= 0.0 && 0.0 <= t1 {
            chosen = Some((t0, t1));
            break;
        }
        if chosen.is_none_or(|(b0, b1)| t1 - t0 > b1 - b0) {
            chosen = Some((t0, t1));
        }
    }
    let (t0, t1) = chosen?;
    let len = t1 - t0;
    if len <= 0.0 {
        return None;
    }
    let inset = inset_nm.min(len * 0.25);
    let at = |t: f64| P::new((c.0 + t * u.0).round() as Nm, (c.1 + t * u.1).round() as Nm);
    Some(PolylineNm {
        pts: vec![at(t0 + inset), at(t1 - inset)],
    })
}

/// Area centroid of `comp` (outer minus holes), in nm. Coordinates are
/// translated to the first outer vertex before the shoelace accumulation to
/// keep the f64 products small, then translated back.
fn area_centroid(comp: &Poly) -> Option<(f64, f64)> {
    let o = comp.outer.first()?;
    let (ox, oy) = (o.x as f64, o.y as f64);
    let mut a2 = 0.0; // twice the signed area
    let mut sx = 0.0; // 6 * area-weighted centroid x
    let mut sy = 0.0;
    let mut accum = |ring: &[P]| {
        for (i, p) in ring.iter().enumerate() {
            let q = &ring[(i + 1) % ring.len()];
            let (px, py) = (p.x as f64 - ox, p.y as f64 - oy);
            let (qx, qy) = (q.x as f64 - ox, q.y as f64 - oy);
            let cr = px * qy - qx * py;
            a2 += cr;
            sx += (px + qx) * cr;
            sy += (py + qy) * cr;
        }
    };
    accum(&comp.outer);
    for hole in &comp.holes {
        accum(hole);
    }
    if a2 == 0.0 {
        return None;
    }
    Some((ox + sx / (3.0 * a2), oy + sy / (3.0 * a2)))
}

/// Principal axis of `ring`'s boundary: unit eigenvector of the largest
/// eigenvalue of the edge-length-weighted covariance of the boundary curve
/// (each edge contributes its own second moment plus its midpoint offset).
fn principal_axis(ring: &[P]) -> Option<(f64, f64)> {
    let o = ring.first()?;
    let (ox, oy) = (o.x as f64, o.y as f64);
    // Length-weighted boundary mean.
    let mut total_len = 0.0;
    let mut mx = 0.0;
    let mut my = 0.0;
    let edges: Vec<(f64, f64, f64, f64)> = (0..ring.len())
        .map(|i| {
            let a = &ring[i];
            let b = &ring[(i + 1) % ring.len()];
            (
                a.x as f64 - ox,
                a.y as f64 - oy,
                b.x as f64 - ox,
                b.y as f64 - oy,
            )
        })
        .collect();
    for &(ax, ay, bx, by) in &edges {
        let len = (bx - ax).hypot(by - ay);
        total_len += len;
        mx += len * (ax + bx) * 0.5;
        my += len * (ay + by) * 0.5;
    }
    if total_len <= 0.0 {
        return None;
    }
    mx /= total_len;
    my /= total_len;
    // Covariance: per edge, segment second moment (d*d^T / 12) plus
    // midpoint offset from the mean, weighted by edge length.
    let (mut cxx, mut cxy, mut cyy) = (0.0, 0.0, 0.0);
    for &(ax, ay, bx, by) in &edges {
        let (dx, dy) = (bx - ax, by - ay);
        let len = dx.hypot(dy);
        let (ex, ey) = ((ax + bx) * 0.5 - mx, (ay + by) * 0.5 - my);
        cxx += len * (dx * dx / 12.0 + ex * ex);
        cxy += len * (dx * dy / 12.0 + ex * ey);
        cyy += len * (dy * dy / 12.0 + ey * ey);
    }
    let theta = 0.5 * (2.0 * cxy).atan2(cxx - cyy);
    Some((theta.cos(), theta.sin()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::NM_PER_UM;

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

    /// Two 10 mm squares, 3 mm apart, joined by a neck of width `w_mm`
    /// centered at y = 5 mm. The neck overlaps 0.5 mm into each square so
    /// the union is unambiguous.
    fn dumbbell(w_mm: f64) -> Vec<Poly> {
        let a = rect_mm(0.0, 0.0, 10.0, 10.0);
        let b = rect_mm(13.0, 0.0, 23.0, 10.0);
        let neck = rect_mm(9.5, 5.0 - w_mm / 2.0, 13.5, 5.0 + w_mm / 2.0);
        let joined = geom::union(&geom::union(&[a], &[b]), &[neck]);
        assert_eq!(joined.len(), 1, "dumbbell must be a single component");
        joined
    }

    fn assert_inside_gap(pl: &PolylineNm, w_mm: f64) {
        let slack = NM_PER_UM; // 1 um
        let (y0, y1) = (
            ((5.0 - w_mm / 2.0) * MM as f64) as Nm,
            ((5.0 + w_mm / 2.0) * MM as f64) as Nm,
        );
        for p in &pl.pts {
            assert!(
                p.x >= 10 * MM - slack && p.x <= 13 * MM + slack,
                "x = {} outside gap",
                p.x
            );
            assert!(
                p.y >= y0 - slack && p.y <= y1 + slack,
                "y = {} outside neck",
                p.y
            );
        }
    }

    #[test]
    fn thin_dumbbell_neck_gets_one_centerline_pass() {
        let region = dumbbell(0.08); // 0.08 mm < 0.15 mm min feature
        let passes = force_clear(&region, 0.15);
        assert_eq!(passes.len(), 1, "exactly one neck, one pass");
        let pl = &passes[0];
        assert_eq!(pl.pts.len(), 2);
        assert_inside_gap(pl, 0.08);
        // The chord must run down the neck: nearly the full 3 mm gap,
        // vertically centered on the neck midline.
        let span = (pl.pts[1].x - pl.pts[0].x).abs();
        assert!(span > 2 * MM, "chord spans only {span} nm of a 3 mm neck");
        for p in &pl.pts {
            assert!(
                (p.y - 5 * MM).abs() < 40 * NM_PER_UM,
                "y = {} off-axis",
                p.y
            );
        }
    }

    #[test]
    fn wide_dumbbell_neck_is_left_alone() {
        // 0.3 mm > 1.2 * 0.15 mm: no residue passes the filters, and in
        // particular the squares' corner roundings must not leak through.
        let region = dumbbell(0.3);
        assert!(force_clear(&region, 0.15).is_empty());
    }

    #[test]
    fn empty_and_degenerate_inputs_yield_nothing() {
        assert!(force_clear(&[], 0.15).is_empty());
        let square = [rect_mm(0.0, 0.0, 10.0, 10.0)];
        assert!(force_clear(&square, 0.0).is_empty());
        assert!(force_clear(&square, -1.0).is_empty());
        assert!(force_clear(&square, f64::NAN).is_empty());
        // A plain square has no necks at all.
        assert!(force_clear(&square, 0.15).is_empty());
    }
}
