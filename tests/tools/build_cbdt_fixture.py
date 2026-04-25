#!/usr/bin/env python3
"""Synthesise a tiny CBDT/CBLC color-bitmap font.

Real Noto Color Emoji ships at 12+ MB; for parser tests we just need
a structurally valid pair of CBDT + CBLC tables with one strike, one
glyph, one PNG. fontTools rejects empty CBDT/CBLC scaffolding when
no image is present, so we splice both tables in as raw bytes via
``DefaultTable`` — mirroring the AAT fixture builder.

The output font ``cbdt_synthetic.ttf`` carries:

- Two glyphs (``.notdef``, ``smile``) with trivial rectangle outlines.
- A cmap mapping U+0041 → smile.
- A CBLC strike at 32 ppem with one IndexSubTable in format 17
  (small metrics + PNG data, the most common CBDT variant today).
- A CBDT record carrying a 67-byte 1×1 transparent PNG.

Run:
    python3 tests/tools/build_cbdt_fixture.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib.tables.DefaultTable import DefaultTable

OUT_PATH = Path(__file__).resolve().parent.parent / "fixtures" / "cbdt_synthetic.ttf"
UPEM = 1000

# Smallest valid PNG: 1x1 RGBA, fully transparent. 67 bytes.
TINY_PNG = bytes.fromhex(
    "89504e470d0a1a0a"  # signature
    "0000000d49484452"  # IHDR length=13
    "00000001000000010806000000"  # 1x1 RGBA
    "1f15c489"
    "0000000d4944415478da6300010000000500010d0a2db40000000049454e44ae426082"
)


def build_rect():
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((100, 0))
    pen.lineTo((100, 100))
    pen.lineTo((0, 100))
    pen.closePath()
    return pen.glyph()


def build_cbdt(png: bytes) -> bytes:
    """CBDT layout for one format-17 record at offset 4 (just past header)."""
    out = bytearray()
    # Header: u16 major=3, u16 minor=0.
    out += struct.pack(">HH", 3, 0)
    # Format 17: SmallGlyphMetrics (5B), u32 dataLen, data.
    # height=10, width=10, bx=0, by=10, advance=12.
    out += struct.pack(">BBbbB", 10, 10, 0, 10, 12)
    out += struct.pack(">I", len(png))
    out += png
    return bytes(out)


def build_cblc(image_data_offset: int, image_size: int) -> bytes:
    """CBLC layout: header + 1 BitmapSize + IndexSubTableArray + IndexSubTable.

    image_data_offset / image_size describe the single record in CBDT;
    we use index format 1 (variable-metric, u32 offsets) so each CBDT
    record carries its own SmallGlyphMetrics inline.
    """
    out = bytearray()
    header_len = 8
    bitmap_size_len = 48
    index_array_off = header_len + bitmap_size_len  # = 56
    # IndexSubTableArray: 1 entry × 8 bytes.
    array_len = 8
    sub_off_relative = array_len  # subtable starts right after the array
    # IndexSubTable header: 8 B + (count+1) × 4 B for fmt 1.
    # Two glyphs covered (gids 0 and 1), three offsets.
    sub_header = 8
    sub_payload = (1 + 1) * 4  # 2 glyphs → 3 offsets ... wait: count + 1
    # Actually: count = lastGid - firstGid + 1, here 1 (only gid 1 in
    # this strike), so 2 offsets. Make first sentinel 0, second sentinel
    # equals image size.
    count = 1  # number of glyphs in range firstGid..lastGid inclusive
    sub_payload = (count + 1) * 4

    index_tables_size = array_len + sub_header + sub_payload

    # CBLC header.
    out += struct.pack(">HHI", 3, 0, 1)  # major, minor, numSizes

    # BitmapSize record.
    out += struct.pack(">III", index_array_off, index_tables_size, 1)
    out += struct.pack(">I", 0)  # colorRef
    # hori SbitLineMetrics (12 B).
    out += struct.pack(
        ">bbBbbbbbbbBB", 12, -4, 12, 1, 0, 0, 0, 0, 12, -4, 0, 0
    )
    # vert SbitLineMetrics (12 B).
    out += struct.pack(
        ">bbBbbbbbbbBB", 12, -4, 12, 1, 0, 0, 0, 0, 12, -4, 0, 0
    )
    # startGlyph=1, endGlyph=1, ppemX=32, ppemY=32, bitDepth=32, flags=1.
    out += struct.pack(">HHBBBb", 1, 1, 32, 32, 32, 1)

    # IndexSubTableArray entry: firstGid=1, lastGid=1, additionalOffset=8.
    out += struct.pack(">HHI", 1, 1, sub_off_relative)

    # IndexSubTable header: indexFormat=1, imageFormat=17, imageDataOffset.
    out += struct.pack(">HHI", 1, 17, image_data_offset)
    # Offsets array: gid 1 starts at 0, sentinel at image_size.
    out += struct.pack(">II", 0, image_size)

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
    fb.setupNameTable({"familyName": "SigilbuzzCBDT", "styleName": "Regular"})
    fb.setupOS2()
    fb.setupPost()

    # Build CBDT first so we know the offset/length of the record.
    cbdt_bytes = build_cbdt(TINY_PNG)
    # CBDT header is 4 bytes; the first (only) glyph record starts at 4.
    image_data_offset = 4
    image_size = len(cbdt_bytes) - 4  # rest of the table

    cblc_bytes = build_cblc(image_data_offset, image_size)

    cbdt_table = DefaultTable("CBDT")
    cbdt_table.data = cbdt_bytes
    fb.font["CBDT"] = cbdt_table

    cblc_table = DefaultTable("CBLC")
    cblc_table.data = cblc_bytes
    fb.font["CBLC"] = cblc_table

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    fb.save(str(OUT_PATH))
    size = OUT_PATH.stat().st_size
    print(f"Wrote {OUT_PATH} ({size} bytes)")


if __name__ == "__main__":
    main()
