# PCBForge Backlog Checklist

53 tasks. Stretch-tagged: CAM-9, VIS-7, DRV-7, RES-4. Human-executed but
agent-prepared: DRV-1's captures, DRV-5's S2–S5, QA-4's burns, every "live"
done-when. Task prompts live in `docs/backlog.md`.

## WS-INF — Repo & infrastructure
- [x] INF-1 — Workspace scaffold + core types *(scaffold.md agent-authored per operator direction — see decisions.md)*
- [ ] INF-2 — CI pipeline
- [ ] INF-3 — Fixture manifest & validator
- [ ] INF-4 — Golden-image test harness

## WS-ING — Ingestion
- [ ] ING-1 — KiCad SVG → Layer
- [ ] ING-2 — Excellon drills
- [ ] ING-3 — Gerber X2 ingest (robust path)
- [ ] ING-4 — Net-ID source + net raster
- [ ] ING-5 — Board metadata (.gbrjob)
- [ ] ING-6 — kicad-cli invoker

## WS-GEO / WS-CAM — Geometry kernel & ablation CAM
- [ ] GEO-1 — Boolean/offset foundation
- [ ] CAM-1 — Isolation + rub-out path generator
- [ ] CAM-2 — Sliver force-clear
- [ ] CAM-3 — Heat-aware ordering
- [ ] CAM-4 — Pass planner
- [ ] CAM-5 — Dual-machine splitter
- [ ] CAM-6 — Process compilers: mask-open / legend / stencil / drill map
- [ ] CAM-7 — Design-rule checker
- [ ] CAM-8 — Fiducial/tooling feature injector
- [ ] CAM-9 — Tiling for the slide extension *(stretch)*

## WS-EMIT — Backends & CLI
- [ ] EMIT-1 — lbrn2 schema report *(blocked: samples/lbrn2 fixtures not yet provided)*
- [ ] EMIT-2 — lbrn2 emitter
- [ ] EMIT-3 — CLI surface

## WS-SIM — Virtual board
- [ ] SIM-1 — Raster sim backend (v1)
- [ ] SIM-2 — Heightmap sim + removal model (v2)

## WS-VIS — Vision & calibration
- [ ] VIS-1 — Capture module
- [ ] VIS-2 — Intrinsics calibration
- [ ] VIS-3 — Bed homography
- [ ] VIS-4 — Fiducial detectors
- [ ] VIS-5 — Affine fit + residuals
- [ ] VIS-6 — Burned-grid galvo calibration + register
- [ ] VIS-7 — TPS field correction *(stretch)*
- [ ] VIS-8 — Clearance classifier
- [ ] VIS-9 — Ladder wizard
- [ ] VIS-10 — Board-frame warper
- [ ] VIS-11 — AprilTag pallet ID
- [ ] VIS-12 — Red-pointer drift check
- [ ] VIS-13 — Verification-cross measurement

## WS-ORC — Orchestration
- [ ] ORC-1 — SQLite persistence
- [ ] ORC-2 — Stage engine
- [ ] ORC-3 — ClearanceLoop executor
- [ ] ORC-4 — Airflow interlock
- [ ] ORC-5 — Cross-machine handoff
- [ ] ORC-6 — Double-sided flip flow
- [ ] ORC-7 — Guided drilling
- [ ] ORC-8 — Mask-open inspection stage

## WS-UI — Operator console
- [ ] UI-1 — egui console skeleton
- [ ] UI-2 — AR overlay
- [ ] UI-3 — Wizard panels
- [ ] UI-4 — Escalation viewer

## WS-DRV — Native drivers by USB sniffing
- [ ] DRV-1 — Capture campaign kit *(blocked: RUNLOG.md with the B4 USB ID not yet provided; needs the machine with usbmon)*
- [ ] DRV-2 — Protocol decode
- [ ] DRV-3 — Transport + replay harness
- [ ] DRV-4 — Driver core against mock
- [ ] DRV-5 — Live bring-up harness
- [ ] DRV-6 — Orchestra integration
- [ ] DRV-7 — Seacad/Omni X protocol reconnaissance *(stretch)*
- [ ] DRV-8 — Correction-mesh pre-warp in the native path

## WS-QA — Testing & quality
- [ ] QA-1 — Geometry property-test expansion
- [ ] QA-2 — CI virtual-fab integration test
- [ ] QA-3 — AOI corpus & annotation tool
- [ ] QA-4 — Nightly hardware-in-loop script
- [ ] QA-5 — Seeded-defect fixture generator

## WS-RES — Research tasks
- [x] RES-1 — Crate due diligence
- [x] RES-2 — LightBurn automation surface
- [x] RES-3 — JCZ protocol public documentation survey
- [x] RES-4 — Consumables & floor benchmarks *(optional)*
