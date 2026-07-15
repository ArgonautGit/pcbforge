# PCBForge Backlog Checklist

62 tasks (CAM-10 operator-added 2026-07-13; FLD-1..8 field follow-ups added 2026-07-14). Stretch-tagged: CAM-9, VIS-7, DRV-7, RES-4. Human-executed but
agent-prepared: DRV-1's captures, DRV-5's S2–S5, QA-4's burns, every "live"
done-when. Task prompts live in `docs/backlog.md`.

## WS-INF — Repo & infrastructure
- [x] INF-1 — Workspace scaffold + core types *(scaffold.md agent-authored per operator direction — see decisions.md)*
- [x] INF-2 — CI pipeline
- [x] INF-3 — Fixture manifest & validator *(complete; `cargo xtask fixtures` now exits 0 on the real repo — 2 kicad + 10 lbrn2 samples present)*
- [x] INF-4 — Golden-image test harness

## WS-ING — Ingestion
- [x] ING-1 — KiCad SVG → Layer
- [x] ING-2 — Excellon drills *(ground truth from authored board source; dual API for G85 slots — see module docs)*
- [x] ING-3 — Gerber X2 ingest (robust path) *(load_gerber_x2/AttributedLayer preserves .N/.P/.AperFunction; geometry identical to plain parse, which is golden-validated vs KiCad — see decisions.md)*
- [x] ING-4 — Net-ID source + net raster *(ingest::net_raster from X2 .N attributes; VCC/GND distinct IDs on valdemo2 — see decisions.md for the net-source decision)*
- [x] ING-5 — Board metadata (.gbrjob)
- [x] ING-6 — kicad-cli invoker

## WS-GEO / WS-CAM — Geometry kernel & ablation CAM
- [x] GEO-1 — Boolean/offset foundation
- [x] CAM-1 — Isolation + rub-out path generator
- [x] CAM-2 — Sliver force-clear
- [x] CAM-3 — Heat-aware ordering
- [x] CAM-4 — Pass planner
- [x] CAM-5 — Dual-machine splitter
- [x] CAM-6 — Process compilers: mask-open / legend / stencil / drill map *(fixture counts from authored board source — see test header)*
- [x] CAM-7 — Design-rule checker
- [x] CAM-8 — Fiducial/tooling feature injector
- [x] CAM-9 — Tiling for the slide extension *(stretch; cam::tiles — centroid-ownership partition, geometry only; stage moves need ComMarker Studio — see decisions.md)*
- [x] CAM-10 — Board-outline cut pass (depaneling with focus stepping) *(operator-added; cam::cut + `pcbforge cut`; plan in docs/plans/cam-10-board-cut.md)*

## WS-EMIT — Backends & CLI
- [x] EMIT-1 — lbrn2 schema report *(docs/lbrn2-schema.md, evidence-derived from 10 operator samples; Path-shape format pending one sample for EMIT-2)*
- [x] EMIT-2 — lbrn2 emitter *(cam::lbrn2; Path/CutSetting encoding golden-checked byte-for-byte against the operator's samples; open-path `Line` primitive is the one inferred field — see decisions.md)*
- [x] EMIT-3 — CLI surface *(`pcbforge emit`: copper Gerber → non-copper Fill layer .lbrn2 with the full process recipe as flags)*

## WS-SIM — Virtual board
- [x] SIM-1 — Raster sim backend (v1)
- [ ] SIM-2 — Heightmap sim + removal model (v2)

## WS-VIS — Vision & calibration
- [~] VIS-1 — Capture module *(the `capture` crate: a File source (any capture app writing frames to disk — verified) + a real webcam backend via nokhwa behind the `camera` feature (compiles all platforms here; runs on the operator's machine). Consumed by both the console live preview and the CLI `pcbforge cam --list/--grab` (FLD-13). The opencv path per the original spec is the one piece still pending — see decisions.md)*
- [~] VIS-2 — Intrinsics calibration *(camera lens-distortion model: `vision::fit_lens` fits a bi-cubic 2-D polynomial camera-px↔true-mm from an imaged known-pitch grid — captures barrel/pincushion a homography can't (synthetic 4% barrel: <30 µm RMS vs >300 µm for a homography). Console **Calibrate → Camera lens** step: print a grid, tape it, image, click 4 corners, Fit; rich visual feedback draws the per-dot distortion field (arrows) + correction-quality rings. `dump_lens` renders the classic radial barrel field. Full OpenCV-style multi-pose intrinsics not needed for the fixed planar bed — see decisions.md)*
- [~] VIS-3 — Bed homography *(console camera→laser calibration: `pcbforge calib-grid` emits an n×n dot grid at known commanded coords; the operator burns + images it, clicks the 4 corners, and `ui::calib::fit_camera_to_machine` fits a camera-px→commanded-mm homography (perspective, absorbs a tilted camera) — reusing VIS-4 dark-dot detection + `fit_homography`. The Place tab then places/etches in true machine coordinates. Session-scoped (camera moves). The printed-grid `pcbforge calib bed` variant + LM refine per the original spec still pending — see decisions.md)*
- [x] VIS-4 — Fiducial detectors *(vision::find_fiducials; synthetic done-when met (<0.15 px); px↔mm parameterized as BedMap pending VIS-3; live annuli check operator-side — see decisions.md)*
- [x] VIS-5 — Affine fit + residuals
- [~] VIS-6 — Burned-grid galvo calibration + register *(register **software half done**: `pcbforge register` + cam::register — fit design→machine affine from fiducial correspondences (explicit or --frame-detected) and bake it into the emitted .lbrn2; RMS gate; frame contract documented. Galvo `calib grid` (burn 121 dots) + live ≤20µm residual are hardware-gated — see decisions.md)*
- [ ] VIS-7 — TPS field correction *(stretch)*
- [ ] VIS-8 — Clearance classifier
- [ ] VIS-9 — Ladder wizard
- [x] VIS-10 — Board-frame warper *(vision::to_board_frame + board_mm_to_raster; inverse-warp bilinear gather, design-mm↔raster; synthetic done-when met (<2 px); live check gated on VIS-6 — see decisions.md)*
- [ ] VIS-11 — AprilTag pallet ID
- [ ] VIS-12 — Red-pointer drift check
- [ ] VIS-13 — Verification-cross measurement

## WS-ORC — Orchestration
- [x] ORC-1 — SQLite persistence *(schema.sql agent-authored per operator direction — see decisions.md)*
- [x] ORC-2 — Stage engine *(pallet-tag read stubbed pending VIS-11; stages.ron agent-authored)*
- [ ] ORC-3 — ClearanceLoop executor
- [x] ORC-4 — Airflow interlock *(live block-the-duct gate deferred to docs/checklists/orc-4-airflow-live.md)*
- [ ] ORC-5 — Cross-machine handoff
- [~] ORC-6 — Double-sided flip flow *(software half done: `cam::flip` (mirror-X + f-theta beam entry→exit parallax `r·(1+t/f)`), `pcbforge emit --mirror-x`, console **Front/Back** selector (back gerbers, thickness/focal, scan-center auto/override; mirrored job in preview/AR/Place; fiducial markers carry mirror+offset), and the **stage engine branch**: `StageKind::Flip` + `next_alt` — single-sided boards pass `flip`→`done`, double-sided (`PCBFORGE_DOUBLE_SIDED=1` bring-up signal) branch through `fiducials_bottom`→`bulk_bottom`→`iso_check_bottom`, walk-tested across restarts. The live ≤20 µm / bottom-cross ≤50 µm done-when is hardware-gated; scan-center default = fiducial centroid pending VIS-3 — see decisions.md)*
- [~] ORC-7 — Guided drilling *(software half done: `pcbforge drill-guide` — Excellon holes ordered largest-bit-first (slots drill both ends), one invocation per step with a text state file (restart-safe, fingerprint guards stale progress), VIS-4 dark-hole confirmation gated at ≤150 µm before advancing, overlay PNG mapping confirmed/current/remaining + bit-change prompts. Unit + e2e walk (undrilled hole refuses; final archive). The live 20-hole done-when is operator-side — see decisions.md)*
- [ ] ORC-8 — Mask-open inspection stage

## WS-UI — Operator console
- [x] UI-1 — egui console skeleton *(crates/ui; egui-only core verified headless (22 tests incl. per-tab full-frame layout), eframe binary behind `native`. Status panel from SQLite + actions shelling `pcbforge` (via `cargo run --bin pcbforge` so it works from a repo checkout) + three central views: **Job preview**, **Fiducial check** (VIS-4 overlay), **Place on board** (drag/rotate the circuit over the bed frame → "Etch here" bakes the placement via `register`). Gaps: live-video pending VIS-1; verb output synchronous — see decisions.md/FLD)*
- [x] UI-2 — AR overlay *(Camera tab: “🔲 AR overlay” projects the registered design over the feed through the fiducial homography, with per-layer toggles (board / copper / ablate); identity placement so Gerber coords go straight through the map — same frame contract as `register --frame`. Falls back to a uniform scale (labelled “unregistered”) until ≥4 fiducials are detected. Generalizes the Place overlay via `place::composite_over`; drill-center layer awaits Excellon ingest in the console — see decisions.md)*
- [ ] UI-3 — Wizard panels
- [ ] UI-4 — Escalation viewer

## WS-DRV — Native drivers by USB sniffing
- [x] DRV-1 — Capture campaign kit *(docs/capture-plan.md + tools/capture.sh + captures/MANIFEST.csv; script logic mock-verified. Live acceptance (dummy usbmon capture) + real B4 captures are operator-side — no USB stack in the cloud container; RUNLOG.md USB ID still to be recorded at the machine)*
- [ ] DRV-2 — Protocol decode
- [ ] DRV-3 — Transport + replay harness
- [ ] DRV-4 — Driver core against mock
- [ ] DRV-5 — Live bring-up harness
- [ ] DRV-6 — Orchestra integration
- [ ] DRV-7 — Seacad/Omni X protocol reconnaissance *(stretch)*
- [ ] DRV-8 — Correction-mesh pre-warp in the native path

## WS-QA — Testing & quality
- [x] QA-1 — Geometry property-test expansion
- [ ] QA-2 — CI virtual-fab integration test
- [ ] QA-3 — AOI corpus & annotation tool
- [ ] QA-4 — Nightly hardware-in-loop script
- [x] QA-5 — Seeded-defect fixture generator *(geometric defect spec; net-named placement awaits ING-4)*

## WS-RES — Research tasks
- [x] RES-1 — Crate due diligence
- [x] RES-2 — LightBurn automation surface
- [x] RES-3 — JCZ protocol public documentation survey
- [x] RES-4 — Consumables & floor benchmarks *(optional)*

## WS-FLD — Field follow-ups *(from the first live burns; details in docs/field-notes.md)*
- [ ] FLD-1 — Verify open-path `Line` primitive on first live Line-mode job (the one inferred lbrn2 field)
- [ ] FLD-2 — Two-polyline LightBurn sample → confirm multi-shape VertID/PrimID convention
- [x] FLD-3 — Silence caught-panic stderr spam from the offset retry ladder *(geom::silence_cavalier_panics filtering hook; self-exec regression test in cavalier_panic_silence.rs)*
- [x] FLD-4 — Emit `anglePerPass` from `fill_angle_step_deg` (per-pass hatch rotation in one layer) *(EmitLayer.fill_angle_step_deg → `--angle-step-deg`; matches two-layer sample C01)*
- [ ] FLD-5 — Board cut via LightBurn's native kerf/tabs/perforation (needs one sample with those set)
- [x] FLD-6 — Job placement flags (`--origin-x/--origin-y/--center`) *(cam::lbrn2::place_frame; corner or bbox-center anchor)*
- [ ] FLD-7 — `uv-base.lbrn2` when a UV device profile exists → UV schema delta
- [x] FLD-8 — Upstream cavalier_contours findings / evaluate newer release *(0.7.0 still latest; findings + minimal repros + version-bump regression gate in docs/research/cavalier-contours-findings.md)*
- [x] FLD-9 — Console: stream verb output incrementally (spawn_verb background thread + channel; run_verb non-blocking, running spinner)
- [ ] FLD-10 — Console live-video panel once VIS-1 lands (replace the camera stub); wire the preview panel into UI-2's AR overlay
- [x] FLD-11 — Fiducial-check view: live tracking on the camera feed (● Live re-detects each frame; reuses camera source + Capture thread; perspective refits live)
- [x] FLD-13 — `pcbforge cam --list/--grab` CLI verbs *(VIS-1 CLI surface; extracted the egui-free camera code into a shared `capture` crate so the CLI reuses it without pulling in the GUI. `pcbforge cam --list` enumerates devices; `--grab <out.png>` writes a gray frame from `--file <path>` (everywhere) or `--device <i>` (needs the `camera` feature). cam_e2e covers grab-from-file, list, no-feature device error, usage error — see decisions.md)*
- [~] FLD-12 — Fiducial-check view: profile selector (DARK_DOT/ANNULUS/BACKLIT) + click-to-place expected fiducials *(profile combo threads a `FiducialProfile` into `check_frame`/live detection — verified a backlit frame the dark-dot matcher won't lock; “✚ click-to-place”: left-click adds an expected fiducial, right-click a ✛ removes it, drag fine-tunes (all edit the layout, the source of truth — so the set shrinks, not just grows). Still on a uniform scale — the real VIS-3 BedMap is hardware-gated — see decisions.md)*
