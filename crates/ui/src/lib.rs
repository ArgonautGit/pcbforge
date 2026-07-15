//! UI-1 — the PCBForge operator console (egui).
//!
//! The console is an egui application (`app::ConsoleApp`) that renders a
//! board/stage status panel, an actions panel shelling the `pcbforge` CLI
//! verbs, a rasterized job preview, a log pane, and a stubbed camera panel
//! (pending VIS-1). All UI is egui-only so it lays out and is tested
//! *headlessly*; the actual OS window is an `eframe` wrapper behind the
//! `native` feature (see `src/main.rs`), which needs a display + GL/X11.

mod app;
mod calib;
mod fiducial;
mod imgview;
mod place;
mod preview;
mod settings;
mod status;

// Camera capture lives in the egui-free `capture` crate (shared with the CLI);
// alias it as `camera` so `crate::camera::…` paths keep resolving.
use capture as camera;

pub use app::{ConsoleApp, LogLine, default_cli_cmd, job_shapes, preview_image, run_capture};
pub use calib::{
    AnchorDot, Calibration, CameraCal, GridSpec, LensDot, fit_camera_lens, fit_camera_to_machine,
};
pub use capture::{Capture, Source, clean_path, grab, list_devices};
pub use fiducial::{
    FidKind, FidResult, FidRow, ProfileKind, check as fiducial_check, check_frame, parse_layout,
};
pub use place::{Placement, bbox_center_mm, composite, composite_over};
pub use preview::{Layer, rasterize};
pub use status::{BoardStatus, StatusSnapshot, snapshot};
pub use vision::{Homography, fit_homography};

/// Launch the native windowed console. Requires the `native` feature (pulls in
/// `eframe`) and a display with GL/X11 — headless/CI builds use the egui-only
/// library path and its frame tests instead.
#[cfg(feature = "native")]
pub fn run_native(db_path: std::path::PathBuf, cli_cmd: Vec<String>) -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "PCBForge console",
        options,
        Box::new(move |_cc| Ok(Box::new(ConsoleApp::new(db_path, cli_cmd)))),
    )
}
