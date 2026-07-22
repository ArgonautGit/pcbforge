use super::*;

/// Which view the central panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CentralTab {
    Job,
    Camera,
    Calibrate,
    Fiducials,
    Place,
}

/// Which face of a (possibly double-sided) board the operator is working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Side {
    Front,
    Back,
}

/// Which calibration the operator is doing on the Calibrate tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CalibMode {
    /// Printed reference grid → lens-distortion map (camera becomes a ruler).
    CameraLens,
    /// Burned grid → the laser anchor (camera-px → commanded machine mm).
    LaserAnchor,
    /// Burned grid through the metric camera → the laser field pre-distortion.
    LaserField,
    /// Auto-lay four fiducial holes for a board centred on the field, generated
    /// with the laser-field pre-distortion so spacing/size burn true.
    FidHoles,
}

/// Correction for how the camera is physically mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Orientation {
    Normal,
    FlipH,
    FlipV,
    Rotate180,
}

impl Orientation {
    pub(super) const ALL: [Orientation; 4] = [
        Orientation::Normal,
        Orientation::FlipH,
        Orientation::FlipV,
        Orientation::Rotate180,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Orientation::Normal => "Normal",
            Orientation::FlipH => "Flip ↔ (mirror)",
            Orientation::FlipV => "Flip ↕",
            Orientation::Rotate180 => "Rotate 180° (upside down)",
        }
    }

    pub(super) fn token(self) -> &'static str {
        match self {
            Orientation::Normal => "normal",
            Orientation::FlipH => "flip_h",
            Orientation::FlipV => "flip_v",
            Orientation::Rotate180 => "rotate180",
        }
    }

    pub(super) fn from_token(s: &str) -> Option<Orientation> {
        Orientation::ALL.into_iter().find(|o| o.token() == s)
    }

    pub(super) fn apply(self, img: image::GrayImage) -> image::GrayImage {
        use image::imageops;
        match self {
            Orientation::Normal => img,
            Orientation::FlipH => imageops::flip_horizontal(&img),
            Orientation::FlipV => imageops::flip_vertical(&img),
            Orientation::Rotate180 => imageops::rotate180(&img),
        }
    }
}

pub(super) struct RuntimeState {
    pub(super) settings_path: PathBuf,
    pub(super) last_settings: String,
    pub(super) settings_error: Option<String>,
    pub(super) status: StatusSnapshot,
    pub(super) log: Vec<LogLine>,
    pub(super) tab: CentralTab,
    pub(super) verb_job: Option<VerbJob>,
    /// A "run in LightBurn" queued to fire when `verb_job` finishes (the
    /// absolute .lbrn2 the export is writing). Cleared once consumed or skipped.
    pub(super) pending_lightburn: Option<PathBuf>,
    /// The active (or most recent) LightBurn run — kept after it finishes so the
    /// terminal state stays visible; a new run replaces it.
    pub(super) lightburn_run: Option<LightburnRun>,
}

pub(super) struct JobState {
    pub(super) kicad_project: String,
    pub(super) emit_copper: String,
    pub(super) emit_outline: String,
    pub(super) emit_lbrn2: String,
    pub(super) offset_mm: f64,
    pub(super) side: Side,
    pub(super) back_copper: String,
    pub(super) back_outline: String,
    pub(super) board_thickness_mm: f64,
    pub(super) focal_mm: f64,
    pub(super) scan_center_auto: bool,
    pub(super) scan_center_mm: (f64, f64),
    pub(super) speed_mm_s: f64,
    pub(super) frequency_khz: f64,
    pub(super) pulse_ns: u32,
    pub(super) interval_mm: f64,
    pub(super) passes: u32,
    pub(super) preview_tex: Option<TextureHandle>,
    pub(super) preview_note: String,
}

pub(super) struct CameraState {
    pub(super) use_device: bool,
    pub(super) device: u32,
    pub(super) file: String,
    pub(super) orientation: Orientation,
    pub(super) live: bool,
    pub(super) tex: Option<TextureHandle>,
    pub(super) note: String,
    pub(super) last: Option<image::GrayImage>,
    pub(super) view_scale: f64,
    pub(super) devices: Vec<(u32, String)>,
    pub(super) show_bed: bool,
    pub(super) field_mm: f32,
    pub(super) field_center_auto: bool,
    pub(super) field_cx_mm: f32,
    pub(super) field_cy_mm: f32,
    pub(super) capture: Option<crate::camera::Capture>,
    pub(super) capture_src: Option<crate::camera::Source>,
}

/// One grid-parameter set (dots per side, pitch, dot size, contrast). The ①
/// printed paper and the ②③ burned grid each get their own — sharing one set
/// across the steps let a burned-grid pitch silently mis-scale the ① metric
/// ruler (and vice versa).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GridParams {
    pub(super) n: usize,
    pub(super) pitch_mm: f64,
    pub(super) dot_mm: f64,
    pub(super) dot_kind: crate::calib::DotKind,
}

pub(super) struct CalibrationState {
    pub(super) anchor: Option<crate::calib::Calibration>,
    pub(super) saved_at: Option<u64>,
    /// ① printed paper: the MEASURED pitch (calipers — printers scale).
    pub(super) paper: GridParams,
    /// ②③ burned grid: the COMMANDED pitch.
    pub(super) burn: GridParams,
    pub(super) grid_origin_mm: (f64, f64),
    pub(super) grid_out: String,
    /// ① printed-paper grid SVG output path (`paper-grid` verb).
    pub(super) paper_out: String,
    pub(super) frame: String,
    pub(super) frame_img: Option<image::GrayImage>,
    pub(super) frame_tex: Option<TextureHandle>,
    pub(super) corners: Vec<(f64, f64)>,
    pub(super) mode: CalibMode,
    pub(super) lens: Option<crate::calib::CameraCal>,
    pub(super) lens_frame_signature: Option<((u32, u32), Orientation)>,
    pub(super) field: Option<crate::calib::FieldCal>,
    pub(super) field_accepted: bool,
    /// Operator-configurable step 3 laser-field acceptance limits (µm): a fit
    /// whose residual RMS / worst per-dot error exceeds these is rejected. The
    /// defaults accept the rig's demonstrated measurement floor.
    pub(super) accept_rms_um: f64,
    pub(super) accept_worst_um: f64,
    /// Operator opt-in: absorb a large uniform burn-vs-paper scale (an oversized
    /// machine field) into the ③ field correction instead of refusing the fit.
    /// Off by default — fixing the field size in LightBurn is the cleaner fix.
    pub(super) allow_machine_scale: bool,
    pub(super) lens_arrow_scale: f32,
    pub(super) anchor_resid_scale: f32,
    pub(super) edit_anchor_dots: bool,
    /// Per-session view toggle: when off, the post-fit feedback overlays (lens
    /// arrows, anchor mesh/residuals, field lattice + REJECTED banner) are
    /// hidden so the operator can see the bare dots to re-click the 4 corners.
    /// Not persisted — like `corners`, it resets with each session/frame.
    pub(super) show_fit_feedback: bool,
    pub(super) live: bool,
    pub(super) capture: Option<crate::camera::Capture>,
    pub(super) capture_src: Option<crate::camera::Source>,
    pub(super) note: String,
}

impl CalibrationState {
    /// The grid-parameter set the active step edits and fits with: ① Camera
    /// lens uses the paper set, ②③ use the burned-grid set.
    pub(super) fn active_params_mut(&mut self) -> &mut GridParams {
        match self.mode {
            CalibMode::CameraLens => &mut self.paper,
            _ => &mut self.burn,
        }
    }
}

pub(super) struct FiducialState {
    pub(super) frame: String,
    pub(super) layout: String,
    pub(super) px_per_mm: f64,
    pub(super) shape: crate::fiducial::ShapeKind,
    /// Circle diameter, or rectangle width (`shape == Rect`).
    pub(super) diameter_mm: f64,
    /// Rectangle height (ignored when `shape == Circle`).
    pub(super) height_mm: f64,
    pub(super) search_mm: f64,
    pub(super) profile: crate::fiducial::ProfileKind,
    /// Output path for the generated fiducial-holes .lbrn2 (`fid-holes` verb).
    pub(super) out: String,
    /// ④ board width for the auto fiducial-hole layout (mm).
    pub(super) board_w_mm: f64,
    /// ④ board height for the auto fiducial-hole layout (mm).
    pub(super) board_h_mm: f64,
    /// ④ margin from board edge to hole CENTRE for the auto layout (mm).
    pub(super) margin_mm: f64,
    pub(super) click_place: bool,
    pub(super) note: String,
    pub(super) rows: Vec<FidRow>,
    pub(super) measured_ppm: Option<f64>,
    pub(super) frame_img: Option<image::GrayImage>,
    pub(super) frame_tex: Option<TextureHandle>,
    pub(super) search: Vec<(f64, f64)>,
    pub(super) found: Vec<Option<(f64, f64)>>,
    pub(super) drag: Option<usize>,
    pub(super) homography: Option<vision::Homography>,
    pub(super) live: bool,
    pub(super) capture: Option<crate::camera::Capture>,
    pub(super) capture_src: Option<crate::camera::Source>,
}

pub(super) struct PlacementState {
    pub(super) frame: String,
    pub(super) lbrn2: String,
    pub(super) px_per_mm: f64,
    pub(super) tx_mm: f64,
    pub(super) ty_mm: f64,
    pub(super) rot_deg: f64,
    pub(super) job: Vec<pcb_core::Poly>,
    pub(super) frame_img: Option<image::GrayImage>,
    /// The frame pre-converted to RGBA, cached so each drag-step recompose
    /// clones it instead of redoing the full-frame gray→color conversion.
    pub(super) base_rgba: Option<ColorImage>,
    pub(super) pivot: (f64, f64),
    pub(super) tex: Option<TextureHandle>,
    pub(super) note: String,
    pub(super) field_correct: bool,
    /// LightBurn device name for the one-click "Etch + run in LightBurn".
    pub(super) lightburn_device: String,
}

pub(super) struct ArState {
    pub(super) overlay: bool,
    pub(super) show_board: bool,
    pub(super) show_copper: bool,
    pub(super) show_ablate: bool,
    pub(super) board: Vec<pcb_core::Poly>,
    pub(super) copper: Vec<pcb_core::Poly>,
    pub(super) ablate: Vec<pcb_core::Poly>,
    pub(super) note: String,
}

pub(super) struct ViewState {
    pub(super) images: std::collections::HashMap<&'static str, crate::imgview::ImageView>,
}
