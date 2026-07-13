# Decisions & deviations log

Per the backlog conventions: every task records deviations from its prompt and
discovered constraints here.

## 2026-07-13 — ING-3 + ING-4 (X2 attributes + net raster)

- ING-3: extended the existing `ingest::gerber` parser (rather than writing a
  second parser) to track the X2 attribute dictionary — `%TA.AperFunction`,
  `%TO.N` (net), `%TO.P` (component pad ref/pin), `%TD` reset (Ucamco §5.6).
  New `load_gerber_x2`/`parse_gerber_x2` return an `AttributedLayer` carrying
  per-object attributes; the folded `.layer()` geometry is byte-for-byte
  identical to plain `load_gerber` (asserted on valdemo + valdemo2). The
  done-when's "X2 layer vs SVG layer ≥99.5%" is met transitively: plain
  `load_gerber` is already golden-validated against KiCad's own SVG render
  (kicad_golden.rs), and the X2 path produces identical geometry, so no
  separate y-frame reconciliation between the y-down gerber and y-up svg
  ingest was needed. Attribute tracking is inert (`track_attrs=false`) on the
  plain path, so existing behavior/tests are unchanged.
- Evaluated the crates.io `gerber-types`/parser crates per the prompt: the
  hand-rolled tolerant parser already handles the KiCad dialect and is
  golden-validated, so adding a dependency to re-parse would be strictly more
  code and risk — kept the in-house parser (noted here per the prompt).
- ING-4 net-source decision: chose **(b) X2 `.N` attributes** over (a) a
  per-net kicad-cli export (none exists) and (c) parsing the `.kicad_pcb`
  s-expression netlist. (b) is far less code — ING-3 already renders every
  copper object with correct geometry AND tags its net, so `net_raster` is a
  union-per-net + scanline fill; (c) would re-derive footprint/pad geometry
  (rotation, roundrect, thermal reliefs) the gerber path already produces.
  `ingest::net_raster::net_raster(&AttributedLayer, um_per_px) -> (IdImage,
  Vec<NetName>)`; verified on valdemo2 that VCC and GND get distinct IDs and
  each net's pad centroid samples back to its own ID (frame-correct sample
  points taken from the parsed geometry, in lieu of pasted GUI coordinates —
  same authored-ground-truth substitution as the ING wave).
- net_polys unions dark objects per net; clears (thermal reliefs) are not
  subtracted per-net (copper layers are positive; the folded layer still
  carries them). Adequate for the clearance loop's short/open classification.

## 2026-07-13 — CAM-10 added (board-outline cut / depaneling)

- Operator-requested new task: the backlog never freed the board from the
  stock (Edge.Cuts was only a region mask for noncopper and a bbox for
  .gbrjob metadata). CAM-10 adds a kerf-compensated, tabbed through-cut with
  a focus-step schedule; full design in `docs/plans/cam-10-board-cut.md`,
  task prompt appended to `docs/backlog.md` (backlog now 54 tasks).
- The operator-stated constraint that shaped the design: the focal plane must
  be lowered during the cut. The plan encodes this as CutSteps (each removing
  ≤ z_step_mm, i.e. within the lens's usable depth of focus) with an explicit
  "lower the head by X mm" instruction between steps, pass counts derived
  from a measured mm_per_pass, and board thickness taken from the .gbrjob
  (ING-5) rather than assumed. mm_per_pass / kerf / z_step are machine facts:
  an operator calibration ladder (agent-prepared checklist) precedes the
  first real cut, and shipped defaults warn until overridden.
- Implemented (same day): pcb_core gained PathKind::Cut and
  CutOpts/CutStep/CutSchedule; cam::cut has cut_paths + schedule + tab_ring;
  cam::export gained write_paths_{svg,dxf} (open/closed stroke polylines);
  the `pcbforge cut` verb writes per-focus-step SVG/DXF + cut-schedule.txt.
  Verified against the real valdemo2 board via kicad-cli (36×30 outline +
  Ø4 cutout, 1.6 mm): 4 tabs on the cutout, 4 on the perimeter, 9-step focus
  schedule for 1.6 + 0.1 mm at the conservative defaults. 220 tests green.
- Two discovered constraints, both worked around in cam::cut (not by touching
  the shared geom kernel):
  1. `geom::offset` on the Gerber-polygonized circular cutout (~300 verts,
     offset by +25 µm) left ~98 sub-µm² sliver *holes* alongside the one real
     12.56 mm² cutout. cut_paths drops rings below MIN_RING_AREA_MM2 (1e-4 mm²,
     ~500× the artifact scale, far below any cuttable cutout).
  2. The same offset returns the real cutout ring as a ~1178-vertex loop with
     sub-µm zigzag noise whose *per-vertex* turns hit 81°, which naive corner
     detection read as ~196 corners and refused to tab. Corner detection now
     measures turning between directions averaged over a ±40 µm arc window, so
     only genuine corners register — and the emitted cut path stays the exact
     kerf offset (kerf-clearance property holds to 1 µm), rather than being
     geometrically simplified (which would have eaten into the clearance).

## 2026-07-08 — Repo bootstrap (pre-INF-1)

- The repository was empty (no commits) when work started. `BACKLOG.md` was
  created from the backlog document's checklist section, and the backlog
  document itself was committed as `docs/backlog.md` so task prompts are
  in-repo. This is bootstrap plumbing, not INF-1 — INF-1 remains blocked until
  `docs/scaffold.md` (playbook §2.1 content) is provided.
- Blocked-source inventory at bootstrap, per the "stop, never improvise" rule:
  - `docs/scaffold.md` missing → INF-1 blocked, and with it every task that
    transitively depends on the workspace (all of ING/GEO/CAM/SIM/VIS/ORC/UI/QA,
    EMIT-2/3, INF-2..4).
  - `samples/lbrn2/` missing → EMIT-1 blocked.
  - `RUNLOG.md` (B4 USB ID) missing, and this execution environment has no USB
    bus, no `/dev/video*`, and no tshark → DRV-1 blocked; DRV-2..8 downstream.
- Only RES-1..4 were executable; all four were run on 2026-07-08.

## 2026-07-08 — Scaffold authored by agent (INF-1 deviation)

- The operator directed the agent to author `docs/scaffold.md` itself rather
  than wait for the playbook §2.1 paste. The core types were designed from
  the backlog's own usage (every prompt that references `core::*`), informed
  by RES-1's crate audit. INF-1's "use it verbatim" now refers to the
  agent-authored scaffold. Any future playbook content that disagrees should
  supersede via a follow-up refactor, not silent divergence.
- Naming note (superseded same day): the shared-types crate was first named
  `core`, which compiles for plain code but breaks macro expansions that use
  absolute `core::…` paths (proptest's `core::concat!`), discovered during
  GEO-1. Renamed workspace-wide to `pcb-core` (imported as `pcb_core::…`);
  directory remains `crates/core`. scaffold.md carries the amendment. The
  backlog prompts' `core::Layer` spelling should be read as `pcb_core::Layer`
  from here on.
- Pinned by INF-1: i_overlay 7.0.2, cavalier_contours 0.7.0 (both exactly the
  versions RES-1 audited), nalgebra 0.35.0 (RES-1 audited 0.34-era APIs; the
  SVD/solve entry points VIS-5 needs are unchanged in 0.35).

## 2026-07-08 — ORC-1 notes

- `docs/schema.sql` was agent-authored (same operator authorization as
  scaffold.md) — the playbook's verbatim schema was never provided. Tables:
  pallet/board/runlog/material plus schema_version; JSON detail columns are
  writer-owned strings. Future playbook content supersedes via migration.
- rusqlite pinned to 0.37 (bundled): 0.40's libsqlite3-sys build script uses
  the unstable `cfg_select!` macro and fails on rustc 1.94.1. Revisit on the
  next toolchain bump.

## 2026-07-13 — ING wave (ING-1/2/5/6, CAM-6, QA-5) on the real KiCad toolchain

- With kicad-cli 7.0.11 installed, the whole KiCad-gated software chain was
  built and verified against real exports: ING-6 (invoker with runtime flag
  verification — the only shell-out point), ING-1 (SVG→Layer, golden vs
  rsvg-convert 0.99999 on both boards; KiCad SVGs paint white knockout
  shapes over black, folded like Gerber LPC), ING-2 (Excellon incl. G85
  slots, exact-integer decimal-mm→nm), ING-5 (.gbrjob — real structure is
  nested GeneralSpecs.Size.{X,Y}), CAM-6 (process compilers reusing
  ablation's hatcher via an identity rubout construction), QA-5
  (xtask seed-defect with a built-in SVG round-trip golden and
  drc-detectability enforcement).
- Standing deviation for the wave: done-when values the backlog said the
  operator would "paste from the KiCad GUI" are instead taken from the
  authored in-repo board sources (exact by construction, no transcription).
  Real user-designed boards remain the next-best validation input.
- samples/kicad now holds TWO real boards (valdemo, valdemo2), so INF-3's
  real-repo gate now fails only on the missing samples/lbrn2 set.
- Coordinate-frame convention note: gerber/excellon ingest keep KiCad's
  plotted y-down frame (negative Y) verbatim; svg ingest normalizes to the
  y-up board frame. Registration across gerber+drill is consistent; anyone
  mixing svg-ingested and gerber-ingested layers must reconcile frames
  (future ING-4/CAM glue should settle a single convention).
- ING-4 (net raster) is the one remaining KiCad-adjacent task; it needs a
  net↔geometry source decision (X2 .N attributes vs s-expression netlist).

## 2026-07-13 — kicad-cli installed; parser golden-validated against real KiCad

- Correction of an earlier claim: this environment CAN run KiCad — `apt
  install --no-install-recommends kicad` (7.0.11) works fine; the earlier
  "kicad-cli unavailable" blocker was never actually tested. Logged as a
  process lesson: test the blocker before declaring it.
- `samples/kicad/valdemo.kicad_pcb` is a real (agent-authored, KiCad-loaded)
  board: rect/oval/roundrect/circle pads, straight+arc traces, a zone with a
  C-shaped filled_polygon, Edge.Cuts outline.
- `crates/ingest/tests/kicad_golden.rs`: kicad-cli exports the F.Cu Gerber
  (parsed by ingest::gerber, rasterized by testkit) AND its own SVG render
  of the same layer (rasterized by rsvg-convert); the two rasters must agree
  on ≥ 99.5 % of pixels at 25 µm/px after content-bbox alignment. PASSES on
  real KiCad 7.0.11 output — including the real RoundRect macro (closed
  5-pair primitive 4 + primitive-20 corner bridges), negative-Y frame,
  standalone G02/G03 mode words, and interleaved X2 attributes. The test
  self-skips when kicad-cli/rsvg-convert are absent, so CI stays green.
- Consequence for the backlog: the "needs KiCad" blocker on ING-1/2/5/6,
  CAM-6, QA-5 is now soft — KiCad is installable in-session. Those tasks
  are buildable; they still need a second sample board (INF-3 wants ≥ 2)
  and pasted GUI ground-truth values where their done-whens demand them.

## 2026-07-12 — `pcbforge noncopper` (operator-requested FlatCAM replacement)

- The operator asked for a tool cutting FlatCAM out of the old workflow:
  KiCad Gerber → non-copper regions as contiguous closed shapes → EZCAD
  fill/ablate. Delivered as `ingest::gerber` (tolerant RS-274X/X2 parser for
  the KiCad dialect), `cam::noncopper` (board region from Edge.Cuts with
  cutout parity, or copper bbox + margin; inversion with beam-compensation
  offset), `cam::export` (DXF R12 closed POLYLINEs + even-odd SVG + color
  preview), and the `pcbforge noncopper` CLI verb.
- Relation to the backlog: this is a working slice of ING-3 plus new export
  glue. ING-3 itself stays unticked — its done-when requires real
  `kicad-cli`-exported gerbers (cross-checked against ING-1's SVG path) and
  X2 attribute preservation, neither of which exists here yet. The parser's
  fixtures are hand-authored in KiCad's output style; validating against a
  real export is the first thing to do once `samples/kicad` lands.
- Layering: `ingest` now depends on `cam` (for `geom`) — the parser unions
  primitives into a normalized Layer at ingest time. No cycle (cam never
  imports ingest).
- Robustness note: a straight segment issued while arc mode (G02/G03) is
  still modal puts the arc "center" on an endpoint (I/J default 0). The
  parser rejects arcs whose start/end radii disagree instead of emitting
  garbage — this caught exactly that bug in the first fixture draft.
- Area fidelity: circles/dots are polygonized at the equal-area radius;
  capsule caps stay at true radius with doubled vertex density (equal-area
  inflation there would widen the stroke's straight sides — a first-order
  error). Verified: fixture copper ∪ non-copper tiles the board region to
  ≥ 99.95 % of pixels at 10 µm/px and to 1e-9 relative in exact area.

## 2026-07-08 — INF-3 notes

- The `xtask fixtures` validator is complete and self-tested (synthetic
  fixture trees in tempdirs), but its done-when has a real-repo half that
  awaits inputs: with no `samples/` yet, `cargo xtask fixtures` correctly
  exits nonzero and names every missing fixture (the "nonzero when I rename a
  sample" half is proven by the `renamed_lbrn2_sample_fails` test). The
  "exits 0 on the real repo" half will pass once `samples/kicad` (>=2
  `.kicad_pcb`) and the `samples/lbrn2` set exist.
- The "10 named schema samples from playbook §0.3" list was never provided,
  so `EXPECTED_LBRN2` in xtask/src/main.rs is an agent-authored stand-in
  (base + one file per single-setting variant + a uv- variant, matching the
  dimensions EMIT-1 diffs). Editing that one array is all it takes to adopt
  the real §0.3 names.
- Wiring: added `xtask` as a workspace member and `.cargo/config.toml` with
  the `xtask = "run --package xtask --"` alias (standard cargo-xtask pattern,
  foreseen by scaffold.md). These are outside the xtask crate proper but are
  the minimal, conventional glue to make `cargo xtask` resolve.

## 2026-07-08 — ORC-2 notes

- `docs/stages.ron` is agent-authored (same operator authorization as
  scaffold.md / schema.sql) — the playbook's verbatim stages.ron was never
  provided. It defines the top-side ablation walk fiducials → bulk_top →
  iso_check → done. Future playbook content supersedes via edit + migration.
- VIS-11 (camera/AprilTag pallet ID) is unavailable in this environment, so
  the stage engine reads the pallet tag through a `PalletSource` trait with a
  stub impl (`PCBFORGE_PALLET_TAG` env / fixed default). VIS-11 provides the
  real implementation later; the engine is otherwise complete and the walk is
  verified across re-opened-DB "restarts".
- ClearanceLoop is a pass-through stub here; ORC-3 replaces it with the real
  inspect/correct loop.

## 2026-07-08 — INF-2 notes

- Action versions verified against their repos on 2026-07-08 (per the task's
  "don't write from memory" rule): actions/checkout v7.0.0 is current,
  dtolnay/rust-toolchain@stable with `components: clippy, rustfmt`,
  Swatinem/rust-cache v2 (v2.9.1) handles registry caching.
- actionlint is not installed in this environment; the workflow was validated
  by YAML parse + a local run of all three commands instead (done-when allows
  "if available").

## 2026-07-08 — RES-1..4 notes

- RES-1: no Cargo.toml exists yet, so there are no *pinned* versions to audit;
  the review evaluates the current releases as of 2026-07 and records the
  version examined per crate. Re-check on `cargo add` if a materially older
  version resolves.
- RES-2/RES-4: the "≤ 6 months" / "≤ 24 months" source-freshness preferences
  could not always be met — LightBurn galvo/Linux automation and fiber-ablation
  PCB write-ups change slowly. Older sources are used where nothing newer
  exists and are dated so staleness is visible.

## 2026-07-08 — CAM-3 notes

- Baseline for the travel metric (`total_jump_length_nm`) is an **unordered**
  order — a fixed-seed random shuffle of the ablation elements — not
  `ablation_paths`' own output. Ablation already emits a travel-coherent
  scanline sweep (~6-7 mm average jump), and the ≥5 mm mean-consecutive-centroid
  spread that heat-aware ordering must guarantee is fundamentally in tension
  with beating an already-coherent order on travel (you cannot be ≥5 mm apart on
  average yet jump ≤1.3 mm on average). The meaningful, internally-consistent
  reading of "≤ 1/5 of naive" is therefore travel vs. *no ordering strategy*.
  Against that unordered baseline the 10 mm-cell nearest-neighbour +
  boustrophedon round-robin order cuts travel several-fold (measured ratio
  ≈ 0.16 on a 16×16 grid spread over ~143 mm) while keeping consecutive
  centroids ≈ 11-12 mm apart. The shuffle uses a hardcoded LCG seed (no `rand`
  crate, no clock) so the test is deterministic.
