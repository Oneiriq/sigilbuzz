//! Subtables whose rebuilt form outgrows 16-bit offsets.
//!
//! Each fixture is a valid source subtable, every offset in reach,
//! whose anchors or ValueRecords share Device and VariationIndex
//! tables. The rewriter gives every anchor and PairSet private copies
//! of those tables, so the rebuilt subtable is well past 64 KiB. Mark
//! attachment and PairPos format 1 must come back split into pieces
//! that resolve every glyph pair exactly as the source does; a
//! CursivePos, which cannot be split, must fail with an error rather
//! than wrap an offset.

use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::tables::gpos::Anchor;
use sigilbuzz::tables::gpos::{MarkBasePos, MarkLigaPos, PairPos};
use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use super::{rewrite_lookup, rewrite_subtable};
use crate::gpos_var::walk_gpos_device_slots;
use crate::layout::{build_gpos, GidMap, RewriterCtx};
use crate::SubsetError;

fn put(buf: &mut [u8], pos: usize, value: usize) {
    let value = u16::try_from(value).expect("fixture offset fits");
    buf[pos..pos + 2].copy_from_slice(&value.to_be_bytes());
}

/// Points the Offset16 at `slot` to the current end of `buf`.
fn point_here(buf: &mut [u8], slot: usize) {
    let at = buf.len();
    put(buf, slot, at);
}

fn push(buf: &mut Vec<u8>, values: &[u16]) {
    for v in values {
        buf.extend_from_slice(&v.to_be_bytes());
    }
}

fn coverage(glyphs: &[u16]) -> Vec<u8> {
    crate::coverage::emit_coverage_from_glyphs(glyphs)
}

/// Keeps every glyph under its own id.
fn identity(max: u16) -> GidMap {
    GidMap::from_kept(&(0..=max).collect::<Vec<u16>>())
}

/// VariationIndex tables the fixtures' anchors share.
const X_DEVICE: [u8; 6] = [0, 0, 0, 7, 0x80, 0];
const Y_DEVICE: [u8; 6] = [0, 0, 0, 9, 0x80, 0];

/// Appends an AnchorFormat3 whose device slots are patched later to
/// the shared tables; returns its position.
fn anchor3(buf: &mut Vec<u8>, x: i16, y: i16, pending: &mut Vec<usize>) -> usize {
    let at = buf.len();
    push(buf, &[3, x as u16, y as u16, 0, 0]);
    pending.push(at);
    at
}

/// Appends the shared device tables to `buf` and points every pending
/// anchor at them.
fn place_devices(buf: &mut Vec<u8>, pending: &[usize]) {
    let x = buf.len();
    buf.extend_from_slice(&X_DEVICE);
    let y = buf.len();
    buf.extend_from_slice(&Y_DEVICE);
    for &anchor in pending {
        put(buf, anchor + 6, x - anchor);
        put(buf, anchor + 8, y - anchor);
    }
}

/// What an anchor resolves to: its coordinates and device tables.
type Resolved = (i16, i16, Option<Vec<u8>>, Option<Vec<u8>>);

fn resolve(sub: &[u8], anchor: &Anchor) -> Resolved {
    let device = |off: u16| {
        (off != 0).then(|| {
            let at = anchor.table_offset + usize::from(off);
            sub[at..at + 6].to_vec()
        })
    };
    (
        anchor.x,
        anchor.y,
        device(anchor.x_device_off),
        device(anchor.y_device_off),
    )
}

/// MarkBasePos: `marks` as `(gid, class)` sharing one format 1 anchor
/// per class; base glyph `i` of `bases` gets format 3 anchors whose
/// coordinates repeat every `period` bases and whose device slots all
/// name the two shared tables.
fn mark_base(class_count: u16, marks: &[(u16, u16)], bases: &[u16], period: usize) -> Vec<u8> {
    let mcc = usize::from(class_count);
    let mut out = vec![0u8; 12];
    put(&mut out, 0, 1);
    put(&mut out, 6, mcc);
    let mark_gids: Vec<u16> = marks.iter().map(|m| m.0).collect();
    point_here(&mut out, 2);
    out.extend_from_slice(&coverage(&mark_gids));
    point_here(&mut out, 4);
    out.extend_from_slice(&coverage(bases));

    let mark_array = out.len();
    put(&mut out, 8, mark_array);
    push(&mut out, &[marks.len() as u16]);
    out.resize(mark_array + 2 + marks.len() * 4, 0);
    let class_anchor = out.len();
    for c in 0..class_count {
        push(&mut out, &[1, 100 + c, 500]);
    }
    for (i, &(_, class)) in marks.iter().enumerate() {
        let rec = mark_array + 2 + i * 4;
        put(&mut out, rec, usize::from(class));
        put(
            &mut out,
            rec + 2,
            class_anchor + usize::from(class) * 6 - mark_array,
        );
    }

    let array = out.len();
    put(&mut out, 10, array);
    push(&mut out, &[bases.len() as u16]);
    out.resize(array + 2 + bases.len() * mcc * 2, 0);
    let mut pending = Vec::new();
    let mut unique = Vec::new();
    for (i, _) in bases.iter().enumerate() {
        for c in 0..mcc {
            let key = (i % period) * mcc + c;
            if unique.len() <= key {
                unique.resize(key + 1, 0);
            }
            if unique[key] == 0 {
                let x = (key % 3000) as i16;
                unique[key] = anchor3(&mut out, x, c as i16, &mut pending);
            }
            put(&mut out, array + 2 + (i * mcc + c) * 2, unique[key] - array);
        }
    }
    place_devices(&mut out, &pending);
    out
}

/// Checks that for every mark and base, the first piece that attaches
/// them (the one a shaper would apply) gives the source's anchors.
fn assert_same_attachments(source: &[u8], pieces: &[Vec<u8>], marks: &[u16], bases: &[u16]) {
    let src = MarkBasePos::parse(source).unwrap();
    let parsed: Vec<MarkBasePos<'_>> = pieces
        .iter()
        .map(|p| MarkBasePos::parse(p).unwrap())
        .collect();
    for &m in marks {
        for &b in bases {
            let expected = src.attach(m, b).map(|a| {
                (
                    resolve(source, &a.mark_anchor),
                    resolve(source, &a.base_anchor),
                )
            });
            let got = parsed.iter().zip(pieces).find_map(|(p, bytes)| {
                p.attach(m, b).map(|a| {
                    (
                        resolve(bytes, &a.mark_anchor),
                        resolve(bytes, &a.base_anchor),
                    )
                })
            });
            assert_eq!(got, expected, "mark {m} on base {b}");
        }
    }
}

fn rewrite_pieces(lookup_type: u16, source: &[u8], max_gid: u16) -> Vec<Vec<u8>> {
    let map = identity(max_gid);
    let ctx = RewriterCtx::new(&map, None);
    let pieces = rewrite_subtable(&ctx, lookup_type, source);
    assert_eq!(ctx.offsets.check("overflow"), Ok(()));
    pieces.into_iter().map(|p| p.bytes).collect()
}

#[test]
fn mark_base_pos_past_64_kib_splits_by_mark_class() {
    let marks: Vec<(u16, u16)> = (0..16).map(|i| (3000 + i, i % 8)).collect();
    let bases: Vec<u16> = (1..=1200).collect();
    let source = mark_base(8, &marks, &bases, 300);
    assert!(source.len() < 0xFFFF, "the source fits: {}", source.len());
    let pieces = rewrite_pieces(4, &source, 3100);
    assert!(pieces.len() > 1, "expected a split, got {}", pieces.len());
    let mark_gids: Vec<u16> = marks.iter().map(|m| m.0).collect();
    assert_same_attachments(&source, &pieces, &mark_gids, &bases);
}

#[test]
fn mark_base_pos_class_too_big_alone_splits_across_bases() {
    // One mark class, 3000 bases with distinct anchors: the class
    // alone needs about 72 KiB once every anchor has its own devices.
    let marks = [(3100u16, 0u16), (3101, 0)];
    let bases: Vec<u16> = (1..=3000).collect();
    let source = mark_base(1, &marks, &bases, 3000);
    let pieces = rewrite_pieces(4, &source, 3200);
    assert!(pieces.len() > 1, "expected a split, got {}", pieces.len());
    assert_same_attachments(&source, &pieces, &[3100, 3101], &bases);
}

/// MarkLigPos: `ligatures` with `components` components each, every
/// component anchor a distinct format 3 anchor naming the shared
/// device tables, which sit after the whole LigatureArray.
fn mark_lig(class_count: u16, marks: &[(u16, u16)], ligatures: &[u16], components: u16) -> Vec<u8> {
    let mcc = usize::from(class_count);
    let mut out = vec![0u8; 12];
    put(&mut out, 0, 1);
    put(&mut out, 6, mcc);
    let mark_gids: Vec<u16> = marks.iter().map(|m| m.0).collect();
    point_here(&mut out, 2);
    out.extend_from_slice(&coverage(&mark_gids));
    point_here(&mut out, 4);
    out.extend_from_slice(&coverage(ligatures));
    let mark_array = out.len();
    put(&mut out, 8, mark_array);
    push(&mut out, &[marks.len() as u16]);
    out.resize(mark_array + 2 + marks.len() * 4, 0);
    let class_anchor = out.len();
    for c in 0..class_count {
        push(&mut out, &[1, 100 + c, 500]);
    }
    for (i, &(_, class)) in marks.iter().enumerate() {
        let rec = mark_array + 2 + i * 4;
        put(&mut out, rec, usize::from(class));
        put(
            &mut out,
            rec + 2,
            class_anchor + usize::from(class) * 6 - mark_array,
        );
    }
    let array = out.len();
    put(&mut out, 10, array);
    push(&mut out, &[ligatures.len() as u16]);
    out.resize(array + 2 + ligatures.len() * 2, 0);
    let mut pending = Vec::new();
    for i in 0..ligatures.len() {
        let attach = out.len();
        put(&mut out, array + 2 + i * 2, attach - array);
        push(&mut out, &[components]);
        let slots = out.len();
        out.resize(slots + usize::from(components) * mcc * 2, 0);
        for k in 0..usize::from(components) * mcc {
            let at = anchor3(&mut out, i as i16, k as i16, &mut pending);
            put(&mut out, slots + k * 2, at - attach);
        }
    }
    place_devices(&mut out, &pending);
    out
}

#[test]
fn mark_lig_pos_past_64_kib_splits_by_mark_class() {
    let marks: Vec<(u16, u16)> = (0..8).map(|i| (3000 + i, i % 4)).collect();
    let ligatures: Vec<u16> = (1..=600).collect();
    let source = mark_lig(4, &marks, &ligatures, 2);
    assert!(source.len() < 0xFFFF, "the source fits: {}", source.len());
    let pieces = rewrite_pieces(5, &source, 3100);
    assert!(pieces.len() > 1, "expected a split, got {}", pieces.len());
    let src = MarkLigaPos::parse(&source).unwrap();
    let parsed: Vec<MarkLigaPos<'_>> = pieces
        .iter()
        .map(|p| MarkLigaPos::parse(p).unwrap())
        .collect();
    for &(m, _) in &marks {
        for &l in &ligatures {
            for k in 0..2 {
                let expected = src.attach(m, l, k).map(|a| {
                    (
                        resolve(&source, &a.mark_anchor),
                        resolve(&source, &a.base_anchor),
                    )
                });
                let got = parsed.iter().zip(&pieces).find_map(|(p, bytes)| {
                    p.attach(m, l, k).map(|a| {
                        (
                            resolve(bytes, &a.mark_anchor),
                            resolve(bytes, &a.base_anchor),
                        )
                    })
                });
                assert_eq!(got, expected, "mark {m} on ligature {l} component {k}");
            }
        }
    }
}

/// Hinting Device table `j` (format 1, ppem 9..=12) for the PairPos
/// fixture; distinct bytes per `j`.
fn hinting_device(j: u16) -> [u8; 8] {
    let [hi, lo] = j.to_be_bytes();
    [0, 9, 0, 12, 0, 1, hi, lo]
}

/// PairPos format 1 kerning every first glyph against every second
/// glyph with an xAdvance and an xAdvance Device; the Device for the
/// `j`th second glyph is shared by all PairSets.
fn pair_pos(firsts: &[u16], seconds: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    push(&mut out, &[1, 0, 0x0044, 0, firsts.len() as u16]);
    out.resize(10 + firsts.len() * 2, 0);
    point_here(&mut out, 2);
    out.extend_from_slice(&coverage(firsts));
    let mut device_slots = Vec::new();
    for (i, &first) in firsts.iter().enumerate() {
        let set = out.len();
        put(&mut out, 10 + i * 2, set);
        push(&mut out, &[seconds.len() as u16]);
        for (j, &second) in seconds.iter().enumerate() {
            push(&mut out, &[second, first + j as u16, 0]);
            device_slots.push((out.len() - 2, set, j));
        }
    }
    let devices = out.len();
    for j in 0..seconds.len() {
        out.extend_from_slice(&hinting_device(j as u16));
    }
    for (slot, set, j) in device_slots {
        put(&mut out, slot, devices + j * 8 - set);
    }
    out
}

/// Device tables the walker reaches from each slot of a lone PairPos
/// subtable, in walk order.
fn pair_devices(sub: &[u8]) -> Vec<Vec<u8>> {
    let mut gpos = Vec::new();
    push(&mut gpos, &[1, 0, 0, 0, 10, 1, 4, 2, 0, 1, 8]);
    gpos.extend_from_slice(sub);
    let mut out = Vec::new();
    walk_gpos_device_slots(&mut gpos, &mut |b, slot| {
        if let Some(target) = slot.target(b) {
            out.push(crate::device::device_table(b, target).unwrap().to_vec());
        }
    });
    out
}

fn assert_same_pairs(source: &[u8], pieces: &[Vec<u8>], firsts: &[u16], seconds: &[u16]) {
    let src = PairPos::parse(source).unwrap();
    let parsed: Vec<PairPos<'_>> = pieces.iter().map(|p| PairPos::parse(p).unwrap()).collect();
    for &f in firsts {
        for &s in seconds {
            let expected = src.lookup(f, s).map(|v| v.0.x_advance);
            let got = parsed
                .iter()
                .find_map(|p| p.lookup(f, s))
                .map(|v| v.0.x_advance);
            assert_eq!(got, expected, "pair ({f}, {s})");
        }
    }
    let expected: Vec<Vec<u8>> = pair_devices(source);
    let got: Vec<Vec<u8>> = pieces.iter().flat_map(|p| pair_devices(p)).collect();
    assert_eq!(got, expected, "device tables in walk order");
}

#[test]
fn pair_pos_format1_past_64_kib_splits_by_first_glyph() {
    let firsts: Vec<u16> = (1..=300).collect();
    let seconds: Vec<u16> = (1001..=1030).collect();
    let source = pair_pos(&firsts, &seconds);
    assert!(source.len() < 0xFFFF, "the source fits: {}", source.len());
    let pieces = rewrite_pieces(2, &source, 1100);
    assert!(pieces.len() > 1, "expected a split, got {}", pieces.len());
    assert_same_pairs(&source, &pieces, &firsts, &seconds);
}

#[test]
fn cursive_pos_past_64_kib_is_an_error() {
    // Distinct entry and exit anchors sharing two device tables; with
    // private copies the anchors need about twice the space.
    let glyphs: Vec<u16> = (1..=2200).collect();
    let mut source = Vec::new();
    push(&mut source, &[1, 0, glyphs.len() as u16]);
    source.resize(6 + glyphs.len() * 4, 0);
    point_here(&mut source, 2);
    source.extend_from_slice(&coverage(&glyphs));
    let mut pending = Vec::new();
    for i in 0..glyphs.len() {
        for side in 0..2 {
            let at = anchor3(&mut source, i as i16, side, &mut pending);
            put(&mut source, 6 + i * 4 + side as usize * 2, at);
        }
    }
    place_devices(&mut source, &pending);
    assert!(source.len() < 0xFFFF, "the source fits: {}", source.len());
    let map = identity(2300);
    let ctx = RewriterCtx::new(&map, None);
    assert_eq!(
        rewrite_lookup(&ctx, 3, 0, None, &[&source]).err(),
        Some(SubsetError::Unsupported(
            "GPOS CursivePos rewrite: an offset exceeds 64 KiB"
        ))
    );
    // Wrapped in an Extension lookup, the error names the inner type.
    let mut wrapped = Vec::new();
    push(&mut wrapped, &[1, 3, 0, 8]);
    wrapped.extend_from_slice(&source);
    assert_eq!(
        rewrite_lookup(&ctx, 9, 0, None, &[&wrapped]).err(),
        Some(SubsetError::Unsupported(
            "GPOS CursivePos rewrite: an offset exceeds 64 KiB"
        ))
    );
}

/// A GPOS with one `kern` feature on the DFLT script running `lookup`,
/// a single lookup of `lookup_type` holding `subtable`.
fn gpos_with(lookup_type: u16, subtable: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    // Header, ScriptList at 10, FeatureList at 30, LookupList at 44.
    push(&mut out, &[1, 0, 10, 30, 44]);
    // ScriptList: DFLT -> Script at 8; Script: default LangSys at 4.
    out.extend_from_slice(b"\0\x01DFLT\0\x08");
    push(&mut out, &[4, 0, 0, 0xFFFF, 1, 0]);
    // FeatureList: kern -> Feature at 8, which runs lookup 0.
    out.extend_from_slice(b"\0\x01kern\0\x08");
    push(&mut out, &[0, 1, 0]);
    // LookupList: one lookup at 4, its subtable 8 bytes further.
    push(&mut out, &[1, 4, lookup_type, 0, 1, 8]);
    assert_eq!(out.len(), 56);
    out.extend_from_slice(subtable);
    out
}

#[test]
fn split_subtables_reach_the_rebuilt_gpos() {
    let firsts: Vec<u16> = (1..=300).collect();
    let seconds: Vec<u16> = (1001..=1030).collect();
    let source = pair_pos(&firsts, &seconds);
    let font = crate::sfnt::build(0x0001_0000, &[(tag::GPOS, gpos_with(2, &source))]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let map = identity(1100);
    let gpos = build_gpos(&face, &RewriterCtx::new(&map, None))
        .unwrap()
        .expect("GPOS survives");
    let font = crate::sfnt::build(0x0001_0000, &[(tag::GPOS, gpos)]);
    let out = Face::parse_bytes(&font, 0).unwrap();
    let table = out.gpos().unwrap().unwrap();
    let lookup = table.lookup_list().get(0).unwrap();
    assert!(lookup.subtable_count() > 1);
    let pieces: Vec<Vec<u8>> = (0..lookup.subtable_count())
        .map(|i| {
            let bytes = lookup.subtable_bytes(i).unwrap();
            // Promoted to Extension lookups: unwrap each subtable.
            if lookup.lookup_type() == 9 {
                let off = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                bytes[off as usize..].to_vec()
            } else {
                bytes.to_vec()
            }
        })
        .collect();
    let src = PairPos::parse(&source).unwrap();
    let parsed: Vec<PairPos<'_>> = pieces.iter().map(|p| PairPos::parse(p).unwrap()).collect();
    for &f in &firsts {
        for &s in &seconds {
            let expected = src.lookup(f, s).map(|v| v.0.x_advance);
            let got = parsed
                .iter()
                .find_map(|p| p.lookup(f, s))
                .map(|v| v.0.x_advance);
            assert_eq!(got, expected, "pair ({f}, {s})");
        }
    }
}
