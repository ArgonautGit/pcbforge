# Calibration samples

Reference artifacts for the console's two-step camera→laser calibration.

## `grid-7x7-10mm.svg` — printed camera-lens grid

A printable reference for **step ① Camera lens (printed grid)**. It's a 7×7
lattice of solid black dots at a nominal **10 mm pitch** (2 mm dots), matching
the `DarkDot` fiducial profile the detector expects. Laid out in the machine
frame: the **lower-left dot is the origin**, +X runs right, +Y runs up, and the
four corners are numbered **1→2→3→4** in the exact order the console asks you to
click them (LL, LR, UR, UL).

### Use it

1. Print at **100% / actual size** — disable "fit to page" / "scale to fit", or
   the pitch prints wrong.
2. Measure the labeled span across the bottom row with calipers. Consumer
   printers scale by a percent or two, so enter the **true measured pitch** in
   the console, not the nominal 10 mm — that number is what makes the lens fit
   metric.
3. Tape it flat to the bed, image it with the camera, click corners 1→2→3→4,
   then **Fit**. The console detects every dot, fits the pixel→true-mm
   polynomial, and shows the distortion field + residuals.

The SVG is sized in real millimetres (A4), so it prints dimensionally exact.

### Regenerate / change pitch or size

Deterministic generator (no dependencies):

```
python3 tools/gen_calib_grid.py --n 7 --pitch 10.0 --dot 2.0 --page A4 \
    --out samples/calibration/grid-7x7-10mm.svg
```

`--n` dots per side, `--pitch` mm, `--dot` mm, `--page A4|Letter`.

## `grid-7x7-10mm-distorted.png` — distorted camera view (calibration test)

A synthetic camera frame for **testing that the camera-lens calibration
actually recovers true geometry**. It's the same 7×7 / 10 mm grid pushed through
a known **perspective** (a slightly tilted camera) plus a **5% radial barrel
distortion** about the image centre — the curvature a homography can't model —
rendered as dark dots on a light, vignetted, mildly noisy field so the
`DarkDot` detector behaves as it would on real optics.

`grid-7x7-10mm-distorted.json` is the ground-truth sidecar: image size, the
distortion parameters, the four corner-dot pixels in `GridSpec::corners_mm`
order (lower-left, lower-right, upper-right, upper-left), and every
`mm → distorted-px` pair.

Feeding the PNG + those four corners into `ui::calib::fit_camera_lens` recovers
all 49 dots at **~25 µm RMS**, while ~10 px of barrel distortion is present that
a perspective-only fit would leave uncorrected. This is exercised automatically
by the `calibrates_from_the_distorted_grid_fixture` test, so a regression that
breaks the lens fit fails CI.

Regenerate (also prints the recovered accuracy as a self-check):

```
cargo run -p ui --example gen_distorted_grid
```

Adjust the grid/distortion constants at the top of
`crates/ui/examples/gen_distorted_grid.rs` (e.g. a stronger `BARREL_K`) to make
a harder test. A pure-Python variant lives in `tools/gen_distorted_grid.py` for
environments with a working Pillow.

## Burned grid (step ②)

Step ② (**Laser anchor**) uses a grid you *burn* with the laser and tape down as
the persistent anchor — there's no static sample for it because it's produced by
the machine. Emit one with `pcbforge calib-grid …`.

After a fit, the console draws the anchor as an overlay on the grid frame: a
**blue mesh** = the machine coordinate grid the camera reconstructs, a **green**
origin + `+X`/`+Y` axes, a per-dot ring colored by residual (green tight,
amber/red loose), an **orange residual vector** (× exaggerated) pointing
commanded→detected, and a red ✕ for any dot that failed to lock. A readout shows
`found/total`, RMS, and worst-dot µm. Systematic outward (radial) vectors mean
the anchor is fighting lens/galvo curvature a flat homography can't model — run
the camera-lens step first, or characterize the galvo.

Preview it on the distorted fixture (rings come up red there because that image
carries barrel a homography-only anchor can't correct — the feedback flagging it
is the point):

```
cargo run -p ui --example dump_anchor_overlay -- anchor.png
```
