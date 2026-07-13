//! Golden validation of the KiCad-SVG parser against an independent
//! rasterizer.
//!
//! For both committed sample boards, `kicad-cli` (through
//! `ingest::kicad_cli`) exports the F.Cu layer as SVG; `load_kicad_svg`
//! parses it and testkit rasterizes the result at 25 µm/px, which must
//! agree with `rsvg-convert`'s render of the *same* SVG on ≥ 99.5 % of
//! pixels after content alignment.
//!
//! Skips (with a message) when `kicad-cli` or `rsvg-convert` is not
//! installed, so plain `cargo test` stays green on machines without them.

use std::path::{Path, PathBuf};
use std::process::Command;

use image::GrayImage;
use ingest::kicad_cli::KicadCli;
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
    let dir = std::env::temp_dir().join(format!("pcbforge-svg-golden-{}", std::process::id()));
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
fn parsed_svg_matches_rsvg_render_on_both_sample_boards() {
    if !ingest::kicad_cli::available() || !command_available("rsvg-convert") {
        eprintln!("SKIP: kicad-cli / rsvg-convert not installed; golden not run");
        return;
    }
    let cli = KicadCli::discover().unwrap();
    let dir = tmp_dir();

    for stem in ["valdemo", "valdemo2"] {
        let board = repo_root().join(format!("samples/kicad/{stem}.kicad_pcb"));
        assert!(board.is_file(), "sample board missing: {}", board.display());

        // 1. F.Cu SVG from KiCad, parsed by us, rasterized by testkit.
        let svg = dir.join(format!("{stem}-fcu.svg"));
        cli.export_svg(&board, "F.Cu", &svg).expect("svg export");
        let layer = ingest::svg::load_kicad_svg(&svg).expect("parse");
        assert!(!layer.polys.is_empty(), "{stem}: no copper parsed");
        let ours = crop_to_content(&rasterize(&layer, UM_PER_PX));

        // 2. Independent render of the same SVG via rsvg-convert.
        let png = dir.join(format!("{stem}-fcu.png"));
        // dpi so that one pixel = UM_PER_PX µm: 25400 / UM_PER_PX.
        let dpi = (25_400 / UM_PER_PX).to_string();
        run(Command::new("rsvg-convert")
            .args(["-d", &dpi, "-p", &dpi, "-b", "white", "-o"])
            .arg(&png)
            .arg(&svg));
        let mut rsvg_img = image::open(&png).expect("png").to_luma8();
        // KiCad draws copper black on white; testkit's convention is copper
        // white on black — invert.
        for p in rsvg_img.pixels_mut() {
            p.0[0] = 255 - p.0[0];
        }
        let theirs = crop_to_content(&rsvg_img);

        // 3. Compare (sizes may differ by rounding pixels; pad to match).
        let dw = ours.width().abs_diff(theirs.width());
        let dh = ours.height().abs_diff(theirs.height());
        assert!(
            dw <= 2 && dh <= 2,
            "{stem}: content sizes diverge: ours {}x{}, rsvg {}x{}",
            ours.width(),
            ours.height(),
            theirs.width(),
            theirs.height()
        );
        let (a, b) = pad_to_common(ours, theirs);
        assert_images_agree(&a, &b, 0.995);
    }
}
