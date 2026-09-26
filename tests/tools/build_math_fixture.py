#!/usr/bin/env python3
"""Synthesise a tiny TTF that carries an OpenType `MATH` table.

Real math fonts (STIX 2 Math, Latin Modern Math, Asana Math) are 150 KB
to 700 KB unsubset, far too heavy to vendor for the integration test
sigilbuzz needs. Instead we hand-roll a minimum-viable font with three
glyphs and a hand-laid `MATH` table that exercises every subtable
parser:

- `MathConstants`: populated with realistic non-zero values so the
  integration test can sanity-check the parser end-to-end.
- `MathGlyphInfo`: italics correction for gid 1 ("italic-f"-shaped
  glyph), top-accent attachment for the same, extended-shape coverage
  for gid 2 (the tall integral), and a top-right kern for gid 1.
- `MathVariants`: gid 2 (the integral, mapped to U+222B via cmap)
  has a vertical glyph construction with two progressive variants
  and an assembly composed of three parts (top, extender, bottom).

The font has 4 glyphs:
  gid 0 = .notdef
  gid 1 = math italic 'f'  (cmap: U+1D453 -> 𝑓)
  gid 2 = ∫                (cmap: U+222B)
  gid 3 = an extender variant for the integral

Glyph outlines are empty: the integration test only consumes the
MATH table via the Face accessor, never the outlines.

Output: tests/fixtures/math_synthetic.ttf
Public-domain / no third-party content.
"""
from __future__ import annotations

import os
import struct
import sys
from pathlib import Path


def u8(v: int) -> bytes:
    return struct.pack(">B", v & 0xFF)


def u16(v: int) -> bytes:
    return struct.pack(">H", v & 0xFFFF)


def i16(v: int) -> bytes:
    return struct.pack(">h", v)


def u32(v: int) -> bytes:
    return struct.pack(">I", v & 0xFFFFFFFF)


def i32(v: int) -> bytes:
    return struct.pack(">i", v)


def pad4(b: bytes) -> bytes:
    return b + b"\x00" * ((4 - (len(b) % 4)) % 4)


def calc_table_checksum(data: bytes) -> int:
    pad = (4 - len(data) % 4) % 4
    padded = data + b"\x00" * pad
    s = 0
    for i in range(0, len(padded), 4):
        s = (s + struct.unpack(">I", padded[i : i + 4])[0]) & 0xFFFFFFFF
    return s


# -- MathValueRecord (4 bytes: i16 value + u16 device offset)
def mvr(v: int, device_off: int = 0) -> bytes:
    return i16(v) + u16(device_off)


# -- Coverage Format 1
def coverage_fmt1(gids: list[int]) -> bytes:
    out = u16(1) + u16(len(gids))
    for g in gids:
        out += u16(g)
    return out


# -- MathConstants subtable (4 i16/u16 + 51 MathValueRecords + 1 i16)
def build_math_constants() -> bytes:
    out = bytearray()
    out += i16(80)        # scriptPercentScaleDown
    out += i16(60)        # scriptScriptPercentScaleDown
    out += u16(1500)      # delimitedSubFormulaMinHeight
    out += u16(1800)      # displayOperatorMinHeight

    # 51 MathValueRecord fields. Realistic-ish numbers so the
    # integration test can spot-check non-zeros.
    fields = [
        150,   # mathLeading
        250,   # axisHeight
        780,   # accentBaseHeight
        700,   # flattenedAccentBaseHeight
        200,   # subscriptShiftDown
        500,   # subscriptTopMax
        100,   # subscriptBaselineDropMin
        450,   # superscriptShiftUp
        420,   # superscriptShiftUpCramped
        100,   # superscriptBottomMin
        100,   # superscriptBaselineDropMax
        80,    # subSuperscriptGapMin
        400,   # superscriptBottomMaxWithSubscript
        50,    # spaceAfterScript
        100,   # upperLimitGapMin
        500,   # upperLimitBaselineRiseMin
        100,   # lowerLimitGapMin
        300,   # lowerLimitBaselineDropMin
        300,   # stackTopShiftUp
        500,   # stackTopDisplayStyleShiftUp
        300,   # stackBottomShiftDown
        500,   # stackBottomDisplayStyleShiftDown
        100,   # stackGapMin
        200,   # stackDisplayStyleGapMin
        300,   # stretchStackTopShiftUp
        300,   # stretchStackBottomShiftDown
        100,   # stretchStackGapAboveMin
        100,   # stretchStackGapBelowMin
        500,   # fractionNumeratorShiftUp
        700,   # fractionNumeratorDisplayStyleShiftUp
        500,   # fractionDenominatorShiftDown
        700,   # fractionDenominatorDisplayStyleShiftDown
        80,    # fractionNumeratorGapMin
        160,   # fractionNumDisplayStyleGapMin
        50,    # fractionRuleThickness
        80,    # fractionDenominatorGapMin
        160,   # fractionDenomDisplayStyleGapMin
        80,    # skewedFractionHorizontalGap
        80,    # skewedFractionVerticalGap
        50,    # overbarVerticalGap
        50,    # overbarRuleThickness
        50,    # overbarExtraAscender
        100,   # underbarVerticalGap
        50,    # underbarRuleThickness
        50,    # underbarExtraDescender
        100,   # radicalVerticalGap
        200,   # radicalDisplayStyleVerticalGap
        50,    # radicalRuleThickness
        50,    # radicalExtraAscender
        50,    # radicalKernBeforeDegree
        -50,   # radicalKernAfterDegree
    ]
    assert len(fields) == 51
    for v in fields:
        out += mvr(v)
    out += i16(65)  # radicalDegreeBottomRaisePercent
    return bytes(out)


# -- MathItalicsCorrectionInfo / MathTopAccentAttachment shape
def value_info_one(gid: int, value: int) -> bytes:
    """Coverage + 1-record value array. Coverage is appended after the
    record array, so the table is self-contained; coverageOffset = 8."""
    out = u16(8)         # coverageOffset
    out += u16(1)        # count
    out += mvr(value)    # the one record
    out += coverage_fmt1([gid])
    return out


# -- MathKern (heights[n] then kerns[n+1], MathValueRecord each).
def build_math_kern(heights: list[int], kerns: list[int]) -> bytes:
    assert len(kerns) == len(heights) + 1
    out = u16(len(heights))
    for h in heights:
        out += mvr(h)
    for k in kerns:
        out += mvr(k)
    return out


# -- MathKernInfo with one record covering one gid, top-right populated.
def build_math_kern_info(gid: int) -> bytes:
    """Returns a MathKernInfo subtable with one entry for `gid`,
    a top-right kern table and zeros for the other three corners.
    """
    kern = build_math_kern([200, 400], [10, 20, 30])
    cov = coverage_fmt1([gid])
    # Layout (offsets from start of MathKernInfo):
    #  0..2   coverageOffset
    #  2..4   mathKernCount = 1
    #  4..12  MathKernInfoRecord (TR/TL/BR/BL)
    #  12..   coverage
    #  ..     kern table
    cov_off = 12
    tr_off = cov_off + len(cov)
    out = u16(cov_off) + u16(1)
    out += u16(tr_off) + u16(0) + u16(0) + u16(0)
    out += cov
    out += kern
    return out


# -- MathGlyphInfo: italics + top accent for gid 1, extended shape for
# gid 2, kern info for gid 1.
def build_math_glyph_info() -> bytes:
    italics = value_info_one(1, 60)         # italics correction = 60
    top_acc = value_info_one(1, 400)        # top accent attachment x = 400
    ext_shape = coverage_fmt1([2])          # gid 2 (∫) is extended
    kern_info = build_math_kern_info(1)

    # Header is 8 bytes: 4 x Offset16.
    italics_off = 8
    top_off = italics_off + len(italics)
    ext_off = top_off + len(top_acc)
    kern_off = ext_off + len(ext_shape)

    out = u16(italics_off) + u16(top_off) + u16(ext_off) + u16(kern_off)
    out += italics + top_acc + ext_shape + kern_info
    return out


# -- MathGlyphConstruction: variant_count progressive variants then
# optional GlyphAssembly with parts.
def build_glyph_construction(
    variants: list[tuple[int, int]],
    assembly_parts: list[tuple[int, int, int, int, int]] | None = None,
    italics_correction: int = 0,
) -> bytes:
    asm_off = 0
    out = bytearray()
    # Header is 4 bytes; variants are 4 bytes each.
    if assembly_parts is not None:
        asm_off = 4 + len(variants) * 4
    out += u16(asm_off)
    out += u16(len(variants))
    for vg, av in variants:
        out += u16(vg) + u16(av)
    if assembly_parts is not None:
        out += mvr(italics_correction)
        out += u16(len(assembly_parts))
        for gid, sc, ec, fa, flags in assembly_parts:
            out += u16(gid) + u16(sc) + u16(ec) + u16(fa) + u16(flags)
    return bytes(out)


# -- MathVariants: gid 2 (∫) gets a vertical construction with two
# variants and a 3-part assembly (top, extender, bottom).
def build_math_variants() -> bytes:
    # Header is 10 bytes; vertical-construction-offset array follows
    # (1 entry, 2 bytes); horizontal array (0 entries here).
    cons = build_glyph_construction(
        variants=[(2, 1000), (3, 2000)],
        assembly_parts=[
            (2, 200, 200, 1000, 0),   # top piece
            (3, 200, 200, 1500, 1),   # extender (PART_FLAG_EXTENDER)
            (2, 200, 200, 1000, 0),   # bottom piece
        ],
        italics_correction=120,
    )
    cov = coverage_fmt1([2])

    header_len = 10
    vert_offsets_len = 2
    horiz_offsets_len = 0
    base = header_len + vert_offsets_len + horiz_offsets_len

    cov_off = base
    cons_off = cov_off + len(cov)

    out = u16(64)            # minConnectorOverlap
    out += u16(cov_off)      # vertCoverageOffset
    out += u16(0)            # horizCoverageOffset (none)
    out += u16(1)            # vertGlyphCount
    out += u16(0)            # horizGlyphCount
    out += u16(cons_off)     # one vertical-construction offset
    out += cov
    out += cons
    return out


def build_math_table() -> bytes:
    """Glue the three subtables behind a 10-byte MATH header."""
    constants = build_math_constants()
    glyph_info = build_math_glyph_info()
    variants = build_math_variants()

    header_len = 10
    const_off = header_len
    gi_off = const_off + len(constants)
    var_off = gi_off + len(glyph_info)

    out = u16(1)        # majorVersion
    out += u16(0)       # minorVersion
    out += u16(const_off)
    out += u16(gi_off)
    out += u16(var_off)
    out += constants
    out += glyph_info
    out += variants
    return out


# -- The bare-minimum SFNT scaffolding ---------------------------------------

UPEM = 1000


def build_head() -> bytes:
    # 54 bytes
    out = i32(0x00010000)        # version 1.0
    out += i32(0x00010000)       # fontRevision
    out += u32(0)                # checksumAdjustment (filled later)
    out += u32(0x5F0F3CF5)       # magicNumber
    out += u16(0)                # flags
    out += u16(UPEM)
    out += u32(0) + u32(0)       # created (LONGDATETIME)
    out += u32(0) + u32(0)       # modified
    out += i16(0) + i16(0) + i16(0) + i16(0)  # xMin/yMin/xMax/yMax
    out += u16(0)                # macStyle
    out += u16(8)                # lowestRecPPEM
    out += i16(2)                # fontDirectionHint (deprecated)
    out += i16(0)                # indexToLocFormat (short = 0)
    out += i16(0)                # glyphDataFormat
    return bytes(out)


def build_hhea(num_glyphs: int) -> bytes:
    out = i32(0x00010000)         # version
    out += i16(800)               # ascent
    out += i16(-200)              # descent
    out += i16(0)                 # lineGap
    out += u16(1000)              # advanceWidthMax
    out += i16(0)                 # minLeftSideBearing
    out += i16(0)                 # minRightSideBearing
    out += i16(1000)              # xMaxExtent
    out += i16(1) + i16(0)        # caretSlopeRise/Run
    out += i16(0)                 # caretOffset
    out += i16(0) * 4             # 4 reserved
    out += i16(0)                 # metricDataFormat
    out += u16(num_glyphs)        # numberOfHMetrics
    return bytes(out)


def build_maxp(num_glyphs: int) -> bytes:
    return i32(0x00005000) + u16(num_glyphs)  # version 0.5


def build_hmtx(num_glyphs: int) -> bytes:
    out = b""
    for _ in range(num_glyphs):
        out += u16(500) + i16(0)  # advance, lsb
    return out


def build_cmap() -> bytes:
    """Format-4 cmap mapping U+222B -> gid 2 and U+1D453 (𝑓) -> gid 1.
    U+1D453 is in the SMP, so format 4 alone can't reach it; we
    cheat and remap the 'italic-f' to a BMP slot the integration
    test agrees on (e.g., U+0066 'f'). This is *only* a fixture.
    """
    # Map ranges for chars f (0x66) -> gid 1 and ∫ (0x222B) -> gid 2.
    # Glyph deltas: gid - char (mod 65536).
    # End/start/idDelta/idRangeOffset arrays:
    end = [0x0066, 0x222B, 0xFFFF]
    start = [0x0066, 0x222B, 0xFFFF]
    delta = [(1 - 0x0066) & 0xFFFF, (2 - 0x222B) & 0xFFFF, 1]
    idro = [0, 0, 0]
    seg_count = len(end)
    seg_count_x2 = seg_count * 2

    # Format 4 binary search params.
    search_range = 1
    while search_range * 2 <= seg_count:
        search_range *= 2
    search_range *= 2  # 2 * largest power of 2 <= seg_count
    entry_selector = 0
    sr = search_range // 2
    while sr > 1:
        sr //= 2
        entry_selector += 1
    range_shift = seg_count_x2 - search_range

    sub = bytearray()
    sub += u16(4)                       # format
    sub += u16(0)                       # length (filled below)
    sub += u16(0)                       # language
    sub += u16(seg_count_x2)
    sub += u16(search_range)
    sub += u16(entry_selector)
    sub += u16(range_shift)
    for v in end:
        sub += u16(v)
    sub += u16(0)                       # reservedPad
    for v in start:
        sub += u16(v)
    for v in delta:
        sub += u16(v)
    for v in idro:
        sub += u16(v)
    sub_len = len(sub)
    sub[2:4] = u16(sub_len)

    cmap = bytearray()
    cmap += u16(0)                      # version
    cmap += u16(1)                      # numTables
    cmap += u16(3) + u16(1)             # platformID = Microsoft, encoding = BMP UCS-2
    cmap += u32(12)                     # offset to subtable
    cmap += sub
    return bytes(cmap)


def build_loca(num_glyphs: int) -> bytes:
    # short loca: (num_glyphs + 1) u16 offsets in 2-byte units. All
    # zero -> every glyph is empty.
    out = b""
    for _ in range(num_glyphs + 1):
        out += u16(0)
    return out


def build_glyf() -> bytes:
    return b""  # all glyphs empty


def build_post() -> bytes:
    out = i32(0x00030000)            # version 3.0 (no glyph names)
    out += i32(0)                    # italicAngle
    out += i16(0) + i16(0)           # underlinePosition/Thickness
    out += u32(1)                    # isFixedPitch
    out += u32(0) * 4                # min/maxMemType42/Type1
    return out


def build_name() -> bytes:
    # Empty name table.
    return u16(0) + u16(0) + u16(6)  # format, count, stringOffset


def build_os2() -> bytes:
    # OS/2 v4, the bare minimum 96 bytes. None of the values matter
    # for sigilbuzz's MATH integration test.
    out = u16(4)                       # version
    out += i16(500)                    # xAvgCharWidth
    out += u16(400)                    # usWeightClass
    out += u16(5)                      # usWidthClass
    out += u16(0)                      # fsType
    out += i16(650)                    # ySubscriptXSize
    out += i16(700)                    # ySubscriptYSize
    out += i16(0)                      # ySubscriptXOffset
    out += i16(140)                    # ySubscriptYOffset
    out += i16(650)                    # ySuperscriptXSize
    out += i16(700)                    # ySuperscriptYSize
    out += i16(0)                      # ySuperscriptXOffset
    out += i16(480)                    # ySuperscriptYOffset
    out += i16(50)                     # yStrikeoutSize
    out += i16(258)                    # yStrikeoutPosition
    out += i16(0)                      # sFamilyClass
    out += b"\x00" * 10                # panose
    out += u32(0) + u32(0) + u32(0) + u32(0)  # ulUnicodeRange1..4
    out += b"FONT"                     # achVendID
    out += u16(0x40)                   # fsSelection (regular)
    out += u16(0)                      # usFirstCharIndex
    out += u16(0xFFFF)                 # usLastCharIndex
    out += i16(800)                    # sTypoAscender
    out += i16(-200)                   # sTypoDescender
    out += i16(200)                    # sTypoLineGap
    out += u16(800)                    # usWinAscent
    out += u16(200)                    # usWinDescent
    out += u32(0) + u32(0)             # ulCodePageRange1, 2
    out += i16(500)                    # sxHeight
    out += i16(700)                    # sCapHeight
    out += u16(0)                      # usDefaultChar
    out += u16(0x20)                   # usBreakChar
    out += u16(0)                      # usMaxContext
    return out


def assemble_sfnt(tables: dict[bytes, bytes]) -> bytes:
    # Required ordering by SFNT spec is alphabetical for offsets-only;
    # in practice every parser walks the directory anyway. Order is
    # arbitrary.
    items = sorted(tables.items(), key=lambda kv: kv[0])
    num_tables = len(items)

    # Compute searchRange / entrySelector / rangeShift.
    pow2 = 1
    while pow2 * 2 <= num_tables:
        pow2 *= 2
    search_range = pow2 * 16
    entry_selector = 0
    p = pow2
    while p > 1:
        p //= 2
        entry_selector += 1
    range_shift = num_tables * 16 - search_range

    header_len = 12 + num_tables * 16
    # Lay out tables back to back, 4-byte aligned.
    offsets = []
    cursor = header_len
    for tag, data in items:
        offsets.append((tag, cursor, len(data), data))
        cursor += len(pad4(data))

    # Build directory entries with checksums.
    out = bytearray()
    out += u32(0x00010000)        # sfntVersion (TrueType)
    out += u16(num_tables)
    out += u16(search_range)
    out += u16(entry_selector)
    out += u16(range_shift)
    for tag, off, length, data in offsets:
        cs = calc_table_checksum(data)
        out += tag
        out += u32(cs)
        out += u32(off)
        out += u32(length)
    for _, _, _, data in offsets:
        out += pad4(data)
    return bytes(out)


def main() -> int:
    here = Path(__file__).resolve().parent.parent
    out_path = here / "fixtures" / "math_synthetic.ttf"

    num_glyphs = 4

    tables = {
        b"OS/2": build_os2(),
        b"cmap": build_cmap(),
        b"glyf": build_glyf(),
        b"head": build_head(),
        b"hhea": build_hhea(num_glyphs),
        b"hmtx": build_hmtx(num_glyphs),
        b"loca": build_loca(num_glyphs),
        b"maxp": build_maxp(num_glyphs),
        b"name": build_name(),
        b"post": build_post(),
        b"MATH": build_math_table(),
    }
    sfnt = assemble_sfnt(tables)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_bytes(sfnt)
    print(f"wrote {out_path} ({len(sfnt)} bytes, {len(tables)} tables)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
