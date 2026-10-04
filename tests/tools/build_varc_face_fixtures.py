#!/usr/bin/env python3
"""Write tests/fixtures/varc_face.ttf, which tests/varc_parity.rs checks.

A TrueType font with the three axes of varc_parity.ttf (AAAA, BBBB,
CCCC, -100 to 100, default 0) whose VARC glyphs exercise what
`Face::glyph_outline_at_coords` and `GlyphOutlines` do across nested
composites:

- RESET_UNSPECIFIED_AXES two and three composites down, reset in the
  middle of a chain, and reset components whose axis values vary at the
  coords of the glyph they belong to;
- components that name their own glyph (HarfBuzz draws the glyph's
  `glyf` outline), at the top and one composite down, with a reset;
- delta sets that end before they fill their tuples, runs cut short in
  the first, a middle and the last tuple, an empty set, a cut i32 run,
  short axis-value sets and a Value condition over a short set;
- conditions read inside nested composites, at the coords of the glyph
  they gate, beside a reset component;
- one glyph reached many times at the same coords, and at others;
- cycles of two and three glyphs, which HarfBuzz's decycler cuts;
- a component the coverage names past the end of the glyph records,
  which HarfBuzz draws as nothing;
- a region that constrains no axis, whose deltas HarfBuzz applies at
  the default instance too, since its font holds a zero per axis there.

The VARC table is written byte by byte with the encoders of
build_varc_morx_parity_fixtures.py. tests/tools/varc_face_expected.py
records HarfBuzz's outlines for it.

Run:
    uv run --no-project --with fonttools python tests/tools/build_varc_face_fixtures.py
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib.tables.DefaultTable import DefaultTable
from fontTools.ttLib.tables.TupleVariation import TupleVariation

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build_varc_morx_parity_fixtures as p  # noqa: E402

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"


def diamond():
    pen = TTGlyphPen(None)
    pen.moveTo((50, 0)); pen.lineTo((100, 50)); pen.lineTo((50, 100)); pen.lineTo((0, 50)); pen.closePath()
    return pen.glyph()


def mvs_raw(regions, subtables):
    """p.multi_var_store, with each delta set given as raw bytes, so a
    set can end early or cut a run short."""
    rl = struct.pack(">H", len(regions))
    bodies = b""
    for r in regions:
        rl += struct.pack(">L", 2 + 4 * len(regions) + len(bodies))
        bodies += struct.pack(">H", len(r)) + b"".join(
            struct.pack(">H", a) + p.f2(s) + p.f2(pk) + p.f2(e) for a, s, pk, e in r)
    rl += bodies
    head_len = 2 + 4 + 2 + 4 * len(subtables)
    subs = []
    for indices, sets in subtables:
        sub = struct.pack(">BH", 1, len(indices)) + b"".join(struct.pack(">H", i) for i in indices)
        sub += p.cff2_index(sets)
        subs.append(sub)
    out = struct.pack(">HLH", 1, head_len, len(subtables))
    off = head_len + len(rl)
    for s in subs:
        out += struct.pack(">L", off); off += len(s)
    return out + rl + b"".join(subs)


def tv(values):
    return p.tuple_values(values)


# Region indices.
RA, RB, RC, RAN, RK = 0, 1, 2, 3, 4
REGIONS = [
    [(0, 0.0, 1.0, 1.0)],    # RA: AAAA up
    [(1, 0.0, 1.0, 1.0)],    # RB: BBBB up
    [(2, 0.0, 1.0, 1.0)],    # RC: CCCC up
    [(0, -1.0, -1.0, 0.0)],  # RAN: AAAA down
    [],                      # RK: no axis, so everywhere
]

# Variation indices, outer << 16 | inner.
def vi(outer, inner):
    return (outer << 16) | inner

SUBTABLES = [
    # outer 0: regions RA, RB; sets of two values per region.
    ([RA, RB], [
        tv([40, 10, -30, 20]),                  # 0: full
        bytes([0x02, 40, 10, (-30) & 0xFF]),     # 1: region RB's tuple has one value
        bytes([0x41, 0x00, 0x05]),               # 2: run of two words cut in RA's tuple
        bytes([0x01, 40, 10, 0x41, 0x00, 0x07]), # 3: run cut in RB's (the last) tuple
        b"",                                     # 4: empty set
        bytes([0xC1, 0, 0, 0, 100, 0, 0]),       # 5: i32 run cut: RB reads 0x00 0x00, 0x00 0x64
        bytes([0x42, 0x10, 0x00, 0xF0, 0x00, 0x08, 0x00]),  # 6: 4096 -4096 2048: axis values
    ]),
    # outer 1: regions RA, RB, RC; sets of two values per region.
    ([RA, RB, RC], [
        bytes([0x41, 0x00, 0x05, 0x03]),         # 0: cut in RA's tuple; RB reads 5, RC nothing
        bytes([0x01, 30, -20 & 0xFF, 0x41, 0x00, 0x06]),  # 1: cut in RB's; RC reads 6
        tv([10, 20, 30, 40, 50]),                # 2: RC's tuple has one value
    ]),
    # outer 2: one value per region (conditions), regions RA, RB.
    ([RA, RB], [
        bytes([0x00, 5]),                        # 0: RA's value only
        b"",                                     # 1: nothing
        tv([2, -3]),                             # 2: full
    ]),
    # outer 3: axis values, regions RB, RAN.
    ([RB, RAN], [
        tv([4096, 2048, -4096, 1024]),           # 0: full, two values per region
        bytes([0x40, 0x10, 0x00]),               # 1: one word: RB's first value
    ]),
    # outer 4: a region that holds everywhere, the default included.
    ([RK], [
        tv([60, -40]),                           # 0: two translations
        tv([2]),                                 # 1: one value (a condition)
    ]),
]

CONDS = [
    p.cond_axis(0, 0.2, 1.0),                    # 0: AAAA in [0.2, 1]
    p.cond_value(-1, vi(2, 0)),                  # 1: -1 + 5*sA > 0 over a short set
    p.cond_value(-1, vi(2, 1)),                  # 2: -1 + nothing
    p.cond_value(1, vi(2, 2)),                   # 3: 1 + 2*sA - 3*sB > 0
    p.cond_list_op(4, [p.cond_axis(1, 0.5, 1.0), p.cond_not(p.cond_axis(0, -1.0, 0.0))]),  # 4
    p.cond_axis(1, -1.0, -0.25),                 # 5: BBBB in [-1, -0.25]
    p.cond_value(-1, vi(4, 1)),                  # 6: -1 + 2 everywhere
]

# Axis indices lists.
AX_A, AX_B, AX_C, AX_AB, AX_BC = 0, 1, 2, 3, 4
AXIS_LISTS = [[0], [1], [2], [0, 1], [1, 2]]

C = p.component

VARC_NAMES = [
    # nested reset
    "n2_outer", "n2_mid",
    "n3_top", "n3_mid1", "n3_mid2",
    "nr_top", "nr_mid", "nr_low",
    "nv_outer", "nv_mid",
    # self reference (these have glyf outlines of their own)
    "s_self", "s_nest", "s_inner", "s_reset",
    # short delta sets
    "d_full", "d_short", "d_cut_first", "d_cut_last", "d_empty", "d_long",
    "d_three_first", "d_three_mid", "d_three_last", "d_axes", "d_axes_short",
    # conditions inside nested composites
    "c_outer", "c_inner", "c_reset_outer", "c_reset_inner",
    # one glyph reached many times
    "m_outer", "m_inner",
    # cycles longer than a self reference
    "y_a", "y_b", "z_a", "z_b", "z_c",
    # a component VARC covers without a glyph record
    "x_user",
    # a region without axes, which holds at the default instance too
    "k_const", "k_cond", "k_outer",
]
# Covered by the VARC coverage, past the glyph records: last in the
# glyph order, so last in the coverage.
NO_RECORD = "x_norecord"
SELF_OUTLINED = {"s_self", "s_inner", "s_reset", "y_b", "x_norecord"}


def make(g):
    box, tri = g["box"], g["tri"]
    rec = {}

    # A RESET_UNSPECIFIED_AXES component two composites down: box takes
    # the font's AAAA and BBBB, not n2_mid's BBBB.
    rec["n2_outer"] = C(g["n2_mid"], axes=(AX_B, [11469]), fields=dict(TY=10))
    rec["n2_mid"] = C(box, reset=True, axes=(AX_C, [3277]), fields=dict(TX=5))

    # Three composites: the reset sits under n3_mid1 (BBBB 0.7) and
    # n3_mid2 (AAAA -0.5), and a sibling without reset inherits both.
    rec["n3_top"] = C(g["n3_mid1"], axes=(AX_B, [11469]), fields=dict(TY=10))
    rec["n3_mid1"] = C(g["n3_mid2"], axes=(AX_A, [-8192]), fields=dict(TX=20))
    rec["n3_mid2"] = (C(box, reset=True, axes=(AX_C, [4915]))
                      + C(box, axes=(AX_C, [4915]), fields=dict(TX=150)))

    # A reset in the middle: nr_mid starts from the font's coords, and
    # nr_low, below it, inherits nr_mid's coords with its own AAAA.
    rec["nr_top"] = C(g["nr_mid"], axes=(AX_AB, [6000, -9000]))
    rec["nr_mid"] = C(g["nr_low"], reset=True, axes=(AX_C, [-6554]), fields=dict(TY=20))
    rec["nr_low"] = (C(box, axes=(AX_A, [8192]))
                     + C(box, reset=True, fields=dict(TX=150)))

    # A reset component whose axis values vary: the deltas are read at
    # nv_mid's coords (BBBB 0.5 from nv_outer), then the reset coords
    # take them.
    rec["nv_outer"] = C(g["nv_mid"], axes=(AX_B, [8192]))
    rec["nv_mid"] = C(box, reset=True, axes=(AX_AB, [0, 0]), axis_var=vi(3, 0))

    # Self reference: HarfBuzz draws the glyph's glyf outline.
    rec["s_self"] = (C(g["s_self"], axes=(AX_A, [8192]), fields=dict(TX=200))
                     + C(box, fields=dict(TY=-120)))
    rec["s_nest"] = C(g["s_inner"], axes=(AX_B, [-8192]), fields=dict(TX=50))
    rec["s_inner"] = (C(g["s_inner"], fields=dict(SX=512, TX=300))
                      + C(box, fields=dict(TY=150)))
    rec["s_reset"] = C(g["s_reset"], reset=True, axes=(AX_C, [8192]), fields=dict(ROT=1024))

    # Short delta sets, on two translations.
    for name, inner in [("d_full", 0), ("d_short", 1), ("d_cut_first", 2),
                        ("d_cut_last", 3), ("d_empty", 4), ("d_long", 5)]:
        rec[name] = C(box, transform_var=vi(0, inner), fields=dict(TX=0, TY=0))
    for name, inner in [("d_three_first", 0), ("d_three_mid", 1), ("d_three_last", 2)]:
        rec[name] = C(box, transform_var=vi(1, inner), fields=dict(TX=0, TY=0))
    rec["d_axes"] = C(box, axes=(AX_AB, [0, 0]), axis_var=vi(3, 0))
    rec["d_axes_short"] = (C(box, axes=(AX_AB, [0, 0]), axis_var=vi(3, 1))
                           + C(tri, axes=(AX_BC, [0, 0]), axis_var=vi(0, 6), fields=dict(TX=200)))

    # Conditions inside nested composites, read at c_inner's coords.
    rec["c_outer"] = (C(g["c_inner"], axes=(AX_A, [9830]))
                      + C(g["c_inner"], axes=(AX_A, [-9830]), fields=dict(TY=300))
                      + C(g["c_inner"], axes=(AX_B, [13107]), fields=dict(TY=600)))
    rec["c_inner"] = (C(box, cond=0)
                      + C(tri, cond=1, fields=dict(TX=150))
                      + C(box, cond=2, fields=dict(TX=300))
                      + C(tri, cond=3, fields=dict(TX=450))
                      + C(box, cond=4, fields=dict(TX=600)))
    # A condition gates a reset component: it is read at the coords of
    # the glyph it belongs to, before the reset.
    rec["c_reset_outer"] = C(g["c_reset_inner"], axes=(AX_B, [-8192]))
    rec["c_reset_inner"] = (C(box, cond=5, reset=True, fields=dict(TX=10))
                            + C(tri, cond=0, reset=True, fields=dict(TX=200)))

    # The same glyph at the same coords three times, then elsewhere.
    rec["m_outer"] = (C(g["m_inner"], axes=(AX_C, [8192]))
                      + C(g["m_inner"], axes=(AX_C, [8192]), fields=dict(TX=300))
                      + C(g["m_inner"], axes=(AX_C, [8192]), fields=dict(TY=300))
                      + C(g["m_inner"], axes=(AX_C, [-8192]), fields=dict(TX=300, TY=300)))
    rec["m_inner"] = (C(box, transform_var=vi(0, 0), fields=dict(TX=0, TY=0))
                      + C(tri, cond=0, fields=dict(TX=120)))

    # Cycles: HarfBuzz's decycler stops a glyph met again on the way
    # down, at a depth that depends on the cycle's length.
    rec["y_a"] = C(box) + C(g["y_b"], axes=(AX_A, [4096]), fields=dict(TX=200))
    rec["y_b"] = C(tri, fields=dict(TY=150)) + C(g["y_a"], fields=dict(TX=100, SX=819))
    rec["z_a"] = C(g["z_b"], fields=dict(TX=150)) + C(box)
    rec["z_b"] = C(tri, fields=dict(TY=200)) + C(g["z_c"], axes=(AX_B, [6000]))
    rec["z_c"] = C(g["z_a"], fields=dict(TY=-300, SX=700)) + C(box, fields=dict(TX=-150))

    # A region that constrains no axis holds everywhere: HarfBuzz's font
    # has a zero per axis at the default instance, so it applies there.
    rec["k_const"] = C(box, transform_var=vi(4, 0), fields=dict(TX=0, TY=0))
    rec["k_cond"] = C(tri, cond=6) + C(box, fields=dict(TX=300))
    rec["k_outer"] = C(g["k_const"], fields=dict(TY=200)) + C(g["k_cond"], fields=dict(TX=400))

    # A component VARC covers but has no record for.
    rec["x_user"] = C(g[NO_RECORD], fields=dict(TX=50)) + C(tri, fields=dict(TX=250))

    store = mvs_raw(REGIONS, SUBTABLES)
    gids = sorted((g[n], rec[n]) for n in VARC_NAMES)
    assert g[NO_RECORD] > gids[-1][0]
    return p.varc_table([x for x, _ in gids] + [g[NO_RECORD]], [r for _, r in gids], store,
                        p.condition_list(CONDS), AXIS_LISTS)


def build(path):
    order = [".notdef", "box", "tri"] + VARC_NAMES + [NO_RECORD]
    fb = FontBuilder(1000, isTTF=True)
    fb.setupGlyphOrder(order)
    fb.setupCharacterMap({0x41 + i: n for i, n in enumerate(order[1:])})
    glyphs = {".notdef": p.empty(), "box": p.rect(0, 0, 100, 100), "tri": p.tri()}
    for n in VARC_NAMES + [NO_RECORD]:
        glyphs[n] = diamond() if n in SELF_OUTLINED else p.empty()
    fb.setupGlyf(glyphs)
    glyf = fb.font["glyf"]
    fb.setupHorizontalMetrics({n: (600, getattr(glyf[n], "xMin", 0)) for n in order})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "VarcFaceTest", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()
    fb.setupFvar(axes=[(t, -100, 0, 100, t) for t in p.AXES], instances=[])
    z = (0, 0)
    variations = {
        "box": [
            TupleVariation({"AAAA": (0, 1, 1)}, [z, (50, 0), (50, 0), z, z, z, z, z]),
            TupleVariation({"BBBB": (0, 1, 1)}, [z, z, (0, 40), (0, 40), z, z, z, z]),
            TupleVariation({"CCCC": (-1, -1, 0)}, [(30, 0), (30, 0), (30, 0), (30, 0), z, z, z, z]),
            TupleVariation({"CCCC": (0, 1, 1)}, [z, z, z, (-20, 60), z, z, z, z]),
            TupleVariation({"AAAA": (0, 1, 1), "BBBB": (0, 1, 1)}, [z, z, (25, 25), z, z, z, z, z]),
        ],
        "tri": [
            TupleVariation({"BBBB": (0, 1, 1)}, [z, (60, 0), (0, 80), z, z, z, z]),
            TupleVariation({"CCCC": (0, 1, 1)}, [(0, -20), z, (-40, 0), z, z, z, z]),
        ],
    }
    # The glyphs that name themselves vary too, so the coords their own
    # outline is drawn at show.
    for n in SELF_OUTLINED:
        variations[n] = [
            TupleVariation({"AAAA": (0, 1, 1)}, [z, (40, 0), z, z, z, z, z, z]),
            TupleVariation({"BBBB": (-1, -1, 0)}, [z, z, (0, -30), z, z, z, z, z]),
            TupleVariation({"CCCC": (0, 1, 1)}, [(0, -25), z, z, (-35, 0), z, z, z, z]),
        ]
    fb.setupGvar(variations)
    gid = {n: i for i, n in enumerate(order)}
    t = DefaultTable("VARC")
    t.data = make(gid)
    fb.font["VARC"] = t
    p.fixed_timestamps(fb)
    fb.save(path)
    # fontTools' VARC decoder rejects the short delta sets, which is
    # the point of them, so the table is not read back here;
    # varc_face_expected.py checks that HarfBuzz draws every glyph.


if __name__ == "__main__":
    build(FIXTURES / "varc_face.ttf")
