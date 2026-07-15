//! Render a *distorted* camera view of the calibration grid and prove the
//! camera-lens calibration recovers true geometry from it.
//!
//! This simulates what the camera sees looking at the printed reference grid:
//! the ideal `n×n` lattice pushed through a mild **perspective** (a slightly
//! tilted camera) plus a **radial barrel distortion** about the image centre —
//! the curvature a homography cannot model and the bi-cubic lens polynomial
//! can. Dots are dark on a light, faintly vignetted, mildly noisy background so
//! the `DarkDot` detector behaves as it would on real optics.
//!
//! It writes the PNG + a JSON ground-truth sidecar (the four corner pixels in
//! `GridSpec::corners_mm` order and every `mm→px` pair), then runs
//! `fit_camera_lens` on its own output and prints the recovered accuracy. If the
//! polynomial fit is tight and the residual is far below the raw distortion, the
//! calibration works.
//!
//! Run: `cargo run -p ui --example gen_distorted_grid`

use image::{GrayImage, Luma};
use ui::{GridSpec, fit_camera_lens};

const PX_PER_MM: f64 = 9.0;
const MARGIN: f64 = 60.0;
const SHEAR_X: f64 = 0.03;
const SHEAR_Y: f64 = -0.02;
const PROJ_X: f64 = 3.0e-4;
const PROJ_Y: f64 = 4.0e-4;
const BARREL_K: f64 = 0.05;

const N: usize = 7;
const PITCH: f64 = 10.0;
const DOT_MM: f64 = 2.0;

/// True grid-mm → distorted camera pixel (perspective + barrel). `y` is flipped
/// so mm-(0,0) lands at the lower-left, matching the printed grid.
fn distort(x: f64, y: f64, span: f64, size: f64) -> (f64, f64) {
    let c = size / 2.0;
    let denom = 1.0 + PROJ_X * (x - span / 2.0) + PROJ_Y * (y - span / 2.0);
    let u = (MARGIN + PX_PER_MM * x + SHEAR_X * y) / denom;
    let v = (size - MARGIN - PX_PER_MM * y + SHEAR_Y * x) / denom;
    let (du, dv) = (u - c, v - c);
    let r2 = (du * du + dv * dv) / (c * c);
    let f = 1.0 + BARREL_K * r2;
    (c + du * f, c + dv * f)
}

/// Cheap deterministic value noise in [-1, 1] from integer pixel coords.
fn noise(x: u32, y: u32) -> f64 {
    let mut h = x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) as f64 / u32::MAX as f64) * 2.0 - 1.0
}

fn main() {
    let span = (N - 1) as f64 * PITCH;
    let size = (MARGIN * 2.0 + span * PX_PER_MM).round() as u32;
    let sf = size as f64;

    // (mm, px) for every dot, row-major.
    let mut truth: Vec<((f64, f64), (f64, f64))> = Vec::with_capacity(N * N);
    for row in 0..N {
        for col in 0..N {
            let mm = (col as f64 * PITCH, row as f64 * PITCH);
            truth.push((mm, distort(mm.0, mm.1, span, sf)));
        }
    }

    // --- render ---------------------------------------------------------
    let bg = 238.0;
    let dark = 26.0;
    let r_px = (DOT_MM / 2.0) * PX_PER_MM;
    let mut img = GrayImage::from_pixel(size, size, Luma([bg as u8]));
    for &(_, (u, v)) in &truth {
        let x0 = ((u - r_px - 1.0).floor().max(0.0)) as u32;
        let x1 = ((u + r_px + 1.0).ceil().min(sf - 1.0)) as u32;
        let y0 = ((v - r_px - 1.0).floor().max(0.0)) as u32;
        let y1 = ((v + r_px + 1.0).ceil().min(sf - 1.0)) as u32;
        for py in y0..=y1 {
            for px in x0..=x1 {
                let d = ((px as f64 - u).powi(2) + (py as f64 - v).powi(2)).sqrt();
                // Analytic edge coverage for anti-aliasing.
                let cov = (r_px - d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let cur = img.get_pixel(px, py)[0] as f64;
                    let val = cur * (1.0 - cov) + dark * cov;
                    img.put_pixel(px, py, Luma([val.round() as u8]));
                }
            }
        }
    }
    // Radial vignette (up to 12% darker at corners) + mild sensor noise.
    let c = sf / 2.0;
    let maxr2 = 2.0 * c * c;
    for y in 0..size {
        for x in 0..size {
            let r2 = ((x as f64 - c).powi(2) + (y as f64 - c).powi(2)) / maxr2;
            let mut val = img.get_pixel(x, y)[0] as f64 * (1.0 - 0.12 * r2);
            val += noise(x, y) * 3.0;
            img.put_pixel(x, y, Luma([val.clamp(0.0, 255.0).round() as u8]));
        }
    }

    let png = "samples/calibration/grid-7x7-10mm-distorted.png";
    let json = "samples/calibration/grid-7x7-10mm-distorted.json";
    img.save(png).expect("save png");

    // --- ground-truth JSON (hand-written; no serde dep) -----------------
    let corner = |mx: f64, my: f64| {
        truth
            .iter()
            .find(|((x, y), _)| (x - mx).abs() < 1e-6 && (y - my).abs() < 1e-6)
            .map(|(_, p)| *p)
            .unwrap()
    };
    let corners = [
        corner(0.0, 0.0),
        corner(span, 0.0),
        corner(span, span),
        corner(0.0, span),
    ];
    let mut pts = String::new();
    for (i, ((mx, my), (u, v))) in truth.iter().enumerate() {
        if i > 0 {
            pts.push_str(",\n");
        }
        pts.push_str(&format!(
            "    {{ \"mm\": [{mx}, {my}], \"px\": [{u:.3}, {v:.3}] }}"
        ));
    }
    let body = format!(
        "{{\n  \"description\": \"Distorted camera view of the calibration grid \
         (perspective + barrel) for testing camera-lens calibration.\",\n  \
         \"image\": {{ \"width\": {size}, \"height\": {size}, \"px_per_mm_nominal\": {PX_PER_MM} }},\n  \
         \"grid\": {{ \"n\": {N}, \"pitch_mm\": {PITCH}, \"dot_mm\": {DOT_MM}, \"origin_mm\": [0, 0] }},\n  \
         \"distortion\": {{ \"barrel_k\": {BARREL_K}, \"shear\": [{SHEAR_X}, {SHEAR_Y}], \
         \"projective\": [{PROJ_X}, {PROJ_Y}], \"note\": \"y is image-up (mm origin lower-left)\" }},\n  \
         \"corners_px\": [[{:.3}, {:.3}], [{:.3}, {:.3}], [{:.3}, {:.3}], [{:.3}, {:.3}]],\n  \
         \"points\": [\n{pts}\n  ]\n}}\n",
        corners[0].0,
        corners[0].1,
        corners[1].0,
        corners[1].1,
        corners[2].0,
        corners[2].1,
        corners[3].0,
        corners[3].1,
    );
    std::fs::write(json, body).expect("write json");
    println!("wrote {png} ({size}x{size}) and {json}");

    // --- self-check: does the calibration recover the geometry? ---------
    let grid = GridSpec {
        origin_mm: (0.0, 0.0),
        pitch_mm: PITCH,
        n: N,
    };
    let corners_px = [corners[0], corners[1], corners[2], corners[3]];
    match fit_camera_lens(&img, corners_px, &grid, DOT_MM) {
        Ok(cal) => {
            let max_distort = cal
                .dots
                .iter()
                .map(|d| (d.distort_px.0.powi(2) + d.distort_px.1.powi(2)).sqrt())
                .fold(0.0_f64, f64::max);
            println!(
                "calibration: {}/{} dots, lens RMS {:.1} µm, worst {:.1} µm",
                cal.found, cal.total, cal.lens.rms_um, cal.lens.max_um
            );
            println!(
                "raw lens distortion present: up to {max_distort:.1} px \
                 (what a perspective-only fit would leave uncorrected)"
            );
            let ok = cal.found == N * N && cal.lens.rms_um < 60.0;
            println!(
                "{}",
                if ok {
                    "PASS ✓ calibration recovers the grid"
                } else {
                    "FAIL ✗"
                }
            );
        }
        Err(e) => println!("FAIL ✗ fit_camera_lens: {e}"),
    }
}
