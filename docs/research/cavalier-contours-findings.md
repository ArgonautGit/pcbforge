# FLD-8: cavalier_contours findings (offset robustness on CAM geometry)

**Task:** evaluate a newer `cavalier_contours` release and consolidate the
issues we work around into a filable, evidence-backed report.
**Date:** 2026-07-14. **Pinned version:** `cavalier_contours = "0.7.0"`.

---

## 1. Version verdict

**0.7.0 is still the latest release on crates.io** (checked 2026-07-14; also
the newest `cavalier_contours_ffi`). There is no newer version to bump to, so
every workaround in `crates/cam/src/geom.rs` remains load-bearing. Re-run this
check — and the regression suite in §4 — whenever a new release appears.

The crate is otherwise the right tool: its raw winding-number offset is exactly
what `geom::offset` leans on for the "never lose area" reference path. The
issues below are all in the *trimmed* `parallel_offset` and in input hygiene,
not in the core algorithm.

## 2. Issues (each with a minimal repro)

All repros are on flat 2D polylines with `f64` coordinates in millimeters, the
regime KiCad-derived PCB geometry produces. Line numbers are from the pinned
0.7.0 source.

### 2.1 Panic on collapsing inward offset — `pline_view.rs:290`

`parallel_offset` (and `create_raw_offset_polyline`) panic internally when an
inward offset collapses a small closed loop, rather than returning no loops.
This is the upstream **issue #79** class. Reproduced directly (see
`crates/cam/tests/cavalier_panic_silence.rs`): a ~0.4 mm square eroded by
0.4–4 mm panics at `polyline/pline_view.rs:290`.

- **Impact on us:** every offset call is wrapped in `catch_unwind`, and — because
  the *default panic hook still prints* the message + backtrace note to stderr
  before the unwind — `geom::silence_cavalier_panics()` installs a filtering
  hook (FLD-3) so the recovered panics don't spam the operator.
- **Requested fix upstream:** return `Ok(empty)` (or a dedicated
  `OffsetCollapsed` result) for a fully-collapsed loop instead of panicking.

### 2.2 Panic on sub-fuzz duplicate vertices — "repeat position vertexes"

Adjacent vertices closer than the internal fuzz make `parallel_offset` panic
with a "repeat position vertexes" message. KiCad 10 emits region contours with
adjacent vertices **1–3 nm apart** (a plotting stutter), which trips this on
essentially every real board.

- **Impact on us:** `geom::DEDUPE_NM = 25 nm` pre-dedupe (including the closing
  wrap) collapses these before the polyline is handed to cavalier. This is the
  reason the original FLD-3 repro (uv_test @ 0.025 mm) no longer panics.
- **Requested fix upstream:** dedupe or tolerate sub-`POS_EQUAL_EPS` adjacent
  vertices in the offset entry point rather than asserting on them.

### 2.3 Result depends on the starting vertex of a closed polyline

The trimmed offset's slice-validation prunes differently depending on which
vertex the closed loop starts at. Measured: eroding one test ring succeeded for
only **9 of its 75** vertex rotations — starting inside a dense fan of
arc-flattening chords makes the validator prune every slice, yielding no loops
for a ring that clearly has an inward offset.

- **Impact on us:** the primary path is a **retry ladder** that rotates the ring
  to start at each of its three longest edges before accepting a result.
- **Requested fix upstream:** slice validation should be rotation-invariant;
  at minimum, document the sensitivity.

### 2.4 Exact-feature-radius erosion is fully degenerate

Eroding a just-dilated region by the same distance `d` (so corner arcs have
radius exactly `d`) returns **no loops for any rotation** — a degenerate case
distinct from a genuine collapse.

- **Impact on us:** the ladder adds two overshoot-and-compensate attempts
  (offset by `d·(1+ε)` then back by `d·ε`) to un-degenerate these, at the cost
  of `ε·|d|`-scale corner rounding.

### 2.5 Silent over-pruning (worst, because it looks fine)

`parallel_offset` can return a **plausible-looking but over-pruned** result.
One observed erode silently lost **~17 % of the region's area** with no error
and no panic — the failure is invisible without an independent area check.

- **Impact on us:** every trimmed attempt is validated against the raw
  **winding-number reference** (from `create_raw_offset_polyline`); an attempt
  is accepted only if its total area matches the reference within the sliver-
  artifact budget. This is the single most important guard — it converts a
  silent-wrong into a caught-and-retried.
- **The empty-validates-against-broken-reference trap:** when the reference
  *also* fails (e.g. panics to empty), its area is 0, so an empty attempt
  "matches" and a real region silently vanishes. Defused by the
  collapse-plausibility coarea bound (`area ≤ perimeter·|η|`) and the
  un-offset last-resort emission — a ring is never silently deleted.

## 3. What we would upstream

Priority order for issues to file (2.1 and 2.2 are the crashes):

1. **#79-class collapse panic (2.1)** — return empty, don't panic.
2. **Sub-fuzz duplicate-vertex panic (2.2)** — dedupe/tolerate at the entry
   point; attach the KiCad 10 nm-stutter repro.
3. **Rotation-dependent pruning (2.3)** and **exact-radius degeneracy (2.4)** —
   rotation-invariant slice validation.
4. **Silent over-pruning (2.5)** — hardest to fix in-crate; at minimum expose
   the raw winding result as a supported "lossless" offset mode so downstream
   users don't have to reach into `polyline::internal`.

Minimal repros for 2.1 live in the test suite already; 2.2–2.5 need small
standalone polyline fixtures extracted from the geom test rings before filing.

## 4. Regression gate on any version bump

Before adopting a newer `cavalier_contours`, re-run and expect green:

- `cargo test -p cam geom` — the offset ladder, DEDUPE, collapse-plausibility,
  and un-offset last-resort guards.
- `cargo test -p cam --test cavalier_panic_silence` — confirms the panic-hook
  fix still matches (if a bump removes the panic, this test's child produces no
  cavalier chatter *and* the collapse path returns empty cleanly — at which
  point `silence_cavalier_panics` and the `catch_unwind` wrappers can be
  retired).
- `cargo test -p pcbforge --test emit_e2e nonconductor_pour_is_kept_by_default`
  — the offset-0.025 leg on the real KiCad 10 board, where the empty-validation
  trap once deleted 8 of 9 pour fragments.

If a bump makes any of these fail, the crate's offset behavior changed shape;
re-derive the workarounds rather than loosening the guards.
