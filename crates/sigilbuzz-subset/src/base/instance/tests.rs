//! Unit tests for the BASE variations of an instance.

use super::*;
use alloc::vec;
use sigilbuzz::tables::base::Base;

fn be16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// A `BASE` 1.1 over two axes with a horizontal axis for `hani`:
///
/// - `ideo` at -120, varied through row (0, 0), and `romn` at 0;
/// - a default MinMax whose min is -200, varied through row (1, 0),
///   and whose max is 880 with a hinting Device table.
///
/// The store's regions peak at axis 0, at axis 1, and at both.
/// Subtable 0 uses all three, its row 0 holding `10 20 4`; subtable 1
/// only the first, its row 0 holding `-30`.
fn base_with_store() -> Vec<u8> {
    let mut b = Vec::new();
    for v in [1, 1, 12, 0] {
        be16(&mut b, v); // version 1.1, horizAxis at 12, no vertAxis
    }
    b.extend_from_slice(&96u32.to_be_bytes()); // itemVarStoreOffset
                                               // Axis at 12: BaseTagList at 16, BaseScriptList at 26.
    be16(&mut b, 4);
    be16(&mut b, 14);
    // BaseTagList at 16.
    be16(&mut b, 2);
    b.extend_from_slice(b"ideoromn");
    // BaseScriptList at 26: hani at 34.
    be16(&mut b, 1);
    b.extend_from_slice(b"hani");
    be16(&mut b, 8);
    // BaseScript at 34: BaseValues at 40, MinMax at 64.
    for v in [6, 30, 0] {
        be16(&mut b, v);
    }
    // BaseValues at 40: coordinates at 48 and 60.
    for v in [0, 2, 8, 20] {
        be16(&mut b, v);
    }
    // 48: format 3, -120, VariationIndex at 54 for row (0, 0).
    for v in [3, (-120i16) as u16, 6, 0, 0, 0x8000] {
        be16(&mut b, v);
    }
    // 60: format 1, 0.
    for v in [1, 0] {
        be16(&mut b, v);
    }
    // MinMax at 64: min at 70, max at 82, no feature extents.
    for v in [6, 18, 0] {
        be16(&mut b, v);
    }
    // 70: format 3, -200, VariationIndex at 76 for row (1, 0).
    for v in [3, (-200i16) as u16, 6, 1, 0, 0x8000] {
        be16(&mut b, v);
    }
    // 82: format 3, 880, a Device table at 88: sizes 12 to 13, two
    // bits each.
    for v in [3, 880, 6, 12, 13, 1, 0x4000] {
        be16(&mut b, v);
    }
    assert_eq!(b.len(), 96);
    // The ItemVariationStore at 96: regions at +16, subtables at +56
    // and +71.
    be16(&mut b, 1);
    b.extend_from_slice(&16u32.to_be_bytes());
    be16(&mut b, 2);
    b.extend_from_slice(&56u32.to_be_bytes());
    b.extend_from_slice(&71u32.to_be_bytes());
    be16(&mut b, 2); // axisCount
    be16(&mut b, 3); // regionCount
    for (p0, p1) in [(0x4000, 0), (0, 0x4000), (0x4000, 0x4000)] {
        for peak in [p0, p1] {
            let start = 0;
            for v in [start, peak, peak] {
                be16(&mut b, v);
            }
        }
    }
    assert_eq!(b.len(), 96 + 56);
    for v in [1, 0, 3, 0, 1, 2] {
        be16(&mut b, v); // one row, byte deltas, regions 0 1 2
    }
    b.extend_from_slice(&[10, 20, 4]);
    for v in [1, 0, 1, 0] {
        be16(&mut b, v); // one row, byte deltas, region 0
    }
    b.push((-30i8) as u8);
    b
}

/// `hani`'s `ideo` baseline and default extents in `bytes` at `coords`.
fn read(bytes: &[u8], coords: &[f32]) -> (Option<i16>, Option<(i16, i16)>) {
    let base = Base::parse(bytes).expect("BASE parses");
    let script = base
        .horizontal_axis()
        .and_then(|axis| axis.script(*b"hani"))
        .expect("hani");
    (
        script.baseline_at_coords(*b"ideo", coords),
        script.min_max(None),
    )
}

#[test]
fn a_full_instance_moves_the_coordinates_and_drops_the_store() {
    let src = base_with_store();
    let out = apply_variations(&src, &[1.0, 0.5], &[], &Warnings::default())
        .unwrap()
        .unwrap();
    // Row (0, 0): 10 + 20 * 0.5 + 4 * 0.5 = 22; row (1, 0): -30.
    assert_eq!(read(&out, &[]), (Some(-98), Some((-230, 880))));
    assert_eq!(&out[..4], &[0, 1, 0, 0], "version 1.0");
    let base = Base::parse(&out).unwrap();
    assert!(base.variation_store().is_none());
    // The store and both VariationIndex tables are gone, the header
    // lost its store offset, and the two varied coordinates are format
    // 1: 80 + 12 + 4 + 4 bytes fewer.
    assert_eq!(out.len(), src.len() - 100);
    // The hinting Device table stays with its coordinate.
    let max_at = layout(&out)
        .unwrap()
        .coords
        .into_iter()
        .find(|&(_, format)| format == 3)
        .map(|(at, _)| at)
        .expect("the max coordinate keeps format 3");
    let device = max_at + usize::from(u16::from_be_bytes([out[max_at + 4], out[max_at + 5]]));
    assert_eq!(&out[device..device + 8], &src[88..96]);
}

#[test]
fn a_partial_instance_folds_the_pinned_axis_into_the_coordinates() {
    let src = base_with_store();
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let out = apply_variations(&src, &[1.0, 0.0], &pins, &Warnings::default())
        .unwrap()
        .unwrap();
    assert_eq!(&out[..4], &[0, 1, 0, 1], "still version 1.1");
    // The default carries the pinned axis: -120 + 10. The kept axis
    // still varies it as the source does at axis 0 = 1.
    for (kept, ideo) in [(0.0, -110), (0.5, -98), (1.0, -86)] {
        let got = read(&out, &[kept]).0;
        assert_eq!(got, Some(ideo));
        assert_eq!(got, read(&src, &[1.0, kept]).0);
    }
    // The regions on axis 1 merged into one; the axis 0 one is gone.
    let base = Base::parse(&out).unwrap();
    let store = base.variation_store().expect("a store for the kept axis");
    assert_eq!(store.variation_region_count(0), Some(1));
    // The min's row varied on axis 0 only: it is static now, -200 - 30,
    // a format 1 coordinate.
    assert_eq!(read(&out, &[1.0]).1, Some((-230, 880)));
    let formats: Vec<u16> = layout(&out).unwrap().coords.values().copied().collect();
    assert_eq!(formats, vec![3, 1, 1, 3]);
}

#[test]
fn pinning_outside_a_region_drops_its_deltas() {
    // At axis 0 = -1 the regions on axis 0 give nothing: the default
    // keeps the source's coordinates, and only axis 1 still varies.
    let src = base_with_store();
    let pins = [AxisPin::Pin, AxisPin::Keep];
    let out = apply_variations(&src, &[-1.0, 0.0], &pins, &Warnings::default())
        .unwrap()
        .unwrap();
    assert_eq!(read(&out, &[0.0]), (Some(-120), Some((-200, 880))));
    assert_eq!(read(&out, &[1.0]).0, Some(-100));
    assert_eq!(read(&out, &[1.0]).0, read(&src, &[-1.0, 1.0]).0);
}

#[test]
fn tables_without_a_store_are_left_alone() {
    let mut src = base_with_store();
    src[8..12].copy_from_slice(&[0; 4]);
    assert_eq!(
        apply_variations(&src, &[1.0, 1.0], &[], &Warnings::default()).unwrap(),
        None
    );
    src[2..4].copy_from_slice(&[0, 0]);
    assert_eq!(
        apply_variations(&src, &[1.0, 1.0], &[], &Warnings::default()).unwrap(),
        None
    );
}

#[test]
fn a_malformed_table_is_left_out_with_a_warning() {
    let mut src = base_with_store();
    // The store's region list runs past the end.
    src[98..102].copy_from_slice(&1000u32.to_be_bytes());
    let font = crate::sfnt::build(0x4F54_544F, &[(tag::BASE, src)]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let sink = Warnings::default();
    assert_eq!(
        instance_base(&face, &[1.0, 1.0], &[], &sink),
        BaseBake::Dropped
    );
    let got: Vec<_> = sink.into_sorted().iter().map(|w| w.table).collect();
    assert_eq!(got, [tag::BASE]);
    // A font without BASE has nothing to change.
    let font = crate::sfnt::build(0x4F54_544F, &[(tag::HEAD, vec![0; 54])]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let sink = Warnings::default();
    assert_eq!(
        instance_base(&face, &[1.0], &[], &sink),
        BaseBake::Unchanged
    );
}
