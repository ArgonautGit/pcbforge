# Plan — console fiducial-check preview (UI, VIS-4 surfaced)

**Goal:** let the operator *see* where the fiducial detector locked on, over the
actual frame, and confirm it found the real drilled holes (not honeycomb-bed
decoys) before any registration is trusted. This surfaces `vision::find_fiducials`
(VIS-4) in the console.

## What the operator sees

A new **"Fiducial check"** tab in the console's central panel (beside "Job
preview"):

- The **frame image** with overlays drawn on top:
  - **expected** positions — cyan crosshair at each nominal fiducial;
  - **detected** center — a ring + center dot, **green** if confidence is
    strong, **amber** if weak (score below threshold);
  - **miss** — a red ✕ at the expected spot for any fiducial not found.
- A **summary list**, one row per fiducial:
  - found → `#i  (ex,ey)→(fx,fy) mm   offset NN µm   score 0.NN` (row tinted by
    confidence);
  - miss → `#i (ex,ey) mm  MISS: <reason>` where reason is the detector's own
    `LowContrast snr=…` / `NoCandidate snr=…` / `OutsideFrame` — so a weak
    result reads as a *lighting/placement* problem, per VIS-4's contract.

The operator glances at it: three green rings on the three holes, small offsets,
good scores ⇒ trust registration. A red ✕ or an amber ring on a decoy ⇒ stop.

## Inputs (console controls)

| control | default | note |
|---|---|---|
| frame image path | — | a saved camera grab or a phone photo of the bed; becomes the **live feed** once VIS-1 lands |
| expected layout | **(10,10), (60,10), (10,60) mm** | the operator's L-layout from the field photo; editable |
| hole diameter | 1.0 mm | drilled-hole size → `FiducialProfile::DarkDot` |
| px per mm | (entered) | bed scale via `BedMap::uniform_scale` **until VIS-3** provides the real homography |
| search radius | 2.0 mm | per-fiducial window |

## Pipeline (all existing APIs)

```
image::open(path).to_luma8()                     // load frame
  → BedMap::uniform_scale(px_per_mm)             // VIS-4 (real bed map = VIS-3 later)
  → vision::find_fiducials(frame, expected, search_mm, DarkDot, bed)
  → render_overlay(frame, results) -> egui::ColorImage
```

`render_overlay` is new (in `crates/ui`): copy the gray frame to RGB, then draw
crosshairs / rings / ✕ into the pixel buffer (small Bresenham helpers). Output is
a `ColorImage` shown via the same texture path the job preview already uses.

## Why rasterize the overlay (not egui-painter vectors)

Same reason the job preview is rasterized: a `ColorImage` is **verifiable
headless** (I can dump it to PNG, assert pixel colors near detected centers, and
show it to you), and it reuses the console's texture display. Crisp zoomable
vector overlays are a later refinement if wanted.

## Verification (no camera needed)

- **Unit tests** on a synthetic frame built like the VIS-4 tests (rendered holes
  + glare gradient + a honeycomb-style decoy): assert the summary reports all
  three found with small offsets and good scores, and that overlay pixels near
  each detected center carry the "found" color; a separate low-contrast frame
  yields a `MISS` row with the SNR reason.
- **Headless frame test**: the Fiducial-check tab lays out under `Context::run`
  with no panic.
- **Visual proof**: an example dumps the overlay PNG on a synthetic frame; I'll
  send it so you can see the markers before it ever runs live.

## Honest limits (tracked, not hidden)

1. **No live camera yet** — the panel reads a *file*. It becomes live when VIS-1
   lands (the frame source swaps; overlay code is unchanged). Follow-up FLD-10
   already covers the live-feed swap.
2. **px/mm is a uniform scale until VIS-3** — good enough to confirm detection on
   a roughly flat, square-on frame; true bed homography + lens undistort come
   with VIS-2/3. The panel will accept a real `BedMap` unchanged when it exists.
3. Overlay is rasterized (not vector) — matches the job preview; refine later.

## Follow-ups (new backlog items)

- **FLD-11** — swap the frame source to the live VIS-1 feed; run detection each
  frame so the operator sees it track as they nudge the board.
- **FLD-12** — profile selector (DARK_DOT / ANNULUS / BACKLIT) + click-to-place
  expected fiducials, for burned-annulus and backlit-hole workflows.

## Scope of the change

`crates/ui` only: a new `fiducial.rs` (overlay + check driver), a `Fiducials`
tab enum + config fields + controls in `app.rs`, deps `vision` + `nalgebra` +
`image` (runtime). No change to `vision` itself — VIS-4's API is used as-is.
