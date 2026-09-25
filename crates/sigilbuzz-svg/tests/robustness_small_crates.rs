//! Robustness tests for the SVG serializer: hostile coordinates,
//! out-of-range glyph ids, and a COLRv1 paint graph whose composed
//! transform grows past the range the number formatter used to handle.

use sigilbuzz::tables::PathOp;
use sigilbuzz::Face;
use sigilbuzz_svg::{glyph_to_svg, glyph_to_svg_at_coords, path_data};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

fn assert_only_finite_tokens(svg: &str) {
    assert!(!svg.contains("inf"), "inf leaked: {svg}");
    assert!(!svg.contains("NaN"), "NaN leaked: {svg}");
}

#[test]
fn path_data_with_huge_finite_coords_has_no_inf() {
    let ops = [
        PathOp::MoveTo {
            x: f32::MAX,
            y: -f32::MAX,
        },
        PathOp::LineTo { x: 1.0e36, y: 0.0 },
        PathOp::Close,
    ];
    let d = path_data(&ops);
    assert_only_finite_tokens(&d);
    assert!(d.starts_with("M 340282346638528859811704183484516925440 "));
}

#[test]
fn out_of_range_gid_returns_none() {
    let face = Face::parse_bytes(OPEN_SANS, 0).expect("Open Sans parses");
    assert!(glyph_to_svg(&face, u16::MAX).is_none());
}

#[test]
fn hostile_coords_do_not_panic() {
    let face = Face::parse_bytes(OPEN_SANS, 0).expect("Open Sans parses");
    let gid = face.cmap().expect("cmap").glyph_id('A').expect("'A'");
    for coords in [
        &[f32::NAN][..],
        &[f32::INFINITY, f32::NEG_INFINITY],
        &[f32::MAX; 64],
    ] {
        if let Some(svg) = glyph_to_svg_at_coords(&face, gid, coords) {
            assert_only_finite_tokens(&svg);
        }
    }
}

// =========================================================================
// COLRv1 with a deep chain of PaintTransform records.
// =========================================================================

/// Rebuilds `base` with extra tables appended. The table directory
/// grows by 16 bytes per new table, so every original offset shifts by
/// that amount. Checksums are left at zero. The parser does not check
/// them.
#[cfg(feature = "color")]
fn append_tables(base: &[u8], extra: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let num_tables = usize::from(u16::from_be_bytes([base[4], base[5]]));
    let old_dir_end = 12 + num_tables * 16;
    let new_count = num_tables + extra.len();
    let shift = extra.len() * 16;

    let mut out = Vec::new();
    out.extend_from_slice(&base[..4]);
    out.extend_from_slice(&(new_count as u16).to_be_bytes());
    out.extend_from_slice(&base[6..12]);
    for rec in base[12..old_dir_end].chunks_exact(16) {
        let offset = u32::from_be_bytes([rec[8], rec[9], rec[10], rec[11]]);
        out.extend_from_slice(&rec[..8]);
        out.extend_from_slice(&(offset + shift as u32).to_be_bytes());
        out.extend_from_slice(&rec[12..16]);
    }
    let mut tail_offset = base.len() + shift;
    let mut tail = Vec::new();
    for (tag, data) in extra {
        while (tail_offset + tail.len()) % 4 != 0 {
            tail.push(0);
        }
        let offset = tail_offset + tail.len();
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        tail.extend_from_slice(data);
    }
    out.extend_from_slice(&base[old_dir_end..]);
    tail_offset = out.len();
    assert_eq!(tail_offset, base.len() + shift);
    out.extend_from_slice(&tail);
    out
}

/// COLRv1 table whose single base glyph `base_gid` paints `leaf_gid`
/// in palette color 0 under `levels` nested PaintTransform records.
/// Each record scales both axes by `scale`.
#[cfg(feature = "color")]
fn nested_transform_colr(base_gid: u16, leaf_gid: u16, levels: usize, scale: i16) -> Vec<u8> {
    let header_len: u32 = 34;
    let mut colr = Vec::new();
    colr.extend_from_slice(&1u16.to_be_bytes()); // version
    colr.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords
    colr.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphRecordsOffset
    colr.extend_from_slice(&header_len.to_be_bytes()); // layerRecordsOffset
    colr.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    colr.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphListOffset
    colr.extend_from_slice(&0u32.to_be_bytes()); // layerListOffset
    colr.extend_from_slice(&0u32.to_be_bytes()); // clipListOffset
    colr.extend_from_slice(&0u32.to_be_bytes()); // varIndexMapOffset
    colr.extend_from_slice(&0u32.to_be_bytes()); // itemVariationStoreOffset
                                                 // BaseGlyphList with one record. The paint follows at offset 10.
    colr.extend_from_slice(&1u32.to_be_bytes());
    colr.extend_from_slice(&base_gid.to_be_bytes());
    colr.extend_from_slice(&10u32.to_be_bytes());

    let fixed_scale = (i32::from(scale) << 16).to_be_bytes();
    for _ in 0..levels {
        // PaintTransform: format 12, child paint after the Affine2x3,
        // Affine2x3 right after this 7-byte record.
        colr.push(12);
        colr.extend_from_slice(&[0, 0, 31]);
        colr.extend_from_slice(&[0, 0, 7]);
        colr.extend_from_slice(&fixed_scale); // xx
        colr.extend_from_slice(&0i32.to_be_bytes()); // yx
        colr.extend_from_slice(&0i32.to_be_bytes()); // xy
        colr.extend_from_slice(&fixed_scale); // yy
        colr.extend_from_slice(&0i32.to_be_bytes()); // dx
        colr.extend_from_slice(&0i32.to_be_bytes()); // dy
    }
    // PaintGlyph (format 10) with a PaintSolid (format 2) child.
    colr.push(10);
    colr.extend_from_slice(&[0, 0, 6]);
    colr.extend_from_slice(&leaf_gid.to_be_bytes());
    colr.push(2);
    colr.extend_from_slice(&0u16.to_be_bytes());
    colr.extend_from_slice(&0x4000u16.to_be_bytes());
    colr
}

#[cfg(feature = "color")]
fn one_color_cpal() -> Vec<u8> {
    let mut cpal = Vec::new();
    cpal.extend_from_slice(&0u16.to_be_bytes()); // version
    cpal.extend_from_slice(&1u16.to_be_bytes()); // numPaletteEntries
    cpal.extend_from_slice(&1u16.to_be_bytes()); // numPalettes
    cpal.extend_from_slice(&1u16.to_be_bytes()); // numColorRecords
    cpal.extend_from_slice(&14u32.to_be_bytes()); // colorRecordsArrayOffset
    cpal.extend_from_slice(&0u16.to_be_bytes()); // colorRecordIndices[0]
    cpal.extend_from_slice(&[0, 0, 255, 255]); // BGRA red
    cpal
}

#[cfg(feature = "color")]
#[test]
fn deep_transform_chain_emits_only_finite_tokens() {
    use sigilbuzz_svg::glyph_to_svg_color;

    let plain = Face::parse_bytes(OPEN_SANS, 0).expect("Open Sans parses");
    let gid = plain.cmap().expect("cmap").glyph_id('A').expect("'A'");

    // 30000^8 is about 6.6e35. Scaling that by 1000 for rounding
    // overflows f32, which used to print "inf" in the matrix.
    let colr = nested_transform_colr(gid, gid, 8, 30000);
    let bytes = append_tables(OPEN_SANS, &[(*b"COLR", colr), (*b"CPAL", one_color_cpal())]);
    let face = Face::parse_bytes(&bytes, 0).expect("patched face parses");

    let svg = glyph_to_svg_color(&face, gid).expect("color glyph renders");
    assert!(svg.contains("transform=\"matrix("), "no transform: {svg}");
    assert_only_finite_tokens(&svg);
}
