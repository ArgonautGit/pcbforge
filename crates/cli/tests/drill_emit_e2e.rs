//! End-to-end tests of `pcbforge drill-emit`: Excellon drill file(s) — or a
//! KiCad board via kicad-cli — → a LightBurn `.lbrn2` job of pure hole
//! geometry (one closed outline per round hole or G85 slot).

use std::path::{Path, PathBuf};
use std::process::Command;

/// KiCad-dialect Excellon: two tools, four round holes, one G85 slot — the
/// same drill set `samples/kicad/valdemo2.kicad_pcb` exports.
const DRL: &str = "\
M48
; DRILL file {KiCad 7.0.11} date 2026-01-01
; FORMAT={-:-/ absolute / metric / decimal}
FMAT,2
METRIC
T1C0.400
T2C1.000
%
G90
G05
T1
X108.0Y-111.0
X110.0Y-100.0
T2
X100.0Y-100.0
X102.54Y-100.0
T2
X105.08Y-99.8G85X105.08Y-100.2
G05
M30
";

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-drillemit-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_drl(dir: &Path) -> PathBuf {
    let p = dir.join("holes.drl");
    std::fs::write(&p, DRL).unwrap();
    p
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args(args)
        .output()
        .expect("binary runs")
}

/// All vertices across every shape in the document, mm.
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

fn bbox(pts: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    pts.iter().fold(
        (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
        |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
    )
}

#[test]
fn drill_emit_writes_one_shape_per_hole() {
    let dir = tmp("fill");
    let drl = write_drl(&dir);
    let out = dir.join("drills.lbrn2");
    let result = run(&[
        "drill-emit",
        "--drills",
        drl.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("4 hole(s) + 1 slot(s)"),
        "summary reports the extraction: {stderr}"
    );

    let doc = std::fs::read_to_string(&out).unwrap();
    // Valid project shell, default Fill layer named DRILL.
    assert!(doc.starts_with("<?xml"));
    assert!(doc.contains("<LightBurnProject"));
    assert!(doc.trim_end().ends_with("</LightBurnProject>"));
    assert!(doc.contains("type=\"Scan\""));
    assert!(doc.contains("<name Value=\"DRILL\"/>"));

    // Pure hole geometry: exactly one closed Path per hole/slot, nothing else.
    let shape_count = doc.matches("Type=\"Path\"").count();
    assert_eq!(shape_count, 5, "4 round holes + 1 slot");
    assert_eq!(doc.matches("<PrimList>LineClosed</PrimList>").count(), 5);

    // Unique VertID per shape (fan-burn regression).
    let mut vert_ids: Vec<&str> = doc
        .split("VertID=\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap())
        .collect();
    vert_ids.sort_unstable();
    vert_ids.dedup();
    assert_eq!(
        vert_ids.len(),
        shape_count,
        "every shape owns a unique VertID"
    );

    // Frame normalization: the KiCad drill frame sits at negative y; the job
    // must land on the workspace with its corner at the origin, extent kept.
    assert!(!doc.contains("V-"), "no negative coordinates");
    let (x0, y0, x1, y1) = bbox(&verts(&doc));
    assert!(x0.abs() < 1e-6 && y0.abs() < 1e-6, "corner at origin");
    // Raw extent: x 99.5..110.2 (Ø1.0 at x=100 … Ø0.4 at x=110), y −111.2..
    // −99.3 (Ø0.4 at y=−111 … slot top at −99.8+0.5). Vertices lie ON the
    // ideal circles, so the polygon bbox is a hair inside the true one.
    assert!((x1 - 10.7).abs() < 0.01, "width {x1}, want ≈10.7");
    assert!((y1 - 11.9).abs() < 0.01, "height {y1}, want ≈11.9");
}

#[test]
fn line_mode_traces_hole_outlines() {
    let dir = tmp("line");
    let drl = write_drl(&dir);
    let out = dir.join("drills.lbrn2");
    let result = run(&[
        "drill-emit",
        "--drills",
        drl.to_str().unwrap(),
        "--mode",
        "line",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let doc = std::fs::read_to_string(&out).unwrap();
    assert!(doc.contains("type=\"Cut\""), "line mode is a Cut layer");
    assert!(!doc.contains("type=\"Scan\""));
    assert_eq!(doc.matches("Type=\"Path\"").count(), 5);
}

#[test]
fn placement_flags_translate_the_pattern() {
    let dir = tmp("place");
    let drl = write_drl(&dir);
    let out = dir.join("drills.lbrn2");
    let result = run(&[
        "drill-emit",
        "--drills",
        drl.to_str().unwrap(),
        "--origin-x",
        "25",
        "--origin-y",
        "40",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let (x0, y0, x1, _) = bbox(&verts(&std::fs::read_to_string(&out).unwrap()));
    assert!((x0 - 25.0).abs() < 1e-6, "corner x {x0}, want 25");
    assert!((y0 - 40.0).abs() < 1e-6, "corner y {y0}, want 40");
    assert!((x1 - x0 - 10.7).abs() < 0.01, "extent preserved");
}

/// --outline pins the frame to the board outline's corner (the corner `emit`
/// normalizes to) instead of the drill pattern's own bbox, so a drill job
/// co-registers with a copper job emitted from the same board.
#[test]
fn outline_pins_the_frame_to_the_board_corner() {
    let dir = tmp("outline");
    let drl = write_drl(&dir);
    let outline = fixture("demo-Edge_Cuts.gbr");
    let out = dir.join("drills.lbrn2");
    let result = run(&[
        "drill-emit",
        "--drills",
        drl.to_str().unwrap(),
        "--outline",
        outline.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let emitted = verts(&std::fs::read_to_string(&out).unwrap());

    // Expected: raw drill geometry translated by −(outline region min), both
    // computed with the same library code the command uses.
    let ops = ingest::excellon::parse_excellon(DRL).unwrap();
    let entries: Vec<cam::process::DrillEntry> = ops
        .iter()
        .map(|op| match *op {
            ingest::excellon::DrillOp::Hole {
                center,
                diameter_nm,
            } => cam::process::DrillEntry {
                x_nm: center.x,
                y_nm: center.y,
                diameter_nm,
                slot_end: None,
            },
            ingest::excellon::DrillOp::Slot { a, b, diameter_nm } => cam::process::DrillEntry {
                x_nm: a.x,
                y_nm: a.y,
                diameter_nm,
                slot_end: Some((b.x, b.y)),
            },
        })
        .collect();
    let raw = cam::drill::drill_polys(&entries);
    let region = cam::noncopper::board_region_from_outline(
        &ingest::gerber::load_gerber(&outline).unwrap().polys,
    );
    let (omin_x, omin_y) = region
        .iter()
        .flat_map(|p| p.outer.iter().chain(p.holes.iter().flatten()))
        .fold((i64::MAX, i64::MAX), |(mx, my), p| {
            (mx.min(p.x), my.min(p.y))
        });
    let (rmin_x, rmin_y) = raw
        .iter()
        .flat_map(|p| p.outer.iter())
        .fold((i64::MAX, i64::MAX), |(mx, my), p| {
            (mx.min(p.x), my.min(p.y))
        });
    let mm = |nm: i64| nm as f64 / 1_000_000.0;
    let want_x = mm(rmin_x - omin_x);
    let want_y = mm(rmin_y - omin_y);

    let (x0, y0, _, _) = bbox(&emitted);
    assert!(
        (x0 - want_x).abs() < 1e-6,
        "emitted min x {x0}, want drill-min − outline-min = {want_x}"
    );
    assert!(
        (y0 - want_y).abs() < 1e-6,
        "emitted min y {y0}, want drill-min − outline-min = {want_y}"
    );
}

/// `--board` drives kicad-cli itself; self-skips (like the ingest tests) when
/// kicad-cli isn't installed.
#[test]
fn board_exports_drills_via_kicad_cli() {
    if !ingest::kicad_cli::available() {
        eprintln!("SKIP board_exports_drills_via_kicad_cli: kicad-cli not available");
        return;
    }
    let dir = tmp("board");
    let board = repo_root().join("samples/kicad/valdemo2.kicad_pcb");
    let out = dir.join("drills.lbrn2");
    let result = run(&[
        "drill-emit",
        "--board",
        board.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let doc = std::fs::read_to_string(&out).unwrap();
    // valdemo2: 2× Ø1.0 pad + 2× Ø0.4 via round holes, 1 oval pad slot.
    assert_eq!(doc.matches("Type=\"Path\"").count(), 5);
    assert!(!doc.contains("V-"), "job sits on the workspace");
}

#[test]
fn rejects_missing_input_and_bad_mode() {
    let dir = tmp("bad");
    let out = dir.join("x.lbrn2");
    let none = run(&["drill-emit", "--out", out.to_str().unwrap()]);
    assert!(!none.status.success());
    assert!(
        String::from_utf8_lossy(&none.stderr).contains("supply --drills"),
        "points at the two input forms"
    );

    let drl = write_drl(&dir);
    let bad_mode = run(&[
        "drill-emit",
        "--drills",
        drl.to_str().unwrap(),
        "--mode",
        "raster",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(!bad_mode.status.success());
    assert!(String::from_utf8_lossy(&bad_mode.stderr).contains("--mode"));
}
