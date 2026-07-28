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

## `brushed-plate-4holes.png` (2592×1944, grayscale)

The operator's real bench frame on **brushed, scratched, specular aluminium**,
and the acceptance fixture for the local search (`vision::find_fiducials`). It
is here because it broke the detector outright: their diagnostic log recorded
95 of 168 checks finding fewer than 3 of the 4 holes.

What makes it the hard case:

* The surface has long dark scratches and brush texture with **more contrast
  than the fiducials**. Sampled patches — a 22×22 px patch on a hole and a
  140×40 px patch of nearby "clean" plate — read `hole: min 15 max 173 mean
  115.1` against `background: min 6 max 176 mean 115.4`. They are statistically
  indistinguishable, so nothing keyed to raw pixel statistics can separate them.
* The holes are **ragged ablated marks, ~23 px across** (2 mm at 11.157 px/mm),
  not clean discs.
* One hole's search window straddles a broad dark brush band that inflates the
  robust noise estimate, which is why that corner is the weakest of the four.

Detection settings: `px_per_mm = 11.157`, `diameter_mm = 2.0`, `search_mm = 2.0`,
profile `DarkDot`, shape `Circle`.

Measured hole centres, in image pixels — located by taking the matched-filter
peak in a ±45 px window around the operator's estimate and confirming each
against the raw pixels, good to ~±3 px:

| corner | x | y |
| --- | --- | --- |
| top-left | 978 | 397 |
| top-right | 1428 | 424 |
| bottom-left | 968 | 832 |
| bottom-right | 1444 | 842 |

**These are the corners of a 40 × 40 mm square on the plate, but they are not a
square in the image.** Like `bench-plate-4holes.png` above, the plate is tilted
relative to the camera: sides measure 450.8 / 476.1 / 435.1 / 418.3 px, a 13.8%
spread about the 446.3 px nominal, and the diagonals are 644.3 / 614.9 px, 4.8%
apart. A least-squares similarity from the ideal square onto these corners
leaves an RMS residual of 16.9 px and a worst residual of 22.5 px — about the
whole 22.3 px search radius. Any test or matcher using this fixture has to
tolerate that; asserting equal sides would be asserting something false about
this bench.

Driven by the `brushed_plate_*` tests in `crates/vision/src/fiducial.rs`. Not
covered by `samples/MANIFEST.toml`, which tracks only `samples/kicad` and
`samples/lbrn2`.
