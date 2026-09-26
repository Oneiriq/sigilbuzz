//! Source CFF reader: DICT walker, INDEX readers, Top DICT operator
//! numbers, and the raw CFF1 layout capture.

use alloc::vec::Vec;

use crate::SubsetError;

// ----------------------------------------------------------------------------
// Source-CFF reader + Top DICT walker.
//
// The cross-cutting orchestration in [`subset_non_identity`] needs to capture
// every byte run the source CFF carries so it can preserve some verbatim
// (Header, Name INDEX, String INDEX) and rebuild others (Top DICT, Global
// Subr INDEX, charset, Encoding, CharStrings INDEX, Private DICT, Local
// Subr INDEX). The reader lives here rather than in the existing
// [`crate::tables::cff`] parser because the parser exposes Top DICT fields
// only as derived data: the raw operator-by-operator walk we need to copy
// the non-offset operators verbatim is not on its public surface.
// ----------------------------------------------------------------------------

/// CFF DICT operand seen by the Top DICT walker. We keep the raw
/// encoded bytes alongside the decoded integer so non-offset operators
/// can be re-emitted byte-for-byte (preserving real-number operands and
/// any non-canonical integer encoding the source font happened to use).
#[derive(Debug, Clone)]
pub(crate) struct DictOperand {
    /// Decoded integer value, when the operand is integer-typed.
    /// `None` for real-number operands (op 30). We never need to
    /// patch a real, so preserving the raw bytes is enough.
    pub(crate) int_value: Option<i32>,
    /// Encoded bytes as they appeared in the source DICT.
    pub(crate) raw: Vec<u8>,
}

/// One operator + its operand list, as captured by [`walk_dict`].
#[derive(Debug, Clone)]
pub(crate) struct DictEntry {
    /// Operator number: single-byte ops are 0..=21, escaped ops are
    /// 0x0C00 | b1.
    pub(crate) op: u16,
    /// Operands that preceded this operator.
    pub(crate) operands: Vec<DictOperand>,
}

/// Decodes one DICT operand at `bytes[pos..]`, returning the operand
/// plus its encoded length. Operator bytes (b0 in 0..=21) terminate
/// the operand stream and are not consumed here.
fn decode_dict_operand(bytes: &[u8], pos: usize) -> Result<(DictOperand, usize), SubsetError> {
    let b0 = *bytes
        .get(pos)
        .ok_or(SubsetError::Unsupported("CFF DICT operand truncated"))?;
    if b0 == 28 {
        let hi = *bytes
            .get(pos + 1)
            .ok_or(SubsetError::Unsupported("CFF DICT shortint truncated"))?;
        let lo = *bytes
            .get(pos + 2)
            .ok_or(SubsetError::Unsupported("CFF DICT shortint truncated"))?;
        let v = i16::from_be_bytes([hi, lo]);
        Ok((
            DictOperand {
                int_value: Some(i32::from(v)),
                raw: alloc::vec![b0, hi, lo],
            },
            3,
        ))
    } else if b0 == 29 {
        let raw = bytes
            .get(pos + 1..pos + 5)
            .ok_or(SubsetError::Unsupported("CFF DICT longint truncated"))?;
        let v = i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let mut keep = alloc::vec![b0];
        keep.extend_from_slice(raw);
        Ok((
            DictOperand {
                int_value: Some(v),
                raw: keep,
            },
            5,
        ))
    } else if b0 == 30 {
        // Real number: nibble-packed BCD, terminated when either nibble
        // of a byte is 0xF. Preserve raw bytes only.
        let mut len = 1;
        loop {
            let b = *bytes
                .get(pos + len)
                .ok_or(SubsetError::Unsupported("CFF DICT real-number truncated"))?;
            len += 1;
            if (b & 0x0F) == 0x0F || (b >> 4) == 0x0F {
                break;
            }
        }
        Ok((
            DictOperand {
                int_value: None,
                raw: bytes[pos..pos + len].to_vec(),
            },
            len,
        ))
    } else if (32..=246).contains(&b0) {
        Ok((
            DictOperand {
                int_value: Some(i32::from(b0) - 139),
                raw: alloc::vec![b0],
            },
            1,
        ))
    } else if (247..=250).contains(&b0) {
        let b1 = *bytes
            .get(pos + 1)
            .ok_or(SubsetError::Unsupported("CFF DICT 2-byte op truncated"))?;
        let v = (i32::from(b0) - 247) * 256 + i32::from(b1) + 108;
        Ok((
            DictOperand {
                int_value: Some(v),
                raw: alloc::vec![b0, b1],
            },
            2,
        ))
    } else if (251..=254).contains(&b0) {
        let b1 = *bytes
            .get(pos + 1)
            .ok_or(SubsetError::Unsupported("CFF DICT 2-byte op truncated"))?;
        let v = -(i32::from(b0) - 251) * 256 - i32::from(b1) - 108;
        Ok((
            DictOperand {
                int_value: Some(v),
                raw: alloc::vec![b0, b1],
            },
            2,
        ))
    } else {
        Err(SubsetError::Unsupported("CFF DICT operand out of range"))
    }
}

/// Walks a CFF DICT (Top DICT or Private DICT) and returns one
/// [`DictEntry`] per operator + its preceding operands. Pure byte walk
/// (no semantic interpretation of operator meanings).
pub(crate) fn walk_dict(bytes: &[u8]) -> Result<Vec<DictEntry>, SubsetError> {
    let mut out = Vec::new();
    let mut pos = 0;
    let mut operands: Vec<DictOperand> = Vec::new();
    while pos < bytes.len() {
        let b0 = bytes[pos];
        // CFF DICT operators are b0 in 0..=21 (CFF1) or 0..=24 (CFF2's
        // VariationStore op = 24). Bytes 22, 23, 25..=27 are reserved
        // and we reject them as malformed if they appear in operand
        // position. Operands start at b0 = 28 (shortint) / 29 / 30 /
        // 32..=254 / 255 (charstring fixed; CFF DICTs don't use 255).
        if b0 <= 24 {
            // Operator.
            let (op, len) = if b0 == 12 {
                let b1 = *bytes
                    .get(pos + 1)
                    .ok_or(SubsetError::Unsupported("CFF DICT escaped op truncated"))?;
                (0x0C00u16 | u16::from(b1), 2usize)
            } else {
                (u16::from(b0), 1usize)
            };
            out.push(DictEntry {
                op,
                operands: core::mem::take(&mut operands),
            });
            pos += len;
        } else {
            let (val, len) = decode_dict_operand(bytes, pos)?;
            operands.push(val);
            pos += len;
        }
    }
    Ok(out)
}

/// Returns the `(size, offset)` operands of a Private (op 18) DICT
/// entry, or `None` when they are missing, non-integer, or negative.
pub(crate) fn private_operands(entry: &DictEntry) -> Option<(u32, u32)> {
    let [.., size, off] = entry.operands.as_slice() else {
        return None;
    };
    let size = u32::try_from(size.int_value?).ok()?;
    let off = u32::try_from(off.int_value?).ok()?;
    Some((size, off))
}

/// INDEX reader for one table flavor ([`read_index`] or
/// [`read_index_cff2`]).
pub(crate) type IndexReader =
    for<'b> fn(&'b [u8], usize) -> Result<(Vec<&'b [u8]>, usize), SubsetError>;

/// Slices the Private DICT at `data[off..off + size]` and reads the
/// local Subrs INDEX its op 19 points at (relative to the Private DICT
/// start). Returns an empty Subrs list when op 19 is absent.
pub(crate) fn read_private_dict<'a>(
    data: &'a [u8],
    size: u32,
    off: u32,
    read_subrs: IndexReader,
    past_end: &'static str,
) -> Result<(&'a [u8], Vec<&'a [u8]>), SubsetError> {
    let start = off as usize;
    let priv_bytes = start
        .checked_add(size as usize)
        .and_then(|end| data.get(start..end))
        .ok_or(SubsetError::Unsupported(past_end))?;
    // Walk the Private DICT for op 19. The last well-formed one wins.
    let mut subrs_rel: Option<u32> = None;
    for e in &walk_dict(priv_bytes)? {
        if e.op == OP_SUBRS {
            if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                if let Ok(v) = u32::try_from(v) {
                    subrs_rel = Some(v);
                }
            }
        }
    }
    let locals = match subrs_rel {
        Some(rel) => {
            let abs = start
                .checked_add(rel as usize)
                .ok_or(SubsetError::Unsupported(past_end))?;
            read_subrs(data, abs)?.0
        }
        None => Vec::new(),
    };
    Ok((priv_bytes, locals))
}

/// Error messages for one INDEX flavor. CFF1 and CFF2 INDEXes share a
/// layout but report malformations under their own names.
struct IndexErrors {
    header_truncated: &'static str,
    off_size_missing: &'static str,
    off_size_out_of_range: &'static str,
    offsets_truncated: &'static str,
    final_offset_zero: &'static str,
    data_past_end: &'static str,
    offsets_non_monotone: &'static str,
}

const CFF1_INDEX_ERRORS: IndexErrors = IndexErrors {
    header_truncated: "CFF INDEX header truncated",
    off_size_missing: "CFF INDEX offSize missing",
    off_size_out_of_range: "CFF INDEX offSize out of range",
    offsets_truncated: "CFF INDEX offsets truncated",
    final_offset_zero: "CFF INDEX final offset zero",
    data_past_end: "CFF INDEX data past end",
    offsets_non_monotone: "CFF INDEX offsets non-monotone",
};

const CFF2_INDEX_ERRORS: IndexErrors = IndexErrors {
    header_truncated: "CFF2 INDEX header truncated",
    off_size_missing: "CFF2 INDEX offSize missing",
    off_size_out_of_range: "CFF2 INDEX offSize out of range",
    offsets_truncated: "CFF2 INDEX offsets truncated",
    final_offset_zero: "CFF2 INDEX final offset zero",
    data_past_end: "CFF2 INDEX data past end",
    offsets_non_monotone: "CFF2 INDEX offsets non-monotone",
};

/// Decodes a big-endian unsigned integer of 1..=4 bytes.
fn read_be_uint(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .fold(0usize, |acc, &b| (acc << 8) | usize::from(b))
}

/// Shared INDEX reader. `count_len` is the width of the count field
/// (2 for CFF1, 4 for CFF2). Every offset is checked against the data
/// region before it is used, so a hostile offset array yields an error
/// instead of an out-of-range slice.
fn read_index_with<'a>(
    bytes: &'a [u8],
    pos: usize,
    count_len: usize,
    errs: &IndexErrors,
) -> Result<(Vec<&'a [u8]>, usize), SubsetError> {
    let count_bytes = bytes
        .get(pos..)
        .and_then(|b| b.get(..count_len))
        .ok_or(SubsetError::Unsupported(errs.header_truncated))?;
    let count = read_be_uint(count_bytes);
    if count == 0 {
        // Empty INDEX: just the count field, no offSize / offsets.
        return Ok((Vec::new(), count_len));
    }
    // `pos + count_len` is in bounds: the count bytes were read above.
    let off_size_pos = pos + count_len;
    let off_size = usize::from(
        *bytes
            .get(off_size_pos)
            .ok_or(SubsetError::Unsupported(errs.off_size_missing))?,
    );
    if !(1..=4).contains(&off_size) {
        return Err(SubsetError::Unsupported(errs.off_size_out_of_range));
    }
    let off_table_start = off_size_pos + 1;
    let off_table = count
        .checked_add(1)
        .and_then(|n| n.checked_mul(off_size))
        .and_then(|len| bytes.get(off_table_start..)?.get(..len))
        .ok_or(SubsetError::Unsupported(errs.offsets_truncated))?;
    let data_start = off_table_start + off_table.len();
    let offsets: Vec<usize> = off_table.chunks_exact(off_size).map(read_be_uint).collect();
    let last = offsets.last().copied().unwrap_or(0);
    if last == 0 {
        return Err(SubsetError::Unsupported(errs.final_offset_zero));
    }
    let data_len = last - 1;
    let data = bytes
        .get(data_start..)
        .and_then(|b| b.get(..data_len))
        .ok_or(SubsetError::Unsupported(errs.data_past_end))?;
    let mut entries = Vec::with_capacity(count);
    for w in offsets.windows(2) {
        let &[a, b] = w else {
            continue;
        };
        if a == 0 || b < a {
            return Err(SubsetError::Unsupported(errs.offsets_non_monotone));
        }
        // An offset past the final one would slice beyond the data
        // region. It also breaks monotonicity with the final offset.
        let entry = data
            .get(a - 1..b - 1)
            .ok_or(SubsetError::Unsupported(errs.offsets_non_monotone))?;
        entries.push(entry);
    }
    Ok((entries, data_start + data_len - pos))
}

/// Reads a CFF INDEX at `bytes[pos..]`, returning the entry slices
/// (zero-copy into the input) plus the byte-length of the entire INDEX
/// structure (so the caller can advance past it).
pub(crate) fn read_index(bytes: &[u8], pos: usize) -> Result<(Vec<&[u8]>, usize), SubsetError> {
    read_index_with(bytes, pos, 2, &CFF1_INDEX_ERRORS)
}

/// CFF2 INDEX reader. CFF2 widens the count field to u32 (CFF1 used
/// u16); the offSize / offsets / data layout is otherwise identical.
/// Returns the entry slices and the byte-length of the full INDEX.
pub(crate) fn read_index_cff2(
    bytes: &[u8],
    pos: usize,
) -> Result<(Vec<&[u8]>, usize), SubsetError> {
    read_index_with(bytes, pos, 4, &CFF2_INDEX_ERRORS)
}

/// Encodes a CFF2 INDEX (count is u32, layout otherwise identical to
/// the CFF1 INDEX). Used for FDArray / Local Subr / Global Subr /
/// CharStrings INDEXes inside CFF2 tables.
#[must_use]
pub fn encode_index_cff2(entries: &[&[u8]]) -> Vec<u8> {
    let count = entries.len();
    let mut out = Vec::new();
    if count == 0 {
        out.extend_from_slice(&0u32.to_be_bytes());
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
    out.extend_from_slice(&(count as u32).to_be_bytes());
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

// ----------------------------------------------------------------------------
// Top DICT operator numbers.
// ----------------------------------------------------------------------------

pub(super) const OP_CHARSET: u16 = 15;
pub(super) const OP_ENCODING: u16 = 16;
pub(crate) const OP_CHARSTRINGS: u16 = 17;
pub(crate) const OP_PRIVATE: u16 = 18;
pub(crate) const OP_SUBRS: u16 = 19;
pub(crate) const OP_FD_ARRAY: u16 = 0x0C24;
pub(crate) const OP_FD_SELECT: u16 = 0x0C25;
const OP_ROS: u16 = 0x0C1E;
pub(crate) const OP_VSTORE: u16 = 24;

/// Captures the source CFF1 layout in raw form so the orchestration
/// can rebuild kept sections while preserving everything else verbatim.
#[derive(Debug)]
pub(super) struct ParsedCff1<'a> {
    /// Header bytes (4), copied verbatim into the output.
    pub(super) header: &'a [u8],
    /// Name INDEX bytes (verbatim copy span: header + offsets + data).
    pub(super) name_index: &'a [u8],
    /// Top DICT body bytes (the single first entry of the Top DICT INDEX).
    pub(super) top_dict: &'a [u8],
    /// String INDEX bytes (verbatim).
    pub(super) string_index: &'a [u8],
    /// Global Subr INDEX entries (one slice per subr).
    pub(super) global_subrs: Vec<&'a [u8]>,
    /// CharStrings INDEX entries: one slice per glyph.
    pub(super) char_strings: Vec<&'a [u8]>,
    /// Charset offset (Top DICT op 15). Default `0` (ISOAdobe).
    pub(super) charset_off: u32,
    /// Encoding offset (Top DICT op 16). Default `0` (Standard).
    pub(super) encoding_off: u32,
    /// Private DICT (size, offset). `None` if op 18 absent.
    pub(super) private: Option<(u32, u32)>,
    /// Private DICT bytes (when `private` is `Some`).
    pub(super) private_dict: &'a [u8],
    /// Local Subr INDEX entries, taken from Private DICT op 19, when
    /// present. Empty when no local subrs.
    pub(super) local_subrs: Vec<&'a [u8]>,
    /// Whether the source uses CID-keyed (FDArray/FDSelect) layout.
    pub(super) is_cid: bool,
    /// FDArray offset (Top DICT op 12 36). `Some` when CID-keyed.
    pub(super) fd_array_off: Option<u32>,
    /// FDSelect offset (Top DICT op 12 37). `Some` when CID-keyed.
    pub(super) fd_select_off: Option<u32>,
}

/// Walks the source CFF1 table and captures every span the rewriter
/// needs. CID-keyed fonts are detected (op `0x0C24` / `0x0C25` /
/// `0x0C1E` present) and surfaced via `is_cid` so the orchestration
/// can route them through `subset_cid_keyed`.
pub(super) fn parse_cff1(data: &[u8]) -> Result<ParsedCff1<'_>, SubsetError> {
    let Some(header) = data.first_chunk::<4>() else {
        return Err(SubsetError::Unsupported("CFF1 header truncated"));
    };
    let &[major, _, hdr_size, _] = header;
    let header = header.as_slice();
    let hdr_size = usize::from(hdr_size);
    if major != 1 {
        return Err(SubsetError::Unsupported("CFF1 major version != 1"));
    }
    if hdr_size < 4 || hdr_size > data.len() {
        return Err(SubsetError::Unsupported("CFF1 hdrSize invalid"));
    }

    // Name INDEX. `read_index` only returns lengths that stay inside
    // `data`, so the spans below are always present.
    let (_, name_len) = read_index(data, hdr_size)?;
    let mut pos = hdr_size + name_len;
    let name_index = data
        .get(hdr_size..pos)
        .ok_or(SubsetError::Unsupported("CFF1 Name INDEX past end"))?;

    // Top DICT INDEX: first entry only.
    let (top_entries, top_index_len) = read_index(data, pos)?;
    let top_dict = top_entries
        .first()
        .copied()
        .ok_or(SubsetError::Unsupported("CFF1 Top DICT INDEX empty"))?;
    pos += top_index_len;

    // String INDEX.
    let (_, string_len) = read_index(data, pos)?;
    let string_index = data
        .get(pos..pos + string_len)
        .ok_or(SubsetError::Unsupported("CFF1 String INDEX past end"))?;
    pos += string_len;

    // Global Subr INDEX.
    let (global_subrs, _) = read_index(data, pos)?;

    // Walk the Top DICT for offsets we need.
    let entries = walk_dict(top_dict)?;
    let mut charset_off: u32 = 0;
    let mut encoding_off: u32 = 0;
    let mut char_strings_off: Option<u32> = None;
    let mut private: Option<(u32, u32)> = None;
    let mut is_cid = false;
    let mut fd_array_off: Option<u32> = None;
    let mut fd_select_off: Option<u32> = None;
    for e in &entries {
        match e.op {
            OP_CHARSET => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        charset_off = v as u32;
                    }
                }
            }
            OP_ENCODING => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        encoding_off = v as u32;
                    }
                }
            }
            OP_CHARSTRINGS => {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        char_strings_off = Some(v as u32);
                    }
                }
            }
            OP_PRIVATE => {
                if let Some(pair) = private_operands(e) {
                    private = Some(pair);
                }
            }
            OP_FD_ARRAY => {
                is_cid = true;
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        fd_array_off = Some(v as u32);
                    }
                }
            }
            OP_FD_SELECT => {
                is_cid = true;
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        fd_select_off = Some(v as u32);
                    }
                }
            }
            OP_ROS => is_cid = true,
            _ => {}
        }
    }

    let cs_off = char_strings_off.ok_or(SubsetError::Unsupported(
        "CFF1 Top DICT missing CharStrings",
    ))? as usize;
    let (char_strings, _) = read_index(data, cs_off)?;

    // Private DICT + Local Subr INDEX.
    let (private_dict, local_subrs): (&[u8], Vec<&[u8]>) = match private {
        Some((size, off)) => {
            read_private_dict(data, size, off, read_index, "CFF1 Private DICT past end")?
        }
        None => (&[][..], Vec::new()),
    };

    Ok(ParsedCff1 {
        header,
        name_index,
        top_dict,
        string_index,
        global_subrs,
        char_strings,
        charset_off,
        encoding_off,
        private,
        private_dict,
        local_subrs,
        is_cid,
        fd_array_off,
        fd_select_off,
    })
}
