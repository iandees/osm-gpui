#!/usr/bin/env python3
"""Draw the placeholder app icon and build the icon files from it.

Usage:
    python3 scripts/make-icon.py

Needs Pillow (`pip install pillow`). Writes:
    assets/AppIcon.png    1024x1024 master (also used by the Linux package)
    assets/AppIcon.ico    multi-size Windows icon
    assets/AppIcon.icns   macOS icon (only when `iconutil` is available)

The artwork follows the macOS Big Sur icon grid: a 1024 px canvas with an
824 px rounded-square tile centred in it and a transparent margin. The glyph is
a way (polyline) through three nodes, one of them highlighted, on a pale
map-like background. It is drawn at 4x and downsampled for anti-aliasing.
"""
import os
import shutil
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
ASSETS = os.path.join(ROOT, "assets")

CANVAS = 1024
TILE = 824
RADIUS = 185  # ~22.4% of the tile, the Big Sur corner curvature (as a circular arc)
SS = 4  # supersampling factor

# Three flat colours plus neutrals.
PAPER = (244, 239, 224)  # map background
PARK = (205, 226, 190)  # pale green block
WATER = (184, 216, 238)  # pale blue block
WAY = (38, 84, 150)  # the way and node outlines
NODE = (255, 255, 255)
HIGHLIGHT = (244, 130, 32)
HALO = (250, 214, 176)  # pale tint of the highlight colour

# Glyph geometry on the 1024 canvas.
POINTS = [(265, 700), (480, 400), (775, 540)]
HIGHLIGHTED = 1
WAY_WIDTH = 84
NODE_R = 100
NODE_OUTLINE = 40


def render() -> Image.Image:
    s = SS
    size = CANVAS * s
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    off = (CANVAS - TILE) // 2
    box = [off * s, off * s, (off + TILE) * s - 1, (off + TILE) * s - 1]

    # Draw the map content on its own layer, then clip it to the rounded tile.
    layer = Image.new("RGBA", (size, size), PAPER + (255,))
    ld = ImageDraw.Draw(layer)
    # Park block, bottom-right; water block, top-left.
    ld.rounded_rectangle([560 * s, 640 * s, 960 * s, 960 * s], radius=40 * s, fill=PARK)
    ld.rounded_rectangle([60 * s, 90 * s, 440 * s, 300 * s], radius=40 * s, fill=WATER)
    ld.polygon(
        [(620 * s, 60 * s), (980 * s, 60 * s), (980 * s, 330 * s), (760 * s, 250 * s)],
        fill=WATER,
    )

    pts = [(x * s, y * s) for x, y in POINTS]
    # Halo behind the highlighted node (drawn first so the way crosses it).
    hx, hy = pts[HIGHLIGHTED]
    h = (NODE_R + 50) * s
    ld.ellipse([hx - h, hy - h, hx + h, hy + h], fill=HALO)
    # Way: thick polyline with round joins and caps.
    ld.line(pts, fill=WAY, width=WAY_WIDTH * s, joint="curve")
    for x, y in pts:
        r = WAY_WIDTH * s // 2
        ld.ellipse([x - r, y - r, x + r, y + r], fill=WAY)
    # Nodes: white discs with a dark outline; the highlighted one is orange.
    for i, (x, y) in enumerate(pts):
        r = NODE_R * s
        ld.ellipse([x - r, y - r, x + r, y + r], fill=WAY)
        r2 = (NODE_R - NODE_OUTLINE) * s
        ld.ellipse(
            [x - r2, y - r2, x + r2, y + r2],
            fill=HIGHLIGHT if i == HIGHLIGHTED else NODE,
        )

    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle(box, radius=RADIUS * s, fill=255)
    img.paste(layer, (0, 0), mask)
    return img.resize((CANVAS, CANVAS), Image.LANCZOS)


def build_iconset(master: Image.Image, path: str) -> None:
    os.makedirs(path)
    for base in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            px = base * scale
            suffix = "" if scale == 1 else "@2x"
            master.resize((px, px), Image.LANCZOS).save(
                os.path.join(path, f"icon_{base}x{base}{suffix}.png")
            )


def main() -> int:
    os.makedirs(ASSETS, exist_ok=True)
    master = render()
    master.save(os.path.join(ASSETS, "AppIcon.png"))

    master.save(
        os.path.join(ASSETS, "AppIcon.ico"),
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )

    if shutil.which("iconutil"):
        with tempfile.TemporaryDirectory() as tmp:
            iconset = os.path.join(tmp, "AppIcon.iconset")
            build_iconset(master, iconset)
            subprocess.run(
                ["iconutil", "-c", "icns", iconset, "-o", os.path.join(ASSETS, "AppIcon.icns")],
                check=True,
            )
    else:
        print("iconutil not found; skipped AppIcon.icns", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
