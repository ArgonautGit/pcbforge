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

## Burned grid (step ②)

Step ② (**Laser anchor**) uses a grid you *burn* with the laser and tape down as
the persistent anchor — there's no static sample for it because it's produced by
the machine. Emit one with `pcbforge calib-grid …`.
