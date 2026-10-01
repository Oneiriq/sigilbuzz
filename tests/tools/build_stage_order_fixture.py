#!/usr/bin/env python3
"""Builds tests/fixtures/stage_order.ttf, a tiny font for GSUB stage order.

Each GSUB lookup substitutes one glyph, and the lookups of different
features interleave by lookup index, so the result shows which feature
ran first. HarfBuzz runs the default features, the direction features
and the caller's features of its default shaper in one stage, in lookup
index order. Its Arabic shaper runs the joining features in the order
isol, fina, medi, init, then `rlig`, then `calt`, then `liga`, `clig`
and `mset` (`collect_features_arabic`).

The Latin glyphs `a`, `b`, `c`, `e` and `f` and the Arabic letters beh,
lam and alef are mapped. `tests/stage_order_parity.rs` checks the
results against HarfBuzz. SIL Open Font License 1.1, no third-party
content.

Run:
    python3 tests/tools/build_stage_order_fixture.py
"""

from __future__ import annotations

from pathlib import Path

from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUT_PATH = Path(__file__).resolve().parent.parent / "fixtures" / "stage_order.ttf"
UPEM = 1000

GLYPHS = [
    (".notdef", None),
    ("a", 0x61),
    ("b", 0x62),
    ("c", 0x63),
    ("e", 0x65),
    ("f", 0x66),
    ("a_b", None),
    ("a.ccmp", None),
    ("c.sc", None),
    ("c.alt", None),
    ("e.vert", None),
    ("e.vc", None),
    ("f.c", None),
    ("f.l", None),
    ("beh", 0x0628),
    ("beh.fina", None),
    ("beh.init", None),
    ("beh.init2", None),
    ("lam", 0x0644),
    ("lam.init", None),
    ("lam.calt", None),
    ("alef", 0x0627),
    ("alef.fina", None),
    ("alef.mset", None),
    ("lam_alef", None),
]

FEATURES = """
languagesystem DFLT dflt;
languagesystem latn dflt;
languagesystem arab dflt;

lookup LIGA_AB { sub a b by a_b; } LIGA_AB;
lookup CCMP_A { sub a by a.ccmp; } CCMP_A;
lookup SMCP_C { sub c by c.sc; } SMCP_C;
lookup CALT_C { sub c by c.alt; } CALT_C;
lookup VERT_E { sub e by e.vert; } VERT_E;
lookup CCMP_E { sub e.vert by e.vc; } CCMP_E;
lookup CCMP_F { sub f by f.c; } CCMP_F;
lookup LTRA_F { sub f by f.l; } LTRA_F;
lookup FINA_BEH { sub beh by beh.fina; } FINA_BEH;
lookup INIT2 { sub beh by beh.init2; } INIT2;
lookup INIT_CTX { sub beh' lookup INIT2 beh.fina; } INIT_CTX;
lookup INIT_PLAIN { sub beh by beh.init; } INIT_PLAIN;
lookup INIT_LAM { sub lam by lam.init; } INIT_LAM;
lookup FINA_ALEF { sub alef by alef.fina; } FINA_ALEF;
lookup LIGA_LAMALEF { sub lam.init alef.fina by lam_alef; } LIGA_LAMALEF;
lookup CALT_LAM { sub lam.init by lam.calt; } CALT_LAM;
lookup MSET_ALEF { sub alef by alef.mset; } MSET_ALEF;

feature ccmp { lookup CCMP_A; lookup CCMP_E; lookup CCMP_F; } ccmp;
feature ltra { lookup LTRA_F; } ltra;
feature fina { lookup FINA_BEH; lookup FINA_ALEF; } fina;
feature init { lookup INIT_CTX; lookup INIT_PLAIN; lookup INIT_LAM; } init;
feature liga { lookup LIGA_AB; lookup LIGA_LAMALEF; } liga;
feature calt { lookup CALT_C; lookup CALT_LAM; } calt;
feature smcp { lookup SMCP_C; } smcp;
feature vert { lookup VERT_E; } vert;
feature mset { lookup MSET_ALEF; } mset;
"""


def box(width: int):
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((0, 500))
    pen.lineTo((width, 500))
    pen.lineTo((width, 0))
    pen.closePath()
    return pen.glyph()


def main() -> None:
    names = [name for name, _ in GLYPHS]
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(names)
    fb.setupCharacterMap({cp: name for name, cp in GLYPHS if cp})
    fb.setupGlyf({name: box(500) for name in names})
    fb.setupHorizontalMetrics({name: (500, 0) for name in names})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "SigilbuzzStageOrder", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()
    addOpenTypeFeaturesFromString(fb.font, FEATURES)
    fb.font["head"].created = fb.font["head"].modified = 0
    fb.save(str(OUT_PATH))


if __name__ == "__main__":
    main()
