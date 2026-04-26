#!/usr/bin/env python3
"""Synthesise a tiny `sbix` color-bitmap font.

Apple Color Emoji on macOS uses ``sbix`` but isn't redistributable.
For parser tests we just need a structurally valid sbix table with
one strike, one PNG entry. We hand-assemble the table bytes and
splice it in via fontTools' ``DefaultTable`` so this fixture builds
deterministically without leaning on fontTools' high-level sbix
encoder.

Output font ``sbix_synthetic.ttf`` carries:

- Two glyphs (``.notdef``, ``smile``) with rectangle outlines.
- A cmap mapping U+0041 → smile.
- An sbix table at version 1 with one strike at 32 ppem, two glyph
  data slots — gid 0 empty, gid 1 carrying a 1×1 PNG payload tagged
  ``'png '``.

Run:
    python3 tests/tools/build_sbix_fixture.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib.tables.DefaultTable import DefaultTable

OUT_PATH = Path(__file__).resolve().parent.parent / "fixtures" / "sbix_synthetic.ttf"
UPEM = 1000

def _make_png(width: int, height: int, rgba: bytes) -> bytes:
    """Builds a minimal but spec-valid PNG (real CRCs, real zlib).

    See `build_cbdt_fixture.py` for the rationale: the previous
    inline hex string was a malformed PNG.
    """
    import binascii
    import zlib

    def chunk(typ: bytes, data: bytes) -> bytes:
        out = struct.pack(">I", len(data)) + typ + data
        crc = binascii.crc32(typ + data) & 0xFFFFFFFF
        return out + struct.pack(">I", crc)

    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
    raw = b"".join(b"\x00" + rgba[y * width * 4 : (y + 1) * width * 4] for y in range(height))
    idat = chunk(b"IDAT", zlib.compress(raw))
    iend = chunk(b"IEND", b"")
    return sig + ihdr + idat + iend


TINY_PNG = _make_png(1, 1, b"\x00\x00\x00\x00")


def build_rect():
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((100, 0))
    pen.lineTo((100, 100))
    pen.lineTo((0, 100))
    pen.closePath()
    return pen.glyph()


def build_sbix(num_glyphs: int) -> bytes:
    """One strike at 32 ppem; gid 0 empty, gid 1 carries a PNG."""
    # Header: u16 version, u16 flags, u32 numStrikes, u32 strikeOffsets[1].
    header_len = 2 + 2 + 4 + 4
    out = bytearray()
    out += struct.pack(">HHI", 1, 1, 1)  # version=1, flags=1, numStrikes=1
    # Reserve strikeOffsets[0]; patch in below.
    strike_off_pos = len(out)
    out += struct.pack(">I", 0)

    # Strike header at out[len()] (= header_len).
    strike_start = len(out)
    out += struct.pack(">HH", 32, 72)  # ppem=32, ppi=72
    # glyphDataOffsets[numGlyphs + 1].
    offsets_pos = len(out)
    out += b"\x00" * (4 * (num_glyphs + 1))

    # Per-glyph entries.
    glyph_offsets = []
    base = len(out) - strike_start

    # gid 0: empty record (offset stays the same).
    glyph_offsets.append(base)

    # gid 1: 8-byte header + PNG.
    glyph_offsets.append(base)
    out += struct.pack(">hh", 0, 0)  # originOffset X/Y
    out += b"png "
    out += TINY_PNG
    base = len(out) - strike_start

    # Trailing sentinel.
    glyph_offsets.append(base)

    # Patch glyphDataOffsets.
    for i, off in enumerate(glyph_offsets):
        out[offsets_pos + i * 4 : offsets_pos + (i + 1) * 4] = struct.pack(">I", off)

    # Now that the strike sits at header_len, patch strikeOffsets[0].
    out[strike_off_pos : strike_off_pos + 4] = struct.pack(">I", strike_start)

    return bytes(out)


def main():
    glyph_order = [".notdef", "smile"]
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(glyph_order)
    fb.setupCharacterMap({ord("A"): "smile"})
    glyphs = {".notdef": build_rect(), "smile": build_rect()}
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({".notdef": (200, 0), "smile": (300, 0)})
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({"familyName": "SigilbuzzSbix", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()

    sbix_bytes = build_sbix(num_glyphs=2)
    sbix_table = DefaultTable("sbix")
    sbix_table.data = sbix_bytes
    fb.font["sbix"] = sbix_table

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    fb.save(str(OUT_PATH))
    size = OUT_PATH.stat().st_size
    print(f"Wrote {OUT_PATH} ({size} bytes)")


if __name__ == "__main__":
    main()
