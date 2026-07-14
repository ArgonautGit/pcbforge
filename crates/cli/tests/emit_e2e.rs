//! End-to-end test of `pcbforge emit` (EMIT-3): KiCad-style copper + outline
//! Gerbers → a LightBurn `.lbrn2` Fill layer of the non-copper regions.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-emit-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn emit_writes_a_fill_layer_lbrn2() {
    let out = tmp("out").join("board.lbrn2");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "emit",
            "--copper",
            fixture("demo-F_Cu.gbr").to_str().unwrap(),
            "--outline",
            fixture("demo-Edge_Cuts.gbr").to_str().unwrap(),
            "--offset-mm",
            "0.05",
            "--frequency-khz",
            "30",
            "--pulse-ns",
            "2",
            "--passes",
            "3",
            "--lbrn2",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("binary runs");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let doc = std::fs::read_to_string(&out).unwrap();
    // Valid project shell.
    assert!(doc.starts_with("<?xml"));
    assert!(doc.contains("<LightBurnProject"));
    assert!(doc.trim_end().ends_with("</LightBurnProject>"));
    // One Fill CutSetting with the schema's units (kHz -> Hz) and our params.
    assert!(doc.contains("type=\"Scan\""));
    assert!(
        doc.contains("<frequency Value=\"30000\"/>"),
        "kHz written as Hz"
    );
    assert!(doc.contains("<QPulseWidth Value=\"2\"/>"));
    assert!(doc.contains("<numPasses Value=\"3\"/>"));
    // The non-copper regions became closed Path shapes.
    assert!(doc.contains("Type=\"Path\""));
    assert!(doc.contains("<PrimList>LineClosed</PrimList>"));
    // Frame normalization: the job sits on the workspace (no negative
    // coordinate — the fixture's stroked outline would otherwise emit
    // V-0.025, and real KiCad exports sit entirely below y = 0).
    assert!(
        !doc.contains("V-"),
        "emitted geometry must not contain negative coordinates"
    );
}

/// Regression on the operator's real KiCad 10 board (uv_test): the first burn
/// produced a fan of rays from the board corner because all 37 Path shapes
/// shared VertID/PrimID 0 and LightBurn cross-linked their vertex lists. The
/// emitted file must give every shape a unique ID, sit on the workspace, and
/// contain the expected ring count.
#[test]
fn uv_test_board_emits_unique_shape_ids() {
    let out = tmp("uvtest").join("job.lbrn2");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "emit",
            "--copper",
            fixture("uv_test-F_Cu.gbr").to_str().unwrap(),
            "--outline",
            fixture("uv_test-Edge_Cuts.gbr").to_str().unwrap(),
            "--offset-mm",
            "0.025",
            "--lbrn2",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("binary runs");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let doc = std::fs::read_to_string(&out).unwrap();

    let shape_count = doc.matches("Type=\"Path\"").count();
    assert!(
        shape_count > 30,
        "uv_test yields many rings, got {shape_count}"
    );

    let mut vert_ids: Vec<&str> = doc
        .split("VertID=\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap())
        .collect();
    assert_eq!(vert_ids.len(), shape_count);
    vert_ids.sort_unstable();
    vert_ids.dedup();
    assert_eq!(
        vert_ids.len(),
        shape_count,
        "every shape must own a unique VertID (fan-burn regression)"
    );
    assert!(!doc.contains("V-"), "job must sit on the workspace");
}

#[test]
fn emit_rejects_out_of_range_offset() {
    let out = tmp("bad").join("x.lbrn2");
    let result = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "emit",
            "--copper",
            fixture("demo-F_Cu.gbr").to_str().unwrap(),
            "--offset-mm",
            "50",
            "--lbrn2",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("binary runs");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("out of range"));
}
