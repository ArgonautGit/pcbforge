//! Shared harness setup for the console's integration tests, declared via
//! `mod common;` by each test file (integration tests can't reach the lib's
//! `#[cfg(test)]` helpers).

use std::ops::{Deref, DerefMut};

use egui_kittest::Harness;
use tempfile::TempDir;
use ui::ConsoleApp;

/// A headless console harness together with the temp directory holding its
/// store, removed when the harness drops.
///
/// Derefs to the [`Harness`], so tests drive it exactly as before.
pub struct Console {
    // The app has to release the store before the directory goes away, so the
    // harness must drop first — fields drop in declaration order.
    harness: Harness<'static, ConsoleApp>,
    _dir: TempDir,
}

impl Deref for Console {
    type Target = Harness<'static, ConsoleApp>;

    fn deref(&self) -> &Self::Target {
        &self.harness
    }
}

impl DerefMut for Console {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.harness
    }
}

/// A fresh headless console harness (temp DB, `true` as the verb command).
///
/// The directory is unique per call: the console persists operator fields to a
/// settings sidecar beside the database, so a shared path would bleed typed
/// input between tests running in parallel and back into the next run.
pub fn console() -> Console {
    let dir = tempfile::Builder::new()
        .prefix("pcbforge-kittest-")
        .tempdir()
        .expect("temp dir for the console store");
    let app = ConsoleApp::new(dir.path().join("console.sqlite"), vec!["true".to_string()]);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .build_state(|ctx, app: &mut ConsoleApp| app.ui(ctx), app);
    harness.run();
    Console { harness, _dir: dir }
}
