//! CFF1 subsetting helpers — Type 2 charstring scanner, subroutine
//! bias arithmetic, and transitive subroutine keep-set computation.
//!
//! # Status
//!
//! This module implements the *analysis* half of CFF1 subsetting:
//!
//! - [`subr_bias`] — the canonical 107 / 1131 / 32768 bias table from
//!   the Type 2 spec, indexed by subroutine count.
//! - [`scan_subr_calls`] — walks a Type 2 charstring's bytes and yields
//!   the (kind, biased index) pair for every `callsubr` / `callgsubr`
//!   it executes against the bias-adjusted index that would resolve
//!   into the subroutine INDEX. The walker handles single-byte ints
//!   (32..=246), two-byte positive (247..=250) / negative (251..=254)
//!   ints, the `shortint` two-byte (op 28) and `fixed16.16` five-byte
//!   (op 255) push forms, and the variable-length tail bytes that
//!   `hintmask` / `cntrmask` pull from the stream.
//! - [`compute_kept_subrs`] — fixed-point closure: given the kept gid
//!   set and the parsed CFF tables, returns the set of local + global
//!   subroutine indices the kept charstrings transitively call.
//!
//! The byte-level CFF rewriter (CharStrings INDEX rebuild, Subr INDEX
//! renumber, Top DICT deferred-offset emission, charset / encoding
//! format selection) is the consumer of these helpers and lives in a
//! follow-up commit; today the [`subset`] entry point still returns
//! `SubsetError::Unsupported` so the upstream `subset()` API is honest
//! about the integration gap.

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
}
