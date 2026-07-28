//! End-to-end test of `pcbforge emit --mirror-x` (double-sided groundwork,
//! ORC-6): the back side of a board is the front design mirrored in X. A left
//! feature must land on the right after mirroring, while the overall bbox and
//! shape count are unchanged.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("pcbforge-mirror-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn identity_field_map(dir: &Path) -> PathBuf {
    use nalgebra::Point2;
    let pairs: Vec<_> = (0..4)
        .flat_map(|row| {
            (0..4).map(move |column| {
                let point = Point2::new(column as f64 * 50.0, row as f64 * 50.0);
                (point, point)
            })
        })
        .collect();
    let path = dir.join("identity-field.txt");
    std::fs::write(&path, vision::fit_field(&pairs).unwrap().serialize()).unwrap();
    path
}

/// Parse the first `V<x> <y>` X-coordinates out of an emitted `.lbrn2`.
fn vert_xs(doc: &str) -> Vec<f64> {
    let mut xs = Vec::new();
    for tok in doc.split("V").skip(1) {
        // token looks like "12.34 5.67c0x1c1x1..."; take the leading number.
        let num: String = tok
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect();
        if let Ok(x) = num.parse::<f64>() {
            xs.push(x);
        }
    }
    xs
}

/// The real pair of layers: F.Cu emitted plain, B.Cu emitted with `--mirror-x`.
/// Both land on the workspace in one board-sized frame, and the mirror actually
/// happens — B.Cu's copper is bunched into the board's left half (nothing right
/// of x 11.7), so on the back it has to come out in the frame's right quarter.
#[test]
fn mirror_x_reflects_the_design_for_the_back() {
    let dir = tmp();
    let field = identity_field_map(&dir);
    let front = dir.join("front.lbrn2");
    let back = dir.join("back.lbrn2");
    let args = |copper: &str, out: &Path, mirror: bool| {
        let mut a = vec![
            "emit".to_string(),
            "--copper".into(),
            fixture(copper).to_str().unwrap().into(),
            "--outline".into(),
            fixture("demo-Edge_Cuts.gbr").to_str().unwrap().into(),
            "--lbrn2".into(),
            out.to_str().unwrap().into(),
            "--field-map".into(),
            field.to_str().unwrap().into(),
        ];
        if mirror {
            a.push("--mirror-x".into());
        }
        a
    };

    for (copper, out, m) in [
        ("demo-F_Cu.gbr", &front, false),
        ("demo-B_Cu.gbr", &back, true),
    ] {
        let r = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
            .args(args(copper, out, m))
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "{copper}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
    }

    let fdoc = std::fs::read_to_string(&front).unwrap();
    let bdoc = std::fs::read_to_string(&back).unwrap();

    // Both are valid, corner-normalized (no negative coords) projects. The two
    // layers carry different copper, so an equal board width is the outline
    // doing its job rather than an artefact of emitting one file twice.
    assert!(!fdoc.contains("V-") && !bdoc.contains("V-"));
    let (fw, _, _) = frame_and_copper_x(&fdoc);
    let (bw, _, bmax) = frame_and_copper_x(&bdoc);
    assert!(
        (fw - bw).abs() < 1e-3,
        "same board width front {fw} vs back {bw}"
    );

    // Mirroring is a real change: B.Cu stops at board x 11.7, so unmirrored its
    // copper could never reach the frame's right quarter — it does here.
    assert!(
        bmax > 0.75 * bw,
        "the back's copper must land on the far side after mirroring: \
         max {bmax} in a {bw} frame"
    );
}

/// The emitted frame width plus the x-extent of the copper boundary inside it.
///
/// The job is the board region minus the copper, so the outer ring is the board
/// and the copper edges are the rings within it. The frame is corner-normalized,
/// so the frame width is simply the largest emitted x. Every board-boundary
/// vertex lies within the outline stroke's 0.025 mm corner radius of x = 0 or
/// x = width, so dropping a 0.1 mm band at each edge leaves the copper boundary
/// alone (the demo copper is >1 mm clear of the board edge on both layers).
fn frame_and_copper_x(doc: &str) -> (f64, f64, f64) {
    let xs = vert_xs(doc);
    let width = xs.iter().cloned().fold(0.0_f64, f64::max);
    let interior: Vec<f64> = xs
        .iter()
        .cloned()
        .filter(|&x| x > 0.1 && x < width - 0.1)
        .collect();
    assert!(
        !interior.is_empty(),
        "no copper boundary in the emitted job"
    );
    (
        width,
        interior.iter().cloned().fold(f64::MAX, f64::min),
        interior.iter().cloned().fold(f64::MIN, f64::max),
    )
}

/// The real front/back alignment check: a feature that exists at the same board
/// coordinate on both layers must land mirror-consistently in the two emitted
/// frames. Both fixtures carry a 1 x 1 mm pad at board (2, 2) — a through-hole
/// — and it is the leftmost copper on each layer.
///
/// The arithmetic the emit path produces (all mm, `--offset-mm 0`):
///
/// * demo-Edge_Cuts is a 0..20 rectangle stroked with a 0.05 mm round aperture,
///   so the board region runs -0.025..20.025 and the frame width is **20.05**.
/// * Front: `normalize_frame` shifts the board min (-0.025) to 0, so the shared
///   pad's left edge (board 1.5) lands at **1.525** — the front's minimum copper x.
/// * Back: `mirror_job` reflects about x = 0 (board 1.5 → -1.5, board max 20.025
///   → -20.025), then `normalize_frame` shifts by +20.025. The pad's left edge
///   becomes the back's **rightmost** copper x, at 20.025 - 1.5 = **18.525**.
/// * 18.525 = 20.05 - 1.525, i.e. `back_max = width - front_min`.
///
/// The two relative identities — `back_max == width - front_min` and
/// `back_width == front_width` — fall out of the board region alone: for a board
/// spanning `[b0, b1]`, the front puts the pad edge at `1.5 - b0` in a frame of
/// width `b1 - b0`, and the back puts it at `b1 - 1.5`, whatever `b0`/`b1` are.
/// So they hold exactly, independently of how the outline's stroke is handled
/// and of how the copper differs between the layers (the copper is >1 mm inside
/// the board edge on both, so the job's bbox is the board's).
///
/// The absolute numbers below are the reconstruction of that stroke handling and
/// carry a loose tolerance: they document what the frame *should* measure without
/// making a half-aperture misprediction here fail a correct emit.
///
/// This is what the old behaviour got wrong: cornering each side on its own
/// copper bbox put the front's pad edge at 0 in a 17.3 mm-wide frame and the
/// back's at 10.2 in a 10.2 mm-wide one — both the equal-width and the
/// mirror-identity assertions below fail on those numbers.
///
/// No `--field-map` here on purpose: the geometry stays exact integer nm
/// through the whole path, so the identity holds to floating-point noise
/// rather than to the warp fit's residual.
#[test]
fn front_and_back_land_in_the_same_frame_through_the_outline() {
    let dir = tmp();
    let front = dir.join("align-front.lbrn2");
    let back = dir.join("align-back.lbrn2");
    let outline = fixture("demo-Edge_Cuts.gbr");

    for (copper, out, mirror) in [
        ("demo-F_Cu.gbr", &front, false),
        ("demo-B_Cu.gbr", &back, true),
    ] {
        let mut args = vec![
            "emit".to_string(),
            "--copper".into(),
            fixture(copper).to_str().unwrap().into(),
            "--outline".into(),
            outline.to_str().unwrap().into(),
            "--offset-mm".into(),
            "0".into(),
            "--lbrn2".into(),
            out.to_str().unwrap().into(),
        ];
        if mirror {
            args.push("--mirror-x".into());
        }
        let r = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
            .args(&args)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "{copper}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
    }

    let (fw, fmin, fmax) = frame_and_copper_x(&std::fs::read_to_string(&front).unwrap());
    let (bw, bmin, bmax) = frame_and_copper_x(&std::fs::read_to_string(&back).unwrap());

    // One frame, taken from the outline rather than from either side's copper:
    // exact, because both frames are the same board region.
    let tol = 1e-6;
    assert!(
        (fw - bw).abs() < tol,
        "both sides must be framed on the board: front {fw} vs back {bw}"
    );
    // Nominal geometry, to a band that absorbs the outline stroke's half-width
    // either way — a frame anchored on copper instead is millimetres out.
    let nominal = 0.06;
    assert!(
        (fw - 20.05).abs() < nominal,
        "frame width {fw}, want the 20 mm board plus the outline stroke"
    );
    assert!(
        (fmin - 1.525).abs() < nominal,
        "front pad edge {fmin}, want the board's 1.5 mm plus the stroke"
    );

    // The shared through-pad: front's leftmost copper edge, back's rightmost.
    assert!(
        (bmax - (fw - fmin)).abs() < tol,
        "shared pad must mirror: back {bmax}, want {} (= {fw} - {fmin})",
        fw - fmin
    );

    // The layers are deliberately asymmetric (back copper stops at board x 11.7,
    // the front reaches 18.8), so this is not a test that would also pass on two
    // identical inputs — the *other* edge does not mirror onto itself.
    assert!(
        (bmin - (fw - fmax)).abs() > 1.0,
        "the fixtures must differ in extent, or the mirror check is vacuous \
         (front {fmin}..{fmax}, back {bmin}..{bmax})"
    );
}

/// Without an outline each side is framed on its own copper extents, so the
/// front and back land in unrelated frames. Refuse rather than emit a back job
/// that silently won't align.
#[test]
fn mirror_x_without_an_outline_is_refused() {
    let dir = tmp();
    let out = dir.join("no-outline-back.lbrn2");
    let r = Command::new(env!("CARGO_BIN_EXE_pcbforge"))
        .args([
            "emit",
            "--copper",
            fixture("demo-F_Cu.gbr").to_str().unwrap(),
            "--lbrn2",
            out.to_str().unwrap(),
            "--mirror-x",
        ])
        .output()
        .unwrap();
    assert!(
        !r.status.success(),
        "--mirror-x without --outline must fail"
    );
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(
        stderr.contains("--mirror-x requires --outline"),
        "stderr: {stderr}"
    );
    assert!(
        !out.exists(),
        "no job may be written when the emit is refused"
    );
}
