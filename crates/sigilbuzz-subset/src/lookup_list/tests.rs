use alloc::vec;
use alloc::vec::Vec;

use sigilbuzz::tables::layout::LookupList;

use super::emit;
use crate::layout::{RewrittenLookup, RewrittenSubtable};

const GPOS_EXTENSION: u16 = 9;

fn lookup(lookup_type: u16, flag: u16, set: Option<u16>, subs: &[Vec<u8>]) -> RewrittenLookup {
    RewrittenLookup {
        lookup_type,
        lookup_flag: flag,
        mark_filtering_set: set,
        subtables: subs
            .iter()
            .map(|b| RewrittenSubtable { bytes: b.clone() })
            .collect(),
    }
}

/// A subtable body of `len` bytes, recognizable by its fill byte.
fn body(fill: u8, len: usize) -> Vec<u8> {
    vec![fill; len]
}

/// `(type, flag, mark filtering set, (subtable type, body) pairs)`.
type LookupBack = (u16, u16, Option<u16>, Vec<(u16, Vec<u8>)>);

/// Reads back every lookup, unwrapping extension subtables to their
/// inner type and body.
fn read_back(list: &[u8]) -> Vec<LookupBack> {
    let parsed = LookupList::parse(list).unwrap();
    (0..parsed.len())
        .map(|i| {
            let l = parsed.get(i).unwrap();
            let subs = (0..l.subtable_count())
                .map(|j| {
                    let bytes = l.subtable_bytes(j).unwrap();
                    if l.lookup_type() == GPOS_EXTENSION {
                        let inner = u16::from_be_bytes([bytes[2], bytes[3]]);
                        let off =
                            u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
                        (inner, bytes[off..].to_vec())
                    } else {
                        (l.lookup_type(), bytes.to_vec())
                    }
                })
                .collect();
            (l.lookup_type(), l.flag(), l.mark_filtering_set(), subs)
        })
        .collect()
}

/// Checks that subtable `(type, body)` pairs start with the expected
/// bodies (subtable slices run to the end of the list, so compare
/// prefixes).
fn assert_bodies(got: &[(u16, Vec<u8>)], want: &[(u16, &[u8])]) {
    assert_eq!(got.len(), want.len());
    for ((gt, gb), (wt, wb)) in got.iter().zip(want) {
        assert_eq!(gt, wt);
        assert_eq!(&gb[..wb.len()], *wb);
    }
}

#[test]
fn small_tables_keep_the_compact_layout() {
    let lookups = [
        lookup(1, 0, None, &[body(1, 6), body(2, 8)]),
        lookup(6, 0x0010, Some(3), &[body(3, 12)]),
    ];
    let list = emit(&lookups, GPOS_EXTENSION).unwrap();
    // Header + one offset per lookup, then each lookup and its bodies.
    assert_eq!(u16::from_be_bytes([list[2], list[3]]), 6);
    let back = read_back(&list);
    assert_eq!((back[0].0, back[1].0), (1, 6));
    assert_eq!((back[1].1, back[1].2), (0x0010, Some(3)));
    assert_bodies(&back[0].3, &[(1, &body(1, 6)), (1, &body(2, 8))]);
    assert_bodies(&back[1].3, &[(6, &body(3, 12))]);
}

/// Four 30 KB lookups cannot be addressed with Offset16s: every lookup
/// becomes an Extension lookup and every body is still reachable.
#[test]
fn lookups_past_64_kib_become_extension_lookups() {
    let lookups: Vec<RewrittenLookup> = (0..4u8)
        .map(|i| lookup(u16::from(i) + 1, u16::from(i), None, &[body(i + 1, 30_000)]))
        .collect();
    let list = emit(&lookups, GPOS_EXTENSION).unwrap();
    let back = read_back(&list);
    assert_eq!(back.len(), 4);
    for (i, (lookup_type, flag, set, subs)) in back.iter().enumerate() {
        assert_eq!(*lookup_type, GPOS_EXTENSION);
        assert_eq!(*flag, i as u16);
        assert_eq!(*set, None);
        assert_bodies(subs, &[(i as u16 + 1, &body(i as u8 + 1, 30_000))]);
    }
}

/// A lookup that already was an Extension lookup is unwrapped, so the
/// output never nests one extension inside another, and a mark
/// filtering set survives the move to the extension layout.
#[test]
fn extension_layout_unwraps_existing_extensions() {
    // The first lookup alone pushes the second header past 64 KiB.
    let mut wrapped = vec![0, 1, 0, 2, 0, 0, 0, 8];
    wrapped.extend_from_slice(&body(7, 70_000));
    let lookups = [
        lookup(GPOS_EXTENSION, 0, None, &[wrapped]),
        lookup(4, 0x0010, Some(1), &[body(8, 100)]),
    ];
    let list = emit(&lookups, GPOS_EXTENSION).unwrap();
    let back = read_back(&list);
    assert_eq!(back[0].0, GPOS_EXTENSION);
    assert_bodies(&back[0].3, &[(2, &body(7, 70_000))]);
    assert_eq!((back[1].0, back[1].2), (GPOS_EXTENSION, Some(1)));
    assert_bodies(&back[1].3, &[(4, &body(8, 100))]);
    // One 8-byte extension subtable per body: bodies do not nest.
    let header = u16::from_be_bytes([list[2], list[3]]) as usize;
    let first_ext = header + u16::from_be_bytes([list[header + 6], list[header + 7]]) as usize;
    assert_eq!(
        u16::from_be_bytes([list[first_ext + 2], list[first_ext + 3]]),
        2
    );
    let body_at = first_ext
        + u32::from_be_bytes([
            list[first_ext + 4],
            list[first_ext + 5],
            list[first_ext + 6],
            list[first_ext + 7],
        ]) as usize;
    assert_eq!(list[body_at], 7, "the body, not a second extension header");
}

#[test]
fn emit_is_deterministic() {
    let lookups: Vec<RewrittenLookup> = (0..3u8)
        .map(|i| lookup(2, 0, None, &[body(i, 25_000), body(i, 9)]))
        .collect();
    assert_eq!(emit(&lookups, 9), emit(&lookups, 9));
}

/// A lookup split into more subtables than a 16-bit count holds cannot
/// be written: its count would wrap.
#[test]
fn a_subtable_count_past_16_bits_is_refused() {
    let subs: Vec<Vec<u8>> = (0..=usize::from(u16::MAX)).map(|_| Vec::new()).collect();
    let lookups = [lookup(1, 0, None, &subs)];
    assert!(emit(&lookups, 7).is_none());
}
