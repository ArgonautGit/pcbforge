//! UI-1 — the PCBForge operator console (egui).
//!
//! The console is an egui application (`app::ConsoleApp`) that renders a
//! board/stage status panel, an actions panel shelling the `pcbforge` CLI
//! verbs, a rasterized job preview, a log pane, and a stubbed camera panel
//! (pending VIS-1). All UI is egui-only so it lays out and is tested
//! *headlessly*; the actual OS window is an `eframe` wrapper behind the
//! `native` feature (see `src/main.rs`), which needs a display + GL/X11.

mod app;
mod preview;
mod status;

pub use app::{ConsoleApp, LogLine, preview_image, run_capture};
pub use preview::{Layer, rasterize};
pub use status::{BoardStatus, StatusSnapshot, snapshot};

/// Launch the native windowed console. Requires the `native` feature (pulls in
/// `eframe`) and a display with GL/X11 — headless/CI builds use the egui-only
/// library path and its frame tests instead.
#[cfg(feature = "native")]
pub fn run_native(db_path: std::path::PathBuf, pcbforge_bin: String) -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "PCBForge console",
        options,
        Box::new(move |_cc| Ok(Box::new(ConsoleApp::new(db_path, pcbforge_bin)))),
    )
}
