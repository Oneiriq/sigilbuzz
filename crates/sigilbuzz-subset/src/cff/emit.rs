//! Byte-level emitter primitives: INDEX and DICT operand encoding,
//! charset, Encoding, and FDSelect writers, plus the FDSelect reader.

use alloc::vec::Vec;

use crate::SubsetError;

// ----------------------------------------------------------------------------
// Emitter primitives.
//
// These are the byte-serialization helpers a full CFF1 subsetter
// composes into a complete table emit. They live alongside the
// analysis layer so the eventual rewriter can call them directly
// without a cross-module dependency, and so unit tests cover the
// spec-mandated edge cases (bias boundaries, format auto-pick, deferred
// offset patching) in one place.
// ----------------------------------------------------------------------------

/// Encodes a CFF INDEX from a slice of entry payloads.
///
/// CFF INDEX layout per Adobe TN 5176 §5:
/// - `count: u16`
/// - if `count == 0`, no further bytes
/// - else: `offSize: u8`, then `count + 1` offsets each `offSize` bytes
///   wide, then concatenated entry data
///
/// Offsets are 1-based and relative to the byte just before the data
/// region, so the first offset is always `1` and the last offset is
/// `1 + total_data_len`. Picks the smallest valid `offSize` for the
/// total data length (1..=4 bytes).
#[must_use]
pub fn encode_index(entries: &[&[u8]]) -> Vec<u8> {
    let count = entries.len();
    let mut out = Vec::new();
    if count == 0 {
        out.extend_from_slice(&0u16.to_be_bytes());
        return out;
    }
    let total: usize = entries.iter().map(|e| e.len()).sum();
    let last_off = 1 + total;
    let off_size: u8 = if last_off <= 0xFF {
        1
    } else if last_off <= 0xFFFF {
        2
    } else if last_off <= 0x00FF_FFFF {
        3
    } else {
        4
    };
    out.extend_from_slice(&(count as u16).to_be_bytes());
    out.push(off_size);

    let write_off = |buf: &mut Vec<u8>, v: u32| {
        let bytes = v.to_be_bytes();
        let start = 4 - off_size as usize;
        buf.extend_from_slice(&bytes[start..]);
    };
    let mut acc: u32 = 1;
    write_off(&mut out, acc);
    for e in entries {
        acc += e.len() as u32;
        write_off(&mut out, acc);
    }
    for e in entries {
        out.extend_from_slice(e);
    }
    out
}

/// Encodes an integer in CFF DICT context (Top DICT / Private DICT
/// operand encoding). DICT integers use the same single-byte / two-
/// byte forms as the charstring encoding for small values, plus a
/// DICT-only `b0=29` 5-byte 32-bit form for full-range integers (the
/// 5-byte form in charstring context is `b0=255` 16.16 fixed). Returns
/// the smallest valid encoding.
///
/// DICT-context encoding table (TN 5176 §4):
/// - `b0` in 32..=246 -> single byte, value = `b0 - 139`
/// - `b0` in 247..=250 -> two-byte positive, value = (b0-247)*256 + b1 + 108
/// - `b0` in 251..=254 -> two-byte negative, value = -(b0-251)*256 - b1 - 108
/// - `b0` = 28 -> three-byte i16
/// - `b0` = 29 -> five-byte i32
#[must_use]
pub fn encode_dict_int(v: i32) -> Vec<u8> {
    if (-107..=107).contains(&v) {
        alloc::vec![(v + 139) as u8]
    } else if (108..=1131).contains(&v) {
        let v0 = v - 108;
        let b0 = ((v0 >> 8) + 247) as u8;
        let b1 = (v0 & 0xff) as u8;
        alloc::vec![b0, b1]
    } else if (-1131..=-108).contains(&v) {
        let v0 = -v - 108;
        let b0 = ((v0 >> 8) + 251) as u8;
        let b1 = (v0 & 0xff) as u8;
        alloc::vec![b0, b1]
    } else if (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(&v) {
        let bytes = (v as i16).to_be_bytes();
        alloc::vec![28u8, bytes[0], bytes[1]]
    } else {
        let mut out = alloc::vec![29u8];
        out.extend_from_slice(&v.to_be_bytes());
        out
    }
}

/// Pads a DICT integer encoding to a fixed minimum byte length by
/// upgrading to a wider form. Used by the Top DICT serializer when an
/// offset operand value is unknown at encode-time and we need to
/// reserve a fixed-width slot the patcher can overwrite.
///
/// The standard trick (matches what HarfBuzz and fontTools do) is to
/// reserve a 5-byte `b0=29` slot for every offset operator: every
/// offset fits in i32, the 5-byte form has a fixed width regardless of
/// the actual value, and patching is a straight copy of `value.to_be_bytes()`
/// into the operand bytes.
#[must_use]
pub fn encode_dict_offset_placeholder() -> Vec<u8> {
    // 5-byte i32 placeholder, value zero. Patcher overwrites bytes 1..5
    // with the real offset.
    alloc::vec![29u8, 0, 0, 0, 0]
}

/// Patches a placeholder offset slot emitted by
/// [`encode_dict_offset_placeholder`] in-place with the real value.
/// `slot_offset` is the byte offset of the leading `b0=29` byte within
/// the buffer. A slot that does not fit inside `buf` is left alone.
pub fn patch_dict_offset(buf: &mut [u8], slot_offset: usize, value: i32) {
    debug_assert_eq!(buf.get(slot_offset), Some(&29), "placeholder must be b0=29");
    let operand = slot_offset
        .checked_add(1)
        .and_then(|start| buf.get_mut(start..)?.get_mut(..4));
    if let Some(operand) = operand {
        operand.copy_from_slice(&value.to_be_bytes());
    }
}

/// Emits a charset in format 0 (per-gid 2-byte SID array, omitting
/// gid 0). For `n_glyphs` glyphs the table is `1 + 2*(n_glyphs - 1)`
/// bytes. `sids[i]` is the SID for gid `i+1` (gid 0 is implicit
/// `.notdef`).
#[must_use]
pub fn emit_charset_format0(sids: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 2 * sids.len());
    out.push(0);
    for sid in sids {
        out.extend_from_slice(&sid.to_be_bytes());
    }
    out
}

/// Emits a charset in format 2 (range records, 2-byte first SID +
/// 2-byte nLeft). Each contiguous run `[first, first+nLeft]` of SIDs
/// at consecutive gids becomes one record. `sids[i]` is the SID for
/// gid `i+1`.
#[must_use]
pub fn emit_charset_format2(sids: &[u16]) -> Vec<u8> {
    let mut out = alloc::vec![2u8];
    let mut i = 0;
    while i < sids.len() {
        let first = sids[i];
        let mut j = i + 1;
        // Extend the run while consecutive SIDs are contiguous and
        // nLeft fits a u16. `checked_add` guards the SID == 0xFFFF
        // boundary. A +1 overflow would panic in debug builds and
        // wrap to 0 in release, silently merging unrelated SIDs into
        // the same record.
        while j < sids.len()
            && sids[j - 1].checked_add(1) == Some(sids[j])
            && u16::try_from(j - i).is_ok()
        {
            j += 1;
        }
        let n_left = (j - i - 1) as u16;
        out.extend_from_slice(&first.to_be_bytes());
        out.extend_from_slice(&n_left.to_be_bytes());
        i = j;
    }
    out
}

/// Picks the shorter of charset formats 0 and 2 for the given SID
/// list, returning `(format_byte_already_in_bytes, bytes)`.
#[must_use]
pub fn emit_charset_auto(sids: &[u16]) -> Vec<u8> {
    let f0 = emit_charset_format0(sids);
    let f2 = emit_charset_format2(sids);
    if f2.len() < f0.len() {
        f2
    } else {
        f0
    }
}

/// Emits an Encoding in format 0 (per-gid 1-byte char-code array,
/// omitting gid 0). `codes[i]` is the char code for gid `i+1`.
/// Returns `1 + 1 + n` bytes: format byte (0), n_codes (u8), then n
/// codes.
///
/// The CFF1 format-0 `nCodes` field is a Card8, so at most 255 codes
/// can be addressed. Inputs longer than that get capped. The spec
/// requires nCodes to match the byte run that follows.
#[must_use]
pub fn emit_encoding_format0(codes: &[u8]) -> Vec<u8> {
    let n = codes.len().min(u8::MAX as usize);
    let mut out = Vec::with_capacity(2 + n);
    out.push(0);
    out.push(n as u8);
    out.extend_from_slice(&codes[..n]);
    out
}

/// Emits an Encoding in format 1 (range records: u8 first + u8 nLeft).
/// Each contiguous run of consecutive char codes at consecutive gids
/// becomes one record.
///
/// The format-1 `nRanges` field is a Card8 so at most 255 ranges can
/// be encoded. Excess ranges are dropped; this matches HarfBuzz's
/// behavior and keeps the emitted bytes parseable.
#[must_use]
pub fn emit_encoding_format1(codes: &[u8]) -> Vec<u8> {
    let mut ranges: Vec<(u8, u8)> = Vec::new();
    let mut i = 0;
    while i < codes.len() {
        let first = codes[i];
        let mut j = i + 1;
        while j < codes.len()
            && codes[j] == codes[j - 1].wrapping_add(1)
            && codes[j - 1] != 0xFF
            && (j - i) <= u8::MAX as usize + 1
        {
            j += 1;
        }
        let n_left = (j - i - 1) as u8;
        ranges.push((first, n_left));
        if ranges.len() == u8::MAX as usize {
            // Format 1's nRanges is a u8; stop emitting before it
            // overflows so the byte stream stays consistent.
            break;
        }
        i = j;
    }
    let mut out = alloc::vec![1u8, ranges.len() as u8];
    for (first, n_left) in ranges {
        out.push(first);
        out.push(n_left);
    }
    out
}

/// Picks the shorter of Encoding formats 0 and 1 for the given char
/// code list. Returns the encoded bytes including the format byte.
#[must_use]
pub fn emit_encoding_auto(codes: &[u8]) -> Vec<u8> {
    let f0 = emit_encoding_format0(codes);
    let f1 = emit_encoding_format1(codes);
    if f1.len() < f0.len() {
        f1
    } else {
        f0
    }
}

/// Parses a CFF FDSelect at `data[off..]` for `n_glyphs` glyphs,
/// returning a per-gid FD-index vector. Recognizes format 0 (per-gid u8)
/// and format 3 (range records, u16 firstGlyph + u8 fd).
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] for unknown formats or when the
/// table is truncated.
pub fn parse_fd_select(data: &[u8], off: usize, n_glyphs: usize) -> Result<Vec<u8>, SubsetError> {
    let Some((&format, body)) = data.get(off..).and_then(<[u8]>::split_first) else {
        return Err(SubsetError::Unsupported("CFF FDSelect offset past end"));
    };
    match format {
        0 => body
            .get(..n_glyphs)
            .map(<[u8]>::to_vec)
            .ok_or(SubsetError::Unsupported("CFF FDSelect format 0 truncated")),
        3 => {
            let Some((n_ranges, mut rest)) = body.split_first_chunk::<2>() else {
                return Err(SubsetError::Unsupported(
                    "CFF FDSelect format 3 header truncated",
                ));
            };
            let n_ranges = usize::from(u16::from_be_bytes(*n_ranges));
            let mut ranges = Vec::with_capacity(n_ranges);
            for _ in 0..n_ranges {
                let Some((record, tail)) = rest.split_first_chunk::<3>() else {
                    return Err(SubsetError::Unsupported(
                        "CFF FDSelect format 3 range truncated",
                    ));
                };
                let &[first_hi, first_lo, fd] = record;
                ranges.push((u16::from_be_bytes([first_hi, first_lo]), fd));
                rest = tail;
            }
            let Some(sentinel) = rest.first_chunk::<2>() else {
                return Err(SubsetError::Unsupported(
                    "CFF FDSelect format 3 sentinel truncated",
                ));
            };
            let sentinel = usize::from(u16::from_be_bytes(*sentinel));
            let mut out = alloc::vec![0u8; n_glyphs];
            for (i, &(first, fd)) in ranges.iter().enumerate() {
                let start = usize::from(first);
                let end = ranges
                    .get(i + 1)
                    .map_or(sentinel, |&(next, _)| usize::from(next));
                if let Some(run) = out.get_mut(start..end.min(n_glyphs)) {
                    run.fill(fd);
                }
            }
            Ok(out)
        }
        _ => Err(SubsetError::Unsupported("CFF FDSelect format not 0 / 3")),
    }
}

/// Emits an FDSelect in format 0 (per-gid 1-byte FD index, one entry
/// per glyph including gid 0). For `n_glyphs` glyphs the table is
/// `1 + n_glyphs` bytes.
#[must_use]
pub fn emit_fd_select_format0(per_gid: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + per_gid.len());
    out.push(0);
    out.extend_from_slice(per_gid);
    out
}

/// Emits an FDSelect in format 3 (range records: u16 firstGlyph, u8 fd,
/// terminated by a u16 sentinel == nGlyphs). Each contiguous run of
/// gids sharing the same FD becomes one record.
#[must_use]
pub fn emit_fd_select_format3(per_gid: &[u8]) -> Vec<u8> {
    let n_glyphs = per_gid.len();
    let mut ranges: Vec<(u16, u8)> = Vec::new();
    let mut i = 0;
    while i < n_glyphs {
        let fd = per_gid[i];
        ranges.push((i as u16, fd));
        let mut j = i + 1;
        while j < n_glyphs && per_gid[j] == fd {
            j += 1;
        }
        i = j;
    }
    let n_ranges = ranges.len();
    let mut out = Vec::with_capacity(3 + n_ranges * 3 + 2);
    out.push(3);
    out.extend_from_slice(&(n_ranges as u16).to_be_bytes());
    for (first, fd) in &ranges {
        out.extend_from_slice(&first.to_be_bytes());
        out.push(*fd);
    }
    out.extend_from_slice(&(n_glyphs as u16).to_be_bytes());
    out
}

/// Picks the shorter of FDSelect formats 0 and 3 for the given per-gid
/// FD-index list. Returns the encoded bytes including the format byte.
///
/// Format 0 is `1 + n` bytes; format 3 is `3 + 3*n_ranges + 2` bytes.
/// On dense / mostly-uniform inputs format 3 wins; on highly fragmented
/// inputs format 0 wins.
#[must_use]
pub fn emit_fd_select_auto(per_gid: &[u8]) -> Vec<u8> {
    let f0 = emit_fd_select_format0(per_gid);
    let f3 = emit_fd_select_format3(per_gid);
    if f3.len() < f0.len() {
        f3
    } else {
        f0
    }
}
