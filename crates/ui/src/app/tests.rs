use super::*;

fn tmp_db() -> PathBuf {
    // Unique per call so each console gets its own settings sidecar
    // (`*.console-settings`) — a shared path would bleed persisted input
    // fields between tests.
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ui-app-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("t.sqlite")
}

fn nonlinear_app() -> ConsoleApp {
    use nalgebra::{Matrix3, Point2, Vector2};
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
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
            crate::calib::FieldDot {
                px,
                physical_mm: (physical.x, physical.y),
                commanded_mm: (commanded.x, commanded.y),
                field_um: ((physical.x - commanded.x).powi(2) + (physical.y - commanded.y).powi(2))
                    .sqrt()
                    * 1000.0,
                resid_um: 0.0,
            }
        })
        .collect();
    app.calibration.lens = Some(crate::calib::CameraCal {
        lens,
        dots: vec![],
        found: 16,
        total: 16,
    });
    app.calibration.field = Some(crate::calib::FieldCal {
        field,
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
    });
    app.calibration.field_accepted = true;
    app.calibration.lens_frame_signature = Some(((800, 800), Orientation::Normal));
    app
}

#[test]
fn accepted_field_uses_nonlinear_projection_and_physical_corrected_place() {
    let mut app = nonlinear_app();
    let projection = app.camera_projection((800, 800)).unwrap().unwrap();
    assert!(matches!(
        projection,
        CameraProjection::CommandedNonlinear { .. }
    ));
    let px = projection.to_px((40.0, 40.0)).unwrap();
    let mm = projection.from_px(px).unwrap();
    assert!((mm.0 - 40.0).abs() < 0.1 && (mm.1 - 40.0).abs() < 0.1);

    app.placement.field_correct = false;
    assert!(matches!(
        app.place_projection(800, 800).unwrap(),
        CameraProjection::CommandedNonlinear { .. }
    ));
    app.placement.field_correct = true;
    assert!(matches!(
        app.place_projection(800, 800).unwrap(),
        CameraProjection::PhysicalLens { .. }
    ));
}

#[test]
fn direct_burn_anchor_is_used_before_full_lens_field_calibration() {
    let mut app = nonlinear_app();
    let anchor = app.calibration.lens.as_ref().unwrap().lens.clone();
    app.calibration.lens = None;
    app.calibration.field = None;
    app.calibration.field_accepted = false;
    app.calibration.anchor_nonlinear = Some(anchor);
    app.calibration.anchor_frame_signature = Some(((800, 800), Orientation::Normal));

    let projection = app.camera_projection((800, 800)).unwrap().unwrap();
    assert!(matches!(
        projection,
        CameraProjection::AnchorNonlinear { .. }
    ));
    let px = projection.to_px((30.0, 40.0)).unwrap();
    let mm = projection.from_px(px).unwrap();
    assert!((mm.0 - 30.0).abs() < 0.1 && (mm.1 - 40.0).abs() < 0.1);
    assert!(matches!(
        app.place_projection(800, 800).unwrap(),
        CameraProjection::AnchorNonlinear { .. }
    ));

    let map = app.calibration.anchor_nonlinear.as_mut().unwrap();
    let mut coefficients = map.mm_to_px.to_coeffs();
    coefficients[0] = f64::NAN;
    map.mm_to_px = vision::Poly2::from_coeffs(&coefficients);
    assert!(app.camera_projection((800, 800)).is_err());
    assert!(app.place_projection(800, 800).is_err());
}

#[test]
fn invalid_nonlinear_projection_fails_closed_without_homography_fallback() {
    let mut app = nonlinear_app();
    let field = &mut app.calibration.field.as_mut().unwrap().field;
    let mut coeffs = field.to_physical.to_coeffs();
    coeffs[0] = f64::NAN;
    field.to_physical = vision::Poly2::from_coeffs(&coeffs);
    assert!(app.camera_projection((800, 800)).is_err());
    assert!(app.place_projection(800, 800).is_err());
}

#[test]
fn field_corrected_emit_refuses_a_missing_map_file() {
    let mut app = nonlinear_app();
    app.placement.field_correct = true;
    app.placement.job = vec![pcb_core::Poly::default()];
    app.job.emit_copper = "board.gbr".into();
    assert!(!app.field_map_path().exists());
    app.emit_at_placement();
    let line = app.runtime.log.last().expect("refusal is logged");
    assert!(
        line.err && line.text.contains("refusing to emit an uncorrected job"),
        "got: {}",
        line.text
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
    assert!(app.debug_summary().contains("camera_projection: nonlinear"));

    let field = &mut app.calibration.field.as_mut().unwrap().field;
    let mut coeffs = field.to_physical.to_coeffs();
    coeffs[0] = f64::NAN;
    field.to_physical = vision::Poly2::from_coeffs(&coeffs);
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

/// The Place-on-board tab lays out headless (form + placement controls).
#[test]
fn place_tab_lays_out_headless() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.runtime.tab = CentralTab::Place;
    let ctx = Context::default();
    let out = ctx.run(egui::RawInput::default(), |ctx| app.ui(ctx));
    assert!(!out.shapes.is_empty(), "place tab must render");
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

    app.fiducials.live = false;
    app.pump_fid_live(&ctx);
    assert!(
        app.fiducials.capture.is_none(),
        "capture stops when Live is off"
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
/// dragged positions are preserved.
#[test]
fn markers_follow_the_layout_field() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.layout = "10,10; 60,10; 10,60".into();
    app.sync_fid_markers();
    assert_eq!(app.fiducials.search.len(), 3);

    app.fiducials.search[0] = (11.5, 9.0); // drag marker 0
    app.fiducials.layout = "10,10; 60,10; 10,60; 60,60".into();
    app.sync_fid_markers();
    assert_eq!(app.fiducials.search.len(), 4, "4th marker appears");
    assert_eq!(
        app.fiducials.search[0],
        (11.5, 9.0),
        "dragged position kept"
    );
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

/// Dragging a search marker onto an off-nominal hole makes detection find
/// it: at the nominal design position the hole is out of the search window
/// (miss); after moving the marker onto the hole, it's found.
#[test]
fn dragging_marker_lets_detection_find_offset_hole() {
    let dir = std::env::temp_dir().join(format!("ui-drag-{}", std::process::id()));
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

    app.render_fiducials(&ctx);
    assert!(
        app.fiducials.found[0].is_none(),
        "misses at nominal (hole is 3 mm off)"
    );

    // Drag the marker onto the hole.
    app.fiducials.search[0] = (13.0, 10.0);
    app.render_fiducials(&ctx);
    assert!(
        app.fiducials.found[0].is_some(),
        "found after dragging the marker onto the hole"
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
    // Fine-tune the 3rd marker via drag, so we can prove removal keeps the
    // *other* markers' dragged positions aligned by index.
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
        "the survivor's dragged position stayed aligned to its token"
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
        &crate::fiducial::ProfileKind::Backlit.to_profile(1.0),
        2.0,
    );
    assert_eq!(backlit.tally.0, 3, "backlit finds the bright blobs");

    let darkdot = fiducial::check_frame(
        &img,
        &holes,
        ppm,
        &crate::fiducial::ProfileKind::DarkDot.to_profile(1.0),
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

/// Place drag tracks the cursor in pixel space, even under perspective: a
/// drag of (dpx, dpy) frame pixels shifts the pivot's *projected pixel* by
/// exactly that — so the overlay follows the mouse over the image instead of
/// sliding along the tilted plane.
#[test]
fn place_drag_tracks_cursor_under_perspective() {
    use nalgebra::Point2;
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    // A keystone homography (bed-mm → px): top edge narrower than bottom.
    let corr = [
        (Point2::new(0.0, 0.0), Point2::new(180.0, 110.0)),
        (Point2::new(60.0, 0.0), Point2::new(460.0, 110.0)),
        (Point2::new(60.0, 50.0), Point2::new(520.0, 380.0)),
        (Point2::new(0.0, 50.0), Point2::new(120.0, 380.0)),
    ];
    app.fiducials.homography = Some(vision::fit_homography(&corr).unwrap());
    app.placement.frame_img = Some(image::GrayImage::from_pixel(640, 480, image::Luma([120])));
    app.placement.px_per_mm = 8.0;
    app.placement.tx_mm = 30.0;
    app.placement.ty_mm = 25.0;

    let pivot = |a: &ConsoleApp| {
        a.fiducials
            .homography
            .as_ref()
            .unwrap()
            .apply(Point2::new(a.placement.tx_mm, a.placement.ty_mm))
    };
    let before = pivot(&app);
    app.drag_place_px(12.0, -7.0).unwrap();
    let after = pivot(&app);
    assert!(
        (after.x - (before.x + 12.0)).abs() < 1e-6,
        "x pixel tracked: {} vs {}",
        after.x,
        before.x + 12.0
    );
    assert!(
        (after.y - (before.y - 7.0)).abs() < 1e-6,
        "y pixel tracked: {} vs {}",
        after.y,
        before.y - 7.0
    );
}

/// Without a homography the drag is the plain uniform-scale move — with
/// bed y **up** (machine frame): dragging the mouse up (−dpy, toward the
/// top of the image) *increases* the bed-mm y of the placement.
#[test]
fn place_drag_uniform_without_homography() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.fiducials.homography = None;
    app.placement.px_per_mm = 10.0;
    app.placement.frame_img = Some(image::GrayImage::from_pixel(100, 100, image::Luma([120])));
    app.placement.tx_mm = 5.0;
    app.placement.ty_mm = 5.0;
    app.drag_place_px(20.0, -30.0).unwrap();
    assert!((app.placement.tx_mm - (5.0 + 2.0)).abs() < 1e-9);
    assert!(
        (app.placement.ty_mm - (5.0 + 3.0)).abs() < 1e-9,
        "mouse up = bed y up: {}",
        app.placement.ty_mm
    );
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
    a.save_settings_if_changed(); // what the per-frame hook does

    // A fresh console over the same DB (a "restart") reloads them.
    let b = ConsoleApp::new(db, vec!["true".into()]);
    assert_eq!(b.job.emit_copper, "/board/F_Cu.gbr");
    assert_eq!(b.job.emit_outline, "/board/Edge.gbr");
    assert!((b.job.offset_mm - 0.05).abs() < 1e-9);
    assert_eq!(b.placement.lbrn2, "placed.lbrn2");
    assert_eq!(b.fiducials.layout, "10,10; 60,10; 10,60; 60,60");
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
calib_saved_at=123456\n";
    std::fs::write(&settings, legacy).unwrap();

    let app = ConsoleApp::new(db, vec!["true".into()]);
    assert_eq!(app.job.emit_copper, "C:/boards/F_Cu.gbr");
    assert_eq!(app.camera.orientation, Orientation::Rotate180);
    assert_eq!(app.calibration.n, 9);
    assert_eq!(app.calibration.dot_kind, crate::calib::DotKind::Bright);
    assert!(app.placement.field_correct);

    let before = crate::settings::parse(legacy);
    let after = crate::settings::parse(&app.settings_blob());
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for (key, value) in before {
        assert_eq!(after.get(&key), Some(&value), "setting {key}");
    }
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
    app.calibration.anchor = Some(crate::calib::Calibration {
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
    a.calibration.n = 7;
    a.calibration.pitch_mm = 10.0;
    a.calibration.anchor = Some(crate::calib::Calibration {
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
    assert_eq!(b.calibration.n, 7);
}

/// Camera-lens calibration through the console: a printed-grid frame + 4
/// corner clicks → a lens fit with per-dot distortion feedback; and the
/// tab lays out with the arrows drawn.
#[test]
fn camera_lens_calibration_flow() {
    let grid = crate::calib::GridSpec {
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
    app.calibration.n = 7;
    app.calibration.pitch_mm = 10.0;
    app.calibration.dot_mm = dot;
    app.calibration.frame_img = Some(img);
    // Corner clicks at the four grid corners (px = mm*ppm + 40).
    app.calibration.corners = vec![(40.0, 40.0), (640.0, 40.0), (640.0, 640.0), (40.0, 640.0)];
    app.calibrate_fit();
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
    app.calibration.n = 7;
    app.calibration.pitch_mm = 10.0;
    app.calibration.dot_mm = 2.0;
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
    app.calibration.anchor = Some(crate::calib::Calibration {
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
    app.calibration.n = 7;
    app.calibration.pitch_mm = 10.0;
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

/// A failed re-fit (wrong corners/polarity — the operator's 0/49 case)
/// must keep the working calibration, not erase it (LR-16).
#[test]
fn a_failed_fit_keeps_the_previous_calibration() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.calibration.anchor = Some(crate::calib::Calibration {
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
/// centre in the overlay's own frame — under an active homography, not the
/// uniform scale that would render it far off-centre (LR-42).
#[test]
fn initial_placement_centers_under_an_active_homography() {
    let mut app = ConsoleApp::new(tmp_db(), vec!["true".into()]);
    app.placement.px_per_mm = 10.0;
    // A keystone homography (bed-mm → px) as the active placement map.
    let h = vision::Homography {
        matrix: nalgebra::Matrix3::new(
            9.8, 0.2, 15.0, //
            -0.1, 10.1, -8.0, //
            0.0006, -0.0004, 1.0,
        ),
        residuals: vec![],
        rms: 0.0,
    };
    app.fiducials.homography = Some(h.clone());
    let (w, ht) = (400.0, 300.0);
    let (tx, ty) = app.initial_center_mm(w, ht).unwrap();
    // The job pivot placed at (tx,ty) must map back to the pixel centre.
    let c = h.apply(nalgebra::Point2::new(tx, ty));
    assert!(
        (c.x - w / 2.0).abs() < 1e-3 && (c.y - ht / 2.0).abs() < 1e-3,
        "pivot maps to ({:.2},{:.2}), want ({},{})",
        c.x,
        c.y,
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
    app.emit_at_placement();
    let last = app.runtime.log.last().expect("a log line was pushed");
    assert!(
        last.err && last.text.contains("back-side"),
        "expected a back-side refusal, got: {}",
        last.text
    );
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
    app.calibration.anchor = Some(crate::calib::Calibration {
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
