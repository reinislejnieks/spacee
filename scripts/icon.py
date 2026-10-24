#!/usr/bin/env python3
"""Draws the app icon, a small icicle chart, into crates/app/AppIcon.icon.

The icon is an Icon Composer package: a light and a dark artwork layer over a fill that
also changes with the appearance, so macOS 26 shows the dark icon in dark mode.
scripts/run.sh compiles it with actool, which also writes the .icns older macOS uses.

Needs Pillow (`pip install pillow`). Rerun it after changing the design. Pass a path to
also save a flattened 1024 px preview of each appearance next to it.
"""
import colorsys
import json
import sys
from pathlib import Path

from PIL import Image, ImageDraw

SCALE = 4  # drawn large and scaled down, for antialiasing
SIZE = 1024 * SCALE
LEFT, RIGHT = 196, 828
GAP = 14
RADIUS = 22
# (top, bottom) of the root bar and the three rows under it, as in the app's chart.
ROWS = [(236, 316), (330, 470), (484, 624), (638, 778)]

# Hue and saturation per file type, from palette() in crates/app/src/chart.rs.
ARCHIVE, CODE, IMAGE, VIDEO, DOC, AUDIO, APP = (
    (0.76, 0.55),
    (0.36, 0.55),
    (0.12, 0.80),
    (0.92, 0.72),
    (0.60, 0.72),
    (0.04, 0.78),
    (0.50, 0.65),
)

# Each node: (share of its parent, file type, children).
TREE = [
    (0.50, ARCHIVE, [
        (0.56, ARCHIVE, [(0.6, ARCHIVE, []), (0.4, DOC, [])]),
        (0.26, VIDEO, [(1.0, VIDEO, [])]),
        (0.18, DOC, []),
    ]),
    (0.32, CODE, [
        (0.62, CODE, [(0.7, CODE, []), (0.3, CODE, [])]),
        (0.38, APP, [(1.0, APP, [])]),
    ]),
    (0.18, IMAGE, [
        (0.6, IMAGE, [(1.0, IMAGE, [])]),
        (0.4, AUDIO, []),
    ]),
]

MINT = (74, 222, 155)
FILLS = {"light": "srgb:0.95686,0.96863,0.98039,1.00000", "dark": "srgb:0.07843,0.10196,0.13725,1.00000"}


def rgb(hue, saturation, lightness):
    return tuple(round(c * 255) for c in colorsys.hls_to_rgb(hue, lightness, saturation))


def block(img, x0, x1, row, top, bottom, alpha):
    """A rounded block with a top-lit vertical gradient from `top` to `bottom`."""
    y0, y1 = ROWS[row]
    box = [round(v * SCALE) for v in (x0 + GAP / 2, y0, x1 - GAP / 2, y1)]
    if box[2] - box[0] < 4 * SCALE:
        return
    w, h = box[2] - box[0], box[3] - box[1]
    shade = Image.new("RGBA", (w, h))
    sd = ImageDraw.Draw(shade)
    for y in range(h):
        t = y / max(h - 1, 1)
        colour = tuple(round(a + (b - a) * t) for a, b in zip(top, bottom))
        sd.line([(0, y), (w, y)], fill=colour + (round(255 * alpha),))
    mask = Image.new("L", (w, h), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, w - 1, h - 1], RADIUS * SCALE, fill=255)
    img.paste(shade, (box[0], box[1]), mask)


def draw(dark):
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    base = 0.58 if dark else 0.50
    block(img, LEFT, RIGHT, 0, rgb(0.42, 0.78, 0.66 if dark else 0.56), MINT if dark else rgb(0.42, 0.70, 0.44), 1.0)

    def place(nodes, x0, x1, row):
        cursor = x0
        for share, (hue, saturation), children in nodes:
            end = cursor + (x1 - x0) * share
            light = base + (row - 1) * (0.05 if dark else 0.07)
            top = rgb(hue, saturation, min(light + 0.08, 0.9))
            bottom = rgb(hue, saturation * 0.9, light - 0.04)
            block(img, cursor, end, row, top, bottom, 1.0 - (row - 1) * 0.18)
            if children:
                place(children, cursor, end, row + 1)
            cursor = end

    place(TREE, LEFT, RIGHT, 1)
    return img.resize((1024, 1024), Image.LANCZOS)


def main():
    root = Path(__file__).resolve().parent.parent
    package = root / "crates/app/AppIcon.icon"
    (package / "Assets").mkdir(parents=True, exist_ok=True)
    art = {appearance: draw(appearance == "dark") for appearance in ("light", "dark")}
    for appearance, img in art.items():
        img.save(package / "Assets" / f"icicle-{appearance}.png")
    manifest = {
        "fill": {"automatic-gradient": FILLS["light"]},
        "fill-specializations": [
            {"appearance": "dark", "value": {"automatic-gradient": FILLS["dark"]}}
        ],
        "groups": [
            {
                # actool keeps a dark image only when the light one is listed as the
                # specialization's base entry, not as a plain image-name.
                "layers": [
                    {
                        "name": "icicle",
                        "image-name-specializations": [
                            {"value": "icicle-light.png"},
                            {"appearance": "dark", "value": "icicle-dark.png"},
                        ],
                    }
                ],
                "shadow": {"kind": "neutral", "opacity": 0.5},
                "translucency": {"enabled": False, "value": 0.5},
            }
        ],
        "supported-platforms": {"squares": ["macOS"]},
    }
    (package / "icon.json").write_text(json.dumps(manifest, indent=2) + "\n")
    if len(sys.argv) > 1:
        out = Path(sys.argv[1])
        for appearance, img in art.items():
            tile = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
            bg = (244, 247, 250) if appearance == "light" else (20, 26, 35)
            mask = Image.new("L", (1024, 1024), 0)
            ImageDraw.Draw(mask).rounded_rectangle([100, 100, 924, 924], 185, fill=255)
            tile.paste(Image.new("RGBA", (1024, 1024), bg + (255,)), (0, 0), mask)
            tile.alpha_composite(img)
            tile.save(out.with_name(f"{out.stem}-{appearance}.png"))
    print(package)


if __name__ == "__main__":
    main()
