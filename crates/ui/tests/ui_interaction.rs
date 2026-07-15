//! Headless interaction tests for the console, driven through the
//! accessibility tree with `egui_kittest` (no GPU/display needed). See
//! AGENT_DEBUGGING.md and `examples/debug_driver.rs`.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use ui::ConsoleApp;

/// A fresh headless console harness (temp DB, `true` as the verb command).
fn console() -> Harness<'static, ConsoleApp> {
    let db = std::env::temp_dir().join("pcbforge-kittest.sqlite");
    let app = ConsoleApp::new(db, vec!["true".to_string()]);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .build_state(|ctx, app: &mut ConsoleApp| app.ui(ctx), app);
    harness.run();
    harness
}

#[test]
fn clicking_a_tab_switches_the_central_view() {
    let mut h = console();
    assert!(h.state().debug_summary().contains("tab=Job"));

    h.get_by_label("📷 Camera").click();
    h.run();
    assert!(
        h.state().debug_summary().contains("tab=Camera"),
        "camera tab active:\n{}",
        h.state().debug_summary()
    );

    h.get_by_label_contains("Calibrate").click();
    h.run();
    assert!(h.state().debug_summary().contains("tab=Calibrate"));
}

#[test]
fn the_refresh_button_is_present_and_clickable() {
    let mut h = console();
    // Exact-label lookup finds the toolbar button; clicking it must not panic.
    h.get_by_label("⟳ Refresh").click();
    h.run();
    // Still laid out and responsive after the action.
    assert!(h.state().debug_summary().contains("tab=Job"));
}

#[test]
fn the_accessibility_tree_exposes_labeled_widgets() {
    let h = console();
    // Buttons carry labels, so they're queryable/drivable by an agent. Use
    // exact labels (the substring queries panic when several nodes match).
    assert!(h.query_by_label("⟳ Refresh").is_some());
    assert!(h.query_by_label("📷 Camera").is_some());
    assert!(h.query_by_label("🎯 Calibrate").is_some());
}
