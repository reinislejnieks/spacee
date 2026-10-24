#!/usr/bin/env python3
"""Draws the app icon, a ringed planet on a night-sky tile, into crates/app/AppIcon.icns.

The ring is cut into pieces of different lengths, like a disk split between folders.

Needs Pillow (`pip install pillow`). Rerun it after changing the design.
"""
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

CENTRE = (512 * SCALE, 520 * SCALE)
PLANET = 215 * SCALE
# The ring is an ellipse tilted by TILT, between INNER and 1 times these semi-axes.
RING_A, RING_B = 395 * SCALE, 105 * SCALE
INNER = 0.74
TILT = math.radians(-18)
GAP = 0.035  # radians of the ellipse's parameter between ring pieces

# Each piece of the ring: (share of the full turn, colour).
PIECES = [
    (0.30, (255, 181, 71)),
    (0.18, (255, 122, 89)),
    (0.14, (255, 227, 163)),
    (0.12, (255, 150, 80)),
    (0.10, (250, 205, 120)),
    (0.09, (255, 122, 89)),
    (0.07, (255, 227, 163)),
]

STARS = [(250, 260, 5), (760, 220, 4), (820, 420, 3), (205, 640, 3), (690, 830, 4), (330, 820, 3)]


def lerp(a, b, t):
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


def ring_point(t, scale):
    x, y = RING_A * scale * math.cos(t), RING_B * scale * math.sin(t)
    c, s = math.cos(TILT), math.sin(TILT)
    return (CENTRE[0] + x * c - y * s, CENTRE[1] + x * s + y * c)


def ring_piece(t0, t1):
    steps = max(4, int((t1 - t0) * 80))
    ts = [t0 + (t1 - t0) * i / steps for i in range(steps + 1)]
    return [ring_point(t, 1) for t in ts] + [ring_point(t, INNER) for t in reversed(ts)]


def ring(front):
    """Ring pieces in front of the planet (lower half of the ellipse) or behind it,
    split where the ring passes behind the planet's edge."""
    shapes, t = [], 0.0
    for share, colour in PIECES:
        start, end = t + GAP / 2, t + share * math.tau - GAP / 2
        t += share * math.tau
        # The ring's lower half, t in (0, pi), passes in front of the planet.
        for lo, hi in ((0, math.pi), (math.pi, math.tau), (math.tau, 2 * math.tau)):
            a, b = max(start, lo), min(end, hi)
            if a < b and (lo % math.tau < math.pi) == front:
                shapes.append((ring_piece(a, b), colour))
    return shapes


def draw():
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))

    # Background: a vertical night-sky gradient clipped to the rounded square.
    gradient = Image.new("RGBA", (SIZE, SIZE))
    gd = ImageDraw.Draw(gradient)
    top, bottom = (30, 38, 74), (9, 12, 28)
    for y in range(SIZE):
        gd.line([(0, y), (SIZE, y)], fill=lerp(top, bottom, y / SIZE) + (255,))
    for x, y, r in STARS:
        x, y, r = x * SCALE, y * SCALE, r * SCALE
        gd.ellipse([x - r, y - r, x + r, y + r], fill=(220, 228, 255, 255))
    mask = Image.new("L", (SIZE, SIZE), 0)
    m = (SIZE - TILE) // 2
    ImageDraw.Draw(mask).rounded_rectangle([m, m, m + TILE, m + TILE], CORNER, fill=255)
    img.paste(gradient, (0, 0), mask)

    d = ImageDraw.Draw(img)
    for shape, colour in ring(front=False):
        d.polygon(shape, fill=colour + (255,))

    # The planet: shrinking circles that drift toward the light at the upper left,
    # shading from deep violet at the rim to pale blue at the highlight.
    cx, cy = CENTRE
    light = (cx - PLANET * 0.45, cy - PLANET * 0.5)
    rim, mid, glow = (58, 34, 140), (66, 110, 235), (150, 215, 255)
    steps = 160
    for i in range(steps):
        t = i / steps
        r = PLANET * (1 - t * 0.85)
        x = cx + (light[0] - cx) * t
        y = cy + (light[1] - cy) * t
        colour = lerp(rim, mid, t / 0.6) if t < 0.6 else lerp(mid, glow, (t - 0.6) / 0.4)
        d.ellipse([x - r, y - r, x + r, y + r], fill=colour + (255,))

    for shape, colour in ring(front=True):
        d.polygon(shape, fill=colour + (255,))

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
