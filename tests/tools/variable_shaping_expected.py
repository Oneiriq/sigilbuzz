#!/usr/bin/env python3
"""Writes tests/fixtures/rubik_variable_shaping.expected from HarfBuzz.

The expectations behind `tests/anchor_variations.rs`: HarfBuzz's glyphs and
positions for short texts shaped with `tests/fixtures/rubik_vf.ttf` at
several `wght` values, set as design coordinates the way
`hb_font_set_variations` takes them. HarfBuzz rounds its variation deltas
with `floor(x + 0.5)`.

One record per line, fields separated by spaces:

    <group> <wght> <ltr|rtl> <code points> <glyph> <glyph> ...

where `<code points>` are hex values joined by commas, and each `<glyph>` is
`gid,x_advance,y_advance,x_offset,y_offset`.

Run (needs uharfbuzz; HarfBuzz 14.5.0 through uharfbuzz 0.56.2 wrote the
committed file):

    uv run --no-project --with uharfbuzz==0.56.2 python tests/tools/variable_shaping_expected.py
"""

from __future__ import annotations

from pathlib import Path

import uharfbuzz as hb

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"
FONT_PATH = FIXTURES / "rubik_vf.ttf"
OUT_PATH = FIXTURES / "rubik_variable_shaping.expected"

# Marked Latin, Cyrillic and Hebrew text whose mark anchors carry
# VariationIndex deltas (`tests/anchor_variations.rs`).
ANCHOR_TEXTS = [
    # q + acute, x + diaeresis, k + circumflex.
    ("q\u0301x\u0308k\u0302", "ltr"),
    # zhe + acute, ef + diaeresis.
    ("\u0436\u0301\u0444\u0308", "ltr"),
    # bet + kamatz, shin + shin dot + kamatz, lamed, vav + holam, mem.
    ("\u05D1\u05B8\u05E9\u05C1\u05B8\u05DC\u05D5\u05B9\u05DD", "rtl"),
    # bereshit: dagesh, sheva, tsere, shin dot, hiriq.
    ("\u05D1\u05BC\u05B0\u05E8\u05B5\u05D0\u05E9\u05C1\u05B4\u05D9\u05EA", "rtl"),
]
ANCHOR_WEIGHTS = [300.0, 450.0, 600.0, 750.0, 900.0]

GROUPS = [
    ("anchors", ANCHOR_WEIGHTS, ANCHOR_TEXTS),
]


def num(v: float) -> str:
    return f"{v:.4f}".rstrip("0").rstrip(".")


def shape(face: hb.Face, wght: float, text: str, direction: str) -> str:
    font = hb.Font(face)
    font.set_variations({"wght": wght})
    buf = hb.Buffer()
    buf.add_str(text)
    buf.direction = direction
    buf.guess_segment_properties()
    hb.shape(font, buf, {})
    return " ".join(
        f"{i.codepoint},{p.x_advance},{p.y_advance},{p.x_offset},{p.y_offset}"
        for i, p in zip(buf.glyph_infos, buf.glyph_positions)
    )


def main() -> None:
    face = hb.Face(FONT_PATH.read_bytes())
    lines = [
        "# HarfBuzz " + hb.version_string() + " on rubik_vf.ttf;",
        "# regenerate with tests/tools/variable_shaping_expected.py.",
    ]
    for group, weights, texts in GROUPS:
        for wght in weights:
            for text, direction in texts:
                cps = ",".join(f"{ord(c):04X}" for c in text)
                glyphs = shape(face, wght, text, direction)
                lines.append(f"{group} {num(wght)} {direction} {cps} {glyphs}")
    OUT_PATH.write_bytes(("\n".join(lines) + "\n").encode())


if __name__ == "__main__":
    main()
