//! VIS-4 — fiducial detection: `find_fiducials`.
//!
//! Locates expected fiducials in a grayscale camera frame and returns their
//! sub-pixel centers in bed millimeters. Detection is *local*: each expected
//! position defines a small search window, which is what makes the detector
//! robust against the operator's honeycomb bed — the bed is covered in dark
//! holes that look exactly like the drilled 1 mm fiducial holes (seen in the
//! 2026-07-14 field photo), so a global blob search would drown in decoys.
//!
//! Pipeline per expected fiducial (backlog VIS-4): threshold → connected
//! components → intensity-weighted centroid → paraboloid check → bed mm.
//! Deviation from the prompt's pipeline, logged in docs/decisions.md: the
//! intensity-weighted centroid *is* the sub-pixel estimate (it is unbiased on
//! an anti-aliased disc), while the paraboloid fit on the matched-filter
//! response serves as an independent consistency check that feeds the
//! confidence score instead of the position.
//!
//! The pixel↔mm mapping is passed in as a [`BedMap`] (any homogeneous 3×3,
//! so VIS-3's bed homography drops in unchanged; until then callers use
//! [`BedMap::uniform_scale`] or an affine). Thresholds are derived per
//! window from robust local statistics (median / MAD), which rides out the
//! specular glare gradient a phone camera sees across bare copper.

use image::GrayImage;
use nalgebra::{Matrix3, Point2, Vector2};

/// Pixel↔bed-millimeter mapping as a homogeneous 3×3 (affine or full
/// homography — points are perspective-divided).
///
/// Pixel-coordinate convention (OpenCV's): the *center* of pixel `(i, j)`
/// is at coordinate `(i, j)`, so a pixel covers `[i-0.5, i+0.5)`. VIS-3's
/// bed calibration must use the same convention or every detection would
/// inherit a half-pixel bias.
#[derive(Debug, Clone)]
pub struct BedMap {
    mm_from_px: Matrix3<f64>,
    px_from_mm: Matrix3<f64>,
}

impl BedMap {
    /// Build from the matrix mapping pixel coordinates to bed mm (the
    /// orientation VIS-3's calibration will store). Returns `None` if the
    /// matrix is singular.
    pub fn new(mm_from_px: Matrix3<f64>) -> Option<Self> {
        let px_from_mm = mm_from_px.try_inverse()?;
        Some(Self {
            mm_from_px,
            px_from_mm,
        })
    }

    /// Axis-aligned camera at `px_per_mm`, pixel (0,0) = bed (0,0).
    /// Test/bring-up convenience until a real bed calibration exists.
    pub fn uniform_scale(px_per_mm: f64) -> Self {
        let s = 1.0 / px_per_mm;
        Self::new(Matrix3::new_nonuniform_scaling(&Vector2::new(s, s)))
            .expect("nonzero scale is invertible")
    }

    /// Axis-aligned camera at `px_per_mm` with the **y axis flipped**: bed
    /// (0,0) is the image's bottom-left and bed y grows upward, matching the
    /// machine/Gerber y-up convention, while pixel rows grow downward
    /// (`py = frame_h_px − y_mm·px_per_mm`). This is the right uniform map
    /// for a camera image of the bed — [`BedMap::uniform_scale`] conflates
    /// image rows with bed y and silently mirrors the frame.
    pub fn uniform_scale_y_flip(px_per_mm: f64, frame_h_px: f64) -> Self {
        let s = 1.0 / px_per_mm;
        // mm_from_px: x = px/s⁻¹, y = (H − py)/px_per_mm.
        let m = Matrix3::new(s, 0.0, 0.0, 0.0, -s, frame_h_px * s, 0.0, 0.0, 1.0);
        Self::new(m).expect("nonzero scale is invertible")
    }

    /// Map a pixel position to bed mm.
    pub fn px_to_mm(&self, p: Point2<f64>) -> Point2<f64> {
        self.mm_from_px.transform_point(&p)
    }

    /// Map a bed-mm position to pixels.
    pub fn mm_to_px(&self, p: Point2<f64>) -> Point2<f64> {
        self.px_from_mm.transform_point(&p)
    }
}

/// What the fiducial looks like in the frame.
#[derive(Debug, Clone, PartialEq)]
pub enum FiducialProfile {
    /// Bright blob on a dark field (hole in the pallet lit from below).
    Backlit {
        /// Blob diameter on the bed, mm.
        diameter_mm: f64,
    },
    /// Bright ablated disc scored by the contrast of the surrounding
    /// untouched ring (burned annulus fiducial).
    Annulus {
        /// Disc diameter on the bed, mm.
        diameter_mm: f64,
    },
    /// Dark dot on a bright field: burned grid dots on anodized plate, and
    /// the operator's drilled 1 mm holes on bare copper (field photo
    /// 2026-07-14 — holes at (10,10)/(60,10)/(10,60) read as dark dots).
    DarkDot {
        /// Dot diameter on the bed, mm.
        diameter_mm: f64,
    },
}

impl FiducialProfile {
    /// The target diameter on the bed, mm (common to every profile).
    pub fn diameter_mm(&self) -> f64 {
        match *self {
            Self::Backlit { diameter_mm }
            | Self::Annulus { diameter_mm }
            | Self::DarkDot { diameter_mm } => diameter_mm,
        }
    }

    /// Dark-on-bright profiles are inverted so the target is always the
    /// bright class internally.
    fn is_dark(&self) -> bool {
        matches!(self, Self::DarkDot { .. })
    }
}

/// Quality measures for one detection. `score` is the scalar gate in
/// `[0, 1]`; the components are kept so a caller (or the operator, per the
/// VIS-4 "low contrast → print SNR and stop" rule) can see *why* a
/// detection is weak.
#[derive(Debug, Clone, PartialEq)]
pub struct Confidence {
    /// Peak contrast over robust background noise (MAD-based σ).
    pub snr: f64,
    /// Component fill ratio against its circumscribed circle (1 = disc).
    pub circularity: f64,
    /// Mean target-disc minus surrounding-ring intensity, polarity
    /// normalized (positive = the profile's expected contrast exists).
    pub ring_contrast: f64,
    /// Disagreement between the centroid and the paraboloid-refined
    /// matched-filter peak, px. Large values flag an asymmetric or
    /// corrupted blob.
    pub centroid_peak_gap_px: f64,
    /// Combined `[0, 1]` score.
    pub score: f64,
}

/// One successfully located fiducial.
#[derive(Debug, Clone, PartialEq)]
pub struct Fiducial {
    /// The expected position this detection answered, bed mm.
    pub expected_mm: Point2<f64>,
    /// Detected center, bed mm.
    pub found_mm: Point2<f64>,
    /// Detected center in frame pixels (for overlay/debug).
    pub found_px: Point2<f64>,
    /// Quality measures.
    pub confidence: Confidence,
}

/// Why an expected fiducial was not found.
#[derive(Debug, Clone, PartialEq)]
pub enum Miss {
    /// The search window falls outside (or nearly outside) the frame.
    OutsideFrame,
    /// Nothing in the window rises above the noise. Per the backlog this is
    /// a lighting problem, not a code problem — the SNR is reported so the
    /// operator can see how far off it is.
    LowContrast {
        /// Measured peak-over-noise ratio.
        snr: f64,
    },
    /// Contrast exists but no blob passed the size/shape/position gates
    /// (e.g. only decoys of the wrong size in the window).
    NoCandidate {
        /// Measured peak-over-noise ratio.
        snr: f64,
    },
}

/// Minimum peak SNR before a window is declared to have any signal. Loosened
/// from 5.0: a real ablated burn under bench glare rides much lower contrast
/// than a printed target — 3.5 still sits well above sensor noise (the MAD σ
/// is robust to the glare gradient) while admitting the operator's dim dots.
const MIN_SNR: f64 = 3.5;
/// Component area gates relative to the profile's nominal disc area. The lower
/// bound is loose (0.12) because a partially-taken burn only thresholds a small
/// bright core; the upper bound (4.6) still rejects a honeycomb-bed hole ~2.2×
/// the dot diameter (≈4.8× area) — the decoy hazard from the field photo.
const AREA_MIN_FRAC: f64 = 0.12;
const AREA_MAX_FRAC: f64 = 4.6;
/// Minimum fill ratio of the circumscribed circle. Loosened from 0.35: an
/// ablated dot is rarely a clean disc (spatter, comet tails, uneven take), so
/// 0.25 admits the ragged real ones while still culling line/edge fragments.
const MIN_CIRCULARITY: f64 = 0.25;
/// Bounding-box aspect-ratio window (bw/bh). Widened from 0.35..=2.86 so a
/// smeared or elongated burn still passes; anything beyond ~3:1 is a streak.
const ASPECT_MIN: f64 = 0.3;
const ASPECT_MAX: f64 = 3.3;

/// Find each of `expected_mm` in `frame`, searching `search_mm` around the
/// expected spot. The result is aligned with `expected_mm`: index `i`
/// answers `expected_mm[i]`.
pub fn find_fiducials(
    frame: &GrayImage,
    expected_mm: &[Point2<f64>],
    search_mm: f64,
    profile: &FiducialProfile,
    bed: &BedMap,
) -> Vec<Result<Fiducial, Miss>> {
    expected_mm
        .iter()
        .map(|&e| find_one(frame, e, search_mm, profile, bed))
        .collect()
}

/// Polarity-normalized search window (target is always bright).
struct Window {
    w: usize,
    h: usize,
    v: Vec<f64>,
}

impl Window {
    fn at(&self, x: usize, y: usize) -> f64 {
        self.v[y * self.w + x]
    }
}

fn find_one(
    frame: &GrayImage,
    expected: Point2<f64>,
    search_mm: f64,
    profile: &FiducialProfile,
    bed: &BedMap,
) -> Result<Fiducial, Miss> {
    let center_px = bed.mm_to_px(expected);

    // Local pixel scale from finite differences of the bed map (exact for
    // affine, first-order for a homography — fine over a few mm).
    let sx = (bed.mm_to_px(Point2::new(expected.x + 1.0, expected.y)) - center_px).norm();
    let sy = (bed.mm_to_px(Point2::new(expected.x, expected.y + 1.0)) - center_px).norm();
    let dot_px = profile.diameter_mm() * 0.5 * (sx + sy);
    let search_px = search_mm * sx.max(sy);
    if dot_px < 2.0 {
        // The dot is sub-2-pixel: no centroid can be meaningful.
        return Err(Miss::OutsideFrame);
    }

    let half = (search_px + dot_px).ceil() as i64;
    let (fw, fh) = (frame.width() as i64, frame.height() as i64);
    let x0 = (center_px.x.round() as i64 - half).max(0);
    let y0 = (center_px.y.round() as i64 - half).max(0);
    let x1 = (center_px.x.round() as i64 + half).min(fw - 1);
    let y1 = (center_px.y.round() as i64 + half).min(fh - 1);
    if x1 - x0 < dot_px.ceil() as i64 * 2 || y1 - y0 < dot_px.ceil() as i64 * 2 {
        return Err(Miss::OutsideFrame);
    }

    let (w, h) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
    let mut v = Vec::with_capacity(w * h);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let p = frame.get_pixel(x as u32, y as u32)[0] as f64;
            v.push(if profile.is_dark() { 255.0 - p } else { p });
        }
    }
    let win = Window { w, h, v };

    // Robust local background: median + MAD ride out the copper glare
    // gradient (the window is a few mm, the gradient is board-scale).
    let bg = median(&win.v);
    let sigma = {
        let mut dev: Vec<f64> = win.v.iter().map(|&x| (x - bg).abs()).collect();
        (1.4826 * median_mut(&mut dev)).max(1e-6)
    };

    // Matched-filter response: box mean at roughly the dot's radius.
    let k = ((dot_px / 4.0).round() as usize).max(1);
    let resp = box_mean(&win, k);
    let peak = resp.iter().cloned().fold(f64::MIN, f64::max);
    let snr = (peak - bg) / sigma;
    if snr < MIN_SNR {
        return Err(Miss::LowContrast { snr });
    }

    // Threshold between background and peak, applied to the raw window.
    let thr = bg + 0.4 * (peak - bg);
    let mask: Vec<bool> = win.v.iter().map(|&x| x > thr).collect();

    // Connected components + gates: size, circularity, distance.
    let nominal_area = std::f64::consts::FRAC_PI_4 * dot_px * dot_px;
    let exp_in_win = (center_px.x - x0 as f64, center_px.y - y0 as f64);
    let best = components(&mask, w, h)
        .into_iter()
        .filter_map(|c| {
            let area = c.pixels.len() as f64;
            if !(nominal_area * AREA_MIN_FRAC..=nominal_area * AREA_MAX_FRAC).contains(&area) {
                return None;
            }
            let bw = (c.max_x - c.min_x + 1) as f64;
            let bh = (c.max_y - c.min_y + 1) as f64;
            let maxdim = bw.max(bh);
            let circularity = area / (std::f64::consts::FRAC_PI_4 * maxdim * maxdim);
            if circularity < MIN_CIRCULARITY || !(ASPECT_MIN..=ASPECT_MAX).contains(&(bw / bh)) {
                return None;
            }
            let (cx, cy) = c.mean();
            let dist = ((cx - exp_in_win.0).powi(2) + (cy - exp_in_win.1).powi(2)).sqrt();
            if dist > search_px {
                return None;
            }
            Some((c, circularity, dist))
        })
        .min_by(|a, b| a.2.total_cmp(&b.2));
    let Some((comp, circularity, _)) = best else {
        return Err(Miss::NoCandidate { snr });
    };

    // Intensity-weighted centroid over the chosen component, dilated one
    // pixel so anti-aliased edge pixels count fractionally — the sub-pixel
    // estimate. Restricting the support to the component keeps a nearby
    // blob (honeycomb bed hole grazing the window) from dragging the
    // centroid.
    let mut support = vec![false; w * h];
    for &(x, y) in &comp.pixels {
        for dy in -1i64..=1 {
            for dx in -1i64..=1 {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx >= 0 && ny >= 0 && nx < w as i64 && ny < h as i64 {
                    support[ny as usize * w + nx as usize] = true;
                }
            }
        }
    }
    let (mut sw, mut swx, mut swy) = (0.0, 0.0, 0.0);
    for y in 0..h {
        for x in 0..w {
            if !support[y * w + x] {
                continue;
            }
            let wgt = (win.at(x, y) - (bg + 0.2 * (peak - bg))).max(0.0);
            sw += wgt;
            swx += wgt * x as f64;
            swy += wgt * y as f64;
        }
    }
    if sw <= 0.0 {
        return Err(Miss::NoCandidate { snr });
    }
    let centroid = (swx / sw, swy / sw);

    // Paraboloid refinement of the matched-filter peak — consistency check.
    let peak_sub = paraboloid_peak(&resp, w, h, centroid);
    let gap = ((peak_sub.0 - centroid.0).powi(2) + (peak_sub.1 - centroid.1).powi(2)).sqrt();

    // Ring contrast: target disc vs the surrounding annulus, polarity
    // normalized (positive = profile-consistent). Scores the tan ring for
    // Annulus and the copper field for DarkDot alike.
    let ring_contrast = disc_ring_contrast(&win, centroid, dot_px);

    let score = (snr / 10.0).min(1.0)
        * circularity.min(1.0)
        * (ring_contrast / (0.5 * (peak - bg))).clamp(0.0, 1.0)
        * (1.0 - (gap / dot_px).min(1.0));

    let found_px = Point2::new(centroid.0 + x0 as f64, centroid.1 + y0 as f64);
    Ok(Fiducial {
        expected_mm: expected,
        found_mm: bed.px_to_mm(found_px),
        found_px,
        confidence: Confidence {
            snr,
            circularity,
            ring_contrast,
            centroid_peak_gap_px: gap,
            score,
        },
    })
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    median_mut(&mut s)
}

fn median_mut(v: &mut [f64]) -> f64 {
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, f64::total_cmp);
    *m
}

/// Box mean of the window at radius `k` via a summed-area table.
fn box_mean(win: &Window, k: usize) -> Vec<f64> {
    let (w, h) = (win.w, win.h);
    let mut sat = vec![0.0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            sat[(y + 1) * (w + 1) + (x + 1)] =
                win.at(x, y) + sat[y * (w + 1) + (x + 1)] + sat[(y + 1) * (w + 1) + x]
                    - sat[y * (w + 1) + x];
        }
    }
    let mut out = vec![0.0f64; w * h];
    for y in 0..h {
        for x in 0..w {
            let x0 = x.saturating_sub(k);
            let y0 = y.saturating_sub(k);
            let x1 = (x + k + 1).min(w);
            let y1 = (y + k + 1).min(h);
            let sum = sat[y1 * (w + 1) + x1] - sat[y0 * (w + 1) + x1] - sat[y1 * (w + 1) + x0]
                + sat[y0 * (w + 1) + x0];
            out[y * w + x] = sum / ((x1 - x0) * (y1 - y0)) as f64;
        }
    }
    out
}

struct Component {
    pixels: Vec<(usize, usize)>,
    min_x: usize,
    max_x: usize,
    min_y: usize,
    max_y: usize,
}

impl Component {
    fn mean(&self) -> (f64, f64) {
        let n = self.pixels.len() as f64;
        let sx: usize = self.pixels.iter().map(|p| p.0).sum();
        let sy: usize = self.pixels.iter().map(|p| p.1).sum();
        (sx as f64 / n, sy as f64 / n)
    }
}

/// 8-connected components of `mask`.
fn components(mask: &[bool], w: usize, h: usize) -> Vec<Component> {
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        let mut comp = Component {
            pixels: Vec::new(),
            min_x: usize::MAX,
            max_x: 0,
            min_y: usize::MAX,
            max_y: 0,
        };
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            comp.pixels.push((x, y));
            comp.min_x = comp.min_x.min(x);
            comp.max_x = comp.max_x.max(x);
            comp.min_y = comp.min_y.min(y);
            comp.max_y = comp.max_y.max(y);
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if mask[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        out.push(comp);
    }
    out
}

/// Sub-pixel peak of `resp` by separable 3-point parabola fits around the
/// integer cell nearest `near`. Falls back to the integer peak when the
/// parabola is degenerate (flat top).
fn paraboloid_peak(resp: &[f64], w: usize, h: usize, near: (f64, f64)) -> (f64, f64) {
    let cx = (near.0.round() as usize).clamp(1, w - 2);
    let cy = (near.1.round() as usize).clamp(1, h - 2);
    let r = |x: usize, y: usize| resp[y * w + x];
    let sub = |lo: f64, c: f64, hi: f64| -> f64 {
        let denom = lo + hi - 2.0 * c;
        if denom < -1e-9 {
            (0.5 * (lo - hi) / denom).clamp(-1.0, 1.0)
        } else {
            0.0
        }
    };
    (
        cx as f64 + sub(r(cx - 1, cy), r(cx, cy), r(cx + 1, cy)),
        cy as f64 + sub(r(cx, cy - 1), r(cx, cy), r(cx, cy + 1)),
    )
}

/// Mean intensity of the target disc minus the surrounding annulus
/// (`[0.8d, 1.4d]` of the center), in polarity-normalized values.
fn disc_ring_contrast(win: &Window, center: (f64, f64), dot_px: f64) -> f64 {
    let (mut disc_sum, mut disc_n, mut ring_sum, mut ring_n) = (0.0, 0usize, 0.0, 0usize);
    let r_out = (1.4 * dot_px).ceil() as i64;
    for dy in -r_out..=r_out {
        for dx in -r_out..=r_out {
            let (x, y) = (center.0.round() as i64 + dx, center.1.round() as i64 + dy);
            if x < 0 || y < 0 || x >= win.w as i64 || y >= win.h as i64 {
                continue;
            }
            let d = ((x as f64 - center.0).powi(2) + (y as f64 - center.1).powi(2)).sqrt();
            let val = win.at(x as usize, y as usize);
            if d <= 0.4 * dot_px {
                disc_sum += val;
                disc_n += 1;
            } else if d >= 0.8 * dot_px && d <= 1.4 * dot_px {
                ring_sum += val;
                ring_n += 1;
            }
        }
    }
    if disc_n == 0 || ring_n == 0 {
        return 0.0;
    }
    disc_sum / disc_n as f64 - ring_sum / ring_n as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The y-flipped uniform map puts bed (0,0) at the image's bottom-left
    /// and round-trips px↔mm exactly.
    #[test]
    fn uniform_scale_y_flip_maps_bottom_left_origin() {
        let bed = BedMap::uniform_scale_y_flip(10.0, 480.0);
        // Bed origin → bottom-left pixel row (y = H).
        let p = bed.mm_to_px(Point2::new(0.0, 0.0));
        assert!((p.x - 0.0).abs() < 1e-9 && (p.y - 480.0).abs() < 1e-9);
        // 10 mm up from the bed origin → 100 px above the bottom.
        let q = bed.mm_to_px(Point2::new(5.0, 10.0));
        assert!((q.x - 50.0).abs() < 1e-9 && (q.y - 380.0).abs() < 1e-9);
        // Round trip.
        let r = bed.px_to_mm(q);
        assert!((r.x - 5.0).abs() < 1e-9 && (r.y - 10.0).abs() < 1e-9);
    }

    /// xorshift64* — deterministic noise without a rand dependency.
    struct Rng(u64);
    impl Rng {
        fn next_f64(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
        }
        /// Uniform in [-a, a].
        fn noise(&mut self, a: f64) -> f64 {
            (self.next_f64() * 2.0 - 1.0) * a
        }
    }

    /// Render a frame the way the operator's field photo looks: a glare
    /// gradient across the copper, uniform sensor noise, and anti-aliased
    /// (4×4 supersampled) discs. `depth > 0` renders dark dots, `< 0`
    /// bright blobs.
    fn render(
        w: u32,
        h: u32,
        dots: &[(f64, f64, f64)], // (cx_px, cy_px, diameter_px)
        depth: f64,
        noise_amp: f64,
        seed: u64,
    ) -> GrayImage {
        let mut rng = Rng(seed | 1);
        GrayImage::from_fn(w, h, |x, y| {
            // Board-scale glare gradient, ~140 → 210 across the frame.
            let bg = 140.0 + 70.0 * (x as f64 + y as f64) / (w + h) as f64;
            let mut cover = 0.0;
            // Pixel centers at integer coordinates (the library's
            // convention): pixel x spans [x-0.5, x+0.5).
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if dots.iter().any(|&(cx, cy, d)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < d / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            let v = bg - depth * cover + rng.noise(noise_amp);
            image::Luma([v.clamp(0.0, 255.0) as u8])
        })
    }

    const PX_PER_MM: f64 = 10.0;

    fn dark_1mm() -> FiducialProfile {
        FiducialProfile::DarkDot { diameter_mm: 1.0 }
    }

    /// VIS-4 done-when: rendered blobs + noise recover centers < 0.15 px.
    #[test]
    fn synthetic_dots_recover_centers_below_0_15_px() {
        let truth_px = [
            (50.3, 40.7),
            (150.6, 42.2),
            (52.1, 141.8),
            (149.4, 139.5),
            (99.9, 90.4),
        ];
        let dots: Vec<_> = truth_px.iter().map(|&(x, y)| (x, y, 10.0)).collect();
        let frame = render(220, 200, &dots, 90.0, 6.0, 42);

        let bed = BedMap::uniform_scale(PX_PER_MM);
        let expected: Vec<_> = truth_px
            .iter()
            // Expected positions deliberately off by up to ~0.7 mm.
            .map(|&(x, y)| bed.px_to_mm(Point2::new(x + 5.0, y - 7.0)))
            .collect();

        let found = find_fiducials(&frame, &expected, 2.0, &dark_1mm(), &bed);
        for (f, &(tx, ty)) in found.iter().zip(&truth_px) {
            let f = f.as_ref().expect("all five dots must be found");
            let err = ((f.found_px.x - tx).powi(2) + (f.found_px.y - ty).powi(2)).sqrt();
            assert!(err < 0.15, "center error {err:.3} px at ({tx}, {ty})");
            assert!(f.confidence.score > 0.2, "score {:?}", f.confidence);
        }
    }

    /// The operator's board: three 1 mm holes drilled at (10,10), (60,10),
    /// (10,60) mm — the L-layout from the 2026-07-14 field photo. Recovery
    /// must be < 0.02 mm at 10 px/mm.
    #[test]
    fn operator_l_layout_recovers_all_three_holes() {
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let expected = [
            Point2::new(10.0, 10.0),
            Point2::new(60.0, 10.0),
            Point2::new(10.0, 60.0),
        ];
        // The board actually sits ~0.4 mm off nominal.
        let (dx, dy) = (0.38, -0.41);
        let dots: Vec<_> = expected
            .iter()
            .map(|e| {
                let p = bed.mm_to_px(Point2::new(e.x + dx, e.y + dy));
                (p.x, p.y, 1.0 * PX_PER_MM)
            })
            .collect();
        let frame = render(700, 700, &dots, 85.0, 5.0, 7);

        for f in find_fiducials(&frame, &expected, 2.0, &dark_1mm(), &bed) {
            let f = f.expect("hole found");
            let err = ((f.found_mm.x - (f.expected_mm.x + dx)).powi(2)
                + (f.found_mm.y - (f.expected_mm.y + dy)).powi(2))
            .sqrt();
            assert!(err < 0.02, "mm error {err:.4} at {:?}", f.expected_mm);
        }
    }

    /// Honeycomb-bed hazard from the field photo: decoy holes that look
    /// exactly like fiducials. A same-size decoy farther from the expected
    /// position must lose; an oversize decoy must be size-gated even when
    /// it is closer.
    #[test]
    fn decoy_holes_do_not_capture_the_detection() {
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let true_c = (100.0, 100.0);
        // Expected position is ~1 mm off the true hole (board nudge), and
        // the oversize decoy is CLOSER to it than the true hole — only the
        // size gate rejects it.
        let frame = render(
            200,
            200,
            &[
                (true_c.0, true_c.1, 10.0),
                (88.0, 120.0, 10.0),  // same-size decoy, 2.8 mm off expected
                (117.0, 105.0, 22.0), // oversize decoy (bed hole), 0.76 mm off
            ],
            90.0,
            5.0,
            99,
        );
        let expected = [bed.px_to_mm(Point2::new(110.0, 102.0))];
        let f = find_fiducials(&frame, &expected, 3.0, &dark_1mm(), &bed)
            .remove(0)
            .expect("true hole found");
        let err = ((f.found_px.x - true_c.0).powi(2) + (f.found_px.y - true_c.1).powi(2)).sqrt();
        assert!(err < 0.5, "locked onto a decoy: found {:?}", f.found_px);
    }

    /// Low contrast is a lighting problem: report SNR, don't hallucinate.
    #[test]
    fn low_contrast_reports_snr_and_misses() {
        let frame = render(200, 200, &[(100.0, 100.0, 10.0)], 6.0, 6.0, 3);
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let expected = [bed.px_to_mm(Point2::new(100.0, 100.0))];
        match find_fiducials(&frame, &expected, 2.0, &dark_1mm(), &bed).remove(0) {
            Err(Miss::LowContrast { snr }) => assert!(snr < MIN_SNR, "snr = {snr}"),
            other => panic!("expected LowContrast, got {other:?}"),
        }
    }

    /// Backlit profile: bright blob on dark field (inverted polarity).
    #[test]
    fn backlit_bright_blob_is_found() {
        let frame = render(200, 200, &[(80.4, 120.6, 12.0)], -90.0, 5.0, 11);
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let expected = [bed.px_to_mm(Point2::new(80.0, 121.0))];
        let f = find_fiducials(
            &frame,
            &expected,
            2.0,
            &FiducialProfile::Backlit { diameter_mm: 1.2 },
            &bed,
        )
        .remove(0)
        .expect("blob found");
        let err = ((f.found_px.x - 80.4).powi(2) + (f.found_px.y - 120.6).powi(2)).sqrt();
        assert!(err < 0.15, "center error {err:.3} px");
    }

    /// Forgiveness: a dim, low-contrast burn — the kind a real ablated grid
    /// throws under bench glare — now locks. Its SNR sits in the band between
    /// the loosened gate (3.5) and the old one (5.0), so the previous detector
    /// would have reported LowContrast and missed it.
    #[test]
    fn dim_low_contrast_burn_is_now_found() {
        // Shallow contrast (depth 25) over noise 6 → peak-over-noise ~4, in the
        // band between the loosened gate (3.5) and the old one (5.0).
        let frame = render(200, 200, &[(100.4, 99.6, 10.0)], 25.0, 6.0, 5);
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let expected = [bed.px_to_mm(Point2::new(100.0, 100.0))];
        let f = find_fiducials(&frame, &expected, 2.0, &dark_1mm(), &bed)
            .remove(0)
            .expect("dim burn found under the loosened SNR gate");
        assert!(
            f.confidence.snr < 5.0,
            "snr {:.2} should sit below the OLD gate (5.0) — proving forgiveness",
            f.confidence.snr
        );
        let err = ((f.found_px.x - 100.4).powi(2) + (f.found_px.y - 99.6).powi(2)).sqrt();
        assert!(err < 0.5, "center still recovered: {err:.3} px");
    }

    /// The bed map is not assumed axis-aligned: a rotated + translated
    /// affine (board on a nudged pallet) must round-trip to mm.
    #[test]
    fn rotated_bed_map_still_reports_mm() {
        let theta: f64 = 0.05;
        let (s, c) = theta.sin_cos();
        // px = R(θ)·mm·scale + t
        let px_from_mm = Matrix3::new(
            PX_PER_MM * c,
            -PX_PER_MM * s,
            30.0,
            PX_PER_MM * s,
            PX_PER_MM * c,
            18.0,
            0.0,
            0.0,
            1.0,
        );
        let bed = BedMap::new(px_from_mm.try_inverse().unwrap()).unwrap();

        let truth_mm = Point2::new(12.0, 9.0);
        let truth_px = bed.mm_to_px(truth_mm);
        let frame = render(300, 300, &[(truth_px.x, truth_px.y, 10.0)], 90.0, 5.0, 21);

        let f = find_fiducials(&frame, &[truth_mm], 2.0, &dark_1mm(), &bed)
            .remove(0)
            .expect("hole found");
        let err =
            ((f.found_mm.x - truth_mm.x).powi(2) + (f.found_mm.y - truth_mm.y).powi(2)).sqrt();
        assert!(err < 0.02, "mm error {err:.4}");
    }

    /// An expected position whose window leaves the frame is a clean miss.
    #[test]
    fn window_outside_frame_is_reported() {
        let frame = render(100, 100, &[], 0.0, 4.0, 5);
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let r = find_fiducials(&frame, &[Point2::new(30.0, 5.0)], 2.0, &dark_1mm(), &bed);
        assert_eq!(r[0], Err(Miss::OutsideFrame));
    }
}
