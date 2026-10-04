//! Full instancing bake: resolves every `blend` at fixed coordinates
//! and inlines subroutines, producing a static CFF2.

use alloc::vec::Vec;

use sigilbuzz::tables::variation_store::ItemVariationStore;

use super::{
    bake_token_budget, biased_subr, charge_token, decode_operand_f64, encode_charstring_number_f64,
    parse_cff2, serialise_cff2_top_dict, BlendCache, MAX_BAKE_DEPTH, MAX_STACK, OP_BLEND,
    OP_CALLGSUBR, OP_CALLSUBR, OP_CNTRMASK, OP_ESCAPE, OP_HHCURVETO, OP_HINTMASK, OP_HLINETO,
    OP_HMOVETO, OP_HSTEM, OP_HSTEMHM, OP_HVCURVETO, OP_RCURVELINE, OP_RETURN, OP_RLINECURVE,
    OP_RLINETO, OP_RMOVETO, OP_RRCURVETO, OP_SHORTINT, OP_VHCURVETO, OP_VLINETO, OP_VMOVETO,
    OP_VSINDEX, OP_VSTEM, OP_VSTEMHM, OP_VVCURVETO,
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
///    masters; emit only the `n` resolved values
///    (`master + Σ scalar(coords) * delta`), each rounded to the nearest
///    whole number, halves away from zero, as HarfBuzz's instancer
///    writes them. The trailing count and the delta operands are
///    dropped.
/// 3. **Strip `vsindex`**: tracks which IVS subtable subsequent
///    `blend`s read from, starting from the one the glyph's Private
///    DICT sets. Dropped from the output (no blend remains).
/// 4. Re-encode all push operands: whole numbers in the integer forms,
///    and a source operand with a fraction in the `b0=255` 16.16 fixed
///    form it came in.
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
    // The blob includes the 2-byte length prefix. The underlying
    // ItemVariationStore parser consumes the body without the prefix.
    let ivs_bytes = match parsed.vstore_blob {
        Some(blob) => Some(blob.get(2..).ok_or(SubsetError::Unsupported(
            "CFF2 VariationStore blob too short",
        ))?),
        None => None,
    };
    let ivs = match ivs_bytes {
        Some(body) => Some(
            ItemVariationStore::parse(body)
                .map_err(|_| SubsetError::Unsupported("CFF2 VariationStore parse failed"))?,
        ),
        None => None,
    };

    // Per-FD: bake every charstring with subr inlining and blend
    // resolution.
    let blend = BlendCache::new(ivs.as_ref(), ivs_bytes.unwrap_or_default(), coords);
    let mut baker = Baker::new(
        blend,
        &parsed.global_subrs,
        bake_token_budget(cff_bytes.len()),
    );
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
            .map(|locals| locals.as_slice())
            .unwrap_or(&[]);
        let vsindex = parsed.per_fd_vsindex.get(fd as usize).copied().unwrap_or(0);
        let baked = baker.bake_charstring(cs, local_subrs, vsindex)?;
        new_charstrings.push(baked);
    }

    // Per-FD Font DICT bodies. We re-emit each Font DICT pointing at
    // its source Private DICT body (with Subrs op stripped: there are
    // no subrs anymore).
    struct FdBakeEmit {
        font_dict_body: Vec<u8>,
        font_dict_private_slot: Option<(usize, usize)>,
        new_private_body: Vec<u8>,
        /// An earlier Font DICT whose Private DICT this one shares.
        shares: Option<usize>,
    }
    let mut fd_emits: Vec<FdBakeEmit> = Vec::with_capacity(parsed.fd_array.len());
    // The Private DICTs' blends resolve at the same coordinates.
    let mut private_blend = BlendCache::new(ivs.as_ref(), ivs_bytes.unwrap_or_default(), coords);
    for (i, (fd_bytes, &private_dict)) in parsed
        .fd_array
        .iter()
        .zip(&parsed.per_fd_private)
        .enumerate()
    {
        let fd_entries = walk_dict(fd_bytes)?;
        let (font_dict_body, font_dict_private_slot) = serialise_font_dict(&fd_entries);
        let shares = parsed.private_of.get(i).copied().filter(|&j| j != i);
        let priv_entries = if private_dict.is_empty() || shares.is_some() {
            Vec::new()
        } else {
            super::private::bake_private(walk_dict(private_dict)?, &mut private_blend)?
        };
        // No local subrs survive the bake.
        let (new_private_body, _) = serialise_private_dict(&priv_entries, false);
        fd_emits.push(FdBakeEmit {
            font_dict_body,
            font_dict_private_slot,
            new_private_body,
            shares,
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
        // A shared Private DICT is written once, where its first Font
        // DICT put it.
        let earlier = f.shares.and_then(|j| {
            per_fd_private_abs
                .get(j)
                .copied()
                .zip(per_fd_private_size.get(j).copied())
        });
        let (abs, size) = earlier.unwrap_or((out.len(), f.new_private_body.len()));
        if earlier.is_none() {
            out.extend_from_slice(&f.new_private_body);
        }
        per_fd_private_abs.push(abs);
        per_fd_private_size.push(size);
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
///
/// One baker serves a whole table: blend data and the token budget are
/// shared across glyphs, and the per-glyph state is reset by
/// [`Baker::bake_charstring`].
struct Baker<'a> {
    blend: BlendCache<'a>,
    global_subrs: &'a [&'a [u8]],
    local_subrs: &'a [&'a [u8]],
    /// Operand stack, in double precision as HarfBuzz keeps it, so a
    /// 16.16 operand round-trips and blend sums don't lose precision.
    stack: Vec<f64>,
    /// Output charstring bytes.
    out: Vec<u8>,
    /// Current vsindex (which IVS subtable blend draws from).
    vsindex: u16,
    /// Running stem count, for hintmask / cntrmask tail size.
    stem_count: usize,
    /// Tokens left for the rest of the table.
    budget: usize,
}

impl<'a> Baker<'a> {
    fn new(blend: BlendCache<'a>, global_subrs: &'a [&'a [u8]], budget: usize) -> Self {
        Self {
            blend,
            global_subrs,
            local_subrs: &[],
            stack: Vec::new(),
            out: Vec::new(),
            vsindex: 0,
            stem_count: 0,
            budget,
        }
    }

    /// Bakes one charstring of a Font DICT with `local_subrs` whose
    /// Private DICT sets `vsindex`.
    fn bake_charstring(
        &mut self,
        cs: &[u8],
        local_subrs: &'a [&'a [u8]],
        vsindex: u16,
    ) -> Result<Vec<u8>, SubsetError> {
        self.local_subrs = local_subrs;
        self.stack.clear();
        self.out = Vec::new();
        self.vsindex = vsindex;
        self.stem_count = 0;
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
        while let Some(&b0) = code.get(pos) {
            charge_token(
                &mut self.budget,
                "CFF2 bake: charstring work budget exceeded",
            )?;
            if b0 >= 32 || b0 == OP_SHORTINT {
                let (val, len) = decode_operand_f64(code, pos)
                    .ok_or(SubsetError::Unsupported("CFF2 bake: operand truncated"))?;
                if self.stack.len() >= MAX_STACK {
                    return Err(SubsetError::Unsupported(
                        "CFF2 bake: operand stack past 513 operands",
                    ));
                }
                self.stack.push(val);
                pos += len;
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
                    if v < 0.0 || v > f64::from(u16::MAX) {
                        return Err(SubsetError::Unsupported(
                            "CFF2 bake: vsindex operand out of range",
                        ));
                    }
                    self.vsindex = v as u16;
                    pos += 1;
                }
                OP_CALLSUBR => {
                    let idx = self.stack.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 bake: callsubr without operand",
                    ))?;
                    let body = biased_subr(self.local_subrs, idx).ok_or(
                        SubsetError::Unsupported("CFF2 bake: callsubr index out of range"),
                    )?;
                    self.run(body, depth + 1)?;
                    pos += 1;
                }
                OP_CALLGSUBR => {
                    let idx = self.stack.pop().ok_or(SubsetError::Unsupported(
                        "CFF2 bake: callgsubr without operand",
                    ))?;
                    let body = biased_subr(self.global_subrs, idx).ok_or(
                        SubsetError::Unsupported("CFF2 bake: callgsubr index out of range"),
                    )?;
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
                    self.stem_count = self.stem_count.saturating_add(self.stack.len() / 2);
                    self.flush_stack();
                    self.out.push(b0);
                    pos += 1;
                }
                OP_HINTMASK | OP_CNTRMASK => {
                    self.stem_count = self.stem_count.saturating_add(self.stack.len() / 2);
                    self.flush_stack();
                    self.out.push(b0);
                    let mask_bytes = self.stem_count.div_ceil(8);
                    let mask = code
                        .get(pos + 1..)
                        .and_then(|rest| rest.get(..mask_bytes))
                        .ok_or(SubsetError::Unsupported(
                            "CFF2 bake: hintmask tail truncated",
                        ))?;
                    self.out.extend_from_slice(mask);
                    pos += 1 + mask_bytes;
                }
                OP_ESCAPE => {
                    let &b1 = code
                        .get(pos + 1)
                        .ok_or(SubsetError::Unsupported("CFF2 bake: escape truncated"))?;
                    self.flush_stack();
                    self.out.push(b0);
                    self.out.push(b1);
                    pos += 2;
                }
                OP_RMOVETO | OP_HMOVETO | OP_VMOVETO | OP_RLINETO | OP_HLINETO | OP_VLINETO
                | OP_RRCURVETO | OP_HHCURVETO | OP_VVCURVETO | OP_HVCURVETO | OP_VHCURVETO
                | OP_RCURVELINE | OP_RLINECURVE => {
                    self.flush_stack();
                    self.out.push(b0);
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
        const UNDERFLOW: SubsetError = SubsetError::Unsupported("CFF2 bake: blend stack underflow");
        // Stack: n master values, n*nRegions delta values, n itself on top.
        let n_raw = self.stack.pop().ok_or(SubsetError::Unsupported(
            "CFF2 bake: blend without count operand",
        ))?;
        if n_raw < 0.0 {
            return Err(SubsetError::Unsupported(
                "CFF2 bake: blend count operand negative",
            ));
        }
        // Saturating cast: NaN gives 0, huge values underflow below.
        let n = n_raw as usize;
        if n == 0 {
            return Ok(());
        }
        let (n_regions, scalars) = self.blend.resolve(self.vsindex);
        let needed = n
            .checked_mul(n_regions)
            .and_then(|total_deltas| total_deltas.checked_add(n))
            .ok_or(UNDERFLOW)?;
        let start = self.stack.len().checked_sub(needed).ok_or(UNDERFLOW)?;
        // The deltas were charged as they were pushed; each value the
        // blend works out is charged here, so blends that leave the same
        // values again and again (no regions) cost what they do.
        self.budget = self.budget.checked_sub(n).ok_or(SubsetError::Unsupported(
            "CFF2 bake: charstring work budget exceeded",
        ))?;
        let (masters, deltas) = self
            .stack
            .get_mut(start..)
            .and_then(|tail| tail.split_at_mut_checked(n))
            .ok_or(UNDERFLOW)?;
        // Each value rounds once its deltas are in, as HarfBuzz's
        // instancer rounds a blend at fixed coordinates.
        for (i, master) in masters.iter_mut().enumerate() {
            // `i * n_regions` stays below `needed`, which did not overflow.
            let row = deltas
                .get(i * n_regions..(i + 1) * n_regions)
                .unwrap_or_default();
            let mut accum = 0.0_f64;
            for (&delta, &s) in row.iter().zip(scalars) {
                accum += f64::from(s) * delta;
            }
            *master = (*master + accum).round();
        }
        self.stack.truncate(start + n);
        Ok(())
    }

    /// Emits the operands currently sitting on the operand stack as
    /// CFF2 push bytes, then clears the stack. Blend results are whole
    /// numbers; a source operand with a fraction keeps the 16.16 fixed
    /// form (`b0=255`).
    fn flush_stack(&mut self) {
        for v in self.stack.drain(..) {
            encode_charstring_number_f64(v, &mut self.out);
        }
    }
}
