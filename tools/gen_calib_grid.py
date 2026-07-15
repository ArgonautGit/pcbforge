#!/usr/bin/env python3
"""Generate a printable camera-lens calibration grid (SVG).

The console's camera-lens step (`ui::calib::fit_camera_lens`) images a printed
grid of known geometry, detects each dot with the `DarkDot` fiducial profile,
and fits a bi-cubic pixel->true-mm polynomial. This script emits that printed
reference: an ``n x n`` lattice of solid black dots at a known pitch, laid out
in the machine frame (origin at the lower-left, +X right, +Y up) so the corner
click order in the console matches what the operator sees on paper.

The SVG is sized in real millimetres, so printing at 100% (actual size, no
"fit to page") yields a physically exact grid. A caliper dimension line under
the bottom row lets the operator verify the print scale and read off the TRUE
pitch to type into the console -- consumer printers scale by a percent or two,
and that error would otherwise be baked straight into the metric fit.

Deterministic: same arguments -> byte-identical output. No dependencies.

Usage:
    python3 tools/gen_calib_grid.py \
        --n 7 --pitch 10.0 --dot 2.0 \
        --out samples/calibration/grid-7x7-10mm.svg
"""

from __future__ import annotations

import argparse


def fmt(x: float) -> str:
    """Trim floats to a stable, compact decimal (no trailing zeros)."""
    s = f"{x:.4f}".rstrip("0").rstrip(".")
    return s if s not in ("", "-0") else "0"


def build_svg(n: int, pitch: float, dot: float, page: tuple[float, float]) -> str:
    pw, ph = page
    span = (n - 1) * pitch  # centre-to-centre across the outer dots
    r = dot / 2.0

    # Centre the grid horizontally; leave headroom at the top for the title and
    # room below for the caliper dimension line + instructions.
    gx0 = (pw - span) / 2.0  # SVG-x of column 0 (origin column)
    text_x = 16.0  # left margin for the title/instructions (full page width)
    gy_top = 56.0  # SVG-y of the top row (row n-1); clears the instruction block

    # Machine frame is y-up with the origin at the lower-left, but SVG-y grows
    # downward, so row 0 (origin) sits at the largest SVG-y.
    def cx(col: int) -> float:
        return gx0 + col * pitch

    def cy(row: int) -> float:
        return gy_top + (n - 1 - row) * pitch

    parts: list[str] = []
    parts.append(
        f'<svg xmlns="http://www.w3.org/2000/svg" '
        f'width="{fmt(pw)}mm" height="{fmt(ph)}mm" '
        f'viewBox="0 0 {fmt(pw)} {fmt(ph)}">'
    )
    # White page so the dark-dot detector sees maximum contrast.
    parts.append(f'<rect x="0" y="0" width="{fmt(pw)}" height="{fmt(ph)}" fill="#ffffff"/>')

    # --- the dot lattice -------------------------------------------------
    parts.append("<g fill=\"#000000\">")
    for row in range(n):
        for col in range(n):
            parts.append(
                f'<circle cx="{fmt(cx(col))}" cy="{fmt(cy(row))}" r="{fmt(r)}"/>'
            )
    parts.append("</g>")

    # --- corner registration marks + click order (1=LL,2=LR,3=UR,4=UL) ---
    corners = [
        (0, 0, "1"),  # lower-left  = origin
        (n - 1, 0, "2"),  # lower-right
        (n - 1, n - 1, "3"),  # upper-right
        (0, n - 1, "4"),  # upper-left
    ]
    ring = r + 2.2
    tick = ring + 1.6
    parts.append('<g fill="none" stroke="#c01818" stroke-width="0.35">')
    for col, row, _ in corners:
        x, y = cx(col), cy(row)
        parts.append(f'<circle cx="{fmt(x)}" cy="{fmt(y)}" r="{fmt(ring)}"/>')
        parts.append(f'<line x1="{fmt(x - tick)}" y1="{fmt(y)}" x2="{fmt(x + tick)}" y2="{fmt(y)}"/>')
        parts.append(f'<line x1="{fmt(x)}" y1="{fmt(y - tick)}" x2="{fmt(x)}" y2="{fmt(y + tick)}"/>')
    parts.append("</g>")
    parts.append('<g fill="#c01818" font-family="sans-serif" font-size="3.2" font-weight="bold">')
    for col, row, label in corners:
        x, y = cx(col), cy(row)
        # Nudge the number outward from the grid centre.
        dx = -1 if col == 0 else 1
        dy = 1 if row == 0 else -1
        lx = x + dx * (tick + 1.0) - (1.0 if dx < 0 else -0.2)
        ly = y + dy * (tick + 3.4)
        parts.append(f'<text x="{fmt(lx)}" y="{fmt(ly)}">{label}</text>')
    parts.append("</g>")

    # --- origin axes: +X right, +Y up from the lower-left dot ------------
    ox, oy = cx(0), cy(0)
    axis_len = pitch * 0.9
    parts.append('<g stroke="#1858c0" stroke-width="0.5" fill="#1858c0">')
    # +X arrow (right)
    parts.append(f'<line x1="{fmt(ox)}" y1="{fmt(oy)}" x2="{fmt(ox + axis_len)}" y2="{fmt(oy)}"/>')
    parts.append(
        f'<polygon points="{fmt(ox + axis_len)},{fmt(oy)} '
        f'{fmt(ox + axis_len - 1.6)},{fmt(oy - 0.9)} '
        f'{fmt(ox + axis_len - 1.6)},{fmt(oy + 0.9)}"/>'
    )
    # +Y arrow (up = decreasing SVG-y)
    parts.append(f'<line x1="{fmt(ox)}" y1="{fmt(oy)}" x2="{fmt(ox)}" y2="{fmt(oy - axis_len)}"/>')
    parts.append(
        f'<polygon points="{fmt(ox)},{fmt(oy - axis_len)} '
        f'{fmt(ox - 0.9)},{fmt(oy - axis_len + 1.6)} '
        f'{fmt(ox + 0.9)},{fmt(oy - axis_len + 1.6)}"/>'
    )
    parts.append("</g>")
    parts.append('<g fill="#1858c0" font-family="sans-serif" font-size="3">')
    parts.append(f'<text x="{fmt(ox + axis_len + 0.8)}" y="{fmt(oy + 1.1)}">+X</text>')
    parts.append(f'<text x="{fmt(ox - 4.6)}" y="{fmt(oy - axis_len - 0.8)}">+Y</text>')
    parts.append("</g>")

    # --- caliper dimension line across the bottom row --------------------
    dim_y = cy(0) + 10.0
    x_l, x_r = cx(0), cx(n - 1)
    parts.append('<g stroke="#000000" stroke-width="0.3" fill="none">')
    parts.append(f'<line x1="{fmt(x_l)}" y1="{fmt(dim_y)}" x2="{fmt(x_r)}" y2="{fmt(dim_y)}"/>')
    for x in (x_l, x_r):
        parts.append(f'<line x1="{fmt(x)}" y1="{fmt(dim_y - 2.0)}" x2="{fmt(x)}" y2="{fmt(dim_y + 2.0)}"/>')
        # extension lines up to the corner dots
        parts.append(f'<line x1="{fmt(x)}" y1="{fmt(cy(0) + r)}" x2="{fmt(x)}" y2="{fmt(dim_y + 2.0)}"/>')
    parts.append("</g>")
    parts.append(
        f'<text x="{fmt((x_l + x_r) / 2.0)}" y="{fmt(dim_y + 4.6)}" '
        f'text-anchor="middle" font-family="sans-serif" font-size="3">'
        f'{fmt(span)} mm nominal across {n} dots ({n - 1} x {fmt(pitch)} mm) '
        f'&#8212; measure with calipers</text>'
    )

    # --- title + instructions --------------------------------------------
    tx = text_x
    parts.append('<g font-family="sans-serif" fill="#000000">')
    parts.append(f'<text x="{fmt(tx)}" y="16" font-size="5" font-weight="bold">PCBForge camera-lens calibration grid</text>')
    parts.append(
        f'<text x="{fmt(tx)}" y="23" font-size="3.2">{n} x {n} dots '
        f'&#183; nominal {fmt(pitch)} mm pitch &#183; {fmt(dot)} mm dots</text>'
    )
    lines = [
        "PRINT AT 100% / ACTUAL SIZE — turn OFF 'fit to page' / 'scale to fit'.",
        "Measure the span below with calipers; enter the TRUE pitch in the console (printers scale ~1-2%).",
        "Tape flat to the bed, image it, click corners 1→2→3→4, then Fit.",
    ]
    for i, ln in enumerate(lines):
        parts.append(f'<text x="{fmt(tx)}" y="{fmt(29 + i * 4.4)}" font-size="3">{ln}</text>')
    parts.append("</g>")

    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--n", type=int, default=7, help="dots per side (default 7)")
    ap.add_argument("--pitch", type=float, default=10.0, help="nominal dot pitch, mm (default 10)")
    ap.add_argument("--dot", type=float, default=2.0, help="dot diameter, mm (default 2)")
    ap.add_argument("--page", default="A4", choices=["A4", "Letter"], help="page size (default A4)")
    ap.add_argument("--out", required=True, help="output SVG path")
    args = ap.parse_args()

    page = (210.0, 297.0) if args.page == "A4" else (215.9, 279.4)
    svg = build_svg(args.n, args.pitch, args.dot, page)
    with open(args.out, "w", encoding="utf-8", newline="\n") as f:
        f.write(svg)
    print(f"wrote {args.out} ({args.n}x{args.n}, {args.pitch} mm pitch, {args.page})")


if __name__ == "__main__":
    main()
