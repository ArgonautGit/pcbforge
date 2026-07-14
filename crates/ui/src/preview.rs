//! Job preview: rasterize CAM geometry (board / kept-copper / to-ablate) into
//! an [`egui::ColorImage`] for the console's preview panel.
//!
//! egui's own path fill fans a triangle from the first vertex, which is wrong
//! for the concave, holed polygons PCB copper produces. We instead scanline-
//! fill with an **even-odd** rule per polygon (outer ring plus its holes), so
//! islands and cut-outs render correctly, and paint layers back-to-front so the
//! kept copper sits over the board field and the ablated channels show through.
//! This mirrors `cam::export::write_preview_svg`'s color scheme so the console
//! preview and the SVG preview match.

use egui::{Color32, ColorImage};
use pcb_core::{NM_PER_MM, Poly};

/// One draw layer: the polygons and the RGB color to fill them.
pub struct Layer<'a> {
    pub polys: &'a [Poly],
    pub color: [u8; 3],
}

/// Colors shared with `write_preview_svg`.
pub const BOARD: [u8; 3] = [0xe8, 0xdd, 0xc8];
pub const ABLATE: [u8; 3] = [0x33, 0x33, 0x33];
pub const COPPER: [u8; 3] = [0xb8, 0x73, 0x33];

/// Rasterize `layers` (painted in order, later over earlier) over a `bg` field
/// at `px_per_mm`, capping the longer side at `max_px`. Returns a filled
/// [`ColorImage`]; an empty/degenerate input yields a 1×1 background pixel.
pub fn rasterize(layers: &[Layer], bg: [u8; 3], px_per_mm: f64, max_px: usize) -> ColorImage {
    let Some((min_x, min_y, max_x, max_y)) = bbox(layers) else {
        return ColorImage::new([1, 1], Color32::from_rgb(bg[0], bg[1], bg[2]));
    };
    let mm = |nm: i64| nm as f64 / NM_PER_MM as f64;
    let (wmm, hmm) = (mm(max_x - min_x).max(1e-3), mm(max_y - min_y).max(1e-3));

    // Fit within max_px on the long side, keeping the requested px_per_mm
    // otherwise.
    let scale = px_per_mm.min(max_px as f64 / wmm).min(max_px as f64 / hmm);
    let w = (wmm * scale).ceil().max(1.0) as usize;
    let h = (hmm * scale).ceil().max(1.0) as usize;

    let mut canvas = Canvas {
        px: vec![Color32::from_rgb(bg[0], bg[1], bg[2]); w * h],
        w,
        h,
        min_x,
        max_y,
        scale,
    };
    for layer in layers {
        let c = Color32::from_rgb(layer.color[0], layer.color[1], layer.color[2]);
        for poly in layer.polys {
            canvas.fill_poly(poly, c);
        }
    }
    ColorImage {
        size: [w, h],
        pixels: canvas.px,
    }
}

/// A pixel target plus the world→pixel transform (origin `min_x`, y flipped
/// about `max_y`, `scale` px per mm).
struct Canvas {
    px: Vec<Color32>,
    w: usize,
    h: usize,
    min_x: i64,
    max_y: i64,
    scale: f64,
}

impl Canvas {
    fn mmx(&self, nm: i64) -> f64 {
        (nm - self.min_x) as f64 / NM_PER_MM as f64 * self.scale
    }
    fn mmy(&self, nm: i64) -> f64 {
        (self.max_y - nm) as f64 / NM_PER_MM as f64 * self.scale
    }

    /// Even-odd scanline fill of one polygon (outer + holes). Row `j` samples
    /// world y at the pixel center; x-crossings over every ring are collected,
    /// sorted, and the odd spans painted.
    fn fill_poly(&mut self, poly: &Poly, color: Color32) {
        let rings: Vec<&Vec<pcb_core::P>> = std::iter::once(&poly.outer)
            .chain(poly.holes.iter())
            .filter(|r| r.len() >= 3)
            .collect();
        if rings.is_empty() {
            return;
        }

        for j in 0..self.h {
            let yc = j as f64 + 0.5;
            let mut xs: Vec<f64> = Vec::new();
            for ring in &rings {
                let n = ring.len();
                for k in 0..n {
                    let a = ring[k];
                    let b = ring[(k + 1) % n];
                    let (ya, yb) = (self.mmy(a.y), self.mmy(b.y));
                    // Half-open edge test avoids double-counting shared vertices.
                    if (ya <= yc && yb > yc) || (yb <= yc && ya > yc) {
                        let t = (yc - ya) / (yb - ya);
                        xs.push(self.mmx(a.x) + t * (self.mmx(b.x) - self.mmx(a.x)));
                    }
                }
            }
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(f64::total_cmp);
            let mut s = 0;
            while s + 1 < xs.len() {
                let x0 = xs[s].max(0.0).ceil() as usize;
                let x1 = (xs[s + 1].min(self.w as f64)).floor() as usize;
                for x in x0..x1 {
                    self.px[j * self.w + x] = color;
                }
                s += 2;
            }
        }
    }
}

fn bbox(layers: &[Layer]) -> Option<(i64, i64, i64, i64)> {
    let mut b: Option<(i64, i64, i64, i64)> = None;
    for layer in layers {
        for poly in layer.polys {
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

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::P;

    const MM: i64 = NM_PER_MM;

    fn rect(x0: i64, y0: i64, x1: i64, y1: i64) -> Vec<P> {
        vec![
            P::new(x0, y0),
            P::new(x1, y0),
            P::new(x1, y1),
            P::new(x0, y1),
        ]
    }

    #[test]
    fn fills_inside_and_leaves_holes_as_background() {
        // 10×10 mm copper square with a 4×4 mm hole in the middle.
        let mut hole = rect(3 * MM, 3 * MM, 7 * MM, 7 * MM);
        hole.reverse(); // CW hole
        let poly = Poly {
            outer: rect(0, 0, 10 * MM, 10 * MM),
            holes: vec![hole],
        };
        let img = rasterize(
            &[Layer {
                polys: std::slice::from_ref(&poly),
                color: COPPER,
            }],
            BOARD,
            20.0,
            400,
        );
        let [w, h] = img.size;
        let at = |xmm: f64, ymm: f64| {
            // world→pixel: y flipped about the 10 mm top.
            let x = (xmm * 20.0) as usize;
            let y = ((10.0 - ymm) * 20.0) as usize;
            img.pixels[y.min(h - 1) * w + x.min(w - 1)]
        };
        let copper = Color32::from_rgb(COPPER[0], COPPER[1], COPPER[2]);
        let board = Color32::from_rgb(BOARD[0], BOARD[1], BOARD[2]);
        assert_eq!(at(1.0, 1.0), copper, "corner is copper");
        assert_eq!(at(5.0, 5.0), board, "center hole shows the board field");
        assert_eq!(at(2.5, 5.0), copper, "just outside the hole is copper");
    }

    #[test]
    fn two_islands_both_fill() {
        let a = Poly {
            outer: rect(0, 0, 2 * MM, 2 * MM),
            holes: vec![],
        };
        let b = Poly {
            outer: rect(8 * MM, 8 * MM, 10 * MM, 10 * MM),
            holes: vec![],
        };
        let polys = vec![a, b];
        let img = rasterize(
            &[Layer {
                polys: &polys,
                color: ABLATE,
            }],
            BOARD,
            20.0,
            400,
        );
        let [w, h] = img.size;
        let ablate = Color32::from_rgb(ABLATE[0], ABLATE[1], ABLATE[2]);
        let at = |xmm: f64, ymm: f64| {
            let x = ((xmm * 20.0) as usize).min(w - 1);
            let y = (((10.0 - ymm) * 20.0) as usize).min(h - 1);
            img.pixels[y * w + x]
        };
        assert_eq!(at(1.0, 1.0), ablate, "island A fills");
        assert_eq!(at(9.0, 9.0), ablate, "island B fills");
        assert_eq!(
            at(5.0, 5.0),
            Color32::from_rgb(BOARD[0], BOARD[1], BOARD[2])
        );
    }

    #[test]
    fn empty_input_is_one_background_pixel() {
        let img = rasterize(&[], BOARD, 20.0, 400);
        assert_eq!(img.size, [1, 1]);
    }

    #[test]
    fn later_layers_paint_over_earlier() {
        let sq = Poly {
            outer: rect(0, 0, 10 * MM, 10 * MM),
            holes: vec![],
        };
        let board_poly = [sq.clone()];
        let copper_poly = [sq];
        let img = rasterize(
            &[
                Layer {
                    polys: &board_poly,
                    color: ABLATE,
                },
                Layer {
                    polys: &copper_poly,
                    color: COPPER,
                },
            ],
            BOARD,
            10.0,
            400,
        );
        let [w, _] = img.size;
        assert_eq!(
            img.pixels[5 * w + 5],
            Color32::from_rgb(COPPER[0], COPPER[1], COPPER[2]),
            "copper (last) wins over ablate"
        );
    }
}
