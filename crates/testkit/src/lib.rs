//! Golden-image test harness (dev-dependency only).
//!
//! Provides a reference software rasterizer for [`pcb_core::Layer`], an image
//! comparison assertion that dumps diff artifacts on failure, and a helper to
//! shell out to external rasterizers (gerbv, tracespace, ...) and load their
//! PNG output.
//!
//! # Conventions
//!
//! * **White (255) = copper/filled, black (0) = empty.** All images produced
//!   and compared by this crate use this convention; external images are
//!   binarized at [`BINARY_THRESHOLD`] when compared.
//! * This crate must only ever be a dev-dependency of production crates.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, ensure};
use image::{GrayImage, Luma, Rgb, RgbImage};
use pcb_core::{Layer, NM_PER_UM, Nm, P, Ring};

/// Pixel value for filled (copper) area.
pub const FILLED: u8 = 255;
/// Pixel value for empty area.
pub const EMPTY: u8 = 0;
/// Pixels `>= BINARY_THRESHOLD` count as filled when comparing images, so
/// anti-aliased output from external rasterizers compares cleanly.
pub const BINARY_THRESHOLD: u8 = 128;

/// Rasterize a layer to a binary grayscale image (white = filled).
///
/// # Geometry-to-image mapping
///
/// * The image frame is the axis-aligned bounding box of **all** vertices in
///   the layer (outers and holes). An empty layer yields a 0x0 image.
/// * One pixel spans `um_per_px` micrometers on each side. Width and height
///   are `ceil(bbox_extent / pixel_pitch)`, minimum 1, so the last column/row
///   may extend past the bounding box.
/// * Image origin is top-left and board +y points **up**: pixel `(i, j)`
///   covers the sample point at its center,
///   `x = min_x + (i + 1/2) * px`, `y = max_y - (j + 1/2) * px`
///   (nanometers, `px = um_per_px * 1000`). Row 0 is the top of the board.
/// * Fill rule is **even-odd scanline fill at pixel centers**, applied per
///   polygon across `outer` plus all `holes` (so holes punch out the outer),
///   then unioned across the layer's polygons. A pixel is white iff its
///   center sample point lies inside some polygon and not in that polygon's
///   holes.
///
/// # Panics
///
/// Panics if `um_per_px == 0` or the resulting image would exceed `u32`
/// dimensions.
pub fn rasterize(layer: &Layer, um_per_px: u32) -> GrayImage {
    assert!(um_per_px > 0, "um_per_px must be positive");
    let px: Nm = Nm::from(um_per_px) * NM_PER_UM;
    let Some((min, max)) = bounds(layer) else {
        return GrayImage::new(0, 0);
    };
    let w = span_px(max.x - min.x, px);
    let h = span_px(max.y - min.y, px);
    let half = px / 2; // px is a multiple of 1000, so this is exact
    let mut img = GrayImage::from_pixel(w, h, Luma([EMPTY]));
    let mut row = vec![false; w as usize];
    let mut crossings: Vec<Nm> = Vec::new();
    for j in 0..h {
        row.fill(false);
        let ys = max.y - Nm::from(j) * px - half;
        for poly in &layer.polys {
            crossings.clear();
            for ring in std::iter::once(&poly.outer).chain(poly.holes.iter()) {
                collect_crossings(ring, ys, &mut crossings);
            }
            crossings.sort_unstable();
            // Even-odd: paint between consecutive crossing pairs. Pixel
            // centers in [a, b) are inside.
            for pair in crossings.chunks_exact(2) {
                let (a, b) = (pair[0], pair[1]);
                // Smallest i with center >= a, and smallest i with
                // center >= b, clamped to the row.
                let i0 = ceil_div(a - min.x - half, px).clamp(0, i64::from(w));
                let i1 = ceil_div(b - min.x - half, px).clamp(0, i64::from(w));
                #[allow(clippy::cast_sign_loss)]
                for cell in &mut row[i0 as usize..i1 as usize] {
                    *cell = true;
                }
            }
        }
        for (i, filled) in row.iter().enumerate() {
            if *filled {
                #[allow(clippy::cast_possible_truncation)]
                img.put_pixel(i as u32, j, Luma([FILLED]));
            }
        }
    }
    img
}

/// Assert that two images agree on at least `min_fraction` of their pixels.
///
/// Pixels are binarized at [`BINARY_THRESHOLD`] before comparison. Images of
/// different dimensions are always a failure. On failure the inputs (and,
/// when sizes match, a diff image with disagreeing pixels in red) are written
/// to `target/test-artifacts/<test-thread-name>-{a,b,diff}.png` and the
/// function panics with the artifact location in the message.
#[track_caller]
pub fn assert_images_agree(a: &GrayImage, b: &GrayImage, min_fraction: f64) {
    if a.dimensions() != b.dimensions() {
        let dir = dump_artifacts(a, b, None);
        panic!(
            "image size mismatch: {:?} vs {:?}; inputs dumped to {}",
            a.dimensions(),
            b.dimensions(),
            dir.display()
        );
    }
    let (w, h) = a.dimensions();
    let total = u64::from(w) * u64::from(h);
    let mut diff = RgbImage::new(w, h);
    let mut agreeing: u64 = 0;
    for y in 0..h {
        for x in 0..w {
            let pa = a.get_pixel(x, y)[0];
            let pb = b.get_pixel(x, y)[0];
            if (pa >= BINARY_THRESHOLD) == (pb >= BINARY_THRESHOLD) {
                agreeing += 1;
                diff.put_pixel(x, y, Rgb([pa, pa, pa]));
            } else {
                diff.put_pixel(x, y, Rgb([255, 0, 0]));
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let fraction = if total == 0 {
        1.0
    } else {
        agreeing as f64 / total as f64
    };
    if fraction < min_fraction {
        let dir = dump_artifacts(a, b, Some(&diff));
        panic!(
            "images agree on {fraction:.6} of pixels, required {min_fraction}; \
             artifacts written to {}",
            dir.display()
        );
    }
}

/// Run an external rasterizer command and load `out_png` as 8-bit luma.
///
/// `cmd[0]` is the binary, the rest are its arguments (typically including
/// `out_png` somewhere). The command must exit successfully and must have
/// produced `out_png`. Callers that want to skip a test when the binary is
/// absent can gate on [`command_available`].
///
/// # Errors
///
/// Fails if `cmd` is empty, the binary cannot be spawned, it exits non-zero
/// (stderr is included in the error), or `out_png` cannot be decoded.
pub fn external_raster(cmd: &[&str], out_png: &Path) -> anyhow::Result<GrayImage> {
    let (bin, args) = cmd
        .split_first()
        .context("external_raster: empty command")?;
    let output = Command::new(bin)
        .args(args)
        .output()
        .with_context(|| format!("external_raster: failed to spawn `{bin}`"))?;
    ensure!(
        output.status.success(),
        "external_raster: `{}` exited with {}; stderr:\n{}",
        cmd.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let img = image::open(out_png)
        .with_context(|| format!("external_raster: cannot load {}", out_png.display()))?;
    Ok(img.to_luma8())
}

/// Whether `bin` can be spawned at all (used to skip external-tool tests).
pub fn command_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Directory where failure artifacts are written:
/// `$CARGO_TARGET_DIR/test-artifacts`, defaulting to the workspace
/// `target/test-artifacts`. Created on demand by the dump.
pub fn test_artifacts_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("target")
        });
    target.join("test-artifacts")
}

/// Best-effort dump of the compared images (and diff, if any) named after the
/// current test thread. Returns the artifact directory.
fn dump_artifacts(a: &GrayImage, b: &GrayImage, diff: Option<&RgbImage>) -> PathBuf {
    let dir = test_artifacts_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("testkit: cannot create {}: {e}", dir.display());
        return dir;
    }
    let stem = artifact_stem();
    let save = |name: &str, res: image::ImageResult<()>| {
        if let Err(e) = res {
            eprintln!("testkit: cannot write {name}: {e}");
        }
    };
    save("a", a.save(dir.join(format!("{stem}-a.png"))));
    save("b", b.save(dir.join(format!("{stem}-b.png"))));
    if let Some(d) = diff {
        save("diff", d.save(dir.join(format!("{stem}-diff.png"))));
    }
    dir
}

/// Artifact file stem: the current thread's name (cargo test names test
/// threads after the test path) with non-filename characters mapped to `_`.
fn artifact_stem() -> String {
    let thread = std::thread::current();
    let raw = thread.name().unwrap_or("unnamed");
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Bounding box over every vertex of every ring; `None` if there are none.
fn bounds(layer: &Layer) -> Option<(P, P)> {
    let mut it = layer
        .polys
        .iter()
        .flat_map(|p| std::iter::once(&p.outer).chain(p.holes.iter()))
        .flatten();
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

/// Image extent in pixels for a bbox extent of `len` nm at `px` nm/pixel.
fn span_px(len: Nm, px: Nm) -> u32 {
    u32::try_from(ceil_div(len, px).max(1)).expect("raster dimensions exceed u32")
}

/// `ceil(n / d)` for `d > 0`, exact for negative `n`.
fn ceil_div(n: i64, d: i64) -> i64 {
    debug_assert!(d > 0);
    n.div_euclid(d) + i64::from(n.rem_euclid(d) != 0)
}

/// Push the x coordinates where `ring`'s edges cross the horizontal line
/// `y = ys`. Uses the half-open rule `(y0 > ys) != (y1 > ys)` so scanlines
/// through vertices are counted consistently. Rings with fewer than 3
/// vertices are ignored.
fn collect_crossings(ring: &Ring, ys: Nm, out: &mut Vec<Nm>) {
    if ring.len() < 3 {
        return;
    }
    for k in 0..ring.len() {
        let p0 = ring[k];
        let p1 = ring[(k + 1) % ring.len()];
        if (p0.y > ys) != (p1.y > ys) {
            // Exact in i128; truncation loses < 1 nm, irrelevant at any
            // sensible pixel pitch.
            let num = i128::from(ys - p0.y) * i128::from(p1.x - p0.x);
            let den = i128::from(p1.y - p0.y);
            #[allow(clippy::cast_possible_truncation)]
            out.push(p0.x + (num / den) as Nm);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_core::{NM_PER_MM, Poly};
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn rect(x0: Nm, y0: Nm, x1: Nm, y1: Nm) -> Ring {
        vec![
            P::new(x0, y0),
            P::new(x1, y0),
            P::new(x1, y1),
            P::new(x0, y1),
        ]
    }

    /// 4 mm square with a centered 2 mm square hole. Filled fraction of the
    /// bounding box is analytically (16 - 4) / 16 = 0.75.
    fn square_with_hole() -> Layer {
        Layer {
            polys: vec![Poly {
                outer: rect(0, 0, 4 * NM_PER_MM, 4 * NM_PER_MM),
                holes: vec![rect(NM_PER_MM, NM_PER_MM, 3 * NM_PER_MM, 3 * NM_PER_MM)],
            }],
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn white_fraction(img: &GrayImage) -> f64 {
        let white = img.pixels().filter(|p| p[0] >= BINARY_THRESHOLD).count();
        white as f64 / img.pixels().count() as f64
    }

    /// Nearest-neighbour upscale by an integer factor (each source pixel
    /// becomes a k x k block), used to compare rasters across scales.
    fn upsample_nearest(img: &GrayImage, k: u32) -> GrayImage {
        let (w, h) = img.dimensions();
        GrayImage::from_fn(w * k, h * k, |x, y| *img.get_pixel(x / k, y / k))
    }

    /// Two-scale self-agreement, as promised by INF-4: the same square (with
    /// a hole) is rasterized at 25 and 50 um/px; both must match the analytic
    /// filled fraction exactly (the edges land on pixel boundaries at both
    /// scales), and the 50 um/px raster upsampled 2x nearest-neighbour must
    /// agree pixel-for-pixel with the 25 um/px raster.
    #[test]
    fn square_rasterized_at_two_scales_agrees_with_itself() {
        let layer = square_with_hole();
        let fine = rasterize(&layer, 25);
        let coarse = rasterize(&layer, 50);
        assert_eq!(fine.dimensions(), (160, 160));
        assert_eq!(coarse.dimensions(), (80, 80));
        assert!((white_fraction(&fine) - 0.75).abs() < 1e-12);
        assert!((white_fraction(&coarse) - 0.75).abs() < 1e-12);
        assert_images_agree(&fine, &upsample_nearest(&coarse, 2), 0.995);
    }

    #[test]
    fn shifted_copy_fails_and_writes_diff_artifact() {
        let layer = square_with_hole();
        // Shift only the hole by 200 um (8 px at 25 um/px) so the bounding
        // box -- and hence the image frame -- stays put. Agreement drops to
        // 1 - (2 * 8 * 80) / 160^2 = 0.95 < 0.995.
        let mut shifted = layer.clone();
        for p in &mut shifted.polys[0].holes[0] {
            p.x += 200 * NM_PER_UM;
        }
        let a = rasterize(&layer, 25);
        let b = rasterize(&shifted, 25);

        let diff_path = test_artifacts_dir().join(format!("{}-diff.png", artifact_stem()));
        let _ = std::fs::remove_file(&diff_path);

        let result = catch_unwind(AssertUnwindSafe(|| assert_images_agree(&a, &b, 0.995)));
        assert!(result.is_err(), "shifted copy unexpectedly agreed");
        assert!(
            diff_path.is_file(),
            "diff artifact not written to {}",
            diff_path.display()
        );
    }

    #[test]
    fn size_mismatch_is_a_failure() {
        let layer = square_with_hole();
        let a = rasterize(&layer, 25);
        let b = rasterize(&layer, 50);
        let result = catch_unwind(AssertUnwindSafe(|| assert_images_agree(&a, &b, 0.0)));
        assert!(result.is_err(), "differently sized images must not agree");
    }

    #[test]
    fn empty_layer_rasterizes_to_empty_image() {
        assert_eq!(rasterize(&Layer::default(), 25).dimensions(), (0, 0));
    }

    #[test]
    fn overlapping_polys_union() {
        // Two 2x2 mm squares overlapping by 1 mm in x cover the whole
        // 3x2 mm bbox: the fully white result proves polygons are unioned
        // (cross-poly even-odd would leave the 1 mm overlap black).
        let layer = Layer {
            polys: vec![
                Poly {
                    outer: rect(0, 0, 2 * NM_PER_MM, 2 * NM_PER_MM),
                    holes: vec![],
                },
                Poly {
                    outer: rect(NM_PER_MM, 0, 3 * NM_PER_MM, 2 * NM_PER_MM),
                    holes: vec![],
                },
            ],
        };
        let img = rasterize(&layer, 50);
        assert_eq!(img.dimensions(), (60, 40));
        assert!((white_fraction(&img) - 1.0).abs() < 1e-12);
    }

    /// Exercises the run-command-then-load-PNG path without a real external
    /// rasterizer: `cp` stands in for a tool that writes `out_png`. Skipped
    /// if `cp` is unavailable (real tools are exercised in ING-1).
    #[test]
    fn external_raster_runs_command_and_loads_png() {
        if !command_available("cp") {
            eprintln!("skipping: `cp` not available");
            return;
        }
        let dir = test_artifacts_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join(format!("{}-src.png", artifact_stem()));
        let dst = dir.join(format!("{}-out.png", artifact_stem()));
        let img = rasterize(&square_with_hole(), 50);
        img.save(&src).unwrap();
        let loaded =
            external_raster(&["cp", src.to_str().unwrap(), dst.to_str().unwrap()], &dst).unwrap();
        assert_images_agree(&img, &loaded, 1.0);
    }

    #[test]
    fn external_raster_reports_missing_binary() {
        let out = test_artifacts_dir().join("never-written.png");
        let err = external_raster(&["testkit-no-such-rasterizer-binary"], &out).unwrap_err();
        assert!(err.to_string().contains("failed to spawn"));
    }
}
