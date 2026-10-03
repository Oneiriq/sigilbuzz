//! `VORG` subsetting.
//!
//! `VORG` gives the y coordinate of the vertical origin: a font-wide
//! `defaultVertOriginY`, plus a sorted list of per-glyph overrides.
//! The core parser can only answer "what is the origin of this glyph",
//! which cannot tell a glyph without an override from one whose
//! override equals the default, so the rewrite here reads the raw
//! entries.
//!
//! A subset keeps the default and the overrides of the kept glyphs,
//! renumbered and sorted by new glyph id.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::{Error, Face};

use crate::read;
use crate::warnings::Warnings;
use crate::GlyphId;

/// Size of the `VORG` header: version, default, entry count.
const HEADER_LEN: usize = 8;

/// The fields of a `VORG` table, with its entries still raw.
struct Vorg<'a> {
    minor: u16,
    default_y: i16,
    /// `(glyphIndex, vertOriginY)` records, four bytes each.
    entries: &'a [u8],
}

impl<'a> Vorg<'a> {
    /// Reads a `VORG` table. Errors carry the byte offset of the
    /// problem from the start of the table.
    fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        const CTX: &str = "VORG header truncated";
        if read::u16_at(bytes, 0, CTX)? != 1 {
            return Err(Error::Malformed {
                offset: 0,
                context: "unsupported VORG major version",
            });
        }
        let minor = read::u16_at(bytes, 2, CTX)?;
        let default_y = read::u16_at(bytes, 4, CTX)? as i16;
        let count = read::u16_at(bytes, 6, CTX)?;
        let entries = read::array_at(
            bytes,
            HEADER_LEN,
            usize::from(count),
            4,
            "VORG metrics truncated",
        )?;
        Ok(Self {
            minor,
            default_y,
            entries,
        })
    }

    /// The entries in table order.
    fn entries(&self) -> impl Iterator<Item = (u16, i16)> + 'a {
        self.entries.chunks_exact(4).map(|e| {
            (
                u16::from_be_bytes([e[0], e[1]]),
                i16::from_be_bytes([e[2], e[3]]),
            )
        })
    }

    /// Serializes a table with this one's version and default and the
    /// given `entries`, which must be sorted by glyph id without
    /// repeats. A glyph id fits in a `u16`, so with no repeats there
    /// are at most 65,536 entries; the count field holds 65,535, and
    /// gid 65,535 can never be a real glyph, so a 65,536th entry is cut.
    fn emit(&self, entries: &[(u16, i16)]) -> Vec<u8> {
        let entries = entries.get(..usize::from(u16::MAX)).unwrap_or(entries);
        let mut out = Vec::with_capacity(HEADER_LEN + entries.len() * 4);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&self.minor.to_be_bytes());
        out.extend_from_slice(&self.default_y.to_be_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (gid, y) in entries {
            out.extend_from_slice(&gid.to_be_bytes());
            out.extend_from_slice(&y.to_be_bytes());
        }
        out
    }
}

/// Subsets `VORG` through `gid_map` (`(old, new)` pairs sorted by old
/// gid). Returns `None` when the source has no `VORG`, or when it is
/// malformed: then it is left out and the reason recorded in
/// `warnings`.
pub(crate) fn subset_vorg(
    face: &Face<'_>,
    gid_map: &[(GlyphId, GlyphId)],
    warnings: &Warnings,
) -> Option<Vec<u8>> {
    let bytes = match face.table_bytes(tag::VORG) {
        Ok(b) => b,
        Err(Error::MissingTable { .. }) => return None,
        Err(e) => {
            warnings.parse_error(tag::VORG, 0, &e, "the whole table");
            return None;
        }
    };
    match remap(bytes, gid_map) {
        Ok(out) => Some(out),
        Err(e) => {
            warnings.parse_error(tag::VORG, 0, &e, "the whole table");
            None
        }
    }
}

/// Keeps the entries of glyphs in `gid_map`, renumbered and sorted by
/// new gid. A glyph listed twice keeps its first entry.
fn remap(bytes: &[u8], gid_map: &[(GlyphId, GlyphId)]) -> Result<Vec<u8>, Error> {
    let vorg = Vorg::parse(bytes)?;
    let new_gid = |old: GlyphId| -> Option<GlyphId> {
        let i = gid_map.binary_search_by_key(&old, |(o, _)| *o).ok()?;
        gid_map.get(i).map(|&(_, new)| new)
    };
    let mut entries: Vec<(u16, i16)> = vorg
        .entries()
        .filter_map(|(old, y)| new_gid(old).map(|new| (new, y)))
        .collect();
    // Stable, so the first of any repeated glyph stays in front and
    // survives the dedup.
    entries.sort_by_key(|&(gid, _)| gid);
    entries.dedup_by_key(|&mut (gid, _)| gid);
    Ok(vorg.emit(&entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A `VORG` with `default` and `entries` in the given order.
    fn vorg_bytes(default: i16, entries: &[(u16, i16)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        b.extend_from_slice(&default.to_be_bytes());
        b.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (gid, y) in entries {
            b.extend_from_slice(&gid.to_be_bytes());
            b.extend_from_slice(&y.to_be_bytes());
        }
        b
    }

    /// The default and entries of `bytes`, read back.
    fn decode(bytes: &[u8]) -> (i16, Vec<(u16, i16)>) {
        let vorg = Vorg::parse(bytes).expect("VORG parses");
        (vorg.default_y, vorg.entries().collect())
    }

    #[test]
    fn subset_keeps_the_kept_glyphs_entries_renumbered() {
        let src = vorg_bytes(880, &[(1, 900), (3, 870), (5, 860), (9, 850)]);
        // Keep old glyphs 0, 3, 5 and 7 as new 0..=3.
        let gid_map = [(0, 0), (3, 1), (5, 2), (7, 3)];
        let out = remap(&src, &gid_map).unwrap();
        assert_eq!(decode(&out), (880, vec![(1, 870), (2, 860)]));
        // The core parser agrees, default included.
        let core = sigilbuzz::tables::Vorg::parse(&out).unwrap();
        assert_eq!(core.vert_origin_y(1), 870);
        assert_eq!(core.vert_origin_y(3), 880);
    }

    #[test]
    fn subset_sorts_by_new_gid_and_drops_repeats() {
        // Out of order, with glyph 4 listed twice: the first wins.
        let src = vorg_bytes(800, &[(6, 1), (4, 2), (2, 3), (4, 9)]);
        let gid_map = [(0, 0), (2, 1), (4, 2), (6, 3)];
        let out = remap(&src, &gid_map).unwrap();
        assert_eq!(decode(&out).1, [(1, 3), (2, 2), (3, 1)]);
    }

    #[test]
    fn subset_without_entries_keeps_the_default() {
        let out = remap(&vorg_bytes(-120, &[]), &[(0, 0)]).unwrap();
        assert_eq!(out, vorg_bytes(-120, &[]));
    }

    #[test]
    fn truncated_tables_report_where() {
        let full = vorg_bytes(880, &[(1, 900), (2, 910)]);
        let err = |b: &[u8]| remap(b, &[(0, 0)]).unwrap_err();
        assert_eq!(
            err(&full[..10]),
            Error::Truncated {
                offset: 8,
                context: "VORG metrics truncated"
            }
        );
        assert!(matches!(
            err(&full[..5]),
            Error::Truncated { offset: 4, .. }
        ));
        let mut bad = full.clone();
        bad[1] = 2;
        assert!(matches!(err(&bad), Error::Malformed { offset: 0, .. }));
    }

    #[test]
    fn subset_vorg_leaves_a_malformed_table_out_with_a_warning() {
        let data = crate::sfnt::build(
            0x4F54_544F,
            &[(tag::VORG, vorg_bytes(880, &[(1, 1)])[..9].to_vec())],
        );
        let face = Face::parse_bytes(&data, 0).unwrap();
        let sink = Warnings::default();
        assert!(subset_vorg(&face, &[(0, 0)], &sink).is_none());
        let got: Vec<_> = sink
            .into_sorted()
            .iter()
            .map(|w| (w.table, w.offset))
            .collect();
        assert_eq!(got, [(tag::VORG, 8)]);
        // Without a VORG there is nothing to do and nothing to report.
        let data = crate::sfnt::build(0x4F54_544F, &[(tag::MAXP, vec![0, 0, 0x50, 0, 0, 1])]);
        let face = Face::parse_bytes(&data, 0).unwrap();
        let sink = Warnings::default();
        assert!(subset_vorg(&face, &[(0, 0)], &sink).is_none());
        assert!(sink.into_sorted().is_empty());
    }
}
