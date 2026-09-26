//! Folds ligature caret variations into static carets when instancing.
//!
//! A CaretValue format 3 names a Device or VariationIndex table through
//! an offset measured from the CaretValue itself (see [`super::lig_caret`]
//! for the layout). When the instancer bakes a variable font and drops
//! the GDEF ItemVariationStore, those carets would fall back to their
//! default-instance coordinate. This pass resolves each VariationIndex
//! at the instance's coords, adds the rounded delta to the caret's
//! coordinate, and clears the offset, the same fold the GPOS bake
//! applies to anchors (see [`crate::gpos_var`]).

use sigilbuzz::tables::variation_store::ItemVariationStore;

use crate::gpos_var::{fold_one_field, DeviceSlot};
use crate::util::{WorkBudget, WORK_LIMIT};

fn read_u16(buf: &[u8], pos: usize) -> Option<usize> {
    let bytes = buf.get(pos..pos.checked_add(2)?)?;
    Some(usize::from(u16::from_be_bytes([bytes[0], bytes[1]])))
}

/// Folds every format 3 caret of the GDEF table in `gdef` in place.
///
/// Like the GPOS bake, the walk is lenient: offsets that run past the
/// table, or structures too short to hold what they claim, are skipped
/// and left as they are. A caret shared by several ligatures is folded
/// once, and its cleared offset makes later visits no-ops. Many
/// ligatures can share one LigGlyph, so the walk charges a
/// [`WorkBudget`] for every caret it visits and stops once it runs out.
pub(crate) fn fold_caret_variations(
    gdef: &mut [u8],
    store: Option<&ItemVariationStore<'_>>,
    coords: &[f32],
) {
    let Some(list) = read_u16(gdef, 8).filter(|&off| off != 0) else {
        return;
    };
    let Some(count) = read_u16(gdef, list + 2) else {
        return;
    };
    let budget = WorkBudget::new(WORK_LIMIT);
    for i in 0..count {
        let Some(lig) = read_u16(gdef, list + 4 + i * 2).filter(|&r| r != 0) else {
            continue;
        };
        let lig = list + lig;
        let Some(carets) = read_u16(gdef, lig) else {
            continue;
        };
        if !budget.spend(1 + carets) {
            return;
        }
        for k in 0..carets {
            let Some(rel) = read_u16(gdef, lig + 2 + k * 2).filter(|&r| r != 0) else {
                continue;
            };
            let caret = lig + rel;
            if read_u16(gdef, caret) == Some(3) && caret + 6 <= gdef.len() {
                let slot = DeviceSlot {
                    base: caret,
                    field: Some(caret + 2),
                    slot: caret + 4,
                };
                fold_one_field(gdef, slot, store, coords);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::fold_caret_variations;
    use sigilbuzz::tables::variation_store::ItemVariationStore;

    fn u16_at(buf: &[u8], pos: usize) -> u16 {
        u16::from_be_bytes([buf[pos], buf[pos + 1]])
    }

    /// One region peaking at 1.0 with deltas 100 (item 0) and 7 (item 1).
    fn store() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&12u32.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&22u32.to_be_bytes());
        for v in [1u16, 1, 0, 0x4000, 0x4000, 2, 1, 1, 0, 100, 7] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// A GDEF 1.0 with only a LigCaretList: one ligature whose carets
    /// are format 1 (coord 300), format 3 naming VariationIndex item 1,
    /// and format 3 naming a hinting Device. Both device offsets are
    /// 50: measured from each CaretValue they reach the real tables; a
    /// decoy VariationIndex (item 0) sits 50 bytes from the GDEF start.
    fn gdef() -> (Vec<u8>, [usize; 3]) {
        let mut out = vec![0u8; 12];
        out[0..2].copy_from_slice(&1u16.to_be_bytes());
        out[8..10].copy_from_slice(&12u16.to_be_bytes());
        // LigCaretList at 12: coverage at 18, one LigGlyph at 24.
        for v in [6u16, 1, 12] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&[0, 1, 0, 1, 0, 9]);
        // LigGlyph at 24: carets at 32, 36 and 42.
        for v in [3u16, 8, 12, 18] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        let carets = [32, 36, 42];
        for v in [1u16, 300, 3, 500, 50, 3, 600, 50] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.resize(100, 0);
        out[50..56].copy_from_slice(&[0, 0, 0, 0, 0x80, 0]);
        out[86..92].copy_from_slice(&[0, 0, 0, 1, 0x80, 0]);
        out[92..100].copy_from_slice(&[0, 9, 0, 12, 0, 1, 0x40, 0]);
        (out, carets)
    }

    #[test]
    fn format3_carets_fold_against_the_caret_value() {
        let (mut gdef, [plain, varied, hinted]) = gdef();
        let ivs = store();
        let store = ItemVariationStore::parse(&ivs).unwrap();
        fold_caret_variations(&mut gdef, Some(&store), &[1.0]);
        assert_eq!(u16_at(&gdef, plain + 2), 300, "format 1 untouched");
        assert_eq!(u16_at(&gdef, varied + 2), 507, "item 1, not the decoy");
        assert_eq!(u16_at(&gdef, varied + 4), 0);
        assert_eq!(u16_at(&gdef, hinted + 2), 600, "hinting is not folded");
        assert_eq!(u16_at(&gdef, hinted + 4), 0);
    }

    #[test]
    fn truncated_lists_are_left_alone() {
        let (gdef, _) = gdef();
        for len in [9, 14, 20, 26, 34] {
            let mut cut = gdef[..len].to_vec();
            let before = cut.clone();
            fold_caret_variations(&mut cut, None, &[1.0]);
            assert_eq!(cut, before, "cut at {len}");
        }
    }
}
