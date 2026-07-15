# PCBForge — working notes

Rust workspace that turns KiCad PCB designs into laser-ablation jobs for a UV
laser. Crates: `core` (i64-nm geometry), `ingest` (KiCad/Gerber → layers), `cam`
(process compilers, tiling, flip), `vision` (fiducials, homography, lens fit),
`orchestra` (stage engine), `drivers`, `capture` (egui-free camera), `cli`
(`pcbforge`), `ui` (egui operator console), `xtask`.

Conventions: log non-trivial decisions in `docs/decisions.md`; test fixtures
live in `samples/` (regenerate/validate with `cargo xtask fixtures`).

## Verifying UI changes (console = `crates/ui`)

**When you add or change a console UI feature, verify it against the real app
headlessly — don't rely only on `ctx.run` shape-assertions or the `dump_*`
example re-draws.** Drive the actual `ConsoleApp` and look at it.

The tool is `crates/ui/examples/debug_driver.rs` (built on `egui_kittest`); full
reference in **`AGENT_DEBUGGING.md`**. Loop:

1. **Drive it.** Write a script and run it:
   ```sh
   printf 'tree\nclick "🎯 Calibrate"\nstate\n' | cargo run -p ui --example debug_driver
   # or: cargo run -p ui --example debug_driver -- script.txt
   ```
   `tree` dumps the accessibility tree (find labels here — don't guess),
   `click`/`type`/`set`/`key` drive widgets, `state` prints
   `ConsoleApp::debug_summary()`, `step`/`settle` advance frames.
2. **See it.** `screenshot out.png` renders the real frame (needs a GPU adapter:
   `source scripts/headless-gpu.sh` — auto-finds SwiftShader). Read the PNG to
   check the actual pixels, not just that it didn't panic.
3. **Lock it in.** Add/extend a headless test in
   `crates/ui/tests/ui_interaction.rs` (runs under `cargo test`, no GPU).
   Optional pixel baselines: `crates/ui/tests/ui_snapshots.rs` (`#[ignore]`d).

When you add UI, keep it drivable:
- Give interactive widgets a **label** (`Button`, `Slider::text`, `Checkbox`);
  associate bare `text_edit_singleline` with `labelled_by(label.id)`.
- Surface new inspectable state in `ConsoleApp::debug_summary()` so `state`
  reports it.

**Limits of headless driving** (cover these with unit/integration tests + a
`dump_*` example that renders the geometry to PNG instead):
- Canvas interactions aren't accessible widgets — marking calibration corners,
  dragging fiducial markers or the placement overlay, and Ctrl-pan/zoom can't be
  driven via accesskit.
- Anything needing a real file/camera frame in an unlabeled path field.
