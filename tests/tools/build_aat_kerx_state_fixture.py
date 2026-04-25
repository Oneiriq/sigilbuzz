#!/usr/bin/env python3
"""Synthesise an AAT-only font that exercises `kerx` subtable
format 1 — state-machine kerning.

Sister script to `build_aat_fixture.py` and
`build_aat_kerx_fmt2_fixture.py`. Single-purpose: the only kerning
in the font is an AAT state machine, so the test that consumes this
fixture is isolated from format 0 / 2 paths.

The font has six glyphs (`.notdef`, A, B, C, D, space) with cmap
entries for the five real letters plus space. The state machine
contextually kerns:

- (A, B) → -40 (only when A is the run start or follows a non-space)
- (C, D) → -25 (always, when the C is on the kern stack)

The state machine has four states so each kern rule fires only in
its specific context:

- State 0 ("idle"): A pushes and goes to state 1; C pushes and goes
  to state 2; space goes to state 3; everything else stays here.
- State 1 ("after A"): B pops the A and emits -40; A re-pushes;
  every other glyph returns to state 0 without consuming the stack.
- State 2 ("after C"): D pops the C and emits -26; A pushes and
  goes to state 1; everything else returns to state 0.
- State 3 ("after space"): A is silently dropped (no push) and
  returns to state 0 — that's the kern-suppression semantics. C
  still pushes (CD kerns even after a space).

Deliberately NO GSUB and NO GPOS so sigilbuzz's `kerx` fallback
runs.

Run:
    python3 tests/tools/build_aat_kerx_state_fixture.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUT_PATH = (
    Path(__file__).resolve().parent.parent / "fixtures" / "aat_kerx_state.ttf"
)

UPEM = 1000

GID_NOTDEF = 0
GID_A = 1
GID_B = 2
GID_C = 3
GID_D = 4
GID_SPACE = 5

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
# Hand-written kerx (version 2, format 1).
# ---------------------------------------------------------------------

# Reserved AAT classes 0..3.
CLASS_EOT = 0
CLASS_OOB = 1
CLASS_DEL = 2
CLASS_EOL = 3
# Real classes start at 4.
CLASS_A = 4
CLASS_B = 5
CLASS_C = 6
CLASS_D = 7
CLASS_SP = 8

N_CLASSES = 9
N_STATES = 4

# Flags per Apple kerx format 1 spec.
FLAG_PUSH = 0x8000
FLAG_DONT_ADVANCE = 0x4000
FLAG_RESET = 0x2000

VALUE_INDEX_NONE = 0xFFFF


def build_lookup_format6(pairs):
    """AAT lookup format 6: sorted (glyph, value) records."""
    pairs = sorted(pairs, key=lambda p: p[0])
    out = bytearray()
    out += struct.pack(">H", 6)        # format
    out += struct.pack(">H", 4)        # unitSize
    out += struct.pack(">H", len(pairs))
    out += b"\x00" * 6                  # search hints (zeros are fine)
    for g, v in pairs:
        out += struct.pack(">HH", g, v)
    return bytes(out)


def build_kerx_table() -> bytes:
    # Class lookup: each real glyph → its class. Anything else falls
    # to the reserved out-of-bounds class (handled by the lookup
    # primitive, returns class 1).
    class_lookup = build_lookup_format6([
        (GID_A, CLASS_A),
        (GID_B, CLASS_B),
        (GID_C, CLASS_C),
        (GID_D, CLASS_D),
        (GID_SPACE, CLASS_SP),
    ])

    # State array (N_STATES rows × N_CLASSES u16 entry indices).
    #
    # Entries:
    #   #0  noop, → state 0
    #   #1  PUSH (A),  → state 1   (so we know an A is on top)
    #   #2  PUSH (C),  → state 2   (so we know a C is on top)
    #   #3  apply -40, → state 0   (B after A)
    #   #4  apply -26, → state 0   (D after C)
    #   #5  noop,      → state 3   (saw a space)
    #   #6  noop,      → state 1   (saw an A — used from state 2 to switch contexts cleanly)
    #
    # State 0 — idle.
    state0 = [0] * N_CLASSES
    state0[CLASS_A] = 1
    state0[CLASS_C] = 2
    state0[CLASS_SP] = 5
    # State 1 — after A pushed.
    state1 = [0] * N_CLASSES
    state1[CLASS_A] = 1   # re-push
    state1[CLASS_B] = 3   # AB kern fires
    state1[CLASS_C] = 2   # rotate to "after C"
    state1[CLASS_SP] = 5
    # State 2 — after C pushed.
    state2 = [0] * N_CLASSES
    state2[CLASS_A] = 6   # switch to "after A" context (re-push happens via entry 6)
    state2[CLASS_C] = 2   # re-push C
    state2[CLASS_D] = 4   # CD kern fires
    state2[CLASS_SP] = 5
    # State 3 — after space.
    state3 = [0] * N_CLASSES
    state3[CLASS_A] = 0   # NO push — suppresses next AB kern
    state3[CLASS_C] = 2   # CD pair still kerns even after a space
    state3[CLASS_SP] = 5  # stay in "after space"

    # Entries: (newState, flags, valueIndex). 6 bytes each.
    entries = [
        (0, 0,         VALUE_INDEX_NONE),  # #0 noop
        (1, FLAG_PUSH, VALUE_INDEX_NONE),  # #1 PUSH A → state 1
        (2, FLAG_PUSH, VALUE_INDEX_NONE),  # #2 PUSH C → state 2
        (0, 0,         0),                 # #3 apply -40 → state 0
        (0, 0,         2),                 # #4 apply -26 → state 0
        (3, 0,         VALUE_INDEX_NONE),  # #5 → state 3
        (1, FLAG_PUSH, VALUE_INDEX_NONE),  # #6 PUSH A → state 1 (rotate)
    ]

    # Value table: two i16 values, each terminator-marked.
    # -40 → 0xFFD8 (even). With bit 0 set: 0xFFD9 (still parses to -40).
    # -25 → 0xFFE7 (odd already — has bit 0 set as a side effect).
    #     Read back: raw=-25 (i16), bit 0 set → terminator; masked
    #     value = -25 & ~1 = -26. To preserve -25 exactly we'd want to
    #     pre-bias by 1 before encoding; but the AAT convention is the
    #     value list always carries even kern deltas (the spec
    #     reserves bit 0 as a flag), so we round to even: encode -26
    #     instead and the test asserts -26.
    # We'll use -40 (which is naturally even) and -26 (we adjust the
    # human-readable spec from -25 to -26 to honour AAT's even-only
    # encoding). The shaper masks bit 0, so what's in the file is the
    # ground truth.
    raw_minus_40 = (-40 & 0xFFFF) | 1   # 0xFFD9
    raw_minus_26 = (-26 & 0xFFFF) | 1   # 0xFFE7

    # Body layout (offsets from subtable body start = byte 12 of full
    # subtable):
    #   0..16   state-table header
    #  16..20   valueTableOffset (u32)
    #  20..     class lookup (aligned to 2)
    #   ..      state array
    #   ..      entry array
    #   ..      value table
    header_len = 20
    class_off = header_len
    class_end = class_off + len(class_lookup)
    # Align state array to 2 bytes.
    state_off = class_end + (class_end % 2)
    state_bytes = N_STATES * N_CLASSES * 2
    entry_off = state_off + state_bytes
    entry_bytes = len(entries) * 6
    value_off = entry_off + entry_bytes
    value_bytes = 4  # two i16

    body_len = value_off + value_bytes

    body = bytearray()
    # State-table header.
    body += struct.pack(">IIIII",
                        N_CLASSES,
                        class_off,
                        state_off,
                        entry_off,
                        value_off)
    body += class_lookup
    while len(body) < state_off:
        body += b"\x00"
    for v in state0 + state1 + state2 + state3:
        body += struct.pack(">H", v)
    for ns, fl, vi in entries:
        body += struct.pack(">HHH", ns, fl, vi)
    body += struct.pack(">H", raw_minus_40)
    body += struct.pack(">H", raw_minus_26)
    assert len(body) == body_len, (len(body), body_len)

    # Wrap in the 12-byte common subtable header.
    sub_len = 12 + len(body)
    sub = bytearray()
    sub += struct.pack(">III", sub_len, 0x00000001, 0)  # length, coverage=fmt1, tupleCount=0
    sub += body

    # kerx table header: u16 version, u16 _pad, u32 nTables.
    table = struct.pack(">HHI", 2, 0, 1) + bytes(sub)
    return table


# ---------------------------------------------------------------------
# Assembly.
# ---------------------------------------------------------------------

def main():
    glyph_order = [".notdef", "A", "B", "C", "D", "space"]
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(glyph_order)
    fb.setupCharacterMap({
        ord("A"): "A",
        ord("B"): "B",
        ord("C"): "C",
        ord("D"): "D",
        ord(" "): "space",
    })
    glyphs = {
        ".notdef": build_rect(400),
        "A": build_rect(500),
        "B": build_rect(500),
        "C": build_rect(500),
        "D": build_rect(500),
        "space": build_rect(250),
    }
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({
        ".notdef": (400, 0),
        "A": (500, 0),
        "B": (500, 0),
        "C": (500, 0),
        "D": (500, 0),
        "space": (250, 0),
    })
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({
        "familyName": "SigilbuzzAATKerxState",
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
