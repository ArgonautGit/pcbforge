use super::*;

fn tmp_db() -> PathBuf {
    // Unique per call so each console gets its own settings sidecar
    // (`*.console-settings`) — a shared path would bleed persisted input
    // fields between tests.
    //
    // Nested under one per-process parent so a run leaves a single directory
    // behind instead of one per call. The callers hold only a `PathBuf`, so
    // there is nowhere to hang a `TempDir` guard without threading one through
    // every call site; grouping keeps the leak proportional to runs rather
    // than to tests (this suite alone calls it ~150 times).
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir()
        .join(format!("ui-app-{}", std::process::id()))
        .join(N.fetch_add(1, Ordering::Relaxed).to_string());
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("t.sqlite")
}

fn nonlinear_app() -> ConsoleApp {
    nonlinear_app_with_db(&tmp_db())
}

fn nonlinear_app_with_db(db: &std::path::Path) -> ConsoleApp {
    use nalgebra::{Matrix3, Point2, Vector2};
    let mut app = ConsoleApp::new(db.to_path_buf(), vec!["true".into()]);
    let coords = [0.0, 20.0, 40.0, 60.0];
    let lens_pairs: Vec<_> = coords
        .iter()
        .flat_map(|&y| {
            coords.iter().map(move |&x| {
                (
                    Point2::new(10.0 * x + 20.0, 800.0 - 10.0 * y),
                    Point2::new(x, y),
                )
            })
        })
        .collect();
    let lens = vision::fit_lens(&lens_pairs).unwrap();
    let field_pairs: Vec<_> = coords
        .iter()
        .flat_map(|&y| {
            coords.iter().map(move |&x| {
                // A small, deterministic field stretch + cross term.
                (
                    Point2::new(x * 1.01 + x * y * 0.0002, y * 0.99),
                    Point2::new(x, y),
                )
            })
        })
        .collect();
    let field = vision::fit_field(&field_pairs).unwrap();
    let dots: Vec<_> = field_pairs
        .iter()
        .map(|(physical, commanded)| {
            let px = lens.mm_to_px.apply(physical.x, physical.y);
            calib::FieldDot {
                px,
                physical_mm: (physical.x, physical.y),
                commanded_mm: (commanded.x, commanded.y),
                field_um: ((physical.x - commanded.x).powi(2) + (physical.y - commanded.y).powi(2))
                    .sqrt()
                    * 1000.0,
                resid_um: 0.0,
                rejected: false,
            }
        })
        .collect();
    app.calibration.lens = Some(calib::CameraCal {
        lens,
        dots: vec![],
        found: 16,
        total: 16,
    });
    app.calibration.field = Some(calib::FieldCal {
        field,
        paper_to_machine: calib::Rigid2::IDENTITY,
        to_px: vision::Homography {
            matrix: Matrix3::new(10.0, 0.0, 20.0, 0.0, -10.0, 800.0, 0.0, 0.0, 1.0),
            residuals: vec![],
            rms: 0.0,
        },
        dots,
        found: 16,
        total: 16,
        field_verdict: vision::classify_field_error(
            &field_pairs
                .iter()
                .map(|(p, c)| (*c, Vector2::new(p.x - c.x, p.y - c.y)))
                .collect::<Vec<_>>(),
        ),
        scale: 1.0,
        extrapolated: 0,
        rejected: 0,
        rejection_note: String::new(),
    });
    app.calibration.field_accepted = true;
    app.calibration.lens_frame_signature = Some(((800, 800), Orientation::Normal));
    app
}

#[test]
fn accepted_field_composes_machine_overlay_and_uses_physical_place_projection() {
    let app = nonlinear_app();
    let projection = app.camera_projection((800, 800)).unwrap().unwrap();
    assert!(matches!(
        projection,
        CameraProjection::CommandedField { .. }
    ));
    let px = projection.to_px((40.0, 40.0)).unwrap();
    let mm = projection.from_px(px).unwrap();
    assert!((mm.0 - 40.0).abs() < 0.1 && (mm.1 - 40.0).abs() < 0.1);

    assert!(matches!(
        app.place_projection(800, 800).unwrap(),
        CameraProjection::PhysicalLens { .. }
    ));
}

/// The exclusion count survives a restart. A restored field map that only
/// passed because a dot was thrown away must not come back indistinguishable
/// from a clean pass — the same argument that persists the fit mode.
#[test]
fn the_excluded_dot_count_survives_a_restart() {
    let db = tmp_db();
    let blob = {
        let mut a = nonlinear_app_with_db(&db);
        let cal = a.calibration.field.as_mut().unwrap();
        cal.rejected = 2;
        a.calibration.field_accepted = true;
        std::fs::write(
            a.field_map_path(),
            a.calibration.field.as_ref().unwrap().field.serialize(),
        )
        .unwrap();
        a.save_settings_if_changed();
        a.settings_blob()
    };
    assert!(
        blob.lines()
            .any(|l| l.starts_with("field_stats=") && l.trim_end().ends_with(" 2")),
        "the count serializes as the 5th field_stats token:
{blob}"
    );

    let b = ConsoleApp::new(db, vec!["true".into()]);
    let restored = b.calibration.field.as_ref().expect("field restored");
    assert_eq!(restored.rejected, 2);
    assert!(
        restored.rejection_note.contains("EXCLUDED"),
        "note: {}",
        restored.rejection_note
    );
    assert!(b.debug_summary().contains("rejected=2"));
}

/// A field fit that only passed because a dot was excluded must not read the
/// same as one that passed outright: the count reaches `debug_summary`, and the
/// calib crate's sentence reaches the ③ block through the standing status.
#[test]
fn excluded_field_dots_are_reported_to_the_operator() {
    let mut app = nonlinear_app();
    assert!(app.debug_summary().contains("rejected=0"));

    {
        let cal = app.calibration.field.as_mut().unwrap();
        cal.rejected = 1;
        cal.dots[0].rejected = true;
        cal.rejection_note =
            "1 of 16 dots EXCLUDED from the fit as outliers (residual 2075 µm)".into();
    }
    app.calibration.field_accepted = true;
    app.calibration.mode = CalibMode::LaserField;
    app.runtime.tab = CentralTab::Calibrate;

    // Renders without panicking on a fit carrying excluded dots: the overlay's
    // strike-out path and the standing exclusion line. The strings themselves
    // are not asserted here — the ③ block lives inside a fixed-height
    // ScrollArea, so its text is clipped out of the shape list.
    let ctx = egui::Context::default();
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(!out.shapes.is_empty());
    assert!(app.debug_summary().contains("rejected=1"));
}

#[test]
fn place_with_no_calibration_at_all_has_no_projection() {
    let app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    let error = app.place_projection(800, 800).unwrap_err();
    assert!(error.contains("needs a projection"), "got: {error}");
}

/// With no projection at all, "⤵ Load design" still loads the geometry and
/// parks it in the middle of the WORK AREA — a position the operator can drag
/// from — instead of refusing because nothing can be mapped yet.
#[test]
fn load_design_without_any_calibration_centers_on_the_work_area() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.camera.field_center_auto = false;
    app.camera.field_cx_mm = 42.0;
    app.camera.field_cy_mm = 17.0;
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../cli/tests/fixtures");
    app.job.emit_copper = format!("{fixtures}/uv_test-F_Cu.gbr");
    app.job.emit_outline = format!("{fixtures}/uv_test-Edge_Cuts.gbr");
    app.load_place();
    assert!(!app.placement.job.is_empty(), "the design geometry loaded");
    assert!(
        (app.placement.tx_mm - 42.0).abs() < 1e-9 && (app.placement.ty_mm - 17.0).abs() < 1e-9,
        "parked on the work-area centre: ({}, {})",
        app.placement.tx_mm,
        app.placement.ty_mm
    );
    assert!(
        app.placement.note.contains("work area"),
        "note says where it went: {}",
        app.placement.note
    );
}

/// With a fiducial frame loaded, "⤵ Load design" starts the design in the
/// middle of THAT frame, mapped through the same projection the outline is
/// drawn with — so it appears where the operator is looking.
#[test]
fn load_design_centers_on_the_fiducial_frame_when_there_is_one() {
    let mut app = nonlinear_app();
    app.fiducials.frame_img = Some(image::GrayImage::new(800, 800));
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../cli/tests/fixtures");
    app.job.emit_copper = format!("{fixtures}/uv_test-F_Cu.gbr");
    app.job.emit_outline = format!("{fixtures}/uv_test-Edge_Cuts.gbr");
    app.load_place();
    let center = app.initial_center_mm(800.0, 800.0).unwrap();
    assert!(
        (app.placement.tx_mm - center.0).abs() < 1e-9
            && (app.placement.ty_mm - center.1).abs() < 1e-9,
        "started on the frame centre: ({}, {}) vs {center:?}",
        app.placement.tx_mm,
        app.placement.ty_mm
    );
    assert!(
        app.placement.note.contains("fiducial frame"),
        "note says where it went: {}",
        app.placement.note
    );
}

/// A saved ② laser anchor gives Place an approximate homography preview;
/// "Etch here" without ① + ③ exports UNWARPED geometry with a logged warning
/// (the operator's call — there is no hard gate).
#[test]
fn anchor_only_place_previews_and_exports_unwarped_with_warning() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.calibration.anchor = Some(calib::Calibration {
        px_to_mm: vision::Homography {
            matrix: nalgebra::Matrix3::new(0.1, 0.0, 0.0, 0.0, -0.1, 80.0, 0.0, 0.0, 1.0),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 0.0,
        found: 0,
        total: 49,
        dots: Vec::new(),
    });
    assert!(matches!(
        app.place_projection(800, 800).unwrap(),
        CameraProjection::Homography { .. }
    ));

    app.placement.job = vec![pcb_core::Poly::default()];
    app.fiducials.frame_img = Some(image::GrayImage::new(800, 800));
    app.job.emit_copper = "board.gbr".into();
    app.emit_at_placement(false);
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.err && l.text.contains("UNWARPED")),
        "the unwarped export is warned"
    );
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.text.contains("Etch here →")),
        "the export proceeds"
    );
}

#[test]
fn invalid_nonlinear_projection_fails_closed_without_homography_fallback() {
    let mut app = nonlinear_app();
    let field = &mut app.calibration.field.as_mut().unwrap().field;
    let mut coeffs = field.to_physical.to_coeffs();
    coeffs[0] = f64::NAN;
    field.to_physical = vision::Poly2::from_coeffs(&coeffs).expect("scale still valid");
    assert!(app.camera_projection((800, 800)).is_err());
    assert!(app.place_projection(800, 800).is_err());
}

#[test]
fn field_corrected_emit_with_a_missing_map_file_warns_and_emits_unwarped() {
    let mut app = nonlinear_app();
    app.placement.job = vec![pcb_core::Poly::default()];
    app.fiducials.frame_img = Some(image::GrayImage::new(800, 800));
    app.job.emit_copper = "board.gbr".into();
    assert!(!app.field_map_path().exists());
    app.emit_at_placement(false);
    assert!(!app.placement.field_correct, "field warp is not armed");
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.err && l.text.contains("UNWARPED")),
        "the unwarped export is warned"
    );
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.text.contains("Etch here →")),
        "the export proceeds"
    );
}

#[test]
fn place_export_always_arms_the_field_warp_when_the_map_exists() {
    let mut app = nonlinear_app();
    app.placement.job = vec![pcb_core::Poly::default()];
    app.fiducials.frame_img = Some(image::GrayImage::new(800, 800));
    app.job.emit_copper = "board.gbr".into();
    let map = app.calibration.field.as_ref().unwrap().field.serialize();
    std::fs::write(app.field_map_path(), map).unwrap();

    app.emit_at_placement(false);
    assert!(app.placement.field_correct);
    assert!(
        app.runtime
            .log
            .iter()
            .any(|line| !line.err && line.text.contains("field-warped geometry"))
    );
}

#[test]
fn job_emit_without_an_accepted_field_map_warns_and_emits_unwarped() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.job.emit_copper = "board.gbr".into();
    app.emit_clicked();
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.err && l.text.contains("UNWARPED")),
        "the unwarped emit is warned"
    );
}

#[test]
fn emit_passes_the_wobble_recipe_only_when_opted_in() {
    // `echo` as the CLI prints the verb args back on stdout, so the log shows
    // exactly what the button would run.
    let run_emit = |wobble: bool| -> String {
        let mut app = ConsoleApp::new(tmp_db(), vec!["echo".into()]);
        app.job.emit_copper = "board.gbr".into();
        app.job.wobble = wobble;
        app.job.wobble_step_mm = 0.05;
        app.job.wobble_size_mm = 0.2;
        app.emit_clicked();
        let ctx = Context::default();
        for _ in 0..500 {
            app.pump_verb(&ctx);
            if app.runtime.verb_job.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        app.runtime
            .log
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let on = run_emit(true);
    assert!(
        on.contains("--wobble")
            && on.contains("--wobble-step-mm 0.05")
            && on.contains("--wobble-size-mm 0.2"),
        "wobble recipe forwarded to the CLI:\n{on}"
    );
    let off = run_emit(false);
    assert!(
        !off.contains("--wobble"),
        "no wobble args when off (the CLI default already writes wobbleEnable=0):\n{off}"
    );
}

#[test]
fn camera_ui_reports_active_and_invalid_nonlinear_projection() {
    let mut app = nonlinear_app();
    app.runtime.tab = CentralTab::Camera;
    app.camera.show_bed = true;
    let ctx = Context::default();
    app.set_camera_frame(
        &ctx,
        image::GrayImage::from_pixel(800, 800, image::Luma([120])),
    );
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(!out.shapes.is_empty());
    assert!(
        app.debug_summary()
            .contains("camera_projection: field-warped-commanded")
    );

    let field = &mut app.calibration.field.as_mut().unwrap().field;
    let mut coeffs = field.to_physical.to_coeffs();
    coeffs[0] = f64::NAN;
    field.to_physical = vision::Poly2::from_coeffs(&coeffs).expect("scale still valid");
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(
        !out.shapes.is_empty(),
        "invalid state still renders its warning"
    );
    assert!(app.debug_summary().contains("camera_projection: invalid"));
}

#[test]
fn run_capture_captures_stdout_and_exit() {
    let out = run_capture(&["echo".into()], &["hello".into()]);
    assert!(out.iter().any(|l| l.text == "hello" && !l.err));
    assert!(out.iter().any(|l| l.text.starts_with("[exit 0]")));
}

#[test]
fn run_capture_reports_spawn_failure() {
    let out = run_capture(&["definitely-not-a-real-binary-xyz".into()], &[]);
    assert!(
        out.iter()
            .any(|l| l.err && l.text.contains("failed to run"))
    );
}

#[test]
fn build_preview_rejects_empty_copper() {
    assert!(preview_image("", "", 0.0).is_err());
}

/// Headless frame: the whole console lays out under a bare egui context
/// with no display and no panic, and produces render output.
#[test]
fn app_lays_out_one_frame_headless() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    let ctx = Context::default();
    let out = ctx.run(egui::RawInput::default(), |ctx| {
        app.ui(ctx);
    });
    // egui produced a tessellated frame (at least the panels' shapes).
    assert!(
        !out.shapes.is_empty(),
        "the console must render some shapes"
    );
}

/// The Fiducial-check tab lays out headless (form + summary + image slot).
#[test]
fn fiducial_tab_lays_out_headless() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Fiducials;
    let ctx = Context::default();
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(!out.shapes.is_empty(), "fiducial tab must render");
}

/// The Fiducial tab lays out with the Rect shape selected — the width/height
/// form branch renders (and the summary reports the rect footprint).
#[test]
fn fiducial_tab_lays_out_with_rect_shape() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Fiducials;
    app.fiducials.shape = crate::fiducial::ShapeKind::Rect;
    app.fiducials.diameter_mm = 2.0;
    app.fiducials.height_mm = 1.5;
    let ctx = Context::default();
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(
        !out.shapes.is_empty(),
        "rect-shape fiducial tab must render"
    );
    assert!(
        app.debug_summary().contains("shape=rect w=2 h=1.5"),
        "summary reports the rect footprint: {}",
        app.debug_summary()
    );
}

/// The Job tab lays out headless with the placement path fields it inherited
/// from the deleted Place tab, and the Actions panel carries the placement
/// controls that came with them.
#[test]
fn job_tab_lays_out_with_the_placement_paths_and_actions() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Job;
    let ctx = Context::default();
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(!out.shapes.is_empty(), "job tab must render");
}

/// The fiducial check runs straight off the camera: "Grab & check" pulls
/// one frame from the camera source (a File source here — the same
/// contract a capture app fulfills) and detects on it in one step, and
/// "Check fiducials" with no frame file falls back to the camera too.
#[test]
fn fiducial_check_grabs_from_the_camera() {
    let dir = std::env::temp_dir().join(format!("ui-fidgrab-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cam.png");
    let ppm = 10.0;
    let holes = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
    let img = image::GrayImage::from_fn(700, 700, |x, y| {
        let mut v = 170.0;
        for (mx, my) in holes {
            let (cx, cy) = (mx * ppm, 700.0 - my * ppm); // bed y-up
            if (((x as f64) - cx).powi(2) + ((y as f64) - cy).powi(2)).sqrt() < 0.5 * ppm {
                v -= 110.0;
            }
        }
        image::Luma([v as u8])
    });
    img.save(&path).unwrap();

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.camera.use_device = false;
    app.camera.file = path.to_string_lossy().into();
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.fiducials.px_per_mm = ppm;
    let ctx = Context::default();

    // One-click grab & check: frame installed and holes detected.
    app.grab_fid_frame(&ctx);
    assert!(app.fiducials.frame_img.is_some(), "camera frame installed");
    assert_eq!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count(),
        3,
        "detected the three holes off the camera grab: {:?}",
        app.fiducials.rows
    );

    // Check with no frame file set also reaches the camera.
    let mut app2 = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app2.camera.use_device = false;
    app2.camera.file = path.to_string_lossy().into();
    app2.fiducials.layout = "10,10; 60,10; 10,60".into();
    app2.fiducials.px_per_mm = ppm;
    assert!(app2.fiducials.frame.is_empty(), "no frame file configured");
    app2.render_fiducials(&ctx);
    assert_eq!(
        app2.fiducials.found.iter().filter(|f| f.is_some()).count(),
        3,
        "Check with no file fell back to the camera: {:?}",
        app2.fiducials.rows
    );

    // A dead camera source reports itself instead of panicking.
    let mut app3 = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app3.camera.use_device = false;
    app3.camera.file = String::new();
    app3.grab_fid_frame(&ctx);
    assert!(
        app3.fiducials.note.starts_with("camera:"),
        "camera error surfaced: {}",
        app3.fiducials.note
    );
}

/// The scale gate. The pose fit is a similarity, so holes at a uniformly wrong
/// spacing fit with a near-zero residual — `POSE_MAX_RMS_MM` waves them
/// through. Only the scale band stops them, and it has to, because applying the
/// fit would resize the burn by that same factor. The placement must be left
/// exactly as it was and the note must name the measured scale.
#[test]
fn an_implausible_fiducial_scale_leaves_the_placement_alone() {
    use nalgebra::Matrix3;
    let dir = std::env::temp_dir().join(format!("ui-fidscale-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cam.png");
    let ppm = 10.0;
    // Nominal layout, and the holes actually drilled at 0.85× that spacing
    // about the layout centroid — a 15% shrink, well outside the band.
    let layout = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
    let c = (80.0 / 3.0, 80.0 / 3.0);
    let holes: Vec<(f64, f64)> = layout
        .iter()
        .map(|&(x, y)| (c.0 + 0.85 * (x - c.0), c.1 + 0.85 * (y - c.1)))
        .collect();
    let img = image::GrayImage::from_fn(700, 700, |x, y| {
        let mut v = 170.0;
        for &(mx, my) in &holes {
            let (cx, cy) = (mx * ppm, 700.0 - my * ppm); // bed y-up
            if (((x as f64) - cx).powi(2) + ((y as f64) - cy).powi(2)).sqrt() < 0.5 * ppm {
                v -= 110.0;
            }
        }
        image::Luma([v as u8])
    });
    img.save(&path).unwrap();

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.camera.use_device = false;
    app.camera.file = path.to_string_lossy().into();
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.fiducials.px_per_mm = ppm;
    // Wide enough that each nominal window still contains its shifted hole —
    // the point of the test is the GATE, not the detector's reach.
    app.fiducials.search_mm = 7.0;
    app.calibration.anchor = Some(calib::Calibration {
        px_to_mm: vision::Homography {
            matrix: Matrix3::new(
                1.0 / ppm,
                0.0,
                0.0,
                0.0,
                -1.0 / ppm,
                700.0 / ppm,
                0.0,
                0.0,
                1.0,
            ),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 10.0,
        found: 3,
        total: 3,
        dots: Vec::new(),
    });
    // A placement the operator set by hand; the refused fit must not touch it.
    app.placement.tx_mm = 11.0;
    app.placement.ty_mm = 22.0;
    app.placement.rot_deg = 3.0;
    app.placement.scale = 1.0;

    app.grab_fid_frame(&Context::default());
    assert_eq!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count(),
        3,
        "all three shrunken holes detected: {:?}",
        app.fiducials.rows
    );
    assert!(
        app.fiducials.note.contains("scale") && app.fiducials.note.contains("not updated"),
        "the note names the measured scale and refuses: {}",
        app.fiducials.note
    );
    assert!(
        !app.placement.auto_pose && !app.fiducials.last_placed,
        "nothing was placed"
    );
    assert_eq!(
        (
            app.placement.tx_mm,
            app.placement.ty_mm,
            app.placement.rot_deg,
            app.placement.scale
        ),
        (11.0, 22.0, 3.0, 1.0),
        "the manual placement is untouched"
    );
}

/// After a fiducial Check, the Place tab's placement is set from the detected
/// holes (rotation + translation) and flagged `auto_pose`; a subsequent Load
/// must NOT recenter over it, and switching side clears the flag.
#[test]
fn fiducial_check_sets_placement_and_load_preserves_it() {
    use nalgebra::Matrix3;
    let dir = std::env::temp_dir().join(format!("ui-fidpose-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cam.png");
    let ppm = 10.0;
    let holes = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
    let img = image::GrayImage::from_fn(700, 700, |x, y| {
        let mut v = 170.0;
        for (mx, my) in holes {
            let (cx, cy) = (mx * ppm, 700.0 - my * ppm); // bed y-up
            if (((x as f64) - cx).powi(2) + ((y as f64) - cy).powi(2)).sqrt() < 0.5 * ppm {
                v -= 110.0;
            }
        }
        image::Luma([v as u8])
    });
    img.save(&path).unwrap();

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.camera.use_device = false;
    app.camera.file = path.to_string_lossy().into();
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.fiducials.px_per_mm = ppm;
    // Homography-fallback place_projection matching the frame's px↔mm map
    // (mm_x = px_x/ppm, mm_y = (H − px_y)/ppm), so from_px recovers machine mm.
    app.calibration.anchor = Some(calib::Calibration {
        px_to_mm: vision::Homography {
            matrix: Matrix3::new(
                1.0 / ppm,
                0.0,
                0.0,
                0.0,
                -1.0 / ppm,
                700.0 / ppm,
                0.0,
                0.0,
                1.0,
            ),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 10.0,
        found: 3,
        total: 3,
        dots: Vec::new(),
    });
    // Real Gerbers so Load installs a job (and would recenter without the guard).
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../cli/tests/fixtures");
    app.job.emit_copper = format!("{fixtures}/uv_test-F_Cu.gbr");
    app.job.emit_outline = format!("{fixtures}/uv_test-Edge_Cuts.gbr");
    let ctx = Context::default();

    // The pose portion of the `place:` line (x/y/rot/auto_pose) — stable across
    // rounding, so equality before/after Load proves preservation.
    let place_pose = |app: &ConsoleApp| {
        let s = app.debug_summary();
        let line = s
            .lines()
            .find(|l| l.trim_start().starts_with("place:"))
            .unwrap()
            .to_string();
        line[..line.find(" job_polys=").unwrap()].trim().to_string()
    };

    // Detection maps px→machine mm, fits the (identity) pose, and writes it.
    app.grab_fid_frame(&ctx);
    assert_eq!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count(),
        3,
        "three holes detected: {:?}",
        app.fiducials.rows
    );
    assert!(app.placement.auto_pose, "placement flagged auto-posed");
    // Identity front pose → tx=ty=layout centroid (26.67), NOT the frame center
    // (35) a recenter would give; rotation ~0.
    assert!(
        (app.placement.tx_mm - 80.0 / 3.0).abs() < 0.15
            && (app.placement.ty_mm - 80.0 / 3.0).abs() < 0.15,
        "placed at the fiducial centroid: ({}, {})",
        app.placement.tx_mm,
        app.placement.ty_mm
    );
    assert!(
        app.placement.rot_deg.abs() < 0.5,
        "rot ~0: {}",
        app.placement.rot_deg
    );
    let pose1 = place_pose(&app);
    assert!(pose1.ends_with("auto_pose=true"), "line: {pose1}");
    assert!(
        app.debug_summary().contains("fid_pose: rot="),
        "fid_pose line is populated from the cached pose: {}",
        app.debug_summary()
    );
    assert!(
        app.fiducials.note.contains("placement set from fiducials"),
        "note reports the auto-place: {}",
        app.fiducials.note
    );
    let (tx1, ty1, rot1) = (
        app.placement.tx_mm,
        app.placement.ty_mm,
        app.placement.rot_deg,
    );

    // Load installs the job but must NOT recenter over the auto pose.
    app.load_place();
    assert!(app.placement.auto_pose, "Load kept the auto-pose flag");
    assert!(
        (app.placement.tx_mm - tx1).abs() < 1e-9
            && (app.placement.ty_mm - ty1).abs() < 1e-9
            && (app.placement.rot_deg - rot1).abs() < 1e-9,
        "Load preserved the pose values"
    );
    assert_eq!(
        place_pose(&app),
        pose1,
        "place line's pose portion unchanged"
    );

    // Switching side clears the auto-pose flag.
    app.set_side(Side::Back);
    assert!(!app.placement.auto_pose, "set_side cleared auto_pose");
    assert!(
        place_pose(&app).ends_with("auto_pose=false"),
        "place line reflects the cleared flag: {}",
        place_pose(&app)
    );

    std::fs::remove_dir_all(dir).ok();
}

/// A synthetic bed frame with dark holes at `holes` (bed mm, y-up), saved to
/// `path` at `ppm` px/mm on a 700×700 field — the fixture the fiducial tests
/// grab/load through the real camera + file paths.
fn write_hole_frame(path: &std::path::Path, ppm: f64, holes: &[(f64, f64)]) {
    let img = image::GrayImage::from_fn(700, 700, |x, y| {
        let mut v = 170.0;
        for &(mx, my) in holes {
            let (cx, cy) = (mx * ppm, 700.0 - my * ppm); // bed y-up
            if (((x as f64) - cx).powi(2) + ((y as f64) - cy).powi(2)).sqrt() < 0.5 * ppm {
                v -= 110.0;
            }
        }
        image::Luma([v as u8])
    });
    img.save(path).unwrap();
}

/// The marking round (Calibrate-style click-in-order): a file Load opens the
/// round at marker 0, `fid_mark_click` advances marker-by-marker WITHOUT
/// detecting, and only the FINAL click closes the round and runs detection.
/// "reset markers" reopens the round.
#[test]
fn marking_round_walks_the_fiducials_and_the_final_click_detects() {
    let dir = std::env::temp_dir().join(format!("ui-fidmark-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bed3.png");
    let ppm = 10.0;
    let holes = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
    write_hole_frame(&path, ppm, &holes);

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Fiducials;
    app.fiducials.frame = path.to_string_lossy().into();
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.fiducials.px_per_mm = ppm;
    let ctx = Context::default();

    // A file Load opens the round at marker 0 and prompts for the first click;
    // nothing is detected yet.
    app.load_fid_frame(&ctx);
    assert_eq!(
        app.fiducials.marking,
        Some(0),
        "load opens the marking round"
    );
    assert!(
        app.fiducials.note.starts_with("click fiducial 1 of 3"),
        "note prompts the first marker: {}",
        app.fiducials.note
    );
    assert!(
        app.fiducials.found.iter().all(Option::is_none) && app.fiducials.rows.is_empty(),
        "no detection before the round completes"
    );

    // Marking the first two holes advances the round but must NOT detect yet.
    app.fid_mark_click(holes[0]);
    assert_eq!(app.fiducials.marking, Some(1), "advanced to marker 1");
    assert!(
        app.fiducials.note.starts_with("click fiducial 2 of 3"),
        "note advanced: {}",
        app.fiducials.note
    );
    app.fid_mark_click(holes[1]);
    assert_eq!(app.fiducials.marking, Some(2), "advanced to marker 2");
    assert!(
        app.fiducials.rows.is_empty(),
        "detection has not run before the final click: {:?}",
        app.fiducials.rows
    );

    // The final click closes the round and runs detection on the marked holes.
    app.fid_mark_click(holes[2]);
    assert_eq!(
        app.fiducials.marking, None,
        "round closed on the final click"
    );
    assert_eq!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count(),
        3,
        "the final click detected the three holes: {:?}",
        app.fiducials.rows
    );

    // "reset markers" reopens the round from marker 0.
    app.reset_fid_markers();
    assert_eq!(app.fiducials.marking, Some(0), "reset reopens the round");
    assert_eq!(
        app.fiducials.search.len(),
        3,
        "markers reseeded from the layout"
    );

    std::fs::remove_dir_all(dir).ok();
}

/// "clear markers" removes EVERY expected fiducial — layout included — so a
/// piled-up marker set (e.g. 15 from click-to-place) actually goes to zero and
/// stays there: reset and sync must find nothing to reseed from afterwards.
#[test]
fn clear_markers_empties_the_layout_so_nothing_reseeds() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.sync_fid_markers();
    app.fiducials.marking = Some(1);
    assert_eq!(app.fiducials.search.len(), 3);

    app.clear_fid_markers();
    assert!(
        app.fiducials.layout.is_empty(),
        "layout emptied — it is the reseed source"
    );
    assert!(app.fiducials.search.is_empty(), "all ✛ markers gone");
    assert!(app.fiducials.found.is_empty() && app.fiducials.rows.is_empty());
    assert_eq!(app.fiducials.marking, None, "any active round is cancelled");

    // Neither the per-frame sync nor an explicit reset may bring them back.
    app.sync_fid_markers();
    assert!(
        app.fiducials.search.is_empty(),
        "sync must not reseed cleared markers"
    );
    app.reset_fid_markers();
    assert!(
        app.fiducials.search.is_empty() && app.fiducials.marking.is_none(),
        "reset after clear has nothing to reseed and opens no round"
    );

    // ⟳ layout from W×H is the ONE way back — an explicit rebuild, not a reset
    // that silently resurrects a cleared set.
    app.apply_fid_rect();
    assert_eq!(
        app.fiducials.search.len(),
        4,
        "the rectangle's four corners are rebuilt: {:?}",
        app.fiducials.layout
    );
}

/// A camera Grab auto-detects at the seeded positions, so it must NOT open a
/// marking round — the round is the Load/reset (no-auto-detect) path only.
#[test]
fn grab_does_not_open_a_marking_round() {
    let dir = std::env::temp_dir().join(format!("ui-fidmarkgrab-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cam.png");
    let ppm = 10.0;
    let holes = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
    write_hole_frame(&path, ppm, &holes);

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.camera.use_device = false;
    app.camera.file = path.to_string_lossy().into();
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.fiducials.px_per_mm = ppm;
    let ctx = Context::default();

    app.grab_fid_frame(&ctx);
    assert_eq!(
        app.fiducials.marking, None,
        "grab must not open a marking round (it auto-detects instead)"
    );
    assert_eq!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count(),
        3,
        "grab still auto-detected the holes: {:?}",
        app.fiducials.rows
    );
    assert!(
        app.debug_summary().contains("marking=-"),
        "summary reports no active round: {}",
        app.debug_summary()
    );

    std::fs::remove_dir_all(dir).ok();
}

/// Clicks are the only placement gesture: a plain canvas click with NO active
/// round implicitly opens one — placing marker 0 where it lands and advancing
/// to marker 1 — so the operator no longer needs Load/reset to start marking.
#[test]
fn a_plain_click_with_no_round_implicitly_starts_marking() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.sync_fid_markers();
    assert_eq!(app.fiducials.marking, None, "no round active to begin with");

    app.fid_mark_click((12.0, 9.0));
    assert_eq!(
        app.fiducials.marking,
        Some(1),
        "the click implicitly opened the round and advanced to marker 1"
    );
    assert_eq!(
        app.fiducials.search[0],
        (12.0, 9.0),
        "the click placed marker 0 where it landed"
    );
    assert!(
        app.fiducials.note.starts_with("click fiducial 2 of 3"),
        "note advanced to the second marker: {}",
        app.fiducials.note
    );
}

/// Dragging one ✛ touches ONLY that search marker. The layout is the design
/// nominal the pose fit and the measured px/mm are taken against, so a dragged
/// position must never leak into it (LR-17) — and the dragged marker's old
/// detection is stale the moment it moves.
#[test]
fn dragging_a_marker_moves_only_its_own_search_entry() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.sync_fid_markers();
    let layout_before = app.fiducials.layout.clone();
    let search_before = app.fiducials.search.clone();
    app.fiducials.found = vec![
        Some((100.0, 600.0)),
        Some((600.0, 600.0)),
        Some((100.0, 100.0)),
    ];

    // Two frames of one gesture: the nudges accumulate on the grabbed marker.
    app.fiducials.marker_drag = Some(1);
    app.fid_drag_marker(1, (0.8, -0.3));
    app.fid_drag_marker(1, (0.2, -0.2));

    assert_eq!(
        app.fiducials.layout, layout_before,
        "the expected layout is byte-identical after a drag"
    );
    let (x, y) = app.fiducials.search[1];
    assert!(
        (x - (search_before[1].0 + 1.0)).abs() < 1e-9
            && (y - (search_before[1].1 - 0.5)).abs() < 1e-9,
        "marker 1 moved by the summed delta: {:?}",
        app.fiducials.search[1]
    );
    assert_eq!(
        (app.fiducials.search[0], app.fiducials.search[2]),
        (search_before[0], search_before[2]),
        "the other markers stayed put"
    );
    assert_eq!(
        app.fiducials.found[1], None,
        "the dragged marker's detection is stale and cleared"
    );
    assert!(
        app.fiducials.found[0].is_some() && app.fiducials.found[2].is_some(),
        "the other detections survive"
    );

    // A layout edit can shrink the ✛ set under an in-flight drag; the latched
    // index must not index past it.
    app.fiducials.marker_drag = Some(7);
    app.fid_drag_marker(7, (1.0, 1.0));
    assert_eq!(
        app.fiducials.marker_drag, None,
        "an out-of-range latch is dropped, not indexed"
    );
}

/// A drag that grabbed a ✛ owns the whole gesture: it must not also drop the
/// next marker of a marking round, add a click-to-place fiducial, or remove one
/// on right-click. Same latch-and-gate shape as the design drag.
#[test]
fn a_marker_drag_suppresses_the_marking_and_click_to_place_paths() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.sync_fid_markers();
    assert!(app.fid_marking_allowed(), "idle canvas marks as usual");

    app.fiducials.marker_drag = Some(0);
    assert!(
        !app.fid_marking_allowed(),
        "a grabbed marker suppresses marking / click-to-place"
    );
    app.fiducials.marker_drag = None;
    app.fiducials.design_drag = true;
    assert!(
        !app.fid_marking_allowed(),
        "the design latch still suppresses them too"
    );

    // The hit test the grab runs: nearest ✛ within the grab radius, nothing
    // outside it.
    use super::fiducial_ui::MARKER_GRAB_PX;
    let markers = [(100.0_f32, 100.0_f32), (140.0, 100.0)];
    assert_eq!(
        crate::fiducial::nearest_marker(&markers, (108.0, 104.0), MARKER_GRAB_PX),
        Some(0),
        "a press near marker 0 grabs it"
    );
    assert_eq!(
        crate::fiducial::nearest_marker(&markers, (125.0, 100.0), MARKER_GRAB_PX),
        Some(1),
        "between the two, the NEAREST inside the radius wins"
    );
    assert_eq!(
        crate::fiducial::nearest_marker(&markers, (120.0, 100.0), MARKER_GRAB_PX),
        None,
        "a press outside every marker's radius grabs nothing"
    );
}

/// The point of the gesture: nudge a marker the detector missed onto its hole,
/// let go, and the check re-runs and finds it — without redoing the other three
/// and without the ladder seeding over the correction.
#[test]
fn releasing_a_dragged_marker_rechecks_and_keeps_the_manual_position() {
    let dir = std::env::temp_dir().join(format!("ui-fiddrag-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bed.png");
    let ppm = 10.0;
    let holes = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0), (60.0, 60.0)];
    write_hole_frame(&path, ppm, &holes);

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.frame = path.to_string_lossy().into();
    app.fiducials.layout = "10,10; 60,10; 10,60; 60,60".into();
    app.fiducials.px_per_mm = ppm;
    app.fiducials.search_mm = 2.0;
    let ctx = Context::default();
    app.load_fid_frame(&ctx);

    // Marker 2 sits 3 mm off its hole — well outside the 2 mm search window —
    // so a check finds the other three and misses that one.
    app.fiducials.search[2] = (7.0, 60.0);
    app.render_fiducials(&ctx);
    assert_eq!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count(),
        3,
        "the offset marker misses: {:?}",
        app.fiducials.rows
    );
    assert_eq!(
        app.fiducials.found[2], None,
        "and it is marker 2 that missed"
    );
    let layout_before = app.fiducials.layout.clone();

    // Drag it onto the hole and let go.
    app.fiducials.marker_drag = Some(2);
    app.fid_drag_marker(2, (2.0, 0.0));
    app.fid_drag_marker(2, (1.0, 0.0));
    app.fid_marker_drag_release();

    assert_eq!(
        app.fiducials.marker_drag, None,
        "the latch is released with the pointer"
    );
    assert_eq!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count(),
        4,
        "the release re-checked and the nudged marker locked: {:?}",
        app.fiducials.rows
    );
    // The detection ladder installs the winning stage's seeds, so this is the
    // assertion that the manual placement was kept rather than seeded over.
    assert_eq!(
        app.fiducials.search[2],
        (10.0, 60.0),
        "the dragged position survives the re-check"
    );
    assert_eq!(
        app.fiducials.layout, layout_before,
        "the expected layout is untouched by the whole gesture"
    );

    std::fs::remove_dir_all(dir).ok();
}

/// FLD-11: live tracking pulls frames from the camera source and re-detects
/// each one — the found rings and the perspective fit update without a
/// manual Check. Verified with a File source of 4 holes.
#[test]
fn live_fiducial_tracking_detects_on_the_feed() {
    let dir = std::env::temp_dir().join(format!("ui-fidlive-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bed4.png");
    let ppm = 10.0;
    let holes = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0), (60.0, 60.0)];
    let img = image::GrayImage::from_fn(700, 700, |x, y| {
        let mut v = 150.0;
        for (mx, my) in holes {
            let (cx, cy) = (mx * ppm, 700.0 - my * ppm); // bed y-up
            if (((x as f64) - cx).powi(2) + ((y as f64) - cy).powi(2)).sqrt() < 0.5 * ppm {
                v -= 90.0;
            }
        }
        image::Luma([v as u8])
    });
    img.save(&path).unwrap();

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Fiducials;
    app.camera.use_device = false;
    app.camera.file = path.to_string_lossy().into();
    app.fiducials.layout = "10,10; 60,10; 10,60; 60,60".into();
    app.fiducials.px_per_mm = ppm;
    app.fiducials.live = true;
    let ctx = Context::default();
    for _ in 0..500 {
        app.pump_fid_live(&ctx);
        if app.fiducials.found.iter().filter(|f| f.is_some()).count() >= 4 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    assert!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count() >= 4,
        "live tracking detected the four holes: {:?}",
        app.fiducials.rows
    );
    assert!(
        app.fiducials.homography.is_some(),
        "perspective fitted from 4 live fiducials"
    );

    // Live off: this tab stops pumping, but the shared capture is the console's
    // and survives until the idle rule releases it (another tab may be live).
    app.fiducials.live = false;
    app.pump_fid_live(&ctx);
    app.runtime.camera_last_used = Some(std::time::Instant::now() - CAMERA_IDLE_RELEASE);
    app.release_idle_capture();
    assert!(
        app.runtime.camera_capture.is_none(),
        "the shared capture is released once no tab is live and it has gone idle"
    );
}

/// One capture, shared: the tabs' pumps reuse the same thread instead of each
/// opening the source, and a source change restarts it. (`File` source — the
/// device timings this exists for need real hardware and aren't testable here.)
#[test]
fn shared_capture_is_reused_and_restarts_on_source_change() {
    let dir = std::env::temp_dir().join(format!("ui-sharedcap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.png");
    let b = dir.join("b.png");
    image::GrayImage::from_pixel(20, 10, image::Luma([90]))
        .save(&a)
        .unwrap();
    image::GrayImage::from_pixel(30, 12, image::Luma([90]))
        .save(&b)
        .unwrap();

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.camera.use_device = false;
    app.camera.file = a.to_string_lossy().into();

    app.ensure_capture();
    let first = app.runtime.camera_capture.as_ref().unwrap() as *const _;
    app.ensure_capture();
    assert_eq!(
        app.runtime.camera_capture.as_ref().unwrap() as *const _,
        first,
        "an unchanged source reuses the running capture"
    );

    // Every tab pulls from that same capture.
    let ctx = Context::default();
    app.camera.live = true;
    app.fiducials.live = true;
    app.pump_camera(&ctx);
    app.pump_fid_live(&ctx);
    assert_eq!(
        app.runtime.camera_capture.as_ref().unwrap() as *const _,
        first,
        "the live pumps share one capture rather than starting their own"
    );
    assert_eq!(
        app.runtime.camera_capture_src,
        Some(crate::camera::Source::File(a.to_string_lossy().into())),
        "still streaming the source it was started for"
    );

    // Switching source restarts it — checked by the frames that arrive, since a
    // freed capture's address can be handed straight back to the new one.
    app.camera.file = b.to_string_lossy().into();
    app.ensure_capture();
    assert_eq!(
        app.runtime.camera_capture_src,
        Some(crate::camera::Source::File(b.to_string_lossy().into()))
    );
    let mut dims = None;
    for _ in 0..200 {
        if let Some(Ok(f)) = app.capture_latest() {
            dims = Some(f.dimensions());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        dims,
        Some((30, 12)),
        "the restarted capture streams the new source"
    );
}

/// The idle-release rule: hold the device while any tab streams, hand it back
/// once nothing does and it has gone quiet — the CLI can't open a busy camera.
#[test]
fn idle_release_holds_while_live_and_frees_when_quiet() {
    let now = std::time::Instant::now();
    let idle = CAMERA_IDLE_RELEASE;
    let fresh = Some(now - idle / 2);
    let stale = Some(now - idle * 2);

    assert!(
        !should_release_capture(true, stale, now, idle),
        "a live tab keeps the device however long ago the last read was"
    );
    assert!(
        !should_release_capture(false, fresh, now, idle),
        "a recent grab keeps the fast path warm for the next one"
    );
    assert!(
        should_release_capture(false, stale, now, idle),
        "nothing live and long idle: release"
    );
    assert!(
        should_release_capture(false, None, now, idle),
        "a capture nobody ever read is released, not held"
    );
}

/// FLD-9: a verb runs on a background thread (run_verb returns at once),
/// streams its output, and completing clears the job + refreshes status.
#[test]
fn run_verb_is_nonblocking_and_streams() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["echo".into()]);
    app.run_verb(&["streamed".into()]);
    assert!(
        app.runtime.verb_job.is_some(),
        "run_verb returned immediately"
    );
    let ctx = Context::default();
    for _ in 0..500 {
        app.pump_verb(&ctx);
        if app.runtime.verb_job.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    assert!(app.runtime.verb_job.is_none(), "job completed and cleared");
    assert!(
        app.runtime.log.iter().any(|l| l.text == "streamed"),
        "stdout streamed"
    );
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.text.starts_with("[exit 0]")),
        "exit footer logged"
    );
}

#[test]
fn spawn_verb_reports_stderr_and_exit() {
    // `sh -c 'echo out; echo err 1>&2; exit 3'` exercises both streams.
    let job = spawn_verb(
        &["sh".into()],
        &["-c".into(), "echo out; echo err 1>&2; exit 3".into()],
    );
    let mut lines = Vec::new();
    for _ in 0..500 {
        lines.extend(job.drain());
        if job.finished() {
            lines.extend(job.drain());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    assert!(lines.iter().any(|l| l.text == "out" && !l.err));
    assert!(lines.iter().any(|l| l.text == "err" && l.err));
    assert!(lines.iter().any(|l| l.text.contains("[exit 3]") && l.err));
}

/// The marker set tracks the layout field: adding a coordinate adds a
/// marker (seeded from the layout), removing one drops it, and existing
/// placed positions are preserved.
#[test]
fn markers_follow_the_layout_field() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.sync_fid_markers();
    assert_eq!(app.fiducials.search.len(), 3);

    app.fiducials.search[0] = (11.5, 9.0); // move marker 0
    app.fiducials.layout = "10,10; 60,10; 10,60; 60,60".into();
    app.sync_fid_markers();
    assert_eq!(app.fiducials.search.len(), 4, "4th marker appears");
    assert_eq!(app.fiducials.search[0], (11.5, 9.0), "placed position kept");
    assert_eq!(
        app.fiducials.search[3],
        (60.0, 60.0),
        "4th seeded from layout"
    );

    app.fiducials.layout = "10,10; 60,10".into();
    app.sync_fid_markers();
    assert_eq!(
        app.fiducials.search.len(),
        2,
        "removing coords drops markers"
    );
}

/// A single-fiducial layout: one plain click both opens and closes the round —
/// it places the lone marker where it lands and immediately detects. A click at
/// the design nominal misses (the hole is 3 mm off); a fresh click (with the
/// round already closed, so it implicitly reopens) landing on the hole finds it.
#[test]
fn clicking_the_lone_marker_places_and_detects() {
    let dir = std::env::temp_dir().join(format!("ui-lone-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hole.png");
    // One dark hole at bed (13,10) mm → px (130, 160−100=60) at 10 px/mm
    // (bed y-up from the frame's bottom-left).
    let ppm = 10.0;
    let (hx, hy) = (13.0 * ppm, 160.0 - 10.0 * ppm);
    let img = image::GrayImage::from_fn(220, 160, |x, y| {
        let bg = 150.0;
        let d = (((x as f64) - hx).powi(2) + ((y as f64) - hy).powi(2)).sqrt();
        let v = if d < 0.5 * ppm { bg - 90.0 } else { bg };
        image::Luma([v as u8])
    });
    img.save(&path).unwrap();

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Fiducials;
    app.fiducials.frame = path.to_string_lossy().into();
    app.fiducials.layout = "10,10".into(); // design nominal, 3 mm from the hole
    app.fiducials.px_per_mm = 10.0;
    app.fiducials.diameter_mm = 1.0;
    app.fiducials.search_mm = 2.0;
    let ctx = Context::default();

    app.load_fid_frame(&ctx);
    assert_eq!(
        app.fiducials.search,
        vec![(10.0, 10.0)],
        "markers seed from design"
    );
    assert_eq!(
        app.fiducials.marking,
        Some(0),
        "the 1-marker layout opens a round at marker 0"
    );

    // Click at the design nominal: the lone click closes the round and detects,
    // but the hole is 3 mm off so nothing is found there.
    app.fid_mark_click((10.0, 10.0));
    assert_eq!(
        app.fiducials.marking, None,
        "the lone click closed the round"
    );
    assert!(
        app.fiducials.found[0].is_none(),
        "misses at nominal (hole is 3 mm off)"
    );

    // A fresh click with no active round implicitly reopens it; landing on the
    // actual hole makes detection lock on.
    app.fid_mark_click((13.0, 10.0));
    assert_eq!(
        app.fiducials.marking, None,
        "the reopened lone click closed again"
    );
    assert!(
        app.fiducials.found[0].is_some(),
        "found the hole the click landed on"
    );
}

/// The Camera tab lays out headless, a File-source grab loads a texture,
/// and Snapshot points the Fiducial + Place tabs at the saved frame.
#[test]
fn camera_grab_and_snapshot_flow() {
    let dir = std::env::temp_dir().join(format!("ui-camflow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let frame = dir.join("live.png");
    image::GrayImage::from_pixel(48, 32, image::Luma([90]))
        .save(&frame)
        .unwrap();

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Camera;
    app.camera.use_device = false;
    app.camera.file = format!("\"{}\"", frame.display()); // quoted on purpose
    let ctx = Context::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));

    app.grab_camera(&ctx);
    assert!(app.camera.tex.is_some(), "grab loaded a texture");
    assert_eq!(app.camera.last.as_ref().unwrap().dimensions(), (48, 32));

    app.snapshot_to_tabs();
    assert!(app.fiducials.frame.ends_with("pcbforge-snapshot.png"));
    assert_eq!(app.fiducials.frame, app.placement.frame);
    assert!(std::path::Path::new(&app.fiducials.frame).is_file());
}

/// FLD-12: click-to-place appends an expected fiducial to the layout and a
/// matching search marker, keeping the layout string as source of truth.
#[test]
fn click_to_place_appends_an_expected_fiducial() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "10,10; 60,10".into();
    app.sync_fid_markers();
    assert_eq!(app.fiducials.search.len(), 2);

    app.add_expected_fiducial(60.0, 60.0);
    assert_eq!(app.fiducials.search.len(), 3, "a 3rd marker appeared");
    assert_eq!(app.fiducials.search[2], (60.0, 60.0), "seeded at the click");
    assert!(
        app.fiducials.layout.contains("60.0,60.0"),
        "layout carries the added point: {}",
        app.fiducials.layout
    );

    // Removal shrinks the set (fixes the add-only pile-up): drop the middle
    // marker and the aligned layout token + search/found entries go with it.
    app.fiducials.layout = "10,10; 60,10; 60,60".into();
    app.sync_fid_markers();
    // Fine-tune the 3rd marker's position, so we can prove removal keeps the
    // *other* markers' placed positions aligned by index.
    app.fiducials.search[2] = (61.5, 59.0);
    app.remove_expected_fiducial(1); // remove the (60,10) middle one
    assert_eq!(app.fiducials.search.len(), 2, "one fewer marker");
    assert!(
        !app.fiducials.layout.contains("60,10"),
        "removed token is gone: {}",
        app.fiducials.layout
    );
    assert!(app.fiducials.layout.contains("10,10") && app.fiducials.layout.contains("60,60"));
    assert_eq!(
        app.fiducials.search[1],
        (61.5, 59.0),
        "the survivor's placed position stayed aligned to its token"
    );

    // Appending onto an empty layout doesn't produce a leading separator.
    app.fiducials.layout = String::new();
    app.fiducials.search.clear();
    app.fiducials.found.clear();
    app.add_expected_fiducial(5.0, 7.0);
    assert_eq!(app.fiducials.layout.trim_start(), "5.0,7.0");
}

/// FLD-12: the selected profile flows into detection. A backlit frame
/// (bright dots on a dark field) is found with the Backlit profile but the
/// dark-dot matcher does not lock onto it — proving the selector is wired.
#[test]
fn profile_selector_changes_detection_polarity() {
    let ppm = 10.0;
    let holes = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
    // Bright blobs on a dark field: inverted polarity vs a drilled hole.
    let img = image::GrayImage::from_fn(700, 700, |x, y| {
        let mut v = 40.0;
        for (mx, my) in holes {
            let (cx, cy) = (mx * ppm, 700.0 - my * ppm); // bed y-up
            if (((x as f64) - cx).powi(2) + ((y as f64) - cy).powi(2)).sqrt() < 0.5 * ppm {
                v += 170.0;
            }
        }
        image::Luma([v as u8])
    });

    let backlit = fiducial::check_frame(
        &img,
        &holes,
        ppm,
        &crate::fiducial::ProfileKind::Backlit
            .to_profile(vision::FidShape::Circle { diameter_mm: 1.0 }),
        2.0,
    );
    assert_eq!(backlit.tally.0, 3, "backlit finds the bright blobs");

    let darkdot = fiducial::check_frame(
        &img,
        &holes,
        ppm,
        &crate::fiducial::ProfileKind::DarkDot
            .to_profile(vision::FidShape::Circle { diameter_mm: 1.0 }),
        2.0,
    );
    assert!(
        darkdot.tally.0 < 3,
        "dark-dot matcher does not strongly lock the bright blobs: {:?}",
        darkdot.rows
    );
}

/// UI-2: the AR overlay blends the registered design over a frame. With a
/// homography mapping design-mm → px, a copper region lands (tinted) at the
/// mapped pixel; with the overlay off, the frame stays untouched gray.
#[test]
fn ar_overlay_projects_design_through_the_homography() {
    use nalgebra::Matrix3;
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    // A single 4 mm copper square centered at design (10,10) mm.
    let mm = pcb_core::NM_PER_MM;
    let sq = pcb_core::Poly {
        outer: vec![
            pcb_core::P::new(8 * mm, 8 * mm),
            pcb_core::P::new(12 * mm, 8 * mm),
            pcb_core::P::new(12 * mm, 12 * mm),
            pcb_core::P::new(8 * mm, 12 * mm),
        ],
        holes: vec![],
    };
    app.ar.copper = vec![sq];
    app.ar.show_copper = true;
    app.ar.show_board = false;
    app.ar.show_ablate = false;
    // Pure 5 px/mm scale: design (10,10) mm → px (50,50).
    app.fiducials.homography = Some(vision::Homography {
        matrix: Matrix3::new(5.0, 0.0, 0.0, 0.0, 5.0, 0.0, 0.0, 0.0, 1.0),
        residuals: vec![],
        rms: 0.0,
    });

    let gray = image::GrayImage::from_pixel(200, 200, image::Luma([120]));
    let over = app.compose_ar(&gray);
    let at = |x: usize, y: usize| over.pixels[y * 200 + x];
    // The copper square maps to px (40,40)..(60,60); its outline (left edge
    // x=40) is crisply tinted, and the interior is at least softly filled.
    assert!(at(40, 50).r() > 150, "copper outline at the mapped edge");
    assert!(at(50, 50).r() > 120, "copper interior softly filled");
    assert_eq!(
        at(150, 150),
        Color32::from_gray(120),
        "far corner untouched"
    );

    // A disabled layer leaves the frame gray.
    app.ar.show_copper = false;
    let plain = app.compose_ar(&gray);
    assert_eq!(plain.pixels[50 * 200 + 50], Color32::from_gray(120));
}

/// Place drag tracks the cursor in pixel space under the calibrated physical
/// lens projection: a
/// drag of (dpx, dpy) frame pixels shifts the pivot's *projected pixel* by
/// exactly that — so the overlay follows the mouse over the image instead of
/// sliding along the tilted plane.
#[test]
fn place_drag_tracks_cursor_in_the_physical_lens_frame() {
    let mut app = nonlinear_app();
    app.placement.tx_mm = 30.0;
    app.placement.ty_mm = 25.0;

    let pivot = |a: &ConsoleApp| {
        a.place_projection(800, 800)
            .unwrap()
            .to_px((a.placement.tx_mm, a.placement.ty_mm))
            .unwrap()
    };
    let before = pivot(&app);
    app.drag_place_px(800, 800, 12.0, -7.0).unwrap();
    let after = pivot(&app);
    assert!(
        (after.0 - (before.0 + 12.0)).abs() < 1e-6,
        "x pixel tracked: {} vs {}",
        after.0,
        before.0 + 12.0
    );
    assert!(
        (after.1 - (before.1 - 7.0)).abs() < 1e-6,
        "y pixel tracked: {} vs {}",
        after.1,
        before.1 - 7.0
    );
}

#[test]
fn place_drag_refuses_an_uncalibrated_uniform_fallback() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.placement.tx_mm = 5.0;
    app.placement.ty_mm = 5.0;
    assert!(
        app.drag_place_px(100, 100, 20.0, -30.0)
            .unwrap_err()
            .contains("needs a projection"),
        "no anchor and no nonlinear cal: dragging has no frame to move in"
    );
}

/// Shift+drag on the design rotates it the way the pointer sweeps ON THE BED,
/// not on the screen. The two senses are opposite (screen y grows down, machine
/// y grows up), and getting it backwards is the obvious failure — so pin the
/// sign: from the pivot's +x axis, dragging the pointer UP the screen is toward
/// machine +y, i.e. a quarter turn counter-clockwise on the bed, i.e. +90°
/// (`Placement::affine` is `[cos, −sin; sin, cos]`, CCW-positive).
#[test]
fn shift_drag_rotates_in_the_machine_sense_not_the_screen_sense() {
    use super::fiducial_ui::rot_delta_deg;
    let pivot = egui::pos2(0.0, 0.0);
    let quarter = rot_delta_deg(pivot, egui::pos2(1.0, 0.0), egui::pos2(0.0, -1.0));
    assert!(
        (quarter - 90.0).abs() < 1e-9,
        "pointer swept up-screen = +90° on the bed, got {quarter}"
    );
    // …and the mirror image of that drag is the mirror image of the rotation.
    let back = rot_delta_deg(pivot, egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0));
    assert!((back + 90.0).abs() < 1e-9, "swept down-screen: {back}");
    // A sweep across the ±180 seam nudges rather than spinning a full turn.
    let seam = rot_delta_deg(pivot, egui::pos2(-1.0, -0.01), egui::pos2(-1.0, 0.01));
    assert!(seam.abs() < 2.0, "no ±360 jump at the seam: {seam}");
}

/// Double-sided: on the back, the expected fiducial positions are the
/// design layout mirrored about its centerline with the beam entry→exit
/// offset applied — matching the kernel `back_expected_fiducial_mm`.
#[test]
fn back_side_expected_points_mirror_and_offset() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "10,10; 60,10; 10,60; 60,60".into();
    app.job.board_thickness_mm = 1.6;
    app.job.focal_mm = 70.0;

    // Front: expected == design.
    assert_eq!(app.job.side, Side::Front);
    assert_eq!(
        app.expected_points(),
        vec![(10.0, 10.0), (60.0, 10.0), (10.0, 60.0), (60.0, 60.0)]
    );

    // Back: mirror about the layout centroid (x=35) + f-theta offset.
    app.set_side(Side::Back);
    let axis = cam::flip::MirrorAxis::VerticalX { x_mm: 35.0 };
    let field = cam::flip::FieldParams {
        scan_center_mm: (35.0, 35.0),
        thickness_mm: 1.6,
        focal_mm: 70.0,
    };
    let want: Vec<(f64, f64)> = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0), (60.0, 60.0)]
        .into_iter()
        .map(|(x, y)| cam::flip::back_expected_fiducial_mm(x, y, &axis, &field))
        .collect();
    let got = app.expected_points();
    assert_eq!(got.len(), 4);
    for (g, w) in got.iter().zip(&want) {
        assert!(
            (g.0 - w.0).abs() < 1e-9 && (g.1 - w.1).abs() < 1e-9,
            "{g:?} vs {w:?}"
        );
    }
    // The left/right holes swapped sides (mirror), so hole #0 (was x=10) now
    // sits right of center.
    assert!(
        got[0].0 > 35.0,
        "left hole mirrored to the right: {:?}",
        got[0]
    );
}

/// An explicit scan-center override changes the back-side parallax: with
/// the lens axis at a fiducial, that hole stops shifting while the others
/// shift more — matching the physics (no parallax on the optical axis).
#[test]
fn scan_center_override_moves_the_parallax_origin() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "10,10; 60,10; 10,60; 60,60".into();
    app.set_side(Side::Back);

    // Auto (centroid 35,35): every hole shifts off its plain mirror image.
    let auto_pts = app.expected_points();

    // Override: lens axis exactly on the first hole (10,10) → that hole's
    // exit == entry, so its expected back position is the *pure* mirror.
    app.job.scan_center_auto = false;
    app.job.scan_center_mm = (10.0, 10.0);
    let over_pts = app.expected_points();
    let mirror_only = |x: f64, y: f64| (2.0 * 35.0 - x, y); // axis stays at centroid
    let (mx, my) = mirror_only(10.0, 10.0);
    assert!(
        (over_pts[0].0 - mx).abs() < 1e-9 && (over_pts[0].1 - my).abs() < 1e-9,
        "on-axis hole has no parallax: {:?} vs ({mx},{my})",
        over_pts[0]
    );
    assert_ne!(
        auto_pts[0], over_pts[0],
        "moving the scan center changes the expectation"
    );
}

/// Switching side clears the per-side caches so nothing from the front
/// bleeds into the back view.
#[test]
fn set_side_resets_per_side_caches() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "10,10; 60,10".into();
    app.sync_fid_markers();
    app.ar.copper = vec![pcb_core::Poly::default()];
    assert!(!app.fiducials.search.is_empty());
    app.set_side(Side::Back);
    assert!(
        app.fiducials.search.is_empty(),
        "markers cleared on side switch"
    );
    assert!(app.ar.copper.is_empty(), "AR design cleared on side switch");
    assert_eq!(app.job.side, Side::Back);
}

/// The Job tab lays out with the Back side selected (the back form renders).
#[test]
fn back_side_job_tab_lays_out() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.set_side(Side::Back);
    app.runtime.tab = CentralTab::Job;
    let ctx = egui::Context::default();
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(!out.shapes.is_empty(), "back-side job tab produced shapes");
}

/// Input paths persist across restarts: a second console over the same DB
/// picks up the Gerber paths (and neighbours) the first one saved.
#[test]
fn input_fields_persist_across_restarts() {
    let db = tmp_db();
    let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
    a.job.emit_copper = "/board/F_Cu.gbr".into();
    a.job.emit_outline = "/board/Edge.gbr".into();
    a.job.offset_mm = 0.05;
    a.placement.lbrn2 = "placed.lbrn2".into();
    a.fiducials.layout = "10,10; 60,10; 10,60; 60,60".into();
    a.camera.use_device = true;
    a.camera.device = 3;
    a.save_settings_if_changed(); // what the per-frame hook does

    // A fresh console over the same DB (a "restart") reloads them.
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert_eq!(b.job.emit_copper, "/board/F_Cu.gbr");
    assert_eq!(b.job.emit_outline, "/board/Edge.gbr");
    assert!((b.job.offset_mm - 0.05).abs() < 1e-9);
    assert_eq!(b.placement.lbrn2, "placed.lbrn2");
    assert_eq!(b.fiducials.layout, "10,10; 60,10; 10,60; 60,60");
    assert!(b.camera.use_device, "camera source choice persists");
    assert_eq!(b.camera.device, 3, "camera device index persists");
}

#[test]
fn pre_refactor_settings_blob_keeps_values_and_keys() {
    let db = tmp_db();
    let settings = crate::settings::path_for_db(&db);
    let legacy = "pcbforge console settings v1\n\
kicad_project=C:/boards/demo.kicad_pcb\n\
copper=C:/boards/F_Cu.gbr\n\
outline=C:/boards/Edge_Cuts.gbr\n\
lbrn2=job.lbrn2\n\
offset_mm=0.075\n\
back_copper=C:/boards/B_Cu.gbr\n\
back_outline=C:/boards/Edge_Cuts.gbr\n\
thickness_mm=1.2\n\
focal_mm=70\n\
place_frame=board.png\n\
place_lbrn2=placed.lbrn2\n\
place_px_per_mm=12.5\n\
fid_frame=fid.png\n\
fid_layout=10,10; 60,10; 10,60; 60,60\n\
fid_px_per_mm=12.5\n\
cam_file=camera.png\n\
cam_orientation=rotate180\n\
cam_use_device=true\n\
cam_device=2\n\
calib_n=9\n\
calib_pitch_mm=8\n\
calib_dot_mm=0.3\n\
calib_dot_kind=bright\n\
calib_grid_origin_x=3\n\
calib_grid_origin_y=4\n\
calib_grid_out=grid.lbrn2\n\
cam_show_bed=false\n\
place_field_correct=true\n\
field_mm=80\n\
field_center_auto=false\n\
field_cx_mm=41\n\
field_cy_mm=39\n\
calib_matrix=1 0 0 0 1 0 0 0 1\n\
calib_saved_at=123456\n\
lens_px_to_mm=\n\
lens_mm_to_px=\n\
lens_stats=\n\
lens_frame_sig=\n\
field_accepted=false\n\
field_to_px=\n\
field_stats=\n\
field_frame=\n";
    std::fs::write(&settings, legacy).unwrap();

    let app = ConsoleApp::new(db, vec!["true".into()]);
    assert_eq!(app.job.emit_copper, "C:/boards/F_Cu.gbr");
    assert_eq!(app.camera.orientation, Orientation::Rotate180);
    assert!(app.camera.use_device);
    assert_eq!(app.camera.device, 2);
    assert_eq!(app.calibration.burn.n, 9);
    assert_eq!(app.calibration.burn.dot_kind, calib::DotKind::Bright);
    // A pre-split blob has one shared parameter set: the paper set is seeded
    // from it, since that's what ① was last fit with.
    assert_eq!(app.calibration.paper, app.calibration.burn);
    assert!(app.placement.field_correct);

    let before = crate::settings::parse(legacy);
    let after = crate::settings::parse(&app.settings_blob());
    // The save keeps every legacy key (with its value) and adds exactly the
    // four new paper-set keys, seeded from the legacy shared values.
    let new_keys: Vec<_> = after
        .keys()
        .filter(|k| !before.contains_key(*k))
        .map(|k| k.as_str())
        .collect();
    assert_eq!(
        new_keys,
        vec![
            "calib_accept_rms_um",
            "calib_accept_worst_um",
            "calib_field_scale",
            "calib_paper_dot_kind",
            "calib_paper_dot_mm",
            "calib_paper_n",
            "calib_paper_out",
            "calib_paper_pitch_mm",
            "fid_diameter_mm",
            "fid_height_mm",
            "fid_live_recover_s",
            "fid_out",
            "fid_profile",
            "fid_rect_h_mm",
            "fid_rect_w_mm",
            "fid_search_mm",
            "fid_shape",
            "job_frequency_khz",
            "job_interval_mm",
            "job_passes",
            "job_pulse_ns",
            "job_speed_mm_s",
            "job_wobble",
            "job_wobble_size_mm",
            "job_wobble_step_mm",
            "lens_px_bounds",
            "place_drill_lbrn2",
            "place_drills",
            "place_lightburn_device",
        ]
    );
    assert_eq!(after.get("calib_paper_n"), Some(&"9".to_string()));
    assert_eq!(after.get("calib_paper_pitch_mm"), Some(&"8".to_string()));
    assert_eq!(
        after.get("calib_paper_dot_kind"),
        Some(&"bright".to_string())
    );
    for (key, value) in before {
        assert_eq!(after.get(&key), Some(&value), "setting {key}");
    }
}

/// The ③ scale handling used to be a bool (`calib_allow_machine_scale`). A
/// blob written by that build must still land the operator on the equivalent
/// three-way choice — and the save writes only the new key.
#[test]
fn retired_machine_scale_bool_migrates_to_the_three_way_choice() {
    for (legacy, want) in [
        ("true", calib::FieldScale::Compensate),
        ("false", calib::FieldScale::Refuse),
    ] {
        let db = tmp_db();
        let settings = crate::settings::path_for_db(&db);
        std::fs::write(
            &settings,
            format!("pcbforge console settings v1\ncalib_allow_machine_scale={legacy}\n"),
        )
        .unwrap();
        let app = ConsoleApp::new(&db, vec!["true".into()]);
        assert_eq!(app.calibration.field_scale, want, "legacy {legacy}");
        let saved = crate::settings::parse(&app.settings_blob());
        assert_eq!(
            saved.get("calib_field_scale").map(String::as_str),
            Some(field_scale_token(want))
        );
    }
    // An absent key (a blob from before either setting existed) is the default.
    let db = tmp_db();
    std::fs::write(
        crate::settings::path_for_db(&db),
        "pcbforge console settings v1\n",
    )
    .unwrap();
    let app = ConsoleApp::new(&db, vec!["true".into()]);
    assert_eq!(app.calibration.field_scale, calib::FieldScale::Refuse);

    // The new key wins over a stale bool left behind by an older build.
    let db = tmp_db();
    std::fs::write(
        crate::settings::path_for_db(&db),
        "pcbforge console settings v1\n\
calib_allow_machine_scale=true\n\
calib_field_scale=distortion_only\n",
    )
    .unwrap();
    let app = ConsoleApp::new(&db, vec!["true".into()]);
    assert_eq!(
        app.calibration.field_scale,
        calib::FieldScale::DistortionOnly
    );
}

/// The operator-configurable step 3 field acceptance limits round-trip.
#[test]
fn field_acceptance_limits_persist() {
    let db = tmp_db();
    {
        let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
        a.calibration.accept_rms_um = 120.0;
        a.calibration.accept_worst_um = 300.0;
        a.save_settings_if_changed();
    }
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert!((b.calibration.accept_rms_um - 120.0).abs() < 1e-9);
    assert!((b.calibration.accept_worst_um - 300.0).abs() < 1e-9);
}

/// The operator-configurable LightBurn export recipe (speed / Q-pulse /
/// interval / passes) round-trips through a save + reload; absent keys keep
/// the defaults (backward compatible).
#[test]
fn job_export_recipe_persists() {
    let db = tmp_db();
    {
        let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
        a.job.speed_mm_s = 2500.0;
        a.job.frequency_khz = 80.0;
        a.job.pulse_ns = 42;
        a.job.interval_mm = 0.05;
        a.job.passes = 3;
        a.save_settings_if_changed();
    }
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert!((b.job.speed_mm_s - 2500.0).abs() < 1e-9);
    assert!((b.job.frequency_khz - 80.0).abs() < 1e-9);
    assert_eq!(b.job.pulse_ns, 42);
    assert!((b.job.interval_mm - 0.05).abs() < 1e-9);
    assert_eq!(b.job.passes, 3);

    // A blob with none of the recipe keys keeps today's defaults.
    let fresh = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    assert!((fresh.job.speed_mm_s - 1000.0).abs() < 1e-9);
    assert!((fresh.job.frequency_khz - 30.0).abs() < 1e-9);
    assert_eq!(fresh.job.pulse_ns, 1);
    assert!((fresh.job.interval_mm - 0.03).abs() < 1e-9);
    assert_eq!(fresh.job.passes, 1);
}

/// The fiducial rectangle's spans round-trip through a save + reload; a blob
/// without them keeps the 50/50 defaults.
#[test]
fn fid_rect_dimensions_persist() {
    let db = tmp_db();
    {
        let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
        a.fiducials.rect_w_mm = 90.0;
        a.fiducials.rect_h_mm = 60.0;
        a.save_settings_if_changed();
    }
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert!((b.fiducials.rect_w_mm - 90.0).abs() < 1e-9);
    assert!((b.fiducials.rect_h_mm - 60.0).abs() < 1e-9);

    // A fresh console with no persisted keys keeps the operator defaults.
    let fresh = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    assert!((fresh.fiducials.rect_w_mm - 50.0).abs() < 1e-9);
    assert!((fresh.fiducials.rect_h_mm - 50.0).abs() < 1e-9);
}

/// The Live re-acquire interval round-trips, defaults to the operator's 500 ms,
/// and cannot be pushed out of range by a hand-edited blob — the DragValue
/// clamps what the console writes, but nothing stops someone typing a value the
/// ladder would then hand to `Duration::from_secs_f64`.
#[test]
fn fid_live_recover_interval_persists_and_clamps() {
    let db = tmp_db();
    {
        let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
        assert!(
            (a.fiducials.live_recover_s - 0.5).abs() < 1e-9,
            "a fresh console re-acquires every 500 ms"
        );
        a.fiducials.live_recover_s = 0.2;
        a.save_settings_if_changed();
    }
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert!((b.fiducials.live_recover_s - 0.2).abs() < 1e-9);

    // Out of range on both sides, planted straight into the settings file.
    for (written, want) in [("99", 10.0), ("-1", 0.1), ("0", 0.1)] {
        let db = tmp_db();
        let settings = crate::settings::path_for_db(&db);
        std::fs::write(
            &settings,
            format!("pcbforge console settings v1\nfid_live_recover_s={written}\n"),
        )
        .unwrap();
        let app = ConsoleApp::new(db, vec!["true".into()]);
        assert!(
            (app.fiducials.live_recover_s - want).abs() < 1e-9,
            "{written} clamps to {want}, got {}",
            app.fiducials.live_recover_s
        );
    }
}

/// ④ Fiducial holes: the `fid_rect:` summary line reports the rectangle spans
/// and the layout computed against the auto-centred field. Field 90 auto →
/// centre 45,45; rect 60×40 → x 15..75, y 25..65.
#[test]
fn fid_rect_summary_reports_the_computed_layout() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Calibrate;
    app.calibration.mode = CalibMode::FidHoles;
    app.camera.field_mm = 90.0;
    app.camera.field_center_auto = true;
    app.sync_auto_field_center();
    app.fiducials.rect_w_mm = 60.0;
    app.fiducials.rect_h_mm = 40.0;

    let summary = app.debug_summary();
    assert!(
        summary.contains("calib_mode=FidHoles"),
        "④ mode active:\n{summary}"
    );
    assert!(
        summary.contains(
            "fid_rect: w=60 h=40 \
             layout=15.00,25.00; 75.00,25.00; 15.00,65.00; 75.00,65.00"
        ),
        "fid_rect line reports the computed layout:\n{summary}"
    );
}

/// The burned-grid frame anchor's mirror flag round-trips as a 5th
/// `field_frame` token (`… 1`), and a legacy 4-token blob (written before X
/// mirroring was representable) restores as un-mirrored.
#[test]
fn field_frame_mirror_flag_round_trips() {
    use nalgebra::{Matrix3, Point2};
    let coords = [0.0, 20.0, 40.0, 60.0];
    let lens = {
        let pairs: Vec<_> = coords
            .iter()
            .flat_map(|&y| {
                coords.iter().map(move |&x| {
                    (
                        Point2::new(10.0 * x + 20.0, 800.0 - 10.0 * y),
                        Point2::new(x, y),
                    )
                })
            })
            .collect();
        vision::fit_lens(&pairs).unwrap()
    };
    let field = {
        let pairs: Vec<_> = coords
            .iter()
            .flat_map(|&y| {
                coords
                    .iter()
                    .map(move |&x| (Point2::new(x, y), Point2::new(x, y)))
            })
            .collect();
        vision::fit_field(&pairs).unwrap()
    };
    let to_px = vision::Homography {
        matrix: Matrix3::new(10.0, 0.0, 20.0, 0.0, -10.0, 800.0, 0.0, 0.0, 1.0),
        residuals: vec![],
        rms: 0.0,
    };
    let flipped = calib::Rigid2 {
        cos: (0.2_f64).cos(),
        sin: (0.2_f64).sin(),
        tx: 3.5,
        ty: -1.25,
        flip_x: true,
    };
    let make_app = |db: &PathBuf, frame: calib::Rigid2| {
        let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
        a.calibration.lens = Some(calib::CameraCal {
            lens: lens.clone(),
            dots: vec![],
            found: 16,
            total: 16,
        });
        a.calibration.field = Some(calib::FieldCal {
            field: field.clone(),
            paper_to_machine: frame,
            to_px: to_px.clone(),
            dots: vec![],
            found: 16,
            total: 16,
            field_verdict: vision::classify_field_error(&[]),
            scale: 1.0,
            extrapolated: 0,
            rejected: 0,
            rejection_note: String::new(),
        });
        a.calibration.field_accepted = true;
        a.calibration.lens_frame_signature = Some(((800, 800), Orientation::Normal));
        // The reload path reads the FieldMap from the field-map file.
        std::fs::write(a.field_map_path(), field.serialize()).unwrap();
        a
    };

    // Save a mirrored frame, reload: flip_x survives as the 5th token = 1.
    let db = tmp_db();
    let blob = {
        let mut a = make_app(&db, flipped);
        a.save_settings_if_changed();
        a.settings_blob()
    };
    assert!(
        blob.lines()
            .any(|l| l.starts_with("field_frame=") && l.trim_end().ends_with(" 1")),
        "flip serializes as the 5th field_frame token = 1:\n{blob}"
    );
    let b = ConsoleApp::new(db, vec!["true".into()]);
    let r = b
        .calibration
        .field
        .as_ref()
        .expect("field restored")
        .paper_to_machine;
    assert!(r.flip_x, "the mirror flag is restored");
    assert!(
        (r.cos - flipped.cos).abs() < 1e-9
            && (r.sin - flipped.sin).abs() < 1e-9
            && (r.tx - flipped.tx).abs() < 1e-9
            && (r.ty - flipped.ty).abs() < 1e-9,
        "the frame parameters survive alongside the flag"
    );

    // A legacy 4-token field_frame (no flip token) restores as un-mirrored.
    let legacy_db = tmp_db();
    let blob4: String = {
        let a = make_app(
            &legacy_db,
            calib::Rigid2 {
                flip_x: false,
                ..flipped
            },
        );
        a.settings_blob()
            .lines()
            .map(|l| match l.strip_prefix("field_frame=") {
                Some(v) => {
                    let toks: Vec<&str> = v.split_whitespace().take(4).collect();
                    format!("field_frame={}\n", toks.join(" "))
                }
                None => format!("{l}\n"),
            })
            .collect()
    };
    std::fs::write(crate::settings::path_for_db(&legacy_db), blob4).unwrap();
    let c = ConsoleApp::new(legacy_db, vec!["true".into()]);
    let rc = c
        .calibration
        .field
        .as_ref()
        .expect("legacy field restored")
        .paper_to_machine;
    assert!(!rc.flip_x, "a 4-token field_frame restores un-mirrored");
}

/// The fiducial shape/footprint/search/profile/out fields round-trip through
/// a save + reload, and a fresh app reports the circle default in its summary.
#[test]
fn fiducial_shape_and_footprint_persist() {
    let db = tmp_db();
    {
        let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
        a.fiducials.shape = crate::fiducial::ShapeKind::Rect;
        a.fiducials.diameter_mm = 2.5;
        a.fiducials.height_mm = 1.75;
        a.fiducials.search_mm = 4.0;
        a.fiducials.profile = crate::fiducial::ProfileKind::Backlit;
        a.fiducials.out = "holes.lbrn2".into();
        a.save_settings_if_changed();
    }
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert_eq!(b.fiducials.shape, crate::fiducial::ShapeKind::Rect);
    assert!((b.fiducials.diameter_mm - 2.5).abs() < 1e-9);
    assert!((b.fiducials.height_mm - 1.75).abs() < 1e-9);
    assert!((b.fiducials.search_mm - 4.0).abs() < 1e-9);
    assert_eq!(b.fiducials.profile, crate::fiducial::ProfileKind::Backlit);
    assert_eq!(b.fiducials.out, "holes.lbrn2");
}

/// A fresh console's summary reports the fiducial shape (circle by default),
/// so the headless `state` command surfaces the new fields.
#[test]
fn fresh_app_summary_reports_circle_shape() {
    let app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    let summary = app.debug_summary();
    assert!(
        summary.contains("shape=circle"),
        "summary carries the shape token: {summary}"
    );
    assert!(
        summary.contains("profile=dark_dot"),
        "summary carries the profile token: {summary}"
    );
    assert!(
        summary.contains("speed=1000 freq_khz=30 pulse_ns=1 interval=0.03 passes=1"),
        "summary carries the export recipe defaults: {summary}"
    );
}

/// The two parameter sets persist independently once both exist.
#[test]
fn paper_and_burn_params_persist_independently() {
    let db = tmp_db();
    {
        let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
        a.calibration.paper = GridParams {
            n: 9,
            pitch_mm: 9.6,
            dot_mm: 0.3,
            dot_kind: calib::DotKind::Dark,
        };
        a.calibration.burn = GridParams {
            n: 7,
            pitch_mm: 10.0,
            dot_mm: 0.4,
            dot_kind: calib::DotKind::Bright,
        };
        a.save_settings_if_changed();
    }
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert_eq!(b.calibration.paper.n, 9);
    assert!((b.calibration.paper.pitch_mm - 9.6).abs() < 1e-9);
    assert_eq!(b.calibration.paper.dot_kind, calib::DotKind::Dark);
    assert_eq!(b.calibration.burn.n, 7);
    assert!((b.calibration.burn.pitch_mm - 10.0).abs() < 1e-9);
    assert_eq!(b.calibration.burn.dot_kind, calib::DotKind::Bright);
}

/// The ① lens fit's pixel bounds survive a save/reload, and a legacy blob
/// written before the `lens_px_bounds` key existed restores them as `None`.
#[test]
fn lens_calibration_pixel_bounds_round_trip() {
    use nalgebra::Point2;
    let coords = [0.0, 20.0, 40.0, 60.0];
    let pairs: Vec<_> = coords
        .iter()
        .flat_map(|&y| {
            coords.iter().map(move |&x| {
                (
                    Point2::new(10.0 * x + 20.0, 800.0 - 10.0 * y),
                    Point2::new(x, y),
                )
            })
        })
        .collect();
    let lens = vision::fit_lens(&pairs).unwrap();
    let expected = lens.calib_px_bounds.expect("fit records bounds");
    let make_cal = |lens: vision::LensMap| calib::CameraCal {
        lens,
        dots: Vec::new(),
        found: 16,
        total: 16,
    };

    // Round-trip through save + reload: bounds survive.
    let db = tmp_db();
    {
        let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
        a.calibration.lens = Some(make_cal(lens.clone()));
        a.save_settings_if_changed();
    }
    let b = ConsoleApp::new(db, vec!["true".into()]);
    let restored = b
        .calibration
        .lens
        .as_ref()
        .expect("lens restored")
        .lens
        .calib_px_bounds
        .expect("bounds restored");
    for (e, r) in expected.iter().zip(&restored) {
        assert!((e - r).abs() < 1e-9, "bound {e} vs {r}");
    }

    // A legacy blob with the lens maps but no lens_px_bounds key restores None.
    let legacy_db = tmp_db();
    let blob = {
        let mut a = ConsoleApp::new(legacy_db.clone(), vec!["true".into()]);
        a.calibration.lens = Some(make_cal(lens));
        a.settings_blob()
    };
    let stripped: String = blob
        .lines()
        .filter(|l| !l.starts_with("lens_px_bounds="))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(
        !stripped.contains("lens_px_bounds"),
        "legacy blob carries no bounds key"
    );
    std::fs::write(crate::settings::path_for_db(&legacy_db), stripped).unwrap();
    let c = ConsoleApp::new(legacy_db, vec!["true".into()]);
    assert!(
        c.calibration
            .lens
            .as_ref()
            .expect("lens restored")
            .lens
            .calib_px_bounds
            .is_none(),
        "a missing bounds key restores as None"
    );
}

/// "Etch here" resolves a bare output filename next to the copper Gerber
/// (so the operator can find it), and leaves an absolute path untouched.
#[test]
fn place_output_resolves_beside_the_copper() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.placement.lbrn2 = "placed.lbrn2".into();
    let out = app.resolve_place_output("/home/nick/uv_test/uv_test-F_Cu.gbr");
    assert_eq!(
        out,
        std::path::PathBuf::from("/home/nick/uv_test/placed.lbrn2"),
        "bare name lands beside the copper input"
    );
    // An absolute output path is honored as given.
    app.placement.lbrn2 = "/tmp/somewhere/job.lbrn2".into();
    assert_eq!(
        app.resolve_place_output("/home/nick/uv_test/uv_test-F_Cu.gbr"),
        std::path::PathBuf::from("/tmp/somewhere/job.lbrn2")
    );
}

/// Camera orientation: Rotate180 maps a corner pixel to the opposite
/// corner (an upside-down mount), and the choice persists across restarts.
#[test]
fn camera_orientation_transforms_and_persists() {
    // A 3×2 frame with a unique bright pixel at the top-left (0,0).
    let mut img = image::GrayImage::from_pixel(3, 2, image::Luma([10]));
    img.put_pixel(0, 0, image::Luma([200]));

    // Rotate 180° sends (0,0) to the bottom-right (2,1).
    let r = Orientation::Rotate180.apply(img.clone());
    assert_eq!(r.get_pixel(2, 1)[0], 200, "top-left → bottom-right");
    assert_eq!(r.get_pixel(0, 0)[0], 10);
    // Flip vertical sends (0,0) to the bottom-left (0,1).
    let v = Orientation::FlipV.apply(img.clone());
    assert_eq!(v.get_pixel(0, 1)[0], 200);
    // Normal is a no-op.
    assert_eq!(
        Orientation::Normal.apply(img.clone()).get_pixel(0, 0)[0],
        200
    );

    // Token round-trip + persistence.
    assert_eq!(
        Orientation::from_token("rotate180"),
        Some(Orientation::Rotate180)
    );
    let db = tmp_db();
    let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
    a.camera.orientation = Orientation::Rotate180;
    a.save_settings_if_changed();
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert_eq!(
        b.camera.orientation,
        Orientation::Rotate180,
        "orientation survived restart"
    );
}

/// The Place tab uses the camera→laser calibration when present (machine
/// frame), and falls back to the fiducial homography otherwise.
#[test]
fn place_uses_calibration_when_present() {
    use nalgebra::Matrix3;
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    // No calibration, no fiducial fit → no placement homography.
    assert!(app.place_homography().is_none());

    // A fiducial fit alone → design frame.
    app.fiducials.homography = Some(vision::Homography {
        matrix: Matrix3::new(9.0, 0.0, 0.0, 0.0, 9.0, 0.0, 0.0, 0.0, 1.0),
        residuals: vec![],
        rms: 0.0,
    });
    let design = app.place_homography().unwrap();
    assert!(
        (design.matrix[(0, 0)] - 9.0).abs() < 1e-9,
        "fiducial homography used"
    );

    // A calibration wins: place_homography is the calibration's inverse
    // (machine-mm → px), independent of the fiducial fit.
    app.calibration.anchor = Some(calib::Calibration {
        // px_to_mm = 0.1 (10 px/mm); inverse = 10 (mm→px).
        px_to_mm: vision::Homography {
            matrix: Matrix3::new(0.1, 0.0, 0.0, 0.0, 0.1, 0.0, 0.0, 0.0, 1.0),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 12.0,
        found: 49,
        total: 49,
        dots: Vec::new(),
    });
    let machine = app.place_homography().unwrap();
    assert!(
        (machine.matrix[(0, 0)] - 10.0).abs() < 1e-6,
        "calibration inverse (mm→px) used: {}",
        machine.matrix[(0, 0)]
    );
}

/// The calibration (the taped-grid fit) persists across restarts as a
/// re-anchor seed: the matrix survives, restored as "unconfirmed"
/// (found==0) until the operator re-anchors.
#[test]
fn calibration_persists_as_a_reanchor_seed() {
    use nalgebra::Matrix3;
    let db = tmp_db();
    let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
    a.calibration.burn.n = 7;
    a.calibration.burn.pitch_mm = 10.0;
    a.calibration.anchor = Some(calib::Calibration {
        px_to_mm: vision::Homography {
            matrix: Matrix3::new(0.1, 0.0, 1.0, 0.0, 0.1, 2.0, 0.0, 0.0, 1.0),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 20.0,
        found: 49,
        total: 49,
        dots: Vec::new(),
    });
    a.save_settings_if_changed();

    let b = ConsoleApp::new(db, vec!["true".into()]);
    let cal = b.calibration.anchor.expect("calibration restored");
    assert_eq!(cal.found, 0, "restored as unconfirmed until re-anchored");
    assert!((cal.px_to_mm.matrix[(0, 0)] - 0.1).abs() < 1e-12);
    assert!(
        (cal.px_to_mm.matrix[(0, 2)] - 1.0).abs() < 1e-12,
        "translation survived"
    );
    assert_eq!(b.calibration.burn.n, 7);
}

/// Camera-lens calibration through the console: a printed-grid frame + 4
/// corner clicks → a lens fit with per-dot distortion feedback; and the
/// tab lays out with the arrows drawn.
#[test]
fn camera_lens_calibration_flow() {
    let grid = calib::GridSpec {
        origin_mm: (0.0, 0.0),
        pitch_mm: 10.0,
        n: 7,
    };
    // A frame of a printed grid with mild barrel (reuse the pattern).
    let ppm = 10.0;
    let dot = 1.5;
    let pts = grid.points();
    let img = image::GrayImage::from_fn(700, 700, |x, y| {
        let dark = pts.iter().any(|&(mx, my)| {
            let (cx, cy) = (mx * ppm + 40.0, my * ppm + 40.0);
            (((x as f64) - cx).powi(2) + ((y as f64) - cy).powi(2)).sqrt() < 0.5 * dot * ppm
        });
        image::Luma([if dark { 40 } else { 210 }])
    });
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Calibrate;
    app.calibration.mode = CalibMode::CameraLens;
    app.calibration.paper.n = 7;
    app.calibration.paper.pitch_mm = 10.0;
    app.calibration.paper.dot_mm = dot;
    app.calibration.frame_img = Some(img);
    // Corner clicks at the four grid corners (px = mm*ppm + 40).
    app.calibration.corners = vec![(40.0, 40.0), (640.0, 40.0), (640.0, 640.0), (40.0, 640.0)];
    // Start with feedback hidden (as a fresh-loaded frame would) so the assertion
    // below actually verifies the successful fit flips it back on.
    app.calibration.show_fit_feedback = false;
    app.calibrate_fit();
    assert!(
        app.calibration.show_fit_feedback,
        "a successful lens fit re-shows the feedback overlay"
    );
    let lens = app.calibration.lens.as_ref().expect("lens fit produced");
    assert!(lens.found >= 45, "locked most dots: {}", lens.found);
    assert!(
        lens.lens.rms_um < 60.0,
        "corrected RMS {} µm",
        lens.lens.rms_um
    );
    assert_eq!(lens.dots.len(), lens.found, "per-dot feedback present");

    // The tab renders (with the distortion arrows) headless.
    let ctx = Context::default();
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(!out.shapes.is_empty());
}

/// The fit-feedback overlay defaults to visible, and loading a fresh grid frame
/// hides it so the operator sees the bare dots to re-click the 4 corners.
#[test]
fn loading_a_frame_hides_stale_fit_feedback() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    assert!(
        app.calibration.show_fit_feedback,
        "feedback visible by default"
    );
    assert!(
        app.debug_summary().contains("feedback=on"),
        "default summary reports feedback=on:\n{}",
        app.debug_summary()
    );
    app.calibration.frame = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../samples/calibration/grid-7x7-10mm-distorted.png"
    )
    .into();
    let ctx = Context::default();
    app.calibrate_load_frame(&ctx);
    assert!(
        !app.calibration.show_fit_feedback,
        "loading a fresh frame hides the stale overlay"
    );
    assert!(
        app.debug_summary().contains("feedback=off"),
        "summary reports feedback=off after load:\n{}",
        app.debug_summary()
    );
}

/// The Calibrate tab lays out headless, including corner clicks.
#[test]
fn calibrate_tab_lays_out() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Calibrate;
    app.calibration.corners = vec![(10.0, 90.0), (90.0, 90.0), (90.0, 10.0), (10.0, 10.0)];
    let ctx = Context::default();
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(!out.shapes.is_empty(), "calibrate tab renders");
}

/// The laser-anchor overlay draws the reconstructed machine grid without
/// panicking: load the burned-grid fixture, mark corners, fit, then lay out
/// the Calibrate tab in LaserAnchor mode with per-dot feedback present.
#[test]
fn anchor_overlay_renders_the_machine_grid() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Calibrate;
    app.calibration.mode = CalibMode::LaserAnchor;
    app.calibration.burn.n = 7;
    app.calibration.burn.pitch_mm = 10.0;
    app.calibration.burn.dot_mm = 2.0;
    app.calibration.frame = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../samples/calibration/grid-7x7-10mm-distorted.png"
    )
    .into();
    let ctx = Context::default();
    app.calibrate_load_frame(&ctx);
    // Corner dots from the fixture's JSON sidecar (LL, LR, UR, UL).
    app.calibration.corners = vec![
        (42.506, 632.64),
        (620.163, 618.878),
        (606.277, 39.892),
        (43.744, 41.83),
    ];
    app.calibrate_fit();
    let cal = app.calibration.anchor.as_ref().expect("anchored");
    assert!(cal.found >= 40, "anchor located the grid: {}", cal.found);
    assert_eq!(cal.dots.len(), cal.found, "per-dot feedback present");
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(!out.shapes.is_empty(), "anchor overlay renders the mesh");
}

#[test]
fn anchor_dot_correction_moves_the_selected_grid_site_and_refits() {
    use nalgebra::Matrix3;
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.calibration.anchor = Some(calib::Calibration {
        px_to_mm: vision::Homography {
            matrix: Matrix3::new(0.1, 0.0, -2.0, 0.0, 0.1, -3.0, 0.0, 0.0, 1.0),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 0.0,
        found: 5,
        total: 49,
        dots: vec![
            calib::AnchorDot {
                px: (20.0, 30.0),
                mm: (0.0, 0.0),
                resid_um: 0.0,
            },
            calib::AnchorDot {
                px: (620.0, 30.0),
                mm: (60.0, 0.0),
                resid_um: 0.0,
            },
            calib::AnchorDot {
                px: (620.0, 630.0),
                mm: (60.0, 60.0),
                resid_um: 0.0,
            },
            calib::AnchorDot {
                px: (20.0, 630.0),
                mm: (0.0, 60.0),
                resid_um: 0.0,
            },
            calib::AnchorDot {
                px: (320.0, 330.0),
                mm: (30.0, 30.0),
                resid_um: 0.0,
            },
        ],
    });

    app.calibrate_edit_anchor_dot((323.0, 332.0), false);
    let anchor = app
        .calibration
        .anchor
        .as_ref()
        .expect("anchor remains fitted");
    let corrected = anchor
        .dots
        .iter()
        .find(|dot| dot.mm == (30.0, 30.0))
        .expect("center dot retained");
    assert_eq!(corrected.px, (323.0, 332.0));
    assert!(app.calibration.note.contains("corrected dot"));
}

/// The camera bed overlay (work area + 50 mm scale) draws without panicking
/// when a calibration is present and a frame is loaded.
#[test]
fn camera_bed_overlay_renders_when_calibrated() {
    use nalgebra::Matrix3;
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Camera;
    let ctx = Context::default();
    // A frame in the camera panel.
    app.set_camera_frame(
        &ctx,
        image::GrayImage::from_pixel(200, 150, image::Luma([180])),
    );
    // A laser anchor: camera-px → mm at 10 px/mm.
    app.calibration.anchor = Some(calib::Calibration {
        px_to_mm: vision::Homography {
            matrix: Matrix3::new(0.1, 0.0, 0.0, 0.0, 0.1, 0.0, 0.0, 0.0, 1.0),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 10.0,
        found: 49,
        total: 49,
        dots: Vec::new(),
    });
    app.camera.show_bed = true;
    app.camera.field_mm = 60.0;
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(!out.shapes.is_empty(), "camera bed overlay renders");
    assert!(
        app.debug_summary()
            .contains("camera_projection: homography"),
        "anchor remains the approximate fallback: {}",
        app.debug_summary()
    );
}

#[test]
fn work_area_defaults_to_auto_centered_seventy_mm_field() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    assert!(app.camera.field_center_auto);
    assert_eq!(app.camera.field_mm, 70.0);
    assert_eq!(
        (app.camera.field_cx_mm, app.camera.field_cy_mm),
        (35.0, 35.0)
    );

    app.camera.field_mm = 90.0;
    app.sync_auto_field_center();
    assert_eq!(
        (app.camera.field_cx_mm, app.camera.field_cy_mm),
        (45.0, 45.0)
    );

    app.camera.field_center_auto = false;
    app.camera.field_cx_mm = 12.0;
    app.camera.field_cy_mm = 34.0;
    app.camera.field_mm = 120.0;
    app.sync_auto_field_center();
    assert_eq!(
        (app.camera.field_cx_mm, app.camera.field_cy_mm),
        (12.0, 34.0),
        "manual centre survives field-size changes"
    );
}

#[test]
fn former_140_at_origin_default_migrates_to_auto_centered_field() {
    let db = tmp_db();
    let settings = crate::settings::blob(&[
        ("field_mm", "140".into()),
        ("field_cx_mm", "0".into()),
        ("field_cy_mm", "0".into()),
    ]);
    crate::settings::save(&crate::settings::path_for_db(&db), &settings).unwrap();

    let app = ConsoleApp::new(db, vec!["true".into()]);
    assert!(app.camera.field_center_auto);
    assert_eq!(app.camera.field_mm, 70.0);
    assert_eq!(
        (app.camera.field_cx_mm, app.camera.field_cy_mm),
        (35.0, 35.0)
    );
}

#[test]
fn legacy_custom_work_area_keeps_its_manual_center() {
    let db = tmp_db();
    let settings = crate::settings::blob(&[
        ("field_mm", "60".into()),
        ("field_cx_mm", "0".into()),
        ("field_cy_mm", "30".into()),
    ]);
    crate::settings::save(&crate::settings::path_for_db(&db), &settings).unwrap();

    let app = ConsoleApp::new(db, vec!["true".into()]);
    assert!(!app.camera.field_center_auto);
    assert_eq!(app.camera.field_mm, 60.0);
    assert_eq!(
        (app.camera.field_cx_mm, app.camera.field_cy_mm),
        (0.0, 30.0)
    );
}

#[test]
fn downscale_view_caps_longest_side_and_reports_ratio() {
    // Below the cap: unchanged, ratio 1.0.
    let small = ColorImage {
        size: [100, 60],
        pixels: vec![Color32::BLACK; 100 * 60],
    };
    let (out, s) = downscale_view(small, CAM_VIEW_MAX);
    assert_eq!(out.size, [100, 60]);
    assert!((s - 1.0).abs() < 1e-12);
    // Above the cap: longest side capped, ratio ≈ cap/longest, pixels match.
    let big = ColorImage {
        size: [2560, 1440],
        pixels: vec![Color32::WHITE; 2560 * 1440],
    };
    let (out, s) = downscale_view(big, 1280);
    assert_eq!(out.size[0], 1280, "longest side capped");
    assert!(out.size[1] <= 1280 && out.size[1] > 700);
    assert!((s - 0.5).abs() < 0.01, "ratio ≈ 0.5, got {s}");
    assert_eq!(out.pixels.len(), out.size[0] * out.size[1]);
}

/// The live *view* downscales, but `cam_last` (the calibration/detection
/// data) stays full resolution, and the overlay scale is recorded.
#[test]
fn camera_view_downscales_but_data_stays_full_res() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    let ctx = Context::default();
    app.set_camera_frame(
        &ctx,
        image::GrayImage::from_pixel(2560, 1440, image::Luma([120])),
    );
    // Data kept full-res.
    assert_eq!(app.camera.last.as_ref().unwrap().dimensions(), (2560, 1440));
    // View was downscaled and the ratio recorded.
    assert!(
        app.camera.view_scale < 1.0,
        "view scale {}",
        app.camera.view_scale
    );
    assert!((app.camera.view_scale - 0.5).abs() < 0.01);
    let tex = app.camera.tex.as_ref().unwrap().size();
    assert_eq!(tex[0], 1280, "view texture capped to 1280");
}

/// Generate grid centres the lattice on the machine field so it lands
/// inside the addressable work area (a centre-origin galvo has 0,0 at the
/// field centre, not a corner).
#[test]
fn generate_grid_centers_on_the_field() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.calibration.burn.n = 7;
    app.calibration.burn.pitch_mm = 10.0;
    app.camera.field_cx_mm = 0.0;
    app.camera.field_cy_mm = 0.0;
    app.calibration.grid_out = "calib-grid.lbrn2".into();
    app.calibrate_generate_grid();
    // A 60 mm span centred on (0,0) → origin (-30,-30), corner (30,30).
    assert!(
        app.calibration.note.contains("centred on work area")
            && app.calibration.note.contains("(-30,-30)"),
        "note: {}",
        app.calibration.note
    );
    // The fit grid must carry that same burn origin — not (0,0) — or every
    // calibrated etch is offset by span/2 − field_center (LR-02).
    assert_eq!(app.calib_grid().origin_mm, (-30.0, -30.0));
    assert_eq!(app.calibration.grid_origin_mm, (-30.0, -30.0));

    // Off-centre work area (LightBurn origin not at 0,0): the grid follows.
    app.camera.field_cx_mm = 0.0;
    app.camera.field_cy_mm = 30.0;
    app.calibrate_generate_grid();
    assert!(
        app.calibration.note.contains("(-30,0)…(30,60)"),
        "grid recentres on the work area: {}",
        app.calibration.note
    );
}

/// The step-1 paper-grid button shells `paper-grid` and leaves a note that
/// tells the operator to caliper the printed pitch before fitting.
#[test]
fn generate_paper_grid_notes_the_caliper_step() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.calibration.mode = CalibMode::CameraLens;
    app.calibration.paper.n = 9;
    app.calibration.paper.pitch_mm = 10.0;
    app.calibration.paper_out = "paper-grid.svg".into();
    app.calibrate_generate_paper_grid();
    assert!(
        app.calibration.note.contains("9×9 paper grid") && app.calibration.note.contains("CALIPER"),
        "note reminds the operator to caliper: {}",
        app.calibration.note
    );
    // A comfortable 80 mm span is not flagged.
    assert!(
        !app.calibration.note.contains("span exceeds"),
        "an in-bounds span is not warned: {}",
        app.calibration.note
    );

    // A too-large span (29×10 = 280 mm) warns but still shells (the CLI reports
    // the hard error in the Log).
    app.calibration.paper.n = 29;
    app.calibrate_generate_paper_grid();
    assert!(
        app.calibration.note.contains("span exceeds the 190 mm"),
        "an oversize span is warned: {}",
        app.calibration.note
    );
}

/// The ① paper-grid output path round-trips through save + reload.
#[test]
fn paper_out_path_persists() {
    let db = tmp_db();
    {
        let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
        a.calibration.paper_out = "sheets/lens-grid.svg".into();
        a.save_settings_if_changed();
    }
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert_eq!(b.calibration.paper_out, "sheets/lens-grid.svg");
}

/// A failed re-fit (wrong corners/polarity — the operator's 0/49 case)
/// must keep the working calibration, not erase it (LR-16).
#[test]
fn a_failed_fit_keeps_the_previous_calibration() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.calibration.anchor = Some(calib::Calibration {
        px_to_mm: vision::Homography {
            matrix: nalgebra::Matrix3::identity(),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 12.0,
        found: 49,
        total: 49,
        dots: Vec::new(),
    });
    app.calibration.mode = CalibMode::LaserAnchor;
    // A blank frame + 4 corners: the detector finds no dots and the fit
    // errors.
    app.calibration.frame_img = Some(image::GrayImage::from_pixel(200, 200, image::Luma([200])));
    app.calibration.corners = vec![(10.0, 10.0), (190.0, 10.0), (190.0, 190.0), (10.0, 190.0)];
    app.calibrate_fit();
    assert!(
        app.calibration.anchor.is_some(),
        "a failed fit must not erase the previous calibration"
    );
    assert!(
        app.calibration.note.contains("kept previous"),
        "note explains the keep: {}",
        app.calibration.note
    );
}

/// The initial placement centre lands the job pivot at the frame's pixel
/// centre in the calibrated physical-lens frame.
#[test]
fn initial_placement_centers_in_the_physical_lens_frame() {
    let app = nonlinear_app();
    let (w, ht) = (800.0, 800.0);
    let (tx, ty) = app.initial_center_mm(w, ht).unwrap();
    let c = app
        .place_projection(w as u32, ht as u32)
        .unwrap()
        .to_px((tx, ty))
        .unwrap();
    assert!(
        (c.0 - w / 2.0).abs() < 1e-3 && (c.1 - ht / 2.0).abs() < 1e-3,
        "pivot maps to ({:.2},{:.2}), want ({},{})",
        c.0,
        c.1,
        w / 2.0,
        ht / 2.0
    );
}

/// Back-side "Etch here" is refused (register can't mirror), rather than
/// silently emitting the front copper unmirrored at the wrong place (LR-03).
#[test]
fn back_side_etch_is_refused() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.job.side = Side::Back;
    app.emit_at_placement(false);
    let last = app.runtime.log.last().expect("a log line was pushed");
    assert!(
        last.err && last.text.contains("back-side"),
        "expected a back-side refusal, got: {}",
        last.text
    );
}

/// "Etch + run in LightBurn" (run_after=true) queues an ABSOLUTE .lbrn2 path
/// once the export actually launches, so `pump_verb` can chain the run.
#[test]
fn etch_and_run_arms_an_absolute_pending_path() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.calibration.anchor = Some(calib::Calibration {
        px_to_mm: vision::Homography {
            matrix: nalgebra::Matrix3::new(0.1, 0.0, 0.0, 0.0, -0.1, 80.0, 0.0, 0.0, 1.0),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 0.0,
        found: 0,
        total: 49,
        dots: Vec::new(),
    });
    app.placement.job = vec![pcb_core::Poly::default()];
    app.fiducials.frame_img = Some(image::GrayImage::new(800, 800));
    app.job.emit_copper = "board.gbr".into();

    app.emit_at_placement(true);
    let pending = app
        .runtime
        .pending_lightburn
        .as_ref()
        .expect("a LightBurn run was queued");
    assert!(
        pending.path.is_absolute(),
        "queued path is absolute: {:?}",
        pending.path
    );
    assert!(pending.start, "the etch chain presses START");
    assert!(app.debug_summary().contains("lightburn=pending"));
}

/// "⚙ Generate holes" queues an ABSOLUTE holes path once the export launches,
/// so the file LOADS in LightBurn — but as a load-only hand-off: `start` is
/// false, so the chain can never press START and the click cannot fire the
/// laser.
#[test]
fn generate_holes_arms_an_absolute_load_only_handoff() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducial_generate_holes();
    let pending = app
        .runtime
        .pending_lightburn
        .as_ref()
        .expect("a LightBurn load was queued");
    assert!(
        pending.path.is_absolute(),
        "queued path is absolute: {:?}",
        pending.path
    );
    assert!(
        pending.path.ends_with("fid-holes.lbrn2"),
        "queued path is the holes output: {:?}",
        pending.path
    );
    assert!(!pending.start, "load-only: the chain never presses START");
    assert!(app.debug_summary().contains("lightburn=pending-load"));
}

/// A bad layout is refused BEFORE the export shells: the note names the layout
/// error and no verb job is spawned. (Asserting "no burn was queued" would be
/// vacuous now that this path never queues one for any input.)
#[test]
fn generate_holes_refuses_a_bad_layout_before_exporting() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "not a layout".into();
    app.fiducial_generate_holes();
    assert!(
        app.runtime.verb_job.is_none(),
        "the export never shelled for a rejected layout"
    );
    assert!(
        app.fiducials.note.starts_with("layout:"),
        "the note names the layout error: {:?}",
        app.fiducials.note
    );
}

/// The placement guard (no frame/job loaded) refuses before the export starts,
/// so the run_after click arms nothing.
#[test]
fn guard_refusal_does_not_arm_a_lightburn_run() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    // Front side, but no placement job loaded → the "load the design" guard.
    app.emit_at_placement(true);
    assert!(
        app.runtime.pending_lightburn.is_none(),
        "nothing queued when the guard refuses"
    );
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.err && l.text.contains("load the design first")),
        "the guard error was logged"
    );
    assert!(app.debug_summary().contains("lightburn=idle"));
}

/// A failed export clears the queued run and says it was skipped, rather than
/// etching a file the export never wrote.
#[test]
fn failed_export_skips_the_queued_lightburn_run() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.pending_lightburn = Some(PendingLightburn {
        path: std::path::PathBuf::from("/tmp/placed.lbrn2"),
        start: true,
    });
    app.chain_lightburn_after_verb(false);
    assert!(
        app.runtime.pending_lightburn.is_none(),
        "the queued path is cleared on failure"
    );
    assert!(app.runtime.lightburn_run.is_none(), "no run started");
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.err && l.text.contains("skipped")),
        "the skip is logged"
    );
}

/// KiCad-dialect Excellon for the drill-emit tests: two round holes + one
/// G85 slot. Raw bbox x [9.5, 20.5], y [−10.5, −4.5] (center 15, −7.5).
const DRILL_SAMPLE: &str = "\
M48
FMAT,2
METRIC
T1C1.000
%
G90
G05
T1
X10.0Y-10.0
X20.0Y-10.0
X15.0Y-5.0G85X15.0Y-7.0
G05
M30
";

// The emitted-geometry parser is shared with the diagnostic log's export
// readback: the record that says where a job landed must read the file the same
// way the tests that assert it do.
use crate::diag::{lbrn2_verts, verts_bbox};

/// "⤓ Emit drill holes → LightBurn (no burn)" writes the hole geometry at the
/// placement affine (translation + rotation, NO frame normalization — the
/// placement is the position), spawns a LOAD-ONLY LightBurn run, and never
/// arms the etch path's export→start chain.
#[test]
fn emit_drill_holes_writes_placed_geometry_without_queueing_a_burn() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    let dir = std::env::temp_dir().join(format!("ui-drill-emit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let drl = dir.join("holes.drl");
    std::fs::write(&drl, DRILL_SAMPLE).unwrap();
    let out = dir.join("drill-holes.lbrn2");

    app.placement.job = vec![pcb_core::Poly::default()];
    app.fiducials.frame_img = Some(image::GrayImage::new(800, 800));
    app.placement.drills = drl.to_string_lossy().into_owned();
    app.placement.drill_lbrn2 = out.to_string_lossy().into_owned();
    app.placement.pivot = (0.0, 0.0);
    app.placement.tx_mm = 100.0;
    app.placement.ty_mm = 50.0;
    app.placement.rot_deg = 0.0;

    app.emit_drill_at_placement();

    let doc = std::fs::read_to_string(&out).expect("the .lbrn2 was written");
    assert!(doc.contains("<name Value=\"DRILL\"/>"));
    assert_eq!(
        doc.matches("Type=\"Path\"").count(),
        3,
        "2 round holes + 1 slot, one shape each"
    );
    // Pure translation (rot 0, pivot 0): the raw drill bbox center (15, −7.5)
    // lands at (115, 42.5) — coordinates are bed mm, not re-normalized.
    let (x0, y0, x1, y1) = verts_bbox(&lbrn2_verts(&doc));
    assert!(
        ((x0 + x1) / 2.0 - 115.0).abs() < 0.01 && ((y0 + y1) / 2.0 - 42.5).abs() < 0.01,
        "placed center: ({}, {})",
        (x0 + x1) / 2.0,
        (y0 + y1) / 2.0
    );

    // The whole point: the file goes TO LightBurn (a load-only run) but the
    // start chain is never armed and the run can never press START.
    assert!(
        app.runtime.pending_lightburn.is_none(),
        "the export→start chain is never armed"
    );
    let run = app
        .runtime
        .lightburn_run
        .as_ref()
        .expect("a LightBurn load was spawned");
    assert!(run.load_only(), "the spawned run is load-only (no START)");
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| !l.err && l.text.contains("NOT starting")),
        "the log says the job is loaded, not started"
    );
    assert!(
        app.placement.note.contains("no burn started"),
        "note: {}",
        app.placement.note
    );

    // Rotating 90° about the pattern center swaps the emitted extents
    // (11×6 mm → 6×11 mm) around the same target point.
    app.placement.pivot = (15.0, -7.5);
    app.placement.rot_deg = 90.0;
    app.emit_drill_at_placement();
    let doc = std::fs::read_to_string(&out).unwrap();
    let (x0, y0, x1, y1) = verts_bbox(&lbrn2_verts(&doc));
    assert!(
        (x1 - x0 - 6.0).abs() < 0.01 && (y1 - y0 - 11.0).abs() < 0.01,
        "rotated extents: {}×{}",
        x1 - x0,
        y1 - y0
    );
    assert!(
        ((x0 + x1) / 2.0 - 100.0).abs() < 0.01 && ((y0 + y1) / 2.0 - 50.0).abs() < 0.01,
        "rotation pivots about the placed center"
    );
    assert!(
        app.runtime.pending_lightburn.is_none(),
        "still nothing queued"
    );
}

/// "⚙ Drills from KiCad" mirrors the Gerbers button: deterministic
/// `pth.drl;npth.drl` paths (next to the Gerbers) fill the drill field
/// immediately, and the export shells the `drills` verb in the background.
#[test]
fn drills_from_kicad_fills_the_field_with_stable_paths() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);

    // Guard: no project set.
    app.drills_from_kicad();
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.err && l.text.contains("KiCad project")),
        "missing-project guard logged"
    );
    assert!(
        app.placement.drills.is_empty(),
        "field untouched on refusal"
    );

    // A real (empty) board file: resolve_board only needs it to exist.
    let dir = std::env::temp_dir().join(format!("ui-drills-kicad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let board = dir.join("demo.kicad_pcb");
    std::fs::write(&board, "").unwrap();
    app.job.kicad_project = board.to_string_lossy().into_owned();

    app.drills_from_kicad();
    let gerber_dir = dir.join("pcbforge-gerbers");
    assert_eq!(
        app.placement.drills,
        format!(
            "{};{}",
            gerber_dir.join("pth.drl").display(),
            gerber_dir.join("npth.drl").display()
        ),
        "both stable drill paths land in the field"
    );
    assert!(
        app.placement.note.contains("exporting drill files"),
        "note: {}",
        app.placement.note
    );
    assert!(
        app.runtime.verb_job.is_some(),
        "the drills verb was shelled"
    );
}

/// An empty drill field is not a refusal while a KiCad project is set: the
/// emit derives the export's deterministic pth/npth pair, fills the field with
/// the files that exist, and proceeds — one click instead of a dead end.
#[test]
fn drill_emit_derives_the_drill_paths_from_the_kicad_project() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    let dir = std::env::temp_dir().join(format!("ui-drill-derive-{}", std::process::id()));
    let gerbers = dir.join("pcbforge-gerbers");
    std::fs::create_dir_all(&gerbers).unwrap();
    let board = dir.join("demo.kicad_pcb");
    std::fs::write(&board, "").unwrap();
    std::fs::write(gerbers.join("pth.drl"), DRILL_SAMPLE).unwrap();
    let out = dir.join("drill-holes.lbrn2");

    app.placement.job = vec![pcb_core::Poly::default()];
    app.fiducials.frame_img = Some(image::GrayImage::new(800, 800));
    app.placement.drill_lbrn2 = out.to_string_lossy().into_owned();
    assert!(app.placement.drills.is_empty(), "the field starts empty");

    // No project either: the clear error still stands.
    app.emit_drill_at_placement();
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.err && l.text.contains("set a drill file")),
        "no project, no derivation — the error is kept"
    );
    assert!(!out.exists(), "nothing written");

    // With the project set, the existing pth.drl is found and used. npth.drl
    // was never exported here, so only the file that exists lands in the field.
    app.job.kicad_project = board.to_string_lossy().into_owned();
    app.emit_drill_at_placement();
    assert_eq!(
        app.placement.drills,
        gerbers.join("pth.drl").display().to_string(),
        "the derived path filled the field"
    );
    assert!(
        out.exists(),
        "the drill job was written: {}",
        app.placement.note
    );

    std::fs::remove_dir_all(dir).ok();
}

/// The drill-emit guards refuse (back side, no job, no drill file) without
/// writing anything or arming the LightBurn chain.
#[test]
fn emit_drill_holes_guards_refuse_without_queueing() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);

    // No job loaded.
    app.emit_drill_at_placement();
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.err && l.text.contains("load the design first")),
        "missing-job guard logged"
    );

    // Job loaded but no drill file named.
    app.placement.job = vec![pcb_core::Poly::default()];
    app.fiducials.frame_img = Some(image::GrayImage::new(800, 800));
    app.emit_drill_at_placement();
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.err && l.text.contains("set a drill file")),
        "missing-drill-file guard logged"
    );

    // Back side: same chirality refusal as the etch buttons.
    app.job.side = Side::Back;
    app.emit_drill_at_placement();
    assert!(
        app.runtime
            .log
            .iter()
            .any(|l| l.err && l.text.contains("back-side drill emit")),
        "back-side guard logged"
    );

    assert!(app.runtime.pending_lightburn.is_none());
    assert!(app.runtime.lightburn_run.is_none());
}

/// The LightBurn device name round-trips through a save + reload, and a fresh
/// console reports the default in its summary.
#[test]
fn lightburn_device_persists() {
    let db = tmp_db();
    {
        let mut a = ConsoleApp::new(db.clone(), vec!["true".into()]);
        assert!(
            a.debug_summary().contains("device=BSLFiber"),
            "fresh console reports the default device: {}",
            a.debug_summary()
        );
        a.placement.lightburn_device = "MyGalvo".into();
        a.save_settings_if_changed();
    }
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert_eq!(b.placement.lightburn_device, "MyGalvo");
}

/// The calibration status distinguishes "never anchored" from a *saved*
/// calibration, reporting the latter's age.
#[test]
fn calibration_reports_age_not_just_this_session() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    assert!(
        app.debug_summary().contains("calib_anchor: none"),
        "never anchored"
    );
    // A restored (unconfirmed) calibration saved 3 days ago.
    app.calibration.anchor = Some(calib::Calibration {
        px_to_mm: vision::Homography {
            matrix: nalgebra::Matrix3::identity(),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 0.0,
        found: 0,
        total: 49,
        dots: Vec::new(),
    });
    app.calibration.saved_at = Some(now_unix().saturating_sub(3 * 86_400));
    assert!(
        app.debug_summary().contains("saved (3 days ago)"),
        "reports age: {}",
        app.debug_summary()
    );
}

/// A second frame after a status refresh still lays out (state survives).
#[test]
fn app_survives_refresh_and_relayout() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.refresh();
    let ctx = Context::default();
    for _ in 0..2 {
        let _ = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    }
}

/// A Check keeps the operator's own markers when they work. Detection must try
/// them FIRST — re-seeding through the calibrated projection up front used to
/// throw away markers that were already on the holes, which turned a working
/// 4-of-4 Check into 0-of-4 for any layout whose coordinates were click-derived
/// rather than true machine coordinates.
#[test]
fn check_locates_via_the_operators_markers_when_they_work() {
    let dir = std::env::temp_dir().join(format!("ui-fidladder-a-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bed4.png");
    let ppm = 10.0;
    let holes = [(15.0, 25.0), (55.0, 25.0), (55.0, 60.0), (15.0, 60.0)];
    write_hole_frame(&path, ppm, &holes);

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Fiducials;
    app.fiducials.frame = path.to_string_lossy().into();
    app.fiducials.layout = "15,25; 55,25; 55,60; 15,60".into();
    app.fiducials.px_per_mm = ppm;
    app.fiducials.diameter_mm = 1.0;
    app.fiducials.search_mm = 2.0;
    let ctx = Context::default();
    app.load_fid_frame(&ctx);
    app.render_fiducials(&ctx);

    assert_eq!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count(),
        4,
        "all four found: {}",
        app.fiducials.note
    );
    assert!(
        app.fiducials.note.contains("located via markers"),
        "the operator's markers were used as-is: {}",
        app.fiducials.note
    );
    // The markers stayed where they were — no stage moved them.
    for (m, h) in app.fiducials.search.iter().zip(&holes) {
        assert!(
            (m.0 - h.0).abs() < 1e-9 && (m.1 - h.1).abs() < 1e-9,
            "marker {m:?} moved off {h:?}"
        );
    }
}

/// The board is 10 mm from where the layout says and there is no calibration,
/// so neither the markers nor a projection can find it. The whole-frame
/// rectangle match recovers all four, moves the markers onto them, and the note
/// says which stage did it plus why the earlier ones did not.
#[test]
fn check_recovers_a_displaced_board_via_the_rectangle_match() {
    let dir = std::env::temp_dir().join(format!("ui-fidladder-c-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bed5.png");
    let ppm = 10.0;
    // Layout corners, and the board actually sitting 10 mm right / 10 mm down
    // from them — five times the 2 mm search window, so every local window is
    // looking at bare copper.
    let layout = [(15.0, 25.0), (55.0, 25.0), (55.0, 60.0), (15.0, 60.0)];
    let holes: Vec<(f64, f64)> = layout.iter().map(|&(x, y)| (x + 10.0, y - 10.0)).collect();
    write_hole_frame(&path, ppm, &holes);

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Fiducials;
    app.fiducials.frame = path.to_string_lossy().into();
    app.fiducials.layout = "15,25; 55,25; 55,60; 15,60".into();
    app.fiducials.px_per_mm = ppm;
    app.fiducials.diameter_mm = 1.0;
    app.fiducials.search_mm = 2.0;
    let ctx = Context::default();
    app.load_fid_frame(&ctx);
    app.render_fiducials(&ctx);

    assert_eq!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count(),
        4,
        "recovered all four: {}",
        app.fiducials.note
    );
    assert!(
        app.fiducials.note.contains("located via rectangle match")
            && app.fiducials.note.contains("candidates"),
        "the note names the stage and its candidate count: {}",
        app.fiducials.note
    );
    // The markers were moved onto the real holes, so the ✛ set now shows where
    // the board actually is.
    for (m, h) in app.fiducials.search.iter().zip(&holes) {
        assert!(
            (m.0 - h.0).abs() < 0.5 && (m.1 - h.1).abs() < 0.5,
            "marker {m:?} not on hole {h:?}"
        );
    }
}

/// Nothing matches: no stage improves on the operator's markers, so the ✛ set
/// is left exactly where they put it rather than parked wherever the last
/// failed attempt happened to leave it.
#[test]
fn a_failed_check_leaves_the_markers_where_the_operator_put_them() {
    let dir = std::env::temp_dir().join(format!("ui-fidladder-d-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bed6.png");
    let ppm = 10.0;
    // A blank bed: no holes at all, so every stage comes up empty.
    write_hole_frame(&path, ppm, &[]);

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Fiducials;
    app.fiducials.frame = path.to_string_lossy().into();
    app.fiducials.layout = "15,25; 55,25; 55,60; 15,60".into();
    app.fiducials.px_per_mm = ppm;
    app.fiducials.diameter_mm = 1.0;
    app.fiducials.search_mm = 2.0;
    let ctx = Context::default();
    app.load_fid_frame(&ctx);
    let placed = [(16.0, 26.0), (54.0, 24.0), (56.0, 61.0), (14.0, 59.0)];
    app.fiducials.search = placed.to_vec();
    app.render_fiducials(&ctx);

    assert_eq!(
        app.fiducials.found.iter().filter(|f| f.is_some()).count(),
        0,
        "nothing to find: {}",
        app.fiducials.note
    );
    assert_eq!(
        app.fiducials.search,
        placed.to_vec(),
        "the markers survived a failed Check: {}",
        app.fiducials.note
    );
    assert!(
        app.fiducials.note.contains("rectangle match"),
        "the note says the rescan was tried and why it came up short: {}",
        app.fiducials.note
    );
}

/// The throttle on the ladder's third stage. Under Live the whole-frame scan
/// costs a visible hitch, so it may fire on a short frame only once per the
/// operator's re-acquire interval — and backs off four times as far after a run
/// that found nothing. A manual Check is never throttled.
///
/// (The live loop itself — real frames streaming off a device while the board
/// moves — is not testable headlessly; this pins the decision, not the feed.)
#[test]
fn the_rectangle_match_is_throttled_under_live_but_not_under_a_check() {
    // Built by addition only: an Instant cannot be safely walked backwards.
    let t0 = std::time::Instant::now();
    // The windows the ladder stamps, derived the way it derives them, so the
    // probe times below stay right whatever the default interval becomes.
    let cooldown = std::time::Duration::from_secs_f64(1.0);
    let backoff = cooldown * RECOVER_BACKOFF_FACTOR;

    // Enough hits: no stage 3 at all, live or not.
    assert!(!should_global_recover(true, 3, None, t0));
    assert!(!should_global_recover(false, 4, None, t0));

    // First short frame under Live scans; a second one inside the window does
    // not, whichever window the previous run earned.
    assert!(should_global_recover(true, 1, None, t0));
    let after_win = Some((t0, cooldown));
    let after_lose = Some((t0, backoff));
    assert!(!should_global_recover(true, 1, after_win, t0 + cooldown / 5));

    // The two windows differ: halfway through the backoff a recovered scan is
    // due again, a failed one is still held off; at the backoff both are due.
    let mid = t0 + backoff / 2;
    assert!(should_global_recover(true, 1, after_win, mid));
    assert!(!should_global_recover(true, 1, after_lose, mid));
    assert!(should_global_recover(true, 1, after_lose, t0 + backoff));
    assert!(
        cooldown < backoff,
        "a failed recovery must back off further than a successful one"
    );

    // The configured interval is the one that governs, not a constant: at
    // 0.2 s the success window is 0.2 s and the failure window 0.8 s.
    let fast = std::time::Duration::from_secs_f64(0.2);
    let fast_lose = fast * RECOVER_BACKOFF_FACTOR;
    assert_eq!(fast_lose, std::time::Duration::from_millis(800));
    let win = Some((t0, fast));
    let lose = Some((t0, fast_lose));
    assert!(!should_global_recover(
        true,
        1,
        win,
        t0 + std::time::Duration::from_millis(199)
    ));
    assert!(should_global_recover(
        true,
        1,
        win,
        t0 + std::time::Duration::from_millis(200)
    ));
    assert!(!should_global_recover(
        true,
        1,
        lose,
        t0 + std::time::Duration::from_millis(799)
    ));
    assert!(should_global_recover(
        true,
        1,
        lose,
        t0 + std::time::Duration::from_millis(800)
    ));

    // A manual Check ignores the cooldown entirely — one deliberate press, and
    // the operator is waiting for the answer.
    assert!(should_global_recover(false, 1, after_lose, t0));
    assert!(should_global_recover(false, 0, after_win, t0));
}

/// The throttle wired into the ladder: under Live a hopeless frame runs the
/// whole-frame scan once, and the next detection on the same feed reports it as
/// throttled instead of paying for a second scan.
#[test]
fn a_second_short_live_frame_does_not_rescan() {
    let dir = std::env::temp_dir().join(format!("ui-fidthrottle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bed7.png");
    let ppm = 10.0;
    // A blank bed: every stage comes up empty, so every frame is "short".
    write_hole_frame(&path, ppm, &[]);

    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Fiducials;
    app.fiducials.frame = path.to_string_lossy().into();
    app.fiducials.layout = "15,25; 55,25; 55,60; 15,60".into();
    app.fiducials.px_per_mm = ppm;
    app.fiducials.diameter_mm = 1.0;
    app.fiducials.search_mm = 2.0;
    let ctx = Context::default();
    app.load_fid_frame(&ctx);
    app.fiducials.live = true;
    // Pin the interval instead of inheriting the operator default: the two
    // detection passes below run back to back, and a slow (dev, loaded) machine
    // could otherwise walk past a sub-second window between them and rescan.
    app.fiducials.live_recover_s = 10.0;

    // Streamed frames driven directly (the pump's path), not through the
    // Check button — the two are now distinct on purpose.
    app.detect_fiducials(true);
    assert!(
        app.fiducials.note.contains("rectangle match")
            && !app.fiducials.note.contains("throttled"),
        "the first short frame under Live still runs the scan: {}",
        app.fiducials.note
    );
    app.detect_fiducials(true);
    assert!(
        app.fiducials.note.contains("rectangle match throttled"),
        "the very next short frame is throttled, not rescanned: {}",
        app.fiducials.note
    );

    // A manual Check is not on the feed's budget, so it scans regardless of
    // how recently the live loop did — WITHOUT Live being switched off first.
    // A button that sometimes does nothing while the feed runs was the wart
    // this parameter exists to remove.
    app.render_fiducials(&ctx);
    assert!(
        !app.fiducials.note.contains("throttled"),
        "a Check ignores the cooldown even while Live is on: {}",
        app.fiducials.note
    );
}

/// Everything a diagnostic-log reader depends on: the records exist, they carry
/// the `check=N` that makes a check greppable as one unit, and the overlay
/// record — the only one reached from a per-frame path — writes once per
/// position rather than once per frame.
#[test]
fn the_diagnostic_log_records_a_check_and_its_overlay_without_flooding() {
    use nalgebra::Matrix3;
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    let log_path = app.runtime.diag.path().to_path_buf();
    // A laser anchor at 10 px/mm, so camera px → machine mm is exact.
    app.calibration.anchor = Some(calib::Calibration {
        px_to_mm: vision::Homography {
            matrix: Matrix3::new(0.1, 0.0, 0.0, 0.0, 0.1, 0.0, 0.0, 0.0, 1.0),
            residuals: vec![],
            rms: 0.0,
        },
        rms_um: 0.0,
        found: 4,
        total: 4,
        dots: vec![],
    });
    app.fiducials.frame_img = Some(image::GrayImage::new(800, 800));
    app.fiducials.layout = "10,10; 60,10; 10,60; 60,60".into();
    // Detected exactly on the nominal layout: an identity fit, so the check is
    // applied rather than gated.
    app.fiducials.found = vec![
        Some((100.0, 100.0)),
        Some((600.0, 100.0)),
        Some((100.0, 600.0)),
        Some((600.0, 600.0)),
    ];
    app.update_placement_from_fiducials();
    assert!(app.fiducials.last_placed, "the fit was applied");

    let text = std::fs::read_to_string(&log_path).expect("the log file exists");
    assert!(
        text.starts_with(|c: char| c.is_ascii_digit()),
        "timestamped"
    );
    assert!(text.contains("startup version="), "record 1");
    assert!(
        text.contains("fid-check check=1") && text.contains("projection=homography"),
        "record 2a names the projection variant: {text}"
    );
    assert!(
        text.contains("detected_machine_mm=[10.000,10.000 60.000,10.000"),
        "record 2a carries the detections in MACHINE mm: {text}"
    );
    assert!(
        text.contains("layout_centroid_mm=35.000,35.000"),
        "record 2b carries the layout centroid: {text}"
    );
    assert!(text.contains("outcome=applied"), "record 2c: {text}");

    // Record 3: the overlay bbox. It is called from the frame path, so it must
    // write once for a position and stay silent while nothing moves.
    app.placement.job = vec![pcb_core::Poly {
        outer: vec![
            pcb_core::P { x: 0, y: 0 },
            pcb_core::P {
                x: 10 * NM_PER_MM,
                y: 0,
            },
            pcb_core::P {
                x: 10 * NM_PER_MM,
                y: 10 * NM_PER_MM,
            },
        ],
        holes: vec![],
    }];
    app.placement.pivot = (5.0, 5.0);
    for _ in 0..20 {
        app.diag_overlay(None, (800, 800));
    }
    let overlays = |t: &str| t.matches("overlay check=").count();
    let text = std::fs::read_to_string(&log_path).unwrap();
    assert_eq!(overlays(&text), 1, "20 frames, one record: {text}");

    // A sub-epsilon nudge is not a new position.
    app.placement.tx_mm += 0.001;
    app.diag_overlay(None, (800, 800));
    let text = std::fs::read_to_string(&log_path).unwrap();
    assert_eq!(overlays(&text), 1, "jitter below the epsilon is not logged");

    // A real move is.
    app.placement.tx_mm += 5.0;
    app.diag_overlay(None, (800, 800));
    let text = std::fs::read_to_string(&log_path).unwrap();
    assert_eq!(overlays(&text), 2, "a real move is recorded");
    assert!(
        text.contains("overlay check=1"),
        "the overlay is tagged with the check it belongs to: {text}"
    );
}

/// A gated-out check still records its fit and names the gate that refused it —
/// a refusal is the case the log exists for.
#[test]
fn the_diagnostic_log_names_the_gate_that_refused_a_check() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    let log_path = app.runtime.diag.path().to_path_buf();
    app.fiducials.frame_img = Some(image::GrayImage::new(800, 800));
    // No calibration at all, so `place_projection` refuses.
    app.update_placement_from_fiducials();
    let text = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        text.contains("outcome=refused-projection"),
        "the gate is named: {text}"
    );
    // Record 5: the projection failure also reaches the file as an error line
    // once the frame's log sweep runs.
    app.runtime.log.push(LogLine {
        text: "synthetic failure".into(),
        err: true,
    });
    app.diag_mirror_errors();
    let text = std::fs::read_to_string(&log_path).unwrap();
    assert!(text.contains("error synthetic failure"), "record 5: {text}");
}

/// The error mirror tracks an index into a log the verb pump trims from the
/// front. A shrunk log must clamp, not slice out of bounds.
#[test]
fn the_error_mirror_survives_the_log_being_trimmed() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    for i in 0..600 {
        app.runtime.log.push(LogLine {
            text: format!("line {i}"),
            err: i % 100 == 0,
        });
    }
    app.diag_mirror_errors();
    // The pump's 500-line trim, and then a full clear — both leave the cursor
    // past the end.
    app.runtime.log.clear();
    app.diag_mirror_errors();
    app.runtime.log.push(LogLine {
        text: "after the trim".into(),
        err: true,
    });
    app.diag_mirror_errors();
    let text = std::fs::read_to_string(app.runtime.diag.path()).unwrap();
    assert!(text.contains("error after the trim"), "mirroring resumes");
}
