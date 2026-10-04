#!/usr/bin/env python3
"""Record HarfBuzz's output for the VARC and morx parity fixtures.

Writes tests/fixtures/varc_parity.expected, the outline of every VARC
glyph of varc_parity.ttf at eight locations, and
tests/fixtures/aat_morx_parity.expected, the glyphs and clusters
HarfBuzz's AAT shaper gives for strings run through
aat_morx_contextual.ttf and aat_morx_ligature.ttf. Build the fonts with
tests/tools/build_varc_morx_parity_fixtures.py first.

Run:
    uv run --no-project --with uharfbuzz==0.56.2 --with fonttools python tests/tools/varc_morx_parity_expected.py
"""

from __future__ import annotations

from pathlib import Path

import uharfbuzz as hb
from fontTools.pens.recordingPen import RecordingPen
from fontTools.ttLib import TTFont

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"

LOCATIONS = [
    "default",
    "AAAA=50,BBBB=-30,CCCC=70",
    "AAAA=33.3,BBBB=66.7,CCCC=-12.5",
    "AAAA=-80,BBBB=90,CCCC=55",
    "AAAA=70,BBBB=-100,CCCC=20",
    "AAAA=100,BBBB=100,CCCC=-100",
    "AAAA=25,BBBB=0,CCCC=0",
    "AAAA=-50,BBBB=10,CCCC=60",
]

MORX_STRINGS = {
    "aat_morx_contextual.ttf": ["ab", "ba", "aab", "abab", "x", "xa", "cx", "ca", "acb", "xx", "bx"],
    "aat_morx_ligature.ttf": [
        "fi", "ff", "ffi", "fff", "fffi", "ffffi", "lx", "lxlx", "xlx",
        "ab", "abc", "cc", "ccc", "fiab",
    ],
}


def num(v: float) -> str:
    text = f"{v:.4f}".rstrip("0").rstrip(".")
    return "0" if text in ("-0", "") else text


def outline(font: hb.Font, gid: int) -> str:
    pen = RecordingPen()
    font.draw_glyph_with_pen(gid, pen)
    ops = []
    start = None
    for op, args in pen.value:
        if op == "moveTo":
            start = args[0]
            ops.append(("M", [*args[0]]))
        elif op == "lineTo":
            ops.append(("L", [*args[0]]))
        elif op == "qCurveTo":
            ops.append(("Q", [*args[0], *args[-1]]))
        elif op == "curveTo":
            ops.append(("C", [*args[0], *args[1], *args[2]]))
        elif op == "closePath":
            # HarfBuzz closes with a line back to the start; sigilbuzz's
            # Close implies it.
            if ops and ops[-1][0] == "L" and start is not None and tuple(ops[-1][1]) == tuple(start):
                ops.pop()
            ops.append(("Z", []))
    return " ".join(" ".join([op] + [num(v) for v in vals]) for op, vals in ops)


def varc() -> None:
    path = FIXTURES / "varc_parity.ttf"
    tt = TTFont(path)
    order = tt.getGlyphOrder()
    covered = tt["VARC"].table.Coverage.glyphs
    face = hb.Face(hb.Blob.from_file_path(str(path)))
    lines = [
        f"# HarfBuzz {hb.version_string()} (uharfbuzz {hb.__version__}) outlines of varc_parity.ttf.",
        "# outline <location> <gid> <glyph> <ops>: M/L x y, Q cx cy x y, C ..., Z.",
    ]
    for loc in LOCATIONS:
        font = hb.Font(face)
        if loc != "default":
            font.set_variations({kv.split("=")[0]: float(kv.split("=")[1]) for kv in loc.split(",")})
        for name in covered:
            gid = order.index(name)
            lines.append(f"outline {loc} {gid} {name} {outline(font, gid)}")
    (FIXTURES / "varc_parity.expected").write_text("\n".join(lines) + "\n", newline="\n")


def morx() -> None:
    lines = [
        f"# HarfBuzz {hb.version_string()} (uharfbuzz {hb.__version__}) AAT shaping of the morx fixtures.",
        "# shape <font> <text> <gid>:<cluster>...",
    ]
    for name, strings in MORX_STRINGS.items():
        font = hb.Font(hb.Face(hb.Blob.from_file_path(str(FIXTURES / name))))
        for text in strings:
            buf = hb.Buffer()
            buf.add_str(text)
            buf.guess_segment_properties()
            hb.shape(font, buf)
            glyphs = " ".join(f"{i.codepoint}:{i.cluster}" for i in buf.glyph_infos)
            lines.append(f"shape {name} {text} {glyphs}")
    (FIXTURES / "aat_morx_parity.expected").write_text("\n".join(lines) + "\n", newline="\n")


if __name__ == "__main__":
    varc()
    morx()
