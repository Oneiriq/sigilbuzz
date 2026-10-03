#!/usr/bin/env python3
"""Writes tests/fixtures/variable_vertical.expected from HarfBuzz.

The expectations behind `tests/variable_vertical_parity.rs`: HarfBuzz's
glyphs, advances and offsets for texts shaped top to bottom with variable
fixture fonts at several `wght` values, set as design coordinates the way
`hb_font_set_variations` takes them. The offsets carry each glyph's vertical
origin, which HarfBuzz takes:

- from `VORG`, moved by the `VVAR` vertical origin delta
  (`vorg`: `noto_sans_kr_vf_vertical_subset.otf`);

One record per line, fields separated by spaces:

    <group> <wght> <code points> <glyph> <glyph> ...

where `<code points>` are hex values joined by commas, and each `<glyph>` is
`gid,x_advance,y_advance,x_offset,y_offset`.

Run (needs uharfbuzz; HarfBuzz 14.5.0 through uharfbuzz 0.56.2 wrote the
committed file):

    uv run --no-project --with uharfbuzz==0.56.2 python tests/tools/variable_vertical_expected.py
"""

from __future__ import annotations

from pathlib import Path

import uharfbuzz as hb

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"
OUT_PATH = FIXTURES / "variable_vertical.expected"

# Every character the subset maps, alone and together. Its wght axis runs
# from 100 (the default) to 900; the per mille sign and U+2170 have VVAR
# vertical origin deltas.
NOTO_KR_TEXTS = [chr(c) for c in (0x20, 0x2030, 0x2170, 0x3001, 0x3002, 0x300C, 0x300D, 0xAC00)]
NOTO_KR_TEXTS.append("".join(NOTO_KR_TEXTS))

GROUPS = [
    (
        "vorg",
        "noto_sans_kr_vf_vertical_subset.otf",
        [100.0, 250.0, 400.0, 555.0, 700.0, 900.0],
        NOTO_KR_TEXTS,
    ),
]


def num(v: float) -> str:
    return f"{v:.4f}".rstrip("0").rstrip(".")


def shape(face: hb.Face, wght: float, text: str) -> str:
    font = hb.Font(face)
    font.set_variations({"wght": wght})
    buf = hb.Buffer()
    buf.add_str(text)
    buf.guess_segment_properties()
    buf.direction = "ttb"
    hb.shape(font, buf, {})
    return " ".join(
        f"{i.codepoint},{p.x_advance},{p.y_advance},{p.x_offset},{p.y_offset}"
        for i, p in zip(buf.glyph_infos, buf.glyph_positions)
    )


def main() -> None:
    lines = [
        "# HarfBuzz " + hb.version_string() + ", top to bottom;",
        "# regenerate with tests/tools/variable_vertical_expected.py.",
    ]
    for group, font_file, weights, texts in GROUPS:
        face = hb.Face((FIXTURES / font_file).read_bytes())
        for wght in weights:
            for text in texts:
                cps = ",".join(f"{ord(c):04X}" for c in text)
                lines.append(f"{group} {num(wght)} {cps} {shape(face, wght, text)}")
    OUT_PATH.write_bytes(("\n".join(lines) + "\n").encode())


if __name__ == "__main__":
    main()
