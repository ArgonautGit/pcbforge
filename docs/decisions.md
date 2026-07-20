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

## 2026-07-14 — Draggable fiducial search markers

- Operator: rather than getting the px/mm seed right, drag each expected marker
  near its hole and let the detector search locally. Implemented: the Fiducial
  tab now shows the frame as a texture with the expected markers (✛) drawn on
  top via the egui painter (not a re-rasterized overlay), each **draggable**.
- Decoupled the two roles the "expected" positions played: the **search center**
  (`fid_search`, per-fiducial bed mm, draggable) is what detection searches
  around; the **design layout** (`fid_layout`) stays fixed for registration.
  Markers seed from the design layout on load, then the operator drags them onto
  the real holes. Detection runs around the dragged positions.
- Painter-over-texture (vs the rasterized overlay used elsewhere) is the right
  call for interactivity — markers move without re-rasterizing the frame, and
  the drag math (screen↔mm) + hit-test (`fiducial::nearest_marker`) are pure and
  unit-tested. `check_frame` now also returns `found_px` per fiducial so the
  view can draw detected rings via the painter.
- Verified headless: a hole placed 3 mm off nominal is **missed** at the seeded
  marker (out of the 2 mm window) and **found** after the marker is moved onto
  it — the core value. Plus `nearest_marker` unit test. clippy clean (default +
  camera); 41 workspace binaries.

## 2026-07-14 — Perspective (homography) from fiducials

- Operator: the camera is tilted, so the fiducials must account for perspective.
  Confirmed the pipeline was affine-only: detection used a uniform-scale BedMap
  and registration fit a 2N×6 affine (translation/rotation/scale/shear) — no
  keystone. A homography is 8 DOF and needs **≥4** non-collinear points; the
  operator's 3-hole L-layout can only ever fix an affine, so this needs a 4th
  fiducial (told them so).
- Built `vision::fit_homography` — normalized DLT (Hartley) with `Homography`
  {matrix, residuals, rms}, `apply` (perspective divide), `try_inverse`. Tests:
  recovers a known keystone (rms<1e-9), the case that proves the point (an
  affine fit leaves rms>0.5 where the homography is exact), inverse round-trip,
  sub-pixel-noise robustness, too-few/degenerate errors.
- **Solver bug caught by the visual proof:** for exactly 4 points `A` is 8×9 and
  nalgebra's thin SVD drops the null-space vector (the 5-point tests passed, 4
  didn't — the example rendered garbage, rms 152). Fixed by solving `A·h=0` via
  the smallest eigenvector of `AᵀA` (always 9×9). rms→0 and the keystone renders
  correctly.
- Integration: the fiducial tab fits the design→pixel homography when ≥4
  fiducials are detected (reports reprojection RMS; "add a 4th fiducial for
  perspective" with 3), stored app-wide. `place::composite` gained an optional
  homography — the Place overlay now keystones onto the tilted board in the
  camera image (bed-mm→px via the homography instead of a uniform scale). Proof:
  `dump_place … persp` renders the uv_test job warped through a keystone.
- Honest scope: this perspective-corrects the **camera view** (detection,
  measurement, the Place overlay). Absolute laser-coordinate registration under
  tilt still needs the camera↔laser/bed calibration (VIS-3, hardware) — the
  fiducial homography rectifies what the camera sees, not where the galvo burns.

## 2026-07-14 — FLD-9 async verb output + FLD-11 live fiducial tracking

- **FLD-9:** `run_verb` was synchronous — the GUI froze for the verb's duration.
  Now `spawn_verb` runs the CLI on a background thread with piped stdout/stderr
  read by two reader threads (so a full pipe can't deadlock), streaming each
  line + an `[exit N]` footer over a channel. `run_verb` returns immediately and
  stores a `VerbJob`; `pump_verb` (called every frame) drains lines into the log
  and refreshes status on completion; a top-bar spinner shows "running…". One
  verb at a time (a second is refused while one runs). Verified with self-exec
  echo/sh tests (streams stdout+stderr, non-blocking, clears on done).
- **FLD-11:** the Fiducial tab gained a **● Live** toggle. It reuses the camera
  source (picked in the Camera tab) and its own `Capture` thread; each frame is
  grabbed, shown, and re-detected around the current markers, with the measured
  scale and (≥4) perspective homography refitting live — so the rings track the
  holes as the board is nudged, no manual Check. Detection is local (small
  windows) so per-frame cost is cheap. Verified with a File source of 4 holes:
  the live loop populates found_px + fits the homography, and stops the capture
  when Live is off.
- Caveat: fiducial-Live and camera-Live each open the source; on the same
  physical device that can conflict (device busy) — use one at a time. File
  sources are fine concurrently.

## 2026-07-14 — UI-2 AR overlay + FLD-12 profile selector / click-to-place

- **UI-2 (AR overlay):** the Camera tab gained a **🔲 AR overlay** toggle that
  projects the *registered* design over the live/grabbed frame — the natural
  generalization of the Place overlay (which only showed the ablate region at a
  *manual* placement). "⤵ Load design" caches the Job-tab Gerbers as three layer
  sets (board / copper / ablate) with per-layer checkboxes; each enabled layer
  is blended over the frame through the shared fiducial homography (design-mm →
  px) with an **identity** placement, so Gerber coordinates go straight through
  the map — the same frame contract as `register --frame` (fiducials at their
  design coords). No homography yet ⇒ it falls back to a uniform `fid_px_per_mm`
  scale and labels itself "unregistered — detect ≥4 fiducials to register", so
  the operator is never misled into trusting an un-registered overlay.
- To stack layers without re-rasterizing, `place::composite` was split into a
  `composite_over(&mut ColorImage, …)` that blends one layer in place;
  `composite` now builds the base image and calls it once. The AR path calls it
  per enabled layer. Verified headless: a copper square maps through a 5 px/mm
  homography to the expected pixel (tinted), a disabled layer leaves the frame
  gray, the far corner is untouched.
- Scope: still an image-space overlay (camera view), not galvo coordinates —
  absolute burn registration under tilt remains VIS-3/hardware. Drill-center
  markers are deferred until the console ingests Excellon (only Gerbers are
  loaded today); the three copper/ablate/board layers are a faithful design
  projection meanwhile.
- **FLD-12 (profile selector):** `check_frame`/`check` took a hard-wired
  `DarkDot`; they now take a `&FiducialProfile`, and the Fiducial tab has a
  profile combo (Dark dot / Annulus / Backlit) via a small `ProfileKind` `Copy`
  enum that pairs with the diameter field. `FiducialProfile::diameter_mm` was
  made `pub` so the overlay ring size derives from the chosen profile. Verified
  the selector is wired: a bright-blob (backlit) frame is found with Backlit but
  the dark-dot matcher does not strongly lock it.
- **FLD-12 (click-to-place):** a **✚ click-to-place** toggle — a click on empty
  frame (not on an existing marker) appends an expected fiducial there via
  `add_expected_fiducial`, which edits the layout string (kept as the single
  source of truth, so `sync_fid_markers` reconciles the ✛ set and the homography
  correspondences follow). Dragging still fine-tunes existing markers. Verified:
  a click appends a coord + marker, and appending onto an empty layout produces
  no leading separator.
- Left partial (`[~]`): the "real VIS-3 BedMap instead of a uniform scale" half
  of FLD-12 is hardware-gated — the px↔mm map stays a uniform scale until the
  bed homography is measured on the machine.

### FLD-12 follow-up — click-to-place was add-only

- Click-to-place could only *append* expected fiducials (on top of the 4
  defaults), so the marker set only ever grew. Added **right-click-to-remove**:
  a secondary click on a ✛ drops that fiducial via `remove_expected_fiducial`,
  which deletes the matching layout token (preserving the others' exact text —
  not a lossy reparse) and the index-aligned search/found entries, so the set
  shrinks and survivors keep their dragged positions. Hint text now reads
  "left-click adds, right-click removes, drag fine-tunes". Verified: removing
  the middle of three drops its token and keeps the survivor's dragged position
  aligned by index.

## 2026-07-14 — FLD-13 `pcbforge cam` verbs + shared `capture` crate

- FLD-13 asked for `pcbforge cam --list/--grab` "reusing ui::camera". The camera
  code was inside `ui`, which depends on `egui` — the CLI shouldn't link the GUI
  stack. Since the capture module and `clean_path` are already egui-free, they
  moved into a new **`capture`** crate (deps: `image`, optional `nokhwa` behind
  its own `camera` feature). Both `ui` and `cli` now depend on `capture` and
  forward the `camera` feature to it (`camera = ["capture/camera"]`).
- `ui` keeps its call sites unchanged via `use capture as camera;` at the crate
  root (so `crate::camera::Capture` still resolves) and re-exports
  `Capture/Source/grab/list_devices/clean_path`. Its old `src/camera.rs` and the
  duplicate `clean_path` were deleted — one implementation now.
- CLI surface: `pcbforge cam --list` prints `index: name` per device (or a
  guide to the `camera` feature / a File source when none); `--grab <out.png>`
  writes a grayscale frame from `--file <path>` (works everywhere) or
  `--device <i>` (needs the feature). `--device` + `--file` together, or neither
  with `--grab`, are usage errors; a device grab without the feature fails with
  a message that names the feature. Verified end-to-end (grab a synthetic frame,
  list, no-feature device error, usage error) and covered by `cam_e2e`.
- Scope: the opencv capture path from VIS-1's original spec is still unbuilt —
  the File + nokhwa sources cover the operator's setup, so it stays deferred.

### Place-drag follow-up — track the cursor under perspective

- Dragging the Place overlay felt wrong once a perspective homography was
  active: the old handler added a uniform mm delta to the placement translation,
  but a uniform mm step is not a uniform pixel step on a tilted plane, so the
  overlay slid *along* the plane instead of following the cursor. Fixed by
  moving in pixel space: map the pivot bed-mm → px through the same homography
  the composite uses, add the (scaled) drag delta in pixels, then invert back to
  bed-mm (`drag_place_px`). With no homography this reduces to the original
  ÷px_per_mm move. Verified: under a keystone homography a (dpx,dpy) drag shifts
  the pivot's projected pixel by exactly (dpx,dpy); the uniform case still adds
  delta÷ppm to the translation.

### Overlay legibility — outline instead of a blob

- The Place/AR overlay filled each region translucently, so a solid ablate area
  read as a red blob. `composite_over` now draws a soft fill (alpha ×0.4) plus a
  crisp 2 px outline on every ring — outer *and* holes (alpha ×1.8, clamped) —
  via a Bresenham `stroke_edge` + bounds-checked `blend_px`. Traces, pads, and
  the board edge are now legible over the board; verified by rendering
  `dump_place` (curved trace + pads + outline all crisp) and by a test asserting
  the edge is ≥30 stronger than the interior fill.

## 2026-07-14 — Double-sided support (ORC-6 groundwork)

Operator asked for front+back Gerbers, the camera locking onto the same
through-holes after a left-right flip, and the beam-angle parallax that puts a
hole's back opening at a different XY. Decisions (all confirmed with the
operator before building):

- **Mirror in software, X only.** KiCad exports `B.Cu` in top-view coords, so a
  left-right flip needs the design mirrored in X. `cam::flip::mirror_job` reflects
  and **re-winds** each ring (reflection flips orientation; reversing restores the
  outer-CCW/holes-CW convention). `register` still rejects reflected *fits*, so
  the mirror is baked into the design, never the fitted affine.
- **Analytic f-theta parallax, ~70 mm lens.** A non-telecentric f-theta sends the
  beam to a field point at `tan θ ≈ r/f`; drilling depth `t`, the exit opening
  sits at radius `r·(1 + t/f)` about the scan center (`entry_to_exit_mm`). At the
  Omni X glass lens (70 mm) through 1.6 mm FR4 that is ~0.8 mm at a 35 mm field
  radius — real, so `back_expected_fiducial_mm` = mirror(exit-offset(design)) is
  what the detector expects on the back.
- **Fiducials: same through-holes, dark-dot from the back.** No extra drilling;
  the console's fiducial markers on the back are seeded from the mirror+offset
  positions so they land on the flipped holes.
- **Scan-center default = fiducial-layout centroid**, and the display mirror axis
  is that centroid's vertical line (keeps the markers on-screen; a mirror about
  x=0 would push them negative). The operator can refine the scan center once
  VIS-3 gives the real bed map — noted as the one un-calibrated assumption.
- **UI wiring:** a Front/Back selector in the actions panel with back-gerber +
  thickness/focal inputs; `active_job()` feeds the (mirrored-on-back) geometry
  to the Job preview, AR overlay, and Place tab; `emit_clicked` shells
  `emit --mirror-x` on the back; `set_side` clears per-side caches. Surface
  copper etch is *not* offset (only through features are) — the parallax is
  applied to fiducial expectations, not the burned traces.
- **Scope left for hardware:** stage-engine flip stages (ORC-6 proper), and the
  live residual/bottom-cross acceptance. The geometry/optics kernel is pure and
  fully unit-tested so those slot on top unchanged.

### ORC-6 continued — scan-center override + the stage-engine flip branch

- **Scan-center field:** the back-side form gained a "scan center: auto" toggle.
  Auto keeps the fiducial-centroid default; unchecked exposes x/y mm inputs for
  the measured lens axis. Physics check in the test: with the axis put exactly
  on a fiducial, that hole's expected back position collapses to the pure
  mirror (no parallax on-axis) while the others keep their offset.
- **Stage engine (the "branching successors reserved for ORC-6"):** `StageDef`
  gained `next_alt` (validated: must name a real stage, and a branch stage must
  keep its default `next`), and executors a `StageOutcome::AdvanceAlt`. New
  `StageKind::Flip` / `FlipExecutor`: single-sided boards record `flip_skip` and
  pass through to `done`; double-sided boards record a `flip_prompt` naming the
  mirror-aware registration coordinates (mirror across the flip axis + beam
  entry-exit offset; console Back side computes them) and branch into
  `fiducials_bottom → bulk_bottom → iso_check_bottom → done`.
- **Where "double-sided" lives:** the DB has no migration machinery (v1 stamp
  only) and `stage_state` is executor-owned, so no schema change. Bring-up
  signal is `PCBFORGE_DOUBLE_SIDED=1` read by the default registry's
  FlipExecutor — the same pattern as `EnvPalletSource`/`PCBFORGE_PALLET_TAG`.
  Tests inject `FlipMode::{SingleSided,DoubleSided}` explicitly (env is
  process-global and hazardous under parallel tests). The real signal becomes a
  board/design attribute (e.g. .gbrjob layer count via ING-5) once the
  scheduler binds real designs — deliberately deferred.
- Walk coverage: the single-sided walk now passes through `flip` (skip row in
  the runlog), and a new walk drives a double-sided board across process
  restarts through the whole bottom flow, asserting the branch, the prompt
  text, and the `ablate-bottom` emit intent.

## 2026-07-14 — ORC-7 guided drilling (software half)

- `pcbforge drill-guide` (cli::drillguide + verb): one invocation per step, the
  `pcbforge next` pattern — a small text state file (v1 header, fingerprint,
  pending index) makes the flow restart-safe, and an FNV fingerprint of the
  ordered hole list rejects progress carried over from a *different* drill file
  instead of silently mis-pairing indices.
- Ordering: largest bit first (one bit change per size, the spec's
  "largest-bit-first"), ties by (y, x) for a stable walkable path. G85 slots
  contribute both endpoints per ING-2's documented drill-then-file workflow.
- Confirmation: VIS-4's dark-dot detector at the bit diameter, gated at
  ≤ `--tol-um` (default 150 µm per the spec) around the target; an undrilled or
  misplaced hole exits non-zero and does not advance. The frame is the
  registered view at a uniform `--px-per-mm` (pre-VIS-3 contract, same as the
  fiducial check).
- Overlay PNG per step (and as the final archive): confirmed holes ringed
  green, current target crosshaired red, remaining dim; bit changes prompted
  ("fit the 0.40 mm bit"). Verified visually and by unit + e2e tests — the e2e
  walks a 3-hole board across process invocations including the refusal and the
  stale-state error.
- Live done-when (a real 20-hole board, every hole confirmed) is operator-side;
  the ≤150 µm gate and overlay archive are exactly what that run needs.

### Fiducial check — camera-first

- The fiducial check previously only read a *file* (the camera reached it via
  ● Live or the Camera tab's snapshot button). Now the camera is the primary
  path: a **📷 Grab & check** button pulls one frame from the camera source
  (device or file, as picked in the Camera tab) and detects in the same click;
  and **🎯 Check fiducials** with the frame-file field left empty falls back to
  grabbing from the camera, so the flow needs no file paths at all. The file
  field stays (relabelled "frame file (optional)") for checking saved images.
- "↺ reset markers" no longer reloads the frame file (which errored when the
  frame had come from the camera) — it just reseeds the ✛ set from the layout
  and keeps the current frame.
- Verified headless with a File camera source (the capture-app contract):
  grab-and-check detects the holes in one step, Check-with-no-file reaches the
  camera, and a dead source surfaces `camera: …` in the note instead of
  panicking.

## 2026-07-15 — Place/etch y-frame fix (operator: "the .lbrn2 doesn't match where I placed it")

- Root cause: two frame conventions were conflated in every **uniform-scale
  fallback**. LightBurn's workspace is y-up (evidence-pinned by the
  asymmetric-triangle test and validated on real burns), but the console
  derived bed mm from camera pixels, whose rows grow **downward** — so
  "Etch here" emitted y as distance-from-the-image-top while the machine reads
  distance-from-the-bed-bottom: the burn landed vertically mirrored from the
  screen placement, with the on-screen overlay also mirrored vs reality.
  Confirming smell: `register --frame`'s detected correspondences fit a
  *reflecting* affine (y-up design → y-down pixels), which the
  negative-determinant gate would reject outright.
- The **homography path was already correct** — it learns its orientation from
  data (the operator's y-up fiducial layout ↔ detected pixels), so placements
  made with ≥4 detected fiducials were unaffected. Only the no-homography
  fallback conflated frames.
- Fix: one convention everywhere — **bed mm is y-up with its origin at the
  frame's bottom-left**, flipped against the frame height only at the image
  boundary (`py = H − y·ppm`). New `BedMap::uniform_scale_y_flip` used by the
  fiducial check, `register --frame` (both detection and the machine-mm
  conversion), `drill-guide`, the Place/AR composite fallback, the Place drag
  fallback, and the fiducial tab's marker screen mapping. Homography branches
  untouched.
- Operator-visible change: bed/layout y is now measured **up from the bottom
  edge of the camera frame** (matching Gerber/machine y), the Place overlay
  renders un-mirrored (as the machine burns it), and mouse-up = bed-y-up.
- Regression pin: `overlay_row_matches_machine_y_up` places asymmetrically
  (3 mm above the bottom of a 200-row frame) and asserts the overlay row is
  `H − ty·ppm`, not the mirrored row — mid-frame-symmetric probes could never
  catch this class of bug, so several tests were rewritten off-center.

### Place "Etch here" — separate output file + self-report (bug fix)

- Symptom: the operator dragged the job to a spot, clicked "Etch here", and the
  `.lbrn2` showed the job at the workspace origin, not the placement. Root cause
  was **not** the math (register places the job correctly — proven: a placement
  at (29.3, 39.5) mm emits geometry centred at (29.30, 39.50)), but a **silent
  file clobber**: "Etch here" (register) and the Job-tab "Emit" both wrote
  `job.lbrn2`, and `emit` normalizes to the origin. Whichever ran last won; the
  operator was viewing the emit output.
- Fix: the Place tab now has its own `out .lbrn2` field (default `placed.lbrn2`),
  so a plain Emit can't overwrite a registered placement. "Etch here" logs the
  placement — `Etch here → placed.lbrn2: job placed at (x, y) mm, r°` — and the
  `register` CLI now prints the emitted geometry's machine-mm bbox + center, so
  the placement is self-verifying (register, unlike emit, does not normalize).

### Console input persistence

- The console lost its Gerber paths on every restart. Added a dependency-free
  `settings` module: a `key=value` file beside the DB
  (`pcbforge.console-settings`) persisting copper/outline/lbrn2 + offset, the
  back-side gerbers + thickness/focal, the place frame/output/scale, the
  fiducial frame/layout/scale, and the camera file. Loaded over the defaults in
  `ConsoleApp::new`; re-saved once per frame only when a field actually changed
  (no per-frame IO otherwise). Unknown/missing keys are tolerated so the file
  survives field churn. Verified by a restart round-trip test.

### Place "Etch here" — absolute output path (find-the-file follow-up)

- Follow-up to the clobber fix: the operator reported the placed job "still" at
  the origin in LightBurn. Diagnosis from the screenshot: the loaded file showed
  Q-Pulse 2 / Pass Count 3, but `register` writes QPulseWidth=1 / passes=1 — so
  LightBurn had a *different/stale* file open, not the freshly-written
  `placed.lbrn2`. Confirmed the register output genuinely carries the placed
  coordinates (identity XForm + absolute verts at the placement, matching
  LightBurn's own `path-shape.lbrn2` sample, whose Path verts are likewise
  absolute and non-normalized — so LightBurn preserves absolute Path positions).
- Root cause of the confusion: `place_lbrn2` was a bare relative filename
  written to the console's launch directory — invisible on a GUI. Fix:
  `resolve_place_output` now writes a bare filename **next to the copper Gerber**
  (beside the operator's inputs) and an absolute path as-is, and the log prints
  the full path with "OPEN THIS FILE (not the Job-tab emit output)".
- Standing limitation (unchanged, worth restating): the placement is in the
  fiducial/design frame the camera homography learned, not an absolute machine
  frame — so the burned position is only correct if the board's fiducial origin
  is aligned to the machine origin (or once VIS-3 provides the bed↔machine map).

### Camera orientation correction

- The operator's camera is mounted upside down relative to the app's expected
  frame (bed y-up, origin bottom-left). Added a camera **orientation** selector
  in the Camera tab — Normal / Flip↔ / Flip↕ / Rotate 180° — applied to every
  frame the moment it enters from the camera (one-shot grab and the live Capture
  threads, for both the Camera tab and the fiducial check), so detection,
  registration, and display all see the corrected image. Persisted in the
  console settings file. Verified: Rotate180 maps a top-left pixel to the
  bottom-right, and the choice survives a restart.

## 2026-07-14 — Camera→laser calibration (VIS-3, console)

Operator report: a job placed on the board burned at a different spot. Diagnosis
was not a placement bug — the software is self-consistent (the AR overlay shows
the design where you dropped it, and `register` emits those coordinates) — but
the **missing camera→laser link**. Fiducials tie the design to the board; nothing
tied the camera to the laser, so a placement in the fiducial/design frame isn't
in the machine's commanded frame, and the burn is offset by however the board
sits. Confirmed with the operator (burn a grid; 60 mm/10 mm 7×7; camera moves →
recal each session).

- **`pcbforge calib-grid`** emits an n×n grid of small filled squares at known
  commanded coordinates (default 7×7 @ 10 mm, dot 0.4 mm). The operator burns it.
- **`ui::calib::fit_camera_to_machine`** (kernel): from the burned-grid frame +
  the four hand-marked corner dots, an initial corner homography predicts every
  dot's pixel; `vision::find_fiducials` (dark-dot) refines each locally; the full
  set re-fits a **camera-px → commanded-mm homography** (perspective — absorbs
  the tilted camera). Synthetic-grid test recovers commanded coords < 200 µm
  through a keystone.
- **Console Calibrate tab**: generate grid → load burned frame (camera or file)
  → click the 4 corners (LL, LR, UR, UL) → Fit. Status shows dots/RMS.
- **Place integration**: `place_homography()` returns the calibration's inverse
  (machine-mm → px) when calibrated, else the fiducial homography. So drag +
  composite + `register` all work in **true machine coordinates** once
  calibrated; the Place note says "machine mm (calibrated)" vs "design frame".
- **Session-scoped**: the calibration is NOT persisted (the operator's camera
  moves between jobs) — recalibrate each session; the tab flags "not calibrated".
- Test-isolation fix uncovered here: `tmp_db()` returned a per-process shared
  path, so the new settings sidecar bled input fields between tests — made it
  unique per call.

### Calibration robustness — re-anchor against the persistent grid

- Operator asked: doesn't the static calibration assume the camera never moves?
  Yes — a single fit is valid only in the pose it was taken, and nothing
  detected drift. Fix: treat the **burned grid as a persistent bed reference**
  and re-fit against it, so camera movement is absorbed rather than silently
  wrong.
- `calib::re_anchor(frame, previous, grid, dot_mm)`: reuses the previous
  calibration to seed the dot search windows, re-detects, and re-fits — no
  corner clicks. Works as long as the grid is still in view and the camera
  hasn't jumped more than ~0.4·pitch (a bigger move fails and needs a fresh
  corner Fit). `fit_camera_to_machine` and `re_anchor` now share one `refit`
  core (seed → find_fiducials → fit_homography).
- Console: **⟳ Re-anchor** (one-click re-fit from a fresh frame) and **● Live
  anchor** (continuous per-frame re-fit via a Capture thread) on the Calibrate
  tab. The workflow becomes: leave the grid on the bed, place the board so ≥4
  dots stay visible, and the mapping tracks the camera live.
- Verified: fit at pose A, shift the camera ~2 mm, and re-anchor recovers
  correct commanded coordinates while the *stale* pose-A calibration is proven
  visibly off (so the re-anchor was genuinely needed).

### Calibration persistence — the taped-grid reference

- Operator's chosen workflow: a sheet of paper taped to the bed, burned with the
  calibration grid, re-anchored to until it's replaced. Since the paper (and its
  known machine coordinates) persist across sessions, the calibration is now
  **persisted** (reversing the earlier session-scoped decision): the px→mm
  matrix + grid params (n / pitch / dot / out) are saved in the console settings
  file. On restart the fit is restored as a **re-anchor seed** and shown as
  "◐ loaded last session — Re-anchor to re-lock to the taped grid" (found==0 ⇒
  unconfirmed); one ⟳ Re-anchor re-locks it to the paper (or a fresh corner Fit
  if the camera jumped too far). Verified the matrix round-trips across restart.

## 2026-07-14 — Camera lens-distortion calibration (VIS-2)

Operator's point: a homography only models a flat plane under perspective — it
can't represent the camera lens's barrel/pincushion *curvature*, and a burned
grid conflates camera + galvo distortion (circular). To get dimensionally
accurate geometry you must (1) make the camera a metric ruler with an
independent known-geometry reference, then (2) characterize the galvo against
it. This lands step (1); the operator chose a **printed** reference grid.

- `vision::fit_lens` (kernel): a bi-cubic 2-D polynomial `pixel ↔ true-mm`
  (normalized, least-squares), fit both directions from `(pixel, known-mm)`
  correspondences. Absorbs perspective *and* lens curvature. Tests: a realistic
  4% barrel fits < 30 µm RMS while a homography over the same points is > 300 µm
  (proving the polynomial was necessary); px↔mm round-trips.
- `ui::calib::fit_camera_lens`: reuses the grid-dot detection (corner seed →
  find_fiducials), fits the lens, and computes a per-dot **distortion field**
  (detected − perspective-predicted, px) + post-fit residual for the overlay.
- Console **Calibrate** tab is now two steps: **① Camera lens (printed grid)**
  and **② Laser anchor (burned grid)**. Camera mode: print grid, tape it, image,
  click 4 corners, Fit — enter the *measured* printed pitch (calipers) since
  printers scale. Visual feedback: magenta arrows per dot show the lens
  distortion (radial = barrel), rings colored by correction quality (green
  < 30 µm), RMS/worst readout, adjustable arrow exaggeration. `dump_lens`
  renders the textbook barrel field.
- Scope/next: full OpenCV 3-D intrinsics (multi-pose) are unnecessary for a
  fixed planar bed — one planar polynomial rectifies the view. Next is using
  this metric camera to characterize the galvo (VIS-6) and pre-warp emitted
  geometry (VIS-7/DRV-8) for dimensional accuracy.

## 2026-07-15 — CI fix: float-literal f32 fallback in Stroke::new

CI (GitHub Actions, `dtolnay/rust-toolchain@stable`) had been failing on every
recent commit at the clippy step — before it ever reached `cargo test`. Root
cause: the newer stable Rust on CI enables `float_literal_f32_fallback`
(rust-lang/rust#154024), which errors under `-D warnings` when a bare float
literal falls back to `f32`. `egui::Stroke::new(width: impl Into<f32>, …)` with
a literal `1.5`/`2.0`/`1.0` triggers it: the literal has no `f64: Into<f32>`
path, so it silently resolves to `f32`. The local toolchain (rustc 1.94.1,
2026-03-25) predates the lint and cannot reproduce it.

Fix: suffix the four affected width literals with `_f32` in
`crates/ui/src/app.rs` (the calibration/fiducial overlay strokes). Non-literal
widths and tuple `(w, color)` stroke forms are already concrete `f32` and
unaffected. No behavior change — purely making the intended type explicit.

## 2026-07-15 — Printable camera-lens calibration grid sample

Added a reusable printed reference for the camera-lens calibration step so the
grid workflow can be tested/used without hand-drawing a target each time.

- `samples/calibration/grid-7x7-10mm.svg`: a 7×7 dot lattice at 10 mm nominal
  pitch (2 mm `DarkDot`-profile dots), laid out in the machine frame
  (lower-left origin, +X right, +Y up) with corners numbered 1→2→3→4 in the
  console's click order, an origin axis marker, and a caliper dimension line
  across the bottom row. Sized in real mm (A4) so it prints dimensionally exact.
- `tools/gen_calib_grid.py`: deterministic, dependency-free generator
  (`--n/--pitch/--dot/--page`) so the grid can be reproduced at other pitches or
  on Letter stock; same args → byte-identical SVG.
- `samples/calibration/README.md`: how to print (100%, measure the true pitch
  with calipers since printers scale) and how it feeds step ①; notes the step ②
  burned grid is machine-produced via `pcbforge calib-grid`.

The fixtures manifest only tracks `samples/kicad` + `samples/lbrn2`, so this new
subtree needs no manifest regeneration.

## 2026-07-15 — Distorted-grid calibration test fixture

To verify the camera-lens calibration actually recovers geometry (not just that
it runs), added a synthetic distorted camera frame and an end-to-end test gate.

- `samples/calibration/grid-7x7-10mm-distorted.png`: the 7×7 / 10 mm grid imaged
  through a known perspective (tilted camera) + 5% radial barrel distortion,
  rendered as dark dots on a light, vignetted, mildly noisy field. Sidecar
  `…-distorted.json` records the distortion params, the four corner-dot pixels
  (GridSpec::corners_mm order), and every mm→px pair as ground truth.
- `crates/ui/examples/gen_distorted_grid.rs`: deterministic Rust generator
  (reuses the `image` crate) that writes the PNG + JSON and self-checks by
  running `fit_camera_lens` on its own output — prints found/RMS and the raw
  distortion magnitude. `tools/gen_distorted_grid.py` is an optional Pillow
  variant of the same math.
- `calibrates_from_the_distorted_grid_fixture` test: loads the committed PNG
  from disk, calibrates, and asserts all 49 dots recover at <60 µm RMS while
  >5 px of barrel distortion is present — so a regression that breaks the lens
  fit fails CI. Measured: 49/49 dots, ~25 µm RMS, 10.4 px raw distortion.

## 2026-07-15 — Laser-anchor overlay: make the machine grid visible

The laser-anchor step (② burned grid → camera-px→commanded-mm homography) gave
only a text RMS, so the operator couldn't see whether/where the anchor locked.
Added rich per-dot visual feedback, mirroring the camera-lens overlay.

- `calib::Calibration` now carries `dots: Vec<AnchorDot>` — per detected dot,
  its detected px, the commanded mm it maps to, and the fitted residual (µm).
  Populated in `refit` (so a fresh Fit, a Re-anchor, and Live anchor all get
  it); an empty vec for a restored-seed calibration until it's re-anchored.
- Console overlay (LaserAnchor mode, `calib_frame_overlay`): projects the full
  commanded lattice through the fit's inverse and draws it as a blue mesh (the
  reconstructed machine coordinate grid over the burned dots), a green origin +
  +X/+Y axes, a residual-quality ring per detected dot, an exaggerated orange
  commanded→detected vector (new `anchor_resid_scale` slider), and a red ✕ for
  unlocked dots. Status line gained worst-dot µm; an inline legend explains the
  colors. Radial vectors visibly flag lens/galvo curvature a homography can't fit.
- `dump_anchor_overlay` example renders the overlay to a PNG for eyeballing/docs
  (reuses the distorted fixture). Tests: `recovers_commanded_coordinates_…` now
  asserts `dots` populate with on-lattice mm + bounded residuals;
  `anchor_overlay_renders_the_machine_grid` drives the fixture through load →
  corners → fit → headless layout without panicking.

## 2026-07-15 — Zoom/pan navigation for every image panel

Operator asked to be able to zoom and navigate on any UI with an image (e.g.
while placing markers): Ctrl+drag to pan, Ctrl+wheel to zoom.

- New `imgview` module: a reusable `show(ui, tex, &mut ImageView)` that draws an
  image with Ctrl+drag pan, Ctrl+wheel zoom about the cursor (egui `zoom_delta`
  with a raw ctrl+scroll fallback), and Ctrl+double-click reset. Returns an
  `ImageXform { panel, img_min, scale }` with `to_screen`/`to_native`, so panels
  map native-px ↔ screen through the live view instead of the ad-hoc rect maths
  each had inlined. Zoom is clamped to [1, 24]× (1 = fit); at fit the pan snaps
  back to centred. `is_navigating(ui)` (Ctrl held) lets panels suppress their
  marker click/drag so navigation never places or moves a marker.
- Zoom math extracted to pure `xform_of` / `zoom_about` and unit-tested:
  native↔screen round-trip, cursor-anchored zoom keeps the point under the
  cursor fixed, and clamping to the limits.
- Applied to all five image panels via a `ConsoleApp::show_image` helper (clones
  the texture handle so it can take `&mut self`): calibration frame, fiducial
  frame (drag/click/right-click now gated on !Ctrl), placement composite (plain
  drag still repositions the job; Ctrl+drag pans), camera, and job preview. A
  shared `NAV_HINT` line documents the controls under each. Overlays draw
  through the transform and are clipped to the panel, so they track zoom/pan.

## 2026-07-15 — Camera bed overlay: work area + 50 mm scale

Operator wants, whenever the camera view is up and the camera is calibrated, a
homography-aligned 50 mm scale and the laser's work area projected onto the bed.

- `ConsoleApp::draw_bed_overlay`: when the Camera tab has a frame and a laser
  anchor exists, project through the anchor (machine-mm → camera-px, then the
  pan/zoom transform) and draw: the laser work-area square, a 50 mm L-scale (one
  arm on +X, one on +Y, capped + labelled — perspective-correct because it goes
  through the same homography), and the machine origin + axes. All clipped to
  the panel, so it tracks zoom/pan. A caveat line shows when the calibration is
  a restored (unconfirmed) seed.
- Controls (Camera tab, only when calibrated): a "⧉ Work area + 50 mm scale"
  toggle plus field size + centre (mm) drag values. Field defaults to 140 mm
  (the galvo field, matching cam::tiles::FIELD_MM) centred on the machine origin
  — suits a centre-origin galvo; the operator nudges size/centre to match their
  device while watching the overlay. All four persist across restarts.
- `dump_bed_overlay` example renders the overlay to a PNG (frames a 60 mm field
  to the fixture grid); the 50 mm arms land exactly on the 6th grid dot,
  confirming the scale. Test `camera_bed_overlay_renders_when_calibrated` drives
  the Camera tab with a calibration + frame through a headless layout.

## 2026-07-15 — Camera capture: prefer highest resolution (2K/4K)

The device-capture path requested `AbsoluteHighestFrameRate`, which on a 2K/4K
sensor negotiates a low-res high-fps mode and discards the resolution. This is a
metric bed-vision tool — calibration, fiducials, and placement on a mostly-
static bed — so pixels-per-mm (accuracy) matters far more than frame rate.
Switched to `AbsoluteHighestResolution` so the full sensor is used.

Nothing else caps resolution: `frame_to_gray` builds the gray image from the
delivered frame's own width/height, and the File source accepts any-resolution
image, so higher-resolution frames flow through detection, calibration, and the
egui texture unchanged. Note the anchor homography is in camera-pixel space, so
calibrate at the same resolution you stream at (now both full-res).

## 2026-07-15 — Downscale the camera VIEW only; keep full-res data

Follow-up to the 2K camera work: the operator wants live framing to be light,
but everything touching calibration/detection to stay full resolution.

- `set_camera_frame` now builds the display image at full res (the AR overlay
  still composites at full res, so it stays accurate) and then downscales the
  *view texture only* to `CAM_VIEW_MAX` (1280 px longest side) via
  `downscale_view`, recording the applied ratio in `cam_view_scale`. `cam_last`
  — the frame used by snapshot→Fiducial/Place, and the source for detection —
  is stored at full resolution, untouched. The camera calibrate path grabs its
  own full-res frames independently. Note shows both sizes (e.g.
  "2560×1440 (view 1280×720)").
- `draw_bed_overlay` multiplies the anchor's full-res camera px by
  `cam_view_scale` before the pan/zoom transform, so the work area + 50 mm scale
  still land correctly on the downscaled view.
- Tests: `downscale_view_caps_longest_side_and_reports_ratio` (pure) and
  `camera_view_downscales_but_data_stays_full_res` (2560×1440 → view 1280, data
  stays 2560×1440, ratio 0.5).

## 2026-07-15 — Headless UI debug driver (egui_kittest) + egui 0.30 bump

Added the ability to drive and screenshot the real console UI headlessly, so UI
work can be verified against the actual app instead of ctx.run shape-assertions
and dump_* re-draws (see AGENT_DEBUGGING.md).

- Bumped egui/eframe 0.29→0.30 (clean, zero code changes) so `egui_kittest`
  0.30 could be added. (Latest 0.35 was declined for now: it removes the
  ctx-based panel API and would force a native-eframe rewrite that can't be
  compiled in the headless sandbox.) ColorImage etc. unchanged at 0.30.
- `crates/ui/examples/debug_driver.rs`: reads a script (stdin/file), steps the
  real `ConsoleApp` via a kittest `Harness`, and runs `tree` (a11y dump),
  `state`, `click`, `type`, `set`, `key`, `step`, `settle`, `screenshot`
  (wgpu → PNG, graceful ERR without a GPU). Label match is exact-then-substring,
  first-of-many (the query_by_* singular forms panic on >1 match).
- `ConsoleApp::debug_summary()` — curated state for the `state` command.
- `scripts/headless-gpu.sh`: finds a software Vulkan ICD (SwiftShader bundled
  with Chromium → lavapipe → software GL) for screenshots/snapshots.
- Tests: `tests/ui_interaction.rs` (headless, no GPU — tab switching, button
  drivability, a11y labels) run under `cargo test`; `tests/ui_snapshots.rs`
  (wgpu pixel baselines) are `#[ignore]`d. Baselines in tests/snapshots/*.png
  (SwiftShader); transient *.new/*.diff.png gitignored.
- dev-deps: egui_kittest (wgpu+snapshot), accesskit_consumer (tree types), egui
  (integration tests can't name normal deps).

## 2026-07-15 — Test the calibration workflow; label the frame-path fields

Drove the calibration workflow through the real console with the headless
debug_driver (the recorded technique). Loading a grid frame was previously not
drivable because the path fields were unlabelled, so:

- Added `labelled_by` to the four frame-path text fields (calibrate grid frame,
  fiducial frame, place bed frame, camera frame file). They now carry an
  accessibility label, so the driver (and any operator using a screen reader)
  can target them — and `type "grid frame" <path>` works.
- Surfaced `calib_frame: WxH` in `ConsoleApp::debug_summary()` so the `state`
  command reports whether a grid frame is loaded.
- New `calibration_frame_loads_from_a_typed_path` interaction test drives
  Calibrate → type the committed distorted-grid fixture path → Load, and asserts
  the 660×660 frame loaded through the actual UI.

Test result: the UI-drivable calibration path (mode switch, frame load, Fit
guard "click all 4 corner dots (have 0)") works; corner-marking is a canvas
click (not accesskit-drivable), and the fit + overlay are covered by the fixture
integration tests (calibrates_from_the_distorted_grid_fixture,
recovers_commanded_coordinates_from_a_burned_grid, anchor_overlay_renders...,
camera_bed_overlay_renders...) plus CLI calib-grid and vision fit tests — all
green. No bugs found.

## 2026-07-15 — Point at a KiCad project → auto-produce the Gerbers it needs

The pipeline consumed copper.gbr + outline.gbr but the operator had to produce
them by hand in KiCad. Now the program produces them from a project via the
existing ING-6 kicad-cli invoker.

- `ingest::kicad_cli::export_job_gerbers(board, out_dir, copper_layer,
  outline_layer)`: exports the conductor + outline on separate calls (so the
  layer→file mapping is unambiguous) and moves each plotted file to a stable
  name — `copper.gbr` / `outline.gbr`. `resolve_board` accepts a `.kicad_pcb`
  *or* a project directory containing exactly one board.
- CLI `pcbforge gerbers --project <.kicad_pcb|dir> --out <dir>
  [--copper-layer F.Cu] [--outline-layer Edge.Cuts]`: prints `board/copper/
  outline` paths for a script or the console to pick up. Verified end-to-end:
  gerbers → emit consumes them → valid job.lbrn2.
- Console Job tab: a labelled "KiCad project" field + "⚙ Gerbers from KiCad"
  button that runs the export next to the board (`pcbforge-gerbers/`) and fills
  the copper/outline fields (back side → B.Cu). Front-side default F.Cu +
  Edge.Cuts; runs inline (a brief pause). Persisted across restarts.
- Tests: ingest `export_job_gerbers_writes_stable_names` + `resolve_board_*`;
  CLI `gerbers_e2e` (2, self-skip without kicad-cli); UI interaction
  `gerbers_from_kicad_fills_the_copper_and_outline_fields` (drives the real
  button — kicad-cli is present here so it exports). Generated `pcbforge-gerbers/`
  is gitignored. Also labelled the copper/outline/frame fields for drivability.

## 2026-07-15 — KiCad Gerber export runs in the background (non-blocking)

The "⚙ Gerbers from KiCad" button previously called kicad-cli inline, freezing
the console for the ~1-2 s export. Moved it onto the existing background verb
runner:

- `gerbers_from_kicad` now resolves the board (cheap, for the output dir),
  pre-fills the copper/outline fields with the deterministic output paths
  (`<board dir>/pcbforge-gerbers/{copper,outline}.gbr`), and shells
  `pcbforge gerbers …` via `run_verb` (thread + channel, non-blocking). The
  files appear when the job finishes; its progress/errors stream to the Log.
  Back side passes `--copper-layer B.Cu`.
- The UI interaction test is now deterministic (no kicad-cli needed): the fields
  are pre-set synchronously, so it asserts `copper=copper.gbr` immediately. The
  actual export is covered by the CLI `gerbers_e2e` tests.
- `debug_driver` gained a `PCBFORGE_CLI` env override so it can drive *real*
  verbs (e.g. `PCBFORGE_CLI=./target/debug/pcbforge`) instead of the `true`
  no-op. Confirmed the button pre-fills the fields instantly (non-blocking) and
  that the shelled `pcbforge gerbers` command produces the files at those paths.

## 2026-07-15 — calib-grid: absolute output path + centre it on the field

Operator feedback burning the anchor grid: (1) couldn't find the output — the
Log echoed the bare relative name, and it landed in the process CWD (not the app
dir for a desktop launch); (2) the grid emitted at 0..60 mm (corner origin) but
a centre-origin galvo (BSLFiber/JCZ) has 0,0 at the field centre, so it sat off
in one quadrant / outside the work area.

- CLI `calib_grid_cmd` now creates the output's parent dir and prints the
  **absolute** path (`std::path::absolute`), so the Log/console shows exactly
  where the file went.
- `--origin` gained `allow_hyphen_values` — clap was rejecting negative
  coordinates (`--origin -30,-30`) as flags.
- Console Generate grid now **centres the lattice on the machine field**
  (`origin = (field_cx, field_cy) − span/2`, from the Camera-tab work-area
  settings), so it lands inside the addressable area; the note reports the
  centre and the resulting span. Default field (0,0) → a 60 mm grid spans
  −30..+30.
- Tests: `generate_grid_centers_on_the_field` (UI note asserts centred origin);
  calib-grid e2e still green; verified the CLI prints the absolute path, creates
  dirs, and accepts a negative origin.

## 2026-07-15 — Report calibration age (missing vs old vs fresh)

Operator asked to know whether the camera calibration is *non-existent* or
*old*, not just "not calibrated this session". The persisted anchor had no
timestamp, so a restored fit only read "loaded last session".

- New `calib_saved_at: Option<u64>` (Unix seconds), stamped on every fresh
  anchor (fit / re-anchor / live), persisted alongside `calib_matrix`, and
  restored on load. `now_unix()` + `human_age()` helpers ("just now / N min /
  N h / N days ago").
- Status line (② Laser anchor) now reads: `○ no camera calibration — never
  anchored`; `◐ saved calibration, 3 days ago — ⟳ Re-anchor to confirm`; or
  `● anchored this session (…)`. `debug_summary` mirrors it (`saved (N days
  ago), unconfirmed`). Pre-timestamp saves show "age unknown".
- Test `calibration_reports_age_not_just_this_session`.

## 2026-07-15 — Grid centres on the real work area (operator-set), not (0,0)

The centre-origin assumption was wrong for the operator's machine — LightBurn
showed the work-area square offset (centred ~(0,30)), so centring the grid on
(0,0) put half of it below the field.

- Surfaced the work-area controls (centre cx/cy + size, the same
  `field_cx/cy/mm` the Camera-tab overlay uses) directly in the ② Laser-anchor
  form, with a hover telling the operator to read them off LightBurn's rulers.
- Generate centres the grid on that work area (`origin = (cx,cy) − span/2`) and
  the note now reports the work area + span, warning if the grid is larger than
  the field (⚠ lower pitch / dots per side).
- Test extended: an off-centre work area (0,30) recentres the grid to
  (−30,0)…(30,60).

## 2026-07-16 — Dot-contrast toggle so an ablated burn can anchor

The operator's ComMarker burn on the dark plate imaged as **light-on-dark**
(ablated marks), but the anchor detector hardcoded `DarkDot` (dark-on-light),
so the fit found 0/49 dots. The vision layer already had a `Backlit`
(bright-on-dark) profile — the calibration just never used it.

- New `calib::DotKind {Dark, Bright}` maps to `FiducialProfile::DarkDot` /
  `Backlit`, threaded through `detect_grid_dots` → `refit` →
  `fit_camera_lens` / `fit_camera_to_machine` / `re_anchor` (all four fit
  call sites in `app.rs`, plus the four example/test call sites).
- Calibrate tab gains a "dot contrast" selectable (◉ dark-on-light /
  ◎ bright-on-dark); `calib_dot_kind` persists via settings (`calib_dot_kind`)
  and shows in `debug_summary` (`contrast=…`).
- Tests: `bright_on_dark_needs_the_bright_polarity` (dark detector fails, bright
  recovers ~(30,30)); headless `the_dot_contrast_toggle_switches_detection_polarity`.
- Default stays Dark (printed grids + dark-anodized burns). Operator guidance:
  bright-on-dark for ablated marks / backlit holes; also reduce glare, which
  hurts either polarity.

## 2026-07-16 — Loosen the fiducial gates for real ablated burns

Even with the right polarity, the detector's gates were tuned for clean
synthetic/printed dots and rejected real ablated grid dots (dim under bench
glare, ragged, size-variable). Relaxed the hard gates in `vision::fiducial`,
keeping the anti-false-positive discrimination:

- `MIN_SNR` 5.0 → 3.5 (glare tolerance; robust MAD σ keeps it above noise).
- `AREA_MIN_FRAC` 0.2 → 0.12; `AREA_MAX_FRAC` 4.0 → 4.6 (still rejects the
  honeycomb-bed decoy hole ~4.8× area from the field photo).
- `MIN_CIRCULARITY` 0.35 → 0.25 (spatter/comet-tail burns).
- Aspect window 0.35..=2.86 → `ASPECT_MIN..=ASPECT_MAX` (0.3..=3.3).
- Score is feedback only — never a gate — so none of this hallucinates; the
  `decoy_holes` and `low_contrast_reports_snr` tests still hold.
- New test `dim_low_contrast_burn_is_now_found` (SNR below the old 5.0 gate is
  now located).

Also fixed a test-isolation weakness the new persistent `calib_dot_kind`
exposed: `the_dot_contrast_toggle_switches_detection_polarity` now drives to a
known polarity first instead of trusting the (persisted) default.

## 2026-07-16 — Laser field-distortion correction (emit pre-distortion)

The register/emit path baked only a rigid affine, so a design square burned as a
perfect square in *commanded* coordinates but physically bowed by the galvo/
f-theta field distortion. Added an end-to-end correction that pre-distorts the
emitted geometry so the beam cancels the field error.

Measurement is camera-metric (operator's choice): the ① camera-lens map is the
metric ruler; a burned grid at known commanded coords is imaged, each dot's true
physical position is read through the lens map, and a **physical→commanded**
bi-cubic (`vision::FieldMap`/`fit_field`, reusing the lens-poly machinery) is
fit. Emitting `to_commanded(geometry)` cancels the distortion.

- vision: FieldMap + fit_field + Poly2 coeff (de)serialization + a text file
  format for the emit subprocess. precompensation_cancels_the_field_distortion.
- cam: transform_shapes_field — affine, then densify each edge (a straight
  design edge is a curved commanded path under pincushion) and warp every point
  via a closure (cam stays dependency-free).
- cli: register --field-map [--field-seg-mm]; e2e test.
- ui: calib::fit_laser_field (burned grid + lens map → FieldMap + a linear
  physical→px map for the overlay); ③ Laser-field calibration step; a Place-tab
  "compensate field" toggle that places in the physical frame and passes
  --field-map to register. The correction file persists beside settings.

Model is a global bi-cubic (operator's choice) — smooth, ideal for f-theta
pincushion, one RMS number. The field cal is session-scoped like the anchor;
"compensate field" only arms with a live field cal so the placement frame and
the baked correction always agree.

## 2026-07-17 — Pincushion-vs-noise diagnostic for the laser field cal

The ③ Laser field calibration reported an RMS/worst-error number but no way to
tell if it was genuine radial distortion (worth correcting) or measurement
scatter (correcting would overfit). Added `vision::classify_field_error`.

- Center: the centroid of the burned grid's **commanded** coordinates, not a
  fitted center — the commanded grid is exact by construction and already
  centered on the field by the operator, so a centroid is a robust,
  always-defined proxy; a fitted (cross-product regression) center was
  considered and rejected as unneeded rigor — it adds a second linear solve,
  a condition-number reliability gate, and a fallback path for a case the
  centroid never hits.
- Radial model: closed-form `rad(r) = k1·r + k3·r³` (2×2 normal equations, no
  SVD) — the cubic term is pincushion/barrel curvature the bi-cubic
  pre-distortion fixes; the linear term isolates a uniform scale/pitch error
  that a LightBurn/EZCAD recal fixes instead, so the verdict can route the
  operator to the right fix.
  Significance: `systematic_um` (RMS of the fitted radial model) vs.
  `noise_um` (RMS of everything the model leaves unexplained — tangential +
  radial residual), gated by an absolute floor (15 µm) and a ratio threshold
  (2.0×, with a 1.3×/7.5µm `Borderline` band below it) rather than a formal
  F-test — the Cauchy–Schwarz bound (fitted signal ≤ raw signal energy)
  already guarantees pure noise concentrates the ratio near its null value
  without needing a critical-value table/dependency.
- Wired into `FieldCal::field_verdict`, the ③ status block, `calib_note`, and
  `debug_summary()`'s `laser_field:` line (`verdict=pincushion(ratio=…)` /
  `barrel(…)` / `uniform_scale(…)` / `borderline(…)` / `noise(…)` /
  `inconclusive(reason)`).
- Test lock-in deviates from the letter of the UI-verification convention:
  marking the 4 calibration corners is a canvas interaction, which
  `AGENT_DEBUGGING.md`/CLAUDE.md already documents as undrivable via
  accesskit. Rather than fake it with raw pointer-event injection at
  hand-computed screen coordinates (fragile — depends on the image-view
  zoom/pan state), the discrimination proof lives as `vision::lens` unit
  tests (pincushion / barrel / uniform-scale / random-noise) plus two
  `calib::fit_laser_field` tests (`laser_field_fit_recovers_precompensation`
  asserts the verdict on a genuine pincushion fixture,
  `laser_field_fit_flat_grid_reads_noise_not_pincushion` asserts an
  undistorted grid is never flagged `Systematic`) — the same
  render-grid-and-fit path the module's existing tests use, exercised through
  the real `FieldCal`, just not through simulated mouse clicks.

### Post-implementation hardening (adversarial verify pass, same day)

Monte-Carlo stress-testing `classify_field_error` against pure isotropic
noise (no true field distortion) at various `n` — not committed as a test
itself, it's too slow/flaky at the trial counts needed to see the tail —
found a real gap at the original `MIN_DOTS = 6`: with only `n − 2` residual
degrees of freedom for the 2-parameter `k1·r + k3·r³` fit, a fixed
`ratio ≥ RATIO_THRESHOLD` gate has a non-negligible false-positive tail at
the smallest allowed `n`. 20,000-trial sweeps at realistic 10–30 µm
per-dot noise:

| n (shape)      | false `Systematic` | false `Borderline` |
|----------------|---------------------|---------------------|
| 6 (old MIN_DOTS, 2×3) | 2–6 / 20,000 (~0.01–0.03%) | ~130 / 20,000 (~0.65%) |
| 9 (3×3)        | 0 / 20,000          | 10 / 20,000 (0.05%) |
| 10 (2×5)       | 0 / 30,000          | 2 / 30,000 |
| 12 (3×4)       | 0 / 30,000          | 1 / 30,000 |
| 16 (4×4)       | 0 / 20,000          | 0 / 20,000 |

So pure noise *could* occasionally read as genuine pincushion/barrel right at
the minimum sample count — a false "correction should help" call, the
opposite of the diagnostic's purpose. `MIN_DOTS` raised **6 → 10**: clears
the observed danger zone with margin, and stays below every real call
site's grid — `calib::fit_laser_field` already floors at a 4×4 (16-dot)
grid, so this only closes a loophole in the public `classify_field_error`
API for hypothetical smaller/non-grid callers, it doesn't change any
production behavior.

Locked in with two cheap, deterministic (xorshift64* + 12-uniform CLT
Gaussian, no `rand` dep — same pattern `fiducial::tests` uses) regression
tests in `vision::lens::tests`, run at normal `cargo test` speed (hundreds
of trials, not tens of thousands):
- `classify_pure_noise_at_min_dots_boundary_never_reads_systematic` — many
  seeds × noise levels at exactly `n = MIN_DOTS`, asserts `Systematic` never
  fires on scatter alone.
- `classify_pincushion_survives_realistic_measurement_noise` — a genuine
  2–4% pincushion PLUS realistic per-dot noise (the earlier pincushion
  tests were noise-free fixtures), on both a 7×7 field grid and the
  smallest grid the UI allows (4×4), asserts the signal still clears
  `RATIO_THRESHOLD` and reads `Systematic { pincushion: true }` — proving
  the raised floor didn't buy safety by eating real sensitivity.

Also fixed two operator-facing accuracy issues found in the same pass
(`crates/ui/src/app.rs`):
- The ③ status block colored `FieldPattern::Noise` the same amber/warning
  as `Borderline`/`Inconclusive`. That's backwards — `Noise` is a
  *conclusive* good-news read (no distortion detected, don't correct),
  same confidence tier as `Systematic`, not the same tier as genuine
  "can't tell yet" uncertainty. Now green like `Systematic`/`UniformScale`;
  amber is reserved for `Borderline`/`Inconclusive`.
- `field_verdict_phrase`'s `Noise` message read as "correction likely won't
  help (check LightBurn/EZCAD instead)", which undersold the actual
  finding and implied something needs fixing. Reworded to lead with "this
  field is likely already good; don't enable correction here" and demote
  the LightBurn/EZCAD line to the fallback if dots are still visibly off.

## 2026-07-17 — Field diagnostic: fable-review fixes (non-radial, fail-closed)

A fable-model review of the pincushion-vs-noise diagnostic found two real
problems; both fixed directly:

1. A pure galvo **rotation/skew** is entirely tangential (zero radial signal),
   so it read as `Noise` ("field is fine, don't correct") even though the
   bi-cubic fixes it. `classify_field_error` now also fits a tangential linear
   term (`t1·r`, exactly a rigid rotation) and the noise floor is the residual
   after removing BOTH radial and tangential models. New `FieldPattern::NonRadial`
   ("systematic but not curvature — correction still helps; check alignment").
   Tangential is fit linear-only (one param) to keep noise sensitivity low; the
   n=10 Monte-Carlo boundary test was broadened to reject false NonRadial too
   and still passes.
2. The console **auto-enabled** correction on every fit, against the verdict.
   `place_field_correct` now auto-arms only when `field_correction_advised`
   (Systematic/NonRadial); the ③ status hint is verdict-conditional.

Also from the review: fail closed on non-finite input (`InconclusiveReason::
NonFinite`) instead of falling through to `Noise`; replaced the axis-aligned
bbox collinearity gate with a second-moment (PCA eigenvalue) test so a diagonal
line of dots is caught; documented the centered-grid (centroid-as-center)
assumption on the API; `UniformScale` wording now also names a mis-scaled
reference/print as a suspect. New tests: rotation→NonRadial, non-finite→
Inconclusive, diagonal→SpanTooThin.

## 2026-07-18 — Repo-wide logic-review remediation (LR-01…LR-50)

Worked the 50 findings from the 2026-07-17 logic review. 46 fixed with
regression tests (443 workspace tests green, clippy clean); 4 deferred with
rationale below. Fixes of note:

- **Safety/etch path:** airflow interlock wired into `LaserExecutor` via an
  `AirflowGate` (bring-up default records an `airflow_skipped` audit row;
  `Require` verifies + Halts, unknown machine fails closed) (LR-01); interlock
  self-test (deassert RTS, require CTS low → `StuckClosed`) + debounce (LR-19);
  back-side "Etch here" refused until `register` can mirror (LR-03).
- **Calibration correctness:** grid burn-origin persisted and returned by
  `calib_grid()` — the fit no longer labels the lower-left dot (0,0) when it was
  burned at `field_center − span/2` (LR-02); a failed fit keeps the previous
  calibration (LR-16); camera scale measured from design spacing, not dragged
  markers (LR-17).
- **Orchestration durability:** each `step` runs in one `BEGIN IMMEDIATE`
  transaction (atomic runlog+advance, no board stranded at `'start'`, serialized
  steppers) + `busy_timeout` (LR-09/10/11); strict `PCBFORGE_DOUBLE_SIDED`
  parse (LR-04); runlog ordered by rowid (LR-28); schema-version checked on open
  (LR-29).
- **CAM coverage:** dual-machine split erodes only the copper side and gives the
  near-copper strip to UV, so fiber ∪ UV == the removal band (LR-05); tiling
  buckets into a field window that contains the element, flagging the rest as
  `unfittable` (LR-06); heat-order tail spread via a bisection permutation
  (LR-07); winding-validation budget capped at 2% of ring area (LR-37).
- **Ingest:** every record in a `%…%` block dispatched (LR-13); apertures before
  `%MO` error (LR-12); decimal-only Excellon coordinates (LR-32).

### Deferred (not fixed) — rationale

- **LR-21** (BedMap y-flip half-pixel): every y-flip producer/consumer currently
  agrees on the `H` convention; changing only `BedMap` to `(H−1)` would
  *introduce* a real 1 px disagreement to remove a latent one. Needs a
  consistent sweep across all ~8 sites + fixtures as its own reviewed change.
- **LR-15** (back-side AR double-mirror): a cross-cutting mirror-convention
  decision (`x_mm: 0.0` vs `cx`, mirrored-vs-unmirrored compositing) on the AR
  overlay — a canvas render not drivable headlessly (see AGENT_DEBUGGING.md).
  Display-only now that back-side etch is refused (LR-03); a blind change risks
  a plausible-but-wrong overlay.
- **LR-44** (click-to-place mm under an active homography): the fiducial-check
  placement is the pre-registration bootstrap that *produces* the homography, so
  inverting it here is circular; the review itself tags it `[suspected]` with two
  conflicting fixes. Unverifiable canvas click path.
- **LR-34** (exact `s==e` full-circle detection): spec-compliant for a
  non-KiCad emitter; the suggested fix is a warning, but the parser has no
  diagnostic/warn channel.

## 2026-07-18 — Fail-closed job generation and crash-safe stage attempts

The repo-wide implementation review found that several boundaries silently
accepted invalid machine inputs, and that LR-09's single transaction enclosed
the executor itself. A database rollback cannot undo a physical burn: a crash
after hardware completion could roll back `stage_start` and cause an automatic
replay, while the long transaction also held SQLite's writer lock throughout
the operation. This entry supersedes that part of LR-09.

- Laser recipes and cut machine facts now validate centrally. Invalid/non-finite
  inputs, zero passes, and cut schedules above 100,000 passes are errors rather
  than clamps or accidental job-file floods. LightBurn text is XML-escaped and
  generated files/settings use same-directory temporary files plus rename.
- Stage execution is a durable three-phase protocol: commit a monotonic attempt
  as `running`; execute without a DB transaction; then verify and finalize that
  exact attempt. `running`/`needs_attention` blocks automatic replay after a
  crash or ambiguous halt. Operators reconcile explicitly with retry or
  mark-done, both recorded in the runlog.
- The default executor registry now halts. Auto-advancing manual/clearance
  stages and skipped airflow exist only through explicit bring-up APIs and the
  CLI's `--bringup-stubs` flag. A new physical board is admitted explicitly
  with `--new-board` after the prior board reaches a clean terminal.
- Stage graphs reject cycles and unreachable nodes. Persisted homographies must
  be finite and invertible. Rust 1.92 is pinned because the installed 1.96
  compiler ICEs while building the current dependency graph.

## 2026-07-19 — Auto-center the operator's corner-origin work area

Camera evidence showed the old generic default (`140 mm` centred at `0,0`)
projecting most of the work-area outline outside the visible bed. This machine
uses a 70 mm, lower-left-origin field: its `0..70` coordinates are centred at
`(35,35)`.

- The work area now defaults to 70 mm with **auto center** enabled. Changing the
  size keeps `cx = cy = size/2`; disabling auto center exposes the persisted
  manual coordinates for offset or centre-origin LightBurn configurations.
- The exact former default (`140/0/0`) migrates to `70/35/35`. Other saved
  tuples are treated as intentional operator settings and remain unchanged in
  manual mode.
- This does not alter the homography. A visibly shifted outline still means the
  saved camera calibration needs **Re-anchor**; auto center only fixes the work
  area's machine-coordinate convention.

## 2026-07-19 — Compose camera-lens and laser-field maps for display/placement

Real burned-grid evidence showed the ② anchor's red detections displaced from
its blue lattice. That view was still a single homography: useful as a robust
fallback, but incapable of representing camera-lens and galvo/f-theta curvature.

- Commanded machine coordinates now project as `commanded → physical` through
  `FieldMap`, then `physical → camera px` through `LensMap`; the inverse composes
  `px → physical → commanded`. Work-area and uncorrected placement overlays
  use this nonlinear pair when an accepted ③ fit exists.
- Field-corrected placement remains in desired physical mm and therefore uses
  only the lens map for display/drag; emit applies `physical → commanded` once.
  Applying the field map in both places would double-compensate the burn.
- ③ is accepted for active projection/emission only with at least 80% of dots,
  all four boundary corners, RMS ≤ 50 µm, worst ≤ 100 µm, matching frame
  dimensions/orientation, and finite polynomial coefficients. Rejected attempts
  remain visible diagnostically but cannot arm correction or overwrite its file.
- An accepted fit whose correction file cannot be saved is not activated, and
  `Etch here` refuses to emit if correction is armed but that file is missing;
  silently producing an uncorrected job would disagree with the placed overlay.
- ② is now explicitly labelled an approximate homography fallback. The field
  overlay shows the nonlinear predicted lattice, detected burns, raw field-error
  vectors, and post-fit RMS/worst readout. The physical square calibration grid
  and the fit algorithms are unchanged.

## 2026-07-19 — Split the console by operator workflow

`crates/ui/src/app.rs` had grown past 5,500 lines and mixed persistent settings,
camera capture, calibration, fiducials, placement, command execution, rendering,
and their tests in one implementation block. That made otherwise local changes
hard to review and encouraged accidental coupling between workflows.

- `ConsoleApp` remains the public application shell, but owns private grouped
  state for runtime, job, camera, calibration, fiducials, placement, AR, and
  image navigation. The public `db_path` and `cli_cmd` fields stay compatible.
- Workflow controllers and views live in private `app/` child modules. Existing
  geometry and fitting modules remain separate; this is an ownership refactor,
  not a change to calibration algorithms or operator behavior.
- Cross-workflow access is limited to private or `pub(super)` interfaces. The
  settings keys, debug-summary tokens, public methods, and widget labels remain
  stable so saved consoles and headless driving scripts continue to work.

## 2026-07-19 — Detect burned square grids as a coherent lattice

A live 7×7 burn showed two failure modes in the generic fiducial detector:
strong illumination falloff/glare changed the apparent foreground across the
frame, and independent search windows could lock to unrelated bright texture.
Changing the calibration polynomial would only fit those wrong observations.

- Burned-square calibration now first applies local mean/variance contrast
  normalization, scores opposing square edges at several apparent sizes, and
  chooses candidates as one smooth lattice. This is polarity independent and
  tolerates both the dark lower field and the saturated central reflection.
- The generic fiducial detector remains the fallback for printed/circular or
  weak fixtures. Grid generation, square dimensions, pitch, and all lens/field
  fit models are unchanged.
- The supplied live frame is a committed UI test fixture with independently
  measured square centers. The regression requires broad coverage and bounded
  center error, rather than merely accepting a successful fit.
- Laser-anchor results can be reviewed manually: left-clicking a burned square
  adds or moves that lattice observation, right-clicking removes a suspect one,
  and the homography/residuals are re-fit immediately. Live re-anchoring pauses
  during correction so it cannot overwrite the operator's review.

## 2026-07-19 — Field frame anchored to the burned grid, not the paper (operator-reported)

- The operator's first real ③ Laser-field fit "failed" with raw worst 52 mm
  and all error arrows sweeping one diagonal: the fit paired commanded coords
  with positions read through the ① lens map, whose metric frame is the
  **printed paper's** — wherever the paper happened to be taped. The paper
  cannot occupy the same spot as the burned grid (it sits on top), so its pose
  offset/rotation appeared as a giant fake "field error" the polynomial had to
  absorb, and the residual noise still tripped the 50/100 µm gate.
- Operator-stated principle, now implemented: **the printed paper is only for
  lens characterization; every coordinate/anchor reference comes from the
  burned laser grid.** `fit_laser_field` maps detected dots px→paper-mm, fits
  a rigid (rotation+translation, `calib::fit_rigid`, 2-D Kabsch) paper→machine
  alignment against the commanded lattice, and defines physical mm as the
  aligned coords. Scale is deliberately excluded from the alignment so the
  paper's printed pitch stays the metric authority and a genuine galvo scale
  error remains measurable (the UniformScale verdict then points at
  printer/galvo scale). An alignment residual ≥ one grid pitch errors out
  ("mirrored view / corner order") instead of fitting nonsense.
- The rigid anchor (`FieldCal::paper_to_machine`) threads through every
  camera↔machine conversion: `camera_px_to_physical`/`physical_to_camera_px`/
  `camera_px_to_commanded`/`commanded_to_camera_px` take it, and
  `CameraProjection::{PhysicalLens, CommandedField}` carry it — so Place and
  the overlays are burned-grid-anchored too (previously they silently used
  the paper frame). Persisted as the `field_frame` settings key; a saved field
  calibration without it (pre-fix, paper-anchored) is NOT restored.

## 2026-07-19 — Calibration persists; export gate relaxed to a warning (operator direction)

- **Persistence:** the ① camera-lens calibration (both bi-cubic Poly2 maps,
  RMS/worst, found/total) and the lens frame signature are now saved in the
  console settings blob and restored at startup; the accepted ③ laser field is
  restored from the existing `pcbforge-field-map.txt` plus persisted
  `field_to_px`/counts/`field_accepted` keys. Per-dot residual vectors are
  display-only and not persisted (a restored cal shows no arrows until re-fit).
  Staleness guards are unchanged: the frame-signature check refuses a restored
  cal when resolution/crop/orientation differ, and a moved camera re-anchors.
  The field verdict is not persisted — a restored FieldCal carries an
  Inconclusive verdict until the next fit.
- **Gate → warning (operator's explicit call, reversing the hard fail-closed
  rule below):** `--field-map` on CLI `emit`/`register` is now optional; when
  absent the geometry is emitted UNWARPED with a loud stderr warning
  ("NOT field-warped"). Console "Etch here" and the Job-tab emit likewise
  field-warp when an accepted ① + ③ calibration and the map file exist, and
  otherwise export unwarped with an error-styled log line and a "⚠ UNWARPED
  export" status label. The stale-signature case also warns-and-continues
  unwarped rather than refusing. Rationale: the operator tests at the machine
  and wants the workflow usable without a per-session recalibration; the
  original hazard (two plausible files) is mitigated by the persistence above,
  the warnings, and the placement-note frame label.

## 2026-07-19 — Place preview decoupled from the export gate (operator-reported)

- The operator reported "Load frame + job isn't loading an image": the
  field-warp requirement (entry below) had made `place_projection` refuse
  *everything* without an in-session accepted ① lens + ③ field calibration —
  including merely displaying the frame — and since those calibrations are
  session-only, Place was dead after every console restart. `load_place` also
  returned before caching the frame, so the operator saw nothing but a small
  gray note.
- Fix, keeping the export gate intact (operator-approved): `place_projection`
  falls back to the saved ② laser-anchor homography for *viewing and rough
  placement* (note labels it "approximate homography"); with no calibration at
  all, `load_place` still displays the bare frame with a note saying which
  calibration is missing. "Etch here" is unchanged — it independently refuses
  to export without accepted ① + ③ and the field-map file, so no unwarped
  geometry can ship. The fail-closed tests moved from `place_projection` to
  the export path accordingly (`anchor_only_place_previews_but_refuses_export`).
- Also surfaced the placement frame/note in `debug_summary` so headless
  driving can see whether a frame actually loaded (verified via debug_driver:
  frame displays, note explains the gap).

## 2026-07-19 — Require field-warped production geometry

Live output showed that the Place overlay could use calibrated coordinates while
the saved place_field_correct=false preference still selected affine-only
geometry for register. An optional correction switch is unsafe because both
files look plausible in LightBurn.

- Place now operates only in desired physical millimeters through the accepted
  camera-lens map. Homography and uniform-scale projections remain diagnostic
  camera aids; they are no longer production placement fallbacks.
- Etch here always passes the accepted field map to register, which densifies
  every edge before mapping physical→commanded. Missing, rejected, stale,
  non-finite, or unsaved calibration fails closed.
- The Job-tab direct .lbrn2 exporter has the same requirement. CLI `emit` and
  `register` both require `--field-map`; both apply the same densify-and-warp
  kernel, so there is no production affine-only command-line bypass.
- Machine-space camera overlays compose commanded→physical field distortion
  with physical→camera lens projection. Work-area edges, scales, and axes are
  sampled before projection so nonlinear curves are not flattened into chords.
- The old persisted place_field_correct key remains readable for settings
  compatibility but no longer selects an unwarped production path.
- Calibration grids remain deliberately unwarped: their commanded-vs-physical
  error is the measurement used to construct the field map.
