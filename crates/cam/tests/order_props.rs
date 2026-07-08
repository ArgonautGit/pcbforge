//! CAM-3 property tests: `order` output is a permutation of its input.

use cam::order::order;
use pcb_core::{P, PathElem, PathKind, Paths};
use proptest::prelude::*;

fn kind_ord(k: PathKind) -> (u8, u32) {
    match k {
        PathKind::Isolation(n) => (0, n),
        PathKind::Rubout(n) => (1, n),
        PathKind::ForceClear => (2, 0),
        PathKind::Boundary => (3, 0),
        PathKind::Mark => (4, 0),
    }
}

/// Canonical, order-independent key for an element.
fn elem_key(e: &PathElem) -> ((u8, u32), bool, Vec<(i64, i64)>) {
    (
        kind_ord(e.kind),
        e.closed,
        e.pts.iter().map(|p| (p.x, p.y)).collect(),
    )
}

fn arb_point() -> impl Strategy<Value = P> {
    // ±200 mm in nanometres.
    (-200_000_000i64..200_000_000, -200_000_000i64..200_000_000).prop_map(|(x, y)| P::new(x, y))
}

fn arb_kind() -> impl Strategy<Value = PathKind> {
    prop_oneof![
        (0u32..4).prop_map(PathKind::Isolation),
        (0u32..4).prop_map(PathKind::Rubout),
        Just(PathKind::ForceClear),
        Just(PathKind::Boundary),
        Just(PathKind::Mark),
    ]
}

fn arb_elem() -> impl Strategy<Value = PathElem> {
    (
        arb_kind(),
        prop::collection::vec(arb_point(), 1..8),
        any::<bool>(),
    )
        .prop_map(|(kind, pts, closed)| PathElem { kind, pts, closed })
}

fn arb_paths() -> impl Strategy<Value = Paths> {
    prop::collection::vec(arb_elem(), 0..200).prop_map(|elems| Paths { elems })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// (c) Every input element appears exactly once in the output.
    #[test]
    fn order_is_a_permutation(paths in arb_paths()) {
        let out = order(&paths);
        prop_assert_eq!(out.elems.len(), paths.elems.len());

        let mut a: Vec<_> = paths.elems.iter().map(elem_key).collect();
        let mut b: Vec<_> = out.elems.iter().map(elem_key).collect();
        a.sort();
        b.sort();
        prop_assert_eq!(a, b);
    }
}
