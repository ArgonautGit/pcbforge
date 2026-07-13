//! QA-5 — `cargo xtask seed-defect`: inject a known artwork defect into a
//! board's copper and emit modified artwork with built-in golden checks.
//!
//! Given a `.kicad_pcb`, the F.Cu copper is exported as Gerber via
//! [`ingest::kicad_cli`], parsed with [`ingest::gerber`], modified with
//! [`cam::geom`], and re-emitted as an SVG in the exact dialect
//! [`ingest::svg::load_kicad_svg`] accepts (mm units, black `evenodd` fills,
//! y-down SVG frame), plus a color debug preview PNG. The tool then *reloads
//! its own SVG* and requires the reloaded artwork to rasterize identically
//! (>= [`GOLDEN_MIN_AGREEMENT`] agreement at [`GOLDEN_UM_PER_PX`] µm/px) to
//! the in-memory defective geometry — the round-trip is the golden check —
//! and verifies the seeded defect is actually detectable (below). Downstream
//! consumers (the ORC-3 live gate, QA-2) load the emitted SVG like any other
//! plotted artwork.
//!
//! # Defect specs are geometric (documented deviation)
//!
//! The backlog phrased `--sliver` as "between `<netA>` `<netB>`", but nets
//! are not parsed yet (ING-4 pending), so the spec is **geometric**: the
//! caller names a location in the KiCad board frame and the sliver shorts
//! whatever copper it touches. Net-aware placement arrives with ING-4.
//!
//! * `--sliver X,Y,W_UM,LEN_MM,ANGLE_DEG` — union a `W_UM`-wide,
//!   `LEN_MM`-long rectangle centered at board-frame (X, Y) mm into the
//!   copper. `ANGLE_DEG` rotates the long axis away from board +X, positive
//!   toward board −Y (counter-clockwise in the y-up geometry frame);
//!   90 = vertical.
//! * `--thin X,Y,RADIUS_MM,THIN_UM` — replace copper ∩ disc(X, Y, RADIUS_MM)
//!   with erode(copper ∩ disc, THIN_UM/2): the local copper loses `THIN_UM`
//!   of total width. The erosion also pulls back from the artificial cut at
//!   the disc rim, so the thinned piece is additionally separated from the
//!   surrounding copper by THIN_UM/2 — the defect is a genuine open *plus* a
//!   sub-floor-thin trace, which is exactly what the opens test wants. This
//!   is the "simpler, test-satisfying" semantics the task settled on, in
//!   place of erosion proportional to the measured local trace width.
//!
//! # Frames
//!
//! KiCad's Gerber plot maps board-frame (x, y) mm — y down, as written in
//! the `.kicad_pcb` — to (x, −y) in the y-up Gerber/geometry frame (verified
//! against `kicad-cli` 7 output); defect specs are given in the board frame
//! and mapped the same way. The emitted SVG is y-down (`svg_y = −geom_y`),
//! which `load_kicad_svg` flips back; its viewBox origin is integer mm, so
//! the reload's frame translation is an exact pixel multiple at any integer
//! µm/px pitch and drops out of the raster comparison.
//!
//! # Detectability verification
//!
//! * `--sliver`: at floor `2·W_UM`, [`cam::drc`] must report a violation
//!   near (X, Y) that the unmodified board did not have there, **or** the
//!   copper connected-component count must drop (a bridging sliver).
//! * `--thin`: the copper area must strictly decrease, and at floor
//!   `THIN_UM` (> the THIN_UM/2 rim gap) `cam::drc` must report a new
//!   violation near (X, Y), **or** the component count must rise (an open).

use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use cam::drc::{Violation, drc};
use cam::geom;
use image::{GrayImage, RgbImage};
use ingest::kicad_cli::KicadCli;
use pcb_core::{Layer, NM_PER_MM, Nm, P, Poly, Ring};
use testkit::{BINARY_THRESHOLD, rasterize};

/// Pixel pitch of the golden round-trip rasters, µm/px.
pub const GOLDEN_UM_PER_PX: u32 = 10;

/// Minimum pixel agreement between in-memory and reloaded artwork.
pub const GOLDEN_MIN_AGREEMENT: f64 = 0.999;

/// Maximum chord sagitta when tessellating the `--thin` disc, nm (1 µm).
const DISC_CHORD_TOL_NM: f64 = 1_000.0;

/// Margin (mm) added to the "near the defect" radius when matching DRC
/// violations to the seeded defect.
const NEAR_MARGIN_MM: f64 = 0.5;

/// CLI usage line for the `seed-defect` command.
pub const USAGE: &str = "usage: cargo xtask seed-defect --board <path.kicad_pcb> --out <dir> \
     (--sliver X,Y,W_UM,LEN_MM,ANGLE_DEG | --thin X,Y,RADIUS_MM,THIN_UM)";

// ---------------------------------------------------------------------------
// Defect specs
// ---------------------------------------------------------------------------

/// A copper bridge: `w_um` × `len_mm` rectangle at board-frame (x, y) mm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliverSpec {
    pub x_mm: f64,
    pub y_mm: f64,
    pub w_um: f64,
    pub len_mm: f64,
    pub angle_deg: f64,
}

/// Local erosion: thin the copper inside disc(x, y, radius) by `thin_um`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThinSpec {
    pub x_mm: f64,
    pub y_mm: f64,
    pub radius_mm: f64,
    pub thin_um: f64,
}

/// One seeded defect (see module docs for exact semantics).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DefectSpec {
    Sliver(SliverSpec),
    Thin(ThinSpec),
}

impl DefectSpec {
    /// Parse `--sliver X,Y,W_UM,LEN_MM,ANGLE_DEG`.
    pub fn parse_sliver(arg: &str) -> Result<Self> {
        let v = parse_floats(arg, 5, "--sliver X,Y,W_UM,LEN_MM,ANGLE_DEG")?;
        ensure!(v[2] > 0.0, "--sliver W_UM must be positive, got {}", v[2]);
        ensure!(v[3] > 0.0, "--sliver LEN_MM must be positive, got {}", v[3]);
        Ok(DefectSpec::Sliver(SliverSpec {
            x_mm: v[0],
            y_mm: v[1],
            w_um: v[2],
            len_mm: v[3],
            angle_deg: v[4],
        }))
    }

    /// Parse `--thin X,Y,RADIUS_MM,THIN_UM`.
    pub fn parse_thin(arg: &str) -> Result<Self> {
        let v = parse_floats(arg, 4, "--thin X,Y,RADIUS_MM,THIN_UM")?;
        ensure!(
            v[2] > 0.0,
            "--thin RADIUS_MM must be positive, got {}",
            v[2]
        );
        ensure!(v[3] > 0.0, "--thin THIN_UM must be positive, got {}", v[3]);
        Ok(DefectSpec::Thin(ThinSpec {
            x_mm: v[0],
            y_mm: v[1],
            radius_mm: v[2],
            thin_um: v[3],
        }))
    }

    /// Defect center in the y-up geometry frame (board y is negated).
    pub fn center_geom(&self) -> P {
        match self {
            DefectSpec::Sliver(s) => P::from_mm(s.x_mm, -s.y_mm),
            DefectSpec::Thin(t) => P::from_mm(t.x_mm, -t.y_mm),
        }
    }

    /// DRC floor (mm) used for the detectability verification.
    pub fn verify_floor_mm(&self) -> f64 {
        match self {
            DefectSpec::Sliver(s) => 2.0 * s.w_um / 1_000.0,
            DefectSpec::Thin(t) => t.thin_um / 1_000.0,
        }
    }

    /// Radius (mm) around the defect center within which a DRC violation is
    /// attributed to the seeded defect.
    pub fn near_radius_mm(&self) -> f64 {
        let floor = self.verify_floor_mm();
        match self {
            DefectSpec::Sliver(s) => s.len_mm / 2.0 + floor + NEAR_MARGIN_MM,
            DefectSpec::Thin(t) => t.radius_mm + floor + NEAR_MARGIN_MM,
        }
    }

    /// One-line human description (board-frame coordinates).
    pub fn summary(&self) -> String {
        match self {
            DefectSpec::Sliver(s) => format!(
                "sliver {} µm × {} mm at board ({}, {}) mm, {}°",
                s.w_um, s.len_mm, s.x_mm, s.y_mm, s.angle_deg
            ),
            DefectSpec::Thin(t) => format!(
                "thin by {} µm within {} mm of board ({}, {}) mm",
                t.thin_um, t.radius_mm, t.x_mm, t.y_mm
            ),
        }
    }
}

/// Parse exactly `n` comma-separated floats.
fn parse_floats(arg: &str, n: usize, what: &str) -> Result<Vec<f64>> {
    let parts: Vec<&str> = arg.split(',').collect();
    ensure!(
        parts.len() == n,
        "expected {n} comma-separated numbers for {what}, got {} in '{arg}'",
        parts.len()
    );
    parts
        .iter()
        .map(|p| {
            p.trim()
                .parse::<f64>()
                .map_err(|e| anyhow!("bad number '{p}' in {what}: {e}"))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Defect geometry (all in the y-up geometry frame, integer nm)
// ---------------------------------------------------------------------------

/// The sliver rectangle as a CCW quad.
fn sliver_poly(s: &SliverSpec) -> Poly {
    let a = s.angle_deg.to_radians();
    let (ux, uy) = (a.cos(), a.sin()); // long axis (geometry frame)
    let (vx, vy) = (-uy, ux); // width axis
    let hl = s.len_mm / 2.0;
    let hw = s.w_um / 2_000.0; // half width, µm → mm
    let (cx, cy) = (s.x_mm, -s.y_mm);
    let pt = |su: f64, sv: f64| {
        P::from_mm(
            cx + su * ux * hl + sv * vx * hw,
            cy + su * uy * hl + sv * vy * hw,
        )
    };
    Poly {
        outer: vec![pt(1.0, 1.0), pt(-1.0, 1.0), pt(-1.0, -1.0), pt(1.0, -1.0)],
        holes: vec![],
    }
}

/// The `--thin` disc, tessellated CCW with chord sagitta ≤ 1 µm.
fn disc_poly(cx_mm: f64, cy_mm: f64, r_mm: f64) -> Poly {
    let r_nm = r_mm * NM_PER_MM as f64;
    // sagitta ≈ r·θ²/8 ≤ tol  ⇒  θ ≤ sqrt(8·tol/r)
    let theta = (8.0 * DISC_CHORD_TOL_NM / r_nm).sqrt();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (std::f64::consts::TAU / theta).ceil().clamp(16.0, 4096.0) as usize;
    let c = P::from_mm(cx_mm, cy_mm);
    let outer: Ring = (0..n)
        .map(|k| {
            let ang = k as f64 * std::f64::consts::TAU / n as f64;
            P::new(
                c.x + (r_nm * ang.cos()).round() as Nm,
                c.y + (r_nm * ang.sin()).round() as Nm,
            )
        })
        .collect();
    Poly {
        outer,
        holes: vec![],
    }
}

/// Apply `spec` to normalized copper, returning normalized modified copper.
pub fn apply_defect(copper: &[Poly], spec: &DefectSpec) -> Vec<Poly> {
    match spec {
        DefectSpec::Sliver(s) => geom::union(copper, &[sliver_poly(s)]),
        DefectSpec::Thin(t) => {
            let disc = [disc_poly(t.x_mm, -t.y_mm, t.radius_mm)];
            let inside = geom::intersect(copper, &disc);
            let rest = geom::difference(copper, &disc);
            let erode_nm = (t.thin_um * 1_000.0 / 2.0).round() as Nm;
            geom::union(&rest, &geom::offset(&inside, -erode_nm))
        }
    }
}

// ---------------------------------------------------------------------------
// SVG emission (the dialect ingest::svg::load_kicad_svg accepts)
// ---------------------------------------------------------------------------

/// Render `polys` as an SVG in the KiCad-plot style `load_kicad_svg` parses:
/// physical mm units with a matching viewBox, y-down coordinates, and one
/// black `fill-rule="evenodd"` path per polygon (outer + hole subpaths). The
/// viewBox origin is integer mm so the reload translation is pixel-aligned.
pub fn layer_svg(polys: &[Poly]) -> String {
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in vertices(polys) {
        let (sx, sy) = (p.x_mm(), -p.y_mm()); // geometry y-up → SVG y-down
        min_x = min_x.min(sx);
        min_y = min_y.min(sy);
        max_x = max_x.max(sx);
        max_y = max_y.max(sy);
    }
    if polys.is_empty() {
        (min_x, min_y, max_x, max_y) = (0.0, 0.0, 1.0, 1.0);
    }
    let x0 = (min_x - 1.0).floor();
    let y0 = (min_y - 1.0).floor();
    let w = (max_x + 1.0).ceil() - x0;
    let h = (max_y + 1.0).ceil() - y0;
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}mm\" height=\"{h}mm\" \
         viewBox=\"{x0} {y0} {w} {h}\">\n\
         <!-- modified copper artwork generated by `cargo xtask seed-defect` (QA-5) -->\n"
    );
    for poly in polys {
        s.push_str("<path fill=\"#000000\" fill-rule=\"evenodd\" d=\"");
        ring_d(&poly.outer, &mut s);
        for hole in &poly.holes {
            ring_d(hole, &mut s);
        }
        s.push_str("\"/>\n");
    }
    s.push_str("</svg>\n");
    s
}

/// Append one closed subpath (geometry frame → y-down SVG mm).
fn ring_d(ring: &Ring, d: &mut String) {
    for (i, p) in ring.iter().enumerate() {
        let cmd = if i == 0 { 'M' } else { 'L' };
        let _ = write!(d, "{cmd} {:.6},{:.6} ", p.x_mm(), -p.y_mm());
    }
    d.push('Z');
}

// ---------------------------------------------------------------------------
// Rasters
// ---------------------------------------------------------------------------

fn vertices(polys: &[Poly]) -> impl Iterator<Item = &P> {
    polys
        .iter()
        .flat_map(|p| std::iter::once(&p.outer).chain(p.holes.iter()))
        .flatten()
}

/// Bounding box over every vertex of every poly set.
fn bounds_of(sets: &[&[Poly]]) -> Option<(P, P)> {
    let mut it = sets.iter().flat_map(|s| vertices(s));
    let first = *it.next()?;
    let (mut min, mut max) = (first, first);
    for p in it {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x);
        max.y = max.y.max(p.y);
    }
    Some((min, max))
}

/// Rasterize `polys` in the fixed frame `(min, max)` by pinning the bounding
/// box with a degenerate two-vertex ring: testkit documents its image frame
/// as the bbox of *all* vertices, and rings with fewer than three vertices
/// contribute no filled area under its even-odd scanline fill.
fn rasterize_pinned(polys: &[Poly], min: P, max: P, um_per_px: u32) -> GrayImage {
    let mut pinned = polys.to_vec();
    pinned.push(Poly {
        outer: vec![min, max],
        holes: vec![],
    });
    rasterize(&Layer { polys: pinned }, um_per_px)
}

/// Fraction of agreeing pixels between two binary rasters whose content is
/// top-left anchored (testkit's convention). Dimensions may differ by up to
/// two pixels of bbox rounding; the images are zero-padded to match.
pub fn raster_agreement(a: &GrayImage, b: &GrayImage) -> Result<f64> {
    ensure!(
        a.width().abs_diff(b.width()) <= 2 && a.height().abs_diff(b.height()) <= 2,
        "raster sizes diverge beyond rounding: {:?} vs {:?}",
        a.dimensions(),
        b.dimensions()
    );
    let (w, h) = (a.width().max(b.width()), a.height().max(b.height()));
    let total = u64::from(w) * u64::from(h);
    if total == 0 {
        return Ok(1.0);
    }
    let on = |img: &GrayImage, x: u32, y: u32| {
        x < img.width() && y < img.height() && img.get_pixel(x, y)[0] >= BINARY_THRESHOLD
    };
    let mut agreeing: u64 = 0;
    for y in 0..h {
        for x in 0..w {
            if on(a, x, y) == on(b, x, y) {
                agreeing += 1;
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    Ok(agreeing as f64 / total as f64)
}

/// Debug preview: unchanged copper light gray, removed copper red, added
/// copper green, substrate black.
fn write_preview(original: &[Poly], modified: &[Poly], path: &Path) -> Result<()> {
    let (min, max) = bounds_of(&[original, modified]).context("no copper to preview")?;
    let before = rasterize_pinned(original, min, max, GOLDEN_UM_PER_PX);
    let after = rasterize_pinned(modified, min, max, GOLDEN_UM_PER_PX);
    ensure!(
        before.dimensions() == after.dimensions(),
        "pinned rasters must share a frame"
    );
    let (w, h) = before.dimensions();
    let img = RgbImage::from_fn(w, h, |x, y| {
        let o = before.get_pixel(x, y)[0] >= BINARY_THRESHOLD;
        let m = after.get_pixel(x, y)[0] >= BINARY_THRESHOLD;
        image::Rgb(match (o, m) {
            (true, true) => [210, 210, 210],
            (true, false) => [230, 50, 50],
            (false, true) => [60, 200, 90],
            (false, false) => [0, 0, 0],
        })
    });
    img.save(path)
        .with_context(|| format!("cannot write preview {}", path.display()))
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// Export the board's F.Cu Gerber into `gerber_dir` and parse it. Returns
/// the normalized copper and the Gerber path.
pub fn export_copper(board: &Path, gerber_dir: &Path) -> Result<(Layer, PathBuf)> {
    let cli = KicadCli::discover().context("kicad-cli is required for seed-defect")?;
    let files = cli
        .export_gerbers(board, &["F.Cu"], gerber_dir)
        .with_context(|| format!("exporting F.Cu Gerber for {}", board.display()))?;
    let gerber = files
        .first()
        .cloned()
        .context("kicad-cli reported no plotted F.Cu file")?;
    let layer = ingest::gerber::load_gerber(&gerber).context("parsing the exported F.Cu Gerber")?;
    Ok((layer, gerber))
}

/// DRC violations at `floor_mm` whose location is within `near_mm` of
/// `center` (geometry frame).
pub fn violations_near(layer: &Layer, floor_mm: f64, center: P, near_mm: f64) -> Vec<Violation> {
    drc(layer, floor_mm)
        .into_iter()
        .filter(|v| dist_mm(v.location, center) <= near_mm)
        .collect()
}

fn dist_mm(a: P, b: P) -> f64 {
    ((a.x - b.x) as f64).hypot((a.y - b.y) as f64) / NM_PER_MM as f64
}

/// What `seed-defect` did and verified; `Display` gives the CLI summary.
#[derive(Debug)]
pub struct Report {
    pub board: PathBuf,
    pub spec_summary: String,
    pub gerber: PathBuf,
    pub svg: PathBuf,
    pub preview: PathBuf,
    /// Golden round-trip agreement at [`GOLDEN_UM_PER_PX`] µm/px.
    pub agreement: f64,
    pub components_before: usize,
    pub components_after: usize,
    /// DRC floor used for the detectability verification.
    pub floor_mm: f64,
    pub violations_near_before: usize,
    pub violations_near_after: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "seed-defect OK: {}", self.spec_summary)?;
        writeln!(f, "  board:  {}", self.board.display())?;
        writeln!(
            f,
            "  golden round-trip agreement @ {GOLDEN_UM_PER_PX} µm/px: {:.6} (>= {GOLDEN_MIN_AGREEMENT})",
            self.agreement
        )?;
        writeln!(
            f,
            "  copper components: {} -> {}",
            self.components_before, self.components_after
        )?;
        writeln!(
            f,
            "  drc @ floor {:.3} mm near defect: {} violation(s) before, {} after",
            self.floor_mm, self.violations_near_before, self.violations_near_after
        )?;
        writeln!(f, "  gerber:  {}", self.gerber.display())?;
        writeln!(f, "  artwork: {}", self.svg.display())?;
        write!(f, "  preview: {}", self.preview.display())
    }
}

/// Run the full pipeline: export → parse → apply defect → emit SVG + preview
/// → golden round-trip check → defect-detectability check.
pub fn run(board: &Path, out_dir: &Path, spec: &DefectSpec) -> Result<Report> {
    ensure!(board.is_file(), "board not found: {}", board.display());
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("cannot create {}", out_dir.display()))?;

    let (copper, gerber) = export_copper(board, &out_dir.join("gerber"))?;
    ensure!(!copper.polys.is_empty(), "board has no F.Cu copper");
    let modified = apply_defect(&copper.polys, spec);
    ensure!(!modified.is_empty(), "the defect erased all copper");

    let svg_path = out_dir.join("defect-F_Cu.svg");
    std::fs::write(&svg_path, layer_svg(&modified))
        .with_context(|| format!("cannot write {}", svg_path.display()))?;

    // Golden check: reload our own SVG and compare rasters.
    let reloaded = ingest::svg::load_kicad_svg(&svg_path).context("re-loading the emitted SVG")?;
    let in_memory = rasterize(
        &Layer {
            polys: modified.clone(),
        },
        GOLDEN_UM_PER_PX,
    );
    let round_trip = rasterize(&reloaded, GOLDEN_UM_PER_PX);
    let agreement = raster_agreement(&in_memory, &round_trip)?;
    if agreement < GOLDEN_MIN_AGREEMENT {
        let a = out_dir.join("golden-in-memory.png");
        let b = out_dir.join("golden-reloaded.png");
        let _ = in_memory.save(&a);
        let _ = round_trip.save(&b);
        bail!(
            "golden round-trip failed: agreement {agreement:.6} < {GOLDEN_MIN_AGREEMENT}; \
             rasters dumped to {} and {}",
            a.display(),
            b.display()
        );
    }

    // Detectability check: the defect must be real.
    let center = spec.center_geom();
    let floor_mm = spec.verify_floor_mm();
    let near_mm = spec.near_radius_mm();
    let before = violations_near(&copper, floor_mm, center, near_mm);
    let after = violations_near(
        &Layer {
            polys: modified.clone(),
        },
        floor_mm,
        center,
        near_mm,
    );
    let (comp_before, comp_after) = (copper.polys.len(), modified.len());
    let new_violation = before.is_empty() && !after.is_empty();
    let detectable = match spec {
        DefectSpec::Sliver(_) => new_violation || comp_after < comp_before,
        DefectSpec::Thin(_) => {
            geom::area_nm2(&modified) < geom::area_nm2(&copper.polys)
                && (new_violation || comp_after > comp_before)
        }
    };
    ensure!(
        detectable,
        "seeded defect is not detectable: drc @ {floor_mm:.3} mm near defect found \
         {} violation(s) before / {} after, components {comp_before} -> {comp_after}",
        before.len(),
        after.len()
    );

    let preview = out_dir.join("preview.png");
    write_preview(&copper.polys, &modified, &preview)?;

    Ok(Report {
        board: board.to_path_buf(),
        spec_summary: spec.summary(),
        gerber,
        svg: svg_path,
        preview,
        agreement,
        components_before: comp_before,
        components_after: comp_after,
        floor_mm,
        violations_near_before: before.len(),
        violations_near_after: after.len(),
    })
}

// ---------------------------------------------------------------------------
// CLI front-end
// ---------------------------------------------------------------------------

/// Parse `seed-defect` flags and run the pipeline.
pub fn cli(args: &[String]) -> Result<Report> {
    let mut board: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut spec: Option<DefectSpec> = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .map(String::as_str)
                .ok_or_else(|| anyhow!("{flag} needs a value\n{USAGE}"))
        };
        match flag.as_str() {
            "--board" => board = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--sliver" => set_spec(&mut spec, DefectSpec::parse_sliver(value()?)?)?,
            "--thin" => set_spec(&mut spec, DefectSpec::parse_thin(value()?)?)?,
            other => bail!("unknown seed-defect flag '{other}'\n{USAGE}"),
        }
    }
    let board = board.ok_or_else(|| anyhow!("--board is required\n{USAGE}"))?;
    let out = out.ok_or_else(|| anyhow!("--out is required\n{USAGE}"))?;
    let spec = spec.ok_or_else(|| anyhow!("one of --sliver / --thin is required\n{USAGE}"))?;
    run(&board, &out, &spec)
}

fn set_spec(slot: &mut Option<DefectSpec>, spec: DefectSpec) -> Result<()> {
    ensure!(
        slot.is_none(),
        "give exactly one of --sliver / --thin\n{USAGE}"
    );
    *slot = Some(spec);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (kicad-cli-free; the end-to-end pipeline is covered by
// tests/seed_defect.rs, which self-skips without kicad-cli)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use cam::drc::ViolationKind;

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

    #[test]
    fn spec_parsing_accepts_good_and_rejects_bad() {
        let s = DefectSpec::parse_sliver("108,112.5,30,4,90").unwrap();
        assert_eq!(
            s,
            DefectSpec::Sliver(SliverSpec {
                x_mm: 108.0,
                y_mm: 112.5,
                w_um: 30.0,
                len_mm: 4.0,
                angle_deg: 90.0,
            })
        );
        let t = DefectSpec::parse_thin("113, 111, 1.0, 100").unwrap();
        assert_eq!(
            t,
            DefectSpec::Thin(ThinSpec {
                x_mm: 113.0,
                y_mm: 111.0,
                radius_mm: 1.0,
                thin_um: 100.0,
            })
        );
        assert!(DefectSpec::parse_sliver("1,2,3,4").is_err(), "arity");
        assert!(
            DefectSpec::parse_sliver("1,2,x,4,5").is_err(),
            "not a number"
        );
        assert!(
            DefectSpec::parse_sliver("1,2,-30,4,0").is_err(),
            "negative width"
        );
        assert!(DefectSpec::parse_thin("1,2,3").is_err(), "arity");
        assert!(DefectSpec::parse_thin("1,2,1,-5").is_err(), "negative thin");
    }

    #[test]
    fn sliver_poly_has_spec_area_and_orientation() {
        let s = SliverSpec {
            x_mm: 10.0,
            y_mm: 20.0,
            w_um: 30.0,
            len_mm: 4.0,
            angle_deg: 90.0,
        };
        let p = sliver_poly(&s);
        // Area = 0.03 mm × 4 mm.
        let mm2 = NM_PER_MM as f64 * NM_PER_MM as f64;
        assert!((geom::poly_area(&p) / mm2 - 0.12).abs() < 1e-6);
        // Vertical: x extent 30 µm around x=10, y extent 4 mm around y=-20.
        let xs: Vec<Nm> = p.outer.iter().map(|q| q.x).collect();
        let ys: Vec<Nm> = p.outer.iter().map(|q| q.y).collect();
        assert_eq!(xs.iter().max().unwrap() - xs.iter().min().unwrap(), 30_000);
        assert_eq!(
            ys.iter().max().unwrap() - ys.iter().min().unwrap(),
            4_000_000
        );
        assert_eq!(*ys.iter().max().unwrap(), -18_000_000);
        // Union with itself keeps it a valid CCW region.
        assert_eq!(geom::union(&[p], &[]).len(), 1);
    }

    #[test]
    fn bridging_sliver_drops_component_count_and_is_drc_visible() {
        // Two pads 0.5 mm apart (board frame y = 1..2 ⇒ geometry y = -2..-1).
        let copper = geom::union(
            &[rect_mm(0.0, -2.0, 2.0, -1.0), rect_mm(2.5, -2.0, 4.5, -1.0)],
            &[],
        );
        assert_eq!(copper.len(), 2);
        let spec = DefectSpec::Sliver(SliverSpec {
            x_mm: 2.25,
            y_mm: 1.5,
            w_um: 30.0,
            len_mm: 1.0,
            angle_deg: 0.0,
        });
        let modified = apply_defect(&copper, &spec);
        assert_eq!(modified.len(), 1, "sliver must bridge the two pads");
        let floor = spec.verify_floor_mm();
        let near = spec.near_radius_mm();
        assert!(
            violations_near(&Layer { polys: copper }, floor, spec.center_geom(), near).is_empty(),
            "pads 0.5 mm apart must be clean at floor {floor}"
        );
        let after = violations_near(&Layer { polys: modified }, floor, spec.center_geom(), near);
        assert!(
            after
                .iter()
                .any(|v| v.kind == ViolationKind::TraceWidthBelowFloor),
            "30 µm sliver must be below a 60 µm floor: {after:?}"
        );
    }

    #[test]
    fn thin_erodes_severs_and_is_drc_visible() {
        // A 10 mm × 0.3 mm trace (board y = 1 centerline).
        let copper = vec![rect_mm(0.0, -1.15, 10.0, -0.85)];
        let spec = DefectSpec::Thin(ThinSpec {
            x_mm: 5.0,
            y_mm: 1.0,
            radius_mm: 1.0,
            thin_um: 100.0,
        });
        let modified = apply_defect(&copper, &spec);
        // The rim pull-back severs the trace: two outer pieces + the thinned
        // middle piece.
        assert_eq!(modified.len(), 3, "thin must sever at the disc rim");
        assert!(geom::area_nm2(&modified) < geom::area_nm2(&copper));
        // At a floor between the old (0.3) and new (0.2) width the thinned
        // piece is flagged.
        let after = violations_near(
            &Layer {
                polys: modified.clone(),
            },
            0.25,
            spec.center_geom(),
            spec.near_radius_mm(),
        );
        assert!(
            after
                .iter()
                .any(|v| v.kind == ViolationKind::TraceWidthBelowFloor),
            "0.2 mm-wide thinned copper must be below a 0.25 mm floor: {after:?}"
        );
        assert!(
            violations_near(
                &Layer { polys: copper },
                0.25,
                spec.center_geom(),
                spec.near_radius_mm()
            )
            .is_empty(),
            "the pristine 0.3 mm trace must be clean at 0.25 mm"
        );
    }

    /// The SVG writer round-trips through the real ingest parser: emitted
    /// text parses, and the reloaded geometry rasterizes identically to the
    /// in-memory geometry at the golden pitch. (Translation by the viewBox
    /// origin drops out because testkit's frame is content-bbox-relative.)
    #[test]
    fn emitted_svg_round_trips_through_ingest() {
        let mut copper = geom::union(
            &[
                rect_mm(0.0, -20.0, 30.0, -5.0),
                rect_mm(32.0, -20.0, 40.0, -5.0),
            ],
            &[],
        );
        // Punch a hole so evenodd subpaths are exercised.
        copper = geom::difference(&copper, &[rect_mm(10.0, -15.0, 20.0, -10.0)]);
        // And a rotated sliver for non-axis-aligned edges.
        let spec = DefectSpec::Sliver(SliverSpec {
            x_mm: 31.0,
            y_mm: 12.0,
            w_um: 50.0,
            len_mm: 3.0,
            angle_deg: 30.0,
        });
        let modified = apply_defect(&copper, &spec);
        assert_eq!(modified.len(), 1, "sliver bridges the two plates");
        assert_eq!(modified[0].holes.len(), 1);

        let svg = layer_svg(&modified);
        let reloaded = ingest::svg::parse_kicad_svg(&svg).expect("own output must parse");
        let a = rasterize(
            &Layer {
                polys: modified.clone(),
            },
            GOLDEN_UM_PER_PX,
        );
        let b = rasterize(&reloaded, GOLDEN_UM_PER_PX);
        let agreement = raster_agreement(&a, &b).expect("comparable sizes");
        assert!(
            agreement >= GOLDEN_MIN_AGREEMENT,
            "round-trip agreement {agreement}"
        );
        // Area survives the trip too (within tessellation noise).
        let (aa, ab) = (geom::area_nm2(&modified), geom::area_nm2(&reloaded.polys));
        assert!((aa - ab).abs() / aa < 1e-3, "area {aa} vs {ab}");
    }

    #[test]
    fn empty_layer_svg_is_still_valid() {
        let svg = layer_svg(&[]);
        let reloaded = ingest::svg::parse_kicad_svg(&svg).expect("parses");
        assert!(reloaded.polys.is_empty());
    }
}
