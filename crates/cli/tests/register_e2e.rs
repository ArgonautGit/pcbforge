//! End-to-end test of `pcbforge register` (VIS-6 host side): fit a
//! design→machine affine from fiducial correspondences and bake it into the
//! emitted `.lbrn2`. The frame contract is that correspondences are in the
//! Gerber frame, so these tests use Gerber-frame points.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-reg-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// All vertices across the emitted job.
fn verts(doc: &str) -> Vec<(f64, f64)> {
    doc.split('V')
        .skip(1)
        .filter_map(|t| {
            let xy = t.split('c').next()?;
            let mut it = xy.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        })
        .collect()
}

fn register(extra: &[&str], name: &str) -> (bool, String, String) {
    let out = tmp("run").join(name);
    let copper = fixture("uv_test-F_Cu.gbr");
    let outline = fixture("uv_test-Edge_Cuts.gbr");
    let default_field = (!extra.contains(&"--field-map")).then(|| identity_field_map("register"));
    let mut args = vec![
        "register",
        "--copper",
        copper.to_str().unwrap(),
        "--outline",
        outline.to_str().unwrap(),
        "--lbrn2",
        out.to_str().unwrap(),
    ];
    if let Some(path) = &default_field {
        args.extend(["--field-map", path.to_str().unwrap()]);
    }
    args.extend_from_slice(extra);
    let r = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args(&args)
        .output()
        .expect("binary runs");
    let doc = std::fs::read_to_string(&out).unwrap_or_default();
    (
        r.status.success(),
        doc,
        String::from_utf8_lossy(&r.stderr).into_owned(),
    )
}

fn emit(extra: &[&str], name: &str) -> (bool, String, String) {
    let out = tmp("emit").join(name);
    let copper = fixture("uv_test-F_Cu.gbr");
    let outline = fixture("uv_test-Edge_Cuts.gbr");
    let default_field = (!extra.contains(&"--field-map")).then(|| identity_field_map("emit"));
    let mut args = vec![
        "emit",
        "--copper",
        copper.to_str().unwrap(),
        "--outline",
        outline.to_str().unwrap(),
        "--lbrn2",
        out.to_str().unwrap(),
        "--origin-x",
        "130",
        "--origin-y=-85",
        "--center",
    ];
    if let Some(path) = &default_field {
        args.extend(["--field-map", path.to_str().unwrap()]);
    }
    args.extend_from_slice(extra);
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args(&args)
        .output()
        .expect("binary runs");
    let doc = std::fs::read_to_string(&out).unwrap_or_default();
    (
        result.status.success(),
        doc,
        String::from_utf8_lossy(&result.stderr).into_owned(),
    )
}

fn pincushion_field_map(tag: &str) -> PathBuf {
    use nalgebra::Point2;
    let field_center = (130.0, -85.0);
    let laser = |cx: f64, cy: f64| {
        let (du, dv) = (cx - field_center.0, cy - field_center.1);
        let r2 = (du * du + dv * dv) / (70.0 * 70.0);
        let f = 1.0 + 0.03 * r2;
        (field_center.0 + du * f, field_center.1 + dv * f)
    };
    let mut pairs = Vec::new();
    for r in 0..7 {
        for c in 0..7 {
            let cmd = (80.0 + c as f64 * 16.0, -130.0 + r as f64 * 16.0);
            let phys = laser(cmd.0, cmd.1);
            pairs.push((Point2::new(phys.0, phys.1), Point2::new(cmd.0, cmd.1)));
        }
    }
    let field = vision::fit_field(&pairs).expect("fit field");
    let path = tmp(tag).join("field.txt");
    std::fs::write(&path, field.serialize()).unwrap();
    path
}

fn identity_field_map(tag: &str) -> PathBuf {
    use nalgebra::Point2;
    let coordinates = [-150.0, -50.0, 50.0, 150.0];
    let pairs: Vec<_> = coordinates
        .iter()
        .flat_map(|&y| {
            coordinates.iter().map(move |&x| {
                let point = Point2::new(x, y);
                (point, point)
            })
        })
        .collect();
    let field = vision::fit_field(&pairs).expect("fit identity field");
    let path = tmp(tag).join("identity-field.txt");
    std::fs::write(&path, field.serialize()).unwrap();
    path
}

#[test]
fn production_emit_refuses_to_run_without_a_field_map() {
    let output = tmp("missing-field").join("uncalibrated.lbrn2");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "emit",
            "--copper",
            fixture("uv_test-F_Cu.gbr").to_str().unwrap(),
            "--lbrn2",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("binary runs");
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("--field-map"),
        "the CLI must name the missing mandatory calibration"
    );
    assert!(!output.exists(), "no unwarped production file is written");
}

/// Identity correspondences leave the Gerber-frame geometry untouched; a pure
/// translation shifts every vertex by exactly that amount.
#[test]
fn identity_unchanged_translation_shifts_exactly() {
    // Three non-collinear points inside the uv_test Gerber bbox.
    let (ok_id, id_doc, _) = register(
        &[
            "--fiducials",
            "131,-92=131,-92; 146,-92=146,-92; 131,-81=131,-81",
        ],
        "id.lbrn2",
    );
    assert!(ok_id, "identity register succeeds");
    let (ok_tr, tr_doc, _) = register(
        &[
            "--fiducials",
            "131,-92=181,-62; 146,-92=196,-62; 131,-81=181,-51",
        ],
        "tr.lbrn2",
    );
    assert!(ok_tr, "translation register succeeds");

    let id = verts(&id_doc);
    let tr = verts(&tr_doc);
    assert_eq!(id.len(), tr.len());
    assert!(!id.is_empty());
    for ((ix, iy), (tx, ty)) in id.iter().zip(&tr) {
        assert!((tx - ix - 50.0).abs() < 1e-3, "x shift: {ix}→{tx}");
        assert!((ty - iy - 30.0).abs() < 1e-3, "y shift: {iy}→{ty}");
    }
}

/// A fit that doesn't match (garbage correspondences with high residual) is
/// rejected rather than silently baking a bad transform.
#[test]
fn high_residual_fit_is_rejected() {
    // Four points whose mapping no affine can satisfy well → large RMS.
    let (ok, _, stderr) = register(
        &[
            "--fiducials",
            "131,-92=0,0; 146,-92=100,0; 131,-81=0,100; 146,-81=999,999",
            "--max-rms-mm",
            "0.05",
        ],
        "bad.lbrn2",
    );
    assert!(!ok, "a high-RMS fit must fail");
    assert!(
        stderr.contains("RMS") && stderr.contains("exceeds"),
        "stderr names the residual: {stderr}"
    );
}

#[test]
fn too_few_fiducials_is_rejected() {
    let (ok, _, stderr) = register(&["--fiducials", "0,0=0,0; 1,1=1,1"], "few.lbrn2");
    assert!(!ok);
    assert!(
        stderr.contains("≥3") || stderr.contains("3 "),
        "stderr: {stderr}"
    );
}

/// The `--frame` detection path: build a synthetic bed frame with three dark
/// holes at known machine positions, and confirm register detects them, fits a
/// low-residual affine, and writes the job.
#[test]
fn frame_detection_path_fits_and_emits() {
    // 10 px/mm; holes at machine mm (13,10),(28,10),(13,25) → px centers.
    // Machine/bed mm is y-up with the origin at the frame's bottom-left, so
    // the pixel row is flipped against the frame height.
    let ppm = 10.0f64;
    let holes = [(13.0, 10.0), (28.0, 10.0), (13.0, 25.0)];
    let (w, h) = (400u32, 320u32);
    let mut seed = 1u64;
    let img = image::GrayImage::from_fn(w, h, |x, y| {
        let bg = 140.0 + 70.0 * (x as f64 + y as f64) / (w + h) as f64;
        let mut v = bg;
        for (mx, my) in holes {
            let (cx, cy) = (mx * ppm, h as f64 - my * ppm);
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < 0.5 * ppm {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            v -= 90.0 * cover;
        }
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let n = ((seed.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64 * 2.0
            - 1.0)
            * 5.0;
        image::Luma([(v + n).clamp(0.0, 255.0) as u8])
    });
    let frame_path = tmp("frame").join("bed.png");
    img.save(&frame_path).unwrap();

    // Design layout == the machine positions here (identity-ish), so the fit is
    // near-identity and the residual is tiny — this exercises detect→fit→emit.
    let (ok, doc, stderr) = register(
        &[
            "--frame",
            frame_path.to_str().unwrap(),
            "--layout",
            "13,10; 28,10; 13,25",
            "--px-per-mm",
            "10",
        ],
        "frame.lbrn2",
    );
    assert!(ok, "frame register succeeds; stderr: {stderr}");
    assert!(
        stderr.contains("fit 3 fiducials"),
        "detected all three: {stderr}"
    );
    assert!(!verts(&doc).is_empty(), "job has geometry");
}

/// The required `--field-map` bakes the laser field pre-distortion into the
/// emit. Both the identity and distorted maps densify every edge; the
/// distorted map additionally moves the vertices. The correction math itself is
/// unit-tested in `vision`; this proves the CLI wiring applies it.
#[test]
fn field_map_predistorts_and_subdivides_the_emit() {
    let map_path = pincushion_field_map("field");

    let corr = "131,-92=131,-92; 146,-92=146,-92; 131,-81=131,-81";
    let (ok_base, base_doc, _) = register(
        &["--fiducials", corr, "--field-seg-mm", "1.0"],
        "fld-base.lbrn2",
    );
    assert!(ok_base);
    let (ok, doc, stderr) = register(
        &[
            "--fiducials",
            corr,
            "--field-map",
            map_path.to_str().unwrap(),
            "--field-seg-mm",
            "1.0",
        ],
        "fld.lbrn2",
    );
    assert!(ok, "field-map register succeeds; stderr: {stderr}");
    assert!(
        stderr.contains("mandatory field warp on"),
        "stderr reports the correction: {stderr}"
    );

    let base = verts(&base_doc);
    let warped = verts(&doc);
    assert!(
        warped.len() == base.len(),
        "both required field maps densify identically: {} vs {} vertices",
        base.len(),
        warped.len()
    );
    // The pre-distortion actually moved geometry: the warped bbox differs from
    // the affine-only baseline by a measurable amount (not a silent no-op).
    let bbox = |v: &[(f64, f64)]| {
        v.iter().fold(
            (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
            |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        )
    };
    let (bx0, by0, bx1, by1) = bbox(&base);
    let (wx0, wy0, wx1, wy1) = bbox(&warped);
    let shift = (bx0 - wx0).abs() + (by0 - wy0).abs() + (bx1 - wx1).abs() + (by1 - wy1).abs();
    assert!(
        shift > 0.01,
        "pre-distortion shifts the geometry: {shift:.4} mm"
    );
}

#[test]
fn field_map_predistorts_every_direct_emit_edge() {
    let map_path = pincushion_field_map("emit-field");
    let (base_ok, base_doc, base_stderr) = emit(&["--field-seg-mm", "1.0"], "emit-base.lbrn2");
    assert!(base_ok, "baseline emit succeeds: {base_stderr}");
    let (ok, warped_doc, stderr) = emit(
        &[
            "--field-map",
            map_path.to_str().unwrap(),
            "--field-seg-mm",
            "1.0",
        ],
        "emit-field.lbrn2",
    );
    assert!(ok, "field-warped emit succeeds: {stderr}");
    assert!(
        stderr.contains("emit: mandatory field warp on"),
        "stderr reports mandatory-capable warp path: {stderr}"
    );
    assert!(
        verts(&warped_doc).len() == verts(&base_doc).len(),
        "direct emit densifies every edge for every required field map"
    );
    assert_ne!(warped_doc, base_doc, "the nonlinear map must move geometry");
}

#[test]
fn frame_and_fiducials_are_mutually_exclusive() {
    let (ok, _, stderr) = register(
        &[
            "--fiducials",
            "0,0=0,0; 1,0=1,0; 0,1=0,1",
            "--frame",
            "/nonexistent.png",
        ],
        "both.lbrn2",
    );
    assert!(!ok);
    assert!(stderr.contains("not both"), "stderr: {stderr}");
}
