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
fn camera_work_area_starts_auto_centered() {
    let mut h = console();
    h.get_by_label("📷 Camera").click();
    h.run();
    let state = h.state().debug_summary();
    assert!(
        state.contains("bed_overlay: show=true field=70mm center=(35.0,35.0) auto=true"),
        "camera work area uses the operator default:\n{state}"
    );
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
fn calibration_frame_loads_from_a_typed_path() {
    let mut h = console();
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    // The grid-frame field is labelled (labelled_by), so it's drivable: type
    // the committed distorted-grid fixture path and load it.
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../samples/calibration/grid-7x7-10mm-distorted.png"
    );
    let field = h.get_by_label("grid frame (optional)");
    field.focus();
    field.type_text(fixture);
    h.run();
    h.get_by_label("⤵ Load grid frame").click();
    h.run();
    // The real frame loaded into the calibrate view (660×660 fixture).
    assert!(
        h.state().debug_summary().contains("calib_frame: 660×660"),
        "grid frame loaded via the UI:\n{}",
        h.state().debug_summary()
    );
}

#[test]
fn gerbers_from_kicad_fills_the_copper_and_outline_fields() {
    let mut h = console();
    // Point the KiCad-project field at the sample board and export. The button
    // pre-fills the deterministic output paths and shells `pcbforge gerbers` in
    // the background (non-blocking), so this asserts the field wiring without
    // needing kicad-cli — the export itself is covered by the CLI e2e test.
    let board = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../samples/kicad/valdemo2.kicad_pcb"
    );
    let field = h.get_by_label("KiCad project");
    field.focus();
    field.type_text(board);
    h.run();
    h.get_by_label("⚙ Gerbers from KiCad").click();
    h.run();

    let state = h.state().debug_summary();
    assert!(
        state.contains("copper=copper.gbr") && state.contains("outline=outline.gbr"),
        "KiCad button filled the Gerber fields:\n{state}"
    );
}

#[test]
fn the_dot_contrast_toggle_switches_detection_polarity() {
    let mut h = console();
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    // Drive to a known state rather than trusting the persisted default (the
    // setting survives across runs, so the starting polarity is whatever was
    // saved last). Dark-on-light is for printed grids / dark-anodized burns.
    h.get_by_label("◉ dark-on-light").click();
    h.run();
    assert!(
        h.state().debug_summary().contains("contrast=dark-on-light"),
        "selected dark:\n{}",
        h.state().debug_summary()
    );
    // Switching to bright-on-dark is what lets an ablated (light-on-dark) burn
    // anchor — the operator's 0/49 case.
    h.get_by_label("◎ bright-on-dark").click();
    h.run();
    assert!(
        h.state()
            .debug_summary()
            .contains("contrast=bright-on-dark"),
        "toggled to bright:\n{}",
        h.state().debug_summary()
    );
}

#[test]
fn the_laser_field_calibration_step_is_selectable() {
    let mut h = console();
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    h.get_by_label_contains("③ Laser field").click();
    h.run();
    let s = h.state().debug_summary();
    assert!(
        s.contains("calib_mode=LaserField") && s.contains("laser_field: none"),
        "③ selects the laser-field step, uncalibrated:\n{s}"
    );
}

#[test]
fn laser_anchor_is_explicitly_labelled_as_approximate() {
    let mut h = console();
    h.get_by_label_contains("Calibrate").click();
    h.run();
    assert!(
        h.query_by_label_contains("② Laser anchor (approximate)")
            .is_some(),
        "the homography-only fallback must not imply lens/field correction"
    );
}

#[test]
fn compensate_field_is_present_and_gated_off_without_a_field_cal() {
    let mut h = console();
    h.get_by_label_contains("Place on board").click();
    h.run();
    // The toggle exists but stays off with no field calibration this session
    // (its physical placement frame needs the fit).
    h.get_by_label("compensate field").click();
    h.run();
    assert!(
        h.state().debug_summary().contains("field_correct=false"),
        "compensate-field can't arm without a field cal:\n{}",
        h.state().debug_summary()
    );
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
