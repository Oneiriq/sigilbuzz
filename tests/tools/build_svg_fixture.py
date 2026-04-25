#!/usr/bin/env python3
"""Synthesise a tiny `SVG `-in-OpenType color font.

Pre-COLRv1 colour-emoji fonts (older Twitter / Mozilla / Adobe builds)
ship inline SVG documents in an `SVG ` table. We need a structurally
valid one to drive the integration tests; fontTools has good support
for the high-level table API, so we describe the table in its native
``DocList`` form and let it serialise.

Output font ``svg_synthetic.ttf`` carries:

- Two glyphs (``.notdef``, ``circle``) with rectangle outlines so the
  ``glyf`` / ``loca`` tables stay well-formed.
- A cmap mapping U+0041 → circle.
- An `SVG ` (note trailing space) table with one document covering
  gid 1, holding a tiny `<svg>...<circle>...</svg>` payload.

Run:
    python3 tests/tools/build_svg_fixture.py
"""

from __future__ import annotations

from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import newTable

OUT_PATH = Path(__file__).resolve().parent.parent / "fixtures" / "svg_synthetic.ttf"
UPEM = 1000

# Inline SVG document. ASCII; the `SVG ` table doesn't require XML
# preamble. Renderers commonly inspect the root <svg> element to set
# up their viewport.
SVG_DOC = (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">'
    '<circle cx="50" cy="50" r="40" fill="#ff6600"/>'
    "</svg>"
)


def build_rect():
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((100, 0))
    pen.lineTo((100, 100))
    pen.lineTo((0, 100))
    pen.closePath()
    return pen.glyph()


def main():
    glyph_order = [".notdef", "circle"]
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(glyph_order)
    fb.setupCharacterMap({ord("A"): "circle"})
    glyphs = {".notdef": build_rect(), "circle": build_rect()}
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({".notdef": (200, 0), "circle": (300, 0)})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "SigilbuzzSvg", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()

    # fontTools' SVG table model is straightforward:
    #   docList: list of [svgDocStr, startGlyphID, endGlyphID]
    svg_table = newTable("SVG ")
    svg_table.docList = [(SVG_DOC, 1, 1)]
    fb.font["SVG "] = svg_table

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    fb.save(str(OUT_PATH))
    size = OUT_PATH.stat().st_size
    print(f"Wrote {OUT_PATH} ({size} bytes)")


if __name__ == "__main__":
    main()
