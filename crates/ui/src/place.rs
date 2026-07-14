//! Drag-to-place: position the job on the camera view and etch it there.
//!
//! The operator drags the circuit overlay over the bed frame and rotates it;
//! the resulting **placement** (translate + rotate) is an affine mapping the
//! job's design/Gerber frame → the bed/machine frame. That affine is expressed
//! as three synthetic fiducial correspondences and handed to `pcbforge
//! register` (Phase A) — so the etch-at-placement path reuses the verified
//! registration emit rather than a second code path.
//!
//! For display, [`composite`] alpha-blends the transformed job over the frame.

use egui::{Color32, ColorImage};
use image::GrayImage;
use pcb_core::{NM_PER_MM, Poly};

/// A manual placement of the job on the bed: its Gerber-frame `pivot` lands at
/// `(tx_mm, ty_mm)` in bed mm, rotated `rot_deg` about that pivot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub tx_mm: f64,
    pub ty_mm: f64,
    pub rot_deg: f64,
    /// Job pivot in Gerber mm (typically the job bbox center).
    pub pivot_mm: (f64, f64),
}

impl Placement {
    /// The affine `[a,b,c,d,e,f]` (mm→mm) taking Gerber coords to bed coords:
    /// `bed = R(rot)·(g − pivot) + (tx,ty)`.
    pub fn affine(&self) -> [f64; 6] {
        let (s, c) = self.rot_deg.to_radians().sin_cos();
        let (px, py) = self.pivot_mm;
        // bed = R·g + (t − R·pivot)
        let cx = self.tx_mm - (c * px - s * py);
        let cy = self.ty_mm - (s * px + c * py);
        [c, -s, cx, s, c, cy]
    }

    /// Three `dx,dy=tx,ty` correspondences (Gerber → bed) encoding this
    /// placement, for `pcbforge register --fiducials`. Uses the pivot and two
    /// unit offsets — non-collinear, so the fit is exact.
    pub fn correspondences(&self) -> String {
        let a = self.affine();
        let apply = |x: f64, y: f64| (a[0] * x + a[1] * y + a[2], a[3] * x + a[4] * y + a[5]);
        let (px, py) = self.pivot_mm;
        let pts = [(px, py), (px + 10.0, py), (px, py + 10.0)];
        pts.iter()
            .map(|&(x, y)| {
                let (tx, ty) = apply(x, y);
                format!("{x:.6},{y:.6}={tx:.6},{ty:.6}")
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The bbox center of `shapes` in Gerber mm — the default placement pivot.
pub fn bbox_center_mm(shapes: &[Poly]) -> (f64, f64) {
    let mut b: Option<(i64, i64, i64, i64)> = None;
    for poly in shapes {
        for p in poly.outer.iter().chain(poly.holes.iter().flatten()) {
            b = Some(match b {
                None => (p.x, p.y, p.x, p.y),
                Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
            });
        }
    }
    let (x0, y0, x1, y1) = b.unwrap_or((0, 0, 0, 0));
    let mm = |v: i64| v as f64 / NM_PER_MM as f64;
    ((mm(x0) + mm(x1)) / 2.0, (mm(y0) + mm(y1)) / 2.0)
}

/// Alpha-blend the placed job over the bed `frame`. The job `shapes` (Gerber
/// mm) are mapped through the placement to bed mm, then to pixels via
/// `px_per_mm`, and even-odd filled in a translucent `color`.
pub fn composite(
    frame: &GrayImage,
    shapes: &[Poly],
    placement: &Placement,
    px_per_mm: f64,
    color: [u8; 3],
    alpha: f64,
) -> ColorImage {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let mut px: Vec<Color32> = frame.pixels().map(|p| Color32::from_gray(p[0])).collect();

    let a = placement.affine();
    // Gerber-nm point → bed-px.
    let to_px = |gx_nm: i64, gy_nm: i64| -> (f64, f64) {
        let x = gx_nm as f64 / NM_PER_MM as f64;
        let y = gy_nm as f64 / NM_PER_MM as f64;
        let bx = a[0] * x + a[1] * y + a[2];
        let by = a[3] * x + a[4] * y + a[5];
        (bx * px_per_mm, by * px_per_mm)
    };
    let (cr, cg, cb) = (color[0] as f64, color[1] as f64, color[2] as f64);
    let blend = |dst: Color32| {
        Color32::from_rgb(
            (dst.r() as f64 * (1.0 - alpha) + cr * alpha) as u8,
            (dst.g() as f64 * (1.0 - alpha) + cg * alpha) as u8,
            (dst.b() as f64 * (1.0 - alpha) + cb * alpha) as u8,
        )
    };

    for poly in shapes {
        let rings: Vec<Vec<(f64, f64)>> = std::iter::once(&poly.outer)
            .chain(poly.holes.iter())
            .filter(|r| r.len() >= 3)
            .map(|r| r.iter().map(|p| to_px(p.x, p.y)).collect())
            .collect();
        if rings.is_empty() {
            continue;
        }
        for j in 0..h {
            let yc = j as f64 + 0.5;
            let mut xs: Vec<f64> = Vec::new();
            for ring in &rings {
                let n = ring.len();
                for k in 0..n {
                    let (_, ya) = ring[k];
                    let (_, yb) = ring[(k + 1) % n];
                    if (ya <= yc && yb > yc) || (yb <= yc && ya > yc) {
                        let (xa, _) = ring[k];
                        let (xb, _) = ring[(k + 1) % n];
                        let t = (yc - ya) / (yb - ya);
                        xs.push(xa + t * (xb - xa));
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
                let x1 = (xs[s + 1].min(w as f64)).floor() as usize;
                for x in x0..x1.min(w) {
                    px[j * w + x] = blend(px[j * w + x]);
                }
                s += 2;
            }
        }
    }
    ColorImage {
        size: [w, h],
        pixels: px,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::P;

    const MM: i64 = NM_PER_MM;

    fn sq(cx: i64, cy: i64, r: i64) -> Poly {
        Poly {
            outer: vec![
                P::new(cx - r, cy - r),
                P::new(cx + r, cy - r),
                P::new(cx + r, cy + r),
                P::new(cx - r, cy + r),
            ],
            holes: vec![],
        }
    }

    #[test]
    fn zero_placement_at_pivot_is_identity() {
        // Pivot at (5,5); place it back at (5,5) with no rotation → identity.
        let p = Placement {
            tx_mm: 5.0,
            ty_mm: 5.0,
            rot_deg: 0.0,
            pivot_mm: (5.0, 5.0),
        };
        let a = p.affine();
        assert!((a[0] - 1.0).abs() < 1e-9 && (a[4] - 1.0).abs() < 1e-9);
        assert!(a[1].abs() < 1e-9 && a[3].abs() < 1e-9);
        assert!(a[2].abs() < 1e-9 && a[5].abs() < 1e-9, "no translation");
    }

    #[test]
    fn translation_moves_pivot_to_target() {
        let p = Placement {
            tx_mm: 40.0,
            ty_mm: 25.0,
            rot_deg: 0.0,
            pivot_mm: (5.0, 5.0),
        };
        let a = p.affine();
        // pivot (5,5) → (40,25)
        let (bx, by) = (
            a[0] * 5.0 + a[1] * 5.0 + a[2],
            a[3] * 5.0 + a[4] * 5.0 + a[5],
        );
        assert!((bx - 40.0).abs() < 1e-9 && (by - 25.0).abs() < 1e-9);
    }

    #[test]
    fn correspondences_recover_the_placement_affine() {
        // The 3 correspondences must fit back to exactly the placement affine
        // (this is what register does downstream).
        use nalgebra::Point2;
        let p = Placement {
            tx_mm: 30.0,
            ty_mm: -10.0,
            rot_deg: 20.0,
            pivot_mm: (7.0, 3.0),
        };
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = p
            .correspondences()
            .split(';')
            .map(|e| {
                let (d, t) = e.trim().split_once('=').unwrap();
                let pt = |s: &str| {
                    let (x, y) = s.trim().split_once(',').unwrap();
                    Point2::new(x.parse().unwrap(), y.parse().unwrap())
                };
                (pt(d), pt(t))
            })
            .collect();
        let fit = vision::fit_affine(&pairs).unwrap();
        assert!(fit.rms < 1e-4, "rms {}", fit.rms);
        let a = p.affine();
        let t = &fit.transform;
        // 6-decimal correspondence formatting → ~1e-5 recovery.
        for (k, (r, cc)) in [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)]
            .into_iter()
            .enumerate()
        {
            assert!(
                (t[(r, cc)] - a[k]).abs() < 1e-4,
                "coeff {k}: {} vs {}",
                t[(r, cc)],
                a[k]
            );
        }
    }

    #[test]
    fn composite_marks_the_placed_footprint() {
        // 100×100 px frame at 10 px/mm; place a 2 mm square's pivot at (5,5) mm
        // → px (50,50). The blended color must appear there and not at a corner.
        let frame = GrayImage::from_pixel(100, 100, image::Luma([120]));
        let job = [sq(0, 0, MM)]; // 2 mm square centered at gerber origin
        let p = Placement {
            tx_mm: 5.0,
            ty_mm: 5.0,
            rot_deg: 0.0,
            pivot_mm: (0.0, 0.0),
        };
        let img = composite(&frame, &job, &p, 10.0, [200, 60, 60], 0.5);
        let at = |x: usize, y: usize| img.pixels[y * 100 + x];
        // Center (50,50) is inside the placed square → reddish (r raised).
        assert!(at(50, 50).r() > 150, "footprint tinted at center");
        // A far corner is untouched frame gray.
        assert_eq!(at(5, 5), Color32::from_gray(120));
    }
}
