//! CAM-10 fixture check against a real KiCad board (`samples/kicad/valdemo2`,
//! a 36 x 30 outline with a Ø4 mm circular cutout, 1.6 mm thick).
//!
//! kicad-cli exports the Edge.Cuts Gerber; the parser + `board_region` +
//! `cut_paths` must yield two cut rings (the cutout, then the perimeter), and
//! the focus schedule for 1.6 + 0.1 mm at the conservative defaults must be
//! the deterministic 9-step ladder. Self-skips when kicad-cli is absent so
//! plain `cargo test` stays green.

use std::path::{Path, PathBuf};

use cam::cut;
use ingest::kicad_cli::KicadCli;
use pcb_core::{CutOpts, NM_PER_MM, PathKind};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/cam is two levels below the root")
        .to_path_buf()
}

fn tmp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pcbforge-cut-valdemo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Larger bounding-box dimension (mm) over a set of path elements' points.
fn combined_extent_mm(elems: &[pcb_core::PathElem]) -> f64 {
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for e in elems {
        for p in &e.pts {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
    }
    ((x1 - x0).max(y1 - y0)) as f64 / NM_PER_MM as f64
}

#[test]
fn valdemo2_outline_cuts_the_cutout_before_the_perimeter() {
    if !ingest::kicad_cli::available() {
        eprintln!("SKIP: kicad-cli not installed; CAM-10 valdemo fixture not run");
        return;
    }
    let cli = KicadCli::discover().unwrap();
    let board = repo_root().join("samples/kicad/valdemo2.kicad_pcb");
    assert!(board.is_file(), "sample board missing");

    let dir = tmp_dir();
    let gerbers = cli.export_gerbers(&board, &["Edge.Cuts"], &dir).unwrap();
    let layer = ingest::gerber::load_gerber(&gerbers[0]).unwrap();
    let region = cam::noncopper::board_region_from_outline(&layer.polys);
    assert_eq!(region.len(), 1, "one board piece");
    assert_eq!(region[0].holes.len(), 1, "the circular cutout is a hole");

    let opts = CutOpts::default();
    let paths = cut::cut_paths(&region, &opts);
    assert!(
        paths
            .elems
            .iter()
            .all(|e| e.kind == PathKind::Cut && !e.closed)
    );

    // Exactly one tab group per ring: cutout then perimeter. The offset
    // artifact slivers (see cam::cut) must be filtered, so the count is exactly
    // 2 * tab_count, not the ~100 raw offset holes.
    let tabs = opts.tab_count as usize;
    assert_eq!(
        paths.elems.len(),
        2 * tabs,
        "expected {} cut segments (cutout + perimeter), slivers filtered",
        2 * tabs
    );

    // The Gerber is in KiCad's plotted frame, so compare by extent rather than
    // absolute coordinates: the first tab_count segments are the ~4 mm circular
    // cutout (small bounding box); the rest are the 36 x 30 perimeter (large).
    let cutout_extent = combined_extent_mm(&paths.elems[..tabs]);
    let perimeter_extent = combined_extent_mm(&paths.elems[tabs..]);
    assert!(
        cutout_extent < 8.0,
        "first {tabs} segments should be the small cutout, extent {cutout_extent:.1} mm"
    );
    assert!(
        perimeter_extent > 25.0,
        "later segments should be the perimeter, extent {perimeter_extent:.1} mm"
    );

    // Schedule for 1.6 + 0.1 mm at defaults (0.05 mm/pass, 0.2 mm steps).
    let sched = cut::schedule(&opts, (1.6 * NM_PER_MM as f64) as i64).unwrap();
    assert_eq!(sched.steps.len(), 9);
    assert_eq!(sched.steps[0].passes, 4);
    assert_eq!(sched.steps.last().unwrap().focus_drop_mm, 0.0);
}
