#!/usr/bin/env python3
"""Synthesise a tiny TTF that exercises composite *anchor-mode* with a
phantom-point reference (advance-width origin).

PR #80 added phantom-point resolution for composite glyphs whose
`ARGS_ARE_XY_VALUES` flag is clear and whose anchor index falls past
the parent's contour-point count: the four phantom points (pp1..pp4).
Neither Open Sans nor Amiri exercises this path, so the code shipped
"dead-but-correct on the bundled corpus, exercised only by a synthetic
unit test". This fixture flips it to a real font.

# Constraint: ttf-parser ignores anchor mode

ttf-parser 0.25.1 (the parity oracle) does NOT resolve anchor-mode
composite components: when `ARGS_ARE_XY_VALUES` is clear, it leaves
the translation at (0, 0) and skips the anchor bytes. To keep the
parity test green while still exercising sigilbuzz's phantom-point
branch, the fixture is hand-crafted so that the phantom-resolved
translation also equals (0, 0). That is, parent's pp2 lines up with
the child's anchor point so `parent.pp2 - child[anchor] == (0, 0)`.

# Fixture layout

UPEM = 1000.

- `.notdef` (gid 0): rectangle (used as a placeholder).
- `base`    (gid 1): rectangle (0, 0) -> (500, 500). 1 contour, 4
  points. Advance = 500, lsb = 0, so pp1 = (0, 0), pp2 = (500, 0).
- `mark`    (gid 2): triangle anchored at its own point 0 = (500, 0).
  3 points: (500, 0), (550, 0), (500, 50). Advance = 100, lsb = 500.
- `combo`   (gid 3): composite of two components. Advance = 500,
  lsb = 0, xMin = 0 -> combo.pp2 = (500, 0).
    1. `base`  in XY mode at translation (0, 0).
    2. `mark`  in anchor mode. arg1 = 5 (= 4 contour points + pp2
       phantom index 1; pp2 lives at `numContourPoints + 1`).
       arg2 = 0.
       NOTE: phantom resolution looks up the phantom on the *parent*
       composite (combo itself), not on the first child component.
       Resolved translation = combo.pp2 - mark[0] = (500, 0) - (500,
       0) = (0, 0).

`combo` consequently flattens to the union of `base` and `mark` with
no displacement. ttf-parser produces the same outline by its
zero-translation default; sigilbuzz takes the phantom-anchor branch,
computes pp2 from hmtx, and emits the matching outline.

The integration test in `tests/outline_parity.rs`:
- Confirms by direct byte inspection that `combo` has at least one
  anchor-mode component (so the path is genuinely exercised).
- Asserts every glyph's `Face::glyph_outline` matches ttf-parser's
  callbacks within a 1e-2 epsilon.

# How we sidestep fontTools' bounds-recompute

fontTools refuses to compile a composite glyph whose `firstPt` index
exceeds the parent's real contour-point count: its bounds path reads
`allCoords[firstPt]` and IndexErrors. We therefore:

1. Build the font with `combo` in XY mode (so fontTools is happy).
2. Save to disk.
3. Re-open the saved bytes, parse the SFNT directory, locate the
   `glyf` and `loca` table bytes, replace the composite glyph body
   with the anchor-mode form, fix `loca`, recompute table checksums
   and the head checkSumAdjustment.

The post-processing is a small SFNT writer keyed on standard offsets,
so the result is a fully valid TTF that any conformant parser can
load. Output: ~1 KB.

Run:
    python3 tests/tools/build_phantom_anchor_fixture.py
"""

from __future__ import annotations

import struct
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib.tables._g_l_y_f import (
    Glyph,
    GlyphComponent,
)

OUT_PATH = (
    Path(__file__).resolve().parent.parent / "fixtures" / "phantom_anchor.ttf"
)

UPEM = 1000

# Composite component flag bits.
ARG_1_AND_2_ARE_WORDS = 0x0001
ARGS_ARE_XY_VALUES = 0x0002
MORE_COMPONENTS = 0x0020


def build_base_rect() -> Glyph:
    """4-point square (0, 0) -> (500, 500)."""
    pen = TTGlyphPen(None)
    pen.moveTo((0, 0))
    pen.lineTo((500, 0))
    pen.lineTo((500, 500))
    pen.lineTo((0, 500))
    pen.closePath()
    return pen.glyph()


def build_mark_triangle() -> Glyph:
    """3-point triangle whose point 0 is at (500, 0)."""
    pen = TTGlyphPen(None)
    pen.moveTo((500, 0))
    pen.lineTo((550, 0))
    pen.lineTo((500, 50))
    pen.closePath()
    return pen.glyph()


def build_composite_xy_placeholder() -> Glyph:
    """Composite of `base` + `mark` both in plain XY mode at (0, 0).
    fontTools is happy to compile this; we patch it post-save."""
    glyph = Glyph()
    glyph.numberOfContours = -1
    glyph.components = []

    base_comp = GlyphComponent()
    base_comp.glyphName = "base"
    base_comp.x = 0
    base_comp.y = 0
    base_comp.flags = ARGS_ARE_XY_VALUES
    glyph.components.append(base_comp)

    mark_comp = GlyphComponent()
    mark_comp.glyphName = "mark"
    mark_comp.x = 0
    mark_comp.y = 0
    mark_comp.flags = ARGS_ARE_XY_VALUES
    glyph.components.append(mark_comp)

    return glyph


# ---------------------------------------------------------------------
# SFNT post-processing.
# ---------------------------------------------------------------------

def _checksum(data: bytes) -> int:
    """OpenType table checksum: sum of u32 big-endian words mod 2^32."""
    s = 0
    # Pad to multiple of 4.
    if len(data) % 4 != 0:
        data = data + b"\x00" * (4 - len(data) % 4)
    for i in range(0, len(data), 4):
        s = (s + struct.unpack(">I", data[i:i + 4])[0]) & 0xFFFFFFFF
    return s


def _build_combo_anchor_mode_body() -> bytes:
    """Hand-assemble combo's glyph body in anchor mode.

    Layout:
      i16  numberOfContours = -1
      i16  xMin / yMin / xMax / yMax
      // component 1: base, XY mode (1-byte args), MORE_COMPONENTS
      u16  flags
      u16  glyphIndex = 1
      i8   arg1 = 0
      i8   arg2 = 0
      // component 2: mark, anchor mode (last)
      u16  flags = 0
      u16  glyphIndex = 2
      u8   arg1 = 5  (= numContourPoints(4) + 1 -> pp2)
      u8   arg2 = 0  (mark's first contour point)
    """
    body = bytearray()
    body += struct.pack(">hhhhh", -1, 0, 0, 550, 500)
    body += struct.pack(">HH", ARGS_ARE_XY_VALUES | MORE_COMPONENTS, 1)
    body += struct.pack(">bb", 0, 0)
    body += struct.pack(">HH", 0, 2)
    body += struct.pack(">BB", 5, 0)
    return bytes(body)


def _patch_sfnt_combo(font_bytes: bytes) -> bytes:
    """Locate the glyf/loca tables in the SFNT, swap combo's body for
    the anchor-mode form, regenerate loca, and fix all checksums."""
    # SFNT header: 4 sfnt tag + u16 numTables + u16 searchRange +
    # u16 entrySelector + u16 rangeShift = 12 bytes.
    num_tables = struct.unpack(">H", font_bytes[4:6])[0]

    # Parse table directory: numTables x 16-byte records.
    table_dir = {}  # tag -> (offset_in_file_of_record, content_offset, length)
    for i in range(num_tables):
        rec_off = 12 + i * 16
        tag = font_bytes[rec_off:rec_off + 4].decode("latin-1")
        _checksum_field, content_off, length = struct.unpack(
            ">III", font_bytes[rec_off + 4:rec_off + 16]
        )
        table_dir[tag] = (rec_off, content_off, length)

    # Determine loca format: head.indexToLocFormat at offset 50 in head.
    head_rec = table_dir["head"]
    head = font_bytes[head_rec[1]:head_rec[1] + head_rec[2]]
    loc_format = struct.unpack(">h", head[50:52])[0]

    # Read existing loca offsets.
    loca_rec = table_dir["loca"]
    loca = font_bytes[loca_rec[1]:loca_rec[1] + loca_rec[2]]
    if loc_format == 0:
        offsets = [
            struct.unpack(">H", loca[i * 2:i * 2 + 2])[0] * 2
            for i in range(len(loca) // 2)
        ]
    else:
        offsets = [
            struct.unpack(">I", loca[i * 4:i * 4 + 4])[0]
            for i in range(len(loca) // 4)
        ]

    # Glyph data slice for gid 3 (`combo`). offsets has numGlyphs + 1
    # entries; for short loca the values are pre-multiplied by 2.
    glyf_rec = table_dir["glyf"]
    glyf = font_bytes[glyf_rec[1]:glyf_rec[1] + glyf_rec[2]]

    new_combo_body = _build_combo_anchor_mode_body()

    # Replace combo's slice. gid 3 spans offsets[3]..offsets[4].
    new_glyf = bytearray()
    new_offsets = []
    for gid in range(4):
        new_offsets.append(len(new_glyf))
        if gid == 3:
            body = new_combo_body
        else:
            body = glyf[offsets[gid]:offsets[gid + 1]]
        # Strip any trailing pad bytes from the original slice: we
        # re-pad uniformly below.
        new_glyf += body
        if len(new_glyf) & 1:
            new_glyf += b"\x00"
    new_offsets.append(len(new_glyf))

    # Re-emit loca in the same format.
    if loc_format == 0:
        # Short: requires offset / 2 < 0x10000.
        for off in new_offsets:
            assert off % 2 == 0
            assert off // 2 < 0x10000
        new_loca = b"".join(struct.pack(">H", off // 2) for off in new_offsets)
    else:
        new_loca = b"".join(struct.pack(">I", off) for off in new_offsets)

    new_glyf = bytes(new_glyf)

    # We assume the saved layout has glyf and loca contiguous (TTFont
    # writes them adjacent for short loca builds). Even if not, our
    # rewrite preserves length: combo's body shrank from "two XY
    # components, 24 B" to "one XY + one anchor, 22 B" -> 2 fewer
    # bytes. Pad new_glyf with zeros to keep total length identical
    # so we don't have to relocate later tables.
    old_glyf_len = glyf_rec[2]
    new_loca_len = len(new_loca)
    old_loca_len = loca_rec[2]
    assert new_loca_len == old_loca_len, (
        f"loca length changed: {old_loca_len} -> {new_loca_len}"
    )

    if len(new_glyf) < old_glyf_len:
        new_glyf = new_glyf + b"\x00" * (old_glyf_len - len(new_glyf))
    elif len(new_glyf) > old_glyf_len:
        # Must not happen for our fixture; raise loudly.
        raise RuntimeError(
            f"new glyf grew: {old_glyf_len} -> {len(new_glyf)}"
        )

    # Splice the new bytes in.
    out = bytearray(font_bytes)
    out[glyf_rec[1]:glyf_rec[1] + old_glyf_len] = new_glyf
    out[loca_rec[1]:loca_rec[1] + old_loca_len] = new_loca

    # Recompute checksums for glyf, loca, and head's checkSumAdjustment.
    new_glyf_csum = _checksum(new_glyf[:glyf_rec[2]])
    new_loca_csum = _checksum(new_loca)
    out[glyf_rec[0] + 4:glyf_rec[0] + 8] = struct.pack(">I", new_glyf_csum)
    out[loca_rec[0] + 4:loca_rec[0] + 8] = struct.pack(">I", new_loca_csum)

    # head.checkSumAdjustment: zero it, compute font-wide checksum,
    # write 0xB1B0AFBA - sum.
    head_off = table_dir["head"][1]
    out[head_off + 8:head_off + 12] = b"\x00\x00\x00\x00"
    font_sum = _checksum(bytes(out))
    adjustment = (0xB1B0AFBA - font_sum) & 0xFFFFFFFF
    out[head_off + 8:head_off + 12] = struct.pack(">I", adjustment)

    return bytes(out)


def build() -> None:
    glyph_order = [".notdef", "base", "mark", "combo"]

    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(glyph_order)
    fb.setupCharacterMap({
        ord("B"): "base",
        ord("M"): "mark",
        ord("C"): "combo",
    })
    glyphs = {
        ".notdef": build_base_rect(),
        "base": build_base_rect(),
        "mark": build_mark_triangle(),
        "combo": build_composite_xy_placeholder(),
    }
    fb.setupGlyf(glyphs)
    fb.setupHorizontalMetrics({
        ".notdef": (500, 0),
        "base": (500, 0),
        "mark": (100, 500),
        # combo's pp2 = xMin - lsb + advance = 0 - 0 + 500 = 500. The
        # anchor-mode component points at this pp2; mark's anchor
        # point 0 is at (500, 0) so the resolved translation is (0, 0).
        "combo": (500, 0),
    })
    fb.setupHorizontalHeader(ascent=800, descent=-200)
    fb.setupNameTable({
        "familyName": "SigilbuzzPhantomAnchor",
        "styleName": "Regular",
    })
    fb.setupOS2()
    fb.setupPost()
    # Force short loca so our patcher can stay in 16-bit-offset mode.
    fb.font["head"].indexToLocFormat = 0
    # Ensure maxp reflects the composite's component count.
    if hasattr(fb.font["maxp"], "maxComponentElements"):
        fb.font["maxp"].maxComponentElements = max(
            getattr(fb.font["maxp"], "maxComponentElements", 0), 2
        )
        fb.font["maxp"].maxComponentDepth = max(
            getattr(fb.font["maxp"], "maxComponentDepth", 0), 1
        )

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    fb.save(str(OUT_PATH))

    # Post-process: replace combo with anchor-mode bytes.
    raw = OUT_PATH.read_bytes()
    patched = _patch_sfnt_combo(raw)
    OUT_PATH.write_bytes(patched)
    print(f"Wrote {OUT_PATH} ({OUT_PATH.stat().st_size} bytes)")


if __name__ == "__main__":
    build()
