//! `kerx` — Apple Extended Kerning.
//!
//! `kerx` is the AAT successor to `kern`. AAT-only fonts ship their
//! kerning here. sigilbuzz consults `kerx` only when GPOS has no
//! `kern` feature, so modern OpenType fonts keep their existing
//! behaviour — this matches HarfBuzz's AAT shaper policy.
//!
//! # Layout
//!
//! ```text
//!   u16 version       (2 or 3)
//!   u16 _pad
//!   u32 nTables
//!   Subtable subtables[nTables]
//!
//!   Subtable:
//!     u32 length       (bytes, incl. this header)
//!     u32 coverage     (low byte = format; high bits = flags)
//!     u32 tupleCount   (variation-font kerning — sigilbuzz ignores)
//!     Body body        (format-specific)
//! ```
//!
//! Two subtable formats are implemented:
//!
//! - Format 0 — ordered pair list (the common case for AAT fonts
//!   that re-use legacy `kern` data).
//! - Format 2 — n-way class kerning. Two AAT lookup tables map
//!   left and right glyph ids to row / column offsets into a 2D
//!   array of i16 deltas; useful for dense matrices like Latin
//!   pair-class tables that would explode if expanded to flat
//!   pairs.
//!
//! Other formats (1 — state machine, 4 — control-point anchoring,
//! 6 — indexed class) are skipped silently — sigilbuzz's apply path
//! still consults the subtables it does understand, so a mixed-format
//! `kerx` degrades gracefully instead of failing the whole font.
//!
//! # Format 0
//!
//! ```text
//!   u32 nPairs
//!   u32 searchRange
//!   u32 entrySelector
//!   u32 rangeShift
//!   Pair pairs[nPairs]:
//!     u16 left
//!     u16 right
//!     i16 value
//! ```
//!
//! Pairs are sorted by the 32-bit key `(left << 16) | right`, so
//! lookup is a binary search — exactly as in the legacy `kern`
//! table, just with a u32 count instead of u16.
//!
//! # Format 2
//!
//! ```text
//!   u32 rowWidth         (bytes per row of the kerning array)
//!   u32 leftClassTable   (offset from start of subtable)
//!   u32 rightClassTable  (offset from start of subtable)
//!   u32 array            (offset from start of subtable to i16 grid)
//! ```
//!
//! The class tables are AAT lookup tables. The left table yields a
//! pre-multiplied byte offset (`class * rowWidth`); the right table
//! yields a u16-aligned byte offset (`class * 2`). The kerning value
//! is the i16 at `array + leftValue + rightValue`. Subtable offsets
//! are measured from the start of the 12-byte common subtable header
//! — the same origin Apple's spec uses.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::tables::layout::state_table::lookup_class;
use crate::tables::parse::Reader;

const COVERAGE_FORMAT_MASK: u32 = 0xFF;
// Coverage flags (high byte of the coverage u32).
const COVERAGE_VERTICAL: u32 = 1 << 31;
const COVERAGE_CROSS_STREAM: u32 = 1 << 30;
const COVERAGE_VARIATION: u32 = 1 << 29;

/// Parsed `kerx` table.
#[derive(Debug, Clone)]
pub struct Kerx<'a> {
    version: u16,
    num_glyphs: u16,
    subtables: Vec<Subtable<'a>>,
}

#[derive(Debug, Clone, Copy)]
enum Subtable<'a> {
    Format0(Format0<'a>),
    Format2(Format2<'a>),
}

#[derive(Debug, Clone, Copy)]
struct Format0<'a> {
    data: &'a [u8],
    pairs_off: usize,
    n_pairs: u32,
}

/// Format 2 — n-way class kerning. Records the subtable-relative
/// offsets to the class tables and the kerning array; a kern lookup
/// resolves both classes through the AAT lookup primitive and reads
/// the i16 cell at `array + leftClassValue + rightClassValue`.
#[derive(Debug, Clone, Copy)]
struct Format2<'a> {
    /// The subtable's full byte slice (the 12-byte common header
    /// plus the format-2 body). All recorded offsets are relative
    /// to byte 0 of this slice, matching the spec.
    sub: &'a [u8],
    row_width: u32,
    left_class_off: usize,
    right_class_off: usize,
    array_off: usize,
}

impl<'a> Kerx<'a> {
    /// Parses a `kerx` table. Returns [`Error::Unsupported`] for
    /// versions outside {2, 3} — every AAT font sigilbuzz targets
    /// ships one of those two. `num_glyphs` is the font's `maxp`
    /// glyph count, used to bound-check format-0 lookup tables in
    /// format-2 class subtables.
    pub fn parse(data: &'a [u8], num_glyphs: u16) -> Result<Self> {
        let mut r = Reader::new(data);
        let version = r.read_u16()?;
        if version != 2 && version != 3 {
            return Err(Error::Unsupported {
                context: "kerx version outside {2, 3}",
            });
        }
        let _pad = r.read_u16()?;
        let n_tables = r.read_u32()?;

        let mut subtables = Vec::new();
        for _ in 0..n_tables {
            let sub_start = r.position();
            if sub_start + 12 > data.len() {
                return Err(Error::Truncated {
                    offset: sub_start,
                    context: "kerx subtable header",
                });
            }
            let length = r.read_u32()? as usize;
            let coverage = r.read_u32()?;
            let _tuple_count = r.read_u32()?;

            let sub_end = sub_start.checked_add(length).ok_or(Error::Malformed {
                offset: sub_start,
                context: "kerx subtable length overflow",
            })?;
            if sub_end > data.len() {
                return Err(Error::Truncated {
                    offset: sub_end,
                    context: "kerx subtable extends past table",
                });
            }

            let format = (coverage & COVERAGE_FORMAT_MASK) as u8;
            // Skip vertical, cross-stream, and variation subtables —
            // sigilbuzz produces horizontal advances only for now.
            // The cross-stream bit moves a glyph's origin in the
            // opposite axis (e.g. Zapfino's connecting ligatures
            // nudge y to tuck the bowls together); applying it
            // blindly would corrupt positions, so we skip until the
            // feature lands.
            if coverage & (COVERAGE_VERTICAL | COVERAGE_CROSS_STREAM | COVERAGE_VARIATION) != 0 {
                r.seek(sub_end)?;
                continue;
            }

            // Format 0 is the common case. Format 2 (compound-class
            // kerning) covers Latin / CJK fonts that ship a dense
            // pair matrix. Formats 1 (state-machine), 4
            // (control-point anchors) and 6 (indexed class kern)
            // exist in the spec but are rare; sigilbuzz skips them
            // silently so a mixed `kerx` still applies the formats
            // we do understand.
            match format {
                0 => {
                    if let Some(sub) = parse_format0(data, r.position(), sub_end)? {
                        subtables.push(Subtable::Format0(sub));
                    }
                }
                2 => {
                    if let Some(sub) = parse_format2(data, sub_start, sub_end)? {
                        subtables.push(Subtable::Format2(sub));
                    }
                }
                _ => {}
            }

            r.seek(sub_end)?;
        }

        Ok(Self {
            version,
            num_glyphs,
            subtables,
        })
    }

    /// Reported version word (2 or 3).
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Sum of kerning deltas across every parsed subtable for the
    /// pair `(left, right)`. Zero when no pair matches.
    #[must_use]
    pub fn kern(&self, left: u16, right: u16) -> i16 {
        let key = (u32::from(left) << 16) | u32::from(right);
        let mut total: i32 = 0;
        for sub in &self.subtables {
            let v = match sub {
                Subtable::Format0(f0) => f0.find(key),
                Subtable::Format2(f2) => f2.find(left, right, self.num_glyphs),
            };
            if let Some(v) = v {
                total += i32::from(v);
            }
        }
        total.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
    }

    /// Number of parsed subtables (any format) — useful in tests to
    /// assert which subtables were retained.
    #[must_use]
    pub fn subtable_count(&self) -> usize {
        self.subtables.len()
    }
}

/// Parses one format-0 subtable body. Returns `Ok(None)` on a
/// recoverable shape error so the rest of `kerx` still loads.
fn parse_format0(
    data: &[u8],
    body_start: usize,
    sub_end: usize,
) -> Result<Option<Format0<'_>>> {
    if body_start + 16 > sub_end {
        return Err(Error::Truncated {
            offset: body_start,
            context: "kerx format 0 header",
        });
    }
    let n_pairs = u32::from_be_bytes([
        data[body_start],
        data[body_start + 1],
        data[body_start + 2],
        data[body_start + 3],
    ]);
    let pairs_off = body_start + 16; // skip nPairs + 3 search hints
    let pairs_bytes = (n_pairs as usize).saturating_mul(6);
    let required = pairs_off.checked_add(pairs_bytes).ok_or(Error::Malformed {
        offset: pairs_off,
        context: "kerx format 0 pairs overflow",
    })?;
    if required > sub_end {
        return Err(Error::Truncated {
            offset: required,
            context: "kerx format 0 pairs exceed subtable",
        });
    }
    Ok(Some(Format0 {
        data,
        pairs_off,
        n_pairs,
    }))
}

/// Parses one format-2 subtable body. Offsets in the on-disk header
/// are relative to the subtable's own origin, so we keep a slice
/// that starts at `sub_start` and stash it on the descriptor.
fn parse_format2(
    data: &[u8],
    sub_start: usize,
    sub_end: usize,
) -> Result<Option<Format2<'_>>> {
    let body_start = sub_start + 12;
    if body_start + 16 > sub_end {
        return Err(Error::Truncated {
            offset: body_start,
            context: "kerx format 2 header",
        });
    }
    let row_width = u32::from_be_bytes([
        data[body_start],
        data[body_start + 1],
        data[body_start + 2],
        data[body_start + 3],
    ]);
    let left_off = u32::from_be_bytes([
        data[body_start + 4],
        data[body_start + 5],
        data[body_start + 6],
        data[body_start + 7],
    ]) as usize;
    let right_off = u32::from_be_bytes([
        data[body_start + 8],
        data[body_start + 9],
        data[body_start + 10],
        data[body_start + 11],
    ]) as usize;
    let array_off = u32::from_be_bytes([
        data[body_start + 12],
        data[body_start + 13],
        data[body_start + 14],
        data[body_start + 15],
    ]) as usize;

    let sub_len = sub_end - sub_start;
    // All three offsets must point inside the subtable. Anything
    // else is a malformed font; bail with `None` so the rest of the
    // table still loads instead of poisoning the whole `kerx` parse.
    if left_off >= sub_len || right_off >= sub_len || array_off >= sub_len {
        return Ok(None);
    }
    let sub = &data[sub_start..sub_end];
    Ok(Some(Format2 {
        sub,
        row_width,
        left_class_off: left_off,
        right_class_off: right_off,
        array_off,
    }))
}

impl Format0<'_> {
    fn pair_at(&self, i: u32) -> (u32, i16) {
        let off = self.pairs_off + i as usize * 6;
        let left = u16::from_be_bytes([self.data[off], self.data[off + 1]]);
        let right = u16::from_be_bytes([self.data[off + 2], self.data[off + 3]]);
        let value = i16::from_be_bytes([self.data[off + 4], self.data[off + 5]]);
        ((u32::from(left) << 16) | u32::from(right), value)
    }

    fn find(&self, key: u32) -> Option<i16> {
        if self.n_pairs == 0 {
            return None;
        }
        let mut lo: u32 = 0;
        let mut hi: u32 = self.n_pairs;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (k, v) = self.pair_at(mid);
            match k.cmp(&key) {
                core::cmp::Ordering::Less => lo = mid + 1,
                core::cmp::Ordering::Greater => hi = mid,
                core::cmp::Ordering::Equal => return Some(v),
            }
        }
        None
    }
}

impl Format2<'_> {
    /// Resolves `(left, right)` through the class tables and reads
    /// the i16 cell. Returns `None` for any defensive failure: bad
    /// offsets, unsupported lookup formats, glyphs that fall in the
    /// reserved-class slots, or a cell that lands outside the
    /// subtable. Format 2 always returns deltas — no half-split,
    /// no cross-stream — so a `Some(0)` would be indistinguishable
    /// from "no rule"; callers don't need the distinction.
    fn find(&self, left: u16, right: u16, num_glyphs: u16) -> Option<i16> {
        let left_table = self.sub.get(self.left_class_off..)?;
        let right_table = self.sub.get(self.right_class_off..)?;

        let left_value = lookup_class(left_table, left, num_glyphs).ok()?;
        let right_value = lookup_class(right_table, right, num_glyphs).ok()?;

        // Reserved-class lookups (out-of-bounds, deleted, etc.) are
        // returned by the AAT lookup helper as small sentinel values
        // (1, 2, 3). Format-2 class tables on real fonts fold these
        // into the row-0 / column-0 default cell, which is *almost*
        // always zero. Rather than special-casing, we follow the
        // spec: read the cell at the resolved offset; out-of-range
        // glyphs land on row 0 (default) and the array there is
        // typically zeroed.

        let cell_off = self
            .array_off
            .checked_add(usize::from(left_value))?
            .checked_add(usize::from(right_value))?;
        // The cell must be a fully-contained i16. row_width is also
        // a sanity hint: a left value beyond row_width would mean a
        // malformed lookup table, but again we tolerate it by
        // letting the slice bound check do the work.
        let _ = self.row_width; // referenced for the doc-driven invariant
        let bytes = self.sub.get(cell_off..cell_off + 2)?;
        Some(i16::from_be_bytes([bytes[0], bytes[1]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn build_kerx_format0(pairs: &[(u16, u16, i16)]) -> Vec<u8> {
        let pair_bytes = pairs.len() * 6;
        let body_len = 16 + pair_bytes; // 4 × u32 + pairs
        let sub_len = 12 + body_len;

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // version
        out.extend_from_slice(&0u16.to_be_bytes()); // pad
        out.extend_from_slice(&1u32.to_be_bytes()); // nTables

        // Subtable header.
        out.extend_from_slice(&(sub_len as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // coverage: horizontal, format 0
        out.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

        // Format 0 body.
        out.extend_from_slice(&(pairs.len() as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // searchRange
        out.extend_from_slice(&0u32.to_be_bytes()); // entrySelector
        out.extend_from_slice(&0u32.to_be_bytes()); // rangeShift
        for (l, r, v) in pairs {
            out.extend_from_slice(&l.to_be_bytes());
            out.extend_from_slice(&r.to_be_bytes());
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// Builds a one-subtable kerx with format 2, two left classes
    /// (mapped via lookup format 0) and two right classes. `n_glyphs`
    /// is the synthetic font's glyph count (keeps the format-0 table
    /// dense). `left_classes[i]` is the class for gid `i` (0-based);
    /// same for `right_classes`. `matrix[l][r]` is the i16 delta.
    fn build_kerx_format2(
        n_glyphs: u16,
        left_classes: &[u16],
        right_classes: &[u16],
        matrix: &[Vec<i16>],
    ) -> Vec<u8> {
        let n_left = matrix.len() as u32;
        let n_right = matrix[0].len() as u32;
        let row_width = n_right * 2;

        // Left table (format 0): each cell already pre-multiplied
        // by row_width.
        let mut left_lookup: Vec<u8> = Vec::new();
        left_lookup.extend_from_slice(&0u16.to_be_bytes()); // format 0
        for &c in left_classes {
            let off = (u32::from(c) * row_width) as u16;
            left_lookup.extend_from_slice(&off.to_be_bytes());
        }
        // Right table (format 0): each cell pre-multiplied by 2.
        let mut right_lookup: Vec<u8> = Vec::new();
        right_lookup.extend_from_slice(&0u16.to_be_bytes());
        for &c in right_classes {
            let off: u16 = c * 2;
            right_lookup.extend_from_slice(&off.to_be_bytes());
        }

        // Body layout (relative to subtable start):
        //   0  : 12 B common header
        //   12 : 16 B fmt2 header (rowWidth, leftOff, rightOff, arrOff)
        //   28 : left lookup
        //   .. : right lookup
        //   .. : kerning array
        let header_size = 12 + 16;
        let left_off = header_size;
        let right_off = left_off + left_lookup.len();
        let array_off = right_off + right_lookup.len();
        let array_bytes = (n_left * row_width) as usize;
        let sub_len = array_off + array_bytes;

        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&2u16.to_be_bytes()); // version
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes()); // nTables

        // Common subtable header.
        out.extend_from_slice(&(sub_len as u32).to_be_bytes());
        out.extend_from_slice(&2u32.to_be_bytes()); // coverage: format 2
        out.extend_from_slice(&0u32.to_be_bytes()); // tupleCount

        // fmt2 header.
        out.extend_from_slice(&row_width.to_be_bytes());
        out.extend_from_slice(&(left_off as u32).to_be_bytes());
        out.extend_from_slice(&(right_off as u32).to_be_bytes());
        out.extend_from_slice(&(array_off as u32).to_be_bytes());

        out.extend_from_slice(&left_lookup);
        out.extend_from_slice(&right_lookup);
        for row in matrix {
            for v in row {
                out.extend_from_slice(&v.to_be_bytes());
            }
        }

        // Sanity: caller's class arrays must cover n_glyphs.
        assert_eq!(left_classes.len(), n_glyphs as usize);
        assert_eq!(right_classes.len(), n_glyphs as usize);
        out
    }

    #[test]
    fn format0_binary_search_finds_pairs() {
        let bytes = build_kerx_format0(&[(10, 20, -30), (10, 30, -5), (40, 5, 7)]);
        let k = Kerx::parse(&bytes, 256).unwrap();
        assert_eq!(k.version(), 2);
        assert_eq!(k.kern(10, 20), -30);
        assert_eq!(k.kern(40, 5), 7);
        assert_eq!(k.kern(99, 99), 0);
    }

    #[test]
    fn empty_kerx_table_yields_no_subtables() {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes()); // nTables
        let k = Kerx::parse(&bytes, 0).unwrap();
        assert_eq!(k.subtable_count(), 0);
        assert_eq!(k.kern(1, 2), 0);
    }

    #[test]
    fn vertical_subtable_is_skipped() {
        // Build a 2-subtable kerx: first horizontal, second vertical
        // (coverage bit 31 set). The vertical one should be dropped.
        let sub_body_len = 16 + 6; // one pair
        let sub_len = 12 + sub_body_len;
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes()); // 2 subtables

        for (coverage, value) in [(0u32, -10i16), (COVERAGE_VERTICAL, 99i16)] {
            bytes.extend_from_slice(&(sub_len as u32).to_be_bytes());
            bytes.extend_from_slice(&coverage.to_be_bytes());
            bytes.extend_from_slice(&0u32.to_be_bytes()); // tupleCount
            bytes.extend_from_slice(&1u32.to_be_bytes()); // nPairs
            bytes.extend_from_slice(&[0u8; 12]);
            bytes.extend_from_slice(&10u16.to_be_bytes());
            bytes.extend_from_slice(&20u16.to_be_bytes());
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        let k = Kerx::parse(&bytes, 256).unwrap();
        assert_eq!(k.subtable_count(), 1);
        assert_eq!(k.kern(10, 20), -10);
    }

    #[test]
    fn rejects_unknown_version() {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&5u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        assert!(matches!(
            Kerx::parse(&bytes, 0),
            Err(Error::Unsupported { .. })
        ));
    }

    #[test]
    fn format2_compound_class_lookup_resolves_pairs() {
        // 4-glyph synthetic font:
        //   gid 0 .notdef       → left class 0, right class 0
        //   gid 1 A             → left class 1, right class 0
        //   gid 2 B             → left class 1, right class 0
        //   gid 3 V             → left class 0, right class 1
        // Matrix [left][right]:
        //   [[ 0,   0],
        //    [-30,-50]]
        // So (A, V) and (B, V) both kern by -50, while every other
        // pair is zero (and therefore never matches).
        let bytes = build_kerx_format2(
            4,
            &[0, 1, 1, 0],
            &[0, 0, 0, 1],
            &[vec![0, 0], vec![-30, -50]],
        );
        let k = Kerx::parse(&bytes, 4).unwrap();
        assert_eq!(k.subtable_count(), 1);
        assert_eq!(k.kern(1, 3), -50, "A-V pair via classes (1, 1)");
        assert_eq!(k.kern(2, 3), -50, "B-V pair via classes (1, 1)");
        assert_eq!(k.kern(1, 1), -30, "A-A pair via classes (1, 0)");
        assert_eq!(k.kern(0, 0), 0, ".notdef pair → row 0 default");
        assert_eq!(k.kern(3, 1), 0, "V-A reversed pair → row 0 default");
    }

    #[test]
    fn format2_with_zero_cell_returns_zero() {
        // Pair lands on a zero entry — kern() must still return 0
        // without surfacing a parser error.
        let bytes = build_kerx_format2(
            3,
            &[0, 1, 1],
            &[0, 1, 1],
            &[vec![0, 0], vec![0, 7]],
        );
        let k = Kerx::parse(&bytes, 3).unwrap();
        assert_eq!(k.kern(1, 0), 0); // left class 1, right class 0 → 0
        assert_eq!(k.kern(2, 2), 7); // left class 1, right class 1
    }

    #[test]
    fn format2_bad_class_offset_silently_drops_subtable() {
        // Build a valid fmt2 then clobber the leftClassTable offset
        // to point past the subtable. parse() must still succeed and
        // simply skip the subtable rather than fail the table.
        let mut bytes = build_kerx_format2(
            3,
            &[0, 1, 1],
            &[0, 1, 1],
            &[vec![0, 0], vec![0, 7]],
        );
        // Subtable starts at offset 8 (kerx header size). fmt2
        // header at offset 8 + 12 = 20; leftClassTable u32 lives at
        // offset 24.
        let bad = u32::MAX.to_be_bytes();
        bytes[24] = bad[0];
        bytes[25] = bad[1];
        bytes[26] = bad[2];
        bytes[27] = bad[3];
        let k = Kerx::parse(&bytes, 3).unwrap();
        assert_eq!(k.subtable_count(), 0);
    }
}
