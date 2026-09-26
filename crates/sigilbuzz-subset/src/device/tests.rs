use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::Error;

use super::{copy_anchor, device_table, read_device, relocate_value_records, Dedup, RecordRun};
use crate::warnings::{Diag, Warnings};

fn u16_at(buf: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([buf[pos], buf[pos + 1]])
}

fn variation_index(outer: u16, inner: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&outer.to_be_bytes());
    out.extend_from_slice(&inner.to_be_bytes());
    out.extend_from_slice(&0x8000u16.to_be_bytes());
    out
}

fn device(start: u16, end: u16, format: u16, words: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&start.to_be_bytes());
    out.extend_from_slice(&end.to_be_bytes());
    out.extend_from_slice(&format.to_be_bytes());
    out.resize(6 + words * 2, 0x11);
    out
}

/// Prefixes `table` with `pad` filler bytes so it sits at a non-zero
/// offset of its parent.
fn at_offset(pad: usize, table: &[u8]) -> Vec<u8> {
    let mut out = vec![0xEEu8; pad];
    out.extend_from_slice(table);
    out
}

#[test]
fn variation_index_is_six_bytes() {
    let parent = at_offset(4, &variation_index(1, 2));
    assert_eq!(device_table(&parent, 4), Some(&parent[4..10]));
}

#[test]
fn device_length_follows_the_packed_delta_count() {
    // (format, ppem count, expected words): 2, 4, and 8 bits per delta.
    for (format, count, words) in [
        (1u16, 1u16, 1usize),
        (1, 8, 1),
        (1, 9, 2),
        (2, 4, 1),
        (2, 5, 2),
        (3, 2, 1),
        (3, 3, 2),
    ] {
        let table = device(10, 10 + count - 1, format, words);
        let parent = at_offset(2, &table);
        assert_eq!(
            device_table(&parent, 2).map(<[u8]>::len),
            Some(6 + words * 2),
            "format {format}, {count} sizes"
        );
    }
}

#[test]
fn device_table_rejects_bad_input() {
    // Null offset, offset past the end, and a truncated header.
    let vi = variation_index(0, 0);
    assert_eq!(device_table(&vi, 0), None);
    assert_eq!(device_table(&vi, 64), None);
    assert_eq!(device_table(&vi[..5], 0x0001), None);
    // Delta formats the spec does not define.
    for format in [0u16, 4, 0x7FFF, 0x8001] {
        let parent = at_offset(2, &device(9, 9, format, 1));
        assert_eq!(device_table(&parent, 2), None, "format {format:#x}");
    }
    // endSize below startSize.
    let parent = at_offset(2, &device(12, 9, 1, 1));
    assert_eq!(device_table(&parent, 2), None);
    // Delta words cut short: 9 sizes at 2 bits need two words.
    let parent = at_offset(2, &device(1, 9, 1, 1));
    assert_eq!(device_table(&parent, 2), None);
}

fn anchor3(x_dev: u16, y_dev: u16) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [3u16, 100, 200, x_dev, y_dev] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

#[test]
fn copy_anchor_formats_1_and_2_are_verbatim() {
    let fmt1 = [0u8, 1, 0, 10, 0xFF, 0xF6];
    let fmt2 = [0u8, 2, 0, 10, 0, 20, 0, 7];
    for anchor in [&fmt1[..], &fmt2[..]] {
        let parent = at_offset(6, anchor);
        assert_eq!(copy_anchor(&parent, 6, true, &Diag::NONE), anchor);
    }
}

#[test]
fn copy_anchor_format3_brings_its_devices_along() {
    // Anchor at parent offset 8, x device 12 bytes past the anchor, y
    // device 24 bytes past it. The copy must hold both tables and
    // point at them relative to the copy's own start.
    let mut anchor = anchor3(12, 24);
    anchor.resize(12, 0);
    anchor.extend_from_slice(&variation_index(0, 5));
    anchor.resize(24, 0);
    anchor.extend_from_slice(&device(12, 13, 1, 1));
    let parent = at_offset(8, &anchor);

    let copy = copy_anchor(&parent, 8, true, &Diag::NONE);
    assert_eq!(&copy[..6], &anchor[..6]);
    assert_eq!(u16_at(&copy, 6), 10);
    assert_eq!(u16_at(&copy, 8), 16);
    assert_eq!(&copy[10..16], &variation_index(0, 5)[..]);
    assert_eq!(&copy[16..24], &device(12, 13, 1, 1)[..]);
    assert_eq!(copy.len(), 24);
}

/// A static subset has no ItemVariationStore: VariationIndex tables
/// are left out and their slots cleared, hinting Device tables stay.
#[test]
fn static_copies_leave_variation_indices_out() {
    let mut anchor = anchor3(12, 24);
    anchor.resize(12, 0);
    anchor.extend_from_slice(&variation_index(0, 5));
    anchor.resize(24, 0);
    anchor.extend_from_slice(&device(12, 13, 1, 1));
    let parent = at_offset(8, &anchor);
    let copy = copy_anchor(&parent, 8, false, &Diag::NONE);
    assert_eq!(u16_at(&copy, 6), 0, "x VariationIndex left out");
    assert_eq!(u16_at(&copy, 8), 10, "y Device kept");
    assert_eq!(&copy[10..], &device(12, 13, 1, 1)[..]);

    // ValueRecords: xAdvDevice names a VariationIndex, yAdvDevice a
    // Device. Only the Device is copied.
    let mut src = vec![0u8, 4, 0, 10];
    src.extend_from_slice(&variation_index(0, 1));
    src.extend_from_slice(&device(9, 9, 1, 1));
    let mut out = src[..4].to_vec();
    let fits = relocate_value_records(
        &mut out,
        0,
        &src,
        &RecordRun {
            first: 0,
            count: 1,
            stride: 4,
            records: &[(0, 0x00C0)],
            keep_variations: false,
        },
        &Diag::NONE,
    );
    assert!(fits);
    assert_eq!(u16_at(&out, 0), 0);
    assert_eq!(u16_at(&out, 2), 4);
    assert_eq!(&out[4..], &device(9, 9, 1, 1)[..]);
}

#[test]
fn copy_anchor_format3_shares_one_copy_of_a_shared_device() {
    let mut anchor = anchor3(10, 10);
    anchor.extend_from_slice(&variation_index(2, 3));
    let parent = at_offset(4, &anchor);
    let copy = copy_anchor(&parent, 4, true, &Diag::NONE);
    assert_eq!(copy.len(), 16);
    assert_eq!(u16_at(&copy, 6), 10);
    assert_eq!(u16_at(&copy, 8), 10);
}

#[test]
fn copy_anchor_format3_clears_null_and_unusable_devices() {
    // x: null. y: points at a table with an undefined delta format.
    let mut anchor = anchor3(0, 10);
    anchor.extend_from_slice(&device(1, 1, 7, 1));
    let parent = at_offset(2, &anchor);
    let copy = copy_anchor(&parent, 2, true, &Diag::NONE);
    assert_eq!(copy.len(), 10);
    assert_eq!(u16_at(&copy, 6), 0);
    assert_eq!(u16_at(&copy, 8), 0);
}

#[test]
fn copy_anchor_rejects_null_truncated_and_unknown_anchors() {
    let anchor = anchor3(0, 0);
    let parent = at_offset(2, &anchor);
    assert!(copy_anchor(&parent, 0, true, &Diag::NONE).is_empty());
    assert!(copy_anchor(&parent, 100, true, &Diag::NONE).is_empty());
    assert!(
        copy_anchor(&parent[..10], 2, true, &Diag::NONE).is_empty(),
        "format 3 cut short"
    );
    let unknown = at_offset(2, &[0, 9, 0, 1, 0, 1]);
    assert!(copy_anchor(&unknown, 2, true, &Diag::NONE).is_empty());
}

#[test]
fn dedup_returns_the_first_position_for_repeat_blobs() {
    let mut out = vec![0u8; 3];
    let mut pool = Dedup::default();
    assert_eq!(pool.place(&mut out, &[1, 2]), 3);
    assert_eq!(pool.place(&mut out, &[3]), 5);
    assert_eq!(pool.place(&mut out, &[1, 2]), 3);
    assert_eq!(out, [0, 0, 0, 1, 2, 3]);
}

/// Two SinglePos-style records (xAdvance + xAdvDevice) copied into a
/// new parent that starts at `out_base = 4` in the output buffer. The
/// source offsets are relative to the source parent; after relocation
/// they are relative to the new parent and point at deduplicated
/// copies appended to the buffer.
#[test]
fn relocate_value_records_copies_and_repoints_devices() {
    let format = 0x0044u16;
    // Source parent: records at 0 and 4, tables at 8 (VI) and 14 (VI).
    let mut src = Vec::new();
    for (adv, dev) in [(10i16, 8u16), (20, 14)] {
        src.extend_from_slice(&adv.to_be_bytes());
        src.extend_from_slice(&dev.to_be_bytes());
    }
    src.extend_from_slice(&variation_index(0, 1));
    src.extend_from_slice(&variation_index(0, 2));

    // Output: 4 bytes of unrelated prefix, then a 2-byte parent header,
    // then the three records (the second source record twice).
    let mut out = vec![0xAAu8; 4];
    out.extend_from_slice(&[0, 3]);
    out.extend_from_slice(&src[0..4]);
    out.extend_from_slice(&src[4..8]);
    out.extend_from_slice(&src[4..8]);
    relocate_value_records(
        &mut out,
        4,
        &src,
        &RecordRun {
            first: 6,
            count: 3,
            stride: 4,
            records: &[(0, format)],
            keep_variations: true,
        },
        &Diag::NONE,
    );
    // Copies land at 18 (inner 1) and 24 (inner 2); offsets from 4.
    assert_eq!(out.len(), 30);
    assert_eq!(u16_at(&out, 8), 14);
    assert_eq!(u16_at(&out, 12), 20);
    assert_eq!(u16_at(&out, 16), 20, "identical table shares the copy");
    assert_eq!(&out[18..24], &variation_index(0, 1)[..]);
    assert_eq!(&out[24..30], &variation_index(0, 2)[..]);
    // Static fields untouched.
    assert_eq!(u16_at(&out, 6), 10);
    assert_eq!(u16_at(&out, 10), 20);
}

/// PairValueRecord groups carry two ValueRecords with different
/// formats; both records' device slots must be found.
#[test]
fn relocate_value_records_walks_every_record_of_a_group() {
    // Group: u16 secondGlyph, VR1 = xPla + xPlaDev, VR2 = yAdvDev.
    let (vf1, vf2) = (0x0011u16, 0x0080u16);
    let mut src = Vec::new();
    src.extend_from_slice(&[0, 9, 0, 5, 0, 8, 0, 14]);
    src.extend_from_slice(&variation_index(1, 1));
    src.extend_from_slice(&variation_index(1, 2));
    let mut out = src[..8].to_vec();
    relocate_value_records(
        &mut out,
        0,
        &src,
        &RecordRun {
            first: 2,
            count: 1,
            stride: 6,
            records: &[(0, vf1), (4, vf2)],
            keep_variations: true,
        },
        &Diag::NONE,
    );
    assert_eq!(u16_at(&out, 4), 8);
    assert_eq!(u16_at(&out, 6), 14);
    assert_eq!(&out[8..], &src[8..]);
}

#[test]
fn relocate_value_records_clears_unusable_and_overflowing_slots() {
    let format = 0x0040u16; // xAdvDevice only
    let mut src = vec![0u8, 4, 0, 60];
    src.extend_from_slice(&variation_index(0, 0));
    // Record 0 names the table at 4; record 1 points past the end.
    let mut out = src[..4].to_vec();
    relocate_value_records(
        &mut out,
        0,
        &src,
        &RecordRun {
            first: 0,
            count: 2,
            stride: 2,
            records: &[(0, format)],
            keep_variations: true,
        },
        &Diag::NONE,
    );
    assert_eq!(u16_at(&out, 0), 4);
    assert_eq!(u16_at(&out, 2), 0);

    // A copy that would land more than 64 KiB past the parent start
    // cannot be addressed by an Offset16: the slot clears instead.
    let mut far = vec![0u8, 4];
    far.resize(70_000, 0);
    let fits = relocate_value_records(
        &mut far,
        0,
        &src,
        &RecordRun {
            first: 0,
            count: 1,
            stride: 2,
            records: &[(0, format)],
            keep_variations: true,
        },
        &Diag::NONE,
    );
    assert_eq!(u16_at(&far, 0), 0);
    assert!(!fits, "the overflow is reported");
}

/// `(offset, context)` of every warning `run` raises against `parent`.
fn warned(parent: &[u8], run: impl FnOnce(&Diag<'_>)) -> Vec<(usize, &'static str)> {
    let sink = Warnings::default();
    run(&Diag::new(&sink).for_table(*b"GPOS", parent));
    sink.into_sorted()
        .iter()
        .map(|w| (w.offset, w.context))
        .collect()
}

#[test]
fn read_device_locates_each_fault_from_the_parent() {
    let offset_of = |parent: &[u8], at: usize| match read_device(parent, at) {
        Err(Error::Truncated { offset, .. } | Error::Malformed { offset, .. }) => offset,
        other => panic!("expected an error, got {other:?}"),
    };
    let vi = at_offset(2, &variation_index(0, 0));
    assert_eq!(read_device(&vi, 0), Ok(None), "a null offset is no table");
    assert_eq!(offset_of(&vi, 64), 64, "past the end");
    assert_eq!(offset_of(&vi[..6], 2), 6, "header cut short");
    let parent = at_offset(2, &device(9, 9, 7, 1));
    assert_eq!(offset_of(&parent, 2), 6, "undefined deltaFormat");
    let parent = at_offset(2, &device(12, 9, 1, 1));
    assert_eq!(offset_of(&parent, 2), 4, "endSize below startSize");
    let parent = at_offset(2, &device(1, 9, 1, 1));
    assert_eq!(offset_of(&parent, 2), 8, "deltaValues cut short");
}

#[test]
fn unusable_devices_and_anchors_are_reported() {
    // An anchor whose y device has an undefined delta format: the slot
    // is cleared and the table reported where its format sits.
    let mut anchor = anchor3(0, 10);
    anchor.extend_from_slice(&device(1, 1, 7, 1));
    let parent = at_offset(2, &anchor);
    let found = warned(&parent, |diag| {
        assert_eq!(u16_at(&copy_anchor(&parent, 2, true, diag), 8), 0);
    });
    assert_eq!(found, [(16, "unknown Device deltaFormat")]);

    // Anchors that cannot be copied at all.
    let unknown = at_offset(2, &[0, 9, 0, 1, 0, 1]);
    let found = warned(&unknown, |diag| {
        assert!(copy_anchor(&unknown, 2, true, diag).is_empty());
    });
    assert_eq!(found, [(2, "unknown Anchor format")]);
    let cut = at_offset(2, &anchor3(0, 0)[..7]);
    let found = warned(&cut, |diag| {
        assert!(copy_anchor(&cut, 2, true, diag).is_empty());
        assert!(copy_anchor(&cut, 40, true, diag).is_empty());
    });
    assert_eq!(
        found,
        [
            (2, "Anchor table truncated"),
            (40, "Anchor offset past the end")
        ]
    );
}

#[test]
fn unusable_value_record_devices_are_reported() {
    // One record (xAdvance + xAdvDevice) whose device offset runs past
    // the source parent.
    let mut src = Vec::new();
    for v in [0u16, 40] {
        src.extend_from_slice(&v.to_be_bytes());
    }
    let mut out = src.clone();
    let found = warned(&src, |diag| {
        relocate_value_records(
            &mut out,
            0,
            &src,
            &RecordRun {
                first: 0,
                count: 1,
                stride: 4,
                records: &[(0, 0x0044)],
                keep_variations: true,
            },
            diag,
        );
    });
    assert_eq!(u16_at(&out, 2), 0, "the slot is cleared");
    assert_eq!(
        found,
        [(
            40,
            "Device or VariationIndex offset past the end of its parent"
        )]
    );
}
