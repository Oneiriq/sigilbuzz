//! Synthetic SFNT, COLR and CPAL byte builders shared by the
//! evaluator tests.

// =========================================================================
// Synthetic SFNT + COLR/CPAL fixtures
// =========================================================================

/// Builds a minimal SFNT containing exactly the COLR and CPAL tables
/// supplied. The face has no glyf / loca so `glyph_outline` would
/// fail, but the evaluator only ever asks the face for COLR + CPAL,
/// which is the contract this fixture exercises.
pub(crate) fn build_face_bytes(colr: &[u8], cpal: &[u8]) -> Vec<u8> {
    // SFNT header (12) + 2 records (16 each) = 44 bytes of directory.
    let dir_len = 12 + 2 * 16;
    let cpal_off = dir_len;
    let colr_off = cpal_off + cpal.len();

    let mut out = Vec::new();
    // sfnt version: TrueType. (sigilbuzz accepts both: color fonts
    // typically ship CBDT/SBIX fronts but the directory is identical.)
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes()); // numTables
    out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift

    // Table records must be sorted by tag. 'C' < 'C' (CPAL == COLR
    // first three bytes), so order is COLR before CPAL alphabetically:
    // 'C','O','L','R' vs 'C','P','A','L': `O` (0x4F) < `P` (0x50).
    // Place COLR first.
    out.extend_from_slice(b"COLR");
    out.extend_from_slice(&0u32.to_be_bytes()); // checksum
    out.extend_from_slice(&(colr_off as u32).to_be_bytes());
    out.extend_from_slice(&(colr.len() as u32).to_be_bytes());

    out.extend_from_slice(b"CPAL");
    out.extend_from_slice(&0u32.to_be_bytes()); // checksum
    out.extend_from_slice(&(cpal_off as u32).to_be_bytes());
    out.extend_from_slice(&(cpal.len() as u32).to_be_bytes());

    out.extend_from_slice(cpal);
    out.extend_from_slice(colr);
    out
}

/// Builds a v0 CPAL with a single palette of `colors`.
pub(crate) fn build_cpal_v0(colors: &[(u8, u8, u8, u8)]) -> Vec<u8> {
    let num_palettes: u16 = 1;
    let entries = colors.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&entries.to_be_bytes());
    out.extend_from_slice(&num_palettes.to_be_bytes());
    out.extend_from_slice(&entries.to_be_bytes()); // numColorRecords
    let header_plus_indices = 12 + num_palettes as usize * 2;
    out.extend_from_slice(&(header_plus_indices as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // colorRecordIndices[0] = 0
    for (r, g, b, a) in colors {
        out.push(*b);
        out.push(*g);
        out.push(*r);
        out.push(*a);
    }
    out
}

/// Header for a v1 COLR with one base-glyph paint record. `num_base`
/// must be 1; the paint body sits at the end of the table starting at
/// offset 10 (relative to the BaseGlyphList start) and the caller
/// appends its bytes after this header returns.
pub(crate) fn build_v1_header(glyph_id: u16) -> Vec<u8> {
    let header_len = 34; // v0 (14) + v1 appendix (20)
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords (v0)
    out.extend_from_slice(&(header_len as u32).to_be_bytes()); // baseGlyphRecordsOffset
    out.extend_from_slice(&(header_len as u32).to_be_bytes()); // layerRecordsOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    out.extend_from_slice(&(header_len as u32).to_be_bytes()); // baseGlyphListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // layerListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // clipListOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // varIndexMapOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // varStoreOffset

    // BaseGlyphList: numRecords = 1, then the record { glyphID,
    // paintOffset = 10 }. The body that follows must start at offset
    // 10 from the BaseGlyphList start (4 header + 6 record).
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&glyph_id.to_be_bytes());
    out.extend_from_slice(&10u32.to_be_bytes());
    out
}

/// Convenience: F2DOT14 encoder for raw alphas / scales / angles.
pub(crate) fn f2dot14(v: f32) -> [u8; 2] {
    let raw = (v * 16384.0).round() as i16;
    raw.to_be_bytes()
}
