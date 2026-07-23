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

/// Pull the single summary line beginning with `prefix` (e.g. `calib_paper:`).
/// Panics with the full summary if the line is missing, so failures are legible.
fn summary_line<'a>(summary: &'a str, prefix: &str) -> &'a str {
    summary
        .lines()
        .find(|l| l.trim_start().starts_with(prefix))
        .unwrap_or_else(|| panic!("no `{prefix}` line in summary:\n{summary}"))
}

#[test]
fn the_dot_contrast_toggle_switches_detection_polarity() {
    let mut h = console();
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    // The Calibrate tab opens in ① CameraLens mode, so the contrast toggle binds
    // the paper set. Drive to a known state rather than trusting the persisted
    // default (the setting survives across runs, so the starting polarity is
    // whatever was saved last). Dark-on-light is for printed grids / dark
    // burns. The summary now carries two `contrast=` lines, so assert on the
    // `calib_paper:` line specifically.
    h.get_by_label("◉ dark-on-light").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "calib_paper:").contains("contrast=dark-on-light"),
        "selected dark:\n{}",
        h.state().debug_summary()
    );
    // Switching to bright-on-dark is what lets an ablated (light-on-dark) burn
    // anchor — the operator's 0/49 case.
    h.get_by_label("◎ bright-on-dark").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "calib_paper:")
            .contains("contrast=bright-on-dark"),
        "toggled to bright:\n{}",
        h.state().debug_summary()
    );
}

#[test]
fn grid_params_are_per_step() {
    let mut h = console();
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    // The tab opens in ① CameraLens mode: this toggle binds the paper set.
    h.get_by_label("◎ bright-on-dark").click();
    h.run();
    // Switch to ③ Laser field, which binds the burn set (exact label — the
    // substring also appears in the Job-tab export help).
    h.get_by_label("3) Laser field (burned grid)").click();
    h.run();
    h.get_by_label("◉ dark-on-light").click();
    h.run();
    // Each step kept its own polarity, proving the form binds per-step through
    // `active_params_mut()` rather than sharing one grid.
    let s = h.state().debug_summary();
    assert!(
        summary_line(&s, "calib_paper:").contains("contrast=bright-on-dark"),
        "paper kept its polarity:\n{s}"
    );
    assert!(
        summary_line(&s, "calib_burn:").contains("contrast=dark-on-light"),
        "burn kept its own polarity:\n{s}"
    );
}

#[test]
fn the_laser_field_calibration_step_is_selectable() {
    let mut h = console();
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    // Exact label: the substring "③ Laser field" also appears in the Job-tab
    // export help text, and substring queries panic on multiple matches.
    h.get_by_label("3) Laser field (burned grid)").click();
    h.run();
    let s = h.state().debug_summary();
    assert!(
        s.contains("calib_mode=LaserField") && s.contains("laser_field: none"),
        "③ selects the laser-field step, uncalibrated:\n{s}"
    );
}

#[test]
fn the_fiducial_holes_calibration_step_is_selectable() {
    let mut h = console();
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    h.get_by_label("4) Fiducial holes (board)").click();
    h.run();
    let s = h.state().debug_summary();
    assert!(
        s.contains("calib_mode=FidHoles")
            && summary_line(&s, "fid_board:").contains("layout="),
        "④ selects the fiducial-holes step and reports the board layout:\n{s}"
    );
}

#[test]
fn fit_feedback_visibility_toggle_is_drivable() {
    let mut h = console();
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    // The toggle is per-session (not persisted), so it starts on regardless of
    // the shared temp DB. debug_summary reports it on the calib_burn: line.
    assert!(
        summary_line(&h.state().debug_summary(), "calib_burn:").contains("feedback=on"),
        "feedback starts visible:\n{}",
        h.state().debug_summary()
    );
    // The checkbox is labelled, so an agent/operator can drive it.
    h.get_by_label("show fit feedback").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "calib_burn:").contains("feedback=off"),
        "checkbox hides the fit feedback:\n{}",
        h.state().debug_summary()
    );
    h.get_by_label("show fit feedback").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "calib_burn:").contains("feedback=on"),
        "checkbox re-shows the fit feedback:\n{}",
        h.state().debug_summary()
    );
}

#[test]
fn laser_field_scale_compensation_opt_in_is_drivable() {
    let mut h = console();
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    // Exact label: "③ Laser field" also appears in the Job-tab export help.
    h.get_by_label("3) Laser field (burned grid)").click();
    h.run();
    // The setting persists across runs (shared temp DB), so drive to a known
    // OFF state before toggling rather than trusting the starting value.
    if summary_line(&h.state().debug_summary(), "laser_field:").contains("scale_comp=on") {
        h.get_by_label("compensate machine scale").click();
        h.run();
    }
    assert!(
        summary_line(&h.state().debug_summary(), "laser_field:").contains("scale_comp=off"),
        "opt-in driven to off:\n{}",
        h.state().debug_summary()
    );
    // The checkbox is labelled, so an agent/operator can drive it.
    h.get_by_label("compensate machine scale").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "laser_field:").contains("scale_comp=on"),
        "checkbox turns scale compensation on:\n{}",
        h.state().debug_summary()
    );
}

#[test]
fn laser_anchor_is_explicitly_labelled_as_approximate() {
    let mut h = console();
    h.get_by_label_contains("Calibrate").click();
    h.run();
    assert!(
        h.query_by_label_contains("2) Laser anchor (approximate)")
            .is_some(),
        "the homography-only fallback must not imply lens/field correction"
    );
}

#[test]
fn laser_anchor_exposes_manual_dot_correction() {
    let mut h = console();
    h.get_by_label_contains("Calibrate").click();
    h.run();
    h.get_by_label_contains("2) Laser anchor").click();
    h.run();
    assert!(
        h.query_by_label("Correct detected dots").is_some(),
        "the review control must remain accessible to headless/operator driving"
    );
    assert!(
        h.state().debug_summary().contains("edit_anchor_dots=false"),
        "correction starts inactive:\n{}",
        h.state().debug_summary()
    );
}

#[test]
fn place_advertises_conditional_field_warp() {
    let mut h = console();
    h.get_by_label_contains("Place on board").click();
    h.run();
    // The export gate is a warning, not a lockout: uncalibrated exports emit
    // unwarped geometry and say so, rather than blocking the operator.
    assert!(
        h.query_by_label_contains("else exports unwarped").is_some(),
        "Place must state that exports without a field map are unwarped"
    );
    assert!(h.query_by_label_contains("export disabled until").is_none());
    assert!(h.query_by_label("compensate field").is_none());
}

#[test]
fn etch_and_run_in_lightburn_button_is_present_and_guards_without_a_job() {
    let mut h = console();
    h.get_by_label_contains("Place on board").click();
    h.run();
    // A fresh console reports the LightBurn defaults on the place: line.
    let s = h.state().debug_summary();
    assert!(
        summary_line(&s, "place:").contains("lightburn=idle") && s.contains("device=BSLFiber"),
        "place line reports idle LightBurn + the default device:\n{s}"
    );
    // The one-click button exists and is drivable by label.
    assert!(
        h.query_by_label("▶ Etch + run in LightBurn").is_some(),
        "the Etch + run button must be present and labelled"
    );
    // Clicking with no frame + job loaded hits the existing guard: nothing is
    // armed, so the summary still reports lightburn=idle.
    h.get_by_label("▶ Etch + run in LightBurn").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "place:").contains("lightburn=idle"),
        "the guard refused: no LightBurn run was queued:\n{}",
        h.state().debug_summary()
    );
}

/// The Place tab carries the no-burn drill-emit controls: the path fields and
/// buttons are present and labelled (drivable), "⚙ Drills from KiCad" fills
/// the drill field with the stable pth/npth paths, and clicking the emit
/// button never queues a LightBurn run. Assertions are positive (type a path,
/// expect the fill) because the kittest consoles share one settings sidecar —
/// an "unset" assertion would be order-dependent.
#[test]
fn place_tab_emits_drill_holes_without_a_burn() {
    let mut h = console();
    h.get_by_label_contains("Place on board").click();
    h.run();
    let s = h.state().debug_summary();
    assert!(
        summary_line(&s, "place:").contains("drill_out=drill.lbrn2"),
        "place line reports the drill output default:\n{s}"
    );
    // Labelled inputs for the drill file(s) and output.
    assert!(
        h.query_by_label("drill .drl").is_some(),
        "the drill-file field label is present"
    );
    assert!(
        h.query_by_label("drill out .lbrn2").is_some(),
        "the drill-output field label is present"
    );
    // "⚙ Drills from KiCad" fills the field with the deterministic pth/npth
    // pair (next to the Gerbers). The verb shells in the background ("true"
    // here), so this asserts the field wiring without needing kicad-cli.
    // Select-all before typing: the shared settings sidecar may have persisted
    // a previous run's project path, and plain typing APPENDS to it.
    let board = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../samples/kicad/valdemo2.kicad_pcb"
    );
    let field = h.get_by_label("KiCad project");
    field.focus();
    field.key_combination(&[
        egui_kittest::kittest::Key::Command,
        egui_kittest::kittest::Key::A,
    ]);
    field.type_text(board);
    h.run();
    h.get_by_label("⚙ Drills from KiCad").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "place:").contains("drills=npth.drl"),
        "the KiCad button filled the drill field (summary shows the pair's basename):\n{}",
        h.state().debug_summary()
    );
    // Clicking emit with no frame + job loaded hits the guard: no file
    // written, and — the point of the button — LightBurn stays idle with
    // nothing queued.
    h.get_by_label("⤓ Emit drill holes (no burn)").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "place:").contains("lightburn=idle"),
        "drill emit never arms a LightBurn run:\n{}",
        h.state().debug_summary()
    );
}

#[test]
fn lightburn_device_field_is_labelled_and_editable() {
    let mut h = console();
    h.get_by_label_contains("Place on board").click();
    h.run();
    // The device field is labelled_by its label, so it's drivable.
    let field = h.get_by_label("LightBurn device");
    field.focus();
    field.type_text("Galvo9");
    h.run();
    // The field is wired to state (typing lands in the device name, which flows
    // into the summary); the default is pre-filled, so typing extends it.
    assert!(
        summary_line(&h.state().debug_summary(), "place:").contains("Galvo9"),
        "typing into the device field updates the state:\n{}",
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

#[test]
fn fiducial_tab_exposes_its_controls_after_the_panel_split() {
    let mut h = console();
    h.get_by_label("◎ Fiducial check").click();
    h.run();
    assert!(
        h.state().debug_summary().contains("tab=Fiducials"),
        "fiducial tab active:\n{}",
        h.state().debug_summary()
    );
    // The controls moved into a resizable top panel + scroll area (matching the
    // Calibrate tab); the labelled buttons must stay queryable/drivable.
    assert!(
        h.query_by_label("🎯 Check fiducials").is_some(),
        "Check button present after the split"
    );
    assert!(
        h.query_by_label("↺ reset markers").is_some(),
        "reset-markers button present after the split"
    );
    // A fresh tab has no active marking round.
    assert!(
        summary_line(&h.state().debug_summary(), "fiducials:").contains("marking=-"),
        "no marking round on a fresh tab:\n{}",
        h.state().debug_summary()
    );
    // The default layout seeds 4 markers; ✕ clear markers must drop them ALL
    // (it empties the layout, so the per-frame sync can't reseed them).
    assert!(
        summary_line(&h.state().debug_summary(), "fiducials:").contains("fiducials: 4 markers"),
        "default layout seeded markers to clear:\n{}",
        h.state().debug_summary()
    );
    h.get_by_label("✕ clear markers").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "fiducials:").contains("fiducials: 0 markers"),
        "clear markers removed every marker:\n{}",
        h.state().debug_summary()
    );
}
