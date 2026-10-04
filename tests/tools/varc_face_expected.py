#!/usr/bin/env python3
"""Record HarfBuzz's outlines for tests/fixtures/varc_face.ttf.

Writes tests/fixtures/varc_face.expected: the outline of every glyph
the VARC coverage names, at the eight locations of
varc_parity.expected, in its format. Build the font with
tests/tools/build_varc_face_fixtures.py first.

Run:
    uv run --no-project --with uharfbuzz==0.56.2 --with fonttools python tests/tools/varc_face_expected.py
"""

from __future__ import annotations

import sys
from pathlib import Path

import uharfbuzz as hb
from fontTools.ttLib import TTFont

sys.path.insert(0, str(Path(__file__).resolve().parent))
from build_varc_face_fixtures import NO_RECORD, VARC_NAMES  # noqa: E402
from varc_morx_parity_expected import LOCATIONS, outline  # noqa: E402

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"


def main() -> None:
    path = FIXTURES / "varc_face.ttf"
    order = TTFont(path).getGlyphOrder()
    face = hb.Face(hb.Blob.from_file_path(str(path)))
    lines = [
        f"# HarfBuzz {hb.version_string()} (uharfbuzz {hb.__version__}) outlines of varc_face.ttf.",
        "# outline <location> <gid> <glyph> <ops>: M/L x y, Q cx cy x y, C ..., Z.",
    ]
    drawn = set()
    for loc in LOCATIONS:
        font = hb.Font(face)
        if loc != "default":
            font.set_variations({kv.split("=")[0]: float(kv.split("=")[1]) for kv in loc.split(",")})
        for name in VARC_NAMES + [NO_RECORD]:
            gid = order.index(name)
            ops = outline(font, gid)
            if ops:
                drawn.add(name)
            lines.append(f"outline {loc} {gid} {name} {ops}")
    # Every glyph but the one without a record draws something
    # somewhere, so none of them is checked only against an empty
    # outline.
    assert NO_RECORD not in drawn
    missing = [n for n in VARC_NAMES if n not in drawn]
    assert not missing, missing
    (FIXTURES / "varc_face.expected").write_text("\n".join(lines) + "\n", newline="\n")


if __name__ == "__main__":
    main()
