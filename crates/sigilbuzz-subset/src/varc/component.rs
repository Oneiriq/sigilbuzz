//! VarComposite glyph record walking and rewriting: component gids,
//! MultiVarStore indexes, and uint32var encoding.

use alloc::vec::Vec;

use super::{
    VC_AXIS_VALUES_HAVE_VARIATION, VC_GID_IS_24BIT, VC_HAVE_AXES, VC_HAVE_CONDITION,
    VC_HAVE_ROTATION, VC_HAVE_SCALE_X, VC_HAVE_SCALE_Y, VC_HAVE_SKEW_X, VC_HAVE_SKEW_Y,
    VC_HAVE_TCENTER_X, VC_HAVE_TCENTER_Y, VC_HAVE_TRANSLATE_X, VC_HAVE_TRANSLATE_Y,
    VC_TRANSFORM_HAS_VARIATION,
};
use crate::{GlyphId, SubsetError};

/// Walks a single VarComposite glyph record and yields the gid of every
/// component. Tolerates malformed records by stopping mid-walk. The
/// caller treats that as "no further components in this record."
pub(super) fn walk_component_gids(record: &[u8]) -> Vec<GlyphId> {
    let mut out: Vec<GlyphId> = Vec::new();
    let mut cursor = 0usize;
    while cursor < record.len() {
        match parse_one_component(record, cursor) {
            Some((info, next)) => {
                out.push(info.gid);
                if next <= cursor {
                    // Defensive: a buggy walk that doesn't advance would
                    // loop forever on malformed input.
                    break;
                }
                cursor = next;
            }
            None => break,
        }
    }
    out
}

/// Walks a single VarComposite glyph record and yields every
/// MultiVarIdx referenced by its component records: both transform
/// deltas (`VC_TRANSFORM_HAS_VARIATION`) and axis-coord deltas
/// (`VC_AXIS_VALUES_HAVE_VARIATION`). Each value is split into its
/// outer/inner halves (high 16 bits -> outer, low 16 -> inner).
///
/// Tolerates malformed records by stopping mid-walk, mirroring
/// [`walk_component_gids`].
pub(super) fn walk_component_var_idxs(record: &[u8]) -> Vec<(u16, u16)> {
    let mut out: Vec<(u16, u16)> = Vec::new();
    let mut cursor = 0usize;
    while cursor < record.len() {
        match parse_one_component(record, cursor) {
            Some((info, next)) => {
                if let Some((_, _, v)) = info.axis_var_idx {
                    out.push(split_var_idx(v));
                }
                if let Some((_, _, v)) = info.transform_var_idx {
                    out.push(split_var_idx(v));
                }
                if next <= cursor {
                    break;
                }
                cursor = next;
            }
            None => break,
        }
    }
    out
}

/// Splits a MultiVarIdx into its `(outer, inner)` halves.
fn split_var_idx(v: u32) -> (u16, u16) {
    ((v >> 16) as u16, (v & 0xFFFF) as u16)
}

/// Per-component metadata extracted by `parse_one_component`.
struct ComponentInfo {
    /// Source-file gid this component points at.
    gid: GlyphId,
    /// Byte range inside the record where the gid lives. The rewrite
    /// path uses these bounds to splice in the new gid.
    gid_range: (usize, usize),
    /// True when the gid was encoded as 24 bits (VC_GID_IS_24BIT). The
    /// rewrite path keeps this width even if the new gid would fit in
    /// 16 bits, so the gid field never changes size.
    gid_is_24bit: bool,
    /// Byte range + old value of the axis-values MultiVarIdx, when
    /// `VC_AXIS_VALUES_HAVE_VARIATION` is set. The MVS pruning path
    /// rewrites the bytes in this range with the remapped index.
    axis_var_idx: Option<(usize, usize, u32)>,
    /// Byte range + old value of the transform MultiVarIdx, when
    /// `VC_TRANSFORM_HAS_VARIATION` is set.
    transform_var_idx: Option<(usize, usize, u32)>,
}

/// Parses one component record at `start`, returning the gid info and
/// the cursor position immediately past the record.
fn parse_one_component(record: &[u8], start: usize) -> Option<(ComponentInfo, usize)> {
    let mut cur = start;
    let (flags, after_flags) = read_uint32var(record, cur)?;
    cur = after_flags;

    let gid_start = cur;
    let (gid, gid_is_24bit, after_gid) = if flags & VC_GID_IS_24BIT != 0 {
        let b = record.get(cur..)?.first_chunk::<3>()?;
        let g = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        // sigilbuzz uses u16 gids throughout; a u24 source gid > 0xFFFF
        // would silently truncate to its low 16 bits and lie about the
        // reference graph (#196). Treat it as a malformed component and
        // bail. The walker's caller treats `None` as "no further
        // components in this record" and skips it tolerantly.
        let gid = u16::try_from(g).ok()?;
        (gid, true, cur + 3)
    } else {
        let g = crate::layout::read_u16(record, cur)?;
        (g, false, cur + 2)
    };
    let gid_end = after_gid;
    cur = after_gid;

    if flags & VC_HAVE_CONDITION != 0 {
        let (_cond, n) = read_uint32var(record, cur)?;
        cur = n;
    }

    if flags & VC_HAVE_AXES != 0 {
        let (_axis_indices_index, n) = read_uint32var(record, cur)?;
        cur = n;
        // axisValues is a TupleValues stream. We consume one run-control
        // + payload. VARC's writers emit a single run per axisValues
        // covering every axis the component touches.
        let n = consume_tuple_values_one_run(record, cur)?;
        cur = n;
    }

    let mut axis_var_idx: Option<(usize, usize, u32)> = None;
    if flags & VC_AXIS_VALUES_HAVE_VARIATION != 0 {
        let var_idx_start = cur;
        let (var_idx, n) = read_uint32var(record, cur)?;
        axis_var_idx = Some((var_idx_start, n, var_idx));
        cur = n;
    }

    let mut transform_var_idx: Option<(usize, usize, u32)> = None;
    if flags & VC_TRANSFORM_HAS_VARIATION != 0 {
        let var_idx_start = cur;
        let (var_idx, n) = read_uint32var(record, cur)?;
        transform_var_idx = Some((var_idx_start, n, var_idx));
        cur = n;
    }

    // i16 transform fields, in spec order. Each present flag adds 2 bytes.
    let field_count = [
        VC_HAVE_TRANSLATE_X,
        VC_HAVE_TRANSLATE_Y,
        VC_HAVE_ROTATION,
        VC_HAVE_SCALE_X,
        VC_HAVE_SCALE_Y,
        VC_HAVE_SKEW_X,
        VC_HAVE_SKEW_Y,
        VC_HAVE_TCENTER_X,
        VC_HAVE_TCENTER_Y,
    ]
    .iter()
    .filter(|&&bit| flags & bit != 0)
    .count();
    let bytes_needed = field_count * 2;
    if cur + bytes_needed > record.len() {
        return None;
    }
    cur += bytes_needed;

    Some((
        ComponentInfo {
            gid,
            gid_range: (gid_start, gid_end),
            gid_is_24bit,
            axis_var_idx,
            transform_var_idx,
        },
        cur,
    ))
}

/// Reads exactly one TupleValues run-control plus its payload, returning
/// the byte position immediately after.
fn consume_tuple_values_one_run(data: &[u8], start: usize) -> Option<usize> {
    let Some(&ctrl) = data.get(start) else {
        return Some(start);
    };
    let cur = start + 1;
    let run_len = (ctrl & 0x3F) as usize + 1;
    let zeros = ctrl & 0x80 != 0;
    let words = ctrl & 0x40 != 0;
    let slot_size = match (zeros, words) {
        (true, false) => 0,
        (true, true) => 4,
        (false, true) => 2,
        (false, false) => 1,
    };
    let need = run_len * slot_size;
    if cur + need > data.len() {
        return None;
    }
    Some(cur + need)
}

/// Reads a uint32var starting at `off` in `data`. Returns the value plus
/// the byte position immediately after.
fn read_uint32var(data: &[u8], off: usize) -> Option<(u32, usize)> {
    let rest = data.get(off..)?;
    let b0 = *rest.first()?;
    match b0 {
        0x00..=0x7F => Some((u32::from(b0), off + 1)),
        0x80..=0xBF => {
            let b = rest.first_chunk::<2>()?;
            Some((((u32::from(b0) - 0x80) << 8) | u32::from(b[1]), off + 2))
        }
        0xC0..=0xDF => {
            let b = rest.first_chunk::<3>()?;
            Some((
                ((u32::from(b0) - 0xC0) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]),
                off + 3,
            ))
        }
        0xE0..=0xEF => {
            let b = rest.first_chunk::<4>()?;
            Some((
                ((u32::from(b0) - 0xE0) << 24)
                    | (u32::from(b[1]) << 16)
                    | (u32::from(b[2]) << 8)
                    | u32::from(b[3]),
                off + 4,
            ))
        }
        0xF0..=0xFF => {
            let b = rest.first_chunk::<5>()?;
            Some((u32::from_be_bytes([b[1], b[2], b[3], b[4]]), off + 5))
        }
    }
}

/// Rewrites every component gid in a single VARC glyph record to the
/// new-gid namespace. Walks the record with [`parse_one_component`] to
/// find each component's gid byte range, then splices the new gid in
/// place. The rewritten record has the same length unless the gid
/// encoding width changes. Today we keep the width identical (24-bit
/// stays 24-bit) for byte-stable output.
///
/// Test-only thin wrapper around [`rewrite_component_record`] with an
/// identity var-idx remap; the production path always goes through the
/// full record-rewrite to also apply the MVS prune remap.
#[cfg(test)]
pub(super) fn rewrite_component_gids(
    record: &[u8],
    new_gid_for: &dyn Fn(GlyphId) -> Option<GlyphId>,
) -> Result<Vec<u8>, SubsetError> {
    rewrite_component_record(record, new_gid_for, &|outer, inner| Some((outer, inner)))
}

/// Encodes a `u32` using VARC's variable-length integer encoding,
/// picking the smallest form that fits. Mirrors `read_uint32var` in
/// `src/tables/varc.rs`.
///
/// Returns the encoded bytes (1-5 bytes long).
fn encode_uint32var(v: u32) -> Vec<u8> {
    let b = v.to_be_bytes();
    if v <= 0x7F {
        alloc::vec![b[3]]
    } else if v <= 0x3FFF {
        // Two-byte form: top bits (0x80..=0xBF) carry the high 6 bits.
        alloc::vec![b[2] | 0x80, b[3]]
    } else if v <= 0x001F_FFFF {
        // Three-byte form: top bits (0xC0..=0xDF).
        alloc::vec![b[1] | 0xC0, b[2], b[3]]
    } else if v <= 0x0FFF_FFFF {
        // Four-byte form: top bits (0xE0..=0xEF).
        alloc::vec![b[0] | 0xE0, b[1], b[2], b[3]]
    } else {
        // Five-byte form: 0xF0 marker + u32 BE.
        alloc::vec![0xF0, b[0], b[1], b[2], b[3]]
    }
}

/// Rewrites a single VARC glyph record:
///
/// - Every component gid is renumbered through `new_gid_for`.
/// - Every `MultiVarIdx` (transform deltas + axis-values deltas) is
///   remapped through `var_idx_remap`. The closure takes
///   `(outer, inner)` halves of the source `MultiVarIdx` and returns
///   the new halves, or `None` when the entry was unreferenced and is
///   being dropped (a hard error in this path: every var-idx the walker
///   sees in a surviving record must be in the kept set, otherwise the
///   pruning closure missed it).
///
/// Layout-wise the record is rebuilt by walking each component, copying
/// the gap from the previous cursor, splicing the new gid in place,
/// then splicing each (possibly width-changed) `MultiVarIdx`. Output
/// length differs from input length when var-idx encoding widths shift.
pub(super) fn rewrite_component_record(
    record: &[u8],
    new_gid_for: &dyn Fn(GlyphId) -> Option<GlyphId>,
    var_idx_remap: &dyn Fn(u16, u16) -> Option<(u16, u16)>,
) -> Result<Vec<u8>, SubsetError> {
    const BAD_SPLICE: SubsetError = SubsetError::Unsupported("VARC component splice out of order");
    let mut out: Vec<u8> = Vec::with_capacity(record.len());
    let mut copy_from = 0usize;
    let mut cursor = 0usize;
    while cursor < record.len() {
        let Some((info, next)) = parse_one_component(record, cursor) else {
            break;
        };
        let new_gid = new_gid_for(info.gid).ok_or(SubsetError::Unsupported(
            "VARC component gid not in kept set",
        ))?;

        // Splice points inside this component, in source byte order:
        // gid first, then axis_var_idx, then transform_var_idx.
        let mut splices: Vec<(usize, usize, Vec<u8>)> = Vec::new();
        splices.push((
            info.gid_range.0,
            info.gid_range.1,
            if info.gid_is_24bit {
                let mut v = alloc::vec![0u8];
                v.extend_from_slice(&new_gid.to_be_bytes());
                v
            } else {
                new_gid.to_be_bytes().to_vec()
            },
        ));
        if let Some((s, e, v)) = info.axis_var_idx {
            let (outer, inner) = split_var_idx(v);
            let (no, ni) = var_idx_remap(outer, inner).ok_or(SubsetError::Unsupported(
                "VARC axis-values MultiVarIdx not in kept MVS set",
            ))?;
            let new_v = (u32::from(no) << 16) | u32::from(ni);
            splices.push((s, e, encode_uint32var(new_v)));
        }
        if let Some((s, e, v)) = info.transform_var_idx {
            let (outer, inner) = split_var_idx(v);
            let (no, ni) = var_idx_remap(outer, inner).ok_or(SubsetError::Unsupported(
                "VARC transform MultiVarIdx not in kept MVS set",
            ))?;
            let new_v = (u32::from(no) << 16) | u32::from(ni);
            splices.push((s, e, encode_uint32var(new_v)));
        }

        for (s, e, bytes) in splices {
            out.extend_from_slice(record.get(copy_from..s).ok_or(BAD_SPLICE)?);
            out.extend_from_slice(&bytes);
            copy_from = e;
        }

        if next <= cursor {
            break;
        }
        cursor = next;
    }
    out.extend_from_slice(record.get(copy_from..).ok_or(BAD_SPLICE)?);
    Ok(out)
}
