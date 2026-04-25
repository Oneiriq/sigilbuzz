#!/usr/bin/env python3
"""Synthesise a small CID-keyed CFF1 fixture used by the real-font
round-trip tests in `crates/sigilbuzz-subset/tests/real_cff_round_trip.rs`.

Real OFL CID-keyed CFF1 fonts in the wild are CJK and large (Source Han
Sans, Noto Serif CJK SC) — far past the 200 KB fixture ceiling
sigilbuzz holds itself to. Rather than vendor a multi-MB blob, this
script builds a minimal CID-keyed CFF1 by hand:

- 6 glyphs: `.notdef` plus five Latin uppercase outlines for
  U+0041..U+0045 (`A` through `E`). Each non-`.notdef` glyph is a
  single `endchar` after a no-op rmoveto.
- 2 Font DICTs in the FDArray, with FDSelect routing
  `{.notdef, A, B}` to FD 0 and `{C, D, E}` to FD 1. Each FD carries
  its own Private DICT; FD 1 also carries its own local-subr INDEX,
  so the rebuild exercises both per-FD Subr renumbering and the FD
  renumber path.
- A small global-subr INDEX (3 globals); FD 0's `A` charstring calls
  global subr 0 to exercise cross-FD subroutine sharing (closes #138's
  caveat for real-fixture coverage); FD 1's `C` charstring calls FD 1's
  local subr 0.
- Top DICT carries ROS / CIDCount / charset / CharStrings / FDArray /
  FDSelect operators in the standard CID-keyed shape.
- The whole SFNT is assembled by hand (CFF, cmap, hmtx, hhea, maxp,
  head, name, OS/2, post) so fontTools' CFF integration doesn't try to
  reflow our hand-built body.

The resulting fixture is well under 2 KB — comfortably under the
workspace's 200 KB-per-fixture cap.

Run:

    python3 tests/tools/build_cid_cff1_fixture.py

Output: `tests/fonts/CidCff1Synthetic.otf`.
"""

from __future__ import annotations

import struct
from pathlib import Path

OUT_PATH = (
    Path(__file__).resolve().parent.parent / "fonts" / "CidCff1Synthetic.otf"
)

UPEM = 1000

# Glyph order. Glyph 0 is .notdef; the rest map to U+0041..U+0045.
GLYPH_ORDER = [".notdef", "A", "B", "C", "D", "E"]
ADVANCES = [600, 600, 620, 640, 660, 680]

# FDSelect routing: gids 0..2 → FD 0, gids 3..5 → FD 1.
FD_SELECT = [0, 0, 0, 1, 1, 1]
N_FDS = 2


# ---------- CFF DICT / INDEX primitives ----------

def encode_dict_int(value: int) -> bytes:
    """CFF DICT integer encoding (Adobe TN 5176 §4)."""
    if -107 <= value <= 107:
        return bytes([value + 139])
    if 108 <= value <= 1131:
        v = value - 108
        return bytes([(v >> 8) + 247, v & 0xFF])
    if -1131 <= value <= -108:
        v = -value - 108
        return bytes([(v >> 8) + 251, v & 0xFF])
    if -32768 <= value <= 32767:
        return bytes([28]) + struct.pack(">h", value)
    return bytes([29]) + struct.pack(">i", value)


def encode_dict_offset_placeholder() -> bytes:
    """5-byte int operand we'll patch later. Always reserves 5 bytes."""
    return bytes([29, 0, 0, 0, 0])


def patch_dict_offset(data: bytearray, slot: int, value: int) -> None:
    data[slot] = 29
    struct.pack_into(">i", data, slot + 1, value)


def encode_index(entries: list[bytes]) -> bytes:
    """CFF INDEX encoding (Adobe TN 5176 §5)."""
    count = len(entries)
    if count == 0:
        return struct.pack(">H", 0)
    total = sum(len(e) for e in entries)
    last_off = 1 + total
    if last_off <= 0xFF:
        off_size = 1
    elif last_off <= 0xFFFF:
        off_size = 2
    elif last_off <= 0xFFFFFF:
        off_size = 3
    else:
        off_size = 4

    def pack_off(v: int) -> bytes:
        return v.to_bytes(off_size, "big")

    out = bytearray()
    out += struct.pack(">H", count)
    out += bytes([off_size])
    acc = 1
    out += pack_off(acc)
    for e in entries:
        acc += len(e)
        out += pack_off(acc)
    for e in entries:
        out += e
    return bytes(out)


def emit_charset_format0(sids: list[int]) -> bytes:
    out = bytearray([0])
    for s in sids:
        out += struct.pack(">H", s)
    return bytes(out)


def emit_fd_select_format0(fds: list[int]) -> bytes:
    return bytes([0]) + bytes(fds)


# ---------- T2 charstring primitives ----------

def encode_t2_int(value: int) -> bytes:
    if -107 <= value <= 107:
        return bytes([value + 139])
    if 108 <= value <= 1131:
        v = value - 108
        return bytes([(v >> 8) + 247, v & 0xFF])
    if -1131 <= value <= -108:
        v = -value - 108
        return bytes([(v >> 8) + 251, v & 0xFF])
    if -32768 <= value <= 32767:
        return bytes([28]) + struct.pack(">h", value)
    raise ValueError(f"value {value} out of T2 short-int range")


def build_charstrings() -> list[bytes]:
    cs: list[bytes] = []
    cs.append(bytes([14]))  # .notdef → endchar.

    # FD 0's A charstring: a no-op rmoveto + a callgsubr to global 0
    # + endchar. The `callgsubr` exercises cross-FD subr sharing — FD
    # 1's C charstring also reaches a subr (its own local), and the
    # rewriter must renumber globals separately from per-FD locals.
    cs_a = bytearray()
    cs_a += encode_t2_int(0)
    cs_a += encode_t2_int(0)
    cs_a += bytes([21])  # rmoveto
    # callgsubr operand 0 → with global-subr bias = 107 (n_globals < 1240)
    # the absolute subr index is 0 + 107 = 107? No — bias is 107 for
    # n in [240, 33899) and 0 for n < 240; subr_bias semantics are
    # detailed in TN 5176 §16. For 3 globals the bias is *0* per CFF
    # spec; the operand is the absolute index. Encode operand 0.
    cs_a += encode_t2_int(0)
    cs_a += bytes([29])  # callgsubr
    cs_a += bytes([14])  # endchar
    cs.append(bytes(cs_a))

    cs_b = bytearray()
    cs_b += encode_t2_int(0)
    cs_b += encode_t2_int(0)
    cs_b += bytes([21])
    cs_b += bytes([14])
    cs.append(bytes(cs_b))

    # FD 1's C charstring: invokes local subr 0 (FD 1's local subrs
    # have only one entry, so operand = 0 - 0 = 0).
    cs_c = bytearray()
    cs_c += encode_t2_int(0)
    cs_c += encode_t2_int(0)
    cs_c += bytes([21])
    cs_c += encode_t2_int(0)
    cs_c += bytes([10])  # callsubr
    cs_c += bytes([14])
    cs.append(bytes(cs_c))

    cs_d = bytearray()
    cs_d += encode_t2_int(0)
    cs_d += encode_t2_int(0)
    cs_d += bytes([21])
    cs_d += bytes([14])
    cs.append(bytes(cs_d))

    cs_e = bytearray()
    cs_e += encode_t2_int(0)
    cs_e += encode_t2_int(0)
    cs_e += bytes([21])
    cs_e += bytes([14])
    cs.append(bytes(cs_e))

    return cs


def build_global_subrs() -> list[bytes]:
    return [bytes([14]), bytes([14]), bytes([14])]


def build_fd_local_subrs() -> list[list[bytes]]:
    return [[], [bytes([14])]]


def build_cff_body() -> bytes:
    charstrings = build_charstrings()
    globals_ = build_global_subrs()
    fd_locals = build_fd_local_subrs()

    header = bytes([1, 0, 4, 1])
    name_index = encode_index([b"CIDSynth"])
    string_index = encode_index([])
    global_subr_index = encode_index(globals_)

    cs_index = encode_index(charstrings)
    charset_sids = list(range(1, len(charstrings)))
    charset_bytes = emit_charset_format0(charset_sids)
    fd_select_bytes = emit_fd_select_format0(FD_SELECT)

    private_bodies: list[bytearray] = []
    private_subrs_slot: list[int | None] = []
    for locals_ in fd_locals:
        body = bytearray()
        body += encode_dict_int(0)
        body += bytes([20])  # defaultWidthX
        if locals_:
            slot = len(body)
            body += encode_dict_offset_placeholder()
            body += bytes([19])  # Subrs op
            private_subrs_slot.append(slot)
        else:
            private_subrs_slot.append(None)
        private_bodies.append(body)

    font_dict_bodies: list[bytearray] = []
    font_dict_priv_slots: list[tuple[int, int]] = []
    for _ in range(N_FDS):
        body = bytearray()
        size_slot = len(body)
        body += encode_dict_offset_placeholder()
        off_slot = len(body)
        body += encode_dict_offset_placeholder()
        body += bytes([18])
        font_dict_priv_slots.append((size_slot, off_slot))
        font_dict_bodies.append(body)

    fd_array_index = encode_index([bytes(b) for b in font_dict_bodies])

    top = bytearray()
    top += encode_dict_int(0)
    top += encode_dict_int(0)
    top += encode_dict_int(0)
    top += bytes([12, 0x1E])  # ROS
    top += encode_dict_int(len(charstrings))
    top += bytes([12, 0x22])  # CIDCount
    charset_slot = len(top)
    top += encode_dict_offset_placeholder()
    top += bytes([15])
    cs_slot = len(top)
    top += encode_dict_offset_placeholder()
    top += bytes([17])
    fd_array_slot = len(top)
    top += encode_dict_offset_placeholder()
    top += bytes([12, 0x24])
    fd_select_slot = len(top)
    top += encode_dict_offset_placeholder()
    top += bytes([12, 0x25])

    top_dict_index = encode_index([bytes(top)])
    total_top = 1 + len(top)
    top_off_size = 1 if total_top <= 0xFF else 2
    top_dict_body_offset_in_index = 2 + 1 + 2 * top_off_size

    out = bytearray()
    out += header
    out += name_index
    top_dict_index_start = len(out)
    out += top_dict_index
    top_dict_body_abs = top_dict_index_start + top_dict_body_offset_in_index
    out += string_index
    out += global_subr_index

    charset_abs = len(out)
    out += charset_bytes
    fd_select_abs = len(out)
    out += fd_select_bytes
    cs_abs = len(out)
    out += cs_index
    fd_array_abs = len(out)
    out += fd_array_index

    fd_index_off_size = 1
    fd_total = sum(len(b) for b in font_dict_bodies)
    if 1 + fd_total > 0xFF:
        fd_index_off_size = 2
    fd_index_data_start = 2 + 1 + (N_FDS + 1) * fd_index_off_size
    fd_body_offsets_in_index: list[int] = []
    acc = fd_index_data_start
    for body in font_dict_bodies:
        fd_body_offsets_in_index.append(acc)
        acc += len(body)

    per_fd_priv_abs: list[int] = []
    per_fd_priv_size: list[int] = []
    for fd_i, (body, locals_) in enumerate(zip(private_bodies, fd_locals)):
        priv_start = len(out)
        out += body
        if locals_:
            local_index = encode_index(locals_)
            local_abs = len(out)
            out += local_index
            relative = local_abs - priv_start
            slot = private_subrs_slot[fd_i]
            assert slot is not None
            patch_dict_offset(out, priv_start + slot, relative)
        per_fd_priv_abs.append(priv_start)
        per_fd_priv_size.append(len(body))

    patch_dict_offset(out, top_dict_body_abs + charset_slot, charset_abs)
    patch_dict_offset(out, top_dict_body_abs + cs_slot, cs_abs)
    patch_dict_offset(out, top_dict_body_abs + fd_array_slot, fd_array_abs)
    patch_dict_offset(out, top_dict_body_abs + fd_select_slot, fd_select_abs)

    for i in range(N_FDS):
        body_abs_in_out = fd_array_abs + fd_body_offsets_in_index[i]
        size_slot, off_slot = font_dict_priv_slots[i]
        patch_dict_offset(out, body_abs_in_out + size_slot, per_fd_priv_size[i])
        patch_dict_offset(out, body_abs_in_out + off_slot, per_fd_priv_abs[i])

    return bytes(out)


# ---------- SFNT helpers ----------

def build_head() -> bytes:
    head = bytearray()
    head += struct.pack(">I", 0x00010000)  # version 1.0
    head += struct.pack(">I", 0x00010000)  # fontRevision
    head += struct.pack(">I", 0)  # checkSumAdjustment (patched later)
    head += struct.pack(">I", 0x5F0F3CF5)  # magic
    head += struct.pack(">H", 0)  # flags
    head += struct.pack(">H", UPEM)  # unitsPerEm
    head += struct.pack(">q", 0)  # created
    head += struct.pack(">q", 0)  # modified
    head += struct.pack(">h", 0)  # xMin
    head += struct.pack(">h", -200)  # yMin
    head += struct.pack(">h", 700)  # xMax
    head += struct.pack(">h", 800)  # yMax
    head += struct.pack(">H", 0)  # macStyle
    head += struct.pack(">H", 8)  # lowestRecPPEM
    head += struct.pack(">h", 2)  # fontDirectionHint
    head += struct.pack(">h", 0)  # indexToLocFormat
    head += struct.pack(">h", 0)  # glyphDataFormat
    return bytes(head)


def build_hhea(num_h_metrics: int) -> bytes:
    h = bytearray()
    h += struct.pack(">I", 0x00010000)  # version
    h += struct.pack(">h", 800)  # ascender
    h += struct.pack(">h", -200)  # descender
    h += struct.pack(">h", 100)  # lineGap
    h += struct.pack(">H", max(ADVANCES))  # advanceWidthMax
    h += struct.pack(">h", 0)  # minLeftSideBearing
    h += struct.pack(">h", 0)  # minRightSideBearing
    h += struct.pack(">h", max(ADVANCES))  # xMaxExtent
    h += struct.pack(">h", 1)  # caretSlopeRise
    h += struct.pack(">h", 0)  # caretSlopeRun
    h += struct.pack(">h", 0)  # caretOffset
    for _ in range(4):
        h += struct.pack(">h", 0)  # reserved (4 shorts)
    h += struct.pack(">h", 0)  # metricDataFormat
    h += struct.pack(">H", num_h_metrics)
    return bytes(h)


def build_maxp_v05(num_glyphs: int) -> bytes:
    m = bytearray()
    m += struct.pack(">I", 0x00005000)  # version 0.5
    m += struct.pack(">H", num_glyphs)
    return bytes(m)


def build_hmtx() -> bytes:
    out = bytearray()
    for adv in ADVANCES:
        out += struct.pack(">Hh", adv, 0)
    return bytes(out)


def build_cmap() -> bytes:
    """Format 4 cmap mapping U+0041..U+0045 → gids 1..5 plus 0xFFFF tail."""
    header = struct.pack(">HHHHI", 0, 1, 3, 1, 12)
    seg_count = 2
    seg_count_x2 = seg_count * 2
    search_range = 4
    entry_selector = 1
    range_shift = 0
    length = 14 + 2 * seg_count_x2 + 2 + 2 + 2 * seg_count
    sub = bytearray()
    sub += struct.pack(
        ">HHHHHHH",
        4,
        length,
        0,
        seg_count_x2,
        search_range,
        entry_selector,
        range_shift,
    )
    sub += struct.pack(">HH", 0x45, 0xFFFF)  # endCount
    sub += struct.pack(">H", 0)  # reservedPad
    sub += struct.pack(">HH", 0x41, 0xFFFF)  # startCount
    delta_a = (1 - 0x41) & 0xFFFF
    sub += struct.pack(">HH", delta_a, 1)  # idDelta
    sub += struct.pack(">HH", 0, 0)  # idRangeOffset
    return bytes(header) + bytes(sub)


def build_post_format3() -> bytes:
    p = bytearray()
    p += struct.pack(">I", 0x00030000)  # version 3.0
    p += struct.pack(">i", 0)  # italicAngle
    p += struct.pack(">h", 0)  # underlinePosition
    p += struct.pack(">h", 0)  # underlineThickness
    p += struct.pack(">I", 1)  # isFixedPitch
    p += struct.pack(">I", 0)  # minMemType42
    p += struct.pack(">I", 0)  # maxMemType42
    p += struct.pack(">I", 0)  # minMemType1
    p += struct.pack(">I", 0)  # maxMemType1
    return bytes(p)


def build_minimal_name() -> bytes:
    # Single name record (familyName) so OS/2 + name parsers tolerate.
    family = "CidCff1Synthetic".encode("utf-16-be")
    n = bytearray()
    n += struct.pack(">H", 0)  # version
    n += struct.pack(">H", 1)  # count
    storage_off = 6 + 12
    n += struct.pack(">H", storage_off)
    # NameRecord: platform=3 encoding=1 language=0x409 nameID=1
    # length=len(family) offset=0
    n += struct.pack(">HHHHHH", 3, 1, 0x409, 1, len(family), 0)
    n += family
    return bytes(n)


def build_minimal_os2() -> bytes:
    o = bytearray()
    o += struct.pack(">H", 4)  # version 4
    o += struct.pack(">h", 600)  # xAvgCharWidth
    o += struct.pack(">H", 400)  # usWeightClass
    o += struct.pack(">H", 5)  # usWidthClass
    o += struct.pack(">H", 0)  # fsType
    o += struct.pack(">h", 600)  # ySubscriptXSize
    o += struct.pack(">h", 600)  # ySubscriptYSize
    o += struct.pack(">h", 0)  # ySubscriptXOffset
    o += struct.pack(">h", 75)  # ySubscriptYOffset
    o += struct.pack(">h", 600)  # ySuperscriptXSize
    o += struct.pack(">h", 600)  # ySuperscriptYSize
    o += struct.pack(">h", 0)  # ySuperscriptXOffset
    o += struct.pack(">h", 350)  # ySuperscriptYOffset
    o += struct.pack(">h", 50)  # yStrikeoutSize
    o += struct.pack(">h", 250)  # yStrikeoutPosition
    o += struct.pack(">h", 0)  # sFamilyClass
    o += bytes(10)  # panose
    o += struct.pack(">IIII", 0, 0, 0, 0)  # ulUnicodeRange1..4
    o += b"\0\0\0\0"  # achVendID
    o += struct.pack(">H", 0x40)  # fsSelection
    o += struct.pack(">H", 0x41)  # usFirstCharIndex
    o += struct.pack(">H", 0x45)  # usLastCharIndex
    o += struct.pack(">h", 800)  # sTypoAscender
    o += struct.pack(">h", -200)  # sTypoDescender
    o += struct.pack(">h", 100)  # sTypoLineGap
    o += struct.pack(">H", 900)  # usWinAscent
    o += struct.pack(">H", 200)  # usWinDescent
    o += struct.pack(">II", 0, 0)  # ulCodePageRange1/2
    o += struct.pack(">h", 500)  # sxHeight
    o += struct.pack(">h", 700)  # sCapHeight
    o += struct.pack(">H", 0)  # usDefaultChar
    o += struct.pack(">H", 0x20)  # usBreakChar
    o += struct.pack(">H", 1)  # usMaxContext
    return bytes(o)


def calc_table_checksum(data: bytes) -> int:
    pad = (4 - (len(data) % 4)) % 4
    padded = data + b"\0" * pad
    s = 0
    for i in range(0, len(padded), 4):
        s = (s + struct.unpack(">I", padded[i : i + 4])[0]) & 0xFFFFFFFF
    return s


def build_sfnt(tables: list[tuple[bytes, bytes]]) -> bytes:
    tables = sorted(tables, key=lambda t: t[0])
    n = len(tables)
    # Compute search range / entry selector / range shift.
    # Largest power of 2 <= n.
    largest_pow2 = 1
    while largest_pow2 * 2 <= n:
        largest_pow2 *= 2
    search_range = largest_pow2 * 16
    entry_selector = 0
    while (1 << entry_selector) < largest_pow2:
        entry_selector += 1
    if largest_pow2 != 1 and (1 << entry_selector) > largest_pow2:
        entry_selector -= 1
    range_shift = n * 16 - search_range

    out = bytearray()
    out += struct.pack(">I", 0x4F54544F)  # 'OTTO'
    out += struct.pack(">HHHH", n, search_range, entry_selector, range_shift)

    header_len = 12 + n * 16
    # Lay out table data sequentially with 4-byte padding between.
    data_offsets: list[int] = []
    data_lens: list[int] = []
    cur = header_len
    for _, body in tables:
        data_offsets.append(cur)
        data_lens.append(len(body))
        cur += len(body)
        cur = (cur + 3) & ~3

    # Directory records.
    for (tag, body), off, length in zip(tables, data_offsets, data_lens):
        cs = calc_table_checksum(body)
        out += tag
        out += struct.pack(">I", cs)
        out += struct.pack(">I", off)
        out += struct.pack(">I", length)

    # Pad the directory header to 4-byte alignment (already 4-byte
    # aligned because each record is 16 bytes).
    while len(out) < header_len:
        out.append(0)

    # Table bodies, padded to 4 bytes.
    for body in (b for (_, b) in tables):
        out += body
        while len(out) % 4 != 0:
            out.append(0)

    # Patch head.checkSumAdjustment.
    head_idx = next(i for i, (t, _) in enumerate(tables) if t == b"head")
    head_off = data_offsets[head_idx]
    # Whole-font checksum.
    pad = (4 - (len(out) % 4)) % 4
    padded = bytes(out) + b"\0" * pad
    s = 0
    for i in range(0, len(padded), 4):
        s = (s + struct.unpack(">I", padded[i : i + 4])[0]) & 0xFFFFFFFF
    adjustment = (0xB1B0AFBA - s) & 0xFFFFFFFF
    struct.pack_into(">I", out, head_off + 8, adjustment)

    return bytes(out)


def main() -> None:
    cff_body = build_cff_body()
    head = build_head()
    hhea = build_hhea(len(GLYPH_ORDER))
    maxp = build_maxp_v05(len(GLYPH_ORDER))
    hmtx = build_hmtx()
    cmap = build_cmap()
    post = build_post_format3()
    name = build_minimal_name()
    os2 = build_minimal_os2()

    sfnt = build_sfnt(
        [
            (b"CFF ", cff_body),
            (b"OS/2", os2),
            (b"cmap", cmap),
            (b"head", head),
            (b"hhea", hhea),
            (b"hmtx", hmtx),
            (b"maxp", maxp),
            (b"name", name),
            (b"post", post),
        ]
    )

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    OUT_PATH.write_bytes(sfnt)
    print(f"Wrote {OUT_PATH} ({len(sfnt)} bytes)")


if __name__ == "__main__":
    main()
