#!/usr/bin/env python3
"""Builds tests/fixtures/attach_chain.ttf, a tiny font for GPOS attachment chains.

The font has bases `a` `b` `f` `i` `v`, the ligature `f_i` (GSUB `liga`), and the
combining marks U+0300, U+0301, U+0302 and U+0303. Its GPOS lookups run in this
order:

0. `mkmk`: U+0300 stacks on U+0301 before U+0301 is attached to anything.
1. `curs`: `b` joins `b` with the RightToLeft flag, so in left-to-right text
   each `b` hangs from the next one and a run of `b` makes one long chain.
2. `mark`: U+0301 and U+0302 attach to the bases.
3. `mark`: U+0301 and U+0302 attach to the components of `f_i`.
4. `mkmk`: U+0303 stacks on U+0301 and U+0302.
5. `blwm`: a single positioning lookup moves the bases after the marks are on
   them, along and across the line.

`tests/attach_offsets_parity.rs` checks the attached marks against HarfBuzz.
SIL Open Font License 1.1, no third-party content.

Run:
    python3 tests/tools/build_attach_chain_fixture.py
"""

from __future__ import annotations

from io import StringIO
from pathlib import Path

from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

OUT_PATH = Path(__file__).resolve().parent.parent / "fixtures" / "attach_chain.ttf"
UPEM = 1000

GLYPHS = {
    ".notdef": (500, 0),
    "a": (500, 0x61),
    "b": (600, 0x62),
    "f": (300, 0x66),
    "i": (250, 0x69),
    "v": (550, 0x76),
    "f_i": (550, None),
    "gravecomb": (200, 0x0300),
    "acutecomb": (200, 0x0301),
    "circumflexcomb": (200, 0x0302),
    "tildecomb": (200, 0x0303),
}

FEATURES = """
languagesystem DFLT dflt;
languagesystem latn dflt;

markClass gravecomb <anchor 0 0> @UPPER;
markClass [acutecomb circumflexcomb] <anchor 100 0> @TOP;
markClass tildecomb <anchor 0 0> @STACK;

lookup EARLY_STACK {
    pos mark acutecomb <anchor 0 700> mark @UPPER;
} EARLY_STACK;

lookup JOIN {
    lookupflag RightToLeft IgnoreMarks;
    pos cursive b <anchor 0 0> <anchor 500 100>;
} JOIN;

lookup ON_BASE {
    pos base [a b f i v] <anchor 250 600> mark @TOP;
} ON_BASE;

lookup ON_LIGATURE {
    pos ligature f_i <anchor 150 650> mark @TOP
        ligComponent <anchor 450 650> mark @TOP;
} ON_LIGATURE;

lookup STACK {
    pos mark [acutecomb circumflexcomb] <anchor 0 650> mark @STACK;
} STACK;

lookup MOVE {
    pos a <20 150 0 0>;
    pos b <0 50 0 0>;
    pos v <40 -70 0 0>;
    pos f_i <0 90 0 0>;
} MOVE;

feature mkmk { lookup EARLY_STACK; lookup STACK; } mkmk;
feature curs { lookup JOIN; } curs;
feature mark { lookup ON_BASE; lookup ON_LIGATURE; } mark;
feature blwm { lookup MOVE; } blwm;

feature liga { sub f i by f_i; } liga;

table GDEF {
    GlyphClassDef [a b f i v], [f_i], [gravecomb acutecomb circumflexcomb tildecomb], ;
} GDEF;
"""


def box(width: int, top: int):
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((0, top))
    pen.lineTo((max(width, 10), top))
    pen.lineTo((max(width, 10), 0))
    pen.closePath()
    return pen.glyph()


def main() -> None:
    order = list(GLYPHS)
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(order)
    fb.setupCharacterMap({cp: name for name, (_, cp) in GLYPHS.items() if cp})
    fb.setupGlyf({name: box(w, 500) for name, (w, _) in GLYPHS.items()})
    fb.setupHorizontalMetrics({name: (w, 0) for name, (w, _) in GLYPHS.items()})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "SigilbuzzAttachChain", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()
    addOpenTypeFeaturesFromString(fb.font, FEATURES)
    fb.font["head"].created = fb.font["head"].modified = 0
    fb.save(str(OUT_PATH))


if __name__ == "__main__":
    main()
