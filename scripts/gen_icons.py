# SPDX-License-Identifier: AGPL-3.0-only
"""Generate the blazonry app icon set from the brand mark.

The mark is the dither itself: ordered-dither cells dissolving from a solid
corner into paper — the exact signature the native renderer puts on e-ink.
Legible at 16px (reads as a gradient tile), distinctive at 512px.

Regenerates: web-app/src/icons/{icon.svg,icon-192.png,icon-512.png,
icon-192-maskable.png,icon-512-maskable.png} and favicon-32.png.
Run: python3 scripts/gen_icons.py   (any PIL-capable python)
"""
import pathlib
from PIL import Image, ImageDraw

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "web-app" / "src" / "icons"
INK = (28, 26, 22)        # #1C1A16
PAPER = (244, 242, 237)   # #F4F2ED

def dither_cells(size, inset):
    """Ordered-dither cells: cell (i,j) is filled if threshold(i,j) < t,
    where t ramps from full at top-left to empty at bottom-right.
    Grid coarsens at small sizes so the dissolve stays legible at 16px."""
    n = 4 if size <= 48 else 8  # 4x4 under 48px, 8x8 above
    region = size - 2 * inset
    cell = region / n
    cells = []
    for j in range(n):
        for i in range(n):
            # diagonal ramp + bayer jitter so the dissolve is dither-like, not a hard triangle
            t = 1.0 - (i + j) / (2 * n - 2)
            bayer = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]][j % 4][i % 4] / 16
            if t > 0.06 and (t + bayer * 0.35) > 0.28:
                cells.append((inset + i * cell, inset + j * cell, cell))
    return cells

def render_png(size, path, maskable=False):
    inset = size * (0.18 if maskable else 0.10)   # maskable: keep inside safe zone
    img = Image.new("RGB", (size, size), INK)
    d = ImageDraw.Draw(img)
    for x, y, c in dither_cells(size, inset):
        d.rectangle([x, y, x + c - max(1, size // 256), y + c - max(1, size // 256)], fill=PAPER)
    # round the corners like the SVG
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, size - 1, size - 1], radius=size * 0.1875, fill=255)
    bg = Image.new("RGB", (size, size), (0, 0, 0))
    img = Image.composite(img, bg, mask)
    img.save(path)

def render_svg(path):
    size, inset = 512, 51.2
    rects = "".join(
        f'<rect x="{x:.1f}" y="{y:.1f}" width="{c - 2:.1f}" height="{c - 2:.1f}" fill="#F4F2ED"/>'
        for x, y, c in dither_cells(size, inset))
    path.write_text(
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">'
        f'<rect width="512" height="512" rx="96" fill="#1C1A16"/>{rects}</svg>\n')

def main():
    OUT.mkdir(parents=True, exist_ok=True)
    render_svg(OUT / "icon.svg")
    render_png(192, OUT / "icon-192.png")
    render_png(512, OUT / "icon-512.png")
    render_png(192, OUT / "icon-192-maskable.png", maskable=True)
    render_png(512, OUT / "icon-512-maskable.png", maskable=True)
    render_png(32, OUT / "favicon-32.png")
    render_png(16, OUT / "favicon-16.png")
    for f in sorted(OUT.iterdir()):
        print(f.name, f.stat().st_size)

if __name__ == "__main__":
    main()
