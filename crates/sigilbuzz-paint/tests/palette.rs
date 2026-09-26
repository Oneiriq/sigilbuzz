//! Palette selection: `evaluate_with_palette` resolves colors in the
//! CPAL palette the caller picks.

use sigilbuzz::Face;
use sigilbuzz_paint::{evaluate_at_coords, evaluate_with_palette, Color, DrawCmd, PaintSource};

/// Minimal SFNT holding only `COLR` and `CPAL`.
fn build_face_bytes(colr: &[u8], cpal: &[u8]) -> Vec<u8> {
    let dir_len = 12 + 2 * 16;
    let colr_off = dir_len;
    let cpal_off = colr_off + colr.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes()); // numTables
    out.extend_from_slice(&[0; 6]); // searchRange, entrySelector, rangeShift
    for (tag, off, len) in [
        (b"COLR", colr_off, colr.len()),
        (b"CPAL", cpal_off, cpal.len()),
    ] {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum
        out.extend_from_slice(&(off as u32).to_be_bytes());
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
    out.extend_from_slice(colr);
    out.extend_from_slice(cpal);
    out
}

/// A v1 COLR whose glyph 7 is a PaintSolid using palette entry 0.
fn build_solid_colr() -> Vec<u8> {
    let header_len: u32 = 34;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords
    out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphRecordsOffset
    out.extend_from_slice(&header_len.to_be_bytes()); // layerRecordsOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphListOffset
    out.extend_from_slice(&[0; 16]); // layerList, clipList, varIndexMap, varStore
    out.extend_from_slice(&1u32.to_be_bytes()); // BaseGlyphList count
    out.extend_from_slice(&7u16.to_be_bytes()); // glyph id
    out.extend_from_slice(&10u32.to_be_bytes()); // paint offset
    out.push(2); // PaintSolid
    out.extend_from_slice(&0u16.to_be_bytes()); // palette entry 0
    out.extend_from_slice(&0x4000u16.to_be_bytes()); // alpha 1.0
    out
}

/// A v0 CPAL with two palettes of one entry: red, then blue.
fn build_two_palette_cpal() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&1u16.to_be_bytes()); // numPaletteEntries
    out.extend_from_slice(&2u16.to_be_bytes()); // numPalettes
    out.extend_from_slice(&2u16.to_be_bytes()); // numColorRecords
    out.extend_from_slice(&16u32.to_be_bytes()); // colorRecordsArrayOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // palette 0 starts at record 0
    out.extend_from_slice(&1u16.to_be_bytes()); // palette 1 starts at record 1
    out.extend_from_slice(&[0, 0, 255, 255]); // BGRA red
    out.extend_from_slice(&[255, 0, 0, 255]); // BGRA blue
    out
}

fn solid_color(cmds: &[DrawCmd]) -> Color {
    match cmds {
        [DrawCmd::FillGlyph {
            paint: PaintSource::Solid(c),
            ..
        }] => *c,
        other => panic!("expected one solid fill, got {other:?}"),
    }
}

#[test]
fn palette_selects_the_cpal_palette() {
    let bytes = build_face_bytes(&build_solid_colr(), &build_two_palette_cpal());
    let face = Face::parse_bytes(&bytes, 0).unwrap();

    let red = solid_color(&evaluate_with_palette(&face, 7, &[], 0));
    assert!((red.r - 1.0).abs() < 1e-6 && red.b.abs() < 1e-6, "{red:?}");
    let blue = solid_color(&evaluate_with_palette(&face, 7, &[], 1));
    assert!(
        (blue.b - 1.0).abs() < 1e-6 && blue.r.abs() < 1e-6,
        "{blue:?}"
    );

    // evaluate_at_coords keeps using palette 0.
    let default = solid_color(&evaluate_at_coords(&face, 7, &[]));
    assert_eq!(default, red);
}

#[test]
fn missing_palette_resolves_to_transparent() {
    let bytes = build_face_bytes(&build_solid_colr(), &build_two_palette_cpal());
    let face = Face::parse_bytes(&bytes, 0).unwrap();
    let color = solid_color(&evaluate_with_palette(&face, 7, &[], 5));
    assert_eq!(color, Color::TRANSPARENT);
}
