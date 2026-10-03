#!/usr/bin/env python3
"""Writes tests/fixtures/hahmlet_gvar_subset.expected from HarfBuzz.

The expectations behind `tests/gvar_iup_parity.rs`: for every glyph of
`tests/fixtures/hahmlet_gvar_subset.ttf` at several `wght` values, HarfBuzz's
outline points, glyph extents, and horizontal advance, the advance both with
the font's `HVAR` and with `HVAR` hidden (its table tag renamed to `HVAX`), so
HarfBuzz falls back to the glyph's varied phantom points.

One record per line, fields separated by spaces:

    outline <wght> <gid> <x0> <y0> <x1> <y1> ...   every point HarfBuzz draws
    extents <wght> <gid> <x_bearing> <y_bearing> <width> <height>
    advance <wght> <gid> <with HVAR> <HVAR hidden>

Run (needs uharfbuzz; HarfBuzz 14.5.0 through uharfbuzz 0.56.2 wrote the
committed file):

    uv run --no-project --with uharfbuzz==0.56.2 python tests/tools/gvar_iup_expected.py
"""

from __future__ import annotations

import struct
from pathlib import Path

import uharfbuzz as hb

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"
FONT_PATH = FIXTURES / "hahmlet_gvar_subset.ttf"
OUT_PATH = FIXTURES / "hahmlet_gvar_subset.expected"
WEIGHTS = [100.0, 250.0, 400.0, 650.0, 900.0]


def hide_hvar(data: bytes) -> bytes:
    """Renames the HVAR table record so HarfBuzz no longer finds it."""
    out = bytearray(data)
    num_tables = struct.unpack(">H", out[4:6])[0]
    for i in range(num_tables):
        rec = 12 + 16 * i
        if out[rec : rec + 4] == b"HVAR":
            out[rec : rec + 4] = b"HVAX"
    return bytes(out)


class PointPen:
    """Collects every point a fontTools-style pen receives."""

    def __init__(self) -> None:
        self.points: list[tuple[float, float]] = []

    def moveTo(self, p):  # noqa: N802 (fontTools pen protocol)
        self.points.append(p)

    def lineTo(self, p):  # noqa: N802
        self.points.append(p)

    def qCurveTo(self, *ps):  # noqa: N802
        self.points.extend(ps)

    def curveTo(self, *ps):  # noqa: N802
        self.points.extend(ps)

    def closePath(self):  # noqa: N802
        pass

    def endPath(self):  # noqa: N802
        pass


def num(v: float) -> str:
    return f"{v:.4f}".rstrip("0").rstrip(".")


def main() -> None:
    data = FONT_PATH.read_bytes()
    face = hb.Face(data)
    hidden_face = hb.Face(hide_hvar(data))
    lines = [
        "# HarfBuzz " + hb.version_string() + " on hahmlet_gvar_subset.ttf;",
        "# regenerate with tests/tools/gvar_iup_expected.py.",
    ]
    for wght in WEIGHTS:
        font = hb.Font(face)
        font.set_variations({"wght": wght})
        hidden = hb.Font(hidden_face)
        hidden.set_variations({"wght": wght})
        w = num(wght)
        for gid in range(face.glyph_count):
            pen = PointPen()
            font.draw_glyph_with_pen(gid, pen)
            coords = " ".join(f"{num(x)} {num(y)}" for x, y in pen.points)
            lines.append(f"outline {w} {gid} {coords}".rstrip())
            e = font.get_glyph_extents(gid)
            lines.append(f"extents {w} {gid} {e.x_bearing} {e.y_bearing} {e.width} {e.height}")
            lines.append(
                f"advance {w} {gid} {font.get_glyph_h_advance(gid)} "
                f"{hidden.get_glyph_h_advance(gid)}"
            )
    OUT_PATH.write_bytes(("\n".join(lines) + "\n").encode())


if __name__ == "__main__":
    main()
