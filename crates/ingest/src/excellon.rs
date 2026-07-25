//! ING-2 — Excellon drill ingest.
//!
//! Parses the Excellon dialect `kicad-cli pcb export drill` (KiCad 7)
//! produces: decimal-coordinate, absolute, metric (or inch) files with an
//! `M48` header, `TnC<dia>` tool definitions, `%` header end, `G90`/`G05`
//! body, plain `Xx.xYy.y` hits, and `X1Y1G85X2Y2` oval slots. Coordinates
//! are kept verbatim (KiCad plots y-down, so Y is negative); the Gerber
//! ingest uses the same frame, so drills register against copper as-is.
//!
//! Coordinates and diameters are decimal strings in file units; they are
//! converted to integer nanometers by parsing the digits directly (exact
//! integer arithmetic, no f64 round-trip), so e.g. `102.54` mm is exactly
//! `102_540_000` nm.
//!
//! Anything outside this dialect — non-decimal formats (`FMAT,1`, zero
//! suppression qualifiers), tool-definition qualifiers (feed/speed/repeat
//! codes), `R` repeat codes, incremental mode (`G91`), G-codes other than
//! `G90`/`G05`/`G85` — is a hard [`ExcellonError`] naming the construct,
//! never a silent guess.
//!
//! # API deviation from the backlog
//!
//! The backlog return type `Vec<(P, Nm)>` cannot represent G85 slots, so
//! this module provides both:
//!
//! * [`load_excellon`] — the backlog API. Round holes only; a slot is
//!   returned as its two endpoint holes. This suits the drill-guide use
//!   case, where a slot is made by drilling both ends and filing the web.
//! * [`load_excellon_full`] — the lossless form, returning [`DrillOp`]s
//!   that keep slots as slots.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use pcb_core::{Nm, P};

/// Nanometers per millimeter / per inch (file-unit scales).
const NM_PER_MM: i64 = pcb_core::NM_PER_MM;
const NM_PER_INCH: i64 = 25_400_000;

/// Parse error with 1-based line information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcellonError {
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for ExcellonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "excellon parse error (line {}): {}", self.line, self.msg)
    }
}

impl std::error::Error for ExcellonError {}

/// One drill operation, lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrillOp {
    /// A round hole.
    Hole { center: P, diameter_nm: Nm },
    /// A G85 oval slot from `a` to `b` at the tool diameter.
    Slot { a: P, b: P, diameter_nm: Nm },
}

/// Load an Excellon drill file as `(center, diameter)` round holes.
///
/// This is the backlog ING-2 API. A G85 slot cannot be represented in this
/// return type, so it contributes its two endpoint holes (the drill-guide
/// workflow treats a slot as drill-then-drill-then-file). Use
/// [`load_excellon_full`] when slots must stay slots.
pub fn load_excellon(path: &Path) -> Result<Vec<(P, Nm)>, ExcellonError> {
    Ok(load_excellon_full(path)?
        .into_iter()
        .flat_map(|op| match op {
            DrillOp::Hole {
                center,
                diameter_nm,
            } => vec![(center, diameter_nm)],
            DrillOp::Slot { a, b, diameter_nm } => vec![(a, diameter_nm), (b, diameter_nm)],
        })
        .collect())
}

/// Load an Excellon drill file losslessly (holes and slots).
pub fn load_excellon_full(path: &Path) -> Result<Vec<DrillOp>, ExcellonError> {
    let src = std::fs::read_to_string(path).map_err(|e| ExcellonError {
        line: 0,
        msg: format!("cannot read {}: {e}", path.display()),
    })?;
    parse_excellon(&src)
}

/// Parse Excellon source text losslessly (holes and slots).
pub fn parse_excellon(src: &str) -> Result<Vec<DrillOp>, ExcellonError> {
    let mut p = Parser::default();
    for (i, raw) in src.lines().enumerate() {
        p.line = i + 1;
        let stmt = raw.trim();
        if stmt.is_empty() || stmt.starts_with(';') {
            continue; // comments, including "#@!" attribute comments
        }
        p.statement(stmt)?;
        if p.ended {
            break;
        }
    }
    if !p.saw_m48 {
        return Err(ExcellonError {
            line: 1,
            msg: "not an Excellon drill file: missing M48 header start".into(),
        });
    }
    if !p.ended {
        return Err(ExcellonError {
            line: src.lines().count(),
            msg: "missing M30 end-of-file".into(),
        });
    }
    Ok(p.ops)
}

#[derive(Default)]
struct Parser {
    line: usize,
    /// nm per file unit; None until METRIC / INCH.
    scale_nm: Option<i64>,
    tools: HashMap<u32, Nm>,
    current: Option<Nm>,
    saw_m48: bool,
    in_header: bool,
    ended: bool,
    ops: Vec<DrillOp>,
}

impl Parser {
    fn err(&self, msg: impl Into<String>) -> ExcellonError {
        ExcellonError {
            line: self.line,
            msg: msg.into(),
        }
    }

    fn statement(&mut self, stmt: &str) -> Result<(), ExcellonError> {
        if !self.saw_m48 {
            if stmt == "M48" {
                self.saw_m48 = true;
                self.in_header = true;
                return Ok(());
            }
            return Err(self.err(format!(
                "expected M48 header start, found '{stmt}' — not an Excellon drill file"
            )));
        }
        if self.in_header {
            self.header_statement(stmt)
        } else {
            self.body_statement(stmt)
        }
    }

    fn header_statement(&mut self, stmt: &str) -> Result<(), ExcellonError> {
        match stmt {
            "%" | "M95" => {
                self.in_header = false;
                Ok(())
            }
            "FMAT,2" => Ok(()),
            "METRIC" => {
                self.scale_nm = Some(NM_PER_MM);
                Ok(())
            }
            "INCH" => {
                self.scale_nm = Some(NM_PER_INCH);
                Ok(())
            }
            _ if stmt.starts_with("FMAT") => Err(self.err(format!(
                "unsupported format declaration '{stmt}' (only FMAT,2)"
            ))),
            _ if stmt.starts_with("METRIC") || stmt.starts_with("INCH") => Err(self.err(format!(
                "unsupported units qualifier '{stmt}' (only plain METRIC / INCH decimal format)"
            ))),
            _ if stmt.starts_with('T') => self.tool_definition(stmt),
            _ => Err(self.err(format!("unknown header statement '{stmt}'"))),
        }
    }

    /// Header `Tn C<dia>` tool definition, diameter in decimal file units.
    fn tool_definition(&mut self, stmt: &str) -> Result<(), ExcellonError> {
        let rest = &stmt[1..];
        let split = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let num: u32 = rest[..split]
            .parse()
            .map_err(|_| self.err(format!("malformed tool number in '{stmt}'")))?;
        let dia_str = rest[split..].strip_prefix('C').ok_or_else(|| {
            self.err(format!(
                "unsupported tool definition '{stmt}' (expected T{num}C<diameter>)"
            ))
        })?;
        if !dia_str
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '+' || c == '-')
        {
            return Err(self.err(format!(
                "unsupported tool definition qualifier after diameter in '{stmt}' \
                 (feed/speed/repeat codes not supported)"
            )));
        }
        let scale = self
            .scale_nm
            .ok_or_else(|| self.err(format!("tool definition '{stmt}' before METRIC/INCH")))?;
        require_decimal_point("tool diameter", dia_str).map_err(|e| self.err(e))?;
        let dia = decimal_to_nm(dia_str, scale)
            .map_err(|e| self.err(format!("tool T{num} diameter: {e}")))?;
        if dia <= 0 {
            return Err(self.err(format!("tool T{num} diameter must be positive")));
        }
        self.tools.insert(num, dia);
        Ok(())
    }

    fn body_statement(&mut self, stmt: &str) -> Result<(), ExcellonError> {
        match stmt {
            "G90" => return Ok(()), // absolute mode — the only mode we accept
            "G91" => return Err(self.err("incremental coordinate mode (G91) unsupported")),
            "G05" => return Ok(()), // drill mode
            "M30" => {
                self.ended = true;
                return Ok(());
            }
            _ => {}
        }
        match stmt.as_bytes()[0] {
            b'G' => Err(self.err(format!(
                "unsupported G-code '{stmt}' (only G90, G05, and inline G85 slots)"
            ))),
            b'M' => Err(self.err(format!("unsupported M-code '{stmt}'"))),
            b'R' => Err(self.err(format!("repeat code '{stmt}' unsupported"))),
            b'T' => self.tool_select(stmt),
            b'X' | b'Y' => self.hit(stmt),
            _ => Err(self.err(format!("unknown statement '{stmt}'"))),
        }
    }

    fn tool_select(&mut self, stmt: &str) -> Result<(), ExcellonError> {
        let num: u32 = stmt[1..].parse().map_err(|_| {
            self.err(format!(
                "unsupported tool statement '{stmt}' in body (expected plain Tn select)"
            ))
        })?;
        if num == 0 {
            self.current = None; // T0: tool unload
            return Ok(());
        }
        let dia = *self
            .tools
            .get(&num)
            .ok_or_else(|| self.err(format!("select of undefined tool T{num}")))?;
        self.current = Some(dia);
        Ok(())
    }

    /// `Xx.xYy.y` hole hit, or `X1Y1G85X2Y2` slot.
    fn hit(&mut self, stmt: &str) -> Result<(), ExcellonError> {
        let diameter_nm = self
            .current
            .ok_or_else(|| self.err("drill hit with no tool selected"))?;
        let (a, rest) = self.coordinate_pair(stmt, stmt)?;
        if rest.is_empty() {
            self.ops.push(DrillOp::Hole {
                center: a,
                diameter_nm,
            });
            return Ok(());
        }
        let rest = rest.strip_prefix("G85").ok_or_else(|| {
            self.err(format!(
                "unexpected trailing '{rest}' after coordinates in '{stmt}'"
            ))
        })?;
        let (b, tail) = self.coordinate_pair(rest, stmt)?;
        if !tail.is_empty() {
            return Err(self.err(format!(
                "unexpected trailing '{tail}' after G85 slot end in '{stmt}'"
            )));
        }
        self.ops.push(DrillOp::Slot { a, b, diameter_nm });
        Ok(())
    }

    /// Parse a leading `X<dec>Y<dec>` pair; returns the point and the rest.
    fn coordinate_pair<'a>(&self, s: &'a str, whole: &str) -> Result<(P, &'a str), ExcellonError> {
        let scale = self
            .scale_nm
            .ok_or_else(|| self.err("coordinate data before METRIC/INCH"))?;
        let (x_str, rest) = take_field(s, 'X').ok_or_else(|| {
            self.err(format!(
                "expected X coordinate in '{whole}' (modal/partial coordinates unsupported)"
            ))
        })?;
        let (y_str, rest) = take_field(rest, 'Y').ok_or_else(|| {
            self.err(format!(
                "expected Y coordinate in '{whole}' (modal/partial coordinates unsupported)"
            ))
        })?;
        require_decimal_point("X coordinate", x_str).map_err(|e| self.err(e))?;
        require_decimal_point("Y coordinate", y_str).map_err(|e| self.err(e))?;
        let x = decimal_to_nm(x_str, scale)
            .map_err(|e| self.err(format!("X coordinate in '{whole}': {e}")))?;
        let y = decimal_to_nm(y_str, scale)
            .map_err(|e| self.err(format!("Y coordinate in '{whole}': {e}")))?;
        Ok((P::new(x, y), rest))
    }
}

/// If `s` starts with `letter`, split off the decimal number that follows.
fn take_field(s: &str, letter: char) -> Option<(&str, &str)> {
    let rest = s.strip_prefix(letter)?;
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '+' || c == '-'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some((&rest[..end], &rest[end..]))
}

/// A coordinate/diameter field must carry an explicit decimal point. A bare
/// integer is the signature of a zero-suppressed / fixed-format file this
/// parser can't disambiguate — `X001234` would silently read as 1234 mm
/// instead of, say, 1.234 mm (LR-32). (The general [`decimal_to_nm`] converter
/// still accepts whole numbers; only file coordinate/diameter fields are gated.)
fn require_decimal_point(kind: &str, s: &str) -> Result<(), String> {
    if s.contains('.') {
        Ok(())
    } else {
        Err(format!(
            "{kind} '{s}' has no decimal point — only explicit-decimal Excellon \
             is supported (zero-suppressed / fixed-format coordinates are ambiguous)"
        ))
    }
}

/// Convert a decimal string in file units to integer nanometers exactly.
///
/// The digits are parsed directly and scaled with integer arithmetic —
/// no f64 round-trip — so any value with up to 9 fractional digits maps to
/// the mathematically nearest nanometer (ties away from zero); values whose
/// fraction divides the unit scale (e.g. any ≤6-digit mm fraction) convert
/// exactly.
fn decimal_to_nm(s: &str, scale_nm: i64) -> Result<Nm, String> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int_part, frac_part) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(format!("malformed decimal number '{s}'"));
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(format!("malformed decimal number '{s}'"));
    }
    if frac_part.len() > 9 {
        return Err(format!("more than 9 fractional digits in '{s}'"));
    }
    let int_val: i128 = if int_part.is_empty() {
        0
    } else {
        int_part
            .parse()
            .map_err(|_| format!("integer part of '{s}' out of range"))?
    };
    // `int_part` may carry up to 38 digits, so the scale multiply can overflow
    // even i128 — it must not wrap into a plausible-looking coordinate.
    let mut total = int_val
        .checked_mul(scale_nm as i128)
        .ok_or_else(|| format!("'{s}' out of i64 nanometer range"))?;
    if !frac_part.is_empty() {
        let frac_val: i128 = frac_part.parse().expect("all-digit string");
        let den = 10_i128.pow(frac_part.len() as u32);
        // Round half away from zero (sign applied below, so value is >= 0).
        total = total
            .checked_add((frac_val * scale_nm as i128 + den / 2) / den)
            .ok_or_else(|| format!("'{s}' out of i64 nanometer range"))?;
    }
    if neg {
        total = -total;
    }
    Nm::try_from(total).map_err(|_| format!("'{s}' out of i64 nanometer range"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod decimal_tests {
    use super::*;

    #[test]
    fn coordinate_and_diameter_fields_require_a_decimal_point() {
        // Zero-suppressed / fixed-format fields are ambiguous and must be
        // rejected, not misread as whole millimeters (LR-32) — while the
        // general converter still accepts whole numbers.
        assert!(require_decimal_point("X coordinate", "1234").is_err());
        assert!(require_decimal_point("tool diameter", "1").is_err());
        assert!(require_decimal_point("X coordinate", "1.234").is_ok());
        assert!(require_decimal_point("Y coordinate", "-2.5").is_ok());
        assert_eq!(decimal_to_nm("3", NM_PER_MM), Ok(3_000_000));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kicad_cli;
    use std::path::PathBuf;

    /// The dialect sample from the backlog (statically, no kicad needed).
    const SAMPLE: &str = "\
M48
; DRILL file {KiCad 7.0.11} ...
; FORMAT={-:-/ absolute / metric / decimal}
; #@! TF.CreationDate,2026-01-01T00:00:00+00:00
FMAT,2
METRIC
; #@! TA.AperFunction,Plated,PTH,ViaDrill
T1C0.400
T2C1.000
%
G90
G05
T1
X108.0Y-111.0
X110.0Y-100.0
T2
X100.0Y-100.0
X102.54Y-100.0
T2
X105.08Y-99.8G85X105.08Y-100.2
G05
M30
";

    fn mm(v: f64) -> Nm {
        // Test-only helper; all asserted values are exact in nm.
        (v * NM_PER_MM as f64).round() as Nm
    }

    #[test]
    fn parses_the_kicad_dialect_sample() {
        let ops = parse_excellon(SAMPLE).expect("parse");
        assert_eq!(
            ops,
            vec![
                DrillOp::Hole {
                    center: P::new(108_000_000, -111_000_000),
                    diameter_nm: 400_000,
                },
                DrillOp::Hole {
                    center: P::new(110_000_000, -100_000_000),
                    diameter_nm: 400_000,
                },
                DrillOp::Hole {
                    center: P::new(100_000_000, -100_000_000),
                    diameter_nm: 1_000_000,
                },
                DrillOp::Hole {
                    center: P::new(102_540_000, -100_000_000),
                    diameter_nm: 1_000_000,
                },
                DrillOp::Slot {
                    a: P::new(105_080_000, -99_800_000),
                    b: P::new(105_080_000, -100_200_000),
                    diameter_nm: 1_000_000,
                },
            ]
        );
    }

    #[test]
    fn backlog_api_flattens_slots_to_endpoint_holes() {
        let dir = std::env::temp_dir().join(format!("pcbforge-ing2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.drl");
        std::fs::write(&path, SAMPLE).unwrap();
        let holes = load_excellon(&path).expect("load");
        assert_eq!(holes.len(), 6, "4 holes + slot as 2 endpoint holes");
        assert_eq!(holes[4], (P::new(105_080_000, -99_800_000), 1_000_000));
        assert_eq!(holes[5], (P::new(105_080_000, -100_200_000), 1_000_000));
    }

    #[test]
    fn decimal_conversion_is_exact_integer_arithmetic() {
        assert_eq!(decimal_to_nm("102.54", NM_PER_MM), Ok(102_540_000));
        assert_eq!(decimal_to_nm("-99.8", NM_PER_MM), Ok(-99_800_000));
        assert_eq!(decimal_to_nm("0.123456", NM_PER_MM), Ok(123_456));
        assert_eq!(decimal_to_nm("-0.000001", NM_PER_MM), Ok(-1));
        assert_eq!(decimal_to_nm("110.0", NM_PER_MM), Ok(110_000_000));
        assert_eq!(decimal_to_nm("3", NM_PER_MM), Ok(3_000_000));
        assert_eq!(decimal_to_nm(".5", NM_PER_MM), Ok(500_000));
        // 7 mm-fraction digits round to nearest nm, ties away from zero.
        assert_eq!(decimal_to_nm("0.0000015", NM_PER_MM), Ok(2));
        assert_eq!(decimal_to_nm("-0.0000015", NM_PER_MM), Ok(-2));
        assert!(decimal_to_nm("1.2.3", NM_PER_MM).is_err());
        assert!(decimal_to_nm("", NM_PER_MM).is_err());
        assert!(decimal_to_nm(".", NM_PER_MM).is_err());
    }

    #[test]
    fn inch_units_scale_by_25_4() {
        let src = "\
M48
FMAT,2
INCH
T1C0.040
%
G90
G05
T1
X1.0Y-0.5
X0.1Y0.0G85X0.2Y0.0
M30
";
        let ops = parse_excellon(src).expect("parse");
        assert_eq!(
            ops,
            vec![
                DrillOp::Hole {
                    center: P::new(25_400_000, -12_700_000),
                    diameter_nm: 1_016_000, // 0.040 in
                },
                DrillOp::Slot {
                    a: P::new(2_540_000, 0),
                    b: P::new(5_080_000, 0),
                    diameter_nm: 1_016_000,
                },
            ]
        );
    }

    #[test]
    fn unsupported_constructs_error_loudly() {
        const HEADER: &str = "M48\nFMAT,2\nMETRIC\nT1C0.4\n%\nG90\nG05\nT1\n";
        // (full source, needle the error message must contain)
        let cases: &[(String, &str)] = &[
            // header errors
            ("G90\nM30\n".into(), "M48"),
            ("M48\nFMAT,1\nMETRIC\n%\nM30\n".into(), "FMAT,1"),
            ("M48\nFMAT,2\nMETRIC,TZ\n%\nM30\n".into(), "METRIC,TZ"),
            (
                "M48\nFMAT,2\nT1C0.4\nMETRIC\n%\nM30\n".into(),
                "before METRIC/INCH",
            ),
            (
                "M48\nFMAT,2\nMETRIC\nT1C0.4F200S100\n%\nM30\n".into(),
                "T1C0.4F200S100",
            ),
            ("M48\nFMAT,2\nMETRIC\nT1\n%\nM30\n".into(), "T1"),
            ("M48\nFMAT,2\nMETRIC\nICI,ON\n%\nM30\n".into(), "ICI,ON"),
            // body errors
            (format!("{HEADER}G91\nM30\n"), "G91"),
            (format!("{HEADER}G82\nM30\n"), "G82"),
            (format!("{HEADER}R5X1.0\nM30\n"), "repeat code"),
            (format!("{HEADER}M71\nM30\n"), "M71"),
            (format!("{HEADER}T9\nM30\n"), "undefined tool T9"),
            (format!("{HEADER}T1C0.4\nM30\n"), "T1C0.4"),
            (
                "M48\nFMAT,2\nMETRIC\n%\nX1.0Y1.0\nM30\n".into(),
                "no tool selected",
            ),
            (format!("{HEADER}Y-1.0\nM30\n"), "expected X coordinate"),
            (format!("{HEADER}X1.0\nM30\n"), "expected Y coordinate"),
            (format!("{HEADER}X1.0Y2.0G84X1.0Y3.0\nM30\n"), "G84"),
            (format!("{HEADER}X1.0Y2.0G85X1.0Y3.0Z1\nM30\n"), "Z1"),
            (format!("{HEADER}X1..0Y2.0\nM30\n"), "malformed decimal"),
            (format!("{HEADER}X1.0Y2.0\n"), "missing M30"),
        ];
        for (src, needle) in cases {
            let err = parse_excellon(src).expect_err(needle);
            assert!(
                err.msg.contains(needle),
                "expected '{needle}' in '{}' for input:\n{src}",
                err.msg
            );
            assert!(err.line > 0, "error must carry a line number");
        }
    }

    #[test]
    fn hostile_coordinate_magnitudes_error_instead_of_panicking() {
        const HEADER: &str = "M48\nFMAT,2\nMETRIC\nT1C0.4\n%\nG90\nG05\nT1\n";
        // A 38-digit integer part parses as i128 but overflows the scale
        // multiply — in release that wraps into a plausible coordinate and
        // drills a hole in the wrong place.
        let long = "9".repeat(38);
        let bodies = [
            format!("X{long}.0Y1.0\nM30\n"),
            format!("X1.0Y-{long}.0\nM30\n"),
            format!("X{}.5Y1.0\nM30\n", "9".repeat(33)),
            // Just inside the scale multiply, so the fractional `+=` is the op
            // that overflows.
            "X170141183460469231731687303715884.5Y1.0\nM30\n".to_string(),
            format!("T2C{long}.0\n"),
        ];
        for body in bodies {
            let src = format!("{HEADER}{body}");
            assert!(
                parse_excellon(&src).is_err(),
                "expected a clean Err for body:\n{body}"
            );
        }
    }

    #[test]
    fn error_reports_the_offending_line_number() {
        let src = "M48\nFMAT,2\nMETRIC\nT1C0.4\n%\nG90\nG05\nT1\nG82\nM30\n";
        let err = parse_excellon(src).expect_err("G82");
        assert_eq!(err.line, 9);
    }

    // -- ground truth against kicad-cli drill export -------------------------
    //
    // Expected values come from the authored board source
    // (samples/kicad/valdemo2.kicad_pcb) rather than GUI-pasted numbers —
    // a deliberate deviation: the .kicad_pcb text is what the GUI displays,
    // minus transcription risk.

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn ground_truth_kicad_export_round_trips() {
        if !kicad_cli::available() {
            eprintln!("SKIP: kicad-cli not installed");
            return;
        }
        let cli = kicad_cli::KicadCli::discover().unwrap();
        let board = repo_root().join("samples/kicad/valdemo2.kicad_pcb");
        assert!(board.is_file(), "sample board missing");
        let dir = std::env::temp_dir().join(format!("pcbforge-ing2-drill-{}", std::process::id()));
        let files = cli.export_drill(&board, &dir).unwrap();

        let mut ops = Vec::new();
        for f in &files {
            ops.extend(load_excellon_full(f).unwrap());
        }

        let holes: Vec<(P, Nm)> = ops
            .iter()
            .filter_map(|op| match *op {
                DrillOp::Hole {
                    center,
                    diameter_nm,
                } => Some((center, diameter_nm)),
                DrillOp::Slot { .. } => None,
            })
            .collect();
        let slots: Vec<(P, P, Nm)> = ops
            .iter()
            .filter_map(|op| match *op {
                DrillOp::Slot { a, b, diameter_nm } => Some((a, b, diameter_nm)),
                DrillOp::Hole { .. } => None,
            })
            .collect();
        assert_eq!(holes.len(), 4, "exactly 4 round holes: {ops:?}");
        assert_eq!(slots.len(), 1, "exactly 1 slot: {ops:?}");

        // Via drills: 0.4 mm at (110, -100) and (108, -111) mm — exact nm.
        for c in [
            P::new(110_000_000, -100_000_000),
            P::new(108_000_000, -111_000_000),
        ] {
            assert!(
                holes.contains(&(c, 400_000)),
                "missing via hole at {c:?}: {holes:?}"
            );
        }
        // Component holes: 1.0 mm at (100, -100) and (102.54, -100) mm.
        for c in [
            P::new(100_000_000, -100_000_000),
            P::new(102_540_000, -100_000_000),
        ] {
            assert!(
                holes.contains(&(c, 1_000_000)),
                "missing component hole at {c:?}: {holes:?}"
            );
        }
        // Slot: 1.0 mm dia from (105.08, -99.8) to (105.08, -100.2) mm.
        let (a, b, d) = slots[0];
        assert_eq!(d, 1_000_000);
        let want_a = P::new(105_080_000, -99_800_000);
        let want_b = P::new(105_080_000, -100_200_000);
        assert!(
            (a, b) == (want_a, want_b) || (a, b) == (want_b, want_a),
            "slot endpoints {a:?}..{b:?}"
        );

        // The backlog API sees the slot as its two endpoint holes: 4 + 2.
        let flat: Vec<(P, Nm)> = files
            .iter()
            .flat_map(|f| load_excellon(f).unwrap())
            .collect();
        assert_eq!(flat.len(), 6);
        assert!(flat.contains(&(want_a, 1_000_000)) && flat.contains(&(want_b, 1_000_000)));

        // mm() sanity: the exact literals above match rounded-f64 conversion.
        assert_eq!(mm(102.54), 102_540_000);
        assert_eq!(mm(-99.8), -99_800_000);
    }
}
