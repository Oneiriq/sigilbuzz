#!/usr/bin/env python3
"""Synthesise an AAT-only font that exercises `morx` subtable
type 4: non-contextual substitution.

Type 4 is the simplest morx subtable: just an AAT lookup table
mapping gid -> gid, applied unconditionally to every glyph in the
run. Real-world use case: small-caps subsidiary glyphs swapped in
when the user enables a small-cap feature.

This fixture's mapping: A -> A.smcp (gid 5). Other glyphs pass
through. The font also has B and a separate `B.smcp` slot so the
test can prove only the mapped glyph swaps.

Glyphs:
  gid 0 .notdef
  gid 1 A
  gid 2 B
  gid 3 A.smcp
  gid 4 B.smcp

Mapping (type 4 lookup): A=1 -> 3 (A.smcp).
B is *not* mapped, so it stays gid 2 even though B.smcp exists.

Deliberately NO GSUB so sigilbuzz's `morx` fallback runs.

Run:
    python3 tests/tools/build_aat_morx_noncontext_fixture.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUT_PATH = (
    Path(__file__).resolve().parent.parent / "fixtures" / "aat_morx_noncontext.ttf"
)

UPEM = 1000


def build_rect(width: int):
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((width, 0))
    pen.lineTo((width, 700))
    pen.lineTo((0, 700))
    pen.closePath()
    return pen.glyph()


def build_lookup_format6(pairs):
    pairs = sorted(pairs, key=lambda p: p[0])
    out = bytearray()
    out += struct.pack(">H", 6)
    out += struct.pack(">H", 4)
    out += struct.pack(">H", len(pairs))
    out += b"\x00" * 6
    for g, v in pairs:
        out += struct.pack(">HH", g, v)
    return bytes(out)


def build_morx_table() -> bytes:
    # Type 4 body = the AAT lookup table itself.
    body = build_lookup_format6([(1, 3)])  # gid 1 (A) -> gid 3 (A.smcp)

    sub_len = 12 + len(body)
    sub = bytearray()
    sub += struct.pack(">III", sub_len, 0x00000004, 0x00000001)
    sub += body

    chain_len = 16 + len(sub)
    chain = bytearray()
    chain += struct.pack(">IIII", 0x00000001, chain_len, 0, 1)
    chain += sub

    return struct.pack(">HHI", 2, 0, 1) + bytes(chain)


def main():
    glyph_order = [".notdef", "A", "B", "A.smcp", "B.smcp"]
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(glyph_order)
    fb.setupCharacterMap({
        ord("A"): "A",
        ord("B"): "B",
    })
    glyphs = {
        ".notdef": build_rect(400),
        "A": build_rect(500),
        "B": build_rect(500),
        "A.smcp": build_rect(400),
        "B.smcp": build_rect(400),
    }
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({
        ".notdef": (400, 0),
        "A": (500, 0),
        "B": (500, 0),
        "A.smcp": (400, 0),
        "B.smcp": (400, 0),
    })
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({
        "familyName": "SigilbuzzAATMorxNonContext",
        "styleName": "Regular",
    })
    fb.setupOS2()
    fb.setupPost()

    from fontTools.ttLib.tables.DefaultTable import DefaultTable

    morx_table = DefaultTable("morx")
    morx_table.data = build_morx_table()
    fb.font["morx"] = morx_table

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    fb.save(str(OUT_PATH))
    size = OUT_PATH.stat().st_size
    print(f"Wrote {OUT_PATH} ({size} bytes)")


if __name__ == "__main__":
    main()
