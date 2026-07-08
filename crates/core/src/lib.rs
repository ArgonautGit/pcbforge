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
}
