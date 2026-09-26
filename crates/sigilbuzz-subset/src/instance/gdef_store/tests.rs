//! Full instancing takes the GDEF ItemVariationStore out by rebuilding
//! the table, so tables laid out after the store survive.

use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::tables::gdef::{Gdef, GlyphClass};
use sigilbuzz::tables::layout::Coverage;
use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use super::{prune_gdef_store, GdefBake};
use crate::coverage::emit_coverage_from_glyphs;
use crate::{instance, InstanceInput};

const RUBIK: &[u8] = include_bytes!("../../../../../tests/fixtures/rubik_vf.ttf");
const OPEN_SANS: &[u8] = include_bytes!("../../../../../tests/fixtures/opensans_regular.ttf");

fn u16_at(buf: &[u8], pos: usize) -> usize {
    usize::from(u16::from_be_bytes([buf[pos], buf[pos + 1]]))
}

fn put(buf: &mut [u8], pos: usize, value: usize) {
    let value = u16::try_from(value).unwrap();
    buf[pos..pos + 2].copy_from_slice(&value.to_be_bytes());
}

fn push(buf: &mut Vec<u8>, values: &[u16]) {
    for v in values {
        buf.extend_from_slice(&v.to_be_bytes());
    }
}

/// One region peaking at the top of the axis, one item with `delta`.
fn store(delta: i16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&12u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&22u32.to_be_bytes());
    push(&mut out, &[1, 1, 0, 0x4000, 0x4000, 1, 1, 1, 0]);
    out.extend_from_slice(&delta.to_be_bytes());
    out
}

/// A GDEF 1.3 whose header-level tables all sit before the store but
/// whose LigCaretList Coverage and mark glyph set Coverage are placed
/// after it. Glyph 10 is a base, 11 a mark, 12 a ligature with one
/// format 3 caret at 100 varied by +40 at the top of the axis.
fn gdef_with_tables_after_the_store() -> Vec<u8> {
    let mut out = vec![0u8; 18];
    put(&mut out, 0, 1);
    put(&mut out, 2, 3);
    let at = out.len();
    put(&mut out, 4, at);
    out.extend_from_slice(&crate::classdef::emit_classdef(&[
        (10, 1),
        (11, 3),
        (12, 2),
    ]));
    // LigCaretList: Coverage offset patched below, one LigGlyph.
    let list = out.len();
    put(&mut out, 8, list);
    push(&mut out, &[0, 1, 6]);
    push(&mut out, &[1, 4]); // LigGlyph: one caret, 4 bytes on
    push(&mut out, &[3, 100, 6]); // CaretValue format 3, device 6 on
    push(&mut out, &[0, 0, 0x8000]); // VariationIndex (0, 0)
                                     // MarkGlyphSetsDef: one set, Offset32 patched below.
    let sets = out.len();
    put(&mut out, 12, sets);
    push(&mut out, &[1, 1, 0, 0]);
    let at = out.len() as u32;
    out[14..18].copy_from_slice(&at.to_be_bytes());
    out.extend_from_slice(&store(40));
    // The nested Coverages, after the store.
    let cov = out.len();
    put(&mut out, list, cov - list);
    out.extend_from_slice(&emit_coverage_from_glyphs(&[12]));
    let cov = out.len() as u32 - sets as u32;
    out[sets + 4..sets + 8].copy_from_slice(&cov.to_be_bytes());
    out.extend_from_slice(&emit_coverage_from_glyphs(&[10, 11]));
    out
}

/// Rubik with its GDEF replaced by `gdef`, or removed for `None`.
fn rubik_with_gdef(gdef: Option<&[u8]>) -> Vec<u8> {
    let rubik = Face::parse_bytes(RUBIK, 0).unwrap();
    let tables: Vec<([u8; 4], Vec<u8>)> = rubik
        .records()
        .iter()
        .filter_map(|rec| match (rec.tag, gdef) {
            (tag::GDEF, Some(gdef)) => Some((rec.tag, gdef.to_vec())),
            (tag::GDEF, None) => None,
            _ => Some((rec.tag, rubik.table_bytes(rec.tag).unwrap().to_vec())),
        })
        .collect();
    crate::sfnt::build(rubik.sfnt_version(), &tables)
}

fn instance_at_top(font: &[u8]) -> Vec<u8> {
    let face = Face::parse_bytes(font, 0).unwrap();
    let input = InstanceInput {
        coords: vec![1.0],
        drop_var_tables: true,
        axis_pins: Vec::new(),
    };
    instance(&face, &input).unwrap().bytes
}

#[test]
fn a_gdef_without_a_store_rides_through() {
    // Open Sans carries GDEF 1.0.
    let face = Face::parse_bytes(OPEN_SANS, 0).unwrap();
    assert!(matches!(
        prune_gdef_store(&face, &[]).unwrap(),
        GdefBake::Unchanged
    ));
}

#[test]
fn tables_nested_after_the_store_survive_the_prune() {
    let source = gdef_with_tables_after_the_store();
    let out = instance_at_top(&rubik_with_gdef(Some(&source)));
    let face = Face::parse_bytes(&out, 0).unwrap();
    let gdef = face.table_bytes(tag::GDEF).unwrap();
    let parsed = Gdef::parse(gdef).unwrap();
    assert!(parsed.item_variation_store().is_none());
    assert!(u16_at(gdef, 2) < 3, "no store slot left");
    assert_eq!(parsed.glyph_class(10), GlyphClass::Base);
    assert_eq!(parsed.glyph_class(11), GlyphClass::Mark);
    assert_eq!(parsed.glyph_class(12), GlyphClass::Ligature);
    let set = parsed.mark_filtering_set(0).expect("mark set kept");
    assert!(set.index_of(10).is_some() && set.index_of(11).is_some());
    // The caret list's Coverage and the folded caret: 100 + 40.
    let list = u16_at(gdef, 8);
    let cov = Coverage::parse(&gdef[list + u16_at(gdef, list)..]).unwrap();
    let index = usize::from(cov.index_of(12).expect("ligature covered"));
    let lig = list + u16_at(gdef, list + 4 + index * 2);
    assert_eq!(u16_at(gdef, lig), 1);
    let caret = lig + u16_at(gdef, lig + 2);
    assert_eq!(u16_at(gdef, caret + 2), 140);
}

#[test]
fn a_gdef_holding_only_a_store_is_dropped() {
    let mut gdef = vec![0u8; 18];
    put(&mut gdef, 0, 1);
    put(&mut gdef, 2, 3);
    gdef[14..18].copy_from_slice(&18u32.to_be_bytes());
    gdef.extend_from_slice(&store(40));
    let out = instance_at_top(&rubik_with_gdef(Some(&gdef)));
    let face = Face::parse_bytes(&out, 0).unwrap();
    assert!(face.record(tag::GDEF).is_none());
}
