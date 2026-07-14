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
