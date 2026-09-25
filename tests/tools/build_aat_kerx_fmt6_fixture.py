#!/usr/bin/env python3
"""Synthesise an AAT-only font that exercises `kerx` subtable
format 6: simple n x m kerning array.

Sister script to `build_aat_kerx_fmt2_fixture.py`. Format 6 mirrors
format 2's compound-class layout but the row / column lookup tables
yield direct row / column *indices* into a 2D grid (rather than
pre-multiplied byte offsets), and the array is sized by
`rowCount × columnCount`.

The font has six glyphs (`.notdef`, A, B, V, W, X) with cmap entries
for the four real letters. Hand-authored kerx layout:

- One subtable, format 6, no long-values flag.
- Row index table (AAT lookup format 0):
    A=1, B=1, everything else=0.
- Column index table (AAT lookup format 0):
    V=1, W=2, everything else=0.
- Kerning array (rowCount=2 x columnCount=3):
    row 0: [0,    0,    0]
    row 1: [0,  -30,  -50]

So shaping:

- "AV" -> -30 (A is row 1, V is col 1)
- "BV" -> -30
- "AW" -> -50 (A is row 1, W is col 2)
- "BW" -> -50
- "VA" ->   0 (V is row 0, A is col 0, default)

Deliberately NO GSUB and NO GPOS so sigilbuzz's `kerx` fallback
runs.

Run:
    python3 tests/tools/build_aat_kerx_fmt6_fixture.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUT_PATH = (
    Path(__file__).resolve().parent.parent / "fixtures" / "aat_kerx_fmt6.ttf"
)

UPEM = 1000

GID_NOTDEF = 0
GID_A = 1
GID_B = 2
GID_V = 3
GID_W = 4
GID_X = 5

NUM_GLYPHS = 6


def build_rect(width: int):
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((width, 0))
    pen.lineTo((width, 700))
    pen.lineTo((0, 700))
    pen.closePath()
    return pen.glyph()


# ---------------------------------------------------------------------
# Hand-written kerx (version 2, format 6).
# ---------------------------------------------------------------------

def lookup_format0(values):
    """AAT lookup format 0: u16 format + u16-per-glyph values."""
    body = bytearray()
    body += struct.pack(">H", 0)
    for v in values:
        body += struct.pack(">H", v)
    return bytes(body)


def build_kerx_table() -> bytes:
    row_count = 2
    column_count = 3

    # Row index table: gid -> row index.
    row_index_of_gid = [0, 1, 1, 0, 0, 0]
    # Column index table: gid -> column index.
    col_index_of_gid = [0, 0, 0, 1, 2, 0]

    row_lookup = lookup_format0(row_index_of_gid)
    col_lookup = lookup_format0(col_index_of_gid)

    # Kerning matrix [row][col] -> i16.
    matrix = [
        [0,   0,   0],
        [0, -30, -50],
    ]

    # Layout, all offsets relative to the subtable start (the 12-byte
    # common header sits at offset 0):
    #   0  : 12 B common header
    #   12 : 20 B fmt6 header (flags, rowCount, columnCount,
    #                          rowIndexOff, colIndexOff, arrayOff)
    #   32 : row lookup
    #   .. : col lookup
    #   .. : kerning array (row_count x column_count i16s)
    fmt6_header_off = 12
    row_off = fmt6_header_off + 20
    col_off = row_off + len(row_lookup)
    array_off = col_off + len(col_lookup)
    array_bytes = row_count * column_count * 2
    sub_len = array_off + array_bytes

    body = bytearray()
    # Common subtable header.
    body += struct.pack(">III", sub_len, 0x00000006, 0)  # length, coverage=fmt6, tupleCount=0
    # fmt6 header.
    body += struct.pack(
        ">IHHIII",
        0,                # flags (no long values)
        row_count,
        column_count,
        row_off,
        col_off,
        array_off,
    )
    body += row_lookup
    body += col_lookup
    for row in matrix:
        for v in row:
            body += struct.pack(">h", v)
    assert len(body) == sub_len, (len(body), sub_len)

    # kerx table header: u16 version, u16 _pad, u32 nTables.
    table = struct.pack(">HHI", 2, 0, 1) + bytes(body)
    return table


# ---------------------------------------------------------------------
# Assembly.
# ---------------------------------------------------------------------

def main():
    glyph_order = [".notdef", "A", "B", "V", "W", "X"]
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(glyph_order)
    fb.setupCharacterMap({
        ord("A"): "A",
        ord("B"): "B",
        ord("V"): "V",
        ord("W"): "W",
    })
    glyphs = {
        ".notdef": build_rect(400),
        "A": build_rect(500),
        "B": build_rect(500),
        "V": build_rect(500),
        "W": build_rect(500),
        "X": build_rect(500),
    }
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({
        ".notdef": (400, 0),
        "A": (500, 0),
        "B": (500, 0),
        "V": (500, 0),
        "W": (500, 0),
        "X": (500, 0),
    })
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({
        "familyName": "SigilbuzzAATKerxFmt6",
        "styleName": "Regular",
    })
    fb.setupOS2()
    fb.setupPost()

    from fontTools.ttLib.tables.DefaultTable import DefaultTable

    kerx_table = DefaultTable("kerx")
    kerx_table.data = build_kerx_table()
    fb.font["kerx"] = kerx_table

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    fb.save(str(OUT_PATH))
    size = OUT_PATH.stat().st_size
    print(f"Wrote {OUT_PATH} ({size} bytes)")


if __name__ == "__main__":
    main()
