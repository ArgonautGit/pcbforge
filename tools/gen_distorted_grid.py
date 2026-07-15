#!/usr/bin/env python3
"""Render a *distorted* camera view of the calibration grid, for testing that
the camera-lens calibration recovers true geometry.

This simulates what the camera actually sees when looking at the printed
reference grid: the ideal `n x n` lattice pushed through a mild **perspective**
(a slightly tilted camera) and a **radial barrel distortion** about the image
centre — exactly the curvature a homography cannot model and the bi-cubic lens
polynomial can. Dots are rendered dark on a light, faintly vignetted, mildly
noisy background so the `DarkDot` detector behaves as it would on real optics.

Feeding the output PNG + the four corner pixels (from the JSON sidecar) into
`ui::calib::fit_camera_lens` should recover the ~`pitch` mm geometry with a
small RMS, while a plain homography over the same dots leaves a large residual.
If it doesn't, the calibration is broken.

Deterministic: fixed noise seed -> reproducible output. Needs Pillow.

Usage:
    python3 tools/gen_distorted_grid.py \
        --n 7 --pitch 10.0 --dot 2.0 --barrel 0.05 \
        --png  samples/calibration/grid-7x7-10mm-distorted.png \
        --json samples/calibration/grid-7x7-10mm-distorted.json
"""

from __future__ import annotations

import argparse
import json
import random

from PIL import Image, ImageDraw, ImageFilter

# Image geometry: ~9 px/mm with a 60 px margin, centred barrel.
PX_PER_MM = 9.0
MARGIN = 60.0
# Mild perspective (a tilted camera): a projective denominator + a little shear.
SHEAR_X = 0.03
SHEAR_Y = -0.02
PROJ_X = 3.0e-4
PROJ_Y = 4.0e-4
SS = 4  # supersample factor for anti-aliased dots


def distort(mm, span, size, barrel):
    """True grid-mm -> distorted camera pixel (perspective + barrel).

    y is flipped so mm-(0,0) lands at the lower-left of the image, matching the
    printed grid and the console's corner order.
    """
    x, y = mm
    cx = cy = size / 2.0
    # Ideal (perspective) placement, origin lower-left.
    denom = 1.0 + PROJ_X * (x - span / 2.0) + PROJ_Y * (y - span / 2.0)
    u = (MARGIN + PX_PER_MM * x + SHEAR_X * y) / denom
    v = (size - MARGIN - PX_PER_MM * y + SHEAR_Y * x) / denom
    # Radial barrel about the image centre.
    du, dv = u - cx, v - cy
    r2 = (du * du + dv * dv) / (cx * cx)
    f = 1.0 + barrel * r2
    return (cx + du * f, cy + dv * f)


def build(n, pitch, dot, barrel):
    span = (n - 1) * pitch
    size = int(round(MARGIN * 2 + span * PX_PER_MM))

    truth = []  # (mm, distorted_px)
    for row in range(n):
        for col in range(n):
            mm = (col * pitch, row * pitch)
            truth.append((mm, distort(mm, span, size, barrel)))

    # Render dots on a supersampled canvas, then downscale for smooth edges.
    big = size * SS
    img = Image.new("L", (big, big), color=238)  # light background
    draw = ImageDraw.Draw(img)
    r_px = (dot / 2.0) * PX_PER_MM * SS
    for _mm, (u, v) in truth:
        U, V = u * SS, v * SS
        draw.ellipse([U - r_px, V - r_px, U + r_px, V + r_px], fill=26)  # dark dot
    img = img.resize((size, size), Image.LANCZOS)

    # Subtle radial vignette + mild sensor noise, so detection isn't tested on a
    # sterile image. Applied after downscale so dots stay crisp.
    px = img.load()
    rng = random.Random(0xC0FFEE)
    cx = cy = size / 2.0
    maxr2 = 2.0 * cx * cx
    for yy in range(size):
        for xx in range(size):
            r2 = ((xx - cx) ** 2 + (yy - cy) ** 2) / maxr2
            val = px[xx, yy] * (1.0 - 0.12 * r2)  # up to 12% darker at corners
            val += rng.uniform(-3.0, 3.0)
            px[xx, yy] = int(max(0, min(255, val)))
    img = img.filter(ImageFilter.GaussianBlur(0.4))

    # Corner dots in GridSpec.corners_mm order: LL, LR, UR, UL.
    def find(mmx, mmy):
        for mm, p in truth:
            if abs(mm[0] - mmx) < 1e-6 and abs(mm[1] - mmy) < 1e-6:
                return [round(p[0], 3), round(p[1], 3)]
        raise KeyError((mmx, mmy))

    corners = [find(0, 0), find(span, 0), find(span, span), find(0, span)]
    meta = {
        "description": "Distorted camera view of the calibration grid "
        "(perspective + barrel) for testing camera-lens calibration.",
        "image": {"width": size, "height": size, "px_per_mm_nominal": PX_PER_MM},
        "grid": {"n": n, "pitch_mm": pitch, "dot_mm": dot, "origin_mm": [0, 0]},
        "distortion": {
            "barrel_k": barrel,
            "shear": [SHEAR_X, SHEAR_Y],
            "projective": [PROJ_X, PROJ_Y],
            "note": "y is image-up (mm origin at lower-left).",
        },
        # The four corner-dot pixels to feed fit_camera_lens as corners_px,
        # in GridSpec.corners_mm order: lower-left, lower-right, upper-right,
        # upper-left.
        "corners_px": corners,
        # Every true (mm -> distorted px) pair, for a full recovery check.
        "points": [
            {"mm": [mm[0], mm[1]], "px": [round(p[0], 3), round(p[1], 3)]}
            for mm, p in truth
        ],
    }
    return img, meta


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--n", type=int, default=7)
    ap.add_argument("--pitch", type=float, default=10.0)
    ap.add_argument("--dot", type=float, default=2.0)
    ap.add_argument("--barrel", type=float, default=0.05, help="barrel k (corner ~= k*r^2)")
    ap.add_argument("--png", required=True)
    ap.add_argument("--json", required=True)
    args = ap.parse_args()

    img, meta = build(args.n, args.pitch, args.dot, args.barrel)
    img.save(args.png)
    with open(args.json, "w", encoding="utf-8", newline="\n") as f:
        json.dump(meta, f, indent=2)
        f.write("\n")
    print(f"wrote {args.png} ({img.size[0]}x{img.size[1]}) and {args.json}")


if __name__ == "__main__":
    main()
