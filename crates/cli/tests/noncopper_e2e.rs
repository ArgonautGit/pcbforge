//! End-to-end test of the FlatCAM-replacement pipeline: KiCad-style Gerber
//! fixtures → `pcbforge noncopper` → DXF/SVG, plus the library-level tiling
//! invariant (copper ∪ non-copper exactly tiles the board region).
//!
//! The fixtures are hand-authored in KiCad's output style (X46/mm, X2
//! attributes, RoundRect macro, slit-connected zone); a golden run against a
//! real `kicad-cli` export is still pending real sample boards (see
//! docs/decisions.md).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn tmp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pcbforge-nc-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn binary_inverts_fixture_board_and_writes_outputs() {
    let dir = tmp_dir();
    let dxf = dir.join("nc.dxf");
    let svg = dir.join("nc.svg");
    let preview = dir.join("nc-preview.svg");

    let out = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "noncopper",
            "--copper",
            fixture("demo-F_Cu.gbr").to_str().unwrap(),
            "--outline",
            fixture("demo-Edge_Cuts.gbr").to_str().unwrap(),
            "--offset-mm",
            "0.05",
            "--dxf",
            dxf.to_str().unwrap(),
            "--svg",
            svg.to_str().unwrap(),
            "--preview",
            preview.to_str().unwrap(),
        ])
        .output()
        .expect("binary runs");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let dxf_text = std::fs::read_to_string(&dxf).unwrap();
    let polylines = dxf_text.matches("POLYLINE").count();
    assert!(polylines >= 2, "expected several rings, got {polylines}");
    assert_eq!(dxf_text.matches("SEQEND").count(), polylines);
    assert!(dxf_text.ends_with("0\nEOF\n"));

    let svg_text = std::fs::read_to_string(&svg).unwrap();
    assert!(svg_text.matches("<path").count() >= 1);
    assert!(svg_text.contains("evenodd"));
    assert!(std::fs::metadata(&preview).unwrap().len() > 0);
}

#[test]
fn missing_output_flags_is_an_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "noncopper",
            "--copper",
            fixture("demo-F_Cu.gbr").to_str().unwrap(),
        ])
        .output()
        .expect("binary runs");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--dxf"));
}

/// The invariant that makes the tool trustworthy: at zero offset, copper and
/// non-copper tile the board region exactly — checked both in exact
/// geometric area and pixel-by-pixel at 10 µm/px.
#[test]
fn copper_and_noncopper_tile_the_board_exactly() {
    let copper = ingest::gerber::load_gerber(&fixture("demo-F_Cu.gbr")).unwrap();
    let outline = ingest::gerber::load_gerber(&fixture("demo-Edge_Cuts.gbr")).unwrap();
    let board = cam::noncopper::board_region_from_outline(&outline.polys);
    assert_eq!(board.len(), 1, "one board region");

    // Copper must lie inside the board.
    let clipped = cam::geom::intersect(&copper.polys, &board);
    let copper_area = cam::geom::area_nm2(&copper.polys);
    assert!(
        (cam::geom::area_nm2(&clipped) - copper_area).abs() / copper_area < 1e-9,
        "fixture copper must be inside the outline"
    );

    let nc = cam::noncopper::noncopper(&board, &copper.polys, 0);

    // Exact area tiling.
    let board_area = cam::geom::area_nm2(&board);
    let total = cam::geom::area_nm2(&nc) + copper_area;
    assert!(
        (total - board_area).abs() / board_area < 1e-9,
        "area tiling: copper {copper_area} + nc {} != board {board_area}",
        cam::geom::area_nm2(&nc)
    );
    assert!(cam::geom::intersect(&nc, &copper.polys).is_empty());

    // Pixel tiling at 10 µm/px: rasterize(copper ∪ nc) == rasterize(board).
    // The union reaches the board edge, so both rasters share the same frame.
    let both = cam::geom::union(&copper.polys, &nc);
    let img_union = testkit::rasterize(&pcb_core::Layer { polys: both }, 10);
    let img_board = testkit::rasterize(&pcb_core::Layer { polys: board }, 10);
    testkit::assert_images_agree(&img_union, &img_board, 0.9995);
}

/// With a beam offset, every non-copper vertex keeps its distance from the
/// copper (checked through cam::split's exact distance helper).
#[test]
fn offset_keeps_beam_clearance() {
    let copper = ingest::gerber::load_gerber(&fixture("demo-F_Cu.gbr")).unwrap();
    let outline = ingest::gerber::load_gerber(&fixture("demo-Edge_Cuts.gbr")).unwrap();
    let board = cam::noncopper::board_region_from_outline(&outline.polys);

    let off_nm = 50_000; // 0.05 mm
    let nc = cam::noncopper::noncopper(&board, &copper.polys, off_nm);
    assert!(!nc.is_empty());
    for shape in &nc {
        let d = cam::split::min_dist_to_polys_nm(&shape.outer, true, &copper.polys);
        assert!(
            d >= (off_nm - 2_000) as f64, // 2 µm arc-flattening tolerance
            "non-copper ring only {d} nm from copper"
        );
    }
}
