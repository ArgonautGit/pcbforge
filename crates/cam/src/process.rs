//! CAM-6 — process compilers: mask-open, legend, stencil, drill-map.
//!
//! Four small compilers over already-ingested [`Layer`]s (cam stays
//! ingest-free at the production level; only the fixture tests pull in
//! `ingest` as a dev-dependency):
//!
//! * [`mask_open`] — solder-mask openings as filled regions: one exact
//!   design-edge [`PathKind::Boundary`] contour per opening ring plus a
//!   hatch fill of each opening.
//! * [`legend`] — silkscreen artwork as raster fills, same geometry
//!   treatment as `mask_open`. "Low power" is a process *parameter*, not
//!   geometry: laser params attach at emit time (CAM-4/CAM-5 job
//!   assembly), keyed by process and [`PathKind`]; this module only shapes
//!   and tags paths.
//! * [`stencil`] — paste apertures as cut contours only (no fill).
//! * [`drill_map`] — drill centers as a JSON list for the printed drill
//!   guide; no laser output, so no [`Paths`] involved.
//!
//! # Tag choices
//!
//! [`PathKind`] carries no process identity, so the compilers use it as
//! follows (documented contract for the emit stage):
//!
//! * Contours are always `Boundary` — the exact design edge, as in the UV
//!   finishing set.
//! * Fill hatches are `Rubout(set)` with the *set index doubling as the
//!   process discriminator*: `Rubout(0)` = mask-open fill, `Rubout(1)` =
//!   legend fill. Within one process output only one set is emitted, so
//!   the index is unambiguous even when jobs are merged.
//!
//! # Hatch reuse
//!
//! The hatch generator lives privately inside [`ablation`]; rather than
//! duplicating its rotate/scanline conventions, the fill compilers reuse
//! [`ablation::ablation_paths`] through a synthetic [`CamOpts`]: with
//! `n_contours = 0`, `clearance_mm = -W` and `band_mm = W` (for `W` large
//! enough to erode the layer to nothing — half its bounding-box major
//! dimension plus 1 mm), [`ablation::rubout_band`] degenerates to
//! `xor(offset(polys, 0), offset(polys, -W)) = xor(polys, ∅) = polys`,
//! i.e. the fill region *is* the layer itself. Hatches therefore carry
//! exactly ablation's spacing (`opts.interval_mm`), angle
//! (`opts.base_angle_deg`), minimum-segment, and rounding conventions.

use crate::ablation;
use pcb_core::{CamOpts, Layer, NM_PER_MM, Nm, P, PathElem, PathKind, Paths, Poly};

/// Compile solder-mask openings (the mask layer's polygons are the
/// openings) into a fill job: for each opening, one closed
/// [`PathKind::Boundary`] element per ring (outer and holes, exact design
/// edge, vertices untouched), followed by a `Rubout(0)` hatch fill of the
/// whole layer at `opts.interval_mm` spacing and `opts.base_angle_deg`.
///
/// Element order: all boundaries (layer order), then all hatches.
pub fn mask_open(mask_layer: &Layer, opts: &CamOpts) -> Paths {
    filled_process(mask_layer, opts, 0)
}

/// Compile silkscreen artwork into a raster-fill job. Geometry and element
/// order are identical to [`mask_open`]; hatches are tagged `Rubout(1)` so
/// legend fill is distinguishable from mask fill (module docs). Low-power
/// legend parameters attach at emit time — they are not a geometry concern.
pub fn legend(silk_layer: &Layer, opts: &CamOpts) -> Paths {
    filled_process(silk_layer, opts, 1)
}

/// Compile paste apertures into stencil cut contours: one closed
/// [`PathKind::Boundary`] element per polygon ring (outers and holes),
/// vertices untouched. No fill — a stencil aperture is cut, not cleared.
pub fn stencil(paste_layer: &Layer) -> Paths {
    Paths {
        elems: boundary_elems(&paste_layer.polys),
    }
}

/// One entry of the drill guide: a round hole, or (with `slot_end`) an
/// oval slot from `(x_nm, y_nm)` to `slot_end` at the tool diameter.
///
/// Defined here (not in `ingest`) so cam stays ingest-free; callers map
/// their drill source (e.g. `ingest`'s Excellon ops) into this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrillEntry {
    pub x_nm: i64,
    pub y_nm: i64,
    pub diameter_nm: i64,
    pub slot_end: Option<(i64, i64)>,
}

/// Serialize drill entries as JSON for the drill guide (no laser output).
///
/// Schema — a flat array, one object per entry, all lengths integer
/// nanometers:
///
/// ```json
/// [
///   {"x_nm": 110000000, "y_nm": -100000000, "diameter_nm": 400000, "slot_end": null},
///   {"x_nm": 105080000, "y_nm": -99800000, "diameter_nm": 1000000, "slot_end": [105080000, -100200000]}
/// ]
/// ```
///
/// `slot_end` is `null` for a round hole, or the `[x_nm, y_nm]` far end of
/// a slot (the near end is the entry's own coordinates). Hand-rolled —
/// i64 → JSON number is lossless in text form, and the flat shape does not
/// justify a serde dependency. Entries keep input order. The result ends
/// with a newline.
pub fn drill_map(entries: &[DrillEntry]) -> String {
    if entries.is_empty() {
        return "[]\n".to_string();
    }
    let mut out = String::from("[\n");
    for (i, e) in entries.iter().enumerate() {
        let slot = match e.slot_end {
            Some((x, y)) => format!("[{x},{y}]"),
            None => "null".to_string(),
        };
        let comma = if i + 1 < entries.len() { "," } else { "" };
        out.push_str(&format!(
            "  {{\"x_nm\":{},\"y_nm\":{},\"diameter_nm\":{},\"slot_end\":{}}}{}\n",
            e.x_nm, e.y_nm, e.diameter_nm, slot, comma
        ));
    }
    out.push_str("]\n");
    out
}

// ---------------------------------------------------------------------------
// Shared machinery
// ---------------------------------------------------------------------------

/// Boundary contours + `Rubout(set)` hatch fill of the layer's polygons.
fn filled_process(layer: &Layer, opts: &CamOpts, set: u32) -> Paths {
    let mut elems = boundary_elems(&layer.polys);
    elems.extend(fill_hatch_elems(layer, opts, set));
    Paths { elems }
}

/// One closed `Boundary` element per ring (outer and holes), vertices
/// passed through verbatim. Rings with fewer than 3 vertices are dropped.
fn boundary_elems(polys: &[Poly]) -> Vec<PathElem> {
    let mut elems = Vec::new();
    for poly in polys {
        for ring in std::iter::once(&poly.outer).chain(poly.holes.iter()) {
            if ring.len() >= 3 {
                elems.push(PathElem {
                    kind: PathKind::Boundary,
                    pts: ring.clone(),
                    closed: true,
                });
            }
        }
    }
    elems
}

/// Hatch-fill the layer's own region by reusing [`ablation::ablation_paths`]
/// with the synthetic rub-out construction from the module docs, retagging
/// the resulting segments `Rubout(set)`.
fn fill_hatch_elems(layer: &Layer, opts: &CamOpts, set: u32) -> Vec<PathElem> {
    let Some(w_mm) = collapse_offset_mm(layer) else {
        return Vec::new();
    };
    let synthetic = CamOpts {
        n_contours: 0,
        clearance_mm: -w_mm,
        band_mm: w_mm,
        ..opts.clone()
    };
    let mut elems = ablation::ablation_paths(layer, &synthetic, 1).elems;
    debug_assert!(
        elems
            .iter()
            .all(|e| e.kind == PathKind::Rubout(0) && !e.closed)
    );
    if set != 0 {
        for e in &mut elems {
            e.kind = PathKind::Rubout(set);
        }
    }
    elems
}

/// An erosion distance (mm) guaranteed to collapse the layer to nothing:
/// half the bounding box's major dimension plus 1 mm (any inscribed disc
/// radius is bounded by half the *minor* dimension). `None` for a layer
/// with no vertices.
fn collapse_offset_mm(layer: &Layer) -> Option<f64> {
    let mut lo = P::new(Nm::MAX, Nm::MAX);
    let mut hi = P::new(Nm::MIN, Nm::MIN);
    let mut any = false;
    for poly in &layer.polys {
        for ring in std::iter::once(&poly.outer).chain(poly.holes.iter()) {
            for p in ring {
                any = true;
                lo = P::new(lo.x.min(p.x), lo.y.min(p.y));
                hi = P::new(hi.x.max(p.x), hi.y.max(p.y));
            }
        }
    }
    if !any {
        return None;
    }
    let major_nm = (hi.x - lo.x).max(hi.y - lo.y);
    Some(major_nm as f64 / (2.0 * NM_PER_MM as f64) + 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ablation::point_in_polys;

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

    fn as_hole(mut r: Vec<P>) -> Vec<P> {
        r.reverse();
        r
    }

    fn kinds(paths: &Paths, kind: PathKind) -> Vec<&PathElem> {
        paths.elems.iter().filter(|e| e.kind == kind).collect()
    }

    #[test]
    fn mask_open_square_has_exact_boundary_and_full_hatch_fill() {
        // One 2 mm square opening at (0,0)..(2,2).
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 2.0, 2.0)],
        };
        let opts = CamOpts::default(); // interval 0.03 mm, base angle 0
        let paths = mask_open(&layer, &opts);

        let bounds = kinds(&paths, PathKind::Boundary);
        assert_eq!(bounds.len(), 1);
        assert!(bounds[0].closed);
        // Exact design edge: input vertices verbatim.
        assert_eq!(bounds[0].pts, layer.polys[0].outer);

        // Hatches: ablation's scanline convention over the square itself —
        // horizontal lines at y = interval/2 + k*interval for y < 2 mm,
        // i.e. 15 µm + k·30 µm < 2 mm → k = 0..=66 → 67 segments.
        let hatches = kinds(&paths, PathKind::Rubout(0));
        assert_eq!(hatches.len(), 67);
        for h in &hatches {
            assert!(!h.closed);
            assert_eq!(h.pts.len(), 2);
            assert_eq!(h.pts[0].y, h.pts[1].y, "base angle 0 → horizontal");
            let mid = P::new((h.pts[0].x + h.pts[1].x) / 2, h.pts[0].y);
            assert!(point_in_polys(mid, &layer.polys, 10));
        }
        // Order: boundaries first, then hatches.
        assert_eq!(paths.elems[0].kind, PathKind::Boundary);
        assert_eq!(paths.elems.len(), 68);
    }

    #[test]
    fn mask_open_hatches_every_disjoint_opening() {
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 1.0, 1.0), rect_mm(5.0, 3.0, 6.5, 4.0)],
        };
        let paths = mask_open(&layer, &CamOpts::default());
        assert_eq!(kinds(&paths, PathKind::Boundary).len(), 2);
        for poly in &layer.polys {
            let n = paths
                .elems
                .iter()
                .filter(|e| e.kind == PathKind::Rubout(0))
                .filter(|e| {
                    let mid = P::new((e.pts[0].x + e.pts[1].x) / 2, (e.pts[0].y + e.pts[1].y) / 2);
                    point_in_polys(mid, std::slice::from_ref(poly), 10)
                })
                .count();
            assert!(n >= 1, "opening without fill: {poly:?}");
        }
    }

    #[test]
    fn legend_is_mask_geometry_with_the_legend_fill_tag() {
        let layer = Layer {
            polys: vec![rect_mm(0.0, 0.0, 1.0, 0.5)],
        };
        let opts = CamOpts::default();
        let leg = legend(&layer, &opts);
        assert!(!kinds(&leg, PathKind::Rubout(1)).is_empty());
        assert!(kinds(&leg, PathKind::Rubout(0)).is_empty());
        assert_eq!(kinds(&leg, PathKind::Boundary).len(), 1);
        // Same geometry as mask_open, only the fill tag differs.
        let mask = mask_open(&layer, &opts);
        assert_eq!(leg.elems.len(), mask.elems.len());
        for (a, b) in leg.elems.iter().zip(&mask.elems) {
            assert_eq!(a.pts, b.pts);
            assert_eq!(a.closed, b.closed);
        }
    }

    #[test]
    fn hatch_respects_holes_in_a_filled_region() {
        // Legend/mask fill must not paint inside a hole.
        let poly = Poly {
            outer: rect_mm(0.0, 0.0, 3.0, 3.0).outer,
            holes: vec![as_hole(rect_mm(1.0, 1.0, 2.0, 2.0).outer)],
        };
        let layer = Layer {
            polys: vec![poly.clone()],
        };
        let paths = mask_open(&layer, &CamOpts::default());
        // Outer + hole ring → 2 boundaries.
        assert_eq!(kinds(&paths, PathKind::Boundary).len(), 2);
        let hole_center = P::new(MM * 3 / 2, MM * 3 / 2);
        for e in kinds(&paths, PathKind::Rubout(0)) {
            let mid = P::new((e.pts[0].x + e.pts[1].x) / 2, (e.pts[0].y + e.pts[1].y) / 2);
            assert!(point_in_polys(mid, &layer.polys, 10));
            assert_ne!(mid, hole_center);
        }
    }

    #[test]
    fn stencil_emits_one_closed_boundary_per_ring_and_nothing_else() {
        let with_hole = Poly {
            outer: rect_mm(0.0, 0.0, 3.0, 3.0).outer,
            holes: vec![as_hole(rect_mm(1.0, 1.0, 2.0, 2.0).outer)],
        };
        let layer = Layer {
            polys: vec![with_hole.clone(), rect_mm(5.0, 0.0, 6.0, 1.0)],
        };
        let paths = stencil(&layer);
        assert_eq!(paths.elems.len(), 3, "2 rings + 1 ring");
        assert!(
            paths
                .elems
                .iter()
                .all(|e| e.kind == PathKind::Boundary && e.closed)
        );
        assert_eq!(paths.elems[0].pts, with_hole.outer);
        assert_eq!(paths.elems[1].pts, with_hole.holes[0]);
    }

    #[test]
    fn empty_layers_compile_to_empty_jobs() {
        let empty = Layer::default();
        let opts = CamOpts::default();
        assert!(mask_open(&empty, &opts).elems.is_empty());
        assert!(legend(&empty, &opts).elems.is_empty());
        assert!(stencil(&empty).elems.is_empty());
    }

    #[test]
    fn drill_map_json_shape_is_exact() {
        let entries = [
            DrillEntry {
                x_nm: 110_000_000,
                y_nm: -100_000_000,
                diameter_nm: 400_000,
                slot_end: None,
            },
            DrillEntry {
                x_nm: 105_080_000,
                y_nm: -99_800_000,
                diameter_nm: 1_000_000,
                slot_end: Some((105_080_000, -100_200_000)),
            },
        ];
        let json = drill_map(&entries);
        assert_eq!(
            json,
            "[\n  {\"x_nm\":110000000,\"y_nm\":-100000000,\"diameter_nm\":400000,\
             \"slot_end\":null},\n  {\"x_nm\":105080000,\"y_nm\":-99800000,\
             \"diameter_nm\":1000000,\"slot_end\":[105080000,-100200000]}\n]\n"
        );
        assert_eq!(drill_map(&[]), "[]\n");
    }
}
