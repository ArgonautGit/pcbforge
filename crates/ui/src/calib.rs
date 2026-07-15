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
use vision::{BedMap, FiducialProfile, Homography, find_fiducials, fit_homography};

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
    // 1. Initial commanded-mm → px homography from the four corners.
    let corner_pairs: Vec<(Point2<f64>, Point2<f64>)> = grid
        .corners_mm()
        .iter()
        .zip(corners_px.iter())
        .map(|(&(mx, my), &(px, py))| (Point2::new(mx, my), Point2::new(px, py)))
        .collect();
    let mm_to_px = fit_homography(&corner_pairs).map_err(|e| format!("corner fit: {e}"))?;

    // 2. Build a BedMap (mm↔px) from that initial fit and refine every dot
    //    locally with the dark-dot detector, searching ~0.4·pitch around each
    //    predicted spot (so windows don't overlap).
    let mm_from_px: Matrix3<f64> = mm_to_px
        .matrix
        .try_inverse()
        .ok_or("initial corner homography is singular")?;
    let bed = BedMap::new(mm_from_px).ok_or("initial bed map is singular")?;
    let commanded = grid.points();
    let expected: Vec<Point2<f64>> = commanded.iter().map(|&(x, y)| Point2::new(x, y)).collect();
    let profile = FiducialProfile::DarkDot {
        diameter_mm: dot_mm,
    };
    let search_mm = (grid.pitch_mm * 0.4).max(dot_mm);
    let results = find_fiducials(frame, &expected, search_mm, &profile, &bed);

    // 3. Collect (found_px, commanded_mm) and fit the final px → mm homography.
    let pairs: Vec<(Point2<f64>, Point2<f64>)> = commanded
        .iter()
        .zip(&results)
        .filter_map(|(&(mx, my), r)| r.as_ref().ok().map(|f| (f.found_px, Point2::new(mx, my))))
        .collect();
    let total = commanded.len();
    let found = pairs.len();
    if found < 4 {
        return Err(format!(
            "only {found}/{total} grid dots detected — need ≥4 (check the frame, \
             the corner clicks, and the dot size)"
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
