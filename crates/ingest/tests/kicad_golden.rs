//! Golden validation of the Gerber parser against real KiCad output.
//!
//! Uses the committed sample board `samples/kicad/valdemo.kicad_pcb`:
//! `kicad-cli` exports both the F.Cu Gerber (what we parse) and its own SVG
//! render of the same layer (independent ground truth, rasterized via
//! `rsvg-convert`). The parsed geometry rasterized by testkit must agree
//! with KiCad's render on ≥ 99.5 % of pixels after content alignment.
//!
//! Skips (with a message) when `kicad-cli` or `rsvg-convert` is not
//! installed, so plain `cargo test` stays green on machines without KiCad;
//! CI or a dev box with KiCad runs the real check.

use std::path::{Path, PathBuf};
use std::process::Command;

use image::GrayImage;
use testkit::{BINARY_THRESHOLD, assert_images_agree, command_available, rasterize};

const UM_PER_PX: u32 = 25;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/ingest is two levels below the root")
        .to_path_buf()
}

fn tmp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pcbforge-kicad-golden-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("spawn");
    assert!(
        out.status.success(),
        "{cmd:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Tight bounding box of pixels ≥ threshold, as (x0, y0, x1, y1) inclusive.
fn content_bbox(img: &GrayImage) -> Option<(u32, u32, u32, u32)> {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for (x, y, p) in img.enumerate_pixels() {
        if p.0[0] >= BINARY_THRESHOLD {
            b = Some(match b {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    b
}

/// Crop to the content bbox, so the two renders' differing page frames and
/// margins drop out and only the copper geometry is compared.
fn crop_to_content(img: &GrayImage) -> GrayImage {
    let (x0, y0, x1, y1) = content_bbox(img).expect("image has content");
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut out = GrayImage::new(w, h);
    for (x, y, p) in out.enumerate_pixels_mut() {
        *p = *img.get_pixel(x0 + x, y0 + y);
    }
    out
}

/// Pad both images to a common size (content stays top-left aligned; the
/// content bboxes already match within a pixel of rounding).
fn pad_to_common(a: GrayImage, b: GrayImage) -> (GrayImage, GrayImage) {
    let (w, h) = (a.width().max(b.width()), a.height().max(b.height()));
    let pad = |img: GrayImage| {
        let mut out = GrayImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels() {
            out.put_pixel(x, y, *p);
        }
        out
    };
    (pad(a), pad(b))
}

#[test]
fn parsed_gerber_matches_kicads_own_render() {
    if !command_available("kicad-cli") || !command_available("rsvg-convert") {
        eprintln!("SKIP: kicad-cli / rsvg-convert not installed; golden not run");
        return;
    }
    let board = repo_root().join("samples/kicad/valdemo.kicad_pcb");
    assert!(board.is_file(), "sample board missing: {}", board.display());
    let dir = tmp_dir();

    // 1. Real Gerber from KiCad, parsed by us, rasterized by testkit.
    run(Command::new("kicad-cli")
        .args(["pcb", "export", "gerbers", "--layers", "F.Cu"])
        .arg(&board)
        .arg("-o")
        .arg(&dir));
    let layer = ingest::gerber::load_gerber(&dir.join("valdemo-F_Cu.gtl")).expect("parse");
    assert!(!layer.polys.is_empty());
    let ours = crop_to_content(&rasterize(&layer, UM_PER_PX));

    // 2. KiCad's own SVG render of the same layer, rasterized independently.
    let svg = dir.join("kicad-fcu.svg");
    run(Command::new("kicad-cli")
        .args([
            "pcb",
            "export",
            "svg",
            "--layers",
            "F.Cu",
            "--black-and-white",
            "--exclude-drawing-sheet",
            "--page-size-mode",
            "2",
        ])
        .arg(&board)
        .arg("-o")
        .arg(&svg));
    let png = dir.join("kicad-fcu.png");
    // dpi so that one pixel = UM_PER_PX µm: 25400 / UM_PER_PX.
    let dpi = (25_400 / UM_PER_PX).to_string();
    run(Command::new("rsvg-convert")
        .args(["-d", &dpi, "-p", &dpi, "-b", "white", "-o"])
        .arg(&png)
        .arg(&svg));
    let mut kicad_img = image::open(&png).expect("png").to_luma8();
    // KiCad draws copper black on white; testkit's convention is copper
    // white on black — invert.
    for p in kicad_img.pixels_mut() {
        p.0[0] = 255 - p.0[0];
    }
    let theirs = crop_to_content(&kicad_img);

    // 3. Compare (sizes may differ by a rounding pixel; pad to match).
    let dw = ours.width().abs_diff(theirs.width());
    let dh = ours.height().abs_diff(theirs.height());
    assert!(
        dw <= 2 && dh <= 2,
        "content sizes diverge: ours {}x{}, kicad {}x{}",
        ours.width(),
        ours.height(),
        theirs.width(),
        theirs.height()
    );
    let (a, b) = pad_to_common(ours, theirs);
    assert_images_agree(&a, &b, 0.995);
}
