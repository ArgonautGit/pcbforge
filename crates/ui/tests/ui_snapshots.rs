//! Pixel snapshot tests for the console. These need a wgpu adapter (software is
//! fine), so they're `#[ignore]`d to keep `cargo test` green without a GPU.
//!
//!   source scripts/headless-gpu.sh
//!   UPDATE_SNAPSHOTS=1 cargo test -p ui --test ui_snapshots -- --ignored  # write baselines
//!   cargo test -p ui --test ui_snapshots -- --ignored                     # compare
//!
//! Baselines land in `tests/snapshots/*.png`; on mismatch kittest writes
//! `*.new.png` / `*.diff.png` next to them for inspection.

use egui_kittest::kittest::Queryable;

mod common;
use common::console;

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
