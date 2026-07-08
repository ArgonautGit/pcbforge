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

## 2026-07-08 — RES-1..4 notes

- RES-1: no Cargo.toml exists yet, so there are no *pinned* versions to audit;
  the review evaluates the current releases as of 2026-07 and records the
  version examined per crate. Re-check on `cargo add` if a materially older
  version resolves.
- RES-2/RES-4: the "≤ 6 months" / "≤ 24 months" source-freshness preferences
  could not always be met — LightBurn galvo/Linux automation and fiber-ablation
  PCB write-ups change slowly. Older sources are used where nothing newer
  exists and are dated so staleness is visible.
