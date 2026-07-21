//! VIS-10 — board-frame warper: `to_board_frame`.
//!
//! Resamples an undistorted camera frame into the **design frame**: the flat,
//! axis-aligned raster the CAM geometry lives in, at a chosen µm/px. Every
//! output pixel is an even grid point in board millimeters; the warp gathers
//! the source pixel that imaged that board point, so a feature at a known
//! board coordinate lands at its predicted raster position regardless of how
//! the board sits on the bed.
//!
//! Two calibrated maps compose to place each output pixel (design frame →
//! camera pixels):
//!
//! 1. the **board affine** (VIS-5/VIS-6 registration) — design-mm → bed-mm,
//!    i.e. where the board actually sits on the machine;
//! 2. the **bed map** ([`BedMap`], VIS-3 homography) — bed-mm → undistorted
//!    camera pixels.
//!
//! The warp is a *gather* (inverse map + bilinear sample), so it has no holes:
//! for each design-frame output pixel we walk design-mm → bed-mm → source px
//! and sample. Pixels whose source falls outside the frame read as 0 (black).
//!
//! Axis convention: the design-frame raster is y-down like every other image
//! here — output pixel `(col, row)` is design-mm `(col·mpp, row·mpp)` — so
//! [`board_mm_to_raster`] is the exact inverse a caller uses to predict where
//! a board coordinate should appear. (If a y-up design raster is ever wanted,
//! flip the board affine's y, not this module.)

use image::GrayImage;
use nalgebra::{Matrix3, Point2};

use crate::fiducial::BedMap;

/// Predicted raster position (output pixels) of a design-frame coordinate
/// `board_mm`, at `um_per_px`. The exact inverse of the grid [`to_board_frame`]
/// samples on, so a caller can check "did the feature land where I expected?".
pub fn board_mm_to_raster(board_mm: Point2<f64>, um_per_px: f64) -> Point2<f64> {
    let mpp = um_per_px / 1000.0;
    Point2::new(board_mm.x / mpp, board_mm.y / mpp)
}

/// Inverse: the design-mm coordinate imaged at output pixel `(col, row)`.
fn raster_to_board_mm(col: f64, row: f64, um_per_px: f64) -> Point2<f64> {
    let mpp = um_per_px / 1000.0;
    Point2::new(col * mpp, row * mpp)
}

/// Warp `frame` into the design frame.
///
/// `board_affine` maps design-mm → bed-mm (registration output); `bed` maps
/// bed-mm → camera pixels. `um_per_px` sets the output resolution, and
/// `out_w`/`out_h` its size in pixels (typically `ceil(board_size_mm /
/// mm_per_px)`). Bilinear sampling; out-of-frame samples are black.
pub fn to_board_frame(
    frame: &GrayImage,
    bed: &BedMap,
    board_affine: &Matrix3<f64>,
    um_per_px: f64,
    out_w: u32,
    out_h: u32,
) -> GrayImage {
    let (fw, fh) = (frame.width(), frame.height());
    GrayImage::from_fn(out_w, out_h, |col, row| {
        // Output pixel centers at integer coordinates (OpenCV convention,
        // shared with BedMap): pixel (col,row) samples design point
        // ((col)·mpp, (row)·mpp).
        let board_mm = raster_to_board_mm(col as f64, row as f64, um_per_px);
        let bed_mm = board_affine.transform_point(&board_mm);
        let src = bed.mm_to_px(bed_mm);
        image::Luma([bilinear(frame, src.x, src.y, fw, fh)])
    })
}

/// Bilinear sample of `frame` at `(x, y)` in pixel coordinates (integer
/// coordinates = pixel centers). Returns 0 when the sample falls outside the
/// frame — the four-tap footprint must lie fully inside.
fn bilinear(frame: &GrayImage, x: f64, y: f64, fw: u32, fh: u32) -> u8 {
    if x < 0.0 || y < 0.0 {
        return 0;
    }
    let x0 = x.floor();
    let y0 = y.floor();
    let (ix, iy) = (x0 as u32, y0 as u32);
    if ix + 1 >= fw || iy + 1 >= fh {
        return 0;
    }
    let fx = x - x0;
    let fy = y - y0;
    let p = |xx: u32, yy: u32| frame.get_pixel(xx, yy)[0] as f64;
    let top = p(ix, iy) * (1.0 - fx) + p(ix + 1, iy) * fx;
    let bot = p(ix, iy + 1) * (1.0 - fx) + p(ix + 1, iy + 1) * fx;
    (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fiducial::{FidShape, FiducialProfile, find_fiducials};

    /// A source frame with a single anti-aliased bright disc at `(cx, cy)` px.
    fn frame_with_dot(w: u32, h: u32, cx: f64, cy: f64, d: f64) -> GrayImage {
        GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < d / 2.0 {
                        cover += 1.0_f64 / 16.0;
                    }
                }
            }
            image::Luma([(30.0 + 200.0 * cover).clamp(0.0, 255.0) as u8])
        })
    }

    /// A board sitting rotated + scaled + offset on the bed: design-mm →
    /// bed-mm. Representative registration output.
    fn board_affine(theta: f64, sx: f64, sy: f64, tx: f64, ty: f64) -> Matrix3<f64> {
        let (s, c) = theta.sin_cos();
        Matrix3::new(sx * c, -sy * s, tx, sx * s, sy * c, ty, 0.0, 0.0, 1.0)
    }

    /// VIS-10 done-when (synthetic mirror of the live test): a feature at a
    /// known board coordinate must land within 2 px of its predicted raster
    /// position in the warped image. Here the feature is imaged through a
    /// realistic bed map + board affine, warped back, and its recovered
    /// position compared to `board_mm_to_raster`.
    #[test]
    fn feature_lands_at_predicted_raster_position() {
        // Bed map: 12 px/mm, slightly rotated camera, offset origin.
        let px_per_mm = 12.0;
        let (s, c) = 0.03_f64.sin_cos();
        let px_from_mm = Matrix3::new(
            px_per_mm * c,
            -px_per_mm * s,
            40.0,
            px_per_mm * s,
            px_per_mm * c,
            25.0,
            0.0,
            0.0,
            1.0,
        );
        let bed = BedMap::new(px_from_mm.try_inverse().unwrap()).unwrap();
        let affine = board_affine(0.08, 1.002, 0.997, 7.5, -4.0);

        // Known board coordinate (design mm) and where it images on the bed.
        let board_pt = Point2::new(18.0, 11.0);
        let bed_mm = affine.transform_point(&board_pt);
        let src_px = bed.mm_to_px(bed_mm);
        let frame = frame_with_dot(640, 480, src_px.x, src_px.y, 10.0);

        let um_per_px = 50.0; // 20 px/mm design raster
        let warped = to_board_frame(&frame, &bed, &affine, um_per_px, 800, 600);

        // Expected raster position of the board coordinate.
        let expected = board_mm_to_raster(board_pt, um_per_px);

        // Recover the dot in the warped image with the fiducial detector,
        // using a design-frame BedMap (raster px ↔ design mm at this mpp).
        let design_bed = BedMap::uniform_scale(1000.0 / um_per_px);
        let found = find_fiducials(
            &warped,
            &[board_pt],
            3.0,
            &FiducialProfile::Backlit {
                shape: FidShape::Circle { diameter_mm: 0.5 },
            },
            &design_bed,
        )
        .remove(0)
        .expect("dot recovered in warped frame");

        let err = ((found.found_px.x - expected.x).powi(2)
            + (found.found_px.y - expected.y).powi(2))
        .sqrt();
        assert!(err < 2.0, "warped feature off by {err:.2} px (want < 2)");
    }

    /// Warping through the identity bed map + identity board affine at
    /// 1000 µm/px (1 px/mm) with a 1 px/mm source is a straight copy: a dot
    /// at bed-mm (x,y) stays at raster (x,y).
    #[test]
    fn identity_calibration_is_a_copy() {
        let bed = BedMap::uniform_scale(1.0); // 1 px/mm
        let affine = Matrix3::identity();
        let frame = frame_with_dot(200, 200, 60.0, 40.0, 8.0);
        let warped = to_board_frame(&frame, &bed, &affine, 1000.0, 200, 200);

        // Brightest pixel unchanged in position.
        let peak = |img: &GrayImage| {
            let mut best = (0u32, 0u32, 0u8);
            for (x, y, p) in img.enumerate_pixels() {
                if p[0] > best.2 {
                    best = (x, y, p[0]);
                }
            }
            (best.0, best.1)
        };
        let (sx, sy) = peak(&frame);
        let (wx, wy) = peak(&warped);
        assert!(
            (sx as i32 - wx as i32).abs() <= 1 && (sy as i32 - wy as i32).abs() <= 1,
            "identity warp moved the peak: src ({sx},{sy}) warped ({wx},{wy})"
        );
    }

    /// Samples that map outside the source frame come back black, and the warp
    /// never panics for an output larger than the imaged region.
    #[test]
    fn out_of_frame_samples_are_black() {
        let bed = BedMap::uniform_scale(10.0);
        let affine = Matrix3::identity();
        let frame = frame_with_dot(100, 100, 50.0, 50.0, 6.0);
        // Output covers 0..40 mm at 100 µm/px = 400 px, but the 100×100 px /
        // 10 px-per-mm frame only images 0..10 mm — most output is off-frame.
        let warped = to_board_frame(&frame, &bed, &affine, 100.0, 400, 400);
        // Far corner (design ~40 mm) has no source: black.
        assert_eq!(warped.get_pixel(399, 399)[0], 0);
        // Near origin is imaged.
        assert!(warped.get_pixel(5, 5)[0] > 0);
    }
}
