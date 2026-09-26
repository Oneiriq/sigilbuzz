//! The GPOS rewriters carry `Device` / `VariationIndex` tables along
//! with the ValueRecords and Anchors that reference them.
//!
//! Every fixture parks its device tables at the far end of the source
//! subtable, away from the record that names them, so a rewriter that
//! copied the records verbatim would leave offsets pointing into
//! unrelated bytes of the rebuilt subtable. Each `VariationIndex` gets
//! a distinct inner index; the checks walk the output with the
//! instancer's device-slot walker and compare the inner indices the
//! slots resolve to.

use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::tables::gpos::{MarkBasePos, PairPos, SinglePos};

use super::rewrite_subtable;
use crate::gpos_var::walk_gpos_device_slots;
use crate::layout::{GidMap, RewriterCtx};

/// A slot the walker resolved: a VariationIndex inner index, or the
/// raw bytes of a hinting Device table.
#[derive(Debug, PartialEq, Eq)]
enum Resolved {
    Inner(u16),
    Device(Vec<u8>),
}

fn u16_at(buf: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([buf[pos], buf[pos + 1]])
}

fn vi(inner: u16) -> Vec<u8> {
    let mut out = vec![0u8, 0];
    out.extend_from_slice(&inner.to_be_bytes());
    out.extend_from_slice(&0x8000u16.to_be_bytes());
    out
}

/// Device format 1 covering ppem 9..=12 (one packed word).
fn hinting_device() -> Vec<u8> {
    vec![0, 9, 0, 12, 0, 1, 0x12, 0x34]
}

/// Builds a subtable whose device tables are appended at the end once
/// the body is laid out.
#[derive(Default)]
struct Builder {
    bytes: Vec<u8>,
    /// (slot position, base position, table bytes)
    devices: Vec<(usize, usize, Vec<u8>)>,
    /// (slot position, target label)
    offsets: Vec<(usize, &'static str)>,
    labels: Vec<(&'static str, usize)>,
}

impl Builder {
    fn u16(&mut self, v: u16) -> &mut Self {
        self.bytes.extend_from_slice(&v.to_be_bytes());
        self
    }

    fn i16(&mut self, v: i16) -> &mut Self {
        self.bytes.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// An Offset16 at the current position that will point at `label`,
    /// measured from the subtable start.
    fn offset_to(&mut self, label: &'static str) -> &mut Self {
        self.offsets.push((self.bytes.len(), label));
        self.u16(0)
    }

    /// Records the current position under `label`.
    fn label(&mut self, label: &'static str) -> &mut Self {
        self.labels.push((label, self.bytes.len()));
        self
    }

    /// A device slot at the current position whose offset is measured
    /// from `base` and names `table`.
    fn device(&mut self, base: usize, table: Vec<u8>) -> &mut Self {
        self.devices.push((self.bytes.len(), base, table));
        self.u16(0)
    }

    fn pos(&self) -> usize {
        self.bytes.len()
    }

    /// An AnchorFormat3 at the current position with the given x and y
    /// device tables (`None` leaves the slot null).
    fn anchor3(&mut self, x: i16, y: i16, xd: Option<Vec<u8>>, yd: Option<Vec<u8>>) -> &mut Self {
        let base = self.pos();
        self.u16(3).i16(x).i16(y);
        for table in [xd, yd] {
            match table {
                Some(t) => self.device(base, t),
                None => self.u16(0),
            };
        }
        self
    }

    fn finish(mut self) -> Vec<u8> {
        // Park the device tables after some filler so they sit well
        // away from the records naming them.
        self.bytes.extend_from_slice(&[0xEE; 7]);
        for (slot, base, table) in core::mem::take(&mut self.devices) {
            let pos = self.bytes.len();
            self.bytes.extend_from_slice(&table);
            let rel = (pos - base) as u16;
            self.bytes[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
        }
        for (slot, label) in &self.offsets {
            let target = self.labels.iter().find(|(l, _)| l == label).unwrap().1;
            self.bytes[*slot..*slot + 2].copy_from_slice(&(target as u16).to_be_bytes());
        }
        self.bytes
    }
}

fn coverage(glyphs: &[u16]) -> Vec<u8> {
    crate::coverage::emit_coverage_from_glyphs(glyphs)
}

fn keep(pairs: &[(u16, u16)]) -> GidMap {
    let max = pairs.iter().map(|p| p.0).max().unwrap_or(0) as usize;
    let mut table = vec![None; max + 1];
    for &(old, new) in pairs {
        table[old as usize] = Some(new);
    }
    GidMap::from_table(table)
}

fn rewrite(lookup_type: u16, sub: &[u8], map: &GidMap) -> Vec<u8> {
    let ctx = RewriterCtx::new(map, None);
    let mut pieces = rewrite_subtable(&ctx, lookup_type, sub);
    assert_eq!(pieces.len(), 1, "subtable survives whole");
    pieces.remove(0).bytes
}

/// Walks every device slot of `sub` (a `lookup_type` subtable) and
/// lists what each non-null slot resolves to, in walk order.
fn resolved(lookup_type: u16, sub: &[u8]) -> Vec<Resolved> {
    let mut gpos = Vec::new();
    for v in [1u16, 0, 0, 0, 10, 1, 4, lookup_type, 0, 1, 8] {
        gpos.extend_from_slice(&v.to_be_bytes());
    }
    gpos.extend_from_slice(sub);
    let mut out = Vec::new();
    walk_gpos_device_slots(&mut gpos, &mut |b, slot| {
        let Some(target) = slot.target(b) else {
            return;
        };
        let table = crate::device::device_table(b, target).expect("slot resolves to a table");
        if u16_at(table, 4) == 0x8000 {
            out.push(Resolved::Inner(u16_at(table, 2)));
        } else {
            out.push(Resolved::Device(table.to_vec()));
        }
    });
    out
}

#[test]
fn single_pos_format1_carries_its_device() {
    let mut b = Builder::default();
    b.u16(1).offset_to("cov").u16(0x0044).i16(-30);
    b.device(0, vi(77));
    b.label("cov");
    b.bytes.extend_from_slice(&coverage(&[10, 20]));
    let src = b.finish();

    let out = rewrite(1, &src, &keep(&[(0, 0), (20, 1)]));
    assert_eq!(resolved(1, &out), [Resolved::Inner(77)]);
    let parsed = SinglePos::parse(&out).unwrap();
    assert_eq!(parsed.adjustment(1).unwrap().x_advance, -30);
}

#[test]
fn single_pos_format2_carries_devices_of_kept_records_only() {
    let mut b = Builder::default();
    b.u16(2).offset_to("cov").u16(0x0044).u16(3);
    for (adv, table) in [(1i16, vi(10)), (2, vi(20)), (3, hinting_device())] {
        b.i16(adv).device(0, table);
    }
    b.label("cov");
    b.bytes.extend_from_slice(&coverage(&[10, 20, 30]));
    let src = b.finish();

    let out = rewrite(1, &src, &keep(&[(0, 0), (10, 1), (30, 2)]));
    assert_eq!(
        resolved(1, &out),
        [Resolved::Inner(10), Resolved::Device(hinting_device())]
    );
}

/// PairPos format 1 measures its device offsets from each PairSet, so
/// the tables have to travel inside the rebuilt PairSets.
#[test]
fn pair_pos_format1_carries_pair_set_devices() {
    let mut b = Builder::default();
    b.u16(1).offset_to("cov").u16(0x0044).u16(0).u16(2);
    b.offset_to("set5").offset_to("set6");
    b.label("set5");
    let set5 = b.pos();
    b.u16(2);
    b.u16(7).i16(-5).device(set5, vi(57));
    b.u16(8).i16(-6).device(set5, vi(58));
    b.label("set6");
    let set6 = b.pos();
    b.u16(2);
    b.u16(7).i16(-7).device(set6, vi(67));
    b.u16(8).i16(-8).device(set6, vi(67));
    b.label("cov");
    b.bytes.extend_from_slice(&coverage(&[5, 6]));
    let src = b.finish();

    // Drop glyph 8: each PairSet keeps its pair with glyph 7.
    let map = keep(&[(0, 0), (5, 1), (6, 2), (7, 3)]);
    let out = rewrite(2, &src, &map);
    assert_eq!(
        resolved(2, &out),
        [Resolved::Inner(57), Resolved::Inner(67)]
    );
    let parsed = PairPos::parse(&out).unwrap();
    assert_eq!(parsed.lookup(1, 3).unwrap().0.x_advance, -5);
    assert_eq!(parsed.lookup(2, 3).unwrap().0.x_advance, -7);
}

/// Identical device tables inside one PairSet are stored once.
#[test]
fn pair_set_devices_share_one_copy() {
    let mut b = Builder::default();
    b.u16(1).offset_to("cov").u16(0x0044).u16(0).u16(1);
    b.offset_to("set");
    b.label("set");
    let set = b.pos();
    b.u16(2);
    b.u16(7).i16(-5).device(set, vi(9));
    b.u16(8).i16(-6).device(set, vi(9));
    b.label("cov");
    b.bytes.extend_from_slice(&coverage(&[5]));
    let src = b.finish();

    let map = keep(&[(0, 0), (5, 1), (7, 2), (8, 3)]);
    let out = rewrite(2, &src, &map);
    assert_eq!(resolved(2, &out), [Resolved::Inner(9), Resolved::Inner(9)]);
    let set_off = u16_at(&out, 10) as usize;
    let first = u16_at(&out, set_off + 6) as usize;
    let second = u16_at(&out, set_off + 12) as usize;
    assert_eq!(first, second);
}

fn class_def(pairs: &[(u16, u16)]) -> Vec<u8> {
    crate::classdef::emit_classdef(pairs)
}

/// Class-pair PairPos: glyph 5 is class 1 on the first axis, glyph 7
/// class 1 on the second. Cells (1, 0) and (1, 1) carry devices.
fn pair_pos_format2(first_glyphs: &[u16]) -> Vec<u8> {
    let mut b = Builder::default();
    b.u16(2).offset_to("cov").u16(0x0044).u16(0);
    b.offset_to("cd1").offset_to("cd2").u16(2).u16(2);
    // class1 0: two empty cells.
    b.i16(0).u16(0).i16(0).u16(0);
    // class1 1: (1, 0) and (1, 1).
    b.i16(-10).device(0, vi(70));
    b.i16(-20).device(0, vi(71));
    b.label("cov");
    b.bytes.extend_from_slice(&coverage(first_glyphs));
    b.label("cd1");
    let cd1: Vec<(u16, u16)> = first_glyphs.iter().map(|&g| (g, 1)).collect();
    b.bytes.extend_from_slice(&class_def(&cd1));
    b.label("cd2");
    b.bytes.extend_from_slice(&class_def(&[(7, 1)]));
    b.finish()
}

/// Small subsets turn format 2 into explicit format 1 pairs; the cell
/// devices move from subtable-relative to PairSet-relative.
#[test]
fn pair_pos_format2_to_format1_moves_devices_into_pair_sets() {
    let src = pair_pos_format2(&[5]);
    let map = keep(&[(0, 0), (5, 1), (7, 2)]);
    let out = rewrite(2, &src, &map);
    assert_eq!(u16_at(&out, 0), 1, "fallback emits format 1");
    // Seconds 0 and 1 are class 0 (cell (1, 0)), second 2 is class 1.
    assert_eq!(
        resolved(2, &out),
        [
            Resolved::Inner(70),
            Resolved::Inner(70),
            Resolved::Inner(71)
        ]
    );
}

/// Larger subsets keep the class matrix; its devices stay
/// subtable-relative and are appended after the ClassDefs.
#[test]
fn pair_pos_format2_matrix_carries_its_devices() {
    let firsts: Vec<u16> = (1..=20).collect();
    let src = pair_pos_format2(&firsts);
    let pairs: Vec<(u16, u16)> = (0..40).map(|g| (g, g)).collect();
    let out = rewrite(2, &src, &keep(&pairs));
    assert_eq!(u16_at(&out, 0), 2, "large subset keeps format 2");
    assert_eq!(
        resolved(2, &out),
        [Resolved::Inner(70), Resolved::Inner(71)]
    );
}

#[test]
fn cursive_anchors_carry_their_devices() {
    let mut b = Builder::default();
    b.u16(1).offset_to("cov").u16(2);
    b.offset_to("entry3").offset_to("exit3");
    b.offset_to("entry4").u16(0);
    b.label("entry3")
        .anchor3(10, 20, Some(vi(31)), Some(vi(32)));
    b.label("exit3")
        .anchor3(30, 40, None, Some(hinting_device()));
    b.label("entry4").anchor3(50, 60, Some(vi(41)), None);
    b.label("cov");
    b.bytes.extend_from_slice(&coverage(&[3, 4]));
    let src = b.finish();

    let out = rewrite(3, &src, &keep(&[(0, 0), (3, 1)]));
    assert_eq!(
        resolved(3, &out),
        [
            Resolved::Inner(31),
            Resolved::Inner(32),
            Resolved::Device(hinting_device())
        ]
    );
}

/// MarkBasePos with two marks and two bases; the second of each is
/// kept. Mark anchors vary on both axes, the kept base only on x.
#[test]
fn mark_base_anchors_carry_their_devices() {
    let mut b = Builder::default();
    b.u16(1).offset_to("mcov").offset_to("bcov").u16(1);
    b.offset_to("marks").offset_to("bases");
    b.label("marks");
    let marks = b.pos();
    b.u16(2);
    let mark_rec = b.pos();
    b.u16(0).u16(0).u16(0).u16(0);
    b.label("bases");
    let bases = b.pos();
    b.u16(2);
    let base_rec = b.pos();
    b.u16(0).u16(0);
    // (record slot, anchor offset relative to its array)
    let mut patches = Vec::new();
    for (i, (x, xi, yi)) in [(1i16, 20u16, 120u16), (2, 21, 121)]
        .into_iter()
        .enumerate()
    {
        patches.push((mark_rec + i * 4 + 2, b.pos() - marks));
        b.anchor3(x, x, Some(vi(xi)), Some(vi(yi)));
    }
    for (i, (x, xi)) in [(5i16, 5u16), (6, 6)].into_iter().enumerate() {
        patches.push((base_rec + i * 2, b.pos() - bases));
        b.anchor3(x, x, Some(vi(xi)), None);
    }
    b.label("mcov");
    b.bytes.extend_from_slice(&coverage(&[20, 21]));
    b.label("bcov");
    b.bytes.extend_from_slice(&coverage(&[5, 6]));
    let mut src = b.finish();
    for (slot, rel) in patches {
        src[slot..slot + 2].copy_from_slice(&(rel as u16).to_be_bytes());
    }

    let map = keep(&[(0, 0), (6, 1), (21, 2)]);
    let out = rewrite(4, &src, &map);
    assert_eq!(
        resolved(4, &out),
        [
            Resolved::Inner(21),
            Resolved::Inner(121),
            Resolved::Inner(6)
        ]
    );
    let parsed = MarkBasePos::parse(&out).unwrap();
    let attach = parsed.attach(2, 1).unwrap();
    assert_eq!(attach.mark_anchor.x, 2);
    assert_eq!(attach.base_anchor.x, 6);
}

/// MarkLigPos: the component anchors live inside a LigatureAttach, and
/// their devices still resolve against each anchor after the rebuild.
#[test]
fn mark_lig_anchors_carry_their_devices() {
    let mut b = Builder::default();
    b.u16(1).offset_to("mcov").offset_to("lcov").u16(1);
    b.offset_to("marks").offset_to("ligs");
    b.label("marks");
    let marks = b.pos();
    b.u16(1).u16(0);
    let mark_slot = b.pos();
    b.u16(0);
    b.label("ligs");
    let ligs = b.pos();
    b.u16(1);
    let attach_slot = b.pos();
    b.u16(0);
    let attach = b.pos();
    b.u16(2);
    let comp_slots = b.pos();
    b.u16(0).u16(0);
    let mark_anchor = b.pos();
    b.anchor3(1, 1, Some(vi(20)), Some(vi(20)));
    let comp0 = b.pos();
    b.anchor3(2, 2, Some(vi(50)), None);
    let comp1 = b.pos();
    b.anchor3(3, 3, None, Some(vi(51)));
    b.label("mcov");
    b.bytes.extend_from_slice(&coverage(&[20]));
    b.label("lcov");
    b.bytes.extend_from_slice(&coverage(&[9]));
    let mut src = b.finish();
    for (slot, value) in [
        (mark_slot, mark_anchor - marks),
        (attach_slot, attach - ligs),
        (comp_slots, comp0 - attach),
        (comp_slots + 2, comp1 - attach),
    ] {
        src[slot..slot + 2].copy_from_slice(&(value as u16).to_be_bytes());
    }

    let out = rewrite(5, &src, &keep(&[(0, 0), (9, 1), (20, 2)]));
    assert_eq!(
        resolved(5, &out),
        [
            Resolved::Inner(20),
            Resolved::Inner(20),
            Resolved::Inner(50),
            Resolved::Inner(51)
        ]
    );
}

const RUBIK: &[u8] = include_bytes!("../../../../tests/fixtures/rubik_vf.ttf");

/// Subsets Rubik to a handful of Latin letters and combining marks,
/// enough to keep kerning pairs and mark attachments alive.
fn rubik_subset_gpos(retain_variations: bool) -> Vec<u8> {
    let face = sigilbuzz::Face::parse_bytes(RUBIK, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let gids = "AVTWYaovyqxbf\u{300}\u{301}\u{303}\u{308}\u{323}"
        .chars()
        .filter_map(|c| cmap.glyph_id(c))
        .collect();
    let input = crate::SubsetInput {
        gids,
        retain_variations,
        ..Default::default()
    };
    let bytes = crate::subset(&face, &input).unwrap().bytes;
    let out = sigilbuzz::Face::parse_bytes(&bytes, 0).unwrap();
    out.table_bytes(sigilbuzz::tables::tag::GPOS)
        .unwrap()
        .to_vec()
}

/// Every non-null device slot of a GPOS table: `Some((outer, inner))`
/// for a VariationIndex, `None` for anything else.
fn gpos_device_targets(gpos: &[u8]) -> Vec<Option<(u16, u16)>> {
    let mut buf = gpos.to_vec();
    let mut out = Vec::new();
    assert!(walk_gpos_device_slots(&mut buf, &mut |b, slot| {
        if let Some(target) = slot.target(b) {
            let is_vi = slot.delta_format(b) == Some(0x8000);
            out.push(is_vi.then(|| (u16_at(b, target), u16_at(b, target + 2))));
        }
    }));
    out
}

/// Rubik's anchors and PairSet kerning vary through VariationIndex
/// tables. After a subset every surviving slot must still resolve to
/// one of the source's rows; a verbatim copy of the records would leave
/// the offsets pointing into unrelated bytes of the rebuilt subtables.
#[test]
fn rubik_subset_device_slots_resolve_to_source_rows() {
    let face = sigilbuzz::Face::parse_bytes(RUBIK, 0).unwrap();
    let source_gpos = face.table_bytes(sigilbuzz::tables::tag::GPOS).unwrap();
    let source_rows: alloc::collections::BTreeSet<(u16, u16)> = gpos_device_targets(source_gpos)
        .into_iter()
        .flatten()
        .collect();

    let targets = gpos_device_targets(&rubik_subset_gpos(true));
    assert!(targets.len() > 50, "expected surviving device slots");
    for row in targets {
        let row = row.expect("every slot resolves to a VariationIndex");
        assert!(
            source_rows.contains(&row),
            "row {row:?} is not in the source"
        );
    }
}

/// A static subset (`retain_variations = false`) drops the GDEF
/// ItemVariationStore, so no GPOS slot may still reference it.
#[test]
fn rubik_static_subset_keeps_no_variation_indices() {
    let targets = gpos_device_targets(&rubik_subset_gpos(false));
    assert!(targets.iter().all(Option::is_none), "{targets:?}");
}
