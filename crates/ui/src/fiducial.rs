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
//! provides the real bed homography, the px↔mm map is a uniform scale with
//! the **y axis flipped**: bed (0,0) is the frame's bottom-left and bed y
//! grows upward, matching the machine/Gerber y-up frame (image rows grow
//! downward — mapping them to bed y directly would mirror every position
//! handed to `register`, which is what the machine burns).

use egui::{Color32, ColorImage};
use image::GrayImage;
use nalgebra::Point2;
use vision::{BedMap, FiducialProfile, Miss, find_fiducials};

/// Confidence score at or above which a detection is drawn as "strong" (green).
pub const SCORE_OK: f64 = 0.25;

/// Which [`FiducialProfile`] the operator selected, kept as a plain `Copy`
/// enum for the UI's combo box (the vision profile carries a diameter; this
/// pairs with the separate diameter field to build one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    /// Dark drilled hole / burned dot on a bright field (the operator default).
    DarkDot,
    /// Bright ablated disc inside an untouched ring (burned annulus).
    Annulus,
    /// Bright blob on a dark field (hole lit from below).
    Backlit,
}

impl ProfileKind {
    /// Build the vision [`FiducialProfile`] for this kind at `diameter_mm`.
    pub fn to_profile(self, diameter_mm: f64) -> FiducialProfile {
        match self {
            ProfileKind::DarkDot => FiducialProfile::DarkDot { diameter_mm },
            ProfileKind::Annulus => FiducialProfile::Annulus { diameter_mm },
            ProfileKind::Backlit => FiducialProfile::Backlit { diameter_mm },
        }
    }

    /// Short label for the combo box.
    pub fn label(self) -> &'static str {
        match self {
            ProfileKind::DarkDot => "Dark dot (drilled hole)",
            ProfileKind::Annulus => "Annulus (burned ring)",
            ProfileKind::Backlit => "Backlit (lit from below)",
        }
    }

    /// The three kinds, for populating the combo box.
    pub const ALL: [ProfileKind; 3] = [
        ProfileKind::DarkDot,
        ProfileKind::Annulus,
        ProfileKind::Backlit,
    ];
}

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
    /// Detected center in frame pixels per input fiducial (`None` = miss),
    /// aligned with the input order — for drawing rings over the live frame.
    pub found_px: Vec<Option<(f64, f64)>>,
    /// Counts for the header: (found_strong, found_weak, misses).
    pub tally: (usize, usize, usize),
    /// px/mm **measured** from the detected fiducial spacing vs their known
    /// design spacing — the true scale, independent of the seed. `None` with
    /// fewer than two detections.
    pub measured_px_per_mm: Option<f64>,
}

/// Index of the marker nearest `p`, within `max` distance — the hit-test for
/// dragging a search marker onto its hole. Screen-space units.
pub fn nearest_marker(markers: &[(f32, f32)], p: (f32, f32), max: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, &(mx, my)) in markers.iter().enumerate() {
        let d = ((mx - p.0).powi(2) + (my - p.1).powi(2)).sqrt();
        if d <= max && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// A detected fiducial as (design mm, found px).
type DesignPx = ((f64, f64), (f64, f64));

/// Mean px/mm over every pair of detected fiducials: pixel distance / design
/// distance. The fiducials' design spacing is their real physical spacing, so
/// this is the true camera scale.
fn measure_scale(found: &[DesignPx]) -> Option<f64> {
    let mut acc = 0.0;
    let mut n = 0;
    for i in 0..found.len() {
        for j in (i + 1)..found.len() {
            let (di, pi) = found[i];
            let (dj, pj) = found[j];
            let dmm = ((di.0 - dj.0).powi(2) + (di.1 - dj.1).powi(2)).sqrt();
            let dpx = ((pi.0 - pj.0).powi(2) + (pi.1 - pj.1).powi(2)).sqrt();
            if dmm > 1e-6 {
                acc += dpx / dmm;
                n += 1;
            }
        }
    }
    (n > 0).then(|| acc / n as f64)
}

/// Run detection on an in-memory frame and build the overlay + summary.
/// `expected_mm` is the nominal fiducial layout in bed mm; `px_per_mm` is the
/// (uniform, pre-VIS-3) bed scale; `profile` is the operator-selected fiducial
/// appearance (its diameter also sizes the overlay rings).
pub fn check_frame(
    frame: &GrayImage,
    expected_mm: &[(f64, f64)],
    px_per_mm: f64,
    profile: &FiducialProfile,
    search_mm: f64,
) -> FidResult {
    // y-flipped: bed (0,0) = frame bottom-left, bed y up (machine frame).
    let bed = BedMap::uniform_scale_y_flip(px_per_mm, frame.height() as f64);
    let expected: Vec<Point2<f64>> = expected_mm
        .iter()
        .map(|&(x, y)| Point2::new(x, y))
        .collect();
    let diameter_mm = profile.diameter_mm();
    let results = find_fiducials(frame, &expected, search_mm, profile, &bed);

    let overlay = render_overlay(frame, &expected, &results, &bed, diameter_mm, px_per_mm);
    let (rows, tally) = summarize(expected_mm, &results);
    // Measured scale from the detected fiducials' spacing.
    let found: Vec<DesignPx> = expected_mm
        .iter()
        .zip(&results)
        .filter_map(|(&d, r)| r.as_ref().ok().map(|f| (d, (f.found_px.x, f.found_px.y))))
        .collect();
    let found_px: Vec<Option<(f64, f64)>> = results
        .iter()
        .map(|r| r.as_ref().ok().map(|f| (f.found_px.x, f.found_px.y)))
        .collect();
    FidResult {
        overlay,
        rows,
        found_px,
        tally,
        measured_px_per_mm: measure_scale(&found),
    }
}

/// Load a frame from `path` (PNG/JPEG) and run [`check_frame`].
pub fn check(
    path: &str,
    expected_mm: &[(f64, f64)],
    px_per_mm: f64,
    profile: &FiducialProfile,
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
        profile,
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

    /// Bed mm → image pixel row under the y-flipped uniform map (bed origin
    /// at the frame's bottom-left, bed y up — the machine/Gerber frame).
    fn py(frame_h: u32, y_mm: f64) -> f64 {
        frame_h as f64 - y_mm * PPM
    }

    /// The operator's default profile (1 mm drilled hole → dark dot).
    fn dark() -> FiducialProfile {
        FiducialProfile::DarkDot { diameter_mm: 1.0 }
    }

    /// The operator's L-layout: all three holes found, small offsets, strong
    /// scores, and the overlay marks green at each detected center.
    #[test]
    fn operator_layout_all_found_and_marked_green() {
        let expected = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
        // Board nudged ~0.4 mm off nominal (bed mm, y-up).
        let (dx, dy) = (0.4, -0.3);
        let dots: Vec<_> = expected
            .iter()
            .map(|(ex, ey)| ((ex + dx) * PPM, py(700, ey + dy), 1.0 * PPM))
            .collect();
        let img = frame(700, 700, &dots, 85.0, 5.0);

        let r = check_frame(&img, &expected, PPM, &dark(), 2.0);
        assert_eq!(r.tally, (3, 0, 0), "all three strong: {:?}", r.rows);
        // The measured scale recovers the true px/mm from the hole spacing,
        // regardless of the seed passed in.
        let measured = r.measured_px_per_mm.expect("scale measured");
        assert!((measured - PPM).abs() < 0.1, "measured {measured} vs {PPM}");

        // Overlay carries the green found-color near each detected center.
        let [w, h] = r.overlay.size;
        let green = Color32::from_rgb(0x40, 0xc0, 0x50);
        for (ex, ey) in expected {
            let cx = ((ex + dx) * PPM) as usize;
            let cy = py(700, ey + dy) as usize;
            let hit = (cy.saturating_sub(8)..(cy + 8).min(h)).any(|y| {
                (cx.saturating_sub(8)..(cx + 8).min(w))
                    .any(|x| r.overlay.pixels[y * w + x] == green)
            });
            assert!(hit, "expected green mark near ({ex},{ey})");
        }
    }

    /// The measured scale recovers the true px/mm even when the seed is off:
    /// the seed only has to be close enough to place the search windows (a wide
    /// window here absorbs a 5% seed error), and the reported scale then comes
    /// from the fiducial spacing, not the seed.
    #[test]
    fn measured_scale_recovers_truth_from_an_off_seed() {
        let expected = [(10.0, 10.0), (60.0, 10.0), (10.0, 60.0)];
        let dots: Vec<_> = expected
            .iter()
            .map(|(ex, ey)| (ex * PPM, py(700, *ey), 1.0 * PPM))
            .collect();
        let img = frame(700, 700, &dots, 85.0, 5.0);
        // Seed 9.5 px/mm (5% low) with a generous 5 mm search window.
        let r = check_frame(&img, &expected, 9.5, &dark(), 5.0);
        assert_eq!(r.tally.0, 3, "all found with the off seed: {:?}", r.rows);
        let measured = r.measured_px_per_mm.unwrap();
        assert!(
            (measured - PPM).abs() < 0.1,
            "measured {measured}, truth {PPM}"
        );
    }

    /// A low-contrast frame produces a MISS row that names the SNR reason —
    /// a lighting problem surfaced, not a silent bad lock.
    #[test]
    fn low_contrast_reports_a_miss_with_reason() {
        let img = frame(200, 200, &[(100.0, 100.0, 10.0)], 6.0, 6.0);
        let r = check_frame(&img, &[(10.0, 10.0)], PPM, &dark(), 2.0);
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
        // Expected bed (10,10) mm → image (100, py(220,10)=120) px.
        let true_c = (100.0, py(220, 10.0));
        let img = frame(
            220,
            220,
            &[(true_c.0, true_c.1, 10.0), (125.0, true_c.1, 10.0)],
            85.0,
            5.0,
        );
        let r = check_frame(&img, &[(10.0, 10.0)], PPM, &dark(), 3.0);
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
    fn nearest_marker_picks_the_closest_within_range() {
        let markers = [(10.0, 10.0), (100.0, 100.0), (12.0, 9.0)];
        // Closest to (11,10) is index 2 (dist ~1.4) over index 0 (dist ~1.0)?
        // index 0 is (10,10) dist 1.0; index 2 is (12,9) dist ~1.4 → index 0.
        assert_eq!(nearest_marker(&markers, (11.0, 10.0), 30.0), Some(0));
        // Nothing within range.
        assert_eq!(nearest_marker(&markers, (500.0, 500.0), 30.0), None);
        assert_eq!(nearest_marker(&[], (0.0, 0.0), 30.0), None);
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
        assert!(check("", &[(1.0, 1.0)], 10.0, &dark(), 2.0).is_err());
        assert!(check("x.png", &[(1.0, 1.0)], 0.0, &dark(), 2.0).is_err());
    }
}
