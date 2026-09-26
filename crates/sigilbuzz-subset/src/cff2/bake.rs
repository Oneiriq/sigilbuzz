//! Full instancing bake: resolves every `blend` at fixed coordinates
//! and inlines subroutines, producing a static CFF2.

use alloc::vec::Vec;

use sigilbuzz::tables::variation_store::ItemVariationStore;

use super::{
    decode_operand_f32, encode_charstring_number, parse_cff2, serialise_cff2_top_dict, subr_bias,
    MAX_BAKE_DEPTH, OP_BLEND, OP_CALLGSUBR, OP_CALLSUBR, OP_CNTRMASK, OP_ESCAPE, OP_HHCURVETO,
    OP_HINTMASK, OP_HLINETO, OP_HMOVETO, OP_HSTEM, OP_HSTEMHM, OP_HVCURVETO, OP_RCURVELINE,
    OP_RETURN, OP_RLINECURVE, OP_RLINETO, OP_RMOVETO, OP_RRCURVETO, OP_SHORTINT, OP_VHCURVETO,
    OP_VLINETO, OP_VMOVETO, OP_VSINDEX, OP_VSTEM, OP_VSTEMHM, OP_VVCURVETO,
};
use crate::cff::{
    emit_fd_select_auto, encode_index_cff2, patch_dict_offset, serialise_font_dict,
    serialise_private_dict, walk_dict, DictEntry, OP_VSTORE,
};
use crate::SubsetError;

// ----------------------------------------------------------------------------
// Blend baking (instancing).
// ----------------------------------------------------------------------------

/// Bakes the `blend` operator out of every CharString in `cff_bytes` at
/// `coords`, producing a new CFF2 table that no longer references its
/// VariationStore. The result is a static CFF2 (no `blend`, no
/// `vsindex`) carrying the same outlines the source would draw at
/// `coords`.
///
/// Implementation strategy:
///
/// 1. **Inline-expand subroutines** into each charstring. CFF2 `blend`
///    can occur in a subr while masters were pushed by the caller:
///    a stack-tracking pass that doesn't inline would have to model
///    cross-subr stack flows, which is more bookkeeping than just
///    pasting the body. Inlining also lets us drop the local + global
///    Subr INDEX entirely.
/// 2. **Resolve `blend`**: pop `n`, then `n*nRegions` deltas, then `n`
///    masters; emit only the `n` resolved scalars
///    (`master + Σ scalar(coords) * delta`). The trailing count and the
///    delta operands are dropped.
/// 3. **Strip `vsindex`**: tracks which IVS subtable subsequent
///    `blend`s read from. Dropped from the output (no blend remains).
/// 4. Re-encode all push operands. After blend resolution masters can
///    become non-integer floats; we emit them via the `b0=255` 16.16
///    fixed form when fractional, integer forms otherwise.
///
/// The output drops VariationStore, GlobalSubr INDEX entries, and per-FD
/// LocalSubr INDEX entries. Top DICT keeps CharStrings / FDArray /
/// FDSelect; the VariationStore operator is omitted.
pub fn bake_at_coords(cff_bytes: &[u8], coords: &[f32]) -> Result<Vec<u8>, SubsetError> {
    let parsed = parse_cff2(cff_bytes)?;
    let n_glyphs = parsed.char_strings.len();
    if n_glyphs == 0 {
        return Err(SubsetError::Unsupported("CFF2 source has zero glyphs"));
    }

    // Parse the VariationStore (when present) so we can resolve blends.
    let ivs = if let Some(blob) = parsed.vstore_blob {
        // blob includes the 2-byte length prefix; the underlying
        // ItemVariationStore parser consumes the body without the
        // prefix.
        if blob.len() < 2 {
            return Err(SubsetError::Unsupported(
                "CFF2 VariationStore blob too short",
            ));
        }
        Some(
            ItemVariationStore::parse(&blob[2..])
                .map_err(|_| SubsetError::Unsupported("CFF2 VariationStore parse failed"))?,
        )
    } else {
        None
    };

    // Per-FD: bake every charstring with subr inlining and blend
    // resolution.
    let mut new_charstrings: Vec<Vec<u8>> = Vec::with_capacity(n_glyphs);
    for (gid, cs) in parsed.char_strings.iter().enumerate() {
        let fd = parsed
            .fd_select
            .get(gid)
            .copied()
            .ok_or(SubsetError::Unsupported("CFF2 FDSelect gid past end"))?;
        let local_subrs = parsed
            .per_fd_local_subrs
            .get(fd as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut baker = Baker::new(coords, ivs.as_ref(), &parsed.global_subrs, local_subrs);
        let baked = baker.bake_charstring(cs)?;
        new_charstrings.push(baked);
    }

    // Per-FD Font DICT bodies. We re-emit each Font DICT pointing at
    // its source Private DICT body (with Subrs op stripped: there are
    // no subrs anymore).
    struct FdBakeEmit {
        font_dict_body: Vec<u8>,
        font_dict_private_slot: Option<(usize, usize)>,
        new_private_body: Vec<u8>,
    }
    let mut fd_emits: Vec<FdBakeEmit> = Vec::with_capacity(parsed.fd_array.len());
    for fd_bytes in &parsed.fd_array {
        let fd_entries = walk_dict(fd_bytes)?;
        let (font_dict_body, font_dict_private_slot) = serialise_font_dict(&fd_entries);
        let priv_entries = if parsed.per_fd_private[fd_emits.len()].is_empty() {
            Vec::new()
        } else {
            walk_dict(parsed.per_fd_private[fd_emits.len()])?
        };
        // No local subrs survive the bake.
        let (new_private_body, _) = serialise_private_dict(&priv_entries, false);
        fd_emits.push(FdBakeEmit {
            font_dict_body,
            font_dict_private_slot,
            new_private_body,
        });
    }

    // Empty global subrs after baking.
    let global_subr_index = encode_index_cff2(&[]);

    // FDArray INDEX.
    let fd_array_refs: Vec<&[u8]> = fd_emits
        .iter()
        .map(|f| f.font_dict_body.as_slice())
        .collect();
    let fd_array_index = encode_index_cff2(&fd_array_refs);

    // FDSelect re-emit (carry the source mapping; gid namespace is
    // unchanged).
    let fd_select_bytes = emit_fd_select_auto(&parsed.fd_select);

    // CharStrings INDEX.
    let cs_refs: Vec<&[u8]> = new_charstrings.iter().map(Vec::as_slice).collect();
    let charstrings_index = encode_index_cff2(&cs_refs);

    // Top DICT: VariationStore op omitted from the rebuilt body.
    let top_entries: Vec<DictEntry> = walk_dict(parsed.top_dict)?
        .into_iter()
        .filter(|e| e.op != OP_VSTORE)
        .collect();
    let (top_dict_body, top_slots) = serialise_cff2_top_dict(&top_entries);

    // Layout:
    //   header -> Top DICT -> Global Subr INDEX -> FDSelect ->
    //   CharStrings INDEX -> FDArray INDEX -> [per-FD: Private DICT].
    let mut out = Vec::with_capacity(cff_bytes.len() / 2);
    out.push(2u8);
    out.push(0u8);
    out.push(parsed.hdr_size as u8);
    out.extend_from_slice(&(top_dict_body.len() as u16).to_be_bytes());
    while out.len() < parsed.hdr_size {
        out.push(0);
    }

    let top_dict_body_abs = out.len();
    out.extend_from_slice(&top_dict_body);

    out.extend_from_slice(&global_subr_index);

    let fd_select_abs = out.len();
    out.extend_from_slice(&fd_select_bytes);

    let charstrings_abs = out.len();
    out.extend_from_slice(&charstrings_index);

    let fd_array_abs = out.len();
    out.extend_from_slice(&fd_array_index);

    let fd_count = fd_emits.len();
    let fd_index_off_size: usize = {
        let total: usize = fd_emits.iter().map(|f| f.font_dict_body.len()).sum();
        let last_off = 1 + total;
        if last_off <= 0xFF {
            1
        } else if last_off <= 0xFFFF {
            2
        } else if last_off <= 0x00FF_FFFF {
            3
        } else {
            4
        }
    };
    let fd_index_data_start = 4 + 1 + (fd_count + 1) * fd_index_off_size;
    let mut fd_body_offsets_in_index: Vec<usize> = Vec::with_capacity(fd_count);
    let mut acc = fd_index_data_start;
    for f in &fd_emits {
        fd_body_offsets_in_index.push(acc);
        acc += f.font_dict_body.len();
    }

    let mut per_fd_private_abs: Vec<usize> = Vec::with_capacity(fd_count);
    let mut per_fd_private_size: Vec<usize> = Vec::with_capacity(fd_count);
    for f in &fd_emits {
        per_fd_private_abs.push(out.len());
        per_fd_private_size.push(f.new_private_body.len());
        out.extend_from_slice(&f.new_private_body);
    }

    // Patch Top DICT placeholders.
    if let Some(slot) = top_slots.char_strings_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, charstrings_abs as i32);
    }
    if let Some(slot) = top_slots.fd_array_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, fd_array_abs as i32);
    }
    if let Some(slot) = top_slots.fd_select_slot {
        patch_dict_offset(&mut out, top_dict_body_abs + slot, fd_select_abs as i32);
    }
    debug_assert!(top_slots.vstore_slot.is_none());

    // Patch Font DICT Private slots.
    for (i, f) in fd_emits.iter().enumerate() {
        if let Some((size_slot, off_slot)) = f.font_dict_private_slot {
            let body_abs_in_out = fd_array_abs + fd_body_offsets_in_index[i];
            patch_dict_offset(
                &mut out,
                body_abs_in_out + size_slot,
                per_fd_private_size[i] as i32,
            );
            patch_dict_offset(
                &mut out,
                body_abs_in_out + off_slot,
                per_fd_private_abs[i] as i32,
            );
        }
    }

    Ok(out)
}

/// CFF2 charstring baker. Walks a charstring's operand stream
/// resolving `blend` to scalars and inlining `callsubr` /
/// `callgsubr`. Emits a fresh charstring with no blend / vsindex /
/// subr-call ops.
struct Baker<'a> {
    coords: &'a [f32],
    ivs: Option<&'a ItemVariationStore<'a>>,
    global_subrs: &'a [&'a [u8]],
    local_subrs: &'a [&'a [u8]],
    /// Operand stack: floats so blend deltas don't lose precision.
    stack: Vec<f32>,
    /// Output charstring bytes.
    out: Vec<u8>,
    /// Current vsindex (which IVS subtable blend draws from).
    vsindex: u16,
    /// Running stem count, for hintmask / cntrmask tail size.
    stem_count: u32,
    /// Set after the first move/hint operator (used by stem tracking).
    seen_first_op: bool,
}

impl<'a> Baker<'a> {
    fn new(
        coords: &'a [f32],
        ivs: Option<&'a ItemVariationStore<'a>>,
        global_subrs: &'a [&'a [u8]],
        local_subrs: &'a [&'a [u8]],
    ) -> Self {
        Self {
            coords,
            ivs,
            global_subrs,
            local_subrs,
            stack: Vec::new(),
            out: Vec::new(),
            vsindex: 0,
            stem_count: 0,
            seen_first_op: false,
        }
    }

    fn bake_charstring(&mut self, cs: &[u8]) -> Result<Vec<u8>, SubsetError> {
        self.run(cs, 0)?;
        Ok(core::mem::take(&mut self.out))
    }

    fn run(&mut self, code: &[u8], depth: u8) -> Result<(), SubsetError> {
        if depth > MAX_BAKE_DEPTH {
            return Err(SubsetError::Unsupported(
                "CFF2 bake: subroutine depth exceeded",
            ));
        }
        let mut pos = 0;
        while pos < code.len() {
            let b0 = code[pos];
            if b0 >= 32 {
                let (val, len) = decode_operand_f32(code, pos)
                    .ok_or(SubsetError::Unsupported("CFF2 bake: operand truncated"))?;
                self.stack.push(val);
                pos += len;
                continue;
            }
            if b0 == OP_SHORTINT {
                if pos + 3 > code.len() {
                    return Err(SubsetError::Unsupported("CFF2 bake: shortint truncated"));
                }
                let v = i16::from_be_bytes([code[pos + 1], code[pos + 2]]);
                self.stack.push(f32::from(v));
                pos += 3;
                continue;
            }
            // Operator.
            match b0 {
                OP_BLEND => {
                    self.apply_blend()?;
                    pos += 1;
                }
                OP_VSINDEX => {
                    // Pop the new vsindex; emit nothing.
                    let v = self.stack.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 bake: vsindex without operand",
                    ))?;
                    if v < 0.0 || v > f32::from(u16::MAX) {
                        return Err(SubsetError::Unsupported(
                            "CFF2 bake: vsindex operand out of range",
                        ));
                    }
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        self.vsindex = v as u16;
                    }
                    pos += 1;
                }
                OP_CALLSUBR => {
                    let idx = self.stack.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 bake: callsubr without operand",
                    ))?;
                    #[allow(clippy::cast_possible_truncation)]
                    let raw = idx as i32;
                    let bias = subr_bias(self.local_subrs.len());
                    let abs = raw + bias;
                    if abs < 0 || (abs as usize) >= self.local_subrs.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 bake: callsubr index out of range",
                        ));
                    }
                    let body = self.local_subrs[abs as usize];
                    self.run(body, depth + 1)?;
                    pos += 1;
                }
                OP_CALLGSUBR => {
                    let idx = self.stack.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 bake: callgsubr without operand",
                    ))?;
                    #[allow(clippy::cast_possible_truncation)]
                    let raw = idx as i32;
                    let bias = subr_bias(self.global_subrs.len());
                    let abs = raw + bias;
                    if abs < 0 || (abs as usize) >= self.global_subrs.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 bake: callgsubr index out of range",
                        ));
                    }
                    let body = self.global_subrs[abs as usize];
                    self.run(body, depth + 1)?;
                    pos += 1;
                }
                OP_RETURN => {
                    // CFF2 charstrings don't terminate on return; this
                    // appears inside a subr body. Stop walking the
                    // current body.
                    self.stack.clear();
                    return Ok(());
                }
                OP_HSTEM | OP_VSTEM | OP_HSTEMHM | OP_VSTEMHM => {
                    let n_pairs = (self.stack.len() as u32) / 2;
                    self.stem_count += n_pairs;
                    self.flush_stack();
                    self.out.push(b0);
                    self.seen_first_op = true;
                    pos += 1;
                }
                OP_HINTMASK | OP_CNTRMASK => {
                    let extra_pairs = (self.stack.len() as u32) / 2;
                    self.stem_count += extra_pairs;
                    self.flush_stack();
                    self.out.push(b0);
                    self.seen_first_op = true;
                    let mask_bytes = (self.stem_count as usize).div_ceil(8);
                    if pos + 1 + mask_bytes > code.len() {
                        return Err(SubsetError::Unsupported(
                            "CFF2 bake: hintmask tail truncated",
                        ));
                    }
                    self.out
                        .extend_from_slice(&code[pos + 1..pos + 1 + mask_bytes]);
                    pos += 1 + mask_bytes;
                }
                OP_ESCAPE => {
                    if pos + 2 > code.len() {
                        return Err(SubsetError::Unsupported("CFF2 bake: escape truncated"));
                    }
                    self.flush_stack();
                    self.out.push(b0);
                    self.out.push(code[pos + 1]);
                    self.seen_first_op = true;
                    pos += 2;
                }
                OP_RMOVETO | OP_HMOVETO | OP_VMOVETO | OP_RLINETO | OP_HLINETO | OP_VLINETO
                | OP_RRCURVETO | OP_HHCURVETO | OP_VVCURVETO | OP_HVCURVETO | OP_VHCURVETO
                | OP_RCURVELINE | OP_RLINECURVE => {
                    self.flush_stack();
                    self.out.push(b0);
                    self.seen_first_op = true;
                    pos += 1;
                }
                _ => {
                    return Err(SubsetError::Unsupported(
                        "CFF2 bake: unknown charstring operator",
                    ));
                }
            }
        }
        Ok(())
    }

    fn apply_blend(&mut self) -> Result<(), SubsetError> {
        // Stack: n master values, n*nRegions delta values, n itself on top.
        let n_raw = self.stack.pop().ok_or(SubsetError::Unsupported(
            "CFF2 bake: blend without count operand",
        ))?;
        if n_raw < 0.0 {
            return Err(SubsetError::Unsupported(
                "CFF2 bake: blend count operand negative",
            ));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = n_raw as usize;
        if n == 0 {
            return Ok(());
        }
        let n_regions = self
            .ivs
            .and_then(|s| s.variation_region_count(self.vsindex))
            .map(|c| c as usize)
            .unwrap_or(0);
        let total_deltas = n * n_regions;
        if self.stack.len() < n + total_deltas {
            return Err(SubsetError::Unsupported("CFF2 bake: blend stack underflow"));
        }
        let scalars = self
            .ivs
            .and_then(|s| s.region_scalars(self.vsindex, self.coords))
            .unwrap_or_default();
        let start = self.stack.len() - n - total_deltas;
        for i in 0..n {
            let mut accum = 0.0_f32;
            for j in 0..n_regions {
                let delta = self.stack[start + n + i * n_regions + j];
                if let Some(&s) = scalars.get(j) {
                    accum += s * delta;
                }
            }
            self.stack[start + i] += accum;
        }
        self.stack.truncate(start + n);
        Ok(())
    }

    /// Emits the operands currently sitting on the operand stack as
    /// CFF2 push bytes, then clears the stack. The numbers that
    /// survive blend resolution may be fractional; we use the 16.16
    /// fixed form (`b0=255`) when needed and the integer forms
    /// otherwise.
    fn flush_stack(&mut self) {
        let stack = core::mem::take(&mut self.stack);
        for v in stack {
            encode_charstring_number(v, &mut self.out);
        }
    }
}
