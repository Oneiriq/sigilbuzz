//! `BASE` and `STAT` subsetting.
//!
//! `STAT` names the font's design axes and styles. It refers to `name`
//! entries, never to glyphs, so it passes through unchanged.
//!
//! `BASE` gives the baselines (Roman, ideographic, hanging, ...) and
//! extents each script lines up on, for the horizontal and vertical
//! axes, which CJK layout in particular relies on. Only one structure
//! in it names a glyph: a format 2 `BaseCoord`, whose coordinate a
//! renderer may adjust by where a contour point of `referenceGlyph`
//! lands after hinting or variation. A `BASE` without one passes
//! through unchanged. Otherwise each reference is renumbered; a
//! coordinate whose glyph is not in the subset becomes a format 1
//! coordinate with the same value, the coordinate it stands for
//! before any adjustment, so every offset in the table stays valid.
//!
//! A `BASE` that cannot be walked is left out and reported.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::{Error, Face};

use crate::read;
use crate::util::{WorkBudget, WORK_LIMIT};
use crate::warnings::Warnings;
use crate::GlyphId;

/// The `STAT` table tag.
pub(crate) const STAT: [u8; 4] = *b"STAT";

/// The bytes `table_bytes` returned for the source table `table`, or
/// `None` when the source has no such table. Bytes that cannot be read
/// leave the table out, and the reason is recorded in `warnings`.
fn source_table<'a>(
    table: [u8; 4],
    table_bytes: Result<&'a [u8], Error>,
    warnings: &Warnings,
) -> Option<&'a [u8]> {
    match table_bytes {
        Ok(bytes) => Some(bytes),
        Err(Error::MissingTable { .. }) => None,
        Err(e) => {
            warnings.parse_error(table, 0, &e, "the whole table");
            None
        }
    }
}

/// Returns the source `STAT`, unchanged, when there is one. A `STAT`
/// whose bytes cannot be read is left out and recorded in `warnings`.
pub(crate) fn subset_stat(face: &Face<'_>, warnings: &Warnings) -> Option<Vec<u8>> {
    source_table(STAT, face.table_bytes(STAT), warnings).map(<[u8]>::to_vec)
}

/// Subsets `BASE` through `gid_map` (`(old, new)` pairs sorted by old
/// gid). Returns `None` when the source has no `BASE`, or when it
/// cannot be walked: then it is left out and the reason recorded in
/// `warnings`.
pub(crate) fn subset_base(
    face: &Face<'_>,
    gid_map: &[(GlyphId, GlyphId)],
    warnings: &Warnings,
) -> Option<Vec<u8>> {
    let bytes = source_table(tag::BASE, face.table_bytes(tag::BASE), warnings)?;
    match remap(bytes, gid_map) {
        Ok(out) => Some(out),
        Err(e) => {
            warnings.parse_error(tag::BASE, 0, &e, "the whole table");
            None
        }
    }
}

/// Renumbers the reference glyph of every format 2 `BaseCoord` in
/// `bytes`, or turns the coordinate into format 1 when its glyph is
/// not kept.
fn remap(bytes: &[u8], gid_map: &[(GlyphId, GlyphId)]) -> Result<Vec<u8>, Error> {
    let format2 = format2_coords(bytes)?;
    let mut out = bytes.to_vec();
    for at in format2 {
        // `format2_coords` checked that all eight bytes are there.
        let old = read::u16_at(bytes, at + 4, "BaseCoord truncated")?;
        let patch = match gid_map.binary_search_by_key(&old, |&(o, _)| o) {
            Ok(i) => (at + 4, gid_map[i].1),
            Err(_) => (at, 1),
        };
        if let Some(field) = out
            .get_mut(patch.0..)
            .and_then(<[u8]>::first_chunk_mut::<2>)
        {
            *field = patch.1.to_be_bytes();
        }
    }
    Ok(out)
}

/// The offset of every format 2 `BaseCoord` in the `BASE` table
/// `bytes`, after walking (and so checking) the whole table: both axes,
/// each script's values and its default and per-language extents, and
/// every coordinate they point at. Each subtable is walked once, however
/// many offsets share it, and the walk gives up on a table that would
/// cost more than [`WORK_LIMIT`].
pub(crate) fn format2_coords(bytes: &[u8]) -> Result<BTreeSet<usize>, Error> {
    let mut walk = Walk {
        bytes,
        budget: WorkBudget::new(WORK_LIMIT),
        seen: BTreeSet::new(),
        format2: BTreeSet::new(),
    };
    if read::u16_at(bytes, 0, CTX)? != 1 {
        return Err(Error::Malformed {
            offset: 0,
            context: "unsupported BASE major version",
        });
    }
    for axis_slot in [4, 6] {
        let axis = read::u16_at(bytes, axis_slot, CTX)?;
        if axis != 0 {
            walk.axis(usize::from(axis))?;
        }
    }
    Ok(walk.format2)
}

/// Context for a read that runs past the table.
const CTX: &str = "BASE truncated";

/// The state of one walk over a `BASE` table.
struct Walk<'a> {
    bytes: &'a [u8],
    budget: WorkBudget,
    /// Subtables already walked, by offset.
    seen: BTreeSet<usize>,
    /// Format 2 `BaseCoord`s found, by offset.
    format2: BTreeSet<usize>,
}

impl Walk<'_> {
    /// Charges `units` of work. Fails once the budget is spent.
    fn charge(&self, units: usize) -> Result<(), Error> {
        if self.budget.spend(units) {
            Ok(())
        } else {
            Err(Error::Unsupported {
                context: "BASE costs too much to walk",
            })
        }
    }

    /// True the first time `at` is visited.
    fn first_visit(&mut self, at: usize) -> bool {
        self.seen.insert(at)
    }

    /// The position of the subtable an Offset16 at `slot` points at,
    /// measured from `base`; `None` for a null offset.
    fn offset16(&self, slot: usize, base: usize) -> Result<Option<usize>, Error> {
        let off = read::u16_at(self.bytes, slot, CTX)?;
        Ok((off != 0).then(|| base + usize::from(off)))
    }

    /// An Axis table: its BaseScriptList. The BaseTagList names no
    /// glyphs.
    fn axis(&mut self, at: usize) -> Result<(), Error> {
        read::slice_at(self.bytes, at, 4, CTX)?;
        let Some(list) = self.offset16(at + 2, at)? else {
            return Ok(());
        };
        let count = usize::from(read::u16_at(self.bytes, list, CTX)?);
        read::array_at(self.bytes, list + 2, count, 6, CTX)?;
        self.charge(count)?;
        for i in 0..count {
            if let Some(script) = self.offset16(list + 2 + i * 6 + 4, list)? {
                self.script(script)?;
            }
        }
        Ok(())
    }

    /// A BaseScript: its BaseValues, default MinMax, and the MinMax of
    /// each language system.
    fn script(&mut self, at: usize) -> Result<(), Error> {
        if !self.first_visit(at) {
            return Ok(());
        }
        let count = usize::from(read::u16_at(self.bytes, at + 4, CTX)?);
        read::array_at(self.bytes, at + 6, count, 6, CTX)?;
        self.charge(count + 1)?;
        if let Some(values) = self.offset16(at, at)? {
            self.values(values)?;
        }
        if let Some(min_max) = self.offset16(at + 2, at)? {
            self.min_max(min_max)?;
        }
        for i in 0..count {
            if let Some(min_max) = self.offset16(at + 6 + i * 6 + 4, at)? {
                self.min_max(min_max)?;
            }
        }
        Ok(())
    }

    /// A BaseValues table: one coordinate per baseline tag.
    fn values(&mut self, at: usize) -> Result<(), Error> {
        if !self.first_visit(at) {
            return Ok(());
        }
        let count = usize::from(read::u16_at(self.bytes, at + 2, CTX)?);
        read::array_at(self.bytes, at + 4, count, 2, CTX)?;
        self.charge(count + 1)?;
        for i in 0..count {
            if let Some(coord) = self.offset16(at + 4 + i * 2, at)? {
                self.coord(coord)?;
            }
        }
        Ok(())
    }

    /// A MinMax table: the default extents and those of each feature.
    fn min_max(&mut self, at: usize) -> Result<(), Error> {
        if !self.first_visit(at) {
            return Ok(());
        }
        let count = usize::from(read::u16_at(self.bytes, at + 4, CTX)?);
        read::array_at(self.bytes, at + 6, count, 8, CTX)?;
        self.charge(count + 1)?;
        let slots = [at, at + 2]
            .into_iter()
            .chain((0..count).flat_map(|i| [at + 6 + i * 8 + 4, at + 6 + i * 8 + 6]));
        for slot in slots {
            if let Some(coord) = self.offset16(slot, at)? {
                self.coord(coord)?;
            }
        }
        Ok(())
    }

    /// A BaseCoord. Format 3 adds a Device or VariationIndex table,
    /// which names no glyphs.
    fn coord(&mut self, at: usize) -> Result<(), Error> {
        if !self.first_visit(at) {
            return Ok(());
        }
        self.charge(1)?;
        let len = match read::u16_at(self.bytes, at, CTX)? {
            1 => 4,
            2 => 8,
            3 => 6,
            _ => {
                return Err(Error::Malformed {
                    offset: at,
                    context: "unknown BaseCoord format",
                })
            }
        };
        read::slice_at(self.bytes, at, len, "BaseCoord truncated")?;
        if len == 8 {
            self.format2.insert(at);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A BaseCoord: format 1 with `y`, or format 2 with `y` and a
    /// reference glyph.
    fn coord(y: i16, glyph: Option<u16>) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(if glyph.is_some() { 2u16 } else { 1 }).to_be_bytes());
        b.extend_from_slice(&y.to_be_bytes());
        if let Some(g) = glyph {
            b.extend_from_slice(&g.to_be_bytes());
            b.extend_from_slice(&7u16.to_be_bytes()); // baseCoordPoint
        }
        b
    }

    /// A `BASE` with a vertical axis only: tags `ideo` and `romn`, one
    /// script whose BaseValues has `coords`, and a default MinMax whose
    /// min and max are `min_max`.
    fn base_bytes(coords: &[Vec<u8>], min_max: [Vec<u8>; 2]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&1u16.to_be_bytes()); // major
        b.extend_from_slice(&0u16.to_be_bytes()); // minor
        b.extend_from_slice(&0u16.to_be_bytes()); // horizAxis
        b.extend_from_slice(&8u16.to_be_bytes()); // vertAxis
                                                  // Axis at 8: tag list at +4 (12), script list at +14 (22).
        b.extend_from_slice(&4u16.to_be_bytes());
        b.extend_from_slice(&14u16.to_be_bytes());
        // BaseTagList at 12.
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(b"ideoromn");
        // BaseScriptList at 22: one record, script at +8 (30).
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(b"hani");
        b.extend_from_slice(&8u16.to_be_bytes());
        // BaseScript at 30: values at +6 (36), MinMax after them.
        let values_len = 4 + 2 * coords.len() + coords.iter().map(Vec::len).sum::<usize>();
        b.extend_from_slice(&6u16.to_be_bytes());
        b.extend_from_slice(&((6 + values_len) as u16).to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes()); // no language systems
                                                  // BaseValues at 36.
        b.extend_from_slice(&0u16.to_be_bytes()); // defaultBaselineIndex
        b.extend_from_slice(&(coords.len() as u16).to_be_bytes());
        let mut at = 4 + 2 * coords.len();
        for c in coords {
            b.extend_from_slice(&(at as u16).to_be_bytes());
            at += c.len();
        }
        for c in coords {
            b.extend_from_slice(c);
        }
        // MinMax.
        b.extend_from_slice(&6u16.to_be_bytes());
        b.extend_from_slice(&((6 + min_max[0].len()) as u16).to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes()); // no feature extents
        b.extend_from_slice(&min_max[0]);
        b.extend_from_slice(&min_max[1]);
        b
    }

    /// `(format, coordinate, reference glyph)` of the BaseCoord at `at`.
    fn read_coord(bytes: &[u8], at: usize) -> (u16, i16, u16) {
        let u = |i: usize| u16::from_be_bytes([bytes[at + i], bytes[at + i + 1]]);
        (u(0), u(2) as i16, u(4))
    }

    #[test]
    fn a_base_without_reference_glyphs_passes_through() {
        let src = base_bytes(
            &[coord(-120, None), coord(0, None)],
            [coord(-150, None), coord(900, None)],
        );
        assert!(format2_coords(&src).unwrap().is_empty());
        assert_eq!(remap(&src, &[(0, 0)]).unwrap(), src);
    }

    #[test]
    fn reference_glyphs_are_renumbered_or_dropped() {
        // The ideographic baseline refers to glyph 5, which is kept as
        // glyph 2; the max extent refers to glyph 9, which is not.
        let src = base_bytes(
            &[coord(-120, Some(5)), coord(0, None)],
            [coord(-150, None), coord(900, Some(9))],
        );
        let found: Vec<usize> = format2_coords(&src).unwrap().into_iter().collect();
        assert_eq!(found.len(), 2);
        let out = remap(&src, &[(0, 0), (3, 1), (5, 2)]).unwrap();
        assert_eq!(out.len(), src.len(), "offsets stay where they were");
        assert_eq!(read_coord(&out, found[0]), (2, -120, 2));
        // Format 1 now, with the same coordinate.
        assert_eq!(read_coord(&out, found[1]).0, 1);
        assert_eq!(read_coord(&out, found[1]).1, 900);
        // The core parser reads the result.
        let base = sigilbuzz::tables::base::Base::parse(&out).expect("BASE parses");
        let script = base
            .vertical_axis()
            .and_then(|axis| axis.script(*b"hani"))
            .expect("hani script");
        assert_eq!(script.baseline(*b"ideo"), Some(-120));
        assert_eq!(script.min_max(None), Some((-150, 900)));
    }

    #[test]
    fn shared_subtables_are_walked_once() {
        // Every coordinate offset of the BaseValues points at the same
        // format 2 coordinate.
        let mut src = base_bytes(
            &[coord(1, Some(4)), coord(2, None)],
            [coord(3, None), coord(4, None)],
        );
        // Point the second BaseValues entry at the first coordinate.
        let first = u16::from_be_bytes([src[40], src[41]]);
        src[42..44].copy_from_slice(&first.to_be_bytes());
        let found = format2_coords(&src).unwrap();
        assert_eq!(found.len(), 1);
        let out = remap(&src, &[(0, 0), (4, 1)]).unwrap();
        assert_eq!(read_coord(&out, 36 + usize::from(first)), (2, 1, 1));
    }

    #[test]
    fn malformed_tables_report_where() {
        let src = base_bytes(&[coord(1, Some(4))], [coord(3, None), coord(4, None)]);
        let err = |b: &[u8]| remap(b, &[(0, 0)]).unwrap_err();
        // Cut inside the format 2 coordinate.
        let coord_at = 36 + usize::from(u16::from_be_bytes([src[40], src[41]]));
        assert_eq!(
            err(&src[..coord_at + 6]),
            Error::Truncated {
                offset: coord_at,
                context: "BaseCoord truncated"
            }
        );
        let mut bad = src.clone();
        bad[coord_at + 1] = 7;
        assert_eq!(
            err(&bad),
            Error::Malformed {
                offset: coord_at,
                context: "unknown BaseCoord format"
            }
        );
        let mut major = src.clone();
        major[1] = 2;
        assert!(matches!(err(&major), Error::Malformed { offset: 0, .. }));
        assert!(matches!(err(&src[..5]), Error::Truncated { offset: 4, .. }));
    }

    #[test]
    fn subset_base_leaves_a_malformed_table_out_with_a_warning() {
        let src = base_bytes(&[coord(1, None)], [coord(3, None), coord(4, None)]);
        let font = crate::sfnt::build(0x4F54_544F, &[(tag::BASE, src[..30].to_vec())]);
        let face = Face::parse_bytes(&font, 0).unwrap();
        let sink = Warnings::default();
        assert!(subset_base(&face, &[(0, 0)], &sink).is_none());
        let got: Vec<_> = sink.into_sorted().iter().map(|w| w.table).collect();
        assert_eq!(got, [tag::BASE]);
        // A well-formed one comes back unchanged, and STAT always does.
        let font = crate::sfnt::build(
            0x4F54_544F,
            &[(tag::BASE, src.clone()), (STAT, vec![1, 2, 3, 4])],
        );
        let face = Face::parse_bytes(&font, 0).unwrap();
        let sink = Warnings::default();
        assert_eq!(subset_base(&face, &[(0, 0)], &sink), Some(src));
        assert_eq!(subset_stat(&face, &sink), Some(vec![1, 2, 3, 4]));
        assert!(sink.into_sorted().is_empty());
    }

    #[test]
    fn unreadable_table_bytes_are_left_out_with_a_warning() {
        // `Face::parse_bytes` checks every table's range, so the error
        // is built by hand. It used to be dropped without a word, and
        // strict mode then rejected STAT as a table it cannot handle.
        let sink = Warnings::default();
        let past_end = Error::Malformed {
            offset: 40,
            context: "table extends past end of font",
        };
        assert_eq!(source_table(STAT, Err(past_end), &sink), None);
        let missing = Error::MissingTable { tag: STAT };
        assert_eq!(source_table(STAT, Err(missing), &sink), None);
        assert_eq!(source_table(STAT, Ok(&[7]), &sink), Some(&[7][..]));
        let got: Vec<_> = sink
            .into_sorted()
            .iter()
            .map(|w| (w.table, w.offset, w.context, w.dropped))
            .collect();
        assert_eq!(
            got,
            [(
                STAT,
                40,
                "table extends past end of font",
                "the whole table"
            )]
        );

        // Missing tables stay quiet in the subset too.
        let font = crate::sfnt::build(0x4F54_544F, &[(tag::BASE, vec![0; 4])]);
        let face = Face::parse_bytes(&font, 0).unwrap();
        let sink = Warnings::default();
        assert_eq!(subset_stat(&face, &sink), None);
        assert!(sink.into_sorted().is_empty());
    }
}
