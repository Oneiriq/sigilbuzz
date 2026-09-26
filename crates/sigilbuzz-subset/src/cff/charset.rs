//! Kept-glyph SID and code extraction from explicit and predefined
//! CFF1 charsets and Encodings.

use alloc::vec::Vec;

use crate::SubsetError;

// ----------------------------------------------------------------------------
// Predefined charsets / encodings.
//
// CFF1 defines three "predefined" charsets and two predefined encodings.
// When the Top DICT's charset / Encoding operand is `0`, `1`, or `2`, the
// font uses the predefined table. No charset / Encoding bytes appear in
// the source CFF. The orchestration must reproduce the kept-gid SIDs
// (or char codes) the predefined table encodes when the source uses one.
// ----------------------------------------------------------------------------

/// ISOAdobe predefined charset SIDs for gid 1..=228. Gid 0 is implicit
/// `.notdef` (SID 0). Source: Adobe TN 5176 Appendix C, Table 22.
/// SID `i` for gid `i` (i in 1..=228). The SIDs are sequential.
const ISO_ADOBE_LEN: u16 = 228;

/// Reads the kept-gid SIDs from the source charset. The returned vec
/// has one SID per *kept gid except gid 0* in the new compacted order.
/// Predefined charsets (off 0/1/2) are expanded via lookup; explicit
/// charsets (off >= 3) are walked from the source bytes.
///
/// The kept-gid order is the input slice of source-font gids in old
/// order. Gid 0 is dropped (it's implicitly `.notdef`, never carries
/// an entry).
pub(super) fn extract_kept_charset_sids(
    data: &[u8],
    charset_off: u32,
    n_glyphs: usize,
    kept_gids: &[u16],
) -> Result<Vec<u16>, SubsetError> {
    // First materialize the SID-per-gid table for gid 1..n_glyphs-1.
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
        // Expert / ExpertSubset: we don't reproduce these. Treat as
        // unsupported so the caller falls back to the dropping path
        // rather than emit a corrupt charset.
        return Err(SubsetError::Unsupported(
            "CFF1 predefined Expert / ExpertSubset charset not yet supported",
        ));
    } else {
        read_explicit_charset(data, charset_off as usize, n_glyphs.saturating_sub(1))?
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

/// Walks an explicit charset (format 0, 1, or 2) at `data[off..]` and
/// returns the SIDs of gids `1..=n_left`.
fn read_explicit_charset(data: &[u8], off: usize, n_left: usize) -> Result<Vec<u16>, SubsetError> {
    let (&format, body) = data
        .get(off..)
        .and_then(<[u8]>::split_first)
        .ok_or(SubsetError::Unsupported("CFF1 charset offset past end"))?;
    let mut sids = alloc::vec![0u16; n_left];
    match format {
        0 => {
            let body = n_left
                .checked_mul(2)
                .and_then(|len| body.get(..len))
                .ok_or(SubsetError::Unsupported("CFF1 charset format 0 truncated"))?;
            for (slot, sid) in sids.iter_mut().zip(body.chunks_exact(2)) {
                *slot = u16::from_be_bytes([sid[0], sid[1]]);
            }
        }
        1 | 2 => {
            let record_size = if format == 1 { 3 } else { 4 };
            let mut records = body;
            let mut written = 0usize;
            while written < n_left {
                let (record, rest) =
                    records
                        .split_at_checked(record_size)
                        .ok_or(SubsetError::Unsupported(
                            "CFF1 charset format 1/2 truncated",
                        ))?;
                records = rest;
                let (first, n_l) = match *record {
                    [f0, f1, n] => (u16::from_be_bytes([f0, f1]), usize::from(n)),
                    [f0, f1, n0, n1] => (
                        u16::from_be_bytes([f0, f1]),
                        usize::from(u16::from_be_bytes([n0, n1])),
                    ),
                    // `record_size` is 3 or 4, so no other shape occurs.
                    _ => (0, 0),
                };
                let take = (n_l + 1).min(n_left - written);
                // A range that runs past SID 0xFFFF is malformed. The
                // SIDs wrap so the walk stays total.
                for (k, slot) in sids.iter_mut().skip(written).take(take).enumerate() {
                    *slot = first.wrapping_add(k as u16);
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
    Ok(sids)
}

/// Reads the kept-gid char codes from the source Encoding. Returns one
/// code per kept gid except gid 0, matching the charset's shape.
///
/// Predefined encoding offsets `0` (Standard) and `1` (Expert) are not
/// expanded: every gid gets code 0. Explicit encodings (>= 2) are
/// walked.
pub(super) fn extract_kept_encoding_codes(
    data: &[u8],
    encoding_off: u32,
    charset_per_gid_sids: &[u16],
    kept_gids: &[u16],
) -> Result<Vec<u8>, SubsetError> {
    // Build the per-gid code table for gid 1..=charset_per_gid_sids.len().
    let n_left = charset_per_gid_sids.len();
    let mut per_gid = alloc::vec![0u8; n_left];
    if encoding_off == 0 || encoding_off == 1 {
        // Predefined Standard / Expert encoding maps SID to code via a
        // fixed table. Reproducing those tables byte-for-byte for an
        // arbitrary source font is large and rarely useful. Most CFF
        // fonts ship explicit Encodings. Conservative: when the source
        // uses a predefined Encoding, pass it through *by emitting a
        // best-effort all-zeros encoding* and rely on cmap for char ->
        // gid mapping. The Type 2 spec allows any Encoding shape; the
        // round-trip test suite covers the explicit-Encoding path.
        // (Implemented as a no-op zero table, later format-auto'd.)
    } else {
        let Some((&format_byte, body)) = data
            .get(encoding_off as usize..)
            .and_then(<[u8]>::split_first)
        else {
            return Err(SubsetError::Unsupported("CFF1 Encoding offset past end"));
        };
        let format = format_byte & 0x7F; // strip supplemental-encodings bit
        match format {
            0 => {
                let Some((&n_codes, codes)) = body.split_first() else {
                    return Err(SubsetError::Unsupported("CFF1 Encoding fmt 0 truncated"));
                };
                let codes = codes
                    .get(..usize::from(n_codes))
                    .ok_or(SubsetError::Unsupported("CFF1 Encoding fmt 0 short"))?;
                for (slot, &code) in per_gid.iter_mut().zip(codes) {
                    *slot = code;
                }
            }
            1 => {
                let Some((&n_ranges, mut ranges)) = body.split_first() else {
                    return Err(SubsetError::Unsupported("CFF1 Encoding fmt 1 truncated"));
                };
                let mut written = 0usize;
                for _ in 0..n_ranges {
                    let Some((&[first, n_left_rec], rest)) = ranges.split_first_chunk::<2>() else {
                        return Err(SubsetError::Unsupported("CFF1 Encoding fmt 1 short"));
                    };
                    ranges = rest;
                    let take = (usize::from(n_left_rec) + 1).min(n_left.saturating_sub(written));
                    for (k, slot) in per_gid.iter_mut().skip(written).take(take).enumerate() {
                        *slot = first.wrapping_add(k as u8);
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
