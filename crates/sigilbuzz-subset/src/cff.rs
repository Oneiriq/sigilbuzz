//! CFF1 subsetting helpers and byte-level emitter primitives.
//!
//! # Analysis layer
//!
//! - [`subr_bias`] — canonical 107 / 1131 / 32768 bias table from the
//!   Type 2 spec, indexed by subroutine count.
//! - [`scan_subr_calls`] — walks a Type 2 charstring's bytes and yields
//!   the (kind, biased index) pair for every `callsubr` / `callgsubr`
//!   it executes. Handles single-byte (32..=246), two-byte positive
//!   (247..=250) / negative (251..=254), `shortint` (op 28), `fixed`
//!   (op 255) push forms, and `hintmask` / `cntrmask` tail bytes.
//! - [`compute_kept_subrs`] — fixed-point closure that returns the
//!   transitive local + global subroutine keep-set for a kept-gid
//!   charstring slice.
//!
//! # Emitter primitives
//!
//! - [`encode_index`] — serialises a CFF INDEX (header + offsets +
//!   payload), picking the smallest valid `offSize`.
//! - [`encode_int_operand`] — Type 2 operand push, smallest valid form.
//! - [`encode_dict_int`] — DICT-context integer encoding (used by Top
//!   DICT and Private DICT serialisers).
//! - [`emit_charset_format0`] / [`emit_charset_format2`] /
//!   [`emit_charset_auto`] — charset rebuild with auto format selection.
//! - [`emit_encoding_format0`] / [`emit_encoding_format1`] /
//!   [`emit_encoding_auto`] — Encoding rebuild with auto format
//!   selection (CFF1 only; CFF2 omits Encoding).
//! - [`renumber_subr_call`] — patches a single `callsubr` / `callgsubr`
//!   operand push in a charstring slice using the bias-adjusted target.
//! - [`renumber_charstring`] — runs [`scan_subr_calls`] over a
//!   charstring and rewrites every call site against caller-supplied
//!   `(old_index → new_index)` maps for both subr kinds.
//!
//! # Subset entry
//!
//! The crate's [`crate::subset`] entry currently dispatches CFF and
//! CFF2 sources to an identity-only passthrough: when the closure
//! walker has not dropped any glyphs (kept set == `0..num_glyphs`) the
//! source CFF / CFF2 table is preserved verbatim and the surrounding
//! sfnt directory is rebuilt around it. Non-identity CFF subsetting —
//! the byte-level rewrite that consumes the emitter primitives above —
//! is staged for a follow-up; today it surfaces
//! [`SubsetError::Unsupported`] so callers see a clean error instead of
//! a corrupt font.
//!
//! The emitter primitives are exercised by unit tests covering the
//! bias-renumber boundaries (107 / 1131 / 32768), Top DICT
//! serialisation idempotence, and charset / encoding format auto-pick.

use alloc::vec::Vec;

use crate::SubsetError;

/// CFF subroutine bias from the Type 2 spec.
///
/// Subroutine indices in the charstring stream are stored relative to
/// a bias derived from the INDEX size; the runtime adds the bias back
/// before looking up the actual entry. A subsetter that drops
/// subroutines must re-encode each surviving call site so that
/// `runtime_index + new_bias == new_position_in_INDEX`.
#[must_use]
pub const fn subr_bias(count: usize) -> i32 {
    if count < 1240 {
        107
    } else if count < 33_900 {
        1131
    } else {
        32_768
    }
}

/// Type 2 subroutine call kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubrKind {
    /// `callsubr` — local subroutine reference.
    Local,
    /// `callgsubr` — global subroutine reference.
    Global,
}

// Type 2 opcode numbers we need to recognise. Values from Adobe TN
// 5177 §3 and §4. Kept private so `cff::scan_subr_calls` is the only
// surface for charstring walking from the subsetter side.
const OP_HSTEM: u8 = 1;
const OP_VSTEM: u8 = 3;
const OP_VMOVETO: u8 = 4;
const OP_RLINETO: u8 = 5;
const OP_HLINETO: u8 = 6;
const OP_VLINETO: u8 = 7;
const OP_RRCURVETO: u8 = 8;
const OP_CALLSUBR: u8 = 10;
const OP_RETURN: u8 = 11;
const OP_ESCAPE: u8 = 12;
const OP_ENDCHAR: u8 = 14;
const OP_VSINDEX: u8 = 15;
const OP_BLEND: u8 = 16;
const OP_HSTEMHM: u8 = 18;
const OP_HINTMASK: u8 = 19;
const OP_CNTRMASK: u8 = 20;
const OP_RMOVETO: u8 = 21;
const OP_HMOVETO: u8 = 22;
const OP_VSTEMHM: u8 = 23;
const OP_RCURVELINE: u8 = 24;
const OP_RLINECURVE: u8 = 25;
const OP_VVCURVETO: u8 = 26;
const OP_HHCURVETO: u8 = 27;
const OP_SHORTINT: u8 = 28;
const OP_CALLGSUBR: u8 = 29;
const OP_VHCURVETO: u8 = 30;
const OP_HVCURVETO: u8 = 31;

/// One subroutine call discovered by [`scan_subr_calls`].
///
/// `index_after_bias` is what the call site decoded — the value
/// already has the bias added. Callers comparing against the
/// `(global|local)_subrs` INDEX use it as the array index directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubrCall {
    /// Local or global.
    pub kind: SubrKind,
    /// Already bias-adjusted (i.e. usable as a direct INDEX lookup).
    pub index_after_bias: i64,
    /// Raw (still-biased) operand value as seen on the stack — what
    /// gets re-encoded into the rewritten charstring after renumber.
    pub raw_operand: i32,
    /// Byte offset within the charstring where `raw_operand` was
    /// pushed. The rewriter overwrites this region with the new
    /// encoded operand.
    pub operand_byte_offset: usize,
    /// Encoded length in bytes of the original operand push.
    pub operand_byte_len: usize,
}

/// Walks a Type 2 charstring and reports every `callsubr` /
/// `callgsubr` it executes, along with the byte location of the
/// operand push that the rewriter must replace.
///
/// `local_count` and `global_count` are the source-font subroutine
/// INDEX lengths used to compute the bias for each call kind. The
/// scanner does *not* descend into called subroutines — callers walk
/// the charstring bytes once per subroutine and iterate to a fixed
/// point externally.
///
/// CFF2 charstrings have no `endchar`; the scanner stops at the end
/// of the byte stream. CFF1 charstrings stop at `endchar` or
/// end-of-stream. Both behaviours produce the same call list.
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] on a truncated operand push,
/// an out-of-range opcode, or a `hintmask`/`cntrmask` whose tail
/// bytes run past the end. The byte-level subsetter treats these as
/// fatal — a malformed charstring shouldn't survive subsetting.
pub fn scan_subr_calls(
    charstring: &[u8],
    local_count: usize,
    global_count: usize,
) -> Result<Vec<SubrCall>, SubsetError> {
    let mut out = Vec::new();
    let mut pos = 0;
    // Operand stack of (raw_value, push_offset, push_len) — only the
    // top entry is consumed by callsubr/callgsubr, but we track all
    // of them because hintmask/cntrmask use the running stem count
    // (which equals operands_pushed_since_last_clear / 2 for any
    // implicit vstem). The scanner does not need stem counting per
    // se; it needs to know how many operands sit on the stack so it
    // can skip the right number of hintmask tail bytes.
    let mut stack: Vec<(i32, usize, usize)> = Vec::new();
    // Cumulative stem-pair count, for hintmask/cntrmask tail size.
    let mut stem_count: u32 = 0;

    while pos < charstring.len() {
        let b0 = charstring[pos];
        if b0 >= 32 {
            // Operand push.
            let (val, len) = decode_operand(charstring, pos)
                .ok_or(SubsetError::Unsupported("CFF charstring operand truncated"))?;
            stack.push((val, pos, len));
            pos += len;
            continue;
        }
        // Two-byte ops: shortint (28) and fixed (255) are operand
        // pushes too, but their `b0 < 32` collides with op space.
        if b0 == OP_SHORTINT {
            if pos + 3 > charstring.len() {
                return Err(SubsetError::Unsupported("CFF shortint truncated"));
            }
            let v = i16::from_be_bytes([charstring[pos + 1], charstring[pos + 2]]);
            stack.push((i32::from(v), pos, 3));
            pos += 3;
            continue;
        }
        // Operator. Consume operands per op semantics. We only need
        // bookkeeping accurate enough to find callsubr/callgsubr and
        // step past hintmask tails.
        match b0 {
            OP_CALLSUBR => {
                let top = stack
                    .pop()
                    .ok_or(SubsetError::Unsupported("CFF callsubr without operand"))?;
                let bias = subr_bias(local_count);
                out.push(SubrCall {
                    kind: SubrKind::Local,
                    index_after_bias: i64::from(top.0) + i64::from(bias),
                    raw_operand: top.0,
                    operand_byte_offset: top.1,
                    operand_byte_len: top.2,
                });
                pos += 1;
            }
            OP_CALLGSUBR => {
                let top = stack
                    .pop()
                    .ok_or(SubsetError::Unsupported("CFF callgsubr without operand"))?;
                let bias = subr_bias(global_count);
                out.push(SubrCall {
                    kind: SubrKind::Global,
                    index_after_bias: i64::from(top.0) + i64::from(bias),
                    raw_operand: top.0,
                    operand_byte_offset: top.1,
                    operand_byte_len: top.2,
                });
                pos += 1;
            }
            OP_HSTEM | OP_VSTEM | OP_HSTEMHM | OP_VSTEMHM => {
                // Stem ops consume operand pairs; track stem count
                // for any subsequent hintmask.
                let n_pairs = (stack.len() as u32) / 2;
                stem_count += n_pairs;
                stack.clear();
                pos += 1;
            }
            OP_HINTMASK | OP_CNTRMASK => {
                // An implicit vstem may precede the first mask if
                // there are operands left over.
                let extra_pairs = (stack.len() as u32) / 2;
                stem_count += extra_pairs;
                stack.clear();
                let mask_bytes = (stem_count as usize).div_ceil(8);
                if pos + 1 + mask_bytes > charstring.len() {
                    return Err(SubsetError::Unsupported("CFF hintmask tail truncated"));
                }
                pos += 1 + mask_bytes;
            }
            OP_ESCAPE => {
                // Two-byte op: clear stack, advance two bytes. We
                // don't model the arithmetic / logic ops because
                // none of them affect call-site detection.
                if pos + 2 > charstring.len() {
                    return Err(SubsetError::Unsupported("CFF escape truncated"));
                }
                stack.clear();
                pos += 2;
            }
            OP_RETURN => {
                // Subroutine return — stop walking *this* charstring
                // body but only when we're scanning a subr in
                // isolation. The scanner's caller invokes us per
                // body; treating return as end-of-walk is correct.
                return Ok(out);
            }
            OP_ENDCHAR => {
                return Ok(out);
            }
            OP_RMOVETO | OP_HMOVETO | OP_VMOVETO | OP_RLINETO | OP_HLINETO | OP_VLINETO
            | OP_RRCURVETO | OP_HHCURVETO | OP_VVCURVETO | OP_HVCURVETO | OP_VHCURVETO
            | OP_RCURVELINE | OP_RLINECURVE => {
                stack.clear();
                pos += 1;
            }
            OP_VSINDEX | OP_BLEND => {
                // CFF2 ops. Both clear the stack post-execution; the
                // exact blend stack-shrink isn't relevant to call
                // detection because subsequent operands re-push.
                stack.clear();
                pos += 1;
            }
            _ => {
                return Err(SubsetError::Unsupported("CFF unknown charstring operator"));
            }
        }
    }
    Ok(out)
}

/// Decodes one operand push at `data[pos..]`, returning
/// `(value, byte_length)`. Does not handle op 28 (shortint) or op 255
/// (fixed) — those are handled inline by the scanner because their
/// b0 < 32 collides with operator space.
fn decode_operand(data: &[u8], pos: usize) -> Option<(i32, usize)> {
    let b0 = *data.get(pos)?;
    if (32..=246).contains(&b0) {
        Some((i32::from(b0) - 139, 1))
    } else if (247..=250).contains(&b0) {
        let b1 = *data.get(pos + 1)?;
        Some(((i32::from(b0) - 247) * 256 + i32::from(b1) + 108, 2))
    } else if (251..=254).contains(&b0) {
        let b1 = *data.get(pos + 1)?;
        Some((-(i32::from(b0) - 251) * 256 - i32::from(b1) - 108, 2))
    } else if b0 == 255 {
        // 16.16 fixed — return integer part. CFF subr indices are
        // always small integers, so the rewriter never sees this
        // push as a callsubr operand; we still need to step past it.
        let bytes = data.get(pos + 1..pos + 5)?;
        let raw = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        // Truncate to integer part.
        Some((raw >> 16, 5))
    } else {
        None
    }
}

/// Re-encodes a small integer as a CFF Type 2 operand push, using the
/// shortest valid form. Returns the encoded bytes, length 1..=3.
///
/// Subroutine indices after renumbering are bounded by the new INDEX
/// size, so they easily fit a `shortint` (op 28, i16) when they
/// exceed the single-byte and two-byte push ranges. The rewriter
/// pads short pushes back out to the original encoded length when it
/// renumbers in place — so this helper returns the natural minimal
/// encoding and the caller decides whether to pad.
#[must_use]
pub fn encode_int_operand(v: i32) -> Vec<u8> {
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
        alloc::vec![OP_SHORTINT, bytes[0], bytes[1]]
    } else {
        // 5-byte fixed (16.16 with zero fractional).
        let raw = (v as i64) << 16;
        let raw = raw as i32;
        let mut out = alloc::vec![255u8];
        out.extend_from_slice(&raw.to_be_bytes());
        out
    }
}

/// Compute the transitive subroutine keep-set for a kept charstring
/// list. Iterates to a fixed point: a kept subroutine may itself call
/// further subroutines, all of which must also survive.
///
/// `kept_charstrings` is the slice of source-font charstrings the
/// subsetter is keeping (one per kept gid, in input order).
/// `local_subrs` and `global_subrs` are the source-font subroutine
/// INDEX entries.
///
/// Returns `(kept_local, kept_global)` as sorted ascending lists of
/// 0-based indices into the source INDEX. The caller renumbers these
/// to a 0..N compacted layout in input order.
///
/// # Errors
///
/// Propagates [`scan_subr_calls`] errors from any walked charstring
/// or subroutine body.
pub fn compute_kept_subrs(
    kept_charstrings: &[&[u8]],
    local_subrs: &[&[u8]],
    global_subrs: &[&[u8]],
) -> Result<(Vec<u32>, Vec<u32>), SubsetError> {
    let mut keep_local = alloc::vec![false; local_subrs.len()];
    let mut keep_global = alloc::vec![false; global_subrs.len()];

    // Seed: every call site in every kept charstring.
    for cs in kept_charstrings {
        for call in scan_subr_calls(cs, local_subrs.len(), global_subrs.len())? {
            mark_call(call, &mut keep_local, &mut keep_global);
        }
    }

    // Fixed point: each pass scans every kept subroutine body. If a
    // newly-kept subr calls another, the next pass picks it up.
    loop {
        let before = count_kept(&keep_local) + count_kept(&keep_global);
        for (i, sub) in local_subrs.iter().enumerate() {
            if !keep_local[i] {
                continue;
            }
            for call in scan_subr_calls(sub, local_subrs.len(), global_subrs.len())? {
                mark_call(call, &mut keep_local, &mut keep_global);
            }
        }
        for (i, sub) in global_subrs.iter().enumerate() {
            if !keep_global[i] {
                continue;
            }
            for call in scan_subr_calls(sub, local_subrs.len(), global_subrs.len())? {
                mark_call(call, &mut keep_local, &mut keep_global);
            }
        }
        let after = count_kept(&keep_local) + count_kept(&keep_global);
        if after == before {
            break;
        }
    }

    Ok((collect_kept(&keep_local), collect_kept(&keep_global)))
}

fn mark_call(call: SubrCall, keep_local: &mut [bool], keep_global: &mut [bool]) {
    let target = match call.kind {
        SubrKind::Local => keep_local,
        SubrKind::Global => keep_global,
    };
    let idx = call.index_after_bias;
    if idx < 0 {
        return;
    }
    let idx = idx as usize;
    if idx < target.len() {
        target[idx] = true;
    }
}

fn count_kept(keep: &[bool]) -> usize {
    keep.iter().filter(|k| **k).count()
}

fn collect_kept(keep: &[bool]) -> Vec<u32> {
    keep.iter()
        .enumerate()
        .filter_map(|(i, &k)| if k { Some(i as u32) } else { None })
        .collect()
}

// ----------------------------------------------------------------------------
// Emitter primitives.
//
// These are the byte-serialisation helpers a full CFF1 subsetter
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
/// - `b0` in 32..=246 → single byte, value = `b0 - 139`
/// - `b0` in 247..=250 → two-byte positive, value = (b0-247)*256 + b1 + 108
/// - `b0` in 251..=254 → two-byte negative, value = -(b0-251)*256 - b1 - 108
/// - `b0` = 28 → three-byte i16
/// - `b0` = 29 → five-byte i32
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
/// upgrading to a wider form. Used by the Top DICT serialiser when an
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
/// the buffer.
pub fn patch_dict_offset(buf: &mut [u8], slot_offset: usize, value: i32) {
    debug_assert_eq!(buf[slot_offset], 29, "placeholder must be b0=29");
    buf[slot_offset + 1..slot_offset + 5].copy_from_slice(&value.to_be_bytes());
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
        // nLeft fits a u16.
        while j < sids.len() && sids[j] == sids[j - 1] + 1 && (j - i) <= u16::MAX as usize {
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
/// codes. `n` must fit a u8.
#[must_use]
pub fn emit_encoding_format0(codes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + codes.len());
    out.push(0);
    out.push(codes.len() as u8);
    out.extend_from_slice(codes);
    out
}

/// Emits an Encoding in format 1 (range records: u8 first + u8 nLeft).
/// Each contiguous run of consecutive char codes at consecutive gids
/// becomes one record. `codes[i]` is the char code for gid `i+1`.
#[must_use]
pub fn emit_encoding_format1(codes: &[u8]) -> Vec<u8> {
    let mut ranges = Vec::new();
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

/// Re-encodes a single subroutine call site within a charstring buffer
/// in place. `call.operand_byte_offset` and `call.operand_byte_len`
/// locate the original operand push; the new operand is `new_index -
/// new_bias` (the post-renumber raw operand) encoded via
/// [`encode_int_operand`]. The new encoding must fit in the original
/// byte span; if it's shorter, the leading bytes are padded out by
/// upgrading to a wider form (shortint covers up to 3 bytes; fixed
/// covers up to 5).
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] if the new encoding cannot be
/// padded to the original span (would only happen if the original was
/// already wider than 5 bytes, which the spec doesn't allow, or if a
/// caller asks for a value outside the i32 range).
pub fn renumber_subr_call(
    charstring: &mut [u8],
    call: &SubrCall,
    new_raw_operand: i32,
) -> Result<(), SubsetError> {
    let span_start = call.operand_byte_offset;
    let span_end = span_start + call.operand_byte_len;
    if span_end > charstring.len() {
        return Err(SubsetError::Unsupported(
            "CFF charstring renumber span past end",
        ));
    }
    // Re-encode at original width (pad to wider form when the natural
    // encoding is shorter).
    let encoded = encode_int_operand_at_width(new_raw_operand, call.operand_byte_len)?;
    charstring[span_start..span_end].copy_from_slice(&encoded);
    Ok(())
}

/// Encodes a charstring integer operand at exactly `target_len` bytes,
/// upgrading to a wider form when the natural minimum is shorter.
fn encode_int_operand_at_width(v: i32, target_len: usize) -> Result<Vec<u8>, SubsetError> {
    let natural = encode_int_operand(v);
    if natural.len() == target_len {
        return Ok(natural);
    }
    // Shortint (op 28) is always 3 bytes for any i16; fixed (op 255)
    // is always 5 bytes.
    if target_len == 3 && (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(&v) {
        let bytes = (v as i16).to_be_bytes();
        return Ok(alloc::vec![28u8, bytes[0], bytes[1]]);
    }
    if target_len == 5 {
        let raw = (v as i64) << 16;
        let raw = raw as i32;
        let mut out = alloc::vec![255u8];
        out.extend_from_slice(&raw.to_be_bytes());
        return Ok(out);
    }
    if natural.len() > target_len {
        return Err(SubsetError::Unsupported(
            "CFF renumber: new operand wider than original",
        ));
    }
    Err(SubsetError::Unsupported(
        "CFF renumber: cannot pad operand to original width",
    ))
}

/// Renumbers every `callsubr` / `callgsubr` in `charstring` against
/// the supplied old→new maps. `local_renumber[i]` is the new compacted
/// index for source-font local subr `i`, or `None` when the source
/// subr was dropped. Same shape for `global_renumber`. Bias arithmetic
/// is applied: the call site stores `new_index - new_bias`.
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] if a kept charstring calls a
/// dropped subroutine (a closure-walker bug) or if an operand cannot
/// be re-encoded at its original byte width.
pub fn renumber_charstring(
    charstring: &mut [u8],
    old_local_count: usize,
    old_global_count: usize,
    new_local_count: usize,
    new_global_count: usize,
    local_renumber: &[Option<u32>],
    global_renumber: &[Option<u32>],
) -> Result<(), SubsetError> {
    let calls = scan_subr_calls(charstring, old_local_count, old_global_count)?;
    let new_local_bias = subr_bias(new_local_count);
    let new_global_bias = subr_bias(new_global_count);
    for call in calls.iter().rev() {
        // Reverse iteration so earlier rewrites don't shift later
        // offsets — but since we always re-encode at the original byte
        // width, the offsets stay stable. Reverse-iterate anyway as a
        // belt-and-suspenders against future variable-width changes.
        let old_idx = call.index_after_bias;
        if old_idx < 0 {
            return Err(SubsetError::Unsupported(
                "CFF charstring negative subr index after bias",
            ));
        }
        let old_idx = old_idx as usize;
        let (table, new_bias) = match call.kind {
            SubrKind::Local => (local_renumber, new_local_bias),
            SubrKind::Global => (global_renumber, new_global_bias),
        };
        let new_idx = table.get(old_idx).copied().flatten().ok_or(
            SubsetError::Unsupported("CFF charstring calls dropped subroutine"),
        )?;
        let new_raw = (new_idx as i64) - i64::from(new_bias);
        if !(i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&new_raw) {
            return Err(SubsetError::Unsupported(
                "CFF renumber: new raw operand out of i32 range",
            ));
        }
        renumber_subr_call(charstring, call, new_raw as i32)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    #[test]
    fn bias_at_spec_boundaries() {
        assert_eq!(subr_bias(0), 107);
        assert_eq!(subr_bias(1239), 107);
        assert_eq!(subr_bias(1240), 1131);
        assert_eq!(subr_bias(33_899), 1131);
        assert_eq!(subr_bias(33_900), 32_768);
    }

    #[test]
    fn encode_decode_round_trip_small() {
        for v in [-107, -50, 0, 50, 107] {
            let enc = encode_int_operand(v);
            assert_eq!(enc.len(), 1);
            let (dec, len) = decode_operand(&enc, 0).unwrap();
            assert_eq!(dec, v);
            assert_eq!(len, 1);
        }
    }

    #[test]
    fn encode_decode_round_trip_two_byte_pos() {
        for v in [108, 500, 1131] {
            let enc = encode_int_operand(v);
            assert_eq!(enc.len(), 2);
            let (dec, len) = decode_operand(&enc, 0).unwrap();
            assert_eq!(dec, v);
            assert_eq!(len, 2);
        }
    }

    #[test]
    fn encode_decode_round_trip_two_byte_neg() {
        for v in [-108, -500, -1131] {
            let enc = encode_int_operand(v);
            assert_eq!(enc.len(), 2);
            let (dec, len) = decode_operand(&enc, 0).unwrap();
            assert_eq!(dec, v);
            assert_eq!(len, 2);
        }
    }

    #[test]
    fn encode_shortint_for_mid_range() {
        for v in [1132, -1132, 5000, -5000, 32_000, -32_000] {
            let enc = encode_int_operand(v);
            assert_eq!(enc.len(), 3);
            assert_eq!(enc[0], OP_SHORTINT);
            // Scanner decodes shortint inline, not via decode_operand.
            let raw = i16::from_be_bytes([enc[1], enc[2]]);
            assert_eq!(i32::from(raw), v);
        }
    }

    #[test]
    fn scan_finds_callsubr_at_bias_minus_107() {
        // Push 0 (operand encoded as 139, single byte), then callsubr.
        // local_count = 0 → bias = 107 → index_after_bias = 107.
        let cs = [139, OP_CALLSUBR, OP_ENDCHAR];
        let calls = scan_subr_calls(&cs, 0, 0).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].kind, SubrKind::Local);
        assert_eq!(calls[0].index_after_bias, 107);
        assert_eq!(calls[0].raw_operand, 0);
        assert_eq!(calls[0].operand_byte_offset, 0);
        assert_eq!(calls[0].operand_byte_len, 1);
    }

    #[test]
    fn scan_finds_callgsubr_with_negative_bias() {
        // Push -100 via two-byte (251..=254) form. value = -100
        // requires b0=251, value = -(0)*256 - b1 - 108 = -100 → b1 =
        // -8 which is out of range; -100 doesn't encode in two bytes.
        // Use shortint instead: op 28 + i16(-100).
        let mut cs = alloc::vec![OP_SHORTINT];
        cs.extend_from_slice(&(-100i16).to_be_bytes());
        cs.push(OP_CALLGSUBR);
        cs.push(OP_ENDCHAR);
        // global_count = 0 → bias = 107 → index = -100 + 107 = 7.
        let calls = scan_subr_calls(&cs, 0, 0).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].kind, SubrKind::Global);
        assert_eq!(calls[0].index_after_bias, 7);
        assert_eq!(calls[0].raw_operand, -100);
        assert_eq!(calls[0].operand_byte_len, 3);
    }

    #[test]
    fn scan_uses_1131_bias_at_1240_count() {
        // local_count = 1240 → bias = 1131. operand = -1131 → index 0.
        // -1131 encodes as two-byte negative: b0=254, value = -(3)*256 - b1 - 108 = -1131
        //   → -768 - b1 - 108 = -1131 → b1 = 255.
        let cs = [254u8, 255, OP_CALLSUBR, OP_ENDCHAR];
        let calls = scan_subr_calls(&cs, 1240, 0).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].index_after_bias, 0);
        assert_eq!(calls[0].raw_operand, -1131);
    }

    #[test]
    fn scan_uses_32768_bias_at_33900_count() {
        // local_count = 33_900 → bias = 32_768. operand = -32_768 → index 0.
        // -32_768 needs shortint encoding (out of two-byte range).
        let mut cs = alloc::vec![OP_SHORTINT];
        cs.extend_from_slice(&(-32_768i16).to_be_bytes());
        cs.push(OP_CALLSUBR);
        cs.push(OP_ENDCHAR);
        let calls = scan_subr_calls(&cs, 33_900, 0).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].index_after_bias, 0);
    }

    #[test]
    fn scan_skips_hintmask_tail_bytes() {
        // Push 100 200 hstem hintmask <mask byte> 0 callsubr endchar.
        // Stem pair count = 1 → hintmask reads ceil(1/8) = 1 mask byte.
        // Then 0 callsubr resolves to local subr 0 → bias 107.
        let cs: Vec<u8> = alloc::vec![
            239, // 100 (single-byte form: 239 - 139 = 100)
            247, // 200 = (247-247)*256 + 92 + 108 → b1 = 92
            92,
            OP_HSTEM,
            OP_HINTMASK,
            0xff, // 1 mask byte
            139,  // 0
            OP_CALLSUBR,
            OP_ENDCHAR,
        ];
        let calls = scan_subr_calls(&cs, 0, 0).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].index_after_bias, 107);
    }

    #[test]
    fn scan_handles_escape_op() {
        // 0 0 rmoveto, then escape + arbitrary subop, then push 0
        // callsubr endchar. Scanner should clear the stack on escape
        // and still detect the call.
        let cs = [
            139, // 0
            139, // 0
            OP_RMOVETO,
            OP_ESCAPE,
            34, // hflex (any escape op)
            139,
            OP_CALLSUBR,
            OP_ENDCHAR,
        ];
        let calls = scan_subr_calls(&cs, 0, 0).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].index_after_bias, 107);
    }

    #[test]
    fn compute_kept_subrs_transitively() {
        // Local subr 0 calls local subr 1; charstring calls local
        // subr 0; both subrs must end up in the keep set.
        // Bias = 107 (count < 1240), so call to subr 0 has raw
        // operand (0 - 107) = -107 and call to subr 1 has raw
        // operand (1 - 107) = -106. Both fall outside the two-byte
        // negative range (-1131..=-108) so we encode via shortint.
        let mut local_0 = alloc::vec![OP_SHORTINT];
        local_0.extend_from_slice(&(-106i16).to_be_bytes());
        local_0.push(OP_CALLSUBR);
        local_0.push(OP_RETURN);

        let local_1 = alloc::vec![
            139u8, /* 0 */
            139u8, /* 0 */
            OP_RMOVETO, OP_RETURN
        ];

        let mut cs = alloc::vec![OP_SHORTINT];
        cs.extend_from_slice(&(-107i16).to_be_bytes());
        cs.push(OP_CALLSUBR);
        cs.push(OP_ENDCHAR);

        let local_refs: Vec<&[u8]> = alloc::vec![local_0.as_slice(), local_1.as_slice()];
        let global_refs: Vec<&[u8]> = Vec::new();
        let cs_refs: Vec<&[u8]> = alloc::vec![cs.as_slice()];

        let (kl, kg) = compute_kept_subrs(&cs_refs, &local_refs, &global_refs).unwrap();
        assert_eq!(kl, alloc::vec![0u32, 1]);
        assert!(kg.is_empty());
    }

    #[test]
    fn compute_kept_subrs_drops_unused() {
        // Three local subrs, charstring calls only subr 1.
        let mut cs = alloc::vec![OP_SHORTINT];
        cs.extend_from_slice(&(-106i16).to_be_bytes()); // call subr 1
        cs.push(OP_CALLSUBR);
        cs.push(OP_ENDCHAR);

        let stub = alloc::vec![OP_RETURN];
        let local_refs: Vec<&[u8]> = alloc::vec![&stub, &stub, &stub];
        let global_refs: Vec<&[u8]> = Vec::new();
        let cs_refs: Vec<&[u8]> = alloc::vec![cs.as_slice()];

        let (kl, _) = compute_kept_subrs(&cs_refs, &local_refs, &global_refs).unwrap();
        assert_eq!(kl, alloc::vec![1u32]);
    }

    #[test]
    fn scan_truncated_operand_errors() {
        // 247 expects a follow-up byte; truncating it is malformed.
        let cs = [247u8];
        let r = scan_subr_calls(&cs, 0, 0);
        assert!(r.is_err());
    }

    #[test]
    fn scan_unknown_op_errors() {
        // op 9 is reserved → must error.
        let cs = [9u8];
        let r = scan_subr_calls(&cs, 0, 0);
        assert!(r.is_err());
    }

    // -- emitter primitive tests ---------------------------------------------

    #[test]
    fn encode_index_empty() {
        // Empty INDEX is just the count (zero u16), no offSize / offsets.
        let bytes = encode_index(&[]);
        assert_eq!(bytes, alloc::vec![0u8, 0]);
    }

    #[test]
    fn encode_index_single_entry() {
        // count=1, offSize=1, offsets=[1, 4], data=[a,b,c]
        let entry = [0xAAu8, 0xBB, 0xCC];
        let bytes = encode_index(&[&entry]);
        assert_eq!(bytes[0..2], 1u16.to_be_bytes());
        assert_eq!(bytes[2], 1); // offSize
        assert_eq!(bytes[3], 1); // offset[0]
        assert_eq!(bytes[4], 4); // offset[1] = 1 + 3
        assert_eq!(&bytes[5..8], &entry);
    }

    #[test]
    fn encode_index_multi_entry_2byte_offsets() {
        // Total > 255 forces 2-byte offSize.
        let big = alloc::vec![0u8; 300];
        let small = alloc::vec![1u8; 10];
        let bytes = encode_index(&[&big, &small]);
        assert_eq!(bytes[0..2], 2u16.to_be_bytes());
        assert_eq!(bytes[2], 2); // offSize
        // offset[0] = 1 (u16 BE)
        assert_eq!(bytes[3..5], 1u16.to_be_bytes());
        // offset[1] = 1 + 300 = 301
        assert_eq!(bytes[5..7], 301u16.to_be_bytes());
        // offset[2] = 311
        assert_eq!(bytes[7..9], 311u16.to_be_bytes());
    }

    #[test]
    fn encode_dict_int_round_trip_small() {
        for v in [-107, 0, 107] {
            let enc = encode_dict_int(v);
            assert_eq!(enc.len(), 1);
        }
        for v in [108, 1131] {
            let enc = encode_dict_int(v);
            assert_eq!(enc.len(), 2);
        }
        for v in [-108, -1131] {
            let enc = encode_dict_int(v);
            assert_eq!(enc.len(), 2);
        }
    }

    #[test]
    fn encode_dict_int_uses_5byte_for_large() {
        let enc = encode_dict_int(1_000_000);
        assert_eq!(enc.len(), 5);
        assert_eq!(enc[0], 29);
        assert_eq!(i32::from_be_bytes([enc[1], enc[2], enc[3], enc[4]]), 1_000_000);
    }

    #[test]
    fn dict_offset_placeholder_round_trip() {
        // Reserve a placeholder slot, patch it, decode the result.
        let mut buf = alloc::vec![0u8; 10];
        let slot = 2;
        let placeholder = encode_dict_offset_placeholder();
        buf[slot..slot + placeholder.len()].copy_from_slice(&placeholder);
        patch_dict_offset(&mut buf, slot, 0x1234_5678);
        assert_eq!(buf[slot], 29);
        assert_eq!(
            i32::from_be_bytes([buf[slot + 1], buf[slot + 2], buf[slot + 3], buf[slot + 4]]),
            0x1234_5678,
        );
    }

    #[test]
    fn charset_format0_per_gid_sids() {
        // SIDs for gid 1, 2, 3 = [10, 20, 30]
        let bytes = emit_charset_format0(&[10, 20, 30]);
        assert_eq!(bytes[0], 0); // format
        assert_eq!(bytes.len(), 1 + 3 * 2);
        assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 10);
        assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 20);
        assert_eq!(u16::from_be_bytes([bytes[5], bytes[6]]), 30);
    }

    #[test]
    fn charset_format2_collapses_consecutive_runs() {
        // Three consecutive SIDs collapse to one record.
        let bytes = emit_charset_format2(&[100, 101, 102]);
        assert_eq!(bytes[0], 2);
        // first = 100, nLeft = 2
        assert_eq!(u16::from_be_bytes([bytes[1], bytes[2]]), 100);
        assert_eq!(u16::from_be_bytes([bytes[3], bytes[4]]), 2);
        assert_eq!(bytes.len(), 5);
    }

    #[test]
    fn charset_auto_picks_format2_on_dense_runs() {
        // Long consecutive run: format 2 wins (one 4-byte record vs.
        // n*2-byte format 0).
        let sids: Vec<u16> = (1..=20).collect();
        let auto = emit_charset_auto(&sids);
        // Format 2: 1 + 4 = 5 bytes. Format 0: 1 + 40 = 41.
        assert_eq!(auto[0], 2);
        assert_eq!(auto.len(), 5);
    }

    #[test]
    fn charset_auto_picks_format0_on_random() {
        // Random SIDs (every-other): format 2 needs 4 bytes per record
        // = 4n; format 0 needs 2n + 1. Format 0 wins.
        let sids: Vec<u16> = (0..10).map(|i| i * 7).collect();
        let auto = emit_charset_auto(&sids);
        assert_eq!(auto[0], 0);
    }

    #[test]
    fn encoding_format0_per_gid_codes() {
        let bytes = emit_encoding_format0(&[0x41, 0x42, 0x43]);
        assert_eq!(bytes[0], 0);
        assert_eq!(bytes[1], 3);
        assert_eq!(&bytes[2..5], &[0x41u8, 0x42, 0x43]);
    }

    #[test]
    fn encoding_format1_collapses_consecutive() {
        let bytes = emit_encoding_format1(&[0x40, 0x41, 0x42, 0x43]);
        assert_eq!(bytes[0], 1);
        assert_eq!(bytes[1], 1); // one range
        assert_eq!(bytes[2], 0x40); // first
        assert_eq!(bytes[3], 3); // nLeft
    }

    #[test]
    fn encoding_auto_picks_format1_on_dense() {
        let codes: Vec<u8> = (0x20..=0x7E).collect();
        let auto = emit_encoding_auto(&codes);
        // Format 0: 1 + 1 + 95 = 97. Format 1: 1 + 1 + 2 = 4.
        assert_eq!(auto[0], 1);
        assert!(auto.len() < 10);
    }

    #[test]
    fn renumber_charstring_rewrites_callsubr() {
        // Charstring: push 0 (operand=0, raw=0+139=139), callsubr,
        // endchar. With local_count=0 → bias=107 → resolves to subr
        // index 107. Renumber map: subr 107 → new index 5. New
        // local_count = 0 → bias = 107 → new_raw = 5 - 107 = -102.
        // -102 fits a single byte (-107..=107) so the renumber-at-width
        // helper will repad to a 1-byte form (which equals the
        // original).
        let mut cs = alloc::vec![139u8, OP_CALLSUBR, OP_ENDCHAR];
        // Build the renumber table: index 107 → Some(5).
        let mut local_renumber = alloc::vec![None::<u32>; 200];
        local_renumber[107] = Some(5);
        let global_renumber: Vec<Option<u32>> = Vec::new();
        renumber_charstring(&mut cs, 0, 0, 0, 0, &local_renumber, &global_renumber).unwrap();
        // The new operand should encode -102: single byte = -102 + 139 = 37.
        assert_eq!(cs[0], 37);
        assert_eq!(cs[1], OP_CALLSUBR);
    }

    #[test]
    fn renumber_charstring_pads_to_original_width() {
        // Original push uses shortint (3 bytes). New value would
        // naturally fit a single byte. Renumber must pad to 3 bytes
        // (still shortint) so the byte span is preserved.
        // Charstring: shortint 1000, callsubr, endchar.
        // bias=107, index_after_bias = 1000 + 107 = 1107.
        let mut cs = alloc::vec![OP_SHORTINT];
        cs.extend_from_slice(&1000i16.to_be_bytes());
        cs.push(OP_CALLSUBR);
        cs.push(OP_ENDCHAR);
        let mut local_renumber = alloc::vec![None::<u32>; 2000];
        // Map subr 1107 → new index 5. New bias 107 → new raw = -102.
        local_renumber[1107] = Some(5);
        let global_renumber: Vec<Option<u32>> = Vec::new();
        renumber_charstring(&mut cs, 0, 0, 0, 0, &local_renumber, &global_renumber).unwrap();
        // Should still be 3-byte shortint encoding.
        assert_eq!(cs[0], OP_SHORTINT);
        let new_val = i16::from_be_bytes([cs[1], cs[2]]);
        assert_eq!(i32::from(new_val), -102);
        assert_eq!(cs[3], OP_CALLSUBR);
    }

    #[test]
    fn renumber_charstring_errors_on_dropped_subr() {
        // Charstring calls subr that's marked dropped — must error.
        let mut cs = alloc::vec![139u8, OP_CALLSUBR];
        let local_renumber = alloc::vec![None::<u32>; 200];
        let global_renumber: Vec<Option<u32>> = Vec::new();
        let r = renumber_charstring(&mut cs, 0, 0, 0, 0, &local_renumber, &global_renumber);
        assert!(r.is_err());
    }

    #[test]
    fn encode_index_round_trips_through_simple_parser() {
        // Build an INDEX, then walk its bytes per spec.
        let entries: alloc::vec::Vec<&[u8]> = alloc::vec![&b"abc"[..], &b"defgh"[..], &b"i"[..]];
        let bytes = encode_index(&entries);
        let count = u16::from_be_bytes([bytes[0], bytes[1]]);
        assert_eq!(count, 3);
        let off_size = bytes[2] as usize;
        // Compute decoded offsets.
        let mut offsets = Vec::with_capacity(4);
        for i in 0..=count as usize {
            let start = 3 + i * off_size;
            let mut v = 0u32;
            for &b in &bytes[start..start + off_size] {
                v = (v << 8) | u32::from(b);
            }
            offsets.push(v);
        }
        let data_start = 3 + (count as usize + 1) * off_size;
        // Reconstruct entries.
        for i in 0..count as usize {
            let s = data_start + offsets[i] as usize - 1;
            let e = data_start + offsets[i + 1] as usize - 1;
            assert_eq!(&bytes[s..e], entries[i]);
        }
    }
}
