//! CAM-6 fixture tests: process compilers against the in-repo sample board
//! `samples/kicad/valdemo2.kicad_pcb`, exported through `ingest::kicad_cli`
//! and parsed with `ingest::gerber` / `ingest::excellon`.
//!
//! `ingest` is a dev-dependency only (dev-only cycle cam -> ingest -> cam,
//! which cargo permits); production `cam` stays ingest-free.
//!
//! # Ground truth
//!
//! The backlog says "I'll paste counts from KiCad"; this deviates
//! deliberately: the board lives in this repo, so every expected value is
//! read straight from the authored `.kicad_pcb` source (what the GUI would
//! display, minus transcription risk):
//!
//! * F.Mask — 5 pad openings at `pad_to_mask_clearance 0.05`:
//!   3 THT (rect 1.8x1.8, circle 1.8, oval 1.8x2.2) + 2 SMD (roundrect and
//!   rect, both 1.5x0.9). All disjoint (pad pitch 2.54 mm ≫ pad widths).
//! * F.Silkscreen — one fp_line stroke (0.12 mm wide) plus the "J1"
//!   reference text rendered as strokes by KiCad. The polygon count after
//!   Gerber parse is not derivable from the source by inspection (it
//!   depends on KiCad's text stroker), so it was observed once and
//!   hardcoded below with the tie to the board content.
//! * F.Paste — exactly the 2 SMD pads.
//! * Drills — 4 round holes (2x 1.0 mm pad drills, 2x 0.4 mm vias) plus
//!   1 oval slot (1.0 x 1.4 mm drill on the oval pad).

use std::path::{Path, PathBuf};

use cam::ablation::point_in_polys;
use cam::process::{DrillEntry, drill_map, legend, mask_open, stencil};
use ingest::excellon::{DrillOp, load_excellon_full};
use ingest::gerber::load_gerber;
use ingest::kicad_cli::{self, KicadCli};
use pcb_core::{CamOpts, Layer, P, PathKind, Paths};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn board() -> PathBuf {
    let b = repo_root().join("samples/kicad/valdemo2.kicad_pcb");
    assert!(b.is_file(), "sample board missing: {}", b.display());
    b
}

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-cam6-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Export one layer's Gerber and parse it. kicad-cli names files with the
/// layer's canonical name, dots replaced (`F.Mask` -> `...-F_Mask.gts`).
fn export_layer(tag: &str, layer: &str, fragment: &str) -> Layer {
    let cli = KicadCli::discover().unwrap();
    let files = cli
        .export_gerbers(&board(), &[layer], &tmp_dir(tag))
        .unwrap();
    let file = files
        .iter()
        .find(|p| p.file_name().unwrap().to_string_lossy().contains(fragment))
        .unwrap_or_else(|| panic!("no exported file matching '{fragment}' in {files:?}"));
    load_gerber(file).unwrap()
}

fn count_kind(paths: &Paths, kind: PathKind) -> usize {
    paths.elems.iter().filter(|e| e.kind == kind).count()
}

/// Hatch segments of `kind` whose midpoint lies in `polys`.
fn hatches_inside(paths: &Paths, kind: PathKind, polys: &[pcb_core::Poly]) -> usize {
    paths
        .elems
        .iter()
        .filter(|e| e.kind == kind)
        .filter(|e| {
            let mid = P::new((e.pts[0].x + e.pts[1].x) / 2, (e.pts[0].y + e.pts[1].y) / 2);
            point_in_polys(mid, polys, 10)
        })
        .count()
}

#[test]
fn mask_open_fixture_five_openings_all_filled() {
    if !kicad_cli::available() {
        eprintln!("SKIP: kicad-cli not installed");
        return;
    }
    let mask = export_layer("mask", "F.Mask", "F_Mask");
    // 3 THT + 2 SMD pad openings, all disjoint and hole-free.
    assert_eq!(mask.polys.len(), 5, "board source: 5 pads on *.Mask/F.Mask");
    assert!(mask.polys.iter().all(|p| p.holes.is_empty()));

    let paths = mask_open(&mask, &CamOpts::default());
    assert_eq!(
        count_kind(&paths, PathKind::Boundary),
        5,
        "one exact-edge contour per opening"
    );
    // Every opening gets at least one hatch segment.
    for poly in &mask.polys {
        assert!(
            hatches_inside(&paths, PathKind::Rubout(0), std::slice::from_ref(poly)) >= 1,
            "unfilled mask opening: {poly:?}"
        );
    }
    // Nothing but boundaries and mask fill in this job.
    assert_eq!(
        paths.elems.len(),
        5 + count_kind(&paths, PathKind::Rubout(0))
    );
}

#[test]
fn stencil_fixture_two_paste_apertures() {
    if !kicad_cli::available() {
        eprintln!("SKIP: kicad-cli not installed");
        return;
    }
    let paste = export_layer("paste", "F.Paste", "F_Paste");
    // Exactly the two SMD pads sit on F.Paste.
    assert_eq!(paste.polys.len(), 2, "board source: 2 SMD pads on F.Paste");
    assert!(paste.polys.iter().all(|p| p.holes.is_empty()));

    let paths = stencil(&paste);
    assert_eq!(paths.elems.len(), 2, "one cut contour per aperture");
    assert!(
        paths
            .elems
            .iter()
            .all(|e| e.kind == PathKind::Boundary && e.closed)
    );
    // Cut contours are the parsed aperture edges verbatim.
    assert_eq!(paths.elems[0].pts, paste.polys[0].outer);
    assert_eq!(paths.elems[1].pts, paste.polys[1].outer);
}

#[test]
fn legend_fixture_silkscreen_strokes() {
    if !kicad_cli::available() {
        eprintln!("SKIP: kicad-cli not installed");
        return;
    }
    let silk = export_layer("silk", "F.Silkscreen", "F_Silkscreen");
    // Board source: one fp_line stroke + reference text "J1" stroked by
    // KiCad. The exact polygon count depends on KiCad's text stroker, so it
    // was observed once from the kicad-cli 7.0.11 export and hardcoded
    // (satisfying the spec's >= 3 lower bound): 3 disjoint hole-free
    // polygons whose bounding boxes identify them as the fp_line
    // (x 96.94..108.06 mm), the glyph "J" (x 99.21..99.74) and the glyph
    // "1" (x 100.02..100.74; its overlapping strokes union into one region).
    assert_eq!(silk.polys.len(), 3, "fp_line + 'J' + '1'");
    let rings: usize = silk.polys.iter().map(|p| 1 + p.holes.len()).sum();
    assert_eq!(rings, 3, "all silk strokes are hole-free");

    let paths = legend(&silk, &CamOpts::default());
    assert_eq!(
        count_kind(&paths, PathKind::Boundary),
        rings,
        "one contour per parsed ring"
    );
    // Legend fill is tagged Rubout(1) (module contract), never Rubout(0).
    assert_eq!(count_kind(&paths, PathKind::Rubout(0)), 0);
    for poly in &silk.polys {
        assert!(
            hatches_inside(&paths, PathKind::Rubout(1), std::slice::from_ref(poly)) >= 1,
            "unfilled silk stroke: {poly:?}"
        );
    }
}

#[test]
fn drill_map_fixture_four_holes_one_slot() {
    if !kicad_cli::available() {
        eprintln!("SKIP: kicad-cli not installed");
        return;
    }
    let cli = KicadCli::discover().unwrap();
    let files = cli.export_drill(&board(), &tmp_dir("drill")).unwrap();
    let mut entries: Vec<DrillEntry> = Vec::new();
    for f in &files {
        for op in load_excellon_full(f).unwrap() {
            entries.push(match op {
                DrillOp::Hole {
                    center,
                    diameter_nm,
                } => DrillEntry {
                    x_nm: center.x,
                    y_nm: center.y,
                    diameter_nm,
                    slot_end: None,
                },
                DrillOp::Slot { a, b, diameter_nm } => DrillEntry {
                    x_nm: a.x,
                    y_nm: a.y,
                    diameter_nm,
                    slot_end: Some((b.x, b.y)),
                },
            });
        }
    }
    // Board source: 2 pad drills + 2 via drills + 1 oval-pad slot.
    assert_eq!(entries.len(), 5, "4 holes + 1 slot: {entries:?}");
    assert_eq!(entries.iter().filter(|e| e.slot_end.is_some()).count(), 1);

    let json = drill_map(&entries);
    assert_eq!(json.matches("\"x_nm\":").count(), 5, "5 JSON entries");
    assert_eq!(json.matches("\"slot_end\":null").count(), 4);
    // Spot-check one coordinate exactly: the via at (110, -100) mm,
    // 0.4 mm drill (board source), in the exact nm JSON form.
    assert!(
        json.contains(
            "{\"x_nm\":110000000,\"y_nm\":-100000000,\"diameter_nm\":400000,\"slot_end\":null}"
        ),
        "via entry missing from:\n{json}"
    );
    // The slot spans (105.08, -99.8)..(105.08, -100.2) mm (either
    // direction) at 1.0 mm diameter.
    let slot = entries.iter().find(|e| e.slot_end.is_some()).unwrap();
    let ends = ((slot.x_nm, slot.y_nm), slot.slot_end.unwrap());
    let a = (105_080_000, -99_800_000);
    let b = (105_080_000, -100_200_000);
    assert!(ends == (a, b) || ends == (b, a), "slot endpoints {ends:?}");
    assert_eq!(slot.diameter_nm, 1_000_000);
}
