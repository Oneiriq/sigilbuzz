#!/usr/bin/env python3
"""Write the progressive JPEG fixtures of sigilbuzz-render's decoder
tests with Pillow (libjpeg-turbo).

Each picture is saved twice with the same quality and subsampling, as a
progressive file (libjpeg's default script, with DC and AC refinement
scans) and as a baseline file, so both carry the same quantized
coefficients and must decode to the same pixels. For the layouts
without chroma subsampling, Pillow's own decode is saved next to them as
raw RGB or gray samples; the tests allow the few levels that integer
and float IDCTs differ by.

Run:
    uv run --with pillow python tests/tools/build_progressive_jpeg_fixtures.py
"""

from __future__ import annotations

import math
from pathlib import Path

from PIL import Image

OUT = (
    Path(__file__).resolve().parent.parent.parent
    / "crates/sigilbuzz-render/src/jpeg_decode/tests/fixtures"
)


def emoji(w: int, h: int) -> Image.Image:
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        for x in range(w):
            d = math.hypot(x - w * 0.4, y - h * 0.45)
            r = max(0, min(255, int(250 - d * 3)))
            g = max(0, min(255, int(210 - d * 2.5)))
            inside = (x - w * 0.6) ** 2 + (y - h * 0.6) ** 2 <= (min(w, h) * 0.15) ** 2
            px[x, y] = (r, g, 240 if inside else 30)
    return img


def shapes(w: int, h: int) -> Image.Image:
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        for x in range(w):
            d = math.hypot(x - w / 2, y - h / 2)
            r = 230 if d < min(w, h) / 3 else 20
            g = 200 if (x // 5 + y // 7) % 2 else 40
            b = int(128 + 127 * math.sin(x / 3.0) * math.cos(y / 4.0))
            px[x, y] = (r, g, b)
    return img


# (name, picture, Pillow mode, subsampling, keep Pillow's decode)
CASES = [
    ("rgb444_20x12", emoji(20, 12), "RGB", 0, True),
    ("rgb422_33x17", shapes(33, 17), "RGB", 1, False),
    ("rgb420_33x17", emoji(33, 17), "RGB", 2, False),
    ("gray_47x9", emoji(47, 9), "L", None, True),
]


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    for name, picture, mode, subsampling, keep in CASES:
        img = picture.convert(mode)
        kw = {"quality": 90}
        if subsampling is not None:
            kw["subsampling"] = subsampling
        for kind, progressive in [("prog", True), ("base", False)]:
            path = OUT / f"pillow_{name}_{kind}.jpg"
            img.save(path, progressive=progressive, **kw)
        if keep:
            decoded = Image.open(OUT / f"pillow_{name}_prog.jpg")
            decoded.load()
            (OUT / f"pillow_{name}_prog.raw").write_bytes(decoded.tobytes())


if __name__ == "__main__":
    main()
