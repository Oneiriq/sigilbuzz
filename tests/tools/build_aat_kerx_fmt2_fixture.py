#!/usr/bin/env python3
"""Synthesise an AAT-only font that exercises `kerx` subtable
format 2 — compound-class (n-way) kerning.

Sister script to `build_aat_fixture.py`. The two outputs split
duties so each test fixture is small and single-purpose:

- `aat_synthetic.ttf` covers `morx` ligation and `kerx` format 0.
- `aat_kerx_fmt2.ttf` covers `kerx` format 2 only.

The font has six glyphs (`.notdef`, A, B, V, W, X) with cmap
entries for the four real letters (X is unmapped — it just sits
in the glyph order so the per-glyph format-0 lookup tables stay
dense). Hand-authored kerx layout:

- One subtable, format 2.
- Left class table (AAT lookup format 0):
    A=1, B=1, everything else=0.
- Right class table (AAT lookup format 0):
    V=1, W=2, everything else=0.
- Kerning array (2 left classes × 3 right classes, rowWidth = 6):
    row 0 (left class 0): [0,    0,    0]
    row 1 (left class 1): [0,  -30,  -50]

So shaping:

- "AV" → -30 (A is left class 1, V is right class 1)
- "BV" → -30
- "AW" → -50 (A is left class 1, W is right class 2)
- "BW" → -50
- "VA" →   0 (V is left class 0, A is right class 0 — default)
- "AA" →   0 (A is left class 1, A is right class 0)

Deliberately NO GSUB and NO GPOS so sigilbuzz's `kerx` fallback
runs.

Run:
    python3 tests/tools/build_aat_kerx_fmt2_fixture.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUT_PATH = (
    Path(__file__).resolve().parent.parent / "fixtures" / "aat_kerx_fmt2.ttf"
)

UPEM = 1000

GID_NOTDEF = 0
GID_A = 1
GID_B = 2
GID_V = 3
GID_W = 4
GID_X = 5  # unmapped — keeps the format-0 array's last cell exercised

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
# Hand-written kerx (version 2, format 2).
# ---------------------------------------------------------------------

def lookup_format0(values):
    """AAT lookup format 0: u16 format + u16-per-glyph values."""
    body = bytearray()
    body += struct.pack(">H", 0)
    for v in values:
        body += struct.pack(">H", v)
    return bytes(body)


def build_kerx_table() -> bytes:
    n_left_classes = 2
    n_right_classes = 3
    row_width = n_right_classes * 2  # bytes (i16 cells)

    # Left class table: each cell is class * row_width.
    left_class_of_gid = [0, 1, 1, 0, 0, 0]  # gid → class
    left_values = [c * row_width for c in left_class_of_gid]
    assert all(v < 0x10000 for v in left_values)

    # Right class table: each cell is class * 2.
    right_class_of_gid = [0, 0, 0, 1, 2, 0]  # gid → class
    right_values = [c * 2 for c in right_class_of_gid]

    left_lookup = lookup_format0(left_values)
    right_lookup = lookup_format0(right_values)

    # Kerning matrix [leftClass][rightClass] → i16.
    matrix = [
        [0,   0,   0],
        [0, -30, -50],
    ]

    # Layout, all offsets relative to the subtable start (i.e. the
    # 12-byte common header is at offset 0):
    #   0  : 12 B common header
    #   12 : 16 B fmt2 header (rowWidth, leftOff, rightOff, arrayOff)
    #   28 : left lookup
    #   .. : right lookup
    #   .. : kerning array (n_left_classes rows × row_width bytes)
    fmt2_header_off = 12
    left_off = fmt2_header_off + 16
    right_off = left_off + len(left_lookup)
    array_off = right_off + len(right_lookup)
    array_bytes = n_left_classes * row_width
    sub_len = array_off + array_bytes

    body = bytearray()
    # Common subtable header.
    body += struct.pack(">III", sub_len, 0x00000002, 0)  # length, coverage=fmt2, tupleCount=0
    # fmt2 header.
    body += struct.pack(
        ">IIII",
        row_width,
        left_off,
        right_off,
        array_off,
    )
    body += left_lookup
    body += right_lookup
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
        "familyName": "SigilbuzzAATKerxFmt2",
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
