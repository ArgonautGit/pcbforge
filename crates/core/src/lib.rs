//! PCBForge shared types.
//!
//! All geometry is integer nanometers (`Nm = i64`). i64 nanometers span
//! ±9.2e9 m — effectively unbounded for PCB work — and make equality,
//! hashing, and boolean-op robustness exact. Conversion to f64 happens only
//! at library boundaries (cam::geom, emitters) and must be lossless for
//! coordinates within ±1 m.

/// Integer nanometers.
pub type Nm = i64;

pub const NM_PER_UM: Nm = 1_000;
pub const NM_PER_MM: Nm = 1_000_000;

/// A point in board space, nanometers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct P {
    pub x: Nm,
    pub y: Nm,
}

impl P {
    pub const fn new(x: Nm, y: Nm) -> Self {
        Self { x, y }
    }

    /// Construct from millimeters (rounds to nearest nanometer).
    pub fn from_mm(x_mm: f64, y_mm: f64) -> Self {
        Self {
            x: (x_mm * NM_PER_MM as f64).round() as Nm,
            y: (y_mm * NM_PER_MM as f64).round() as Nm,
        }
    }

    pub fn x_mm(&self) -> f64 {
        self.x as f64 / NM_PER_MM as f64
    }

    pub fn y_mm(&self) -> f64 {
        self.y as f64 / NM_PER_MM as f64
    }
}

/// A closed ring of vertices. Closure is implicit: the last vertex connects
/// back to the first; do not repeat the first vertex at the end.
pub type Ring = Vec<P>;

/// A polygon with holes. Convention: `outer` is counter-clockwise, each hole
/// is clockwise. Producers must uphold this; `cam::geom` may rely on it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Poly {
    pub outer: Ring,
    pub holes: Vec<Ring>,
}

/// One board layer (copper, mask, silk, paste) as filled polygons.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layer {
    pub polys: Vec<Poly>,
}

/// What a path element is for. CAM stages tag their output so ordering,
/// pass-planning, and machine-splitting can treat kinds differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathKind {
    /// Isolation contour; 0 = closest to the copper boundary.
    Isolation(u32),
    /// Rub-out hatch line, tagged with the pass index whose angle it carries.
    Rubout(u32),
    /// Force-clear centerline through a sub-min-feature sliver.
    ForceClear,
    /// The exact design-edge contour (UV finishing set).
    Boundary,
    /// Fiducial / tooling mark.
    Mark,
    /// Board-outline through-cut (depaneling) segment. Kerf-compensated onto
    /// the waste side and broken by holding tabs; run as the board's final
    /// job with a lowering focal plane (see `cam::cut`).
    Cut,
}

/// A single polyline the laser will trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathElem {
    pub kind: PathKind,
    pub pts: Vec<P>,
    /// If true the last vertex connects back to the first.
    pub closed: bool,
}

/// An ordered collection of path elements (one machine's job geometry).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Paths {
    pub elems: Vec<PathElem>,
}

/// CAM geometry options. Lengths in millimeters (converted internally).
#[derive(Debug, Clone, PartialEq)]
pub struct CamOpts {
    /// Effective laser spot diameter, mm.
    pub spot_mm: f64,
    /// Number of isolation contours outside each copper boundary.
    pub n_contours: u32,
    /// Copper-to-rubout clearance, mm.
    pub clearance_mm: f64,
    /// Rub-out band width beyond the clearance, mm.
    pub band_mm: f64,
    /// Hatch line spacing, mm.
    pub interval_mm: f64,
    /// First-pass hatch angle, degrees.
    pub base_angle_deg: f64,
    /// Per-pass hatch angle increment, degrees.
    pub fill_angle_step_deg: f64,
    /// Minimum feature the machine can reliably clear, mm.
    pub min_feature_mm: f64,
    /// Fiber-vs-UV guard band for the dual-machine split, mm.
    pub guard_mm: f64,
}

impl Default for CamOpts {
    fn default() -> Self {
        Self {
            spot_mm: 0.045,
            n_contours: 2,
            clearance_mm: 0.5,
            band_mm: 1.0,
            interval_mm: 0.03,
            base_angle_deg: 0.0,
            fill_angle_step_deg: 17.0,
            min_feature_mm: 0.15,
            guard_mm: 0.15,
        }
    }
}

/// Laser process parameters for one op (one material-table row).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AblationParams {
    pub power_pct: f64,
    pub speed_mm_s: f64,
    pub frequency_khz: f64,
    /// Q-pulse width, ns (MOPA); 0 = source default.
    pub pulse_ns: u32,
    pub passes: u32,
}

/// Validation failure for parameters that can reach a laser or generate a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamError(pub &'static str);

impl std::fmt::Display for ParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ParamError {}

impl AblationParams {
    /// Reject recipes that are silent no-ops or expand into unreasonable work.
    pub fn validate(&self) -> Result<(), ParamError> {
        if !self.power_pct.is_finite() || !(0.0..=100.0).contains(&self.power_pct) {
            return Err(ParamError("power_pct must be finite and in 0..=100"));
        }
        if self.power_pct == 0.0 {
            return Err(ParamError("power_pct must be greater than zero"));
        }
        if !self.speed_mm_s.is_finite() || self.speed_mm_s <= 0.0 {
            return Err(ParamError(
                "speed_mm_s must be finite and greater than zero",
            ));
        }
        if !self.frequency_khz.is_finite() || self.frequency_khz <= 0.0 {
            return Err(ParamError(
                "frequency_khz must be finite and greater than zero",
            ));
        }
        if self.passes == 0 {
            return Err(ParamError("passes must be at least one"));
        }
        if self.passes > 100_000 {
            return Err(ParamError("passes must not exceed 100000"));
        }
        Ok(())
    }
}

/// How passes are grouped into checkpointed job files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassPlan {
    pub group_size: u32,
    pub max_corrective_iters: u32,
}

/// One pass within a group: its global index and the hatch angle it uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassSpec {
    pub pass_index: u32,
    pub hatch_angle_deg: f64,
}

/// A group of passes emitted as one job file, ending at a checkpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct PassGroup {
    pub passes: Vec<PassSpec>,
    pub checkpoint: bool,
}

/// Which physical machine a job targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Machine {
    Fiber,
    Uv,
}

/// Board-outline through-cut (depaneling) parameters. Lengths in millimeters.
///
/// `kerf_mm`, `mm_per_pass`, and `z_step_mm` are **machine facts** measured
/// on scrap FR4 (see `cam::cut` and docs/plans/cam-10-board-cut.md); the
/// [`Default`] values are deliberately conservative placeholders that the CLI
/// flags as un-calibrated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutOpts {
    /// Measured beam kerf width at focus, mm. The cut centerline is offset
    /// onto the waste side by `kerf_mm / 2`.
    pub kerf_mm: f64,
    /// Holding tabs left per closed ring.
    pub tab_count: u32,
    /// Width of solid material each tab leaves standing, mm.
    pub tab_mm: f64,
    /// Depth of FR4 removed per pass at the cut process params, mm.
    pub mm_per_pass: f64,
    /// Maximum focal-plane drop per step, mm — must not exceed the lens's
    /// usable depth of focus, or the beam defocuses at the step's floor.
    pub z_step_mm: f64,
    /// Extra commanded depth past the far face so the cut fully severs, mm.
    pub overcut_mm: f64,
    /// Which machine cuts (fiber for FR4 bulk; UV for thin/finishing).
    pub machine: Machine,
}

impl Default for CutOpts {
    fn default() -> Self {
        Self {
            kerf_mm: 0.05,
            tab_count: 4,
            tab_mm: 0.5,
            mm_per_pass: 0.05,
            z_step_mm: 0.2,
            overcut_mm: 0.1,
            machine: Machine::Fiber,
        }
    }
}

impl CutOpts {
    /// Validate measured machine facts before computing a focus schedule.
    pub fn validate(&self, thickness_nm: Nm) -> Result<(), ParamError> {
        if thickness_nm <= 0 {
            return Err(ParamError("board thickness must be greater than zero"));
        }
        if !self.kerf_mm.is_finite() || self.kerf_mm <= 0.0 {
            return Err(ParamError("kerf_mm must be finite and greater than zero"));
        }
        if !self.tab_mm.is_finite() || self.tab_mm < 0.0 {
            return Err(ParamError("tab_mm must be finite and non-negative"));
        }
        if !self.mm_per_pass.is_finite() || self.mm_per_pass <= 0.0 {
            return Err(ParamError(
                "mm_per_pass must be finite and greater than zero",
            ));
        }
        if !self.z_step_mm.is_finite() || self.z_step_mm <= 0.0 {
            return Err(ParamError("z_step_mm must be finite and greater than zero"));
        }
        if !self.overcut_mm.is_finite() || self.overcut_mm < 0.0 {
            return Err(ParamError("overcut_mm must be finite and non-negative"));
        }
        Ok(())
    }
}

/// One focus step of a through-cut: run `passes` passes at the current focal
/// plane, then lower the head (or raise the bed) by `focus_drop_mm` so focus
/// tracks the descending cut floor. `focus_drop_mm` is `0.0` on the final
/// step (the cut is through; nothing follows).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutStep {
    pub passes: u32,
    pub focus_drop_mm: f64,
}

/// The full focus schedule for a through-cut: an ordered list of steps whose
/// commanded depth reaches `total_depth_mm` (board thickness + overcut).
#[derive(Debug, Clone, PartialEq)]
pub struct CutSchedule {
    pub steps: Vec<CutStep>,
    /// Total commanded cut depth, mm (thickness + overcut).
    pub total_depth_mm: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mm_round_trip_is_lossless_at_pcb_scale() {
        let p = P::from_mm(123.456789, -87.654321);
        assert_eq!(p.x, 123_456_789);
        assert_eq!(p.y, -87_654_321);
        assert!((p.x_mm() - 123.456789).abs() < 1e-12);
    }

    #[test]
    fn i64_headroom_at_one_meter() {
        let p = P::new(1_000_000_000, -1_000_000_000); // ±1 m in nm
        assert_eq!(p.x.checked_mul(4), Some(4_000_000_000)); // room to spare
        let _ = p;
    }

    #[test]
    fn laser_params_reject_noops_and_non_finite_values() {
        let mut params = AblationParams {
            power_pct: 20.0,
            speed_mm_s: 1000.0,
            frequency_khz: 30.0,
            pulse_ns: 0,
            passes: 1,
        };
        assert!(params.validate().is_ok());
        params.passes = 0;
        assert!(params.validate().is_err());
        params.passes = 1;
        params.speed_mm_s = f64::NAN;
        assert!(params.validate().is_err());
    }
}
