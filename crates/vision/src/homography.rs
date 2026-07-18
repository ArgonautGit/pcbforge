//! Projective (perspective) fit from point correspondences — the 8-DOF
//! homography a *tilted* camera needs, where the 6-DOF [`fit_affine`] can only
//! model translation/rotation/scale/shear but not keystone foreshortening.
//!
//! A flat plane (the board / bed) viewed by a pinhole camera projects to the
//! image by a homography; recovering it lets pixel↔plane mapping account for
//! camera tilt. Needs **≥ 4** correspondences in general position (no 3
//! collinear) — 3 points can only ever fix an affine.
//!
//! Solved by the **normalized DLT**: isotropically normalize both point sets
//! (centroid to origin, mean distance √2) for conditioning, assemble the
//! `2N×9` constraint matrix, take the right singular vector of its smallest
//! singular value, then de-normalize. See Hartley & Zisserman, *Multiple View
//! Geometry*, Alg. 4.2.
//!
//! [`fit_affine`]: crate::fit_affine

use nalgebra::{DMatrix, Matrix3, Point2, SymmetricEigen};

/// A fitted plane-to-plane projective transform.
#[derive(Debug, Clone, PartialEq)]
pub struct Homography {
    /// Homogeneous 3×3, normalized so `matrix[(2,2)] == 1`. Maps source to
    /// destination with a perspective divide (see [`Homography::apply`]).
    pub matrix: Matrix3<f64>,
    /// Per-point Euclidean reprojection residual (dst units), input order.
    pub residuals: Vec<f64>,
    /// RMS of `residuals`.
    pub rms: f64,
}

impl Homography {
    /// Apply to a point: `[x y 1]ᵀ → H·[x y 1]ᵀ`, then divide by `w`.
    pub fn apply(&self, p: Point2<f64>) -> Point2<f64> {
        let m = &self.matrix;
        let w = m[(2, 0)] * p.x + m[(2, 1)] * p.y + m[(2, 2)];
        Point2::new(
            (m[(0, 0)] * p.x + m[(0, 1)] * p.y + m[(0, 2)]) / w,
            (m[(1, 0)] * p.x + m[(1, 1)] * p.y + m[(1, 2)]) / w,
        )
    }

    /// The inverse transform (dst → src), or `None` if singular.
    pub fn try_inverse(&self) -> Option<Homography> {
        let inv = self.matrix.try_inverse()?;
        // `inv[(2,2)]` can be exactly 0 even for a nonsingular H (it is the
        // source matrix's top-left 2×2 minor over det); normalizing by it
        // would spray inf/NaN through an otherwise "successful" inverse.
        if inv[(2, 2)].abs() < 1e-12 {
            return None;
        }
        let m = inv / inv[(2, 2)];
        Some(Homography {
            matrix: m,
            residuals: Vec::new(),
            rms: 0.0,
        })
    }
}

/// Why a homography fit could not be computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomographyError {
    /// Fewer than four correspondences: perspective is underdetermined.
    TooFewPoints {
        /// Number of pairs supplied.
        got: usize,
    },
    /// The points are degenerate (collinear / coincident), so the constraint
    /// matrix is rank-deficient and the homography is not unique.
    Degenerate,
    /// The SVD failed to produce singular vectors.
    SolveFailed,
}

impl std::fmt::Display for HomographyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewPoints { got } => write!(
                f,
                "homography needs at least 4 correspondences, got {got}: \
                 perspective (8 DOF) is underdetermined — add a fourth fiducial"
            ),
            Self::Degenerate => write!(
                f,
                "homography fit is degenerate: the points are collinear or \
                 coincident (need 4 in general position, no 3 on a line)"
            ),
            Self::SolveFailed => write!(f, "homography SVD solve failed"),
        }
    }
}

impl std::error::Error for HomographyError {}

/// Isotropic normalization: translate `pts` to their centroid and scale so the
/// mean distance to the origin is √2. Returns the transformed points and the
/// 3×3 similarity `T` (so `T·original = normalized`). `None` if all points
/// coincide (zero spread).
fn normalize(pts: &[Point2<f64>]) -> Option<(Vec<Point2<f64>>, Matrix3<f64>)> {
    let n = pts.len() as f64;
    let (cx, cy) = pts
        .iter()
        .fold((0.0, 0.0), |(sx, sy), p| (sx + p.x, sy + p.y));
    let (cx, cy) = (cx / n, cy / n);
    let mean_d = pts
        .iter()
        .map(|p| ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt())
        .sum::<f64>()
        / n;
    if mean_d < 1e-12 {
        return None;
    }
    let s = std::f64::consts::SQRT_2 / mean_d;
    let t = Matrix3::new(s, 0.0, -s * cx, 0.0, s, -s * cy, 0.0, 0.0, 1.0);
    let out = pts
        .iter()
        .map(|p| Point2::new(s * (p.x - cx), s * (p.y - cy)))
        .collect();
    Some((out, t))
}

/// Fit the homography mapping `pairs[i].0` (source) onto `pairs[i].1` (dst).
///
/// # Errors
/// - [`HomographyError::TooFewPoints`] with fewer than 4 pairs.
/// - [`HomographyError::Degenerate`] for collinear/coincident points.
/// - [`HomographyError::SolveFailed`] if the SVD back-substitution fails.
pub fn fit_homography(pairs: &[(Point2<f64>, Point2<f64>)]) -> Result<Homography, HomographyError> {
    let n = pairs.len();
    if n < 4 {
        return Err(HomographyError::TooFewPoints { got: n });
    }
    let src: Vec<Point2<f64>> = pairs.iter().map(|p| p.0).collect();
    let dst: Vec<Point2<f64>> = pairs.iter().map(|p| p.1).collect();
    let (ns, ts) = normalize(&src).ok_or(HomographyError::Degenerate)?;
    let (nd, td) = normalize(&dst).ok_or(HomographyError::Degenerate)?;

    // 2N×9 DLT constraint matrix on the normalized points.
    let mut a = DMatrix::<f64>::zeros(2 * n, 9);
    for (i, (s, d)) in ns.iter().zip(&nd).enumerate() {
        let (x, y, u, v) = (s.x, s.y, d.x, d.y);
        let r = 2 * i;
        a[(r, 0)] = -x;
        a[(r, 1)] = -y;
        a[(r, 2)] = -1.0;
        a[(r, 6)] = u * x;
        a[(r, 7)] = u * y;
        a[(r, 8)] = u;
        a[(r + 1, 3)] = -x;
        a[(r + 1, 4)] = -y;
        a[(r + 1, 5)] = -1.0;
        a[(r + 1, 6)] = v * x;
        a[(r + 1, 7)] = v * y;
        a[(r + 1, 8)] = v;
    }

    // Solve `A·h = 0` via the smallest eigenvector of `AᵀA` (always 9×9, so
    // this is robust even for exactly 4 points where `A` is 8×9 and a thin SVD
    // would drop the null-space vector).
    let ata = a.transpose() * a;
    let eig = SymmetricEigen::new(ata);
    let mut order: Vec<usize> = (0..9).collect();
    order.sort_by(|&i, &j| eig.eigenvalues[i].total_cmp(&eig.eigenvalues[j]));
    let lmax = eig.eigenvalues[order[8]].max(1e-300);
    // Two near-zero eigenvalues ⇒ the null space isn't 1-D ⇒ degenerate.
    if eig.eigenvalues[order[1]] < lmax * 1e-12 {
        return Err(HomographyError::Degenerate);
    }
    let h = eig.eigenvectors.column(order[0]);
    let h_norm = Matrix3::new(h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], h[8]);

    // De-normalize: H = Td⁻¹ · Hn · Ts.
    let td_inv = td.try_inverse().ok_or(HomographyError::SolveFailed)?;
    let mut m = td_inv * h_norm * ts;
    if m[(2, 2)].abs() < 1e-12 {
        return Err(HomographyError::SolveFailed);
    }
    m /= m[(2, 2)];

    let mut h = Homography {
        matrix: m,
        residuals: Vec::with_capacity(n),
        rms: 0.0,
    };
    let mut sq = 0.0;
    for (s, d) in pairs {
        let r = (h.apply(*s) - d).norm();
        sq += r * r;
        h.residuals.push(r);
    }
    h.rms = (sq / n as f64).sqrt();
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    /// A genuine perspective (keystone) transform: a unit-ish projective map
    /// with non-zero bottom row.
    fn truth() -> Matrix3<f64> {
        Matrix3::new(
            1.02, 0.03, 12.0, //
            -0.02, 0.98, -7.0, //
            0.0012, -0.0008, 1.0,
        )
    }

    fn apply(m: &Matrix3<f64>, p: Point2<f64>) -> Point2<f64> {
        let w = m[(2, 0)] * p.x + m[(2, 1)] * p.y + m[(2, 2)];
        Point2::new(
            (m[(0, 0)] * p.x + m[(0, 1)] * p.y + m[(0, 2)]) / w,
            (m[(1, 0)] * p.x + m[(1, 1)] * p.y + m[(1, 2)]) / w,
        )
    }

    fn square_plus() -> Vec<Point2<f64>> {
        // 4 corners + a 5th interior point (general position).
        vec![
            Point2::new(0.0, 0.0),
            Point2::new(60.0, 0.0),
            Point2::new(60.0, 50.0),
            Point2::new(0.0, 50.0),
            Point2::new(23.0, 17.0),
        ]
    }

    #[test]
    fn recovers_a_known_perspective_transform() {
        let t = truth();
        let pairs: Vec<_> = square_plus()
            .into_iter()
            .map(|p| (p, apply(&t, p)))
            .collect();
        let h = fit_homography(&pairs).unwrap();
        assert!(h.rms < 1e-9, "rms = {}", h.rms);
        // Maps a fresh point the same way the truth does.
        let q = Point2::new(40.0, 30.0);
        let want = apply(&t, q);
        assert_relative_eq!(h.apply(q).x, want.x, epsilon = 1e-6);
        assert_relative_eq!(h.apply(q).y, want.y, epsilon = 1e-6);
    }

    #[test]
    fn affine_cannot_but_homography_does_model_keystone() {
        // A strong keystone: an affine fit leaves large residuals; the
        // homography nails it — the reason perspective needs this path.
        let t = Matrix3::new(1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.004, 0.0, 1.0);
        let src = square_plus();
        let pairs: Vec<_> = src.iter().map(|&p| (p, apply(&t, p))).collect();

        let aff = crate::fit_affine(&pairs).unwrap();
        let hom = fit_homography(&pairs).unwrap();
        assert!(
            aff.rms > 0.5,
            "affine should struggle with keystone, rms = {}",
            aff.rms
        );
        assert!(hom.rms < 1e-9, "homography fits it, rms = {}", hom.rms);
    }

    #[test]
    fn inverse_round_trips() {
        let t = truth();
        let pairs: Vec<_> = square_plus()
            .into_iter()
            .map(|p| (p, apply(&t, p)))
            .collect();
        let h = fit_homography(&pairs).unwrap();
        let inv = h.try_inverse().unwrap();
        let p = Point2::new(31.0, 19.0);
        let round = inv.apply(h.apply(p));
        assert_relative_eq!(round.x, p.x, epsilon = 1e-6);
        assert_relative_eq!(round.y, p.y, epsilon = 1e-6);
    }

    #[test]
    fn recovers_under_subpixel_noise() {
        let t = truth();
        let src = square_plus();
        let noise: [(f64, f64); 5] = [
            (0.1, -0.1),
            (-0.1, 0.1),
            (0.1, 0.1),
            (-0.1, -0.1),
            (0.05, -0.05),
        ];
        let pairs: Vec<_> = src
            .iter()
            .zip(noise)
            .map(|(&p, (nx, ny))| {
                let d = apply(&t, p);
                (p, Point2::new(d.x + nx, d.y + ny))
            })
            .collect();
        let h = fit_homography(&pairs).unwrap();
        // Sub-pixel input noise → sub-pixel reprojection, and the model still
        // predicts a fresh point close to truth.
        assert!(h.rms < 0.25, "rms = {}", h.rms);
        let q = Point2::new(45.0, 25.0);
        assert!((h.apply(q) - apply(&t, q)).norm() < 0.5);
    }

    #[test]
    fn too_few_points_is_an_error() {
        let p = Point2::new(0.0, 0.0);
        let three = vec![(p, p), (p, p), (p, p)];
        assert_eq!(
            fit_homography(&three),
            Err(HomographyError::TooFewPoints { got: 3 })
        );
    }

    #[test]
    fn inverse_of_zero_bottom_right_minor_is_none_not_nan() {
        // Nonsingular (det = −1) but its top-left 2×2 minor is 0, so the
        // inverse's [(2,2)] is 0 — the old code divided by it and returned a
        // matrix full of inf/NaN (LR-26). Now it's a clean `None`.
        let h = Homography {
            matrix: Matrix3::new(1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.0, 1.0, 1.0),
            residuals: Vec::new(),
            rms: 0.0,
        };
        assert!(h.matrix.determinant().abs() > 0.5, "H is nonsingular");
        assert!(h.try_inverse().is_none());
    }

    #[test]
    fn collinear_points_are_degenerate() {
        // 4 points on the line y = x → no valid homography.
        let pairs: Vec<_> = [0.0, 10.0, 20.0, 30.0]
            .iter()
            .map(|&x| (Point2::new(x, x), Point2::new(x + 1.0, x + 1.0)))
            .collect();
        assert_eq!(fit_homography(&pairs), Err(HomographyError::Degenerate));
    }
}
