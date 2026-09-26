#!/usr/bin/env python3
"""Synthesise a tiny AAT-only font with hand-written `morx` and
`kerx` tables.

fontTools does not build `morx` / `kerx` from XML today, so we
assemble the AAT tables as raw big-endian bytes and splice them
into the SFNT directory that fontTools produces for the outline /
cmap / hmtx scaffolding.

The output font `aat_synthetic.ttf` carries:

- Six glyphs (`.notdef`, `f`, `i`, `fi`, `A`, `V`), each a tiny
  rectangle outline so glyf stays under ~200 bytes.
- A cmap mapping U+0066 -> f, U+0069 -> i, U+0041 -> A, U+0056 -> V.
  The ligature `fi` glyph is only reachable via the morx substitution,
  so it is absent from the cmap.
- A `morx` version-2 table with one chain and one type-2 (ligature)
  subtable: (f, i) -> fi.
- A `kerx` version-2 table with one format-0 subtable: (A, V) -> -50.
- Deliberately NO GSUB and NO GPOS, so sigilbuzz exercises the AAT
  fallback path.

The fixture is ~2.8 KB, small enough to check into the repo.

Run:
    python3 tests/tools/build_aat_fixture.py
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont, newTable

OUT_PATH = Path(__file__).resolve().parent.parent / "fixtures" / "aat_synthetic.ttf"

UPEM = 1000

# Glyph indices in the final font. We control glyph order so the
# morx action table offsets work out deterministically.
GID_NOTDEF = 0
GID_F = 1
GID_I = 2
GID_FI = 3
GID_A = 4
GID_V = 5

LIG_F_I = -50  # kerx (A, V)


def build_rect(width: int):
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((width, 0))
    pen.lineTo((width, 700))
    pen.lineTo((0, 700))
    pen.closePath()
    return pen.glyph()


# ---------------------------------------------------------------------
# Hand-written morx (version 2, one chain, one type-2 subtable).
# ---------------------------------------------------------------------

# AAT lookup format 6: (glyph, value) sorted pairs. Values are
# per-glyph classes. Classes 0..3 are reserved; classes 4..5 are
# the f / i slots the state table branches on.
def lookup_format6(pairs):
    body = bytearray()
    body += struct.pack(">HHHHHH", 6, 4, len(pairs), 0, 0, 0)
    for gid, value in pairs:
        body += struct.pack(">HH", gid, value)
    return bytes(body)


def build_morx_body() -> bytes:
    """Returns a morx subtable body (type 2, ligature substitution)
    that maps (f, i) -> fi_lig.

    Layout, relative to the subtable body start:

      0  : state-table header (16 B)
      16 : ligActionOffset, componentOffset, ligatureOffset (12 B)
      28 : class subtable (format 6)
      .. : state array
      .. : entry array
      .. : ligAction (2 x u32)
      .. : components (u16 table, indexed by glyph id + signed offset)
      .. : ligatures  (u16 table, indexed by accumulated offset)
    """

    # Classes: 0=EOT, 1=OOB, 2=Deleted, 3=EOL, 4=f, 5=i.
    classes = lookup_format6([(GID_F, 4), (GID_I, 5)])

    # Body placeholder (28 B) then class table starts.
    body = bytearray(28)
    class_off = len(body)
    body += classes

    # Pad to even for u16 arrays.
    if len(body) & 1:
        body += b"\x00"
    state_off = len(body)

    # 2 states x 6 classes x u16.
    n_classes = 6
    n_states = 2
    state_array = [0] * (n_states * n_classes)
    # State 0: class 4 (f) -> entry 1.
    state_array[0 * n_classes + 4] = 1
    # State 1: class 5 (i) -> entry 2. Any other class falls back to
    # entry 0 (noop). We pre-initialized with zeros.
    state_array[1 * n_classes + 5] = 2
    for v in state_array:
        body += struct.pack(">H", v)

    # Entries (6 B each): newState, flags, actionIndex.
    entry_off = len(body)
    # #0 noop
    body += struct.pack(">HHH", 0, 0x0000, 0)
    # #1 SetComponent, newState=1
    body += struct.pack(">HHH", 1, 0x8000, 0)
    # #2 SetComponent | PerformAction, newState=0, actionIdx=0
    body += struct.pack(">HHH", 0, 0xA000, 0)

    # LigAction (2 x u32). Walked in reverse pop order, so:
    #   action[0] corresponds to i_gid (last pushed)
    #   action[1] corresponds to f_gid (first pushed, carries LAST|STORE)
    # Offsets are chosen so the sum into the ligature index is 0,
    # i.e. ligatures[0] = fi_lig.
    LAST = 1 << 31
    STORE = 1 << 30
    SIGN = 1 << 29
    MASK = 0x3FFFFFFF

    def action(last: bool, store: bool, signed_off: int) -> bytes:
        raw = (signed_off & 0xFFFFFFFF) & MASK
        flag = 0
        if last:
            flag |= LAST
        if store:
            flag |= STORE
        if signed_off < 0:
            flag |= SIGN
        return struct.pack(">I", flag | raw)

    lig_action_off = len(body)
    body += action(False, False, -GID_I)  # i: + comp[0] = 0
    body += action(True, True, -GID_F)    # f: + comp[0] = 0 -> ligatures[0]

    # Components: index = glyph + signed offset = 0 for both paths.
    # So components[0] = 0 suffices. Padded so any in-range glyph id
    # read stays inside the slice.
    comp_off = len(body)
    comp_table_len = max(GID_F, GID_I) + 1
    body += b"\x00\x00" * comp_table_len

    # Ligatures: one entry at index 0.
    lig_off = len(body)
    body += struct.pack(">H", GID_FI)

    # Patch header: nClasses, classOff, stateOff, entryOff,
    # ligActionOff, componentOff, ligatureOff.
    struct.pack_into(">IIII", body, 0,
                     n_classes, class_off, state_off, entry_off)
    struct.pack_into(">III", body, 16,
                     lig_action_off, comp_off, lig_off)

    return bytes(body)


def build_morx_table() -> bytes:
    body = build_morx_body()
    sub_len = 12 + len(body)
    subtable = struct.pack(">III", sub_len, 0x00000002, 0x00000001) + body
    chain_len = 16 + len(subtable)
    # defaultFlags=1 so the subtable (subFeatureFlags=1) participates.
    chain = struct.pack(">IIII", 0x00000001, chain_len, 0, 1) + subtable
    # Pad table header: u16 version, u16 _pad, u32 nChains.
    table = struct.pack(">HHI", 2, 0, 1) + chain
    return table


# ---------------------------------------------------------------------
# Hand-written kerx (version 2, format 0).
# ---------------------------------------------------------------------

def build_kerx_table() -> bytes:
    pairs = [(GID_A, GID_V, -50)]
    body = struct.pack(">IIII", len(pairs), 0, 0, 0)
    for left, right, value in pairs:
        body += struct.pack(">HHh", left, right, value)
    sub_len = 12 + len(body)
    subtable = struct.pack(">III", sub_len, 0x00000000, 0) + body
    return struct.pack(">HHI", 2, 0, 1) + subtable


# ---------------------------------------------------------------------
# Assembly.
# ---------------------------------------------------------------------

def main():
    glyph_order = [".notdef", "f", "i", "fi", "A", "V"]
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(glyph_order)
    fb.setupCharacterMap({
        ord("f"): "f",
        ord("i"): "i",
        ord("A"): "A",
        ord("V"): "V",
    })
    glyphs = {
        ".notdef": build_rect(400),
        "f":       build_rect(500),
        "i":       build_rect(300),
        "fi":      build_rect(800),
        "A":       build_rect(500),
        "V":       build_rect(500),
    }
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({
        ".notdef": (400, 0),
        "f":       (500, 0),
        "i":       (300, 0),
        "fi":      (800, 0),
        "A":       (500, 0),
        "V":       (500, 0),
    })
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "SigilbuzzAATSynthetic", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()

    # Splice in the raw AAT tables. fontTools does not know how to
    # round-trip morx / kerx, so we attach them as DefaultTable which
    # copies their raw bytes into the SFNT directly.
    from fontTools.ttLib.tables.DefaultTable import DefaultTable

    morx_table = DefaultTable("morx")
    morx_table.data = build_morx_table()
    fb.font["morx"] = morx_table

    kerx_table = DefaultTable("kerx")
    kerx_table.data = build_kerx_table()
    fb.font["kerx"] = kerx_table

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    fb.save(str(OUT_PATH))
    size = OUT_PATH.stat().st_size
    print(f"Wrote {OUT_PATH} ({size} bytes)")


if __name__ == "__main__":
    main()
