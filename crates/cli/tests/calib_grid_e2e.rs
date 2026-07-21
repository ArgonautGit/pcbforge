//! End-to-end test of `pcbforge calib-grid`: emit an n×n dot grid `.lbrn2`
//! at known commanded coordinates for camera→laser calibration.

use std::path::PathBuf;
use std::process::Command;

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-calibgrid-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn calib_grid_emits_the_expected_dot_lattice() {
    let out = tmp().join("grid.lbrn2");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "calib-grid",
            "--out",
            out.to_str().unwrap(),
            "--n",
            "7",
            "--pitch-mm",
            "10",
            "--dot-mm",
            "0.4",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let doc = std::fs::read_to_string(&out).unwrap();
    // 49 lattice dots + 2 off-lattice orientation markers, each a closed Path.
    assert_eq!(
        doc.matches("Type=\"Path\"").count(),
        51,
        "7×7 lattice + 2 orientation markers"
    );
    assert!(doc.contains("<PrimList>LineClosed</PrimList>"));
    // The lattice reaches the far corner (a dot centered at (60,60) has an edge
    // at 60.2 mm).
    assert!(doc.contains("V60.2 "), "grid spans to x=60");
    assert!(doc.contains(" 60.2c"), "grid spans to y=60");
    // Valid project shell.
    assert!(doc.starts_with("<?xml") && doc.contains("<LightBurnProject"));
}

#[test]
fn calib_grid_rejects_bad_args() {
    let out = tmp().join("bad.lbrn2");
    let n1 = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args(["calib-grid", "--out", out.to_str().unwrap(), "--n", "1"])
        .output()
        .unwrap();
    assert!(!n1.status.success(), "n<2 rejected");

    let origin = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "calib-grid",
            "--out",
            out.to_str().unwrap(),
            "--origin",
            "nonsense",
        ])
        .output()
        .unwrap();
    assert!(!origin.status.success(), "bad --origin rejected");
}
