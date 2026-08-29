#!/usr/bin/env python3
"""Render the Zeron app-icon rasters from the mark vector (dist/zeron.svg).

dist/zeron.svg is the single source of truth: a black rounded-rect "squircle"
plus the 34 white cells of the Zeron pixel mark, positioned by a translate +
scale transform. This script parses that geometry (it never hardcodes the
cells) and paints it with Pillow — the mark is only rounded rectangles, so no
SVG rasterizer is needed.

Outputs (all committed so CI never needs Pillow — rerun only when the mark in
dist/zeron.svg changes):
  dist/macos/icon-1024.png    macOS .icns source: RGBA, squircle + margins,
                              subtle baked drop shadow (sips + iconutil turn
                              this into zeron.icns in scripts/package-macos.sh)
  dist/zeron.png              Linux icon: RGBA, same squircle, no shadow
  apps/ios/Zeron/Assets.xcassets/AppIcon.appiconset/AppIcon1024.png
                              iOS app icon: RGB, full-bleed, NO alpha and NO
                              rounded corners (iOS masks/rounds it itself)

Rendered with Pillow 12.3.0.

Usage: python3 scripts/generate-icons.py [--no-shadow]
"""

import os
import re
import sys

from PIL import Image, ImageDraw, ImageFilter

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SVG = os.path.join(ROOT, "dist/zeron.svg")

SS = 4  # supersample: ImageDraw has no anti-aliasing, so render at 4x then
SIZE = 1024  # Lanczos-downscale to this edge. Radii scale with SS too.

BLACK = (0, 0, 0, 255)
WHITE = (255, 255, 255, 255)

# iOS full-bleed glyph scale. The glyph bbox is 820x940; at Sc the top-right
# cell's device right edge is 512 + 410*Sc, which must clear iOS's ~229px
# corner mask (safe for Sc <= 0.69). 0.66 keeps comfortable headroom while
# filling more of the canvas than the margined macOS squircle, as full-bleed
# art should.
IOS_SCALE = 0.66

# Subtle baked drop shadow for the macOS squircle (final px, pre-supersample);
# only ever visible in the 100px transparent margin.
SHADOW_BLUR = 16.0
SHADOW_DY = 10.0
SHADOW_ALPHA = 64  # ~0.25 * 255


def _attrs(rect):
    return dict(re.findall(r'(\w+)="([^"]*)"', rect))


def parse_mark():
    """Parse dist/zeron.svg into (squircle, cells, transform, bbox).

    squircle = (x, y, w, h, rx); cells = [(x, y, w, h, rx), ...] in the glyph's
    local coordinates; transform = (tx, ty, scale) placing the glyph on the
    squircle; bbox = (minx, miny, maxx, maxy) of the glyph cells.
    """
    with open(SVG, encoding="utf-8") as f:
        svg = f.read()

    squircle = None
    cells = []
    for rect in re.findall(r"<rect\b[^>]*>", svg):
        a = _attrs(rect)
        geom = (
            float(a.get("x", 0)),
            float(a.get("y", 0)),
            float(a["width"]),
            float(a["height"]),
            float(a.get("rx", 0)),
        )
        if a.get("fill", "").lower() in ("#000000", "#000", "black"):
            squircle = geom
        else:
            cells.append(geom)

    m = re.search(
        r"translate\(\s*([-\d.]+)[ ,]+([-\d.]+)\s*\)\s*scale\(\s*([-\d.]+)\s*\)",
        svg,
    )
    transform = (float(m.group(1)), float(m.group(2)), float(m.group(3)))

    bbox = (
        min(x for x, y, w, h, rx in cells),
        min(y for x, y, w, h, rx in cells),
        max(x + w for x, y, w, h, rx in cells),
        max(y + h for x, y, w, h, rx in cells),
    )

    # Drift alarms: if the mark in dist/zeron.svg is ever edited, these fire so
    # the renderer is never silently wrong.
    assert squircle is not None, "no black squircle rect in dist/zeron.svg"
    assert len(cells) == 34, f"expected 34 glyph cells, found {len(cells)}"
    assert bbox == (0.0, 0.0, 820.0, 940.0), f"unexpected glyph bbox {bbox}"
    assert abs(squircle[4] / squircle[2] - 0.225) < 1e-3, "bad squircle rx ratio"
    return squircle, cells, transform, bbox


def _rr(draw, x, y, w, h, rx, fill):
    """Rounded rect given in final px; drawn at supersample scale."""
    draw.rounded_rectangle(
        [x * SS, y * SS, (x + w) * SS, (y + h) * SS], radius=rx * SS, fill=fill
    )


def _glyph(draw, cells, tx, ty, scale):
    for lx, ly, w, h, rx in cells:
        _rr(draw, tx + scale * lx, ty + scale * ly, scale * w, scale * h,
            scale * rx, WHITE)


def render_squircle(squircle, cells, transform, shadow):
    """dist/zeron.svg reproduced faithfully on a transparent canvas (macOS /
    Linux). With `shadow`, a subtle drop shadow is baked into the margin."""
    tx, ty, scale = transform
    sx, sy, sw, sh, srx = squircle
    big = SIZE * SS
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))

    if shadow:
        layer = Image.new("RGBA", (big, big), (0, 0, 0, 0))
        _rr(ImageDraw.Draw(layer), sx, sy, sw, sh, srx, (0, 0, 0, SHADOW_ALPHA))
        layer = layer.filter(ImageFilter.GaussianBlur(SHADOW_BLUR * SS))
        img.alpha_composite(layer, (0, round(SHADOW_DY * SS)))

    draw = ImageDraw.Draw(img)
    _rr(draw, sx, sy, sw, sh, srx, BLACK)  # opaque squircle covers shadow inside
    _glyph(draw, cells, tx, ty, scale)
    return img.resize((SIZE, SIZE), Image.Resampling.LANCZOS)


def render_fullbleed(cells, bbox, scale):
    """Opaque black to the edges with the glyph centered — iOS full-bleed (no
    alpha, no rounded corners; iOS applies its own mask)."""
    gw, gh = bbox[2] - bbox[0], bbox[3] - bbox[1]
    big = SIZE * SS
    img = Image.new("RGBA", (big, big), BLACK)
    tx = SIZE / 2 - scale * gw / 2
    ty = SIZE / 2 - scale * gh / 2
    _glyph(ImageDraw.Draw(img), cells, tx, ty, scale)
    return img.resize((SIZE, SIZE), Image.Resampling.LANCZOS)


def _save(img, rel):
    path = os.path.join(ROOT, rel)
    img.save(path, optimize=True)
    print(f"wrote {rel} ({img.size[0]}x{img.size[1]}, {img.mode})")


def main():
    shadow = "--no-shadow" not in sys.argv[1:]
    squircle, cells, transform, bbox = parse_mark()
    print(f"parsed {len(cells)} cells, bbox {bbox}, squircle {squircle}")

    _save(render_squircle(squircle, cells, transform, shadow),
          "dist/macos/icon-1024.png")
    _save(render_squircle(squircle, cells, transform, False),
          "dist/zeron.png")
    _save(render_fullbleed(cells, bbox, IOS_SCALE).convert("RGB"),
          "apps/ios/Zeron/Assets.xcassets/AppIcon.appiconset/AppIcon1024.png")


if __name__ == "__main__":
    main()
