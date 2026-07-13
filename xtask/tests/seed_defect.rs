//! QA-5 end-to-end test of `cargo xtask seed-defect` as library calls,
//! against the committed sample board `samples/kicad/valdemo2.kicad_pcb`.
//!
//! Board geometry (KiCad board frame, y down): a 0.25 mm GND trace runs along
//! y = 111 and the GND zone fill starts at y = 114.25; the channel between
//! them is empty copper. A vertical sliver at (108, 112.5) of length 4 mm
//! spans the channel and bridges trace to zone. The long trace between
//! x ≈ 102.5 and 118 is the target for `--thin`.
//!
//! Self-skips when `kicad-cli` is not installed (same policy as the ingest
//! golden tests).

use std::path::{Path, PathBuf};

use cam::drc::ViolationKind;
use pcb_core::Layer;
use testkit::{assert_images_agree, rasterize};
use xtask::seed_defect::{
    DefectSpec, GOLDEN_MIN_AGREEMENT, GOLDEN_UM_PER_PX, apply_defect, export_copper,
    raster_agreement, run, violations_near,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is nested under the workspace root")
        .to_path_buf()
}

fn board() -> PathBuf {
    repo_root().join("samples/kicad/valdemo2.kicad_pcb")
}

fn out_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-qa5-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// Sliver bridging the known channel between the GND trace (y = 111) and the
/// zone (y >= 114.25): full pipeline, golden round-trip, and detectability.
#[test]
fn sliver_bridges_known_channel_and_round_trips() {
    if !ingest::kicad_cli::available() {
        eprintln!("SKIP: kicad-cli not installed; seed-defect golden not run");
        return;
    }
    let out = out_dir("sliver");
    let spec = DefectSpec::parse_sliver("108,112.5,30,4,90").unwrap();

    let report = run(&board(), &out, &spec).expect("seed-defect pipeline");
    // The tool's own golden check already enforces the threshold; the report
    // must reflect it and the emitted files must exist.
    assert!(report.agreement >= GOLDEN_MIN_AGREEMENT, "{report}");
    assert!(report.svg.is_file() && report.preview.is_file(), "{report}");
    // Bridging sliver: the copper component count drops (trace+pads blob and
    // the zone fill merge into one).
    assert!(
        report.components_after < report.components_before,
        "sliver must bridge two components: {report}"
    );
    // And the sliver is DRC-detectable where the pristine board was clean.
    assert_eq!(report.violations_near_before, 0, "{report}");
    assert!(report.violations_near_after > 0, "{report}");

    // Independent golden check with testkit: reload the emitted SVG and
    // compare against defect-applied geometry recomputed from the Gerber.
    let (copper, _) = export_copper(&board(), &out.join("gerber2")).expect("export");
    let modified = apply_defect(&copper.polys, &spec);
    let reloaded = ingest::svg::load_kicad_svg(&report.svg).expect("emitted SVG loads back");
    let a = rasterize(
        &Layer {
            polys: modified.clone(),
        },
        GOLDEN_UM_PER_PX,
    );
    let b = rasterize(&reloaded, GOLDEN_UM_PER_PX);
    if a.dimensions() == b.dimensions() {
        assert_images_agree(&a, &b, GOLDEN_MIN_AGREEMENT);
    } else {
        // Bounding boxes may differ by a rounding pixel; the padded
        // comparison is what the tool itself uses.
        let agreement = raster_agreement(&a, &b).expect("comparable sizes");
        assert!(agreement >= GOLDEN_MIN_AGREEMENT, "agreement {agreement}");
    }

    // The defect is a *new* sub-floor feature near (108, -112.5): a 30 µm
    // sliver is below a 60 µm floor.
    let floor = spec.verify_floor_mm();
    let near = spec.near_radius_mm();
    let center = spec.center_geom();
    assert!(violations_near(&copper, floor, center, near).is_empty());
    let after = violations_near(&Layer { polys: modified }, floor, center, near);
    assert!(
        after
            .iter()
            .any(|v| v.kind == ViolationKind::TraceWidthBelowFloor),
        "expected the sliver flagged as below-floor copper: {after:?}"
    );
}

/// Thinning the 0.25 mm GND trace at (113, 111) by 100 µm: the trace drops
/// to 0.15 mm, so a floor between old and new width flags it.
#[test]
fn thin_takes_trace_below_floor_and_round_trips() {
    if !ingest::kicad_cli::available() {
        eprintln!("SKIP: kicad-cli not installed; seed-defect golden not run");
        return;
    }
    let out = out_dir("thin");
    let spec = DefectSpec::parse_thin("113,111,1.0,100").unwrap();

    let report = run(&board(), &out, &spec).expect("seed-defect pipeline");
    assert!(report.agreement >= GOLDEN_MIN_AGREEMENT, "{report}");
    // The rim pull-back severs the trace, so components rise.
    assert!(
        report.components_after > report.components_before,
        "thin must open the trace: {report}"
    );

    // Floor between the old (0.25) and new (0.15) width flags the thinned
    // piece; the pristine board is clean there at the same floor.
    let (copper, _) = export_copper(&board(), &out.join("gerber2")).expect("export");
    let modified = apply_defect(&copper.polys, &spec);
    let floor = 0.2;
    let near = spec.near_radius_mm();
    let center = spec.center_geom();
    assert!(
        violations_near(&copper, floor, center, near).is_empty(),
        "pristine 0.25 mm trace must be clean at a 0.2 mm floor"
    );
    let after = violations_near(&Layer { polys: modified }, floor, center, near);
    assert!(
        after
            .iter()
            .any(|v| v.kind == ViolationKind::TraceWidthBelowFloor),
        "expected the 0.15 mm thinned trace flagged at a 0.2 mm floor: {after:?}"
    );
}
