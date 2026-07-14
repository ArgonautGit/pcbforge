//! EMIT-2 — LightBurn `.lbrn2` emitter.
//!
//! Writes the CutSetting/layer/shape subset derived in `docs/lbrn2-schema.md`
//! from the operator's real device (`BSLFiber`, LightBurn Pro 2.1.03). Turns a
//! set of ablation layers — each a process recipe plus its toolpaths — into a
//! project file that opens in LightBurn and runs, replacing the SVG/DXF import
//! step.
//!
//! # Evidence and units (see docs/lbrn2-schema.md)
//!
//! * layer mode is the `CutSetting type` attribute: `Scan` = Fill, `Cut` = Line;
//! * `frequency` is **Hz** (`AblationParams.frequency_khz * 1000`);
//! * `QPulseWidth` is **integer ns** (`AblationParams.pulse_ns`) — a MOPA
//!   fluence knob, emitted only when non-zero;
//! * power (`maxPower`/`maxPower2`) is written from `power_pct` for rigs that
//!   vary it, though this MOPA runs it fixed;
//! * a `Type="Path"` shape holds an arbitrary polyline: vertices
//!   `V<x> <y>c0x1c1x1` in **absolute mm** (identity `XForm`), and a
//!   `PrimList` of `LineClosed` (closed) or `Line` (open).
//!
//! Numbers are formatted with `Display`, which reproduces LightBurn's own
//! style (`14`, `0.03`, `30000`) so emitted files stay byte-close to
//! hand-authored ones.
//!
//! Open-path `PrimList` (`Line`) is the one field inferred rather than
//! observed (the sample drew a closed shape); closed paths — all the
//! non-copper fill workflow needs — are byte-verified against the sample.

use std::io::Write;
use std::path::Path;

use pcb_core::{AblationParams, NM_PER_MM, PathElem, PathKind, Poly};

/// The operator's device; the root `DeviceName` must match a configured
/// LightBurn device or it prompts on open.
pub const DEFAULT_DEVICE: &str = "BSLFiber";

/// Layer cut mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerMode {
    /// Filled scan (`type="Scan"`) — non-copper regions, rub-out.
    Fill,
    /// Line/vector (`type="Cut"`) — isolation contours, board cut.
    Line,
}

/// One LightBurn layer: a process recipe plus the geometry that runs at it.
#[derive(Debug, Clone)]
pub struct EmitLayer {
    /// Layer name, e.g. `C00`.
    pub name: String,
    pub mode: LayerMode,
    pub params: AblationParams,
    /// Fill line interval, mm (emitted for `Fill`).
    pub interval_mm: f64,
    /// Fill scan angle, deg (emitted when non-zero).
    pub angle_deg: f64,
    /// Cross-hatch fill.
    pub cross_hatch: bool,
    /// Wobble (the operator's base config runs with it on).
    pub wobble: bool,
    /// Sub-layer name (cosmetic; matches the operator's samples).
    pub subname: Option<String>,
    /// Geometry to run at this layer.
    pub elems: Vec<PathElem>,
}

impl EmitLayer {
    /// A Fill layer from ablation params and closed region geometry.
    pub fn fill(name: impl Into<String>, params: AblationParams, elems: Vec<PathElem>) -> Self {
        EmitLayer {
            name: name.into(),
            mode: LayerMode::Fill,
            params,
            interval_mm: 0.03,
            angle_deg: 0.0,
            cross_hatch: true,
            wobble: true,
            subname: Some("sublayername".into()),
            elems,
        }
    }

    /// A Line layer (isolation / board cut) from params and polyline geometry.
    pub fn line(name: impl Into<String>, params: AblationParams, elems: Vec<PathElem>) -> Self {
        EmitLayer {
            name: name.into(),
            mode: LayerMode::Line,
            params,
            interval_mm: 0.03,
            angle_deg: 0.0,
            cross_hatch: false,
            wobble: false,
            subname: None,
            elems,
        }
    }
}

/// Normalize geometry into LightBurn's workspace: translate so the
/// bounding-box min corner sits at **(0, 0)**.
///
/// KiCad's Gerber export negates its internal y-down sheet coordinate, so the
/// plotted frame is **already y-up and unmirrored** — but offset entirely into
/// negative y (and to the sheet's x position). Emitting those coordinates
/// verbatim put the job below LightBurn's origin, off the workspace (caught on
/// the first real board the operator emitted). Translation is the whole fix:
/// no reflection — a flip here would *introduce* a mirror. Orientation is
/// pinned by the asymmetric-triangle test below.
pub fn normalize_frame(polys: &[Poly]) -> Vec<Poly> {
    let mut pts = polys
        .iter()
        .flat_map(|p| p.outer.iter().chain(p.holes.iter().flatten()));
    let Some(first) = pts.next() else {
        return Vec::new();
    };
    let (mut min_x, mut min_y) = (first.x, first.y);
    for p in pts {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
    }
    let map = |p: &pcb_core::P| pcb_core::P::new(p.x - min_x, p.y - min_y);
    polys
        .iter()
        .map(|p| Poly {
            outer: p.outer.iter().map(&map).collect(),
            holes: p
                .holes
                .iter()
                .map(|h| h.iter().map(&map).collect())
                .collect(),
        })
        .collect()
}

/// Each ring of `polys` (outer and holes) as a closed [`PathElem`] — the
/// input a Fill layer wants for non-copper regions. LightBurn's fill resolves
/// nested rings (islands/holes) itself, matching the SVG even-odd approach.
pub fn polys_to_elems(polys: &[Poly]) -> Vec<PathElem> {
    let mut elems = Vec::new();
    for poly in polys {
        for ring in std::iter::once(&poly.outer).chain(poly.holes.iter()) {
            if ring.len() >= 3 {
                elems.push(PathElem {
                    kind: PathKind::Rubout(0),
                    pts: ring.clone(),
                    closed: true,
                });
            }
        }
    }
    elems
}

/// Write `layers` as a `.lbrn2` project to `path`.
pub fn write_lbrn2(device: &str, layers: &[EmitLayer], path: &Path) -> std::io::Result<()> {
    std::fs::File::create(path)?.write_all(lbrn2_string(device, layers).as_bytes())
}

/// Render the full `.lbrn2` document as a string.
pub fn lbrn2_string(device: &str, layers: &[EmitLayer]) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str(&format!(
        "<LightBurnProject AppVersion=\"2.1.03\" DeviceName=\"{device}\" FormatVersion=\"1\" \
         MaterialHeight=\"0\" MirrorX=\"False\" MirrorY=\"False\" AskForSendName=\"True\">\n"
    ));
    s.push_str(EDITOR_BLOCKS);
    for (i, layer) in layers.iter().enumerate() {
        s.push_str(&cut_setting_xml(i as u32, layer));
    }
    // VertID/PrimID must be unique per shape: they identify the shape's
    // vertex/primitive lists. The operator's first burn proved that reusing
    // an ID cross-links the lists — every ring's closing segment ran back to
    // shape 0's first vertex, fanning burned rays from the board corner.
    let mut shape_id = 0u32;
    for (i, layer) in layers.iter().enumerate() {
        for elem in &layer.elems {
            if let Some(shape) = shape_xml(i as u32, shape_id, elem) {
                s.push_str(&shape);
                shape_id += 1;
            }
        }
    }
    s.push_str("    <Notes ShowOnLoad=\"0\" Notes=\"\"/>\n");
    s.push_str("</LightBurnProject>\n");
    s
}

/// The constant VariableText + UIPrefs blocks LightBurn writes; reproduced so
/// emitted files match the operator's samples and open without surprises.
const EDITOR_BLOCKS: &str = concat!(
    "    <VariableText>\n",
    "        <Start Value=\"0\"/>\n",
    "        <End Value=\"999\"/>\n",
    "        <Current Value=\"0\"/>\n",
    "        <Increment Value=\"1\"/>\n",
    "        <AutoAdvance Value=\"0\"/>\n",
    "    </VariableText>\n",
    "    <UIPrefs>\n",
    "        <Optimize_ByLayer Value=\"0\"/>\n",
    "        <Optimize_ByGroup Value=\"-1\"/>\n",
    "        <Optimize_ByPriority Value=\"1\"/>\n",
    "        <Optimize_WhichDirection Value=\"0\"/>\n",
    "        <Optimize_InnerToOuter Value=\"1\"/>\n",
    "        <Optimize_ByDirection Value=\"0\"/>\n",
    "        <Optimize_ReduceTravel Value=\"1\"/>\n",
    "        <Optimize_HideBacklash Value=\"0\"/>\n",
    "        <Optimize_ReduceDirChanges Value=\"0\"/>\n",
    "        <Optimize_ChooseCorners Value=\"0\"/>\n",
    "        <Optimize_AllowReverse Value=\"1\"/>\n",
    "        <Optimize_RemoveOverlaps Value=\"0\"/>\n",
    "        <Optimize_OptimalEntryPoint Value=\"0\"/>\n",
    "        <Optimize_OverlapDist Value=\"0.025\"/>\n",
    "    </UIPrefs>\n",
);

fn cut_setting_xml(index: u32, layer: &EmitLayer) -> String {
    let ty = match layer.mode {
        LayerMode::Fill => "Scan",
        LayerMode::Line => "Cut",
    };
    let p = &layer.params;
    let mut s = format!("    <CutSetting type=\"{ty}\">\n");
    let mut field =
        |name: &str, val: String| s.push_str(&format!("        <{name} Value=\"{val}\"/>\n"));
    field("index", index.to_string());
    field("name", layer.name.clone());
    field("maxPower", num(p.power_pct));
    field("maxPower2", num(p.power_pct));
    field("speed", num(p.speed_mm_s));
    field("frequency", num(p.frequency_khz * 1000.0)); // schema: Hz
    if p.pulse_ns > 0 {
        field("QPulseWidth", p.pulse_ns.to_string());
    }
    if layer.wobble {
        field("wobbleEnable", "1".into());
    }
    if layer.mode == LayerMode::Fill {
        if layer.cross_hatch {
            field("crossHatch", "1".into());
        }
        field("interval", num(layer.interval_mm));
        if layer.angle_deg != 0.0 {
            field("angle", num(layer.angle_deg));
        }
    }
    if p.passes > 1 {
        field("numPasses", p.passes.to_string());
    }
    if let Some(sub) = &layer.subname {
        field("subname", sub.clone());
    }
    field("priority", index.to_string());
    field("tabCount", "1".into());
    field("tabCountMax", "1".into());
    s.push_str("    </CutSetting>\n");
    s
}

/// One `Type="Path"` shape from a polyline element (skips degenerate ones).
fn shape_xml(cut_index: u32, shape_id: u32, elem: &PathElem) -> Option<String> {
    if elem.pts.len() < 2 {
        return None;
    }
    let mut verts = String::new();
    for p in &elem.pts {
        verts.push_str(&format!("V{} {}c0x1c1x1", num_mm(p.x), num_mm(p.y)));
    }
    // Closed polyline -> LineClosed; open -> Line (open form inferred).
    let prim = if elem.closed { "LineClosed" } else { "Line" };
    Some(format!(
        "    <Shape Type=\"Path\" CutIndex=\"{cut_index}\" VertID=\"{shape_id}\" PrimID=\"{shape_id}\">\n\
         \x20       <XForm>1 0 0 1 0 0</XForm>\n\
         \x20       <VertList>{verts}</VertList>\n\
         \x20       <PrimList>{prim}</PrimList>\n\
         \x20   </Shape>\n"
    ))
}

/// LightBurn-style number: shortest round-trip `Display` (`14`, `0.03`).
fn num(v: f64) -> String {
    format!("{v}")
}

/// nm → mm, LightBurn-style.
fn num_mm(nm: i64) -> String {
    num(nm as f64 / NM_PER_MM as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::{Machine, NM_PER_MM, P};

    const MM: i64 = NM_PER_MM;

    fn base_params() -> AblationParams {
        AblationParams {
            power_pct: 20.0,
            speed_mm_s: 1000.0,
            frequency_khz: 30.0,
            pulse_ns: 1,
            passes: 1,
        }
    }

    /// The VertList/PrimList this emitter produces for the operator's exact
    /// hand-drawn 5-sided closed path must match `samples/lbrn2/…shapes` byte
    /// for byte — the evidence that the Path encoding is correct.
    #[test]
    fn path_encoding_matches_lightburns_own_output() {
        let verts = [(14, 45), (15, 53), (22, 53), (22, 47), (17, 49)];
        let elem = PathElem {
            kind: PathKind::Isolation(0),
            pts: verts.iter().map(|&(x, y)| P::new(x * MM, y * MM)).collect(),
            closed: true,
        };
        let shape = shape_xml(1, 0, &elem).unwrap();
        assert!(shape.contains(
            "<VertList>V14 45c0x1c1x1V15 53c0x1c1x1V22 53c0x1c1x1V22 47c0x1c1x1V17 49c0x1c1x1</VertList>"
        ));
        assert!(shape.contains("<PrimList>LineClosed</PrimList>"));
        assert!(shape.contains("Type=\"Path\" CutIndex=\"1\""));
    }

    #[test]
    fn fill_cutsetting_carries_base_values_with_correct_units() {
        let layer = EmitLayer::fill("C00", base_params(), Vec::new());
        let xml = cut_setting_xml(0, &layer);
        assert!(xml.contains("type=\"Scan\""), "Fill => Scan");
        assert!(xml.contains("<speed Value=\"1000\"/>"));
        assert!(xml.contains("<frequency Value=\"30000\"/>"), "kHz -> Hz");
        assert!(xml.contains("<QPulseWidth Value=\"1\"/>"));
        assert!(xml.contains("<interval Value=\"0.03\"/>"));
        assert!(xml.contains("<maxPower Value=\"20\"/>"));
        // Defaults omitted, matching hand-authored files.
        assert!(
            !xml.contains("numPasses"),
            "1 pass is the default (omitted)"
        );
        assert!(!xml.contains("<angle"), "0 angle omitted");
    }

    #[test]
    fn line_mode_is_cut_type() {
        let layer = EmitLayer::line("C00", base_params(), Vec::new());
        let xml = cut_setting_xml(0, &layer);
        assert!(xml.contains("type=\"Cut\""), "Line => Cut");
        assert!(!xml.contains("interval"), "Line omits fill interval");
    }

    #[test]
    fn passes_and_angle_emitted_when_non_default() {
        let mut layer = EmitLayer::fill(
            "C00",
            AblationParams {
                passes: 5,
                ..base_params()
            },
            Vec::new(),
        );
        layer.angle_deg = 45.0;
        let xml = cut_setting_xml(0, &layer);
        assert!(xml.contains("<numPasses Value=\"5\"/>"));
        assert!(xml.contains("<angle Value=\"45\"/>"));
    }

    #[test]
    fn open_path_uses_line_primitive() {
        let elem = PathElem {
            kind: PathKind::Cut,
            pts: vec![P::new(0, 0), P::new(5 * MM, 0), P::new(5 * MM, 5 * MM)],
            closed: false,
        };
        let shape = shape_xml(0, 0, &elem).unwrap();
        assert!(shape.contains("<PrimList>Line</PrimList>"));
    }

    #[test]
    fn full_document_is_well_formed_and_round_trips_layers() {
        let square = PathElem {
            kind: PathKind::Rubout(0),
            pts: vec![
                P::new(0, 0),
                P::new(10 * MM, 0),
                P::new(10 * MM, 10 * MM),
                P::new(0, 10 * MM),
            ],
            closed: true,
        };
        let layers = vec![EmitLayer::fill("C00", base_params(), vec![square])];
        let doc = lbrn2_string(DEFAULT_DEVICE, &layers);
        assert!(doc.starts_with("<?xml"));
        assert!(doc.contains("DeviceName=\"BSLFiber\""));
        assert!(doc.trim_end().ends_with("</LightBurnProject>"));
        // One CutSetting, one Path shape.
        assert_eq!(doc.matches("<CutSetting").count(), 1);
        assert_eq!(doc.matches("Type=\"Path\"").count(), 1);
        // Tag balance (crude well-formedness): every opened CutSetting closes.
        assert_eq!(
            doc.matches("<CutSetting").count(),
            doc.matches("</CutSetting>").count()
        );
    }

    /// The bug the operator's first real board caught: KiCad-plotted
    /// (all-negative-Y) geometry must come out at the origin **without a
    /// reflection** — the plotted frame is already y-up (KiCad negates its
    /// internal y-down coordinate on export). An asymmetric triangle pins the
    /// orientation: the board's top vertex must stay the top.
    #[test]
    fn normalize_frame_unmirrors_kicad_plotted_geometry() {
        // Board-frame right triangle (0,0) (20,0) (0,10), as KiCad plots it:
        // y_plot = y_board - 90 for a board at sheet y 80..90, x offset 100.
        let plotted = Poly {
            outer: vec![
                P::new(100 * MM, -90 * MM),
                P::new(120 * MM, -90 * MM),
                P::new(100 * MM, -80 * MM),
            ],
            holes: vec![],
        };
        let out = normalize_frame(std::slice::from_ref(&plotted));
        assert_eq!(out.len(), 1);
        // Winding restored to CCW (positive shoelace) after the reflection.
        let r = &out[0].outer;
        let shoelace: i128 = (0..r.len())
            .map(|i| {
                let a = r[i];
                let b = r[(i + 1) % r.len()];
                a.x as i128 * b.y as i128 - b.x as i128 * a.y as i128
            })
            .sum();
        assert!(shoelace > 0, "outer ring must stay CCW");
        // Exact expected vertices: bbox min at origin, y-up, unmirrored.
        let mut got = r.clone();
        got.sort();
        let mut want = vec![
            P::new(0, 0),
            P::new(20 * MM, 0),
            P::new(0, 10 * MM), // the board's top-left stays the top
        ];
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn normalize_frame_only_translates_y_up_input() {
        // Already y-up geometry away from the origin: translated, not flipped.
        let poly = Poly {
            outer: vec![
                P::new(5 * MM, 3 * MM),
                P::new(9 * MM, 3 * MM),
                P::new(5 * MM, 7 * MM),
            ],
            holes: vec![],
        };
        let out = normalize_frame(std::slice::from_ref(&poly));
        let mut got = out[0].outer.clone();
        got.sort();
        let mut want = vec![P::new(0, 0), P::new(4 * MM, 0), P::new(0, 4 * MM)];
        want.sort();
        assert_eq!(got, want);
    }

    /// Regression for the operator's fan-burn: every emitted shape must have
    /// its own VertID/PrimID. Reused IDs cross-link LightBurn's vertex lists
    /// and every ring's closing segment runs to shape 0's first vertex.
    #[test]
    fn every_shape_gets_unique_vert_and_prim_ids() {
        let tri = |x: i64| PathElem {
            kind: PathKind::Rubout(0),
            pts: vec![
                P::new(x * MM, 0),
                P::new((x + 2) * MM, 0),
                P::new(x * MM, 2 * MM),
            ],
            closed: true,
        };
        let layers = vec![
            EmitLayer::fill("C00", base_params(), vec![tri(0), tri(5), tri(10)]),
            EmitLayer::line("C01", base_params(), vec![tri(15)]),
        ];
        let doc = lbrn2_string(DEFAULT_DEVICE, &layers);
        let mut ids: Vec<&str> = doc
            .split("VertID=\"")
            .skip(1)
            .map(|s| s.split('"').next().unwrap())
            .collect();
        assert_eq!(ids.len(), 4, "one VertID per shape");
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4, "VertIDs must be unique per shape");
        // PrimID mirrors VertID.
        for id in &ids {
            assert!(doc.contains(&format!("VertID=\"{id}\" PrimID=\"{id}\"")));
        }
    }

    #[test]
    fn polys_to_elems_makes_one_closed_elem_per_ring() {
        let mut hole = vec![
            P::new(2 * MM, 2 * MM),
            P::new(4 * MM, 2 * MM),
            P::new(4 * MM, 4 * MM),
            P::new(2 * MM, 4 * MM),
        ];
        hole.reverse();
        let poly = Poly {
            outer: vec![
                P::new(0, 0),
                P::new(10 * MM, 0),
                P::new(10 * MM, 10 * MM),
                P::new(0, 10 * MM),
            ],
            holes: vec![hole],
        };
        let elems = polys_to_elems(std::slice::from_ref(&poly));
        assert_eq!(elems.len(), 2, "outer + hole");
        assert!(elems.iter().all(|e| e.closed));
        let _ = Machine::Fiber;
    }
}
