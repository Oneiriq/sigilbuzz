//! Partial instancing renumbers the GDEF ItemVariationStore; every
//! VariationIndex in GDEF and GPOS must follow.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use rustybuzz::ttf_parser::Tag;
use rustybuzz::{Face as RbFace, UnicodeBuffer, Variation};
use sigilbuzz::tables::layout::Coverage;
use sigilbuzz::tables::tag;
use sigilbuzz::tables::variation_store::ItemVariationStore;
use sigilbuzz::Face;

use super::super::gdef_store::GdefBake;
use super::super::AxisPin;
use super::{bake_gdef_bytes_partial, remap_gpos_variation_indices, StoreRemap};
use crate::gpos_var::{walk_gpos_device_slots, VARIATION_INDEX_DELTA_FORMAT};
use crate::layout::GidMap;
use crate::warnings::Warnings;
use crate::{instance, InstanceInput};

const RUBIK: &[u8] = include_bytes!("../../../../../tests/fixtures/rubik_vf.ttf");

fn u16_at(buf: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([buf[pos], buf[pos + 1]])
}

fn u32_at(buf: &[u8], pos: usize) -> usize {
    u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize
}

fn put(buf: &mut [u8], pos: usize, value: u16) {
    buf[pos..pos + 2].copy_from_slice(&value.to_be_bytes());
}

fn push(buf: &mut Vec<u8>, values: &[u16]) {
    for v in values {
        buf.extend_from_slice(&v.to_be_bytes());
    }
}

/// A two-axis store. Subtable 0 varies only along axis 0 and so
/// collapses once axis 0 is pinned at its default; subtable 1 varies
/// along axis 1 with deltas 50 and 70 and survives as subtable 0.
fn two_axis_store() -> Vec<u8> {
    let mut out = Vec::new();
    push(&mut out, &[1, 0, 16, 2, 0, 44, 0, 54]);
    // Region list at 16: two axes, two regions.
    push(&mut out, &[2, 2]);
    push(&mut out, &[0, 0x4000, 0x4000, 0, 0, 0]);
    push(&mut out, &[0, 0, 0, 0, 0x4000, 0x4000]);
    // Subtable 0 at 44: one item on region 0.
    push(&mut out, &[1, 1, 1, 0, 30]);
    // Subtable 1 at 54: two items on region 1.
    push(&mut out, &[2, 1, 1, 1, 50, 70]);
    out
}

/// A GDEF 1.3 with a LigCaretList for ligature 5: carets naming rows
/// (0, 0) and (1, 1) of [`two_axis_store`].
fn gdef_with_varied_carets() -> Vec<u8> {
    let mut out = vec![0u8; 18];
    put(&mut out, 0, 1);
    put(&mut out, 2, 3);
    put(&mut out, 8, 18);
    // LigCaretList at 18: Coverage at +6+... patched, one LigGlyph at 6.
    push(&mut out, &[0, 1, 6]);
    push(&mut out, &[2, 6, 18]); // LigGlyph: two carets
    push(&mut out, &[3, 100, 6, 0, 0, 0x8000]); // caret row (0, 0)
    push(&mut out, &[3, 200, 6, 1, 1, 0x8000]); // caret row (1, 1)
    let cov = out.len() - 18;
    put(&mut out, 18, cov as u16);
    out.extend_from_slice(&crate::coverage::emit_coverage_from_glyphs(&[5]));
    let at = out.len() as u32;
    out[14..18].copy_from_slice(&at.to_be_bytes());
    out.extend_from_slice(&two_axis_store());
    out
}

/// `(coordinate, VariationIndex row)` of each caret of the ligature
/// the LigCaretList covers first.
fn carets(gdef: &[u8]) -> Vec<(u16, Option<(u16, u16)>)> {
    let list = usize::from(u16_at(gdef, 8));
    let lig = list + usize::from(u16_at(gdef, list + 4));
    (0..usize::from(u16_at(gdef, lig)))
        .map(|k| {
            let caret = lig + usize::from(u16_at(gdef, lig + 2 + k * 2));
            let dev = usize::from(u16_at(gdef, caret + 4));
            let row =
                (dev != 0).then(|| (u16_at(gdef, caret + dev), u16_at(gdef, caret + dev + 2)));
            (u16_at(gdef, caret + 2), row)
        })
        .collect()
}

#[test]
fn caret_rows_follow_the_projected_store() {
    let gdef = gdef_with_varied_carets();
    let map = GidMap::from_kept(&[0, 1, 2, 3, 4, 5]);
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let (bake, remap) =
        bake_gdef_bytes_partial(&gdef, &map, &[0.0, 0.0], &pins, &Warnings::default()).unwrap();
    let GdefBake::Rebuilt(out) = bake else {
        panic!("expected a rebuilt GDEF");
    };
    let remap = remap.expect("a store remap");
    assert_eq!(remap.lookup(0, 0), None, "subtable 0 collapsed");
    assert_eq!(remap.lookup(1, 1), Some((0, 1)), "subtable 1 moved down");
    // Row (0, 0) is gone: its caret keeps its coordinate, no table.
    // Row (1, 1) is now (0, 1).
    assert_eq!(carets(&out), vec![(100, None), (200, Some((0, 1)))]);
    let store = ItemVariationStore::parse(&out[u32_at(&out, 14)..]).unwrap();
    assert_eq!(store.subtable_count(), 1);
    assert_eq!(store.delta(0, 1, &[0.5]), 35.0);
    // The LigCaretList's Coverage made it through the rebuild.
    let list = usize::from(u16_at(&out, 8));
    let cov = Coverage::parse(&out[list + usize::from(u16_at(&out, list))..]).unwrap();
    assert_eq!(cov.index_of(5), Some(0));
}

/// A lone SinglePos format 1 wrapped in a GPOS, its ValueRecord
/// placement and advance both naming `tables` (VariationIndex rows;
/// equal rows share one table).
fn gpos_with_rows(rows: &[(u16, u16)]) -> Vec<u8> {
    let mut sub = Vec::new();
    push(&mut sub, &[1, 0, 0x0055, 0, 0, 0, 0]);
    // Coverage right after the 14-byte subtable.
    put(&mut sub, 2, 14);
    sub.extend_from_slice(&crate::coverage::emit_coverage_from_glyphs(&[3]));
    let mut placed: Vec<((u16, u16), usize)> = Vec::new();
    for (slot, &row) in [10usize, 12].iter().zip(rows) {
        let at = match placed.iter().find(|p| p.0 == row) {
            Some(p) => p.1,
            None => {
                let at = sub.len();
                push(&mut sub, &[row.0, row.1, VARIATION_INDEX_DELTA_FORMAT]);
                placed.push((row, at));
                at
            }
        };
        put(&mut sub, *slot, at as u16);
    }
    let mut gpos = Vec::new();
    push(&mut gpos, &[1, 0, 0, 0, 10, 1, 4, 1, 0, 1, 8]);
    gpos.extend_from_slice(&sub);
    gpos
}

/// The VariationIndex rows the GPOS device slots reach, in walk order
/// (`None` for a cleared slot).
fn gpos_rows(gpos: &[u8]) -> Vec<Option<(u16, u16)>> {
    let mut gpos = gpos.to_vec();
    let mut rows = Vec::new();
    walk_gpos_device_slots(&mut gpos, &mut |buf, slot| {
        rows.push(
            slot.target(buf)
                .map(|t| (u16_at(buf, t), u16_at(buf, t + 2))),
        );
    });
    rows
}

#[test]
fn gpos_rows_are_renumbered_once_and_gone_rows_cleared() {
    let remap = |rows: &[(u16, u16)]| {
        let gdef = gdef_with_varied_carets();
        let map = GidMap::from_kept(&[0]);
        let pins = [AxisPin::Pin, AxisPin::Keep];
        let (_, remap) =
            bake_gdef_bytes_partial(&gdef, &map, &[0.0, 0.0], &pins, &Warnings::default()).unwrap();
        let mut gpos = gpos_with_rows(rows);
        remap_gpos_variation_indices(&mut gpos, &remap.unwrap());
        gpos_rows(&gpos)
    };
    // Two slots sharing row (1, 1): renumbered once, not twice.
    assert_eq!(remap(&[(1, 1), (1, 1)]), vec![Some((0, 1)), Some((0, 1))]);
    // One gone row, one moved row.
    assert_eq!(remap(&[(0, 0), (1, 0)]), vec![None, Some((0, 0))]);
    // Sharing a gone row: both slots clear.
    assert_eq!(remap(&[(0, 0), (0, 0)]), vec![None, None]);
    // A store the projection cannot read clears every row.
    let mut gpos = gpos_with_rows(&[(1, 1), (0, 0)]);
    remap_gpos_variation_indices(&mut gpos, &StoreRemap::Cleared);
    assert_eq!(gpos_rows(&gpos), vec![None, None]);
}

/// A lookup of two SinglePos subtables whose xAdvance device slots
/// share one VariationIndex table (row (1, 1)) placed after both.
fn gpos_sharing_a_table_between_subtables() -> Vec<u8> {
    let mut gpos = Vec::new();
    // Header, LookupList at 10, Lookup at 14 with subtables at 24, 40.
    push(&mut gpos, &[1, 0, 0, 0, 10, 1, 4, 1, 0, 2, 10, 26]);
    // SinglePos format 1, valueFormat xAdvance | xAdvDevice, then its
    // Coverage; the shared table sits at 56.
    push(&mut gpos, &[1, 10, 0x0044, 0, 56 - 24, 1, 1, 3]);
    push(&mut gpos, &[1, 10, 0x0044, 0, 56 - 40, 1, 1, 4]);
    push(&mut gpos, &[1, 1, VARIATION_INDEX_DELTA_FORMAT]);
    gpos
}

#[test]
fn a_table_shared_between_subtables_is_renumbered_once() {
    let gdef = gdef_with_varied_carets();
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let (_, remap) = bake_gdef_bytes_partial(
        &gdef,
        &GidMap::from_kept(&[0]),
        &[0.0, 0.0],
        &pins,
        &Warnings::default(),
    )
    .unwrap();
    let mut gpos = gpos_sharing_a_table_between_subtables();
    assert_eq!(gpos_rows(&gpos), vec![Some((1, 1)), Some((1, 1))]);
    remap_gpos_variation_indices(&mut gpos, &remap.unwrap());
    assert_eq!(gpos_rows(&gpos), vec![Some((0, 1)), Some((0, 1))]);
}

/// Rubik, with an ItemVariationData that has rows but no regions put in
/// front of its GDEF store. Every VariationIndex in GDEF and GPOS moves
/// up one subtable to keep naming the same deltas, except every fifth
/// distinct GPOS table, which is pointed at the new subtable and so no
/// longer varies. The font shapes like Rubik apart from those.
///
/// Partial instancing elides the empty subtable, which moves every
/// other one down: exactly the renumbering the VariationIndex tables
/// have to follow. Rubik alone has one axis and nothing to pin, so it
/// never collapses a subtable on its own.
fn rubik_with_leading_empty_subtable() -> Vec<u8> {
    let face = Face::parse_bytes(RUBIK, 0).unwrap();
    let gdef = face.table_bytes(tag::GDEF).unwrap();
    let store_off = u32_at(gdef, 14);
    let store = &gdef[store_off..];
    let regions = u32_at(store, 2);
    let count = usize::from(u16_at(store, 6));
    let offsets: Vec<usize> = (0..count).map(|i| u32_at(store, 8 + i * 4)).collect();
    assert!(offsets.windows(2).all(|w| w[0] < w[1]) && regions < offsets[0]);

    let mut new_store = Vec::new();
    push(&mut new_store, &[1, 0, 0, count as u16 + 1]);
    new_store.resize(8 + (count + 1) * 4, 0);
    let region_at = new_store.len();
    new_store[2..6].copy_from_slice(&(region_at as u32).to_be_bytes());
    new_store.extend_from_slice(&store[regions..offsets[0]]);
    let empty_at = new_store.len() as u32;
    new_store[8..12].copy_from_slice(&empty_at.to_be_bytes());
    push(&mut new_store, &[4, 0, 0]);
    for (i, &off) in offsets.iter().enumerate() {
        let end = offsets.get(i + 1).copied().unwrap_or(store.len());
        let at = new_store.len() as u32;
        new_store[12 + i * 4..16 + i * 4].copy_from_slice(&at.to_be_bytes());
        new_store.extend_from_slice(&store[off..end]);
    }
    let mut new_gdef = gdef[..store_off].to_vec();
    new_gdef.extend_from_slice(&new_store);

    // Caret VariationIndex tables move up one subtable.
    let list = usize::from(u16_at(&new_gdef, 8));
    let mut seen = BTreeSet::new();
    for i in 0..usize::from(u16_at(&new_gdef, list + 2)) {
        let lig = list + usize::from(u16_at(&new_gdef, list + 4 + i * 2));
        for k in 0..usize::from(u16_at(&new_gdef, lig)) {
            let caret = lig + usize::from(u16_at(&new_gdef, lig + 2 + k * 2));
            let dev = usize::from(u16_at(&new_gdef, caret + 4));
            if u16_at(&new_gdef, caret) != 3 || dev == 0 {
                continue;
            }
            let table = caret + dev;
            if u16_at(&new_gdef, table + 4) == VARIATION_INDEX_DELTA_FORMAT && seen.insert(table) {
                let outer = u16_at(&new_gdef, table);
                put(&mut new_gdef, table, outer + 1);
            }
        }
    }
    assert!(!seen.is_empty(), "Rubik's carets vary");

    // GPOS VariationIndex tables: up one subtable, or onto the empty one.
    let mut gpos = face.table_bytes(tag::GPOS).unwrap().to_vec();
    // Tables are told apart by their position in the whole GPOS: Rubik
    // shares VariationIndex tables between subtables, and the walk
    // hands each subtable its own slice.
    let mut tables = BTreeSet::new();
    let origin = gpos.as_ptr() as usize;
    walk_gpos_device_slots(&mut gpos, &mut |buf, slot| {
        if slot.delta_format(buf) != Some(VARIATION_INDEX_DELTA_FORMAT) {
            return;
        }
        let Some(table) = slot.target(buf) else {
            return;
        };
        if tables.insert(buf.as_ptr() as usize - origin + table) {
            let k = tables.len() as u16;
            let row = if k % 5 == 0 {
                (0, k % 4)
            } else {
                (u16_at(buf, table) + 1, u16_at(buf, table + 2))
            };
            put(buf, table, row.0);
            put(buf, table + 2, row.1);
        }
    });
    assert!(tables.len() > 20, "Rubik's GPOS varies");

    let out: Vec<([u8; 4], Vec<u8>)> = face
        .records()
        .iter()
        .map(|rec| match rec.tag {
            tag::GDEF => (rec.tag, new_gdef.clone()),
            tag::GPOS => (rec.tag, gpos.clone()),
            _ => (rec.tag, face.table_bytes(rec.tag).unwrap().to_vec()),
        })
        .collect();
    crate::sfnt::build(face.sfnt_version(), &out)
}

/// Marks on bases, stacked marks, and kerning pairs.
const CORPUS: &[&str] = &[
    "q\u{301}",
    "x\u{303}",
    "b\u{308}",
    "v\u{300}\u{301}",
    "q\u{308}\u{304}",
    "X\u{302}\u{303}",
    "AVATAWAY",
    "To Ty Va Wa Ya",
    "LTLVFAPA",
];

fn shape(bytes: &[u8], wght: f32, text: &str) -> Vec<(u32, i32, i32, i32)> {
    let mut face = RbFace::from_slice(bytes, 0).expect("rustybuzz parses");
    face.set_variations(&[Variation {
        tag: Tag::from_bytes(b"wght"),
        value: wght,
    }]);
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    let out = rustybuzz::shape(&face, &[], buffer);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance, p.x_offset, p.y_offset))
        .collect()
}

#[test]
fn partial_instance_keeps_variation_indices_in_step_with_the_store() {
    let source = rubik_with_leading_empty_subtable();
    let face = Face::parse_bytes(&source, 0).unwrap();
    let input = InstanceInput {
        coords: vec![0.0],
        drop_var_tables: false,
        axis_pins: vec![AxisPin::Keep],
    };
    let out = instance(&face, &input).unwrap().bytes;

    // The empty subtable is gone, so the renumbering did happen.
    let count = |font: &[u8]| {
        let face = Face::parse_bytes(font, 0).unwrap();
        let gdef = face.gdef().unwrap().unwrap();
        gdef.item_variation_store().unwrap().subtable_count()
    };
    assert_eq!(count(&out), count(&source) - 1);

    let mut mismatches: Vec<String> = Vec::new();
    for wght in [300.0f32, 450.0, 650.0, 900.0] {
        for text in CORPUS {
            let expected = shape(&source, wght, text);
            let got = shape(&out, wght, text);
            if expected != got {
                mismatches.push(alloc::format!(
                    "wght={wght} {text:?}:\n  source   {expected:?}\n  instance {got:?}"
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// Guards against a vacuous pass: the corpus must move between the
/// light and heavy ends of the axis in the modified source.
#[test]
fn the_corpus_varies_with_weight() {
    let source = rubik_with_leading_empty_subtable();
    assert!(CORPUS
        .iter()
        .any(|text| shape(&source, 300.0, text) != shape(&source, 900.0, text)));
}
