#!/usr/bin/env python3
"""Synthesise an AAT-only font that exercises `kerx` subtable
format 4 — control-point kerning (parse-only coverage).

Format 4 needs glyf-point or ankr coordinate reads to produce real
offsets; sigilbuzz's apply path is a stub for now (see the kerx
module docs). This fixture's role is structural: a font that
ships a parseable format-4 subtable AND a format-0 subtable with one
pair, so the integration test can prove both:

  1. The format-4 subtable doesn't trip the parser.
  2. The format-4 subtable doesn't drop its surrounding format-0
     pair on the floor — both subtables stay in the parsed kerx.

The font has six glyphs (`.notdef`, A, B, V, W, X) with cmap entries
for the four real letters.

Run:
    python3 tests/tools/build_aat_kerx_fmt4_fixture.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUT_PATH = (
    Path(__file__).resolve().parent.parent / "fixtures" / "aat_kerx_fmt4.ttf"
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


def build_format4_subtable() -> bytes:
    """One format-4 subtable. State machine has one state and four
    classes (the AAT-reserved minimum); everything points at a single
    no-op entry. The flags word selects action type 2 (coordinates)
    and points at a single 8-byte action record of zeros — enough to
    exercise the parser without driving any offset.
    """
    n_classes = 4
    header_len = 20

    # Empty format-6 class lookup.
    class_lookup = bytearray()
    class_lookup += struct.pack(">H", 6)  # format
    class_lookup += struct.pack(">H", 4)  # unitSize
    class_lookup += struct.pack(">H", 0)  # nUnits
    class_lookup += b"\x00" * 6           # search hints

    class_off = header_len
    class_end = class_off + len(class_lookup)
    state_off = class_end + (class_end % 2)
    state_bytes = n_classes * 1 * 2
    entry_off = state_off + state_bytes
    entry_bytes = 6
    action_off = entry_off + entry_bytes
    action_bytes = 8
    body_len = action_off + action_bytes

    body = bytearray()
    body += struct.pack(">III", n_classes, class_off, state_off)
    body += struct.pack(">I", entry_off)
    # Flags: action type 2 in bits 30-31, action_off in low 30.
    flags = (2 << 30) | action_off
    body += struct.pack(">I", flags)
    body += class_lookup
    while len(body) < state_off:
        body += b"\x00"
    # State row (1 state × 4 classes).
    for _ in range(n_classes):
        body += struct.pack(">H", 0)
    # Entry 0: noop.
    body += struct.pack(">HHH", 0, 0, 0)
    # Action record.
    body += b"\x00" * action_bytes

    assert len(body) == body_len, (len(body), body_len)

    sub_len = 12 + len(body)
    sub = bytearray()
    sub += struct.pack(">III", sub_len, 0x00000004, 0)  # length, coverage=fmt4
    sub += body
    return bytes(sub)


def build_format0_subtable() -> bytes:
    """One format-0 subtable with a single pair (A, V) → -42 so the
    integration test can prove the format-4 neighbour didn't poison
    the rest of the kerx."""
    pairs = [(GID_A, GID_V, -42)]
    n_pairs = len(pairs)
    body_len = 16 + n_pairs * 6
    sub_len = 12 + body_len

    body = bytearray()
    body += struct.pack(">III", sub_len, 0x00000000, 0)  # length, coverage=fmt0
    body += struct.pack(">III", n_pairs, 0, 0)
    body += struct.pack(">I", 0)  # rangeShift placeholder
    for l, r, v in pairs:
        body += struct.pack(">HHh", l, r, v)
    return bytes(body)


def build_kerx_table() -> bytes:
    f4 = build_format4_subtable()
    f0 = build_format0_subtable()
    table = struct.pack(">HHI", 2, 0, 2) + f4 + f0
    return table


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
        "familyName": "SigilbuzzAATKerxFmt4",
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
