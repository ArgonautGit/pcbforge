//! CAM-1 — isolation and rub-out ablation path generation.
//!
//! # Isolation contours
//!
//! For each copper boundary (outer rings *and* hole rings — a hole boundary
//! is also a copper boundary) [`ablation_paths`] emits `opts.n_contours`
//! closed offset loops, tagged [`PathKind::Isolation`]`(k)`:
//!
//! * **Contour 0** centerline sits at `spot_mm / 2` outside the copper edge,
//!   so the beam edge kisses the copper edge.
//! * **Subsequent contours** are spaced `spot_mm * 0.7` apart (30 % beam
//!   overlap), i.e. contour `k` sits at `spot_mm/2 + k * spot_mm * 0.7`.
//!
//! See [`isolation_offset_nm`] for the exact (rounded-to-nm) figure. Offsets
//! use round joins, so contour corners are arcs; the straight-edge portions
//! sit at exactly the stated distance.
//!
//! # Rub-out hatching
//!
//! The rub-out **band** is the annular region between the copper dilated by
//! `clearance_mm` and the copper dilated by `clearance_mm + band_mm`
//! (computed as an xor; see [`rubout_band`]). It is hatched with parallel
//! lines spaced `interval_mm` apart, one hatch set per pass:
//!
//! * `hatch_sets` is an explicit argument — `CamOpts` carries no pass count;
//!   CAM-4 will map pass index → angle when planning pass groups.
//! * Set `k` (for `k in 0..hatch_sets`) uses angle
//!   `base_angle_deg + k * fill_angle_step_deg` (see
//!   [`hatch_set_angle_deg`]) and its segments are tagged
//!   [`PathKind::Rubout`]`(k)`.
//! * Each hatch segment is an open two-point [`PathElem`]. Segments shorter
//!   than [`MIN_HATCH_SEG_NM`] (1 µm, far below any spot size) are dropped.
//!
//! Hatching works by rotating the band's rings into a frame where the hatch
//! direction is horizontal (an f64 isometry, rounded back to the nm grid:
//! ≤ 0.5 nm per coordinate), then intersecting integer horizontal scanlines
//! with the rings using exact `i64`/`i128` arithmetic and even–odd pairing.
//! Scanlines start half an interval above the band's lowest point.

use crate::geom;
use pcb_core::{CamOpts, Layer, NM_PER_MM, Nm, P, PathElem, PathKind, Paths, Poly, Ring};

/// Fraction of the spot diameter between adjacent isolation-contour
/// centerlines (30 % overlap between adjacent beam passes).
pub const CONTOUR_PITCH_FACTOR: f64 = 0.7;

/// Hatch segments shorter than this (1 µm) are dropped: they are far below
/// any spot size and typically arise from scanlines grazing the band edge.
pub const MIN_HATCH_SEG_NM: Nm = 1_000;

/// Generate the full ablation tool-path set for one copper layer.
///
/// * Isolation: for `k in 0..opts.n_contours`, the copper region offset
///   outward by [`isolation_offset_nm`]`(opts, k)`; every resulting ring
///   (outer and holes) becomes one closed `Isolation(k)` element.
/// * Rub-out: the [`rubout_band`] hatched once per set for
///   `k in 0..hatch_sets` at [`hatch_set_angle_deg`]`(opts, k)` degrees with
///   `opts.interval_mm` spacing; each segment is an open two-point
///   `Rubout(k)` element.
///
/// Elements are ordered: all isolation contours (ascending `k`), then all
/// hatch sets (ascending `k`). A non-positive `interval_mm` disables
/// hatching.
pub fn ablation_paths(layer: &Layer, opts: &CamOpts, hatch_sets: u32) -> Paths {
    let mut elems = Vec::new();

    for k in 0..opts.n_contours {
        for poly in geom::offset(&layer.polys, isolation_offset_nm(opts, k)) {
            push_closed(&mut elems, PathKind::Isolation(k), poly.outer);
            for hole in poly.holes {
                push_closed(&mut elems, PathKind::Isolation(k), hole);
            }
        }
    }

    let interval_nm = mm_to_nm(opts.interval_mm);
    if interval_nm > 0 && hatch_sets > 0 {
        let band = rubout_band(layer, opts);
        if !band.is_empty() {
            for k in 0..hatch_sets {
                for (a, b) in hatch_region(&band, interval_nm, hatch_set_angle_deg(opts, k)) {
                    elems.push(PathElem {
                        kind: PathKind::Rubout(k),
                        pts: vec![a, b],
                        closed: false,
                    });
                }
            }
        }
    }

    Paths { elems }
}

/// Centerline offset of isolation contour `k` from the copper boundary, in
/// nm: `spot_mm/2 + k * spot_mm * `[`CONTOUR_PITCH_FACTOR`], rounded to the
/// nearest nanometer.
pub fn isolation_offset_nm(opts: &CamOpts, k: u32) -> Nm {
    mm_to_nm(opts.spot_mm / 2.0 + k as f64 * opts.spot_mm * CONTOUR_PITCH_FACTOR)
}

/// The rub-out band region:
/// `xor(dilate(copper, clearance), dilate(copper, clearance + band))`, i.e.
/// the annular strip between `clearance_mm` and `clearance_mm + band_mm`
/// outside the copper. Normalized polys (outer CCW, holes CW).
pub fn rubout_band(layer: &Layer, opts: &CamOpts) -> Vec<Poly> {
    let clearance_nm = mm_to_nm(opts.clearance_mm);
    let band_nm = mm_to_nm(opts.band_mm);
    let inner = geom::offset(&layer.polys, clearance_nm);
    let outer = geom::offset(&layer.polys, clearance_nm + band_nm);
    geom::xor(&outer, &inner)
}

/// Hatch angle of set `k`: `base_angle_deg + k * fill_angle_step_deg`.
/// Computed as a single fused expression so consecutive sets differ by
/// exactly `fill_angle_step_deg` whenever the operands make that difference
/// representable (e.g. any dyadic-rational degree grid).
pub fn hatch_set_angle_deg(opts: &CamOpts, k: u32) -> f64 {
    opts.base_angle_deg + k as f64 * opts.fill_angle_step_deg
}

/// Point-in-region test over normalized polys, used by the property tests.
///
/// Returns `true` if `pt` is inside by even–odd parity, **or** within
/// `tol_nm` of any ring edge (the boundary, padded by `tol_nm`, counts as
/// inside). Parity is computed exactly in `i128`; the edge-distance check is
/// f64 (exact for PCB-scale coordinates).
///
/// **Precondition:** `polys` must be *normalized* (non-self-overlapping, holes
/// nested in outers). Even–odd parity reads a region covered by two
/// overlapping un-normalized outers as *outside* — pass geometry through the
/// crate's normalization first, matching its nonzero-winding convention (LR-50).
pub fn point_in_polys(pt: P, polys: &[Poly], tol_nm: Nm) -> bool {
    let tol2 = tol_nm as f64 * tol_nm as f64;
    let mut inside = false;
    for poly in polys {
        for ring in std::iter::once(&poly.outer).chain(poly.holes.iter()) {
            for (a, b) in ring_edges(ring) {
                if seg_dist2(pt, a, b) <= tol2 {
                    return true;
                }
                // Half-open crossing rule; horizontal edges never cross.
                if (a.y > pt.y) != (b.y > pt.y) {
                    // Crossing x satisfies x * den = num (exact i128).
                    let den = (b.y - a.y) as i128;
                    let num = (pt.y - a.y) as i128 * (b.x - a.x) as i128 + a.x as i128 * den;
                    let diff = num - pt.x as i128 * den;
                    // Count crossings strictly to the right of `pt`.
                    if diff != 0 && (diff > 0) == (den > 0) {
                        inside = !inside;
                    }
                }
            }
        }
    }
    inside
}

// ---------------------------------------------------------------------------
// Hatching (exact integer scanlines in a rotated frame)
// ---------------------------------------------------------------------------

/// Hatch `region` with parallel segments at `angle_deg` (CCW from +x),
/// spaced `interval_nm` apart. Returns segment endpoints in board frame.
fn hatch_region(region: &[Poly], interval_nm: Nm, angle_deg: f64) -> Vec<(P, P)> {
    debug_assert!(interval_nm > 0);
    let (sin, cos) = angle_deg.to_radians().sin_cos();

    // Rotate every ring by -angle so hatch lines become horizontal.
    let rings: Vec<Ring> = region
        .iter()
        .flat_map(|poly| std::iter::once(&poly.outer).chain(poly.holes.iter()))
        .map(|ring| rotate_ring(ring, cos, -sin))
        .collect();

    let ys = rings.iter().flatten().map(|p| p.y);
    let Some(y_min) = ys.clone().min() else {
        return Vec::new();
    };
    let y_max = ys.max().unwrap();

    let mut segments = Vec::new();
    let mut crossings: Vec<Nm> = Vec::new();
    let mut y0 = y_min + interval_nm / 2;
    while y0 < y_max {
        crossings.clear();
        for ring in &rings {
            for (a, b) in ring_edges(ring) {
                // Half-open rule: each non-horizontal edge crossed by the
                // scanline counts exactly once, so parity is consistent.
                if (a.y > y0) != (b.y > y0) {
                    crossings.push(scanline_x(a, b, y0));
                }
            }
        }
        crossings.sort_unstable();
        for pair in crossings.chunks_exact(2) {
            if pair[1] - pair[0] >= MIN_HATCH_SEG_NM {
                segments.push((
                    rotate_pt_f64(pair[0], y0, cos, sin),
                    rotate_pt_f64(pair[1], y0, cos, sin),
                ));
            }
        }
        y0 += interval_nm;
    }
    segments
}

/// Exact x of the intersection of edge `(a, b)` with the horizontal line
/// `y = y0`, rounded to the nearest nm. Caller guarantees `a.y != b.y` and
/// that `y0` lies between them.
fn scanline_x(a: P, b: P, y0: Nm) -> Nm {
    let num = (y0 - a.y) as i128 * (b.x - a.x) as i128;
    let den = (b.y - a.y) as i128;
    (a.x as i128 + div_round_nearest(num, den)) as Nm
}

/// `num / den` rounded to nearest, ties away from zero. Exact in i128.
fn div_round_nearest(num: i128, den: i128) -> i128 {
    let q = num / den;
    let r = num % den;
    if 2 * r.abs() >= den.abs() {
        q + if (num < 0) == (den < 0) { 1 } else { -1 }
    } else {
        q
    }
}

/// Rotate `ring` by the angle with cosine `cos` and sine `sin`, rounding
/// each vertex to the nm grid (≤ 0.5 nm per coordinate).
fn rotate_ring(ring: &Ring, cos: f64, sin: f64) -> Ring {
    ring.iter()
        .map(|p| rotate_pt_f64(p.x, p.y, cos, sin))
        .collect()
}

fn rotate_pt_f64(x: Nm, y: Nm, cos: f64, sin: f64) -> P {
    let (xf, yf) = (x as f64, y as f64);
    P::new(
        (xf * cos - yf * sin).round() as Nm,
        (xf * sin + yf * cos).round() as Nm,
    )
}

/// Squared distance from `pt` to segment `(a, b)`, in nm² (f64).
fn seg_dist2(pt: P, a: P, b: P) -> f64 {
    let (abx, aby) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let (apx, apy) = ((pt.x - a.x) as f64, (pt.y - a.y) as f64);
    let len2 = abx * abx + aby * aby;
    let t = if len2 > 0.0 {
        ((apx * abx + apy * aby) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (dx, dy) = (apx - t * abx, apy - t * aby);
    dx * dx + dy * dy
}

/// Iterate a ring's edges (implicit closure: last vertex connects to first).
fn ring_edges(ring: &Ring) -> impl Iterator<Item = (P, P)> + '_ {
    ring.iter()
        .enumerate()
        .map(|(i, &a)| (a, ring[(i + 1) % ring.len()]))
}

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

    fn disc_mm(cx: f64, cy: f64, r: f64, n: usize) -> Poly {
        let outer = (0..n)
            .map(|i| {
                let t = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
                P::from_mm(cx + r * t.cos(), cy + r * t.sin())
            })
            .collect();
        Poly {
            outer,
            holes: vec![],
        }
    }

    /// y-coordinates where `ring` (closed) crosses the vertical line `x`.
    fn crossings_at_x(ring: &Ring, x: Nm) -> Vec<f64> {
        ring_edges(ring)
            .filter(|(a, b)| (a.x > x) != (b.x > x))
            .map(|(a, b)| a.y as f64 + (x - a.x) as f64 * (b.y - a.y) as f64 / (b.x - a.x) as f64)
            .collect()
    }

    fn seg_len_nm(e: &PathElem) -> f64 {
        ((e.pts[1].x - e.pts[0].x) as f64).hypot((e.pts[1].y - e.pts[0].y) as f64)
    }

    #[test]
    fn square_isolation_contour_count_and_spacing() {
        // 10 mm square, 3 contours: one closed loop per contour, centerline
        // at spot/2 + k * 0.7 * spot from the edge, sampled at edge
        // midpoints (corners are round joins, so never sample corners).
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 10.0, 10.0)],
        };
        let opts = CamOpts {
            n_contours: 3,
            ..CamOpts::default()
        };
        let paths = ablation_paths(&layer, &opts, 0);
        assert_eq!(paths.elems.len(), 3);

        for k in 0..3u32 {
            let loops: Vec<&PathElem> = paths
                .elems
                .iter()
                .filter(|e| e.kind == PathKind::Isolation(k))
                .collect();
            assert_eq!(loops.len(), 1, "one loop per contour on a square");
            let e = loops[0];
            assert!(e.closed);

            // Exact convention: d_k = round((spot/2 + 0.7*spot*k) mm).
            let d = isolation_offset_nm(&opts, k);
            let expect_d_mm = opts.spot_mm / 2.0 + k as f64 * 0.7 * opts.spot_mm;
            assert!((d as f64 - expect_d_mm * MM as f64).abs() <= 0.5);

            // Vertical line through the top/bottom edge midpoints.
            let ys = crossings_at_x(&e.pts, 5 * MM);
            let y_hi = ys.iter().cloned().fold(f64::MIN, f64::max);
            let y_lo = ys.iter().cloned().fold(f64::MAX, f64::min);
            let um = 1_000.0; // 1 µm in nm
            assert!((y_hi - (10 * MM + d) as f64).abs() < um, "k={k}: top edge");
            assert!((y_lo - (-d) as f64).abs() < um, "k={k}: bottom edge");
        }

        // Contour pitch: exactly spot * 0.7 (within 1 nm of rounding).
        let pitch = mm_to_nm(opts.spot_mm * CONTOUR_PITCH_FACTOR);
        for k in 0..2 {
            let diff = isolation_offset_nm(&opts, k + 1) - isolation_offset_nm(&opts, k);
            assert!((diff - pitch).abs() <= 1, "pitch {diff} != {pitch}");
        }
    }

    #[test]
    fn isolation_traces_hole_boundaries_too() {
        // Copper with a hole: each contour must produce two rings (outer
        // grown outward, hole shrunk inward — both copper boundaries).
        let mut p = rect_mm(0.0, 0.0, 10.0, 10.0);
        let mut hole = rect_mm(3.0, 3.0, 7.0, 7.0).outer;
        hole.reverse();
        p.holes.push(hole);
        let layer = Layer { polys: vec![p] };
        let opts = CamOpts {
            n_contours: 1,
            ..CamOpts::default()
        };
        let paths = ablation_paths(&layer, &opts, 0);
        let iso: Vec<&PathElem> = paths
            .elems
            .iter()
            .filter(|e| e.kind == PathKind::Isolation(0))
            .collect();
        assert_eq!(iso.len(), 2, "outer ring + hole ring");
        assert!(iso.iter().all(|e| e.closed));
    }

    #[test]
    fn annulus_rubout_hatch_length_matches_area_over_interval() {
        // Disc copper -> annular band between r+clearance and
        // r+clearance+band. Total hatch length ~= band area / interval.
        let layer = Layer {
            polys: vec![disc_mm(0.0, 0.0, 2.0, 720)],
        };
        let opts = CamOpts {
            n_contours: 0,
            clearance_mm: 0.5,
            band_mm: 1.0,
            interval_mm: 0.03,
            base_angle_deg: 0.0,
            ..CamOpts::default()
        };

        // Sanity: band area matches the analytic annulus within 0.5 %.
        let band = rubout_band(&layer, &opts);
        let band_area = geom::area_nm2(&band);
        let analytic = std::f64::consts::PI * (3.5 * 3.5 - 2.5 * 2.5) * (MM as f64) * (MM as f64);
        assert!(
            (band_area - analytic).abs() / analytic < 5e-3,
            "band area {band_area} vs analytic {analytic}"
        );

        let paths = ablation_paths(&layer, &opts, 1);
        assert!(!paths.elems.is_empty());
        let mut total_len = 0.0;
        for e in &paths.elems {
            assert_eq!(e.kind, PathKind::Rubout(0));
            assert!(!e.closed);
            assert_eq!(e.pts.len(), 2);
            total_len += seg_len_nm(e);
        }
        let expect_len = band_area / mm_to_nm(opts.interval_mm) as f64;
        assert!(
            (total_len - expect_len).abs() / expect_len < 1e-2,
            "hatch length {total_len} vs analytic {expect_len}"
        );
    }

    #[test]
    fn hatch_sets_carry_their_pass_angle() {
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 5.0, 5.0)],
        };
        let opts = CamOpts {
            n_contours: 0,
            base_angle_deg: 0.0,
            fill_angle_step_deg: 17.0,
            ..CamOpts::default()
        };
        let paths = ablation_paths(&layer, &opts, 2);

        // Set 0 at 0 deg: rotation is the identity, so segments are exactly
        // horizontal.
        let set0: Vec<&PathElem> = paths
            .elems
            .iter()
            .filter(|e| e.kind == PathKind::Rubout(0))
            .collect();
        assert!(!set0.is_empty());
        assert!(set0.iter().all(|e| e.pts[0].y == e.pts[1].y));

        // Set 1 at 17 deg: measure the longest segment's direction.
        let longest = paths
            .elems
            .iter()
            .filter(|e| e.kind == PathKind::Rubout(1))
            .max_by(|a, b| seg_len_nm(a).total_cmp(&seg_len_nm(b)))
            .expect("set 1 present");
        let ang = ((longest.pts[1].y - longest.pts[0].y) as f64)
            .atan2((longest.pts[1].x - longest.pts[0].x) as f64)
            .to_degrees()
            .rem_euclid(180.0);
        assert!((ang - 17.0).abs() < 1e-3, "measured {ang} deg");
    }

    #[test]
    fn consecutive_hatch_set_angles_differ_by_exactly_step() {
        let opts = CamOpts {
            base_angle_deg: 11.25,
            fill_angle_step_deg: 6.5,
            ..CamOpts::default()
        };
        for k in 0..16 {
            let diff = hatch_set_angle_deg(&opts, k + 1) - hatch_set_angle_deg(&opts, k);
            assert_eq!(diff, opts.fill_angle_step_deg, "k={k}");
        }
    }

    #[test]
    fn hatch_midpoints_inside_band_smoke() {
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 4.0, 2.0), rect_mm(6.0, 1.0, 8.0, 5.0)],
        };
        let opts = CamOpts {
            n_contours: 0,
            base_angle_deg: 33.7,
            ..CamOpts::default()
        };
        let band = rubout_band(&layer, &opts);
        let paths = ablation_paths(&layer, &opts, 3);
        for e in &paths.elems {
            let mid = P::new((e.pts[0].x + e.pts[1].x) / 2, (e.pts[0].y + e.pts[1].y) / 2);
            assert!(
                point_in_polys(mid, &band, 10),
                "midpoint {mid:?} outside band"
            );
        }
    }

    #[test]
    fn empty_layer_yields_no_paths() {
        let layer = Layer::default();
        let paths = ablation_paths(&layer, &CamOpts::default(), 3);
        assert!(paths.elems.is_empty());
    }

    #[test]
    fn point_in_polys_basics() {
        let sq = rect_mm(0.0, 0.0, 1.0, 1.0);
        let polys = vec![sq];
        assert!(point_in_polys(P::from_mm(0.5, 0.5), &polys, 0));
        assert!(!point_in_polys(P::from_mm(1.5, 0.5), &polys, 0));
        // On the boundary: inside with tolerance.
        assert!(point_in_polys(P::from_mm(1.0, 0.5), &polys, 1));
        // Just outside, beyond the tolerance.
        assert!(!point_in_polys(P::new(MM + 10, MM / 2), &polys, 1));
    }

    #[test]
    fn div_round_nearest_is_symmetric() {
        assert_eq!(div_round_nearest(7, 2), 4);
        assert_eq!(div_round_nearest(-7, 2), -4);
        assert_eq!(div_round_nearest(7, -2), -4);
        assert_eq!(div_round_nearest(1, 3), 0);
        assert_eq!(div_round_nearest(2, 3), 1);
        assert_eq!(div_round_nearest(-2, 3), -1);
    }
}
