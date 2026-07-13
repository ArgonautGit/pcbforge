# PCBForge Agent Task Backlog — Ablation-Only Edition

Every software task in the project, packaged so each can be handed to a fresh AI coding agent (Claude Code or similar) as a self-contained prompt. Physical builds (pallet, enclosure, camera mounts, ducting) stay with you; where a task needs hardware in the loop, the prompt makes the agent produce a checklist *you* execute and report back.

## How to run this backlog

One task = one fresh agent session opened in `~/pcbforge`. Before starting a task: its **Depends** tasks are merged and `cargo test` is green. Paste the prompt verbatim. Every task ends the same way (this is baked into each prompt's conventions, but enforce it): commit as `"<ID>: <title>"`, tick the task in `BACKLOG.md`, and record any deviation or discovered constraint in `docs/decisions.md`. If a prompt's named sources don't exist yet (samples, captures, calib files), the agent must stop and say so — that's your cue to produce them, never the agent's cue to improvise. Agents never execute anything that can emit laser light; hardware "done when" items are checklists you run with the machine, results pasted back to the agent.

**Suggested execution order (maps to playbook gates):** INF-1 → ING-1/2 → GEO-1 → CAM-1..3 → EMIT-1..3 → *Gate P3* → VIS-1..6 → *Gate P4* → VIS-8/9 → *Gate P5* → ORC-1..4 → *Gate P6* → CAM-5, ORC-5/6, VIS-7(second camera reuse) → *Gate P7* → DRV-1..6 → *Gate P8* → CAM-6, ORC-7/8 → *Gate P9*. SIM-1 can start any time after GEO-1 and unlocks QA-2; everything marked *stretch* is post-P9.

**Tiers** (they set prompt length and agent effort): *trivial* = few-sentence prompt, one done-when; *standard* = full structure, 1–2 verification steps; *complex* = plan-first, checkpoints, retry budget.

---

## WS-INF — Repo & infrastructure

### INF-1 — Workspace scaffold + core types
Tier: trivial · Depends: — · Delivers: compiling workspace, `crates/core` types
```
Create the cargo workspace per docs/scaffold.md (I will paste the playbook §2.1
commands and core/src/lib.rs content into that file first — use it verbatim, do
not redesign the types). Run the cargo add commands exactly as written, commit
Cargo.lock. Done when `cargo build` and `cargo test` succeed at the root and
`git status` is clean. Do not add any dependency not listed.
```

### INF-2 — CI pipeline
Tier: trivial · Depends: INF-1 · Delivers: `.github/workflows/ci.yml` (or `ci/local.sh` if no remote)
```
Add CI running: cargo fmt --check, cargo clippy --all-targets -D warnings,
cargo test --workspace. Cache the cargo registry. Read the current GitHub Actions
Rust setup action's README before writing the workflow (actions churn — verify
action names/versions against their repos, don't write from memory). Done when
the workflow file lints (actionlint if available) and a local run of the same
three commands passes. Do not add release/publish jobs.
```

### INF-3 — Fixture manifest & validator
Tier: trivial · Depends: INF-1 · Delivers: `xtask fixtures` command
```
Add a workspace xtask (cargo xtask pattern — read an existing xtask example via
its README before writing) with `fixtures`: verify samples/kicad has ≥2 .kicad_pcb
projects, samples/lbrn2 has the 10 named schema samples from playbook §0.3, and
captures/ layout matches docs/capture-plan.md if present; emit samples/MANIFEST.toml
listing files + sha256. Done when `cargo xtask fixtures` exits 0 on the real repo
and nonzero when I temporarily rename a sample. Touch only the xtask crate.
```

### INF-4 — Golden-image test harness
Tier: standard · Depends: INF-1 · Delivers: `crates/testkit`
```
## Task
Create crates/testkit: (a) `rasterize(layer: &core::Layer, um_per_px: u32) -> GrayImage`
scanline fill honoring holes; (b) `assert_images_agree(a, b, min_fraction)` with a
diff-image dump to target/test-artifacts on failure; (c) a helper to shell out to
external rasterizers and load their PNG.

## Sources
- The image crate is pinned after you `cargo add image`; read its pinned docs for
  the GrayImage API rather than writing from memory.

## Constraints
- Test-only crate; no production crate may depend on it outside #[cfg(test)]/dev-deps.

## Done when
- `cargo test -p testkit` passes self-tests: a square rasterized at two scales
  agrees with itself; a deliberately shifted copy fails with a diff artifact written.
```

---

## WS-ING — Ingestion

### ING-1 — KiCad SVG → Layer
Tier: standard · Depends: INF-1, INF-4 · Delivers: `ingest::load_kicad_svg`
```
## Task
In crates/ingest implement `pub fn load_kicad_svg(path: &Path) -> Result<core::Layer>`:
filled polygons, integer nanometers, holes via fill rule.

## Sources (read first; no prior-knowledge APIs)
- usvg pinned in Cargo.toml: `cargo doc -p usvg --no-deps` before writing —
  its flattening API differs across versions.
- Real inputs: generate with
  `kicad-cli pcb export svg samples/kicad/<proj>.kicad_pcb --layers F.Cu
   --exclude-drawing-sheet --black-and-white -o /tmp/fcu.svg`
  (confirm every flag via `kicad-cli pcb export svg --help`). Support only the
  constructs that actually appear in these files.

## Constraints
Flatten transforms; curves→polylines ≤2 µm chord error. Touch only crates/ingest.

## Done when
`cargo test -p ingest` golden test: testkit::rasterize(parsed) at 25 µm/px agrees
≥99.5 % with a PNG render of the same SVG (rsvg-convert, or kicad-cli PNG export
if --help shows one), on both sample projects.

## If blocked
Unflattenable construct → stop and name it; do not approximate silently.
```

### ING-2 — Excellon drills
Tier: trivial · Depends: INF-1 · Delivers: `ingest::load_excellon`
```
In crates/ingest add `load_excellon(path) -> Result<Vec<(core::P, Nm)>>` (center,
diameter). Ground truth: files produced by `kicad-cli pcb export drill
samples/kicad/<proj>.kicad_pcb -o /tmp/drl/` (flags per --help) — parse exactly
that dialect, read the files before coding. Unit test: hole count + two
coordinates vs values I read from the KiCad GUI (I'll paste them). Touch only
crates/ingest; done when `cargo test -p ingest` passes; unknown format feature →
stop and ask.
```

### ING-3 — Gerber X2 ingest (robust path)
Tier: complex · Depends: ING-1 · Delivers: `ingest::load_gerber_x2`
```
## Task
Parse KiCad-exported Gerber X2 into core::Layer, preserving aperture attributes:
flag pads (.AperFunction), fiducials (.FiducialPad), and net names (.N) per polygon
group where present.

## Sources
- Real files first: `kicad-cli pcb export gerbers samples/kicad/<proj>.kicad_pcb
  -o /tmp/gbr/` (flags per --help). The dialect in these files is the spec you
  target — read several before designing.
- crates.io has gerber-types and a companion parser crate: evaluate whether the
  pinned versions parse the KiCad dialect by testing against the real files
  *before* committing to them; if they fall short, hand-roll a tolerant parser
  for the observed subset and say so in docs/decisions.md.
- The Ucamco Gerber spec PDF for X2 attribute semantics — cite section numbers in
  code comments for each attribute you interpret.

## Plan first
Post the construct inventory found in the real files + your parser strategy;
wait for OK.

## Constraints
Touch only crates/ingest. Output identical Layer geometry to ING-1 for the same
board (that's the cross-check).

## Done when
Golden test: X2-parsed layer vs SVG-parsed layer rasterize to ≥99.5 % agreement
on both samples; attribute test asserts a known pad and the fiducials are flagged.

## If blocked
Two attempts at any construct → document it as unsupported with a file:line
example instead of guessing.
```

### ING-4 — Net-ID source + net raster
Tier: standard · Depends: ING-1 (or ING-3) · Delivers: `ingest::net_raster`
```
## Task
Produce a per-net ID raster of a copper layer: `net_raster(board, um_per_px)
-> (IdImage, Vec<NetName>)`, for the clearance loop's short/open classification.

## Sources — investigate, then choose the simpler
(a) `kicad-cli --help` tree: any per-net or attribute-preserving export;
(b) X2 net attributes if ING-3 landed; (c) parsing the .kicad_pcb s-expression
netlist directly (read a real file first). Pick the least code; record the choice
and rejected options in docs/decisions.md.

## Done when
On a sample board, two chosen nets rasterize to distinct IDs, verified by
sampling three coordinates per net that I read from the KiCad GUI (I'll paste
them). Touch only crates/ingest.

## If blocked
If no source yields reliable net↔geometry mapping, stop with a comparison memo.
```

### ING-5 — Board metadata (.gbrjob)
Tier: trivial · Depends: ING-3 · Delivers: `ingest::load_gbrjob`
```
Parse the .gbrjob JSON emitted alongside gerbers (read a real one first) into
BoardMeta { size, copper_layers, thickness }. serde_json; unit test against the
real file's known values. Touch only crates/ingest.
```

### ING-6 — kicad-cli invoker
Tier: trivial · Depends: INF-1 · Delivers: `ingest::kicad_cli`
```
A subprocess wrapper for kicad-cli: discovers supported flags at runtime by
parsing `kicad-cli <subcmd> --help`, exposes export_svg/export_gerbers/export_drill
with our required options, and returns a clear error naming any missing flag.
Integration test runs against the installed kicad-cli on a sample project.
Touch only crates/ingest; do not shell out anywhere else in the workspace —
all callers must use this module.
```

---

## WS-GEO / WS-CAM — Geometry kernel & ablation CAM

### GEO-1 — Boolean/offset foundation
Tier: standard · Depends: INF-1 · Delivers: `cam::geom` wrappers + property tests
```
## Task
In crates/cam create geom.rs wrapping the pinned i-overlay (boolean union/xor/
difference) and cavalier_contours (offset in/out) behind our i64-nm Poly types,
with lossless nm↔f64 conversion at the boundary.

## Sources
`cargo doc -p i-overlay --no-deps` and `cargo doc -p cavalier_contours --no-deps`
plus each crate's examples/ directory — mirror their actual APIs; both are niche
enough that memory will be wrong.

## Done when
`cargo test -p cam` property tests (proptest): union idempotence; offset(+d)
then offset(−d) area within 0.5 % on random convex polys; difference of a poly
with itself is empty; all invariants hold at coordinates near ±1 m in nm (i64
headroom). Touch only crates/cam. If either crate can't express an op, stop and
propose the fallback before writing it.
```

### CAM-1 — Isolation + rub-out path generator
Tier: standard · Depends: GEO-1 · Delivers: `cam::ablation_paths`
```
## Task
`ablation_paths(layer, opts: &CamOpts) -> Paths` producing: ISOLATION = n_contours
offsets outside each copper boundary spaced spot_mm*0.7; RUBOUT = the band region
(dilate(copper, clearance) xor dilate(copper, clearance+band)) hatched at
interval_mm, one hatch set per pass angle (base + k*fill_angle_step), sets tagged
with their pass index.

## Sources
geom.rs from GEO-1 only — do not call i-overlay/cavalier_contours directly.

## Done when
Tests: contour count/spacing on a square analytic; annulus rub-out hatch length
within 1 % of analytic; property: no hatch midpoint outside the band; every pass
angle differs from its neighbor by exactly fill_angle_step. Touch only crates/cam.
```

### CAM-2 — Sliver force-clear
Tier: standard · Depends: GEO-1 · Delivers: `cam::force_clear`
```
`force_clear(region, min_feature_mm) -> Vec<Polyline>`: morphological opening
(offset in by min_feature/2 then out) subtracted from the region yields necks
thinner than min_feature; emit a centerline pass down each (inward-offset collapse
path or bounding-strip midline — your choice, test-satisfying). Read the pinned
cavalier_contours docs for the offset behavior on collapsing regions first.
Property test: random rectangles joined by random-width necks — every neck <
min_feature gets a polyline fully inside it, none elsewhere. Touch only crates/cam;
if the opening isn't expressible, stop and propose before coding.
```

### CAM-3 — Heat-aware ordering
Tier: standard · Depends: CAM-1 · Delivers: `cam::order`
```
Order segments/contours: nearest-neighbor within 10 mm grid cells, then
round-robin across cells so consecutive elements are spatially distant. Done when:
(a) total jump length ≤ 1/5 of naive order on a dense fixture; (b) mean distance
between consecutive centroids ≥ 5 mm on the same fixture; (c) every input element
appears exactly once (property test). Pure function, touch only crates/cam.
```

### CAM-4 — Pass planner
Tier: trivial · Depends: CAM-1 · Delivers: `cam::plan`
```
`plan(paths, params: &AblationParams, pp: &PassPlan) -> Vec<PassGroup>`: split
params.passes into groups of pp.group_size, attach the correct hatch-angle set
per pass, tag each group with a checkpoint marker. Unit test: 14 passes /
group_size 4 → groups of 4,4,4,2 with monotonically rotating angles. Touch only
crates/cam.
```

### CAM-5 — Dual-machine splitter
Tier: standard · Depends: CAM-1, CAM-2 · Delivers: `cam::split`
```
## Task
`split(layer, opts) -> { fiber: Paths, uv: Paths }`: fiber = removal region eroded
by guard_mm (default 0.15) at fiber params; uv = the guard band (final contours +
all force_clear output + one boundary contour at the exact design edge) at UV
params. Both in the shared board frame.

## Done when
Property test: minimum distance from any fiber element to any final copper
boundary ≥ guard_mm on random fixtures; visual fixture: an SVG debug dump
(write one) shows the two path sets in different colors for my eyeball check.
Touch only crates/cam.
```

### CAM-6 — Process compilers: mask-open / legend / stencil / drill map
Tier: standard · Depends: CAM-1, ING-2 · Delivers: `cam::process`
```
Four small compilers reusing ablation_paths: mask-open = pad apertures (from the
mask layer export) as filled hatches at UV params; legend = silkscreen layer as
low-power raster fills; stencil = paste layer as cut contours; drill-map = Excellon
centers as a JSON list for the drill guide (no laser output). Each gets a fixture
test asserting element counts vs the sample board's known values (I'll paste
counts from KiCad). Touch only crates/cam.
```

### CAM-7 — Design-rule checker
Tier: standard · Depends: GEO-1 · Delivers: `cam::drc`
```
`drc(layer, machine_floor_mm) -> Vec<Violation>`: minimum copper-to-copper gap and
minimum trace width below the active machine's floor are hard errors with
locations. Implement gap check via offset-based erosion (geom.rs); done when a
fixture with a known 0.1 mm gap is flagged at floor 0.15 and passes at floor 0.08,
and `pcbforge compile` (later) can refuse on violations. Touch only crates/cam.
```

### CAM-8 — Fiducial/tooling feature injector
Tier: trivial · Depends: CAM-1 · Delivers: `cam::features`
```
Prepend to any job: three annulus ops (1 mm disc, 1 mm cleared ring) at
(5,5),(5,H−5),(W−5,5) board-frame, plus optional tooling-hole center marks at the
two Ø3.02 positions. Unit test asserts coordinates for a 100×80 board. Touch only
crates/cam.
```

### CAM-9 — Tiling for the slide extension *(stretch)*
Tier: complex · Depends: CAM-1, VIS-6 · Delivers: `cam::tiles`
```
Split oversize jobs into ≤140 mm fields with 2 mm stitch overlap and a per-tile
re-registration request record. Plan first (tile graph + overlap dedup strategy),
then implement with a property test: union of tile geometry == original within
1 µm tolerance, and every stitched element appears in exactly one tile's
"authoritative" set. Touch only crates/cam. Note in docs/decisions.md that
execution requires ComMarker Studio for stage moves — this task is geometry only.
```

### CAM-10 — Board-outline cut pass (depaneling with focus stepping)
Tier: standard · Depends: GEO-1, ING-1(gerber path), ING-5, CAM-4 · Delivers: `cam::cut`, `pcbforge cut`, `PathKind::Cut`
```
## Task
Free the board from the stock: turn Edge.Cuts into a kerf-compensated,
tabbed through-cut job with a focus-step schedule. Read
docs/plans/cam-10-board-cut.md first and implement it — it is the design.

## The one physics constraint that shapes everything
The galvo's focal plane is fixed and its depth of focus is far shallower than
the 1.6 mm board, so THE FOCAL PLANE MUST BE LOWERED DURING THE CUT: group
passes into CutSteps that each remove ≤ z_step_mm, and after each step emit
the literal instruction "lower the head by X mm" (focus follows the cut
floor). Board thickness comes from the .gbrjob (ING-5), never assumed.

## Shape
cam::cut::{cut_paths(edge_cuts_region, &CutOpts) -> Paths,
schedule(&CutOpts, thickness_nm) -> CutSchedule}. Kerf compensation is one
geom::offset of the board region by +kerf/2 (winding puts the beam on the
waste side of perimeter AND cutouts). Tabs: tab_count gaps of tab_mm+kerf per
ring, evenly spread by arc length, nudged off corners. Interior rings before
perimeter; the cut job is always a board's final job. New
pcb_core::PathKind::Cut; CAM-3 must never interleave it, CAM-5 never routes
it to the UV set. CLI: per-focus-step SVG/DXF files + cut-schedule.txt for
the LightBurn workflow.

## Done when
The four property tests and the valdemo2 fixture/E2E tests in the plan's
"Done-when" section pass verbatim (kerf clearance ±1 µm; tab arithmetic
closes to ring length; schedule sums to thickness+overcut with every drop
≤ z_step_mm and final drop 0; interior-before-perimeter ordering). Defaults
print the run-the-calibration-ladder warning. Touch crates/{core,cam,cli}.
```

---

## WS-EMIT — Backends & CLI

### EMIT-1 — lbrn2 schema report
Tier: standard (analysis only) · Depends: fixtures in `samples/lbrn2/` · Delivers: `docs/lbrn2-schema.md`
```
## Task
Derive the LightBurn galvo .lbrn2 schema subset we need, by evidence only.

## Sources (the only ones permitted)
samples/lbrn2/*.lbrn2 — pairs differing in exactly one setting (power, speed,
frequency, interval, passes, fill angle, Line vs Fill, layer count), plus uv-*
variants. Diff each pair.

## Done when
docs/lbrn2-schema.md contains a table: our field → XML element/attribute →
evidence pair (filenames) → observed value encoding; plus the shape/vertex
encoding for a square, decoded and verified by predicting the coordinates in one
sample from its known geometry. Anything not observable is listed as UNKNOWN —
never inferred. No code in this task.
```

### EMIT-2 — lbrn2 emitter
Tier: standard · Depends: EMIT-1, CAM-4 · Delivers: `cam::write_lbrn2`
```
Implement write_lbrn2(groups, params, path) strictly from docs/lbrn2-schema.md —
if a needed mapping is UNKNOWN there, stop and name the extra sample I must save.
quick-xml or string building; write-only. Fixture test: emitted file contains the
mapped attributes for two layers with different params/passes/angles. Manual gate
(I run): LightBurn opens with zero warnings, preview scale correct — bake a 100 mm
ruler line into a --debug-ruler flag for that check. Touch only crates/cam.
```

### EMIT-3 — CLI surface
Tier: standard · Depends: EMIT-2, CAM-5..8, ING-6 · Delivers: `pcbforge compile|register|…` skeleton
```
## Task
In crates/cli (clap derive — read the pinned clap docs) wire:
  compile <pcb> --process ablate-top|ablate-bottom|mask-open|legend|stencil
      --clearance band:0.5|full --machine fiber|uv|split
      --preset <name> --fiducials --out-dir jobs/
Presets from docs/presets.toml (define the tiny schema; I fill values). Emits
job-g01.lbrn2… per pass-group (and jobs/fiber|uv subdirs for split). Refuses on
CAM-7 DRC violations unless --force.

## Done when
`pcbforge compile samples/kicad/<proj>.kicad_pcb --process ablate-top --machine
fiber --preset F0 --fiducials --out-dir /tmp/j` exits 0; every file opens in
LightBurn (I check); DRC refusal demonstrated on the violation fixture.
Touch only crates/cli. Don't invent preset values — error if missing.
```

---

## WS-SIM — Virtual board

### SIM-1 — Raster sim backend (v1)
Tier: standard · Depends: CAM-4 · Delivers: `drivers::sim`
```
A Marker backend that "marks" Paths into a binary raster at 10 µm/px with a
Gaussian spot of configurable diameter, honoring passes. Done when: golden test —
compiling and sim-marking the sample board produces a raster whose XOR against
testkit::rasterize(design + band geometry) is < 0.5 % disagreeing pixels; and the
backend implements the same trait signature the real drivers will (define
`trait Marker` in crates/drivers now, doc-commented). Touch crates/drivers only.
```

### SIM-2 — Heightmap sim + removal model (v2)
Tier: complex · Depends: SIM-1, VIS-9 · Delivers: depth sim + simulated camera
```
## Task
Upgrade sim to a µm heightmap: per-pass removal Δz(power, speed, interval) from a
model fitted to data/ladder-fits.toml (produced by the ladder wizard from real
boards — read its format there), Gaussian spot, plus a heat-noise term in densely
revisited cells. Provide a simulated camera render: COPPER where depth < 35 µm,
SUBSTRATE ≥ 35 µm, CHAR where a heat threshold tripped, with configurable noise.

## Plan first
Post the removal-model equation + fitting method against the real ladder data;
wait for OK.

## Done when
(a) sim ladder reproduces the real ladder's first-cleared cell within ±1 row;
(b) the full clearance loop (ORC-3) converges on the seeded-sliver fixture inside
CI using only this backend. Touch crates/drivers (+test wiring). Two failed fits →
report residuals, don't force it.
```

---

## WS-VIS — Vision & calibration

### VIS-1 — Capture module
Tier: trivial · Depends: INF-1 · Delivers: `vision::capture`
```
Frame capture from /dev/video* via the pinned opencv crate's videoio (read
`cargo doc -p opencv --no-deps` first — Rust naming differs from C++; don't
transliterate). Enumerate devices, select by index or path, grab at max supported
resolution, save PNG. Done when `pcbforge cam --list` and `pcbforge cam --grab
out.png` work on the real camera; if 4K isn't exposed, print available modes and
stop. Touch vision/cli.
```

### VIS-2 — Intrinsics calibration
Tier: standard · Depends: VIS-1 · Delivers: `pcbforge calib intrinsics`
```
Guided flow: ~20 shots of a printed 9×6 chessboard (I enter measured square size),
opencv calib3d calibrateCamera (read the pinned crate's calib3d docs first),
save calib/intrinsics-<machine>.json, print reprojection RMS. --check undistorts
a live frame. Done when RMS < 0.5 px on my capture set. Touch vision/cli.
```

### VIS-3 — Bed homography
Tier: standard · Depends: VIS-2 · Delivers: `pcbforge calib bed`
```
Printed dot grid of known pitch on the bed; detect ≥20 dots in the undistorted
frame; DLT homography + Levenberg–Marquardt refine via nalgebra (verify solver
signatures in the pinned nalgebra docs); save calib/bed-<machine>.json; print RMS
in µm. Done when live residual < 30 µm and a re-run after nudging the target
reports consistent mm coordinates for the same physical dots. Touch vision/cli.
```

### VIS-4 — Fiducial detectors
Tier: standard · Depends: VIS-3 · Delivers: `vision::find_fiducials`
```
`find_fiducials(frame, expected: &[P], search_mm, profile) -> Vec<(P, Confidence)>`
with profiles BACKLIT (bright blob), ANNULUS (bright disc scored by surrounding
tan-ring contrast), DARK_DOT (grid dots on anodized). Pipeline: threshold →
connected components → intensity-weighted centroid → paraboloid sub-pixel → bed mm.
Done when: synthetic test with rendered blobs + noise recovers centers < 0.15 px;
live: three burned annuli detected, positions shift consistently under a 1 mm
pallet nudge. Low contrast → print SNR and stop (lighting problem, not code).
Touch crates/vision.
```

### VIS-5 — Affine fit + residuals
Tier: trivial · Depends: INF-1 · Delivers: `vision::fit_affine`
```
Least-squares affine from point pairs: 2N×6 design matrix, SVD solve (verify the
pinned nalgebra svd/solve signatures), returning Matrix3 + per-point residuals.
Unit test: known affine + 3 µm noise on 5 points recovered < 5 µm RMS; degenerate
(collinear) input returns Err, not garbage. Touch crates/vision.
```

### VIS-6 — Burned-grid galvo calibration + register
Tier: standard · Depends: VIS-4, VIS-5, EMIT-3 · Delivers: `pcbforge calib grid`, `pcbforge register`
```
## Task
(1) calib grid --machine <m> --pitch 10 --n 11: detect the 121 burned dots
(DARK_DOT), associate to the commanded grid (coarse similarity fit → nearest
neighbor), fit full affine commanded→measured, save calib/galvo-<m>.json +
residual heat-map PNG.
(2) register --jobs jobs/ --board <pcb> --machine <m>: find the three annuli,
compose board × galvo affines, apply the inverse to all geometry in every
job-g*.lbrn2, re-emit as reg-g*.lbrn2; print residuals; nonzero exit > 20 µm RMS.

## Done when
Synthetic grid through a known affine recovers < 5 µm; live heat-map max < 40 µm;
register runs on a real jobs dir. A > 100 µm bowl-shaped residual is reported as
f-theta distortion (expected; TPS later), not chased as a bug. Touch
vision/cam/cli.
```

### VIS-7 — TPS field correction *(stretch)*
Tier: complex · Depends: VIS-6 · Delivers: thin-plate-spline fit + pre-warp
```
Replace/augment the grid affine with a thin-plate spline (implement the standard
radial-basis formulation; cite the reference you used in a comment), a pre-warp
applied to emitted geometry, and ≥5-fiducial board-level TPS fallback when affine
residuals are structured. Plan first (math + evaluation grid). Done when the live
grid residual max drops below 15 µm on the same physical card that measured 40 µm
under affine, and a synthetic barrel-distortion test recovers < 3 µm. Touch
vision/cam. Two failed live improvements → report residual maps and stop.
```

### VIS-8 — Clearance classifier
Tier: standard · Depends: VIS-3 · Delivers: `pcbforge classify`
```
## Task
Pixel classes COPPER/SUBSTRATE/CHAR over inspection polygons in the board-frame-
warped image (HSV thresholds + open/close morphology; thresholds in
calib/classes-<machine>.json). --learn-coupon samples the three coupon patches at
their calib/pallet.ron coordinates and fits thresholds; --check-coupon prints
per-class IoU vs the learned masks. Returns per-polygon coverage stats + residual
region polygons in mm.

## Done when
--check-coupon IoU ≥ 0.95 all classes across two sessions on different days (I
run both); per-cell cleared-fraction ranking of the Phase-1 ladder photo matches
my recorded loupe ranking. Unstable IoU → dump both masks and stop (lighting).
Touch vision/cli.
```

### VIS-9 — Ladder wizard
Tier: standard · Depends: VIS-8, EMIT-3, ORC-1 · Delivers: `pcbforge ladder`
```
Compile the 24-cell ladder (passes {4,6,8,10,12,14,16,20} × speed {0.75,1.0,1.25}
× preset) as pass-grouped jobs; after I burn them, capture + classify each cell;
write the first fully-cleared cell (100 % SUBSTRATE, CHAR < 2 %) +1 group margin
into the material table, and dump per-cell stats to data/ladder-fits.toml (format
documented for SIM-2). Done when the wizard's pick matches my logged eye reading
±1 row on the same physical ladder, and a second blank from the batch reproduces
the pick. Touch vision/orchestra/cli.
```

### VIS-10 — Board-frame warper
Tier: standard · Depends: VIS-4..6 · Delivers: `vision::to_board_frame`
```
Warp an undistorted frame into the design frame via the current board affine
(and homography): `to_board_frame(frame, calibs, board_affine, um_per_px)`.
Done when a burned annulus at a known board coordinate lands within 2 px of its
expected raster position in the warped image, tested live. Touch crates/vision.
```

### VIS-11 — AprilTag pallet ID
Tier: trivial · Depends: VIS-1 · Delivers: `vision::pallet_id`
```
Detect the pallet's tag36h11 and return its ID. First verify whether the pinned
opencv crate exposes aruco/AprilTag dictionaries (cargo doc); if not, cargo add
the apriltag crate and note it in docs/decisions.md. Done when the real pallet
tag reads correctly at working distance in 10/10 grabs. Touch crates/vision.
```

### VIS-12 — Red-pointer drift check
Tier: standard · Depends: VIS-3 · Delivers: `pcbforge calib drift`
```
Flow: I command the machine's red pointer to 4 stored positions (via a tiny
emitted pointer job or manual jog — the tool just prompts and captures); detect
the red dot (HSV red blob), compare to the positions recorded at last grid
calibration (stored in calib/galvo-<m>.json by VIS-6 — extend its schema), print
per-point drift, warn > 25 µm. Done on live hardware with a deliberate 0.5 mm
bump of the camera detected. Touch vision/cli.
```

### VIS-13 — Verification-cross measurement
Tier: trivial · Depends: VIS-6, VIS-10 · Delivers: `pcbforge verify-cross`
```
After register burns the four 2 mm crosses on label stock, capture and measure
each cross center vs commanded, print µm errors, append to runlog. Cross detector:
template match or two-line intersection — your pick, sub-pixel. Done when a live
run prints four numbers and the gate script can consume them as JSON. Touch
vision/cli.
```

---

## WS-ORC — Orchestration

### ORC-1 — SQLite persistence
Tier: trivial · Depends: INF-1 · Delivers: `orchestra::db`
```
rusqlite layer over the fixed schema in docs/schema.sql (I'll commit it verbatim
from the playbook — use as-is, no redesign): open/migrate, CRUD for pallet/board/
runlog/material. Done when `cargo test -p orchestra` round-trips each table and a
second open sees the data. Touch crates/orchestra.
```

### ORC-2 — Stage engine
Tier: standard · Depends: ORC-1, VIS-11 · Delivers: `pcbforge next`
```
DAG engine over stages.ron (RON via serde; docs/stages.ron committed verbatim from
the playbook): `pcbforge next` reads the pallet tag, loads the board's stage, runs
its executor, advances, persists. Stage kinds Laser/ClearanceLoop/Manual as trait
objects so executors land in later tasks (stub ClearanceLoop now). Done when a
test board walks fiducials→bulk_top→(stub)→iso_check→done across separate process
invocations with state surviving restarts, runlog rows per stage. Touch
orchestra/cli.
```

### ORC-3 — ClearanceLoop executor
Tier: complex · Depends: ORC-2, VIS-8, VIS-10, EMIT-2, CAM-1 · Delivers: the closed loop
```
## Task
Implement ClearanceLoop: capture → warp → classify the CAM inspection zones →
residual COPPER/CHAR regions → dilate 50 µm → compile corrective lbrn2 (preset
from material table) → register → prompt "press play" → iterate. Converge when all
zones are 100 % SUBSTRATE with CHAR < 2 %. Escalate after PassPlan.
max_corrective_iters: stop with an annotated overlay PNG circling survivors.
OPENS: any expected-COPPER zone classifying SUBSTRATE aborts immediately with
overlay. Every iteration logged (region count, areas, files emitted).

## Plan first
Post the loop state machine (states, transitions, persisted fields) and wait for OK.

## Done when
(a) full loop converges in CI on SIM-2's seeded-sliver fixture within 3 corrective
iterations (skip this criterion if SIM-2 not landed; then rely on b+c);
(b) unit tests for convergence math, escalation, and opens-abort on synthetic
classifier outputs; (c) live seeded-sliver board (I fabricate) converges ≤3
iterations hands-off except play — I report the result.

## Constraints / blocked
Touch orchestra/vision glue only. The loop never emits without a passing airflow
check (ORC-4). If classifier confidence is degenerate mid-loop, pause and surface
it — never loop blind.
```

### ORC-4 — Airflow interlock
Tier: trivial · Depends: INF-1 · Delivers: `orchestra::airflow`
```
Check per-machine sail switch via the AIR-<m> USB-serial dongle: assert RTS, read
CTS (verify the exact modem-line method names in the pinned serialport crate docs
before coding). Expose `require_airflow(machine) -> Result<()>` used by every
laser-emitting stage. Done when blocking the duct flips a live check from Ok to a
clear error naming the machine and dongle. Touch crates/orchestra.
```

### ORC-5 — Cross-machine handoff
Tier: standard · Depends: VIS-6 (both machines calibrated), CAM-5 · Delivers: `pcbforge handoff`
```
Re-find the three annuli with the destination machine's camera, fit, re-emit that
machine's job set as registered files; refuse > 20 µm RMS; log the transform.
Done when a live handoff prints residuals ≤ 20 µm and a verify-cross burned by the
second machine lands ≤ 50 µm from a cross burned by the first (I measure with
VIS-13). Touch orchestra/cli.
```

### ORC-6 — Double-sided flip flow
Tier: standard · Depends: ORC-2, VIS-4 · Delivers: flip stages
```
Stages gain flip support: after top-side stages, prompt the flip; bottom
registration uses the BACKLIT profile on the three margin holes with mirror-aware
expected coordinates (derive the mirror transform from the pin-line geometry in
docs/pallet.md — read it, don't assume). Done when a flipped board registers with
residual ≤ 20 µm and a bottom verify-cross lands ≤ 50 µm from its top-side twin
(through-hole pair I drill). Touch orchestra/vision.
```

### ORC-7 — Guided drilling
Tier: standard · Depends: VIS-10, CAM-6, ING-2 · Delivers: `pcbforge drill-guide`
```
AR flow: project Excellon centers on the live registered view, step largest-bit-
first; after each hole, re-image and confirm a dark hole within 150 µm of target
before advancing; archive a final overlay PNG. Done when a 20-hole board completes
with every hole confirmed (I drill). Touch vision/cli.
```

### ORC-8 — Mask-open inspection stage
Tier: trivial · Depends: VIS-8, CAM-6 · Delivers: mask inspect
```
A stage variant of classify: every mask opening must classify as clean COPPER
(mask class = whatever the coupon's mask patch teaches — extend --learn-coupon
with a fourth patch; I'll add the physical chip). Done when a live masked board
with one deliberately un-opened pad is flagged, exactly that pad. Touch
vision/orchestra.
```

---

## WS-UI — Operator console

### UI-1 — egui console skeleton
Tier: standard · Depends: ORC-2, VIS-1 · Delivers: `crates/ui`
```
eframe/egui app: live camera panel, board/stage status from SQLite, buttons that
shell the existing CLI verbs (no logic duplication — the CLI stays the API).
Read the pinned egui/eframe examples before writing (API churns). Done when the
app shows live video and the current board state, and "Next stage" invokes
`pcbforge next` streaming its output into a log pane. Touch crates/ui only.
```

### UI-2 — AR overlay
Tier: standard · Depends: UI-1, VIS-10 · Delivers: design-over-camera view
```
Project the registered design (traces + inspection zones + drill centers) onto the
live frame using the current transforms, toggleable layers, adjustable opacity.
Done when a burned annulus and its overlay ring coincide within ~2 px on screen
live, and misregistration (I rotate the board) is visibly obvious. Touch crates/ui.
```

### UI-3 — Wizard panels
Tier: standard · Depends: UI-1, VIS-2/3/6/9 · Delivers: calib + ladder UI
```
Wrap the four wizard CLIs (intrinsics, bed, grid, ladder) as guided panels with
image feedback per step and the residual heat-map rendered inline. No new logic —
panels drive the CLI/library calls. Done when a full grid calibration completes
from the UI alone. Touch crates/ui.
```

### UI-4 — Escalation viewer
Tier: trivial · Depends: ORC-3 · Delivers: defect review screen
```
Show ClearanceLoop escalation overlays with zoom, mark each region resolved/
accepted, write the decision to runlog. Done when an escalated live board's
regions can be reviewed and dispositioned from the UI. Touch crates/ui.
```

---

## WS-DRV — Native drivers by USB sniffing (the "unknown driver" method)

The reusable method, which DRV-1..5 instantiate for the B4 and DRV-7 scouts for the Omni X: **(1)** capture real traffic from the working vendor/LightBurn driver while varying exactly one parameter per session; **(2)** decode offline into a protocol document where every claim carries packet-level evidence; **(3)** build the driver against a mock transport that replays captures, requiring byte-exact regeneration before hardware is allowed; **(4)** bring up live in staged, human-supervised steps with kill-switch tests; **(5)** only then integrate. Agents do 2–5; you drive the laser during 1 and 4.

### DRV-1 — Capture campaign kit
Tier: standard · Depends: — · Delivers: `docs/capture-plan.md`, `tools/capture.sh`, `captures/MANIFEST.csv`
```
## Task
Produce the capture kit for sniffing the B4's USB protocol under Linux: (a)
docs/capture-plan.md — exact operator procedure; (b) tools/capture.sh wrapping
tshark to record one experiment per file with a manifest row.

## Sources
- `man usbmon` and the tshark man page on this machine — verify every command
  and capture-filter syntax against them, not memory (`modprobe usbmon`, listing
  usbmonX interfaces, filtering to the B4's bus/device from lsusb).
- The USB ID recorded in RUNLOG.md.

## The experiment matrix the plan must contain (one variable per capture)
00 enumeration only (plug in) · 01 LightBurn connect, idle 30 s (isolates
keepalive/status polling) · 02 red-pointer frame trace · 03 one 10 mm line, Line
mode, params recorded · 04 = 03 with power +10 % · 05 = 03 with speed ×2 ·
06 = 03 with frequency changed · 07 10 mm square Fill at known interval ·
08 = 07 interval ×2 · 09 = 07 passes = 2 · 10 = 07 fill angle 17° · 11 = 07 but
press STOP mid-job (finds the abort command) · 12 job at a +25 mm offset
(coordinate scaling evidence) · 13 disconnect.

## Done when
capture.sh records a dummy run on any USB device to prove the tooling; the plan
names exact filenames, the manifest schema (file, date, params...), and safety
notes (marks land on anodized card, lid closed). No driver code in this task.
```

### DRV-2 — Protocol decode
Tier: complex · Depends: DRV-1 executed by me (captures committed) · Delivers: `docs/jcz-protocol.md`, `captures/expected/`
```
## Task
Decode the B4's USB protocol from captures/*.pcapng into docs/jcz-protocol.md.

## Sources (authority order)
1. The captures + captures/MANIFEST.csv — ground truth. Use tshark JSON export
   (verify flags via man tshark) for analysis.
2. Public written protocol documentation from the Balor project and MeerK40t's
   galvo driver — as reference to label your findings, cited per claim.
   LICENSING: reference documentation only; do not copy or translate GPL source
   into this repo.

## Plan first
Post your analysis approach (differencing strategy across the matrix, how you'll
derive coordinate scaling from capture 12 vs 03, how you'll separate list payload
from control traffic using capture 01), wait for OK.

## Required contents
Endpoint map; framing/packet sizes; session lifecycle (open→configure→list→
execute→status→stop→close); a command table where EVERY row has: opcode, payload
layout, units/scaling hypothesis with the regression evidence (which captures,
which packet numbers), and confidence; the abort command (from capture 11); status
word meanings observed; open questions listed as UNKNOWN. Also emit
captures/expected/<NN>.bin — the exact list payloads a reimplementation must
reproduce per experiment.

## Done when
Coordinate scaling predicts the 25 mm offset of capture 12 from capture 03's
payload within 1 LSB; the power/speed/freq fields each explained by their
differing capture pair; every table row carries evidence pointers.

## If blocked
Any field unexplained after two differencing strategies → UNKNOWN with the raw
bytes shown; never fill by analogy to another controller.
```

### DRV-3 — Transport + replay harness
Tier: standard · Depends: DRV-2 · Delivers: `crates/driver-jcz` skeleton
```
crates/driver-jcz: a Transport trait (bulk write/read) with two impls — NusbTransport
(read the pinned nusb crate's docs and examples first; async) and MockTransport
replaying/asserting against captures/expected/. Implement message
encoding per docs/jcz-protocol.md ONLY (UNKNOWN fields are compile-time-visible
placeholders that error if reached). Done when a "regeneration test" passes: for
experiments 03, 07, 09, encoding the manifest's job params produces payloads
byte-identical to captures/expected (excluding fields DRV-2 marked
session-variable). Touch only crates/driver-jcz; no live USB in this task.
```

### DRV-4 — Driver core against mock
Tier: complex · Depends: DRV-3 · Delivers: full Marker impl (mock-verified)
```
Implement the Marker trait for JczUsb: configure params, stream a Paths job as list
buffers with correct delays (per protocol doc), start, poll status, stop/abort,
and a watchdog task that issues the abort on heartbeat loss. Plan first (task/
ownership model for the async watchdog vs streaming). Done when: full job
lifecycle passes against MockTransport including an injected mid-stream transport
error → abort issued (assert the abort bytes) → clean shutdown; and a mock kill
test (drop the driver mid-job) shows abort-on-Drop. Touch only crates/driver-jcz.
UNKNOWN protocol fields encountered on a needed path → stop and name the capture
experiment that would resolve them.
```

### DRV-5 — Live bring-up harness
Tier: complex, hardware-in-loop · Depends: DRV-4, enclosure interlock done · Delivers: `pcbforge drv-bringup`, `docs/bringup-checklist.md`
```
## Task
A staged bring-up tool + operator checklist. Stages, each gated behind --arm and
my explicit confirmation prompt: S1 enumerate + read status only (no emission
possible) · S2 red-pointer square trace (verify against protocol doc that pointer
frames carry no laser-enable) · S3 mark a 10 mm line at minimum power into the
beam-dump card, lid closed · S4 the DRV-2 experiment-07 square, compared to the
original LightBurn burn · S5 kill test: SIGKILL the process mid-S4 job; on
restart, read status to prove the controller stopped, and document what physically
happened.

## Constraints (hard)
Refuse to run S2+ unless ORC-4 airflow and the enclosure interlock check pass.
Every stage logs raw TX/RX to captures/bringup/. Nothing in this tool bypasses
the --arm gate, including in tests.

## Done when
docs/bringup-checklist.md exists; S1 runs live; S2–S5 have I-run-it checklists
with expected observations; after I execute them, the S4 mark is visually
identical to the LightBurn original and S5's post-restart status shows stopped.

## If blocked
Any live behavior contradicting docs/jcz-protocol.md → stop, log the packets,
and open a decode follow-up; two failed S-stage attempts → stop, never param-sweep
against live hardware.
```

### DRV-6 — Orchestra integration
Tier: standard · Depends: DRV-5 passed, ORC-3 · Delivers: hands-off fiber loop
```
Wire JczUsb as the FiberB4 backend: Laser and ClearanceLoop stages fire pass-groups
and correctives with no operator prompt when the machine config selects the native
driver; LightBurn path remains selectable. Arming still requires airflow +
interlock checks per emission. Done when the P6 seeded-sliver scenario reruns with
zero keyboard input after `pcbforge next` (I supervise, lid closed) and every
iteration appears in runlog. Touch orchestra + driver glue.
```

### DRV-7 — Seacad/Omni X protocol reconnaissance *(stretch, research)*
Tier: complex research · Depends: DRV-1 kit · Delivers: `docs/seacad-feasibility.md`
```
## Task
Feasibility memo — not a driver — for natively driving the Omni X (LightBurn's
"Seacad" device class): is a clean-room driver tractable?

## Method / sources
Reuse the DRV-1 kit: capture enumeration, idle, and experiments 03/07/11 with
LightBurn driving the Omni X. From the captures + `lsusb -v` descriptors: transport
class (vendor bulk vs HID vs CDC), framing regularity, any signs of
encryption/obfuscation (entropy of payloads across identical jobs), lifecycle
symmetry with the JCZ findings. Also survey public prior art on Seacad-class
controllers with URL+date citations, ≤12 months preferred; mark anything
unverified as unverified.

## Done when
The memo gives verdicts on: transport type, framing decodability, obfuscation
risk, estimated effort relative to the JCZ track, and a go/no-go recommendation
with the conditions that would flip it. No driver code; no speculative commands
sent to the machine beyond passive capture.
```

### DRV-8 — Correction-mesh pre-warp in the native path
Tier: standard · Depends: DRV-4, VIS-6 (or VIS-7) · Delivers: host-side field correction
```
Apply calib/galvo-<m>.json (affine, or TPS if present) as a pre-warp on all
coordinates entering the JczUsb encoder, so the controller only sees corrected
coordinates; add a --no-warp escape for calibration burns themselves (the grid
must be burned unwarped — enforce that calib grid sets it). Done when unit tests
show grid-job coordinates pass through identity while normal jobs are warped, and
a live re-run of calib grid through the native driver reproduces VIS-6's residual
within 5 µm. Touch driver-jcz + cam glue.
```

---

## WS-QA — Testing & quality

### QA-1 — Geometry property-test expansion
Tier: standard · Depends: GEO-1..CAM-5 · Delivers: hardened proptest suite
```
Extend proptest coverage: offset/boolean round-trips at extreme aspect ratios and
near-degenerate slivers; force_clear completeness on adversarial neck shapes
(S-curves, tapered); splitter guard-band invariant under random affines; ordering
permutation-completeness. Done when `cargo test --workspace` runs the new suites
green in < 3 min and each new property has a shrunken counterexample test proving
it *would* catch a seeded bug (temporarily break the code to demonstrate, then
revert — show both runs in your summary). Touch test code only.
```

### QA-2 — CI virtual-fab integration test
Tier: complex · Depends: SIM-2, ORC-3 · Delivers: `tests/virtual_fab.rs`
```
An integration test that "fabricates" every board in samples/kicad on the SIM-2
backend: compile → loop → assert convergence within the iteration budget, zero
opens, and final sim raster agrees with design ≥ 99.5 %. Plan first (fixture
runtime budget; parallelism). Done when it runs in CI < 5 min and a deliberately
mis-set passes_bulk (−4) still converges via correctives while passes_bulk (−12)
fails with the escalation artifact — both asserted. Touch tests only.
```

### QA-3 — AOI corpus & annotation tool
Tier: standard · Depends: VIS-8 · Delivers: `tools/annotate`, `data/aoi-corpus/`
```
A minimal image annotation flow (CLI or one egui screen) to label real board
photos with COPPER/SUBSTRATE/CHAR polygons, stored as JSON beside the image;
plus a regression test that runs the classifier over the corpus and fails if
per-class IoU drops > 2 % from the committed baseline. Done when the two photos I
provide are annotated and the baseline is committed. Touch tools + tests.
```

### QA-4 — Nightly hardware-in-loop script
Tier: standard · Depends: VIS-6, VIS-13 · Delivers: `tools/hil-nightly.sh`
```
A script (I trigger it; nothing autonomous) that walks me through: grid burn →
calib grid → verify-cross → appends the residual numbers to data/hil-history.csv
and fails loudly if the error budget (heat-map max < 40 µm, crosses < 50 µm) is
exceeded or has drifted > 20 % vs the 7-run median. Done when a dry run against
last session's saved images produces the CSV row and both failure modes trigger on
doctored inputs. Touch tools only.
```

### QA-5 — Seeded-defect fixture generator
Tier: standard · Depends: ING-1, GEO-1 · Delivers: `xtask seed-defect`
```
Given a board and a defect spec, emit modified artwork: --sliver w=30um between
<netA> <netB> at a chosen channel, or --thin trace to below floor (opens test).
Done when the generated artwork rasterizes with exactly the intended defect
(golden check) and feeds both the ORC-3 live gate and QA-2. Touch xtask.
```

---

## WS-RES — Research tasks (agents with web access; citation-first)

### RES-1 — Crate due diligence
Tier: standard research · Delivers: `docs/research/crates.md`
```
## Task
Verify our load-bearing crate choices before deep dependence: i-overlay,
cavalier_contours, nusb, opencv (Rust bindings), usvg, serialport, egui.

## Criteria (verdict each, per crate)
1. Maintenance pulse (last release/commit) 2. API stability signals 3. Does it
actually cover our use (booleans on polygons-with-holes; offsets incl. collapse
behavior; async bulk USB; videoio+calib3d+aruco exposure; SVG flatten; modem
control lines; galvo-scale immediate-mode UI) 4. Known blocking issues.

## Sources
docs.rs, the crates' repos/issue trackers, release notes — prefer the last 6
months; cite URL + date per load-bearing claim; anything unverifiable is listed
as unverified, not asserted.

## Done when
Each crate has verdicts on all four criteria plus a named fallback, and any
red flag maps to the task ID it would affect.
```

### RES-2 — LightBurn automation surface
Tier: standard research · Delivers: `docs/research/lightburn-automation.md`
```
Determine what job automation LightBurn's current version offers on Linux for
galvo devices: CLI file loading/queueing flags, watch-folder behavior, anything
that could remove the "press play" prompt before DRV-6 lands. Criteria: exists on
Linux; works for galvo devices; stability across updates. Sources: current
LightBurn docs/changelogs/forum posts ≤ 6 months, URL + date per claim; unverified
items flagged. Done when each criterion has a verdict and a recommendation with
flip conditions.
```

### RES-3 — JCZ protocol public documentation survey
Tier: standard research · Depends: — (feeds DRV-2) · Delivers: `docs/research/jcz-prior-art.md`
```
Collect the public written documentation of the EZCAD2/BJJCZ galvo USB protocol:
the Balor project's protocol notes, MeerK40t galvo driver documentation, and any
independent write-ups. For each: what it documents (commands, correction handling,
timing params), its license, and what may safely inform a clean-room
implementation vs what must not be copied. URL + date per source; explicitly
separate "protocol facts asserted" from "code we must not derive from". Done when
DRV-2 can cite this file's inventory instead of searching mid-task.
```

### RES-4 — Consumables & floor benchmarks *(optional)*
Tier: standard research · Delivers: `docs/research/ablation-benchmarks.md`
```
Survey recent (≤ 24 months) documented results for fiber-laser copper isolation
on FR4 and UV finishing quality: reported minimum trace/space, passes for 1 oz
clearance, char mitigation practices. Cite URL + date per claim; mark forum
anecdotes as such. Verdict: does anything contradict our 8/8 → 6/6 → 4/4 floor
plan or suggest parameter starting points better than PRESET-F0? Do not treat any
single anecdote as authoritative.
```

---

## Backlog checklist (copy into BACKLOG.md)

INF-1..4 · ING-1..6 · GEO-1 · CAM-1..10 · EMIT-1..3 · SIM-1..2 · VIS-1..13 · ORC-1..8 · UI-1..4 · DRV-1..8 · QA-1..5 · RES-1..4 — 54 tasks (CAM-10 operator-added 2026-07-13). Stretch-tagged: CAM-9, VIS-7, DRV-7, RES-4. Human-executed but agent-prepared: DRV-1's captures, DRV-5's S2–S5, QA-4's burns, every "live" done-when.
