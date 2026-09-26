//! Hand-built GDEF fixtures for the rewriter: one per subtable, the
//! version and drop rules, and truncated or malformed input.
//!
//! Outputs are read back with the core crate's GDEF parser where it
//! covers the subtable (class defs, mark glyph sets, the variation
//! store) and with the small readers below for AttachList and
//! LigCaretList, which the core parser does not expose.

use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::tables::gdef::Gdef;
use sigilbuzz::tables::layout::{ClassDef, Coverage};
use sigilbuzz::tables::variation_store::ItemVariationStore;
use sigilbuzz::Error;

use super::attach_list::attach_point;
use super::item_var_store::store_len;
use super::lig_caret::rewrite_lig_glyph;
use super::read::coverage;
use super::{mark_glyph_sets, rewrite_classdef_subtable, rewrite_gdef_bytes, Header, StorePlan};
use crate::coverage::emit_coverage_from_glyphs;
use crate::layout::GidMap;
use crate::warnings::{Diag, Warnings};
use crate::{SubsetError, SubsetWarning};

fn u16_at(buf: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([buf[pos], buf[pos + 1]])
}

fn put_u16(buf: &mut [u8], pos: usize, v: u16) {
    buf[pos..pos + 2].copy_from_slice(&v.to_be_bytes());
}

/// Keeps `old -> new` for every pair, plus `.notdef`.
fn keep(pairs: &[(u16, u16)]) -> GidMap {
    let max = pairs.iter().map(|p| p.0).max().unwrap_or(0) as usize;
    let mut table = vec![None; max + 1];
    table[0] = Some(0);
    for &(old, new) in pairs {
        table[old as usize] = Some(new);
    }
    GidMap::from_table(table)
}

fn variation_index(outer: u16, inner: u16) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [outer, inner, 0x8000] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

/// Device format 1 for ppem 11..=12.
fn hinting_device() -> Vec<u8> {
    vec![0, 11, 0, 12, 0, 1, 0x50, 0]
}

/// One region peaking at 1.0 and one ItemVariationData with `deltas`
/// as i16 rows.
fn ivs(deltas: &[i16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&12u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&22u32.to_be_bytes());
    for v in [1u16, 1, 0, 0x4000, 0x4000] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    for v in [deltas.len() as u16, 1, 1, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    for d in deltas {
        out.extend_from_slice(&d.to_be_bytes());
    }
    out
}

/// The subtables of a fixture, placed after the header in field order.
/// `ivs_first` places the store before the others instead, so the
/// rewriter has to size it rather than copy to the end of the table.
#[derive(Default)]
struct Parts {
    minor: u16,
    glyph_class: Option<Vec<u8>>,
    attach_list: Option<Vec<u8>>,
    lig_carets: Option<Vec<u8>>,
    mark_attach: Option<Vec<u8>>,
    mark_sets: Option<Vec<u8>>,
    ivs: Option<Vec<u8>>,
    ivs_first: bool,
}

fn build_gdef(p: &Parts) -> Vec<u8> {
    let header_len = match p.minor {
        0 => 12,
        2 => 14,
        _ => 18,
    };
    let mut out = vec![0u8; header_len];
    put_u16(&mut out, 0, 1);
    put_u16(&mut out, 2, p.minor);
    let place_ivs = |out: &mut Vec<u8>| {
        if let Some(store) = &p.ivs {
            let at = out.len() as u32;
            out[14..18].copy_from_slice(&at.to_be_bytes());
            out.extend_from_slice(store);
        }
    };
    if p.ivs_first {
        place_ivs(&mut out);
    }
    for (slot, body) in [
        (4, &p.glyph_class),
        (6, &p.attach_list),
        (8, &p.lig_carets),
        (10, &p.mark_attach),
        (12, &p.mark_sets),
    ] {
        if let Some(body) = body {
            let at = out.len() as u16;
            put_u16(&mut out, slot, at);
            out.extend_from_slice(body);
        }
    }
    if !p.ivs_first {
        place_ivs(&mut out);
    }
    out
}

fn class_def(pairs: &[(u16, u16)]) -> Vec<u8> {
    crate::classdef::emit_classdef(pairs)
}

/// A format 2 Coverage with one range per `(start, end)`.
fn coverage_ranges(ranges: &[(u16, u16)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
    let mut index = 0u16;
    for &(start, end) in ranges {
        for v in [start, end, index] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        index += end - start + 1;
    }
    out
}

/// A MarkGlyphSetsDef whose sets are the given Coverage tables (`None`
/// is a null offset).
fn mark_sets(sets: &[Option<Vec<u8>>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(sets.len() as u16).to_be_bytes());
    out.resize(4 + sets.len() * 4, 0);
    for (i, set) in sets.iter().enumerate() {
        if let Some(cov) = set {
            let at = out.len() as u32;
            out[4 + i * 4..8 + i * 4].copy_from_slice(&at.to_be_bytes());
            out.extend_from_slice(cov);
        }
    }
    out
}

/// An AttachList with one AttachPoint per `(glyph, points)`; the
/// Coverage goes last.
fn attach_list(entries: &[(u16, &[u16])]) -> Vec<u8> {
    let mut out = vec![0u8; 4 + entries.len() * 2];
    put_u16(&mut out, 2, entries.len() as u16);
    for (i, (_, points)) in entries.iter().enumerate() {
        let at = out.len() as u16;
        put_u16(&mut out, 4 + i * 2, at);
        out.extend_from_slice(&(points.len() as u16).to_be_bytes());
        for p in *points {
            out.extend_from_slice(&p.to_be_bytes());
        }
    }
    let at = out.len() as u16;
    put_u16(&mut out, 0, at);
    let glyphs: Vec<u16> = entries.iter().map(|e| e.0).collect();
    out.extend_from_slice(&emit_coverage_from_glyphs(&glyphs));
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Caret {
    Coord(i16),
    Point(u16),
    /// Format 3: coordinate plus a device table (`None` = null offset).
    Device(i16, Option<Vec<u8>>),
}

/// A LigCaretList with one LigGlyph per `(glyph, carets)`. The format 3
/// device tables are parked after the Coverage, far from their carets,
/// so a verbatim copy of a caret would point at the wrong bytes.
fn lig_caret_list(ligs: &[(u16, Vec<Caret>)]) -> Vec<u8> {
    let mut out = vec![0u8; 4 + ligs.len() * 2];
    put_u16(&mut out, 2, ligs.len() as u16);
    let mut devices: Vec<(usize, Vec<u8>)> = Vec::new();
    for (i, (_, carets)) in ligs.iter().enumerate() {
        let lig_glyph = out.len();
        put_u16(&mut out, 4 + i * 2, lig_glyph as u16);
        out.extend_from_slice(&(carets.len() as u16).to_be_bytes());
        out.resize(lig_glyph + 2 + carets.len() * 2, 0);
        for (k, caret) in carets.iter().enumerate() {
            let at = out.len();
            put_u16(&mut out, lig_glyph + 2 + k * 2, (at - lig_glyph) as u16);
            match caret {
                Caret::Coord(c) => {
                    out.extend_from_slice(&1u16.to_be_bytes());
                    out.extend_from_slice(&c.to_be_bytes());
                }
                Caret::Point(p) => {
                    out.extend_from_slice(&2u16.to_be_bytes());
                    out.extend_from_slice(&p.to_be_bytes());
                }
                Caret::Device(c, device) => {
                    out.extend_from_slice(&3u16.to_be_bytes());
                    out.extend_from_slice(&c.to_be_bytes());
                    out.extend_from_slice(&0u16.to_be_bytes());
                    if let Some(d) = device {
                        devices.push((at, d.clone()));
                    }
                }
            }
        }
    }
    let at = out.len() as u16;
    put_u16(&mut out, 0, at);
    let glyphs: Vec<u16> = ligs.iter().map(|l| l.0).collect();
    out.extend_from_slice(&emit_coverage_from_glyphs(&glyphs));
    out.extend_from_slice(&[0xEE; 5]);
    for (caret, device) in devices {
        let at = out.len();
        put_u16(&mut out, caret + 4, (at - caret) as u16);
        out.extend_from_slice(&device);
    }
    out
}

/// Header offset of the subtable in `slot`, or 0.
fn subtable(gdef: &[u8], slot: usize) -> usize {
    usize::from(u16_at(gdef, slot))
}

/// Reads the AttachPoint indices of `gid` from a rewritten GDEF.
fn attach_points(gdef: &[u8], gid: u16) -> Option<Vec<u16>> {
    let list = subtable(gdef, 6);
    let cov = Coverage::parse(&gdef[list + subtable(gdef, list)..]).unwrap();
    let index = usize::from(cov.index_of(gid)?);
    let point = list + usize::from(u16_at(gdef, list + 4 + index * 2));
    let count = usize::from(u16_at(gdef, point));
    Some(
        (0..count)
            .map(|k| u16_at(gdef, point + 2 + k * 2))
            .collect(),
    )
}

/// Reads the carets of ligature `gid` from a rewritten GDEF, resolving
/// format 3 device offsets against each CaretValue.
fn lig_carets(gdef: &[u8], gid: u16) -> Option<Vec<Caret>> {
    let list = subtable(gdef, 8);
    let cov = Coverage::parse(&gdef[list + subtable(gdef, list)..]).unwrap();
    let index = usize::from(cov.index_of(gid)?);
    let lig = list + usize::from(u16_at(gdef, list + 4 + index * 2));
    let count = usize::from(u16_at(gdef, lig));
    let carets = (0..count)
        .map(|k| {
            let caret = lig + usize::from(u16_at(gdef, lig + 2 + k * 2));
            match u16_at(gdef, caret) {
                1 => Caret::Coord(u16_at(gdef, caret + 2) as i16),
                2 => Caret::Point(u16_at(gdef, caret + 2)),
                _ => {
                    let dev = usize::from(u16_at(gdef, caret + 4));
                    let table = (dev != 0).then(|| {
                        crate::device::device_table(gdef, caret + dev)
                            .unwrap()
                            .to_vec()
                    });
                    Caret::Device(u16_at(gdef, caret + 2) as i16, table)
                }
            }
        })
        .collect();
    Some(carets)
}

fn rewrite(gdef: &[u8], map: &GidMap, keep_variations: bool) -> Vec<u8> {
    rewrite_warned(gdef, map, keep_variations)
        .0
        .expect("GDEF survives")
}

/// The rewritten table, if any, and the warnings the rewrite raised.
fn rewrite_warned(
    gdef: &[u8],
    map: &GidMap,
    keep_variations: bool,
) -> (Option<Vec<u8>>, Vec<SubsetWarning>) {
    let warnings = Warnings::default();
    let out = rewrite_gdef_bytes(gdef, map, keep_variations, &warnings).unwrap();
    (out, warnings.into_sorted())
}

/// `(offset, dropped)` of each warning, all of which must be on GDEF.
fn warned(warnings: &[SubsetWarning]) -> Vec<(usize, &'static str)> {
    assert!(warnings.iter().all(|w| w.table == *b"GDEF"), "{warnings:?}");
    warnings.iter().map(|w| (w.offset, w.dropped)).collect()
}

#[test]
fn rewrite_classdef_filters_dropped_gids() {
    // Class assignments: gid 5->1, gid 6->2, gid 10->3.
    let cd_bytes = class_def(&[(5, 1), (6, 2), (10, 3)]);
    let map = keep(&[(5, 1), (10, 3)]);
    let new_cd = rewrite_classdef_subtable(&cd_bytes, 0, &map, &Diag::NONE, "").unwrap();
    let parsed = ClassDef::parse(&new_cd).unwrap();
    assert_eq!(parsed.class_of(1), 1, "gid 5->1 keeps class 1");
    assert_eq!(parsed.class_of(3), 3, "gid 10->3 keeps class 3");
    assert_eq!(parsed.class_of(2), 0, "gid 6 dropped -> unlisted = class 0");
}

/// Lookups name mark glyph sets by index, so the second set keeps its
/// slot even though all of its glyphs are gone.
#[test]
fn mark_glyph_sets_keep_their_indices_when_a_set_empties() {
    let gdef = build_gdef(&Parts {
        minor: 2,
        glyph_class: Some(class_def(&[(10, 3), (11, 3), (12, 3), (13, 3), (14, 3)])),
        mark_sets: Some(mark_sets(&[
            Some(emit_coverage_from_glyphs(&[10, 11])),
            Some(emit_coverage_from_glyphs(&[12])),
            Some(coverage_ranges(&[(13, 14)])),
        ])),
        ..Parts::default()
    });
    let out = rewrite(&gdef, &keep(&[(10, 1), (13, 2)]), true);
    assert_eq!(u16_at(&out, 2), 2, "minor version 2");
    let parsed = Gdef::parse(&out).unwrap();
    let set0 = parsed.mark_filtering_set(0).unwrap();
    let set1 = parsed.mark_filtering_set(1).unwrap();
    let set2 = parsed.mark_filtering_set(2).unwrap();
    assert!(set0.contains(1) && !set0.contains(2));
    assert!(set1.is_empty());
    assert!(set2.contains(2) && !set2.contains(1));
    assert!(parsed.mark_filtering_set(3).is_none());
}

#[test]
fn mark_glyph_set_with_null_offset_stays_an_empty_set() {
    let gdef = build_gdef(&Parts {
        minor: 2,
        mark_sets: Some(mark_sets(&[None, Some(emit_coverage_from_glyphs(&[4]))])),
        ..Parts::default()
    });
    let out = rewrite(&gdef, &keep(&[(4, 1)]), true);
    let parsed = Gdef::parse(&out).unwrap();
    assert!(parsed.mark_filtering_set(0).unwrap().is_empty());
    assert!(parsed.mark_filtering_set(1).unwrap().contains(1));
}

/// Only empty mark glyph sets left and nothing else: the table drops.
/// With a surviving class def, the empty sets ride along instead.
#[test]
fn empty_mark_glyph_sets_alone_do_not_keep_gdef() {
    let sets = mark_sets(&[Some(emit_coverage_from_glyphs(&[12]))]);
    let alone = build_gdef(&Parts {
        minor: 2,
        mark_sets: Some(sets.clone()),
        ..Parts::default()
    });
    let map = keep(&[(5, 1)]);
    assert!(rewrite_warned(&alone, &map, true).0.is_none());

    let with_classes = build_gdef(&Parts {
        minor: 2,
        glyph_class: Some(class_def(&[(5, 1)])),
        mark_sets: Some(sets),
        ..Parts::default()
    });
    let out = rewrite(&with_classes, &map, true);
    assert_eq!(u16_at(&out, 2), 2);
    let parsed = Gdef::parse(&out).unwrap();
    assert!(parsed.mark_filtering_set(0).unwrap().is_empty());
}

#[test]
fn attach_list_keeps_points_of_kept_glyphs_in_coverage_order() {
    let gdef = build_gdef(&Parts {
        attach_list: Some(attach_list(&[(5, &[1, 2]), (7, &[3]), (9, &[4, 5, 6])])),
        ..Parts::default()
    });
    let out = rewrite(&gdef, &keep(&[(5, 1), (9, 2)]), true);
    assert_eq!(u16_at(&out, 2), 0, "minor version 0");
    assert_eq!(attach_points(&out, 1), Some(vec![1, 2]));
    assert_eq!(attach_points(&out, 2), Some(vec![4, 5, 6]));
    assert_eq!(attach_points(&out, 3), None);
    let list = subtable(&out, 6);
    assert_eq!(u16_at(&out, list + 2), 2, "glyphCount");
}

/// Coverage format 2 carries explicit coverage indices; the rewriter
/// must use them, not the glyph's rank.
#[test]
fn attach_list_honors_format2_coverage_indices() {
    let mut list = attach_list(&[(20, &[7]), (21, &[8]), (30, &[9])]);
    let cov_at = usize::from(u16_at(&list, 0));
    list.truncate(cov_at);
    list.extend_from_slice(&coverage_ranges(&[(20, 21), (30, 30)]));
    let gdef = build_gdef(&Parts {
        attach_list: Some(list),
        ..Parts::default()
    });
    let out = rewrite(&gdef, &keep(&[(21, 1), (30, 2)]), true);
    assert_eq!(attach_points(&out, 1), Some(vec![8]));
    assert_eq!(attach_points(&out, 2), Some(vec![9]));
}

#[test]
fn lig_caret_list_copies_every_caret_format() {
    let carets20 = vec![
        Caret::Coord(300),
        Caret::Point(7),
        Caret::Device(500, Some(variation_index(0, 3))),
        Caret::Device(600, Some(hinting_device())),
        Caret::Device(700, None),
    ];
    let gdef = build_gdef(&Parts {
        lig_carets: Some(lig_caret_list(&[
            (20, carets20.clone()),
            (21, vec![Caret::Coord(100)]),
        ])),
        ..Parts::default()
    });
    let out = rewrite(&gdef, &keep(&[(20, 1)]), true);
    assert_eq!(lig_carets(&out, 1), Some(carets20));
    assert_eq!(lig_carets(&out, 2), None);
}

/// A static subset keeps no ItemVariationStore, so format 3 carets
/// drop their VariationIndex tables and keep hinting Device tables.
#[test]
fn lig_caret_variation_indices_drop_without_variations() {
    let gdef = build_gdef(&Parts {
        lig_carets: Some(lig_caret_list(&[(
            20,
            vec![
                Caret::Device(500, Some(variation_index(0, 3))),
                Caret::Device(600, Some(hinting_device())),
            ],
        )])),
        ..Parts::default()
    });
    let out = rewrite(&gdef, &keep(&[(20, 1)]), false);
    assert_eq!(
        lig_carets(&out, 1),
        Some(vec![
            Caret::Device(500, None),
            Caret::Device(600, Some(hinting_device()))
        ])
    );
}

#[test]
fn item_variation_store_rides_along_when_variations_are_kept() {
    let store = ivs(&[-40, 25]);
    for ivs_first in [false, true] {
        let gdef = build_gdef(&Parts {
            minor: 3,
            glyph_class: Some(class_def(&[(5, 1)])),
            ivs: Some(store.clone()),
            ivs_first,
            ..Parts::default()
        });
        let out = rewrite(&gdef, &keep(&[(5, 1)]), true);
        assert_eq!(u16_at(&out, 2), 3, "minor version 3");
        let ivs_at = u32::from_be_bytes([out[14], out[15], out[16], out[17]]) as usize;
        assert_eq!(&out[ivs_at..], &store[..], "store copied verbatim, last");
        let parsed = Gdef::parse(&out).unwrap();
        let parsed_store = parsed.item_variation_store().unwrap();
        assert_eq!(parsed_store.delta(0, 0, &[1.0]), -40.0);
        assert_eq!(parsed_store.delta(0, 1, &[0.5]), 12.5);
    }
}

#[test]
fn version_is_the_lowest_that_holds_what_survived() {
    let store = ivs(&[1]);
    let full = Parts {
        minor: 3,
        glyph_class: Some(class_def(&[(5, 1)])),
        mark_sets: Some(mark_sets(&[Some(emit_coverage_from_glyphs(&[6]))])),
        ivs: Some(store),
        ..Parts::default()
    };
    let map = keep(&[(5, 1), (6, 2)]);
    let gdef = build_gdef(&full);
    // Everything kept: 1.3 with an 18-byte header.
    let out = rewrite(&gdef, &map, true);
    assert_eq!((u16_at(&out, 2), subtable(&out, 4)), (3, 18));
    // Static subset: the store drops, the mark sets keep 1.2.
    let out = rewrite(&gdef, &map, false);
    assert_eq!((u16_at(&out, 2), subtable(&out, 4)), (2, 14));
    assert!(Gdef::parse(&out).unwrap().item_variation_store().is_none());
    // No mark sets either: 1.0 with a 12-byte header.
    let gdef = build_gdef(&Parts {
        mark_sets: None,
        ivs: None,
        minor: 3,
        ..full
    });
    let out = rewrite(&gdef, &map, true);
    assert_eq!((u16_at(&out, 2), subtable(&out, 4)), (0, 12));
}

#[test]
fn gdef_drops_only_when_nothing_survives() {
    let gdef = build_gdef(&Parts {
        glyph_class: Some(class_def(&[(5, 1)])),
        attach_list: Some(attach_list(&[(6, &[1])])),
        lig_carets: Some(lig_caret_list(&[(7, vec![Caret::Coord(1)])])),
        mark_attach: Some(class_def(&[(8, 1)])),
        ..Parts::default()
    });
    let (out, warnings) = rewrite_warned(&gdef, &keep(&[(9, 1)]), true);
    assert!(out.is_none());
    assert!(warnings.is_empty(), "nothing malformed: {warnings:?}");
    // Keeping just the ligature keeps the table, with only the
    // LigCaretList in it.
    let out = rewrite(&gdef, &keep(&[(7, 1)]), true);
    assert_eq!([4, 6, 10].map(|s| subtable(&out, s)), [0, 0, 0]);
    assert_eq!(lig_carets(&out, 1), Some(vec![Caret::Coord(1)]));
}

/// A header that cannot be read drops the whole table; the reader
/// still reports where it failed.
#[test]
fn unreadable_headers_drop_gdef() {
    let gdef = build_gdef(&Parts {
        glyph_class: Some(class_def(&[(5, 1)])),
        ..Parts::default()
    });
    let map = keep(&[(5, 1)]);
    let (out, warnings) = rewrite_warned(&gdef[..9], &map, true);
    assert!(out.is_none());
    assert_eq!(warned(&warnings), [(8, "the whole table")]);
    let mut major2 = gdef.clone();
    put_u16(&mut major2, 0, 2);
    let (out, warnings) = rewrite_warned(&major2, &map, true);
    assert!(out.is_none());
    assert_eq!(warned(&warnings), [(0, "the whole table")]);
    let err = Header::read(&gdef[..9]).err().unwrap();
    assert!(matches!(err, Error::Truncated { offset: 8, .. }), "{err:?}");
    let err = Header::read(&major2).err().unwrap();
    assert!(matches!(err, Error::Malformed { offset: 0, .. }), "{err:?}");
}

/// A truncated AttachPoint drops that glyph's entry only.
#[test]
fn truncated_attach_point_drops_its_entry() {
    let gdef = build_gdef(&Parts {
        attach_list: Some(attach_list(&[(5, &[1, 2, 3]), (6, &[4])])),
        ..Parts::default()
    });
    let list = subtable(&gdef, 6);
    let point = list + usize::from(u16_at(&gdef, list + 4));
    // Claim more points than the table holds before the Coverage.
    let mut bad = gdef.clone();
    put_u16(&mut bad, point, 0x4000);
    let err = attach_point(&bad, point).err().unwrap();
    assert!(
        matches!(err, Error::Truncated { offset, .. } if offset == point),
        "{err:?}"
    );
    let (out, warnings) = rewrite_warned(&bad, &keep(&[(5, 1), (6, 2)]), true);
    let out = out.expect("GDEF survives");
    assert_eq!(attach_points(&out, 1), None);
    assert_eq!(attach_points(&out, 2), Some(vec![4]));
    assert_eq!(warned(&warnings), [(point, "one glyph's AttachPoint")]);
}

/// A list whose Coverage cannot be read is dropped; the rest of GDEF
/// survives.
#[test]
fn unreadable_coverage_drops_its_list() {
    let gdef = build_gdef(&Parts {
        glyph_class: Some(class_def(&[(5, 1)])),
        attach_list: Some(attach_list(&[(5, &[1])])),
        ..Parts::default()
    });
    let list = subtable(&gdef, 6);
    let cov = list + subtable(&gdef, list);
    let mut bad = gdef.clone();
    put_u16(&mut bad, cov, 7);
    let err = coverage(&bad, cov).err().unwrap();
    assert!(
        matches!(err, Error::Malformed { offset, .. } if offset == cov),
        "{err:?}"
    );
    let (out, warnings) = rewrite_warned(&bad, &keep(&[(5, 1)]), true);
    let out = out.expect("GDEF survives");
    assert_eq!(subtable(&out, 6), 0, "AttachList dropped");
    assert_eq!(warned(&warnings), [(cov, "the AttachList")]);
    assert_eq!(
        ClassDef::parse(&out[subtable(&out, 4)..])
            .unwrap()
            .class_of(1),
        1
    );
    // Cut inside the Coverage: same outcome.
    let (cut, warnings) = rewrite_warned(&gdef[..cov + 5], &keep(&[(5, 1)]), true);
    assert_eq!(subtable(&cut.expect("GDEF survives"), 6), 0);
    assert_eq!(warned(&warnings), [(cov + 4, "the AttachList")]);
}

/// A LigGlyph with an unknown caret format or a null caret offset
/// drops that ligature's entry; the reader locates the fault.
#[test]
fn malformed_caret_values_drop_their_ligature() {
    let gdef = build_gdef(&Parts {
        lig_carets: Some(lig_caret_list(&[
            (20, vec![Caret::Coord(5)]),
            (21, vec![Caret::Coord(9)]),
        ])),
        ..Parts::default()
    });
    let list = subtable(&gdef, 8);
    let lig = list + usize::from(u16_at(&gdef, list + 4));
    let caret = lig + usize::from(u16_at(&gdef, lig + 2));
    let map = keep(&[(20, 1), (21, 2)]);

    let mut bad_format = gdef.clone();
    put_u16(&mut bad_format, caret, 9);
    let mut null_caret = gdef.clone();
    put_u16(&mut null_caret, lig + 2, 0);
    for (bad, at) in [(&bad_format, caret), (&null_caret, lig + 2)] {
        match rewrite_lig_glyph(bad, lig, StorePlan::Keep, &Diag::NONE) {
            Err(SubsetError::Parse(Error::Malformed { offset, .. })) => assert_eq!(offset, at),
            other => panic!("expected a parse error, got {other:?}"),
        }
        let (out, warnings) = rewrite_warned(bad, &map, true);
        let out = out.expect("GDEF survives");
        assert_eq!(lig_carets(&out, 1), None);
        assert_eq!(lig_carets(&out, 2), Some(vec![Caret::Coord(9)]));
        assert_eq!(warned(&warnings), [(at, "one ligature's carets")]);
    }
}

/// A MarkGlyphSetsDef with an unknown format, or a set count running
/// past the table, is dropped; a set whose Coverage is unreadable or
/// out of reach becomes empty and keeps its index.
#[test]
fn malformed_mark_glyph_sets_are_dropped_or_emptied() {
    let gdef = build_gdef(&Parts {
        minor: 2,
        glyph_class: Some(class_def(&[(4, 3)])),
        mark_sets: Some(mark_sets(&[
            Some(emit_coverage_from_glyphs(&[4])),
            Some(emit_coverage_from_glyphs(&[4])),
        ])),
        ..Parts::default()
    });
    let sets = subtable(&gdef, 12);
    let map = keep(&[(4, 1)]);
    let mut bad = gdef.clone();
    put_u16(&mut bad, sets, 2);
    let err = mark_glyph_sets::rewrite(&bad, sets, &map, &Diag::NONE)
        .err()
        .unwrap();
    assert!(
        matches!(err, Error::Malformed { offset, .. } if offset == sets),
        "{err:?}"
    );
    let (out, warnings) = rewrite_warned(&bad, &map, true);
    assert_eq!(u16_at(&out.unwrap(), 2), 0, "GDEF 1.0: the sets are gone");
    assert_eq!(warned(&warnings), [(sets, "the MarkGlyphSetsDef")]);

    let mut long = gdef.clone();
    put_u16(&mut long, sets + 2, 50);
    assert!(matches!(
        mark_glyph_sets::rewrite(&long, sets, &map, &Diag::NONE),
        Err(Error::Truncated { .. })
    ));
    assert_eq!(u16_at(&rewrite(&long, &map, true), 2), 0);

    // Set 0's Coverage turns unreadable and set 1's offset points far
    // past the table: both become empty sets, in place.
    let mut sick = gdef.clone();
    let cov = sets
        + u32::from_be_bytes([
            gdef[sets + 4],
            gdef[sets + 5],
            gdef[sets + 6],
            gdef[sets + 7],
        ]) as usize;
    put_u16(&mut sick, cov, 9);
    sick[sets + 8..sets + 12].copy_from_slice(&(u32::MAX - 1).to_be_bytes());
    let (out, warnings) = rewrite_warned(&sick, &map, true);
    let parsed = Gdef::parse(out.as_deref().unwrap()).unwrap();
    assert!(parsed.mark_filtering_set(0).unwrap().is_empty());
    assert!(parsed.mark_filtering_set(1).unwrap().is_empty());
    assert_eq!(
        warned(&warnings),
        [
            (sets + 8, "the glyphs of one mark glyph set"),
            (cov, "the glyphs of one mark glyph set")
        ]
    );
}

/// A malformed store is dropped; everything else survives.
#[test]
fn malformed_item_variation_store_is_dropped() {
    let gdef = build_gdef(&Parts {
        minor: 3,
        glyph_class: Some(class_def(&[(5, 1)])),
        ivs: Some(ivs(&[1, 2, 3])),
        ..Parts::default()
    });
    let store = u32::from_be_bytes([gdef[14], gdef[15], gdef[16], gdef[17]]) as usize;
    let err = store_len(&gdef[..gdef.len() - 1], store).err().unwrap();
    assert!(matches!(err, Error::Truncated { .. }), "{err:?}");
    for keep_variations in [true, false] {
        let (out, warnings) =
            rewrite_warned(&gdef[..gdef.len() - 1], &keep(&[(5, 1)]), keep_variations);
        let out = out.expect("GDEF survives");
        assert_eq!(u16_at(&out, 2), 0, "GDEF 1.0 without the store");
        // A static rewrite never reads the store, so only a rewrite
        // that keeps it notices the damage.
        if keep_variations {
            assert_eq!(
                warned(&warnings),
                [(gdef.len() - 1, "the ItemVariationStore")]
            );
        } else {
            assert!(warnings.is_empty(), "{warnings:?}");
        }
        assert_eq!(
            ClassDef::parse(&out[subtable(&out, 4)..])
                .unwrap()
                .class_of(1),
            1
        );
    }
}

/// Offset32s near `u32::MAX` must be caught as running past the table.
/// Added to a table position they would wrap a 32-bit `usize`, so the
/// store is sized in wider arithmetic.
#[test]
fn offset32s_near_the_top_of_the_range_are_caught() {
    // regionListOffset, then itemVariationDataOffsets[0].
    for field in [2, 8] {
        let mut store = ivs(&[1]);
        store[field..field + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        let err = store_len(&store, 0).err().unwrap();
        assert!(matches!(err, Error::Truncated { .. }), "{err:?}");
    }
    // The largest counts every u16 allows still size without wrapping:
    // axisCount, regionCount, itemCount, wordDeltaCount (LONG_WORDS),
    // regionIndexCount.
    let mut store = ivs(&[1]);
    for field in [12, 14, 22, 24, 26] {
        put_u16(&mut store, field, u16::MAX);
    }
    let err = store_len(&store, 0).err().unwrap();
    assert!(matches!(err, Error::Truncated { .. }), "{err:?}");
}

/// The item store is sized, not copied to the end of the table: with
/// the store first and a class def after it, the copy stops at the
/// store's last byte.
#[test]
fn item_variation_store_extent_covers_long_word_rows() {
    // One region, two items, LONG_WORDS with one i32 column.
    let mut store = Vec::new();
    store.extend_from_slice(&1u16.to_be_bytes());
    store.extend_from_slice(&12u32.to_be_bytes());
    store.extend_from_slice(&1u16.to_be_bytes());
    store.extend_from_slice(&22u32.to_be_bytes());
    for v in [1u16, 1, 0, 0x4000, 0x4000] {
        store.extend_from_slice(&v.to_be_bytes());
    }
    for v in [2u16, 0x8001, 1, 0] {
        store.extend_from_slice(&v.to_be_bytes());
    }
    store.extend_from_slice(&70_000i32.to_be_bytes());
    store.extend_from_slice(&(-3i32).to_be_bytes());
    let gdef = build_gdef(&Parts {
        minor: 3,
        glyph_class: Some(class_def(&[(5, 1)])),
        ivs: Some(store.clone()),
        ivs_first: true,
        ..Parts::default()
    });
    let out = rewrite(&gdef, &keep(&[(5, 1)]), true);
    let ivs_at = u32::from_be_bytes([out[14], out[15], out[16], out[17]]) as usize;
    assert_eq!(&out[ivs_at..], &store[..]);
    let parsed = ItemVariationStore::parse(&out[ivs_at..]).unwrap();
    assert_eq!(parsed.delta(0, 0, &[1.0]), 70_000.0);
}

/// End to end: Open Sans with a GDEF whose LigCaretList holds an
/// unknown caret format still subsets, keeping the glyph classes.
#[test]
fn a_malformed_gdef_piece_does_not_fail_the_subset() {
    const OPEN_SANS: &[u8] = include_bytes!("../../../../tests/fixtures/opensans_regular.ttf");
    let source = sigilbuzz::Face::parse_bytes(OPEN_SANS, 0).unwrap();
    let mut gdef = build_gdef(&Parts {
        glyph_class: Some(class_def(&[(36, 1), (37, 1), (38, 3)])),
        lig_carets: Some(lig_caret_list(&[(38, vec![Caret::Coord(5)])])),
        ..Parts::default()
    });
    let list = subtable(&gdef, 8);
    let lig = list + usize::from(u16_at(&gdef, list + 4));
    let caret = lig + usize::from(u16_at(&gdef, lig + 2));
    put_u16(&mut gdef, caret, 9);
    let tables: Vec<([u8; 4], Vec<u8>)> = source
        .records()
        .iter()
        .map(|rec| match &rec.tag {
            b"GDEF" => (rec.tag, gdef.clone()),
            _ => (rec.tag, source.table_bytes(rec.tag).unwrap().to_vec()),
        })
        .collect();
    let font = crate::sfnt::build(source.sfnt_version(), &tables);
    let face = sigilbuzz::Face::parse_bytes(&font, 0).unwrap();
    let input = crate::SubsetInput {
        gids: vec![36, 37, 38],
        ..crate::SubsetInput::default()
    };
    let out = crate::subset(&face, &input).expect("the subset succeeds");
    let new = |old: u16| out.gid_map.iter().find(|m| m.0 == old).unwrap().1;
    let result = sigilbuzz::Face::parse_bytes(&out.bytes, 0).unwrap();
    let bytes = result.table_bytes(*b"GDEF").unwrap();
    assert_eq!(u16_at(bytes, 8), 0, "no LigCaretList");
    let dropped: Vec<(&[u8; 4], usize, &str)> = out
        .warnings
        .iter()
        .map(|w| (&w.table, w.offset, w.dropped))
        .collect();
    assert_eq!(
        dropped,
        [(b"GDEF", caret, "one ligature's carets")],
        "the subset reports what it left out"
    );
    let classes = ClassDef::parse(&bytes[subtable(bytes, 4)..]).unwrap();
    assert_eq!(classes.class_of(new(36)), 1);
    assert_eq!(classes.class_of(new(38)), 3);
}
