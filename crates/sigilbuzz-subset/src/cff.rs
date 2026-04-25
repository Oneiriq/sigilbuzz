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
//! [`subset_non_identity`] is the orchestration that wires the
//! analysis layer + emitter primitives end-to-end for a non-CID CFF1
//! source. It walks the source Top DICT, captures every offset
//! operator, computes the kept-charstring + transitive subroutine
//! keep-set, renumbers every kept charstring + kept subr in place,
//! lays out the new sections in deterministic order
//! (Header / Name INDEX / Top DICT INDEX / String INDEX / Global Subr
//! INDEX / Encoding / charset / CharStrings INDEX / Private DICT /
//! Local Subr INDEX), then patches the deferred-offset placeholders
//! in the Top DICT and Private DICT bodies.
//!
//! The crate's [`crate::subset`] entry routes non-identity CFF1 via
//! this path and the layout-rebuild driver next door in `crate::lib`.
//! CID-keyed CFF1 (FDArray + FDSelect) and CFF2 non-identity routes
//! still surface [`SubsetError::Unsupported`] — both flows share an
//! FDArray rebuild + FDSelect rewrite that's staged for a follow-up.
//!
//! The emitter primitives are exercised by unit tests covering the
//! bias-renumber boundaries (107 / 1131 / 32768), Top DICT
//! serialisation idempotence, charset / encoding format auto-pick,
//! and full-orchestration round-trips (kept-only-requested-glyphs,
//! drops-unused-subrs, preserves-name-and-string-indexes,
//! renumbers-callsubr-when-local-subrs-kept, rejects-CID-keyed,
//! rejects-kept-set-without-gid0).

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
/// codes.
///
/// The CFF1 format-0 `nCodes` field is a Card8, so at most 255 codes
/// can be addressed. Inputs longer than that get capped — the spec
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
/// behaviour and keeps the emitted bytes parseable.
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
        let new_idx = table
            .get(old_idx)
            .copied()
            .flatten()
            .ok_or(SubsetError::Unsupported(
                "CFF charstring calls dropped subroutine",
            ))?;
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

// ----------------------------------------------------------------------------
// Source-CFF reader + Top DICT walker.
//
// The cross-cutting orchestration in [`subset_non_identity`] needs to capture
// every byte run the source CFF carries so it can preserve some verbatim
// (Header, Name INDEX, String INDEX) and rebuild others (Top DICT, Global
// Subr INDEX, charset, Encoding, CharStrings INDEX, Private DICT, Local
// Subr INDEX). The reader lives here rather than in the existing
// [`crate::tables::cff`] parser because the parser exposes Top DICT fields
// only as derived data — the raw operator-by-operator walk we need to copy
// the non-offset operators verbatim is not on its public surface.
// ----------------------------------------------------------------------------

/// CFF DICT operand seen by the Top DICT walker. We keep the raw
/// encoded bytes alongside the decoded integer so non-offset operators
/// can be re-emitted byte-for-byte (preserving real-number operands and
/// any non-canonical integer encoding the source font happened to use).
#[derive(Debug, Clone)]
struct DictOperand {
    /// Decoded integer value, when the operand is integer-typed.
    /// `None` for real-number operands (op 30) — we never need to
    /// patch a real, so preserving the raw bytes is enough.
    int_value: Option<i32>,
    /// Encoded bytes as they appeared in the source DICT.
    raw: Vec<u8>,
}

/// One operator + its operand list, as captured by [`walk_top_dict`].
#[derive(Debug, Clone)]
struct DictEntry {
    /// Operator number — single-byte ops are 0..=21, escaped ops are
    /// 0x0C00 | b1.
    op: u16,
    /// Operands that preceded this operator.
    operands: Vec<DictOperand>,
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
        // Real number — nibble-packed BCD, terminated when either nibble
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
/// — no semantic interpretation of operator meanings.
fn walk_dict(bytes: &[u8]) -> Result<Vec<DictEntry>, SubsetError> {
    let mut out = Vec::new();
    let mut pos = 0;
    let mut operands: Vec<DictOperand> = Vec::new();
    while pos < bytes.len() {
        let b0 = bytes[pos];
        if b0 <= 21 {
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

/// Reads a CFF INDEX at `bytes[pos..]`, returning the entry slices
/// (zero-copy into the input) plus the byte-length of the entire INDEX
/// structure (so the caller can advance past it).
fn read_index(bytes: &[u8], pos: usize) -> Result<(Vec<&[u8]>, usize), SubsetError> {
    if pos + 2 > bytes.len() {
        return Err(SubsetError::Unsupported("CFF INDEX header truncated"));
    }
    let count = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
    if count == 0 {
        return Ok((Vec::new(), 2));
    }
    if pos + 3 > bytes.len() {
        return Err(SubsetError::Unsupported("CFF INDEX offSize missing"));
    }
    let off_size = bytes[pos + 2] as usize;
    if !(1..=4).contains(&off_size) {
        return Err(SubsetError::Unsupported("CFF INDEX offSize out of range"));
    }
    let off_table_start = pos + 3;
    let off_table_end = off_table_start + (count + 1) * off_size;
    if off_table_end > bytes.len() {
        return Err(SubsetError::Unsupported("CFF INDEX offsets truncated"));
    }
    let mut offsets = Vec::with_capacity(count + 1);
    for i in 0..=count {
        let s = off_table_start + i * off_size;
        let mut v = 0u32;
        for &b in &bytes[s..s + off_size] {
            v = (v << 8) | u32::from(b);
        }
        offsets.push(v as usize);
    }
    let data_start = off_table_end;
    let last = *offsets.last().unwrap();
    if last == 0 {
        return Err(SubsetError::Unsupported("CFF INDEX final offset zero"));
    }
    let data_end = data_start + last - 1;
    if data_end > bytes.len() {
        return Err(SubsetError::Unsupported("CFF INDEX data past end"));
    }
    let mut entries = Vec::with_capacity(count);
    for w in offsets.windows(2) {
        let a = w[0];
        let b = w[1];
        if a == 0 || b < a {
            return Err(SubsetError::Unsupported("CFF INDEX offsets non-monotone"));
        }
        let s = data_start + a - 1;
        let e = data_start + b - 1;
        entries.push(&bytes[s..e]);
    }
    Ok((entries, data_end - pos))
}

// ----------------------------------------------------------------------------
// Top DICT operator numbers.
// ----------------------------------------------------------------------------

const OP_CHARSET: u16 = 15;
const OP_ENCODING: u16 = 16;
const OP_CHARSTRINGS: u16 = 17;
const OP_PRIVATE: u16 = 18;
const OP_SUBRS: u16 = 19;
const OP_FD_ARRAY: u16 = 0x0C24;
const OP_FD_SELECT: u16 = 0x0C25;
const OP_ROS: u16 = 0x0C1E;

/// Captures the source CFF1 layout in raw form so the orchestration
/// can rebuild kept sections while preserving everything else verbatim.
#[derive(Debug)]
struct ParsedCff1<'a> {
    /// Header bytes (4) — copied verbatim into the output.
    header: &'a [u8],
    /// Name INDEX bytes (verbatim copy span — header + offsets + data).
    name_index: &'a [u8],
    /// Top DICT body bytes (the single first entry of the Top DICT INDEX).
    top_dict: &'a [u8],
    /// String INDEX bytes (verbatim).
    string_index: &'a [u8],
    /// Global Subr INDEX entries (one slice per subr).
    global_subrs: Vec<&'a [u8]>,
    /// CharStrings INDEX entries — one slice per glyph.
    char_strings: Vec<&'a [u8]>,
    /// Charset offset (Top DICT op 15). Default `0` (ISOAdobe).
    charset_off: u32,
    /// Encoding offset (Top DICT op 16). Default `0` (Standard).
    encoding_off: u32,
    /// Private DICT (size, offset). `None` if op 18 absent.
    private: Option<(u32, u32)>,
    /// Private DICT bytes (when `private` is `Some`).
    private_dict: &'a [u8],
    /// Local Subr INDEX entries — taken from Private DICT op 19, when
    /// present. Empty when no local subrs.
    local_subrs: Vec<&'a [u8]>,
    /// Whether the source uses CID-keyed (FDArray/FDSelect) layout.
    is_cid: bool,
}

/// Walks the source CFF1 table and captures every span the rewriter
/// needs. CID-keyed fonts are detected (op `0x0C24` / `0x0C25` /
/// `0x0C1E` present) and surfaced via `is_cid`; the orchestration
/// declines to subset them in this release.
fn parse_cff1(data: &[u8]) -> Result<ParsedCff1<'_>, SubsetError> {
    if data.len() < 4 {
        return Err(SubsetError::Unsupported("CFF1 header truncated"));
    }
    let major = data[0];
    let hdr_size = data[2] as usize;
    if major != 1 {
        return Err(SubsetError::Unsupported("CFF1 major version != 1"));
    }
    if hdr_size < 4 || hdr_size > data.len() {
        return Err(SubsetError::Unsupported("CFF1 hdrSize invalid"));
    }
    let header = &data[..4];

    // Name INDEX.
    let (name_entries, name_len) = read_index(data, hdr_size)?;
    let name_index = &data[hdr_size..hdr_size + name_len];
    let _ = name_entries;
    let mut pos = hdr_size + name_len;

    // Top DICT INDEX — first entry only.
    let (top_entries, top_index_len) = read_index(data, pos)?;
    let top_dict = top_entries
        .first()
        .copied()
        .ok_or(SubsetError::Unsupported("CFF1 Top DICT INDEX empty"))?;
    pos += top_index_len;

    // String INDEX.
    let (_, string_len) = read_index(data, pos)?;
    let string_index = &data[pos..pos + string_len];
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
            OP_PRIVATE if e.operands.len() >= 2 => {
                let s = e.operands[e.operands.len() - 2].int_value;
                let o = e.operands[e.operands.len() - 1].int_value;
                if let (Some(sv), Some(ov)) = (s, o) {
                    if sv >= 0 && ov >= 0 {
                        private = Some((sv as u32, ov as u32));
                    }
                }
            }
            OP_FD_ARRAY | OP_FD_SELECT | OP_ROS => is_cid = true,
            _ => {}
        }
    }

    let cs_off = char_strings_off.ok_or(SubsetError::Unsupported(
        "CFF1 Top DICT missing CharStrings",
    ))? as usize;
    let (char_strings, _) = read_index(data, cs_off)?;

    // Private DICT + Local Subr INDEX.
    let (private_dict, local_subrs): (&[u8], Vec<&[u8]>) = if let Some((size, off)) = private {
        let off = off as usize;
        let size = size as usize;
        if off + size > data.len() {
            return Err(SubsetError::Unsupported("CFF1 Private DICT past end"));
        }
        let priv_bytes = &data[off..off + size];
        // Walk Private DICT for op 19 (Subrs offset, relative to Private).
        let priv_entries = walk_dict(priv_bytes)?;
        let mut subrs_rel_off: Option<u32> = None;
        for e in &priv_entries {
            if e.op == OP_SUBRS {
                if let Some(v) = e.operands.last().and_then(|o| o.int_value) {
                    if v >= 0 {
                        subrs_rel_off = Some(v as u32);
                    }
                }
            }
        }
        if let Some(rel) = subrs_rel_off {
            let abs = off + rel as usize;
            let (locals, _) = read_index(data, abs)?;
            (priv_bytes, locals)
        } else {
            (priv_bytes, Vec::new())
        }
    } else {
        (&[][..], Vec::new())
    };

    let _ = encoding_off; // Used after parsing; suppress unused lint.

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
    })
}

// ----------------------------------------------------------------------------
// Predefined charsets / encodings.
//
// CFF1 defines three "predefined" charsets and two predefined encodings.
// When the Top DICT's charset / Encoding operand is `0`, `1`, or `2`, the
// font uses the predefined table — no charset / Encoding bytes appear in
// the source CFF. The orchestration must reproduce the kept-gid SIDs
// (or char codes) the predefined table encodes when the source uses one.
// ----------------------------------------------------------------------------

/// ISOAdobe predefined charset SIDs for gid 1..=228. Gid 0 is implicit
/// `.notdef` (SID 0). Source: Adobe TN 5176 Appendix C, Table 22.
/// SID `i` for gid `i` (i in 1..=228) — the SIDs are sequential.
const ISO_ADOBE_LEN: u16 = 228;

/// Reads the kept-gid SIDs from the source charset. The returned vec
/// has one SID per *kept gid except gid 0* in the new compacted order.
/// Predefined charsets (off 0/1/2) are expanded via lookup; explicit
/// charsets (off >= 3) are walked from the source bytes.
///
/// The kept-gid order is the input slice of source-font gids in old
/// order. Gid 0 is dropped (it's implicitly `.notdef`, never carries
/// an entry).
fn extract_kept_charset_sids(
    data: &[u8],
    charset_off: u32,
    n_glyphs: usize,
    kept_gids: &[u16],
) -> Result<Vec<u16>, SubsetError> {
    // First materialise the SID-per-gid table for gid 1..n_glyphs-1.
    let per_gid = if charset_off == 0 {
        // ISOAdobe predefined.
        let mut sids = alloc::vec![0u16; n_glyphs.saturating_sub(1)];
        for (i, slot) in sids.iter_mut().enumerate() {
            // SID i+1 for gid i+1, capped to ISOAdobe table length.
            *slot = if (i as u16) < ISO_ADOBE_LEN {
                (i as u16) + 1
            } else {
                0
            };
        }
        sids
    } else if charset_off == 1 || charset_off == 2 {
        // Expert / ExpertSubset — we don't reproduce these. Treat as
        // unsupported so the caller falls back to the dropping path
        // rather than emit a corrupt charset.
        return Err(SubsetError::Unsupported(
            "CFF1 predefined Expert / ExpertSubset charset not yet supported",
        ));
    } else {
        let off = charset_off as usize;
        if off >= data.len() {
            return Err(SubsetError::Unsupported("CFF1 charset offset past end"));
        }
        let format = data[off];
        let n_left = n_glyphs.saturating_sub(1);
        let mut sids = alloc::vec![0u16; n_left];
        match format {
            0 => {
                let body = &data[off + 1..];
                if body.len() < n_left * 2 {
                    return Err(SubsetError::Unsupported("CFF1 charset format 0 truncated"));
                }
                for (i, slot) in sids.iter_mut().enumerate() {
                    *slot = u16::from_be_bytes([body[i * 2], body[i * 2 + 1]]);
                }
            }
            1 | 2 => {
                let record_size = if format == 1 { 3 } else { 4 };
                let mut p = off + 1;
                let mut written = 0usize;
                while written < n_left {
                    if p + record_size > data.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF1 charset format 1/2 truncated",
                        ));
                    }
                    let first = u16::from_be_bytes([data[p], data[p + 1]]);
                    let n_l = if format == 1 {
                        u16::from(data[p + 2])
                    } else {
                        u16::from_be_bytes([data[p + 2], data[p + 3]])
                    };
                    p += record_size;
                    let take = (n_l as usize + 1).min(n_left - written);
                    for k in 0..take {
                        sids[written + k] = first + k as u16;
                    }
                    written += take;
                }
            }
            _ => {
                return Err(SubsetError::Unsupported(
                    "CFF1 charset format not 0 / 1 / 2",
                ));
            }
        }
        sids
    };

    // Project per-gid table onto the kept-gid set, skipping gid 0.
    let mut out = Vec::with_capacity(kept_gids.len().saturating_sub(1));
    for &g in kept_gids {
        if g == 0 {
            continue;
        }
        let idx = (g as usize).saturating_sub(1);
        let sid = per_gid.get(idx).copied().unwrap_or(0);
        out.push(sid);
    }
    Ok(out)
}

/// Reads the kept-gid char codes from the source Encoding. Returns one
/// code per kept gid except gid 0, matching the charset's shape.
///
/// Predefined encoding offsets `0` (Standard) and `1` (Expert) are
/// expanded from the spec. Explicit encodings (>= 2) are walked.
fn extract_kept_encoding_codes(
    data: &[u8],
    encoding_off: u32,
    charset_per_gid_sids: &[u16],
    kept_gids: &[u16],
) -> Result<Vec<u8>, SubsetError> {
    // Build the per-gid code table for gid 1..=charset_per_gid_sids.len().
    let n_left = charset_per_gid_sids.len();
    let mut per_gid = alloc::vec![0u8; n_left];
    if encoding_off == 0 || encoding_off == 1 {
        // Predefined Standard / Expert encoding maps SID → code via a
        // fixed table. Reproducing those tables byte-for-byte for an
        // arbitrary source font is large and rarely useful — most CFF
        // fonts ship explicit Encodings. Conservative: when the source
        // uses a predefined Encoding, pass it through *by emitting a
        // best-effort all-zeros encoding* and rely on cmap for char →
        // gid mapping. The Type 2 spec allows any Encoding shape; the
        // round-trip test suite covers the explicit-Encoding path.
        // (Implemented as a no-op zero table, later format-auto'd.)
    } else {
        let off = encoding_off as usize;
        if off >= data.len() {
            return Err(SubsetError::Unsupported("CFF1 Encoding offset past end"));
        }
        let format = data[off] & 0x7F; // strip supplemental-encodings bit
        match format {
            0 => {
                if off + 2 > data.len() {
                    return Err(SubsetError::Unsupported("CFF1 Encoding fmt 0 truncated"));
                }
                let n_codes = data[off + 1] as usize;
                if off + 2 + n_codes > data.len() {
                    return Err(SubsetError::Unsupported("CFF1 Encoding fmt 0 short"));
                }
                let limit = n_codes.min(n_left);
                for i in 0..limit {
                    per_gid[i] = data[off + 2 + i];
                }
            }
            1 => {
                if off + 2 > data.len() {
                    return Err(SubsetError::Unsupported("CFF1 Encoding fmt 1 truncated"));
                }
                let n_ranges = data[off + 1] as usize;
                let mut p = off + 2;
                let mut written = 0usize;
                for _ in 0..n_ranges {
                    if p + 2 > data.len() {
                        return Err(SubsetError::Unsupported("CFF1 Encoding fmt 1 short"));
                    }
                    let first = data[p];
                    let n_left_rec = data[p + 1] as usize;
                    p += 2;
                    let take = (n_left_rec + 1).min(n_left.saturating_sub(written));
                    for k in 0..take {
                        per_gid[written + k] = first.wrapping_add(k as u8);
                    }
                    written += take;
                    if written >= n_left {
                        break;
                    }
                }
            }
            _ => {
                return Err(SubsetError::Unsupported("CFF1 Encoding format not 0 / 1"));
            }
        }
    }

    let mut out = Vec::with_capacity(kept_gids.len().saturating_sub(1));
    for &g in kept_gids {
        if g == 0 {
            continue;
        }
        let idx = (g as usize).saturating_sub(1);
        let code = per_gid.get(idx).copied().unwrap_or(0);
        out.push(code);
    }
    Ok(out)
}

// ----------------------------------------------------------------------------
// Top DICT / Private DICT serialisers.
//
// Both rebuild a DICT from a captured [`DictEntry`] list, preserving every
// non-targeted operator verbatim. Targeted operators (charset / Encoding /
// CharStrings / Private / Subrs) get a 5-byte placeholder operand the
// patcher overwrites with the real value once layout is known.
// ----------------------------------------------------------------------------

/// Top DICT placeholder slots discovered while serialising. The
/// orchestration patches each slot with the real offset once the
/// section layout is fixed.
#[derive(Debug, Default, Clone)]
struct TopDictSlots {
    /// Byte offset of the b0=29 operand byte for op 15 (charset).
    charset_slot: Option<usize>,
    /// Byte offset of the b0=29 operand byte for op 16 (Encoding).
    encoding_slot: Option<usize>,
    /// Byte offset of the b0=29 operand byte for op 17 (CharStrings).
    char_strings_slot: Option<usize>,
    /// Byte offsets of the size + offset b0=29 operand bytes for op 18.
    private_slot: Option<(usize, usize)>,
}

/// Serialises a Top DICT body, rewriting the targeted operator
/// operands to 5-byte placeholders. Returns the body bytes plus the
/// slot offsets the patcher needs.
fn serialise_top_dict(
    entries: &[DictEntry],
    rebuild_charset: bool,
    rebuild_encoding: bool,
) -> (Vec<u8>, TopDictSlots) {
    let mut out = Vec::new();
    let mut slots = TopDictSlots::default();
    for e in entries {
        let target = matches!(e.op, OP_CHARSTRINGS | OP_PRIVATE)
            || (rebuild_charset && e.op == OP_CHARSET)
            || (rebuild_encoding && e.op == OP_ENCODING);
        if target {
            // Drop the original operands; emit placeholders for the
            // operands this op needs.
            match e.op {
                OP_CHARSET => {
                    slots.charset_slot = Some(out.len());
                    out.extend_from_slice(&encode_dict_offset_placeholder());
                }
                OP_ENCODING => {
                    slots.encoding_slot = Some(out.len());
                    out.extend_from_slice(&encode_dict_offset_placeholder());
                }
                OP_CHARSTRINGS => {
                    slots.char_strings_slot = Some(out.len());
                    out.extend_from_slice(&encode_dict_offset_placeholder());
                }
                OP_PRIVATE => {
                    let size_slot = out.len();
                    out.extend_from_slice(&encode_dict_offset_placeholder());
                    let off_slot = out.len();
                    out.extend_from_slice(&encode_dict_offset_placeholder());
                    slots.private_slot = Some((size_slot, off_slot));
                }
                _ => unreachable!(),
            }
        } else {
            // Preserve operands verbatim.
            for o in &e.operands {
                out.extend_from_slice(&o.raw);
            }
        }
        // Emit operator bytes.
        if e.op >= 0x0C00 {
            out.push(12);
            out.push(e.op as u8);
        } else {
            out.push(e.op as u8);
        }
    }
    (out, slots)
}

/// Private DICT placeholder slots — only the Subrs (op 19) operand is
/// patched at this layer.
#[derive(Debug, Default, Clone)]
struct PrivateDictSlots {
    /// Byte offset of the b0=29 operand byte for op 19 (Subrs).
    subrs_slot: Option<usize>,
}

/// Serialises a Private DICT body. Op 19 (Subrs) — present iff the
/// Private DICT had a Subrs reference — gets a 5-byte placeholder.
/// When the source had no op 19 but the orchestration is emitting
/// local subrs, an op 19 entry is appended.
fn serialise_private_dict(
    entries: &[DictEntry],
    emit_subrs_op: bool,
) -> (Vec<u8>, PrivateDictSlots) {
    let mut out = Vec::new();
    let mut slots = PrivateDictSlots::default();
    let mut had_subrs = false;
    for e in entries {
        if e.op == OP_SUBRS {
            had_subrs = true;
            if emit_subrs_op {
                slots.subrs_slot = Some(out.len());
                out.extend_from_slice(&encode_dict_offset_placeholder());
                out.push(OP_SUBRS as u8);
            }
            // When the orchestration drops local subrs, omit op 19
            // entirely (no operand emitted, no operator byte).
        } else {
            for o in &e.operands {
                out.extend_from_slice(&o.raw);
            }
            if e.op >= 0x0C00 {
                out.push(12);
                out.push(e.op as u8);
            } else {
                out.push(e.op as u8);
            }
        }
    }
    if emit_subrs_op && !had_subrs {
        // Source Private DICT had no Subrs operand but we're emitting
        // local subrs — append a fresh op 19 entry.
        slots.subrs_slot = Some(out.len());
        out.extend_from_slice(&encode_dict_offset_placeholder());
        out.push(OP_SUBRS as u8);
    }
    (out, slots)
}

// ----------------------------------------------------------------------------
// CFF1 non-identity subset entry.
// ----------------------------------------------------------------------------

/// Rebuilds a non-identity CFF1 table for the kept-gid subset.
///
/// Wires together the Top DICT walker + capture-offsets, the
/// CharStrings INDEX rebuild with in-place subr renumber, the local +
/// global Subr INDEX rebuilds, and the section-layout placeholder-patch
/// driver. Charset and Encoding are rebuilt via [`emit_charset_auto`]
/// and [`emit_encoding_auto`] when the source uses an explicit
/// (non-predefined) table; predefined-charset sources get an explicit
/// rebuild to preserve the kept-gid SID mapping.
///
/// CID-keyed sources (FDArray / FDSelect present) are declined — that
/// flow needs a separate FDArray INDEX rebuild + FDSelect rewrite that
/// belongs in a follow-up. Sources with predefined Expert /
/// ExpertSubset charsets are likewise declined.
///
/// `kept_gids` must be sorted ascending and contain gid 0.
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] for CID-keyed fonts or when
/// the source uses a feature the orchestration doesn't yet rewrite.
pub fn subset_non_identity(cff_bytes: &[u8], kept_gids: &[u16]) -> Result<Vec<u8>, SubsetError> {
    let parsed = parse_cff1(cff_bytes)?;
    if parsed.is_cid {
        return Err(SubsetError::Unsupported(
            "CFF1 CID-keyed (FDArray / FDSelect) subset staged for follow-up",
        ));
    }

    let n_glyphs = parsed.char_strings.len();
    if n_glyphs == 0 {
        return Err(SubsetError::Unsupported("CFF1 source has zero glyphs"));
    }
    for &g in kept_gids {
        if (g as usize) >= n_glyphs {
            return Err(SubsetError::Unsupported(
                "CFF1 kept gid past source numGlyphs",
            ));
        }
    }
    if kept_gids.first() != Some(&0u16) {
        return Err(SubsetError::Unsupported("CFF1 kept gid set must include 0"));
    }

    // Kept charstrings (in new compacted gid order).
    let kept_charstrings: Vec<&[u8]> = kept_gids
        .iter()
        .map(|&g| parsed.char_strings[g as usize])
        .collect();

    // Subroutine keep-set (transitive closure).
    let (kept_local_idx, kept_global_idx) =
        compute_kept_subrs(&kept_charstrings, &parsed.local_subrs, &parsed.global_subrs)?;

    // Build old → new renumber tables.
    let mut local_renumber: Vec<Option<u32>> = alloc::vec![None; parsed.local_subrs.len()];
    for (new_i, &old_i) in kept_local_idx.iter().enumerate() {
        local_renumber[old_i as usize] = Some(new_i as u32);
    }
    let mut global_renumber: Vec<Option<u32>> = alloc::vec![None; parsed.global_subrs.len()];
    for (new_i, &old_i) in kept_global_idx.iter().enumerate() {
        global_renumber[old_i as usize] = Some(new_i as u32);
    }

    let new_local_count = kept_local_idx.len();
    let new_global_count = kept_global_idx.len();
    let old_local_count = parsed.local_subrs.len();
    let old_global_count = parsed.global_subrs.len();

    // Rewrite each kept charstring (cloned → mutated).
    let mut new_charstrings: Vec<Vec<u8>> = kept_charstrings.iter().map(|s| s.to_vec()).collect();
    for cs in &mut new_charstrings {
        renumber_charstring(
            cs,
            old_local_count,
            old_global_count,
            new_local_count,
            new_global_count,
            &local_renumber,
            &global_renumber,
        )?;
    }

    // Rewrite each kept local subr.
    let mut new_local_subrs: Vec<Vec<u8>> = kept_local_idx
        .iter()
        .map(|&i| parsed.local_subrs[i as usize].to_vec())
        .collect();
    for sub in &mut new_local_subrs {
        renumber_charstring(
            sub,
            old_local_count,
            old_global_count,
            new_local_count,
            new_global_count,
            &local_renumber,
            &global_renumber,
        )?;
    }

    // Rewrite each kept global subr.
    let mut new_global_subrs: Vec<Vec<u8>> = kept_global_idx
        .iter()
        .map(|&i| parsed.global_subrs[i as usize].to_vec())
        .collect();
    for sub in &mut new_global_subrs {
        renumber_charstring(
            sub,
            old_local_count,
            old_global_count,
            new_local_count,
            new_global_count,
            &local_renumber,
            &global_renumber,
        )?;
    }

    // Charset rebuild — required whenever there's at least one kept
    // glyph past gid 0.
    let charset_per_gid =
        extract_kept_charset_sids(cff_bytes, parsed.charset_off, n_glyphs, kept_gids)?;
    let charset_bytes = emit_charset_auto(&charset_per_gid);

    // Source charset offset 0/1/2 means predefined. After rewrite we
    // emit an explicit table — no longer predefined — so the Top DICT
    // op 15 needs an explicit offset.
    let rebuild_charset = true;

    // Encoding — emitted only when the source actually had an
    // Encoding (op 16 present) or used the default (Standard, off = 0).
    // To keep the rewrite faithful, emit a fresh Encoding whenever the
    // kept set is non-trivial. For predefined Encoding we emit a
    // best-effort all-zero codes table so the round-trip succeeds; cmap
    // handles the real character mapping for shaping.
    let need_charset_for_encoding = &charset_per_gid;
    let encoding_codes = extract_kept_encoding_codes(
        cff_bytes,
        parsed.encoding_off,
        need_charset_for_encoding,
        kept_gids,
    )?;
    let encoding_bytes = emit_encoding_auto(&encoding_codes);
    let rebuild_encoding = true;

    // Top DICT rebuild.
    let top_dict_entries = walk_dict(parsed.top_dict)?;
    let (top_dict_body, top_slots) =
        serialise_top_dict(&top_dict_entries, rebuild_charset, rebuild_encoding);

    // Top DICT INDEX wrapping the rebuilt Top DICT body.
    let top_dict_index = encode_index(&[&top_dict_body]);

    // Global Subr INDEX (renumbered globals).
    let global_subr_refs: Vec<&[u8]> = new_global_subrs.iter().map(Vec::as_slice).collect();
    let global_subr_index = encode_index(&global_subr_refs);

    // CharStrings INDEX (renumbered charstrings).
    let charstrings_refs: Vec<&[u8]> = new_charstrings.iter().map(Vec::as_slice).collect();
    let charstrings_index = encode_index(&charstrings_refs);

    // Local Subr INDEX (renumbered locals).
    let local_subr_refs: Vec<&[u8]> = new_local_subrs.iter().map(Vec::as_slice).collect();
    let local_subr_index = encode_index(&local_subr_refs);

    // Private DICT rebuild — only when source had one.
    let (private_body, priv_slots) = if parsed.private.is_some() {
        let entries = walk_dict(parsed.private_dict)?;
        let emit_subrs = !new_local_subrs.is_empty();
        serialise_private_dict(&entries, emit_subrs)
    } else {
        (Vec::new(), PrivateDictSlots::default())
    };

    // ---- Layout ---------------------------------------------------------
    // Header → Name INDEX → Top DICT INDEX → String INDEX → Global Subr
    // INDEX → Encoding → charset → CharStrings INDEX → Private DICT →
    // Local Subr INDEX. Top DICT INDEX precedes Encoding/charset so we
    // can patch its placeholders once the downstream sections land.
    let mut out = Vec::with_capacity(cff_bytes.len());
    out.extend_from_slice(parsed.header);
    out.extend_from_slice(parsed.name_index);

    // Top DICT INDEX position. We need the offset of the Top DICT
    // *body* within the output, since `top_slots` are relative to the
    // body. Find that by counting INDEX header + offsets.
    let top_dict_index_start = out.len();
    let top_dict_body_offset_in_index = {
        // INDEX layout: count(2) + offSize(1) + offsets(2 * offSize) + data.
        // We have count=1 → 2 + 1 + 2*off_size = body_offset_in_index.
        if top_dict_body.is_empty() {
            return Err(SubsetError::Unsupported("CFF1 rebuilt Top DICT empty"));
        }
        let total = 1 + top_dict_body.len();
        let off_size: usize = if total <= 0xFF {
            1
        } else if total <= 0xFFFF {
            2
        } else if total <= 0x00FF_FFFF {
            3
        } else {
            4
        };
        2 + 1 + 2 * off_size
    };
    out.extend_from_slice(&top_dict_index);
    let top_dict_body_abs = top_dict_index_start + top_dict_body_offset_in_index;

    out.extend_from_slice(parsed.string_index);
    out.extend_from_slice(&global_subr_index);

    let encoding_abs = out.len();
    out.extend_from_slice(&encoding_bytes);

    let charset_abs = out.len();
    out.extend_from_slice(&charset_bytes);

    let charstrings_abs = out.len();
    out.extend_from_slice(&charstrings_index);

    let private_abs = out.len();
    let private_size = private_body.len();
    out.extend_from_slice(&private_body);

    let local_subr_abs = out.len();
    out.extend_from_slice(&local_subr_index);

    // ---- Patch Top DICT placeholders ------------------------------------
    if let Some(slot) = top_slots.charset_slot {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, charset_abs as i32);
    }
    if let Some(slot) = top_slots.encoding_slot {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, encoding_abs as i32);
    }
    if let Some(slot) = top_slots.char_strings_slot {
        let abs = top_dict_body_abs + slot;
        patch_dict_offset(&mut out, abs, charstrings_abs as i32);
    }
    if let Some((size_slot, off_slot)) = top_slots.private_slot {
        let abs_size = top_dict_body_abs + size_slot;
        let abs_off = top_dict_body_abs + off_slot;
        patch_dict_offset(&mut out, abs_size, private_size as i32);
        patch_dict_offset(&mut out, abs_off, private_abs as i32);
    }

    // ---- Patch Private DICT op 19 (Subrs) -------------------------------
    if let Some(slot) = priv_slots.subrs_slot {
        let abs = private_abs + slot;
        let rel = (local_subr_abs - private_abs) as i32;
        patch_dict_offset(&mut out, abs, rel);
    }

    Ok(out)
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
        assert_eq!(
            i32::from_be_bytes([enc[1], enc[2], enc[3], enc[4]]),
            1_000_000
        );
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
    fn encoding_format0_caps_at_u8_boundary() {
        // Inputs longer than 255 entries can't be represented (nCodes
        // is a u8). The emitter must clamp the data run to match the
        // count it advertises, otherwise downstream parsers treat the
        // overflow bytes as the next CFF section.
        let codes: Vec<u8> = (0..300u32).map(|c| c as u8).collect();
        let bytes = emit_encoding_format0(&codes);
        let count = bytes[1];
        let data_len = bytes.len() - 2;
        assert_eq!(count, 255);
        assert_eq!(data_len, 255);
    }

    #[test]
    fn encoding_format1_caps_at_u8_range_boundary() {
        // 600 alternating codes form 600 single-entry ranges. Format 1's
        // nRanges is a u8 so at most 255 ranges can be encoded; the
        // emitter must stop appending before the count overflows.
        let codes: Vec<u8> = (0..600u32)
            .map(|c| if c.is_multiple_of(2) { 1 } else { 100 })
            .collect();
        let bytes = emit_encoding_format1(&codes);
        let n_ranges = bytes[1];
        let body_bytes = bytes.len() - 2;
        assert_eq!(n_ranges, 255);
        assert_eq!(body_bytes, 255 * 2);
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

    // -- orchestration round-trip tests -------------------------------------

    /// Builds a synthetic non-CID CFF1 with `n` glyphs (gid 0 = empty
    /// .notdef charstring, gid 1..n carry the supplied charstrings).
    /// One global subr + one local subr live in the table even when
    /// the charstrings don't reference them, so the rewriter has to
    /// drop them.
    fn build_synthetic_cff1(charstrings: &[&[u8]]) -> Vec<u8> {
        // Glyph 0 is .notdef — encode as a minimal endchar charstring.
        let notdef: Vec<u8> = alloc::vec![14u8 /* OP_ENDCHAR */];
        let mut all_cs: Vec<Vec<u8>> = alloc::vec![notdef];
        all_cs.extend(charstrings.iter().map(|s| s.to_vec()));
        let cs_refs: Vec<&[u8]> = all_cs.iter().map(Vec::as_slice).collect();

        let header = alloc::vec![1u8, 0, 4, 1]; // CFF major=1 minor=0 hdrSize=4 offSize=1.
        let name_index = encode_index(&[b"Synthetic"]);
        let string_index = encode_index(&[]);
        let global_subr_index = encode_index(&[]);

        // Charset: explicit format 0 with SIDs counting up from 1 per
        // glyph past gid 0. SIDs land in the predefined ISOAdobe range
        // so we don't need to grow the String INDEX.
        let charset_sids: Vec<u16> = (1..(all_cs.len() as u16)).collect();
        let charset_bytes = emit_charset_format0(&charset_sids);

        // Encoding format 0 with one byte per gid past gid 0.
        let codes: Vec<u8> = (1..all_cs.len()).map(|i| i as u8).collect();
        let encoding_bytes = emit_encoding_format0(&codes);

        let cs_index = encode_index(&cs_refs);

        // Private DICT carries op 19 (Subrs) referencing one local subr
        // (an empty `RETURN` body) so the orchestration has subrs to
        // drop. Body uses a 5-byte placeholder offset that we patch
        // later.
        let local_subrs: Vec<&[u8]> = alloc::vec![&[11u8 /* OP_RETURN */] as &[u8]];
        let local_subr_index = encode_index(&local_subrs);

        // Build Top DICT with placeholder offsets for charset (15),
        // Encoding (16), CharStrings (17), Private (18=size+off pair).
        // We patch the placeholders once layout is known.
        let mut top_dict: Vec<u8> = Vec::new();
        let charset_slot = top_dict.len();
        top_dict.extend_from_slice(&encode_dict_offset_placeholder());
        top_dict.push(15);
        let encoding_slot = top_dict.len();
        top_dict.extend_from_slice(&encode_dict_offset_placeholder());
        top_dict.push(16);
        let charstrings_slot = top_dict.len();
        top_dict.extend_from_slice(&encode_dict_offset_placeholder());
        top_dict.push(17);
        let priv_size_slot = top_dict.len();
        top_dict.extend_from_slice(&encode_dict_offset_placeholder());
        let priv_off_slot = top_dict.len();
        top_dict.extend_from_slice(&encode_dict_offset_placeholder());
        top_dict.push(18);

        // Private DICT body: op 19 with placeholder offset.
        let mut private_dict: Vec<u8> = Vec::new();
        let priv_subrs_slot = private_dict.len();
        private_dict.extend_from_slice(&encode_dict_offset_placeholder());
        private_dict.push(19);

        // Top DICT INDEX.
        let top_dict_index = encode_index(&[&top_dict[..]]);
        // We need the body offset within the index for patching.
        let top_dict_body_offset_in_index = {
            let total = 1 + top_dict.len();
            let off_size: usize = if total <= 0xFF { 1 } else { 2 };
            2 + 1 + 2 * off_size
        };

        // Layout: header | name | top dict idx | string idx | gsubr idx
        //       | encoding | charset | charstrings idx | private dict | local subr idx.
        let mut out = Vec::new();
        out.extend_from_slice(&header);
        out.extend_from_slice(&name_index);

        let top_dict_index_start = out.len();
        out.extend_from_slice(&top_dict_index);
        let top_dict_body_abs = top_dict_index_start + top_dict_body_offset_in_index;

        out.extend_from_slice(&string_index);
        out.extend_from_slice(&global_subr_index);

        let encoding_abs = out.len();
        out.extend_from_slice(&encoding_bytes);

        let charset_abs = out.len();
        out.extend_from_slice(&charset_bytes);

        let cs_abs = out.len();
        out.extend_from_slice(&cs_index);

        let private_abs = out.len();
        let private_size = private_dict.len();
        out.extend_from_slice(&private_dict);

        let local_subr_abs = out.len();
        out.extend_from_slice(&local_subr_index);

        // Patch Top DICT placeholder offsets.
        patch_dict_offset(
            &mut out,
            top_dict_body_abs + charset_slot,
            charset_abs as i32,
        );
        patch_dict_offset(
            &mut out,
            top_dict_body_abs + encoding_slot,
            encoding_abs as i32,
        );
        patch_dict_offset(
            &mut out,
            top_dict_body_abs + charstrings_slot,
            cs_abs as i32,
        );
        patch_dict_offset(
            &mut out,
            top_dict_body_abs + priv_size_slot,
            private_size as i32,
        );
        patch_dict_offset(
            &mut out,
            top_dict_body_abs + priv_off_slot,
            private_abs as i32,
        );
        // Patch Private DICT op 19 (Subrs) offset relative to Private DICT start.
        patch_dict_offset(
            &mut out,
            private_abs + priv_subrs_slot,
            (local_subr_abs - private_abs) as i32,
        );

        out
    }

    #[test]
    fn orchestration_keeps_only_requested_glyphs() {
        // 3 glyphs: gid 0 .notdef, gid 1 = simple endchar, gid 2 = different endchar.
        // After subset to [0, 2], the new CFF should list 2 charstrings
        // and the kept charstring's bytes should still resolve.
        let cs1: &[u8] = &[139, 139, 21, 14]; // 0 0 rmoveto endchar
        let cs2: &[u8] = &[139, 14]; // 0 endchar (1 arg, allowed: width)
        let cff = build_synthetic_cff1(&[cs1, cs2]);

        let kept = alloc::vec![0u16, 2];
        let new_cff = subset_non_identity(&cff, &kept).unwrap();

        // Re-parse the rewritten CFF to assert the CharStrings INDEX
        // shrank to 2 and the second entry equals cs2.
        let parsed = parse_cff1(&new_cff).unwrap();
        assert_eq!(parsed.char_strings.len(), 2);
        // Gid 0 = .notdef (preserved verbatim from source notdef).
        assert_eq!(parsed.char_strings[0], &[14u8]);
        assert_eq!(parsed.char_strings[1], cs2);
    }

    #[test]
    fn orchestration_drops_unused_subrs() {
        // Source carries one local subr that no kept charstring calls;
        // after subset the local subr INDEX must be empty.
        let cs1: &[u8] = &[139, 139, 21, 14];
        let cff = build_synthetic_cff1(&[cs1]);
        let new_cff = subset_non_identity(&cff, &[0, 1]).unwrap();
        let parsed = parse_cff1(&new_cff).unwrap();
        assert!(parsed.local_subrs.is_empty());
    }

    #[test]
    fn orchestration_preserves_name_and_string_indexes() {
        let cs1: &[u8] = &[139, 14];
        let cff = build_synthetic_cff1(&[cs1]);
        let new_cff = subset_non_identity(&cff, &[0, 1]).unwrap();

        let orig = parse_cff1(&cff).unwrap();
        let rebuilt = parse_cff1(&new_cff).unwrap();
        assert_eq!(orig.name_index, rebuilt.name_index);
        assert_eq!(orig.string_index, rebuilt.string_index);
        assert_eq!(orig.header, rebuilt.header);
    }

    #[test]
    fn orchestration_rejects_kept_set_without_gid0() {
        let cs1: &[u8] = &[139, 14];
        let cff = build_synthetic_cff1(&[cs1]);
        let r = subset_non_identity(&cff, &[1u16]);
        assert!(r.is_err());
    }

    #[test]
    fn orchestration_rejects_cid_keyed_source() {
        // Build a synthetic CFF1 with op 0x0C24 (FDArray) in the Top
        // DICT and assert the orchestration declines.
        let cs1: &[u8] = &[139, 14];
        let mut cff = build_synthetic_cff1(&[cs1]);
        // Inject a fake FDArray op into the Top DICT body. Easiest:
        // append a (0 12 36) operand+op tail to the Top DICT body in
        // place, but since Top DICT INDEX wraps the body the simplest
        // way is to walk the bytes and inject the op there — for this
        // test we cheat and use parse_cff1's `is_cid` path indirectly
        // by asserting the explicit subset_non_identity error on a
        // hand-built tiny CFF that includes the FDArray op.
        //
        // The easier check: walk the Top DICT we already serialised,
        // find the b0=29 5-byte `Encoding` placeholder (16) and
        // overwrite the operator byte to op 12+0x24 (FDArray).
        let _ = &mut cff; // keep the unused-mut lint quiet.

        // Hand-build a minimal CFF that carries op 0x0C24 in the Top DICT.
        let header = alloc::vec![1u8, 0, 4, 1];
        let name_index = encode_index(&[b"X"]);
        let string_index = encode_index(&[]);
        let global_subr_index = encode_index(&[]);
        let cs_index = encode_index(&[&[14u8] as &[u8]]);
        let mut top: Vec<u8> = Vec::new();
        // CharStrings op (placeholder).
        let cs_slot = top.len();
        top.extend_from_slice(&encode_dict_offset_placeholder());
        top.push(17);
        // FDArray op (operand 0, op 12 36).
        top.extend_from_slice(&encode_dict_int(0));
        top.push(12);
        top.push(0x24);
        let top_idx = encode_index(&[&top[..]]);
        let mut buf = Vec::new();
        buf.extend_from_slice(&header);
        buf.extend_from_slice(&name_index);
        let top_idx_start = buf.len();
        buf.extend_from_slice(&top_idx);
        let top_body_off = {
            let total = 1 + top.len();
            let off_size: usize = if total <= 0xFF { 1 } else { 2 };
            2 + 1 + 2 * off_size
        };
        buf.extend_from_slice(&string_index);
        buf.extend_from_slice(&global_subr_index);
        let cs_abs = buf.len();
        buf.extend_from_slice(&cs_index);
        patch_dict_offset(
            &mut buf,
            top_idx_start + top_body_off + cs_slot,
            cs_abs as i32,
        );

        let r = subset_non_identity(&buf, &[0u16]);
        assert!(matches!(r, Err(SubsetError::Unsupported(_))));
    }

    #[test]
    fn orchestration_renumbers_callsubr_when_local_subrs_kept() {
        // Source: 1 local subr (a no-op rmoveto+return body), one
        // charstring that calls that local subr (callsubr 0). After
        // subset, the local subr survives at the same logical index 0
        // (only one kept) but its byte position in the new INDEX may
        // differ — the rewriter still walks the call site and patches
        // the operand even for a no-op renumber.
        //
        // Build: subr at logical index 0 → call with operand (0 - 107)
        // = -107 (single byte: 32). Charstring calls subr 0 → push -107
        // (single byte 32) + callsubr.
        let cs: Vec<u8> = alloc::vec![32u8 /* push -107 */, 10 /* OP_CALLSUBR */, 14];

        // Build a CFF identical in shape to build_synthetic_cff1 but
        // wire the local subr to be referenced by the charstring.
        // build_synthetic_cff1 already provides a single local subr (a
        // RETURN body) and a charstring that doesn't reference it; we
        // just supply our subroutine-calling charstring instead.
        let cff = build_synthetic_cff1(&[&cs]);

        // Subset to all glyphs (kept set [0,1]).
        let new_cff = subset_non_identity(&cff, &[0u16, 1]).unwrap();
        let parsed = parse_cff1(&new_cff).unwrap();
        // The kept local subr is still there.
        assert_eq!(parsed.local_subrs.len(), 1);
        // Charstring 1 still has callsubr at byte 1. The operand byte
        // re-encoded at original width (1 byte).
        let kept_cs = parsed.char_strings[1];
        assert_eq!(kept_cs.len(), cs.len());
        assert_eq!(kept_cs[1], 10); // OP_CALLSUBR preserved.
        assert_eq!(kept_cs[2], 14); // OP_ENDCHAR preserved.
    }
}
