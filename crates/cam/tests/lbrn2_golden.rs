//! EMIT-2 golden checks against the operator's committed `.lbrn2` samples.
//!
//! Grounds the emitter in real evidence rather than transcribed strings:
//! * the Path encoding this crate emits for the sample's exact hand-drawn
//!   5-sided closed polyline must appear verbatim in `path-shape.lbrn2`;
//! * emitting the base process recipe must reproduce every CutSetting value in
//!   `base.lbrn2` (with the schema's units — kHz→Hz, etc.).

use std::path::{Path, PathBuf};

use cam::lbrn2::{self, EmitLayer};
use pcb_core::{AblationParams, NM_PER_MM, P, PathElem, PathKind};

const MM: i64 = NM_PER_MM;

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("samples/lbrn2")
        .join(name)
}

#[test]
fn emitted_path_matches_committed_sample() {
    let src = std::fs::read_to_string(sample("path-shape.lbrn2")).unwrap();
    // The sample's hand-drawn closed 5-gon.
    let verts = [(14, 45), (15, 53), (22, 53), (22, 47), (17, 49)];
    let elem = PathElem {
        kind: PathKind::Isolation(0),
        pts: verts.iter().map(|&(x, y)| P::new(x * MM, y * MM)).collect(),
        closed: true,
    };
    let layer = EmitLayer::line("C01", base_params(), vec![elem]);
    let doc = lbrn2::lbrn2_string("BSLFiber", &[layer]).unwrap();

    // The exact VertList + PrimList the sample contains must appear in ours.
    let vertlist = "<VertList>V14 45c0x1c1x1V15 53c0x1c1x1V22 53c0x1c1x1V22 47c0x1c1x1V17 49c0x1c1x1</VertList>";
    assert!(
        src.contains(vertlist),
        "sample must contain the reference VertList"
    );
    assert!(doc.contains(vertlist), "emitter must reproduce it verbatim");
    assert!(src.contains("<PrimList>LineClosed</PrimList>"));
    assert!(doc.contains("<PrimList>LineClosed</PrimList>"));
}

#[test]
fn emitted_cutsetting_reproduces_base_values() {
    let base = std::fs::read_to_string(sample("base.lbrn2")).unwrap();
    let layer = EmitLayer::fill("C00", base_params(), Vec::new());
    let doc = lbrn2::lbrn2_string("BSLFiber", &[layer]).unwrap();

    // Every process value present in base.lbrn2's CutSetting is reproduced.
    for field in [
        "type=\"Scan\"",
        "<speed Value=\"1000\"/>",
        "<frequency Value=\"30000\"/>",
        "<QPulseWidth Value=\"1\"/>",
        "<interval Value=\"0.03\"/>",
        "<maxPower Value=\"20\"/>",
        "<crossHatch Value=\"1\"/>",
        "<wobbleEnable Value=\"1\"/>",
    ] {
        assert!(base.contains(field), "sanity: base.lbrn2 has {field}");
        assert!(doc.contains(field), "emitter must reproduce {field}");
    }
}

fn base_params() -> AblationParams {
    AblationParams {
        power_pct: 20.0,
        speed_mm_s: 1000.0,
        frequency_khz: 30.0,
        pulse_ns: 1,
        passes: 1,
    }
}
