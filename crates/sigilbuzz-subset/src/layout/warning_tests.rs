//! Malformed GSUB and GPOS pieces that the rewrite leaves out are
//! reported as warnings, located from the start of their table.
//!
//! Each test damages one structure of a real font's layout table,
//! subsets the font, and checks both that the subset still succeeds
//! and that the dropped piece is reported at the right offset.

use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::{subset, SubsetInput, SubsetWarning};

const OPEN_SANS: &[u8] = include_bytes!("../../../../tests/fixtures/opensans_regular.ttf");
const RUBIK: &[u8] = include_bytes!("../../../../tests/fixtures/rubik_vf.ttf");

fn u16_at(buf: &[u8], pos: usize) -> usize {
    usize::from(u16::from_be_bytes([buf[pos], buf[pos + 1]]))
}

fn put_u16(buf: &mut [u8], pos: usize, v: u16) {
    buf[pos..pos + 2].copy_from_slice(&v.to_be_bytes());
}

/// `font` with its `table` replaced by `edit` applied to a copy.
fn with_edited(font: &[u8], table: [u8; 4], edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let face = Face::parse_bytes(font, 0).unwrap();
    let mut tables: Vec<([u8; 4], Vec<u8>)> = face
        .records()
        .iter()
        .map(|rec| (rec.tag, face.table_bytes(rec.tag).unwrap().to_vec()))
        .collect();
    let (_, bytes) = tables.iter_mut().find(|(t, _)| *t == table).unwrap();
    edit(bytes);
    crate::sfnt::build(face.sfnt_version(), &tables)
}

/// Subsets `font` to a handful of Latin glyphs and returns the warnings.
fn warnings_of(font: &[u8]) -> Vec<SubsetWarning> {
    let face = Face::parse_bytes(font, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let gids = "AVfiTo".chars().filter_map(|c| cmap.glyph_id(c)).collect();
    let input = SubsetInput {
        gids,
        ..SubsetInput::default()
    };
    subset(&face, &input).expect("the subset succeeds").warnings
}

/// Position of lookup `li` in the GSUB or GPOS table `table`.
fn lookup_at(table: &[u8], li: usize) -> usize {
    let list = u16_at(table, 8);
    list + u16_at(table, list + 2 + li * 2)
}

#[test]
fn well_formed_fonts_raise_no_warnings() {
    assert!(warnings_of(OPEN_SANS).is_empty());
    assert!(warnings_of(RUBIK).is_empty());
}

#[test]
fn a_subtable_offset_past_the_table_is_reported_at_its_slot() {
    let mut slot = 0;
    let font = with_edited(OPEN_SANS, tag::GSUB, |gsub| {
        slot = lookup_at(gsub, 0) + 6;
        put_u16(gsub, slot, 0xFFF0);
    });
    let warnings = warnings_of(&font);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        (warnings[0].table, warnings[0].offset, warnings[0].dropped),
        (tag::GSUB, slot, "a lookup subtable")
    );
}

#[test]
fn a_subtable_the_parser_rejects_is_reported_at_its_start() {
    // Rubik's first GPOS lookup gets a Coverage format no shaper reads
    // in its first subtable. The subset drops the subtable and says where.
    let mut sub = 0;
    let font = with_edited(RUBIK, tag::GPOS, |gpos| {
        sub = lookup_at(gpos, 0) + u16_at(gpos, lookup_at(gpos, 0) + 6);
        let coverage = sub + u16_at(gpos, sub + 2);
        put_u16(gpos, coverage, 7);
    });
    let warnings = warnings_of(&font);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        (warnings[0].table, warnings[0].offset, warnings[0].dropped),
        (tag::GPOS, sub, "a lookup subtable")
    );
}

#[test]
fn an_unreadable_language_system_is_reported() {
    // Point the first script's default LangSys past the table.
    let mut slot = 0;
    let font = with_edited(OPEN_SANS, tag::GSUB, |gsub| {
        let scripts = u16_at(gsub, 4);
        let script = scripts + u16_at(gsub, scripts + 6);
        slot = script;
        put_u16(gsub, slot, 0xFFF0);
    });
    let warnings = warnings_of(&font);
    let found: Vec<_> = warnings
        .iter()
        .map(|w| (w.table, w.offset, w.dropped))
        .collect();
    assert_eq!(found, vec![(tag::GSUB, slot, "a language system")]);
}
