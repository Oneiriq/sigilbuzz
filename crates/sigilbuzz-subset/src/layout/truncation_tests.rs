//! The lookup rewriters must survive truncated subtables.
//!
//! Every GSUB and GPOS subtable of the fixture fonts is cut short at
//! every length up to a few hundred bytes, and at a spread of lengths
//! past that, and run through the rewriters under a gid map that keeps
//! some glyphs and drops others. A malformed font may lose the
//! subtable, but it must never panic the subsetter.

use alloc::vec::Vec;

use sigilbuzz::Face;

use super::{GidMap, RewriterCtx};
use crate::{gpos, gsub};

const FONTS: &[&[u8]] = &[
    include_bytes!("../../../../tests/fixtures/opensans_regular.ttf"),
    include_bytes!("../../../../tests/fixtures/amiri_regular.ttf"),
    include_bytes!("../../../../tests/fixtures/rubik_vf.ttf"),
];

/// Every cut length worth trying for a subtable that runs `len` bytes
/// to the end of its table.
fn cuts(len: usize) -> impl Iterator<Item = usize> {
    let dense = len.min(160);
    let step = (len / 48).max(1);
    (0..dense).chain((dense..len).step_by(step))
}

fn sweep(face: &Face<'_>, is_gsub: bool) -> usize {
    let num_glyphs = face.maxp().unwrap().num_glyphs;
    // Keep two glyphs in three so every structure sees both outcomes.
    let kept: Vec<u16> = (0..num_glyphs).filter(|g| g % 3 != 1).collect();
    let map = GidMap::from_kept(&kept);
    let renumber: Vec<Option<u16>> = (0..512).map(|i| (i % 2 == 0).then_some(i / 2)).collect();
    let lookups = if is_gsub {
        face.gsub().unwrap().map(|t| *t.lookup_list())
    } else {
        face.gpos().unwrap().map(|t| *t.lookup_list())
    };
    let Some(lookups) = lookups else {
        return 0;
    };
    let mut runs = 0;
    for li in 0..lookups.len() {
        let lookup = lookups.get(li).unwrap();
        for si in 0..lookup.subtable_count() {
            let Some(sub) = lookup.subtable_bytes(si) else {
                continue;
            };
            for cut in cuts(sub.len()) {
                let body = [&sub[..cut]];
                for renumber in [None, Some(renumber.as_slice())] {
                    let ctx = RewriterCtx {
                        gid_map: &map,
                        lookup_renumber: renumber,
                    };
                    let kind = lookup.lookup_type();
                    if is_gsub {
                        let _ = gsub::rewrite_lookup(&ctx, kind, 0, None, &body);
                    } else {
                        let _ = gpos::rewrite_lookup(&ctx, kind, 0, None, &body);
                    }
                    runs += 1;
                }
            }
        }
    }
    runs
}

#[test]
fn truncated_gsub_subtables_do_not_panic() {
    let runs: usize = FONTS
        .iter()
        .map(|bytes| sweep(&Face::parse_bytes(bytes, 0).unwrap(), true))
        .sum();
    assert!(runs > 1000, "only {runs} rewrites ran");
}

#[test]
fn truncated_gpos_subtables_do_not_panic() {
    let runs: usize = FONTS
        .iter()
        .map(|bytes| sweep(&Face::parse_bytes(bytes, 0).unwrap(), false))
        .sum();
    assert!(runs > 1000, "only {runs} rewrites ran");
}
