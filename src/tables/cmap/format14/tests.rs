//! Format 14 parsing and lookups on hand-built subtables.

use alloc::vec;
use alloc::vec::Vec;

use super::{build_format14, Format14, GlyphVariant};
use crate::error::Error;
use crate::tables::cmap::{build_cmap_wrapper, build_format12, Cmap};

const VS1: u32 = 0xFE00;
const VS2: u32 = 0xFE01;
const VS17: u32 = 0xE0100;

/// A cmap mapping 'A'..='Z' to glyphs 1..=26 (format 12 under (3, 10))
/// with `uvs` under (0, 5).
fn cmap_bytes(uvs: Vec<u8>) -> Vec<u8> {
    build_cmap_wrapper(&[(0, 5, uvs), (3, 10, build_format12(&[(0x41, 0x5A, 1)]))])
}

fn sample() -> Vec<u8> {
    build_format14(&[
        // VS1: 'A'..='C' default, 'D' -> 100, 'E' -> glyph 0.
        (VS1, &[(0x41, 2)], &[(0x44, 100), (0x45, 0)]),
        // VS2: only its own glyphs.
        (VS2, &[], &[(0x41, 200), (0x5A, 201)]),
        // VS17: 'X' default (the font maps it), U+4E00 default (it does not).
        (VS17, &[(0x58, 0), (0x4E00, 0)], &[]),
    ])
}

#[test]
fn parses_records_and_resolves_each_kind() {
    let data = sample();
    let uvs = Format14::parse(&data).unwrap();
    assert_eq!(uvs.glyph_variant(0x41, VS1), GlyphVariant::UseDefault);
    assert_eq!(uvs.glyph_variant(0x43, VS1), GlyphVariant::UseDefault);
    assert_eq!(uvs.glyph_variant(0x44, VS1), GlyphVariant::Found(100));
    assert_eq!(uvs.glyph_variant(0x41, VS2), GlyphVariant::Found(200));
    assert_eq!(uvs.glyph_variant(0x5A, VS2), GlyphVariant::Found(201));
    assert_eq!(uvs.glyph_variant(0x42, VS2), GlyphVariant::NotFound);
}

#[test]
fn a_mapping_to_glyph_zero_is_not_found() {
    let data = sample();
    let uvs = Format14::parse(&data).unwrap();
    assert_eq!(uvs.glyph_variant(0x45, VS1), GlyphVariant::NotFound);
}

#[test]
fn default_ranges_cover_start_through_start_plus_count() {
    let data = sample();
    let uvs = Format14::parse(&data).unwrap();
    assert_eq!(uvs.glyph_variant(0x40, VS1), GlyphVariant::NotFound);
    assert_eq!(uvs.glyph_variant(0x41, VS1), GlyphVariant::UseDefault);
    assert_eq!(uvs.glyph_variant(0x43, VS1), GlyphVariant::UseDefault);
    // 0x44 is past the range, so the non-default table answers.
    assert_eq!(uvs.glyph_variant(0x44, VS1), GlyphVariant::Found(100));
}

#[test]
fn an_unknown_selector_is_not_found() {
    let data = sample();
    let uvs = Format14::parse(&data).unwrap();
    assert_eq!(uvs.glyph_variant(0x41, 0xFE02), GlyphVariant::NotFound);
    assert_eq!(uvs.glyph_variant(0x41, 0), GlyphVariant::NotFound);
}

#[test]
fn cmap_variation_glyph_falls_back_to_the_base_glyph_for_default_sequences() {
    let bytes = cmap_bytes(sample());
    let cmap = Cmap::parse(&bytes).unwrap();
    assert_eq!(cmap.variation_glyph('A', '\u{FE00}'), Some(1));
    assert_eq!(cmap.variation_glyph('D', '\u{FE00}'), Some(100));
    assert_eq!(cmap.variation_glyph('X', '\u{E0100}'), Some(24));
    // A default sequence whose base the font does not map has no glyph.
    assert_eq!(cmap.variation_glyph('\u{4E00}', '\u{E0100}'), None);
    // A sequence the font does not list has no glyph, even though the
    // base has one.
    assert_eq!(cmap.variation_glyph('B', '\u{FE01}'), None);
    assert_eq!(cmap.glyph_id('B'), Some(2));
}

#[test]
fn a_cmap_without_format_14_has_no_variation_glyphs() {
    let bytes = build_cmap_wrapper(&[(3, 10, build_format12(&[(0x41, 0x5A, 1)]))]);
    let cmap = Cmap::parse(&bytes).unwrap();
    assert_eq!(cmap.variation_glyph('A', '\u{FE00}'), None);
    assert!(cmap.variation_selectors().is_empty());
    assert!(cmap.variation_unicodes(VS1).is_empty());
}

#[test]
fn collects_selectors_and_unicodes() {
    let bytes = cmap_bytes(sample());
    let cmap = Cmap::parse(&bytes).unwrap();
    assert_eq!(cmap.variation_selectors(), [VS1, VS2, VS17]);
    // Default and non-default entries, glyph 0 mappings included.
    assert_eq!(cmap.variation_unicodes(VS1), [0x41, 0x42, 0x43, 0x44, 0x45]);
    assert_eq!(cmap.variation_unicodes(VS2), [0x41, 0x5A]);
    assert_eq!(cmap.variation_unicodes(VS17), [0x58, 0x4E00]);
    assert!(cmap.variation_unicodes(0xFE0F).is_empty());
}

#[test]
fn collected_unicodes_merge_overlapping_ranges_and_stop_at_the_unicode_maximum() {
    let data = build_format14(&[(
        VS1,
        &[(0x10, 3), (0x12, 4), (0x10_FFFE, 255), (0x11_0000, 5)],
        &[(0x11, 1), (0x20_0000, 2)],
    )]);
    let uvs = Format14::parse(&data).unwrap();
    let mut want: Vec<u32> = (0x10..=0x16).collect();
    want.extend([0x10_FFFE, 0x10_FFFF, 0x20_0000]);
    assert_eq!(uvs.unicodes(VS1), want);
}

#[test]
fn selectors_are_sorted_without_repeats() {
    let data = build_format14(&[
        (VS2, &[], &[(0x41, 1)]),
        (VS1, &[], &[(0x41, 2)]),
        (VS2, &[], &[(0x42, 3)]),
    ]);
    let uvs = Format14::parse(&data).unwrap();
    assert_eq!(uvs.selectors(), [VS1, VS2]);
}

#[test]
fn rejects_a_truncated_header_or_record_array() {
    let data = sample();
    assert!(matches!(
        Format14::parse(&data[..9]),
        Err(Error::Truncated { .. })
    ));
    // The header says three records. Cut into the third.
    assert!(matches!(
        Format14::parse(&data[..10 + 2 * 11 + 5]),
        Err(Error::Truncated { .. })
    ));
    let mut wrong = data.clone();
    wrong[0..2].copy_from_slice(&4u16.to_be_bytes());
    assert!(matches!(
        Format14::parse(&wrong),
        Err(Error::Malformed { .. })
    ));
}

#[test]
fn a_bad_format_14_subtable_leaves_the_cmap_usable() {
    // A record count far past the data: HarfBuzz drops the subtable.
    let mut bad = sample();
    bad[6..10].copy_from_slice(&u32::MAX.to_be_bytes());
    let bytes = cmap_bytes(bad);
    let cmap = Cmap::parse(&bytes).unwrap();
    assert_eq!(cmap.glyph_id('A'), Some(1));
    assert_eq!(cmap.variation_glyph('D', '\u{FE00}'), None);
    assert!(cmap.variation_selectors().is_empty());

    // A (0, 5) record that points at a format 4 or past the table.
    let f12 = build_format12(&[(0x41, 0x5A, 1)]);
    let bytes = build_cmap_wrapper(&[(0, 5, f12.clone()), (3, 10, f12)]);
    let cmap = Cmap::parse(&bytes).unwrap();
    assert_eq!(cmap.variation_glyph('A', '\u{FE00}'), None);
    let mut bytes = cmap_bytes(sample());
    bytes[8..12].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
    let cmap = Cmap::parse(&bytes).unwrap();
    assert!(cmap.variation_selectors().is_empty());
}

#[test]
fn a_uvs_table_that_does_not_fit_reads_as_empty() {
    let data = sample();
    // Point VS1's default table past the end: 'A' is no longer a
    // default sequence, and its non-default table still works.
    let mut moved = data.clone();
    let record = 10 + 3;
    moved[record..record + 4].copy_from_slice(&(data.len() as u32).to_be_bytes());
    let uvs = Format14::parse(&moved).unwrap();
    assert_eq!(uvs.glyph_variant(0x41, VS1), GlyphVariant::NotFound);
    assert_eq!(uvs.glyph_variant(0x44, VS1), GlyphVariant::Found(100));

    // Give VS2's non-default table a count past the end.
    let mut long = data.clone();
    let offset = u32::from_be_bytes([
        long[10 + 11 + 7],
        long[10 + 11 + 8],
        long[10 + 11 + 9],
        long[10 + 11 + 10],
    ]) as usize;
    long[offset..offset + 4].copy_from_slice(&1000u32.to_be_bytes());
    let uvs = Format14::parse(&long).unwrap();
    assert_eq!(uvs.glyph_variant(0x41, VS2), GlyphVariant::NotFound);
    assert!(uvs.unicodes(VS2).is_empty());
}

#[test]
fn an_unsorted_record_array_probes_like_harfbuzz() {
    // HarfBuzz's binary search looks at the middle record first
    // (index 1 of 3), then moves by the comparison. VS1 and VS2 both
    // sort before the middle VS17, so both searches go left to the
    // first record. VS2 is there. VS1 sorts before it too and is
    // missed, though the last record holds it.
    let data = build_format14(&[
        (VS2, &[], &[(0x41, 7)]),
        (VS17, &[], &[(0x41, 8)]),
        (VS1, &[], &[(0x41, 9)]),
    ]);
    let uvs = Format14::parse(&data).unwrap();
    assert_eq!(uvs.glyph_variant(0x41, VS17), GlyphVariant::Found(8));
    assert_eq!(uvs.glyph_variant(0x41, VS2), GlyphVariant::Found(7));
    assert_eq!(uvs.glyph_variant(0x41, VS1), GlyphVariant::NotFound);
}

#[test]
fn every_prefix_of_a_subtable_parses_or_fails_cleanly() {
    let data = sample();
    for len in 0..=data.len() {
        let bytes = cmap_bytes(data[..len].to_vec());
        let cmap = Cmap::parse(&bytes).unwrap();
        for selector in ['\u{FE00}', '\u{FE01}', '\u{E0100}'] {
            let _ = cmap.variation_glyph('A', selector);
            let _ = cmap.variation_unicodes(selector as u32);
        }
        let _ = cmap.variation_selectors();
    }
    let empty: Vec<u8> = vec![];
    assert!(Format14::parse(&empty).is_err());
}
