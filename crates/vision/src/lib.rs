//! WS-VIS — vision & calibration.
//!
//! Vision-side geometry is `f64` millimeters: camera observations arrive as
//! sub-pixel centroids and never round-trip through the integer-nanometer
//! board representation until a correction is applied.

mod affine;
mod fiducial;
mod homography;
mod lens;
mod warp;

pub use affine::{AffineError, AffineFit, fit_affine};
pub use fiducial::{BedMap, Confidence, FidShape, Fiducial, FiducialProfile, Miss, find_fiducials};
pub use homography::{Homography, HomographyError, fit_homography};
pub use lens::{
    FieldMap, FieldPattern, FieldVerdict, InconclusiveReason, LensMap, Poly2, classify_field_error,
    fit_field, fit_lens,
};
pub use warp::{board_mm_to_raster, to_board_frame};
