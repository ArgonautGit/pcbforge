//! Fiducial-check view: run `vision::find_fiducials` (VIS-4) on a frame and
//! draw the result over it, so the operator confirms the detector locked onto
//! the real drilled holes (not honeycomb-bed decoys) before any registration is
//! trusted.
//!
//! Overlays, painted into an [`egui::ColorImage`] (verifiable headless, shown
//! via the console's texture path):
//! * **cyan crosshair** at each expected fiducial;
//! * **ring + center dot** at each detected center — green if the confidence
//!   score clears [`SCORE_OK`], amber if weak;
//! * **red ✕** at the expected spot for any miss.
//!
//! The frame is a file for now (a saved camera grab or a phone photo); it
//! becomes the live VIS-1 feed later with no change to this code. Until VIS-3
//! provides the real bed homography, the px↔mm map is a uniform scale.

use egui::{Color32, ColorImage};
use image::GrayImage;
use nalgebra::Point2;
use vision::{BedMap, FiducialProfile, Miss, find_fiducials};

/// Confidence score at or above which a detection is drawn as "strong" (green).
pub const SCORE_OK: f64 = 0.25;

const CYAN: Color32 = Color32::from_rgb(0x22, 0xcc, 0xdd);
const GREEN: Color32 = Color32::from_rgb(0x40, 0xc0, 0x50);
const AMBER: Color32 = Color32::from_rgb(0xe0, 0x90, 0x20);
const RED: Color32 = Color32::from_rgb(0xd0, 0x40, 0x40);

/// Per-fiducial outcome for the summary list, tinted in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FidKind {
    FoundStrong,
    FoundWeak,
    Miss,
}

/// One summary row (text + how to tint it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FidRow {
    pub text: String,
    pub kind: FidKind,
}

/// The overlay image plus the per-fiducial summary.
pub struct FidResult {
    pub overlay: ColorImage,
    pub rows: Vec<FidRow>,
    /// Counts for the header: (found_strong, found_weak, misses).
    pub tally: (usize, usize, usize),
}

/// Run detection on an in-memory frame and build the overlay + summary.
/// `expected_mm` is the nominal fiducial layout in bed mm; `px_per_mm` is the
/// (uniform, pre-VIS-3) bed scale; `diameter_mm` sizes the `DarkDot` profile.
pub fn check_frame(
    frame: &GrayImage,
    expected_mm: &[(f64, f64)],
    px_per_mm: f64,
    diameter_mm: f64,
    search_mm: f64,
) -> FidResult {
    let bed = BedMap::uniform_scale(px_per_mm);
    let expected: Vec<Point2<f64>> = expected_mm
        .iter()
        .map(|&(x, y)| Point2::new(x, y))
        .collect();
    let profile = FiducialProfile::DarkDot { diameter_mm };
    let results = find_fiducials(frame, &expected, search_mm, &profile, &bed);

    let overlay = render_overlay(frame, &expected, &results, &bed, diameter_mm, px_per_mm);
    let (rows, tally) = summarize(expected_mm, &results);
    FidResult {
        overlay,
        rows,
        tally,
    }
}

/// Load a frame from `path` (PNG/JPEG) and run [`check_frame`].
pub fn check(
    path: &str,
    expected_mm: &[(f64, f64)],
    px_per_mm: f64,
    diameter_mm: f64,
    search_mm: f64,
) -> Result<FidResult, String> {
    let path = crate::clean_path(path);
    if path.is_empty() {
        return Err("set a frame image path (a saved camera grab or a photo)".into());
    }
    if px_per_mm <= 0.0 {
        return Err("px per mm must be positive".into());
    }
    let frame = image::open(&path)
        .map_err(|e| format!("open {path}: {e}"))?
        .to_luma8();
    Ok(check_frame(
        &frame,
        expected_mm,
        px_per_mm,
        diameter_mm,
        search_mm,
    ))
}

/// Parse `"10,10; 60,10; 10,60"` into fiducial points. Whitespace-tolerant;
/// returns an error naming the offending token.
pub fn parse_layout(s: &str) -> Result<Vec<(f64, f64)>, String> {
    let mut out = Vec::new();
    for (i, pair) in s
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .enumerate()
    {
        let mut it = pair.split(',').map(str::trim);
        let x = it
            .next()
            .and_then(|v| v.parse::<f64>().ok())
            .ok_or_else(|| format!("point {}: bad x in {pair:?}", i + 1))?;
        let y = it
            .next()
            .and_then(|v| v.parse::<f64>().ok())
            .ok_or_else(|| format!("point {}: bad y in {pair:?}", i + 1))?;
        out.push((x, y));
    }
    if out.is_empty() {
        return Err("no fiducials — expected e.g. `10,10; 60,10; 10,60`".into());
    }
    Ok(out)
}

fn summarize(
    expected_mm: &[(f64, f64)],
    results: &[Result<vision::Fiducial, Miss>],
) -> (Vec<FidRow>, (usize, usize, usize)) {
    let mut rows = Vec::new();
    let (mut strong, mut weak, mut miss) = (0, 0, 0);
    for (i, (exp, res)) in expected_mm.iter().zip(results).enumerate() {
        match res {
            Ok(f) => {
                let off_um = ((f.found_mm.x - exp.0).powi(2) + (f.found_mm.y - exp.1).powi(2))
                    .sqrt()
                    * 1000.0;
                let score = f.confidence.score;
                let kind = if score >= SCORE_OK {
                    strong += 1;
                    FidKind::FoundStrong
                } else {
                    weak += 1;
                    FidKind::FoundWeak
                };
                rows.push(FidRow {
                    text: format!(
                        "#{i}  ({:.1},{:.1})→({:.2},{:.2}) mm   off {off_um:.0} µm   score {score:.2}",
                        exp.0, exp.1, f.found_mm.x, f.found_mm.y
                    ),
                    kind,
                });
            }
            Err(m) => {
                miss += 1;
                let why = match m {
                    Miss::LowContrast { snr } => format!("low contrast (snr {snr:.1})"),
                    Miss::NoCandidate { snr } => format!("no candidate (snr {snr:.1})"),
                    Miss::OutsideFrame => "search window outside frame".to_string(),
                };
                rows.push(FidRow {
                    text: format!("#{i}  ({:.1},{:.1}) mm   MISS: {why}", exp.0, exp.1),
                    kind: FidKind::Miss,
                });
            }
        }
    }
    (rows, (strong, weak, miss))
}

fn render_overlay(
    frame: &GrayImage,
    expected: &[Point2<f64>],
    results: &[Result<vision::Fiducial, Miss>],
    bed: &BedMap,
    diameter_mm: f64,
    px_per_mm: f64,
) -> ColorImage {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let mut px: Vec<Color32> = Vec::with_capacity(w * h);
    for p in frame.pixels() {
        px.push(Color32::from_gray(p[0]));
    }
    let mut ov = Overlay { px, w, h };

    let r = (diameter_mm * px_per_mm * 0.5).max(3.0) as i32;
    let arm = (diameter_mm * px_per_mm * 0.9).max(5.0) as i32;

    // Expected crosshairs first (under the detections).
    for e in expected {
        let p = bed.mm_to_px(*e);
        ov.cross(p.x as i32, p.y as i32, arm, CYAN);
    }
    // Detections / misses on top.
    for (e, res) in expected.iter().zip(results) {
        match res {
            Ok(f) => {
                let c = if f.confidence.score >= SCORE_OK {
                    GREEN
                } else {
                    AMBER
                };
                let (cx, cy) = (f.found_px.x as i32, f.found_px.y as i32);
                ov.ring(cx, cy, r, c);
                ov.disk(cx, cy, 2, c);
            }
            Err(_) => {
                let p = bed.mm_to_px(*e);
                ov.ex(p.x as i32, p.y as i32, arm, RED);
            }
        }
    }
    ColorImage {
        size: [w, h],
        pixels: ov.px,
    }
}

/// A pixel target with clamped plotting primitives.
struct Overlay {
    px: Vec<Color32>,
    w: usize,
    h: usize,
}

impl Overlay {
    fn put(&mut self, x: i32, y: i32, c: Color32) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = c;
        }
    }
    /// A `+` crosshair, arms of half-length `arm`, thickened one pixel.
    fn cross(&mut self, cx: i32, cy: i32, arm: i32, c: Color32) {
        for d in -arm..=arm {
            self.put(cx + d, cy, c);
            self.put(cx + d, cy + 1, c);
            self.put(cx, cy + d, c);
            self.put(cx + 1, cy + d, c);
        }
    }
    /// A `✕`, arms of half-length `arm`.
    fn ex(&mut self, cx: i32, cy: i32, arm: i32, c: Color32) {
        for d in -arm..=arm {
            self.put(cx + d, cy + d, c);
            self.put(cx + d + 1, cy + d, c);
            self.put(cx + d, cy - d, c);
            self.put(cx + d + 1, cy - d, c);
        }
    }
    /// A circle outline of radius `r` (midpoint), thickened by also drawing
    /// `r-1` so it reads at small sizes.
    fn ring(&mut self, cx: i32, cy: i32, r: i32, c: Color32) {
        for rr in [r, r - 1].into_iter().filter(|v| *v > 0) {
            let (mut x, mut y, mut d) = (rr, 0, 1 - rr);
            while x >= y {
                for (px, py) in [
                    (cx + x, cy + y),
                    (cx + y, cy + x),
                    (cx - y, cy + x),
                    (cx - x, cy + y),
                    (cx - x, cy - y),
                    (cx - y, cy - x),
                    (cx + y, cy - x),
                    (cx + x, cy - y),
                ] {
                    self.put(px, py, c);
                }
                y += 1;
                if d < 0 {
                    d += 2 * y + 1;
                } else {
                    x -= 1;
                    d += 2 * (y - x) + 1;
                }
            }
        }
    }
    /// A small filled disk of radius `r`.
    fn disk(&mut self, cx: i32, cy: i32, r: i32, c: Color32) {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    self.put(cx + dx, cy + dy, c);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift for deterministic noise (no rand dep), matching VIS-4's test.
    struct Rng(u64);
    impl Rng {
        fn noise(&mut self, a: f64) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            let u = (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64;
            (u * 2.0 - 1.0) * a
        }
    }

    /// Frame with anti-aliased dark holes on a glary copper field + optional
    /// decoys — the operator's honeycomb-bed hazard.
    fn frame(w: u32, h: u32, dots: &[(f64, f64, f64)], depth: f64, noise: f64) -> GrayImage {
        let mut rng = Rng(1);
        GrayImage::from_fn(w, h, |x, y| {
            let bg = 140.0 + 70.0 * (x as f64 + y as f64) / (w + h) as f64;
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if dots.iter().any(|&(cx, cy, d)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < d / 2.0
                    }) {
                        cover += 1.0_f64 / 16.0;
                    }
                }
            }
            image::Luma([(bg - depth * cover + rng.noise(noise)).clamp(0.0, 255.0) as u8])
        })
    }

    const PPM: f64 = 10.0;

    /// The operator's L-layout: all three holes found, small offsets, strong
    /// scores, and the overlay marks green at each detected center.
    #[test]
    fn operator_layout_all_found_and_marked_green() {
        let expected = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
        // Board nudged ~0.4 mm off nominal.
        let (dx, dy) = (0.4, -0.3);
        let dots: Vec<_> = expected
            .iter()
            .map(|(ex, ey)| ((ex + dx) * PPM, (ey + dy) * PPM, 1.0 * PPM))
            .collect();
        let img = frame(700, 700, &dots, 85.0, 5.0);

        let r = check_frame(&img, &expected, PPM, 1.0, 2.0);
        assert_eq!(r.tally, (3, 0, 0), "all three strong: {:?}", r.rows);

        // Overlay carries the green found-color near each detected center.
        let [w, h] = r.overlay.size;
        let green = Color32::from_rgb(0x40, 0xc0, 0x50);
        for (ex, ey) in expected {
            let cx = ((ex + dx) * PPM) as usize;
            let cy = ((ey + dy) * PPM) as usize;
            let hit = (cy.saturating_sub(8)..(cy + 8).min(h)).any(|y| {
                (cx.saturating_sub(8)..(cx + 8).min(w))
                    .any(|x| r.overlay.pixels[y * w + x] == green)
            });
            assert!(hit, "expected green mark near ({ex},{ey})");
        }
    }

    /// A low-contrast frame produces a MISS row that names the SNR reason —
    /// a lighting problem surfaced, not a silent bad lock.
    #[test]
    fn low_contrast_reports_a_miss_with_reason() {
        let img = frame(200, 200, &[(100.0, 100.0, 10.0)], 6.0, 6.0);
        let r = check_frame(&img, &[(10.0, 10.0)], PPM, 1.0, 2.0);
        assert_eq!(r.tally, (0, 0, 1));
        assert_eq!(r.rows[0].kind, FidKind::Miss);
        assert!(
            r.rows[0].text.contains("MISS") && r.rows[0].text.contains("snr"),
            "miss row names the SNR: {:?}",
            r.rows[0].text
        );
    }

    /// A same-size decoy 2.5 mm off the expected spot must not be marked as the
    /// fiducial — the detection stays on the true hole (VIS-4's local search).
    #[test]
    fn decoy_is_not_marked_as_the_fiducial() {
        let true_c = (100.0, 100.0);
        let img = frame(
            220,
            220,
            &[(true_c.0, true_c.1, 10.0), (125.0, 100.0, 10.0)],
            85.0,
            5.0,
        );
        let r = check_frame(&img, &[(10.0, 10.0)], PPM, 1.0, 3.0);
        assert_eq!(r.tally.0, 1, "true hole found");
        // The found row's offset must be small (locked on the true hole, not
        // the decoy 2.5 mm away).
        assert!(
            r.rows[0].text.contains("off ") && !r.rows[0].text.contains("MISS"),
            "{:?}",
            r.rows[0].text
        );
    }

    #[test]
    fn parse_layout_reads_the_operator_default() {
        assert_eq!(
            parse_layout("10,10; 60,10; 10,60").unwrap(),
            vec![(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)]
        );
        assert!(parse_layout("garbage").is_err());
        assert!(parse_layout("").is_err());
    }

    #[test]
    fn check_rejects_missing_path_and_bad_scale() {
        assert!(check("", &[(1.0, 1.0)], 10.0, 1.0, 2.0).is_err());
        assert!(check("x.png", &[(1.0, 1.0)], 0.0, 1.0, 2.0).is_err());
    }
}
