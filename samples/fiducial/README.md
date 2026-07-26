# Fiducial-detection fixtures

## `bench-plate-4holes.png` (2592×1944, grayscale)

A camera grab from the bench, used as the acceptance fixture for the
whole-frame fiducial recovery (`vision::find_fiducial_candidates` +
`ui::fiducial::match_layout_to_candidates`). It is the real thing, not a
render, and it is here because thresholds tuned on synthetic frames do not
survive contact with real pixels.

What makes it the useful case:

* A light laser-parameter test plate on a dark honeycomb bed, with four dark
  drilled fiducial holes (~30 px across, ~6% of the fiducial rectangle's span).
* The left third of the frame is bare bed: dozens of dark round holes at the
  same apparent size. These are the decoys the arrangement match has to reject,
  and a permissive candidate pass returns them by the dozen.
* The plate is tilted relative to the camera — its diagonals differ by ~9%, so
  the observed quad is genuinely perspective-warped and no similarity fits it.
  This is what forces the span-relative match tolerance.

Measured hole centres (by eye, ±5 px), in image pixels:

| corner | x | y |
| --- | --- | --- |
| top-left | 1360 | 213 |
| top-right | 1830 | 243 |
| bottom-left | 1275 | 650 |
| bottom-right | 1760 | 703 |

The operator's layout for this plate is `81.7,20.2; 128.4,27.9; 125.3,71.6;
76.3,64.8` (bed mm, y-up) at roughly 9.97 px/mm — a hand-clicked quad, so its
corners are already a few mm out of square before the camera adds anything.

Driven by `bench_frame_recovers_the_four_plate_holes` in
`crates/ui/src/fiducial.rs`. Not covered by `samples/MANIFEST.toml`, which
tracks only `samples/kicad` and `samples/lbrn2`.
