#!/usr/bin/env python3
"""Synthesise an AAT-only font that exercises `morx` subtable
type 5: insertion substitution.

Type 5 walks the glyph stream through a state machine; on an
insertion entry it splices up to 31 glyphs from an "insertion glyph
table" into the run, either before or after the current (or marked)
glyph. The classic real-world use case: AAT Hebrew / Arabic fonts
that inject cantillation marks based on context.

This fixture's state machine inserts a "mark" glyph (gid 5) after
every "trigger" glyph (A = gid 1) it sees. The rest of the run
passes through unchanged.

Glyphs:
  gid 0 .notdef
  gid 1 A         (trigger)
  gid 2 B
  gid 3 C
  gid 4 D
  gid 5 mark      (the inserted glyph)

State machine (1 state, 5 classes):
  class 4 = trigger (A)
  state 0:
    class 4 -> entry 1: insert one glyph (gid 5) AFTER current.
    other  -> entry 0: noop.

Insertion glyph table is a flat `[mark_gid]` u16 array.

Deliberately NO GSUB so sigilbuzz's `morx` fallback runs.

Run:
    python3 tests/tools/build_aat_morx_insertion_fixture.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUT_PATH = (
    Path(__file__).resolve().parent.parent / "fixtures" / "aat_morx_insertion.ttf"
)

UPEM = 1000

GID_NOTDEF = 0
GID_A = 1
GID_B = 2
GID_C = 3
GID_D = 4
GID_MARK = 5

NUM_GLYPHS = 6


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


# Reserved AAT classes 0..3.
N_CLASSES = 5  # +1 for the trigger glyph
N_STATES = 1
ENTRY_SIZE = 8

CURRENT_COUNT_SHIFT = 5  # bits 5..9 hold the current-insert count


def build_morx_table() -> bytes:
    class_lookup = build_lookup_format6([(GID_A, 4)])

    # Body layout:
    #   0..16   state-table header
    #  16..20   insertionGlyphTable offset (u32)
    #  20..     class lookup (aligned to 2)
    #  ..       state array (1 x 5 x u16) = 10 B
    #  ..       entry array (2 x 8 B) = 16 B
    #  ..       insertion glyph table (1 x u16) = 2 B
    header_len = 20
    class_off = header_len
    class_end = class_off + len(class_lookup)
    state_off = class_end + (class_end % 2)
    state_bytes = N_STATES * N_CLASSES * 2
    entry_off = state_off + state_bytes
    entry_bytes = 2 * ENTRY_SIZE
    ins_off = entry_off + entry_bytes
    ins_bytes = 2

    body_len = ins_off + ins_bytes

    body = bytearray()
    body += struct.pack(
        ">IIIII",
        N_CLASSES,
        class_off,
        state_off,
        entry_off,
        ins_off,
    )
    body += class_lookup
    while len(body) < state_off:
        body += b"\x00"
    # State row: class 4 -> entry 1, else entry 0.
    s0 = [0, 0, 0, 0, 1]
    for v in s0:
        body += struct.pack(">H", v)
    # Entry 0: noop. (newState, flags, currentInsertIndex, markedInsertIndex)
    body += struct.pack(">HHHH", 0, 0, 0xFFFF, 0xFFFF)
    # Entry 1: insert 1 glyph from index 0 AFTER current.
    flags = 1 << CURRENT_COUNT_SHIFT  # currentInsertCount = 1, no before-flag
    body += struct.pack(">HHHH", 0, flags, 0, 0xFFFF)
    # Insertion glyph table: one u16 = mark gid.
    body += struct.pack(">H", GID_MARK)
    assert len(body) == body_len, (len(body), body_len)

    sub_len = 12 + len(body)
    sub = bytearray()
    sub += struct.pack(">III", sub_len, 0x00000005, 0x00000001)
    sub += body

    chain_len = 16 + len(sub)
    chain = bytearray()
    chain += struct.pack(">IIII", 0x00000001, chain_len, 0, 1)
    chain += sub

    return struct.pack(">HHI", 2, 0, 1) + bytes(chain)


def main():
    glyph_order = [".notdef", "A", "B", "C", "D", "mark"]
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(glyph_order)
    fb.setupCharacterMap({
        ord("A"): "A",
        ord("B"): "B",
        ord("C"): "C",
        ord("D"): "D",
    })
    glyphs = {
        ".notdef": build_rect(400),
        "A": build_rect(500),
        "B": build_rect(500),
        "C": build_rect(500),
        "D": build_rect(500),
        "mark": build_rect(200),
    }
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({
        ".notdef": (400, 0),
        "A": (500, 0),
        "B": (500, 0),
        "C": (500, 0),
        "D": (500, 0),
        "mark": (200, 0),
    })
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({
        "familyName": "SigilbuzzAATMorxInsertion",
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
