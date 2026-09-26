//! Extent of the GDEF `ItemVariationStore`.
//!
//! The store is not keyed by glyph id: GPOS and GDEF reach its rows by
//! `(outer, inner)` index, and the subsetter keeps every one of those
//! references unchanged. So the store is copied verbatim; all that is
//! needed is how many bytes it spans.
//!
//! ```text
//!   ItemVariationStore:
//!     u16      format = 1
//!     Offset32 variationRegionListOffset
//!     u16      itemVariationDataCount
//!     Offset32 itemVariationDataOffsets[itemVariationDataCount]
//!   VariationRegionList:
//!     u16 axisCount
//!     u16 regionCount
//!     RegionAxisCoordinates regions[regionCount][axisCount]   (6 bytes each)
//!   ItemVariationData:
//!     u16 itemCount
//!     u16 wordDeltaCount          (bit 15: LONG_WORDS)
//!     u16 regionIndexCount
//!     u16 regionIndexes[regionIndexCount]
//!     DeltaSet deltaSets[itemCount]
//! ```
//!
//! A delta set holds `wordCount` wide deltas (i32 with LONG_WORDS, else
//! i16) followed by `regionIndexCount - wordCount` narrow ones (i16
//! with LONG_WORDS, else i8).

use sigilbuzz::Error;

use super::read::{u16_at, u32_at};

const CTX: &str = "GDEF ItemVariationStore truncated";

/// Returns the length in bytes of the ItemVariationStore at `off` (from
/// the GDEF start): the furthest byte its header, region list, or any
/// ItemVariationData reaches. All of it must lie inside `table`.
pub(super) fn store_len(table: &[u8], off: usize) -> Result<usize, Error> {
    if u16_at(table, off, CTX)? != 1 {
        return Err(Error::Malformed {
            offset: off,
            context: "unsupported GDEF ItemVariationStore format",
        });
    }
    let region_list = u32_at(table, off + 2, CTX)? as usize;
    let data_count = usize::from(u16_at(table, off + 6, CTX)?);
    let mut end = 8 + data_count * 4;
    if region_list != 0 {
        let at = off + region_list;
        let axes = usize::from(u16_at(table, at, CTX)?);
        let regions = usize::from(u16_at(table, at + 2, CTX)?);
        end = end.max(region_list + 4 + axes * regions * 6);
    }
    for i in 0..data_count {
        let data = u32_at(table, off + 8 + i * 4, CTX)? as usize;
        if data == 0 {
            continue;
        }
        let at = off + data;
        let items = usize::from(u16_at(table, at, CTX)?);
        let word_delta_count = u16_at(table, at + 2, CTX)?;
        let region_indexes = usize::from(u16_at(table, at + 4, CTX)?);
        let words = usize::from(word_delta_count & 0x7FFF);
        if words > region_indexes {
            return Err(Error::Malformed {
                offset: at + 2,
                context: "GDEF ItemVariationData has more word deltas than regions",
            });
        }
        let (wide, narrow) = if word_delta_count & 0x8000 != 0 {
            (4, 2)
        } else {
            (2, 1)
        };
        let row = words * wide + (region_indexes - words) * narrow;
        end = end.max(data + 6 + region_indexes * 2 + items * row);
    }
    if off + end > table.len() {
        return Err(Error::Truncated {
            offset: table.len(),
            context: CTX,
        });
    }
    Ok(end)
}
