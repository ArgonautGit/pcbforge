//! QA-1 — extended proptest coverage for the CAM geometry pipeline.
//!
//! Four adversarial property families, each disjoint from the existing suites:
//!
//! 1. **Boolean / offset round-trips at extreme aspect ratios and
//!    near-degenerate slivers** — 100:1‥1000:1 thin rectangles and razor
//!    triangles. `union` idempotence (area-preserving), self-`difference` and
//!    self-`xor` empty, and dilate-then-erode area recovery on shapes whose
//!    short dimension is microns.
//! 2. **`force_clear` completeness on adversarial necks** — a two-neck bent
//!    ("S-curve") chain and a linearly tapered neck. Every sub-`min_feature`
//!    stretch gets a centerline that lies inside the copper region; the
//!    comfortably-wide stretch of the taper gets none.
//! 3. **Splitter guard invariant under random affines** — a base copper layer
//!    put through a random rotation + translation + uniform scale (with the
//!    process options scaled by the same factor, a similarity transform). The
//!    fiber territory must stay `guard_mm · scale` clear of every copper edge.
//! 4. **`order` permutation-completeness on adversarial inputs** — heavy
//!    duplicate multisets, empty-`pts` elements, and single-point elements,
//!    the cases the general random generator almost never produces.
//!
//! See the `qa1_notes` module test at the bottom for the documented
//! limitations that are deliberately *not* asserted (curved-neck centerline
//! caveat).

use cam::ablation::point_in_polys;
use cam::force_clear::force_clear;
use cam::geom::{area_nm2, difference, offset, union, xor};
use cam::order::order;
use cam::split::{GUARD_TOLERANCE_NM, min_dist_to_polys_nm, split};
use pcb_core::{
    CamOpts, Layer, NM_PER_MM, NM_PER_UM, Nm, P, PathElem, PathKind, Paths, Poly, Ring,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

const MM: f64 = NM_PER_MM as f64;

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

/// Twice the signed shoelace area, exact in i128 (test-local copy).
fn ring_doubled_area(ring: &Ring) -> i128 {
    let mut sum: i128 = 0;
    for (i, a) in ring.iter().enumerate() {
        let b = &ring[(i + 1) % ring.len()];
        sum += a.x as i128 * b.y as i128 - b.x as i128 * a.y as i128;
    }
    sum
}

/// Output orientation convention: outer CCW, holes CW, no degenerate rings.
fn assert_convention(polys: &[Poly]) {
    for p in polys {
        assert!(p.outer.len() >= 3, "degenerate outer ring");
        assert!(ring_doubled_area(&p.outer) > 0, "outer must be CCW");
        for h in &p.holes {
            assert!(h.len() >= 3, "degenerate hole ring");
            assert!(ring_doubled_area(h) < 0, "hole must be CW");
        }
    }
}

fn relative_close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * a.abs().max(b.abs())
}

// ===========================================================================
// Area 1 — extreme aspect ratios & near-degenerate slivers
// ===========================================================================

/// A 100:1‥1000:1 thin axis-aligned rectangle: length 10‥200 mm, the short
/// side `length / aspect` (always >= 10 µm on the nm grid, well clear of
/// degeneracy).
fn thin_rect() -> impl Strategy<Value = Poly> {
    (10.0f64..200.0, 100.0f64..1000.0).prop_map(|(len_mm, aspect)| {
        let w_mm = len_mm / aspect;
        rect_mm(0.0, 0.0, len_mm, w_mm)
    })
}

/// A near-degenerate razor triangle: a right triangle 20‥200 mm long and
/// 10‥200 µm tall (aspect up to ~2×10⁴) but with unambiguously positive area.
fn razor_tri() -> impl Strategy<Value = Poly> {
    (20.0f64..200.0, 10.0f64..200.0).prop_map(|(len_mm, h_um)| {
        let h_mm = h_um / 1000.0;
        Poly {
            outer: vec![
                P::from_mm(0.0, 0.0),
                P::from_mm(len_mm, 0.0),
                P::from_mm(len_mm, h_mm),
            ],
            holes: vec![],
        }
    })
}

fn extreme_poly() -> impl Strategy<Value = Poly> {
    prop_oneof![thin_rect(), razor_tri()]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(160))]

    /// `union` normalizes an extreme sliver to a single area-preserving poly
    /// and is idempotent under a second union with itself.
    #[test]
    fn extreme_union_idempotent(p in extreme_poly()) {
        let a_in = area_nm2(std::slice::from_ref(&p));
        prop_assert!(a_in > 0.0, "sliver must have positive area");

        let u = union(std::slice::from_ref(&p), &[]);
        prop_assert_eq!(u.len(), 1, "one convex sliver -> one poly");
        assert_convention(&u);
        prop_assert!(relative_close(a_in, area_nm2(&u), 1e-9), "normalize changed area");

        let uu = union(&u, &u);
        prop_assert_eq!(uu.len(), u.len());
        assert_convention(&uu);
        prop_assert!(relative_close(area_nm2(&u), area_nm2(&uu), 1e-9), "union(u,u) changed area");
    }

    /// Self-`difference` and self-`xor` of an extreme sliver are empty.
    #[test]
    fn extreme_self_difference_and_xor_empty(p in extreme_poly()) {
        let d = difference(std::slice::from_ref(&p), std::slice::from_ref(&p));
        prop_assert!(d.is_empty(), "p \\ p left {} polys (area {})", d.len(), area_nm2(&d));
        let x = xor(std::slice::from_ref(&p), std::slice::from_ref(&p));
        prop_assert!(x.is_empty(), "p xor p left {} polys", x.len());
    }

    /// Dilate-then-erode by the same `d` recovers the area of an extreme thin
    /// rectangle. `d` is a fraction of the (tiny) short side, so this is the
    /// regime where cavalier's offset is most brittle. Dilating first means
    /// the round trip never fully collapses.
    #[test]
    fn thin_rect_offset_round_trip(
        len_mm in 10.0f64..200.0,
        aspect in 100.0f64..1000.0,
        frac in 0.1f64..1.5,
    ) {
        let w_mm = len_mm / aspect;
        let p = rect_mm(0.0, 0.0, len_mm, w_mm);
        let a0 = area_nm2(std::slice::from_ref(&p));
        prop_assert!(a0 > 0.0);

        let d = (frac * w_mm * MM).round() as Nm;
        prop_assume!(d > 0);

        let grown = offset(std::slice::from_ref(&p), d);
        prop_assert!(!grown.is_empty(), "dilate of a thin rect must not vanish");
        assert_convention(&grown);
        prop_assert!(area_nm2(&grown) > a0, "positive offset must dilate");

        let back = offset(&grown, -d);
        prop_assert!(!back.is_empty(), "erode-back of a just-dilated rect must survive");
        assert_convention(&back);
        let a1 = area_nm2(&back);
        prop_assert!(
            relative_close(a0, a1, 0.01),
            "round-trip area off by {:.4}%: {} -> {} (d = {} nm, w = {:.4} mm)",
            100.0 * (a1 - a0).abs() / a0, a0, a1, d, w_mm
        );
    }
}

// ===========================================================================
// Area 2 — force_clear on adversarial necks
// ===========================================================================

/// Union a slice of polys into a single normalized region.
fn union_all(polys: &[Poly]) -> Vec<Poly> {
    let mut acc: Vec<Poly> = Vec::new();
    for p in polys {
        acc = union(&acc, std::slice::from_ref(p));
    }
    acc
}

/// A two-neck bent chain (the "S-curve in series at an angle" family): three
/// square pads A→B→C with B below C, joined by a horizontal neck (A–B) and a
/// vertical neck (B–C) at 90° to each other, both of width `w`. Pads have side
/// `ps`, necks span `g`.
fn two_neck_region(ps: f64, g: f64, w: f64) -> Vec<Poly> {
    let a = rect_mm(0.0, 0.0, ps, ps);
    let b = rect_mm(ps + g, 0.0, 2.0 * ps + g, ps);
    let c = rect_mm(ps + g, ps + g, 2.0 * ps + g, 2.0 * ps + g);
    // Horizontal neck A-B centered at y = ps/2, overlapping 0.5 mm into each pad.
    let yc = ps / 2.0;
    let neck_ab = rect_mm(ps - 0.5, yc - w / 2.0, ps + g + 0.5, yc + w / 2.0);
    // Vertical neck B-C centered at x = center of B/C, overlapping 0.5 mm.
    let xc = ps + g + ps / 2.0;
    let neck_bc = rect_mm(xc - w / 2.0, ps - 0.5, xc + w / 2.0, ps + g + 0.5);
    union_all(&[a, b, c, neck_ab, neck_bc])
}

/// A linearly tapered neck between two pads. The exposed neck runs from the
/// pad-A edge `x = ps` (width `w0`) to the pad-B edge `x = ps + g` (width
/// `w1`), centered on `y = ps/2`; constant-width connector stubs overlap
/// 0.5 mm into each pad so the union is unambiguous and the exposed neck width
/// is exactly `w0` at the pad edge (no extrapolation surprises).
fn tapered_region(ps: f64, g: f64, w0: f64, w1: f64) -> Vec<Poly> {
    let a = rect_mm(0.0, 0.0, ps, ps);
    let b = rect_mm(ps + g, 0.0, 2.0 * ps + g, ps);
    let yc = ps / 2.0;
    let (x0, x1) = (ps, ps + g);
    let taper = Poly {
        outer: vec![
            P::from_mm(x0, yc - w0 / 2.0),
            P::from_mm(x1, yc - w1 / 2.0),
            P::from_mm(x1, yc + w1 / 2.0),
            P::from_mm(x0, yc + w0 / 2.0),
        ],
        holes: vec![],
    };
    let conn_l = rect_mm(x0 - 0.5, yc - w0 / 2.0, x0, yc + w0 / 2.0);
    let conn_r = rect_mm(x1, yc - w1 / 2.0, x1 + 0.5, yc + w1 / 2.0);
    union_all(&[a, b, taper, conn_l, conn_r])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Two sub-`min_feature` necks in series at 90° each get a force-clear
    /// pass (two components → two passes), and every pass vertex lies inside
    /// the copper region.
    #[test]
    fn two_neck_chain_gets_two_passes_inside_region(
        ps in 3.0f64..6.0,
        g in 2.0f64..5.0,
        w in 0.04f64..0.12,
    ) {
        let min_feature = 0.15;
        prop_assume!(w < min_feature);
        let region = two_neck_region(ps, g, w);
        prop_assert_eq!(region.len(), 1, "chain must be one connected component");

        let passes = force_clear(&region, min_feature);
        prop_assert_eq!(
            passes.len(), 2,
            "two series necks (ps={}, g={}, w={}) -> two passes, got {}",
            ps, g, w, passes.len()
        );
        for pl in &passes {
            prop_assert!(pl.pts.len() >= 2);
            for &pt in &pl.pts {
                prop_assert!(
                    point_in_polys(pt, &region, NM_PER_UM),
                    "force-clear vertex ({}, {}) is outside the copper region",
                    pt.x, pt.y
                );
            }
        }
    }

    /// A neck tapering from sub-`min_feature` to comfortably-wide gets at
    /// least one pass; every pass vertex lies inside the region *and* in the
    /// narrow half — nothing spurious appears where the neck is wider than
    /// `min_feature`.
    #[test]
    fn tapered_neck_flags_only_the_narrow_stretch(
        ps in 3.0f64..6.0,
        g in 3.0f64..6.0,
        w0f in 0.2f64..0.55,   // narrow width as a fraction of min_feature
        w1f in 2.0f64..3.5,    // wide width as a multiple of min_feature
    ) {
        let min_feature = 0.15;
        let w0 = w0f * min_feature;
        let w1 = w1f * min_feature;
        let region = tapered_region(ps, g, w0, w1);
        prop_assert_eq!(region.len(), 1, "tapered dumbbell must be one component");

        // Width along the exposed neck is linear from w0 at x=ps to w1 at
        // x=ps+g; find where it reaches min_feature. Everything to the right
        // is "comfortably wide" and must stay pass-free.
        let x_cross = ps + (min_feature - w0) / (w1 - w0) * g;
        let wide_x_nm = ((x_cross + min_feature) * MM).round() as Nm;

        let passes = force_clear(&region, min_feature);
        prop_assert!(!passes.is_empty(), "narrow taper end must get a pass");
        for pl in &passes {
            for &pt in &pl.pts {
                prop_assert!(
                    point_in_polys(pt, &region, NM_PER_UM),
                    "taper pass vertex ({}, {}) outside region", pt.x, pt.y
                );
                prop_assert!(
                    pt.x <= wide_x_nm,
                    "taper pass vertex x={} reaches into the wide stretch (> {})",
                    pt.x, wide_x_nm
                );
            }
        }
    }
}

// ===========================================================================
// Area 3 — splitter guard invariant under random affines
// ===========================================================================

/// The base copper layer (fixed): a rectangle plus a disc, a few mm across.
fn base_layer() -> Layer {
    let rect = rect_mm(0.0, 0.0, 4.0, 2.5);
    let disc = {
        let (cx, cy, r) = (8.0, 1.5, 1.6);
        Poly {
            outer: (0..64)
                .map(|i| {
                    let t = 2.0 * std::f64::consts::PI * i as f64 / 64.0;
                    P::from_mm(cx + r * t.cos(), cy + r * t.sin())
                })
                .collect(),
            holes: vec![],
        }
    };
    Layer {
        polys: vec![rect, disc],
    }
}

/// Apply a similarity transform (uniform scale `s`, rotation `theta`,
/// translation `(tx, ty)` in nm) to every vertex of a poly.
fn affine_poly(p: &Poly, s: f64, theta: f64, tx: f64, ty: f64) -> Poly {
    let (sin, cos) = theta.sin_cos();
    let map = |q: &P| {
        let (x, y) = (q.x as f64, q.y as f64);
        let (rx, ry) = (s * (x * cos - y * sin), s * (x * sin + y * cos));
        P::new((rx + tx).round() as Nm, (ry + ty).round() as Nm)
    };
    Poly {
        outer: p.outer.iter().map(map).collect(),
        holes: p
            .holes
            .iter()
            .map(|h| h.iter().map(map).collect())
            .collect(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Split commutes with a similarity transform up to the guard: after a
    /// random rotation + translation + uniform scale (options scaled to
    /// match), every fiber element stays `guard_mm · scale` clear of every
    /// copper edge. Reuses `split::min_dist_to_polys_nm`.
    #[test]
    fn guard_invariant_under_random_affine(
        s in 0.5f64..3.0,
        theta in 0.0f64..std::f64::consts::TAU,
        tx_mm in -50.0f64..50.0,
        ty_mm in -50.0f64..50.0,
    ) {
        let base = base_layer();
        let (tx, ty) = (tx_mm * MM, ty_mm * MM);
        let layer = Layer {
            polys: base.polys.iter().map(|p| affine_poly(p, s, theta, tx, ty)).collect(),
        };

        // Scale the process options by the same factor (a true similarity).
        let d = CamOpts::default();
        let opts = CamOpts {
            spot_mm: d.spot_mm * s,
            clearance_mm: d.clearance_mm * s,
            band_mm: d.band_mm * s,
            interval_mm: d.interval_mm * s,
            min_feature_mm: d.min_feature_mm * s,
            guard_mm: d.guard_mm * s,
            ..d
        };

        let jobs = split(&layer, &opts);
        let copper = offset(&layer.polys, 0);
        let guard_nm = opts.guard_mm * MM;
        // Tolerance scales with the geometry (arc-flattening chord error grows
        // with the offset radius); a generous multiple, far below the real
        // clearance-sized margin the construction leaves.
        let tol = GUARD_TOLERANCE_NM as f64 * (s + 1.0);

        for e in &jobs.fiber.elems {
            let dist = min_dist_to_polys_nm(&e.pts, e.closed, &copper);
            prop_assert!(
                dist >= guard_nm - tol,
                "fiber element only {:.1} nm from copper (guard {:.1} nm, tol {:.1}, scale {:.3})",
                dist, guard_nm, tol, s
            );
        }
    }
}

// ===========================================================================
// Area 4 — order() permutation-completeness on adversarial inputs
// ===========================================================================

fn kind_ord(k: PathKind) -> (u8, u32) {
    match k {
        PathKind::Isolation(n) => (0, n),
        PathKind::Rubout(n) => (1, n),
        PathKind::ForceClear => (2, 0),
        PathKind::Boundary => (3, 0),
        PathKind::Mark => (4, 0),
    }
}

fn elem_key(e: &PathElem) -> ((u8, u32), bool, Vec<(i64, i64)>) {
    (
        kind_ord(e.kind),
        e.closed,
        e.pts.iter().map(|p| (p.x, p.y)).collect(),
    )
}

fn assert_permutation(input: &Paths, output: &Paths) {
    assert_eq!(input.elems.len(), output.elems.len(), "length changed");
    let mut a: Vec<_> = input.elems.iter().map(elem_key).collect();
    let mut b: Vec<_> = output.elems.iter().map(elem_key).collect();
    a.sort();
    b.sort();
    assert_eq!(
        a, b,
        "multiset changed (dropped/duplicated/mutated element)"
    );
}

/// An adversarial element: heavily biased toward duplicates (a 4-element
/// pool), plus empty-`pts` and single-point degenerates that the general
/// random generator practically never emits.
fn adversarial_elem() -> impl Strategy<Value = PathElem> {
    let pool = prop::sample::select(vec![
        PathElem {
            kind: PathKind::Mark,
            pts: vec![P::new(0, 0)],
            closed: false,
        },
        PathElem {
            kind: PathKind::Boundary,
            pts: vec![],
            closed: false,
        },
        PathElem {
            kind: PathKind::Isolation(0),
            pts: vec![P::new(5 * NM_PER_MM, 5 * NM_PER_MM)],
            closed: true,
        },
        PathElem {
            kind: PathKind::ForceClear,
            pts: vec![
                P::new(-3 * NM_PER_MM, 7 * NM_PER_MM),
                P::new(3 * NM_PER_MM, 7 * NM_PER_MM),
            ],
            closed: false,
        },
    ]);
    prop_oneof![
        // 70 % duplicates from the pool.
        7 => pool,
        // 15 % fresh empty-pts elements.
        2 => any::<bool>().prop_map(|closed| PathElem { kind: PathKind::Mark, pts: vec![], closed }),
        // 15 % fresh single-point elements at the origin cell.
        2 => (-2_000_000i64..2_000_000, -2_000_000i64..2_000_000)
            .prop_map(|(x, y)| PathElem { kind: PathKind::Rubout(0), pts: vec![P::new(x, y)], closed: false }),
    ]
}

fn adversarial_paths() -> impl Strategy<Value = Paths> {
    prop::collection::vec(adversarial_elem(), 0..300).prop_map(|elems| Paths { elems })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `order` is a permutation even for pathological multisets (many
    /// identical elements, empty `pts`, single points) — every input element
    /// appears in the output exactly as many times as it went in.
    #[test]
    fn order_permutation_on_adversarial_multisets(paths in adversarial_paths()) {
        let out = order(&paths);
        assert_permutation(&paths, &out);
    }
}

// ---------------------------------------------------------------------------
// Documented limitations deliberately NOT asserted
// ---------------------------------------------------------------------------

/// This is a documentation anchor, not a behavioural test. Consistent with
/// `force_clear`'s module docs ("for a strongly curved neck the single
/// straight chord hugs the centroid region rather than following the curve"),
/// the Area-2 properties do **not** assert that every neck's centerline stays
/// inside its own *neck rectangle* for curved/bent necks — only that it stays
/// inside the overall copper *region* (which the clip-to-component guarantees)
/// and that pass *counts* are correct. A skeleton-based centerline would be
/// needed to make the stronger midline claim on curved necks; that is out of
/// scope for QA-1.
#[test]
fn qa1_documented_limitations_noted() {
    // A bent neck: assert only region-containment, not neck-box containment.
    let region = two_neck_region(4.0, 3.0, 0.08);
    let passes = force_clear(&region, 0.15);
    assert_eq!(passes.len(), 2);
    for pl in &passes {
        for &pt in &pl.pts {
            assert!(point_in_polys(pt, &region, NM_PER_UM));
        }
    }
}
