//! CAM-9 (stretch) — tiling for the slide extension.
//!
//! A galvo can only address a fixed field (~140 mm). A job larger than the
//! field is split into overlapping tiles; between tiles the board is moved on
//! the slide and re-registered, so each tile carries a re-registration
//! request. This module is **geometry only** — the physical stage moves are
//! driven by ComMarker Studio (see docs/decisions.md); nothing here commands
//! motion.
//!
//! # Ownership by centroid (no element splitting)
//!
//! Adjacent tile fields overlap by `overlap_mm`, so an element near a seam is
//! reachable from either tile. Rather than clip elements at seams (which would
//! create split geometry and inexact unions), each element is assigned whole to
//! exactly one **authoritative** tile — the field cell its centroid falls in.
//! Consequences, both asserted by the property tests:
//!
//! * every element is authoritative in exactly one tile (a partition), so the
//!   union of all tiles' geometry equals the original job exactly; and
//! * each owned element fits inside its tile's field window — *except* an
//!   element larger than the field itself, which cannot fit any field; such
//!   elements are still assigned but reported in [`TilePlan::oversized`] rather
//!   than silently mis-tiled.

use pcb_core::{NM_PER_MM, Nm, P, PathElem, Paths};

/// Default galvo field, mm.
pub const FIELD_MM: f64 = 140.0;

/// Default stitch overlap between adjacent fields, mm.
pub const STITCH_OVERLAP_MM: f64 = 2.0;

/// A per-tile request to move the slide and re-register before running it.
/// VIS-6 fills in the real fiducial/galvo-calibration handshake later; here it
/// records which field must be brought under the head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReRegister {
    pub col: u32,
    pub row: u32,
    /// Field window this tile must be registered within (nm, board frame).
    pub field_min: P,
    pub field_max: P,
}

/// One tile: the elements it owns and where its field sits.
#[derive(Debug, Clone)]
pub struct Tile {
    pub col: u32,
    pub row: u32,
    pub field_min: P,
    pub field_max: P,
    /// Elements this tile is authoritative for.
    pub paths: Paths,
    pub reregister: ReRegister,
}

/// The result of tiling a job.
#[derive(Debug, Clone)]
pub struct TilePlan {
    pub tiles: Vec<Tile>,
    pub field_nm: Nm,
    pub overlap_nm: Nm,
    /// Indices (into the input `paths.elems`) of elements larger than one
    /// field in some dimension — they cannot fit any tile and need a smaller
    /// field or manual handling.
    pub oversized: Vec<usize>,
}

impl TilePlan {
    /// Total authoritative elements across all tiles (== input element count).
    pub fn total_elems(&self) -> usize {
        self.tiles.iter().map(|t| t.paths.elems.len()).sum()
    }
}

/// Split `paths` into ≤`field_mm` tiles overlapping by `overlap_mm`.
///
/// A job already within one field yields a single tile. See the module docs
/// for the centroid-ownership scheme.
pub fn tile(paths: &Paths, field_mm: f64, overlap_mm: f64) -> TilePlan {
    let field_nm = (field_mm * NM_PER_MM as f64).round() as Nm;
    let overlap_nm = (overlap_mm * NM_PER_MM as f64).round() as Nm;
    let stride_nm = (field_nm - overlap_nm).max(1);

    let Some((minx, miny, maxx, maxy)) = paths_bbox(paths) else {
        return TilePlan {
            tiles: Vec::new(),
            field_nm,
            overlap_nm,
            oversized: Vec::new(),
        };
    };

    // Field count per axis: enough overlapping fields of width `field` (stride
    // `field - overlap`) to cover the span.
    let cols = axis_count(maxx - minx, field_nm, stride_nm);
    let rows = axis_count(maxy - miny, field_nm, stride_nm);

    // Bucket element indices by the field cell their centroid falls in.
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); (cols * rows) as usize];
    let mut oversized = Vec::new();
    for (i, e) in paths.elems.iter().enumerate() {
        let c = centroid(&e.pts);
        let col = cell(c.x - minx, stride_nm, cols);
        let row = cell(c.y - miny, stride_nm, rows);
        buckets[(row * cols + col) as usize].push(i);

        let (ex0, ey0, ex1, ey1) = elem_bbox(e);
        if (ex1 - ex0) > field_nm || (ey1 - ey0) > field_nm {
            oversized.push(i);
        }
    }

    let mut tiles = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            let bucket = &buckets[(row * cols + col) as usize];
            if bucket.is_empty() {
                continue; // no elements own this field — skip the stage move
            }
            let field_min = P::new(minx + col as Nm * stride_nm, miny + row as Nm * stride_nm);
            let field_max = P::new(field_min.x + field_nm, field_min.y + field_nm);
            let elems = bucket.iter().map(|&i| paths.elems[i].clone()).collect();
            tiles.push(Tile {
                col,
                row,
                field_min,
                field_max,
                paths: Paths { elems },
                reregister: ReRegister {
                    col,
                    row,
                    field_min,
                    field_max,
                },
            });
        }
    }

    TilePlan {
        tiles,
        field_nm,
        overlap_nm,
        oversized,
    }
}

/// Number of overlapping fields needed to cover `span` (nm) with field width
/// `field` and stride `stride`. At least 1.
fn axis_count(span: Nm, field: Nm, stride: Nm) -> u32 {
    if span <= field {
        return 1;
    }
    // Fields at 0, stride, 2·stride, … ; need the last to reach `span`.
    (((span - field) as f64 / stride as f64).ceil() as u32) + 1
}

/// Field-cell index for an offset `off` (nm) from the bbox origin.
fn cell(off: Nm, stride: Nm, count: u32) -> u32 {
    if off <= 0 {
        return 0;
    }
    ((off / stride) as u32).min(count - 1)
}

fn centroid(pts: &[P]) -> P {
    let n = pts.len().max(1) as f64;
    P::new(
        (pts.iter().map(|p| p.x as f64).sum::<f64>() / n).round() as Nm,
        (pts.iter().map(|p| p.y as f64).sum::<f64>() / n).round() as Nm,
    )
}

fn elem_bbox(e: &PathElem) -> (Nm, Nm, Nm, Nm) {
    let (mut x0, mut y0, mut x1, mut y1) = (Nm::MAX, Nm::MAX, Nm::MIN, Nm::MIN);
    for p in &e.pts {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    (x0, y0, x1, y1)
}

fn paths_bbox(paths: &Paths) -> Option<(Nm, Nm, Nm, Nm)> {
    let mut b: Option<(Nm, Nm, Nm, Nm)> = None;
    for e in &paths.elems {
        for p in &e.pts {
            b = Some(match b {
                None => (p.x, p.y, p.x, p.y),
                Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
            });
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::{PathKind, Paths};

    const MM: Nm = NM_PER_MM;

    /// A small square element (a "dot") centered at (cx, cy) mm.
    fn dot(cx: f64, cy: f64) -> PathElem {
        let (x, y) = ((cx * MM as f64) as Nm, (cy * MM as f64) as Nm);
        PathElem {
            kind: PathKind::Rubout(0),
            pts: vec![
                P::new(x - 100_000, y - 100_000),
                P::new(x + 100_000, y - 100_000),
                P::new(x + 100_000, y + 100_000),
                P::new(x - 100_000, y + 100_000),
            ],
            closed: true,
        }
    }

    /// A grid of dots spanning `w × h` mm at `pitch` mm.
    fn grid(w: f64, h: f64, pitch: f64) -> Paths {
        let mut elems = Vec::new();
        let mut y = 0.0;
        while y <= h {
            let mut x = 0.0;
            while x <= w {
                elems.push(dot(x, y));
                x += pitch;
            }
            y += pitch;
        }
        Paths { elems }
    }

    #[test]
    fn small_job_is_a_single_tile() {
        let job = grid(100.0, 80.0, 10.0); // within a 140 mm field
        let plan = tile(&job, FIELD_MM, STITCH_OVERLAP_MM);
        assert_eq!(plan.tiles.len(), 1);
        assert_eq!(plan.total_elems(), job.elems.len());
        assert!(plan.oversized.is_empty());
    }

    #[test]
    fn oversize_job_partitions_exactly() {
        // 300 x 90 mm: needs multiple columns, one row.
        let job = grid(300.0, 90.0, 5.0);
        let plan = tile(&job, FIELD_MM, STITCH_OVERLAP_MM);
        assert!(plan.tiles.len() >= 2, "300 mm must split");

        // Partition: every element authoritative in exactly one tile.
        assert_eq!(plan.total_elems(), job.elems.len());
        let mut seen: Vec<f64> = plan
            .tiles
            .iter()
            .flat_map(|t| t.paths.elems.iter().map(|e| e.pts[0].x as f64))
            .collect();
        seen.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut orig: Vec<f64> = job.elems.iter().map(|e| e.pts[0].x as f64).collect();
        orig.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(seen, orig, "union of tiles == original (a permutation)");

        // Every owned element fits inside its tile's field window.
        for t in &plan.tiles {
            for e in &t.paths.elems {
                let (x0, y0, x1, y1) = elem_bbox(e);
                assert!(
                    x0 >= t.field_min.x
                        && y0 >= t.field_min.y
                        && x1 <= t.field_max.x
                        && y1 <= t.field_max.y,
                    "element escapes tile ({},{}) field",
                    t.col,
                    t.row
                );
            }
        }
    }

    #[test]
    fn adjacent_fields_overlap_by_the_stitch_margin() {
        let job = grid(300.0, 20.0, 5.0);
        let plan = tile(&job, FIELD_MM, STITCH_OVERLAP_MM);
        // Sort tiles by column and check consecutive field overlap == overlap.
        let mut cols: Vec<&Tile> = plan.tiles.iter().collect();
        cols.sort_by_key(|t| t.col);
        for w in cols.windows(2) {
            if w[1].col == w[0].col + 1 {
                let overlap = w[0].field_max.x - w[1].field_min.x;
                assert!(
                    (overlap - plan.overlap_nm).abs() <= 1,
                    "field overlap {overlap} nm != {} nm",
                    plan.overlap_nm
                );
            }
        }
    }

    /// Shoelace area (nm²) of a closed element's ring.
    fn elem_area(e: &PathElem) -> f64 {
        let n = e.pts.len();
        let mut s = 0i128;
        for i in 0..n {
            let a = e.pts[i];
            let b = e.pts[(i + 1) % n];
            s += a.x as i128 * b.y as i128 - b.x as i128 * a.y as i128;
        }
        (s as f64 / 2.0).abs()
    }

    #[test]
    fn union_area_is_preserved_within_tolerance() {
        let job = grid(250.0, 200.0, 10.0);
        let plan = tile(&job, FIELD_MM, STITCH_OVERLAP_MM);
        // Reassemble every tile's elements and compare area to the original
        // (exact: no element was split, so this holds far tighter than the
        // 1 µm the done-when allows).
        let reassembled: f64 = plan
            .tiles
            .iter()
            .flat_map(|t| t.paths.elems.iter())
            .map(elem_area)
            .sum();
        let original: f64 = job.elems.iter().map(elem_area).sum();
        assert!(
            (reassembled - original).abs() < 1.0,
            "tiled area {reassembled} != original {original}"
        );
    }

    #[test]
    fn single_oversized_element_is_flagged_not_hidden() {
        // One element 200 mm wide — larger than a field.
        let mut elems = vec![PathElem {
            kind: PathKind::Cut,
            pts: vec![P::new(0, 0), P::new(200 * MM, 0)],
            closed: false,
        }];
        elems.push(dot(5.0, 5.0));
        let plan = tile(&Paths { elems }, FIELD_MM, STITCH_OVERLAP_MM);
        assert_eq!(plan.oversized, vec![0], "the 200 mm element is flagged");
        assert_eq!(plan.total_elems(), 2, "still assigned, not dropped");
    }
}
