//! WS-DRV — [`Marker`] trait and machine backends.
//!
//! A **Marker** is a laser-ablation backend: it is handed process parameters
//! ([`pcb_core::AblationParams`]) and a set of tool-paths ([`pcb_core::Paths`],
//! produced by `cam::ablation::ablation_paths`) and "marks" them.
//!
//! The trait is deliberately small but shaped like the real thing so the
//! native hardware driver (the JCZ/EZCAD fiber controller) and the simulator
//! implement the *same* signature:
//!
//! * a real backend **streams** the paths to the galvo controller, honoring
//!   `passes` by re-running the job, and reports live machine state;
//! * the [`SimMarker`] here **accumulates** the marked energy into an
//!   in-memory raster so tests (and the operator UI preview) can inspect what
//!   *would* be ablated without any hardware.
//!
//! See [`SimMarker`] for the spot/dose model and the image frame convention.

use image::{GrayImage, Luma};
use pcb_core::{AblationParams, NM_PER_UM, Nm, P, PathElem, Paths};

/// Errors a [`Marker`] backend can report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerError {
    /// [`Marker::mark`] was called before [`Marker::configure`] supplied the
    /// process parameters (pass count, power, ...).
    NotConfigured,
    /// A hardware backend failed mid-job. The simulator never returns this;
    /// it exists so the trait surface matches the native driver, which surfaces
    /// controller faults (interlock open, no laser-ready, comms timeout) here.
    Backend(String),
    /// [`configure`](Marker::configure) got invalid process parameters (e.g.
    /// zero passes, which would no-op `mark` yet still report `Complete`).
    InvalidParams(String),
    /// Geometry would require an unsafe or unbounded amount of simulator work.
    InvalidGeometry(String),
}

impl std::fmt::Display for MarkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MarkerError::NotConfigured => {
                f.write_str("marker used before configure() set process parameters")
            }
            MarkerError::Backend(msg) => write!(f, "marker backend fault: {msg}"),
            MarkerError::InvalidParams(msg) => write!(f, "invalid marker parameters: {msg}"),
            MarkerError::InvalidGeometry(msg) => write!(f, "invalid marker geometry: {msg}"),
        }
    }
}

impl std::error::Error for MarkerError {}

/// Coarse backend state, returned by [`Marker::status`].
///
/// A hardware backend maps this onto live controller state (idle → running →
/// done, or the error path). The simulator reports [`MarkerStatus::Idle`]
/// until the first job is accumulated and [`MarkerStatus::Complete`] after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerStatus {
    /// Configured (or not) but nothing has been marked yet.
    Idle,
    /// At least one mark job has been accepted / streamed.
    Complete,
}

/// A laser-ablation backend.
///
/// Lifecycle: [`configure`](Marker::configure) once per material/op, then one
/// or more [`mark`](Marker::mark) calls, polling [`status`](Marker::status)
/// for completion. Implementors honor [`AblationParams::passes`] (a real driver
/// re-streams the geometry; the simulator re-stamps it so dose accumulates).
pub trait Marker {
    /// Load process parameters for the ops that follow. Must be called before
    /// [`mark`](Marker::mark).
    fn configure(&mut self, params: &AblationParams) -> Result<(), MarkerError>;

    /// Mark `paths`, honoring the configured pass count. Returns
    /// [`MarkerError::NotConfigured`] if [`configure`](Marker::configure) has
    /// not run.
    fn mark(&mut self, paths: &Paths) -> Result<(), MarkerError>;

    /// Current backend state (completion signal for the caller).
    fn status(&self) -> MarkerStatus;
}

/// Simulator backend: marks [`Paths`] into a binary raster with a Gaussian
/// laser spot.
///
/// # Image frame
///
/// The frame reproduces `testkit::rasterize`'s convention so a sim raster and a
/// reference raster of the same geometry line up pixel-for-pixel:
///
/// * one pixel spans `um_per_px` micrometers (the golden test uses 10);
/// * the frame is a caller-supplied bounding box `[min, max]` (nm); width and
///   height are `ceil(extent / pitch)` (minimum 1) — the same formula testkit
///   uses over a layer's vertex bbox;
/// * image origin is top-left, board **+y points up**: pixel `(i, j)` samples
///   its center at `x = min.x + (i + 1/2)·px`, `y = max.y - (j + 1/2)·px`.
///
/// # Spot / dose model
///
/// The spot is a circular Gaussian of configurable **1/e² diameter**
/// `spot_um`: at `w0 = spot_um/2` from a stamp center the deposited intensity
/// has fallen to `1/e²`. A stamp adds `exp(-2·d²/w0²)` to every pixel within
/// `2.5·w0` of it. Each [`PathElem`] polyline is stamped at a step of
/// `spot_um/4` (≤ spot/4, so overlapping stamps accumulate smoothly), and the
/// whole job is re-stamped `passes` times so dose **adds** across passes. A
/// pixel is "marked" (binary white) once its accumulated dose reaches
/// [`SimMarker::THRESHOLD`].
///
/// [`THRESHOLD`](SimMarker::THRESHOLD) is calibrated so that a densely-stamped
/// straight path (single pass) marks a stripe one spot-diameter wide: the
/// perpendicular dose profile of an infinite line of these Gaussians is itself
/// `∝ exp(-2·d²/w0²)`, and the threshold sits `e²` below that line's peak dose,
/// putting the marked edge at `d = w0 = spot/2`.
pub struct SimMarker {
    um_per_px: u32,
    spot_um: f64,
    min: P,
    max: P,
    w: u32,
    h: u32,
    passes: u32,
    configured: bool,
    marked: bool,
    /// Accumulated dose, row-major, length `w * h`.
    dose: Vec<f64>,
}

impl SimMarker {
    /// Dose at which a pixel flips to "marked". See the type-level docs for the
    /// calibration (line peak dose `≈ 2.507` per pass at a `spot/4` step;
    /// `2.507 · e⁻² ≈ 0.339`).
    pub const THRESHOLD: f64 = 0.339;

    /// Stamps beyond this many `w0` contribute negligibly (`e⁻¹²·⁵ ≈ 4e-6`).
    const CUTOFF_W0: f64 = 2.5;

    /// Stamp spacing as a fraction of the spot diameter (≤ 1/4 keeps the
    /// accumulated line profile smooth).
    const STEP_FRAC: f64 = 0.25;

    /// A corrupt coordinate or implausibly small spot must not turn one
    /// segment into a multi-billion-iteration loop.
    const MAX_STAMPS_PER_SEGMENT: u64 = 10_000_000;

    /// New simulator over the frame `[min, max]` (nm) at `um_per_px`
    /// micrometers/pixel, with a Gaussian spot of `spot_um` 1/e² diameter.
    ///
    /// Dimensions follow testkit's `ceil(extent / pitch).max(1)`.
    ///
    /// # Panics
    ///
    /// Panics if `um_per_px == 0` or the frame would exceed `u32` dimensions.
    pub fn new(min: P, max: P, um_per_px: u32, spot_um: f64) -> Self {
        assert!(um_per_px > 0, "um_per_px must be positive");
        assert!(
            spot_um.is_finite() && spot_um >= 0.001,
            "spot_um must be finite and at least 0.001 um"
        );
        assert!(max.x >= min.x && max.y >= min.y, "invalid simulator frame");
        let px: Nm = Nm::from(um_per_px) * NM_PER_UM;
        let w = span_px(
            max.x.checked_sub(min.x).expect("frame x extent overflow"),
            px,
        );
        let h = span_px(
            max.y.checked_sub(min.y).expect("frame y extent overflow"),
            px,
        );
        let len = (w as usize)
            .checked_mul(h as usize)
            .expect("simulator raster allocation overflow");
        Self {
            um_per_px,
            spot_um,
            min,
            max,
            w,
            h,
            passes: 0,
            configured: false,
            marked: false,
            dose: vec![0.0; len],
        }
    }

    /// Frame dimensions in pixels (`width`, `height`).
    pub fn dimensions(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    /// Consume the simulator and threshold the accumulated dose into a binary
    /// grayscale image (white = marked), matching testkit's white=filled
    /// convention.
    pub fn into_raster(self) -> GrayImage {
        let mut img = GrayImage::from_pixel(self.w, self.h, Luma([0]));
        for (idx, &d) in self.dose.iter().enumerate() {
            if d >= Self::THRESHOLD {
                let x = (idx % self.w as usize) as u32;
                let y = (idx / self.w as usize) as u32;
                img.put_pixel(x, y, Luma([255]));
            }
        }
        img
    }

    fn stamp_elem(&mut self, elem: &PathElem) -> Result<(), MarkerError> {
        let pts = &elem.pts;
        let n = pts.len();
        if n == 0 {
            return Ok(());
        }
        if n == 1 {
            self.stamp_point(pts[0].x as f64, pts[0].y as f64);
            return Ok(());
        }
        let last = if elem.closed { n } else { n - 1 };
        for k in 0..last {
            self.stamp_segment(pts[k], pts[(k + 1) % n])?;
        }
        // Open polylines: segments sample [a, b), so the final vertex is never
        // a segment start — stamp it explicitly.
        if !elem.closed {
            self.stamp_point(pts[n - 1].x as f64, pts[n - 1].y as f64);
        }
        Ok(())
    }

    fn stamp_segment(&mut self, a: P, b: P) -> Result<(), MarkerError> {
        let (ax, ay) = (a.x as f64, a.y as f64);
        let (bx, by) = (b.x as f64, b.y as f64);
        let step = self.spot_um * Self::STEP_FRAC * NM_PER_UM as f64;
        let len = (bx - ax).hypot(by - ay);
        let steps = (len / step).ceil().max(1.0) as u64;
        if steps > Self::MAX_STAMPS_PER_SEGMENT {
            return Err(MarkerError::InvalidGeometry(format!(
                "segment requires {steps} samples (limit {})",
                Self::MAX_STAMPS_PER_SEGMENT
            )));
        }
        for k in 0..steps {
            let t = k as f64 / steps as f64;
            self.stamp_point(ax + (bx - ax) * t, ay + (by - ay) * t);
        }
        Ok(())
    }

    fn stamp_point(&mut self, sx: f64, sy: f64) {
        let px = self.um_per_px as f64 * NM_PER_UM as f64;
        let w0 = self.spot_um * 0.5 * NM_PER_UM as f64;
        let cutoff = Self::CUTOFF_W0 * w0;
        let cutoff2 = cutoff * cutoff;
        let inv = 2.0 / (w0 * w0);
        let minx = self.min.x as f64;
        let maxy = self.max.y as f64;
        let half = px / 2.0;

        // Pixel index windows whose centers fall within `cutoff` of (sx, sy).
        let i_lo = (((sx - cutoff) - minx - half) / px).ceil();
        let i_hi = (((sx + cutoff) - minx - half) / px).floor();
        let j_lo = ((maxy - half - (sy + cutoff)) / px).ceil();
        let j_hi = ((maxy - half - (sy - cutoff)) / px).floor();
        let i0 = i_lo.max(0.0) as i64;
        let i1 = (i_hi.min((self.w as i64 - 1) as f64)) as i64;
        let j0 = j_lo.max(0.0) as i64;
        let j1 = (j_hi.min((self.h as i64 - 1) as f64)) as i64;

        for j in j0..=j1 {
            let cy = maxy - j as f64 * px - half;
            let dy = cy - sy;
            for i in i0..=i1 {
                let cx = minx + i as f64 * px + half;
                let dx = cx - sx;
                let d2 = dx * dx + dy * dy;
                if d2 <= cutoff2 {
                    let idx = j as usize * self.w as usize + i as usize;
                    self.dose[idx] += (-d2 * inv).exp();
                }
            }
        }
    }
}

impl Marker for SimMarker {
    fn configure(&mut self, params: &AblationParams) -> Result<(), MarkerError> {
        params
            .validate()
            .map_err(|e| MarkerError::InvalidParams(e.to_string()))?;
        // Zero passes would make `mark` a silent no-op that still reports
        // Complete — a board that never got ablated but looks done (LR-30).
        if params.passes == 0 {
            return Err(MarkerError::InvalidParams("passes must be ≥ 1".into()));
        }
        self.passes = params.passes;
        self.configured = true;
        Ok(())
    }

    fn mark(&mut self, paths: &Paths) -> Result<(), MarkerError> {
        if !self.configured {
            return Err(MarkerError::NotConfigured);
        }
        // Honor passes: re-stamp the whole job so dose accumulates.
        for _ in 0..self.passes {
            for elem in &paths.elems {
                self.stamp_elem(elem)?;
            }
        }
        self.marked = true;
        Ok(())
    }

    fn status(&self) -> MarkerStatus {
        if self.marked {
            MarkerStatus::Complete
        } else {
            MarkerStatus::Idle
        }
    }
}

/// Image extent in pixels for a bbox extent of `len` nm at `px` nm/pixel:
/// `ceil(len / px).max(1)` (testkit's convention).
fn span_px(len: Nm, px: Nm) -> u32 {
    debug_assert!(px > 0);
    let n = len.div_euclid(px) + Nm::from(len.rem_euclid(px) != 0);
    u32::try_from(n.max(1)).expect("raster dimensions exceed u32")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::{CamOpts, Layer, NM_PER_MM, PathKind, Poly};

    fn cfg(passes: u32) -> AblationParams {
        AblationParams {
            power_pct: 50.0,
            speed_mm_s: 1000.0,
            frequency_khz: 30.0,
            pulse_ns: 0,
            passes,
        }
    }

    fn rect_mm(x0: f64, y0: f64, x1: f64, y1: f64) -> Poly {
        Poly {
            outer: vec![
                P::from_mm(x0, y0),
                P::from_mm(x1, y0),
                P::from_mm(x1, y1),
                P::from_mm(x0, y1),
            ],
            holes: vec![],
        }
    }

    /// Bounding box over every vertex of a layer, matching testkit::rasterize.
    fn layer_bounds(layer: &Layer) -> (P, P) {
        let mut it = layer
            .polys
            .iter()
            .flat_map(|p| std::iter::once(&p.outer).chain(p.holes.iter()))
            .flatten();
        let first = *it.next().expect("non-empty layer");
        let (mut min, mut max) = (first, first);
        for p in it {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
        (min, max)
    }

    fn white_count(img: &GrayImage) -> u64 {
        img.pixels().filter(|p| p[0] >= 128).count() as u64
    }

    #[test]
    fn zero_passes_is_rejected_at_configure() {
        // Otherwise `mark` no-ops yet `status()` reports Complete — a board
        // that was never ablated but looks done (LR-30).
        let mut sim = SimMarker::new(P::from_mm(0.0, 0.0), P::from_mm(1.0, 1.0), 10, 45.0);
        assert!(matches!(
            sim.configure(&cfg(0)),
            Err(MarkerError::InvalidParams(_))
        ));
        assert!(sim.configure(&cfg(1)).is_ok());
    }

    #[test]
    #[should_panic(expected = "spot_um must be finite")]
    fn zero_spot_is_rejected_at_construction() {
        let _ = SimMarker::new(P::new(0, 0), P::new(1, 1), 10, 0.0);
    }

    #[test]
    fn implausibly_long_segment_is_rejected_before_sampling() {
        let mut sim = SimMarker::new(P::new(0, 0), P::from_mm(1.0, 1.0), 10, 45.0);
        sim.configure(&cfg(1)).unwrap();
        let paths = Paths {
            elems: vec![PathElem {
                kind: PathKind::Mark,
                pts: vec![P::new(Nm::MIN, 0), P::new(Nm::MAX, 0)],
                closed: false,
            }],
        };
        assert!(matches!(
            sim.mark(&paths),
            Err(MarkerError::InvalidGeometry(_))
        ));
    }

    // (1) A single straight path marks a stripe of ~spot width. -----------
    #[test]
    fn straight_path_stripe_is_about_one_spot_wide() {
        let spot_um = 45.0;
        let um_per_px = 10;
        // Horizontal 2 mm line at y = 0, framed with a 0.2 mm margin.
        let min = P::from_mm(-0.2, -0.2);
        let max = P::from_mm(2.2, 0.2);
        let paths = Paths {
            elems: vec![PathElem {
                kind: PathKind::Mark,
                pts: vec![P::from_mm(0.0, 0.0), P::from_mm(2.0, 0.0)],
                closed: false,
            }],
        };
        let mut sim = SimMarker::new(min, max, um_per_px, spot_um);
        sim.configure(&cfg(1)).unwrap();
        sim.mark(&paths).unwrap();
        assert_eq!(sim.status(), MarkerStatus::Complete);
        let (w, _h) = sim.dimensions();
        let img = sim.into_raster();

        // Count marked pixels in the column through the line midpoint (x=1mm).
        let col = w / 2;
        let stripe: u32 = (0..img.height())
            .filter(|&y| img.get_pixel(col, y)[0] >= 128)
            .count() as u32;
        let expect_px = spot_um / um_per_px as f64; // 4.5 px
        assert!(
            (stripe as f64 - expect_px).abs() <= 2.0,
            "stripe {stripe} px, expected ~{expect_px} (spot width) within 2 px"
        );
    }

    // (3) Passes accumulate: 2 passes cover at least as much as 1. ---------
    #[test]
    fn two_passes_cover_at_least_one_pass() {
        let spot_um = 45.0;
        let min = P::from_mm(-0.2, -0.2);
        let max = P::from_mm(2.2, 0.2);
        let paths = Paths {
            elems: vec![PathElem {
                kind: PathKind::Mark,
                pts: vec![P::from_mm(0.0, 0.0), P::from_mm(2.0, 0.0)],
                closed: false,
            }],
        };
        let mark = |passes| {
            let mut sim = SimMarker::new(min, max, 10, spot_um);
            sim.configure(&cfg(passes)).unwrap();
            sim.mark(&paths).unwrap();
            white_count(&sim.into_raster())
        };
        let one = mark(1);
        let two = mark(2);
        assert!(one > 0, "one pass marked nothing");
        assert!(two >= one, "two passes {two} < one pass {one}");
    }

    /// Force `layer`'s rasterize bbox to exactly `[min, max]` by appending a
    /// degenerate 2-vertex poly at the frame corners (ignored by fill, which
    /// skips rings with < 3 vertices, but counted by testkit's vertex bbox).
    fn framed(mut layer: Layer, min: P, max: P) -> Layer {
        layer.polys.push(Poly {
            outer: vec![min, max],
            holes: vec![],
        });
        layer
    }

    // (2) Golden: sim-marking a synthetic board's ablation paths covers the
    // rub-out band + isolation geometry to < 0.5 % disagreement. ----------
    //
    // NOTE ON THE SYNTHETIC-BOARD SUBSTITUTION: there is no samples/kicad board
    // in the repo, so the "design" is built synthetically here (two copper
    // rectangles: a trace and a pad) rather than ingested from a real
    // .kicad_pcb. We run the *real* `cam::ablation::ablation_paths` over it,
    // sim-mark the result, and XOR-compare against `testkit::rasterize` of the
    // geometry that marking is meant to cover:
    //
    //   * the rub-out band dilated by the spot radius (the hatch fills the band;
    //     sweeping it with the Gaussian spot grows it by ~spot/2), unioned with
    //   * the isolation annulus: copper grown to the outermost isolation
    //     contour's swept edge, minus the copper itself.
    //
    // Three crossing hatch sets are used so the swept coverage is isotropic and
    // matches an isotropic spot/2 dilation (a single hatch angle reaches full
    // spot/2 only along the hatch direction, biasing the effective radius). A
    // real-board golden is DEFERRED until samples/kicad exists.
    #[test]
    fn golden_marks_cover_band_and_isolation_geometry() {
        let um_per_px = 10;
        let spot_um = 45.0;
        let passes = 1;
        let hatch_sets = 3;

        // Synthetic "design": a trace and a pad.
        let layer = Layer {
            polys: vec![
                rect_mm(1.0, 1.0, 1.3, 5.0), // 0.3 x 4 mm trace
                rect_mm(3.0, 2.0, 4.0, 3.0), // 1 x 1 mm pad
            ],
        };
        let opts = CamOpts {
            n_contours: 2,
            clearance_mm: 0.5,
            band_mm: 1.0,
            interval_mm: 0.03,
            ..CamOpts::default()
        };

        // Real CAM tool-paths: isolation contours + crossing rub-out hatch sets.
        let paths = cam::ablation::ablation_paths(&layer, &opts, hatch_sets);

        // Reference geometry the marks should cover. Effective sweep radius is
        // the spot radius (the sim's THRESHOLD is calibrated so a dense line
        // marks a spot-wide stripe).
        let r_eff: Nm = (spot_um / 2.0 * NM_PER_UM as f64).round() as Nm; // spot/2
        let band = cam::ablation::rubout_band(&layer, &opts);
        let band_swept = cam::geom::offset(&band, r_eff);
        let iso_outer = cam::ablation::isolation_offset_nm(&opts, opts.n_contours - 1) + r_eff;
        let iso_region =
            cam::geom::difference(&cam::geom::offset(&layer.polys, iso_outer), &layer.polys);
        let reference = Layer {
            polys: cam::geom::union(&band_swept, &iso_region),
        };

        // Fixed frame (copper bbox + clearance + band + one spot), independent
        // of r_eff, shared by both rasters so they line up pixel-for-pixel.
        let expand = ((opts.clearance_mm + opts.band_mm) * NM_PER_MM as f64) as Nm
            + (spot_um * NM_PER_UM as f64) as Nm;
        let (cmin, cmax) = layer_bounds(&layer);
        let min = P::new(cmin.x - expand, cmin.y - expand);
        let max = P::new(cmax.x + expand, cmax.y + expand);
        let ref_img = testkit::rasterize(&framed(reference, min, max), um_per_px);

        let mut sim = SimMarker::new(min, max, um_per_px, spot_um);
        sim.configure(&cfg(passes)).unwrap();
        sim.mark(&paths).unwrap();
        let sim_img = sim.into_raster();

        assert_eq!(
            sim_img.dimensions(),
            ref_img.dimensions(),
            "frames must match"
        );
        // Sanity: not two blank images agreeing trivially.
        assert!(white_count(&ref_img) > 1000);
        // < 0.5 % disagreement == agree on >= 99.5 % (measured ~0.9973).
        testkit::assert_images_agree(&sim_img, &ref_img, 0.995);
    }
}
