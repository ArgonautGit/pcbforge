# Field notes — live-burn findings, quirks, and future work

Forward-looking knowledge base from the first real burns (2026-07-13/14,
operator's BSLFiber MOPA + LightBurn Pro 2.1.03, KiCad 10.0.3). The
chronological forensics live in `docs/decisions.md`; this file is what a
future bug-fix or feature session needs to know, organized by component.
Update it whenever a live burn teaches something.

## Verified by live burns (trust these)

- **lbrn2 `VertID`/`PrimID` are list identifiers and must be unique per
  shape.** Reused IDs cross-link vertex lists; each ring's closing segment
  runs to the shared list's vertex 0 (burned as a fan of rays from the board
  corner). Emitter assigns monotonically increasing IDs; single-shape files
  keep ID 0 (byte-matches the pentagon sample).
- **KiCad Gerber frame is y-up, unmirrored, offset fully negative** (KiCad
  negates its internal y-down sheet coordinate). Correct normalization is
  translation only — a y-flip *introduces* a mirror. Pinned by the
  asymmetric-triangle test in `cam::lbrn2`.
- **`type="Scan"` = Fill, `type="Cut"` = Line; frequency in Hz; QPulseWidth
  integer ns; omitted element = default.** Full map in `docs/lbrn2-schema.md`,
  each field pinned to the sample that proves it.
- **No-net copper zones arrive as G36 regions tagged
  `%TA.AperFunction,NonConductor*%`** (KiCad 10). They are real, often
  intentional copper (the operator's isolated ground pour). Default keeps
  them; `--clear-nonconductor` rubs them out.
- **KiCad 10 emits adjacent vertices 1–3 nm apart** in region contours.
  Anything feeding cavalier_contours must collapse sub-25 nm steps first
  (`geom::DEDUPE_NM`) or it panics ("repeat position vertexes").
- The full-field rub-out profile burns clean at 20% / 1000 mm/s / 30 kHz /
  2 ns / 3 passes / 0.03 interval on the operator's rig (third burn, waffle
  texture = cleanly ablated FR4).

## Machine / operator facts

- **MOPA fluence knobs are Q-pulse width + frequency**, not Max Power % (the
  power field sits fixed at 20 and is greyed in the operator's Cut Settings
  Editor). Peak power ≈ P_avg / (frequency × pulse_width). Operator runs
  pulse widths 1–5 ns.
- Device name is exactly `BSLFiber`; the root `DeviceName` must match a
  configured LightBurn device or LightBurn prompts on open.
- The operator's Cut Settings Editor screenshot shows LightBurn has **native
  Kerf offset (mm) and Tabs/Bridges** (tab size / spacing / tabs-per-shape)
  on Line layers, plus **Perforation Mode**. CAM-10's own kerf/tab logic
  could optionally delegate to these for the board cut (see FLD-5).
- LightBurn on Linux ended at 1.7.08; operator is on Windows Pro 2.1.03.
  Sub-layers exist (one `1 Line` sub-layer observed); a disabled sub-layer
  greys its process fields — early "locked power" confusion was actually the
  fixed MOPA power field.

## Known gaps and quirks (ranked, with repro)

1. **Open-path `PrimList` = `Line` is still inferred, not observed.** Every
   sample path is closed. First live Line-mode job (isolation contours,
   board cut) must verify it; if wrong, the evidence needed is one LightBurn
   file containing a drawn *open* polyline. (`cam::lbrn2` module docs flag
   this.)
2. **Multi-Path ID convention is inferred from failure evidence only.** The
   unique-ID fix works (fourth job renders correctly in preview), but a
   LightBurn-authored file with TWO drawn polylines would confirm the
   canonical numbering (per-shape? per-type pool? gaps allowed?).
3. **Caught-panic spam on stderr.** cavalier panics are absorbed by
   catch_unwind but the default panic hook still prints backtraces during
   `emit`/`noncopper` on KiCad 10 boards. Cosmetic; fix is a scoped panic
   hook around the offset ladder (or upstreaming the fixes). Repro:
   `pcbforge emit --copper crates/cli/tests/fixtures/uv_test-F_Cu.gbr
   --outline crates/cli/tests/fixtures/uv_test-Edge_Cuts.gbr --offset-mm
   0.025 --lbrn2 /tmp/x.lbrn2`.
4. **cavalier_contours 0.7.0 upstream issues we work around:** panic on
   collapsing offsets (#79), rotation-dependent results, over-pruned slices,
   panic on sub-fuzz duplicate vertices, and the empty-result-validates-
   against-broken-reference trap. All defused in `cam::geom` (retry ladder,
   winding reference, DEDUPE_NM, collapse-plausibility guard, un-offset
   last-resort). Any cavalier version bump must re-run `geom::tests` +
   `emit_e2e::nonconductor_pour_is_kept_by_default` (offset 0.025 leg).
5. **`Ellipse` shape encoding is recorded but unimplemented** (`Rx`/`Ry`,
   center in XForm — from the path-shape sample). Emitter polygonizes
   circles instead; fine for now, native ellipses would shrink files.
6. **`anglePerPass` (two-layer sample, C01) is parsed into the schema but
   never emitted.** CAM's per-pass hatch rotation currently expects one
   angle per pass group; wiring `fill_angle_step_deg` → `anglePerPass`
   would let LightBurn rotate within a single layer.
7. **Emitted job always lands at workspace origin.** Fine for preview +
   drag; a `--origin-x/--origin-y` (or `--center`) placement flag would
   remove the manual move for repeat runs on a registered pallet.
8. **Fill grouping assumption:** nested rings on one Fill layer rely on
   LightBurn's "fill all shapes at once" grouping to resolve holes/islands.
   The operator's burns confirm their config does this; a job emitted for a
   differently-configured machine could fill holes solid. Consider emitting
   explicit per-shape grouping metadata if the schema for it is ever
   sampled.
9. **Probe-tested behaviors** (`emit_e2e::in_fill` ray-caster): pour interior
   kept at offsets 0 and 0.025, isolation channel + edge margin always fill,
   pad centers never fill, no negative coordinates, unique VertIDs. Extend
   these probes when adding geometry features — they catch what renders
   can't be diffed for.

## Reproduction toolkit

- Real-board fixtures: `crates/cli/tests/fixtures/uv_test-{F_Cu,Edge_Cuts}.gbr`
  (KiCad 10, pour + stutter vertices), `demo-*.gbr` (hand-authored KiCad 9
  style), `samples/kicad/valdemo{,2}.kicad_pcb` (kicad-cli-loadable), 11
  operator `.lbrn2` samples in `samples/lbrn2/`.
- Render-what-will-burn: extract `VertList`s, build one even-odd SVG path
  per ring, y-flip about the board height, rasterize (the review renders in
  decisions.md were produced this way; ~15-line python or reuse
  `cam::export::write_svg` on the shapes before emission).
- The KiCad-gated tests self-skip without kicad-cli; `apt install
  --no-install-recommends kicad` provides 7.0.11 in this container. Note the
  operator runs KiCad **10** — dialect differences (NonConductor zones,
  named %TD%, \u escapes, nm stutter) only show up in their real exports,
  so prefer the committed uv_test fixtures for parser regressions.

## Follow-up backlog (FLD)

Tracked in BACKLOG.md under "WS-FLD — Field follow-ups":

- FLD-1: verify open-path `Line` primitive on first live Line-mode job (or
  one open-polyline sample).
- FLD-2: two-polyline LightBurn sample → confirm multi-shape ID convention.
- FLD-3: silence caught-panic stderr spam (scoped hook around the offset
  ladder).
- FLD-4: emit `anglePerPass` from `fill_angle_step_deg`.
- FLD-5: board-cut job → optionally delegate kerf/tabs/perforation to
  LightBurn's native Line-layer features (operator screenshot proves the
  fields exist; needs one saved sample with tabs+kerf set to derive the
  schema).
- FLD-6: `--origin-x/--origin-y/--center` job placement flags.
- FLD-7: `uv-base.lbrn2` when a UV device profile exists → UV schema delta.
- FLD-8: report cavalier findings upstream (dedupe need, #79 interaction,
  empty-validation trap) / evaluate newer cavalier_contours.
