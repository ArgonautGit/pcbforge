//! Non-copper region extraction (the FlatCAM-replacement inversion).
//!
//! The operator's ablation workflow fills and lases everything that is *not*
//! copper. Given a parsed copper layer and a board region, the non-copper
//! geometry is `board − offset(copper, +beam_offset)`: contiguous closed
//! shapes (outers with holes) ready for the DXF/SVG exporters and an EZCAD
//! even-odd fill.
//!
//! The board region comes from one of two sources:
//! * [`board_region_from_outline`] — an Edge.Cuts layer, drawn as stroked
//!   contours. Each connected component's outermost ring is filled; a
//!   component nested inside another filled component at odd depth is a
//!   cutout and is subtracted (so a slot drawn inside the outline stays
//!   open).
//! * [`board_region_bbox`] — no outline available: the copper bounding box
//!   grown by a margin.

use pcb_core::{Nm, P, Poly};

use crate::ablation::point_in_polys;
use crate::geom;

/// Board region enclosed by a stroked outline layer (e.g. KiCad Edge.Cuts).
///
/// `outline` must already be normalized disjoint polys (as produced by the
/// gerber ingest). Returns the filled board area with cutouts open; empty if
/// the outline encloses nothing.
pub fn board_region_from_outline(outline: &[Poly]) -> Vec<Poly> {
    // Candidate fill per component: its outermost ring, holes dropped (the
    // hole of a stroked border ring is the board interior we want filled).
    let mut candidates: Vec<Poly> = outline
        .iter()
        .filter(|p| p.outer.len() >= 3)
        .map(|p| Poly {
            outer: p.outer.clone(),
            holes: vec![],
        })
        .collect();
    // Largest first, so containment against already-placed regions is
    // well-defined at every step.
    candidates.sort_by(|a, b| {
        geom::poly_area(b)
            .abs()
            .partial_cmp(&geom::poly_area(a).abs())
            .expect("areas are finite")
    });

    let mut region: Vec<Poly> = Vec::new();
    let mut placed: Vec<Poly> = Vec::new();
    for cand in candidates {
        let probe = cand.outer[0];
        // Nesting depth = how many already-placed outers contain this ring.
        let depth = placed
            .iter()
            .filter(|p| point_in_polys(probe, std::slice::from_ref(*p), 0))
            .count();
        region = if depth % 2 == 0 {
            geom::union(&region, std::slice::from_ref(&cand))
        } else {
            geom::difference(&region, std::slice::from_ref(&cand))
        };
        placed.push(cand);
    }
    region
}

/// Fallback board region: the copper bounding box grown by `margin_nm` on
/// every side. Empty when there is no copper.
pub fn board_region_bbox(copper: &[Poly], margin_nm: Nm) -> Vec<Poly> {
    let mut min = P::new(Nm::MAX, Nm::MAX);
    let mut max = P::new(Nm::MIN, Nm::MIN);
    let mut any = false;
    for poly in copper {
        for p in &poly.outer {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
            any = true;
        }
    }
    if !any {
        return Vec::new();
    }
    let (x0, y0) = (min.x - margin_nm, min.y - margin_nm);
    let (x1, y1) = (max.x + margin_nm, max.y + margin_nm);
    vec![Poly {
        outer: vec![
            P::new(x0, y0),
            P::new(x1, y0),
            P::new(x1, y1),
            P::new(x0, y1),
        ],
        holes: vec![],
    }]
}

/// The region to ablate: `board − offset(copper, offset_nm)`.
///
/// `offset_nm` is the beam-compensation clearance kept around every copper
/// edge (typically half the effective spot diameter); `0` is the exact
/// geometric inverse. Output shapes are disjoint, outers CCW, holes CW —
/// each one a contiguous closed region as the fill-and-burn workflow expects.
pub fn noncopper(board: &[Poly], copper: &[Poly], offset_nm: Nm) -> Vec<Poly> {
    let grown = if offset_nm == 0 {
        copper.to_vec()
    } else {
        geom::offset(copper, offset_nm)
    };
    geom::difference(board, &grown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::NM_PER_MM;

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

    /// A stroked rectangular border like a parsed Edge.Cuts: ring outer with
    /// the interior as a hole.
    fn stroked_border(x0: Nm, y0: Nm, x1: Nm, y1: Nm, w: Nm) -> Poly {
        let outer = rect(x0 - w / 2, y0 - w / 2, x1 + w / 2, y1 + w / 2).outer;
        let mut hole = rect(x0 + w / 2, y0 + w / 2, x1 - w / 2, y1 - w / 2).outer;
        hole.reverse(); // holes are CW by convention
        Poly {
            outer,
            holes: vec![hole],
        }
    }

    #[test]
    fn outline_border_fills_the_interior() {
        let border = stroked_border(0, 0, 50 * MM, 30 * MM, 100_000);
        let region = board_region_from_outline(&[border]);
        assert_eq!(region.len(), 1);
        let area = geom::area_nm2(&region);
        let expected = (50.1 * 30.1) * (MM as f64) * (MM as f64);
        assert!((area - expected).abs() / expected < 1e-6, "area {area}");
    }

    #[test]
    fn nested_cutout_stays_open() {
        let border = stroked_border(0, 0, 50 * MM, 30 * MM, 100_000);
        let slot = stroked_border(10 * MM, 10 * MM, 20 * MM, 20 * MM, 100_000);
        let region = board_region_from_outline(&[border, slot]);
        // Cutout removed: area = filled border − slot's filled outer.
        let area = geom::area_nm2(&region);
        let expected = (50.1 * 30.1 - 10.1 * 10.1) * (MM as f64) * (MM as f64);
        assert!((area - expected).abs() / expected < 1e-6);
        assert!(
            region.iter().any(|p| !p.holes.is_empty()),
            "cutout must appear as a hole"
        );
    }

    #[test]
    fn bbox_region_grows_by_margin() {
        let copper = vec![rect(0, 0, 10 * MM, 5 * MM)];
        let region = board_region_bbox(&copper, MM);
        assert_eq!(region.len(), 1);
        assert!((geom::area_nm2(&region) - (12.0 * 7.0) * (MM as f64) * (MM as f64)).abs() < 1.0);
        assert!(board_region_bbox(&[], MM).is_empty());
    }

    #[test]
    fn noncopper_is_the_exact_inverse_at_zero_offset() {
        let board = vec![rect(0, 0, 20 * MM, 10 * MM)];
        let copper = vec![
            rect(2 * MM, 2 * MM, 8 * MM, 8 * MM),
            rect(12 * MM, 3 * MM, 18 * MM, 7 * MM),
        ];
        let nc = noncopper(&board, &copper, 0);
        let total = geom::area_nm2(&nc) + geom::area_nm2(&copper);
        let board_area = geom::area_nm2(&board);
        assert!(
            (total - board_area).abs() / board_area < 1e-9,
            "tiling failed"
        );
        assert!(geom::intersect(&nc, &copper).is_empty(), "overlap");
    }

    #[test]
    fn positive_offset_keeps_clearance_from_copper() {
        let board = vec![rect(0, 0, 20 * MM, 10 * MM)];
        let copper = vec![rect(5 * MM, 3 * MM, 15 * MM, 7 * MM)];
        let off = 50_000; // 0.05 mm
        let nc = noncopper(&board, &copper, off);
        // Nothing of the result may lie within the grown copper.
        let grown = geom::offset(&copper, off - 1_000);
        assert!(geom::intersect(&nc, &grown).is_empty());
        // But it must still reach the board edge.
        let area = geom::area_nm2(&nc);
        assert!(area > 0.0);
        let exact_inverse = geom::area_nm2(&noncopper(&board, &copper, 0));
        assert!(
            area < exact_inverse,
            "offset must shrink the ablation region"
        );
    }

    #[test]
    fn copper_island_inside_pour_survives_as_hole_island() {
        // Copper ring (pour with a window) — noncopper must fill the window
        // as its own contiguous shape.
        let mut window = rect(4 * MM, 4 * MM, 6 * MM, 6 * MM).outer;
        window.reverse();
        let pour = Poly {
            outer: rect(2 * MM, 2 * MM, 8 * MM, 8 * MM).outer,
            holes: vec![window],
        };
        let board = vec![rect(0, 0, 10 * MM, 10 * MM)];
        let nc = noncopper(&board, &[pour], 0);
        // Two shapes: the moat around the pour, and the window inside it.
        assert_eq!(nc.len(), 2, "window must be its own contiguous shape");
    }
}
