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

    /// The 23 numbers that define this map: 10 `cx`, 10 `cy`, then `center.0`,
    /// `center.1`, `scale`. Round-trips through [`Poly2::from_coeffs`] — the
    /// serialization the field-correction file and the UI persist.
    pub fn to_coeffs(&self) -> [f64; 23] {
        let mut v = [0.0; 23];
        v[..10].copy_from_slice(&self.cx);
        v[10..20].copy_from_slice(&self.cy);
        v[20] = self.center.0;
        v[21] = self.center.1;
        v[22] = self.scale;
        v
    }

    /// Rebuild a map from [`Poly2::to_coeffs`] output.
    pub fn from_coeffs(v: &[f64; 23]) -> Poly2 {
        let mut cx = [0.0; 10];
        let mut cy = [0.0; 10];
        cx.copy_from_slice(&v[..10]);
        cy.copy_from_slice(&v[10..20]);
        Poly2 {
            cx,
            cy,
            center: (v[20], v[21]),
            scale: v[22],
        }
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

/// A fitted **laser field** pre-distortion: the same bi-cubic machinery as
/// [`LensMap`], but between the physical machine frame and the laser's
/// *commanded* frame. Emitting `to_commanded(design)` cancels the galvo/
/// f-theta field distortion, so the beam lands on the intended geometry.
#[derive(Debug, Clone)]
pub struct FieldMap {
    /// Physical machine mm → commanded mm. Apply this to emitted geometry.
    pub to_commanded: Poly2,
    /// Commanded mm → physical machine mm (the field distortion itself), for
    /// simulation, overlays, and the round-trip check.
    pub to_physical: Poly2,
    /// RMS of the `physical→commanded` fit over the burned dots, µm.
    pub rms_um: f64,
    /// Worst single-dot residual, µm.
    pub max_um: f64,
}

impl FieldMap {
    /// Pre-distort a physical machine-mm point to the commanded mm the laser
    /// must be told so the beam lands there.
    pub fn precompensate(&self, x_mm: f64, y_mm: f64) -> (f64, f64) {
        self.to_commanded.apply(x_mm, y_mm)
    }

    /// Serialize to the field-correction file format (whitespace-separated,
    /// one directive per line) that `pcbforge register --field-map` reads.
    pub fn serialize(&self) -> String {
        let row = |c: [f64; 23]| {
            c.iter()
                .map(|v| format!("{v:.10}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        format!(
            "pcbforge-field 1\nto_commanded {}\nto_physical {}\nrms_um {:.6}\nmax_um {:.6}\n",
            row(self.to_commanded.to_coeffs()),
            row(self.to_physical.to_coeffs()),
            self.rms_um,
            self.max_um,
        )
    }

    /// Parse the field-correction file format written by [`FieldMap::serialize`].
    pub fn parse(text: &str) -> Result<FieldMap, String> {
        let mut to_commanded = None;
        let mut to_physical = None;
        let (mut rms_um, mut max_um) = (0.0, 0.0);
        let coeffs = |rest: &str| -> Result<[f64; 23], String> {
            let nums: Vec<f64> = rest
                .split_whitespace()
                .map(|s| s.parse::<f64>().map_err(|e| e.to_string()))
                .collect::<Result<_, _>>()?;
            let arr: [f64; 23] = nums
                .try_into()
                .map_err(|_| "expected 23 coefficients".to_string())?;
            Ok(arr)
        };
        for line in text.lines() {
            let line = line.trim();
            let Some((key, rest)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            match key {
                "pcbforge-field" => {
                    if rest.trim() != "1" {
                        return Err(format!("unsupported field-map version {rest:?}"));
                    }
                }
                "to_commanded" => to_commanded = Some(Poly2::from_coeffs(&coeffs(rest)?)),
                "to_physical" => to_physical = Some(Poly2::from_coeffs(&coeffs(rest)?)),
                "rms_um" => rms_um = rest.trim().parse().map_err(|_| "bad rms_um")?,
                "max_um" => max_um = rest.trim().parse().map_err(|_| "bad max_um")?,
                _ => {}
            }
        }
        Ok(FieldMap {
            to_commanded: to_commanded.ok_or("missing to_commanded")?,
            to_physical: to_physical.ok_or("missing to_physical")?,
            rms_um,
            max_um,
        })
    }
}

/// Fit a laser-field pre-distortion from `(physical_mm, commanded_mm)`
/// correspondences: the physical position each burned dot actually landed at
/// (read through the metric camera-lens map) paired with the commanded
/// coordinate it was burned at. Needs ≥ 10 non-degenerate points.
pub fn fit_field(pairs: &[(Point2<f64>, Point2<f64>)]) -> Result<FieldMap, String> {
    let phys: Vec<(f64, f64)> = pairs.iter().map(|(p, _)| (p.x, p.y)).collect();
    let cmd: Vec<(f64, f64)> = pairs.iter().map(|(_, c)| (c.x, c.y)).collect();
    let to_commanded = Poly2::fit(&phys, &cmd)?;
    let to_physical = Poly2::fit(&cmd, &phys)?;
    let mut sumsq = 0.0;
    let mut max = 0.0_f64;
    for (p, c) in pairs {
        let (ex, ey) = to_commanded.apply(p.x, p.y);
        let d_um = ((ex - c.x).powi(2) + (ey - c.y).powi(2)).sqrt() * 1000.0;
        sumsq += d_um * d_um;
        max = max.max(d_um);
    }
    Ok(FieldMap {
        to_commanded,
        to_physical,
        rms_um: (sumsq / pairs.len() as f64).sqrt(),
        max_um: max,
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

    /// The laser's field distortion: a commanded coordinate physically lands
    /// pincushioned outward about the field center (70,70) — up to ~6% at the
    /// edge. This is the map the pre-distortion must cancel.
    fn laser_field(cmd: (f64, f64)) -> (f64, f64) {
        let (du, dv) = (cmd.0 - 70.0, cmd.1 - 70.0);
        let r2 = (du * du + dv * dv) / (70.0 * 70.0);
        let f = 1.0 + 0.03 * r2; // ~3% pincushion — a realistic residual field
        (70.0 + du * f, 70.0 + dv * f)
    }

    #[test]
    fn precompensation_cancels_the_field_distortion() {
        // Calibration: we command a grid and observe where it physically lands.
        let mut pairs = Vec::new();
        for r in 0..7 {
            for c in 0..7 {
                let cmd = (c as f64 * 20.0, r as f64 * 20.0);
                let phys = laser_field(cmd); // where the beam actually went
                pairs.push((Point2::new(phys.0, phys.1), Point2::new(cmd.0, cmd.1)));
            }
        }
        let field = fit_field(&pairs).expect("fit");
        assert!(field.rms_um < 60.0, "field fit RMS {} µm", field.rms_um);

        // A desired physical target we did NOT calibrate on: pre-distort it,
        // then push the commanded value through the real field — it lands back
        // on target far tighter than the raw command would.
        for &(tx, ty) in &[(35.0, 35.0), (10.0, 110.0), (120.0, 15.0)] {
            let (cx, cy) = field.precompensate(tx, ty);
            let (lx, ly) = laser_field((cx, cy));
            let err = ((lx - tx).powi(2) + (ly - ty).powi(2)).sqrt() * 1000.0;

            // Without pre-distortion the same command lands visibly off — the
            // error the correction removes.
            let raw = laser_field((tx, ty));
            let raw_err = ((raw.0 - tx).powi(2) + (raw.1 - ty).powi(2)).sqrt() * 1000.0;
            assert!(
                err < raw_err / 10.0,
                "pre-distortion cuts error ≥10×: {err:.0} µm vs raw {raw_err:.0} µm at ({tx},{ty})"
            );
            assert!(err < 80.0, "target ({tx},{ty}) lands off by {err:.1} µm");
        }
    }

    #[test]
    fn field_map_serialize_round_trips() {
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = (0..7)
            .flat_map(|r| {
                (0..7).map(move |c| {
                    let cmd = (c as f64 * 20.0, r as f64 * 20.0);
                    let phys = laser_field(cmd);
                    (Point2::new(phys.0, phys.1), Point2::new(cmd.0, cmd.1))
                })
            })
            .collect();
        let field = fit_field(&pairs).unwrap();
        let restored = FieldMap::parse(&field.serialize()).expect("parse");
        // The de/serialized map precompensates identically.
        let (a, b) = field.precompensate(33.0, 47.0);
        let (c, d) = restored.precompensate(33.0, 47.0);
        assert!(
            (a - c).abs() < 1e-6 && (b - d).abs() < 1e-6,
            "round-trip precompensation: ({a},{b}) vs ({c},{d})"
        );
        assert!((restored.rms_um - field.rms_um).abs() < 1e-3);
    }
}
