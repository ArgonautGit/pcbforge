//! Pixel snapshot tests for the console. These need a wgpu adapter (software is
//! fine), so they're `#[ignore]`d to keep `cargo test` green without a GPU.
//!
//!   source scripts/headless-gpu.sh
//!   UPDATE_SNAPSHOTS=1 cargo test -p ui --test ui_snapshots -- --ignored  # write baselines
//!   cargo test -p ui --test ui_snapshots -- --ignored                     # compare
//!
//! Baselines land in `tests/snapshots/*.png`; on mismatch kittest writes
//! `*.new.png` / `*.diff.png` next to them for inspection.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use ui::ConsoleApp;

fn console() -> Harness<'static, ConsoleApp> {
    let db = std::env::temp_dir().join("pcbforge-kittest-snap.sqlite");
    let app = ConsoleApp::new(db, vec!["true".to_string()]);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .build_state(|ctx, app: &mut ConsoleApp| app.ui(ctx), app);
    harness.run();
    harness
}

#[test]
#[ignore = "needs a wgpu adapter; run with --ignored after sourcing scripts/headless-gpu.sh"]
fn snapshot_job_tab() {
    let h = console();
    h.wgpu_snapshot("console_job_tab");
}

#[test]
#[ignore = "needs a wgpu adapter; run with --ignored after sourcing scripts/headless-gpu.sh"]
fn snapshot_camera_tab() {
    let mut h = console();
    h.get_by_label("📷 Camera").click();
    h.run();
    h.wgpu_snapshot("console_camera_tab");
}
