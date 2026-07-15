//! Camera→laser calibration: learn where the laser's *commanded* coordinates
//! land in the camera image, so a placement in the camera view can be turned
//! into machine coordinates the laser actually burns at.
//!
//! Fiducials tie the design to the board; they do NOT tie the camera to the
//! laser. That second link is what makes "place it here → burn it here" true,
//! and this module measures it: the operator burns a grid of dots at known
//! commanded coordinates, images it, and we fit a **camera-px → commanded-mm**
//! homography (perspective, so a tilted camera is absorbed).
//!
//! Flow: an initial homography from the four hand-marked corner dots predicts
//! every dot's pixel position; [`vision::find_fiducials`] refines each locally;
//! the full set is re-fit for the final, accurate transform. Because the
//! operator's camera moves between sessions, the fit is cheap to redo and the
//! console flags a stale calibration.

use image::GrayImage;
use nalgebra::{Matrix3, Point2};
use vision::{
    BedMap, FiducialProfile, Homography, LensMap, find_fiducials, fit_homography, fit_lens,
};

/// The commanded dot grid the operator burns: an `n×n` lattice at `pitch_mm`
/// starting from `origin_mm` (the lower-left dot), in machine mm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridSpec {
    pub origin_mm: (f64, f64),
    pub pitch_mm: f64,
    pub n: usize,
}

impl GridSpec {
    /// All commanded dot centers, row-major (x fastest), machine mm.
    pub fn points(&self) -> Vec<(f64, f64)> {
        let mut v = Vec::with_capacity(self.n * self.n);
        for row in 0..self.n {
            for col in 0..self.n {
                v.push((
                    self.origin_mm.0 + col as f64 * self.pitch_mm,
                    self.origin_mm.1 + row as f64 * self.pitch_mm,
                ));
            }
        }
        v
    }

    /// The four corner dots in commanded mm, ordered lower-left, lower-right,
    /// upper-right, upper-left — the order the operator clicks them.
    pub fn corners_mm(&self) -> [(f64, f64); 4] {
        let m = (self.n.saturating_sub(1)) as f64 * self.pitch_mm;
        let (ox, oy) = self.origin_mm;
        [(ox, oy), (ox + m, oy), (ox + m, oy + m), (ox, oy + m)]
    }
}

/// A fitted camera→laser calibration.
#[derive(Debug, Clone)]
pub struct Calibration {
    /// Camera pixel → commanded machine mm.
    pub px_to_mm: Homography,
    /// Fit residual RMS, µm (in the commanded-mm frame).
    pub rms_um: f64,
    /// Dots detected vs commanded.
    pub found: usize,
    pub total: usize,
}

/// A detected dot as `(found_px, grid_mm)`.
type DotPair = (Point2<f64>, Point2<f64>);

/// Detect every grid dot: `mm_to_px_seed` (grid-mm → px) places the local
/// search windows, `find_fiducials` refines each. Returns the pairs for the
/// dots that locked, plus the commanded total.
fn detect_grid_dots(
    frame: &GrayImage,
    mm_to_px_seed: &Homography,
    grid: &GridSpec,
    dot_mm: f64,
) -> Result<(Vec<DotPair>, usize), String> {
    let mm_from_px: Matrix3<f64> = mm_to_px_seed
        .matrix
        .try_inverse()
        .ok_or("seed homography is singular")?;
    let bed = BedMap::new(mm_from_px).ok_or("seed bed map is singular")?;
    let commanded = grid.points();
    let expected: Vec<Point2<f64>> = commanded.iter().map(|&(x, y)| Point2::new(x, y)).collect();
    let profile = FiducialProfile::DarkDot {
        diameter_mm: dot_mm,
    };
    // ~0.4·pitch so windows don't overlap, but at least the dot size. This also
    // bounds how far the camera may drift between re-anchors and still lock on.
    let search_mm = (grid.pitch_mm * 0.4).max(dot_mm);
    let results = find_fiducials(frame, &expected, search_mm, &profile, &bed);
    let pairs: Vec<(Point2<f64>, Point2<f64>)> = commanded
        .iter()
        .zip(&results)
        .filter_map(|(&(mx, my), r)| r.as_ref().ok().map(|f| (f.found_px, Point2::new(mx, my))))
        .collect();
    Ok((pairs, commanded.len()))
}

/// Corner homography (grid-mm → px) from the four hand-marked corner dots.
fn corner_seed(corners_px: [(f64, f64); 4], grid: &GridSpec) -> Result<Homography, String> {
    let pairs: Vec<(Point2<f64>, Point2<f64>)> = grid
        .corners_mm()
        .iter()
        .zip(corners_px.iter())
        .map(|(&(mx, my), &(px, py))| (Point2::new(mx, my), Point2::new(px, py)))
        .collect();
    fit_homography(&pairs).map_err(|e| format!("corner fit: {e}"))
}

/// Detect the grid dots via a seed and fit the final camera-px → commanded-mm
/// **homography** (the laser anchor). The seed is the corner homography for a
/// fresh fit, or the previous calibration for a re-anchor.
fn refit(
    frame: &GrayImage,
    mm_to_px_seed: &Homography,
    grid: &GridSpec,
    dot_mm: f64,
) -> Result<Calibration, String> {
    let (pairs, total) = detect_grid_dots(frame, mm_to_px_seed, grid, dot_mm)?;
    let found = pairs.len();
    if found < 4 {
        return Err(format!(
            "only {found}/{total} grid dots detected — need ≥4 (check the frame, \
             the seed, and the dot size; a big camera move needs a fresh corner fit)"
        ));
    }
    let px_to_mm = fit_homography(&pairs).map_err(|e| format!("grid fit: {e}"))?;
    let rms_um = px_to_mm.rms * 1000.0;
    Ok(Calibration {
        px_to_mm,
        rms_um,
        found,
        total,
    })
}

// ---- camera lens distortion (printed grid) --------------------------------

/// One dot's calibration feedback for the overlay: where it was detected, the
/// lens **distortion** it exhibits (detected − perspective-predicted, px), and
/// how well the polynomial fit corrected it (µm).
#[derive(Debug, Clone, Copy)]
pub struct LensDot {
    pub px: (f64, f64),
    /// Detected px minus where a pure-perspective (homography) model predicts —
    /// i.e. the lens distortion at this dot, in pixels. Drawn as an arrow.
    pub distort_px: (f64, f64),
    /// Post-fit residual of the polynomial lens map at this dot, µm.
    pub resid_um: f64,
}

/// The camera lens calibration: the metric map plus per-dot feedback.
#[derive(Debug, Clone)]
pub struct CameraCal {
    pub lens: LensMap,
    pub dots: Vec<LensDot>,
    pub found: usize,
    pub total: usize,
}

/// Fit the camera lens-distortion map from a frame of the **printed** reference
/// grid (known `grid` geometry) and the four hand-marked corner dots. Also
/// computes the per-dot distortion field for visual feedback.
pub fn fit_camera_lens(
    frame: &GrayImage,
    corners_px: [(f64, f64); 4],
    grid: &GridSpec,
    dot_mm: f64,
) -> Result<CameraCal, String> {
    let seed = corner_seed(corners_px, grid)?;
    let (pairs, total) = detect_grid_dots(frame, &seed, grid, dot_mm)?;
    let found = pairs.len();
    if found < 10 {
        return Err(format!(
            "only {found}/{total} grid dots detected — the lens fit needs ≥10 \
             (check the printed grid, the corner clicks, and the dot size)"
        ));
    }
    // Perspective-only model over the same points, to show the distortion the
    // polynomial had to absorb (grid-mm → px).
    let persp = fit_homography(&pairs.iter().map(|&(px, mm)| (mm, px)).collect::<Vec<_>>())
        .map_err(|e| format!("perspective ref: {e}"))?;
    let lens = fit_lens(&pairs).map_err(|e| format!("lens fit: {e}"))?;

    let dots: Vec<LensDot> = pairs
        .iter()
        .zip(&lens.residuals)
        .map(|(&(px, mm), &(_, _, resid_um))| {
            let pred = persp.apply(mm);
            LensDot {
                px: (px.x, px.y),
                distort_px: (px.x - pred.x, px.y - pred.y),
                resid_um,
            }
        })
        .collect();
    Ok(CameraCal {
        lens,
        dots,
        found,
        total,
    })
}

/// Fit the camera→laser homography from a frame of the burned grid and the
/// four hand-marked corner-dot pixel positions (same order as
/// [`GridSpec::corners_mm`]). `dot_mm` sizes the dark-dot detector.
pub fn fit_camera_to_machine(
    frame: &GrayImage,
    corners_px: [(f64, f64); 4],
    grid: &GridSpec,
    dot_mm: f64,
) -> Result<Calibration, String> {
    if grid.n < 2 {
        return Err("grid must be at least 2×2".into());
    }
    // Initial commanded-mm → px homography from the four corners, then refine
    // every dot and re-fit.
    let corner_pairs: Vec<(Point2<f64>, Point2<f64>)> = grid
        .corners_mm()
        .iter()
        .zip(corners_px.iter())
        .map(|(&(mx, my), &(px, py))| (Point2::new(mx, my), Point2::new(px, py)))
        .collect();
    let seed = fit_homography(&corner_pairs).map_err(|e| format!("corner fit: {e}"))?;
    refit(frame, &seed, grid, dot_mm)
}

/// Re-anchor an existing calibration to a fresh frame — no corner clicks. Uses
/// the previous calibration to place the search windows, so as long as the
/// burned grid is still in view and the camera hasn't jumped more than ~0.4·
/// pitch, it re-locks the dots and re-fits, absorbing camera drift. A bigger
/// move fails and needs a fresh corner fit ([`fit_camera_to_machine`]).
pub fn re_anchor(
    frame: &GrayImage,
    previous: &Calibration,
    grid: &GridSpec,
    dot_mm: f64,
) -> Result<Calibration, String> {
    let mm_to_px = Homography {
        matrix: previous
            .px_to_mm
            .matrix
            .try_inverse()
            .ok_or("calibration is singular")?,
        residuals: Vec::new(),
        rms: 0.0,
    };
    refit(frame, &mm_to_px, grid, dot_mm)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render the grid as anti-aliased dark discs on a bright field, through
    /// `mm_to_px` (commanded-mm → px) with dots ~`dot_mm`·10 px across.
    fn render_grid(
        grid: &GridSpec,
        mm_to_px: &Homography,
        dot_mm: f64,
        w: u32,
        h: u32,
    ) -> GrayImage {
        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(mx, my)| {
                let p = mm_to_px.apply(Point2::new(mx, my));
                (p.x, p.y, dot_mm * 10.0)
            })
            .collect();
        GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        })
    }

    /// (commanded-mm, pixel) corner pair.
    type MmPx = ((f64, f64), (f64, f64));

    fn homog(pairs: &[MmPx]) -> Homography {
        let p: Vec<_> = pairs
            .iter()
            .map(|&((mx, my), (px, py))| (Point2::new(mx, my), Point2::new(px, py)))
            .collect();
        fit_homography(&p).unwrap()
    }

    #[test]
    fn grid_points_and_corners() {
        let g = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let pts = g.points();
        assert_eq!(pts.len(), 49);
        assert_eq!(pts[0], (0.0, 0.0));
        assert_eq!(pts[6], (60.0, 0.0)); // end of first row
        assert_eq!(*pts.last().unwrap(), (60.0, 60.0));
        assert_eq!(
            g.corners_mm(),
            [(0.0, 0.0), (60.0, 0.0), (60.0, 60.0), (0.0, 60.0)]
        );
    }

    /// Render a grid of dark dots through a known perspective homography
    /// (commanded-mm → px), then confirm the fit recovers commanded coords
    /// from pixels to sub-pixel-equivalent accuracy.
    #[test]
    fn recovers_commanded_coordinates_from_a_burned_grid() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        // A mild keystone: commanded (0..60, 0..60) mm imaged into a ~600px
        // frame, top edge slightly narrower than the bottom (tilted camera).
        let corr = [
            ((0.0, 0.0), (60.0, 540.0)),
            ((60.0, 0.0), (560.0, 540.0)),
            ((60.0, 60.0), (520.0, 60.0)),
            ((0.0, 60.0), (100.0, 60.0)),
        ];
        let pairs: Vec<(Point2<f64>, Point2<f64>)> = corr
            .iter()
            .map(|&((mx, my), (px, py))| (Point2::new(mx, my), Point2::new(px, py)))
            .collect();
        let mm_to_px = fit_homography(&pairs).unwrap();
        let dot_mm = 1.5;

        // Render the 49 dots as anti-aliased dark discs on a bright field.
        let (w, h) = (620u32, 620u32);
        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(mx, my)| {
                let p = mm_to_px.apply(Point2::new(mx, my));
                // dot radius in px ≈ dot_mm * local scale (~10 px/mm here).
                (p.x, p.y, dot_mm * 10.0)
            })
            .collect();
        let img = GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        });

        // Corner clicks = the true corner-dot pixels (what the operator marks).
        let corners_px = grid.corners_mm().map(|(mx, my)| {
            let p = mm_to_px.apply(Point2::new(mx, my));
            (p.x, p.y)
        });

        let cal = fit_camera_to_machine(&img, corners_px, &grid, dot_mm).expect("fit");
        assert!(
            cal.found >= 45,
            "detected most dots: {}/{}",
            cal.found,
            cal.total
        );
        assert!(cal.rms_um < 200.0, "tight fit: {} µm", cal.rms_um);

        // A pixel we didn't feed in: the center dot (commanded (30,30)) maps
        // back to ~(30,30) mm.
        let center_px = mm_to_px.apply(Point2::new(30.0, 30.0));
        let back = cal.px_to_mm.apply(center_px);
        assert!(
            (back.x - 30.0).abs() < 0.2 && (back.y - 30.0).abs() < 0.2,
            "center maps to ~(30,30): ({:.3},{:.3})",
            back.x,
            back.y
        );
    }

    /// Camera drift: fit at pose A, then the camera shifts ~2 mm (the grid
    /// moves in the image); re-anchoring from the pose-A calibration re-locks
    /// the dots and recovers correct commanded coordinates — no corner clicks.
    #[test]
    fn re_anchor_absorbs_a_camera_shift() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        // Pose A: 10 px/mm, grid at image (30..630, 30..630).
        let a = homog(&[
            ((0.0, 0.0), (30.0, 30.0)),
            ((60.0, 0.0), (630.0, 30.0)),
            ((60.0, 60.0), (630.0, 630.0)),
            ((0.0, 60.0), (30.0, 630.0)),
        ]);
        let img_a = render_grid(&grid, &a, dot_mm, 680, 680);
        let corners_a = grid.corners_mm().map(|(mx, my)| {
            (
                a.apply(Point2::new(mx, my)).x,
                a.apply(Point2::new(mx, my)).y,
            )
        });
        let cal_a = fit_camera_to_machine(&img_a, corners_a, &grid, dot_mm).expect("fit A");

        // Pose B: the camera shifted, so the grid sits ~20 px (2 mm) right and
        // ~12 px down — well within the ~4 mm search window.
        let b = homog(&[
            ((0.0, 0.0), (50.0, 42.0)),
            ((60.0, 0.0), (650.0, 42.0)),
            ((60.0, 60.0), (650.0, 642.0)),
            ((0.0, 60.0), (50.0, 642.0)),
        ]);
        let img_b = render_grid(&grid, &b, dot_mm, 680, 680);

        // Re-anchor from cal_A onto frame B — no corner clicks.
        let cal_b = re_anchor(&img_b, &cal_a, &grid, dot_mm).expect("re-anchor B");
        assert!(cal_b.found >= 45, "re-locked most dots: {}", cal_b.found);
        assert!(cal_b.rms_um < 200.0, "tight re-fit: {} µm", cal_b.rms_um);

        // The re-anchored map reads the new frame correctly: dot (30,30) mm,
        // now at pose-B pixels, maps back to ~(30,30).
        let c = b.apply(Point2::new(30.0, 30.0));
        let back = cal_b.px_to_mm.apply(c);
        assert!(
            (back.x - 30.0).abs() < 0.3 && (back.y - 30.0).abs() < 0.3,
            "re-anchored center ~(30,30): ({:.3},{:.3})",
            back.x,
            back.y
        );
        // And the STALE pose-A calibration would have been wrong on frame B:
        let stale = cal_a.px_to_mm.apply(c);
        assert!(
            (stale.x - 30.0).abs() > 1.0 || (stale.y - 30.0).abs() > 1.0,
            "stale calibration is visibly off ({:.2},{:.2}) — re-anchor was needed",
            stale.x,
            stale.y
        );
    }

    /// The camera lens fit locks the printed grid, corrects the barrel
    /// distortion to a low residual, and reports a non-trivial distortion
    /// field (arrows) for the operator to see.
    #[test]
    fn camera_lens_fit_corrects_barrel_and_reports_distortion() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        let dot_mm = 1.5;
        // Perspective ~10 px/mm plus 4% barrel about the image center.
        let base = homog(&[
            ((0.0, 0.0), (40.0, 40.0)),
            ((60.0, 0.0), (640.0, 40.0)),
            ((60.0, 60.0), (640.0, 640.0)),
            ((0.0, 60.0), (40.0, 640.0)),
        ]);
        let distort = |p: Point2<f64>| {
            let (cx, cy) = (340.0, 340.0);
            let (du, dv) = (p.x - cx, p.y - cy);
            let r2 = (du * du + dv * dv) / (340.0 * 340.0);
            let k = 0.04;
            Point2::new(cx + du * (1.0 + k * r2), cy + dv * (1.0 + k * r2))
        };
        let centers: Vec<(f64, f64, f64)> = grid
            .points()
            .iter()
            .map(|&(mx, my)| {
                let p = distort(base.apply(Point2::new(mx, my)));
                (p.x, p.y, dot_mm * 10.0)
            })
            .collect();
        let (w, h) = (700u32, 700u32);
        let img = GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if centers.iter().any(|&(cx, cy, r)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < r / 2.0
                    }) {
                        cover += 1.0 / 16.0;
                    }
                }
            }
            image::Luma([(210.0 - 150.0 * cover) as u8])
        });
        let corners_px = grid.corners_mm().map(|(mx, my)| {
            let p = distort(base.apply(Point2::new(mx, my)));
            (p.x, p.y)
        });

        let cal = fit_camera_lens(&img, corners_px, &grid, dot_mm).expect("lens fit");
        assert!(cal.found >= 45, "locked most dots: {}", cal.found);
        assert!(
            cal.lens.rms_um < 60.0,
            "corrected RMS {} µm",
            cal.lens.rms_um
        );
        // The distortion field is real: at least one corner dot deviates several
        // px from the perspective model (that's the barrel we corrected).
        let max_distort = cal
            .dots
            .iter()
            .map(|d| (d.distort_px.0.powi(2) + d.distort_px.1.powi(2)).sqrt())
            .fold(0.0, f64::max);
        assert!(
            max_distort > 3.0,
            "shows the barrel field: {max_distort:.1} px"
        );
    }

    #[test]
    fn too_few_dots_is_an_error() {
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 3,
        };
        // A blank frame → nothing detected.
        let img = GrayImage::from_pixel(200, 200, image::Luma([200]));
        let corners = [(10.0, 10.0), (190.0, 10.0), (190.0, 190.0), (10.0, 190.0)];
        assert!(fit_camera_to_machine(&img, corners, &grid, 1.0).is_err());
    }

    /// End-to-end gate: load the committed distorted-grid fixture (a real PNG
    /// rendered by the `gen_distorted_grid` example — perspective + 5% barrel)
    /// and confirm the camera-lens calibration recovers all 49 dots to a tight
    /// RMS. This proves the calibration works on an on-disk image, not just an
    /// in-memory render.
    #[test]
    fn calibrates_from_the_distorted_grid_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../samples/calibration/grid-7x7-10mm-distorted.png"
        );
        let img = image::open(path)
            .expect("distorted-grid fixture present")
            .to_luma8();
        let grid = GridSpec {
            origin_mm: (0.0, 0.0),
            pitch_mm: 10.0,
            n: 7,
        };
        // The four corner dots (lower-left, lower-right, upper-right,
        // upper-left) as recorded in the fixture's JSON sidecar.
        let corners = [
            (42.506, 632.64),
            (620.163, 618.878),
            (606.277, 39.892),
            (43.744, 41.83),
        ];
        let cal = fit_camera_lens(&img, corners, &grid, 2.0).expect("calibrate fixture");
        assert_eq!(cal.found, 49, "all dots detected");
        assert!(
            cal.lens.rms_um < 60.0,
            "recovered RMS {:.1} µm too high",
            cal.lens.rms_um
        );
        // The raw barrel the fit had to absorb is well above its residual —
        // proving the fixture carries distortion a homography couldn't model.
        let max_distort = cal
            .dots
            .iter()
            .map(|d| (d.distort_px.0.powi(2) + d.distort_px.1.powi(2)).sqrt())
            .fold(0.0_f64, f64::max);
        assert!(
            max_distort > 5.0,
            "fixture should carry real distortion: {max_distort:.1} px"
        );
    }
}
