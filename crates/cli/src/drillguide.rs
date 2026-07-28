//! ORC-7 (software half) — `pcbforge drill-guide`: step an operator through
//! hand-drilling every Excellon hole, largest bit first, confirming each hole
//! on a camera frame before advancing.
//!
//! One invocation = one step (the `pcbforge next` pattern — state lives in a
//! small text file, so the flow survives process restarts):
//!
//! 1. Load the drill file and order the holes **largest bit first** (fewest
//!    bit changes; ties broken by y then x for a stable path). A G85 slot
//!    contributes its two endpoints (drill-then-drill-then-file).
//! 2. If a previous target is pending, re-image and require a **dark hole
//!    within `tol_um` of the target** (VIS-4's detector at the bit diameter)
//!    before advancing — an undrilled or misplaced hole refuses to advance.
//! 3. Render the overlay PNG: confirmed holes ringed green, the current
//!    target crosshaired red, remaining holes dim — the operator's map.
//!
//! **Which frame these numbers live in.** The only mapping this verb has is a
//! uniform `px_per_mm` with the y axis flipped: raw (still lens-distorted)
//! pixels divided by a number the operator typed, with no bed anchor under it.
//! That is the CAMERA PLANE, not the machine frame — the console draws the same
//! distinction, where the anchorless homography fallback is "a viewing aid only,
//! labelled approximate" and export refuses until the lens and field
//! calibrations are accepted (`ui::app::projection`).
//!
//! So the guide reports only what survives the missing calibration:
//!
//! - Hole positions are printed as **drill-file mm** — the coordinates in the
//!   file the operator handed over. They are never labelled machine
//!   coordinates, because nothing here can know where the machine origin is.
//! - The confirmation number is a **displacement** between the detected hole
//!   and the search position it was sought at, measured in the camera plane and
//!   converted at `px_per_mm`. A displacement is what a change of frame leaves
//!   intact (up to the scale error in the typed `px_per_mm`), so it is a
//!   meaningful "did the drill land where the map says", whereas subtracting a
//!   drill-file coordinate from a camera-plane one is a frame collision that
//!   only reads as zero when the board happens to sit at its design
//!   coordinates.
//! - The overlay is the operator's real instrument: it is drawn in the frame
//!   the detection happened in, so it needs no calibration to be true.

use std::path::Path;

use image::{GrayImage, Rgb, RgbImage};
use ingest::excellon::DrillOp;
use nalgebra::Point2;
use pcb_core::NM_PER_MM;
use vision::{BedMap, FidShape, FiducialProfile, find_fiducials};

/// One hole the operator must drill, in drill-file mm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HoleTarget {
    pub x_mm: f64,
    pub y_mm: f64,
    /// Bit diameter, mm.
    pub d_mm: f64,
}

/// Flatten drill ops to hole targets and order them **largest bit first**
/// (all holes of one bit are consecutive, so the operator changes bits at
/// most once per size), ties broken by (y, x) for a stable, walkable path.
pub fn order_holes(ops: &[DrillOp]) -> Vec<HoleTarget> {
    let nm = |v: i64| v as f64 / NM_PER_MM as f64;
    let mut holes: Vec<HoleTarget> = ops
        .iter()
        .flat_map(|op| match *op {
            DrillOp::Hole {
                center,
                diameter_nm,
            } => vec![HoleTarget {
                x_mm: nm(center.x),
                y_mm: nm(center.y),
                d_mm: nm(diameter_nm),
            }],
            // A slot is drilled at both ends, then filed out by hand.
            DrillOp::Slot { a, b, diameter_nm } => vec![
                HoleTarget {
                    x_mm: nm(a.x),
                    y_mm: nm(a.y),
                    d_mm: nm(diameter_nm),
                },
                HoleTarget {
                    x_mm: nm(b.x),
                    y_mm: nm(b.y),
                    d_mm: nm(diameter_nm),
                },
            ],
        })
        .collect();
    holes.sort_by(|p, q| {
        q.d_mm
            .total_cmp(&p.d_mm) // largest bit first
            .then(p.y_mm.total_cmp(&q.y_mm))
            .then(p.x_mm.total_cmp(&q.x_mm))
    });
    holes
}

/// FNV-1a over the ordered hole list (µm-quantized) — the state file carries
/// it so a changed drill file invalidates stale progress instead of silently
/// mis-pairing hole indices.
pub fn fingerprint(holes: &[HoleTarget]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut eat = |v: i64| {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for t in holes {
        eat((t.x_mm * 1000.0).round() as i64);
        eat((t.y_mm * 1000.0).round() as i64);
        eat((t.d_mm * 1000.0).round() as i64);
    }
    h
}

/// Progress: which target the operator was last told to drill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuideState {
    /// Fingerprint of the ordered hole list this progress belongs to.
    pub fingerprint: u64,
    /// Index of the hole currently pending confirmation (== holes drilled so
    /// far). `pending == holes.len()` means every hole is confirmed.
    pub pending: usize,
}

impl GuideState {
    /// Serialize to the state-file text.
    pub fn render(&self) -> String {
        format!(
            "pcbforge drill-guide state v1\nfingerprint={:016x}\npending={}\n",
            self.fingerprint, self.pending
        )
    }

    /// Parse a state file previously written by [`GuideState::render`].
    pub fn parse(src: &str) -> Result<Self, String> {
        let mut lines = src.lines();
        if lines.next().map(str::trim) != Some("pcbforge drill-guide state v1") {
            return Err("not a drill-guide state file (bad header)".into());
        }
        let mut fp = None;
        let mut pending = None;
        for l in lines {
            if let Some(v) = l.strip_prefix("fingerprint=") {
                fp = u64::from_str_radix(v.trim(), 16).ok();
            } else if let Some(v) = l.strip_prefix("pending=") {
                pending = v.trim().parse::<usize>().ok();
            }
        }
        Ok(Self {
            fingerprint: fp.ok_or("state file missing fingerprint")?,
            pending: pending.ok_or("state file missing pending index")?,
        })
    }
}

/// Look for the drilled hole at `target` in `frame` and gate it: the detected
/// dark-hole center must sit within `tol_um` of where the hole was sought.
/// Returns that displacement in µm, or why the hole is not confirmed.
///
/// The displacement is measured **in pixels**, between the detection and the
/// search position the target maps to, and only then divided by `px_per_mm`.
/// Both ends are then in one frame — the camera plane — which is what makes the
/// number mean anything: `found_mm` is camera-plane mm (raw px ÷ typed
/// `px_per_mm`, no lens undistortion and no bed anchor), so differencing it
/// against a drill-file coordinate would mix two frames. See the module docs.
/// A wrong `px_per_mm` still scales this reading, so the gate is only as tight
/// as that number is right.
pub fn check_hole(
    frame: &GrayImage,
    target: &HoleTarget,
    px_per_mm: f64,
    tol_um: f64,
    search_mm: f64,
) -> Result<f64, String> {
    // y-flipped: drill coordinates are y-up (Gerber frame); image rows grow
    // downward, so bed (0,0) is the frame's bottom-left.
    let bed = BedMap::uniform_scale_y_flip(px_per_mm, frame.height() as f64);
    let profile = FiducialProfile::DarkDot {
        shape: FidShape::Circle {
            diameter_mm: target.d_mm,
        },
    };
    let target_pt = Point2::new(target.x_mm, target.y_mm);
    let expected = [target_pt];
    let res = find_fiducials(frame, &expected, search_mm, &profile, &bed);
    match &res[0] {
        Ok(f) => {
            let sought_px = bed.mm_to_px(target_pt);
            let off_um = (f.found_px - sought_px).norm() / px_per_mm * 1000.0;
            if off_um <= tol_um {
                Ok(off_um)
            } else {
                Err(format!(
                    "hole found {off_um:.0} µm from the mapped position (tolerance \
                     {tol_um:.0} µm) — re-check before advancing"
                ))
            }
        }
        Err(m) => Err(format!("no drilled hole at the target: {m:?}")),
    }
}

const GREEN: Rgb<u8> = Rgb([0x40, 0xc0, 0x50]);
const RED: Rgb<u8> = Rgb([0xe0, 0x40, 0x30]);
const DIM: Rgb<u8> = Rgb([0x90, 0x90, 0x90]);

/// Render the operator's map over `frame`: confirmed holes (below `pending`)
/// ringed green, the pending target crosshaired + ringed red, the rest dim.
pub fn render_overlay(
    frame: &GrayImage,
    holes: &[HoleTarget],
    pending: usize,
    px_per_mm: f64,
) -> RgbImage {
    let mut img = RgbImage::from_fn(frame.width(), frame.height(), |x, y| {
        let g = frame.get_pixel(x, y)[0];
        Rgb([g, g, g])
    });
    for (i, t) in holes.iter().enumerate() {
        let cx = (t.x_mm * px_per_mm).round() as i32;
        // Drill y is y-up (Gerber); image rows grow downward.
        let cy = (frame.height() as f64 - t.y_mm * px_per_mm).round() as i32;
        let r = ((t.d_mm * px_per_mm * 0.5) as i32 + 4).max(6);
        let color = match i.cmp(&pending) {
            std::cmp::Ordering::Less => GREEN,
            std::cmp::Ordering::Equal => RED,
            std::cmp::Ordering::Greater => DIM,
        };
        ring(&mut img, cx, cy, r, color);
        if i == pending {
            cross(&mut img, cx, cy, r + 6, color);
        }
    }
    img
}

fn put(img: &mut RgbImage, x: i32, y: i32, c: Rgb<u8>) {
    if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
        img.put_pixel(x as u32, y as u32, c);
    }
}

/// Midpoint circle, doubled (r and r−1) so it reads at small sizes.
fn ring(img: &mut RgbImage, cx: i32, cy: i32, r: i32, c: Rgb<u8>) {
    for rr in [r, r - 1].into_iter().filter(|v| *v > 0) {
        let (mut x, mut y, mut d) = (rr, 0, 1 - rr);
        while x >= y {
            for (px, py) in [
                (cx + x, cy + y),
                (cx + y, cy + x),
                (cx - y, cy + x),
                (cx - x, cy + y),
                (cx - x, cy - y),
                (cx - y, cy - x),
                (cx + y, cy - x),
                (cx + x, cy - y),
            ] {
                put(img, px, py, c);
            }
            y += 1;
            if d < 0 {
                d += 2 * y + 1;
            } else {
                x -= 1;
                d += 2 * (y - x) + 1;
            }
        }
    }
}

/// A `+` crosshair, thickened one pixel.
fn cross(img: &mut RgbImage, cx: i32, cy: i32, arm: i32, c: Rgb<u8>) {
    for d in -arm..=arm {
        put(img, cx + d, cy, c);
        put(img, cx + d, cy + 1, c);
        put(img, cx, cy + d, c);
        put(img, cx + 1, cy + d, c);
    }
}

/// One drill-guide step (see the module docs). Returns the human-readable
/// report lines to print; errors refuse to advance (undrilled/misplaced hole,
/// stale state, bad inputs).
#[allow(clippy::too_many_arguments)]
pub fn step(
    drills: &Path,
    frame_path: Option<&Path>,
    state_path: &Path,
    overlay_path: &Path,
    px_per_mm: f64,
    tol_um: f64,
    search_mm: f64,
    accept: bool,
    skip: bool,
) -> Result<Vec<String>, String> {
    if px_per_mm <= 0.0 {
        return Err("px per mm must be positive".into());
    }
    let ops = ingest::excellon::load_excellon_full(drills).map_err(|e| e.to_string())?;
    let holes = order_holes(&ops);
    if holes.is_empty() {
        return Err("drill file contains no holes".into());
    }
    // Frame contract: coordinates must be non-negative (aux-origin export). A
    // default KiCad export keeps the sheet frame (negative Y), which maps
    // off-frame — every hole misses and the blank canvas clips (LR-14).
    if let Some(t) = holes.iter().find(|t| t.x_mm < 0.0 || t.y_mm < 0.0) {
        return Err(format!(
            "hole at ({:.3}, {:.3}) mm has a negative coordinate — export the drill file at the \
             aux/drill origin (not the sheet frame) so all holes are non-negative",
            t.x_mm, t.y_mm
        ));
    }
    let fp = fingerprint(&holes);

    // Resume (validating the fingerprint) or start at the first hole. Only a
    // resumed guide confirms a hole — the first invocation just presents the
    // first target.
    let (mut state, started) = match std::fs::read_to_string(state_path) {
        Ok(src) => {
            let s = GuideState::parse(&src)?;
            if s.fingerprint != fp {
                return Err(format!(
                    "state file {} belongs to a different drill file — delete it to restart",
                    state_path.display()
                ));
            }
            if s.pending > holes.len() {
                return Err(format!(
                    "state file {} is corrupt: pending {} exceeds {} holes",
                    state_path.display(),
                    s.pending,
                    holes.len()
                ));
            }
            (s, true)
        }
        // Only an absent state file is a fresh start. Any other read error
        // (permissions, I/O) must surface, not silently restart at hole 0 (LR-24).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            GuideState {
                fingerprint: fp,
                pending: 0,
            },
            false,
        ),
        Err(e) => return Err(format!("read {}: {e}", state_path.display())),
    };

    let mut out = Vec::new();
    // Say which frame every number below lives in, up front. With only a typed
    // px/mm there is no bed anchor and no lens map, so nothing here is a machine
    // coordinate and the guide must not let the operator read one.
    out.push(format!(
        "frame: camera plane at {px_per_mm:.2} px/mm (uniform fallback — no lens or \
         bed calibration); positions below are DRILL-FILE mm and offsets are \
         camera-plane displacements, NOT machine coordinates"
    ));

    // Confirm the pending hole on the frame before advancing (only once the
    // guide has started — the first invocation just presents the first target).
    if started && state.pending < holes.len() {
        let t = holes[state.pending];
        // Escape hatches so a correctly-drilled hole the detector can't confirm
        // (e.g. a short slot whose two drilled ends merge into one oval, putting
        // the centroid L/2 off target) can't hard-lock the flow (LR-08).
        if skip {
            out.push(format!(
                "skipped hole #{} at ({:.3}, {:.3}) drill-file mm — NOT confirmed",
                state.pending + 1,
                t.x_mm,
                t.y_mm
            ));
            state.pending += 1;
        } else if accept {
            out.push(format!(
                "force-accepted hole #{} at ({:.3}, {:.3}) drill-file mm — gate overridden",
                state.pending + 1,
                t.x_mm,
                t.y_mm
            ));
            state.pending += 1;
        } else {
            let fpath = frame_path
                .ok_or("pass --frame <image> to confirm the pending hole (or --skip / --accept)")?;
            let frame = image::open(fpath)
                .map_err(|e| format!("open {}: {e}", fpath.display()))?
                .to_luma8();
            let off = check_hole(&frame, &t, px_per_mm, tol_um, search_mm).map_err(|e| {
                format!("{e} — rerun with --accept to force-confirm this hole or --skip to pass it")
            })?;
            out.push(format!(
                "confirmed hole #{} at ({:.3}, {:.3}) drill-file mm — {off:.0} µm from the mapped position",
                state.pending + 1,
                t.x_mm,
                t.y_mm
            ));
            state.pending += 1;
        }
    }

    // Render the map over the frame if given, else over a blank canvas sized
    // to the holes' extent (so the first, frameless invocation still maps).
    let base = match frame_path {
        Some(p) => image::open(p)
            .map_err(|e| format!("open {}: {e}", p.display()))?
            .to_luma8(),
        None => {
            let w = holes.iter().map(|t| t.x_mm).fold(0.0, f64::max) * px_per_mm + 40.0;
            let h = holes.iter().map(|t| t.y_mm).fold(0.0, f64::max) * px_per_mm + 40.0;
            GrayImage::from_pixel(w.max(64.0) as u32, h.max(64.0) as u32, image::Luma([200]))
        }
    };
    let overlay = render_overlay(&base, &holes, state.pending, px_per_mm);
    overlay
        .save(overlay_path)
        .map_err(|e| format!("save {}: {e}", overlay_path.display()))?;
    out.push(format!("overlay: {}", overlay_path.display()));

    if state.pending < holes.len() {
        let t = holes[state.pending];
        let bit_change =
            state.pending == 0 || (holes[state.pending - 1].d_mm - t.d_mm).abs() > 1e-9;
        if bit_change {
            out.push(format!("fit the {:.2} mm bit", t.d_mm));
        }
        out.push(format!(
            "drill hole #{}/{}: ({:.3}, {:.3}) drill-file mm, bit {:.2} mm — find it on the overlay, then rerun with a fresh frame",
            state.pending + 1,
            holes.len(),
            t.x_mm,
            t.y_mm,
            t.d_mm
        ));
    } else {
        out.push(format!(
            "all {} holes confirmed — archive: {}",
            holes.len(),
            overlay_path.display()
        ));
    }

    std::fs::write(state_path, state.render()).map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::P;

    fn hole(x: f64, y: f64, d: f64) -> DrillOp {
        let nm = |v: f64| (v * NM_PER_MM as f64).round() as i64;
        DrillOp::Hole {
            center: P::new(nm(x), nm(y)),
            diameter_nm: nm(d),
        }
    }

    #[test]
    fn orders_largest_bit_first_then_position() {
        let ops = [
            hole(5.0, 5.0, 0.4),
            hole(1.0, 1.0, 1.0),
            hole(3.0, 1.0, 1.0),
            hole(2.0, 2.0, 0.4),
        ];
        let ordered = order_holes(&ops);
        let ds: Vec<f64> = ordered.iter().map(|t| t.d_mm).collect();
        assert_eq!(ds, vec![1.0, 1.0, 0.4, 0.4], "largest bit first");
        // Within the 1.0 bit: (1,1) before (3,1) (y ties, x orders).
        assert!((ordered[0].x_mm, ordered[0].y_mm) == (1.0, 1.0));
        assert!((ordered[1].x_mm, ordered[1].y_mm) == (3.0, 1.0));
        // Within the 0.4 bit: (2,2) before (5,5) (y orders).
        assert!((ordered[2].x_mm, ordered[2].y_mm) == (2.0, 2.0));
    }

    #[test]
    fn slot_contributes_both_endpoints() {
        let nm = |v: f64| (v * NM_PER_MM as f64).round() as i64;
        let ops = [DrillOp::Slot {
            a: P::new(nm(1.0), nm(1.0)),
            b: P::new(nm(2.0), nm(1.0)),
            diameter_nm: nm(0.8),
        }];
        let ordered = order_holes(&ops);
        assert_eq!(ordered.len(), 2, "slot = drill both ends");
        assert!(ordered.iter().all(|t| (t.d_mm - 0.8).abs() < 1e-9));
    }

    #[test]
    fn state_round_trips_and_rejects_garbage() {
        let s = GuideState {
            fingerprint: 0xdeadbeefcafef00d,
            pending: 7,
        };
        assert_eq!(GuideState::parse(&s.render()).unwrap(), s);
        assert!(GuideState::parse("not a state file").is_err());
    }

    #[test]
    fn fingerprint_changes_with_the_hole_list() {
        let a = order_holes(&[hole(1.0, 1.0, 1.0)]);
        let b = order_holes(&[hole(1.0, 1.5, 1.0)]);
        assert_ne!(fingerprint(&a), fingerprint(&b));
        assert_eq!(fingerprint(&a), fingerprint(&a));
    }

    /// Synthetic frame with anti-aliased dark holes on a bright field.
    fn frame(w: u32, h: u32, dots: &[(f64, f64, f64)]) -> GrayImage {
        GrayImage::from_fn(w, h, |x, y| {
            let mut cover = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let px = x as f64 + (sx as f64 + 0.5) / 4.0 - 0.5;
                    let py = y as f64 + (sy as f64 + 0.5) / 4.0 - 0.5;
                    if dots.iter().any(|&(cx, cy, d)| {
                        ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() < d / 2.0
                    }) {
                        cover += 1.0_f64 / 16.0;
                    }
                }
            }
            image::Luma([(200.0 - 160.0 * cover).clamp(0.0, 255.0) as u8])
        })
    }

    const PPM: f64 = 10.0;

    #[test]
    fn check_hole_confirms_on_target_and_rejects_offsets() {
        // Hole drilled 0.05 mm off target: inside a 150 µm gate.
        let img = frame(200, 200, &[(100.5, 100.0, 10.0)]);
        let t = HoleTarget {
            x_mm: 10.0,
            y_mm: 10.0,
            d_mm: 1.0,
        };
        let off = check_hole(&img, &t, PPM, 150.0, 1.0).expect("confirmed");
        assert!((10.0..120.0).contains(&off), "off {off} µm");

        // Hole 0.4 mm off target: found but out of tolerance.
        let img2 = frame(200, 200, &[(104.0, 100.0, 10.0)]);
        let err = check_hole(&img2, &t, PPM, 150.0, 1.0).unwrap_err();
        assert!(err.contains("µm from the mapped position"), "{err}");

        // No hole at all.
        let img3 = frame(200, 200, &[]);
        assert!(check_hole(&img3, &t, PPM, 150.0, 1.0).is_err());
    }

    /// The confirmation number is a DISPLACEMENT in the camera plane, so it is
    /// unchanged by where that plane's origin happens to sit. Imaging the same
    /// drilled hole in a frame whose whole content is shifted, with the target
    /// shifted the same way, must read the same offset — the old
    /// `found_mm − design_mm` form could not, because it subtracted a
    /// drill-file coordinate from a camera-plane one and so measured the frame
    /// origin as well as the drill error.
    #[test]
    fn check_hole_offset_is_a_camera_plane_displacement() {
        // 0.05 mm (0.5 px) of real drill error at two different frame origins.
        let a = frame(200, 200, &[(100.5, 100.0, 10.0)]);
        let ta = HoleTarget {
            x_mm: 10.0,
            y_mm: 10.0,
            d_mm: 1.0,
        };
        // Same hole, same error, imaged 3 mm further along x and y; the target
        // travels with it (the operator re-seeds, or the board moved on the bed).
        let b = frame(200, 200, &[(130.5, 70.0, 10.0)]);
        let tb = HoleTarget {
            x_mm: 13.0,
            y_mm: 13.0,
            d_mm: 1.0,
        };
        let off_a = check_hole(&a, &ta, PPM, 150.0, 1.0).expect("confirmed");
        let off_b = check_hole(&b, &tb, PPM, 150.0, 1.0).expect("confirmed");
        assert!(
            (off_a - off_b).abs() < 5.0,
            "the same drill error reads the same in any frame: {off_a} vs {off_b} µm"
        );
    }

    #[test]
    fn overlay_marks_confirmed_current_and_remaining() {
        let holes = [
            HoleTarget {
                x_mm: 5.0,
                y_mm: 5.0,
                d_mm: 1.0,
            },
            HoleTarget {
                x_mm: 15.0,
                y_mm: 5.0,
                d_mm: 1.0,
            },
            HoleTarget {
                x_mm: 10.0,
                y_mm: 15.0,
                d_mm: 0.5,
            },
        ];
        let base = GrayImage::from_pixel(220, 220, image::Luma([180]));
        let img = render_overlay(&base, &holes, 1, PPM);
        let has = |color: Rgb<u8>, cx: i32, cy: i32| {
            (-14..14).any(|dy| {
                (-14..14).any(|dx| {
                    img.get_pixel_checked((cx + dx) as u32, (cy + dy) as u32) == Some(&color)
                })
            })
        };
        // Drill y is y-up: y_mm=5 → pixel row 220−50 = 170; y_mm=15 → 70.
        assert!(has(GREEN, 50, 170), "confirmed hole ringed green");
        assert!(has(RED, 150, 170), "current target red");
        assert!(has(DIM, 100, 70), "remaining hole dim");
    }
}
