//! Vector exporters for the EZCAD import workflow.
//!
//! * [`write_dxf`] — minimal DXF R12 with one closed `POLYLINE` per ring
//!   (outers and holes alike, all on one layer). EZCAD2's DXF import reads
//!   these directly; select everything and hatch-fill with the even-odd
//!   rule and the nesting reproduces holes/islands automatically.
//! * [`write_svg`] — one `<path>` per polygon (outer + hole subpaths),
//!   `fill-rule="evenodd"` — the shapes read exactly as they will ablate.
//! * [`write_preview_svg`] — copper + to-ablate regions overlaid in color
//!   for a quick eyeball before importing anything into EZCAD.
//!
//! All coordinates are emitted in millimeters. DXF keeps the board's y-up
//! frame; SVG output is y-flipped into screen space so previews look right.

use std::io::Write;
use std::path::Path;

use pcb_core::{NM_PER_MM, Poly, Ring};

fn mm(v: i64) -> f64 {
    v as f64 / NM_PER_MM as f64
}

/// Bounding box over polys as (min_x, min_y, max_x, max_y) in nm.
fn bbox(polys: &[&[Poly]]) -> Option<(i64, i64, i64, i64)> {
    let mut b: Option<(i64, i64, i64, i64)> = None;
    for set in polys {
        for poly in *set {
            for p in poly.outer.iter().chain(poly.holes.iter().flatten()) {
                b = Some(match b {
                    None => (p.x, p.y, p.x, p.y),
                    Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
                });
            }
        }
    }
    b
}

/// Write `polys` as DXF R12: each ring a closed `POLYLINE` on layer
/// `NONCOPPER`, coordinates in mm, y-up (native board frame).
pub fn write_dxf(polys: &[Poly], path: &Path) -> std::io::Result<()> {
    let mut out = String::new();
    out.push_str("0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\nAC1009\n0\nENDSEC\n");
    out.push_str("0\nSECTION\n2\nENTITIES\n");
    let mut ring_dxf = |ring: &Ring| {
        out.push_str("0\nPOLYLINE\n8\nNONCOPPER\n66\n1\n70\n1\n");
        for p in ring {
            out.push_str(&format!(
                "0\nVERTEX\n8\nNONCOPPER\n10\n{:.6}\n20\n{:.6}\n",
                mm(p.x),
                mm(p.y)
            ));
        }
        out.push_str("0\nSEQEND\n");
    };
    for poly in polys {
        ring_dxf(&poly.outer);
        for hole in &poly.holes {
            ring_dxf(hole);
        }
    }
    out.push_str("0\nENDSEC\n0\nEOF\n");
    std::fs::File::create(path)?.write_all(out.as_bytes())
}

fn svg_path_d(poly: &Poly, y_flip_about_mm: f64) -> String {
    let mut d = String::new();
    let mut sub = |ring: &Ring| {
        for (i, p) in ring.iter().enumerate() {
            let cmd = if i == 0 { 'M' } else { 'L' };
            d.push_str(&format!(
                "{cmd}{:.6} {:.6}",
                mm(p.x),
                y_flip_about_mm - mm(p.y)
            ));
        }
        d.push('Z');
    };
    sub(&poly.outer);
    for hole in &poly.holes {
        sub(hole);
    }
    d
}

fn svg_document(body: &str, x0: f64, y0: f64, w: f64, h: f64) -> String {
    format!(
        concat!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" ",
            "width=\"{w}mm\" height=\"{h}mm\" viewBox=\"{x0} {y0} {w} {h}\">\n{body}</svg>\n"
        ),
        x0 = x0,
        y0 = y0,
        w = w,
        h = h,
        body = body
    )
}

/// Write `polys` as an SVG of black even-odd-filled paths (mm units, one
/// path per polygon). This is the importable/checkable form of exactly what
/// will be ablated.
pub fn write_svg(polys: &[Poly], path: &Path) -> std::io::Result<()> {
    let (x0, y0, x1, y1) = bbox(&[polys]).unwrap_or((0, 0, 0, 0));
    let flip = mm(y0) + mm(y1); // maps y to (top+bottom) − y
    let mut body = String::new();
    for poly in polys {
        body.push_str(&format!(
            "  <path d=\"{}\" fill=\"#000\" fill-rule=\"evenodd\"/>\n",
            svg_path_d(poly, flip)
        ));
    }
    let doc = svg_document(
        &body,
        mm(x0),
        mm(y0),
        mm(x1 - x0).max(0.001),
        mm(y1 - y0).max(0.001),
    );
    std::fs::File::create(path)?.write_all(doc.as_bytes())
}

/// Preview: board (light), to-ablate (dark), copper (copper-colored) —
/// for eyeballing the inversion before importing into EZCAD.
pub fn write_preview_svg(
    board: &[Poly],
    copper: &[Poly],
    ablate: &[Poly],
    path: &Path,
) -> std::io::Result<()> {
    let (x0, y0, x1, y1) = bbox(&[board, copper, ablate]).unwrap_or((0, 0, 0, 0));
    let flip = mm(y0) + mm(y1);
    let mut body = String::new();
    let mut layer = |polys: &[Poly], fill: &str, opacity: &str| {
        for poly in polys {
            body.push_str(&format!(
                "  <path d=\"{}\" fill=\"{fill}\" fill-opacity=\"{opacity}\" fill-rule=\"evenodd\"/>\n",
                svg_path_d(poly, flip)
            ));
        }
    };
    layer(board, "#e8ddc8", "1");
    layer(ablate, "#333333", "1");
    layer(copper, "#b87333", "1");
    let doc = svg_document(
        &body,
        mm(x0),
        mm(y0),
        mm(x1 - x0).max(0.001),
        mm(y1 - y0).max(0.001),
    );
    std::fs::File::create(path)?.write_all(doc.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::{NM_PER_MM, P};

    const MM: i64 = NM_PER_MM;

    fn fixture() -> Vec<Poly> {
        let mut hole = vec![
            P::new(2 * MM, 2 * MM),
            P::new(4 * MM, 2 * MM),
            P::new(4 * MM, 4 * MM),
            P::new(2 * MM, 4 * MM),
        ];
        hole.reverse();
        vec![
            Poly {
                outer: vec![
                    P::new(0, 0),
                    P::new(10 * MM, 0),
                    P::new(10 * MM, 6 * MM),
                    P::new(0, 6 * MM),
                ],
                holes: vec![hole],
            },
            Poly {
                outer: vec![
                    P::new(12 * MM, 0),
                    P::new(15 * MM, 0),
                    P::new(15 * MM, 3 * MM),
                ],
                holes: vec![],
            },
        ]
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pcbforge-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn dxf_has_one_closed_polyline_per_ring() {
        let path = tmp("out.dxf");
        write_dxf(&fixture(), &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // 3 rings: outer+hole+outer.
        assert_eq!(text.matches("POLYLINE").count(), 3);
        assert_eq!(text.matches("SEQEND").count(), 3);
        // 4+4+3 vertices.
        assert_eq!(text.matches("VERTEX").count(), 11);
        // Closed flag and mm coordinates present.
        assert!(text.contains("70\n1\n"));
        assert!(text.contains("10\n10.000000"));
        assert!(text.ends_with("0\nEOF\n"));
    }

    #[test]
    fn svg_has_one_evenodd_path_per_poly_and_flips_y() {
        let path = tmp("out.svg");
        write_svg(&fixture(), &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("<path").count(), 2);
        assert_eq!(text.matches("evenodd").count(), 2);
        assert!(text.contains("viewBox=\"0 0 15 6\""));
        // Board-frame origin (0,0) is the bottom-left → svg y = 6.
        assert!(text.contains("M0.000000 6.000000"));
    }

    #[test]
    fn preview_layers_stack_board_ablate_copper() {
        let path = tmp("prev.svg");
        let board = fixture();
        write_preview_svg(&board, &board, &board, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let b = text.find("#e8ddc8").unwrap();
        let a = text.find("#333333").unwrap();
        let c = text.find("#b87333").unwrap();
        assert!(b < a && a < c, "paint order must be board, ablate, copper");
    }
}
