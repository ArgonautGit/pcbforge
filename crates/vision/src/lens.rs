//! Camera lens-distortion model: a bi-cubic 2-D polynomial mapping camera
//! pixels to true bed millimeters (and back), fit from an imaged grid of known
//! geometry (e.g. a printed reference grid of known pitch).
//!
//! A homography models only a flat plane under a tilted camera (perspective) —
//! it cannot represent the lens's barrel/pincushion *curvature*. A degree-3
//! polynomial does, so once fit the camera becomes a metric ruler across the
//! whole field: straight → curved and back is captured, and residuals report
//! how well (µm). The map is fit both directions (`px→mm` for measurement,
//! `mm→px` for drawing the corrected grid back onto the image).

use nalgebra::{DMatrix, DVector, Point2};

/// The 10 bi-cubic basis terms of normalized coordinates.
fn basis(u: f64, v: f64) -> [f64; 10] {
    [
        1.0,
        u,
        v,
        u * u,
        u * v,
        v * v,
        u * u * u,
        u * u * v,
        u * v * v,
        v * v * v,
    ]
}

/// A bi-cubic 2-D polynomial mapping one plane to another, with input
/// normalization for numerical conditioning.
#[derive(Debug, Clone, PartialEq)]
pub struct Poly2 {
    cx: [f64; 10],
    cy: [f64; 10],
    /// Input is normalized as `n = (raw − center) · scale` before the basis.
    center: (f64, f64),
    scale: f64,
}

impl Poly2 {
    /// Map an input point through the polynomial.
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let u = (x - self.center.0) * self.scale;
        let v = (y - self.center.1) * self.scale;
        let b = basis(u, v);
        let mut ox = 0.0;
        let mut oy = 0.0;
        for ((cx, cy), bj) in self.cx.iter().zip(&self.cy).zip(&b) {
            ox += cx * bj;
            oy += cy * bj;
        }
        (ox, oy)
    }

    /// Fit `src → dst` (both `(x, y)`) by least squares. Needs ≥ 10 points.
    fn fit(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Result<Poly2, String> {
        let n = src.len();
        if n < 10 {
            return Err(format!("lens fit needs ≥10 points, got {n}"));
        }
        // Normalize the input to ~[-1, 1] so the design matrix is well
        // conditioned (raw pixels or mm span hundreds).
        let (mut cx, mut cy) = (0.0, 0.0);
        for &(x, y) in src {
            cx += x;
            cy += y;
        }
        cx /= n as f64;
        cy /= n as f64;
        let mut half = 1e-9_f64;
        for &(x, y) in src {
            half = half.max((x - cx).abs()).max((y - cy).abs());
        }
        let scale = 1.0 / half;

        let mut a = DMatrix::<f64>::zeros(n, 10);
        for (i, &(x, y)) in src.iter().enumerate() {
            let b = basis((x - cx) * scale, (y - cy) * scale);
            for (j, bj) in b.iter().enumerate() {
                a[(i, j)] = *bj;
            }
        }
        let bx = DVector::from_iterator(n, dst.iter().map(|&(x, _)| x));
        let by = DVector::from_iterator(n, dst.iter().map(|&(_, y)| y));
        let svd = a.svd(true, true);
        let sol_x = svd.solve(&bx, 1e-12).map_err(|e| format!("x fit: {e}"))?;
        let sol_y = svd.solve(&by, 1e-12).map_err(|e| format!("y fit: {e}"))?;
        let mut coeff_x = [0.0; 10];
        let mut coeff_y = [0.0; 10];
        for (i, (cx, cy)) in coeff_x.iter_mut().zip(coeff_y.iter_mut()).enumerate() {
            *cx = sol_x[i];
            *cy = sol_y[i];
        }
        Ok(Poly2 {
            cx: coeff_x,
            cy: coeff_y,
            center: (cx, cy),
            scale,
        })
    }
}

/// A fitted lens model: camera pixels ↔ true bed millimeters, with residuals.
#[derive(Debug, Clone)]
pub struct LensMap {
    /// Camera pixel → true bed mm (the metric ruler).
    pub px_to_mm: Poly2,
    /// True bed mm → camera pixel (to draw the corrected grid on the frame).
    pub mm_to_px: Poly2,
    /// RMS of the `px→mm` reprojection over the fit points, µm.
    pub rms_um: f64,
    /// Worst single-point residual, µm.
    pub max_um: f64,
    /// Per fit point: `(px_x, px_y, residual_µm)` — for the heat-map / vectors.
    pub residuals: Vec<(f64, f64, f64)>,
}

/// Fit a lens model from `(pixel, true_mm)` correspondences (the imaged known
/// grid). Needs ≥ 10 non-degenerate points.
pub fn fit_lens(pairs: &[(Point2<f64>, Point2<f64>)]) -> Result<LensMap, String> {
    let px: Vec<(f64, f64)> = pairs.iter().map(|(p, _)| (p.x, p.y)).collect();
    let mm: Vec<(f64, f64)> = pairs.iter().map(|(_, m)| (m.x, m.y)).collect();
    let px_to_mm = Poly2::fit(&px, &mm)?;
    let mm_to_px = Poly2::fit(&mm, &px)?;

    let mut residuals = Vec::with_capacity(pairs.len());
    let mut sumsq = 0.0;
    let mut max = 0.0_f64;
    for (p, m) in pairs {
        let (ex, ey) = px_to_mm.apply(p.x, p.y);
        let d_um = ((ex - m.x).powi(2) + (ey - m.y).powi(2)).sqrt() * 1000.0;
        residuals.push((p.x, p.y, d_um));
        sumsq += d_um * d_um;
        max = max.max(d_um);
    }
    let rms_um = (sumsq / pairs.len() as f64).sqrt();
    Ok(LensMap {
        px_to_mm,
        mm_to_px,
        rms_um,
        max_um: max,
        residuals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 7×7 grid of true-mm points at 10 mm pitch.
    fn grid_mm() -> Vec<(f64, f64)> {
        let mut v = Vec::new();
        for r in 0..7 {
            for c in 0..7 {
                v.push((c as f64 * 10.0, r as f64 * 10.0));
            }
        }
        v
    }

    /// Camera model for the test: perspective (a mild keystone) plus **radial
    /// barrel distortion** about the image center — exactly the curvature a
    /// homography cannot represent.
    fn image(mm: (f64, f64)) -> (f64, f64) {
        // Perspective-ish linear part: ~9 px/mm with a slight shear.
        let (x, y) = mm;
        let ideal_u = 60.0 + 9.0 * x + 0.03 * y;
        let ideal_v = 60.0 + 9.0 * y - 0.02 * x;
        // Barrel distortion about the image center.
        let (cx, cy) = (330.0, 330.0);
        let (du, dv) = (ideal_u - cx, ideal_v - cy);
        let r2 = (du * du + dv * dv) / (330.0 * 330.0);
        let k = 0.04; // ~4% barrel at the corner — a realistic machine-vision lens
        (cx + du * (1.0 + k * r2), cy + dv * (1.0 + k * r2))
    }

    #[test]
    fn polynomial_captures_lens_curvature_a_homography_cannot() {
        let mm = grid_mm();
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = mm
            .iter()
            .map(|&(x, y)| {
                let (u, v) = image((x, y));
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();

        // The polynomial lens model fits the distorted grid tightly.
        let lens = fit_lens(&pairs).expect("fit");
        assert!(lens.rms_um < 30.0, "lens RMS {} µm too high", lens.rms_um);
        assert!(lens.max_um < 80.0, "lens max {} µm", lens.max_um);
        assert_eq!(lens.residuals.len(), 49);

        // A homography over the SAME points leaves a large structured residual
        // (the barrel it can't model) — proving the polynomial was needed.
        let h = crate::fit_homography(&pairs).expect("homography");
        let mut hmax = 0.0_f64;
        for (p, m) in &pairs {
            let e = h.apply(*p);
            hmax = hmax.max(((e.x - m.x).powi(2) + (e.y - m.y).powi(2)).sqrt() * 1000.0);
        }
        assert!(
            hmax > 300.0,
            "homography should be visibly worse ({hmax} µm) than the polynomial"
        );
    }

    #[test]
    fn px_to_mm_and_mm_to_px_round_trip() {
        let mm = grid_mm();
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = mm
            .iter()
            .map(|&(x, y)| {
                let (u, v) = image((x, y));
                (Point2::new(u, v), Point2::new(x, y))
            })
            .collect();
        let lens = fit_lens(&pairs).unwrap();

        // A pixel we didn't fit (image of (25,25) mm) maps to ~(25,25),
        // and mm→px returns near the original pixel.
        let (u, v) = image((25.0, 25.0));
        let (x, y) = lens.px_to_mm.apply(u, v);
        assert!(
            (x - 25.0).abs() < 0.1 && (y - 25.0).abs() < 0.1,
            "px→mm ({x:.3},{y:.3})"
        );
        let (u2, v2) = lens.mm_to_px.apply(x, y);
        assert!(
            (u2 - u).abs() < 1.5 && (v2 - v).abs() < 1.5,
            "round trip px ({u2:.1},{v2:.1})"
        );
    }

    #[test]
    fn too_few_points_errors() {
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = (0..6)
            .map(|i| (Point2::new(i as f64, 0.0), Point2::new(i as f64, 0.0)))
            .collect();
        assert!(fit_lens(&pairs).is_err());
    }
}
