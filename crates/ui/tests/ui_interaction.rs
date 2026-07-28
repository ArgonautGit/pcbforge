//! Headless interaction tests for the console, driven through the
//! accessibility tree with `egui_kittest` (no GPU/display needed). See
//! AGENT_DEBUGGING.md and `examples/debug_driver.rs`.

use egui_kittest::kittest::Queryable;

mod common;
use common::console;

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
        state.contains("copper=copper-F_Cu.gbr") && state.contains("outline=outline.gbr"),
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
        s.contains("calib_mode=FidHoles") && summary_line(&s, "fid_rect:").contains("layout="),
        "④ selects the fiducial-holes step and reports the rectangle layout:\n{s}"
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
fn laser_field_scale_handling_choice_is_drivable() {
    let mut h = console();
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    // Exact label: "③ Laser field" also appears in the Job-tab export help.
    h.get_by_label("3) Laser field (burned grid)").click();
    h.run();
    // Each of the three choices is its own labelled node, so an operator (or a
    // headless driver) can select any of them by name. The setting persists
    // across runs on the shared temp DB, so drive to a known one first.
    for (label, token) in [
        ("refuse a machine scale error", "refuse"),
        ("compensate machine scale", "compensate"),
        (
            "correct distortion only (keep 1:1 work area)",
            "distortion_only",
        ),
        ("refuse a machine scale error", "refuse"),
    ] {
        h.get_by_label(label).click();
        h.run();
        assert!(
            summary_line(&h.state().debug_summary(), "laser_field:")
                .contains(&format!("scale_mode={token}")),
            "{label} selects {token}:\n{}",
            h.state().debug_summary()
        );
    }
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
    let h = console();
    // The export gate is a warning, not a lockout: uncalibrated exports emit
    // unwarped geometry and say so, rather than blocking the operator.
    assert!(
        h.query_by_label_contains("else exports unwarped").is_some(),
        "the placement controls must state that exports without a field map are unwarped"
    );
    assert!(h.query_by_label_contains("export disabled until").is_none());
    assert!(h.query_by_label("compensate field").is_none());
}

#[test]
fn etch_and_run_in_lightburn_button_is_present_and_guards_without_a_job() {
    let mut h = console();
    // A fresh console reports the LightBurn defaults on the place: line.
    let s = h.state().debug_summary();
    assert!(
        summary_line(&s, "place:").contains("lightburn=idle") && s.contains("device=BSLFiber"),
        "place line reports idle LightBurn + the default device:\n{s}"
    );
    // The one-click button exists and is drivable by label.
    assert!(
        h.query_by_label("🔥 Etch + Run").is_some(),
        "the Etch + Run button must be present and labelled"
    );
    // Clicking with no design loaded hits the existing guard: nothing is
    // armed, so the summary still reports lightburn=idle.
    h.get_by_label("🔥 Etch + Run").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "place:").contains("lightburn=idle"),
        "the guard refused: no LightBurn run was queued:\n{}",
        h.state().debug_summary()
    );
}

/// The no-burn drill-emit controls: the Job-tab path fields and the
/// Actions-panel buttons are present and labelled (drivable), "⚙ Drills from
/// KiCad" fills the drill field with the stable pth/npth paths, and clicking
/// the emit button never queues a LightBurn run. Assertions are positive (type
/// a path, expect the fill) because the kittest consoles share one settings
/// sidecar — an "unset" assertion would be order-dependent.
#[test]
fn drill_holes_are_emitted_without_a_burn() {
    let mut h = console();
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
    // Clicking emit with no design loaded hits the guard: no file written,
    // no load spawned — LightBurn stays idle with nothing queued.
    h.get_by_label("⤓ Emit drill holes → LightBurn (no burn)")
        .click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "place:").contains("lightburn=idle"),
        "a guarded drill emit spawns no LightBurn activity:\n{}",
        h.state().debug_summary()
    );
}

#[test]
fn lightburn_device_field_is_labelled_and_editable() {
    let mut h = console();
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
fn generate_holes_buttons_are_present_and_labelled() {
    let mut h = console();
    // Fiducial tab: the button writes the .lbrn2 and LOADS it in LightBurn
    // without starting it, so the label must say "(no burn)" — matching the
    // Actions panel's drill emit.
    h.get_by_label("◎ Fiducial check").click();
    h.run();
    assert!(
        h.query_by_label("⚙ Generate holes → LightBurn (no burn)")
            .is_some(),
        "the fiducial-tab generate button must be present and labelled"
    );
    // ④ Fiducial holes step: same contract on the calibration flow's button.
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    h.get_by_label("4) Fiducial holes (board)").click();
    h.run();
    assert!(
        h.query_by_label("⚙ Generate fiducial holes → LightBurn (no burn)")
            .is_some(),
        "the ④-step generate button must be present and labelled"
    );
}

#[test]
fn wobble_is_opt_in_and_drivable() {
    let mut h = console();
    // The setting persists across runs (shared temp DB), so drive to the
    // known OFF state before asserting rather than trusting the start value.
    if summary_line(&h.state().debug_summary(), "gerbers:").contains("wobble=true") {
        h.get_by_label("wobble").click();
        h.run();
    }
    assert!(
        summary_line(&h.state().debug_summary(), "gerbers:").contains("wobble=false"),
        "wobble starts (or is driven) off:\n{}",
        h.state().debug_summary()
    );
    // The checkbox is labelled, so an agent/operator can drive it; enabling it
    // reveals the step/size fields in the same row.
    h.get_by_label("wobble").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "gerbers:").contains("wobble=true"),
        "checkbox turns wobble on:\n{}",
        h.state().debug_summary()
    );
    // Leave the persisted setting at the default (off) for the next test run.
    h.get_by_label("wobble").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "gerbers:").contains("wobble=false"),
        "checkbox turns wobble back off:\n{}",
        h.state().debug_summary()
    );
}

/// The double-sided side selector: a fresh console works the front, and picking
/// Back both switches the side and reveals the back-only inputs.
#[test]
fn the_side_selector_switches_to_the_back_form() {
    let mut h = console();
    assert!(
        h.state().debug_summary().contains("side=Front"),
        "a fresh console works the front:\n{}",
        h.state().debug_summary()
    );
    // The back inputs only exist on the back, so they are absent to start with.
    assert!(h.query_by_label("back copper .gbr").is_none());

    h.get_by_label("Back").click();
    h.run();
    let s = h.state().debug_summary();
    assert!(s.contains("side=Back"), "Back selected:\n{s}");
    for label in ["back copper .gbr", "back outline .gbr", "back out .lbrn2"] {
        assert!(
            h.query_by_label(label).is_some(),
            "the back form must expose `{label}`"
        );
    }
}

/// Each back path field is associated with its caption (labelled_by), so it is
/// reachable by name rather than being an anonymous box in the form — and what
/// the lookup returns has to be the field, not the caption, or the typing goes
/// nowhere.
#[test]
fn the_back_gerber_fields_are_drivable_by_label() {
    let mut h = console();
    h.get_by_label("Back").click();
    h.run();

    for (label, path, token) in [
        (
            "back copper .gbr",
            "/boards/demo-B_Cu.gbr",
            "copper=demo-B_Cu.gbr",
        ),
        (
            "back outline .gbr",
            "/boards/outline.gbr",
            "outline=outline.gbr",
        ),
    ] {
        h.get_by_label(label).focus();
        // egui hands over focus on the NEXT frame; typing in the same frame
        // would land in whichever field held focus before.
        h.run();
        h.get_by_label(label).type_text(path);
        h.run();
        assert!(
            summary_line(&h.state().debug_summary(), "back:").contains(token),
            "typing into `{label}` must land in the back state:\n{}",
            h.state().debug_summary()
        );
    }
}

/// Back → Front → Back through the real selector: the operator's typed paths
/// belong to the side and survive the round trip, and no per-side cache (fitted
/// pose, placement scale, loaded job geometry) shows up on the far end. The
/// clearing itself is pinned by `set_side_resets_per_side_caches` in the lib's
/// unit tests, which can seed the caches first; this covers the driven path.
#[test]
fn switching_sides_keeps_the_back_form_across_the_round_trip() {
    let mut h = console();
    h.get_by_label("Back").click();
    h.run();
    let field = h.get_by_label("back copper .gbr");
    field.focus();
    field.type_text("/boards/demo-B_Cu.gbr");
    h.run();

    h.get_by_label("Front").click();
    h.run();
    assert!(
        h.state().debug_summary().contains("side=Front"),
        "switched back to the front:\n{}",
        h.state().debug_summary()
    );
    assert!(
        h.query_by_label("back copper .gbr").is_none(),
        "the back form is hidden while working the front"
    );

    h.get_by_label("Back").click();
    h.run();
    let s = h.state().debug_summary();
    assert!(s.contains("side=Back"), "back again:\n{s}");
    assert!(
        summary_line(&s, "back:").contains("copper=demo-B_Cu.gbr"),
        "the back's own paths survive the round trip:\n{s}"
    );
    let place = summary_line(&s, "place:");
    assert!(
        place.contains("job_polys=0")
            && place.contains("auto_pose=false")
            && place.contains("scale=1.0000"),
        "the side switch left no placement cache behind:\n{s}"
    );
    assert!(
        summary_line(&s, "fid_pose:").contains("none"),
        "the other face's fiducial pose must not carry over:\n{s}"
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

/// The Live re-acquire interval is an operator setting, so it has to be
/// reachable by name and readable back out of the summary — the same contract
/// every other drivable field on this tab has.
#[test]
fn the_live_reacquire_interval_is_labelled_and_reported() {
    let mut h = console();
    h.get_by_label("◎ Fiducial check").click();
    h.run();
    assert!(
        summary_line(&h.state().debug_summary(), "fiducials:").contains("live_recover_s=0.5"),
        "a fresh console re-acquires every 500 ms:\n{}",
        h.state().debug_summary()
    );
    // Associated with its label, so it is queryable rather than an anonymous
    // number in a wrapped row of a dozen widgets — and typing into what that
    // lookup returns has to land in the setting, or the association found the
    // caption instead of the field.
    let field = h.get_by_label("re-acquire s");
    field.focus();
    field.type_text("2");
    field.key_press(egui_kittest::kittest::Key::Enter);
    h.run();
    assert!(
        // The field opens for editing pre-filled, so typing EXTENDS the value
        // (0.5 → 0.52) rather than replacing it — same as the device field.
        summary_line(&h.state().debug_summary(), "fiducials:").contains("live_recover_s=0.52"),
        "the re-acquire field is drivable by its label:\n{}",
        h.state().debug_summary()
    );
}

/// The three compensation heights live on three different tabs — each beside
/// the thing that fixes it — so the only way to prove all three are wired to
/// the right state is to drive all three and read the one summary line they
/// share. A crossed pair here reads on screen as a plausible number and on the
/// bench as a burn in the wrong place.
#[test]
fn the_three_heights_are_labelled_on_their_own_tabs_and_reported_together() {
    let mut h = console();
    assert!(
        summary_line(&h.state().debug_summary(), "height_comp:")
            .contains("paper=0.00 laser=0.00 surface=0.00 mm"),
        "a fresh console is uncompensated:\n{}",
        h.state().debug_summary()
    );
    h.get_by_label("🎯 Calibrate").click();
    h.run();
    // ① is the mode the console starts in; the paper height sits with it.
    let field = h.get_by_label("paper grid height mm");
    field.focus();
    field.type_text("2");
    field.key_press(egui_kittest::kittest::Key::Enter);
    h.run();

    h.get_by_label("3) Laser field (burned grid)").click();
    h.run();
    let field = h.get_by_label("laser grid height mm");
    field.focus();
    field.type_text("4");
    field.key_press(egui_kittest::kittest::Key::Enter);
    h.run();

    h.get_by_label("◎ Fiducial check").click();
    h.run();
    let field = h.get_by_label("surface height mm");
    field.focus();
    field.type_text("6");
    field.key_press(egui_kittest::kittest::Key::Enter);
    h.run();

    // Each drag field opens for editing pre-filled at its current value, "0.0"
    // at this speed, so a typed digit EXTENDS it: 2 → 0.02. Three different
    // digits, so a crossed pair cannot read as a pass.
    assert!(
        summary_line(&h.state().debug_summary(), "height_comp:")
            .contains("paper=0.02 laser=0.04 surface=0.06 mm"),
        "each height lands in its own state:\n{}",
        h.state().debug_summary()
    );
}

/// The camera↔laser loop readout sits at the TOP of the fiducial tab, above the
/// form — the signal it replaces was a clause in a note line under a button row
/// and went unread through seven consecutive checks. On a console that has
/// never burned holes it says so rather than reading as a pass.
#[test]
fn the_camera_laser_loop_readout_is_on_the_fiducial_tab() {
    let mut h = console();
    h.get_by_label("◎ Fiducial check").click();
    h.run();
    assert!(
        h.query_by_label_contains("camera↔laser loop").is_some(),
        "the loop readout is drawn on the tab"
    );
    assert!(
        h.query_by_label_contains("no fiducial holes have been burned")
            .is_some(),
        "and an unarmed loop says why it has no number"
    );
    assert!(
        summary_line(&h.state().debug_summary(), "fid_loop:").contains("unmeasured ref_holes=0"),
        "a headless run can read the same verdict:\n{}",
        h.state().debug_summary()
    );
    // No alarm to dismiss and nothing to confirm adopting on a fresh console:
    // both actions appear only once there is something to act on.
    assert!(h.query_by_label("✕ dismiss map alarm").is_none());
    assert!(h.query_by_label_contains("adopt anyway").is_none());
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
