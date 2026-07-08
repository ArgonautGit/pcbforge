//! WS-VIS — vision & calibration.
//!
//! Vision-side geometry is `f64` millimeters: camera observations arrive as
//! sub-pixel centroids and never round-trip through the integer-nanometer
//! board representation until a correction is applied.

mod affine;

pub use affine::{AffineError, AffineFit, fit_affine};
