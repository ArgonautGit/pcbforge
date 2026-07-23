//! Drill-hole geometry: [`DrillEntry`] lists → hole outline polygons.
//!
//! Extracts *pure* hole geometry from a board's drill data — every entry
//! becomes one closed outline `Poly`, nothing else — so downstream consumers
//! (the LightBurn emitter, previews) can burn, trace, or draw the drill
//! pattern without caring where it came from.
//!
//! * A round hole becomes a regular-polygon circle approximation with
//!   vertices *on* the ideal circle ([`crate::features::circle_segments`]
//!   chord-error bound), rounded to the nm grid.
//! * A G85 slot becomes a capsule (stadium): a semicircular cap at each end
//!   joined by two straight sides — the outline swept by the drill bit
//!   travelling between the slot ends.
//!
//! Rings wind counter-clockwise (positive shoelace area) in the source frame.
//! Coordinates pass through verbatim: frame normalization and placement stay
//! the emitter's concern (`lbrn2::normalize_frame` / `lbrn2::place_frame`).

use pcb_core::{NM_PER_MM, P, Poly};

use crate::features::circle_segments;
use crate::process::DrillEntry;

/// Hole outline polygons for `entries`: one single-ring `Poly` per entry, in
/// input order.
///
/// Entries with a non-positive diameter are skipped (a zero-width hole has
/// no outline); a slot whose ends coincide degenerates to its round hole.
pub fn drill_polys(entries: &[DrillEntry]) -> Vec<Poly> {
    entries
        .iter()
        .filter(|e| e.diameter_nm > 0)
        .map(|e| {
            let r_mm = e.diameter_nm as f64 / (2.0 * NM_PER_MM as f64);
            let center = (nm_to_mm(e.x_nm), nm_to_mm(e.y_nm));
            let outer = match e.slot_end {
                Some(end) if end != (e.x_nm, e.y_nm) => {
                    capsule_ring(center, (nm_to_mm(end.0), nm_to_mm(end.1)), r_mm)
                }
                _ => circle_ring(center, r_mm),
            };
            Poly {
                outer,
                holes: vec![],
            }
        })
        .collect()
}

fn nm_to_mm(nm: i64) -> f64 {
    nm as f64 / NM_PER_MM as f64
}

/// A CCW regular-polygon circle: [`circle_segments`] vertices on the ideal
/// circle, rounded to the nm grid.
fn circle_ring((cx, cy): (f64, f64), r_mm: f64) -> Vec<P> {
    let n = circle_segments(r_mm);
    (0..n)
        .map(|i| {
            let t = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
            P::from_mm(cx + r_mm * t.cos(), cy + r_mm * t.sin())
        })
        .collect()
}

/// A CCW capsule from `a` to `b` at tool radius `r_mm`: the two straight
/// sides come free from ring closure between the cap endpoints.
fn capsule_ring(a: (f64, f64), b: (f64, f64), r_mm: f64) -> Vec<P> {
    use std::f64::consts::PI;
    let theta = (b.1 - a.1).atan2(b.0 - a.0);
    // Split a full circle's segment budget across the two caps.
    let k = circle_segments(r_mm).div_ceil(2);
    let cap = |(cx, cy): (f64, f64), from: f64| {
        (0..=k).map(move |i| {
            let t = from + PI * i as f64 / k as f64;
            P::from_mm(cx + r_mm * t.cos(), cy + r_mm * t.sin())
        })
    };
    // The b cap sweeps θ−90°→θ+90° (through the far end), then the a cap
    // sweeps θ+90°→θ+270° (through the near end): counter-clockwise overall,
    // matching the circle winding.
    cap(b, theta - PI / 2.0)
        .chain(cap(a, theta + PI / 2.0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MM: i64 = NM_PER_MM;

    fn entry(x_mm: f64, y_mm: f64, d_mm: f64) -> DrillEntry {
        DrillEntry {
            x_nm: (x_mm * MM as f64).round() as i64,
            y_nm: (y_mm * MM as f64).round() as i64,
            diameter_nm: (d_mm * MM as f64).round() as i64,
            slot_end: None,
        }
    }

    fn slot(x_mm: f64, y_mm: f64, ex_mm: f64, ey_mm: f64, d_mm: f64) -> DrillEntry {
        DrillEntry {
            slot_end: Some((
                (ex_mm * MM as f64).round() as i64,
                (ey_mm * MM as f64).round() as i64,
            )),
            ..entry(x_mm, y_mm, d_mm)
        }
    }

    /// Signed shoelace area, mm² (positive = CCW).
    fn area_mm2(ring: &[P]) -> f64 {
        let mut acc = 0.0;
        for (i, p) in ring.iter().enumerate() {
            let q = &ring[(i + 1) % ring.len()];
            acc += p.x_mm() * q.y_mm() - q.x_mm() * p.y_mm();
        }
        acc / 2.0
    }

    #[test]
    fn round_hole_is_a_ccw_circle_on_the_ideal_radius() {
        // KiCad drill frame: negative y, like a real Excellon export.
        let polys = drill_polys(&[entry(110.0, -100.0, 1.0)]);
        assert_eq!(polys.len(), 1);
        let ring = &polys[0].outer;
        assert!(ring.len() >= 16, "even small holes stay round");
        for p in ring {
            let d = ((p.x_mm() - 110.0).powi(2) + (p.y_mm() + 100.0).powi(2)).sqrt();
            assert!((d - 0.5).abs() < 1e-5, "vertex on the ideal circle: {d}");
        }
        let area = area_mm2(ring);
        assert!(area > 0.0, "outer ring winds CCW");
        // An inscribed n-gon under-covers by at most n·chord-error·r-ish;
        // with e = 2 µm this is far inside 1 %.
        let ideal = std::f64::consts::PI * 0.25;
        assert!(
            (area - ideal).abs() / ideal < 0.01,
            "area {area} vs {ideal}"
        );
    }

    #[test]
    fn slot_becomes_a_ccw_capsule() {
        // Horizontal 4 mm center-to-center slot, 2 mm tool.
        let polys = drill_polys(&[slot(10.0, 5.0, 14.0, 5.0, 2.0)]);
        let ring = &polys[0].outer;
        // Capsule area = circle + rect: π·r² + 2r·L.
        let ideal = std::f64::consts::PI + 8.0;
        let area = area_mm2(ring);
        assert!(area > 0.0, "capsule winds CCW");
        assert!(
            (area - ideal).abs() / ideal < 0.01,
            "area {area} vs {ideal}"
        );
        // Bounding box: [9, 15] × [4, 6], within the 2 µm chord-error bound
        // (vertices sit ON the circle, so extremes may fall a sagitta short).
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in ring {
            min_x = min_x.min(p.x_mm());
            max_x = max_x.max(p.x_mm());
            min_y = min_y.min(p.y_mm());
            max_y = max_y.max(p.y_mm());
        }
        assert!((min_x - 9.0).abs() < 0.003 && (max_x - 15.0).abs() < 0.003);
        assert!((min_y - 4.0).abs() < 0.003 && (max_y - 6.0).abs() < 0.003);
    }

    #[test]
    fn angled_slot_matches_the_horizontal_one() {
        // Same length and tool at 45°: area is rotation-invariant.
        let l = 4.0 / std::f64::consts::SQRT_2;
        let flat = drill_polys(&[slot(0.0, 0.0, 4.0, 0.0, 2.0)]);
        let tilted = drill_polys(&[slot(1.0, 1.0, 1.0 + l, 1.0 + l, 2.0)]);
        let a0 = area_mm2(&flat[0].outer);
        let a1 = area_mm2(&tilted[0].outer);
        assert!((a0 - a1).abs() / a0 < 1e-3, "flat {a0} vs tilted {a1}");
    }

    #[test]
    fn degenerate_slot_is_the_round_hole() {
        let round = drill_polys(&[entry(3.0, 4.0, 0.8)]);
        let degenerate = drill_polys(&[slot(3.0, 4.0, 3.0, 4.0, 0.8)]);
        assert_eq!(round, degenerate);
    }

    #[test]
    fn nonpositive_diameters_are_skipped() {
        let polys = drill_polys(&[
            entry(0.0, 0.0, 0.0),
            DrillEntry {
                diameter_nm: -5,
                ..entry(1.0, 1.0, 1.0)
            },
            entry(2.0, 2.0, 0.3),
        ]);
        assert_eq!(polys.len(), 1, "only the real hole survives");
    }
}
