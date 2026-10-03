//! `vmtx` and `vhea` subsetting.
//!
//! `vmtx` mirrors `hmtx`: `numberOfLongVerMetrics` long records
//! (advance height and top side bearing), then a tail of top side
//! bearings for glyphs that share the last advance. The kept glyphs are
//! re-emitted in new-gid order with the same trailing fold as `hmtx`,
//! and `vhea.numberOfLongVerMetrics` is patched to match. Every other
//! `vhea` field (the vertical ascent, descent and line gap, the
//! extents, the caret) is a font-wide value and passes through.
//!
//! The two tables travel together. A `vhea` or `vmtx` that is
//! malformed, or that comes without its partner, is left out of the
//! subset with its partner and reported as a warning. The subset then
//! lays vertical text out with the shaper's fallbacks, as every subset
//! did before these tables were kept.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::Face;

use crate::hmtx::emit_long_metrics;
use crate::util;
use crate::warnings::Warnings;
use crate::GlyphId;

/// What a warning about a vertical metrics table left out.
const DROPPED: &str = "the vhea and vmtx tables";

/// Rebuilt `vhea` and `vmtx` for a subset.
pub(crate) struct VerticalMetrics {
    /// The source `vhea` with `numberOfLongVerMetrics` patched.
    pub(crate) vhea: Vec<u8>,
    /// The rebuilt `vmtx`.
    pub(crate) vmtx: Vec<u8>,
}

/// Subsets `vhea` and `vmtx` for `kept` (kept gids in new-gid order).
/// Returns `None` when the source has neither table, or when they
/// cannot be kept, in which case the reason is in `warnings`.
pub(crate) fn subset_vertical_metrics(
    face: &Face<'_>,
    kept: &[GlyphId],
    warnings: &Warnings,
) -> Option<VerticalMetrics> {
    let has_vhea = face.record(tag::VHEA).is_some();
    let has_vmtx = face.record(tag::VMTX).is_some();
    match (has_vhea, has_vmtx) {
        (false, false) => return None,
        (true, false) => {
            warnings.push(tag::VHEA, 0, "vhea without a vmtx table", DROPPED);
            return None;
        }
        (false, true) => {
            warnings.push(tag::VMTX, 0, "vmtx without a vhea table", DROPPED);
            return None;
        }
        (true, true) => {}
    }
    // Parse `vhea` on its own first, so a problem there is reported
    // against `vhea` rather than the `vmtx` that depends on it.
    if let Err(e) = face.vhea() {
        warnings.parse_error(tag::VHEA, 0, &e, DROPPED);
        return None;
    }
    let vmtx = match face.vmtx() {
        Ok(Some(vmtx)) => vmtx,
        Ok(None) => return None,
        Err(e) => {
            warnings.parse_error(tag::VMTX, 0, &e, DROPPED);
            return None;
        }
    };

    let advances: Vec<u16> = kept
        .iter()
        .map(|&old_gid| vmtx.advance(old_gid).unwrap_or(0))
        .collect();
    let tsbs: Vec<i16> = kept
        .iter()
        .map(|&old_gid| vmtx.tsb(old_gid).unwrap_or(0))
        .collect();
    let (vmtx_out, long_count) = emit_long_metrics(&advances, &tsbs);

    // `face.vhea()` parsed the table, so it holds the count at byte 34
    // and the patch cannot fail; stay total anyway.
    let mut vhea_out = face.table_bytes(tag::VHEA).ok()?.to_vec();
    if util::write_vhea_metrics_count(&mut vhea_out, long_count).is_err() {
        warnings.push(tag::VHEA, 0, "vhea too short to patch", DROPPED);
        return None;
    }
    Some(VerticalMetrics {
        vhea: vhea_out,
        vmtx: vmtx_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use sigilbuzz::tables::{Vhea, Vmtx};

    /// A `vhea` of `len` bytes (36 for a plain table) whose
    /// `numberOfLongVerMetrics` is `long`.
    fn vhea_bytes(long: u16, len: usize) -> Vec<u8> {
        let mut b = Vec::with_capacity(len.max(36));
        b.extend_from_slice(&1u16.to_be_bytes()); // major
        b.extend_from_slice(&0x1000u16.to_be_bytes()); // minor (1.1)
        b.extend_from_slice(&500i16.to_be_bytes()); // vertTypoAscender
        b.extend_from_slice(&(-500i16).to_be_bytes()); // vertTypoDescender
        b.extend_from_slice(&0i16.to_be_bytes()); // vertTypoLineGap
        b.extend_from_slice(&[0; 22]); // extents, caret, reserved
        b.extend_from_slice(&0i16.to_be_bytes()); // metricDataFormat
        b.extend_from_slice(&long.to_be_bytes());
        b.resize(len.max(36), 0xEE);
        b
    }

    /// A `vmtx` body: `longs` long metrics, then `tsbs` short ones.
    fn vmtx_bytes(longs: &[(u16, i16)], tsbs: &[i16]) -> Vec<u8> {
        let mut b = Vec::new();
        for (advance, tsb) in longs {
            b.extend_from_slice(&advance.to_be_bytes());
            b.extend_from_slice(&tsb.to_be_bytes());
        }
        for tsb in tsbs {
            b.extend_from_slice(&tsb.to_be_bytes());
        }
        b
    }

    /// A `maxp` 0.5 table for `num_glyphs` glyphs.
    fn maxp_bytes(num_glyphs: u16) -> Vec<u8> {
        let mut b = 0x0000_5000u32.to_be_bytes().to_vec();
        b.extend_from_slice(&num_glyphs.to_be_bytes());
        b
    }

    /// An SFNT holding `maxp` plus the given vertical tables.
    fn font(num_glyphs: u16, vhea: Option<Vec<u8>>, vmtx: Option<Vec<u8>>) -> Vec<u8> {
        let mut tables = vec![(tag::MAXP, maxp_bytes(num_glyphs))];
        tables.extend(vhea.map(|b| (tag::VHEA, b)));
        tables.extend(vmtx.map(|b| (tag::VMTX, b)));
        crate::sfnt::build(0x4F54_544F, &tables)
    }

    /// Glyph metrics `(advance, tsb)` of the rebuilt pair, through the
    /// core parsers.
    fn read_back(out: &VerticalMetrics, glyphs: u16) -> Vec<(u16, i16)> {
        let vhea = Vhea::parse(&out.vhea).expect("vhea parses");
        let vmtx =
            Vmtx::parse(&out.vmtx, glyphs, vhea.number_of_long_ver_metrics).expect("vmtx parses");
        (0..glyphs)
            .map(|g| (vmtx.advance(g).unwrap(), vmtx.tsb(g).unwrap()))
            .collect()
    }

    #[test]
    fn long_and_short_metrics_follow_the_kept_glyphs() {
        // Six glyphs: three long metrics, then three that share the
        // last advance (1000) with their own top side bearings.
        let vmtx = vmtx_bytes(&[(1000, 10), (1200, 20), (1000, 30)], &[40, 50, 60]);
        let data = font(6, Some(vhea_bytes(3, 36)), Some(vmtx));
        let face = Face::parse_bytes(&data, 0).unwrap();
        let sink = Warnings::default();
        let out = subset_vertical_metrics(&face, &[0, 1, 4, 5], &sink).expect("kept");
        assert!(sink.into_sorted().is_empty());
        assert_eq!(
            read_back(&out, 4),
            [(1000, 10), (1200, 20), (1000, 50), (1000, 60)]
        );
        // The trailing run of 1000s starts at new gid 2, which stays
        // long; new gid 3 folds into the tail.
        let long = Vhea::parse(&out.vhea).unwrap().number_of_long_ver_metrics;
        assert_eq!(long, 3);
        assert_eq!(out.vmtx.len(), 3 * 4 + 2);
    }

    #[test]
    fn a_kept_tail_glyph_can_become_a_long_metric() {
        // Keeping a short-metric glyph followed by a long one with a
        // different advance puts the short one in the long block.
        let vmtx = vmtx_bytes(&[(900, 1), (1000, 2)], &[3]);
        let data = font(3, Some(vhea_bytes(2, 36)), Some(vmtx));
        let face = Face::parse_bytes(&data, 0).unwrap();
        let out = subset_vertical_metrics(&face, &[0, 2], &Warnings::default()).unwrap();
        assert_eq!(read_back(&out, 2), [(900, 1), (1000, 3)]);
    }

    #[test]
    fn a_padded_vhea_keeps_its_tail_and_gets_its_count_at_byte_34() {
        let vmtx = vmtx_bytes(&[(1000, 0), (800, 0), (700, 0)], &[]);
        let data = font(3, Some(vhea_bytes(3, 40)), Some(vmtx));
        let face = Face::parse_bytes(&data, 0).unwrap();
        let out = subset_vertical_metrics(&face, &[0, 2], &Warnings::default()).unwrap();
        assert_eq!(out.vhea.len(), 40);
        assert_eq!(&out.vhea[34..36], &2u16.to_be_bytes());
        assert_eq!(&out.vhea[36..], &[0xEE; 4]);
        assert_eq!(read_back(&out, 2), [(1000, 0), (700, 0)]);
    }

    /// Runs the subset on `data` and returns the `(table, offset)` of
    /// each warning, asserting that nothing was kept.
    fn dropped_with(data: &[u8]) -> Vec<([u8; 4], usize)> {
        let face = Face::parse_bytes(data, 0).unwrap();
        let sink = Warnings::default();
        assert!(subset_vertical_metrics(&face, &[0], &sink).is_none());
        sink.into_sorted()
            .iter()
            .map(|w| {
                assert_eq!(w.dropped, DROPPED);
                (w.table, w.offset)
            })
            .collect()
    }

    #[test]
    fn malformed_or_unpaired_tables_are_left_out_with_a_warning() {
        let vmtx = vmtx_bytes(&[(1000, 0)], &[0]);
        // A vmtx shorter than vhea and maxp say.
        let short = font(2, Some(vhea_bytes(1, 36)), Some(vmtx[..4].to_vec()));
        assert_eq!(dropped_with(&short), [(tag::VMTX, 4)]);
        // A truncated vhea: the parser stops at the extents it skips,
        // which start at byte 10.
        let cut = font(
            2,
            Some(vhea_bytes(1, 36)[..20].to_vec()),
            Some(vmtx.clone()),
        );
        assert_eq!(dropped_with(&cut), [(tag::VHEA, 10)]);
        // A vhea that claims no long metrics.
        let zero = font(2, Some(vhea_bytes(0, 36)), Some(vmtx.clone()));
        assert_eq!(dropped_with(&zero), [(tag::VHEA, 34)]);
        // Either table without the other.
        assert_eq!(
            dropped_with(&font(2, Some(vhea_bytes(1, 36)), None)),
            [(tag::VHEA, 0)]
        );
        assert_eq!(dropped_with(&font(2, None, Some(vmtx))), [(tag::VMTX, 0)]);
    }

    #[test]
    fn no_vertical_tables_means_nothing_to_do() {
        let data = font(2, None, None);
        let face = Face::parse_bytes(&data, 0).unwrap();
        let sink = Warnings::default();
        assert!(subset_vertical_metrics(&face, &[0, 1], &sink).is_none());
        assert!(sink.into_sorted().is_empty());
    }
}
