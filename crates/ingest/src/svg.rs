//! ING-1 — KiCad plotted-SVG copper-layer ingest.
//!
//! Parses the SVG dialect `kicad-cli pcb export svg --black-and-white
//! --exclude-drawing-sheet --page-size-mode 2` emits (obtained through
//! [`crate::kicad_cli::KicadCli::export_svg`]) into a normalized
//! [`pcb_core::Layer`] (polygons-with-holes, integer nanometers).
//!
//! # Supported subset (what KiCad 7 emits)
//!
//! * Nested `<g>` with plain style inheritance and affine transforms
//!   (usvg resolves the cascade; each path is mapped through its
//!   `abs_transform()` — usvg 0.47 keeps path data in *local* coordinates).
//! * `<path>` fills with `fill-rule:evenodd` (KiCad's pads and zones) or the
//!   default `nonzero` (usvg-lowered `<circle>` primitives). usvg lowers all
//!   path data — including elliptical `A` arcs — to move/line/quad/cubic
//!   segments.
//! * Stroked paths with **round** caps and joins (tracks, graphic lines,
//!   board-edge arcs): outlined as one capsule per flattened segment at
//!   half the stroke width; round joins fall out of capsule overlap at
//!   shared vertices, and a zero-length subpath becomes a dot. Butt/square
//!   caps, miter/bevel joins, and dash arrays are hard errors.
//! * Black-and-white paint only: black shapes add copper, white shapes
//!   subtract it (KiCad plots drill knockouts white over the copper),
//!   folded in document/paint order exactly like Gerber `%LPD%`/`%LPC%`
//!   polarity batches.
//!
//! Anything else — gradients, patterns, partial opacity, blend modes, clip
//! paths, masks, filters, raster images, text — is a hard [`SvgError`]
//! naming the construct, never a silent approximation.
//!
//! # Geometry
//!
//! Curves become polylines with chord error ≤ [`CHORD_TOL_NM`] (2 µm) via
//! adaptive midpoint subdivision (control-point distance to the chord
//! bounds the curve's distance to it by convexity). The file is parsed at
//! 25.4 dpi so one usvg canvas unit is exactly one millimeter — KiCad
//! writes physical mm width/height with a matching viewBox — and
//! `nm = unit * 1e6`. The SVG y axis points down; y is negated so the
//! parsed layer lives in the usual y-up board frame.
//!
//! # Fill-rule resolution
//!
//! Subpath rings are combined with `cam::geom` exact-integer booleans:
//! `evenodd` folds the rings with XOR (the symmetric difference *is* the
//! even-odd region); `nonzero` requires every ring of the path to share one
//! winding direction — a plain union is then exact — and errors on mixed
//! windings, which these files never contain.

use std::fmt;
use std::path::Path as FsPath;

use cam::geom;
use pcb_core::{Layer, NM_PER_MM, Nm, P, Poly, Ring};
use usvg::tiny_skia_path::PathSegment;

/// Maximum chord error when flattening curves, nm (2 µm).
pub const CHORD_TOL_NM: f64 = 2_000.0;

/// Parse at 25.4 dpi so one canvas unit is exactly 1 mm.
const MM_DPI: f32 = 25.4;

/// Chord tolerance in canvas units (mm).
const CHORD_TOL_MM: f64 = CHORD_TOL_NM / NM_PER_MM as f64;

/// Cap on adaptive-subdivision recursion; deviation shrinks ~4x per level,
/// so 24 levels cover any curve a board-sized file can contain.
const MAX_SPLIT_DEPTH: u32 = 24;

/// Error from loading or interpreting a KiCad-plotted SVG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgError {
    pub msg: String,
}

impl fmt::Display for SvgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "kicad svg parse error: {}", self.msg)
    }
}

impl std::error::Error for SvgError {}

fn err(msg: impl Into<String>) -> SvgError {
    SvgError { msg: msg.into() }
}

/// Load a KiCad-plotted SVG file into a normalized [`Layer`].
pub fn load_kicad_svg(path: &FsPath) -> Result<Layer, SvgError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| err(format!("cannot read {}: {e}", path.display())))?;
    parse_kicad_svg(&text)
}

/// Parse KiCad-plotted SVG source text into a normalized [`Layer`].
///
/// The output is the boolean result of all black (add) and white (subtract)
/// shapes folded in document order: disjoint outers (CCW) with holes (CW).
pub fn parse_kicad_svg(text: &str) -> Result<Layer, SvgError> {
    let opt = usvg::Options {
        dpi: MM_DPI,
        ..usvg::Options::default()
    };
    let tree =
        usvg::Tree::from_str(text, &opt).map_err(|e| err(format!("svg does not parse: {e}")))?;
    let mut ing = Ingester {
        batches: vec![(true, Vec::new())],
    };
    ing.group(tree.root())?;
    let mut acc: Vec<Poly> = Vec::new();
    for (dark, polys) in &ing.batches {
        if polys.is_empty() {
            continue;
        }
        acc = if *dark {
            geom::union(&acc, polys)
        } else {
            geom::difference(&acc, polys)
        };
    }
    Ok(Layer { polys: acc })
}

// ---------------------------------------------------------------------------
// Tree walk
// ---------------------------------------------------------------------------

struct Ingester {
    /// (dark?, polys) batches in paint order; consecutive same-polarity
    /// shapes share a batch (same folding scheme as the Gerber parser).
    batches: Vec<(bool, Vec<Poly>)>,
}

impl Ingester {
    fn emit(&mut self, dark: bool, polys: Vec<Poly>) {
        if polys.is_empty() {
            return;
        }
        let last = self.batches.last_mut().expect("batches never empty");
        if last.0 == dark {
            last.1.extend(polys);
        } else {
            self.batches.push((dark, polys));
        }
    }

    fn group(&mut self, g: &usvg::Group) -> Result<(), SvgError> {
        let id = g.id();
        if g.opacity().get() < 1.0 {
            return Err(err(format!(
                "group '{id}' has opacity {} (only fully opaque supported)",
                g.opacity().get()
            )));
        }
        if g.blend_mode() != usvg::BlendMode::Normal {
            return Err(err(format!(
                "group '{id}' uses blend mode {:?}",
                g.blend_mode()
            )));
        }
        if g.clip_path().is_some() {
            return Err(err(format!("group '{id}' has a clip-path")));
        }
        if g.mask().is_some() {
            return Err(err(format!("group '{id}' has a mask")));
        }
        if !g.filters().is_empty() {
            return Err(err(format!("group '{id}' has filters")));
        }
        for child in g.children() {
            match child {
                usvg::Node::Group(sub) => self.group(sub)?,
                usvg::Node::Path(p) => self.path(p)?,
                usvg::Node::Image(i) => {
                    return Err(err(format!("raster <image> '{}' unsupported", i.id())));
                }
                usvg::Node::Text(t) => {
                    return Err(err(format!("<text> '{}' unsupported", t.id())));
                }
            }
        }
        Ok(())
    }

    fn path(&mut self, p: &usvg::Path) -> Result<(), SvgError> {
        if !p.is_visible() {
            return Ok(());
        }
        // usvg 0.47 keeps path data in local coordinates; flatten the whole
        // transform stack into the points first.
        let abs = p.abs_transform();
        let data = p
            .data()
            .clone()
            .transform(abs)
            .ok_or_else(|| err(format!("path '{}': degenerate transform {abs:?}", p.id())))?;
        let subs = flatten_path(&data);
        let phases: [bool; 2] = match p.paint_order() {
            usvg::PaintOrder::FillAndStroke => [true, false],
            usvg::PaintOrder::StrokeAndFill => [false, true],
        };
        for fill_phase in phases {
            if fill_phase {
                if let Some(f) = p.fill() {
                    self.fill(p.id(), f, &subs)?;
                }
            } else if let Some(s) = p.stroke() {
                self.stroke(p.id(), s, abs, &subs)?;
            }
        }
        Ok(())
    }

    fn fill(&mut self, id: &str, f: &usvg::Fill, subs: &[Subpath]) -> Result<(), SvgError> {
        let Some(dark) = classify_paint(f.paint(), f.opacity().get(), "fill", id)? else {
            return Ok(());
        };
        // Every subpath is implicitly closed for filling. Zero-area rings
        // contribute nothing under either fill rule.
        let rings: Vec<Ring> = subs
            .iter()
            .map(|s| to_ring(&s.pts))
            .filter(|r| r.len() >= 3 && signed_area2(r) != 0)
            .collect();
        if rings.is_empty() {
            return Ok(());
        }
        let polys = match f.rule() {
            usvg::FillRule::EvenOdd => {
                // Symmetric difference of the rings is exactly the even-odd
                // region, regardless of ring orientation.
                let mut acc: Vec<Poly> = Vec::new();
                for ring in rings {
                    acc = geom::xor(&acc, &[ring_poly(ring)]);
                }
                acc
            }
            usvg::FillRule::NonZero => {
                // With one shared winding direction the nonzero region is
                // the plain union. Mixed windings (holes under nonzero)
                // never appear in KiCad output — refuse rather than guess.
                let ccw: Vec<bool> = rings.iter().map(|r| signed_area2(r) > 0).collect();
                if ccw.windows(2).any(|w| w[0] != w[1]) {
                    return Err(err(format!(
                        "path '{id}': nonzero fill with mixed-winding subpaths unsupported"
                    )));
                }
                let polys: Vec<Poly> = rings.into_iter().map(ring_poly).collect();
                geom::union(&polys, &[])
            }
        };
        self.emit(dark, polys);
        Ok(())
    }

    fn stroke(
        &mut self,
        id: &str,
        s: &usvg::Stroke,
        abs: usvg::Transform,
        subs: &[Subpath],
    ) -> Result<(), SvgError> {
        let Some(dark) = classify_paint(s.paint(), s.opacity().get(), "stroke", id)? else {
            return Ok(());
        };
        if s.dasharray().is_some() {
            return Err(err(format!("path '{id}': dashed stroke unsupported")));
        }
        if s.linecap() != usvg::LineCap::Round {
            return Err(err(format!(
                "path '{id}': stroke-linecap {:?} unsupported (only Round)",
                s.linecap()
            )));
        }
        if s.linejoin() != usvg::LineJoin::Round {
            return Err(err(format!(
                "path '{id}': stroke-linejoin {:?} unsupported (only Round)",
                s.linejoin()
            )));
        }
        // Points were already transformed; the width must scale the same
        // way, which is only well-defined for a similarity transform.
        let scale = similarity_scale(abs).ok_or_else(|| {
            err(format!(
                "path '{id}': stroked under non-uniform transform {abs:?}"
            ))
        })?;
        let r_nm = f64::from(s.width().get()) * scale * NM_PER_MM as f64 / 2.0;
        let mut polys = Vec::new();
        for sub in subs {
            let pts = to_points(&sub.pts);
            let Some(&first) = pts.first() else { continue };
            let mut a = first;
            let mut segments = 0usize;
            let closing = sub.closed.then_some(first);
            for &b in pts.iter().skip(1).chain(closing.iter()) {
                if a == b {
                    continue;
                }
                polys.push(ring_poly(capsule_ring(a, b, r_nm)));
                segments += 1;
                a = b;
            }
            if segments == 0 {
                // Zero-length subpath with a round cap: a dot.
                polys.push(ring_poly(circle_ring(first, r_nm)));
            }
        }
        self.emit(dark, polys);
        Ok(())
    }
}

/// Classify a paint: `Some(true)` = black (add copper), `Some(false)` =
/// white (subtract), `None` = fully transparent (skip). Anything else —
/// other colors, gradients, patterns, partial opacity — is an error.
fn classify_paint(
    paint: &usvg::Paint,
    opacity: f32,
    what: &str,
    id: &str,
) -> Result<Option<bool>, SvgError> {
    if opacity <= 0.0 {
        return Ok(None);
    }
    if opacity < 1.0 {
        return Err(err(format!(
            "path '{id}': {what} opacity {opacity} unsupported (only 0 or 1)"
        )));
    }
    match paint {
        usvg::Paint::Color(c) if *c == usvg::Color::black() => Ok(Some(true)),
        usvg::Paint::Color(c) if *c == usvg::Color::white() => Ok(Some(false)),
        usvg::Paint::Color(c) => Err(err(format!(
            "path '{id}': {what} color #{:02x}{:02x}{:02x} is neither black nor white \
             (expected a --black-and-white export)",
            c.red, c.green, c.blue
        ))),
        usvg::Paint::LinearGradient(_) => Err(err(format!(
            "path '{id}': {what} linear gradient unsupported"
        ))),
        usvg::Paint::RadialGradient(_) => Err(err(format!(
            "path '{id}': {what} radial gradient unsupported"
        ))),
        usvg::Paint::Pattern(_) => Err(err(format!("path '{id}': {what} pattern unsupported"))),
    }
}

/// The average scale of a similarity transform (rotation + uniform scale +
/// translation), or `None` if the transform skews or scales anisotropically.
fn similarity_scale(t: usvg::Transform) -> Option<f64> {
    let (ax, ay) = (f64::from(t.sx), f64::from(t.ky)); // image of unit x
    let (bx, by) = (f64::from(t.kx), f64::from(t.sy)); // image of unit y
    let (la, lb) = (ax.hypot(ay), bx.hypot(by));
    if la <= 0.0 || lb <= 0.0 {
        return None;
    }
    let dot = ax * bx + ay * by;
    if dot.abs() > 1e-6 * la * lb || (la - lb).abs() > 1e-6 * (la + lb) {
        return None;
    }
    Some((la + lb) / 2.0)
}

// ---------------------------------------------------------------------------
// Path flattening (canvas units == mm, f64)
// ---------------------------------------------------------------------------

/// A point in canvas units (mm, y down) during flattening.
#[derive(Clone, Copy)]
struct V {
    x: f64,
    y: f64,
}

fn v(p: usvg::tiny_skia_path::Point) -> V {
    V {
        x: f64::from(p.x),
        y: f64::from(p.y),
    }
}

fn mid(a: V, b: V) -> V {
    V {
        x: (a.x + b.x) / 2.0,
        y: (a.y + b.y) / 2.0,
    }
}

/// Distance from `p` to the segment `a`..`b` (degenerates to point distance).
fn seg_dist(p: V, a: V, b: V) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.x + t * dx, a.y + t * dy);
    (p.x - cx).hypot(p.y - cy)
}

/// One flattened subpath in canvas units.
struct Subpath {
    pts: Vec<V>,
    closed: bool,
}

/// Flatten an (already absolute) tiny-skia path into polyline subpaths with
/// chord error ≤ [`CHORD_TOL_NM`].
fn flatten_path(data: &usvg::tiny_skia_path::Path) -> Vec<Subpath> {
    let mut subs: Vec<Subpath> = Vec::new();
    let mut cur: Vec<V> = Vec::new();
    let mut finish = |cur: &mut Vec<V>, closed: bool| {
        if !cur.is_empty() {
            subs.push(Subpath {
                pts: std::mem::take(cur),
                closed,
            });
        }
    };
    for seg in data.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                finish(&mut cur, false);
                cur.push(v(p));
            }
            PathSegment::LineTo(p) => cur.push(v(p)),
            PathSegment::QuadTo(c, p) => {
                let Some(&s) = cur.last() else { continue };
                // Degree-elevate the quadratic and reuse the cubic splitter.
                let (c, p) = (v(c), v(p));
                let c1 = V {
                    x: s.x + 2.0 / 3.0 * (c.x - s.x),
                    y: s.y + 2.0 / 3.0 * (c.y - s.y),
                };
                let c2 = V {
                    x: p.x + 2.0 / 3.0 * (c.x - p.x),
                    y: p.y + 2.0 / 3.0 * (c.y - p.y),
                };
                flatten_cubic(s, c1, c2, p, 0, &mut cur);
            }
            PathSegment::CubicTo(c1, c2, p) => {
                let Some(&s) = cur.last() else { continue };
                flatten_cubic(s, v(c1), v(c2), v(p), 0, &mut cur);
            }
            PathSegment::Close => finish(&mut cur, true),
        }
    }
    finish(&mut cur, false);
    subs
}

/// Adaptive midpoint subdivision of a cubic Bézier. A cubic lies in the
/// convex hull of its control points, so once both inner controls are
/// within [`CHORD_TOL_MM`] of the chord the whole curve is too.
fn flatten_cubic(p0: V, p1: V, p2: V, p3: V, depth: u32, out: &mut Vec<V>) {
    if depth >= MAX_SPLIT_DEPTH
        || (seg_dist(p1, p0, p3) <= CHORD_TOL_MM && seg_dist(p2, p0, p3) <= CHORD_TOL_MM)
    {
        out.push(p3);
        return;
    }
    // de Casteljau split at t = 1/2.
    let p01 = mid(p0, p1);
    let p12 = mid(p1, p2);
    let p23 = mid(p2, p3);
    let p012 = mid(p01, p12);
    let p123 = mid(p12, p23);
    let m = mid(p012, p123);
    flatten_cubic(p0, p01, p012, m, depth + 1, out);
    flatten_cubic(m, p123, p23, p3, depth + 1, out);
}

// ---------------------------------------------------------------------------
// Canvas units → nanometers (y flipped to the y-up board frame)
// ---------------------------------------------------------------------------

fn to_nm(p: V) -> P {
    P::new(
        (p.x * NM_PER_MM as f64).round() as Nm,
        (-p.y * NM_PER_MM as f64).round() as Nm,
    )
}

/// Convert to nm, dropping consecutive duplicates created by rounding.
fn to_points(pts: &[V]) -> Vec<P> {
    let mut out: Vec<P> = Vec::with_capacity(pts.len());
    for &p in pts {
        let q = to_nm(p);
        if out.last() != Some(&q) {
            out.push(q);
        }
    }
    out
}

/// Convert to an implicit-closure ring (no repeated first vertex).
fn to_ring(pts: &[V]) -> Ring {
    let mut ring = to_points(pts);
    if ring.len() >= 2 && ring.first() == ring.last() {
        ring.pop();
    }
    ring
}

// ---------------------------------------------------------------------------
// Geometry helpers (all nm; same conventions as the Gerber parser)
// ---------------------------------------------------------------------------

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

/// Angular step keeping chord sagitta ≤ [`CHORD_TOL_NM`] at radius `r`.
fn max_step(r: f64) -> f64 {
    if r <= CHORD_TOL_NM {
        return std::f64::consts::FRAC_PI_2;
    }
    (2.0 * (1.0 - CHORD_TOL_NM / r).acos()).clamp(1e-3, std::f64::consts::FRAC_PI_2)
}

/// Equal-area vertex radius for an `n`-gon approximating radius `r` (the
/// polygon's area equals the disc's, so tessellation doesn't bias area).
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

/// Capsule: the round-cap stroke of the segment `a`..`b` at radius `r`.
/// Caps stay on the true radius with doubled vertex density (see the Gerber
/// parser for the area-bias rationale). CCW.
fn capsule_ring(a: P, b: P, r: f64) -> Ring {
    let r = r.max(1.0);
    let theta = ((b.y - a.y) as f64).atan2((b.x - a.x) as f64);
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
    cap(b, theta - std::f64::consts::FRAC_PI_2);
    cap(a, theta + std::f64::consts::FRAC_PI_2);
    ring
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(body: &str) -> String {
        format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="50mm" height="40mm" viewBox="0 0 50 40">{body}</svg>"##
        )
    }

    fn parse(body: &str) -> Layer {
        parse_kicad_svg(&wrap(body)).expect("parse")
    }

    fn parse_err(body: &str) -> SvgError {
        parse_kicad_svg(&wrap(body)).expect_err("expected error")
    }

    fn area_mm2(layer: &Layer) -> f64 {
        geom::area_nm2(&layer.polys) / (NM_PER_MM as f64 * NM_PER_MM as f64)
    }

    #[test]
    fn filled_rect_is_exact_nanometers() {
        let layer = parse(
            r##"<path fill="#000000" fill-rule="evenodd" d="M 10,10 L 14,10 L 14,13 L 10,13 Z"/>"##,
        );
        assert_eq!(layer.polys.len(), 1);
        assert!((area_mm2(&layer) - 12.0).abs() < 1e-9);
        let xs: Vec<Nm> = layer.polys[0].outer.iter().map(|p| p.x).collect();
        let ys: Vec<Nm> = layer.polys[0].outer.iter().map(|p| p.y).collect();
        assert_eq!(xs.iter().min(), Some(&10_000_000));
        assert_eq!(xs.iter().max(), Some(&14_000_000));
        // SVG y-down is flipped to y-up: y in [-13, -10] mm.
        assert_eq!(ys.iter().min(), Some(&-13_000_000));
        assert_eq!(ys.iter().max(), Some(&-10_000_000));
    }

    #[test]
    fn evenodd_subpaths_make_a_hole() {
        let layer = parse(concat!(
            r##"<path fill="#000000" fill-rule="evenodd" d="##,
            r##""M 0,0 L 10,0 L 10,10 L 0,10 Z M 3,3 L 7,3 L 7,7 L 3,7 Z"/>"##,
        ));
        assert_eq!(layer.polys.len(), 1);
        assert_eq!(layer.polys[0].holes.len(), 1);
        assert!((area_mm2(&layer) - 84.0).abs() < 1e-9);
    }

    #[test]
    fn circle_flattens_to_pi_r_squared() {
        let layer = parse(r##"<circle cx="10" cy="10" r="2" fill="#000000"/>"##);
        assert_eq!(layer.polys.len(), 1);
        let expected = std::f64::consts::PI * 4.0;
        assert!(
            (area_mm2(&layer) - expected).abs() / expected < 0.002,
            "area {}",
            area_mm2(&layer)
        );
    }

    #[test]
    fn arc_path_command_flattens() {
        // Filled semicircle of radius 5 via an elliptical-arc command.
        let layer = parse(r##"<path fill="#000000" d="M 10,10 A 5 5 0 0 1 20,10 L 10,10 Z"/>"##);
        let expected = std::f64::consts::PI * 25.0 / 2.0;
        assert!(
            (area_mm2(&layer) - expected).abs() / expected < 0.002,
            "area {}",
            area_mm2(&layer)
        );
    }

    #[test]
    fn white_shapes_subtract_in_document_order() {
        let layer = parse(concat!(
            r##"<path fill="#000000" d="M 0,0 L 10,0 L 10,10 L 0,10 Z"/>"##,
            r##"<circle cx="5" cy="5" r="2" fill="#ffffff"/>"##,
        ));
        assert_eq!(layer.polys.len(), 1);
        assert_eq!(
            layer.polys[0].holes.len(),
            1,
            "white circle must punch a hole"
        );
        let expected = 100.0 - std::f64::consts::PI * 4.0;
        assert!((area_mm2(&layer) - expected).abs() / expected < 0.001);
    }

    #[test]
    fn stroke_is_a_capsule() {
        let layer = parse(concat!(
            r##"<path d="M 5,5 L 15,5" fill="none" stroke="#000000" stroke-width="1" "##,
            r##"stroke-linecap="round" stroke-linejoin="round"/>"##,
        ));
        assert_eq!(layer.polys.len(), 1);
        let expected = 10.0 * 1.0 + std::f64::consts::PI * 0.25;
        assert!(
            (area_mm2(&layer) - expected).abs() / expected < 0.001,
            "area {}",
            area_mm2(&layer)
        );
    }

    #[test]
    fn zero_length_stroke_is_a_dot() {
        let layer = parse(concat!(
            r##"<path d="M 5,5 L 5,5" fill="none" stroke="#000000" stroke-width="1" "##,
            r##"stroke-linecap="round" stroke-linejoin="round"/>"##,
        ));
        let expected = std::f64::consts::PI * 0.25;
        assert!(
            (area_mm2(&layer) - expected).abs() / expected < 0.001,
            "area {}",
            area_mm2(&layer)
        );
    }

    #[test]
    fn connected_stroke_segments_union() {
        let layer = parse(concat!(
            r##"<path d="M 5,5 L 15,5 L 15,15" fill="none" stroke="#000000" stroke-width="1" "##,
            r##"stroke-linecap="round" stroke-linejoin="round"/>"##,
        ));
        assert_eq!(layer.polys.len(), 1, "L-trace must union into one shape");
    }

    #[test]
    fn group_transform_is_flattened() {
        let layer = parse(concat!(
            r##"<g transform="translate(5 5) scale(2)">"##,
            r##"<path fill="#000000" d="M 1,1 L 2,1 L 2,2 L 1,2 Z"/></g>"##,
        ));
        assert!((area_mm2(&layer) - 4.0).abs() < 1e-9);
        let xs: Vec<Nm> = layer.polys[0].outer.iter().map(|p| p.x).collect();
        assert_eq!(xs.iter().min(), Some(&7_000_000));
        assert_eq!(xs.iter().max(), Some(&9_000_000));
    }

    #[test]
    fn stroke_width_scales_with_the_transform() {
        let layer = parse(concat!(
            r##"<g transform="scale(2)">"##,
            r##"<path d="M 5,5 L 10,5" fill="none" stroke="#000000" stroke-width="1" "##,
            r##"stroke-linecap="round" stroke-linejoin="round"/></g>"##,
        ));
        // 10 mm long, 2 mm wide after the 2x scale.
        let expected = 10.0 * 2.0 + std::f64::consts::PI;
        assert!(
            (area_mm2(&layer) - expected).abs() / expected < 0.001,
            "area {}",
            area_mm2(&layer)
        );
    }

    #[test]
    fn zero_opacity_paint_is_skipped() {
        let layer =
            parse(r##"<path fill="#000000" fill-opacity="0" d="M 0,0 L 10,0 L 10,10 L 0,10 Z"/>"##);
        assert!(layer.polys.is_empty());
    }

    #[test]
    fn unsupported_constructs_error_loudly() {
        for (body, needle) in [
            (
                r##"<path d="M 5,5 L 15,5" fill="none" stroke="#000000" stroke-width="1"/>"##,
                "linecap",
            ),
            (
                concat!(
                    r##"<path d="M 5,5 L 15,5" fill="none" stroke="#000000" stroke-width="1" "##,
                    r##"stroke-linecap="round"/>"##,
                ),
                "linejoin",
            ),
            (
                concat!(
                    r##"<path d="M 5,5 L 15,5" fill="none" stroke="#000000" stroke-width="1" "##,
                    r##"stroke-linecap="round" stroke-linejoin="round" stroke-dasharray="1 1"/>"##,
                ),
                "dashed",
            ),
            (
                r##"<path fill="#808080" d="M 0,0 L 10,0 L 10,10 Z"/>"##,
                "neither black nor white",
            ),
            (
                r##"<path fill="#000000" fill-opacity="0.5" d="M 0,0 L 10,0 L 10,10 Z"/>"##,
                "opacity",
            ),
            (
                concat!(
                    r##"<defs><linearGradient id="g"><stop offset="0" stop-color="black"/>"##,
                    r##"<stop offset="1" stop-color="white"/></linearGradient></defs>"##,
                    r##"<path fill="url(#g)" d="M 0,0 L 10,0 L 10,10 Z"/>"##,
                ),
                "gradient",
            ),
            (
                concat!(
                    r##"<path fill="#000000" fill-rule="nonzero" "##,
                    r##"d="M 0,0 L 10,0 L 10,10 L 0,10 Z M 3,7 L 7,7 L 7,3 L 3,3 Z"/>"##,
                ),
                "mixed-winding",
            ),
        ] {
            let e = parse_err(body);
            assert!(
                e.msg.to_lowercase().contains(&needle.to_lowercase()),
                "expected '{needle}' in '{}'",
                e.msg
            );
        }
    }

    #[test]
    fn nonzero_fill_with_consistent_winding_unions() {
        // Two overlapping same-winding squares under nonzero: their union.
        let layer = parse(concat!(
            r##"<path fill="#000000" fill-rule="nonzero" "##,
            r##"d="M 0,0 L 10,0 L 10,10 L 0,10 Z M 5,0 L 15,0 L 15,10 L 5,10 Z"/>"##,
        ));
        assert_eq!(layer.polys.len(), 1);
        assert!((area_mm2(&layer) - 150.0).abs() < 1e-9);
    }
}
