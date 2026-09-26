#!/usr/bin/env python3
"""Synthesise a tiny variable font with a `wght`-varying kern pair.

The output font `var_kern.ttf` carries:
- Two real glyphs ("A" and "V") plus `.notdef`, with hand-built
  TrueType outlines so the file stays under ~10 KB.
- An `fvar` axis `wght` with (min=400, default=400, max=900).
- An HVAR-like ItemVariationStore referenced from GDEF v1.3 that
  contributes a -100 design-unit delta to the (A, V) kern pair at
  wght=900 and zero at the default.
- A GPOS `kern` feature whose format-1 PairPos value record carries
  an `xAdvDevice` offset pointing at a VariationIndex pair. The
  default-instance kern is 0; at wght=900 the pair tightens by 100
  design units.

The fixture is intentionally minimal: the point is to exercise
sigilbuzz's feature-variation wiring, not to look good.
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables import otTables as ot
from fontTools.ttLib.tables.otBase import OTTableWriter

OUT_PATH = Path(__file__).resolve().parent.parent / "fixtures" / "var_kern.ttf"

UPEM = 1000

GLYPH_A = "A"
GLYPH_V = "V"


def build_glyph_outline(width_units):
    pen = TTGlyphPen(None)
    # Two-step diamond so the outline is valid but minimal.
    pen.moveTo((0, 0))
    pen.lineTo((width_units, 0))
    pen.lineTo((width_units, UPEM))
    pen.lineTo((0, UPEM))
    pen.closePath()
    return pen.glyph()


def build_variation_index_bytes(outer: int, inner: int) -> bytes:
    # VariationIndex: outer u16, inner u16, deltaFormat = 0x8000.
    return struct.pack(">HHH", outer, inner, 0x8000)


def main():
    glyphs_order = [".notdef", GLYPH_A, GLYPH_V]

    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(glyphs_order)
    fb.setupCharacterMap({ord("A"): GLYPH_A, ord("V"): GLYPH_V})

    # Minimal outlines. Both glyphs are 500-unit wide rectangles.
    pen_notdef = TTGlyphPen(None)
    pen_notdef.moveTo((0, 0))
    pen_notdef.lineTo((400, 0))
    pen_notdef.lineTo((400, 700))
    pen_notdef.lineTo((0, 700))
    pen_notdef.closePath()
    glyphs = {
        ".notdef": pen_notdef.glyph(),
        GLYPH_A: build_glyph_outline(500),
        GLYPH_V: build_glyph_outline(500),
    }
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({
        ".notdef": (500, 0),
        GLYPH_A: (500, 0),
        GLYPH_V: (500, 0),
    })
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200)
    fb.setupNameTable({"familyName": "SigilbuzzVarKern", "styleName": "Regular"})
    fb.setupPost()

    # fvar: one axis, `wght`, range 400..900 default 400.
    fb.setupFvar(
        axes=[("wght", 400.0, 400.0, 900.0, "Weight")],
        instances=[],
    )

    # Build a tiny ItemVariationStore by hand and paste it into a GDEF
    # v1.3 table alongside a valid GPOS PairPos format 1 subtable.
    font = fb.font

    # ItemVariationStore: one region (peak 1.0 on wght), one
    # ItemVariationData subtable carrying a single delta of -100.
    # We append this to GDEF below.
    def build_ivs() -> bytes:
        # format = 1, variationRegionListOffset (Offset32),
        # itemVariationDataCount = 1, itemVariationDataOffsets[1]
        header_size = 2 + 4 + 2 + 4  # 12 bytes
        # region list: u16 axisCount, u16 regionCount, then
        # regionCount x axisCount x (start, peak, end) F2DOT14.
        region_list = (
            struct.pack(">HH", 1, 1)  # axisCount, regionCount
            + struct.pack(">hhh", 0, 0x4000, 0x4000)  # start=0, peak=1, end=1
        )
        region_list_off = header_size
        # ivd subtable: u16 itemCount=1, u16 wordDeltaCount=1 (wide short),
        # u16 regionIndexCount=1, u16 regionIndex=0, i16 delta=-100.
        ivd = struct.pack(">HHHHh", 1, 1, 1, 0, -100)
        ivd_off = region_list_off + len(region_list)
        header = struct.pack(">H I H I", 1, region_list_off, 1, ivd_off)
        return header + region_list + ivd

    ivs_bytes = build_ivs()

    # Build the GPOS table: one `kern` feature with a PairPos format 1
    # subtable. The ValueRecord for the first glyph carries
    # valueFormat = X_ADVANCE | X_ADVANCE_DEVICE -> 4 bytes. The
    # device offset points at a VariationIndex table we embed in the
    # subtable tail. The X_ADVANCE i16 is 0 (default instance).

    # Coverage format 1: covers glyph 'A' (gid 1).
    def build_gpos() -> bytes:
        gid_a = 1
        gid_v = 2
        # PairPos format 1:
        #   u16 posFormat = 1
        #   Offset16 coverageOffset
        #   u16 valueFormat1 = X_ADVANCE | X_ADVANCE_DEVICE = 0x0044
        #   u16 valueFormat2 = 0
        #   u16 pairSetCount = 1
        #   Offset16 pairSetOffsets[1]
        # Then pair set #0:
        #   u16 pairValueCount = 1
        #   PairValueRecord:
        #     u16 secondGlyph = gid_v
        #     ValueRecord1:
        #       i16 x_advance = 0
        #       Offset16 x_advance_device -> VariationIndex at subtable-relative offset
        # Then coverage and variation index blobs appended.
        value_format1 = 0x0044
        value_format2 = 0
        vr1_size = 4  # X_ADVANCE (i16) + X_ADVANCE_DEVICE (Offset16)
        vr2_size = 0
        header_size = 2 + 2 + 2 + 2 + 2 + 2  # through pairSetOffsets[1]
        pair_set_off = header_size
        pair_set_size = 2 + 2 + vr1_size + vr2_size  # count + record
        coverage_off = pair_set_off + pair_set_size
        coverage = struct.pack(">HH", 1, 1) + struct.pack(">H", gid_a)
        # VariationIndex (6 bytes) at coverage_off + len(coverage).
        var_index_off = coverage_off + len(coverage)
        var_index = build_variation_index_bytes(0, 0)

        header = (
            struct.pack(">H", 1)                            # posFormat
            + struct.pack(">H", coverage_off)               # coverageOffset
            + struct.pack(">H", value_format1)
            + struct.pack(">H", value_format2)
            + struct.pack(">H", 1)                          # pairSetCount
            + struct.pack(">H", pair_set_off)               # pairSetOffsets[0]
        )
        pair_set = (
            struct.pack(">H", 1)                            # pairValueCount
            + struct.pack(">H", gid_v)                      # secondGlyph
            + struct.pack(">h", 0)                          # x_advance = 0
            + struct.pack(">H", var_index_off)              # x_advance_device
        )
        pairpos_subtable = header + pair_set + coverage + var_index

        # Now wrap this in GPOS header + LookupList + FeatureList + ScriptList.
        # Lookup with one subtable (the PairPos above). lookupType=2.
        # Offsets are relative to lookup start.
        # Lookup layout: u16 lookupType, u16 lookupFlag, u16 subtableCount,
        #   Offset16 subtableOffsets[1]
        lookup_size = 2 + 2 + 2 + 2
        lookup = (
            struct.pack(">HHH", 2, 0, 1) + struct.pack(">H", lookup_size)
            + pairpos_subtable
        )
        # LookupList: u16 lookupCount, Offset16 lookupOffsets[1]
        lookup_list_off = 4  # after u16 lookupCount + Offset16 offset
        lookup_list = struct.pack(">HH", 1, lookup_list_off) + lookup

        # FeatureList: u16 featureCount, FeatureRecord[1]
        # FeatureRecord: Tag + Offset16
        # Feature: u16 featureParamsOffset(0), u16 lookupIndexCount, u16 lookupListIndexes[1]
        feature_rec_size = 4 + 2  # Tag + Offset16
        feat_table_off = 2 + feature_rec_size  # after featureCount and FeatureRecord
        feature_table = struct.pack(">HHH", 0, 1, 0)  # featureParams=0, lookupIndexCount=1, lookup=0
        feature_list = (
            struct.pack(">H", 1)
            + b"kern"
            + struct.pack(">H", feat_table_off)
            + feature_table
        )

        # ScriptList: u16 scriptCount, ScriptRecord[1]
        # ScriptRecord: Tag + Offset16
        # Script: Offset16 defaultLangSysOffset, u16 langSysCount, LangSysRecord[0]
        # LangSys: u16 lookupOrderOffset(0), u16 requiredFeatureIndex=0xFFFF, u16 featureIndexCount=1, u16 featureIndexes[1]
        script_table_off = 2 + 6  # after scriptCount + scriptRecord
        lang_sys_off_rel_to_script = 4  # skip defaultLangSysOffset+langSysCount to LangSys body
        script_table = (
            struct.pack(">H", lang_sys_off_rel_to_script)  # defaultLangSysOffset rel to Script
            + struct.pack(">H", 0)                          # langSysCount
            + struct.pack(">HHH", 0, 0xFFFF, 1)             # LangSys header
            + struct.pack(">H", 0)                          # featureIndex[0]
        )
        script_list = (
            struct.pack(">H", 1)
            + b"DFLT"
            + struct.pack(">H", script_table_off)
            + script_table
        )

        # GPOS header: u16 major, u16 minor, Offset16 scriptList, Offset16 featureList, Offset16 lookupList
        gpos_header_size = 10
        script_list_off = gpos_header_size
        feature_list_off = script_list_off + len(script_list)
        lookup_list_off_in_gpos = feature_list_off + len(feature_list)
        gpos_header = struct.pack(
            ">HHHHH", 1, 0, script_list_off, feature_list_off, lookup_list_off_in_gpos
        )
        return gpos_header + script_list + feature_list + lookup_list

    gpos_bytes = build_gpos()

    # GDEF v1.3: u16 major, u16 minor, u16 x 5 (v1.2 offsets), u32 itemVarStoreOff
    # Version 1.3 header is 18 bytes. Keep glyphClassDef, etc. absent.
    gdef_header_size = 18
    gdef = (
        struct.pack(">HH", 1, 3)                # version 1.3
        + struct.pack(">HHHHH", 0, 0, 0, 0, 0)  # five u16 offsets, all null
        + struct.pack(">I", gdef_header_size)   # itemVarStoreOffset
        + ivs_bytes
    )

    # Inject raw tables via the ttLib low-level interface.
    font["GDEF"] = newRawTable("GDEF", gdef)
    font["GPOS"] = newRawTable("GPOS", gpos_bytes)

    # Flush.
    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    font.save(OUT_PATH)
    print(f"wrote {OUT_PATH} ({OUT_PATH.stat().st_size} bytes)")


def newRawTable(tag, data):
    from fontTools.ttLib.tables.DefaultTable import DefaultTable
    t = DefaultTable(tag)
    t.data = data
    return t


if __name__ == "__main__":
    sys.exit(main() or 0)
