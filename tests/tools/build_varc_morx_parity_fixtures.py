#!/usr/bin/env python3
"""Write the VARC and morx fonts of tests/varc_parity.rs and
tests/aat_morx_parity.rs.

Both tables are written byte by byte from their specs, so every flag
and edge case the parity tests need is under control:

- `varc_parity.ttf`: a TrueType font with three axes (AAAA, BBBB, CCCC,
  -100 to 100, default 0) whose glyphs `box` and `tri` vary through
  gvar, and 14 VARC glyphs over them: transforms (ScaleY defaulting to
  ScaleX, skew, center), axis values with deltas over several regions,
  transform deltas, an axis-values index without axes, a nested
  composite, conditions of every format, reserved flag bits,
  RESET_UNSPECIFIED_AXES at the top and one level down, and an axis
  index past HarfBuzz's 4096-axis limit. fontTools reads the table
  back to check the encoding.
- `aat_morx_contextual.ttf` and `aat_morx_ligature.ttf`: AAT fonts
  without GSUB. The first has a contextual subtable with mark, current,
  end-of-text and unset-mark entries over an unsized substitution
  table; the second a ligature subtable with cascading ligatures, a
  component set twice (DontAdvance), Store without Last, an action at
  end of text, and a stack underflow.

tests/tools/varc_morx_parity_expected.py records HarfBuzz's output for
them.

Run:
    uv run --no-project --with fonttools python tests/tools/build_varc_morx_parity_fixtures.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables.DefaultTable import DefaultTable
from fontTools.ttLib.tables.TupleVariation import TupleVariation

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"

AXES = ["AAAA", "BBBB", "CCCC"]

# --- VARC encoding --------------------------------------------------------

def u32var(v):
    if v < 0x80:
        return struct.pack(">B", v)
    if v < 0x4000:
        return struct.pack(">H", v | 0x8000)
    if v < 0x200000:
        return struct.pack(">L", v | 0xC00000)[1:]
    if v < 0x10000000:
        return struct.pack(">L", v | 0xE0000000)
    return b"\xF0" + struct.pack(">L", v)

def tuple_values(values):
    out = bytearray()
    i = 0
    while i < len(values):
        v = values[i]
        kind = 0 if -128 <= v <= 127 else (1 if -32768 <= v <= 32767 else 2)
        run = []
        while i < len(values) and len(run) < 64:
            w = values[i]
            k = 0 if -128 <= w <= 127 else (1 if -32768 <= w <= 32767 else 2)
            if k != kind:
                break
            run.append(w); i += 1
        ctrl = {0: 0x00, 1: 0x40, 2: 0xC0}[kind] | (len(run) - 1)
        out.append(ctrl)
        for w in run:
            out += struct.pack({0: ">b", 1: ">h", 2: ">i"}[kind], w)
    return bytes(out)

def cff2_index(items):
    out = struct.pack(">L", len(items))
    if not items:
        return out
    total = sum(len(x) for x in items) + 1
    size = 1 if total < 0x100 else 2 if total < 0x10000 else 3 if total < 0x1000000 else 4
    out += bytes([size])
    off = 1
    offs = [off]
    for x in items:
        off += len(x); offs.append(off)
    for o in offs:
        out += o.to_bytes(size, "big")
    for x in items:
        out += x
    return out

def coverage(gids):
    return struct.pack(">HH", 1, len(gids)) + b"".join(struct.pack(">H", g) for g in gids)

def f2(v):
    return struct.pack(">h", round(v * 16384))

def multi_var_store(regions, subtables):
    """regions: list of [(axis, start, peak, end)]. subtables: list of
    (region_indices, [delta sets as region-major value lists])."""
    rl = struct.pack(">H", len(regions))
    bodies = b""
    for r in regions:
        rl_off = 2 + 4 * len(regions) + len(bodies)
        rl += struct.pack(">L", rl_off)
        bodies += struct.pack(">H", len(r)) + b"".join(
            struct.pack(">H", a) + f2(s) + f2(p) + f2(e) for a, s, p, e in r)
    rl += bodies
    head_len = 2 + 4 + 2 + 4 * len(subtables)
    subs = []
    for indices, sets in subtables:
        sub = struct.pack(">BH", 1, len(indices)) + b"".join(struct.pack(">H", i) for i in indices)
        sub += cff2_index([tuple_values(s) for s in sets])
        subs.append(sub)
    out = struct.pack(">HLH", 1, head_len, len(subtables))
    off = head_len + len(rl)
    for s in subs:
        out += struct.pack(">L", off); off += len(s)
    return out + rl + b"".join(subs)

def off24(v):
    return struct.pack(">L", v)[1:]

def cond_axis(axis, lo, hi):
    return struct.pack(">HH", 1, axis) + f2(lo) + f2(hi)

def cond_value(default, var_idx):
    return struct.pack(">HhL", 2, default, var_idx)

def cond_list_op(fmt, children):
    """And (3) / Or (4) with inline children placed after the offsets."""
    head = struct.pack(">HB", fmt, len(children))
    off = len(head) + 3 * len(children)
    offs = b""; body = b""
    for c in children:
        offs += off24(off + len(body)); body += c
    return head + offs + body

def cond_not(child):
    return struct.pack(">H", 5) + off24(5) + child

def condition_list(conds):
    out = struct.pack(">L", len(conds))
    off = 4 + 4 * len(conds)
    offs = b""; body = b""
    for c in conds:
        offs += struct.pack(">L", off + len(body)); body += c
    return out + offs + body

FLAG = dict(RESET=1 << 0, HAVE_AXES=1 << 1, AXIS_VAR=1 << 2, TRANSFORM_VAR=1 << 3,
            TX=1 << 4, TY=1 << 5, ROT=1 << 6, COND=1 << 7, SX=1 << 8, SY=1 << 9,
            CX=1 << 10, CY=1 << 11, GID24=1 << 12, SKX=1 << 13, SKY=1 << 14)
FIELD_ORDER = ["TX", "TY", "ROT", "SX", "SY", "SKX", "SKY", "CX", "CY"]

def component(gid, *, reset=False, cond=None, axes=None, axis_var=None,
              transform_var=None, fields=None, reserved=()):
    """axes: (axis_indices_index, [F2DOT14 ints]). fields: raw int16 per
    FIELD_ORDER name. reserved: [(bit, value)] extra uint32vars."""
    flags = 0
    body = b""
    if reset:
        flags |= FLAG["RESET"]
    body += struct.pack(">H", gid)
    if cond is not None:
        flags |= FLAG["COND"]; body += u32var(cond)
    if axes is not None:
        flags |= FLAG["HAVE_AXES"]; body += u32var(axes[0]) + tuple_values(axes[1])
    if axis_var is not None:
        flags |= FLAG["AXIS_VAR"]; body += u32var(axis_var)
    if transform_var is not None:
        flags |= FLAG["TRANSFORM_VAR"]; body += u32var(transform_var)
    for name in FIELD_ORDER:
        if fields and name in fields:
            flags |= FLAG[name]; body += struct.pack(">h", fields[name])
    for bit, value in reserved:
        flags |= 1 << bit; body += u32var(value)
    return u32var(flags) + body

def varc_table(cov_gids, records, store=b"", conds=b"", axis_lists=()):
    parts = [coverage(cov_gids), store, conds,
             cff2_index([tuple_values(l) for l in axis_lists]) if axis_lists else b"",
             cff2_index(records)]
    out = b""
    off = 24
    offsets = []
    for p in parts:
        offsets.append(off if p else 0)
        out += p; off += len(p)
    offsets[4] = offsets[4] or 24 + sum(len(p) for p in parts[:4])
    return struct.pack(">HHLLLLL", 1, 0, *offsets) + out

# --- font -----------------------------------------------------------------

def fixed_timestamps(fb):
    """Pins head.created and head.modified, so a rebuild writes the same
    bytes."""
    fb.font.recalcTimestamp = False
    fb.font["head"].created = fb.font["head"].modified = 3_600_000_000


def rect(x0, y0, x1, y1):
    pen = TTGlyphPen(None)
    pen.moveTo((x0, y0)); pen.lineTo((x1, y0)); pen.lineTo((x1, y1)); pen.lineTo((x0, y1)); pen.closePath()
    return pen.glyph()

def tri():
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0)); pen.lineTo((200, 0)); pen.lineTo((100, 300)); pen.closePath()
    return pen.glyph()

def empty():
    return TTGlyphPen(None).glyph()

def build_varc_font(path, varc_glyphs, make_varc):
    order = [".notdef", "box", "tri"] + varc_glyphs
    fb = FontBuilder(1000, isTTF=True)
    fb.setupGlyphOrder(order)
    fb.setupCharacterMap({0x41 + i: n for i, n in enumerate(order[1:])})
    glyphs = {".notdef": empty(), "box": rect(0, 0, 100, 100), "tri": tri()}
    for n in varc_glyphs:
        glyphs[n] = empty()
    fb.setupGlyf(glyphs)
    glyf = fb.font["glyf"]
    fb.setupHorizontalMetrics({n: (600, getattr(glyf[n], "xMin", 0)) for n in order})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "VarcTest", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()
    fb.setupFvar(axes=[(t, -100, 0, 100, t) for t in AXES], instances=[])
    z = (0, 0)
    fb.setupGvar({
        # box points: (0,0) (100,0) (100,100) (0,100) + 4 phantoms
        "box": [
            TupleVariation({"AAAA": (0, 1, 1)}, [z, (50, 0), (50, 0), z, z, z, z, z]),
            TupleVariation({"BBBB": (0, 1, 1)}, [z, z, (0, 40), (0, 40), z, z, z, z]),
            TupleVariation({"CCCC": (-1, -1, 0)}, [(30, 0), (30, 0), (30, 0), (30, 0), z, z, z, z]),
            TupleVariation({"AAAA": (0, 1, 1), "BBBB": (0, 1, 1)}, [z, z, (25, 25), z, z, z, z, z]),
        ],
        "tri": [
            TupleVariation({"BBBB": (0, 1, 1)}, [z, (60, 0), (0, 80), z, z, z, z]),
            TupleVariation({"CCCC": (0, 1, 1)}, [(0, -20), z, (-40, 0), z, z, z, z]),
        ],
    })
    gid = {n: i for i, n in enumerate(order)}
    t = DefaultTable("VARC")
    t.data = make_varc(gid)
    fb.font["VARC"] = t
    fixed_timestamps(fb)
    fb.save(path)
    # Read back with fontTools' VARC decoder.
    f = TTFont(path)
    f["VARC"].table  # decompiles
    return gid

# --- VARC glyphs ---------------------------------------------------------

VARC_NAMES = ["v_affine", "v_skew", "v_axes", "v_tvar", "v_axisvar_noaxes", "v_nested",
         "v_reset1", "v_cond", "v_reserved", "v_inner", "v_outer", "v_inner_keep", "v_outer_keep",
         "v_bigaxis"]

def make(g):
    R_A = [(0, 0.0, 1.0, 1.0)]
    R_B = [(1, 0.0, 1.0, 1.0)]
    R_AB = [(0, 0.0, 1.0, 1.0), (1, 0.0, 1.0, 1.0)]
    store = multi_var_store(
        [R_A, R_B, R_AB],
        [([0, 1], [
            [1000, 2000, -3000, 500],          # 0: two axis values, region-major
            [50, -20, 410, 256, -30, 60, 0, -128],  # 1: TX TY ROT SX
            [2, 0],                             # 2: one value (condition)
            [1500, -800],                       # 3: one axis value
        ]),
         ([2], [[700, -300]])],                 # outer 1: two values, one region
    )
    conds = condition_list([
        cond_axis(0, 0.2, 1.0),
        cond_value(-1, 2),
        cond_list_op(3, [cond_axis(1, -1.0, 0.0), cond_not(cond_axis(2, 0.5, 1.0))]),
        cond_list_op(4, [cond_axis(0, -1.0, -0.5), cond_axis(1, 0.5, 1.0)]),
    ])
    axis_lists = [[0, 2], [1], [0], [5000]]
    box, tri = g["box"], g["tri"]
    rec = {}
    rec["v_affine"] = component(box, fields=dict(TX=10, TY=20, ROT=683, SX=1536))
    rec["v_skew"] = component(box, fields=dict(SX=820, SY=1229, SKX=410, SKY=-205, CX=50, CY=50))
    rec["v_axes"] = component(box, axes=(0, [8192, -4096]), axis_var=0, fields=dict(TX=5))
    rec["v_tvar"] = component(box, transform_var=1, fields=dict(TX=100, TY=0, ROT=0, SX=1024))
    rec["v_axisvar_noaxes"] = component(box, axis_var=0, fields=dict(TX=200))
    rec["v_nested"] = component(g["v_axes"], axes=(1, [4915]), fields=dict(TX=300)) + \
        component(tri, axes=(0, [-8192, 8192]), axis_var=(1 << 16), fields=dict(TX=500, ROT=-410))
    rec["v_reset1"] = component(box, reset=True, axes=(2, [3277]))
    rec["v_cond"] = (component(box, cond=0) + component(tri, cond=1, fields=dict(TX=150))
                     + component(box, cond=2, fields=dict(TX=300))
                     + component(tri, cond=3, fields=dict(TX=450))
                     + component(box, cond=99, fields=dict(TX=600))
                     + component(tri, fields=dict(TX=750)))
    rec["v_reserved"] = component(box, fields=dict(TX=10), reserved=[(15, 5), (20, 300)]) + \
        component(tri, fields=dict(TX=500))
    rec["v_inner"] = component(box, reset=True, axes=(2, [3277]))
    rec["v_outer"] = component(g["v_inner"], axes=(1, [11469]), fields=dict(TY=10))
    rec["v_inner_keep"] = component(box, axes=(2, [3277]))
    rec["v_outer_keep"] = component(g["v_inner_keep"], axes=(1, [11469]), fields=dict(TY=10))
    # Axis index 5000 is past HarfBuzz's 4096-axis limit: ignored.
    rec["v_bigaxis"] = component(box, axes=(3, [8192]), fields=dict(TX=1))
    gids = sorted((g[n], rec[n]) for n in VARC_NAMES)
    return varc_table([x for x, _ in gids], [r for _, r in gids], store, conds, axis_lists)

# --- morx -----------------------------------------------------------------

GLYPHS = [".notdef", "a", "b", "f", "i", "l", "x", "A", "B", "f_i", "f_f", "f_f_i", "l_x", "X", "c"]
G = {n: i for i, n in enumerate(GLYPHS)}
CMAP = {ord(c): c for c in "abfilxc"}

def lookup6(pairs):
    pairs = sorted(pairs)
    out = struct.pack(">HHHHHH", 6, 4, len(pairs), 0, 0, 0)
    for g, v in pairs:
        out += struct.pack(">HH", g, v)
    return out

def stx(n_classes, class_pairs, states, entries, ext):
    """Extended state table body. states: rows of entry indices.
    entries: packed entry bytes. ext: list of byte blobs, each placed
    after the arrays and referenced by a u32 offset after the header."""
    head = 16 + 4 * len(ext)
    classes = lookup6(class_pairs)
    class_off = head
    state_off = class_off + len(classes)
    if state_off % 2:
        state_off += 1
    state_bytes = b"".join(struct.pack(">H", e) for row in states for e in row)
    entry_off = state_off + len(state_bytes)
    entry_bytes = b"".join(entries)
    body_tail_off = entry_off + len(entry_bytes)
    ext_offs = []
    tail = b""
    for blob in ext:
        while (body_tail_off + len(tail)) % 4:
            tail += b"\0"
        ext_offs.append(body_tail_off + len(tail))
        tail += blob
    out = struct.pack(">IIII", n_classes, class_off, state_off, entry_off)
    out += b"".join(struct.pack(">I", o) for o in ext_offs)
    out += classes
    out += b"\0" * (state_off - class_off - len(classes))
    out += state_bytes + entry_bytes + tail
    return out

def subtable(kind, body, flags=1):
    return struct.pack(">III", 12 + len(body), kind, flags) + body

def morx(subtables):
    body = b"".join(subtables)
    chain = struct.pack(">IIII", 1, 16 + len(body), 0, len(subtables)) + body
    return struct.pack(">HHI", 2, 0, 1) + chain

def build_morx_font(path, morx_bytes):
    fb = FontBuilder(1000, isTTF=True)
    fb.setupGlyphOrder(GLYPHS)
    fb.setupCharacterMap(CMAP)
    pen = TTGlyphPen(None)
    fb.setupGlyf({n: pen.glyph() for n in GLYPHS})
    fb.setupHorizontalMetrics({n: (100 + 10 * i, 0) for i, n in enumerate(GLYPHS)})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "MorxTest", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()
    t = DefaultTable("morx")
    t.data = morx_bytes
    fb.font["morx"] = t
    fixed_timestamps(fb)
    fb.save(path)

# Entry flags.
SET_MARK = 0x8000
DONT_ADVANCE = 0x4000
SET_COMPONENT = 0x8000
PERFORM_ACTION = 0x2000
LAST = 0x80000000
STORE = 0x40000000

def ctx_entry(new_state, flags, mark, current):
    return struct.pack(">HHHH", new_state, flags, mark, current)

def lig_entry(new_state, flags, action):
    return struct.pack(">HHH", new_state, flags, action)

def lig_action(offset, last=False, store=False):
    return struct.pack(">I", (offset & 0x3FFFFFFF) | (LAST if last else 0) | (STORE if store else 0))

def contextual():
    # Classes: a=4, b=5, x=6, c=7.
    classes = [(G["a"], 4), (G["b"], 5), (G["x"], 6), (G["c"], 7)]
    N = 0xFFFF
    entries = [
        ctx_entry(0, 0, N, N),            # 0 noop
        ctx_entry(2, SET_MARK, N, N),     # 1 a: mark it
        ctx_entry(0, 0, 1, 0),            # 2 b after a: a->A (mark), b->B (current)
        ctx_entry(3, SET_MARK, N, N),     # 3 x: mark, wait for end of text
        ctx_entry(0, 0, N, 2),            # 4 end of text after x: current -> X
        ctx_entry(0, 0, 3, N),            # 5 c with no mark set: mark lookup 3 (c->A)
    ]
    states = [
        [0, 0, 0, 0, 1, 0, 3, 5],
        [0, 0, 0, 0, 1, 0, 3, 5],
        [0, 0, 0, 0, 1, 2, 3, 5],
        [4, 0, 0, 0, 1, 0, 3, 5],
    ]
    lookups = [lookup6([(G["b"], G["B"])]), lookup6([(G["a"], G["A"])]),
               lookup6([(G["x"], G["X"])]), lookup6([(G["c"], G["A"]), (G["a"], G["B"])])]
    # Unsized array of u32 offsets from the table start, then the lookups.
    offs = b""
    at = 4 * len(lookups)
    for lk in lookups:
        offs += struct.pack(">I", at); at += len(lk)
    table = offs + b"".join(lookups)
    return subtable(1, stx(8, classes, states, entries, [table]))

def ligatures():
    classes = [(G["f"], 4), (G["i"], 5), (G["l"], 6), (G["x"], 7), (G["a"], 8), (G["b"], 9), (G["c"], 10), (G["f_f"], 4)]
    ligs = [G["f_i"], G["f_f"], G["f_f_i"], G["l_x"], G["X"], G["A"], G["B"]]
    comps = [0] * 1400
    def comp(off, glyph, value):
        comps[off + glyph] = value
    actions = b""
    def action_list(pairs):
        nonlocal actions
        start = len(actions) // 4
        for off, last, store in pairs:
            actions += lig_action(off, last, store)
        return start
    A_fi = action_list([(100, False, False), (200, True, True)]); comp(100, G["i"], 0); comp(200, G["f"], 0)
    A_ff = action_list([(300, False, False), (400, True, True)]); comp(300, G["f"], 1); comp(400, G["f"], 0)
    A_ffi = action_list([(500, False, False), (600, True, True)]); comp(500, G["i"], 2); comp(600, G["f_f"], 0)
    A_lx = action_list([(700, False, False), (800, True, True)]); comp(700, G["x"], 3); comp(800, G["l"], 0)
    A_ab = action_list([(900, False, False), (1000, True, True)]); comp(900, G["b"], 4); comp(1000, G["a"], 0)
    A_cc = action_list([(1100, False, True), (1200, True, False)]); comp(1100, G["c"], 5); comp(1200, G["c"], 1)
    SC, PA, DA = SET_COMPONENT, PERFORM_ACTION, DONT_ADVANCE
    entries = [
        lig_entry(0, 0, 0),             # 0 noop
        lig_entry(2, SC, 0),            # 1 f: push
        lig_entry(3, SC | PA, A_ff),    # 2 f after f: ff, ligature stays on the stack
        lig_entry(0, SC | PA, A_fi),    # 3 i after f: fi
        lig_entry(0, SC | PA, A_ffi),   # 4 i after ff: ffi
        lig_entry(4, SC, 0),            # 5 l: push
        lig_entry(5, SC | DA, 0),       # 6 x after l: push, stay
        lig_entry(0, SC | PA, A_lx),    # 7 x again: push (same glyph), lx
        lig_entry(6, SC, 0),            # 8 a: push
        lig_entry(7, SC, 0),            # 9 b after a: push
        lig_entry(0, PA, A_ab),         # 10 end of text after a b: act
        lig_entry(8, SC, 0),            # 11 c: push
        lig_entry(0, SC | PA, A_cc),    # 12 c after c: two stores
    ]
    #        EOT OOB DEL EOL  f  i  l  x  a  b  c
    s0 = [0, 0, 0, 0, 1, 0, 5, 0, 8, 0, 11]
    states = [
        s0, s0,
        [0, 0, 0, 0, 2, 3, 5, 0, 8, 0, 11],   # 2 after f
        [0, 0, 0, 0, 1, 4, 5, 0, 8, 0, 11],   # 3 after ff
        [0, 0, 0, 0, 1, 0, 5, 6, 8, 0, 11],   # 4 after l
        [0, 0, 0, 0, 1, 0, 5, 7, 8, 0, 11],   # 5 after l x (x not advanced)
        [0, 0, 0, 0, 1, 0, 5, 0, 8, 9, 11],   # 6 after a
        [10, 0, 0, 0, 1, 0, 5, 0, 8, 0, 11],  # 7 after a b
        [0, 0, 0, 0, 1, 0, 5, 0, 8, 0, 12],   # 8 after c
    ]
    comp_bytes = b"".join(struct.pack(">H", c) for c in comps)
    lig_bytes = b"".join(struct.pack(">H", g) for g in ligs)
    return subtable(2, stx(11, classes, states, entries, [actions, comp_bytes, lig_bytes]))


def main() -> None:
    build_varc_font(FIXTURES / "varc_parity.ttf", VARC_NAMES, make)
    build_morx_font(FIXTURES / "aat_morx_contextual.ttf", morx([contextual()]))
    build_morx_font(FIXTURES / "aat_morx_ligature.ttf", morx([ligatures()]))


if __name__ == "__main__":
    main()
