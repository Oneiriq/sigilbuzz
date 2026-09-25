#!/usr/bin/env python3
"""Synthesise an AAT-only font that exercises `kerx` subtable
format 4: control-point and anchor-point apply paths.

Two fonts are emitted side by side:

    aat_kerx_fmt4_type0.ttf: fmt 4 action type 0 (glyf control points)
    aat_kerx_fmt4_type1.ttf: fmt 4 action type 1 (ankr anchor points)

Both fonts ship six glyphs (.notdef, A, B, V, W, X) and a kerx v2
table whose single fmt-4 subtable carries a real state machine:
class 4 = "A", class 5 = "B"; the entry for (state 0, class 4) marks
"A" and goes to state 1; (state 1, class 5) fires action 0 and
returns to state 0. The action records are crafted so a known offset
falls out of `mark - current`:

    type 0:  A's contour point 1 sits at (500, 0); B's point 0 sits
             at (0, 0). Expected offset on B: (+500, 0).
    type 1:  `ankr` carries one anchor per real glyph. A's anchor 0
             at (500, 0); B's anchor 0 at (0, 0). Same expected
             offset.

The Rust integration test shapes "AB" through each font and asserts
glyph B's positioning offset matches.

Run:
    python3 tests/tools/build_aat_kerx_fmt4_apply_fixture.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib.tables.DefaultTable import DefaultTable

OUT_DIR = Path(__file__).resolve().parent.parent / "fixtures"

UPEM = 1000

GID_NOTDEF = 0
GID_A = 1
GID_B = 2
GID_V = 3
GID_W = 4
GID_X = 5

NUM_GLYPHS = 6


def build_rect(width: int, height: int = 700):
    pen = TTGlyphPen(None)
    # Four contour points, kept on-curve so glyf-point indices map
    # cleanly to the corners. The Rust shaper sees:
    #   point 0 = (0, 0)
    #   point 1 = (width, 0)
    #   point 2 = (width, height)
    #   point 3 = (0, height)
    pen.moveTo((0, 0))
    pen.lineTo((width, 0))
    pen.lineTo((width, height))
    pen.lineTo((0, height))
    pen.closePath()
    return pen.glyph()


def build_format4_state_machine(action_type: int, action_records: bytes) -> bytes:
    """Emits one kerx fmt-4 subtable bytes (incl. 12-B common header).

    Class lookup (format 6) maps GID_A -> class 4, GID_B -> class 5.
    State table:
        state 0 / class 4 -> entry 1 (mark, -> state 1)
        state 1 / class 5 -> entry 2 (action 0, -> state 0)
        state 1 / class 4 -> entry 3 (re-mark, stay in state 1)
        everything else  -> entry 0 (noop)
    """
    n_classes = 6
    n_states = 2
    n_entries = 4

    # AAT lookup format 6: sorted (glyph, value) pairs.
    pairs = sorted([(GID_A, 4), (GID_B, 5)])
    class_lookup = bytearray()
    class_lookup += struct.pack(">H", 6)  # format
    class_lookup += struct.pack(">H", 4)  # unitSize
    class_lookup += struct.pack(">H", len(pairs))  # nUnits
    class_lookup += b"\x00" * 6  # search hints
    for gid, val in pairs:
        class_lookup += struct.pack(">HH", gid, val)

    header_len = 20  # 16 B state-table header + 4 B flags
    class_off = header_len
    class_end = class_off + len(class_lookup)
    state_off = class_end + (class_end % 2)
    state_bytes = n_states * n_classes * 2
    entry_off = state_off + state_bytes
    entry_bytes = n_entries * 6
    action_off = entry_off + entry_bytes

    body = bytearray()
    body += struct.pack(">III", n_classes, class_off, state_off)
    body += struct.pack(">I", entry_off)
    flags = (action_type << 30) | action_off
    body += struct.pack(">I", flags)
    body += class_lookup
    while len(body) < state_off:
        body += b"\x00"
    # State 0: only class 4 (A) is interesting -> entry 1 (mark, ->s1).
    s0 = [0, 0, 0, 0, 1, 0]
    # State 1: class 5 (B) -> entry 2 (action), class 4 (A) -> entry 3
    # (re-mark, stay in s1).
    s1 = [0, 0, 0, 0, 3, 2]
    for v in s0 + s1:
        body += struct.pack(">H", v)

    MARK = 0x8000
    NO_ACTION = 0xFFFF
    entries = [
        (0, 0, NO_ACTION),       # 0: noop
        (1, MARK, NO_ACTION),    # 1: mark, -> state 1
        (0, 0, 0),               # 2: fire action 0, -> state 0
        (1, MARK, NO_ACTION),    # 3: re-mark, stay s1
    ]
    for ns, fl, ai in entries:
        body += struct.pack(">HHH", ns, fl, ai)
    body += action_records

    sub_len = 12 + len(body)
    sub = bytearray()
    sub += struct.pack(">III", sub_len, 0x00000004, 0)  # length, coverage=fmt4
    sub += body
    return bytes(sub)


def build_kerx_table(action_type: int, action_records: bytes) -> bytes:
    sub = build_format4_state_machine(action_type, action_records)
    return struct.pack(">HHI", 2, 0, 1) + sub


def build_ankr_table() -> bytes:
    """ankr v0 with one anchor per gid for A and B.
    A -> anchor 0 = (500, 0); B -> anchor 0 = (0, 0).
    """
    # Format-6 lookup: gid -> byte offset into anchor block.
    pairs = sorted([(GID_A, 0), (GID_B, 8)])
    lookup = bytearray()
    lookup += struct.pack(">H", 6)
    lookup += struct.pack(">H", 4)
    lookup += struct.pack(">H", len(pairs))
    lookup += b"\x00" * 6
    for gid, off in pairs:
        lookup += struct.pack(">HH", gid, off)

    header_len = 12
    lookup_off = header_len
    anchor_off = lookup_off + len(lookup)
    while anchor_off % 4 != 0:
        anchor_off += 1

    # Anchor block: A's record (n=1, x=500, y=0) at offset 0; B's
    # record (n=1, x=0, y=0) at offset 8.
    anchors = bytearray()
    anchors += struct.pack(">I", 1)  # nPoints
    anchors += struct.pack(">hh", 500, 0)
    anchors += struct.pack(">I", 1)
    anchors += struct.pack(">hh", 0, 0)

    out = bytearray()
    out += struct.pack(">HH", 0, 0)  # version, flags
    out += struct.pack(">II", lookup_off, anchor_off)
    out += lookup
    while len(out) < anchor_off:
        out += b"\x00"
    out += anchors
    return bytes(out)


def build_font(out_path: Path, action_type: int, action_records: bytes,
               extra_tables: dict) -> None:
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
        "familyName": f"SigilbuzzAATKerxFmt4Type{action_type}",
        "styleName": "Regular",
    })
    fb.setupOS2()
    fb.setupPost()

    kerx = DefaultTable("kerx")
    kerx.data = build_kerx_table(action_type, action_records)
    fb.font["kerx"] = kerx

    for tag, data in extra_tables.items():
        t = DefaultTable(tag)
        t.data = data
        fb.font[tag] = t

    out_path.parent.mkdir(parents=True, exist_ok=True)
    fb.save(str(out_path))
    print(f"Wrote {out_path} ({out_path.stat().st_size} bytes)")


def main():
    # Type 0: control points. A's contour point 1 sits at (500, 0)
    # (the rectangle's bottom-right corner). B's point 0 is at (0, 0).
    # Expected `mark - current` = (500, 0).
    type0_action = struct.pack(">HH", 1, 0)  # mark_point=1, current_point=0
    build_font(OUT_DIR / "aat_kerx_fmt4_type0.ttf", 0, type0_action, {})

    # Type 1: anchor points. ankr maps A->anchor 0 at (500, 0), B->
    # anchor 0 at (0, 0). The action record points at index 0 on each
    # side.
    type1_action = struct.pack(">HH", 0, 0)  # mark_anchor=0, current_anchor=0
    build_font(
        OUT_DIR / "aat_kerx_fmt4_type1.ttf",
        1,
        type1_action,
        {"ankr": build_ankr_table()},
    )


if __name__ == "__main__":
    main()
