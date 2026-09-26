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
///
/// The Offset32s and the sizes derived from them are summed in `u64`,
/// where they cannot overflow, and only a sum proven to lie inside
/// `table` is turned back into a position. On a 32-bit target a
/// crafted offset would otherwise wrap `usize`.
pub(super) fn store_len(table: &[u8], off: usize) -> Result<usize, Error> {
    if u16_at(table, off, CTX)? != 1 {
        return Err(Error::Malformed {
            offset: off,
            context: "unsupported GDEF ItemVariationStore format",
        });
    }
    let region_list = u64::from(u32_at(table, off + 2, CTX)?);
    let data_count = u16_at(table, off + 6, CTX)?;
    let mut end = 8 + u64::from(data_count) * 4;
    if region_list != 0 {
        let at = position(table, off, region_list)?;
        let axes = u64::from(u16_at(table, at, CTX)?);
        let regions = u64::from(u16_at(table, at + 2, CTX)?);
        end = end.max(region_list + 4 + axes * regions * 6);
    }
    for i in 0..usize::from(data_count) {
        let data = u64::from(u32_at(table, off + 8 + i * 4, CTX)?);
        if data == 0 {
            continue;
        }
        let at = position(table, off, data)?;
        let items = u64::from(u16_at(table, at, CTX)?);
        let word_delta_count = u16_at(table, at + 2, CTX)?;
        let region_indexes = u64::from(u16_at(table, at + 4, CTX)?);
        let words = u64::from(word_delta_count & 0x7FFF);
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
    match usize::try_from(end) {
        Ok(len) if off.checked_add(len).is_some_and(|stop| stop <= table.len()) => Ok(len),
        _ => Err(Error::Truncated {
            offset: table.len(),
            context: CTX,
        }),
    }
}

/// Resolves the Offset32 `rel`, measured from the store at `off`, to a
/// position inside `table`.
fn position(table: &[u8], off: usize, rel: u64) -> Result<usize, Error> {
    usize::try_from(rel)
        .ok()
        .and_then(|rel| off.checked_add(rel))
        .filter(|&at| at < table.len())
        .ok_or(Error::Truncated {
            offset: table.len(),
            context: CTX,
        })
}
