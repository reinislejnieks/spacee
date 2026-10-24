#!/usr/bin/env python3
"""Draws the app icon, a sunburst on a dark rounded square, into crates/app/AppIcon.icns.

Needs Pillow (`pip install pillow`). Rerun it after changing the design.
"""
import colorsys
import math
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw

SCALE = 4  # drawn large and scaled down, for antialiasing
SIZE = 1024 * SCALE
# macOS icon grid: an 824 px rounded square centred on the 1024 px canvas.
TILE = 824 * SCALE
CORNER = 185 * SCALE
RADIUS = 330 * SCALE
HOLE = 0.3
RINGS = 3
GAP = 6 * SCALE

# Each ring: (start, sweep) pairs as fractions of a turn. The outer rings subdivide
# parts of the inner ones, as folders inside folders do.
LAYOUT = [
    [(0.00, 0.40), (0.40, 0.24), (0.64, 0.17), (0.81, 0.12), (0.93, 0.07)],
    [(0.00, 0.22), (0.22, 0.12), (0.40, 0.15), (0.55, 0.06), (0.64, 0.11), (0.81, 0.08)],
    [(0.00, 0.13), (0.13, 0.06), (0.22, 0.09), (0.40, 0.08), (0.64, 0.06)],
]


def colour(hue, depth):
    # The app's chart palette: saturation falls and lightness rises with depth.
    s = 0.72 - depth * 0.07
    l = 0.52 + depth * 0.05
    r, g, b = colorsys.hls_to_rgb(hue, l, s)
    return (round(r * 255), round(g * 255), round(b * 255), 255)


def sector(cx, cy, inner, outer, a0, a1):
    """Polygon of an annular sector, angles in radians clockwise from 12 o'clock."""
    steps = max(8, int((a1 - a0) * 120))
    gap_out, gap_in = GAP / 2 / outer, GAP / 2 / inner
    outer_pts = [
        (cx + outer * math.sin(a), cy - outer * math.cos(a))
        for a in (a0 + gap_out + (a1 - a0 - 2 * gap_out) * i / steps for i in range(steps + 1))
    ]
    inner_pts = [
        (cx + inner * math.sin(a), cy - inner * math.cos(a))
        for a in (a1 - gap_in - (a1 - a0 - 2 * gap_in) * i / steps for i in range(steps + 1))
    ]
    return outer_pts + inner_pts


def draw():
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))

    # Background: a vertical gradient clipped to the rounded square.
    gradient = Image.new("RGBA", (SIZE, SIZE))
    top, bottom = (40, 44, 62), (14, 16, 24)
    gd = ImageDraw.Draw(gradient)
    for y in range(SIZE):
        t = y / SIZE
        gd.line(
            [(0, y), (SIZE, y)],
            fill=tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)) + (255,),
        )
    mask = Image.new("L", (SIZE, SIZE), 0)
    m = (SIZE - TILE) // 2
    ImageDraw.Draw(mask).rounded_rectangle([m, m, m + TILE, m + TILE], CORNER, fill=255)
    img.paste(gradient, (0, 0), mask)

    d = ImageDraw.Draw(img)
    cx = cy = SIZE / 2
    hole = RADIUS * HOLE
    width = (RADIUS - hole) / RINGS
    for depth, ring in enumerate(LAYOUT):
        inner = hole + depth * width + GAP / 2
        outer = hole + (depth + 1) * width - GAP / 2
        for start, sweep in ring:
            hue = start + sweep / 2
            a0, a1 = start * math.tau, (start + sweep) * math.tau
            d.polygon(sector(cx, cy, inner, outer, a0, a1), fill=colour(hue, depth))

    return img.resize((1024, 1024), Image.LANCZOS)


def main():
    root = Path(__file__).resolve().parent.parent
    icon = draw()
    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "AppIcon.iconset"
        iconset.mkdir()
        for size in (16, 32, 128, 256, 512):
            icon.resize((size, size), Image.LANCZOS).save(iconset / f"icon_{size}x{size}.png")
            icon.resize((size * 2, size * 2), Image.LANCZOS).save(
                iconset / f"icon_{size}x{size}@2x.png"
            )
        out = root / "crates/app/AppIcon.icns"
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out)], check=True)
        if len(sys.argv) > 1:
            shutil.copy(iconset / "icon_512x512@2x.png", sys.argv[1])
    print(out)


if __name__ == "__main__":
    main()
