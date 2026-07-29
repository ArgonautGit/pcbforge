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
    /// Uniform scale about the pivot, from the fiducial fit (`fit_board_pose`).
    /// This RESIZES the emitted job — 1.038 burns the design 3.8% larger — so
    /// it is not a view setting. Nominal is 1.0.
    pub scale: f64,
    /// Job pivot in Gerber mm (typically the job bbox center).
    pub pivot_mm: (f64, f64),
}

impl Placement {
    /// The affine `[a,b,c,d,e,f]` (mm→mm) taking Gerber coords to bed coords:
    /// `bed = s·R(rot)·(g − pivot) + (tx,ty)`.
    pub fn affine(&self) -> [f64; 6] {
        let (s, c) = self.rot_deg.to_radians().sin_cos();
        let (px, py) = self.pivot_mm;
        let k = self.scale;
        // Build the constant from the SCALED coefficients rather than scaling a
        // separately-rounded R·pivot: that way `affine(pivot)` cancels to
        // exactly (tx,ty) in floating point at any scale, which the placement
        // readout and the register round-trip both depend on.
        let (a0, a1) = (k * c, -(k * s));
        let (a3, a4) = (k * s, k * c);
        let cx = self.tx_mm - (a0 * px + a1 * py);
        let cy = self.ty_mm - (a3 * px + a4 * py);
        [a0, a1, cx, a3, a4, cy]
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

/// The bbox extent (width, height) of `shapes` in Gerber mm, or `None` for an
/// empty input — the auto fiducial-layout board size.
pub(crate) fn bbox_size_mm(shapes: &[Poly]) -> Option<(f64, f64)> {
    let mut b: Option<(i64, i64, i64, i64)> = None;
    for poly in shapes {
        for p in poly.outer.iter().chain(poly.holes.iter().flatten()) {
            b = Some(match b {
                None => (p.x, p.y, p.x, p.y),
                Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
            });
        }
    }
    let (x0, y0, x1, y1) = b?;
    let mm = |v: i64| v as f64 / NM_PER_MM as f64;
    Some((mm(x1) - mm(x0), mm(y1) - mm(y0)))
}

/// Alpha-blend the placed job over the bed `frame`. The job `shapes` (Gerber
/// mm) are mapped through the placement to bed mm, then to pixels, and even-odd
/// filled in a translucent `color`.
///
/// bed-mm → pixels uses the perspective `homography` (bed/design-mm → pixel)
/// when the camera is tilted, so the overlay keystones onto the board in the
/// image; otherwise it falls back to a uniform `px_per_mm` scale.
pub fn composite(
    frame: &GrayImage,
    shapes: &[Poly],
    placement: &Placement,
    px_per_mm: f64,
    homography: Option<&vision::Homography>,
    color: [u8; 3],
    alpha: f64,
) -> ColorImage {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let px: Vec<Color32> = frame.pixels().map(|p| Color32::from_gray(p[0])).collect();
    let mut img = ColorImage {
        size: [w, h],
        pixels: px,
    };
    composite_over(
        &mut img, shapes, placement, px_per_mm, homography, color, alpha,
    );
    img
}

/// Blend the placed job into an existing `img` (in place), so several design
/// layers can be stacked over one frame for the AR overlay (UI-2). Same
/// mapping as [`composite`]: Gerber-mm → bed-mm (placement) → pixels (the
/// perspective `homography` when present, else a uniform `px_per_mm` scale).
pub fn composite_over(
    img: &mut ColorImage,
    shapes: &[Poly],
    placement: &Placement,
    px_per_mm: f64,
    homography: Option<&vision::Homography>,
    color: [u8; 3],
    alpha: f64,
) {
    let [_, h] = img.size;
    let project = |bx: f64, by: f64| match homography {
        Some(hgt) => {
            let p = hgt.apply(nalgebra::Point2::new(bx, by));
            (p.x.is_finite() && p.y.is_finite()).then_some((p.x, p.y))
        }
        None => Some((bx * px_per_mm, h as f64 - by * px_per_mm)),
    };
    // Existing callers only supply fit homographies or a finite uniform scale;
    // the fallible API below is used where an operator calibration can be bad.
    let _ = composite_over_projected(img, shapes, placement, &project, color, alpha);
}

/// Alpha-blend a placement using an arbitrary nonlinear bed-mm → camera-px
/// projection. Any non-finite/unprojectable vertex rejects the entire result so
/// a bad calibration cannot leave a plausible-looking partial overlay.
pub fn composite_projected(
    frame: &GrayImage,
    shapes: &[Poly],
    placement: &Placement,
    project: &dyn Fn(f64, f64) -> Option<(f64, f64)>,
    color: [u8; 3],
    alpha: f64,
) -> Result<ColorImage, String> {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let mut img = ColorImage {
        size: [w, h],
        pixels: frame.pixels().map(|p| Color32::from_gray(p[0])).collect(),
    };
    composite_over_projected(&mut img, shapes, placement, project, color, alpha)?;
    Ok(img)
}

/// In-place variant of [`composite_projected`] for callers that keep a cached
/// RGBA base frame (the AR overlay re-blends on every live frame — rebuilding
/// the base from the gray frame each time costs a full-frame conversion).
pub(crate) fn composite_over_projected(
    img: &mut ColorImage,
    shapes: &[Poly],
    placement: &Placement,
    project: &dyn Fn(f64, f64) -> Option<(f64, f64)>,
    color: [u8; 3],
    alpha: f64,
) -> Result<(), String> {
    let [w, h] = img.size;
    let px = &mut img.pixels;
    let a = placement.affine();
    let to_px = |gx_nm: i64, gy_nm: i64| -> Option<(f64, f64)> {
        let x = gx_nm as f64 / NM_PER_MM as f64;
        let y = gy_nm as f64 / NM_PER_MM as f64;
        let bx = a[0] * x + a[1] * y + a[2];
        let by = a[3] * x + a[4] * y + a[5];
        let out = project(bx, by)?;
        (out.0.is_finite() && out.1.is_finite()).then_some(out)
    };
    let rgb = (color[0] as f64, color[1] as f64, color[2] as f64);
    // A soft fill (so a solid region reads as area, not an opaque blob) with a
    // crisp, thicker outline on every ring (outer + holes) so the shape edges —
    // the traces — are clearly legible over the board.
    let fill_a = (alpha * 0.4).clamp(0.0, 1.0);
    let edge_a = (alpha * 1.8).clamp(0.0, 1.0);

    let (wf, hf) = (w as f64, h as f64);
    // Cohen–Sutherland outcode against the frame, padded by the 2 px stroke
    // block. A polygon (or edge) that is entirely off one side contributes no
    // visible pixel, so it can be skipped wholesale — this is what keeps the
    // per-drag cost proportional to *visible* geometry rather than
    // (poly count × frame height).
    let outcode = |x: f64, y: f64| -> u8 {
        let mut c = 0u8;
        if x < -2.0 {
            c |= 1; // left
        }
        if x > wf + 1.0 {
            c |= 2; // right
        }
        if y < -2.0 {
            c |= 4; // top
        }
        if y > hf + 1.0 {
            c |= 8; // bottom
        }
        c
    };

    let mut xs: Vec<f64> = Vec::new();
    for poly in shapes {
        let rings: Option<Vec<Vec<(f64, f64)>>> = std::iter::once(&poly.outer)
            .chain(poly.holes.iter())
            .filter(|r| r.len() >= 3)
            .map(|r| r.iter().map(|p| to_px(p.x, p.y)).collect())
            .collect();
        // Projection (and its fail-closed non-finite check) has already run;
        // culling below only changes which off-frame pixels we skip, never the
        // error contract nor any pixel that lands on the frame.
        let rings = rings.ok_or("camera projection returned a non-finite polygon vertex")?;
        if rings.is_empty() {
            continue;
        }
        // Pixel bbox over every ring vertex. If it (padded for the stroke) does
        // not intersect the frame, the whole poly is invisible — skip it.
        let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
        let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for ring in &rings {
            for &(x, y) in ring {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
        if max_x + 2.0 < 0.0 || min_x - 2.0 >= wf || max_y + 2.0 < 0.0 || min_y - 2.0 >= hf {
            continue;
        }
        // Even-odd fill (light). Only rows the bbox spans can yield crossings, so
        // clamp the scanline loop to the poly's row range within [0, h).
        let y0 = min_y.floor().clamp(0.0, hf) as usize;
        let y1 = max_y.ceil().clamp(0.0, hf) as usize;
        for j in y0..y1 {
            let yc = j as f64 + 0.5;
            xs.clear();
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
                let x1 = (xs[s + 1].min(wf)).floor() as usize;
                for x in x0..x1.min(w) {
                    blend_px(px, w, h, x as i32, j as i32, rgb, fill_a);
                }
                s += 2;
            }
        }
        // Outline (crisp) — draw each ring edge as a thickened line. Trivially
        // reject an edge whose endpoints are both ≥2 px off the *same* side of
        // the frame: its whole 2 px span is off-screen (every pixel would be
        // bounds-rejected in the blend anyway), and same-side outcodes can never
        // straddle the visible area, so no visible edge is dropped.
        for ring in &rings {
            let n = ring.len();
            for k in 0..n {
                let p0 = ring[k];
                let p1 = ring[(k + 1) % n];
                if outcode(p0.0, p0.1) & outcode(p1.0, p1.1) != 0 {
                    continue;
                }
                stroke_edge(px, w, h, p0, p1, rgb, edge_a);
            }
        }
    }
    Ok(())
}

/// Alpha-blend `rgb` at pixel `(x, y)` (bounds-checked, no-op if off-image).
fn blend_px(px: &mut [Color32], w: usize, h: usize, x: i32, y: i32, rgb: (f64, f64, f64), a: f64) {
    if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
        return;
    }
    let i = y as usize * w + x as usize;
    let d = px[i];
    px[i] = Color32::from_rgb(
        (d.r() as f64 * (1.0 - a) + rgb.0 * a) as u8,
        (d.g() as f64 * (1.0 - a) + rgb.1 * a) as u8,
        (d.b() as f64 * (1.0 - a) + rgb.2 * a) as u8,
    );
}

/// Draw a 2 px-thick line from `p0` to `p1` (Bresenham) in `rgb` at alpha `a`.
fn stroke_edge(
    px: &mut [Color32],
    w: usize,
    h: usize,
    p0: (f64, f64),
    p1: (f64, f64),
    rgb: (f64, f64, f64),
    a: f64,
) {
    let (mut x0, mut y0) = (p0.0.round() as i32, p0.1.round() as i32);
    let (x1, y1) = (p1.0.round() as i32, p1.1.round() as i32);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        // 2×2 block for a legible line at any zoom.
        for oy in 0..2 {
            for ox in 0..2 {
                blend_px(px, w, h, x0 + ox, y0 + oy, rgb, a);
            }
        }
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
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
    fn bbox_size_measures_the_extent_and_rejects_empty() {
        // A 3 mm half-side square about (5,7) spans 6 mm × 6 mm.
        let (w, h) = bbox_size_mm(&[sq(5 * MM, 7 * MM, 3 * MM)]).unwrap();
        assert!((w - 6.0).abs() < 1e-9 && (h - 6.0).abs() < 1e-9);
        // A wider-than-tall box: outer 10 mm wide, 4 mm tall about origin.
        let rect = Poly {
            outer: vec![
                P::new(-5 * MM, -2 * MM),
                P::new(5 * MM, -2 * MM),
                P::new(5 * MM, 2 * MM),
                P::new(-5 * MM, 2 * MM),
            ],
            holes: vec![],
        };
        let (w, h) = bbox_size_mm(&[rect]).unwrap();
        assert!((w - 10.0).abs() < 1e-9 && (h - 4.0).abs() < 1e-9);
        assert!(bbox_size_mm(&[]).is_none());
    }

    #[test]
    fn zero_placement_at_pivot_is_identity() {
        // Pivot at (5,5); place it back at (5,5) with no rotation → identity.
        let p = Placement {
            tx_mm: 5.0,
            ty_mm: 5.0,
            rot_deg: 0.0,
            scale: 1.0,
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
            scale: 1.0,
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

    /// A non-unit scale still lands the pivot exactly on (tx,ty) — the whole
    /// point of scaling ABOUT the pivot — and stretches everything else by
    /// exactly that factor.
    #[test]
    fn scale_resizes_about_the_pivot() {
        for rot_deg in [0.0, 37.0, -110.0] {
            let p = Placement {
                tx_mm: 40.0,
                ty_mm: 25.0,
                rot_deg,
                scale: 1.038,
                pivot_mm: (5.0, 5.0),
            };
            let a = p.affine();
            let apply = |x: f64, y: f64| (a[0] * x + a[1] * y + a[2], a[3] * x + a[4] * y + a[5]);
            let (bx, by) = apply(5.0, 5.0);
            assert_eq!(
                (bx, by),
                (40.0, 25.0),
                "the pivot maps EXACTLY to (tx,ty) at any scale/rotation"
            );
            // 10 mm from the pivot in the design lands 10·s mm from it on the bed.
            let (qx, qy) = apply(15.0, 5.0);
            assert!(
                ((qx - bx).hypot(qy - by) - 10.0 * p.scale).abs() < 1e-9,
                "rot {rot_deg}: span {} vs {}",
                (qx - bx).hypot(qy - by),
                10.0 * p.scale
            );
        }
    }

    #[test]
    fn correspondences_recover_the_placement_affine() {
        // The 3 correspondences must fit back to exactly the placement affine
        // (this is what register does downstream). The non-unit scale is
        // load-bearing: it proves the fiducial-fitted resize survives the trip
        // through `register --fiducials` into the burned file, rather than
        // being silently dropped at the 3-pair encoding.
        use nalgebra::Point2;
        let p = Placement {
            tx_mm: 30.0,
            ty_mm: -10.0,
            rot_deg: 20.0,
            scale: 1.05,
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
    fn composite_applies_perspective_homography() {
        use nalgebra::Matrix3;
        let frame = GrayImage::from_pixel(200, 200, image::Luma([120]));
        let job = [sq(0, 0, MM)]; // 2 mm square centered at gerber origin
        let p = Placement {
            tx_mm: 10.0,
            ty_mm: 10.0,
            rot_deg: 0.0,
            scale: 1.0,
            pivot_mm: (0.0, 0.0),
        };
        // Homography = a pure 8 px/mm scale (bed-mm (10,10) → px (80,80)),
        // deliberately different from the uniform 10 px/mm fallback (→100,100).
        let hgt = vision::Homography {
            matrix: Matrix3::new(8.0, 0.0, 0.0, 0.0, 8.0, 0.0, 0.0, 0.0, 1.0),
            residuals: vec![],
            rms: 0.0,
        };
        let img = composite(&frame, &job, &p, 10.0, Some(&hgt), [200, 60, 60], 0.9);
        let red = |x: usize, y: usize| img.pixels[y * 200 + x].r() > 150;
        // The square's outline (left edge x=72) sits at the homography-mapped
        // location, not the uniform-scale one (which would be near x=90).
        assert!(
            red(72, 80),
            "outline sits at the homography-mapped location"
        );
        assert!(!red(100, 100), "not at the uniform-scale location");
    }

    #[test]
    fn composite_projected_uses_nonlinear_map_and_fails_closed() {
        let frame = GrayImage::from_pixel(200, 200, image::Luma([120]));
        let job = [sq(0, 0, MM)];
        let p = Placement {
            tx_mm: 10.0,
            ty_mm: 10.0,
            rot_deg: 0.0,
            scale: 1.0,
            pivot_mm: (0.0, 0.0),
        };
        let curved = |x: f64, y: f64| Some((8.0 * x + 0.02 * x * x, 8.0 * y));
        let img = composite_projected(&frame, &job, &p, &curved, [200, 60, 60], 0.9)
            .expect("finite nonlinear projection");
        let red = |x: usize, y: usize| img.pixels[y * 200 + x].r() > 150;
        // Left edge x=9 mm → 73.62 px, distinct from both a linear 8 px/mm
        // map (72) and the old uniform 10 px/mm fallback (90).
        assert!(red(74, 80), "overlay uses the nonlinear projection");
        assert!(!red(90, 100), "does not silently use the uniform fallback");

        let invalid = |_x: f64, _y: f64| None;
        assert!(
            composite_projected(&frame, &job, &p, &invalid, [200, 60, 60], 0.9).is_err(),
            "an unavailable projection returns no plausible partial image"
        );
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
            scale: 1.0,
            pivot_mm: (0.0, 0.0),
        };
        let img = composite(&frame, &job, &p, 10.0, None, [200, 60, 60], 0.5);
        let at = |x: usize, y: usize| img.pixels[y * 100 + x];
        // The outline edge (left side x=40) reads strongly; the interior is
        // softly filled (so a solid region isn't an opaque blob); a far corner
        // is untouched frame gray.
        assert!(at(40, 50).r() > 170, "crisp outline on the footprint edge");
        assert!(
            at(50, 50).r() > 120 && at(50, 50).r() < 170,
            "interior softly filled, not a blob: {}",
            at(50, 50).r()
        );
        assert_eq!(at(5, 5), Color32::from_gray(120), "far corner untouched");
    }

    /// The regression behind "the .lbrn2 doesn't match where I placed it":
    /// bed mm is y-up (machine frame) while image rows grow downward, so the
    /// overlay must draw a placement at ty mm at pixel row `H − ty·ppm` — the
    /// same physical spot `register` emits at machine y = ty. An asymmetric
    /// placement (ty well below mid-frame) catches any y-frame conflation
    /// that symmetric mid-frame probes cannot.
    #[test]
    fn overlay_row_matches_machine_y_up() {
        let frame = GrayImage::from_pixel(100, 200, image::Luma([120]));
        let job = [sq(0, 0, MM)]; // 2 mm square at the gerber origin
        let p = Placement {
            tx_mm: 5.0,
            ty_mm: 3.0, // 3 mm up from the machine origin — near the BOTTOM
            rot_deg: 0.0,
            scale: 1.0,
            pivot_mm: (0.0, 0.0),
        };
        let img = composite(&frame, &job, &p, 10.0, None, [200, 60, 60], 0.9);
        let red = |x: usize, y: usize| img.pixels[y * 100 + x].r() > 150;
        // 3 mm up @10 px/mm in a 200-row frame → rows 160..180 (near the
        // bottom); probe the crisp left-edge outline at (40, 170).
        assert!(red(40, 170), "overlay sits 3 mm above the frame bottom");
        // NOT around row 30 — where the old y-down conflation drew it (near
        // the top), mirrored from where the machine would burn.
        assert!(
            !(20..45).any(|y| (35..65).any(|x| red(x, y))),
            "no footprint at the mirrored y-down position"
        );
    }

    #[test]
    fn outline_reads_stronger_than_the_fill() {
        // The whole point of the clearer overlay: the edge is more saturated
        // than the interior fill of the same region.
        let frame = GrayImage::from_pixel(100, 100, image::Luma([120]));
        let job = [sq(0, 0, 2 * MM)]; // 4 mm square → 40 px, edges well clear of center
        let p = Placement {
            tx_mm: 5.0,
            ty_mm: 5.0,
            rot_deg: 0.0,
            scale: 1.0,
            pivot_mm: (0.0, 0.0),
        };
        let img = composite(&frame, &job, &p, 10.0, None, [220, 40, 40], 0.6);
        let r = |x: usize, y: usize| img.pixels[y * 100 + x].r();
        let edge = r(30, 50); // left edge at x = (5-2)*10 = 30
        let interior = r(50, 50); // center
        assert!(
            edge > interior + 30,
            "edge {edge} should be clearly stronger than interior {interior}"
        );
    }

    #[test]
    fn off_frame_finite_poly_is_skipped_without_error_or_pixel_change() {
        // A poly whose projection lands far off-frame must leave every pixel
        // untouched (the bbox cull is pixel-equivalent to the old bounds-checked
        // blends) and must NOT error as long as its vertices are finite.
        let frame = GrayImage::from_pixel(64, 48, image::Luma([120]));
        let job = [sq(0, 0, MM)];
        let p = Placement {
            tx_mm: 0.0,
            ty_mm: 0.0,
            rot_deg: 0.0,
            scale: 1.0,
            pivot_mm: (0.0, 0.0),
        };
        // Finite projection pushing the whole poly far negative (off-frame).
        let far = |x: f64, y: f64| Some((x - 1.0e6, y - 1.0e6));
        let img = composite_projected(&frame, &job, &p, &far, [200, 60, 60], 0.9)
            .expect("finite vertices must not error even when fully off-frame");
        assert!(
            img.pixels.iter().all(|c| *c == Color32::from_gray(120)),
            "an off-frame poly must not touch any frame pixel"
        );
        // The fail-closed contract is unchanged: the non-finite check runs at
        // projection time, before the bbox cull, so a non-finite vertex still
        // errors even for an off-frame poly.
        let nonfinite = |_x: f64, _y: f64| Some((f64::NAN, 0.0));
        assert!(
            composite_projected(&frame, &job, &p, &nonfinite, [200, 60, 60], 0.9).is_err(),
            "non-finite vertices must still fail closed regardless of the bbox"
        );
    }

    #[test]
    fn partially_clipped_poly_still_draws_its_visible_part() {
        // The boundary-crossing case the bbox clamp/outcode reject must get
        // right: a square that overhangs the frame's bottom-left corner. Its
        // left edge (col -10) and bottom edge (row 110) are off-frame; its top
        // and right edges are partly visible. Forces min_y clamped up to 0-ish
        // and edges with exactly one endpoint off-frame (reject NOT triggered).
        let frame = GrayImage::from_pixel(100, 100, image::Luma([120]));
        let job = [sq(0, 0, 2 * MM)]; // 4 mm square, ±2 mm about gerber origin
        let p = Placement {
            tx_mm: 1.0,
            ty_mm: 1.0,
            rot_deg: 0.0,
            scale: 1.0,
            pivot_mm: (0.0, 0.0),
        };
        // Uniform 10 px/mm, y-up: bed x∈[-1,3] → cols [-10,30]; bed y∈[-1,3] →
        // rows [110,70]. So the square covers cols −10..30, rows 70..110.
        let img = composite(&frame, &job, &p, 10.0, None, [200, 60, 60], 0.9);
        let r = |x: usize, y: usize| img.pixels[y * 100 + x].r();
        // Right edge (col 30) is visible for rows 70..100 — crisp outline.
        assert!(r(30, 85) > 170, "visible right edge drawn: {}", r(30, 85));
        // Top edge (row 70) has one endpoint off-frame (col −10) and one on
        // (col 30); the reject must NOT fire, so its visible span is drawn.
        assert!(r(15, 70) > 170, "clipped top edge drawn: {}", r(15, 70));
        // Interior, on-frame and clear of any edge — softly filled, not a blob.
        assert!(
            (120..170).contains(&r(15, 90)),
            "interior softly filled: {}",
            r(15, 90)
        );
        // A far corner outside the footprint stays untouched frame gray.
        assert_eq!(img.pixels[5 * 100 + 60], Color32::from_gray(120));
    }
}
