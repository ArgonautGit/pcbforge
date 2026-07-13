//! ING-5 — board metadata from the `.gbrjob` sidecar KiCad writes next to
//! exported gerbers.
//!
//! KiCad 7's job file (Gerber Job Format, JSON) carries the board-level
//! facts the rest of the pipeline needs under `GeneralSpecs`:
//!
//! ```json
//! "GeneralSpecs": {
//!   "Size": { "X": 36.05, "Y": 30.05 },
//!   "LayerNumber": 2,
//!   "BoardThickness": 1.6
//! }
//! ```
//!
//! Sizes and thickness are millimeters; [`load_gbrjob`] converts them to
//! integer nanometers (rounded to nearest). Note that KiCad computes `Size`
//! from the Edge.Cuts *drawn* bounding box, so the outline stroke width is
//! included — a 36 x 30 mm outline drawn with a 0.05 mm stroke reports
//! 36.05 x 30.05.

use std::fmt;
use std::path::Path;

use pcb_core::{NM_PER_MM, Nm};
use serde_json::Value;

/// Board-level metadata parsed from a `.gbrjob` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardMeta {
    /// Bounding-box width of the drawn outline (stroke included), nm.
    pub size_x_nm: Nm,
    /// Bounding-box height of the drawn outline (stroke included), nm.
    pub size_y_nm: Nm,
    /// Number of copper layers (`GeneralSpecs.LayerNumber`).
    pub copper_layers: u32,
    /// Finished board thickness, nm.
    pub thickness_nm: Nm,
}

/// Errors from reading or interpreting a `.gbrjob` file.
#[derive(Debug)]
pub enum GbrjobError {
    /// The file could not be read.
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    /// The file is not valid JSON.
    Json(serde_json::Error),
    /// A required field is absent. The string is the dotted path,
    /// e.g. `GeneralSpecs.Size.X`.
    Missing(&'static str),
    /// A field is present but has the wrong type or an out-of-range value.
    Malformed {
        field: &'static str,
        expected: &'static str,
        found: String,
    },
}

impl fmt::Display for GbrjobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GbrjobError::Io { path, source } => {
                write!(f, "cannot read gbrjob file {}: {source}", path.display())
            }
            GbrjobError::Json(e) => write!(f, "gbrjob is not valid JSON: {e}"),
            GbrjobError::Missing(field) => {
                write!(f, "gbrjob is missing required field {field}")
            }
            GbrjobError::Malformed {
                field,
                expected,
                found,
            } => write!(
                f,
                "gbrjob field {field}: expected {expected}, found {found}"
            ),
        }
    }
}

impl std::error::Error for GbrjobError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GbrjobError::Io { source, .. } => Some(source),
            GbrjobError::Json(e) => Some(e),
            _ => None,
        }
    }
}

/// Read and parse a `.gbrjob` file into [`BoardMeta`].
pub fn load_gbrjob(path: &Path) -> Result<BoardMeta, GbrjobError> {
    let text = std::fs::read_to_string(path).map_err(|source| GbrjobError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_gbrjob(&text)
}

/// Parse `.gbrjob` JSON text into [`BoardMeta`].
pub fn parse_gbrjob(text: &str) -> Result<BoardMeta, GbrjobError> {
    let root: Value = serde_json::from_str(text).map_err(GbrjobError::Json)?;
    let specs = root
        .get("GeneralSpecs")
        .ok_or(GbrjobError::Missing("GeneralSpecs"))?;
    let size = specs
        .get("Size")
        .ok_or(GbrjobError::Missing("GeneralSpecs.Size"))?;

    let size_x_nm = mm_field(size.get("X"), "GeneralSpecs.Size.X")?;
    let size_y_nm = mm_field(size.get("Y"), "GeneralSpecs.Size.Y")?;
    let thickness_nm = mm_field(specs.get("BoardThickness"), "GeneralSpecs.BoardThickness")?;
    let copper_layers = layer_number(specs.get("LayerNumber"))?;

    Ok(BoardMeta {
        size_x_nm,
        size_y_nm,
        copper_layers,
        thickness_nm,
    })
}

/// A required millimeter-valued field, converted to nanometers
/// (rounded to nearest).
fn mm_field(value: Option<&Value>, field: &'static str) -> Result<Nm, GbrjobError> {
    let value = value.ok_or(GbrjobError::Missing(field))?;
    let mm = value.as_f64().ok_or_else(|| GbrjobError::Malformed {
        field,
        expected: "a number (mm)",
        found: value.to_string(),
    })?;
    if !mm.is_finite() {
        return Err(GbrjobError::Malformed {
            field,
            expected: "a finite number (mm)",
            found: value.to_string(),
        });
    }
    Ok((mm * NM_PER_MM as f64).round() as Nm)
}

/// `GeneralSpecs.LayerNumber` — a non-negative integer copper-layer count.
fn layer_number(value: Option<&Value>) -> Result<u32, GbrjobError> {
    const FIELD: &str = "GeneralSpecs.LayerNumber";
    let value = value.ok_or(GbrjobError::Missing(FIELD))?;
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| GbrjobError::Malformed {
            field: FIELD,
            expected: "a non-negative integer",
            found: value.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kicad_cli::{KicadCli, available};
    use std::path::PathBuf;

    /// Minimal but structurally faithful copy of what KiCad 7.0.11 writes
    /// for samples/kicad/valdemo2.kicad_pcb.
    const VALDEMO2_LIKE: &str = r#"{
        "Header": {
            "GenerationSoftware": { "Vendor": "KiCad", "Application": "Pcbnew" }
        },
        "GeneralSpecs": {
            "ProjectId": { "Name": "valdemo2" },
            "Size": { "X": 36.05, "Y": 30.05 },
            "LayerNumber": 2,
            "BoardThickness": 1.6,
            "Finish": "None"
        }
    }"#;

    #[test]
    fn parses_known_static_json() {
        let meta = parse_gbrjob(VALDEMO2_LIKE).unwrap();
        assert_eq!(meta.size_x_nm, 36_050_000);
        assert_eq!(meta.size_y_nm, 30_050_000);
        assert_eq!(meta.copper_layers, 2);
        assert_eq!(meta.thickness_nm, 1_600_000);
    }

    #[test]
    fn not_json_is_a_json_error() {
        let err = parse_gbrjob("this is not json").unwrap_err();
        assert!(matches!(err, GbrjobError::Json(_)), "got: {err}");
    }

    #[test]
    fn missing_general_specs_is_named() {
        let err = parse_gbrjob(r#"{ "Header": {} }"#).unwrap_err();
        assert!(matches!(err, GbrjobError::Missing("GeneralSpecs")));
        assert!(err.to_string().contains("GeneralSpecs"), "got: {err}");
    }

    #[test]
    fn missing_size_component_is_named() {
        let err = parse_gbrjob(
            r#"{ "GeneralSpecs": { "Size": { "X": 36.05 },
                 "LayerNumber": 2, "BoardThickness": 1.6 } }"#,
        )
        .unwrap_err();
        assert!(matches!(err, GbrjobError::Missing("GeneralSpecs.Size.Y")));
    }

    #[test]
    fn non_numeric_size_is_malformed() {
        let err = parse_gbrjob(
            r#"{ "GeneralSpecs": { "Size": { "X": "wide", "Y": 30.05 },
                 "LayerNumber": 2, "BoardThickness": 1.6 } }"#,
        )
        .unwrap_err();
        match &err {
            GbrjobError::Malformed { field, found, .. } => {
                assert_eq!(*field, "GeneralSpecs.Size.X");
                assert_eq!(found, "\"wide\"");
            }
            other => panic!("expected Malformed, got: {other}"),
        }
        let msg = err.to_string();
        assert!(msg.contains("GeneralSpecs.Size.X"), "got: {msg}");
    }

    #[test]
    fn bad_layer_number_is_malformed() {
        for bad in ["-2", "1.5", "\"two\""] {
            let text = format!(
                r#"{{ "GeneralSpecs": {{ "Size": {{ "X": 1, "Y": 1 }},
                     "LayerNumber": {bad}, "BoardThickness": 1.6 }} }}"#
            );
            let err = parse_gbrjob(&text).unwrap_err();
            assert!(
                matches!(
                    &err,
                    GbrjobError::Malformed { field, .. }
                        if *field == "GeneralSpecs.LayerNumber"
                ),
                "LayerNumber {bad}: {err}"
            );
        }
    }

    #[test]
    fn unreadable_file_is_an_io_error() {
        let err = load_gbrjob(Path::new("/nonexistent/nope-job.gbrjob")).unwrap_err();
        match err {
            GbrjobError::Io { path, .. } => {
                assert!(path.ends_with("nope-job.gbrjob"));
            }
            other => panic!("expected Io, got: {other}"),
        }
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn live_export_sidecar_matches_known_board() {
        if !available() {
            eprintln!("SKIP: kicad-cli not installed");
            return;
        }
        let cli = KicadCli::discover().unwrap();
        let board = repo_root().join("samples/kicad/valdemo2.kicad_pcb");
        assert!(board.is_file(), "sample board missing");

        let dir = std::env::temp_dir().join(format!("pcbforge-ing5-gbrjob-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        cli.export_gerbers(&board, &["F.Cu", "B.Cu", "Edge.Cuts"], &dir)
            .unwrap();

        // kicad-cli names the sidecar <proj>-job.gbrjob next to the gerbers.
        let sidecar = dir.join("valdemo2-job.gbrjob");
        assert!(sidecar.is_file(), "gbrjob sidecar not written");

        let meta = load_gbrjob(&sidecar).unwrap();
        // The valdemo2 outline is 36 x 30 mm, but KiCad's Size is the drawn
        // Edge.Cuts bounding box, which includes the 0.05 mm outline stroke
        // width (0.025 mm on each side) — so KiCad writes 36.05 x 30.05.
        assert_eq!(meta.size_x_nm, 36_050_000);
        assert_eq!(meta.size_y_nm, 30_050_000);
        assert_eq!(meta.copper_layers, 2);
        assert_eq!(meta.thickness_nm, 1_600_000);
    }
}
