//! Least-squares 2D affine fit from point correspondences.
//!
//! Solves `dst ≈ A · src` for the six affine parameters by stacking the
//! correspondences into a `2N×6` design matrix and solving the normal-free
//! least-squares problem via SVD. Units are whatever the caller uses
//! (millimeters in practice — see `crates/core`: vision works in f64 mm).

use nalgebra::{DMatrix, DVector, Matrix3, Point2, SVD};

/// Result of a successful affine fit.
#[derive(Debug, Clone, PartialEq)]
pub struct AffineFit {
    /// Homogeneous 2D affine transform; last row is `[0, 0, 1]`.
    /// Maps source points onto destination points: `dst ≈ transform * src`.
    pub transform: Matrix3<f64>,
    /// Per-point Euclidean distance between `transform * src` and `dst`,
    /// in the caller's units, in input order.
    pub residuals: Vec<f64>,
    /// Root-mean-square of `residuals`.
    pub rms: f64,
}

/// Why an affine fit could not be computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AffineError {
    /// Fewer than three point pairs were supplied; the six affine
    /// parameters are underdetermined.
    TooFewPoints {
        /// Number of pairs supplied.
        got: usize,
    },
    /// The source points are collinear (or numerically indistinguishable
    /// from collinear), so the design matrix is rank-deficient and the
    /// affine parameters are not uniquely determined.
    DegenerateSources {
        /// Numerical rank of the `2N×6` design matrix (6 required).
        rank: usize,
    },
    /// The SVD solve itself failed (singular vectors unavailable).
    SolveFailed(&'static str),
    /// A source or destination coordinate was non-finite (NaN/∞). nalgebra's
    /// singular-value sort panics on NaN, and a NaN that survived would flow
    /// into machine coordinates, so this fails closed before the solve.
    NonFinite,
}

impl std::fmt::Display for AffineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewPoints { got } => write!(
                f,
                "affine fit needs at least 3 point pairs, got {got}: \
                 six parameters cannot be determined"
            ),
            Self::DegenerateSources { rank } => write!(
                f,
                "affine fit is degenerate: source points are collinear \
                 (design matrix rank {rank} < 6)"
            ),
            Self::SolveFailed(msg) => write!(f, "affine SVD solve failed: {msg}"),
            Self::NonFinite => write!(
                f,
                "affine fit input contains a non-finite (NaN/∞) coordinate"
            ),
        }
    }
}

impl std::error::Error for AffineError {}

/// Fit the least-squares affine transform mapping `pairs[i].0` (source)
/// onto `pairs[i].1` (destination).
///
/// Builds the `2N×6` design matrix
///
/// ```text
/// | x_i  y_i  1   0    0    0 |   | a |   | u_i |
/// | 0    0    0   x_i  y_i  1 | · | b | = | v_i |
///                                 | … |
/// ```
///
/// and solves for the parameter vector `[a b c d e f]ᵀ` via SVD, yielding
///
/// ```text
///             | a  b  c |
/// transform = | d  e  f |
///             | 0  0  1 |
/// ```
///
/// # Errors
///
/// - [`AffineError::TooFewPoints`] if fewer than 3 pairs are given.
/// - [`AffineError::DegenerateSources`] if the source points are collinear
///   (numerical rank of the design matrix below 6).
/// - [`AffineError::SolveFailed`] if the SVD back-substitution fails.
/// - [`AffineError::NonFinite`] if any coordinate is NaN or infinite.
pub fn fit_affine(pairs: &[(Point2<f64>, Point2<f64>)]) -> Result<AffineFit, AffineError> {
    let n = pairs.len();
    if n < 3 {
        return Err(AffineError::TooFewPoints { got: n });
    }
    if pairs.iter().any(|(src, dst)| {
        !src.x.is_finite() || !src.y.is_finite() || !dst.x.is_finite() || !dst.y.is_finite()
    }) {
        return Err(AffineError::NonFinite);
    }

    let mut design = DMatrix::<f64>::zeros(2 * n, 6);
    let mut rhs = DVector::<f64>::zeros(2 * n);
    for (i, (src, dst)) in pairs.iter().enumerate() {
        let r = 2 * i;
        design[(r, 0)] = src.x;
        design[(r, 1)] = src.y;
        design[(r, 2)] = 1.0;
        rhs[r] = dst.x;
        design[(r + 1, 3)] = src.x;
        design[(r + 1, 4)] = src.y;
        design[(r + 1, 5)] = 1.0;
        rhs[r + 1] = dst.y;
    }

    let svd = SVD::new(design, true, true);

    // Relative rank threshold: singular values below eps · σ_max count as
    // zero. 1e-10 sits far above f64 round-off from the decomposition yet
    // far below any σ-ratio produced by a usable fiducial layout.
    let sigma_max = svd.singular_values.max();
    let eps = sigma_max * 1e-10;
    let rank = svd.rank(eps);
    if rank < 6 {
        return Err(AffineError::DegenerateSources { rank });
    }

    let params = svd.solve(&rhs, eps).map_err(AffineError::SolveFailed)?;

    let transform = Matrix3::new(
        params[0], params[1], params[2], //
        params[3], params[4], params[5], //
        0.0, 0.0, 1.0,
    );

    let residuals: Vec<f64> = pairs
        .iter()
        .map(|(src, dst)| {
            let mapped = transform.transform_point(src);
            (mapped - dst).norm()
        })
        .collect();
    let rms = (residuals.iter().map(|r| r * r).sum::<f64>() / n as f64).sqrt();

    Ok(AffineFit {
        transform,
        residuals,
        rms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    /// Ground-truth affine: rotation + anisotropic scale + shear + offset.
    /// Representative of a board sitting slightly rotated and stretched on
    /// the machine bed.
    fn known_affine() -> Matrix3<f64> {
        let theta: f64 = 0.02; // rad, ~1.15°
        let (s, c) = theta.sin_cos();
        let (sx, sy) = (1.001, 0.9985);
        let shear = 3.0e-4;
        Matrix3::new(
            sx * c,
            -sy * s + shear,
            12.345,
            sx * s,
            sy * c,
            -7.890,
            0.0,
            0.0,
            1.0,
        )
    }

    fn fiducials() -> Vec<Point2<f64>> {
        // mm; a typical asymmetric 5-fiducial layout on a 100×80 board.
        vec![
            Point2::new(5.0, 5.0),
            Point2::new(95.0, 5.0),
            Point2::new(95.0, 75.0),
            Point2::new(5.0, 75.0),
            Point2::new(60.0, 40.0),
        ]
    }

    #[test]
    fn recovers_known_affine_exactly_without_noise() {
        let truth = known_affine();
        let pairs: Vec<_> = fiducials()
            .into_iter()
            .map(|p| (p, truth.transform_point(&p)))
            .collect();

        let fit = fit_affine(&pairs).unwrap();
        assert_relative_eq!(fit.transform, truth, epsilon = 1e-9);
        assert!(fit.rms < 1e-9, "rms = {}", fit.rms);
    }

    #[test]
    fn recovers_known_affine_under_3um_noise_below_5um_rms() {
        let truth = known_affine();
        let sources = fiducials();

        // Deterministic per-point noise, each component ≤ 3 µm = 0.003 mm.
        let noise_mm: [(f64, f64); 5] = [
            (0.003, -0.002),
            (-0.001, 0.003),
            (0.002, 0.001),
            (-0.003, -0.001),
            (0.001, -0.003),
        ];

        let pairs: Vec<_> = sources
            .iter()
            .zip(noise_mm)
            .map(|(&p, (nx, ny))| {
                let exact = truth.transform_point(&p);
                (p, Point2::new(exact.x + nx, exact.y + ny))
            })
            .collect();

        let fit = fit_affine(&pairs).unwrap();

        // Residual RMS against the noisy observations stays below 5 µm.
        assert!(fit.rms < 0.005, "rms = {} mm, expected < 0.005 mm", fit.rms);
        assert_eq!(fit.residuals.len(), 5);

        // The recovered transform reproduces the noise-free truth to
        // better than 5 µm at every fiducial.
        for p in &sources {
            let err = (fit.transform.transform_point(p) - truth.transform_point(p)).norm();
            assert!(err < 0.005, "model error {err} mm at {p}");
        }
    }

    #[test]
    fn collinear_sources_return_descriptive_err() {
        // 5 points on the line y = 0.5 x + 2 — rank-deficient by design.
        let pairs: Vec<_> = [0.0_f64, 10.0, 20.0, 30.0, 40.0]
            .iter()
            .map(|&x| {
                let p = Point2::new(x, 0.5 * x + 2.0);
                (p, Point2::new(p.x + 1.0, p.y - 1.0))
            })
            .collect();

        match fit_affine(&pairs) {
            Err(AffineError::DegenerateSources { rank }) => {
                assert!(rank < 6, "reported rank {rank} should be < 6");
            }
            other => panic!("expected DegenerateSources, got {other:?}"),
        }
    }

    #[test]
    fn fewer_than_three_pairs_is_an_error() {
        let p = Point2::new(1.0, 2.0);
        assert_eq!(fit_affine(&[]), Err(AffineError::TooFewPoints { got: 0 }));
        assert_eq!(
            fit_affine(&[(p, p), (p, p)]),
            Err(AffineError::TooFewPoints { got: 2 })
        );
    }

    /// nalgebra's singular-value sort carries an `.expect("Singular value was
    /// NaN")`, and a NaN that survived the solve would flow into machine
    /// coordinates — so the guard runs before the SVD, like `fit_homography`'s.
    #[test]
    fn non_finite_input_is_rejected_before_the_solve() {
        let truth = known_affine();
        let base: Vec<_> = fiducials()
            .into_iter()
            .map(|p| (p, truth.transform_point(&p)))
            .collect();

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for slot in 0..4 {
                let mut pairs = base.clone();
                match slot {
                    0 => pairs[1].0.x = bad,
                    1 => pairs[1].0.y = bad,
                    2 => pairs[3].1.x = bad,
                    _ => pairs[3].1.y = bad,
                }
                assert_eq!(
                    fit_affine(&pairs),
                    Err(AffineError::NonFinite),
                    "{bad} in slot {slot} must be refused"
                );
            }
        }
    }

    #[test]
    fn residuals_match_definition() {
        let truth = known_affine();
        let mut pairs: Vec<_> = fiducials()
            .into_iter()
            .map(|p| (p, truth.transform_point(&p)))
            .collect();
        // Perturb one observation by exactly 10 µm in x.
        pairs[2].1.x += 0.010;

        let fit = fit_affine(&pairs).unwrap();
        for ((src, dst), &res) in pairs.iter().zip(&fit.residuals) {
            let expect = (fit.transform.transform_point(src) - dst).norm();
            assert_relative_eq!(res, expect, epsilon = 1e-12);
        }
        let rms = (fit.residuals.iter().map(|r| r * r).sum::<f64>() / pairs.len() as f64).sqrt();
        assert_relative_eq!(fit.rms, rms, epsilon = 1e-12);
    }
}
