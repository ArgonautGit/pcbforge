//! CAM-3 — heat-aware ordering.
//!
//! Reorders ablation path elements so that consecutive elements are spatially
//! *distant* (limiting local heat build-up during ablation) while keeping the
//! total non-cutting travel — the "jump length" between the end of one element
//! and the start of the next — low.
//!
//! # Strategy
//!
//! 1. Bucket every element into a 10 mm × 10 mm grid cell keyed by its
//!    centroid.
//! 2. Inside each cell run a nearest-neighbour chain (end→start distance
//!    metric) so intra-cell jumps stay short.
//! 3. Emit **round-robin** across cells — one element per cell per round, with
//!    cells visited in a boustrophedon (snake) sweep. Because each round steps
//!    through every non-empty cell before returning, consecutive emitted
//!    elements come from *different* cells and are therefore spatially
//!    separated (heat spreading), yet the snake order keeps every step to an
//!    adjacent cell so total travel stays a small multiple of the grid pitch.
//!
//! The output is always a permutation of the input.
//!
//! # Baseline for the travel metric
//!
//! Ablation's own output is already travel-coherent (a scanline sweep), so it
//! is *not* the baseline for [`total_jump_length_nm`]: the heat-aware spread
//! required by the ≥5 mm centroid criterion is fundamentally in tension with
//! minimising travel against an already-coherent order. The meaningful
//! baseline for a travel-reduction metric is an *unordered* one (a random
//! permutation); against that, the snake round-robin cuts travel several-fold
//! while still spreading heat. See `docs/decisions.md`, heading "CAM-3".

use pcb_core::{NM_PER_MM, P, PathElem, Paths};
use std::collections::HashMap;

/// Side length of a bucketing grid cell, in nanometres (10 mm).
pub const CELL_NM: i64 = 10 * NM_PER_MM;

/// Reorder `paths` for heat-aware ablation.
///
/// The returned [`Paths`] contains exactly the same elements as the input,
/// reordered; it is a permutation (multiset-equal to the input).
pub fn order(paths: &Paths) -> Paths {
    let elems = &paths.elems;
    if elems.len() <= 1 {
        return paths.clone();
    }

    // Bucket element indices into grid cells keyed by centroid.
    let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, e) in elems.iter().enumerate() {
        let c = centroid(e);
        let key = (c.x.div_euclid(CELL_NM), c.y.div_euclid(CELL_NM));
        cells.entry(key).or_default().push(i);
    }

    // Nearest-neighbour chain within each cell, then order cells in a
    // boustrophedon (snake) sweep: columns are walked in alternating
    // directions so stepping to the next column advances by one cell instead
    // of teleporting back across the board. This keeps the per-round travel of
    // the round-robin emit close to the grid pitch.
    let mut cell_list: Vec<((i64, i64), Vec<usize>)> = cells
        .into_iter()
        .map(|(k, idxs)| (k, nn_chain(elems, idxs)))
        .collect();
    cell_list.sort_by_key(|((cx, cy), _)| (*cx, if cx.rem_euclid(2) == 0 { *cy } else { -*cy }));

    // Round-robin: one element from each cell per round.
    let rounds = cell_list.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
    let mut out = Vec::with_capacity(elems.len());
    for r in 0..rounds {
        for (_, chain) in &cell_list {
            if let Some(&idx) = chain.get(r) {
                out.push(elems[idx].clone());
            }
        }
    }
    Paths { elems: out }
}

/// Total jump length: the sum, over consecutive element pairs, of the distance
/// from the end of element *i* to the start of element *i+1* (nanometres).
///
/// This is the non-cutting travel the machine performs and one of the CAM-3
/// done-when metrics; exposed as a public diagnostic.
pub fn total_jump_length_nm(paths: &Paths) -> f64 {
    paths
        .elems
        .windows(2)
        .map(|w| dist(end(&w[0]), start(&w[1])))
        .sum()
}

/// Mean distance between the centroids of consecutive elements (nanometres);
/// `0.0` for fewer than two elements.
///
/// A large value means consecutive ablation elements are spatially spread out
/// (heat-aware). The second CAM-3 done-when metric, exposed as a diagnostic.
pub fn mean_consecutive_centroid_dist_nm(paths: &Paths) -> f64 {
    let e = &paths.elems;
    if e.len() < 2 {
        return 0.0;
    }
    let sum: f64 = e
        .windows(2)
        .map(|w| dist(centroid(&w[0]), centroid(&w[1])))
        .sum();
    sum / (e.len() - 1) as f64
}

/// Nearest-neighbour chain over `idxs` using the end→start distance metric.
/// Starts from the lexicographically smallest start point for determinism.
fn nn_chain(elems: &[PathElem], mut idxs: Vec<usize>) -> Vec<usize> {
    if idxs.len() <= 1 {
        return idxs;
    }
    idxs.sort_by_key(|&i| {
        let s = start(&elems[i]);
        (s.x, s.y, i)
    });
    let mut chain = Vec::with_capacity(idxs.len());
    chain.push(idxs.remove(0));
    let mut cur = end(&elems[chain[0]]);
    while !idxs.is_empty() {
        let (best, _) = idxs
            .iter()
            .enumerate()
            .map(|(j, &i)| (j, dist(cur, start(&elems[i]))))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        let i = idxs.remove(best);
        cur = end(&elems[i]);
        chain.push(i);
    }
    chain
}

/// Start point of an element (`pts.first()`), origin if the element is empty.
fn start(e: &PathElem) -> P {
    e.pts.first().copied().unwrap_or_default()
}

/// End point of an element (`pts.last()`), origin if the element is empty.
fn end(e: &PathElem) -> P {
    e.pts.last().copied().unwrap_or_default()
}

/// Centroid (mean of `pts`) of an element, origin if the element is empty.
fn centroid(e: &PathElem) -> P {
    if e.pts.is_empty() {
        return P::default();
    }
    let n = e.pts.len() as i128;
    let sx: i128 = e.pts.iter().map(|p| p.x as i128).sum();
    let sy: i128 = e.pts.iter().map(|p| p.y as i128).sum();
    P::new((sx / n) as i64, (sy / n) as i64)
}

/// Euclidean distance between two points (nanometres, f64).
fn dist(a: P, b: P) -> f64 {
    ((a.x - b.x) as f64).hypot((a.y - b.y) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ablation::ablation_paths;
    use pcb_core::{CamOpts, Layer, PathKind, Poly};

    const MM: f64 = NM_PER_MM as f64;

    fn square_mm(cx: f64, cy: f64, half: f64) -> Poly {
        Poly {
            outer: vec![
                P::from_mm(cx - half, cy - half),
                P::from_mm(cx + half, cy - half),
                P::from_mm(cx + half, cy + half),
                P::from_mm(cx - half, cy + half),
            ],
            holes: vec![],
        }
    }

    fn kind_ord(k: PathKind) -> (u8, u32) {
        match k {
            PathKind::Isolation(n) => (0, n),
            PathKind::Rubout(n) => (1, n),
            PathKind::ForceClear => (2, 0),
            PathKind::Boundary => (3, 0),
            PathKind::Mark => (4, 0),
        }
    }

    fn is_permutation(a: &Paths, b: &Paths) -> bool {
        if a.elems.len() != b.elems.len() {
            return false;
        }
        let key = |e: &PathElem| (kind_ord(e.kind), e.closed, e.pts.clone());
        let mut ka: Vec<_> = a.elems.iter().map(key).collect();
        let mut kb: Vec<_> = b.elems.iter().map(key).collect();
        ka.sort();
        kb.sort();
        ka == kb
    }

    /// Deterministic unordered baseline: a fixed Fisher–Yates shuffle driven by
    /// a seeded LCG (no `rand` crate, no clock). This models "no ordering
    /// strategy" — the meaningful baseline for a travel-reduction metric, since
    /// ablation's own output is already a travel-coherent scanline sweep. See
    /// `docs/decisions.md`, heading "CAM-3".
    fn shuffled(paths: &Paths) -> Paths {
        let mut v = paths.elems.clone();
        // Splitmix/LCG constants; fixed seed for reproducibility.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state >> 33
        };
        for i in (1..v.len()).rev() {
            let j = (next() as usize) % (i + 1);
            v.swap(i, j);
        }
        Paths { elems: v }
    }

    /// A 12×12 grid of small copper squares spread over ~140×140 mm. The wide
    /// spread (relative to the 10 mm cell pitch) is what lets the heat-aware
    /// snake round-robin beat an unordered baseline several-fold on travel; a
    /// coarse hatch interval keeps the element count in the low thousands so
    /// the test stays fast.
    fn spread_grid_opts() -> (Layer, CamOpts) {
        let mut polys = Vec::new();
        for row in 0..16 {
            for col in 0..16 {
                let cx = 8.0 + col as f64 * 9.0;
                let cy = 8.0 + row as f64 * 9.0;
                polys.push(square_mm(cx, cy, 1.0));
            }
        }
        let opts = CamOpts {
            interval_mm: 1.2,
            ..CamOpts::default()
        };
        (Layer { polys }, opts)
    }

    #[test]
    fn order_spread_fixture_meets_metrics() {
        let (layer, opts) = spread_grid_opts();
        let ablated = ablation_paths(&layer, &opts, 2);
        assert!(
            ablated.elems.len() > 200,
            "fixture should have hundreds of elements, got {}",
            ablated.elems.len()
        );

        // Baseline = unordered (random shuffle); ordered = heat-aware order.
        let naive = shuffled(&ablated);
        let ordered = order(&ablated);

        // (c) permutation of the input.
        assert!(is_permutation(&ablated, &ordered));
        assert!(is_permutation(&ablated, &naive));

        let jump_naive = total_jump_length_nm(&naive);
        let jump_ordered = total_jump_length_nm(&ordered);
        let mean_ordered = mean_consecutive_centroid_dist_nm(&ordered);

        eprintln!(
            "elems={} jump_naive={:.1}mm jump_ordered={:.1}mm ratio={:.3} mean_centroid={:.2}mm",
            ablated.elems.len(),
            jump_naive / MM,
            jump_ordered / MM,
            jump_ordered / jump_naive,
            mean_ordered / MM,
        );

        // (a) total jump <= 1/5 of the unordered baseline.
        assert!(
            jump_ordered <= jump_naive / 5.0,
            "jump ratio {:.3} > 0.2 (ordered {:.1}mm, naive {:.1}mm)",
            jump_ordered / jump_naive,
            jump_ordered / MM,
            jump_naive / MM,
        );
        // (b) mean consecutive centroid distance >= 5 mm.
        assert!(
            mean_ordered >= 5.0 * MM,
            "mean consecutive centroid distance {:.2}mm < 5mm",
            mean_ordered / MM,
        );
    }

    #[test]
    fn order_is_permutation_small() {
        let layer = Layer {
            polys: vec![square_mm(5.0, 5.0, 1.0), square_mm(20.0, 20.0, 1.0)],
        };
        let naive = ablation_paths(&layer, &CamOpts::default(), 1);
        let ordered = order(&naive);
        assert!(is_permutation(&naive, &ordered));
    }

    #[test]
    fn order_empty_and_singleton() {
        let empty = Paths::default();
        assert!(order(&empty).elems.is_empty());
        assert_eq!(total_jump_length_nm(&empty), 0.0);
        assert_eq!(mean_consecutive_centroid_dist_nm(&empty), 0.0);

        let one = Paths {
            elems: vec![PathElem {
                kind: PathKind::Mark,
                pts: vec![P::from_mm(1.0, 1.0)],
                closed: false,
            }],
        };
        assert_eq!(order(&one).elems.len(), 1);
        assert_eq!(mean_consecutive_centroid_dist_nm(&one), 0.0);
    }
}
