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

/// Even-odd membership of a point in the job's fill (all Path rings).
fn in_fill(doc: &str, px: f64, py: f64) -> bool {
    let mut crossings = 0usize;
    for shape in doc.split("<Shape Type=\"Path\"").skip(1) {
        let vl = shape
            .split("<VertList>")
            .nth(1)
            .and_then(|s| s.split("</VertList>").next())
            .unwrap_or("");
        let pts: Vec<(f64, f64)> = vl
            .split('V')
            .filter(|t| !t.is_empty())
            .filter_map(|t| {
                let xy = t.split('c').next()?;
                let mut it = xy.split_whitespace();
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            })
            .collect();
        let mut c = false;
        let mut j = pts.len().wrapping_sub(1);
        for i in 0..pts.len() {
            let (xi, yi) = pts[i];
            let (xj, yj) = pts[j];
            if (yi > py) != (yj > py) && px < xi + (py - yi) / (yj - yi) * (xj - xi) {
                c = !c;
            }
            j = i;
        }
        if c {
            crossings += 1;
        }
    }
    crossings % 2 == 1
}

/// The operator's second burn left the whole right side of uv_test un-ablated:
/// that area is a no-net zone the Gerber tags NonConductor, and it was kept as
/// copper. By default it must now be cleared (part of the fill) — at offset 0,
/// where no offset machinery can mask the result — and --keep-nonconductor
/// must restore the old behavior.
#[test]
fn nonconductor_zones_are_cleared_by_default() {
    let dir = tmp("noncond");
    let copper = fixture("uv_test-F_Cu.gbr");
    let outline = fixture("uv_test-Edge_Cuts.gbr");
    let run = |extra: &[&str], name: &str| -> String {
        let out = dir.join(name);
        let mut args = vec![
            "emit",
            "--copper",
            copper.to_str().unwrap(),
            "--outline",
            outline.to_str().unwrap(),
            "--lbrn2",
            out.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        let r = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
            .args(&args)
            .output()
            .expect("binary runs");
        assert!(
            r.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        std::fs::read_to_string(&out).unwrap()
    };

    // (12.5, 2.0) normalized = deep inside the right-side zone, well clear of
    // the hex traces and their clearance bands; (16.8, 6.0) = the bare margin
    // band between zone edge and outline (plain substrate, always fill);
    // (3.86, 3.27) = a pad center (never fill).
    let cleared = run(&[], "default.lbrn2");
    assert!(
        in_fill(&cleared, 12.5, 2.0),
        "no-net zone must be ablated by default"
    );
    assert!(in_fill(&cleared, 16.8, 6.0), "plain substrate is fill");
    assert!(!in_fill(&cleared, 3.86, 3.27), "pad copper is never fill");

    let kept = run(&["--keep-nonconductor"], "kept.lbrn2");
    assert!(
        !in_fill(&kept, 12.5, 2.0),
        "--keep-nonconductor must keep the zone as copper"
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
