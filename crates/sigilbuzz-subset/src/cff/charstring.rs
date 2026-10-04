//! Type 2 charstring analysis and rewriting: subroutine bias, call-site
//! scanning, the transitive subroutine keep-set, and call renumbering.

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
    /// `callsubr`: local subroutine reference.
    Local,
    /// `callgsubr`: global subroutine reference.
    Global,
}

// Type 2 opcode numbers we need to recognize. Values from Adobe TN
// 5177 §3 and §4. Shared with the subsetter's charstring walk.
pub(super) const OP_HSTEM: u8 = 1;
pub(super) const OP_VSTEM: u8 = 3;
pub(super) const OP_VMOVETO: u8 = 4;
pub(super) const OP_RLINETO: u8 = 5;
pub(super) const OP_HLINETO: u8 = 6;
pub(super) const OP_VLINETO: u8 = 7;
pub(super) const OP_RRCURVETO: u8 = 8;
pub(super) const OP_CALLSUBR: u8 = 10;
pub(super) const OP_RETURN: u8 = 11;
pub(super) const OP_ESCAPE: u8 = 12;
pub(super) const OP_ENDCHAR: u8 = 14;
pub(super) const OP_VSINDEX: u8 = 15;
pub(super) const OP_BLEND: u8 = 16;
pub(super) const OP_HSTEMHM: u8 = 18;
pub(super) const OP_HINTMASK: u8 = 19;
pub(super) const OP_CNTRMASK: u8 = 20;
pub(super) const OP_RMOVETO: u8 = 21;
pub(super) const OP_HMOVETO: u8 = 22;
pub(super) const OP_VSTEMHM: u8 = 23;
pub(super) const OP_RCURVELINE: u8 = 24;
pub(super) const OP_RLINECURVE: u8 = 25;
pub(super) const OP_VVCURVETO: u8 = 26;
pub(super) const OP_HHCURVETO: u8 = 27;
pub(super) const OP_SHORTINT: u8 = 28;
pub(super) const OP_CALLGSUBR: u8 = 29;
pub(super) const OP_VHCURVETO: u8 = 30;
pub(super) const OP_HVCURVETO: u8 = 31;

/// One subroutine call discovered by [`scan_subr_calls`].
///
/// `index_after_bias` is what the call site decoded: the value
/// already has the bias added. Callers comparing against the
/// `(global|local)_subrs` INDEX use it as the array index directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubrCall {
    /// Local or global.
    pub kind: SubrKind,
    /// Already bias-adjusted (i.e. usable as a direct INDEX lookup).
    pub index_after_bias: i64,
    /// Raw (still-biased) operand value as seen on the stack: what
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
/// scanner does *not* descend into called subroutines. Callers walk
/// the charstring bytes once per subroutine and iterate to a fixed
/// point externally.
///
/// CFF2 charstrings have no `endchar`; the scanner stops at the end
/// of the byte stream. CFF1 charstrings stop at `endchar` or
/// end-of-stream. Both behaviors produce the same call list.
///
/// The scan reads one body on its own. The stem count that sizes
/// `hintmask` / `cntrmask` data starts at 0 for every body, so a
/// subroutine whose masks count stems its caller declared is misread,
/// and the operands a CFF2 `blend` drops depend on a variation store
/// the scan does not see, so the stack is cleared there. The subsetter
/// itself does not scan bodies alone: it runs each kept glyph through
/// its subroutine calls (see `cff::walk`), which sizes masks and blends
/// as HarfBuzz does.
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] on a truncated operand push,
/// an out-of-range opcode, or a `hintmask`/`cntrmask` whose tail
/// bytes run past the end. The byte-level subsetter treats these as
/// fatal. A malformed charstring shouldn't survive subsetting.
pub fn scan_subr_calls(
    charstring: &[u8],
    local_count: usize,
    global_count: usize,
) -> Result<Vec<SubrCall>, SubsetError> {
    let mut out = Vec::new();
    let mut pos = 0;
    // Operand stack of (raw_value, push_offset, push_len). Only the
    // top entry is consumed by callsubr/callgsubr, but we track all
    // of them because hintmask/cntrmask use the running stem count
    // (which equals operands_pushed_since_last_clear / 2 for any
    // implicit vstem). The scanner does not need stem counting per
    // se; it needs to know how many operands sit on the stack so it
    // can skip the right number of hintmask tail bytes.
    let mut stack: Vec<(i32, usize, usize)> = Vec::new();
    // Cumulative stem-pair count, for hintmask/cntrmask tail size.
    let mut stem_count: usize = 0;

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
                stem_count = stem_count.saturating_add(stack.len() / 2);
                stack.clear();
                pos += 1;
            }
            OP_HINTMASK | OP_CNTRMASK => {
                // An implicit vstem may precede the first mask if
                // there are operands left over.
                stem_count = stem_count.saturating_add(stack.len() / 2);
                stack.clear();
                let mask_bytes = stem_count.div_ceil(8);
                // `pos < len` here, so `pos + 1` cannot overflow.
                let next = (pos + 1)
                    .checked_add(mask_bytes)
                    .filter(|&next| next <= charstring.len())
                    .ok_or(SubsetError::Unsupported("CFF hintmask tail truncated"))?;
                pos = next;
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
            // Subroutine return / endchar: stop walking *this*
            // charstring body. The scanner's caller invokes us per
            // body; treating either op as end-of-walk is correct.
            OP_RETURN | OP_ENDCHAR => {
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
/// (fixed). Those are handled inline by the scanner because their
/// b0 < 32 collides with operator space.
pub(super) fn decode_operand(data: &[u8], pos: usize) -> Option<(i32, usize)> {
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
        // 16.16 fixed: return integer part. CFF subr indices are
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
/// renumbers in place, so this helper returns the natural minimal
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
/// Each body is read on its own, with the limits [`scan_subr_calls`]
/// describes. The subsetter runs the kept glyphs through their calls
/// instead, which sizes hint masks and CFF2 blends as HarfBuzz does.
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
    // Subroutines marked kept whose bodies still need a scan. Each
    // subroutine enters the list at most once, so the walk costs one
    // scan per kept body even when the call graph is a long chain.
    let mut pending: Vec<(SubrKind, usize)> = Vec::new();

    // Seed: every call site in every kept charstring.
    for cs in kept_charstrings {
        for call in scan_subr_calls(cs, local_subrs.len(), global_subrs.len())? {
            mark_call(call, &mut keep_local, &mut keep_global, &mut pending);
        }
    }

    // Transitive closure: a kept subroutine may call further ones.
    while let Some((kind, idx)) = pending.pop() {
        let body = match kind {
            SubrKind::Local => local_subrs.get(idx),
            SubrKind::Global => global_subrs.get(idx),
        };
        let Some(body) = body else {
            continue;
        };
        for call in scan_subr_calls(body, local_subrs.len(), global_subrs.len())? {
            mark_call(call, &mut keep_local, &mut keep_global, &mut pending);
        }
    }

    Ok((collect_kept(&keep_local), collect_kept(&keep_global)))
}

/// Marks the subroutine `call` targets as kept and queues it for a
/// scan when it was not kept before. Out-of-range targets are ignored.
fn mark_call(
    call: SubrCall,
    keep_local: &mut [bool],
    keep_global: &mut [bool],
    pending: &mut Vec<(SubrKind, usize)>,
) {
    let target = match call.kind {
        SubrKind::Local => keep_local,
        SubrKind::Global => keep_global,
    };
    let Ok(idx) = usize::try_from(call.index_after_bias) else {
        return;
    };
    if let Some(slot) = target.get_mut(idx) {
        if !*slot {
            *slot = true;
            pending.push((call.kind, idx));
        }
    }
}

/// Identifies the global subroutines whose body (directly or
/// transitively through other globals) calls a local subroutine, from
/// the calls of each global body given: `(global index, calls)` pairs
/// for `n` globals. A global without a pair calls nothing.
///
/// In CID-keyed CFF1 fonts each gid belongs to a Font DICT (FD) with
/// its own local subroutine INDEX. A `callsubr` from inside a *global*
/// subroutine resolves at runtime against the locals of the FD that
/// originated the call chain. When a global subr calls a local (the
/// "cross-FD" case), the same global body cannot point at a single
/// concrete local-index after subsetting because different FDs renumber
/// their locals independently.
///
/// Returns a boolean mask of `n` entries: index `i` is `true` when
/// global `i` reaches a local call (and therefore must be duplicated
/// per kept FD by the rewriter). Globals that only call other
/// non-cross-FD globals are *not* marked.
pub(super) fn cross_fd_globals<'c>(
    n: usize,
    bodies: impl IntoIterator<Item = (usize, &'c [SubrCall])>,
) -> Vec<bool> {
    let mut is_cross: Vec<bool> = alloc::vec![false; n];
    // `callers[g]` lists every global whose body calls global `g`.
    let mut callers: Vec<Vec<usize>> = alloc::vec![Vec::new(); n];
    let mut pending: Vec<usize> = Vec::new();
    // Pass 1: mark every global whose body directly calls a local, and
    // record the global-to-global call edges.
    for (i, calls) in bodies {
        for call in calls {
            match call.kind {
                SubrKind::Local => {
                    if let Some(flag) = is_cross.get_mut(i) {
                        if !*flag {
                            *flag = true;
                            pending.push(i);
                        }
                    }
                }
                SubrKind::Global => {
                    let callee = usize::try_from(call.index_after_bias)
                        .ok()
                        .and_then(|idx| callers.get_mut(idx));
                    if let Some(list) = callee {
                        list.push(i);
                    }
                }
            }
        }
    }
    // Pass 2: propagate transitively. If global G calls global G' and
    // G' is cross-FD, then G is cross-FD too (G's emitted body would
    // need to point at *one* duplicate of G' for *one* FD, which is
    // exactly the cross-FD condition). Walking the reversed call edges
    // visits each global at most once.
    while let Some(callee) = pending.pop() {
        let Some(list) = callers.get(callee) else {
            continue;
        };
        for &caller in list {
            if let Some(flag) = is_cross.get_mut(caller) {
                if !*flag {
                    *flag = true;
                    pending.push(caller);
                }
            }
        }
    }
    is_cross
}

fn collect_kept(keep: &[bool]) -> Vec<u32> {
    keep.iter()
        .enumerate()
        .filter_map(|(i, &k)| if k { Some(i as u32) } else { None })
        .collect()
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
    let span = call
        .operand_byte_offset
        .checked_add(call.operand_byte_len)
        .and_then(|span_end| charstring.get_mut(call.operand_byte_offset..span_end))
        .ok_or(SubsetError::Unsupported(
            "CFF charstring renumber span past end",
        ))?;
    // Re-encode at original width (pad to wider form when the natural
    // encoding is shorter). The encoding always has `operand_byte_len`
    // bytes, so it fills the span exactly.
    let encoded = encode_int_operand_at_width(new_raw_operand, call.operand_byte_len)?;
    if encoded.len() != span.len() {
        return Err(SubsetError::Unsupported(
            "CFF renumber: cannot pad operand to original width",
        ));
    }
    span.copy_from_slice(&encoded);
    Ok(())
}

/// Encodes a charstring integer operand at exactly `target_len` bytes,
/// upgrading to a wider form when the natural minimum is shorter.
///
/// `target_len` 1: only values in `-107..=107`.
/// `target_len` 2: only values in `-1131..=-108 ∪ 108..=1131` (the two
/// 2-byte natural forms; there is no Type 2 mechanism to express a
/// `-107..=107` value in 2 bytes).
/// `target_len` 3: any `i16` (re-encoded as shortint op 28).
/// `target_len` 5: any `i32` (re-encoded as fixed op 255 with a zero
/// fractional part).
///
/// Returns [`SubsetError::Unsupported`] when the supplied value cannot
/// be expressed at `target_len` bytes (the rewriter then falls back to
/// keeping the offending subroutine verbatim, see
/// [`subset_non_identity`](super::subset_non_identity)).
pub(crate) fn encode_int_operand_at_width(
    v: i32,
    target_len: usize,
) -> Result<Vec<u8>, SubsetError> {
    // Op 255 (5-byte fixed) is a 16.16 number: i16 integer + u16
    // fractional. Any `v` outside `i16` range cannot be represented:
    // `encode_int_operand` itself silently wraps via `(v as i64) << 16
    // as i32` (#187), which would emit a corrupt subroutine index.
    // Refuse early so the rewriter falls back to keeping the offending
    // subroutine verbatim instead of producing a silently broken font.
    if !(i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(&v) {
        return Err(SubsetError::Unsupported(
            "CFF renumber: operand outside i16 has no Type 2 representation",
        ));
    }
    let natural = encode_int_operand(v);
    if natural.len() == target_len {
        return Ok(natural);
    }
    // Two-byte natural forms only span 108..=1131 and -1131..=-108.
    // No way to express -107..=107 in 2 bytes.
    if target_len == 2 {
        if (108..=1131).contains(&v) {
            let v0 = v - 108;
            let b0 = ((v0 >> 8) + 247) as u8;
            let b1 = (v0 & 0xff) as u8;
            return Ok(alloc::vec![b0, b1]);
        }
        if (-1131..=-108).contains(&v) {
            let v0 = -v - 108;
            let b0 = ((v0 >> 8) + 251) as u8;
            let b1 = (v0 & 0xff) as u8;
            return Ok(alloc::vec![b0, b1]);
        }
        return Err(SubsetError::Unsupported(
            "CFF renumber: cannot pad operand to original width",
        ));
    }
    // Shortint (op 28) is always 3 bytes for any i16; fixed (op 255)
    // is always 5 bytes.
    if target_len == 3 && (i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(&v) {
        let bytes = (v as i16).to_be_bytes();
        return Ok(alloc::vec![28u8, bytes[0], bytes[1]]);
    }
    if target_len == 5 {
        // v is guaranteed to fit i16 by the early-return guard above,
        // so the 16.16 fixed encode (i16 integer << 16, zero
        // fractional) cannot overflow.
        let raw = (i32::from(v as i16)) << 16;
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
/// the supplied old->new maps. `local_renumber[i]` is the new compacted
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
    renumber_charstring_with_cross_fd(
        charstring,
        old_local_count,
        old_global_count,
        new_local_count,
        new_global_count,
        local_renumber,
        global_renumber,
        &[],
    )
}

/// Like [`renumber_charstring`] but accepts a per-FD override map for
/// cross-FD globals. `cross_fd_override[i]`, when `Some(new_idx)`,
/// rewrites a `callgsubr` to old global `i` to point at the FD-specific
/// duplicate at `new_idx` in the rebuilt global INDEX, instead of the
/// `global_renumber` entry. The override table must be indexed in the
/// source-font global numbering (length == old_global_count) and is
/// allowed to be empty for the non-cross-FD case.
///
/// # Errors
///
/// Returns [`SubsetError::Unsupported`] under the same conditions as
/// [`renumber_charstring`].
#[allow(clippy::too_many_arguments)]
pub fn renumber_charstring_with_cross_fd(
    charstring: &mut [u8],
    old_local_count: usize,
    old_global_count: usize,
    new_local_count: usize,
    new_global_count: usize,
    local_renumber: &[Option<u32>],
    global_renumber: &[Option<u32>],
    cross_fd_override: &[Option<u32>],
) -> Result<(), SubsetError> {
    renumber_charstring_impl(
        charstring,
        old_local_count,
        old_global_count,
        new_local_count,
        new_global_count,
        local_renumber,
        global_renumber,
        |old_idx| cross_fd_override.get(old_idx).copied().flatten(),
    )
}

/// Shared body of [`renumber_charstring_with_cross_fd`]. The cross-FD
/// override is a lookup function so callers can keep a sparse table.
// Mirrors the argument list of the public wrapper above.
#[allow(clippy::too_many_arguments)]
pub(super) fn renumber_charstring_impl(
    charstring: &mut [u8],
    old_local_count: usize,
    old_global_count: usize,
    new_local_count: usize,
    new_global_count: usize,
    local_renumber: &[Option<u32>],
    global_renumber: &[Option<u32>],
    cross_fd_override: impl Fn(usize) -> Option<u32>,
) -> Result<(), SubsetError> {
    let calls = scan_subr_calls(charstring, old_local_count, old_global_count)?;
    for call in calls.iter().rev() {
        // Reverse iteration so earlier rewrites don't shift later
        // offsets, but since we always re-encode at the original byte
        // width, the offsets stay stable. Reverse-iterate anyway as a
        // belt-and-suspenders against future variable-width changes.
        let new_raw = new_raw_operand(
            call,
            new_local_count,
            new_global_count,
            local_renumber,
            global_renumber,
            &cross_fd_override,
        )?;
        renumber_subr_call(charstring, call, new_raw)?;
    }
    Ok(())
}

/// The raw operand `call` takes once its subroutine is renumbered: the
/// new index, through `local_renumber`, `cross_fd_override` or
/// `global_renumber`, less the bias of the new INDEX.
fn new_raw_operand(
    call: &SubrCall,
    new_local_count: usize,
    new_global_count: usize,
    local_renumber: &[Option<u32>],
    global_renumber: &[Option<u32>],
    cross_fd_override: &impl Fn(usize) -> Option<u32>,
) -> Result<i32, SubsetError> {
    if call.index_after_bias < 0 {
        return Err(SubsetError::Unsupported(
            "CFF charstring negative subr index after bias",
        ));
    }
    let old_idx = usize::try_from(call.index_after_bias)
        .map_err(|_| SubsetError::Unsupported("CFF charstring calls dropped subroutine"))?;
    let dropped = SubsetError::Unsupported("CFF charstring calls dropped subroutine");
    let (new_idx, new_bias) = match call.kind {
        SubrKind::Local => (
            local_renumber
                .get(old_idx)
                .copied()
                .flatten()
                .ok_or(dropped)?,
            subr_bias(new_local_count),
        ),
        SubrKind::Global => (
            match cross_fd_override(old_idx) {
                Some(target) => target,
                None => global_renumber
                    .get(old_idx)
                    .copied()
                    .flatten()
                    .ok_or(dropped)?,
            },
            subr_bias(new_global_count),
        ),
    };
    let new_raw = i64::from(new_idx) - i64::from(new_bias);
    i32::try_from(new_raw)
        .map_err(|_| SubsetError::Unsupported("CFF renumber: new raw operand out of i32 range"))
}

/// The body `body` with the subroutine number of each call in `calls`,
/// call sites found in it in byte order, renumbered against the
/// old-to-new maps (see [`renumber_charstring_with_cross_fd`]).
///
/// A new number that fits the width of the push it replaces is padded
/// to that width, so the body keeps its size. One that cannot be, a
/// value in `-107..=107` replacing a 2-byte push or any value wider
/// than its push, is written in its own shortest form and the bytes
/// after it move: nothing in a charstring addresses its own bytes, and
/// HarfBuzz re-encodes every call this way.
pub(crate) fn rewrite_calls(
    body: &[u8],
    calls: &[SubrCall],
    new_local_count: usize,
    new_global_count: usize,
    local_renumber: &[Option<u32>],
    global_renumber: &[Option<u32>],
    cross_fd_override: impl Fn(usize) -> Option<u32>,
) -> Result<Vec<u8>, SubsetError> {
    const BAD_SITE: SubsetError =
        SubsetError::Unsupported("CFF charstring call sites out of order");
    let mut out = Vec::with_capacity(body.len());
    let mut pos = 0;
    for call in calls {
        let start = call.operand_byte_offset;
        let end = start.checked_add(call.operand_byte_len).ok_or(BAD_SITE)?;
        out.extend_from_slice(body.get(pos..start).ok_or(BAD_SITE)?);
        if end > body.len() {
            return Err(BAD_SITE);
        }
        let new_raw = new_raw_operand(
            call,
            new_local_count,
            new_global_count,
            local_renumber,
            global_renumber,
            &cross_fd_override,
        )?;
        match encode_int_operand_at_width(new_raw, call.operand_byte_len) {
            Ok(padded) if padded.len() == call.operand_byte_len => out.extend_from_slice(&padded),
            _ => out.extend_from_slice(&encode_int_operand(new_raw)),
        }
        pos = end;
    }
    out.extend_from_slice(body.get(pos..).ok_or(BAD_SITE)?);
    Ok(out)
}
