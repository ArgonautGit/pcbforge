//! ING-4 — per-net ID raster of a copper layer.
//!
//! The clearance loop's short/open classifier needs to know *which net* each
//! blob of copper belongs to. [`net_raster`] turns an X2-attributed copper
//! layer (ING-3) into a pixel grid whose value is the net's 1-based ID (0 =
//! no copper), plus the ID→name table.
//!
//! # Net-source decision (recorded here and in docs/decisions.md)
//!
//! The task offered three sources: (a) a per-net `kicad-cli` export, (b) the
//! Gerber X2 `.N` object attributes, (c) parsing the `.kicad_pcb`
//! s-expression netlist directly. We use **(b)**. It is by far the least code:
//! ING-3 already renders every copper object with correct pad/zone/trace
//! geometry *and* tags it with its `.N` net, so grouping by that attribute is
//! a union per name — no re-deriving footprint/pad geometry (with rotation,
//! roundrect corners, thermal reliefs) from the s-expression, which (c) would
//! require. Rejected: (a) — `kicad-cli` has no per-net raster/geometry export;
//! (c) — correct but re-implements the geometry ING-3 already produces.

use pcb_core::{NM_PER_UM, Nm, P, Poly};

use crate::gerber::AttributedLayer;

/// A net name.
pub type NetName = String;

/// A raster whose pixels hold a net's 1-based ID (0 = background/no copper).
pub struct IdImage {
    pub width: u32,
    pub height: u32,
    /// Board-frame nm coordinate of the raster's `(0, 0)` corner.
    pub origin: P,
    /// Pixel pitch in micrometers.
    pub um_per_px: u32,
    /// Row-major net IDs (`y * width + x`).
    pub ids: Vec<u16>,
}

impl IdImage {
    /// Net ID at pixel `(x, y)`; 0 outside the grid.
    pub fn at(&self, x: u32, y: u32) -> u16 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.ids[(y * self.width + x) as usize]
    }

    /// Net ID at a board-frame nm point (0 if outside the grid or not copper).
    pub fn id_at_nm(&self, p: P) -> u16 {
        let px_nm = self.um_per_px as Nm * NM_PER_UM;
        if p.x < self.origin.x || p.y < self.origin.y {
            return 0;
        }
        let x = ((p.x - self.origin.x) / px_nm) as u32;
        let y = ((p.y - self.origin.y) / px_nm) as u32;
        self.at(x, y)
    }

    /// Number of pixels carrying net ID `id`.
    pub fn count(&self, id: u16) -> usize {
        self.ids.iter().filter(|&&v| v == id).count()
    }
}

/// Rasterize `layer`'s copper into per-net IDs at `um_per_px`.
///
/// Returns the [`IdImage`] and the ID→name table (`names[id - 1]`); IDs follow
/// the sorted net order. Pixels are assigned by pixel-center membership;
/// distinct nets are disjoint copper, so on the rare boundary-pixel tie the
/// first (lower-ID) net wins.
pub fn net_raster(layer: &AttributedLayer, um_per_px: u32) -> (IdImage, Vec<NetName>) {
    let names = layer.net_names();
    let px_nm = um_per_px.max(1) as Nm * NM_PER_UM;

    // Frame = bounding box of all copper (so every net shares one grid).
    let bbox = layer_bbox(layer);
    let Some((minx, miny, maxx, maxy)) = bbox else {
        return (
            IdImage {
                width: 0,
                height: 0,
                origin: P::new(0, 0),
                um_per_px,
                ids: Vec::new(),
            },
            names,
        );
    };
    let width = (((maxx - minx) / px_nm) + 1).max(1) as u32;
    let height = (((maxy - miny) / px_nm) + 1).max(1) as u32;
    let origin = P::new(minx, miny);
    let mut ids = vec![0u16; (width as usize) * (height as usize)];

    for (i, name) in names.iter().enumerate() {
        let id = (i + 1) as u16;
        let polys = layer.net_polys(name);
        let Some((nx0, ny0, nx1, ny1)) = polys_bbox(&polys) else {
            continue;
        };
        // Only scan this net's own bbox.
        let px0 = (((nx0 - minx) / px_nm).max(0)) as u32;
        let py0 = (((ny0 - miny) / px_nm).max(0)) as u32;
        let px1 = ((((nx1 - minx) / px_nm) + 1) as u32).min(width);
        let py1 = ((((ny1 - miny) / px_nm) + 1) as u32).min(height);
        for py in py0..py1 {
            let cy = miny + py as Nm * px_nm + px_nm / 2;
            for px in px0..px1 {
                let idx = (py * width + px) as usize;
                if ids[idx] != 0 {
                    continue; // first net wins a boundary tie
                }
                let cx = minx + px as Nm * px_nm + px_nm / 2;
                if point_in_polys(cx, cy, &polys) {
                    ids[idx] = id;
                }
            }
        }
    }

    (
        IdImage {
            width,
            height,
            origin,
            um_per_px,
            ids,
        },
        names,
    )
}

fn layer_bbox(layer: &AttributedLayer) -> Option<(Nm, Nm, Nm, Nm)> {
    polys_bbox(&layer.layer().polys)
}

fn polys_bbox(polys: &[Poly]) -> Option<(Nm, Nm, Nm, Nm)> {
    let mut b: Option<(Nm, Nm, Nm, Nm)> = None;
    for poly in polys {
        for p in poly.outer.iter().chain(poly.holes.iter().flatten()) {
            b = Some(match b {
                None => (p.x, p.y, p.x, p.y),
                Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
            });
        }
    }
    b
}

/// Even-odd membership of `(px, py)` in a normalized poly set (outer CCW,
/// holes CW): a point inside an outer but inside one of its holes toggles
/// twice and is correctly excluded.
fn point_in_polys(px: Nm, py: Nm, polys: &[Poly]) -> bool {
    let mut inside = false;
    for poly in polys {
        for ring in std::iter::once(&poly.outer).chain(poly.holes.iter()) {
            if ray_crosses_odd(ring, px, py) {
                inside = !inside;
            }
        }
    }
    inside
}

/// Odd number of crossings of the +x ray from `(px, py)` with `ring`.
fn ray_crosses_odd(ring: &[P], px: Nm, py: Nm) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut c = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = (ring[i].x, ring[i].y);
        let (xj, yj) = (ring[j].x, ring[j].y);
        if (yi > py) != (yj > py) {
            let t = (py - yi) as f64 / (yj - yi) as f64;
            let x_int = xi as f64 + t * (xj - xi) as f64;
            if (px as f64) < x_int {
                c = !c;
            }
        }
        j = i;
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gerber::parse_gerber_x2;
    use pcb_core::NM_PER_MM;

    const MM: Nm = NM_PER_MM;

    /// Two square pads on two nets, hand-authored in KiCad's X2 style.
    fn two_net_source() -> String {
        let mut s = String::new();
        s.push_str("%TF.FileFunction,Copper,L1,Top*%\n%FSLAX46Y46*%\n%MOMM*%\n");
        s.push_str("G04 two pads, two nets*\n%LPD*%\n");
        // A 1 mm square pad aperture.
        s.push_str("%TA.AperFunction,SMDPad,CuDef*%\n");
        s.push_str("%ADD10R,1.000000X1.000000*%\n%TD*%\n");
        s.push_str("D10*\n");
        // Pad on VCC at (2,2).
        s.push_str("%TO.P,R1,1*%\n%TO.N,VCC*%\nX2000000Y2000000D03*\n%TD*%\n");
        // Pad on GND at (8,2).
        s.push_str("%TO.P,R1,2*%\n%TO.N,GND*%\nX8000000Y2000000D03*\n%TD*%\n");
        s.push_str("M02*\n");
        s
    }

    #[test]
    fn two_nets_rasterize_to_distinct_ids() {
        let layer = parse_gerber_x2(&two_net_source()).unwrap();
        let (img, names) = net_raster(&layer, 50); // 50 µm/px
        assert_eq!(names, vec!["GND".to_string(), "VCC".to_string()]);
        let gnd = 1u16; // sorted: GND first
        let vcc = 2u16;

        // Each pad center maps to its own net id.
        assert_eq!(img.id_at_nm(P::new(2 * MM, 2 * MM)), vcc);
        assert_eq!(img.id_at_nm(P::new(8 * MM, 2 * MM)), gnd);
        // A point between the pads is background.
        assert_eq!(img.id_at_nm(P::new(5 * MM, 2 * MM)), 0);

        // Both nets have area, and the two footprints are disjoint.
        assert!(img.count(vcc) > 0 && img.count(gnd) > 0);
        for (i, &v) in img.ids.iter().enumerate() {
            if v == vcc {
                // No pixel is both — trivially true since each pixel holds one
                // id; assert the counts partition sensibly instead.
                let _ = i;
            }
        }
        // A 1 mm pad at 50 µm/px is ~20x20 px each; both similar.
        assert!(img.count(vcc).abs_diff(img.count(gnd)) < img.count(vcc) / 2);
    }

    #[test]
    fn empty_layer_yields_empty_raster() {
        let src =
            "%TF.FileFunction,Copper,L1,Top*%\n%FSLAX46Y46*%\n%MOMM*%\nG04 x*\n%LPD*%\nM02*\n";
        let layer = parse_gerber_x2(src).unwrap();
        let (img, names) = net_raster(&layer, 50);
        assert!(names.is_empty());
        assert_eq!(img.ids.len(), 0);
    }

    #[test]
    fn holes_are_excluded_from_membership() {
        // A pad on a net that fully surrounds a cleared window would be a hole;
        // here just confirm point_in_polys respects a hole ring.
        let mut hole = vec![
            P::new(3 * MM, 3 * MM),
            P::new(7 * MM, 3 * MM),
            P::new(7 * MM, 7 * MM),
            P::new(3 * MM, 7 * MM),
        ];
        hole.reverse(); // CW hole
        let poly = Poly {
            outer: vec![
                P::new(0, 0),
                P::new(10 * MM, 0),
                P::new(10 * MM, 10 * MM),
                P::new(0, 10 * MM),
            ],
            holes: vec![hole],
        };
        assert!(point_in_polys(MM, MM, std::slice::from_ref(&poly)));
        assert!(!point_in_polys(5 * MM, 5 * MM, std::slice::from_ref(&poly)));
    }
}
