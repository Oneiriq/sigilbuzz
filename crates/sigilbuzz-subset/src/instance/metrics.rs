//! The metric bakes of a full instance: hmtx through HVAR, vmtx through
//! VVAR, and the OS/2, hhea, vhea and post fields MVAR varies.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use super::glyf::clamp_i16;
use crate::SubsetError;

// ---------------------------------------------------------------------------
// hmtx bake
// ---------------------------------------------------------------------------

pub(super) struct HmtxBake {
    pub(super) bytes: Vec<u8>,
    pub(super) number_of_h_metrics: u16,
}

pub(super) fn bake_hmtx(
    face: &Face<'_>,
    coords: &[f32],
    num_glyphs: u16,
) -> Result<HmtxBake, SubsetError> {
    let hmtx = face.hmtx().map_err(SubsetError::from)?;
    let hvar = face.hvar().map_err(SubsetError::from)?;

    let mut advances: Vec<u16> = Vec::with_capacity(num_glyphs as usize);
    let mut lsbs: Vec<i16> = Vec::with_capacity(num_glyphs as usize);
    for gid in 0..num_glyphs {
        let base_adv = hmtx.advance(gid).unwrap_or(0);
        let base_lsb = hmtx.lsb(gid).unwrap_or(0);
        let adv_delta = match hvar.as_ref() {
            Some(h) if !coords.is_empty() => h.advance_delta(gid, coords),
            _ => 0.0,
        };
        // hmtx advances are unsigned; clamp at 0 if a delta would
        // underflow. In practice this only happens with malformed
        // HVAR data.
        let new_adv = (f32::from(base_adv) + adv_delta).round().max(0.0) as i32;
        advances.push(new_adv.clamp(0, i32::from(u16::MAX)) as u16);
        lsbs.push(base_lsb);
    }

    // Compress trailing identical advances into the LSB-only tail.
    let mut long_count = advances.len();
    if long_count > 1 {
        let last = advances[long_count - 1];
        while long_count > 1 && advances[long_count - 1] == last {
            long_count -= 1;
        }
        long_count += 1;
    }
    if long_count == 0 {
        long_count = 1;
    }

    let mut out = Vec::with_capacity(advances.len() * 4);
    for (advance, lsb) in advances.iter().zip(lsbs.iter()).take(long_count) {
        out.extend_from_slice(&advance.to_be_bytes());
        out.extend_from_slice(&lsb.to_be_bytes());
    }
    for lsb in lsbs.iter().skip(long_count) {
        out.extend_from_slice(&lsb.to_be_bytes());
    }

    Ok(HmtxBake {
        bytes: out,
        number_of_h_metrics: long_count as u16,
    })
}

// ---------------------------------------------------------------------------
// vmtx bake (VVAR-aware)
// ---------------------------------------------------------------------------

pub(super) struct VmtxBake {
    /// New `vmtx` bytes, or `None` when the source has no `vmtx`.
    pub(super) vmtx_bytes: Option<Vec<u8>>,
    /// Recomputed `numberOfLongVerMetrics` for the rebuilt table. The
    /// caller must patch `vhea` with this value when it differs from
    /// the source's count. Holds zero when no vmtx was emitted.
    pub(super) number_of_long_ver_metrics: u16,
}

pub(super) fn bake_vmtx(
    face: &Face<'_>,
    coords: &[f32],
    num_glyphs: u16,
) -> Result<VmtxBake, SubsetError> {
    let vmtx = face.vmtx().map_err(SubsetError::from)?;
    let Some(vmtx) = vmtx else {
        return Ok(VmtxBake {
            vmtx_bytes: None,
            number_of_long_ver_metrics: 0,
        });
    };
    // vhea must be present whenever vmtx is. The parser uses
    // `numberOfLongVerMetrics` to slice the table. Confirm presence
    // here so a malformed source (vmtx without vhea) errors cleanly
    // before we try to re-emit. The actual long count is recomputed
    // below from the post-VVAR advance vector.
    let _ = face
        .vhea()
        .map_err(SubsetError::from)?
        .ok_or(SubsetError::Unsupported(
            "instance: vmtx present without vhea",
        ))?;

    let vvar = face.vvar().map_err(SubsetError::from)?;

    // Compute the new (advance, tsb) per gid. Every glyph that ends
    // up in the long range carries its own advance; trailing glyphs
    // share the last advance. We resolve VVAR deltas for *every* gid
    // (including those originally past the source's long count) so
    // that a trailing glyph whose advance now diverges from the
    // shared one extends the long range below.
    let mut advances: Vec<u16> = Vec::with_capacity(num_glyphs as usize);
    let mut tsbs: Vec<i16> = Vec::with_capacity(num_glyphs as usize);
    for gid in 0..num_glyphs {
        let base_adv = vmtx.advance(gid).unwrap_or(0);
        let base_tsb = vmtx.tsb(gid).unwrap_or(0);
        let adv_delta = match vvar.as_ref() {
            Some(v) if !coords.is_empty() => v.advance_height_delta(gid, coords),
            _ => 0.0,
        };
        let tsb_delta = match vvar.as_ref() {
            Some(v) if !coords.is_empty() => v.top_side_bearing_delta(gid, coords).unwrap_or(0.0),
            _ => 0.0,
        };
        let new_adv = (f32::from(base_adv) + adv_delta).round().max(0.0) as i32;
        advances.push(new_adv.clamp(0, i32::from(u16::MAX)) as u16);
        let new_tsb = (f32::from(base_tsb) + tsb_delta).round() as i32;
        tsbs.push(clamp_i16(new_tsb));
    }

    let (out, long_count) = emit_vmtx_bytes(&advances, &tsbs);

    Ok(VmtxBake {
        vmtx_bytes: Some(out),
        number_of_long_ver_metrics: long_count,
    })
}

/// Emits a vmtx body from per-gid `advances` + `tsbs`, recomputing the
/// `numberOfLongVerMetrics` count so trailing glyphs that now share an
/// advance compress into the tsb-only tail. Mirrors `bake_hmtx`'s long-
/// count compression so VVAR-induced advance deltas at trailing gids
/// extend the long range below.
pub(super) fn emit_vmtx_bytes(advances: &[u16], tsbs: &[i16]) -> (Vec<u8>, u16) {
    debug_assert_eq!(advances.len(), tsbs.len());
    let mut long_count = advances.len();
    if long_count > 1 {
        let last = advances[long_count - 1];
        while long_count > 1 && advances[long_count - 1] == last {
            long_count -= 1;
        }
        long_count += 1;
    }
    if long_count == 0 {
        long_count = 1;
    }
    let mut out = Vec::with_capacity(advances.len() * 4);
    for (advance, tsb) in advances.iter().zip(tsbs.iter()).take(long_count) {
        out.extend_from_slice(&advance.to_be_bytes());
        out.extend_from_slice(&tsb.to_be_bytes());
    }
    for tsb in tsbs.iter().skip(long_count) {
        out.extend_from_slice(&tsb.to_be_bytes());
    }
    (out, long_count as u16)
}

// ---------------------------------------------------------------------------
// MVAR bake (OS/2 + hhea + vhea + post)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(super) struct MvarBake {
    pub(super) os2: Option<Vec<u8>>,
    pub(super) hhea: Option<Vec<u8>>,
    pub(super) vhea: Option<Vec<u8>>,
    pub(super) post: Option<Vec<u8>>,
}

/// Walks the source's `MVAR` records, applies each delta to its target
/// field in OS/2 / hhea / vhea / post, and returns the patched table
/// bytes. Tables that don't exist in the source, or whose fields no
/// MVAR record references, return `None` (caller passes through the
/// source bytes).
pub(super) fn bake_mvar_metrics(face: &Face<'_>, coords: &[f32]) -> Result<MvarBake, SubsetError> {
    let mvar = face.mvar().map_err(SubsetError::from)?;
    let Some(mvar) = mvar else {
        return Ok(MvarBake::default());
    };
    if coords.is_empty() {
        return Ok(MvarBake::default());
    }

    let os2 = face.table_bytes(*b"OS/2").ok().map(<[u8]>::to_vec);
    let hhea = face.table_bytes(tag::HHEA).ok().map(<[u8]>::to_vec);
    let vhea = face.table_bytes(tag::VHEA).ok().map(<[u8]>::to_vec);
    let post = face.table_bytes(tag::POST).ok().map(<[u8]>::to_vec);

    apply_mvar_records(&mvar, coords, os2, hhea, vhea, post)
}

/// Walks `mvar.entries()` and patches the rebuilt OS/2 / hhea / vhea /
/// post buffers in place. Splits out from [`bake_mvar_metrics`] so the
/// duplicate-tag dedup policy is unit-testable without spinning up a
/// full Face.
pub(super) fn apply_mvar_records(
    mvar: &sigilbuzz::tables::Mvar<'_>,
    coords: &[f32],
    mut os2: Option<Vec<u8>>,
    hhea: Option<Vec<u8>>,
    mut vhea: Option<Vec<u8>>,
    mut post: Option<Vec<u8>>,
) -> Result<MvarBake, SubsetError> {
    use sigilbuzz::tables::mvar::tag as mvar_tag;
    // OS/2 v0 is 78 bytes; v1+ goes through 96/100. Field offsets
    // (per OpenType OS/2 spec):
    //   sxHeight        (s i16) at v2+ offset 0x56 (86)
    //   sCapHeight      (s i16) at v2+ offset 0x58 (88)
    //   ySubscriptXSize (s i16) 0x0A (10)
    //   ySubscriptYSize          0x0C (12)
    //   ySubscriptXOffset        0x0E (14)
    //   ySubscriptYOffset        0x10 (16)
    //   ySuperscriptXSize        0x12 (18)
    //   ySuperscriptYSize        0x14 (20)
    //   ySuperscriptXOffset      0x16 (22)
    //   ySuperscriptYOffset      0x18 (24)
    //   yStrikeoutSize           0x1A (26)
    //   yStrikeoutPosition       0x1C (28)
    //   sTypoAscender   (i16)    0x44 (68)
    //   sTypoDescender           0x46 (70)
    //   sTypoLineGap             0x48 (72)
    //   usWinAscent     (u16)    0x4A (74)
    //   usWinDescent             0x4C (76)
    //
    // post: italicAngle is offset 4 (Fixed16.16). underlineThickness
    // and underlinePosition are i16 at offsets 10 and 8 respectively.
    //
    // hhea offsets:
    //   ascent / vertTypoAscender at offset 4 (i16)
    //   descent at offset 6
    //   lineGap at offset 8
    //
    // vhea (OpenType / AAT): same layout as hhea, ascent/descent/lineGap
    // are at offsets 4/6/8.

    // Per OpenType MVAR spec each tag appears at most once in a
    // well-formed `valueRecords` array. Malformed fonts can ship the
    // same tag twice; without dedup the patch path applies the delta
    // once per record, doubling its effect on the rebuilt OS/2 / hhea
    // / vhea / post fields. Dedup with first-wins so the rebuild
    // matches the spec-conforming case bit-for-bit.
    //
    // Only the tags below patch a field, so every other record is
    // skipped before its delta is evaluated. The first record for a
    // tag carries the `(outer, inner)` pair `Mvar::metric_delta` would
    // look up, so the delta is read from it directly. Both keep the
    // walk linear in the record count.
    let Some(store) = mvar.variation_store() else {
        return Ok(MvarBake {
            os2,
            hhea,
            vhea,
            post,
        });
    };
    let mut seen: BTreeSet<[u8; 4]> = BTreeSet::new();
    for (rec_tag, (outer, inner)) in mvar.entries() {
        let (buf, off, signed) = match rec_tag {
            t if t == mvar_tag::HORIZ_ASCENDER => (&mut os2, 68, true),
            t if t == mvar_tag::HORIZ_DESCENDER => (&mut os2, 70, true),
            t if t == mvar_tag::HORIZ_LINE_GAP => (&mut os2, 72, true),
            t if t == mvar_tag::HORIZ_CLIPPING_ASCENT => (&mut os2, 74, false),
            t if t == mvar_tag::HORIZ_CLIPPING_DESCENT => (&mut os2, 76, false),
            t if t == mvar_tag::X_HEIGHT => (&mut os2, 86, true),
            t if t == mvar_tag::CAP_HEIGHT => (&mut os2, 88, true),
            t if t == mvar_tag::SUBSCRIPT_X_SIZE => (&mut os2, 10, true),
            t if t == mvar_tag::SUBSCRIPT_Y_SIZE => (&mut os2, 12, true),
            t if t == mvar_tag::SUBSCRIPT_X_OFFSET => (&mut os2, 14, true),
            t if t == mvar_tag::SUBSCRIPT_Y_OFFSET => (&mut os2, 16, true),
            t if t == mvar_tag::SUPERSCRIPT_X_SIZE => (&mut os2, 18, true),
            t if t == mvar_tag::SUPERSCRIPT_Y_SIZE => (&mut os2, 20, true),
            t if t == mvar_tag::SUPERSCRIPT_X_OFFSET => (&mut os2, 22, true),
            t if t == mvar_tag::SUPERSCRIPT_Y_OFFSET => (&mut os2, 24, true),
            t if t == mvar_tag::STRIKEOUT_SIZE => (&mut os2, 26, true),
            t if t == mvar_tag::STRIKEOUT_OFFSET => (&mut os2, 28, true),
            t if t == mvar_tag::VERT_ASCENDER => (&mut vhea, 4, true),
            t if t == mvar_tag::VERT_DESCENDER => (&mut vhea, 6, true),
            t if t == mvar_tag::VERT_LINE_GAP => (&mut vhea, 8, true),
            t if t == mvar_tag::UNDERLINE_SIZE => (&mut post, 10, true),
            t if t == mvar_tag::UNDERLINE_OFFSET => (&mut post, 8, true),
            _ => continue, // unrecognized tag: silently ignore
        };
        if !seen.insert(rec_tag) {
            continue;
        }
        let delta = store.delta(outer, inner, coords).round() as i32;
        if delta == 0 {
            continue;
        }
        if signed {
            patch_i16(buf, off, delta);
        } else {
            patch_u16(buf, off, delta);
        }
    }

    Ok(MvarBake {
        os2,
        hhea,
        vhea,
        post,
    })
}

/// Returns the two bytes at `buf[off..off + 2]`, or `None` when the
/// table is absent or too short.
fn field_bytes(buf: &mut Option<Vec<u8>>, off: usize) -> Option<&mut [u8; 2]> {
    buf.as_mut()?.get_mut(off..)?.first_chunk_mut::<2>()
}

/// Adds `delta` to the big-endian `i16` at `off`, clamping to the field
/// range. A delta from a long-word variation store can reach
/// `i32::MAX`, so the sum saturates before the clamp.
pub(super) fn patch_i16(buf: &mut Option<Vec<u8>>, off: usize, delta: i32) {
    let Some(field) = field_bytes(buf, off) else {
        return;
    };
    let cur = i16::from_be_bytes(*field);
    let new = i32::from(cur)
        .saturating_add(delta)
        .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    *field = new.to_be_bytes();
}

/// Adds `delta` to the big-endian `u16` at `off`, clamping to the field
/// range.
pub(super) fn patch_u16(buf: &mut Option<Vec<u8>>, off: usize, delta: i32) {
    let Some(field) = field_bytes(buf, off) else {
        return;
    };
    let cur = u16::from_be_bytes(*field);
    let new = i32::from(cur)
        .saturating_add(delta)
        .clamp(0, i32::from(u16::MAX)) as u16;
    *field = new.to_be_bytes();
}
