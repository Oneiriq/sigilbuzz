#!/usr/bin/env python3
"""Writes tests/fixtures/vertical_shaping.expected from HarfBuzz.

The expectations behind `tests/variable_vertical_parity.rs` and
`tests/vertical_shaping.rs`: HarfBuzz's glyphs, advances and offsets for
texts shaped top to bottom, for variable fonts at several `wght` values set
as design coordinates the way `hb_font_set_variations` takes them. The
offsets carry each glyph's vertical origin, which HarfBuzz takes:

- from `VORG`, moved by the `VVAR` vertical origin delta (group `vorg`:
  `noto_sans_kr_vf_vertical_subset.otf`);
- in a `glyf` font with `vmtx`, from the top phantom point, moved by its
  `gvar` delta (group `phantom`: `hahmlet_gvar_subset.ttf` with the `vhea`,
  `vmtx` and `gvar` that `phantom_font` below adds, built the same way as in
  `tests/variable_vertical_parity.rs`);
- otherwise from the glyph extents, varied, centered in the ascender to
  descender span (group `extents`: `hahmlet_gvar_subset.ttf` and
  `rubik_vf.ttf`, which have no `vmtx`; group `static`: static fonts,
  including CFF outlines).

One record per line, fields separated by spaces:

    <group> <font> <wght> <code points> <glyph> <glyph> ...

where `<wght>` is `default` when no variations are set, `<code points>` are
hex values joined by commas, and each `<glyph>` is
`gid,x_advance,y_advance,x_offset,y_offset`.

Run (needs uharfbuzz; HarfBuzz 14.5.0 through uharfbuzz 0.56.2 wrote the
committed file):

    uv run --no-project --with uharfbuzz==0.56.2 python tests/tools/vertical_shaping_expected.py
"""

from __future__ import annotations

import struct
from pathlib import Path

import uharfbuzz as hb

TESTS = Path(__file__).resolve().parent.parent
FIXTURES = TESTS / "fixtures"
OUT_PATH = FIXTURES / "vertical_shaping.expected"


def tables(font: bytes) -> dict[bytes, bytes]:
    num_tables = struct.unpack(">H", font[4:6])[0]
    out = {}
    for i in range(num_tables):
        rec = 12 + 16 * i
        tag = font[rec : rec + 4]
        offset, length = struct.unpack(">II", font[rec + 8 : rec + 16])
        out[tag] = font[offset : offset + length]
    return out


def with_tables(font: bytes, extra: dict[bytes, bytes]) -> bytes:
    """`font` with `extra` tables added or replaced, laid out as the
    `with_tables` helper of the Rust tests lays them out."""
    records = tables(font)
    records.update(extra)
    tags = sorted(records)
    out = bytearray(font[:4])
    out += struct.pack(">H", len(tags)) + bytes(6)
    offset = 12 + 16 * len(tags)
    for tag in tags:
        body = records[tag]
        out += tag + struct.pack(">III", 0, offset, len(body))
        offset += (len(body) + 3) & ~3
    for tag in tags:
        out += records[tag]
        out += bytes(-len(out) % 4)
    return bytes(out)


def phantom_font() -> bytes:
    """Hahmlet's subset with a `vhea` and `vmtx` (advance 1000, top side
    bearing 50 for all 12 glyphs) and a `gvar` of its own, which at the
    top of the weight axis moves the top and bottom phantom points of `O`
    (glyph 4) by 40 and -25 units."""
    font = (FIXTURES / "hahmlet_gvar_subset.ttf").read_bytes()
    t = tables(font)
    vhea = bytearray(36)
    vhea[0:4] = struct.pack(">I", 0x00011000)
    vhea[34:36] = struct.pack(">H", 12)
    vmtx = struct.pack(">Hh", 1000, 50) * 12
    # O's point count: the last end point of its contours, plus one.
    loca_long = struct.unpack(">h", t[b"head"][50:52])[0] == 1
    loca = t[b"loca"]
    if loca_long:
        start = struct.unpack(">I", loca[16:20])[0]
    else:
        start = 2 * struct.unpack(">H", loca[8:10])[0]
    glyf = t[b"glyf"]
    contours = struct.unpack(">h", glyf[start : start + 2])[0]
    end_pts = start + 10 + 2 * (contours - 1)
    top = struct.unpack(">H", glyf[end_pts : end_pts + 2])[0] + 1 + 2
    # One axis, no shared tuples, 12 glyphs with long offsets; only glyph
    # 4 has data: one tuple with an embedded peak and private point
    # numbers.
    data_array = 20 + 4 * 13
    gvar = struct.pack(">HHHHIHHI", 1, 0, 1, 0, data_array, 12, 1, data_array)
    data = bytearray(struct.pack(">HHHHH", 1, 10, 0, 0xA000, 0x4000))
    serialized = len(data)
    data += bytes([2, 0x81]) + struct.pack(">HH", top, 1)
    data += bytes([0x81, 0x41]) + struct.pack(">hh", 40, -25)
    data[4:6] = struct.pack(">H", len(data) - serialized)
    for gid in range(13):
        gvar += struct.pack(">I", len(data) if gid > 4 else 0)
    gvar += data
    return with_tables(font, {b"vhea": bytes(vhea), b"vmtx": vmtx, b"gvar": gvar})


def chars(*cps: int) -> list[str]:
    """Each code point alone, then all of them together."""
    texts = [chr(c) for c in cps]
    return texts + ["".join(texts)]


HAHMLET_TEXTS = chars(0x20, 0x41, 0x4F, 0xC1, 0xC5, 0x3143, 0xBE60, 0xBE75, 0xBED0)
HAHMLET_WEIGHTS = [100.0, 250.0, 400.0, 650.0, 900.0]

# (group, font file, font bytes or None to read the file, weights, texts)
GROUPS = [
    (
        "vorg",
        "noto_sans_kr_vf_vertical_subset.otf",
        None,
        [100.0, 250.0, 400.0, 555.0, 700.0, 900.0],
        chars(0x20, 0x2030, 0x2170, 0x3001, 0x3002, 0x300C, 0x300D, 0xAC00),
    ),
    ("phantom", "hahmlet_gvar_subset.ttf", phantom_font(), HAHMLET_WEIGHTS, HAHMLET_TEXTS),
    ("extents", "hahmlet_gvar_subset.ttf", None, HAHMLET_WEIGHTS, HAHMLET_TEXTS),
    (
        "extents",
        "rubik_vf.ttf",
        None,
        [300.0, 493.75, 700.0, 900.0],
        ["Hello", "AVAT", "g\u00E9 x", "\u20AC5"],
    ),
    ("static", "opensans_regular.ttf", None, [None], ["AVAT", "A b"]),
    ("static", "../fonts/SourceCodePro-Latin-Subset.otf", None, [None], ["Abc"]),
    (
        "static",
        "../fonts/NotoSansMongolian-Regular.ttf",
        None,
        [None],
        ["\u1820\u1885", "\u1820\u1821\u1822"],
    ),
]


def num(v: float) -> str:
    return f"{v:.4f}".rstrip("0").rstrip(".")


def shape(face: hb.Face, wght: float | None, text: str) -> str:
    font = hb.Font(face)
    if wght is not None:
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
        "# regenerate with tests/tools/vertical_shaping_expected.py.",
    ]
    for group, font_file, data, weights, texts in GROUPS:
        face = hb.Face(data if data is not None else (FIXTURES / font_file).read_bytes())
        name = font_file.rsplit("/", 1)[-1]
        for wght in weights:
            w = "default" if wght is None else num(wght)
            for text in texts:
                cps = ",".join(f"{ord(c):04X}" for c in text)
                lines.append(f"{group} {name} {w} {cps} {shape(face, wght, text)}")
    OUT_PATH.write_bytes(("\n".join(lines) + "\n").encode())


if __name__ == "__main__":
    main()
