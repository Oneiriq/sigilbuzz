//! Small in-place rewrites for tables we mostly pass through.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::SubsetError;

/// Patches `head.indexToLocFormat` (offset 50: 0=short, 1=long).
pub fn write_index_to_loc_format(head: &mut [u8], long: bool) {
    if head.len() < 52 {
        return;
    }
    let val: i16 = if long { 1 } else { 0 };
    head[50..52].copy_from_slice(&val.to_be_bytes());
}

/// Patches `hhea.numberOfHMetrics` (last two bytes of the table).
pub fn write_hhea_metrics_count(hhea: &mut [u8], n: u16) -> Result<(), SubsetError> {
    if hhea.len() < 36 {
        return Err(SubsetError::Unsupported("hhea too short to patch"));
    }
    let off = hhea.len() - 2;
    hhea[off..off + 2].copy_from_slice(&n.to_be_bytes());
    Ok(())
}

/// Patches `vhea.numberOfLongVerMetrics` (last two bytes of the table,
/// vhea v1.0 / v1.1 share an identical byte layout to `hhea`).
pub fn write_vhea_metrics_count(vhea: &mut [u8], n: u16) -> Result<(), SubsetError> {
    if vhea.len() < 36 {
        return Err(SubsetError::Unsupported("vhea too short to patch"));
    }
    let off = vhea.len() - 2;
    vhea[off..off + 2].copy_from_slice(&n.to_be_bytes());
    Ok(())
}

/// Patches `maxp.numGlyphs` (offset 4..6).
pub fn write_maxp_num_glyphs(maxp: &mut [u8], n: u16) -> Result<(), SubsetError> {
    if maxp.len() < 6 {
        return Err(SubsetError::Unsupported("maxp too short to patch"));
    }
    maxp[4..6].copy_from_slice(&n.to_be_bytes());
    Ok(())
}

/// Builds a `post` format-3 table. Format 3 carries no glyph names
/// at all: the only payload is the 32-byte header. We pull
/// `italicAngle` / `underlinePosition` / `underlineThickness` /
/// `isFixedPitch` from the source font when present so kerning-
/// adjacent renderers that consult these still get sane values.
pub fn synthesize_post_format_3(face: &Face<'_>) -> Result<Vec<u8>, SubsetError> {
    let mut out = Vec::with_capacity(32);
    // version: 3.0 (0x00030000)
    out.extend_from_slice(&0x0003_0000u32.to_be_bytes());

    // Default values when the source has no post.
    let mut italic_angle: u32 = 0; // Fixed16.16
    let mut underline_position: i16 = 0;
    let mut underline_thickness: i16 = 0;
    let mut is_fixed_pitch: u32 = 0;
    let mut min_mem_t42: u32 = 0;
    let mut max_mem_t42: u32 = 0;
    let mut min_mem_t1: u32 = 0;
    let mut max_mem_t1: u32 = 0;

    if let Ok(post) = face.table_bytes(tag::POST) {
        if post.len() >= 32 {
            italic_angle = u32::from_be_bytes([post[4], post[5], post[6], post[7]]);
            underline_position = i16::from_be_bytes([post[8], post[9]]);
            underline_thickness = i16::from_be_bytes([post[10], post[11]]);
            is_fixed_pitch = u32::from_be_bytes([post[12], post[13], post[14], post[15]]);
            min_mem_t42 = u32::from_be_bytes([post[16], post[17], post[18], post[19]]);
            max_mem_t42 = u32::from_be_bytes([post[20], post[21], post[22], post[23]]);
            min_mem_t1 = u32::from_be_bytes([post[24], post[25], post[26], post[27]]);
            max_mem_t1 = u32::from_be_bytes([post[28], post[29], post[30], post[31]]);
        }
    }

    out.extend_from_slice(&italic_angle.to_be_bytes());
    out.extend_from_slice(&underline_position.to_be_bytes());
    out.extend_from_slice(&underline_thickness.to_be_bytes());
    out.extend_from_slice(&is_fixed_pitch.to_be_bytes());
    out.extend_from_slice(&min_mem_t42.to_be_bytes());
    out.extend_from_slice(&max_mem_t42.to_be_bytes());
    out.extend_from_slice(&min_mem_t1.to_be_bytes());
    out.extend_from_slice(&max_mem_t1.to_be_bytes());

    debug_assert_eq!(out.len(), 32);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_loc_format_short_long() {
        let mut head = alloc::vec![0u8; 54];
        write_index_to_loc_format(&mut head, true);
        assert_eq!(head[50], 0);
        assert_eq!(head[51], 1);
        write_index_to_loc_format(&mut head, false);
        assert_eq!(head[50], 0);
        assert_eq!(head[51], 0);
    }

    #[test]
    fn write_hhea_count_patches_tail() {
        let mut hhea = alloc::vec![0u8; 36];
        write_hhea_metrics_count(&mut hhea, 42).unwrap();
        assert_eq!(&hhea[34..36], &42u16.to_be_bytes());
    }

    #[test]
    fn write_maxp_glyphs_patches_offset_4() {
        let mut maxp = alloc::vec![0u8; 6];
        write_maxp_num_glyphs(&mut maxp, 9000).unwrap();
        assert_eq!(&maxp[4..6], &9000u16.to_be_bytes());
    }
}
