//! VIS-4 — fiducial detection: `find_fiducials`.
//!
//! Locates expected fiducials in a grayscale camera frame and returns their
//! sub-pixel centers in bed millimeters. Detection is *local*: each expected
//! position defines a small search window, which is what makes the detector
//! robust against the operator's honeycomb bed — the bed is covered in dark
//! holes that look exactly like the drilled 1 mm fiducial holes (seen in the
//! 2026-07-14 field photo), so a global blob search would drown in decoys.
//!
//! Pipeline per expected fiducial (backlog VIS-4): matched filter → threshold
//! → connected components → intensity-weighted centroid → paraboloid check →
//! bed mm, with the winner across all sites chosen jointly (see below).
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
//!
//! **Brushed metal (2026-07-26).** The operator's real surface is brushed,
//! scratched, specular aluminium, and it broke the detector outright: 95 of 168
//! checks in their diagnostic log found fewer than 3 of 4 holes. On that plate
//! a hole and the "clean" plate beside it are statistically indistinguishable
//! (sampled patches: hole mean 115.1, background mean 115.4), and the long dark
//! scratches carry MORE contrast than the fiducials do. Nothing keyed to raw
//! pixel statistics can work there. Three changes, which only work together:
//!
//! 1. A real disc-matched filter — centre mean minus surround mean at the dot's
//!    own size — replacing a box mean that merely answered "is anything here
//!    dark?". A 120 px scratch answered that just as loudly as a 2 mm hole; it
//!    fails the matched filter because the surround contains the scratch too.
//! 2. SNR measured on the filter RESPONSE rather than on raw pixels, because
//!    the response is what discriminates. The raw MAD on brushed metal is set
//!    by brush texture, and gating on it excluded the real holes.
//! 3. Selection made jointly across all sites against the arrangement the
//!    expected positions describe, instead of each site deciding alone; and
//!    within a site, ranking by match quality with distance to the expected
//!    spot as a prior rather than as the deciding rule. Four independent picks
//!    are four chances to lock onto a different scratch; one geometric decision
//!    is not, because a scratch must also sit where the layout says.
//!
//! `samples/fiducial/brushed-plate-4holes.png` is that frame, and the
//! `brushed_plate_*` tests are the acceptance bar.

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

/// The physical footprint of a fiducial mark.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FidShape {
    Circle {
        diameter_mm: f64,
    },
    /// Axis-aligned (in machine/board frame) rectangle.
    Rect {
        w_mm: f64,
        h_mm: f64,
    },
}

impl FidShape {
    /// Footprint extent on the bed as `(w_mm, h_mm)` (a circle is square).
    fn dims_mm(&self) -> (f64, f64) {
        match *self {
            Self::Circle { diameter_mm } => (diameter_mm, diameter_mm),
            Self::Rect { w_mm, h_mm } => (w_mm, h_mm),
        }
    }
}

/// What the fiducial looks like in the frame.
#[derive(Debug, Clone, PartialEq)]
pub enum FiducialProfile {
    /// Bright blob on a dark field (hole in the pallet lit from below).
    Backlit {
        /// Blob footprint on the bed.
        shape: FidShape,
    },
    /// Bright ablated disc scored by the contrast of the surrounding
    /// untouched ring (burned annulus fiducial).
    Annulus {
        /// Disc footprint on the bed.
        shape: FidShape,
    },
    /// Dark dot on a bright field: burned grid dots on anodized plate, and
    /// the operator's drilled 1 mm holes on bare copper (field photo
    /// 2026-07-14 — holes at (10,10)/(60,10)/(10,60) read as dark dots).
    DarkDot {
        /// Dot footprint on the bed.
        shape: FidShape,
    },
}

impl FiducialProfile {
    /// The target footprint on the bed (common to every profile).
    pub fn shape(&self) -> FidShape {
        match *self {
            Self::Backlit { shape } | Self::Annulus { shape } | Self::DarkDot { shape } => shape,
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
    /// Matched-filter response at the mark over the robust noise floor of the
    /// *response* (MAD-based σ). Since 2026-07-26 this is measured in the
    /// response domain, not on raw pixels: on brushed metal the raw pixel
    /// spread is dominated by scratches (a hole and its "clean" surround are
    /// statistically indistinguishable — mean 115.1 vs 115.4), so a raw-pixel
    /// SNR gated real holes out. The response is what discriminates, so the
    /// response is what gets measured.
    pub snr: f64,
    /// Shape-fill consistency score: how completely the component fills its
    /// expected footprint (circularity for circles — fill of the
    /// circumscribed circle, 1 = disc; rectangularity for rects — fill of the
    /// bounding box, ~1 = axis-aligned rectangle).
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
    /// The dot images below ~2 px across, so no centroid is meaningful. The
    /// fix is scale/diameter (move the camera closer, correct `diameter_mm`
    /// or `px_per_mm`), *not* reframing — a distinct miss from `OutsideFrame`.
    DotTooSmall {
        /// Estimated dot diameter in pixels.
        dot_px: f64,
    },
    /// Nothing in the window rises above the noise. Per the backlog this is
    /// a lighting problem, not a code problem — the SNR is reported so the
    /// operator can see how far off it is.
    LowContrast {
        /// Measured matched-filter response over the response noise floor.
        snr: f64,
    },
    /// Contrast exists but no blob passed the size/shape/position gates
    /// (e.g. only decoys of the wrong size in the window).
    NoCandidate {
        /// Measured matched-filter response over the response noise floor.
        snr: f64,
    },
}

/// Minimum matched-filter response over the response noise floor before a
/// window is declared to have any signal.
///
/// This gate lives in the RESPONSE domain (see [`Confidence::snr`]). The value
/// is set by measurement on `samples/fiducial/brushed-plate-4holes.png`, not
/// lowered until things passed. In the detector's own search windows, seeded
/// 5 px off truth, the four real holes read 3.42 / 6.03 / 5.38 / 6.03 — and at
/// 3.0 they are the ONLY candidates any of those four windows produces. Not one
/// scratch on a plate whose scratches carry more raw contrast than its
/// fiducials clears the gate. That is the bracket: everything true is in,
/// everything else is out, with nothing in between to tune against.
///
/// The floor is the top-left hole at 3.42, which is weakest because its window
/// straddles a broad dark brush band that inflates the response MAD. A window
/// seeded INTO that band drops the same hole to 2.31 and the site reports
/// `LowContrast` — the detector's known worst case on this plate, covered by
/// `brushed_plate_band_seeded_site_refuses_rather_than_locks_on_texture`. The
/// gate is not raised to chase that 10% margin: it would be fitting one frame's
/// noise, and it would start missing real holes.
const MIN_RESPONSE_SNR: f64 = 3.0;
/// Response SNR at which `score`'s SNR factor saturates. Retuned with the gate:
/// the old 10.0 was calibrated to raw-pixel SNR, which runs ~2–4× higher than
/// the response SNR for the same mark, so keeping it would have parked genuine
/// bench detections under `ui`'s `SCORE_OK` and shown the operator four amber
/// "weak" rows for four correct locks.
const SNR_FULL: f64 = 6.0;
/// Candidates kept per site for the joint geometric selection. Five is enough
/// to hold the true mark plus a handful of decoys while keeping `K^N` small
/// (see [`MAX_COMBINATIONS`]).
const TOP_K: usize = 5;
/// Weight of the distance-to-expected penalty when ranking candidates within
/// one site, applied to `(dist / search_px)²`. Distance is a PRIOR here, not
/// the decisive rule it used to be (`min_by(dist)`) — a scratch that happens to
/// sit nearer the seed than the mark no longer wins by proximity alone. The
/// hard `dist <= search_px` bound stays: that is the search-window contract.
const DIST_PENALTY: f64 = 0.5;
/// Weight of the arrangement residual in a combination's score, applied to the
/// sum of `(residual / span)²` over the sites. Span-relative, like the `ui`
/// layout matcher's tolerance, because the observed quad is perspective-warped
/// and absolute pixel tolerances do not transfer between framings.
const RESID_PENALTY: f64 = 8.0;
/// Cap on `∏ Kᵢ` for exhaustive combination search. K=5 over 6 sites is 15625;
/// beyond that the product explodes (5^7 = 78125), so past this bound the
/// selector falls back to consensus-offset selection, which is linear in the
/// candidate count.
const MAX_COMBINATIONS: usize = 20_000;
/// Component threshold and centroid pedestal as fractions of the candidate's
/// own matched-filter response, above the LOCAL median around that candidate.
/// Keying both to the candidate rather than to the window's most extreme pixel
/// is the second half of the brushed-plate fix: the old `bg + 0.4·(peak − bg)`
/// was set by whatever single pixel was darkest in the window, which on
/// scratched metal is essentially never the fiducial.
const THR_FRAC: f64 = 0.5;
const PEDESTAL_FRAC: f64 = 0.2;
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

/// Per-pixel mask threshold for the whole-frame scan, in local σ above the
/// local background. This one is on RAW-pixel σ, a different noise domain from
/// [`MIN_RESPONSE_SNR`], so it is deliberately a standalone literal: it used to
/// be derived as `MIN_SNR * 0.5`, and leaving it derived would have let the
/// response-domain retune silently move the whole-frame scan's mask. The value
/// is unchanged (1.75) so the scan stays bit-for-bit what the bench-plate
/// arrangement match was tuned against. It sits low because it has to keep the
/// blob's anti-aliased skirt — thresholding a dot at its own peak level shaves
/// it to a few core pixels, which then fails the area gate.
const SCAN_THR_SIGMA: f64 = 1.75;
/// Tile edge for the whole-frame local statistics: a multiple of the nominal
/// dot diameter, clamped to a sane pixel range. Large enough that a tile is
/// overwhelmingly background (so its median IS the background even with a
/// fiducial in it), small enough to track the bed glare gradient the module
/// header warns about — a single global threshold drowns in that gradient.
///
/// The clamp BINDS at bench resolution: a 30 px dot in the 2592×1944 grab wants
/// 240 px and gets 128, i.e. ~4.3 dot diameters, where a dot still covers only
/// ~4% of the tile. The ceiling is what keeps the tile smaller than the glare
/// gradient's own scale on a big sensor, so it is the intended behaviour rather
/// than a limit being hit by accident.
const SCAN_TILE_DOTS: f64 = 8.0;
const SCAN_TILE_MIN: f64 = 32.0;
const SCAN_TILE_MAX: f64 = 128.0;

/// One fiducial-sized blob found anywhere in the frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    /// Intensity-weighted centre, frame pixels.
    pub px: Point2<f64>,
    /// Thresholded component area, px².
    pub area_px: f64,
    /// `[0, 1]` plausibility — SNR × shape fill × area agreement. Ranks the
    /// list; it is NOT a decoy filter (see below).
    pub score: f64,
}

/// Every blob in the WHOLE frame whose size and shape could be a fiducial,
/// best-scoring first and truncated to `max_candidates`.
///
/// This is the recovery path for when the caller has no usable guess at where
/// the fiducials image — bad calibration, board moved, no projection — so
/// [`find_fiducials`]' local windows all miss. It cannot resolve the honeycomb
/// decoy hazard on its own (the module header): with no expected position
/// there is no distance tiebreaker, and the loose [`AREA_MAX_FRAC`] band that
/// makes the local search forgiving admits bed holes here. That is by design —
/// the caller rejects decoys by matching the fiducials' known ARRANGEMENT
/// against this list, which is a far stronger discriminator than any per-blob
/// gate. It deliberately does NOT use the centre-minus-surround matched filter
/// that [`find_fiducials`] gained on 2026-07-26, even though the code is shared
/// and available ([`center_surround`]). The matched filter earns its keep by
/// discriminating inside a small window where a handful of features compete;
/// here the caller's arrangement match already supplies discrimination of a
/// far stronger kind, and this pass's job is the opposite one — be permissive,
/// hand the matcher everything that could possibly be a mark. Narrowing it to
/// compact-blob responses would drop marks the arrangement could have vouched
/// for, and the only validation this pass has is the bench-plate recovery test,
/// which the change would silently re-tune. Revisit if whole-frame recovery
/// starts failing on brushed stock.
///
/// Hence `max_candidates` is an O(n²)-budget for that matcher, not a
/// quality filter: it should be generous.
///
/// Returns empty (never panics) for a non-positive `px_per_mm`, a non-finite
/// or sub-2-pixel dot, or a degenerate frame.
pub fn find_fiducial_candidates(
    frame: &GrayImage,
    profile: &FiducialProfile,
    px_per_mm: f64,
    max_candidates: usize,
) -> Vec<Candidate> {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let shape = profile.shape();
    let (w_mm, h_mm) = shape.dims_mm();
    let (wx_px, wy_px) = (w_mm * px_per_mm, h_mm * px_per_mm);
    let dot_px = wx_px.min(wy_px);
    // Stated positively so the whole precondition reads at once. Sub-2-px dots
    // are excluded for the same reason `find_one` reports `Miss::DotTooSmall`:
    // no centroid over one or two pixels means anything.
    let usable = px_per_mm.is_finite()
        && px_per_mm > 0.0
        && wx_px.is_finite()
        && wy_px.is_finite()
        && dot_px >= 2.0
        && w >= 3
        && h >= 3
        && max_candidates > 0;
    if !usable {
        return Vec::new();
    }

    // Polarity-normalise the whole frame exactly as `find_one` does its
    // window: the target ends up the bright class either way.
    let dark = profile.is_dark();
    let v: Vec<f64> = frame
        .pixels()
        .map(|p| {
            let x = f64::from(p[0]);
            if dark { 255.0 - x } else { x }
        })
        .collect();

    // Tiled robust statistics — median + MAD per tile, the same estimator the
    // local search uses, for the same reason (a mean/std would be dragged by
    // the very blobs we are looking for, and by specular highlights).
    let tile = (SCAN_TILE_DOTS * dot_px).clamp(SCAN_TILE_MIN, SCAN_TILE_MAX);
    let nx = ((w as f64 / tile).ceil() as usize).max(1);
    let ny = ((h as f64 / tile).ceil() as usize).max(1);
    let (mut t_bg, mut t_sigma) = (vec![0.0; nx * ny], vec![0.0; nx * ny]);
    let mut buf: Vec<f64> = Vec::new();
    for ty in 0..ny {
        let y0 = ((ty as f64 * tile) as usize).min(h - 1);
        let y1 = (((ty + 1) as f64 * tile) as usize).min(h);
        for tx in 0..nx {
            let x0 = ((tx as f64 * tile) as usize).min(w - 1);
            let x1 = (((tx + 1) as f64 * tile) as usize).min(w);
            buf.clear();
            for y in y0..y1 {
                buf.extend_from_slice(&v[y * w + x0..y * w + x1]);
            }
            let m = median_mut(&mut buf);
            for e in buf.iter_mut() {
                *e = (*e - m).abs();
            }
            t_bg[ty * nx + tx] = m;
            t_sigma[ty * nx + tx] = (1.4826 * median_mut(&mut buf)).max(1e-6);
        }
    }
    // Bilinear from TILE CENTRES, not tile cells: sampling the cell value
    // directly would step at every tile edge and stamp a grid of false
    // components along the seams.
    let interp = |arr: &[f64], x: f64, y: f64| -> f64 {
        let fx = (x / tile - 0.5).clamp(0.0, (nx - 1) as f64);
        let fy = (y / tile - 0.5).clamp(0.0, (ny - 1) as f64);
        let (i0, j0) = (fx.floor() as usize, fy.floor() as usize);
        let (i1, j1) = ((i0 + 1).min(nx - 1), (j0 + 1).min(ny - 1));
        let (ax, ay) = (fx - i0 as f64, fy - j0 as f64);
        let top = arr[j0 * nx + i0] * (1.0 - ax) + arr[j0 * nx + i1] * ax;
        let bot = arr[j1 * nx + i0] * (1.0 - ax) + arr[j1 * nx + i1] * ax;
        top * (1.0 - ay) + bot * ay
    };

    let mut mask = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64, y as f64);
            let thr = interp(&t_bg, fx, fy) + SCAN_THR_SIGMA * interp(&t_sigma, fx, fy);
            mask[y * w + x] = v[y * w + x] > thr;
        }
    }

    // Same shape-aware nominal footprint and gates as `find_one`, minus its
    // distance-to-expected test (there is no expected position here).
    let nominal_area = match shape {
        FidShape::Circle { .. } => std::f64::consts::FRAC_PI_4 * wx_px * wy_px,
        FidShape::Rect { .. } => wx_px * wy_px,
    };
    let expected_aspect = wx_px / wy_px;
    let mut out: Vec<Candidate> = Vec::new();
    for c in components(&mask, w, h) {
        let area = c.pixels.len() as f64;
        if !(nominal_area * AREA_MIN_FRAC..=nominal_area * AREA_MAX_FRAC).contains(&area) {
            continue;
        }
        // A blob touching the border is clipped, so its area/aspect/centroid
        // are all measurements of the crop, not of the mark.
        if c.min_x == 0 || c.min_y == 0 || c.max_x == w - 1 || c.max_y == h - 1 {
            continue;
        }
        let bw = (c.max_x - c.min_x + 1) as f64;
        let bh = (c.max_y - c.min_y + 1) as f64;
        let maxdim = bw.max(bh);
        let shape_fill = match shape {
            FidShape::Circle { .. } => area / (std::f64::consts::FRAC_PI_4 * maxdim * maxdim),
            FidShape::Rect { .. } => area / (bw * bh),
        };
        let rel_aspect = (bw / bh) / expected_aspect;
        if shape_fill < MIN_CIRCULARITY || !(ASPECT_MIN..=ASPECT_MAX).contains(&rel_aspect) {
            continue;
        }

        let (mx, my) = c.mean();
        let bg = interp(&t_bg, mx, my);
        let sigma = interp(&t_sigma, mx, my);
        let peak = c
            .pixels
            .iter()
            .map(|&(x, y)| v[y * w + x])
            .fold(f64::MIN, f64::max);

        // Intensity-weighted centroid over the component dilated one pixel, so
        // anti-aliased edge pixels count fractionally — the same sub-pixel
        // estimate `find_one` makes. The pedestal sits at half the mask
        // threshold so those skirt pixels carry a small positive weight
        // instead of being zeroed by the threshold that excluded them.
        let pedestal = bg + 0.5 * SCAN_THR_SIGMA * sigma;
        let (mut sw, mut swx, mut swy) = (0.0, 0.0, 0.0);
        let mut seen = std::collections::HashSet::with_capacity(c.pixels.len() * 9);
        for &(x, y) in &c.pixels {
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (px, py) = (x as i64 + dx, y as i64 + dy);
                    if px < 0 || py < 0 || px >= w as i64 || py >= h as i64 {
                        continue;
                    }
                    let (px, py) = (px as usize, py as usize);
                    if !seen.insert(py * w + px) {
                        continue;
                    }
                    let wgt = (v[py * w + px] - pedestal).max(0.0);
                    sw += wgt;
                    swx += wgt * px as f64;
                    swy += wgt * py as f64;
                }
            }
        }
        if sw <= 0.0 {
            continue;
        }

        // Area agreement is symmetric in the ratio: 1.0 at nominal, decaying
        // either way. It is what keeps an oversize honeycomb hole (admitted by
        // the loose AREA_MAX_FRAC) below a true dot in the ranking.
        let area_agree = (area / nominal_area).min(nominal_area / area);
        let snr = (peak - bg) / sigma;
        let score = (snr / 10.0).min(1.0) * shape_fill.min(1.0) * area_agree;
        out.push(Candidate {
            px: Point2::new(swx / sw, swy / sw),
            area_px: area,
            score,
        });
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(max_candidates);
    out
}

/// Find each of `expected_mm` in `frame`, searching `search_mm` around the
/// expected spot. The result is aligned with `expected_mm`: index `i`
/// answers `expected_mm[i]`.
///
/// Sites are scanned independently but *chosen* jointly (2026-07-26): each
/// window offers its best few candidates, and the arrangement they form is what
/// picks the winners. Four independent per-site coin-flips could each be won by
/// a different scratch; one geometric decision cannot, because a scratch has to
/// sit where the fiducial layout says a mark should be in order to compete.
pub fn find_fiducials(
    frame: &GrayImage,
    expected_mm: &[Point2<f64>],
    search_mm: f64,
    profile: &FiducialProfile,
    bed: &BedMap,
) -> Vec<Result<Fiducial, Miss>> {
    let scans: Vec<Result<SiteScan, Miss>> = expected_mm
        .iter()
        .map(|&e| scan_site(frame, e, search_mm, profile, bed))
        .collect();
    let picks = select_jointly(&scans);
    scans
        .iter()
        .zip(picks)
        .map(|(scan, pick)| match (scan, pick) {
            (Ok(site), Some(i)) => Ok(site.finish(i, bed)),
            // A site that scanned but offered nothing the gates accepted is a
            // NoCandidate exactly as before. Joint selection never manufactures
            // a miss of its own: it reorders preferences, it does not veto.
            (Ok(site), None) => Err(Miss::NoCandidate { snr: site.snr }),
            (Err(m), _) => Err(m.clone()),
        })
        .collect()
}

/// One candidate mark inside a site's search window, fully evaluated. Every
/// quality term is computed for all `TOP_K` candidates rather than only for the
/// winner, so the number that drives the combination search is the same number
/// that lands in [`Confidence::score`].
#[derive(Debug, Clone, Copy)]
struct Cand {
    /// Centroid in FRAME pixels (not window-local): the joint selector fits an
    /// arrangement across sites, so everything it sees must share one frame.
    px: Point2<f64>,
    dist: f64,
    snr: f64,
    shape_fill: f64,
    ring_contrast: f64,
    gap: f64,
    score: f64,
}

impl Cand {
    /// Rank within a site: match quality with distance-to-expected as a soft
    /// prior. This is the whole of change 3 — `min_by(dist)` is gone.
    fn ranked(&self, search_px: f64) -> f64 {
        self.score - DIST_PENALTY * (self.dist / search_px.max(1e-9)).powi(2)
    }
}

/// A scanned site: where it was looking and what it found.
struct SiteScan {
    expected_mm: Point2<f64>,
    expected_px: Point2<f64>,
    search_px: f64,
    /// Best response SNR anywhere in the window, kept for `Miss` reporting even
    /// when no candidate survives the shape gates.
    snr: f64,
    cands: Vec<Cand>,
}

impl SiteScan {
    fn finish(&self, i: usize, bed: &BedMap) -> Fiducial {
        let c = self.cands[i];
        Fiducial {
            expected_mm: self.expected_mm,
            found_mm: bed.px_to_mm(c.px),
            found_px: c.px,
            confidence: Confidence {
                snr: c.snr,
                circularity: c.shape_fill,
                ring_contrast: c.ring_contrast,
                centroid_peak_gap_px: c.gap,
                score: c.score,
            },
        }
    }
}

/// Pick one candidate per site (or none), maximising total quality against the
/// arrangement the expected positions describe.
fn select_jointly(scans: &[Result<SiteScan, Miss>]) -> Vec<Option<usize>> {
    // Baseline: the per-site favourite. This is the answer for sites with a
    // single candidate, and the whole answer when there is no arrangement to
    // exploit.
    let mut picks: Vec<Option<usize>> = scans
        .iter()
        .map(|s| match s {
            Ok(site) => site
                .cands
                .iter()
                .enumerate()
                .max_by(|a, b| {
                    a.1.ranked(site.search_px)
                        .total_cmp(&b.1.ranked(site.search_px))
                })
                .map(|(i, _)| i),
            Err(_) => None,
        })
        .collect();

    let live: Vec<usize> = picks
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.map(|_| i))
        .collect();
    // A similarity has 4 degrees of freedom, so two point pairs fit it exactly
    // and the residual is identically zero: below three live sites the geometry
    // carries no information and the per-site ranking above is already the
    // answer.
    if live.len() < 3 {
        return picks;
    }
    let site = |i: usize| scans[i].as_ref().expect("live sites scanned Ok");

    // Arrangement scale: RMS radius of the expected positions about their
    // centroid. Residuals are measured against this so the tolerance follows
    // the layout's own size rather than a pixel count that only suits one
    // framing.
    let n = live.len() as f64;
    let cx = live.iter().map(|&i| site(i).expected_px.x).sum::<f64>() / n;
    let cy = live.iter().map(|&i| site(i).expected_px.y).sum::<f64>() / n;
    let span = (live
        .iter()
        .map(|&i| (site(i).expected_px.x - cx).powi(2) + (site(i).expected_px.y - cy).powi(2))
        .sum::<f64>()
        / n)
        .sqrt();
    // Written out rather than negated so the NaN case reads explicitly.
    if !span.is_finite() || span <= 1e-6 {
        return picks;
    }

    // ∏ Kᵢ over the live sites. `checked_mul` rather than a comparison, because
    // the product is what would overflow before any comparison could catch it.
    let total = live
        .iter()
        .try_fold(1usize, |acc, &i| acc.checked_mul(site(i).cands.len()));
    let Some(total) = total.filter(|&t| t <= MAX_COMBINATIONS) else {
        consensus_pick(scans, &live, &mut picks);
        return picks;
    };

    let mut src = Vec::with_capacity(live.len());
    let mut dst = Vec::with_capacity(live.len());
    let mut best: Option<(f64, Vec<usize>)> = None;
    let mut choice = vec![0usize; live.len()];
    for combo in 0..total {
        // Mixed-radix decode of `combo` into one candidate index per live site.
        let mut rest = combo;
        for (slot, &i) in live.iter().enumerate() {
            let k = site(i).cands.len();
            choice[slot] = rest % k;
            rest /= k;
        }
        src.clear();
        dst.clear();
        let mut quality = 0.0;
        for (slot, &i) in live.iter().enumerate() {
            let s = site(i);
            let c = s.cands[choice[slot]];
            quality += c.ranked(s.search_px);
            src.push((s.expected_px.x, s.expected_px.y));
            dst.push((c.px.x, c.px.y));
        }
        // Fit expected→found in PIXEL space, not mm: the bed map may embed a
        // y-flip, and fitting through it would demand a reflection-aware
        // similarity. Composed out of the seed positions the residual fit is
        // near-identity, so a proper similarity is all that is needed.
        let penalty: f64 = similarity_residuals(&src, &dst)
            .iter()
            .map(|r| (r / span).powi(2))
            .sum();
        let value = quality - RESID_PENALTY * penalty;
        if best.as_ref().is_none_or(|(b, _)| value > *b) {
            best = Some((value, choice.clone()));
        }
    }
    if let Some((_, choice)) = best {
        for (slot, &i) in live.iter().enumerate() {
            picks[i] = Some(choice[slot]);
        }
    }
    picks
}

/// Fallback when `∏ Kᵢ` would blow the combination cap: the largest cluster of
/// candidate-minus-expected offsets wins, then each site takes the candidate
/// best reconciling quality with that consensus. Linear in the candidate count,
/// and the same shape of answer the burned-grid lattice selection reaches for.
fn consensus_pick(scans: &[Result<SiteScan, Miss>], live: &[usize], picks: &mut [Option<usize>]) {
    let site = |i: usize| scans[i].as_ref().expect("live sites scanned Ok");
    // Cluster radius: half a search window. Wide enough that a real board
    // offset plus perspective keeps every true mark in one cluster, tight
    // enough that a scratch displaced by most of a window falls out of it.
    let radius = live
        .iter()
        .map(|&i| site(i).search_px)
        .fold(0.0f64, f64::max)
        * 0.5;
    if !radius.is_finite() || radius <= 0.0 {
        return;
    }

    let mut best: Option<(f64, (f64, f64))> = None;
    for &ci in live {
        for centre in &site(ci).cands {
            let cd = (
                centre.px.x - site(ci).expected_px.x,
                centre.px.y - site(ci).expected_px.y,
            );
            let (mut total, mut sw, mut sx, mut sy) = (0.0, 0.0, 0.0, 0.0);
            for &i in live {
                let s = site(i);
                let near = s
                    .cands
                    .iter()
                    .filter(|c| {
                        let d = (c.px.x - s.expected_px.x - cd.0)
                            .hypot(c.px.y - s.expected_px.y - cd.1);
                        d <= radius
                    })
                    .max_by(|a, b| a.ranked(s.search_px).total_cmp(&b.ranked(s.search_px)));
                if let Some(c) = near {
                    let wgt = c.ranked(s.search_px).max(0.0) + 1e-3;
                    total += wgt;
                    sw += wgt;
                    sx += wgt * (c.px.x - s.expected_px.x);
                    sy += wgt * (c.px.y - s.expected_px.y);
                }
            }
            if sw > 0.0 && best.as_ref().is_none_or(|(b, _)| total > *b) {
                best = Some((total, (sx / sw, sy / sw)));
            }
        }
    }
    let Some((_, consensus)) = best else { return };
    for &i in live {
        let s = site(i);
        let agrees = |c: &Cand| {
            (c.px.x - s.expected_px.x - consensus.0).hypot(c.px.y - s.expected_px.y - consensus.1)
                <= radius
        };
        // Agreement with the consensus is a HARD window here, not a penalty.
        // Scoring disagreement continuously would let a candidate a long way
        // outside accumulate a penalty several times any candidate's score and
        // invert the ranking outright — the worst candidate would win. Inside
        // the window, `ranked` (quality with distance-to-expected as a prior)
        // decides, exactly as it does on the exhaustive branch. A site with
        // nothing inside the window keeps its per-site favourite rather than
        // being forced onto a candidate the consensus disowns.
        if let Some((idx, _)) = s
            .cands
            .iter()
            .enumerate()
            .filter(|(_, c)| agrees(c))
            .max_by(|a, b| a.1.ranked(s.search_px).total_cmp(&b.1.ranked(s.search_px)))
        {
            picks[i] = Some(idx);
        }
    }
}

/// Per-point residuals of the least-squares similarity (rotation + uniform
/// scale + translation, no reflection) taking `src` onto `dst`. Closed form
/// (Umeyama in 2D), fitted inline rather than through `calib::fit_similarity`
/// because `calib` depends on `vision` — calling it from here is a cycle.
fn similarity_residuals(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Vec<f64> {
    let n = src.len() as f64;
    let cs = (
        src.iter().map(|p| p.0).sum::<f64>() / n,
        src.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let cd = (
        dst.iter().map(|p| p.0).sum::<f64>() / n,
        dst.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let (mut sxx, mut sxy, mut nrm) = (0.0, 0.0, 0.0);
    for (s, d) in src.iter().zip(dst) {
        let (ax, ay) = (s.0 - cs.0, s.1 - cs.1);
        let (bx, by) = (d.0 - cd.0, d.1 - cd.1);
        sxx += ax * bx + ay * by;
        sxy += ax * by - ay * bx;
        nrm += ax * ax + ay * ay;
    }
    if nrm <= 1e-12 {
        // Coincident expected positions carry no arrangement at all.
        return vec![0.0; src.len()];
    }
    let (a, b) = (sxx / nrm, sxy / nrm);
    src.iter()
        .zip(dst)
        .map(|(s, d)| {
            let (ax, ay) = (s.0 - cs.0, s.1 - cs.1);
            (cd.0 + a * ax - b * ay - d.0).hypot(cd.1 + b * ax + a * ay - d.1)
        })
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

fn scan_site(
    frame: &GrayImage,
    expected: Point2<f64>,
    search_mm: f64,
    profile: &FiducialProfile,
    bed: &BedMap,
) -> Result<SiteScan, Miss> {
    let center_px = bed.mm_to_px(expected);

    // Local pixel scale from finite differences of the bed map (exact for
    // affine, first-order for a homography — fine over a few mm).
    let sx = (bed.mm_to_px(Point2::new(expected.x + 1.0, expected.y)) - center_px).norm();
    let sy = (bed.mm_to_px(Point2::new(expected.x, expected.y + 1.0)) - center_px).norm();
    let shape = profile.shape();
    let (w_mm, h_mm) = shape.dims_mm();
    // Expected footprint in pixels. `dot_px` is the characteristic (smaller)
    // extent: it gates the sub-2px case, scales the matched filter, and
    // normalizes the centroid-peak gap. For a circle under a uniform scale
    // `wx == wy == dot_px`, so every derived quantity below reduces to the
    // pre-shape formulas bit-for-bit (calibration depends on this).
    let wx_px = w_mm * sx;
    let wy_px = h_mm * sy;
    let dot_px = wx_px.min(wy_px);
    let search_px = search_mm * sx.max(sy);
    if dot_px < 2.0 {
        // The dot is sub-2-pixel: no centroid can be meaningful. This is a
        // scale/diameter problem, not a framing one — report it as such so the
        // operator doesn't chase the camera around the bed.
        return Err(Miss::DotTooSmall { dot_px });
    }

    let (fw, fh) = (frame.width() as i64, frame.height() as i64);
    // A degenerate/near-singular seed homography (e.g. a self-intersecting
    // corner-click order) can map this site to a non-finite pixel or blow up the
    // local scale, making the search window absurdly large. Guard before the
    // window arithmetic so such a site reports a clean Miss instead of
    // overflowing the i64 window bounds — a mislabelled corner set then fails
    // detection rather than panicking.
    if !center_px.x.is_finite()
        || !center_px.y.is_finite()
        || !search_px.is_finite()
        || !dot_px.is_finite()
        || search_px > (fw + fh) as f64
    {
        return Err(Miss::OutsideFrame);
    }
    // The window has to hold a candidate anywhere in the search disc PLUS that
    // candidate's whole surround box, or the surround clips against the window
    // edge and reads biased low exactly for the off-centre candidates a
    // mis-seeded site depends on.
    let (kc, ks) = kernel_halves(wx_px, wy_px);
    let half = search_px.ceil() as i64 + ks as i64;
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

    // Matched filter: centre mean minus surrounding-annulus mean at the dot's
    // own size, replacing the old box mean at `dot_px/4`. That box mean was a
    // blur — it answered "is anything here dark?", which a 120 px scratch
    // answers just as loudly as a 2 mm hole. Centre-minus-surround asks "is
    // there a COMPACT dark thing of about this size here?", and an elongated
    // scratch fails it because the surround contains the scratch too and
    // cancels the centre.
    //
    // Centre and surround are square (box) approximations of the disc and
    // annulus, evaluated from one summed-area table so the whole response costs
    // O(1) per pixel. True discs would need a per-radius kernel and buy
    // essentially nothing here: the discriminator is the size and compactness
    // of the support, not its exact outline.
    let resp = center_surround(&win, kc, ks);

    // Only the interior is meaningful: nearer the border than `ks` the surround
    // box is clipped by the window and its mean is biased low, which would
    // manufacture a bright rim of false response. The window is built at
    // `search_px + dot_px` and `ks` is `dot_px`, so the interior is exactly the
    // region a candidate is allowed to sit in anyway.
    let (ix0, iy0) = (ks.min(w - 1), ks.min(h - 1));
    let ix1 = w.saturating_sub(ks).max(ix0 + 1);
    let iy1 = h.saturating_sub(ks).max(iy0 + 1);
    let mut interior: Vec<f64> = Vec::with_capacity((ix1 - ix0) * (iy1 - iy0));
    for y in iy0..iy1 {
        interior.extend_from_slice(&resp[y * w + ix0..y * w + ix1]);
    }

    // SNR from the RESPONSE distribution, not from raw pixels. That is the
    // point of the change: on brushed aluminium the raw MAD is set by brush
    // texture and scratches (σ ≈ 19 grey levels where the hole's own contrast
    // is ~63), so a raw-pixel SNR gated true holes out. The response σ is the
    // spread of "compact dark thing of this size" scores, against which a real
    // mark stands out.
    let r_bg = median(&interior);
    let r_sigma = {
        let mut dev: Vec<f64> = interior.iter().map(|&x| (x - r_bg).abs()).collect();
        (1.4826 * median_mut(&mut dev)).max(1e-6)
    };
    let r_peak = interior.iter().cloned().fold(f64::MIN, f64::max);
    let snr = (r_peak - r_bg) / r_sigma;
    if snr < MIN_RESPONSE_SNR {
        return Err(Miss::LowContrast { snr });
    }

    // Nominal footprint and gates, all shape-aware: for a circle they reduce to
    // the disc formulas. Aspect is on the *relative* aspect (measured /
    // expected), so a long rectangle passes while a 90°-off one is rejected.
    let nominal_area = match shape {
        FidShape::Circle { .. } => std::f64::consts::FRAC_PI_4 * wx_px * wy_px,
        FidShape::Rect { .. } => wx_px * wy_px,
    };
    let expected_aspect = wx_px / wy_px;
    // Ring-contrast band, derived from the shape. The Circle band is the exact
    // historical 0.4/0.8/1.4·dot_px so calibration detection is bit-for-bit
    // unchanged (the diag-based band gives 0.78/1.20·dot_px for a circle — too
    // tight).
    let (inner_r, ring_lo, ring_hi) = match shape {
        FidShape::Circle { .. } => (0.4 * dot_px, 0.8 * dot_px, 1.4 * dot_px),
        FidShape::Rect { .. } => {
            let diag = (wx_px * wx_px + wy_px * wy_px).sqrt();
            (0.4 * wx_px.min(wy_px), 0.55 * diag, 0.85 * diag)
        }
    };
    let exp_in_win = (center_px.x - x0 as f64, center_px.y - y0 as f64);

    let mut cands: Vec<Cand> = Vec::new();
    for ((pxi, pyi), r) in response_peaks(&resp, w, (ix0, iy0, ix1, iy1), dot_px) {
        if (r - r_bg) / r_sigma < MIN_RESPONSE_SNR {
            break; // peaks arrive best-first
        }
        // Threshold and pedestal are keyed to THIS candidate's own response and
        // its own local median, not to the window's most extreme pixel. On a
        // scratched plate the window's extreme pixel is usually a scratch, and
        // a threshold set from it either buries the mark or floods the window.
        let lbg = local_median(&win, pxi, pyi, ks);
        let thr = lbg + THR_FRAC * r;
        let mut mask = vec![false; w * h];
        let (bx0, by0) = (pxi.saturating_sub(ks), pyi.saturating_sub(ks));
        let (bx1, by1) = ((pxi + ks + 1).min(w), (pyi + ks + 1).min(h));
        for y in by0..by1 {
            for x in bx0..bx1 {
                mask[y * w + x] = win.at(x, y) > thr;
            }
        }
        // Confining the flood to the candidate's own box also bounds its area:
        // a fully-flooded box exceeds AREA_MAX_FRAC, so a candidate sitting in
        // a large dark region is size-gated rather than silently accepted.
        let Some(comp) = pick_component(&mask, w, h, pxi, pyi) else {
            continue;
        };
        let area = comp.pixels.len() as f64;
        if !(nominal_area * AREA_MIN_FRAC..=nominal_area * AREA_MAX_FRAC).contains(&area) {
            continue;
        }
        let bw = (comp.max_x - comp.min_x + 1) as f64;
        let bh = (comp.max_y - comp.min_y + 1) as f64;
        let maxdim = bw.max(bh);
        // Shape-fill consistency: circularity (fill of the circumscribed
        // circle) for circles, rectangularity (fill of the bounding box) for
        // rects — ~1.0 for an axis-aligned rect, degrading with rotation.
        let shape_fill = match shape {
            FidShape::Circle { .. } => area / (std::f64::consts::FRAC_PI_4 * maxdim * maxdim),
            FidShape::Rect { .. } => area / (bw * bh),
        };
        let rel_aspect = (bw / bh) / expected_aspect;
        if shape_fill < MIN_CIRCULARITY || !(ASPECT_MIN..=ASPECT_MAX).contains(&rel_aspect) {
            continue;
        }

        // Intensity-weighted centroid over the chosen component, dilated one
        // pixel so anti-aliased edge pixels count fractionally — the sub-pixel
        // estimate. Restricting the support to the component keeps a nearby
        // blob (honeycomb bed hole grazing the window) from dragging it.
        let Some(centroid) = weighted_centroid(&win, &comp, lbg + PEDESTAL_FRAC * r) else {
            continue;
        };
        // The search window is a contract, not a heuristic: a mark outside it
        // is not this site's mark however good it looks.
        let dist = (centroid.0 - exp_in_win.0).hypot(centroid.1 - exp_in_win.1);
        if dist > search_px {
            continue;
        }

        // Paraboloid refinement of the matched-filter peak — consistency check.
        let peak_sub = paraboloid_peak(&resp, w, h, centroid);
        let gap = (peak_sub.0 - centroid.0).hypot(peak_sub.1 - centroid.1);
        // Ring contrast: target interior vs the surrounding annulus, polarity
        // normalized (positive = profile-consistent). Scores the tan ring for
        // Annulus and the copper field for DarkDot alike.
        let ring_contrast = disc_ring_contrast(&win, centroid, inner_r, ring_lo, ring_hi);
        let c_snr = (r - r_bg) / r_sigma;
        let score = (c_snr / SNR_FULL).min(1.0)
            * shape_fill.min(1.0)
            * (ring_contrast / (0.5 * r)).clamp(0.0, 1.0)
            * (1.0 - (gap / dot_px).min(1.0));

        cands.push(Cand {
            px: Point2::new(centroid.0 + x0 as f64, centroid.1 + y0 as f64),
            dist,
            snr: c_snr,
            shape_fill,
            ring_contrast,
            gap,
            score,
        });
        if cands.len() == TOP_K {
            break;
        }
    }

    Ok(SiteScan {
        expected_mm: expected,
        expected_px: center_px,
        search_px,
        snr,
        cands,
    })
}

/// Half-sides of the matched filter's centre box and surround box, from the
/// mark's imaged extents. The centre follows the SMALLER extent so it stays
/// inside the mark, the surround follows the LARGER one so the annulus clears
/// the mark entirely. For a circle both are `dot_px` and this is the plain
/// "centre one diameter, surround two". For a 2 mm × 1 mm rectangle a surround
/// keyed to the smaller extent would be filled ~45% by the mark itself, which
/// cancels the very contrast the filter measures — that mis-sizing cost 1.4 px
/// of centroid accuracy on the rect fixture before this was split.
fn kernel_halves(wx_px: f64, wy_px: f64) -> (usize, usize) {
    let kc = ((wx_px.min(wy_px) * 0.5).round() as usize).max(1);
    let ks = ((wx_px.max(wy_px)).round() as usize).max(kc + 1);
    (kc, ks)
}

/// Centre-minus-surround matched-filter response over the whole window, via one
/// summed-area table: O(1) per pixel regardless of the dot size.
fn center_surround(win: &Window, kc: usize, ks: usize) -> Vec<f64> {
    let (w, h) = (win.w, win.h);
    let mut sat = vec![0.0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            sat[(y + 1) * (w + 1) + (x + 1)] =
                win.at(x, y) + sat[y * (w + 1) + (x + 1)] + sat[(y + 1) * (w + 1) + x]
                    - sat[y * (w + 1) + x];
        }
    }
    let boxed = |x: usize, y: usize, k: usize| -> (f64, f64) {
        let x0 = x.saturating_sub(k);
        let y0 = y.saturating_sub(k);
        let x1 = (x + k + 1).min(w);
        let y1 = (y + k + 1).min(h);
        let sum = sat[y1 * (w + 1) + x1] - sat[y0 * (w + 1) + x1] - sat[y1 * (w + 1) + x0]
            + sat[y0 * (w + 1) + x0];
        (sum, ((x1 - x0) * (y1 - y0)) as f64)
    };
    let mut out = vec![0.0f64; w * h];
    for y in 0..h {
        for x in 0..w {
            let (cs, cn) = boxed(x, y, kc);
            let (ss, sn) = boxed(x, y, ks);
            let ring_n = sn - cn;
            if ring_n > 0.0 && cn > 0.0 {
                out[y * w + x] = cs / cn - (ss - cs) / ring_n;
            }
        }
    }
    out
}

/// Strict 3×3 local maxima of the response inside `(x0, y0, x1, y1)`, returned
/// best-first and thinned so no two sit within a dot diameter of each other
/// (two peaks on one mark are one candidate). Truncated to a small multiple of
/// [`TOP_K`] because later peaks cannot survive the response gate anyway.
fn response_peaks(
    resp: &[f64],
    w: usize,
    bounds: (usize, usize, usize, usize),
    dot_px: f64,
) -> Vec<((usize, usize), f64)> {
    let (x0, y0, x1, y1) = bounds;
    let mut peaks: Vec<((usize, usize), f64)> = Vec::new();
    for y in y0.max(1)..y1.min(resp.len() / w - 1) {
        for x in x0.max(1)..x1.min(w - 1) {
            let c = resp[y * w + x];
            let is_max = (-1i64..=1).all(|dy| {
                (-1i64..=1).all(|dx| {
                    (dx == 0 && dy == 0)
                        || c >= resp[(y as i64 + dy) as usize * w + (x as i64 + dx) as usize]
                })
            });
            if is_max {
                peaks.push(((x, y), c));
            }
        }
    }
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut out: Vec<((usize, usize), f64)> = Vec::new();
    for p in peaks {
        if out.iter().any(|q| {
            (q.0.0 as f64 - p.0.0 as f64).hypot(q.0.1 as f64 - p.0.1 as f64) < dot_px
        }) {
            continue;
        }
        out.push(p);
        if out.len() == TOP_K * 3 {
            break;
        }
    }
    out
}

/// Median of the raw window over a box of half-side `k` around `(x, y)` — the
/// candidate's own local background.
fn local_median(win: &Window, x: usize, y: usize, k: usize) -> f64 {
    let x0 = x.saturating_sub(k);
    let y0 = y.saturating_sub(k);
    let x1 = (x + k + 1).min(win.w);
    let y1 = (y + k + 1).min(win.h);
    let mut buf: Vec<f64> = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for yy in y0..y1 {
        buf.extend_from_slice(&win.v[yy * win.w + x0..yy * win.w + x1]);
    }
    if buf.is_empty() { 0.0 } else { median_mut(&mut buf) }
}

/// The masked component at `(x, y)`, or — when the response peak lands on a
/// pixel the threshold excluded (a smooth response maximum need not sit on an
/// extreme pixel) — the nearest component in the mask.
fn pick_component(mask: &[bool], w: usize, h: usize, x: usize, y: usize) -> Option<Component> {
    let comps = components(mask, w, h);
    let (tx, ty) = (x as f64, y as f64);
    comps.into_iter().min_by(|a, b| {
        let d = |c: &Component| {
            if c.pixels.contains(&(x, y)) {
                return 0.0;
            }
            let (cx, cy) = c.mean();
            (cx - tx).hypot(cy - ty)
        };
        d(a).total_cmp(&d(b))
    })
}

/// Intensity-weighted centroid over `comp` dilated one pixel, above `pedestal`.
fn weighted_centroid(win: &Window, comp: &Component, pedestal: f64) -> Option<(f64, f64)> {
    let (w, h) = (win.w, win.h);
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
            let wgt = (win.at(x, y) - pedestal).max(0.0);
            sw += wgt;
            swx += wgt * x as f64;
            swy += wgt * y as f64;
        }
    }
    (sw > 0.0).then(|| (swx / sw, swy / sw))
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

/// Mean intensity of the target interior (radius `≤ inner_r`) minus the
/// surrounding annulus (`[ring_lo, ring_hi]` of the center), in
/// polarity-normalized values. The radii are supplied by the caller so the
/// band can follow the fiducial's shape.
fn disc_ring_contrast(
    win: &Window,
    center: (f64, f64),
    inner_r: f64,
    ring_lo: f64,
    ring_hi: f64,
) -> f64 {
    let (mut disc_sum, mut disc_n, mut ring_sum, mut ring_n) = (0.0, 0usize, 0.0, 0usize);
    let r_out = ring_hi.ceil() as i64;
    for dy in -r_out..=r_out {
        for dx in -r_out..=r_out {
            let (x, y) = (center.0.round() as i64 + dx, center.1.round() as i64 + dy);
            if x < 0 || y < 0 || x >= win.w as i64 || y >= win.h as i64 {
                continue;
            }
            let d = ((x as f64 - center.0).powi(2) + (y as f64 - center.1).powi(2)).sqrt();
            let val = win.at(x as usize, y as usize);
            if d <= inner_r {
                disc_sum += val;
                disc_n += 1;
            } else if d >= ring_lo && d <= ring_hi {
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
        FiducialProfile::DarkDot {
            shape: FidShape::Circle { diameter_mm: 1.0 },
        }
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
    ///
    /// The rendered depth dropped from 6 to 2 when the detector moved to a
    /// matched filter (2026-07-26). That is not the gate being loosened — it is
    /// the filter genuinely working: averaging over a dot-sized region cuts
    /// uniform pixel noise by roughly √(area), so a 6-level dot under ±6 noise
    /// is now recoverable (it locks 1.5 px from truth) and is no longer an
    /// honest example of "nothing is there". Depth 1.5 is: it reads SNR ~2.3,
    /// where depth 2 already reaches 3.1 and squeaks past the gate.
    #[test]
    fn low_contrast_reports_snr_and_misses() {
        let frame = render(200, 200, &[(100.0, 100.0, 10.0)], 1.5, 6.0, 3);
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let expected = [bed.px_to_mm(Point2::new(100.0, 100.0))];
        match find_fiducials(&frame, &expected, 2.0, &dark_1mm(), &bed).remove(0) {
            Err(Miss::LowContrast { snr }) => assert!(snr < MIN_RESPONSE_SNR, "snr = {snr}"),
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
            &FiducialProfile::Backlit {
                shape: FidShape::Circle { diameter_mm: 1.2 },
            },
            &bed,
        )
        .remove(0)
        .expect("blob found");
        let err = ((f.found_px.x - 80.4).powi(2) + (f.found_px.y - 120.6).powi(2)).sqrt();
        assert!(err < 0.15, "center error {err:.3} px");
    }

    /// Forgiveness: a dim, low-contrast burn — the kind a real ablated grid
    /// throws under bench glare — locks, and locks confidently.
    ///
    /// This test used to prove forgiveness by asserting `snr < 5.0`, the gate
    /// this detector was loosened from. That number no longer means the same
    /// thing: since 2026-07-26 `snr` is measured on the matched-filter response
    /// rather than on raw pixels, and this burn reads ~20 there. Comparing it
    /// to a raw-pixel threshold would be comparing two different quantities, so
    /// the claim is now made where it still holds — the dim burn is found, at
    /// its true centre, with a healthy score.
    #[test]
    fn dim_low_contrast_burn_is_now_found() {
        // Shallow contrast (depth 25) over noise 6: a per-pixel peak-over-noise
        // of ~4, which the pre-2026-07-05 gate of 5.0 would have rejected.
        let frame = render(200, 200, &[(100.4, 99.6, 10.0)], 25.0, 6.0, 5);
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let expected = [bed.px_to_mm(Point2::new(100.0, 100.0))];
        let f = find_fiducials(&frame, &expected, 2.0, &dark_1mm(), &bed)
            .remove(0)
            .expect("dim burn found");
        assert!(f.confidence.score > 0.2, "score {:?}", f.confidence);
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

    /// A dot imaging below ~2 px is a scale/diameter miss, not a framing one
    /// (LR-48): at 1 px/mm a 1 mm dot is only ~1 px across.
    #[test]
    fn sub_two_pixel_dot_reports_dot_too_small() {
        let frame = render(100, 100, &[], 0.0, 4.0, 5);
        let bed = BedMap::uniform_scale(1.0); // 1 px/mm → 1 mm dot ≈ 1 px
        let r = find_fiducials(&frame, &[Point2::new(30.0, 30.0)], 2.0, &dark_1mm(), &bed);
        assert!(
            matches!(r[0], Err(Miss::DotTooSmall { .. })),
            "expected DotTooSmall, got {:?}",
            r[0]
        );
    }

    /// Whole-frame recovery: every drilled hole is a candidate no matter where
    /// it sits, and the sub-pixel centres survive the tiled thresholding.
    #[test]
    fn whole_frame_candidates_include_every_hole() {
        let truth = [(60.4, 55.2), (540.7, 58.9), (63.1, 430.6), (538.2, 433.3)];
        let dots: Vec<_> = truth.iter().map(|&(x, y)| (x, y, 10.0)).collect();
        let frame = render(600, 500, &dots, 85.0, 5.0, 31);

        let cands = find_fiducial_candidates(&frame, &dark_1mm(), PX_PER_MM, 64);
        for &(tx, ty) in &truth {
            let hit = cands
                .iter()
                .any(|c| ((c.px.x - tx).powi(2) + (c.px.y - ty).powi(2)).sqrt() < 0.5);
            assert!(hit, "no candidate at ({tx}, {ty}); got {cands:?}");
        }
        // Sorted best-first.
        assert!(cands.windows(2).all(|p| p[0].score >= p[1].score));
    }

    /// The guards return an empty list rather than panicking.
    #[test]
    fn whole_frame_candidates_guard_bad_scale() {
        let frame = render(100, 100, &[(50.0, 50.0, 10.0)], 85.0, 4.0, 5);
        assert!(find_fiducial_candidates(&frame, &dark_1mm(), 0.0, 32).is_empty());
        assert!(find_fiducial_candidates(&frame, &dark_1mm(), f64::NAN, 32).is_empty());
        // 1 px/mm → a 1 mm dot images sub-2-px: nothing meaningful to centre.
        assert!(find_fiducial_candidates(&frame, &dark_1mm(), 1.0, 32).is_empty());
        assert!(find_fiducial_candidates(&frame, &dark_1mm(), PX_PER_MM, 0).is_empty());
    }

    /// Render a frame with anti-aliased **axis-aligned rectangles** on the
    /// same glary field as [`render`]. Rects are `(cx_px, cy_px, w_px, h_px)`;
    /// `depth > 0` renders dark marks. Mirrors `render`'s supersampling so a
    /// rect's centroid is recoverable to sub-pixel.
    fn render_rects(
        w: u32,
        h: u32,
        rects: &[(f64, f64, f64, f64)],
        depth: f64,
        noise_amp: f64,
        seed: u64,
    ) -> GrayImage {
        let mut rng = Rng(seed | 1);
        GrayImage::from_fn(w, h, |x, y| {
            let bg = 140.0 + 70.0 * (x as f64 + y as f64) / (w + h) as f64;
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if rects.iter().any(|&(cx, cy, rw, rh)| {
                        (px - cx).abs() < rw / 2.0 && (py - cy).abs() < rh / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            let v = bg - depth * cover + rng.noise(noise_amp);
            image::Luma([v.clamp(0.0, 255.0) as u8])
        })
    }

    /// A dark 2 mm × 1 mm rectangle (20 × 10 px at 10 px/mm) is located at its
    /// true centroid when the profile says `Rect { 2.0, 1.0 }`.
    #[test]
    fn dark_rect_is_found_at_its_centroid() {
        let (tx, ty) = (100.3, 90.6);
        let frame = render_rects(200, 200, &[(tx, ty, 20.0, 10.0)], 90.0, 5.0, 17);
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let expected = [bed.px_to_mm(Point2::new(tx + 4.0, ty - 3.0))];
        let profile = FiducialProfile::DarkDot {
            shape: FidShape::Rect {
                w_mm: 2.0,
                h_mm: 1.0,
            },
        };
        let f = find_fiducials(&frame, &expected, 2.0, &profile, &bed)
            .remove(0)
            .expect("2x1 mm rect found");
        let err = ((f.found_px.x - tx).powi(2) + (f.found_px.y - ty).powi(2)).sqrt();
        assert!(err < 0.5, "rect centroid error {err:.3} px");
    }

    /// The relative-aspect gate rejects a rectangle whose measured orientation
    /// is 90° off the expected one: a 4 mm × 0.5 mm mark (40 × 5 px) sought as
    /// `Rect { 0.5, 4.0 }` has relative aspect (40/5)/(5/40) = 64, far outside
    /// the [0.3, 3.3] band — no candidate passes.
    #[test]
    fn rect_with_swapped_extreme_aspect_is_rejected() {
        let (tx, ty) = (100.0, 100.0);
        let frame = render_rects(200, 200, &[(tx, ty, 40.0, 5.0)], 90.0, 5.0, 23);
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let expected = [bed.px_to_mm(Point2::new(tx, ty))];
        let profile = FiducialProfile::DarkDot {
            shape: FidShape::Rect {
                w_mm: 0.5,
                h_mm: 4.0,
            },
        };
        match find_fiducials(&frame, &expected, 2.0, &profile, &bed).remove(0) {
            Err(Miss::NoCandidate { .. }) => {}
            other => panic!("expected NoCandidate for 90°-off rect, got {other:?}"),
        }
    }

    /// Hand-built `SiteScan` for exercising the selectors directly. The
    /// exhaustive branch covers everything the real fixtures reach (K=5 over up
    /// to 6 sites), so the consensus fallback would otherwise ship unrun.
    fn scan_with(expected: (f64, f64), cands: &[((f64, f64), f64)]) -> Result<SiteScan, Miss> {
        Ok(SiteScan {
            expected_mm: Point2::new(expected.0, expected.1),
            expected_px: Point2::new(expected.0, expected.1),
            search_px: 30.0,
            snr: 8.0,
            cands: cands
                .iter()
                .map(|&((x, y), score)| Cand {
                    px: Point2::new(x, y),
                    dist: (x - expected.0).hypot(y - expected.1),
                    snr: 8.0,
                    shape_fill: 0.8,
                    ring_contrast: 20.0,
                    gap: 0.2,
                    score,
                })
                .collect(),
        })
    }

    /// The consensus fallback finds the coherent offset and takes it, even
    /// where the decoy at a site outscores the true mark on its own merits.
    /// Every site here is offset by (+6, −5); the decoys sit on a 28 px circle
    /// at a per-site angle, so each lies outside the consensus window.
    #[test]
    fn consensus_fallback_follows_the_shared_offset() {
        let sites: Vec<Result<SiteScan, Miss>> = (0..8)
            .map(|i| {
                let e = (100.0 + 200.0 * (i % 4) as f64, 100.0 + 200.0 * (i / 4) as f64);
                scan_with(
                    e,
                    &[
                        // A decoy scoring HIGHER than the true mark, placed at a
                        // per-site offset that no other site shares.
                        (
                            (
                                e.0 + 28.0 * ((i as f64) * 0.9).cos(),
                                e.1 + 28.0 * ((i as f64) * 0.9).sin(),
                            ),
                            0.9,
                        ),
                        ((e.0 + 6.0, e.1 - 5.0), 0.5),
                    ],
                )
            })
            .collect();
        let live: Vec<usize> = (0..8).collect();
        let mut picks = vec![Some(0); 8];
        consensus_pick(&sites, &live, &mut picks);
        for (i, p) in picks.iter().enumerate() {
            assert_eq!(
                *p,
                Some(1),
                "site {i} did not follow the shared offset (picked {p:?})"
            );
        }
    }

    /// A candidate far outside the consensus window never wins by accumulating
    /// a penalty larger than any score — the window is hard, so a site with
    /// nothing inside it keeps its per-site favourite instead of being pushed
    /// onto the worst candidate.
    #[test]
    fn consensus_fallback_leaves_a_disagreeing_site_on_its_own_favourite() {
        let mut sites: Vec<Result<SiteScan, Miss>> = (0..4)
            .map(|i| {
                let e = (100.0 + 200.0 * i as f64, 100.0);
                scan_with(e, &[((e.0 + 9.0, e.1 - 7.0), 0.6)])
            })
            .collect();
        // A fifth site whose only candidates both sit far from the consensus,
        // the better-scoring one furthest away.
        sites.push(scan_with(
            (900.0, 100.0),
            &[((922.0, 128.0), 0.8), ((880.0, 118.0), 0.3)],
        ));
        let live: Vec<usize> = (0..5).collect();
        let mut picks = vec![Some(0); 5];
        consensus_pick(&sites, &live, &mut picks);
        for (i, p) in picks.iter().take(4).enumerate() {
            assert_eq!(*p, Some(0), "site {i} left the shared offset");
        }
        // Whatever it picks, it must be the better of the two on its own
        // merits — never the weaker one dragged in by an unbounded penalty.
        assert_eq!(picks[4], Some(0), "disagreeing site inverted its ranking");
    }

    /// End to end over eight sites with five candidates each: 5⁸ = 390 625
    /// combinations, far past [`MAX_COMBINATIONS`], so selection must take the
    /// consensus branch and still land every site on its true dot.
    #[test]
    fn eight_sites_exceed_the_combination_cap_and_still_resolve() {
        let bed = BedMap::uniform_scale(PX_PER_MM);
        let expected: Vec<Point2<f64>> = (0..8)
            .map(|i| Point2::new(10.0 + 20.0 * (i % 4) as f64, 10.0 + 20.0 * (i / 4) as f64))
            .collect();
        // Board offset shared by every true dot, plus four decoys per site on a
        // 28 px circle, rotated per site so no decoy offset is shared by all.
        let (dx, dy) = (0.9, -0.7);
        let mut dots = Vec::new();
        let mut truth = Vec::new();
        for (i, e) in expected.iter().enumerate() {
            let p = bed.mm_to_px(Point2::new(e.x + dx, e.y + dy));
            truth.push((p.x, p.y));
            dots.push((p.x, p.y, 10.0));
            let c = bed.mm_to_px(*e);
            for k in 0..4 {
                let th = std::f64::consts::FRAC_PI_4 * ((3 * i + k) % 8) as f64;
                dots.push((c.x + 28.0 * th.cos(), c.y + 28.0 * th.sin(), 10.0));
            }
        }
        let frame = render(800, 400, &dots, 88.0, 5.0, 77);

        let found = find_fiducials(&frame, &expected, 3.0, &dark_1mm(), &bed);
        for (i, (res, &(tx, ty))) in found.iter().zip(&truth).enumerate() {
            let f = res
                .as_ref()
                .unwrap_or_else(|e| panic!("site {i} missed: {e:?}"));
            let err = (f.found_px.x - tx).hypot(f.found_px.y - ty);
            assert!(err < 3.0, "site {i} landed {err:.2} px from its true dot");
        }
    }

    // ---- The brushed-plate acceptance case (2026-07-26) -------------------
    //
    // `samples/fiducial/brushed-plate-4holes.png` is the operator's real bench
    // frame on brushed, scratched, specular aluminium. It is the fixture that
    // broke the previous detector: from their diagnostic log 95 of 168 checks
    // found fewer than 3 of the 4 holes. Sampled patches on this plate give
    // hole 15..173 mean 115.1 against nearby "clean" plate 6..176 mean 115.4 —
    // the mark and its background are statistically indistinguishable, so
    // nothing keyed to raw pixel statistics can work here.

    const BRUSHED_PPM: f64 = 11.157;

    /// Hole centres in `brushed-plate-4holes.png`, image pixels, in the order
    /// top-left, top-right, bottom-left, bottom-right. Located by taking the
    /// matched-filter peak in a ±45 px window around the operator's estimate
    /// and confirming each against the raw pixels; good to ~±3 px.
    const BRUSHED_TRUTH: [(f64, f64); 4] = [
        (978.0, 397.0),
        (1428.0, 424.0),
        (968.0, 832.0),
        (1444.0, 842.0),
    ];

    fn brushed_plate() -> GrayImage {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../samples/fiducial/brushed-plate-4holes.png"
        );
        image::open(path)
            .expect("brushed-plate fixture is committed")
            .to_luma8()
    }

    fn brushed_profile() -> FiducialProfile {
        FiducialProfile::DarkDot {
            shape: FidShape::Circle { diameter_mm: 2.0 },
        }
    }

    /// Drive the four sites with `offsets` of seed error and return the
    /// detections, insisting all four lock within 15 px of truth.
    fn brushed_locks_all_four(offsets: &[(f64, f64); 4], label: &str) -> Vec<Point2<f64>> {
        let frame = brushed_plate();
        let bed = BedMap::uniform_scale(BRUSHED_PPM);
        let expected: Vec<Point2<f64>> = BRUSHED_TRUTH
            .iter()
            .zip(offsets)
            .map(|(&(x, y), &(dx, dy))| bed.px_to_mm(Point2::new(x + dx, y + dy)))
            .collect();

        let found = find_fiducials(&frame, &expected, 2.0, &brushed_profile(), &bed);
        let mut out = Vec::new();
        for (i, (res, &(tx, ty))) in found.iter().zip(&BRUSHED_TRUTH).enumerate() {
            let f = res
                .as_ref()
                .unwrap_or_else(|e| panic!("{label}: site {i} missed: {e:?}"));
            let err = (f.found_px.x - tx).hypot(f.found_px.y - ty);
            assert!(
                err < 15.0,
                "{label}: site {i} landed {err:.1} px from truth at {:?} (score {:.2}, snr {:.2})",
                f.found_px,
                f.confidence.score,
                f.confidence.snr
            );
            // The operator's console buckets anything under SCORE_OK (0.25) as
            // a weak amber row. Four correct locks must not read as four weak
            // ones, so the composed score is part of the acceptance bar.
            assert!(
                f.confidence.score > 0.25,
                "{label}: site {i} scored {:.3} — would show as weak in the console",
                f.confidence.score
            );
            out.push(f.found_px);
        }
        out
    }

    /// All four holes lock with a realistic ~5 px of seed error.
    #[test]
    fn brushed_plate_locks_all_four_with_small_seed_error() {
        brushed_locks_all_four(&[(5.0, -3.0), (-4.0, 4.0), (3.0, 5.0), (-5.0, -2.0)], "5px");
    }

    /// And with ~20 px of seed error, which at 2 mm of search (22.3 px) leaves
    /// barely 2 px of margin — the true mark is only just inside the window,
    /// and several scratches are nearer the seed than it is. This is the case
    /// `min_by(dist)` could not survive.
    #[test]
    fn brushed_plate_locks_all_four_with_large_seed_error() {
        brushed_locks_all_four(
            &[(18.0, -9.0), (-14.0, 15.0), (12.0, 17.0), (-20.0, -6.0)],
            "20px",
        );
    }

    /// The detected quad's geometry. The four holes are the corners of a
    /// 40 × 40 mm square on the plate, but they are NOT a square in the image:
    /// the plate is tilted relative to the camera, exactly as
    /// `samples/fiducial/README.md` records for the older bench fixture ("the
    /// observed quad is genuinely perspective-warped and no similarity fits
    /// it"). Measured on the ground-truth centres: sides 450.8 / 476.1 / 435.1
    /// / 418.3 px (13.8% spread about the 446.3 px nominal) and diagonals
    /// 644.3 / 614.9 px (4.8% apart). So this asserts what is true of the bench
    /// — a coherent quad at the right scale — and deliberately does not assert
    /// equal sides, which would be a false claim about this fixture.
    #[test]
    fn brushed_plate_quad_is_geometrically_coherent() {
        let p = brushed_locks_all_four(&[(5.0, -3.0), (-4.0, 4.0), (3.0, 5.0), (-5.0, -2.0)], "geom");
        let d = |a: Point2<f64>, b: Point2<f64>| (a.x - b.x).hypot(a.y - b.y);
        let sides = [d(p[0], p[1]), d(p[2], p[3]), d(p[0], p[2]), d(p[1], p[3])];
        let diags = [d(p[0], p[3]), d(p[1], p[2])];
        let nominal = 40.0 * BRUSHED_PPM;

        let (lo, hi) = sides.iter().fold((f64::MAX, 0.0f64), |(l, h), &s| {
            (l.min(s), h.max(s))
        });
        assert!(
            hi / lo < 1.20,
            "sides {sides:?} spread beyond the plate's known perspective warp"
        );
        for s in sides {
            assert!(
                (s - nominal).abs() / nominal < 0.10,
                "side {s:.1} px is not near the 40 mm nominal {nominal:.1} px ({sides:?})"
            );
        }
        let dl = diags[0].max(diags[1]) / diags[0].min(diags[1]);
        assert!(dl < 1.08, "diagonals {diags:?} differ by {:.1}%", (dl - 1.0) * 100.0);
    }

    /// A window centred on the long dark brush band above the top-left hole
    /// refuses rather than fabricates. This is the detector's worst case on
    /// this plate and it is recorded as a test because it is a real limit, not
    /// a hypothetical: seeded 22 px up into the band, the band's own texture
    /// inflates the response noise floor enough that even the true hole (which
    /// is still just inside the search window) reads only 2.31 and the site
    /// reports `LowContrast`. That is the safe failure — the operator gets a
    /// miss naming the SNR, per the VIS-4 "low contrast is a lighting problem"
    /// rule, and not a confident lock on a scratch. Seeded anywhere sane, the
    /// same hole reads 3.42–3.75 and locks (see the two acceptance tests).
    #[test]
    fn brushed_plate_band_seeded_site_refuses_rather_than_locks_on_texture() {
        let frame = brushed_plate();
        let bed = BedMap::uniform_scale(BRUSHED_PPM);
        let expected = [bed.px_to_mm(Point2::new(978.0, 375.0))];
        match find_fiducials(&frame, &expected, 2.0, &brushed_profile(), &bed).remove(0) {
            Err(Miss::LowContrast { snr }) => assert!(snr < MIN_RESPONSE_SNR, "snr {snr}"),
            Err(other) => panic!("expected LowContrast, got {other:?}"),
            Ok(f) => {
                // Locking is acceptable only if it locked on the HOLE.
                let (tx, ty) = BRUSHED_TRUTH[0];
                let err = (f.found_px.x - tx).hypot(f.found_px.y - ty);
                assert!(err < 15.0, "locked on brush band at {:?}", f.found_px);
            }
        }
    }

    /// A site seeded on nothing but scratches does not pass as a confident
    /// fiducial. It reports snr 3.11 / score 0.185, under the console's
    /// `SCORE_OK` of 0.25, where all four real holes score 0.34 to 0.83.
    ///
    /// This asserts a weak detection rather than an outright miss, and that is
    /// deliberate. The response gate sits at 3.0 because the WEAKEST true hole
    /// on this plate reads 3.42 in its own search window — a gate placed to
    /// exclude this scratch would sit inside that 10% margin and would be tuned
    /// to one frame's noise. The composed score separates them by a factor of
    /// 1.8, which is the margin worth trusting, and the console already buckets
    /// on it. Suppressing weak detections in the detector instead would take
    /// away the amber row the operator is meant to see and judge.
    #[test]
    fn brushed_plate_scratch_does_not_pass_as_a_confident_fiducial() {
        let frame = brushed_plate();
        let bed = BedMap::uniform_scale(BRUSHED_PPM);
        // ~57 px above the top-left hole: outside its 22.3 px search window, so
        // the only thing on offer in this window is brush texture.
        let expected = [bed.px_to_mm(Point2::new(978.0, 340.0))];
        match find_fiducials(&frame, &expected, 2.0, &brushed_profile(), &bed).remove(0) {
            Err(_) => {}
            Ok(f) => assert!(
                f.confidence.score < 0.25,
                "a scratch scored {:.3} — the console would show it as a strong lock at {:?}",
                f.confidence.score,
                f.found_px
            ),
        }
    }
}
