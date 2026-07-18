//! Gerber (RS-274X / X2) copper-layer ingest.
//!
//! Parses the dialect KiCad's plotter emits into a normalized
//! [`pcb_core::Layer`] (disjoint polygons-with-holes, integer nanometers).
//! This is the front half of the operator's "cut out FlatCAM" workflow: the
//! parsed copper is inverted by `cam::noncopper` and exported for EZCAD.
//!
//! # Supported subset (what KiCad emits)
//!
//! * `%FSLAXnmYnm*%` coordinate format (leading-zero-omitted, absolute) and
//!   `%MOMM*%` / `%MOIN*%` units. KiCad's `X46` + mm makes raw coordinate
//!   integers *exactly* nanometers.
//! * Standard apertures `C` / `R` / `O` / `P` with optional round hole.
//! * Aperture macros (`%AM…%`): primitives 1 (circle), 4 (outline),
//!   5 (polygon), 20 (vector line), 21 (center line), with `$n` parameters,
//!   arithmetic expressions, and variable assignment — enough for KiCad's
//!   RoundRect / RotRect / FreePoly / HorizOval macros.
//! * `D01` strokes (linear and G02/G03 multi-quadrant arcs) with a circular
//!   aperture → capsule geometry; zero-length strokes → dots.
//! * `D03` flashes, `G36`/`G37` filled regions (including KiCad's
//!   slit-connected zone contours), `%LPD%`/`%LPC%` polarity folding.
//! * X2 attributes (`%TF` `%TA` `%TO` `%TD`) and comments are skipped.
//!
//! Anything geometry-affecting outside this subset — step-repeat, mirroring,
//! scaling, incremental coordinates, single-quadrant arc mode, unknown macro
//! primitives — is a hard [`GerberError`] naming the construct, never a
//! silent approximation.
//!
//! # Tessellation
//!
//! Curves (aperture circles, capsule caps, arcs) become polylines with chord
//! sagitta ≤ [`CHORD_TOL_NM`] (1 µm), vertices on the true curve.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use cam::geom;
use pcb_core::{Layer, Nm, P, Poly, Ring};

/// Maximum chord sagitta when tessellating curves, nm (1 µm).
pub const CHORD_TOL_NM: f64 = 1_000.0;

/// Parse error with 1-based line information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GerberError {
    pub line: usize,
    pub msg: String,
}

impl fmt::Display for GerberError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gerber parse error (line {}): {}", self.line, self.msg)
    }
}

impl std::error::Error for GerberError {}

/// Load a Gerber file into a normalized [`Layer`].
pub fn load_gerber(path: &Path) -> Result<Layer, GerberError> {
    let src = std::fs::read_to_string(path).map_err(|e| GerberError {
        line: 0,
        msg: format!("cannot read {}: {e}", path.display()),
    })?;
    parse_gerber(&src)
}

/// Parse Gerber source text into a normalized [`Layer`].
///
/// The output polys are the boolean result of all draw/flash/region
/// operations with dark/clear polarity folded in order: disjoint outers
/// (CCW) with holes (CW).
pub fn parse_gerber(src: &str) -> Result<Layer, GerberError> {
    let mut p = Parser::new(src);
    p.run()?;
    Ok(Layer {
        polys: fold_batches(&p.batches),
    })
}

/// Fold polarity batches into normalized geometry: dark = union, clear =
/// difference, in stream order.
fn fold_batches(batches: &[(bool, Vec<Poly>)]) -> Vec<Poly> {
    let mut acc: Vec<Poly> = Vec::new();
    for (dark, polys) in batches {
        if polys.is_empty() {
            continue;
        }
        acc = if *dark {
            geom::union(&acc, polys)
        } else {
            geom::difference(&acc, polys)
        };
    }
    acc
}

/// X2 aperture attributes (Ucamco §5.6). Only the fields ING-3/ING-4 consume.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct ApertureAttrs {
    /// Primary value of `.AperFunction` (§5.6.10): e.g. `ComponentPad`,
    /// `SMDPad`, `ViaPad`, `Conductor`, `FiducialPad`.
    pub function: Option<String>,
}

/// One attributed graphical object (a flash, stroke, or region) with the X2
/// attributes that were in force when it was drawn.
#[derive(Clone, Debug)]
pub struct GerberObject {
    /// The object's geometry (nm), before polarity folding.
    pub polys: Vec<Poly>,
    /// False for clear (LPC) knockout objects.
    pub dark: bool,
    /// `.N` net name (§5.6.13), when present.
    pub net: Option<String>,
    /// `.P` component pad `(reference, pin)` (§5.6.14), when present.
    pub pad: Option<(String, String)>,
    /// Primary `.AperFunction` value of the object's aperture, when present.
    pub aper_function: Option<String>,
}

impl GerberObject {
    /// True for a fiducial pad (`.AperFunction,FiducialPad`).
    pub fn is_fiducial(&self) -> bool {
        self.aper_function.as_deref() == Some("FiducialPad")
    }

    /// True for any pad-class aperture function.
    pub fn is_pad(&self) -> bool {
        matches!(
            self.aper_function.as_deref(),
            Some(
                "ComponentPad"
                    | "SMDPad"
                    | "ConnectorPad"
                    | "HeatsinkPad"
                    | "TestPad"
                    | "ViaPad"
                    | "FiducialPad"
            )
        )
    }
}

/// A Gerber layer parsed with X2 attributes preserved (ING-3).
///
/// [`layer`](Self::layer) is the same normalized geometry [`load_gerber`]
/// produces (the ING-1 cross-check); [`objects`](Self::objects) additionally
/// carries each object's net / pad / aperture-function attributes, and
/// [`net_polys`](Self::net_polys) unions the copper of a chosen net (the
/// input ING-4's `net_raster` needs).
pub struct AttributedLayer {
    layer: Layer,
    objects: Vec<GerberObject>,
}

impl AttributedLayer {
    /// The folded, normalized layer geometry (identical to [`load_gerber`]).
    pub fn layer(&self) -> &Layer {
        &self.layer
    }

    /// Every attributed object, in stream order.
    pub fn objects(&self) -> &[GerberObject] {
        &self.objects
    }

    /// Distinct net names present on copper objects, sorted.
    pub fn net_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .objects
            .iter()
            .filter(|o| o.dark)
            .filter_map(|o| o.net.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Union of the dark copper tagged with net `name` (empty if none).
    pub fn net_polys(&self, name: &str) -> Vec<Poly> {
        let mut acc: Vec<Poly> = Vec::new();
        for o in &self.objects {
            if o.dark && o.net.as_deref() == Some(name) {
                acc = geom::union(&acc, &o.polys);
            }
        }
        acc
    }

    /// All fiducial-pad objects.
    pub fn fiducials(&self) -> Vec<&GerberObject> {
        self.objects.iter().filter(|o| o.is_fiducial()).collect()
    }

    /// The folded layer with `NonConductor` copper excluded.
    ///
    /// `.AperFunction,NonConductor` (Ucamco §5.6.10) marks copper that is not
    /// part of the electrical circuit — KiCad emits it for no-net zones and
    /// graphic shapes on copper layers. For the isolation-ablation workflow
    /// that copper is exactly what rub-out removes, so the inverter treats it
    /// as clearable by default (discovered on the operator's uv_test board,
    /// where a NonConductor zone spanned the whole right side and was wrongly
    /// kept as copper). Objects are folded in stream order with polarity, so
    /// with nothing excluded this equals [`layer`](Self::layer).
    pub fn layer_without_nonconductor(&self) -> Layer {
        let mut acc: Vec<Poly> = Vec::new();
        for o in &self.objects {
            if o.aper_function.as_deref() == Some("NonConductor") {
                continue;
            }
            acc = if o.dark {
                geom::union(&acc, &o.polys)
            } else {
                geom::difference(&acc, &o.polys)
            };
        }
        Layer { polys: acc }
    }
}

/// Read and parse a Gerber X2 file, preserving aperture/object attributes
/// (ING-3).
pub fn load_gerber_x2(path: &Path) -> Result<AttributedLayer, GerberError> {
    let src = std::fs::read_to_string(path).map_err(|e| GerberError {
        line: 0,
        msg: format!("cannot read {}: {e}", path.display()),
    })?;
    parse_gerber_x2(&src)
}

/// Parse Gerber X2 source, preserving attributes. The `.layer()` geometry is
/// identical to [`parse_gerber`]'s.
pub fn parse_gerber_x2(src: &str) -> Result<AttributedLayer, GerberError> {
    let mut p = Parser::new(src);
    p.track_attrs = true;
    p.run()?;
    let layer = Layer {
        polys: fold_batches(&p.batches),
    };
    Ok(AttributedLayer {
        layer,
        objects: std::mem::take(&mut p.objects),
    })
}

// ---------------------------------------------------------------------------
// Parser state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Interp {
    Linear,
    ClockwiseArc,
    CounterClockwiseArc,
}

#[derive(Clone)]
enum Aperture {
    /// Pre-tessellated geometry in aperture-local nm, ready to translate.
    Compiled(Vec<Poly>),
    /// Circle keeps its radius so strokes can build capsules.
    Circle { r_nm: f64, compiled: Vec<Poly> },
}

struct MacroDef {
    stmts: Vec<MacroStmt>,
}

enum MacroStmt {
    Assign(u32, Expr),
    Primitive(i64, Vec<Expr>),
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// nm per raw coordinate count: `raw * num / den`.
    coord_num: i128,
    coord_den: i128,
    coord_dec: Option<u32>,
    /// nm per file unit (1e6 for mm, 25.4e6 for inch); None until %MO / G7x.
    unit_nm: Option<f64>,
    apertures: HashMap<u32, Aperture>,
    macros: HashMap<String, MacroDef>,
    current_aperture: Option<u32>,
    interp: Interp,
    /// Multi-quadrant arc mode confirmed (G75). KiCad always emits it.
    multi_quadrant: bool,
    cur: Option<P>,
    in_region: bool,
    region_contour: Vec<P>,
    /// (dark?, polys) batches in stream order; consecutive same-polarity ops
    /// share a batch.
    batches: Vec<(bool, Vec<Poly>)>,
    dark: bool,
    ended: bool,
    // --- X2 attribute tracking (ING-3); inert unless `track_attrs` ---
    /// When true, every emitted object is also recorded in `objects` with the
    /// X2 attributes in force (see [`parse_gerber_x2`]).
    track_attrs: bool,
    /// Current `.N` net-name object attribute (Ucamco §5.6.13).
    obj_net: Option<String>,
    /// Current `.P` component-pad object attribute (ref, pin) (§5.6.14).
    obj_pad: Option<(String, String)>,
    /// Current aperture-attribute dictionary (attached to each `%AD`).
    aper_dict: ApertureAttrs,
    /// `.AperFunction` recorded per aperture D-code at definition time.
    aperture_fn: HashMap<u32, Option<String>>,
    /// Attributed objects, in stream order.
    objects: Vec<GerberObject>,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            coord_num: 1,
            coord_den: 1,
            coord_dec: None,
            unit_nm: None,
            apertures: HashMap::new(),
            macros: HashMap::new(),
            current_aperture: None,
            interp: Interp::Linear,
            multi_quadrant: false,
            cur: None,
            in_region: false,
            region_contour: Vec::new(),
            batches: vec![(true, Vec::new())],
            dark: true,
            ended: false,
            track_attrs: false,
            obj_net: None,
            obj_pad: None,
            aper_dict: ApertureAttrs::default(),
            aperture_fn: HashMap::new(),
            objects: Vec::new(),
        }
    }

    fn err(&self, msg: impl Into<String>) -> GerberError {
        let line = self.src[..self.pos.min(self.src.len())]
            .bytes()
            .filter(|&b| b == b'\n')
            .count()
            + 1;
        GerberError {
            line,
            msg: msg.into(),
        }
    }

    fn emit(&mut self, polys: Vec<Poly>) {
        if self.track_attrs {
            // A G36/G37 region takes the aperture attributes currently in the
            // dictionary (Ucamco §5.6: %TA% applies to subsequently created
            // objects, not only aperture definitions) — KiCad emits
            // %TA.AperFunction,NonConductor*% immediately before each no-net
            // zone region for exactly this reason. A flash/stroke takes the
            // function recorded when its D-code was defined.
            let aper_function = if self.in_region {
                self.aper_dict.function.clone()
            } else {
                self.current_aperture
                    .and_then(|d| self.aperture_fn.get(&d).cloned().flatten())
            };
            self.objects.push(GerberObject {
                polys: polys.clone(),
                dark: self.dark,
                net: self.obj_net.clone(),
                pad: self.obj_pad.clone(),
                aper_function,
            });
        }
        let last = self.batches.last_mut().expect("batches never empty");
        if last.0 == self.dark {
            last.1.extend(polys);
        } else {
            self.batches.push((self.dark, polys));
        }
    }

    /// `%TA<name>,<v1>,<v2>…` — set an aperture-dictionary attribute. Only
    /// `.AperFunction` (§5.6.10) and a standalone `.FiducialPad` are
    /// interpreted; the primary value is kept.
    fn set_aperture_attr(&mut self, rest: &str) {
        let mut it = rest.splitn(2, ',');
        let name = it.next().unwrap_or("");
        let val = it.next().unwrap_or("");
        match name {
            ".AperFunction" => {
                let primary = val.split(',').next().unwrap_or("").to_string();
                self.aper_dict.function = (!primary.is_empty()).then_some(primary);
            }
            ".FiducialPad" => self.aper_dict.function = Some("FiducialPad".to_string()),
            _ => {}
        }
    }

    /// `%TO<name>,<v1>,<v2>…` — set an object attribute. `.N` net name
    /// (§5.6.13) and `.P` component pad (ref, pin) (§5.6.14) are kept.
    fn set_object_attr(&mut self, rest: &str) {
        let mut it = rest.splitn(2, ',');
        let name = it.next().unwrap_or("");
        let val = it.next().unwrap_or("");
        match name {
            ".N" => self.obj_net = (!val.is_empty()).then(|| val.to_string()),
            ".P" => {
                // `.P,<refdes>,<pin>[,<function>]` — split into 3 so the
                // optional pad-function field (§5.6.14) isn't glued onto `pin`.
                let mut p = val.splitn(3, ',');
                let refdes = p.next().unwrap_or("").to_string();
                let pin = p.next().unwrap_or("").to_string();
                self.obj_pad = Some((refdes, pin));
            }
            _ => {}
        }
    }

    /// `%TD[<name>]` — delete one attribute, or all when no name is given
    /// (§5.6.16). KiCad emits a bare `%TD*%` to reset between objects.
    fn delete_attr(&mut self, rest: &str) {
        match rest {
            "" => {
                self.obj_net = None;
                self.obj_pad = None;
                self.aper_dict = ApertureAttrs::default();
            }
            ".N" => self.obj_net = None,
            ".P" => self.obj_pad = None,
            ".AperFunction" | ".FiducialPad" => self.aper_dict.function = None,
            _ => {}
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn run(&mut self) -> Result<(), GerberError> {
        loop {
            self.skip_ws();
            if self.pos >= self.bytes.len() {
                break;
            }
            if self.bytes[self.pos] == b'%' {
                let start = self.pos + 1;
                let end = self.src[start..]
                    .find('%')
                    .map(|i| start + i)
                    .ok_or_else(|| self.err("unterminated %...% extended command"))?;
                let body: String = self.src[start..end]
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                self.extended(&body)?;
                self.pos = end + 1;
            } else {
                let start = self.pos;
                let end = self.src[start..]
                    .find('*')
                    .map(|i| start + i)
                    .ok_or_else(|| self.err("unterminated word command (missing '*')"))?;
                let word: String = self.src[start..end]
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                self.word(&word)?;
                self.pos = end + 1;
            }
            if self.ended {
                break;
            }
        }
        if !self.ended {
            return Err(self.err("missing M02 end-of-file"));
        }
        Ok(())
    }

    // -- extended (%...%) commands ------------------------------------------

    fn extended(&mut self, body: &str) -> Result<(), GerberError> {
        // A %...% block may contain several '*'-terminated records. Most are
        // standalone commands that legally chain (deprecated but valid
        // RS-274X, e.g. `%LPC*LPD*%` or `%FSLAX46Y46*MOMM*%`) — dispatch every
        // one, or a dropped record silently changes polarity/units. `%AM%` is
        // the exception: its records are macro primitives spanning the block.
        let records: Vec<&str> = body.split('*').filter(|r| !r.is_empty()).collect();
        let first = match records.first() {
            Some(f) => *f,
            None => return Ok(()),
        };
        if first.starts_with("AM") {
            return self.cmd_am(&records);
        }
        for rec in &records {
            self.dispatch_extended_record(rec)?;
        }
        Ok(())
    }

    /// Dispatch a single `*`-terminated extended record. Not used for `%AM%`
    /// macros, which span a whole block (see [`Parser::extended`]).
    fn dispatch_extended_record(&mut self, rec: &str) -> Result<(), GerberError> {
        if let Some(rest) = rec.strip_prefix("FS") {
            return self.cmd_fs(rest);
        }
        if let Some(rest) = rec.strip_prefix("MO") {
            return match rest {
                "MM" => {
                    self.unit_nm = Some(1e6);
                    self.recompute_coord_scale();
                    Ok(())
                }
                "IN" => {
                    self.unit_nm = Some(25.4e6);
                    self.recompute_coord_scale();
                    Ok(())
                }
                other => Err(self.err(format!("unsupported unit mode %MO{other}%"))),
            };
        }
        if let Some(rest) = rec.strip_prefix("ADD") {
            return self.cmd_ad(rest);
        }
        if let Some(rest) = rec.strip_prefix("LP") {
            self.dark = match rest {
                "D" => true,
                "C" => false,
                other => return Err(self.err(format!("unsupported polarity %LP{other}%"))),
            };
            return Ok(());
        }
        // X2 attributes (Ucamco §5.6). `.TF` file attributes are pure
        // metadata; `.TA`/`.TO`/`.TD` maintain the aperture/object attribute
        // dictionary that ING-3 preserves (inert unless `track_attrs`).
        if rec.starts_with("TF") {
            return Ok(());
        }
        if let Some(rest) = rec.strip_prefix("TA") {
            if self.track_attrs {
                self.set_aperture_attr(rest);
            }
            return Ok(());
        }
        if let Some(rest) = rec.strip_prefix("TO") {
            if self.track_attrs {
                self.set_object_attr(rest);
            }
            return Ok(());
        }
        if let Some(rest) = rec.strip_prefix("TD") {
            if self.track_attrs {
                self.delete_attr(rest);
            }
            return Ok(());
        }
        if rec.starts_with("IN") || rec.starts_with("LN") || rec == "IPPOS" {
            return Ok(()); // legacy names / positive image polarity
        }
        if let Some(rest) = rec.strip_prefix("SR") {
            // Step-repeat other than the trivial 1x1 block changes geometry.
            if rest.is_empty() || rest == "X1Y1I0J0" {
                return Ok(());
            }
            return Err(self.err(format!("unsupported step-repeat %SR{rest}%")));
        }
        if let Some(rest) = rec.strip_prefix("LM") {
            if rest == "N" {
                return Ok(());
            }
            return Err(self.err(format!("unsupported load-mirror %LM{rest}%")));
        }
        if let Some(rest) = rec.strip_prefix("LR") {
            if rest.parse::<f64>() == Ok(0.0) {
                return Ok(());
            }
            return Err(self.err(format!("unsupported load-rotation %LR{rest}%")));
        }
        if let Some(rest) = rec.strip_prefix("LS") {
            if rest.parse::<f64>() == Ok(1.0) {
                return Ok(());
            }
            return Err(self.err(format!("unsupported load-scale %LS{rest}%")));
        }
        Err(self.err(format!("unsupported extended command %{rec}...%")))
    }

    fn cmd_fs(&mut self, rest: &str) -> Result<(), GerberError> {
        // Expect LA (leading zeros omitted, absolute) — what KiCad emits.
        let rest = rest.strip_prefix("LA").ok_or_else(|| {
            self.err(format!(
                "unsupported coordinate format FS{rest} (only FSLA)"
            ))
        })?;
        let (x, y) = rest
            .split_once('Y')
            .and_then(|(a, b)| Some((a.strip_prefix('X')?, b)))
            .ok_or_else(|| self.err(format!("malformed format spec FSLA{rest}")))?;
        if x != y {
            return Err(self.err(format!("asymmetric X/Y format {x} vs {y} unsupported")));
        }
        if x.len() != 2 {
            return Err(self.err(format!("malformed format digits {x}")));
        }
        let dec: u32 = x[1..2]
            .parse()
            .map_err(|_| self.err(format!("malformed format digits {x}")))?;
        self.coord_dec = Some(dec);
        self.recompute_coord_scale();
        Ok(())
    }

    fn recompute_coord_scale(&mut self) {
        if let (Some(dec), Some(unit)) = (self.coord_dec, self.unit_nm) {
            // nm = raw * unit_nm / 10^dec, kept as an exact rational.
            let num = if unit == 1e6 {
                1_000_000_i128
            } else {
                25_400_000_i128
            };
            self.coord_num = num;
            self.coord_den = 10_i128.pow(dec);
        }
    }

    fn coord_to_nm(&self, raw: i64) -> Result<Nm, GerberError> {
        let v = raw as i128 * self.coord_num;
        // Round half away from zero.
        let d = self.coord_den;
        let r = if v >= 0 {
            (v + d / 2) / d
        } else {
            (v - d / 2) / d
        };
        // An absurd-but-parseable coordinate must not wrap silently through
        // the i128→i64 cast — error naming it instead.
        Nm::try_from(r).map_err(|_| self.err(format!("coordinate {raw} overflows the nm range")))
    }

    fn unit_to_nm(&self, v: f64) -> f64 {
        v * self.unit_nm.unwrap_or(1e6)
    }

    // -- aperture definitions ------------------------------------------------

    fn cmd_am(&mut self, records: &[&str]) -> Result<(), GerberError> {
        if self.unit_nm.is_none() {
            return Err(self.err("aperture macro before %MO% unit mode (mm vs inch unknown)"));
        }
        let name = records[0][2..].to_string();
        if name.is_empty() {
            return Err(self.err("aperture macro with empty name"));
        }
        let mut stmts = Vec::new();
        for rec in &records[1..] {
            if rec.starts_with('0') {
                continue; // primitive 0 = comment
            }
            if let Some(eq) = rec.strip_prefix('$') {
                let (var, expr) = eq
                    .split_once('=')
                    .ok_or_else(|| self.err(format!("malformed macro assignment {rec}")))?;
                let var: u32 = var
                    .parse()
                    .map_err(|_| self.err(format!("malformed macro variable ${var}")))?;
                stmts.push(MacroStmt::Assign(
                    var,
                    parse_expr(expr).map_err(|e| self.err(e))?,
                ));
                continue;
            }
            let mut parts = rec.split(',');
            let code: i64 = parts
                .next()
                .unwrap_or("")
                .trim()
                .parse()
                .map_err(|_| self.err(format!("malformed macro primitive {rec}")))?;
            let args = parts
                .map(|a| parse_expr(a).map_err(|e| self.err(e)))
                .collect::<Result<Vec<_>, _>>()?;
            match code {
                1 | 4 | 5 | 20 | 21 => stmts.push(MacroStmt::Primitive(code, args)),
                other => {
                    return Err(self.err(format!(
                        "unsupported macro primitive {other} in %AM{name}% (supported: 1,4,5,20,21)"
                    )));
                }
            }
        }
        self.macros.insert(name, MacroDef { stmts });
        Ok(())
    }

    fn cmd_ad(&mut self, rest: &str) -> Result<(), GerberError> {
        // Units must be known first — otherwise `unit_to_nm` would silently
        // assume mm and shrink an inch-mode aperture 25.4× (same contract as
        // the coordinate guard: hard error, never a silent approximation).
        if self.unit_nm.is_none() {
            return Err(self.err(format!("aperture ADD{rest} before %MO% unit mode")));
        }
        // rest = "10C,0.200000" or "12RoundRect,0.135X-0.2X..." etc.
        let split = rest
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(|| self.err(format!("malformed aperture definition ADD{rest}")))?;
        let dcode: u32 = rest[..split]
            .parse()
            .map_err(|_| self.err(format!("malformed aperture number in ADD{rest}")))?;
        if dcode < 10 {
            return Err(self.err(format!("aperture number D{dcode} below 10")));
        }
        let (name, params) = match rest[split..].split_once(',') {
            Some((n, p)) => (n, p.split('X').map(str::trim).collect::<Vec<_>>()),
            None => (&rest[split..], Vec::new()),
        };
        let parse_f = |s: &str| -> Result<f64, GerberError> {
            s.parse::<f64>()
                .map_err(|_| self.err(format!("malformed aperture parameter '{s}' in ADD{rest}")))
        };
        let ap = match name {
            "C" => {
                if params.is_empty() {
                    return Err(self.err(format!("circle aperture D{dcode} missing diameter")));
                }
                let d = self.unit_to_nm(parse_f(params[0])?);
                let mut polys = vec![ring_poly(circle_ring(P::new(0, 0), d / 2.0))];
                if let Some(h) = params.get(1) {
                    polys = subtract_hole(polys, self.unit_to_nm(parse_f(h)?));
                }
                Aperture::Circle {
                    r_nm: d / 2.0,
                    compiled: polys,
                }
            }
            "R" | "O" => {
                if params.len() < 2 {
                    return Err(self.err(format!("aperture D{dcode} ({name}) needs X and Y sizes")));
                }
                let x = self.unit_to_nm(parse_f(params[0])?);
                let y = self.unit_to_nm(parse_f(params[1])?);
                let ring = if name == "R" {
                    rect_ring(x, y)
                } else {
                    obround_ring(x, y)
                };
                let mut polys = vec![ring_poly(ring)];
                if let Some(h) = params.get(2) {
                    polys = subtract_hole(polys, self.unit_to_nm(parse_f(h)?));
                }
                Aperture::Compiled(polys)
            }
            "P" => {
                if params.len() < 2 {
                    return Err(self.err(format!(
                        "polygon aperture D{dcode} needs diameter and vertex count"
                    )));
                }
                let od = self.unit_to_nm(parse_f(params[0])?);
                let n = parse_f(params[1])? as usize;
                if !(3..=12).contains(&n) {
                    return Err(self.err(format!(
                        "polygon aperture D{dcode} vertex count {n} out of range"
                    )));
                }
                let rot = params
                    .get(2)
                    .map(|s| parse_f(s))
                    .transpose()?
                    .unwrap_or(0.0);
                let ring: Ring = (0..n)
                    .map(|k| {
                        let a = rot.to_radians() + k as f64 * std::f64::consts::TAU / n as f64;
                        P::new(
                            (od / 2.0 * a.cos()).round() as Nm,
                            (od / 2.0 * a.sin()).round() as Nm,
                        )
                    })
                    .collect();
                let mut polys = vec![ring_poly(ring)];
                if let Some(h) = params.get(3) {
                    polys = subtract_hole(polys, self.unit_to_nm(parse_f(h)?));
                }
                Aperture::Compiled(polys)
            }
            macro_name => {
                let def = self.macros.get(macro_name).ok_or_else(|| {
                    self.err(format!(
                        "aperture D{dcode} references unknown macro '{macro_name}'"
                    ))
                })?;
                let args: Vec<f64> = params
                    .iter()
                    .map(|s| parse_f(s))
                    .collect::<Result<_, _>>()?;
                let polys = eval_macro(def, &args, self.unit_nm.unwrap_or(1e6))
                    .map_err(|e| self.err(format!("macro '{macro_name}' for D{dcode}: {e}")))?;
                Aperture::Compiled(polys)
            }
        };
        if self.track_attrs {
            // Attach the current aperture-dictionary function to this D-code.
            self.aperture_fn
                .insert(dcode, self.aper_dict.function.clone());
        }
        self.apertures.insert(dcode, ap);
        Ok(())
    }

    // -- word commands --------------------------------------------------------

    fn word(&mut self, w: &str) -> Result<(), GerberError> {
        if w.is_empty() || w.starts_with("G04") {
            return Ok(());
        }
        match w {
            "M02" | "M00" => {
                self.ended = true;
                return Ok(());
            }
            "M01" => return Ok(()),
            "G01" => {
                self.interp = Interp::Linear;
                return Ok(());
            }
            "G02" => {
                self.interp = Interp::ClockwiseArc;
                return Ok(());
            }
            "G03" => {
                self.interp = Interp::CounterClockwiseArc;
                return Ok(());
            }
            "G36" => {
                self.in_region = true;
                self.region_contour.clear();
                return Ok(());
            }
            "G37" => {
                self.close_region_contour();
                self.in_region = false;
                return Ok(());
            }
            "G75" => {
                self.multi_quadrant = true;
                return Ok(());
            }
            "G74" => return Err(self.err("single-quadrant arc mode (G74) unsupported")),
            "G70" => {
                self.unit_nm = Some(25.4e6);
                self.recompute_coord_scale();
                return Ok(());
            }
            "G71" => {
                self.unit_nm = Some(1e6);
                self.recompute_coord_scale();
                return Ok(());
            }
            "G90" => return Ok(()),
            "G91" => return Err(self.err("incremental coordinates (G91) unsupported")),
            _ => {}
        }
        // Aperture select: Dnn (nn >= 10), optionally with legacy G54 prefix.
        let sel = w.strip_prefix("G54").unwrap_or(w);
        if let Some(d) = sel.strip_prefix('D')
            && let Ok(n) = d.parse::<u32>()
            && n >= 10
        {
            if !self.apertures.contains_key(&n) {
                return Err(self.err(format!("select of undefined aperture D{n}")));
            }
            self.current_aperture = Some(n);
            return Ok(());
        }
        // Coordinate word: [Gnn]X±nY±nI±nJ±nD0n
        self.coordinate_word(w)
    }

    fn coordinate_word(&mut self, w: &str) -> Result<(), GerberError> {
        if self.coord_dec.is_none() || self.unit_nm.is_none() {
            return Err(self.err("coordinate data before %FS%/%MO% format spec"));
        }
        let mut rest = w;
        // Leading inline G code (KiCad may emit e.g. "G01X...").
        while let Some(r) = rest.strip_prefix('G') {
            let digits: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
            match digits.as_str() {
                "01" | "1" => self.interp = Interp::Linear,
                "02" | "2" => self.interp = Interp::ClockwiseArc,
                "03" | "3" => self.interp = Interp::CounterClockwiseArc,
                other => return Err(self.err(format!("unsupported inline G{other} in '{w}'"))),
            }
            rest = &r[digits.len()..];
        }
        let (mut x, mut y) = match self.cur {
            Some(p) => (p.x, p.y),
            None => (0, 0),
        };
        let (mut i_off, mut j_off): (Nm, Nm) = (0, 0);
        let mut op: Option<u8> = None;
        let mut chars = rest.char_indices().peekable();
        while let Some((idx, c)) = chars.next() {
            let field_start = idx + c.len_utf8();
            let mut end = field_start;
            while end < rest.len() {
                let b = rest.as_bytes()[end];
                if b == b'+' || b == b'-' || b.is_ascii_digit() {
                    end += 1;
                } else {
                    break;
                }
            }
            let num = &rest[field_start..end];
            while chars.peek().is_some_and(|(i, _)| *i < end) {
                chars.next();
            }
            let parse_raw = |s: &str| -> Result<i64, GerberError> {
                s.parse::<i64>()
                    .map_err(|_| self.err(format!("malformed coordinate '{c}{s}' in '{w}'")))
            };
            match c {
                'X' => x = self.coord_to_nm(parse_raw(num)?)?,
                'Y' => y = self.coord_to_nm(parse_raw(num)?)?,
                'I' => i_off = self.coord_to_nm(parse_raw(num)?)?,
                'J' => j_off = self.coord_to_nm(parse_raw(num)?)?,
                'D' => {
                    op = Some(match num {
                        "01" | "1" => 1,
                        "02" | "2" => 2,
                        "03" | "3" => 3,
                        other => return Err(self.err(format!("unsupported operation D{other}"))),
                    });
                }
                other => return Err(self.err(format!("unexpected character '{other}' in '{w}'"))),
            }
        }
        let target = P::new(x, y);
        let op =
            op.ok_or_else(|| self.err(format!("coordinate word without D01/D02/D03: '{w}'")))?;
        match op {
            2 => {
                if self.in_region {
                    self.close_region_contour();
                }
                self.cur = Some(target);
            }
            1 => {
                let from = self
                    .cur
                    .ok_or_else(|| self.err("D01 draw with no current point (missing D02)"))?;
                let pts = self.interpolate(from, target, i_off, j_off)?;
                if self.in_region {
                    if self.region_contour.is_empty() {
                        self.region_contour.push(from);
                    }
                    self.region_contour.extend(pts);
                } else {
                    self.stroke(from, &pts)?;
                }
                self.cur = Some(target);
            }
            3 => {
                if self.in_region {
                    return Err(self.err("flash (D03) inside a G36 region"));
                }
                let ap = self.selected_aperture()?;
                let polys = translate_polys(aperture_polys(ap), target);
                self.emit(polys);
                self.cur = Some(target);
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn selected_aperture(&self) -> Result<&Aperture, GerberError> {
        let d = self
            .current_aperture
            .ok_or_else(|| self.err("draw/flash with no aperture selected"))?;
        Ok(&self.apertures[&d])
    }

    /// Interior points from `from` to `to` (inclusive of `to`), tessellating
    /// arcs at ≤ [`CHORD_TOL_NM`] sagitta.
    fn interpolate(&self, from: P, to: P, i_off: Nm, j_off: Nm) -> Result<Vec<P>, GerberError> {
        match self.interp {
            Interp::Linear => Ok(vec![to]),
            Interp::ClockwiseArc | Interp::CounterClockwiseArc => {
                if !self.multi_quadrant {
                    return Err(self.err("arc (G02/G03) before G75 multi-quadrant mode"));
                }
                let ccw = self.interp == Interp::CounterClockwiseArc;
                let c = P::new(from.x + i_off, from.y + j_off);
                tessellate_arc(from, to, c, ccw).map_err(|e| self.err(e))
            }
        }
    }

    /// Stroke from `from` through the interpolated points with the current
    /// (circular) aperture: a capsule per chord, a dot for zero length.
    fn stroke(&mut self, from: P, pts: &[P]) -> Result<(), GerberError> {
        let r = match self.selected_aperture()? {
            Aperture::Circle { r_nm, .. } => *r_nm,
            Aperture::Compiled(_) => {
                let d = self.current_aperture.unwrap_or(0);
                return Err(self.err(format!(
                    "stroke (D01) with non-circular aperture D{d} unsupported"
                )));
            }
        };
        let mut polys = Vec::new();
        let mut a = from;
        for &b in pts {
            if a == b {
                continue;
            }
            polys.push(ring_poly(capsule_ring(a, b, r)));
            a = b;
        }
        if polys.is_empty() {
            // Zero-length stroke: a dot.
            polys.push(ring_poly(circle_ring(from, r)));
        }
        self.emit(polys);
        Ok(())
    }

    fn close_region_contour(&mut self) {
        if self.region_contour.len() >= 3 {
            let mut ring = std::mem::take(&mut self.region_contour);
            if ring.first() == ring.last() {
                ring.pop();
            }
            if ring.len() >= 3 {
                self.emit(vec![ring_poly(ring)]);
                return;
            }
        }
        self.region_contour.clear();
    }
}

// ---------------------------------------------------------------------------
// Geometry helpers (all nm)
// ---------------------------------------------------------------------------

fn aperture_polys(ap: &Aperture) -> &[Poly] {
    match ap {
        Aperture::Compiled(p) => p,
        Aperture::Circle { compiled, .. } => compiled,
    }
}

/// Wrap a ring as a hole-free `Poly`, forcing CCW orientation.
fn ring_poly(mut ring: Ring) -> Poly {
    if signed_area2(&ring) < 0 {
        ring.reverse();
    }
    Poly {
        outer: ring,
        holes: vec![],
    }
}

/// Twice the signed area (shoelace), exact.
fn signed_area2(ring: &[P]) -> i128 {
    let mut a: i128 = 0;
    for (i, p) in ring.iter().enumerate() {
        let q = &ring[(i + 1) % ring.len()];
        a += p.x as i128 * q.y as i128 - q.x as i128 * p.y as i128;
    }
    a
}

fn subtract_hole(polys: Vec<Poly>, hole_d_nm: f64) -> Vec<Poly> {
    if hole_d_nm <= 0.0 {
        return polys;
    }
    let hole = ring_poly(circle_ring(P::new(0, 0), hole_d_nm / 2.0));
    geom::difference(&polys, &[hole])
}

/// Angular step keeping chord sagitta ≤ `CHORD_TOL_NM` at radius `r`.
fn max_step(r: f64) -> f64 {
    if r <= CHORD_TOL_NM {
        return std::f64::consts::FRAC_PI_2;
    }
    (2.0 * (1.0 - CHORD_TOL_NM / r).acos()).clamp(1e-3, std::f64::consts::FRAC_PI_2)
}

/// Equal-area vertex radius for an `n`-gon approximating radius `r`: the
/// polygon's area equals the disc's, so tessellation doesn't bias copper
/// area (vertices sit ~sagitta/2 outside the true circle, within tolerance).
fn equal_area_radius(r: f64, n: usize) -> f64 {
    let step = std::f64::consts::TAU / n as f64;
    r * (step / step.sin()).sqrt()
}

/// Full circle around `c` with radius `r` (equal-area polygonization).
fn circle_ring(c: P, r: f64) -> Ring {
    let r = r.max(1.0);
    let n = (std::f64::consts::TAU / max_step(r)).ceil().max(8.0) as usize;
    let rr = equal_area_radius(r, n);
    (0..n)
        .map(|k| {
            let a = k as f64 * std::f64::consts::TAU / n as f64;
            P::new(
                c.x + (rr * a.cos()).round() as Nm,
                c.y + (rr * a.sin()).round() as Nm,
            )
        })
        .collect()
}

/// Axis-aligned rectangle centered on the origin.
fn rect_ring(x_nm: f64, y_nm: f64) -> Ring {
    let (hx, hy) = ((x_nm / 2.0).round() as Nm, (y_nm / 2.0).round() as Nm);
    vec![
        P::new(-hx, -hy),
        P::new(hx, -hy),
        P::new(hx, hy),
        P::new(-hx, hy),
    ]
}

/// Obround (stadium) centered on the origin: a capsule along the longer axis.
fn obround_ring(x_nm: f64, y_nm: f64) -> Ring {
    if (x_nm - y_nm).abs() < 1.0 {
        return circle_ring(P::new(0, 0), x_nm / 2.0);
    }
    if x_nm > y_nm {
        let half = ((x_nm - y_nm) / 2.0).round() as Nm;
        capsule_ring(P::new(-half, 0), P::new(half, 0), y_nm / 2.0)
    } else {
        let half = ((y_nm - x_nm) / 2.0).round() as Nm;
        capsule_ring(P::new(0, -half), P::new(0, half), x_nm / 2.0)
    }
}

/// Capsule: the stroke of a circle of radius `r` from `a` to `b`
/// (two semicircle caps joined by straight edges). CCW.
fn capsule_ring(a: P, b: P, r: f64) -> Ring {
    let r = r.max(1.0);
    let theta = ((b.y - a.y) as f64).atan2((b.x - a.x) as f64);
    // Caps stay at the true radius: inflating them (equal-area style) would
    // widen the capsule's straight sides by the same amount — a first-order
    // area error scaling with stroke length. Instead the caps get doubled
    // vertex density, making their inscribed-polygon deficit second-order.
    let step = max_step(r) / 2.0;
    let n = (std::f64::consts::PI / step).ceil().max(6.0) as usize;
    let mut ring = Ring::new();
    let mut cap = |c: P, from_angle: f64| {
        for k in 0..=n {
            let ang = from_angle + k as f64 * std::f64::consts::PI / n as f64;
            ring.push(P::new(
                c.x + (r * ang.cos()).round() as Nm,
                c.y + (r * ang.sin()).round() as Nm,
            ));
        }
    };
    cap(b, theta - std::f64::consts::FRAC_PI_2); // cap around b: -90°..+90°
    cap(a, theta + std::f64::consts::FRAC_PI_2); // cap around a: +90°..+270°
    ring
}

/// Multi-quadrant arc from `s` to `e` about center `c`; equal endpoints mean
/// a full circle. Returns the interior vertices plus the exact endpoint.
fn tessellate_arc(s: P, e: P, c: P, ccw: bool) -> Result<Vec<P>, String> {
    let (sx, sy) = ((s.x - c.x) as f64, (s.y - c.y) as f64);
    let (ex, ey) = ((e.x - c.x) as f64, (e.y - c.y) as f64);
    let (rs, re) = (sx.hypot(sy), ex.hypot(ey));
    let r = (rs + re) / 2.0;
    if r < 1.0 {
        return Ok(vec![e]);
    }
    // A valid multi-quadrant arc has both endpoints on (nearly) the same
    // radius. A big mismatch means malformed data — most often a straight
    // segment drawn while arc mode was still modal (I/J default to 0, putting
    // the "center" on one endpoint). Producing geometry from that would be a
    // silent lie.
    if (rs - re).abs() > 2.0 * CHORD_TOL_NM + r * 0.01 {
        return Err(format!(
            "arc radii disagree (start {rs:.0} nm vs end {re:.0} nm from center): \
             malformed arc — is a linear move missing its G01?"
        ));
    }
    let a0 = sy.atan2(sx);
    let mut a1 = ey.atan2(ex);
    let full_circle = s == e;
    if ccw {
        if full_circle {
            a1 = a0 + std::f64::consts::TAU;
        } else {
            while a1 <= a0 {
                a1 += std::f64::consts::TAU;
            }
        }
    } else if full_circle {
        a1 = a0 - std::f64::consts::TAU;
    } else {
        while a1 >= a0 {
            a1 -= std::f64::consts::TAU;
        }
    }
    let sweep = a1 - a0; // signed
    let n = (sweep.abs() / max_step(r)).ceil().max(1.0) as usize;
    let mut pts = Vec::with_capacity(n);
    for k in 1..n {
        let a = a0 + sweep * k as f64 / n as f64;
        pts.push(P::new(
            c.x + (r * a.cos()).round() as Nm,
            c.y + (r * a.sin()).round() as Nm,
        ));
    }
    pts.push(e);
    Ok(pts)
}

fn translate_polys(polys: &[Poly], by: P) -> Vec<Poly> {
    polys
        .iter()
        .map(|p| Poly {
            outer: p
                .outer
                .iter()
                .map(|v| P::new(v.x + by.x, v.y + by.y))
                .collect(),
            holes: p
                .holes
                .iter()
                .map(|h| h.iter().map(|v| P::new(v.x + by.x, v.y + by.y)).collect())
                .collect(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Aperture-macro expressions and evaluation
// ---------------------------------------------------------------------------

/// Arithmetic expression over macro variables: numbers, `$n`, `+ - x /`,
/// unary minus, parentheses. Gerber spec uses `x`/`X` for multiplication.
enum Expr {
    Num(f64),
    Var(u32),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
}

fn parse_expr(s: &str) -> Result<Expr, String> {
    let tokens: Vec<char> = s.chars().collect();
    let mut pos = 0;
    let e = parse_sum(&tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err(format!("trailing input in expression '{s}'"));
    }
    Ok(e)
}

fn parse_sum(t: &[char], pos: &mut usize) -> Result<Expr, String> {
    let mut lhs = parse_product(t, pos)?;
    while *pos < t.len() {
        match t[*pos] {
            '+' => {
                *pos += 1;
                lhs = Expr::Add(Box::new(lhs), Box::new(parse_product(t, pos)?));
            }
            '-' => {
                *pos += 1;
                lhs = Expr::Sub(Box::new(lhs), Box::new(parse_product(t, pos)?));
            }
            _ => break,
        }
    }
    Ok(lhs)
}

fn parse_product(t: &[char], pos: &mut usize) -> Result<Expr, String> {
    let mut lhs = parse_atom(t, pos)?;
    while *pos < t.len() {
        match t[*pos] {
            'x' | 'X' => {
                *pos += 1;
                lhs = Expr::Mul(Box::new(lhs), Box::new(parse_atom(t, pos)?));
            }
            '/' => {
                *pos += 1;
                lhs = Expr::Div(Box::new(lhs), Box::new(parse_atom(t, pos)?));
            }
            _ => break,
        }
    }
    Ok(lhs)
}

fn parse_atom(t: &[char], pos: &mut usize) -> Result<Expr, String> {
    if *pos >= t.len() {
        return Err("unexpected end of expression".into());
    }
    match t[*pos] {
        '-' => {
            *pos += 1;
            Ok(Expr::Neg(Box::new(parse_atom(t, pos)?)))
        }
        '+' => {
            *pos += 1;
            parse_atom(t, pos)
        }
        '(' => {
            *pos += 1;
            let e = parse_sum(t, pos)?;
            if *pos >= t.len() || t[*pos] != ')' {
                return Err("missing ')'".into());
            }
            *pos += 1;
            Ok(e)
        }
        '$' => {
            *pos += 1;
            let start = *pos;
            while *pos < t.len() && t[*pos].is_ascii_digit() {
                *pos += 1;
            }
            let var: String = t[start..*pos].iter().collect();
            var.parse()
                .map(Expr::Var)
                .map_err(|_| "malformed $ variable".to_string())
        }
        c if c.is_ascii_digit() || c == '.' => {
            let start = *pos;
            while *pos < t.len() && (t[*pos].is_ascii_digit() || t[*pos] == '.') {
                *pos += 1;
            }
            let num: String = t[start..*pos].iter().collect();
            num.parse()
                .map(Expr::Num)
                .map_err(|_| format!("malformed number '{num}'"))
        }
        other => Err(format!("unexpected '{other}' in expression")),
    }
}

fn eval_expr(e: &Expr, env: &HashMap<u32, f64>) -> f64 {
    match e {
        Expr::Num(v) => *v,
        Expr::Var(n) => env.get(n).copied().unwrap_or(0.0),
        Expr::Add(a, b) => eval_expr(a, env) + eval_expr(b, env),
        Expr::Sub(a, b) => eval_expr(a, env) - eval_expr(b, env),
        Expr::Mul(a, b) => eval_expr(a, env) * eval_expr(b, env),
        Expr::Div(a, b) => eval_expr(a, env) / eval_expr(b, env),
        Expr::Neg(a) => -eval_expr(a, env),
    }
}

/// Instantiate a macro with `$1..$n` bound to `args`. Exposure-on primitives
/// union in; exposure-off primitives subtract, in statement order.
fn eval_macro(def: &MacroDef, args: &[f64], unit_nm: f64) -> Result<Vec<Poly>, String> {
    let mut env: HashMap<u32, f64> = HashMap::new();
    for (i, v) in args.iter().enumerate() {
        env.insert(i as u32 + 1, *v);
    }
    let mut acc: Vec<Poly> = Vec::new();
    for stmt in &def.stmts {
        match stmt {
            MacroStmt::Assign(var, expr) => {
                let v = eval_expr(expr, &env);
                env.insert(*var, v);
            }
            MacroStmt::Primitive(code, exprs) => {
                let a: Vec<f64> = exprs.iter().map(|e| eval_expr(e, &env)).collect();
                let (exposure, ring_sets) = eval_primitive(*code, &a, unit_nm)?;
                let polys: Vec<Poly> = ring_sets.into_iter().map(ring_poly).collect();
                acc = if exposure {
                    geom::union(&acc, &polys)
                } else {
                    geom::difference(&acc, &polys)
                };
            }
        }
    }
    Ok(acc)
}

/// One macro primitive → (exposure-on?, rings in nm, already rotated).
fn eval_primitive(code: i64, a: &[f64], unit_nm: f64) -> Result<(bool, Vec<Ring>), String> {
    let need = |n: usize| -> Result<(), String> {
        if a.len() < n {
            Err(format!(
                "macro primitive {code}: expected >= {n} args, got {}",
                a.len()
            ))
        } else {
            Ok(())
        }
    };
    let exposure = |v: f64| v != 0.0;
    let rot_deg = |ring: Ring, deg: f64| -> Ring {
        if deg == 0.0 {
            return ring;
        }
        let (s, c) = deg.to_radians().sin_cos();
        ring.into_iter()
            .map(|p| {
                let (x, y) = (p.x as f64, p.y as f64);
                P::new((x * c - y * s).round() as Nm, (x * s + y * c).round() as Nm)
            })
            .collect()
    };
    match code {
        1 => {
            // exposure, diameter, cx, cy, [rot]
            need(4)?;
            let ring = circle_ring(
                P::new(
                    (a[2] * unit_nm).round() as Nm,
                    (a[3] * unit_nm).round() as Nm,
                ),
                a[1] * unit_nm / 2.0,
            );
            Ok((
                exposure(a[0]),
                vec![rot_deg(ring, *a.get(4).unwrap_or(&0.0))],
            ))
        }
        4 => {
            // exposure, n, x0,y0, …, rot. Per spec n counts the vertices
            // *after* the first (closed list, n+1 pairs); KiCad-style macros
            // also appear with n pairs total and implicit closure — accept
            // both by deducing the pair count from the arg count.
            need(2)?;
            let n = a[1] as usize;
            let pairs = if a.len() == 2 + 2 * (n + 1) + 1 {
                n + 1
            } else if a.len() == 2 + 2 * n + 1 {
                n
            } else {
                return Err(format!(
                    "macro outline: {} args inconsistent with vertex count {n}",
                    a.len()
                ));
            };
            let rot = a[2 + 2 * pairs];
            let mut ring: Ring = (0..pairs)
                .map(|k| {
                    P::new(
                        (a[2 + 2 * k] * unit_nm).round() as Nm,
                        (a[3 + 2 * k] * unit_nm).round() as Nm,
                    )
                })
                .collect();
            if ring.len() >= 2 && ring.first() == ring.last() {
                ring.pop();
            }
            if ring.len() < 3 {
                return Err("macro outline with fewer than 3 distinct vertices".into());
            }
            Ok((exposure(a[0]), vec![rot_deg(ring, rot)]))
        }
        5 => {
            // exposure, n, cx, cy, diameter, [rot]
            need(5)?;
            let n = a[1] as usize;
            if !(3..=12).contains(&n) {
                return Err(format!("macro polygon vertex count {n} out of range"));
            }
            let (cx, cy, d) = (a[2] * unit_nm, a[3] * unit_nm, a[4] * unit_nm);
            let ring: Ring = (0..n)
                .map(|k| {
                    let ang = k as f64 * std::f64::consts::TAU / n as f64;
                    P::new(
                        (cx + d / 2.0 * ang.cos()).round() as Nm,
                        (cy + d / 2.0 * ang.sin()).round() as Nm,
                    )
                })
                .collect();
            Ok((
                exposure(a[0]),
                vec![rot_deg(ring, *a.get(5).unwrap_or(&0.0))],
            ))
        }
        20 => {
            // exposure, width, x1,y1, x2,y2, [rot] — rectangle along the line
            need(6)?;
            let w = a[1] * unit_nm;
            let (x1, y1, x2, y2) = (
                a[2] * unit_nm,
                a[3] * unit_nm,
                a[4] * unit_nm,
                a[5] * unit_nm,
            );
            let (dx, dy) = (x2 - x1, y2 - y1);
            let len = dx.hypot(dy);
            if len < 1.0 || w < 1.0 {
                return Err("macro vector line degenerate".into());
            }
            let (nx, ny) = (-dy / len * w / 2.0, dx / len * w / 2.0);
            let ring: Ring = vec![
                P::new((x1 + nx).round() as Nm, (y1 + ny).round() as Nm),
                P::new((x1 - nx).round() as Nm, (y1 - ny).round() as Nm),
                P::new((x2 - nx).round() as Nm, (y2 - ny).round() as Nm),
                P::new((x2 + nx).round() as Nm, (y2 + ny).round() as Nm),
            ];
            Ok((
                exposure(a[0]),
                vec![rot_deg(ring, *a.get(6).unwrap_or(&0.0))],
            ))
        }
        21 => {
            // exposure, width, height, cx, cy, [rot]
            need(5)?;
            let (w, h) = (a[1] * unit_nm, a[2] * unit_nm);
            let (cx, cy) = (a[3] * unit_nm, a[4] * unit_nm);
            let ring: Ring = vec![
                P::new((cx - w / 2.0).round() as Nm, (cy - h / 2.0).round() as Nm),
                P::new((cx + w / 2.0).round() as Nm, (cy - h / 2.0).round() as Nm),
                P::new((cx + w / 2.0).round() as Nm, (cy + h / 2.0).round() as Nm),
                P::new((cx - w / 2.0).round() as Nm, (cy + h / 2.0).round() as Nm),
            ];
            Ok((
                exposure(a[0]),
                vec![rot_deg(ring, *a.get(5).unwrap_or(&0.0))],
            ))
        }
        other => Err(format!("unsupported macro primitive {other}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::NM_PER_MM;

    const HEADER: &str =
        "%TF.FileFunction,Copper,L1,Top*%\n%FSLAX46Y46*%\n%MOMM*%\nG04 test*\n%LPD*%\nG01*\n";

    fn parse(body: &str) -> Layer {
        let src = format!("{HEADER}{body}M02*\n");
        parse_gerber(&src).expect("parse")
    }

    fn area_mm2(layer: &Layer) -> f64 {
        geom::area_nm2(&layer.polys) / (NM_PER_MM as f64 * NM_PER_MM as f64)
    }

    #[test]
    fn x46_mm_coordinates_are_exact_nanometers() {
        // Flash a 0.2 mm circle at X=1.234567 mm — raw int 1234567 == nm.
        let layer = parse("%ADD10C,0.200000*%\nD10*\nX1234567Y-500000D03*\n");
        assert_eq!(layer.polys.len(), 1);
        let c = &layer.polys[0].outer;
        let cx: i64 = c.iter().map(|p| p.x).sum::<i64>() / c.len() as i64;
        let cy: i64 = c.iter().map(|p| p.y).sum::<i64>() / c.len() as i64;
        assert!((cx - 1_234_567).abs() < 10, "cx={cx}");
        assert!((cy + 500_000).abs() < 10, "cy={cy}");
    }

    #[test]
    fn circle_flash_area_matches_pi_r_squared() {
        let layer = parse("%ADD10C,1.000000*%\nD10*\nX0Y0D03*\n");
        let expected = std::f64::consts::PI * 0.25;
        assert!(
            (area_mm2(&layer) - expected).abs() / expected < 0.001,
            "area {}",
            area_mm2(&layer)
        );
    }

    #[test]
    fn a_combined_fs_mo_block_dispatches_both_records() {
        // `%FSLAX46Y46*MOMM*%` is one block with two records; the old code
        // dispatched only FS, dropped MO, then died "coordinate before %MO%"
        // (LR-13).
        let src = "%TF.FileFunction,Copper,L1,Top*%\n%FSLAX46Y46*MOMM*%\n\
                   %ADD10C,1.000000*%\nD10*\nX0Y0D03*\nM02*\n";
        let layer = parse_gerber(src).expect("combined FS/MO block parses");
        assert_eq!(layer.polys.len(), 1);
    }

    #[test]
    fn chained_polarity_records_all_apply() {
        // `%LPC*LPD*%` must apply BOTH records, ending dark. The old code
        // applied only LPC (clear), so the following flash subtracted instead
        // of adding — silently wrong copper (LR-13).
        let src = "%TF.FileFunction,Copper,L1,Top*%\n%FSLAX46Y46*%\n%MOMM*%\n\
                   %ADD10C,1.000000*%\nD10*\n%LPC*LPD*%\nX0Y0D03*\nM02*\n";
        let layer = parse_gerber(src).expect("parse");
        assert!(!layer.polys.is_empty(), "final polarity must be dark (LPD won)");
    }

    #[test]
    fn aperture_before_unit_mode_is_an_error() {
        // No %MO% yet ⇒ mm-vs-inch unknown ⇒ hard error, never a silent mm
        // assumption that would shrink an inch aperture 25.4× (LR-12).
        let src = "%FSLAX46Y46*%\n%ADD10C,0.200000*%\n%MOMM*%\nM02*\n";
        let err = parse_gerber(src).expect_err("aperture before MO must error");
        assert!(format!("{err}").contains("MO"), "got: {err}");
    }

    #[test]
    fn rect_flash_is_exact() {
        let layer = parse("%ADD11R,2.000000X1.000000*%\nD11*\nX1000000Y2000000D03*\n");
        assert_eq!(layer.polys.len(), 1);
        assert!((area_mm2(&layer) - 2.0).abs() < 1e-9);
        let xs: Vec<i64> = layer.polys[0].outer.iter().map(|p| p.x).collect();
        assert_eq!(xs.iter().min(), Some(&0));
        assert_eq!(xs.iter().max(), Some(&2_000_000));
    }

    #[test]
    fn obround_flash_area() {
        // 2 x 1 mm obround = 1x1 rect + two semicircle caps r=0.5.
        let layer = parse("%ADD12O,2.000000X1.000000*%\nD12*\nX0Y0D03*\n");
        let expected = 1.0 + std::f64::consts::PI * 0.25;
        assert!((area_mm2(&layer) - expected).abs() / expected < 0.001);
    }

    #[test]
    fn aperture_hole_is_subtracted() {
        let layer = parse("%ADD10C,1.000000X0.500000*%\nD10*\nX0Y0D03*\n");
        let expected = std::f64::consts::PI * (0.25 - 0.0625);
        assert!((area_mm2(&layer) - expected).abs() / expected < 0.002);
        assert_eq!(layer.polys[0].holes.len(), 1);
    }

    #[test]
    fn trace_stroke_is_a_capsule() {
        // 10 mm horizontal trace, 0.25 mm wide.
        let layer = parse("%ADD10C,0.250000*%\nD10*\nX0Y0D02*\nX10000000Y0D01*\n");
        let expected = 10.0 * 0.25 + std::f64::consts::PI * 0.125 * 0.125;
        assert!(
            (area_mm2(&layer) - expected).abs() / expected < 0.001,
            "area {}",
            area_mm2(&layer)
        );
    }

    #[test]
    fn connected_traces_union_into_one_poly() {
        let layer =
            parse("%ADD10C,0.250000*%\nD10*\nX0Y0D02*\nX5000000Y0D01*\nX5000000Y5000000D01*\n");
        assert_eq!(layer.polys.len(), 1, "L-trace must union into one shape");
    }

    #[test]
    fn zero_length_stroke_is_a_dot() {
        let layer = parse("%ADD10C,0.300000*%\nD10*\nX0Y0D02*\nX0Y0D01*\n");
        let expected = std::f64::consts::PI * 0.15 * 0.15;
        assert!((area_mm2(&layer) - expected).abs() / expected < 0.001);
    }

    #[test]
    fn g36_region_square_is_exact() {
        let layer = parse(
            "G36*\nX0Y0D02*\nX3000000Y0D01*\nX3000000Y3000000D01*\nX0Y3000000D01*\nX0Y0D01*\nG37*\n",
        );
        assert_eq!(layer.polys.len(), 1);
        assert!((area_mm2(&layer) - 9.0).abs() < 1e-9);
    }

    #[test]
    fn zone_with_slit_hole_yields_poly_with_hole() {
        // KiCad zones connect holes to the outer contour with a zero-width
        // slit: outer square 10x10 with inner square 4x4 drawn via a slit.
        let layer = parse(concat!(
            "G36*\n",
            "X0Y0D02*\n",
            "X10000000Y0D01*\nX10000000Y10000000D01*\nX0Y10000000D01*\nX0Y5000000D01*\n",
            // slit in to the hole boundary, around it CW, back out
            "X3000000Y5000000D01*\n",
            "X3000000Y7000000D01*\nX7000000Y7000000D01*\nX7000000Y3000000D01*\nX3000000Y3000000D01*\n",
            "X3000000Y5000000D01*\nX0Y5000000D01*\nX0Y0D01*\n",
            "G37*\n",
        ));
        let total = area_mm2(&layer);
        assert!((total - (100.0 - 16.0)).abs() < 0.01, "area {total}");
        assert!(
            layer.polys.iter().any(|p| !p.holes.is_empty()),
            "slit contour must produce a hole"
        );
    }

    #[test]
    fn clear_polarity_subtracts() {
        let layer = parse(concat!(
            "%ADD10C,2.000000*%\n%ADD11C,1.000000*%\n",
            "D10*\nX0Y0D03*\n",
            "%LPC*%\nD11*\nX0Y0D03*\n%LPD*%\n",
        ));
        let expected = std::f64::consts::PI * (1.0 - 0.25);
        assert!((area_mm2(&layer) - expected).abs() / expected < 0.002);
    }

    #[test]
    fn full_circle_arc_stroke_is_an_annulus() {
        // Full circle: start == end, center offset I=-2mm; r=2mm, w=0.2mm.
        let layer =
            parse("%ADD10C,0.200000*%\nD10*\nG75*\nX2000000Y0D02*\nG03X2000000Y0I-2000000J0D01*\n");
        assert_eq!(layer.polys.len(), 1);
        assert_eq!(layer.polys[0].holes.len(), 1, "annulus needs a hole");
        let expected = std::f64::consts::PI * ((2.1f64).powi(2) - (1.9f64).powi(2));
        assert!(
            (area_mm2(&layer) - expected).abs() / expected < 0.005,
            "area {}",
            area_mm2(&layer)
        );
    }

    #[test]
    fn quarter_arc_endpoints_are_exact() {
        let layer =
            parse("%ADD10C,0.200000*%\nD10*\nG75*\nX1000000Y0D02*\nG03X0Y1000000I-1000000J0D01*\n");
        // End cap centers at (1,0) and (0,1) mm must be covered.
        let polys = &layer.polys;
        let covers = |x: Nm, y: Nm| cam::ablation::point_in_polys(P::new(x, y), polys, 0);
        assert!(covers(1_000_000, 0) && covers(0, 1_000_000));
        assert!(!covers(0, 0), "arc must not cover its center");
    }

    #[test]
    fn roundrect_macro_flash() {
        // KiCad-style RoundRect: outline of the inner rect + 4 corner circles
        // + 4 edge rectangles expressed with primitive 4 and 1. Simplified
        // here to inner-rect outline + 4 circles + 2 edge center-lines (21).
        // 2x1 mm pad, corner radius 0.25.
        let src = concat!(
            "%AMRoundRect*\n",
            "0 rectangle with rounded corners*\n",
            "0 $1 rounding radius, $2..$9 inner rect corners*\n",
            "4,1,4,$2,$3,$4,$5,$6,$7,$8,$9,0*\n",
            "1,1,$1+$1,$2,$3*\n1,1,$1+$1,$4,$5*\n1,1,$1+$1,$6,$7*\n1,1,$1+$1,$8,$9*\n",
            "21,1,2.0,0.5,0,0,0*\n21,1,1.5,1.0,0,0,0*\n",
            "%\n",
            "%ADD13RoundRect,0.250000X-0.750000X-0.250000X0.750000X-0.250000X0.750000X0.250000X-0.750000X0.250000*%\n",
            "D13*\nX0Y0D03*\n",
        );
        let layer = parse(src);
        assert_eq!(layer.polys.len(), 1);
        // Exact rounded-rect area: 2x1 minus 4 corner squares + 4 quarter circles
        let r = 0.25f64;
        let expected = 2.0 * 1.0 - 4.0 * r * r + std::f64::consts::PI * r * r;
        assert!(
            (area_mm2(&layer) - expected).abs() / expected < 0.005,
            "area {}",
            area_mm2(&layer)
        );
    }

    #[test]
    fn macro_exposure_off_subtracts() {
        let src = concat!(
            "%AMDONUT*\n1,1,$1,0,0*\n1,0,$2,0,0*\n%\n",
            "%ADD14DONUT,1.000000X0.500000*%\nD14*\nX0Y0D03*\n",
        );
        let layer = parse(src);
        let expected = std::f64::consts::PI * (0.25 - 0.0625);
        assert!((area_mm2(&layer) - expected).abs() / expected < 0.002);
    }

    #[test]
    fn inch_units_scale() {
        let src = "%FSLAX24Y24*%\n%MOIN*%\n%ADD10C,0.100000*%\nD10*\nX10000Y0D03*\nM02*\n";
        let layer = parse_gerber(src).expect("parse");
        // X10000 with 2.4 inch format = 1.0000 in = 25.4 mm.
        let cx: i64 = layer.polys[0].outer.iter().map(|p| p.x).sum::<i64>()
            / layer.polys[0].outer.len() as i64;
        assert!((cx - 25_400_000).abs() < 20, "cx={cx}");
    }

    #[test]
    fn polygon_aperture() {
        let layer = parse("%ADD15P,2.000000X6*%\nD15*\nX0Y0D03*\n");
        // Regular hexagon, circumradius 1 mm.
        let expected = 3.0 * (3.0f64).sqrt() / 2.0;
        assert!((area_mm2(&layer) - expected).abs() / expected < 1e-6);
    }

    #[test]
    fn unsupported_constructs_error_loudly() {
        for (body, needle) in [
            ("G74*\n", "G74"),
            ("G91*\n", "G91"),
            ("%SRX2Y1I5J5*%\n", "step-repeat"),
            ("%LM X*%\n", "load-mirror"),
            ("%ADD10C,0.2*%\nD10*\nX0Y0D02*\nG74*\n", "G74"),
            ("D99*\n", "undefined aperture"),
            ("X0Y0D03*\n", "no aperture"),
        ] {
            let src = format!("{HEADER}{body}M02*\n");
            let err = parse_gerber(&src).expect_err(needle);
            assert!(
                err.msg.to_lowercase().contains(&needle.to_lowercase()),
                "expected '{needle}' in '{}'",
                err.msg
            );
        }
    }

    #[test]
    fn missing_m02_is_an_error() {
        let err = parse_gerber(HEADER).expect_err("no M02");
        assert!(err.msg.contains("M02"));
    }

    #[test]
    fn x2_attributes_and_comments_are_skipped() {
        let layer = parse(concat!(
            "%TO.C,R5*%\nG04 #@! TO.C,R5*\n",
            "%ADD10C,0.200000*%\nD10*\nX0Y0D03*\n%TD*%\n",
        ));
        assert_eq!(layer.polys.len(), 1);
    }
}
