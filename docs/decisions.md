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
  name — `copper.gbr` / `outline.gbr` (renamed to `copper-<layer>.gbr` on
  2026-07-28, see that day's entries; outline unchanged). `resolve_board`
  accepts a `.kicad_pcb` *or* a project directory containing exactly one board.
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
  (`<board dir>/pcbforge-gerbers/{copper,outline}.gbr`, renamed to
  `copper-<layer>.gbr` on 2026-07-28, see that day's entries; outline
  unchanged), and shells `pcbforge gerbers …` via `run_verb` (thread + channel,
  non-blocking). The files appear when the job finishes; its progress/errors
  stream to the Log. Back side passes `--copper-layer B.Cu`.
- The UI interaction test is now deterministic (no kicad-cli needed): the fields
  are pre-set synchronously, so it asserts `copper=copper.gbr` immediately
  (that assertion has since followed the 2026-07-28 rename). The actual export
  is covered by the CLI `gerbers_e2e` tests.
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

## 2026-07-20 — Operator opt-in to absorb machine scale in software

A follow-on to the similarity-scale setup guard below. One operator's machine
burns ~33% oversize (LightBurn field size set to 70 mm against a ~93 mm actual
lens field) and they cannot always reach the machine config to fix it, so they
asked PCBForge to compensate in software instead of refusing the ③ fit.

- Added `allow_machine_scale: bool` (last parameter) to `fit_laser_field`, wired
  to a `pub(super) allow_machine_scale` flag on `CalibrationState` (default
  false) and a labelled ③-only "compensate machine scale" checkbox near the Fit
  control. Off: behaviour is exactly as before (the `> FIELD_SCALE_FAIL_FRAC`
  hard gate stands). On: the gate is skipped and any measured scale proceeds —
  the field polynomial's linear terms already absorb uniform scale downstream,
  so shapes burn dimensionally true. `FieldCal::scale` still records it, and the
  accepted-fit note prefixes the verdict with a loud "machine scale {:+.1}%
  ABSORBED in software …" line.
- Energy-density caveat (recorded so it isn't lost): absorbing the scale only
  corrects geometry. The machine's speeds and hatch spacing stay in its own
  oversized units, so physical speed and line spacing scale by the same factor
  and energy density shifts — the operator must re-tune power/speed after
  enabling. The checkbox hover text says this, and states that fixing the field
  size in LightBurn is the cleaner solution.
- Moved the mirror guard from the RIGID-fit residual to the SIMILARITY-fit
  residual. With a genuine 33% scale now legitimately passing through, the old
  rigid residual (~scale_err × grid RMS radius) tripped spuriously — the rigid
  fit carries no scale, so it cannot align a correctly-oriented but scaled grid.
  A mirrored view or scrambled corner order is a reflection, which scale +
  rotation + translation cannot undo, so the similarity residual is the correct
  mirror detector and works in both modes. The paper→machine anchor stays the
  rigid fit (it threads through every camera↔machine conversion; the polynomial
  handles scale). The scale gate still runs before the mirror guard, so a pure
  scale error in the non-allow path is still diagnosed as a setup error, never a
  mirror.
- Persisted as `calib_allow_machine_scale` (true/false), and surfaced in
  `debug_summary`'s `laser_field:` line as `scale_comp={on,off}`.

## 2026-07-20 — Per-step grid parameters + similarity-scale setup guard

A live ③ Laser-field fit was rejected with an apparent ~35% "uniform scale"
error (9.8 mm RMS systematic against a 968 µm scatter floor), and the console
gave the operator no way to tell which of several plausible causes was at
fault: a shared pitch/dot-size field reused between calibration steps, the
camera having moved or zoomed since ①, the printed paper sitting proud of the
burn plane, or a genuine machine field-size misconfiguration.

- Root cause: `CalibrationState` held one n/pitch/dot⌀/contrast set for both
  ① (measured, printed pitch) and ②③ (commanded, burned pitch), so fitting ①
  with the burned-grid pitch silently mis-scaled the metric ruler. Split into
  distinct `paper` and `burn` parameter sets; the form now shows whichever is
  active for the step being run. The legacy `calib_*` settings keys keep
  meaning the burn set; new `calib_paper_*` keys hold the paper set; on first
  load without paper keys, paper is seeded from burn (that was what ① was
  last fit against).
- Added `fit_similarity` (2-D Procrustes with uniform scale) strictly as a
  diagnostic — the paper→machine anchor itself stays rigid, per the 2026-07-19
  entry above, so the printed pitch remains the metric authority and galvo
  scale errors stay measurable. A |scale − 1| beyond `FIELD_SCALE_FAIL_FRAC`
  (5%) now fails ③ early with a ranked-causes message pointing at the likely
  culprit; genuine galvo scale error is only ever ~1–2%, so 5% cleanly
  separates "setup mistake" from "real field distortion." Scale error inside
  1–5% still proceeds, but `FieldCal::scale` now carries the measurement
  through the verdict phrase, `debug_summary`, and the persisted
  `field_stats` (third token; missing on old data implies 1.0).
- The new scale gate runs before the existing rigid-fit alignment-RMS guard,
  not after: over this grid's ~28 mm RMS radius, a 35% scale error alone
  produces ~9.9 mm alignment RMS, which sits right on top of the one-pitch
  (10 mm) mirrored-view threshold. Left in the old order, the same fault could
  either trip a misleading "mirrored view" error or narrowly duck under the
  guard and masquerade as a field-distortion failure.
- The ③ fit note also gains an off-centre warning when the fitted grid centre
  sits more than 25% of the work-area size from the configured field centre:
  `classify_field_error` assumes a centred grid, and an off-axis grid's
  curvature otherwise reads as uniform scale.

## 2026-07-21 — Mirror-blind calibration fixed with an asymmetric grid + reflective frame (bench-caught)

An exported job burned on the mirrored side of the field while the whole
calibration chain reported a clean, self-consistent fit. The operator caught
it by comparing the emitted coordinates, LightBurn's preview, and the physical
burn: the machine negates X (galvo axis config), and calibration never saw it.

- Root cause is structural: an n×n dot lattice is mirror-symmetric, and the
  corner-click instruction labeled dots by *visual* position, so a machine
  X-flip merely relabels dots — the fitted map is internally consistent and
  the mirrored-view guard has nothing to catch. The flip only reappears when
  true commanded coordinates reach the real machine at export.
- Fix, part 1: `calib-grid` now burns two off-lattice orientation markers
  (diagonally outside the LL corner; below the bottom-edge midpoint), and the
  laser-mode corner clicks are keyed to them (LL = corner nearest the lone
  diagonal marker). Dots therefore get their TRUE commanded labels, making a
  machine mirror visible as a genuinely mirrored correspondence. Markers sit
  ≥0.5·pitch off-lattice, outside the detector's search windows. The paper
  grid stays unmarked — its frame is arbitrary by design.
- Fix, part 2: `fit_rigid`/`fit_similarity` are now full Procrustes with
  reflection (try det=+1 and det=−1, keep the better); `Rigid2` carries
  `flip_x` (applied before rotation) through every camera↔machine conversion
  and the persisted `field_frame` (fifth token; absent = no flip). A mirrored
  machine calibrates cleanly with a loud note naming the mirror; clearing the
  axis negate in LightBurn and recalibrating removes it.
- Corner-order scrambles are still rejected, but the mechanism split: square-
  symmetry permutations are geometrically identical to machine flips/rotations
  and are now absorbed by design; non-isometric scrambles bowtie the corner
  seed and fail at dot detection, and a sheared correspondence still trips the
  alignment-residual guard (which now mentions the orientation markers).
- Collateral hardening: a bowtied seed produced a near-singular homography
  whose blown-up local scale panicked the fiducial search-window arithmetic
  (i64 overflow); degenerate centers/scales now return a miss.
- Same session, operator direction: the ③ acceptance limits (residual
  RMS/worst) became editable + persisted (`calib_accept_rms_um`/`_worst_um`,
  default 100/250 µm). The rig's demonstrated measurement floor is ~69/182 µm,
  so the old hardcoded 50/100 rejected every fit the hardware could produce;
  limits now sit at what the operator's process actually needs, and rejection
  text quotes the configured values.

## 2026-07-21 — Fiducial holes: generated by the software, checked shape-aware

The operator was hand-authoring fiducial holes in LightBurn, so the burned
positions/sizes and what the Fiducials check expected could silently disagree.
Now the console generates them: a `fid-holes` CLI verb (shelled from the
Fiducials tab) emits a filled "FID" .lbrn2 layer at the positions of the SAME
layout string the check uses — one source of truth for hole distances. Shapes
are circles (chord-error ≤ 2 µm polygon, ≥16 segments) or axis-aligned
rectangles, configurable width/height.

Detection had a circle baked in (nominal area π/4·d², circularity vs the
circumscribed circle, radial disc/ring contrast). `FiducialProfile` now carries
a `FidShape` (Circle/Rect): expected area per shape, the aspect gate measures
bw/bh RELATIVE to the expected w/h (same 0.3–3.3 band, so circles/squares
behave exactly as before), rectangularity (area / bbox area) replaces
circularity for rects, and the ring-contrast radii derive from the rect
diagonal. Circle paths were kept bit-for-bit identical on purpose — the
bench-validated calibration grid fitter rides on them.

Persistence: the Fiducials tab's diameter/search/profile were never saved;
they are now, along with the new `fid_shape`/`fid_height_mm`/`fid_out` keys
(absent keys keep defaults, so old settings blobs load unchanged).

## 2026-07-21 — Operator export recipe, camera-fresh Place loads, drag-speed fix

LightBurn export settings the operator actually tunes at the bench — speed,
Q-pulse width, fill interval, passes — are now Job-tab fields (persisted).
One recipe intentionally drives BOTH export paths (Job-tab emit and Place
"Etch here"): the register verb grew the same six process flags emit already
had, defaulting to the old hardcoded 20%/1000mm/s/30kHz/1ns/1-pass so
existing invocations are byte-identical. Power % and frequency stay
CLI-only until asked for.

"Load frame + job" with an EMPTY bed-frame path now grabs a fresh frame from
the Camera-tab source (same one-click contract as the fiducial check); a set
path still loads that file. Empty is the default, so the Place view reflects
the board as it sits now instead of a stale file.

Place-tab dragging was O(polys × frame_height): the compositor scanline-filled
every poly over EVERY frame row, per drag step. Scanlines are now clamped to
each poly's projected bbox, off-frame polys and same-side off-frame edges are
culled (Cohen–Sutherland trivial reject), the gray→RGBA base frame is
converted once at load and cloned per recompose, and the GPU texture is
updated in place instead of reallocated. Output pixels are unchanged —
clip-boundary regression tests lock the fill/stroke semantics.

## 2026-07-21 — One-click "Etch + run in LightBurn" from Place

The operator was exporting the registered .lbrn2 from Place, then hand-loading
and starting it in LightBurn. Place now has a second button, "Etch + run in
LightBurn": it runs the same register export and, on success, drives LightBurn
over its officially documented UDP automation interface (datagrams to
127.0.0.1:19840, replies on the fixed port 19841) to PING, LASER:<device>,
FORCELOAD the absolute path, gate on STATUS, START, then poll STATUS to
completion. On this galvo the STATUS busy->idle edge tracks the real burn, so
"once busy has been seen, the next idle = done" is a valid completion signal
(a job that never registers busy within 10 s finishes with a warning).

The UDP client lives in the `drivers` crate (`drivers::lightburn`), kept out of
the UI so the native JCZ/EZCAD driver (DRV-6) can replace it later without
touching the console. The target address + reply port default to the documented
ports but are overridable via `PCBFORGE_LIGHTBURN_ADDR` /
`PCBFORGE_LIGHTBURN_REPLY_PORT`, which lets headless tests point the run at a
fake LightBurn on ephemeral ports. The run is a background job mirroring the
existing verb job: it chains off the export only when the export actually
started and exited cleanly, so a refused or failed export never etches a stale
file. FORCELOAD uses `std::path::absolute` (not canonicalize) to avoid the
`\\?\` prefix, which LightBurn mishandles, on a file that may not exist yet.

## 2026-07-21 — Auto fiducial layout from board dimensions, field-warped holes

Hand-typing the fiducial layout string meant the operator was doing corner
math (board size, margins, field centring) by hand, and the generated holes
ignored the f-theta distortion entirely — positions and hole sizes were only
as true as the lens. Calibration grew a step "4) Fiducial holes (board)":
enter board W×H and an edge margin (board-edge → hole centre), and the console
computes the four corners for a board centred on the effective laser-field
centre, writes them into the SAME layout string the fiducial check reads
(one source of truth, unchanged), and generates the .lbrn2. A "board size
from job" button fills W/H from the active side's Gerber board bbox.
Generation is disabled when 2×margin swallows the shorter side or a hole
falls outside the addressable field.

The layout stays in PHYSICAL mm — the camera check measures reality — and
only the commanded geometry is pre-distorted: `fid-holes` gained
`--field-map`/`--field-seg-mm` mirroring `emit` (densify to ≤0.25 mm, then
`FieldMap::precompensate` per vertex; hard error on an unreadable map, loud
warning + unwarped output when omitted). Both hole-generation paths (the
Fiducials tab and the new step) pass the map whenever an accepted laser-field
fit exists, so hole spacing AND size burn dimensionally true. It sits under
Calibration because the output is only as good as step ③ — the step's copy
says so.

## 2026-07-21 — Fiducial check reworked to the calibration workflow; detection now places the job

The Fiducials tab predated the calibration UX: markers had to be hand-dragged
onto each hole, results lived in a cramped single column, and a successful
check changed nothing downstream — the operator still hand-dragged the design
in Place. Now the tab mirrors Calibrate: a resizable controls-over-image
split, and a click-in-order marking round (loading a frame or resetting
markers opens it; each primary click drops the next numbered ✛, ghosted
markers show what's left, and the final click runs the check). Grab/live
paths still auto-detect at the seeded positions — clicking is for frames
where the seeds land nowhere near the board.

A successful check now fits the board's actual pose and writes it into
Place's x/y/rotation. Pairs are nominal layout → detected machine mm
(via place_projection, NEVER the tab's own design→px homography — inverting
that recovers the layout). The fit is the reflection-aware rigid Procrustes
(fit_rigid): flip_x tells us whether the pattern is mirrored, which must
match the selected side or the update is refused with a pointed note. Both
sides reduce to rot = fit angle, (tx,ty) = fit(layout centroid) — on the
back, the design's x=0 mirror (already applied by active_job) and the fitted
physical flip compose to a proper rotation, and the sources are
exit-magnified (the camera sees drilled holes' EXIT openings; copper is a
surface mark and carries no parallax, so the placement itself is unmagnified
— fiducial-anchored alignment is exact at the scan center). Gates: ≥3
detected, RMS ≤ 0.5 mm, flip matches side; the convention baked in is that
the design centers on the fiducial-layout centroid. load_place preserves an
auto-set pose instead of recentering; side switches clear it. Back-side
EXPORT remains refused (emit_at_placement) — this feature places the
overlay; the back-copper emit path is still future work.

## 2026-07-21 — Fiducials: ✕ clear markers empties the layout, ↺ reset keeps it

With click-to-place able to grow the expected set one click at a time, an
overgrown set (15 markers on a 4-hole board) had no one-step exit: ↺ reset
markers reseeds the ✛s from the layout string, so all 15 came straight back,
and the only true clear was hand-editing the layout field. The two intents
are now two buttons: ↺ reset re-nominalizes positions and reopens the marking
round (layout kept), ✕ clear removes every expected fiducial. Because the
layout string is the source of truth that sync_fid_markers reseeds from every
frame, clear MUST empty the string itself — clearing only search/found would
be undone one frame later. Clear also drops rows/measured/homography and
cancels any marking round, but leaves placement and the cached pose alone:
removing markers must not move an already-registered job.

## 2026-07-23 — Wobble is off by default and configurable per job

Every export inherited `wobble: true` from `EmitLayer::fill` — transcribed
from the operator's base config, where wobble is a device-profile choice, not
a process default. Result: all emitted Fill jobs (emit, register, calibration
grids, fiducial holes) silently ran wobbled, widening lines and softening
calibration dots. `EmitLayer::fill` now defaults wobble OFF like `line`, and
the file keeps writing an explicit `wobbleEnable=0` (LR-36) so the device
profile can't re-enable it. Opting in is per job: `--wobble` on emit/register
(with `--wobble-step-mm` / `--wobble-size-mm`, 0 = device default) and a
wobble checkbox + step/size fields in the console's Job recipe, persisted and
applied to both Emit and Place's "Etch here". `wobbleStep`/`wobbleSize` are
inferred field names (no sample varies them — flagged in lbrn2-schema.md for
verification on first live use). The golden test still reproduces the
operator's base sample by setting wobble explicitly.

## 2026-07-23 — Fiducial holes burn immediately after generation

"⚙ Generate holes" only wrote the .lbrn2; the operator then had to open
LightBurn, load the file, and press start by hand — a gap where the wrong
file (or a stale one) could be run. Both hole generators (the Fiducial tab
and the ④ Fiducial-holes calibration step) now chain the existing LightBurn
automation after the export: the `fid-holes` verb finishing arms the same
`pending_lightburn` path as Place's "Etch + run", which selects the Place-tab
device, FORCELOADs the file, gates on an idle STATUS, STARTs, and polls to
completion — all reported in the log. The queue only arms when the verb
actually launched, the queued path is absolutized without canonicalizing
(\\?\ upsets FORCELOAD), and the buttons (renamed "Generate + burn …")
disable while a run is in flight, like Etch + run. If LightBurn isn't open
the run fails with the friendly not-responding note and the file is still on
disk for a manual run.
## 2026-07-23 — drill-emit: pure drill-hole geometry as a LightBurn job

- New extraction path: `cam::drill::drill_polys` turns `DrillEntry` lists
  (the CAM-6 ingest-free drill carrier) into pure hole outlines — a
  circle-approximation polygon per round hole and a capsule (two straight
  sides plus semicircular caps: the bit's swept outline) per G85 slot. Rings
  wind CCW, vertices sit on the ideal circle under the `circle_segments`
  2 µm chord bound, and coordinates pass through verbatim — placement stays
  the emitter's concern.
- `pcbforge drill-emit` wraps it: Excellon file(s) via `--drills`
  (repeatable, because KiCad exports PTH and NPTH holes as two separate
  files) or a board via `--board` + kicad-cli, emitted as a `DRILL` Fill
  (filled discs) or Line (outline contours) layer through the existing
  lbrn2 emitter with the emit-style recipe, placement, and field-warp
  flags. The fid-holes field-warp helper was generalized (`warp_polys`)
  rather than duplicated.
- Frame decision: drill files share the Gerber y-up-but-offset-negative
  frame, so the default normalization sends the drill pattern's own bbox
  corner to the origin. That corner is NOT the copper job's corner, so
  `--outline` (Edge.Cuts) pins the frame to the board region's corner
  instead — the same corner `emit` normalizes to — keeping a drill job
  co-registered with the copper job emitted from the same board; the
  placement flags then anchor the board region, not the drill bbox, for
  the same reason. Both exports must use the same origin convention
  (KiCad's defaults agree).
- Scope note: drilling is a hand operation today (ORC-7 drill-guide); this
  verb only extracts and emits the geometry. Whether the UV laser actually
  cuts hole outlines through FR4, and at what recipe, is the operator's
  call — defaults mirror the other emit verbs.

## 2026-07-23 — Place tab: "Emit drill holes (no burn)"

- New Place-tab control emitting ONLY the drill-hole geometry at the current
  placement: `drill .drl` input (`;`-separated — KiCad exports PTH and NPTH
  as two files), `drill out .lbrn2` output (bare names land next to the
  drill file), and a `⤓ Emit drill holes (no burn)` button. The button
  writes the file and stops: it never touches `pending_lightburn`, so the
  etch/run chain can't fire — the operator opens and starts the job in
  LightBurn themselves.
- Runs in-process (`ingest::excellon` → `cam::drill::drill_polys` →
  `Placement::affine()` as a `cam::register::Affine2` →
  `transform_shapes[_field]` → `cam::lbrn2::write_lbrn2`) instead of
  shelling a verb: `drill-emit` only takes a translation origin, so a
  rotated placement is not expressible through the CLI. The affine layouts
  match ([a,b,c,d,e,f] row-major), and drill files share the Gerber frame,
  so the copper job's placement affine positions the holes directly — no
  normalization, the placement IS the position, exactly like "Etch here".
- Field-warp fires under the same conditions as "Etch here" (valid
  calibration for the loaded frame + the map file), reading the same
  `pcbforge-field-map.txt` with the CLI's 0.25 mm segment default, so the
  two exports land on the same physical geometry. An unreadable map file
  REFUSES the emit rather than silently exporting unwarped — the operator
  believes exports are warped while that file exists.
- Same guards as the etch buttons: back side refused (no mirror pass —
  wrong chirality at the wrong spot, silently), job + frame required (the
  pose is meaningless before a Load). Recipe = Job-tab params over the
  register verb's default 20 % power, layer name `DRILL`.

## 2026-07-23 — Drill files extracted from the KiCad project, like the Gerbers

- `ingest::kicad_cli::export_job_drills` is the drill counterpart of
  `export_job_gerbers`: `--excellon-separate-th` (flag-checked via
  `require_flags`) always splits plated/non-plated, and the outputs land
  under stable names `pth.drl` + `npth.drl` regardless of kicad-cli's
  board-derived naming. A side the export produced no file for (a board
  with no NPTH holes) gets a valid empty Excellon placeholder — parser-
  verified — so downstream loaders see "zero holes", never a missing path.
  A merged file (should a kicad ever ignore the flag) stands in on the PTH
  side; it still holds every hole. The "Created file" stdout parsing and
  the rename-or-copy move were factored out (`quoted_created_files`,
  `move_into`) and shared with the Gerber exporters.
- New `pcbforge drills --project --out` verb mirrors `gerbers`: resolves
  the board, exports, prints parseable `board:`/`pth:`/`npth:` lines.
- Console: "⚙ Drills from KiCad" on the Place tab mirrors the Job tab's
  Gerbers button — deterministic paths
  (`<board dir>/pcbforge-gerbers/{pth,npth}.drl`, the same directory the
  Gerbers land in) fill the `drill .drl` field immediately, and the verb
  shells in the background with progress in the Log.
- Test-honesty note: kittest consoles share one settings sidecar, and
  focused text fields APPEND typed text — the new interaction test
  select-alls before typing and asserts positively (path in → field
  filled), because "field is unset" assertions are order-dependent there.
  Also recorded: kicad-cli is absent on this dev container, so every
  kicad-gated test self-skips (they print ok) — the export itself needs a
  machine with KiCad to verify.

## 2026-07-23 — Drill emit now LOADS the file in LightBurn (still never starts)

- The Place-tab drill emit grew the missing half of the handoff: after
  writing the `.lbrn2` it now drives a **load-only** LightBurn run —
  PING → device select → FORCELOAD → done. START is never sent (the
  worker's `start_job` flag short-circuits before the STATUS gate), and
  `pending_lightburn` (the etch path's export→start chain) is never
  touched, so the "no burn" contract holds while the operator finds the
  job already open in LightBurn; button renamed
  `⤓ Emit drill holes → LightBurn (no burn)` to say so.
- `spawn_lightburn_load` shares the whole worker with the etch path's
  `spawn_lightburn_run` rather than duplicating the UDP dance;
  `LightburnRun::load_only()` lets tests (and future UI) tell the two
  apart. The fake-LightBurn UDP test proves a load-only run FORCELOADs
  the path and that START never crosses the wire.
- A LightBurn run already in flight skips the load with a warning (the
  file is still written) — replacing a live run's progress reporting
  would be rude; the button is also disabled while one runs, mirroring
  "Etch + run". An unresolvable absolute path likewise degrades to
  "written; open it manually" (FORCELOAD dislikes `\\?\` prefixes, same
  rule as the etch chain).

## 2026-07-25 — CI red for 41 straight runs; three gates in one job hid a red suite

- CI's last green was **2026-07-20T21:48**, followed by **41 consecutive
  failures**. Every one died at `cargo fmt --check`, which was step 1 of 3, so
  for that whole stretch **`cargo clippy` and `cargo test --workspace` never
  executed** — a trailing-comma diff masked both, and PRs #4 and #5 were merged
  red. Behind that gate: 27 rustfmt diffs and 9 clippy lints under
  `-D warnings`.
- Measurement caveat worth recording, since it nearly became a wrong
  conclusion: a `--limit 40` sample showed 40 failures and zero successes and
  read as "CI has never passed." The window happened to start just after the
  last green, so it captured exactly the red streak. Widening to 100 runs shows
  32 earlier successes. The streak was the real finding; "never" was an
  artifact of the sample size.
- Split into three independent jobs (`fmt`, `clippy`, `test`) rather than
  reordering or `continue-on-error`: three sequential gates in one job yield
  one bit of signal, and the cheapest gate was the one masking the expensive
  ones. Toolchain action pinned to 1.92 to match `rust-toolchain.toml`;
  `@stable` read as if CI tested stable while rustup silently honoured the pin.
- Not added: a `windows-latest` leg, despite Windows being the dev platform.
  It is the right follow-up, but adding an unproven runner while making CI
  green for the first time risks landing a required check that is already red.
- `load_only` tripped the dead-code lint but is **not** dead: it is set from
  live state in `run_worker` and read by two tests asserting START is never
  sent. The lint fires only because clippy's non-test config cannot see those
  readers, so the allow is scoped `not(test)` rather than the item deleted.

## 2026-07-25 — The test suite was history-dependent, not flaky

- `cargo test --workspace` passed once and failed on the very next run, same
  tree, same command. `ui_interaction.rs` built every harness from a fixed
  `temp_dir()/pcbforge-kittest.sqlite`, and `path_for_db` derives the settings
  sidecar from the DB path — so 21 parallel tests shared one store, and the
  console's own field persistence wrote the suite's inputs back for the next
  run to read.
- Two concrete poisonings were found in the leftover file:
  `place_lightburn_device=BSLFiber` + `Galvo9`×11 (a test typing onto what a
  previous run persisted), and an empty `fid_layout=` written by the test that
  clicks `✕ clear markers` — which then fails that same test's own
  precondition next run. Causation confirmed by injecting `fid_layout=` by
  hand: the test fails; remove the file and it passes.
- The assertions were correct and were left untouched; the isolation was the
  defect. `src/app/tests.rs` had already solved this with a per-call unique
  directory, and its comment diagnoses the exact bug — the integration tests
  simply never got the fix. Lifted into `tests/common/mod.rs` behind a guard
  that drops a `TempDir` after the harness.
- That same helper was, however, the biggest *leaker* on a dev box: unique per
  call but removed none, ~150 directories per run. Callers hold only a
  `PathBuf`, so there is nowhere to hang a guard without threading one through
  every call site; nesting under one per-process parent makes the residue
  proportional to runs instead of tests. The pre-existing pile (~1700
  `pcbforge-*` and ~5400 `ui-app-*` on this machine) was left for the operator
  to clear.
- `run_capture` came off the public surface while here: it blocks until the
  child exits, which for the default `cargo run -q --bin pcbforge --` is a
  compile plus a run with the window frozen. Only two tests call it.

## 2026-07-25 — Malformed input must error, never wrap into a coordinate

- **The Gerber coordinate path was not already guarded**, contrary to the
  comment at `coord_to_nm`: it guarded the `i128→i64` cast but not magnitude,
  and `signed_area2` squares coordinates into an `i128`, so a four-vertex `G36`
  region at ±9e18 nm — legal under `%FSLAX36Y36*%` — overflows the area
  accumulation. A wrapped area's sign flip **inverts ring orientation, turning
  copper into void**. Bounded at `MAX_COORD_NM` = 1 km. This newly rejects
  coordinates that previously parsed; 1 km is ~1000× any panel and the
  alternative is a silently inverted layer.
- `excellon::decimal_to_nm` overflowed `i128` on the scale multiply — the
  integer part parses up to 38 digits while only the *fractional* side had a
  9-digit cap. Reproduced as a panic; release would have wrapped.
- Aperture and macro parameters were checked for presence but never magnitude
  or finiteness. Guarding the two entry points (`parse_f`, the macro argument
  vector) covers all eleven downstream cast sites, rather than patching casts.
- Macro expression recursion is now depth-bounded: a stack overflow **aborts
  the process and cannot be caught by `catch_unwind`**, so it is the one
  failure this module's recovery discipline could not absorb. Depth-threading
  was chosen over a record-length cap because a short record of unary minuses
  still blows the stack.

## 2026-07-25 — Non-finite values reached machine coordinates

- Rust float→int casts saturate: `NaN → 0`, `∞ → i64::MAX`. Nothing checked
  between a fit and the nm cast, so one non-finite vertex became machine
  coordinate (0, 0) and the beam would draw a line across the board to reach
  it. `transform_shapes_field` now returns `Result`, validating **in mm before
  scaling** — finiteness alone is insufficient, since a finite 1e300 mm still
  saturates the cast. Every field-corrected coordinate funnels through there.
- `Poly2::from_coeffs` accepted `scale = 0`. Because `apply` *multiplies* by
  scale, that collapsed the basis to `[1,0,0,…]` and mapped every point to a
  constant — finite, so it passed every existing check, and the whole job would
  burn at one spot at full dwell.
- The CLI's acceptance gates failed **open**: `fit.rms > max` and
  `det <= 0.0` are both false for `NaN`, so a NaN transform passed. Rewritten
  fail-closed. `f64::from_str` accepts `"nan"`/`"inf"`, so this was reachable
  from `--fiducials` operator text.
- `Poly2::fit`'s floor went 10 → 11, not 20. At exactly 10 points the fit is
  determined and its residual is **0 by construction**, so the operator was
  told "RMS 0 µm — camera is now a metric ruler" for the least trustworthy fit
  possible. 20 was rejected because 16-point grids are used by four existing
  e2e tests and by the UI's own ≥4×4 field gating; 11–19 points are
  optimistically biased but not fabricated.
- `from_coeffs` validates `scale`/`center` but deliberately **not** the 20
  coefficients: coefficient finiteness is already enforced on every path, and
  moving it there would remove the only way to construct the non-finite map
  that three existing fail-closed regression tests assert is refused.

## 2026-07-25 — Calibration fitting split out of the console into `crates/calib`

- `ui/src/calib.rs` + `ui/src/calib/square_grid.rs` were 3,129 lines with
  **zero** egui references and zero references to anything else in `ui` — a
  strict one-way consumer of `vision` parked in the GUI crate. Since nothing
  depends on `ui`, the entire fitting pipeline was invisible to the CLI and
  testable only through the console.
- Moved to a **new `calib` crate rather than into `vision`**: `vision` owns
  primitives (homography/affine/lens fits, blob detection, warp), `calib` owns
  the operator workflow above them (paper grid → lens fit → anchor → laser
  field) and the acceptance gating. Different layers, different reasons to
  change; folding 3,129 lines into `vision` would have doubled it and blurred
  that line.
- Phase 1 is relocation only — no logic changed, tests moved with the code.
  `refit_anchor_dots` was the single item widened (`pub(crate)` → `pub`); the
  other 22 items `ui` uses were already public. The burn-grid fixture moved
  too: `CARGO_MANIFEST_DIR` now resolves to the new crate, which is how the
  suite caught the one thing the mechanical move missed.
- `ui`'s public re-exports of the calibration API were **dropped** rather than
  kept as a shim, so the console re-exports only console things; its `dump_*`
  examples name `calib::` directly.
- `ui/src` drops from 16,305 to 12,702 lines. The egui-free residue left in it
  is now a short list: `app/settings_io.rs` (738, legitimately UI — it
  serializes `ConsoleApp`'s own fields), `app/lightburn_run.rs` (557, a
  candidate for `drivers`), `app/projection.rs` (182), and
  `settings.rs`/`status.rs` (265).
- Still to do: promote the true primitives that remain mixed into `calib`
  (`Rigid2`/`fit_rigid`, `Similarity2`/`fit_similarity`, `invert_poly`, the
  `camera_px_to_*` helpers) down into `vision`; then `app/projection.rs`; then
  split `fiducial.rs` and `place.rs` at the egui boundary — `fit_board_pose` is
  the valuable one, it decides whether the job moves on the board. A
  `pcbforge calib-fit` verb is the payoff, since it makes the fit pipeline
  testable against real captured frames.

## 2026-07-25 — Fiducial positions are a centred rectangle's W×H, not coordinates

- The operator now gives **two numbers** — the centre-to-centre W and H of the
  fiducial rectangle — and the four positions fall out of centring it in the
  work area (the Camera-tab field, `field_cx_mm`/`field_cy_mm`). Typing
  `x,y; x,y; …` was the only place in the console that asked for absolute bed
  coordinates, and it asked for them twice over: the Fiducial-check tab's
  `expected` text field, and ④'s board W/H + margin, which computed the same
  thing a different way and only reached the check on Generate.
- **Centred means centred, not four equal gaps.** `field_mm` is one scalar, so
  a W≠H rectangle has unequal x/y insets; that is correct and intended.
- `layout: String` stays the internal source of truth (`parse_layout` feeds
  `expected_points`, the design-spacing scale, the homography correspondences
  and `fid-holes --layout`). W/H **generate** it on edit rather than replacing
  it — a per-frame rebuild would silently undo click-to-place, which writes
  the layout directly. That containment is what kept the change small: the CLI
  and every consumer downstream of `parse_layout` are untouched.
- `board_fid_layout(cx, cy, w, h, margin)` became
  `centered_fid_layout(cx, cy, w, h)`. Folding the margin out is the point:
  W/H are hole-CENTRE spans, so the corners land exactly on them, and the
  "margin swallows the shorter side" validation disappears with it.
- Persisted `fid_board_w_mm`/`fid_board_h_mm`/`fid_margin_mm` are **not** read
  as the new `fid_rect_*` keys. A stored board size is an outline; reading it
  as a centre span would silently widen a saved layout by 2×margin. Old blobs
  fall through to the 50×50 default, which reproduces the shipped default
  layout exactly under the default 70 mm field.
- ④'s "board size from job" survives as "rect from job board size", insetting
  the Gerber bbox 5 mm per side — a constant, not a third knob back.
- Labels are "fiducial rect W/H mm", deliberately not "width/height mm": those
  already name the Rect *footprint* size in both forms, and a third pair would
  make `click "width mm"` ambiguous in the accessibility tree.

## 2026-07-25 — Fiducial holes: generate only, at the drill recipe

- **The button no longer burns — it hands off.** "⚙ Generate + burn holes"
  became "⚙ Generate holes → LightBurn (no burn)": it shells `fid-holes`, and
  once the export finishes the file is FORCELOADed in LightBurn. START is never
  sent. Exactly the contract the Place tab's drill emit already held, down to
  the label shape.
- That required the queued hand-off to learn the distinction, so
  `pending_lightburn: Option<PathBuf>` became
  `Option<PendingLightburn { path, start }>` and `chain_lightburn_after_verb`
  picks `spawn_lightburn_run` vs. `spawn_lightburn_load`. The drill emit didn't
  need this because it emits **in-process** — the file is already on disk, so
  it can spawn the load directly. The fiducial path shells the CLI
  asynchronously; without the flag there was no way to say "load this when the
  export lands, but don't fire".
- `!lightburn_busy()` still gates both generate buttons, for the reason the
  drill button carries: the load-only run replaces `lightburn_run`, and
  stomping a live burn's progress reporting would be rude.
- `debug_summary` gained a `lightburn=pending-load` token alongside `pending`.
  Whether a queued hand-off can fire the laser is precisely the thing a
  headless `state` dump must not have to guess, and `is_some()` reported both
  identically.
- **These are drilled holes, so they burn at the drill recipe.** `fid-holes`
  baked in `EmitLayer::fill` at a hardcoded 20 % / 1000 mm·s⁻¹ / 30 kHz / 1 ns
  / 1 pass. It now takes `--mode` plus the same process flags `drill-emit`
  takes, and the console passes `--mode line` with the Job-tab params — exactly
  the recipe `emit_drill_at_placement` builds. A 1 mm circle scanned as a fill
  is an engraved dot; traced as a Line layer it is a drilled one.
- `--mode` defaults to **`line`** here, unlike `drill-emit`'s `fill`. The verb
  has one caller (the console), and two defaults disagreeing across the
  CLI/console boundary is the failure mode worth avoiding; `fill` on a fiducial
  was arguably never right. The flag keeps it overridable.
- `fid_holes_cmd` took a `FidHolesArgs` struct, mirroring `DrillEmitArgs` —
  fifteen positional parameters was past the point where the
  `too_many_arguments` allow was carrying its weight.
- New failure path, by construction: Job-tab params now reach
  `AblationParams::validate()` at write time, so a value the Job tab accepts
  but lbrn2 rejects fails the export (visible in the Log). Impossible while the
  recipe was baked in — worth knowing before it shows up on the bench.
- **Open question left for the operator**, not silently resolved here:
  `emit_drill_at_placement` builds a *Fill* layer, so "the drill settings" as
  the repo implements them today are fill mode. Line was used here because it
  was named explicitly. Either the Place-tab drill emit should be Line too (a
  separate fix), or Line is fiducial-specific.

## 2026-07-26 — Fiducial rows report the fit residual, not just detector drift

- Symptom from the bench: four strong detections, every row reading `off < 700
  µm`, and `fiducial fit RMS 3.39 mm too loose — placement not updated`. The
  rows looked like a passing check.
- They were measuring a different thing in a different frame. `summarize`
  compares `found_mm` against the **search marker**, through
  `BedMap::uniform_scale_y_flip` at the *seeded* px/mm. The pose fit compares
  the **design layout** against machine mm through `place_projection`. A small
  `off` only means the detector locked near where you clicked; it stays small
  however wrong the fit is, so the two numbers were never comparable.
- `fit_board_pose` now returns `PoseFit { pose, residuals_mm }` — per-point
  distances aligned with the full layout — and `rms_mm` is derived from those,
  so the aggregate and the per-point numbers cannot drift apart. Each summary
  row gains `fit N.NN mm`.
- Labelled **before** the mirror/RMS gates, deliberately: a rejected fit is
  precisely when the operator needs to see which fiducial is the outlier.
- `update_placement_from_fiducials` also stops swallowing the projection error
  (`Err(_)` → `Err(e)`). `place_projection` fails either because nothing is
  calibrated or because the calibration doesn't match this frame's
  resolution/crop/orientation — and the old blanket "no camera→machine
  calibration" reads as flatly untrue in the second case, which is the one
  that's actually fixable.
- Reverted from the rectangle work: ↺ reset was briefly made to rebuild a
  ✕-cleared layout from W/H. That contradicts 2026-07-21 (clear means gone, or
  an overgrown click-placed set has no one-step exit) and its regression test
  caught it. Regeneration got its own button instead — **⟳ layout from W×H** —
  which also covers the case editing the DragValue can't: wanting the value
  already in the field, since `changed()` only fires on an actual change.

## 2026-07-26 — The fiducial check seeds itself, and carries the operator's offset

- **Seeding through the calibration, not the typed px/mm.** The ✛ markers lived
  in the uniform seeded-px/mm frame, so where they landed was only ever as good
  as the px/mm guess — which is why the tab asked for a click on each hole
  before detection could find anything. But the holes are burned at *known
  machine coordinates*, and `place_projection` is the map that says where a
  machine-mm point images. `seed_fid_markers_from_projection` pushes
  `expected_points()` (side-aware: mirrored + beam-offset on the Back) through
  it and converts the pixels back into the tab's uniform frame. Every marker
  then starts inside its own search window and the click round disappears.
- It runs **only** on the explicit actions — ⤵ Load frame, 📷 Grab & check,
  🎯 Check — and pointedly *not* from `sync_fid_markers`, which the overlay
  calls every frame. Seeding there would drag the markers off their holes as
  fast as an operator could place them; the same argument rules out seeding in
  the Live pump.
- When there is no usable projection every path keeps exactly its old
  behaviour (Load opens the marking round; Grab/Check detect at the raw layout
  seeds) and the projection's own reason is appended to the note — the operator
  should know why they are back to clicking. The append happens *after*
  `start_fid_marking`/`detect_fiducials`, both of which rewrite the note
  wholesale.
- 🎯 Check on a cold tab with a frame file now **returns** after the load
  instead of falling through to a second detection. The load either seeded and
  checked, or opened the click round whose last click checks; the fall-through
  ran `check_frame` twice and buried the round's "click fiducial 1 of N"
  prompt under a tally the operator hadn't asked for.
- **One refine pass** in `detect_fiducials`. A small uniform placement error —
  a nudged board, a slightly-off px/mm — moves every hole the same way, so the
  fiducials that were found say where the missed ones went: shift the misses by
  the mean hit displacement and look once more. Needs ≥2 hits so a single bad
  detection can't drag the misses off on its own, and only the missed seeds
  move, so the hits search unchanged windows and cannot regress. The refined
  seeds stay local — writing them back would make the ✛ walk on every Check and
  every frame under Live, the same stomp the sync rule above avoids.
- **A manual placement adjustment now travels with the board.** Every
  successful Check used to overwrite tx/ty/rot with "design centred on the
  fiducial centroid", discarding whatever the operator had dragged. The fit
  inside `fit_board_pose` is a nominal-bed → measured-bed correction, so the
  adjustment is expressible in the *nominal* frame: map the current placement
  back through the fit it was written under, measure it from the layout
  centroid `b0`, and re-apply that offset under the new fit. `PoseFit` gained
  `fit` and `layout_centroid` to hand both out.
- The reference frame is `fiducials.last_fit`, set only on an APPLIED Check.
  It is dropped wherever the nominal frame stops meaning anything: `set_side`
  (the other face's fit is not a frame this face can be measured against),
  `apply_fid_rect` and `clear_fid_markers` (the layout moved, so `b0` moved,
  and the stored offset would displace the design by the difference). With no
  stored fit the offset is zero — i.e. bit-for-bit the old centre-on-the-
  fiducials behaviour, which is what the first Check after any of those does.
- No `Rigid2::inverse` was added: `inverse_apply` already exists and is exactly
  the `F · Rᵀ` mirrored form needed, with round-trip tests for both `flip_x`
  cases. A second spelling of the same inverse is a second thing to get wrong.
- Known asymmetry, left alone: `fiducials.pose` stays the *board's* fit, so
  the verdict line's rotation is the board's, not the design's, when a rotation
  offset is being carried. The pose is the measurement; the placement is what
  the operator moved.

## Fiducial location: whole-frame recovery when the markers are nowhere near

- **The detector stays local; a second, independent path finds the holes when
  the local search has nothing to search around.** `vision::find_fiducials` is
  local because the honeycomb bed is covered in decoys (module header, field
  photo 2026-07-14) — that has not changed. But the local search assumes the
  markers are already near the holes, and when they are not (bad calibration,
  board moved, a layout that was never in machine coordinates) *every* fiducial
  misses and the operator is handed nothing to act on.
  `vision::find_fiducial_candidates` scans the whole frame for fiducial-SIZED
  blobs and `ui::fiducial::match_layout_to_candidates` picks out the subset
  whose ARRANGEMENT matches the layout. Arrangement is a far stronger decoy
  discriminator than any per-blob gate, so the candidate pass is deliberately
  permissive — it over-admits and lets the match do the rejecting.
- **Tiled local statistics, not a global threshold.** Median + MAD per tile of
  ~8 dot diameters (clamped 32..=128 px), bilinearly interpolated from tile
  CENTRES so there are no seams. A single global threshold drowns in the bed
  glare gradient — the same reason the local search derives its threshold per
  window. The mask sits at `MIN_SNR/2` σ above local background: `MIN_SNR` gates
  a matched-filter *peak*, while a per-pixel mask must also keep the blob's
  anti-aliased skirt or the area gate rejects what is left.
- **Tolerance is span-relative, and the separation gate is a RATIO.** The bench
  frame (`samples/fiducial/bench-plate-4holes.png`) shows the plate tilted
  enough that its diagonals differ by 9%: no similarity fits the observed quad,
  and the best one still leaves ~5% of the span on a corner. An absolute pixel
  tolerance is therefore wrong twice over — it does not track camera distance,
  and the layout's own pixel span is only as good as the operator's typed
  px/mm. Hence `match_tol_px` (9% of the layout span) and a scale-ratio band of
  ±30% instead of an absolute `|d_cand − d_layout|` test. The same perspective
  is why the rotation gate is 20°, not "a few": one edge of that frame reads 16°
  of apparent rotation on a board that is square to the machine.
- **Hypotheses are ranked by candidate QUALITY before residual.** Ranking by RMS
  alone picks decoys: a honeycomb bed is a regular grid, so among a hundred bed
  holes some four sit on a near-perfect scaled copy of the layout and fit
  *tighter* than the real, perspective-warped board. Residual cannot separate
  them; how much each blob looks like a drilled fiducial can — on the bench
  frame the four plate holes rank 0/2/7/11 of 161. `candidates_px` must
  therefore arrive best-first, which is the contract
  `find_fiducial_candidates` documents.
- **A Check tries markers → projection → rectangle match, and keeps the first
  that works.** `render_fiducials` used to re-seed from the calibrated
  projection unconditionally before detecting. That threw away markers the
  operator had already clicked onto the holes, and for a layout whose
  coordinates are themselves click-derived the projection lands off the plate —
  turning a working 4-of-4 Check into 0-of-4. Each stage runs at most once per
  Check, the note names which one succeeded, and if none beats the operator's
  markers they are restored: a failed Check must never park the ✛ set wherever
  the last failed attempt left it. The whole-frame stage is skipped entirely
  under Live — it is a full-frame scan, not something to run per streamed frame.

## 2026-07-26 — "⌖ layout from detection": making the current pose nominal

- The fiducial check exists to compensate for imperfect operator board
  placement, but on the bench it kept refusing: `fit_board_pose` fits
  layout → measured as a RIGID transform, and the operator's layout was a
  hand-clicked quadrilateral standing in for a rectangle. No rotation and
  translation absorb a shape error, so it landed in the residual (3.39 mm) and
  the 0.5 mm gate refused. The gate was right — a job placed from that fit
  burns in the wrong place — but "go hand-fix your layout" is a bad answer when
  the console has just measured exactly where those holes are.
- ⌖ adopts the measurement as the new nominal: the layout is replaced by the
  `place_projection`-mapped detected positions. The residual is then zero by
  construction, and every later Check measures only what changed SINCE — which
  is precisely the placement error the path exists to correct. Teach it once
  with the board where you want it; from then on a Check moves the job by
  however far the board has drifted.
- It also absorbs camera-calibration scale/skew error into the nominal, so a
  rig whose calibration is imperfect still gets usable RELATIVE registration.
  That is a real widening of what the feature tolerates, and worth knowing:
  after adopting, the fit is trustworthy about board MOVEMENT, not about
  absolute machine coordinates.
- Deliberately a button, never automatic. Silently redefining the nominal would
  promote a genuine misdetection to truth — the exact failure the RMS gate is
  there to catch. Enabled only when EVERY fiducial was found, since a partial
  adopt would drop points from the layout.
- Front side only: back-side detections are compared against mirrored,
  beam-offset expected positions, so writing them into the un-mirrored layout
  frame would bake the flip in twice.
- `detected_mm` is cached BEFORE the mirror/RMS gates — a refused fit is exactly
  when ⌖ is needed — and cleared anywhere the layout or side changes, so a stale
  measurement can never be adopted as a layout it no longer corresponds to.

## 2026-07-26 — The fiducial pose fit carries uniform SCALE, and applies it

- The rigid fit could not register the operator's board. Measured fiducial
  spacing came out ~3.8% off the nominal layout. Rotation and translation have
  nowhere to put a spacing error, so all of it landed in the residual: ~1.1 mm
  RMS over a 40 mm square, against a 0.5 mm `POSE_MAX_RMS_MM` gate. The gate
  refused every Check and the placement never moved — the feature was simply
  unavailable on this machine.
- `fit_board_pose` now uses `calib::fit_similarity` (uniform scale + rotation +
  translation, reflection-aware) instead of `fit_rigid`. The residual collapses
  to the true registration error and the fit is accepted.
- The scale is APPLIED, not just reported. `Placement` gained a `scale` term;
  `affine()` is now `bed = s·R(rot)·(g − pivot) + t`, and since
  `correspondences()` encodes that affine as three point pairs and
  `pcbforge register --fiducials` fits a full affine from them, the scale
  survives into the emitted `.lbrn2`. **The burn is resized.** At 1.038 a
  100 mm trace comes out 103.8 mm. `correspondences_recover_the_placement_affine`
  covers a non-unit scale specifically to keep that path honest.
- This is deliberate, at the operator's explicit instruction, after being warned
  of the risk below. It is the opposite of the calibration path's rule: the
  metric paper→machine anchor (`FieldCal::paper_to_machine`) stays RIGID so a
  galvo scale error keeps showing up as residual and `FieldCal::scale` stays a
  pure diagnostic. Only the board pose absorbs scale.
- Safety band: `POSE_SCALE_MIN = 0.90` / `POSE_SCALE_MAX = 1.10`, checked next to
  the RMS gate. This is not belt-and-braces — it is the only guard left. A
  similarity fit absorbs a spacing error instead of exposing it, so a
  self-consistent misdetection at 1.5× now fits with an RMS near zero and would
  sail through `POSE_MAX_RMS_MM`. A few percent is a plausible machine or
  calibration scale error; ±10% is the wrong holes, or a layout that does not
  describe this board. Outside the band the placement is left untouched and the
  note names the measured scale.
- Never silent. The scale and the resulting resize percentage appear in the
  fiducial note, in the coloured verdict line, in `debug_summary`, and as a
  readout with a "reset scale to 1.000" button in the Place tab. It is reset to
  1.0 wherever the placement returns to nominal (the `load_place` recenter
  branch, `set_side`).
- **Residual risk, accepted knowingly.** The fit cannot tell WHERE the 3.8%
  comes from. If it is the machine or the board, applying it is the correction.
  If it originates in the CAMERA CALIBRATION — a mis-scaled lens map or an
  off-nominal `px_per_mm` — then the board is fine and applying the scale
  introduces a real 3.8% dimensional error in the burn, on every job, invisibly
  correct-looking on the overlay because the overlay is drawn through the same
  bad calibration. Cross-check the burned dimensions against calipers before
  trusting this on a job that has to fit real parts.
- Second-order: on the BACK face the fit sources go through
  `cam::flip::entry_to_exit_mm`, itself a magnification. The rigid fit forced any
  error in that exit model into the residual; the similarity fit now absorbs it
  too. So a back-face `pose.scale` is machine scale × exit-model error, not pure
  machine scale — compare front and back before reading either as a machine
  constant.

## 2026-07-26 — The Place tab is deleted; placement moves onto the Fiducial check

Fiducial registration now works on the bench, and the Fiducial-check tab already
draws the placed job as red vector outlines over the very frame the fit was
measured in. That made the Place tab a second, staler view of the same thing:
its image was a separate camera grab, composited a second time, of a board that
might have moved since. The operator's call was to delete it outright rather
than keep two places to look.

- **`CentralTab::Place`, `place_view`, `PlacementState::{frame_img, base_rgba,
  tex}`, `set_place_tex`, `recompose` and `CameraProjection::label` are gone.**
  None had a live consumer once the tab did; keeping them "just in case" would
  have left a full-frame RGBA cache being rebuilt for nothing. `crates/ui/src/place.rs`
  STAYS — `Placement`, `affine`, `correspondences`, `composite_over*` and
  `bbox_center_mm` are a library the Camera tab's AR overlay and
  `examples/dump_place.rs` still use.
- **Frame dimensions, not a frame image.** The only thing the deleted image was
  still load-bearing for was `(w, h)`, which `nonlinear_maps_for_frame` checks
  against `lens_frame_signature` before it will field-warp an export. With
  nothing setting `placement.frame_img` any more, both etch buttons and the
  drill emit would have refused forever. `place_frame_dims()` supplies it from
  the fiducial frame, falling back to the last camera grab, and `drag_place_px`
  takes `(width, height)` explicitly the way `place_projection` already did.
- **The buttons moved to the Actions sidebar**, keeping every safety gate
  unchanged (back-side refusal, `lightburn_busy`, the design-loaded
  precondition, the calibration checks): `⤵ Load design`, `▶ Etch here
  (register)`, `🔥 Etch + Run`, `⚙ Drills from KiCad`, `⤓ Emit drill holes →
  LightBurn (no burn)`. The x/y/rot readout and the fiducial-fitted `scale`
  went with them — the scale RESIZES the burn, so it has to stay visible
  wherever the pose is. The path fields (`out .lbrn2`, LightBurn device, drill
  `.drl`, drill out `.lbrn2`, bed frame) moved to the Job tab, beside the
  Gerbers that feed them. Persistence keys are untouched: same fields, drawn
  elsewhere.
- **`load_place` loads the DESIGN, not a bed frame.** It no longer grabs the
  camera — a capture whose only purpose was to be a backdrop. It parses the
  Job-tab Gerbers, sets `pivot = bbox_center_mm(&ablate)`, and (only when
  `!auto_pose`) parks the job on the centre of the current fiducial frame mapped
  through `place_projection`, or on the work-area centre when there is no frame
  or no calibration to map one with. The `auto_pose` guard is unchanged: a
  fiducial lock is never recentred over.
- **Drill emit derives its own paths.** An empty drill field with a KiCad
  project set is no longer a refusal — the emit resolves
  `<board dir>/pcbforge-gerbers/{pth,npth}.drl`, fills the field with whichever
  exist, and proceeds. It deliberately does NOT re-run kicad-cli: the .drl files
  are re-read from disk on every emit, so a fresh export is picked up for free,
  and shelling the exporter from a burn button would be a surprise. With no
  project, or no files there, the original "name a drill file" error stands.

## 2026-07-26 — Drag the design on the Fiducial-check tab

The placement is now adjusted where it is drawn. A drag that STARTS on the
outlined design moves it; Shift+drag rotates it; a drag starting anywhere else
keeps marking fiducials exactly as before. Ctrl (pan/zoom) always wins.

- **The hit test is the outline's screen-space bounding box**, accumulated while
  the outline is projected. Deliberately coarser than the true outline: a
  point-in-polygon test over every copper ring, every frame, to decide whether a
  press starts a move would cost far more than it buys, and a grab handle that
  is slightly generous is easier to hit than one that is exact.
- **The overlay function was reordered.** The outline used to be projected in
  the paint pass, after the click handlers; the bbox has to exist before input
  is dispatched, so projection now happens first (`project_placed_design`),
  input second, painting last from the collected polylines.
- **"This drag started on the design" is latched**, in `FiducialState::design_drag`,
  on `drag_started` and held to `drag_stopped`. A per-frame local cannot hold it,
  and without it the release of a design drag would drop a ✛ or add a
  click-placed fiducial. It is a gesture, not a setting, so it is never
  persisted.
- **Translation goes through `drag_place_px`,** i.e. it is applied in PIXEL
  space and derived from the pivot each frame, so the outline tracks the cursor
  under a perspective homography and the rounding never accumulates.
- **Rotation sign.** `Placement::affine` builds `[cos, −sin; sin, cos]`, so
  `rot_deg` is counter-clockwise in the y-UP machine frame. Screen rows grow
  DOWNWARD, so the view is a mirror of that frame and `atan2` over raw screen
  coordinates measures angles of the opposite sense. The machine delta is
  therefore the NEGATED screen delta: `angle(prev) − angle(curr)`. Pinned by
  test rather than by inspection —
  `shift_drag_rotates_in_the_machine_sense_not_the_screen_sense` asserts that
  sweeping the pointer up-screen from the pivot's +x axis is +90°, and that a
  sweep across the ±180 seam nudges instead of spinning a full turn.
- **A manual move or rotate does NOT clear `auto_pose`.** The adjustment is
  meant to survive re-Checks: `update_placement_from_fiducials` maps the current
  placement back through `last_fit` and re-applies the offset under the new fit,
  so the nudge travels with the board.

## 2026-07-26 — One shared capture thread, released when idle

Every one-shot camera grab used to open the device, take a frame and close it,
on the UI thread: measured at ~2.1 s (open 280 ms, first-frame warm-up ~1.2 s,
MSMF close 630 ms). The warm-up is device init, not bandwidth — it is still
~1.04 s at 1280×720 — so no resolution or format change touches it. The only
fix is to stop reopening.

- **One `Capture`, owned by `RuntimeState`,** not one per tab. There is one
  camera, so there can only be one open device; the Camera, Calibration and
  Fiducial tabs each owning their own meant two ● Live toggles fought over it.
  `ensure_capture` starts it (or restarts it when the source changes),
  `capture_latest` is what all three Live pumps poll, and `grab_shared` is the
  one-shot path. A later grab is now a slot read instead of a device open.
- **A `Source::File` grab still goes straight through `camera::grab`.** It is
  cheap, needs no device, and pushing it through a capture thread would trade a
  guaranteed-current read of the file for whatever frame the thread last
  happened to take.
- **The first device grab after opening still blocks**, polling at 20 ms up to
  4 s — the warm-up is charged by the device no matter who pays it, and 4 s
  leaves headroom over the measured 1.2–1.5 s while still failing with a message
  rather than hanging. Every grab after that takes the fast path.
- **● Live off no longer releases the device.** This is a real behaviour change.
  A tab's Live toggle now only means "this tab stops asking"; if the per-tab
  pumps still dropped the capture, turning Live off on one tab would kill
  another tab's feed. Release happens in exactly one place, `ui()`'s
  `release_idle_capture`, once no tab is live AND the capture has been unused
  for 10 s. Holding device N locks every other program off it — a
  `pcbforge cam --grab --device 1` fails device-busy — so 10 s is picked to
  outlast the pause between two "grab once" clicks (which is what makes the
  second cheap) without keeping the CLI out for a shift.
- **A failed grab drops the shared capture instead of caching it.** When the
  device won't open, `device_loop` offers the error and its thread exits — the
  slot is then empty forever. Reusing that capture would starve every later grab
  into the 4 s timeout and replace the real "open camera N: …" message with a
  generic one, for the whole idle window. So `grab_shared` clears the capture on
  any error and the next attempt reopens.
- **`device_loop` throttles when nobody is reading.** `Capture::latest` *takes*
  the slot, so a still-occupied slot proves the last frame went unread: the loop
  naps 15 ms instead of decoding another 5 MP frame. Without this, a
  long-lived shared capture would burn a core the whole time it idled. It never
  throttles a real feed — a live pump polls once per UI frame (~16 ms, it calls
  `request_repaint`) against 109–125 ms/frame off the device, so the consumer
  drains ~7× faster than the thread fills and the slot is empty at nearly every
  check. Even `pump_calib_live`, which re-anchors per frame and so polls slower,
  pays at worst one extra nap of latency, not a stall.

## 2026-07-26 — The console keeps a durable diagnostic log

Diagnosing the console meant asking the operator to screenshot the note line.
That is slow, lossy, and gone the moment the app restarts — and the note line
carries a sentence, not the numbers. The immediate motivation is a measured
disagreement between the two paths that are supposed to agree: a fiducial check
locks at 0.09 mm RMS and draws the design inside the fiducial rectangle, while
the exported `placed.lbrn2` holds geometry centred ~67 mm left and ~35 mm below
the detected fiducial centroid. Both paths start at `Placement::affine()`; the
overlay continues through `CameraProjection::to_px` and the export through
`Placement::correspondences()` → `pcbforge register --fiducials`. Nothing on
screen says which leg is wrong.

- **A plain-text file beside the settings blob**, `<db>.console-log`, from
  `crate::diag`. std only, no logging crate, no background thread: the console
  writes a handful of records per operator action, and a dependency (or a
  channel and a drain thread) would be more machinery than the problem needs.
- **One record per line, flushed per record.** Records are infrequent and the
  last one before a crash is the interesting one, so buffering would discard
  exactly what the file exists for. Embedded newlines fold to ` | ` — several
  of the console's own log lines are multi-line, and a reader greps by line.
- **Truncate at startup, rotate once to `.console-log.prev`.** A session's log
  is about that session; keeping history would mean managing it. One rotation
  covers the real case — the operator restarts the console and *then* reports
  the problem. Hard-capped at 8 MB, closing with a "log capped" line rather
  than filling the disk.
- **Never fatal.** Every write failure is swallowed and latched; the app
  mentions it once in the Log panel and then stops trying. A diagnostic that can
  take the console down is worse than no diagnostic.
- **Never per UI frame.** This is the constraint the design is built around:
  `fid_frame_overlay` and the Live pumps run at frame rate. Records come from
  state changes and explicit actions. The one value that *can* move every frame
  — the placed design's machine-mm bbox — is guarded twice: by the placement
  snapshot (which also skips the vertex sweep) and by a 0.05 mm epsilon on the
  resulting box.
- **`check=N` on everything that follows one fiducial check.** The check, the
  overlay it produced and both halves of the export it fed are written from
  different code paths, frames or a CLI round-trip apart — the export readback
  only lands when the child process exits. Physical adjacency is impossible, so
  `grep check=7` provides it instead.
- **Three machine-mm numbers, chosen to isolate the legs.** The detected
  fiducial centroid, the affine bbox (the common prefix of overlay and export,
  so comparing it with the written file isolates the export leg), and the drawn
  outline's screen bbox back-projected through the same projection (so comparing
  it with the affine bbox isolates the projection leg).
- **The export is measured, not assumed.** Once the verb reports success the
  written `.lbrn2` is re-read and its geometry bbox logged, using the same
  vertex parser the tests assert with (`diag::lbrn2_verts`, lifted out of
  `app/tests.rs` rather than written twice). The record names its units:
  coordinates are COMMANDED mm when a field map was applied and physical mm
  otherwise — a distinction that is itself a candidate explanation.
- **The readback is armed only when `run_verb` actually started the job.** A
  refused click must not attribute an older file to it.
- **Failure lines are mirrored by index, not at the call sites.** `LogLine`s
  with `err: true` are pushed from ~50 places across every module; a per-frame
  sweep over the tail of `runtime.log` cannot be forgotten when a new error path
  is added. The cursor is adjusted alongside `pump_verb`'s 500-line trim, which
  shifts every index down.

## 2026-07-26 — ③ gains a third scale mode: correct the distortion, keep 1:1

A rig measured its burn 32.2% smaller than the paper ruler. Step ③ offered only
two answers, and both were wrong for it: refuse the fit, or absorb the scale
into the field polynomial. Absorbing rescales command space by `1/scale`, so a
90 mm work area needs 132 mm of command to cover — the operator loses a third of
the machine to make shapes measure right.

- **`allow_machine_scale: bool` becomes `calib::FieldScale`** with three
  variants: `Refuse` (unchanged default), `Compensate` (the old `true`), and
  the new `DistortionOnly`. The first two are behaviourally identical to before
  — `Refuse`'s gate, its error text's marker, and `Compensate`'s absorbed-scale
  arithmetic are untouched.
- **`DistortionOnly` divides the uniform scale out of the FIELD POLYNOMIAL'S FIT
  TARGETS, and nowhere else.** The measured physical positions are rescaled
  about the commanded lattice's centroid by `1/scale` before `fit_field` sees
  them, so the polynomial learns only the non-uniform component and comes out at
  unit magnification: commanding X mm asks for X mm plus the distortion
  correction. `FieldCal::scale` still reports the measured factor.
- **`paper_to_machine` stays `fit_rigid`.** This is the load-bearing constraint,
  not an implementation detail: that alignment is also `camera_px_to_physical`'s
  metric anchor. Fitting a similarity there would absorb the scale into the
  camera projection, and fiducial measurement, placement and the overlay would
  silently inherit the factor — a far worse bug than the one being fixed. The
  scale is therefore removed downstream of the frame alignment, on the fit
  targets alone.
- **The centroid is the fixed point** because `fit_rigid` maps the measured
  centroid exactly onto the commanded one, so the normalization introduces no
  translation. That assumes the grid is centred on the scan field; ③ already
  warns when it is well off the configured field centre.
- **Extrapolation is intended.** The map is queried across the whole configured
  work area, not just the burned grid's span, and is deliberately not clamped to
  it. A synthetic test sweeps 0–90 mm against a grid covering 15–75 mm and
  asserts the correction stays finite, strictly monotonic, and tracking the true
  radial inverse to ~1.5 mm. That bounds this synthetic pincushion at these
  points; it does not certify extrapolated accuracy on a real measured field.
- **What follows the fit vs. what follows the measurement.** `rms_um`, `max_um`
  and `FieldDot::resid_um` are evaluated in the frame the polynomial was fit in,
  so they always agree with each other and the overlay's dot colouring stays
  meaningful. `physical_mm`, `field_um`, `scale` and `to_px` stay on the RAW
  measurement — `to_px` especially, since the Place overlay shares the rigid
  anchor's true-mm frame.
- **The mis-size stays visible.** `DistortionOnly` does not correct the scale,
  so the ③ status line names the mode and prints the measured percentage for as
  long as that fit is active — not just in the one-shot fit note. The mode that
  produced the active fit is tracked separately from the pending control choice
  and persisted as a fourth `field_stats` token, so a restored calibration
  describes itself honestly.
- **Settings migration.** The retired bool `calib_allow_machine_scale` is still
  read (`true` → `Compensate`, `false`/absent → `Refuse`); the new
  `calib_field_scale` key (`refuse|compensate|distortion_only`) is what gets
  written, and wins when both are present.

## 2026-07-26 — Drag one ✛ onto its hole instead of remarking all four

Fixing a single missed fiducial meant restarting the click-in-order round or
clicking through every marker again. A primary drag that starts on a ✛ now
moves that marker and re-checks on release.

- **Priority: navigation → marker → design → mark.** Ctrl (pan/zoom) suppresses
  everything, as everywhere else. The MARKER hit test runs BEFORE the design's,
  because the design's grab handle is a coarse screen-space bounding box that
  almost always contains the markers — testing the design first would make a ✛
  ungrabbable whenever the outline is shown. Consequence, accepted and stated in
  the on-screen hint: a Shift+drag starting on a ✛ moves the marker rather than
  rotating the design.
- **The grab is latched for the gesture** (`FiducialState::marker_drag`), like
  `design_drag`: re-picking the nearest marker per frame lets a fast drag hop
  between them. Both latches are read through one `fid_marking_allowed()`, so
  the marking round, click-to-place and right-click-remove paths cannot drift
  apart — a drag that grabbed something never also drops a ✛ on release.
- **Only `search` moves; `layout` is never written.** The layout is the design
  nominal that `fit_board_pose` fits against and that `scale_from_design`
  measures the true px/mm from, so a dragged position leaking into it turns a
  1 mm correction over a 50 mm baseline into ~2% of scale error everywhere
  downstream (LR-17). A search marker says "look here"; the layout says "the
  hole is here by design".
- **Re-detect once on RELEASE, not per frame.** The ladder's third stage is a
  whole-frame candidate scan. The ladder tries the operator's markers first, so
  a good manual placement is kept — but only while it clears the ladder's
  `AUTO_RECOVER_BELOW` bar or beats the later stages outright; below that a
  projection seed or rectangle match may still win and move the ✛ set.
- **The latched index is bounds-checked**, as `fid_mark_click`'s is: a typed
  layout edit re-runs `sync_fid_markers` every frame and can shrink `search`
  under an in-flight drag.
- **Unverified end to end.** Canvas gestures are not accessible widgets, so the
  press/move/release path itself is not driven by any test; the hit test, the
  per-frame move, the suppression gate and the release re-check are covered as
  units.

## 2026-07-26 — One scuffed dot must not veto a whole ③ field calibration

A real fit came back 49/49 dots, RMS 310 µm, worst 2075 µm against 100/250 µm
limits — refused. That one dot carried 91% of the total squared residual; the
other 48 sat at ~92 µm RMS, comfortably inside the limit. `fit_laser_field` now
identifies outlying dots, refits without them, and reports what it excluded.

- **MAD, not the standard deviation.** `sigma = 1.4826 · median|r − median r|`.
  The SD is inflated by exactly the outliers being looked for, which is how a
  2 mm dot hides itself inside its own spread estimate.
- **The cut is `median + K·sigma`, floored.** `FieldDot::resid_um` is a
  Euclidean magnitude, not a signed zero-mean residual, so its distribution sits
  well away from zero; a literal `K·sigma` cut about zero lands at ~155 µm on
  the case above and eats healthy 160–200 µm dots. `FIELD_OUTLIER_K = 3.5`, the
  middle of the conventional 3–4 band.
- **`FIELD_OUTLIER_FLOOR_UM = 250` — nothing below it is ever rejected.** A
  near-perfect grid has a tiny sigma and would otherwise start discarding dots
  for being merely ordinary. The floor is the console's default worst-dot
  acceptance limit: a dot inside that limit can never be why a fit was refused,
  so there is nothing to gain by dropping it. The limits are not threaded into
  the fit on purpose — coupling by value means tightening acceptance raises the
  bar without also licensing the fit to discard more evidence.
- **`FIELD_OUTLIER_MAX_FRAC = 0.10`, and above it NOTHING is dropped.** More
  than a small share reading as outliers is a bad capture or a model that does
  not describe this field, not stray dots. The fit still returns so the operator
  has numbers to read, the existing gates refuse as before, and the note says
  the cap is why. Deleting an eighth of the evidence to make a laser
  calibration pass is the failure this whole feature could otherwise become.
- **A bad boundary corner SUSPENDS rejection entirely.** Not just "don't drop
  that corner": drop nothing at all. Deleting the corner's neighbours removes
  the constraints holding the polynomial away from it, the surface bends toward
  the bad corner and its residual falls back under the limit — the same defeat
  the four-corner gate exists to prevent, arriving by deleting the witnesses
  instead of the victim. A corner sits at the bi-cubic's maximum leverage, so
  this is not hypothetical: at 1.2 mm of corner error an earlier
  drop-the-neighbours version excluded 2 innocent dots and pulled the corner's
  residual from ~490 µm down to 258 µm, 8 µm from passing. A sweep from 0.8 to
  1.6 mm now asserts nothing is ever excluded and acceptance always fails.
  Note that `field_live_acceptance` checks corner PRESENCE in `dots`, and
  rejected dots stay in `dots` (the overlay draws them) — so what refuses a
  corner outlier is the residual gate, with the corner's error still in it, not
  the corner gate.
- **Rejection is not "not found".** `found` still counts every detected dot, so
  the ≥80% coverage gate is unaffected. Excluded dots stay in `dots` with
  `rejected = true` and keep a residual measured against the final map, which
  reads large — that is the point.
- **Only the field polynomial is refit.** `paper_to_machine`, the similarity and
  `scale` stay over all dots: one outlier perturbs them by a rigid/uniform-scale
  amount, and the polynomial absorbs constant and linear terms exactly, so the
  survivors' residuals are unchanged by it. Refitting them would only make the
  anchor that placement and the overlay share disagree with the dots it was
  measured from. `to_px` and the pincushion-vs-noise verdict DO move to the
  survivors — a 2 mm outlier reads as scatter and would have the verdict tell
  the operator "correction won't help" about a fit just accepted.
- **Two passes.** The cut comes from a robust spread, so the first pass already
  sees an uninflated sigma and the second finds nothing in practice. Unbounded
  iteration only adds ways for the set to erode one dot at a time until what is
  left agrees with itself.
- **Every failure path still returns a fit.** A refit that errors, or a cut over
  the cap, falls back to the all-dots fit with `rejected = 0` and a note saying
  so. Turning a marginal calibration into an `Err` would take away numbers the
  operator could previously read.
- **It is never silent.** `FieldCal::rejected` and `rejection_note` ride on the
  one-shot fit note, on the standing ③ status line (`N EXCLUDED as outliers`),
  in `debug_summary`, and in the fit-feedback overlay, where excluded dots are
  struck out with a magenta ✕ at double radius instead of being coloured by a
  residual they did not contribute to. A fit that passes only because a dot was
  thrown away has to look different from one that passes outright.
- **The count is persisted, the prose is not.** A fifth `field_stats` token
  carries `rejected` across a restart, for the same reason the fit mode is
  persisted: a restored calibration has to describe itself honestly, and
  "passed once a dot was thrown away" must not come back looking like a clean
  pass. The restored note says how many were excluded and to re-run ③ to see
  which — the per-dot residuals are per-fit feedback and are not stored.
- **Extrapolation is called out by name.** When an excluded dot also lies
  outside the region the ① lens calibration covered, the note says so: that
  combination is the signature of the metric ruler extrapolating rather than of
  the laser field curving, and it routes the operator to a larger paper grid or
  a smaller burn grid instead of to the machine.
- **① `fit_camera_lens` was deliberately left alone.** It has the same shape and
  plausibly the same vulnerability (the same session saw 74/81 dots there), but
  the ① fit IS the metric ruler everything downstream is measured against, so
  discarding evidence there has a wider blast radius and wants its own decision.

## 2026-07-26 — Fiducial detection on brushed metal: matched filter, response-domain SNR, joint selection

The operator's real surface is brushed, scratched, specular aluminium. Their
diagnostic log recorded **95 of 168 checks finding fewer than 3 of the 4 holes**.
`samples/fiducial/brushed-plate-4holes.png` is that frame, now committed as the
acceptance fixture. Sampled patches on it: a hole reads min 15 / max 173 / mean
115.1, and nearby "clean" plate reads min 6 / max 176 / mean 115.4. The mark and
its background are statistically indistinguishable, and the brush scratches carry
*more* contrast than the fiducials. Every part of the old detector keyed off raw
pixel statistics, so every part of it failed here.

- **A real disc-matched filter replaces the box mean.** The old "matched filter"
  was a box mean at `dot_px/4` — a blur, answering "is anything here dark?",
  which a 120 px scratch answers as loudly as a 2 mm hole. It is now centre mean
  minus surrounding-annulus mean at the mark's own size: a compact blob of the
  right size responds strongly, an elongated scratch does not, because the
  surround contains the scratch too and cancels the centre. Centre and surround
  are square approximations evaluated from one summed-area table, O(1) per pixel.
  True discs would need a per-radius kernel and buy nothing — the discriminator
  is the size and compactness of the support, not its outline.
- **The centre follows the smaller extent, the surround the larger.** A surround
  keyed to the smaller extent is filled ~45% by a 2 mm × 1 mm rectangle, which
  cancels the contrast being measured; that mis-sizing cost 1.4 px of centroid
  accuracy on the rect fixture. The search window grew to `search_px + surround`
  to match, so a candidate at the edge of its window still gets an unclipped
  surround.
- **SNR is measured on the filter response, not on raw pixels.** This is the
  point of the change: the response is what discriminates, so the response is
  what gets a noise floor. On this plate the raw MAD is set by brush texture
  (σ ≈ 19 grey levels where a hole's own contrast is ~63), and the old
  `MIN_SNR = 3.5` against it excluded real holes.
- **The gate is `MIN_RESPONSE_SNR = 3.0`, bracketed by measurement.** In the
  detector's own windows, seeded 5 px off truth, the four holes read 3.42 / 6.03
  / 5.38 / 6.03 — and at 3.0 they are the *only* candidates any of those four
  windows produces. Not one scratch clears the gate. That is the evidence: there
  is nothing between "every true hole" and "nothing else" to tune against. The
  floor is the top-left hole at 3.42, weakest because its window straddles a
  broad dark band that inflates the response MAD. The gate was not raised to
  chase that margin — it would be fitting one frame's noise and would start
  missing real holes.
- **`SCAN_THR_SIGMA` was decoupled from the gate.** It used to be `MIN_SNR * 0.5`,
  but it applies to raw-pixel σ in the whole-frame scan — a different noise
  domain. Leaving it derived would have let the response-domain retune silently
  move the scan's mask. It is now a standalone literal at its existing value, so
  the scan stays bit-for-bit what the bench-plate arrangement match was tuned to.
- **Thresholds are keyed to each candidate, not to the window's worst pixel.**
  The old component threshold was `bg + 0.4·(peak − bg)` where `peak` was the
  single most extreme pixel in the window — on scratched metal, essentially
  never the fiducial. Each candidate is now thresholded against its own local
  median and its own filter response.
- **Sites are chosen jointly, not independently.** `find_fiducials` now collects
  the top 5 candidates per site and picks the *combination* maximising summed
  match quality minus a span-relative penalty on the residual of a similarity
  fitted from the expected positions to the chosen points. Four independent
  per-site picks are four chances to lock onto a different scratch; one geometric
  decision is not, because a scratch has to sit where the layout says a mark
  should be in order to compete. The public signature is unchanged — `ui` and the
  CLI both call it.
- **The fit is inline, not `calib::fit_similarity`.** `calib` depends on
  `vision`, so calling it from here is a dependency cycle. It is the closed-form
  2D Umeyama fit, done in *pixel* space rather than mm: the bed map may embed a
  y-flip, and fitting through it would demand a reflection-aware similarity,
  whereas expected-px → found-px is near-identity.
- **Combinations are bounded and the fallback is real.** The product of the
  per-site candidate counts is accumulated with `checked_mul` (it overflows
  before any comparison could catch it) and capped at 20 000 — K=5 over 6 sites
  is 15 625, and 5⁷ is 78 125. Past the cap, selection falls back to
  consensus-offset: the largest cluster of candidate-minus-expected offsets wins,
  then each site takes the candidate best reconciling quality with that
  consensus. Same shape of answer as `calib::square_grid`'s lattice selection,
  and linear in the candidate count. No real fixture reaches that branch (they
  are all K=5 over ≤6 sites), so it is covered directly instead: two unit tests
  on hand-built sites and an eight-site end-to-end case at 5⁸ combinations.
  Agreement with the consensus is a HARD window there, not a penalty — scoring
  disagreement continuously let a far-outside candidate accumulate a penalty
  several times any candidate's score and invert the ranking, so the worst
  candidate would have won. That was found by testing the branch, not by
  reading it.
- **Joint selection never manufactures a miss.** It reorders preferences; it does
  not veto. A site with no candidate surviving the existing area/shape/aspect/
  distance gates still returns `NoCandidate` exactly as before, and the geometric
  term is a soft penalty with no hard reject at any threshold — it has to be,
  since the truth quad's own worst similarity residual (22.5 px) is about the
  whole search radius (22.3 px).
- **Below three sites, geometry is skipped.** A similarity has 4 degrees of
  freedom, so two point pairs fit it exactly and the residual is identically
  zero. With fewer than three live sites the per-site ranking is already the
  whole answer.
- **Distance became a prior, not the rule.** `min_by(dist)` is gone: candidates
  rank on match quality minus a penalty on `(dist / search_px)²`. The hard
  `dist <= search_px` bound stays — that is the search-window contract, not a
  heuristic.
- **`find_fiducial_candidates` deliberately does NOT share the new filter.** The
  code is shared and available, but the whole-frame pass's job is the opposite
  one: be permissive and hand the caller's arrangement matcher everything that
  could be a mark. Narrowing it to compact-blob responses would drop marks the
  arrangement could have vouched for, and its only validation is the bench-plate
  recovery test, which the change would silently re-tune.

### What this fixture is NOT

**The four holes are the corners of a 40 × 40 mm square on the plate, but they
are not a square in the image, and the acceptance test does not claim they are.**
Measured on the ground-truth centres: sides 450.8 / 476.1 / 435.1 / 418.3 px —
13.8% spread about the 446.3 px nominal — and diagonals 644.3 / 614.9 px, 4.8%
apart. Bottom longer than top with left longer than right is a coherent
perspective pattern, not measurement error. `samples/fiducial/README.md` already
records exactly this for the older bench fixture: "the plate is tilted relative
to the camera — its diagonals differ by ~9%, so the observed quad is genuinely
perspective-warped and no similarity fits it." The test therefore asserts
per-corner distance to the measured centres (the load-bearing check), each side
within 10% of nominal, side spread under 20%, and diagonals within 8% — and
explicitly not equal sides, which would be a false claim about this bench.

### Fixtures retuned, and why it is not gate-loosening

Two low-contrast tests rendered a dot at depth 6 over ±6 noise as a stand-in for
"too dim to see". A matched filter averages over a dot-sized region, cutting
uniform pixel noise by roughly √(area), so that dot is now genuinely recoverable
— it locks 1.5 px from truth. The fixtures were re-rendered dimmer (depth 1.5 in
`vision`, 0.6 in `ui`, where the noise model differs) so they still model absence
of signal. `dim_low_contrast_burn_is_now_found` asserted `snr < 5.0` against the
old raw-pixel gate; that comparison is now between two different quantities, so
the claim moved to where it still holds — the dim burn is found, at its true
centre, with a healthy score. `SNR_FULL` (the score's saturation point) dropped
from 10 to 6 for the same reason: response SNR runs lower than raw-pixel SNR for
the same mark, and keeping 10 would have shown the operator four amber "weak"
rows for four correct bench locks.

### Known limit, recorded rather than tuned away

A window seeded 22 px up into the broad dark brush band drops the top-left hole
to SNR 2.31 and the site reports `LowContrast`. That is the safe failure — a miss
naming the SNR, per the VIS-4 "low contrast is a lighting problem" rule, not a
confident lock on a scratch — and it is covered by
`brushed_plate_band_seeded_site_refuses_rather_than_locks_on_texture`. Separately,
a site seeded on nothing but scratches returns a weak detection (snr 3.11, score
0.185) rather than an outright miss. It sits under the console's `SCORE_OK` of
0.25 where all four real holes score 0.34–0.83, so it shows as an amber row. A
detector-side score floor would have suppressed it, but that would also take away
the amber rows the operator is meant to see and judge, and a gate placed to
exclude it would sit inside the 10% margin above the weakest true hole.

## 2026-07-26 — Live re-acquires a moved board: stage 3 throttled, not skipped

The fiducial ladder's third stage — the whole-frame rectangle match — used to be
skipped outright under ● Live (`best_hits < AUTO_RECOVER_BELOW && !live`). That
made a moved board unrecoverable without a manual Check, which is exactly the
moment the operator has their hands on the work and not on the console. Stage 3
now runs under Live too, gated on a cooldown instead of on `!live`.

### Why it was skipped, in numbers

The scan (`find_fiducial_candidates` plus the arrangement matcher) measures
171–190 ms in release on the 2592×1944 bench frames, and 1.3–4.5 s in a dev
build. The live loop's iteration is ~194 ms release (device ~8.7 fps). Running
the scan on every frame that comes up short therefore roughly HALVES the feed —
the original reason for the skip, and still a real constraint. Stages 1–2
(operator markers, projection seed) are cheap and keep running per frame.

### The two windows

`GLOBAL_RECOVER_COOLDOWN` is 1 s, applied after a scan that recovered holes: one
180 ms scan per second is ~18% of live time, dropping the feed from ~5 to
~4.2 fps while it re-acquires. That is the price of following a board that just
moved, and it is paid only while the board is actually lost.

`GLOBAL_RECOVER_BACKOFF` is 4 s, applied after a scan that found nothing — and
after the cheap early `Err` exits (bad scale, too few layout points, no frame)
that never reach the scan at all, since none of those change frame to frame. A
hopeless scene (board removed, lens cap on) must not burn 180 ms every second
forever; at 4 s it costs ~4.5% of live time. This backoff is also what keeps a
DEV build usable, where the same scan costs 1.3–4.5 s: one per 4 s is a painful
feed but a moving one, where per-frame would wedge the console. There is no
build-profile switch — one rule, tuned for the release console that is what the
operator actually runs.

The timestamp is stamped on ATTEMPT, before the outcome is known, then shortened
to the 1 s window if the scan improved things. Stamping only on success would
leave the hopeless case scanning every frame — the precise failure the backoff
exists to prevent.

### Consequences accepted

The scan runs on the UI thread, so when it fires it costs one visible hitch
(~180 ms release). Deliberately not moved to a background thread: the frame, the
ladder's mutable state and egui's context would all have to cross the boundary,
which is out of proportion to a sub-200 ms hitch at most once a second.

Streamed frames are the ONLY thing throttled. A manual Check pressed while Live
is on initially shared the feed's budget (both reached detection with just the
`live` flag to go on), which made the button sometimes do nothing and let a
failed Check suppress the feed's next scans — so detection now takes a
`streamed` parameter set only by the Live pump, and every explicit action
(Check, load, grab, the final marking click, a marker-drag release) scans
unconditionally. When the cooldown suppresses a streamed scan, the note carries
`rectangle match throttled under Live` — consistent with the existing rule that
the note says which stage located the holes and why the others did not. A successful recovery under Live behaves exactly as it does
for a Check: the matched markers are installed, detection re-runs from them, and
the note says `located via rectangle match (…)`. A failed one leaves the
operator's ✛ set untouched.

The cooldown decision is a free-standing predicate, `should_global_recover`
(same shape as `should_release_capture`), unit-tested for first-scan, in-window
suppression, the two differing windows, and a Check ignoring the cooldown
entirely; a second test drives the real ladder and asserts the second
consecutive short Live frame reports throttled rather than rescanning. The live
loop itself — real frames streaming off a device while the board moves — is not
testable headlessly and is not claimed to be.

## 2026-07-26 — The Live re-acquire cadence is an operator setting, not a constant

The stage-3 throttle shipped with two hard-coded windows: 1 s after a
whole-frame rectangle match that RECOVERED holes, 4 s after one that found
nothing. Both numbers were tuned on the bench and neither was reachable from the
console. The operator wants a board that keeps drifting followed more closely
than once a second, and asked for 500 ms.

So the pair collapses to one dial: `FiducialState::live_recover_s`, the
re-acquire interval in seconds, default **0.5**, exposed as a `re-acquire s`
DragValue beside the ● Live checkbox and persisted as `fid_live_recover_s`. The
failure window stays a fixed 4× multiple (`RECOVER_BACKOFF_FACTOR`), which
preserves the shipped 1 s : 4 s ratio rather than inventing a second dial nobody
asked for — the two windows were never independent, they were one cadence and a
"this is going nowhere" multiplier.

### Why the interval is clamped, in two places

0.1 ..= 10.0 s, enforced in the DragValue range AND on load in `settings_io`.
The load clamp is not belt-and-braces: the settings file is plain text an
operator can edit, and the ladder turns the value into a `Duration` with
`from_secs_f64`, which PANICS on a negative or NaN. A clamp only in the widget
would leave a hand-typed `-1` to take the console down on the next lost frame.
The ladder re-clamps at the stamp site too, for the same reason and at no cost.

The floor is also what bounds the real protection here. The backoff is what
keeps a hopeless scene — board removed, lens cap on, wrong layout — from burning
~180 ms every interval forever for a result that cannot change, and what keeps a
DEV build alive, where the same scan costs 1.3–4.5 s. A very low interval
weakens that, so 0.1 s is the floor and 0.4 s of backoff is the worst the
operator can dial in.

### Consequences accepted

At the 0.5 s default the scan costs about a third of live time while it is
re-acquiring, against ~18% at the old 1 s — a slower feed, deliberately traded
for following a moving board. That is the operator's call to make, which is the
point of surfacing it.

`should_global_recover` is untouched: it already read the window out of
`last_global_recover`'s `(Instant, Duration)` tuple, so making the window
configurable needed no change to the predicate at all. Its test now derives the
probe times from the windows instead of naming 2 s and 4 s outright, and asserts
the configured interval is what governs (0.2 s in → a 0.2 s success window and a
0.8 s failure window). The ladder-level test pins the interval at 10 s rather
than inheriting the default, so two back-to-back detection passes on a loaded
machine cannot walk past a sub-second window and rescan. The value appears in
`debug_summary()`'s `fiducials:` line, so a headless `state` dump shows it, and
a headless interaction test drives the field by its label and reads the change
back out of that line — presence alone would have passed on the caption.

## 2026-07-28 — LR-03 blocker list

LR-03 (back-side "Etch here" refused until `register` gains `--mirror-x`,
IMP-05) is not just a missing flag. Everything below is a dead path only
because the mirror is refused; each becomes a live burn-geometry hazard the
day `register --mirror-x` ships. Recorded so closing LR-03 does not happen
without closing these too.

- The mirror gate (`pose.flipped` vs the selected side) is degenerate for
  mirror-symmetric fiducial layouts — including the default 4-corner
  rectangle — where a physical flip is indistinguishable from a correspondence
  swap and fits as a pure ~1.023 scale that passes the 0.90–1.10
  `POSE_SCALE_MIN`/`MAX` gate. (Being fixed on this branch.)
- Stage-3 whole-frame recovery matches the raw layout under proper rotations
  ≤20° only — reflection-blind, so it fails outright on the back for
  asymmetric layouts and silently succeeds with swapped correspondences for
  symmetric ones. (Being fixed on this branch.)
- Unset focal length/board thickness silently absorbs the ~2.3% exit parallax
  (`entry_to_exit_mm`) into `pose.scale`, inside the 0.90–1.10 gate — an
  oversized back job that reads as a clean fit. (Being fixed on this branch.)
- `emit --mirror-x` without `--outline` corners each side on its own copper
  bounding box, so front and back land in unrelated frames. (Being fixed on
  this branch: mirror-x now requires `--outline`.)
- LR-15 (back-side AR overlay double-mirror) was deferred as "display-only" —
  that deferral is conditional on LR-03 staying closed. It is not independently
  safe.
- `exit_to_entry_mm` (the inverse of the parallax model) does not exist, so
  adopting a measured back-side layout as the new nominal stays front-only
  until it does.

## 2026-07-28 — Scan-center frame convention

`scan_center_mm` is a DESIGN-frame point; `scan_center_auto` is the
fiducial-layout centroid in that same frame. `cam::flip` already documents the
field as design-frame, but the console's hover text implied a measured machine
quantity, and the two must not drift apart.

The fit itself does not care: a wrong scan center is a constant offset in
source space that the similarity fit's translation term absorbs, so it never
shows up as residual. Downstream uses are not so forgiving — the parallax
model (`entry_to_exit_mm`) is applied about this point, so a design-frame
value fed a machine-frame number silently mis-shifts every back-side fiducial
expectation by the difference. One convention is recorded: design frame. The
console setting is persisted as of this branch. VIS-3 (field-center
calibration) will supply a measured value mapped into the design frame at that
point — not a second, competing frame.

## 2026-07-28 — Recovery cannot choose a branch

`recover --mark-done` followed `next` unconditionally at every stage,
including one with a `next_alt`. At `flip`, that meant MarkDone jumped a
double-sided board straight to `done`, skipping
`fiducials_bottom`/`bulk_bottom`/`iso_check_bottom` — recreating, through the
recovery door, exactly the LR-04 "silently skip the bottom side" scrap
scenario the strict `PCBFORGE_DOUBLE_SIDED` parse was built to prevent.

Fixed fail-closed: `recover --mark-done` now refuses at any stage that has a
`next_alt` rather than guessing which branch the operator meant.
