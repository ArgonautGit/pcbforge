# Decisions & deviations log

Per the backlog conventions: every task records deviations from its prompt and
discovered constraints here.

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
