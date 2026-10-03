//! Hex decoding plus the synthetic SFNT, maxp, and sbix strike
//! builders shared by the bitmap tests.

pub(crate) fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = from_hex(bytes[i]);
        let lo = from_hex(bytes[i + 1]);
        out.push(hi << 4 | lo);
        i += 2;
    }
    out
}

fn from_hex(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => panic!("non-hex digit"),
    }
}

// ---------------------------------------------------------------------------
// Synthetic SFNT builder for sbix dupe-tag and Unsupported-bitmap tests.
//
// Builds a minimal SFNT carrying just `maxp` and `sbix`. The rest of
// the font directory is unnecessary because Face::glyph_bitmap only
// needs maxp.numGlyphs to walk sbix; the renderer never touches hmtx /
// head / cmap on the bitmap path.
// ---------------------------------------------------------------------------

/// Builds an SFNT directory + payload with the given tagged tables.
/// Tables are written in the order supplied; the SFNT directory is
/// sorted by tag, as the spec requires.
pub(crate) fn build_sfnt(mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|(tag, _)| *tag);
    let n = tables.len();
    let header_len = 12 + 16 * n;
    let mut payloads_off: u32 = header_len as u32;
    // Pad each payload to a 4-byte boundary, as the SFNT spec requires.
    let padded_lens: Vec<usize> = tables.iter().map(|(_, p)| (p.len() + 3) & !3).collect();
    let total: usize = header_len + padded_lens.iter().sum::<usize>();

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfnt_version (TrueType)
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift

    for ((tag, payload), &padded) in tables.iter().zip(padded_lens.iter()) {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum (parser ignores)
        out.extend_from_slice(&payloads_off.to_be_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        payloads_off += padded as u32;
    }
    for ((_, payload), &padded) in tables.iter().zip(padded_lens.iter()) {
        out.extend_from_slice(payload);
        out.resize(out.len() + (padded - payload.len()), 0);
    }
    out
}

pub(crate) fn maxp_05(num_glyphs: u16) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&0x0000_5000u32.to_be_bytes()); // version 0.5
    b.extend_from_slice(&num_glyphs.to_be_bytes());
    b
}

/// Builds an `sbix` table with a single strike at `ppem` whose glyphs
/// carry the supplied tagged payloads. `glyphs[gid] = Some((tag,
/// payload))` writes a glyph entry with a zero origin offset; `None`
/// writes an empty (length-0) slot. `glyphs.len()` must equal
/// `num_glyphs`.
pub(crate) fn build_sbix_strike(
    num_glyphs: u16,
    ppem: u16,
    glyphs: &[Option<([u8; 4], Vec<u8>)>],
) -> Vec<u8> {
    let glyphs: Vec<_> = glyphs
        .iter()
        .map(|g| g.clone().map(|(tag, payload)| (tag, payload, (0, 0))))
        .collect();
    build_sbix_strike_with_offsets(num_glyphs, ppem, &glyphs)
}

/// One sbix glyph record: graphic type tag, payload, and origin offset
/// `(x, y)`, the pixel offset of the image's bottom-left corner from the
/// glyph origin, y up.
pub(crate) type SbixRecord = ([u8; 4], Vec<u8>, (i16, i16));

/// [`build_sbix_strike`] with an origin offset per glyph.
pub(crate) fn build_sbix_strike_with_offsets(
    num_glyphs: u16,
    ppem: u16,
    glyphs: &[Option<SbixRecord>],
) -> Vec<u8> {
    assert_eq!(glyphs.len(), num_glyphs as usize);
    // Strike body: header (4) + offsets[num_glyphs+1] (u32 each) + per-glyph payloads.
    let offset_arr_bytes = 4 * (num_glyphs as usize + 1);
    let mut offsets = Vec::with_capacity(num_glyphs as usize + 1);
    let mut payloads = Vec::new();
    let mut cursor: u32 = 4 + offset_arr_bytes as u32;
    for slot in glyphs {
        offsets.push(cursor);
        if let Some((tag, payload, (x, y))) = slot {
            // 4 bytes of origin offset, 4-byte tag, payload bytes.
            payloads.extend_from_slice(&x.to_be_bytes());
            payloads.extend_from_slice(&y.to_be_bytes());
            payloads.extend_from_slice(tag);
            payloads.extend_from_slice(payload);
            cursor += 8 + payload.len() as u32;
        }
    }
    offsets.push(cursor); // sentinel
    let mut strike = Vec::new();
    strike.extend_from_slice(&ppem.to_be_bytes());
    strike.extend_from_slice(&72u16.to_be_bytes()); // ppi
    for o in &offsets {
        strike.extend_from_slice(&o.to_be_bytes());
    }
    strike.extend_from_slice(&payloads);

    // sbix table: header (8) + strike offsets (u32 per strike) + strike body.
    let mut sbix = Vec::new();
    sbix.extend_from_slice(&1u16.to_be_bytes()); // version
    sbix.extend_from_slice(&0u16.to_be_bytes()); // flags
    sbix.extend_from_slice(&1u32.to_be_bytes()); // numStrikes
    let strike_off: u32 = 8 + 4;
    sbix.extend_from_slice(&strike_off.to_be_bytes());
    sbix.extend_from_slice(&strike);
    sbix
}
