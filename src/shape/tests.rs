//! Tests for the shaping entry point: segmentation, cmap and advance
//! mapping, clusters, normalization, and Extension unwrapping. The
//! GSUB lookup tests live in the `lookups` child module.

use super::segment::build_segments;
use super::*;
use crate::blob::Blob;
use crate::buffer::Buffer;
use crate::face::Face;
use crate::font::Font;
use crate::tables::cmap::{build_cmap_wrapper, build_format4};
use alloc::vec::Vec;

mod lookups;

#[test]
fn kana_led_segments_use_the_kana_script_tag() {
    let priority = |text: &str| {
        let cps: Vec<char> = text.chars().collect();
        build_segments(&cps)[0].script_priority
    };
    assert_eq!(priority("\u{30AB}\u{30CA}")[0], *b"kana");
    assert_eq!(priority("\u{3067}\u{3059}\u{65E5}\u{672C}")[0], *b"kana");
    assert_eq!(priority("\u{65E5}\u{672C}\u{3067}\u{3059}")[0], *b"hani");
    assert_eq!(priority("12 \u{30AB}")[0], *b"kana");
}

/// Minimal font with head / maxp / hhea / hmtx / cmap sufficient
/// for `shape()` to run against real ASCII text. Glyph 0 is
/// `.notdef` (advance 0); glyph 1 is 'A' (advance 500); glyph 2
/// is 'B' (advance 600); glyph 3 is 'C' (advance 700).
fn build_shapeable_font() -> Vec<u8> {
    // head table.
    let mut head = Vec::new();
    head.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
    head.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
    head.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // fontRevision
    head.extend_from_slice(&0u32.to_be_bytes()); // checksumAdjustment
    head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes()); // magic
    head.extend_from_slice(&0u16.to_be_bytes()); // flags
    head.extend_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
    head.extend_from_slice(&[0; 8 + 8 + 8 + 2 + 2 + 2]); // dates + bboxes + macStyle + ppem + hint
    head.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat
    head.extend_from_slice(&0i16.to_be_bytes()); // glyphDataFormat

    // maxp 0.5: 4 glyphs.
    let mut maxp = Vec::new();
    maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp.extend_from_slice(&4u16.to_be_bytes());

    // hhea: numberOfHMetrics = 4.
    let mut hhea = Vec::new();
    hhea.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
    hhea.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
    hhea.extend_from_slice(&800i16.to_be_bytes()); // ascent
    hhea.extend_from_slice(&(-200i16).to_be_bytes()); // descent
    hhea.extend_from_slice(&0i16.to_be_bytes()); // lineGap
    hhea.extend_from_slice(&[0; 14]); // advanceWidthMax + six more
    hhea.extend_from_slice(&[0; 8]); // four reserved
    hhea.extend_from_slice(&0i16.to_be_bytes()); // metricDataFormat
    hhea.extend_from_slice(&4u16.to_be_bytes()); // numberOfHMetrics

    // hmtx: (advance, lsb) x 4.
    let mut hmtx = Vec::new();
    for (adv, lsb) in &[(0u16, 0i16), (500, 0), (600, 0), (700, 0)] {
        hmtx.extend_from_slice(&adv.to_be_bytes());
        hmtx.extend_from_slice(&lsb.to_be_bytes());
    }

    // cmap: format 4 mapping 'A'..='C' to glyphs 1..=3.
    // idDelta = -64 gives: 'A' (0x41) -> 1, 'B' -> 2, 'C' -> 3.
    let cmap_sub = build_format4(&[(b'A' as u16, b'C' as u16, -64)]);
    let cmap = build_cmap_wrapper(&[(3, 1, cmap_sub)]);

    // Now assemble the SFNT directory with all five tables.
    let tables: Vec<([u8; 4], Vec<u8>)> = alloc::vec![
        (*b"cmap", cmap),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"maxp", maxp),
    ];
    assemble_sfnt(&tables)
}

fn assemble_sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let header_len = 12 + tables.len() * 16;
    let mut body_offset = header_len;
    let mut offsets = Vec::with_capacity(tables.len());
    for (_tag, body) in tables {
        offsets.push(body_offset);
        body_offset += body.len();
    }

    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);

    for ((tag, body), off) in tables.iter().zip(offsets.iter()) {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes()); // checksum
        out.extend_from_slice(&(*off as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    }
    for (_, body) in tables {
        out.extend_from_slice(body);
    }
    out
}

#[test]
fn shape_empty_text_returns_no_glyphs() {
    let data = build_shapeable_font();
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let buffer = Buffer::new();

    let shaped = shape(&font, &buffer, &[]).unwrap();
    assert!(shaped.is_empty());
}

#[test]
fn shape_maps_chars_to_glyph_ids_and_advances() {
    let data = build_shapeable_font();
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    buffer.push_str("AB");

    let shaped = shape(&font, &buffer, &[]).unwrap();
    assert_eq!(shaped.len(), 2);
    assert_eq!(shaped.glyphs[0].glyph_id, 1); // 'A'
    assert_eq!(shaped.glyphs[0].x_advance, 500);
    assert_eq!(shaped.glyphs[0].cluster, 0);
    assert_eq!(shaped.glyphs[1].glyph_id, 2); // 'B'
    assert_eq!(shaped.glyphs[1].x_advance, 600);
    assert_eq!(shaped.glyphs[1].cluster, 1);
}

#[test]
fn unmappable_chars_fall_back_to_notdef_with_zero_advance() {
    let data = build_shapeable_font();
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    // 'Z' is not in the font's cmap.
    buffer.push_str("AZ");

    let shaped = shape(&font, &buffer, &[]).unwrap();
    assert_eq!(shaped.glyphs[0].glyph_id, 1);
    assert_eq!(shaped.glyphs[0].x_advance, 500);
    assert_eq!(shaped.glyphs[1].glyph_id, 0); // .notdef
    assert_eq!(shaped.glyphs[1].x_advance, 0);
}

#[test]
fn clusters_are_utf8_byte_offsets() {
    let data = build_shapeable_font();
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    // 'é' is two bytes in UTF-8, so the second glyph's cluster
    // skips from 0 past the two-byte character.
    buffer.push_str("éA");

    let shaped = shape(&font, &buffer, &[]).unwrap();
    assert_eq!(shaped.len(), 2);
    assert_eq!(shaped.glyphs[0].cluster, 0);
    assert_eq!(shaped.glyphs[1].cluster, 2);
}

#[test]
fn feature_slice_is_accepted_but_ignored_today() {
    let data = build_shapeable_font();
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    buffer.push_str("A");

    // Valid feature tag, non-zero value. Should parse fine and
    // not affect the output until M2.
    let features = [Feature {
        tag: *b"liga",
        value: 1,
    }];
    let shaped = shape(&font, &buffer, &features).unwrap();
    assert_eq!(shaped.len(), 1);
    assert_eq!(shaped.glyphs[0].glyph_id, 1);
}

#[test]
fn normalize_nfc_flag_collapses_decomposed_input() {
    // Test font has no cmap entry for 'e', combining acute, or
    // precomposed 'é', so every path ends up at .notdef. The
    // meaningful difference is glyph count: NFC off -> 2 glyphs
    // (e + combining acute both go to .notdef); NFC on -> 1 glyph
    // (the pair composes to 'é' before cmap).
    let data = build_shapeable_font();
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);

    let mut buffer = Buffer::new();
    buffer.push_str("e\u{0301}");
    let before = shape(&font, &buffer, &[]).unwrap();
    assert_eq!(before.len(), 2);

    buffer.set_normalize_nfc(true);
    let after = shape(&font, &buffer, &[]).unwrap();
    assert_eq!(after.len(), 1);
}

#[test]
fn resolve_extension_decodes_inner_offset() {
    // format=1, inner_type=2, inner_off=8, then payload "inner".
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&8u32.to_be_bytes());
    bytes.extend_from_slice(b"inner");
    let (inner_type, slice) = resolve_extension(&bytes).unwrap();
    assert_eq!(inner_type, 2);
    assert_eq!(&slice[..5], b"inner");
}

#[test]
fn resolve_extension_rejects_bad_format() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&9u16.to_be_bytes());
    bytes.extend_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    assert!(resolve_extension(&bytes).is_none());
}

#[test]
fn resolve_extension_rejects_short_header() {
    let bytes = [0u8; 4];
    assert!(resolve_extension(&bytes).is_none());
}

#[test]
fn resolve_extension_rejects_offset_past_end() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&2u16.to_be_bytes());
    bytes.extend_from_slice(&9999u32.to_be_bytes());
    assert!(resolve_extension(&bytes).is_none());
}
