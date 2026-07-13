# CAM-10 — Board-outline cut pass (plan)

Operator-requested addition (2026-07-13): the backlog ablates, drills, masks,
and legends, but never frees the board from the stock. This plan adds a
depaneling cut pass. The backlog task prompt lives in `docs/backlog.md`
(CAM-10); this document is the design it executes.

## The problem the plan must solve

Cutting through 1.6 mm FR4 on a galvo is not "one more hatch job":

1. **The focal plane must be lowered during the cut.** An F-theta lens has a
   fixed focal plane and a shallow effective depth of focus (order ±0.2–0.5 mm
   for the fiber head, tighter for UV). The cut floor descends the full board
   thickness — several times the depth of focus — so a fixed-focus cut stalls:
   the beam defocuses at the floor, energy density collapses, the kerf chars
   and widens instead of deepening. Focus must step down with the floor.
2. **Geometry**: kerf-compensated centerlines on the waste side, for the outer
   perimeter *and* interior cutouts, with holding tabs so nothing shifts or
   drops mid-cut.
3. **Sequencing**: the cut destroys registration and rigidity, so it must be
   the last laser op on the board, and interior cutouts must precede the
   perimeter.

## Geometry (crates/cam, new module `cam::cut`)

**Source region.** `cam::noncopper::board_region_from_outline(&edge_cuts.polys)`
already yields the board as `Vec<Poly>` — outer ring CCW, interior cutouts as
CW holes, cutout parity handled. CAM-10 consumes exactly that.

**Kerf compensation is a single offset.** Offsetting the *board region* by
`+kerf/2` with the existing `cam::geom::offset` moves every boundary onto the
waste side automatically: the outer ring grows outward (beam runs outside the
finished edge), holes shrink inward (beam runs inside the cutout). One call,
both sides correct, courtesy of the winding convention. `kerf_mm` is measured,
not assumed (see calibration).

**Tabs.** Each closed ring is parameterized by arc length and broken into open
segments by `tab_count` gaps of length `tab_mm + kerf_mm` (leaving ≈ `tab_mm`
of solid material). Gap midpoints are spread evenly by arc length, then nudged
off corners (vertex turn angle over a threshold) onto the nearest straight
span — a tab across a corner snaps unpredictably. v1 leaves tabs at every
depth (gaps in all passes); a `tab_start_fraction` refinement (full-depth cut
until X %, tabs only after) is explicitly deferred. Rings shorter than
`tab_count * (tab_mm + kerf_mm) * 2` get fewer tabs, minimum 1; a cutout small
enough that its slug could jam the kerf is warned about in the schedule.

**Path kind.** New `pcb_core::PathKind::Cut` variant, so ordering, splitting,
and future emitters can treat through-cuts differently from surface work
(CAM-3 must not interleave cut segments with anything; CAM-5 must never route
`Cut` to the UV guard-band set).

## Focus schedule (the core of the task)

New types in `pcb_core`:

```rust
/// Through-cut parameters. Lengths mm.
pub struct CutOpts {
    pub kerf_mm: f64,          // measured beam kerf at focus
    pub tab_count: u32,        // per ring
    pub tab_mm: f64,           // solid tab width left standing
    pub mm_per_pass: f64,      // measured depth removed per pass at cut params
    pub z_step_mm: f64,        // max focus drop per step ≤ usable depth of focus
    pub overcut_mm: f64,       // extra commanded depth past the far face
    pub machine: Machine,
}

/// One focus step: run these passes, then lower the focal plane.
pub struct CutStep {
    pub passes: u32,
    /// How far to lower the head (or raise the bed) AFTER these passes, mm.
    /// 0.0 on the final step.
    pub focus_drop_mm: f64,
}

pub struct CutSchedule {
    pub steps: Vec<CutStep>,
    pub total_depth_mm: f64,   // thickness + overcut
}
```

Derivation, all integer/exact where it matters:

- `total_depth = thickness + overcut_mm`, thickness taken from the board's
  `.gbrjob` (`ingest::gbrjob::BoardMeta::thickness_nm`, ING-5) — never assumed.
- `passes_per_step = max(1, floor(z_step_mm / mm_per_pass))` — never let a
  step's removal exceed the depth-of-focus budget.
- Steps repeat until cumulative commanded depth ≥ `total_depth`; the last step
  gets the remainder and `focus_drop_mm = 0`.
- Each step's `focus_drop_mm = passes * mm_per_pass` — focus follows the
  floor exactly, so the beam waist is always within one step of the material
  being removed.

Sign convention, stated once and tested: the focal plane moves **down into
the material**, which on the physical machines means **lowering the head**
(or raising the bed) by `focus_drop_mm` — the lens-to-cut-floor distance is
held at the focal distance. The schedule text says "lower the head by X mm"
verbatim to leave no room for a sign error at the machine.

This maps directly onto the existing checkpoint machinery: each `CutStep`
boundary is a CAM-4-style checkpoint. In the LightBurn workflow the checkpoint
is a human action; under ORC/DRV it becomes a stage-engine pause (prompted
manual crank, or motorized Z when available).

## Calibration (operator, before first real cut)

`mm_per_pass`, `kerf_mm`, and a usable `z_step_mm` are machine facts, not
constants. One scrap-FR4 ladder, agent-prepared checklist style (like
ORC-4's live gate):

1. Burn N short parallel cut lines at cut params with 5, 10, 15, … passes at
   fixed focus; measure depth (calipers / microscope) → `mm_per_pass` in the
   linear region, and the depth where the curve flattens → effective
   depth-of-focus → `z_step_mm` (use half of it, conservatively).
2. Measure kerf width at the surface → `kerf_mm`.

Defaults ship deliberately conservative (`z_step_mm = 0.2`,
`mm_per_pass = 0.05`, `overcut_mm = 0.1`) and the CLI prints a
"defaults are placeholders — run the ladder" warning until overridden.

## Sequencing rules

- The cut job is emitted as the **final** job of a board's plan. It never
  appears before ablation/mask/legend/drill ops (registration is gone once
  the tabs are the only connection, and rigidity is gone once cut).
- Within the cut: all interior cutout rings first, perimeter last, so the
  panel keeps maximum stiffness for as long as possible.
- Per-step direction alternation (reverse segment order on alternate passes)
  to avoid parking heat at the same start corners.

## CLI surface (usable in the LightBurn workflow today)

`pcbforge cut --board <.kicad_pcb> | --outline <Edge.Cuts.gbr> --thickness-mm T`
plus `--kerf-mm --tabs --tab-mm --mm-per-pass --z-step-mm --overcut-mm`:

- writes `cut-step-01.svg/.dxf`, `cut-step-02.…`, … — one file per focus step
  (all steps share the same geometry in v1; per-step files exist so LightBurn
  runs exactly `passes` of each and the operator's stopping points are files,
  not counted passes),
- writes `cut-schedule.txt`: per step — file, pass count, cut params, then
  "lower the head by X mm"; slug warnings; total depth; the
  run-the-ladder warning when defaults are in play.

When EMIT-2 lands, the same `CutSchedule` compiles to per-step `.lbrn2` files
with pass counts baked in.

## Done-when (test plan)

Property tests (proptest, style of CAM-2/CAM-5):

1. **Kerf clearance**: min distance from any emitted cut vertex to the board
   region boundary == kerf/2 within 1 µm, on random rectangles-with-holes.
2. **Tabs**: per ring, gap count == effective tab count; each gap arc length
   == `tab_mm + kerf_mm` within 1 µm; segment lengths + gap lengths == full
   ring length within 1 µm (nothing lost, nothing doubled).
3. **Schedule**: Σ(passes · mm_per_pass) ≥ thickness + overcut and less than
   one extra pass over it; every `focus_drop_mm ≤ z_step_mm`; last drop == 0;
   Σ(focus drops) == total commanded depth − last step's removal (focus never
   overtakes the remaining material).
4. **Ordering**: interior rings strictly precede the perimeter ring in the
   emitted `Paths`; every element is `PathKind::Cut`.

Fixture test on `samples/kicad/valdemo2` (36×30 outline + circular cutout,
1.6 mm from its real `.gbrjob`): 2 rings, expected gap counts, schedule for
1.6 + 0.1 mm at defaults (deterministic step/pass numbers asserted). E2E CLI
test in the noncopper_e2e style: files exist, schedule text names every file,
sign convention string present.

## Explicitly out of scope (v1)

- Motorized-Z control (lands with DRV/ORC integration; the schedule is
  already shaped for it).
- Per-depth kerf taper compensation (kerf narrows with depth; v1 accepts a
  slightly trapezoidal edge).
- `tab_start_fraction` (tabs only near the bottom of the cut).
- Tiling interaction (CAM-9): a cut ring crossing a tile boundary is a
  stop-and-propose in v1.
