//! End-to-end test of `pcbforge cut` (CAM-10): a KiCad-style Edge.Cuts
//! Gerber fixture → per-focus-step SVG/DXF files + a cut-schedule.txt that
//! names every step file and spells out the focal-plane drops.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pcbforge-cut-e2e-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn cut_writes_per_step_files_and_a_focus_schedule() {
    let out = tmp_dir("out");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "cut",
            "--outline",
            fixture("demo-Edge_Cuts.gbr").to_str().unwrap(),
            "--thickness-mm",
            "1.6",
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("binary runs");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    // Un-calibrated defaults: 1.6 + 0.1 mm over 0.2 mm steps = 9 files.
    let schedule = std::fs::read_to_string(out.join("cut-schedule.txt")).unwrap();
    let mut n_steps = 0;
    for i in 1..=9 {
        let stem = format!("cut-step-{i:02}");
        let svg = out.join(format!("{stem}.svg"));
        let dxf = out.join(format!("{stem}.dxf"));
        assert!(svg.is_file(), "missing {}", svg.display());
        assert!(dxf.is_file(), "missing {}", dxf.display());
        // The schedule must reference every step file it wrote.
        assert!(schedule.contains(&stem), "schedule omits {stem}");
        n_steps += 1;
    }
    assert_eq!(n_steps, 9);
    assert!(
        !out.join("cut-step-10.svg").exists(),
        "should be exactly 9 steps"
    );

    // The focus-lowering instruction and the calibration warning must be there.
    assert!(
        schedule.contains("LOWER THE HEAD"),
        "schedule must spell out the focal-plane drop"
    );
    assert!(
        schedule.contains("final step"),
        "the last step must be marked as the through-cut"
    );
    assert!(
        schedule.contains("UN-CALIBRATED"),
        "defaults must warn the operator to run the ladder"
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("placeholder defaults"));

    // The DXF holds open cut polylines on the CUT layer.
    let dxf = std::fs::read_to_string(out.join("cut-step-01.dxf")).unwrap();
    assert!(dxf.contains("8\nCUT\n"));
    assert!(dxf.contains("70\n0\n"), "cut segments are open polylines");
    assert!(dxf.ends_with("0\nEOF\n"));
}

#[test]
fn calibrated_run_has_no_warning() {
    let out = tmp_dir("cal");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "cut",
            "--outline",
            fixture("demo-Edge_Cuts.gbr").to_str().unwrap(),
            "--thickness-mm",
            "1.6",
            "--kerf-mm",
            "0.04",
            "--mm-per-pass",
            "0.06",
            "--z-step-mm",
            "0.18",
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("binary runs");
    assert!(result.status.success());
    let schedule = std::fs::read_to_string(out.join("cut-schedule.txt")).unwrap();
    assert!(!schedule.contains("UN-CALIBRATED"), "measured values used");
    assert!(!String::from_utf8_lossy(&result.stderr).contains("placeholder"));
}

#[test]
fn missing_source_is_an_error() {
    let out = tmp_dir("err");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args(["cut", "--out", out.to_str().unwrap()])
        .output()
        .expect("binary runs");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--board or --outline"));
}

#[test]
fn outline_without_thickness_is_an_error() {
    let out = tmp_dir("nothick");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "cut",
            "--outline",
            fixture("demo-Edge_Cuts.gbr").to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("binary runs");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--thickness-mm"));
}
