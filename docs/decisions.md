# Decisions & deviations log

Per the backlog conventions: every task records deviations from its prompt and
discovered constraints here.

## 2026-07-14 — CORRECTION: the pour is intentional; default flipped to keep

- The operator corrected the previous entry's premise: the no-net zone is an
  **isolated ground pour, deliberately part of the design** ("It shouldn't
  ablate the whole thing... where is that?"). Their earlier "why did it only
  leave the right side" meant "why did only one pour fragment SURVIVE", not
  "why wasn't the right side cleared". The clear-by-default decision below
  was built on that misreading and is reversed: **default keeps all copper
  in the Gerber** (a no-net pour is real copper); `--clear-nonconductor`
  opts into dead-copper rubout. (`--keep-nonconductor` existed for one
  commit and is gone.)
- With the intent corrected, the third burn's real defect was the
  geom::offset ring-deletion bug alone: at --offset-mm 0.025 it silently ate
  8 of the 9 pour fragments (cavalier panics on KiCad 10's 1–3 nm vertex
  stutter), leaving only the right-side fragment — exactly what the board
  shows. That bug is fixed (dedupe + empty-result guard, previous entry);
  the e2e now probes pour fragments at offset 0 AND 0.025 so the deletion
  can't regress, and verifies --clear-nonconductor still rubs the pour out.
- The expected v5 job ablates only the isolation channels + edge margin —
  the minimal-ablation profile of the operator's original workflow — and its
  render matches the KiCad screenshot's copper-free (black) areas.
- Process lesson recorded: an ambiguous operator question ("why did it only
  leave X") was resolved by guessing instead of asking, and the guess drove
  a wrong default + wrong-direction "fix". The KiCad screenshot settled it
  in one message. Ask when the design intent is ambiguous.

## 2026-07-14 — third live burn: NonConductor zones + two latent bugs

- The operator's third burn was "almost perfect" except the board's right
  side was left un-ablated, and they supplied the KiCad screenshot: that area
  is a **filled copper zone with no net** (parts are REF** placeholders).
  KiCad plots no-net zone fill as G36 regions tagged
  `%TA.AperFunction,NonConductor*%` — copper not part of the circuit. The
  inverter kept it as copper; the operator expects dead copper ablated.
- New default: `pcbforge emit`/`noncopper` clear NonConductor copper
  (`AttributedLayer::layer_without_nonconductor`), with `--keep-nonconductor`
  restoring pour-keeping (the old FlatCAM-style minimal-ablation workflow).
  The console reports what was cleared/kept.
- Investigating why my "fix verification" render looked right EXPOSED THAT
  THE FIRST VERSION OF THE FIX WAS DEAD CODE plus two latent bugs:
  1. **Regions carried no aperture attributes.** emit() assumed a G36 region
     has no aperture function; per Ucamco §5.6 (%TA applies to subsequently
     created objects) and KiCad's emit pattern, a region takes the current
     dictionary attributes. Fixed: regions record `aper_dict.function`.
  2. **geom::offset silently deleted rings.** KiCad 10 emits adjacent region
     vertices 1–3 nm apart; cavalier's 10 nm position fuzz treats them as
     "repeat position vertexes" and panics in BOTH the trimmed attempts and
     the winding reference. ref_area became 0, an empty attempt "validated"
     against it, and the ring vanished — which is why my offset-0.025 runs
     happened to clear the zone (accidentally correct) while the operator's
     offset-0 run kept it. Two fixes: consecutive vertices within 25 nm are
     collapsed before entering cavalier (5 decades below the 1 µm chord
     tolerance), and an empty offset result is only accepted when collapse is
     geometrically plausible (erosion with area ≤ perimeter·|η|); if both the
     ladder and reference fail on a non-collapsible ring, the ring is emitted
     un-offset (conservative) rather than deleted.
- Regression tests: geom (stuttered-vertex square survives dilate and erode),
  ingest (hand-authored NonConductor region carries the attribute; exclusion
  removes exactly the zone area), e2e on the operator's gerbers at offset 0
  (zone-interior probe in fill by default, copper with --keep-nonconductor;
  substrate always fill; pad never fill).
- Meta-lesson recorded: the offset-0.025 "success" render was right for the
  wrong reason; only probing the un-offset run separated the exclusion fix
  from the accidental ring-deletion. Verify fixes at the parameter values
  that isolate them.

## 2026-07-14 — emit fan-burn fix: VertID/PrimID must be unique per shape

- The operator burned the first emitted job on a real KiCad 10 board
  (uv_test) and returned a photo, the LightBurn preview, the job file, and
  the source gerbers. Symptom: a fan of rays burned from the board's
  top-left corner across the pad field, visible identically in LightBurn's
  own preview (so a file-interpretation defect, not a machine one).
- Forensics: every vertex in the job file was textually clean and rendering
  the file's geometry as intended reproduced the design perfectly — the
  geometry was right, LightBurn's reading of it was not. The fan anchor
  (0, 12.025) is exactly shape 0's first vertex. All 37 Path shapes carried
  VertID="0" PrimID="0", copied verbatim from the single-path sample:
  LightBurn treats these as vertex/primitive-list identifiers and
  cross-links shapes that share them, so each ring's closing segment ran
  back to the shared list's vertex 0 — a fan with one ray per shape.
- Fix: the emitter assigns a unique, monotonically increasing VertID/PrimID
  per shape (first shape keeps 0, so the single-shape golden byte-match vs
  the pentagon sample is unchanged). Regression tests: unit (4 shapes → 4
  unique IDs) and e2e on the operator's committed uv_test gerbers (>30
  rings, all IDs unique, on-workspace).
- The operator's KiCad 10.0.3 gerbers are committed as fixtures
  (crates/cli/tests/fixtures/uv_test-*.gbr) — first real-user board in the
  test suite; the parser handled the KiCad 10 dialect (named %TD.AperFunction%
  deletes, \u escapes in .P values, NonConductor zone regions) unchanged.
- Note: cavalier_contours prints caught-panic traces to stderr during the
  offset retry ladder on this board (upstream #79 class; results unaffected
  — geom's catch_unwind + winding fallback handles it). Cosmetic; a panic
  hook silencer is a possible future nicety.
- Remaining verification on the operator: open the re-emitted job in
  LightBurn preview — fan gone confirms the ID semantics; if not, the next
  evidence needed is a LightBurn-authored file containing TWO drawn
  polylines to observe its own multi-path ID convention.

## 2026-07-14 — emit frame fix (from the operator's first real emitted job)

- The operator ran `pcbforge emit` on a real board and returned the output:
  structurally perfect (37 closed paths, correct recipe), but the geometry sat
  at y ∈ [-92.5, -80.5] — KiCad's plotted frame passed through verbatim, which
  lands below LightBurn's origin, off the workspace.
- Frame analysis (test-driven, after one false start): KiCad *negates* its
  internal y-down coordinate on Gerber export, so the plotted frame is
  **already y-up and unmirrored** — merely offset entirely negative. The first
  fix attempt added a y-reflection; the asymmetric-triangle orientation test
  failed and proved a flip would *introduce* a mirror. Correct fix:
  `cam::lbrn2::normalize_frame` = pure translation of the bbox min corner to
  (0,0). The earlier decisions note calling the gerber frame "y-down" is
  hereby corrected: it is y-up with a negative offset.
- Regression guards: exact-coordinate unit tests (plotted triangle keeps its
  top vertex on top; already-positive input only translates) and an e2e
  assertion that no emitted vertex is negative (the fixture's stroked outline
  reaches -0.025 without normalization, so this bites even without KiCad).

## 2026-07-13 — EMIT-2 + EMIT-3 (lbrn2 emitter + `pcbforge emit`)

- The operator supplied an 11th sample (`path-shape.lbrn2`, committed): a
  hand-drawn closed 5-sided polyline + an ellipse, establishing the
  `Type="Path"` encoding (identity XForm, absolute-mm `V<x> <y>` vertices,
  constant `c0x1c1x1` vertex tag, `PrimList` = `LineClosed`). There is no
  polygon tool in their LightBurn; the line tool produced exactly what was
  needed. The schema doc's Path gap is closed.
- `cam::lbrn2` (EMIT-2): EmitLayer{Fill→type="Scan", Line→type="Cut"} +
  AblationParams → CutSetting (frequency kHz→Hz, QPulseWidth int ns, defaults
  omitted like LightBurn does); geometry as Type="Path" shapes. Golden tests
  reproduce the sample pentagon's VertList byte-for-byte and every base
  CutSetting value from the committed samples — not transcriptions.
- The ONE inferred (not observed) field: open-path `PrimList` = `Line` (the
  sample's path is closed). Flagged in module docs + schema doc; verify on
  first live open-path job. Closed paths — the entire noncopper fill flow —
  are fully evidence-backed.
- `pcbforge emit` (EMIT-3): copper Gerber (+optional Edge.Cuts) → noncopper
  inversion → one Fill-layer .lbrn2, process recipe (power/speed/frequency/
  pulse/passes/interval/angle) as flags with the operator's base values as
  defaults, device default "BSLFiber". Holes/islands ride LightBurn's own
  fill grouping (nested closed shapes on one Fill layer), mirroring the SVG
  even-odd behavior the operator already uses.
- EMIT-2's prompt-level scope (multi-layer pass-grouped jobs from
  `Vec<PassGroup>`) is available via lbrn2_string(&[EmitLayer,...]) — the
  CAM-4 group → EmitLayer glue lands when the full compile pipeline (ORC)
  consumes it; the emitter itself is layer-count agnostic (two-layer sample
  validated the multi-CutSetting form).

## 2026-07-13 — EMIT-1 (lbrn2 schema) + samples landed

- The operator provided 10 real `.lbrn2` files (LightBurn Pro 2.1.03, device
  `BSLFiber`), each one setting apart from `base`. Placed in `samples/lbrn2/`
  under canonical names; `docs/lbrn2-schema.md` derives the field map by diff
  (frequency in Hz, QPulseWidth in integer ns, `type=Scan/Cut` = Fill/Line,
  `angle`/`numPasses`/`globalRepeat` omitted-⇒-default, Rect `XForm` affine).
- Fixture manifest change: `uv-base.lbrn2` was unobtainable (fiber-only rig),
  so EXPECTED_LBRN2 uses `global-passes.lbrn2` instead — the operator supplied
  both pass-field variants (`numPasses` and `globalRepeat`), which is more
  useful than a UV stand-in. UV-device schema deferred until a UV profile
  exists. `cargo xtask fixtures` now exits 0 on the real repo, closing INF-3's
  last gate.
- Power is operator-fixed at 20% (MOPA fluence = pulse width + frequency); the
  emitter still writes power from AblationParams so power-varying rigs work.
- EMIT-2 gap: the samples contain only `Type="Rect"` shapes. Arbitrary
  toolpaths need LightBurn's `Type="Path"` encoding, which is absent from the
  samples and (per the evidence-only rule) will not be guessed — EMIT-2's
  geometry emitter waits on one sample containing a drawn polyline/polygon.
  The CutSetting/layer/project serialization is fully determined and can be
  built + golden-checked against the samples now.

## 2026-07-13 — Operator correction: MOPA fluence is pulse-width + frequency

- Operator (at the machine) corrected a wrong mental model: on their MOPA
  fiber the effective ablation energy is governed by **Q-pulse width and
  frequency** (plus speed/interval), not the "Max Power %" field, which sits
  fixed (observed greyed at 20%). Physics they confirmed: peak power
  ~= P_avg / (frequency * pulse_width), so a shorter pulse concentrates the
  same pulse energy into less time and raises peak power (more aggressive,
  cleaner ablation). Frequency trades the other way.
- Consequences:
  - EMIT fixture set: replaced `power.lbrn2` with `pulse-width.lbrn2` in
    xtask's EXPECTED_LBRN2; frequency.lbrn2 retained. The two files that
    isolate the real knobs are pulse-width and frequency.
  - `AblationParams` already carries `frequency_khz` + `pulse_ns` alongside
    `power_pct`; the lbrn2 emitter (EMIT-2) will treat pulse width and
    frequency as first-class, and a derived peak-power term
    (P_avg/(f*pulse)) is the natural input for heat-aware logic / the material
    table's ablation-strength axis (rather than the % field).
  - The earlier "unlock the greyed Max Power field" line of investigation was
    a misread of the machine and is abandoned.

## 2026-07-13 — CAM-9 (tiling, stretch) — geometry only

- `cam::tiles::tile(&Paths, field_mm, overlap_mm) -> TilePlan` splits an
  oversize job into ≤ field-mm tiles overlapping by overlap-mm (defaults
  140 / 2 mm). Ownership is by element **centroid** — each element is assigned
  whole to exactly one authoritative tile, never clipped. That makes the union
  of tiles exactly the original job (the done-when's "== original within 1 µm"
  holds to 0), and "every stitched element in exactly one authoritative set" a
  true partition. Adjacent field windows overlap by exactly overlap-mm by
  construction (stride = field − overlap).
- An element larger than one field in some axis cannot fit any tile; rather
  than mis-tile it silently it is still assigned but reported in
  `TilePlan.oversized` (the backlog's "stop and surface" rule). Real jobs are
  many small path elements, so this is an edge guard, not the common case.
- Per the prompt: execution needs ComMarker Studio to drive the slide between
  tiles; VIS-6 will later fill in the real re-registration handshake. This
  task is geometry only — `ReRegister` records which field to bring under the
  head; nothing here commands motion. Deviation from the stated
  "Depends: VIS-6": the geometry stands alone and is tested without it.

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

## 2026-07-14 — VIS-4 notes (fiducial detector, built from the field photo)

- The operator sent a phone photo of the real blank on the machine bed with
  the actual fiducial recipe: **three 1 mm holes drilled at (10,10), (60,10),
  (10,60) mm** — an L-layout that disambiguates orientation. The photo bytes
  arrived as chat vision only and could not be committed as a fixture; the
  synthetic test renderer reproduces its salient hazards instead (specular
  glare gradient across bare copper, sensor noise, honeycomb-bed decoy holes
  that look exactly like drilled fiducials).
- VIS-4 formally depends on VIS-3 (bed homography), which needs the machine.
  The detector is implemented now with the px↔mm mapping parameterized as
  `BedMap` (any homogeneous 3×3, perspective-divided) so VIS-3's calibration
  drops in unchanged; tests use scale/rotation affines. The live done-when
  (three burned annuli + 1 mm pallet-nudge consistency) remains operator-side.
- Pipeline deviation from the prompt's "centroid → paraboloid sub-pixel": the
  intensity-weighted centroid over the (1-px-dilated) component IS the
  sub-pixel estimate — it is unbiased on an anti-aliased disc — while the
  paraboloid fit on the matched-filter response is a consistency check that
  feeds the confidence score, not the position. Two synthetic findings drove
  this: (a) a square weighting window let a grazing decoy blob drag the
  centroid ~2.6 px, fixed by restricting support to the component's own
  pixels; (b) a half-pixel bias traced to coordinate conventions, resolved by
  adopting OpenCV's pixel-centers-at-integer-coordinates convention,
  documented on `BedMap` — VIS-3 must use the same convention.
- Local robust statistics (median/MAD per search window) rather than global
  thresholds: the photo's glare gradient is board-scale, so a few-mm window
  sees it as nearly flat. Low contrast returns `Miss::LowContrast { snr }`
  (lighting problem, not code — per the prompt), and decoys of the wrong
  size are rejected by area/circularity gates even when closer to the
  expected position than the true hole.

## 2026-07-14 — FLD-3/4/6 notes (emit-path field follow-ups)

- FLD-3 (panic spam): the field-notes repro (uv_test @ offset 0.025) no longer
  panics — the earlier DEDUPE_NM pre-dedupe removed that trigger — but a direct
  probe proved cavalier_contours 0.7.0 still panics internally on pathological
  collapsing offsets (`pline_view.rs:290`), and `catch_unwind` recovers the
  value while the *default panic hook* still dumps the message + backtrace note
  to stderr. Fix: `geom::silence_cavalier_panics()`, a `Once`-guarded hook that
  suppresses panics whose `location().file()` is inside `cavalier_contours` and
  forwards everything else to the previously-installed hook (so genuine panics
  still report). Verified by a self-exec regression (`cavalier_panic_silence.rs`)
  that re-execs the child under a pathological-offset hammer and asserts the
  child's stderr carries a completion marker but no cavalier chatter — the only
  reliable way to observe a process-global hook without racy fd redirection.
- FLD-4 (anglePerPass): added `EmitLayer.fill_angle_step_deg`, emitted as
  `<anglePerPass>` in the Fill branch when non-zero (omitted at 0, matching
  hand-authored files). Value/units confirmed against samples/lbrn2 two-layer
  C01 (`anglePerPass=20` beside `numPasses=25`). CLI flag `--angle-step-deg`.
- FLD-6 (placement): `cam::lbrn2::place_frame(polys, tx, ty, center)` — bbox
  min corner (default) or bbox center (`--center`) lands on `--origin-x/-y`.
  Kept separate from `normalize_frame` (which stays the corner-to-origin
  default) so existing frame tests are untouched; emit applies it only when a
  placement flag is non-default. e2e test asserts rigid translation (extent
  preserved) and the center-straddles-origin case.

## 2026-07-14 — VIS-10 notes (board-frame warper)

- `to_board_frame` is a gather (inverse-map + bilinear): each design-frame
  output pixel walks design-mm → bed-mm (board affine, VIS-5/6 registration) →
  camera px (BedMap, VIS-3 homography) and samples. Gather, not scatter, so the
  output has no holes; out-of-frame taps read black.
- Reuses the pieces already built: `BedMap` (VIS-4) is the bed↔px map VIS-3
  will populate, `fit_affine` (VIS-5) produces the board affine. VIS-10 nominally
  depends on VIS-6 (real galvo/board registration), which needs the machine —
  but the warp itself is calibration-agnostic (takes the affine + bed map as
  inputs), so it's implemented and verified synthetically now; the live
  done-when (burned annulus within 2 px of expected raster position) stays
  operator-side.
- Synthetic done-when mirrors the live one: a disc imaged through a realistic
  bed map + board affine is warped back and re-detected with `find_fiducials`;
  its recovered position sits < 2 px from `board_mm_to_raster(board_pt)`. The
  paired `board_mm_to_raster` helper is the exact inverse of the sampling grid,
  so callers (ORC-7 drill guide) can predict where any board coordinate should
  appear in the warped view.
- Axis convention documented on the module: design-frame raster is y-down like
  every other image here; a y-up design raster (if ever wanted) should flip the
  board affine's y, not the warper. This unblocks ORC-7 (guided drilling).

## 2026-07-14 — DRV-1 notes (capture campaign kit)

- Delivered the three artifacts: `docs/capture-plan.md` (operator procedure +
  the 14-row one-variable-per-capture matrix 00–13 + safety), `tools/capture.sh`
  (dumpcap/tshark recorder, one experiment per file, CSV manifest rows, clobber
  guard), and `captures/` with `MANIFEST.csv` header + README. The xtask
  fixtures validator already had the hook (`captures/` required once
  capture-plan.md exists) — now satisfied.
- **Verification gap, stated up front in the plan:** the cloud container has no
  USB stack — `usbmon`, `tshark`, `lsusb`, and any usbmon interface are all
  absent — so DRV-1's own done-when (dumpcap records a dummy run on any USB
  device) and the `man usbmon`/`man tshark` syntax cross-check could not be
  executed here. Instead the script's *logic* (arg validation, slug, CSV
  comma/quote escaping, baseline/variable columns, one-file-per-experiment
  clobber guard) was verified against a mocked `dumpcap` on PATH. The live
  dry-run + the real B4 captures are operator-side; the plan flags the syntax
  as needing man-page reconciliation before trust.
- Design choices: capture the whole `usbmonN` **bus** interface (not `usbmon0`
  all-buses, not a device display filter) so URB context survives for DRV-2's
  differencing; device-address filtering deferred to decode. Device address is
  called out as changing per replug (must re-check lsusb each session); bus is
  stable per physical port. Manifest carries `baseline` + `variable` columns
  precisely because DRV-2 differences on them; `sha256` left `-` for
  `cargo xtask fixtures` to fill when captures are committed.
- DRV-2..8 remain hard-blocked on the real captures (evidence-only decode —
  the prompt forbids filling protocol fields by analogy). DRV-1 is the unblock:
  its output is the entire input to DRV-2.

## 2026-07-14 — Machine identity correction: Omni X (UV), not B4 (fiber)

- Operator corrected that the production laser is the **ComMarker Omni X**, a
  **355 nm UV galvo**, not the "ComMarker B4" fiber MOPA the backlog/RES-4 were
  written around. Researched with citations → `docs/research/commarker-omni-x.md`;
  headline facts propagated to `docs/field-notes.md`.
- Reconciling finding: the Omni X is still a **JCZ / XY2-100** controller
  (EZCAD/BJJCZ family, `.cor` + `markcfg7`), the *same lineage* as the B4, so the
  DRV reverse-engineering method and the DRV-1 capture kit transfer directly —
  retarget the device (B4 → Omni X) and the LightBurn profile (fiber → Seacad/UV).
  DRV-7's open question ("is a Seacad clean-room driver tractable?") is answered:
  yes, it's JCZ, so DRV-7 folds into the main DRV-1..6 track rather than being a
  stretch side-quest.
- Process consequence: 355 nm UV is **cold ablation** — high absorption in
  copper and FR4, small HAZ, minimal char, < 50 µm tolerances. This substantially
  defuses the fiber-era char/HAZ concern (and the operator's earlier HAZ
  question). But **RES-4's fiber copper-ablation benchmarks do not transfer**; a
  UV recipe must be found empirically via the VIS-9 ladder. RES-4 should be
  re-headed fiber-only/archival. Throughput/max-copper-thickness at 5–12 W UV is
  unknown — do not assume industrial-depaneling tolerances at this power.
- **Resolved same day (operator):** the machine is the 355 nm UV Omni X, and
  its LightBurn device is merely *named* `BSLFiber` (a label on a UV machine, not
  a fiber source). So: keep `DEFAULT_DEVICE = "BSLFiber"` (the .lbrn2 root
  `DeviceName` must match the LightBurn device name); the UV research is
  load-bearing; the earlier MOPA-fluence behavior belongs to the UV laser's
  1–50 ns / 20–200 kHz regime. Asked before assuming (process lesson from the
  NonConductor episode) — the answer flipped the wavelength question cleanly.
- Did NOT bulk-rewrite the backlog's B4/"jcz-protocol" naming or re-head RES-4 in
  this pass — those are broad edits worth doing deliberately; captured as a
  follow-up rather than churned now. The DRV method still applies (Omni X is
  JCZ/XY2-100); the naming is cosmetic.

## 2026-07-14 — UI-1 notes (egui operator console)

- **Architecture forced by the environment:** the container has the GL/X11
  runtime `.so`s but no pkg-config dev files, and no display. So the whole UI is
  an **egui-only library** (`crates/ui`, pure Rust — compiles and is *tested*
  headless) and the OS window is a thin **`eframe` wrapper behind the `native`
  feature** (`src/main.rs`, bin `pcbforge-console`). egui computes real frames
  without a display; only eframe needs one. Result: the lib + logic are fully
  verified here, and `--features native` even *compiles* (43 s) — it just can't
  open a window headless.
- **Verified:** 11 tests — the scanline even-odd preview rasterizer (holes /
  islands / layer order), the DB status snapshot (missing-DB and fresh-DB), the
  CLI-verb runner (`run_capture` stdout+exit and spawn-failure), and two
  **headless full-console frame** tests (`ctx.run` lays out every panel with no
  display, asserting tessellated output). The preview panel image was dumped to
  PNG on the real uv_test board (via the `dump_preview` example) and matches the
  SVG preview / KiCad screenshot exactly.
- **UI-1 done-when coverage:** current board state ✓ (status panel), Next-stage
  shells `pcbforge next` into the log pane ✓. Two gaps left as follow-ups:
  (a) the **live-video panel is a stub** — VIS-1 (camera) is hardware-gated
  (FLD-10); (b) verb output is **synchronous-captured**, not incrementally
  streamed — the console blocks for the verb's duration (FLD-9, thread+channel).
- **Constraint honored:** the actions panel only *shells the CLI* — no engine
  logic in the UI. The one in-process computation is the preview rasterization
  (`preview_image` inverts copper via `cam::noncopper`, a pure geometry
  function), which is a *view* concern, not a duplicate of the stage engine; the
  real job is still produced by shelling `pcbforge emit`.

## 2026-07-14 — Console fiducial-check view (VIS-4 surfaced in UI-1)

- Operator request: a preview that shows where fiducials are being detected so
  they can confirm correctness before trusting registration. Planned in
  docs/plans/ui-fiducial-check.md (approved), implemented in crates/ui
  (`fiducial.rs` + a Fiducials tab in `app.rs`); no change to `vision` — VIS-4's
  `find_fiducials` is used as-is.
- The overlay is rasterized into an `egui::ColorImage` (cyan expected
  crosshairs, green/amber detected rings by confidence, red ✕ + reason on
  misses) rather than drawn with egui-painter vectors — same rationale as the
  job preview: a ColorImage is verifiable headless (dumped to PNG, pixel-
  asserted) and shown to the operator, and reuses the console's texture path.
- Verified: 6 fiducial unit tests (operator L-layout all-found + green marks;
  low-contrast → MISS row naming the SNR; a decoy 2.5 mm off is NOT marked;
  layout parser; input guards) + a headless Fiducials-tab layout test. The
  `dump_fiducials` example renders the overlay on a synthetic field-photo-like
  frame (holes + glare + decoy): 3 strong, ~500 µm offsets matching the seeded
  board nudge, decoy correctly unmarked.
- Two pre-VIS deferrals, tracked and stated in-UI: the frame is a *file* (saved
  grab / photo) until VIS-1 gives a live feed (FLD-11), and px↔mm is a uniform
  `BedMap::uniform_scale` until VIS-3 gives the real homography (FLD-12). The
  overlay/detection code takes a real `BedMap` and live frame unchanged when
  those land.

## 2026-07-14 — VIS-6 register (host-side fiducial registration, software half)

- Operator asked whether emit adjusts LightBurn coordinates to the fiducials —
  it did not. Built `pcbforge register` + `cam::register`: fit a design→machine
  affine (via `vision::fit_affine`, VIS-5) from fiducial correspondences and
  apply it to the emitted geometry so the job burns where the board sits.
  `cam::register` is dependency-free (takes 6 affine coeffs, applies to Poly
  vertices nm→mm→affine→nm); the CLI owns the nalgebra/vision fit.
- Two input modes: `--fiducials "dx,dy=tx,ty; …"` (explicit) and `--frame +
  --layout + --px-per-mm` (detect via VIS-4 find_fiducials, DarkDot). Misses are
  skipped; ≥3 required; a fit whose RMS exceeds `--max-rms-mm` (default 50 µm) is
  rejected rather than baking a bad transform; a negative-determinant
  (reflecting) fit is refused.
- **FRAME CONTRACT (important, documented in --help + here):** the fit is applied
  to the **Gerber-frame** geometry with no origin normalization, so the "design"
  side of each correspondence must be in the Gerber frame. Exporting the KiCad
  Gerber with the drill/place-file (aux) origin makes Gerber coords = board
  coords, so a fiducial drilled at board (10,10) is simply `10,10`. Verified:
  identity correspondences leave geometry untouched; a pure +50,+30 translation
  shifts every vertex exactly (e2e).
- **Composition with galvo calibration:** a full registration is
  `board_affine ∘ galvo_affine` (design→bed→galvo). The galvo half needs a
  burned calibration grid (VIS-6's `calib grid`, hardware). Until it exists the
  caller supplies correspondences already in the target/machine frame (jog the
  pointer to each fiducial and read mm, or a workspace-calibrated camera); the
  two affines multiply trivially when the galvo one lands. So VIS-6 is marked
  `[~]` — register software half done, galvo grid + live ≤20µm residual gated on
  hardware.
- Verified: cam::register unit tests (identity/translation/rotation/holes/
  reflection-flag) + register_e2e (identity-unchanged, translation-exact,
  high-RMS rejected, too-few rejected, --frame detect→fit→emit, mutually-
  exclusive inputs). 41 workspace test binaries green.

## 2026-07-14 — Console drag-to-place + CLI invocation fix

- Operator request: drag a live preview of the circuit over the camera view to
  choose where it's etched. Added a **Place-on-board** tab (ui::place): overlay
  the job's to-ablate geometry semi-transparent over the bed frame at a
  Placement (translate + rotate about the job bbox center); drag the overlay or
  use x/y/rot controls; "Etch here" bakes the placement in.
- **Reuses Phase A, no second emit path:** a manual placement *is* an affine, so
  it's encoded as three synthetic fiducial correspondences (pivot + two unit
  offsets, non-collinear) and shelled to `pcbforge register --fiducials`. The
  console never re-implements the transform/emit — it drives the verified CLI.
  Correspondences are formatted to 6 decimals so register recovers the affine to
  ~1e-5 (unit-tested via fit_affine roundtrip).
- Compositing is a translucent even-odd scanline fill of the placed geometry
  over the frame → ColorImage (verifiable headless, shown via texture). Proof:
  `dump_place` example renders the uv_test job placed at (40,30) mm rotated 15°
  over a synthetic bed — the tilt is visible.
- Same camera→machine calibration caveat as register (px/mm uniform scale until
  VIS-3); the placement frame is the bed/camera frame.
- **Operator-reported bug fixed:** the actions panel shelled `pcbforge`, absent
  from PATH in a repo checkout. The invocation is now a command vector
  (`cli_cmd`) defaulting to `cargo run -q --bin pcbforge --` (works from the
  repo); `--pcbforge <path>` overrides with a prebuilt binary. run_capture takes
  the program + prefix args.
- Verified: place unit tests (identity/translation placement, correspondences→
  affine roundtrip, composite footprint) + a headless Place-tab layout test. 41
  workspace test binaries green.

## 2026-07-14 — Console: quote-tolerant paths + live camera preview

- **Quoted paths (operator request):** file managers / drag-and-drop quote paths
  with spaces. Added `ui::clean_path` (strips balanced surrounding '…' or "…"
  plus whitespace) and applied it at every path input — Gerbers (job_shapes),
  fiducial/place frames, emit/register args. Unit-tested.
- **Live camera preview (VIS-1 surfaced):** new Camera tab (`ui::camera`) with
  two source kinds so a preview works whatever the operator has:
  - `Source::File` — re-read an image file each grab; any capture app that writes
    a frame to disk drives it. Default, cross-platform, verified headless.
  - `Source::Device(index)` — real webcam via **nokhwa** behind the `camera`
    feature (v4l2/AVFoundation/MSMF). Chose nokhwa over the spec's opencv: pure
    Rust, no system OpenCV, and it **compiled here for all three platform
    backends** (runs on the operator's machine; no camera in the container).
  - Live mode grabs each frame + `request_repaint()`; "Snapshot" saves the frame
    to a PNG and points the Fiducial + Place tabs at it — the bridge from live
    view into detection/placement.
- **VIS-1 deviation logged:** the original spec wanted opencv videoio + a
  `pcbforge cam --list/--grab` CLI. Delivered the *capability* in the console via
  nokhwa instead; the CLI verbs + opencv path remain (FLD-13). VIS-1 marked
  `[~]`. Live continuous fiducial detection off the feed is FLD-11.
- Verified: clean_path tests, camera File-source grab (incl. quoted path),
  device-without-feature message, and a headless Camera-tab grab→snapshot flow
  that lands the frame in the Fiducial/Place tabs. clippy clean with and without
  the `camera` feature; 41 workspace test binaries green; `--features camera`
  builds (nokhwa, 31 s).

## 2026-07-14 — Camera threading + fiducial-derived scale

- **Camera I/O off the UI thread (operator-reported freeze):** live grabbing was
  synchronous in `ui()`, blocking the GUI. Added `camera::Capture` — a
  background thread that streams frames over a 1-slot channel; the UI polls
  `latest()` each frame (non-blocking, drains to newest) and `request_repaint()`s.
  Device capture opens the camera **once** and loops (no per-frame reopen). The
  thread stops on `Drop`. "Grab once" stays synchronous (a deliberate one-shot).
  Verified with a background-capture test that polls without blocking.
- **px/mm derived from the fiducials (operator question "why type it?"):** the
  field is only a *seed* — detection must convert the expected mm layout into
  pixel search windows before anything is found, so it has to be roughly right
  (tighter for far fiducials: a 60 mm hole needs the seed within ~search_mm/60).
  After detection, `measure_scale` computes the true px/mm from the detected
  fiducial spacing vs their known design spacing and reports it; a "use measured"
  button adopts it for the Fiducial + Place tabs.
- **register --frame now anchored to the fiducials, not the seed:** machine mm =
  detected px / *measured* px/mm, so the target spacing equals the design
  spacing and the fit is a pure rigid placement (rotation + translation, unit
  scale). Previously a wrong seed silently scaled the emitted job. The CLI logs
  the measured scale. Verified: seed 10 → measured 10.00, RMS 0.0; ui tests show
  the measured scale recovers truth from an off (9.5) seed with a wide window.
- Note: the seed still has to be close enough to *detect* far fiducials; an
  iterative detect→re-measure→re-detect pass (FLD-11 territory) would remove even
  that, but the current flow (Fiducial tab: adjust seed until 3 found → read
  measured → "use measured") is a clean manual loop.
