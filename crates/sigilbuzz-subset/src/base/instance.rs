//! The `BASE` table of an instance.
//!
//! A `BASE` 1.1 varies a format 3 `BaseCoord` through a VariationIndex
//! table into its `ItemVariationStore`. An instance moves each such
//! coordinate the way HarfBuzz's and fontTools' instancers do:
//!
//! - A full instance adds the coordinate's delta at the instance's
//!   location, rounded (halves up), makes it a format 1 coordinate,
//!   and drops the store: the table becomes `BASE` 1.0.
//! - A partial instance adds the delta at the new default location
//!   (the pinned axes at their pins, the kept ones at their defaults),
//!   projects the store onto the kept axes without the regions that
//!   lie on the pinned axes only (their deltas are in the coordinate
//!   now), and points the VariationIndex at the projected row. A
//!   coordinate whose row no longer varies becomes format 1.
//!
//! A format 3 coordinate with a hinting Device table keeps it. The
//! table is rebuilt without the bytes nothing points at any more (the
//! store, and the VariationIndex tables of format 1 coordinates), every
//! other structure moving up in order, so no offset grows.

use alloc::vec::Vec;

use sigilbuzz::tables::tag;
use sigilbuzz::tables::variation_store::ItemVariationStore;
use sigilbuzz::{Error, Face};

use super::{layout, CTX};
use crate::instance::{project_ivs_with, AxisPin, PinnedOnly, Projection, RegionRemap};
use crate::read;
use crate::util::{round_half_up, StoreDeltas};
use crate::warnings::Warnings;
use crate::SubsetError;

/// What an instance does with `BASE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BaseBake {
    /// No variations to apply: the source table, if any, passes
    /// through.
    Unchanged,
    /// The table with its variations applied.
    Rebuilt(Vec<u8>),
    /// The table could not be read; it is left out and reported.
    Dropped,
}

/// `DeltaFormat` of a VariationIndex table.
const VARIATION_INDEX: u16 = 0x8000;

/// Applies the `BASE` variations of `face` at the post-avar `coords`.
/// `pins` is empty, or all [`AxisPin::Pin`], for a full instance;
/// otherwise the axes it keeps stay variable. A `BASE` that cannot be
/// read is left out and reported in `warnings`.
pub(crate) fn instance_base(
    face: &Face<'_>,
    coords: &[f32],
    pins: &[AxisPin],
    warnings: &Warnings,
) -> BaseBake {
    let bytes = match face.table_bytes(tag::BASE) {
        Ok(bytes) => bytes,
        Err(Error::MissingTable { .. }) => return BaseBake::Unchanged,
        Err(e) => {
            warnings.parse_error(tag::BASE, 0, &e, "the whole table");
            return BaseBake::Dropped;
        }
    };
    match apply_variations(bytes, coords, pins) {
        Ok(Some(out)) => BaseBake::Rebuilt(out),
        Ok(None) => BaseBake::Unchanged,
        Err(SubsetError::Parse(e)) => {
            warnings.parse_error(tag::BASE, 0, &e, "the whole table");
            BaseBake::Dropped
        }
        Err(_) => {
            warnings.push(
                tag::BASE,
                0,
                "BASE variations could not be applied",
                "the whole table",
            );
            BaseBake::Dropped
        }
    }
}

/// [`instance_base`] on the table `bytes`: the rebuilt table, or `None`
/// when it has no store to apply.
fn apply_variations(
    bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Result<Option<Vec<u8>>, SubsetError> {
    let minor = read::u16_at(bytes, 2, CTX)?;
    if minor < 1 || read::u32_at(bytes, 8, CTX)? == 0 {
        return Ok(None);
    }
    let store_off = read::offset32_at(bytes, 8, 0, "BASE store offset past the end")?;
    let store_bytes = &bytes[store_off..];
    let store = ItemVariationStore::parse(store_bytes).map_err(|e| shift(e, store_off))?;
    let layout = layout(bytes)?;

    let full = pins.iter().all(|p| *p == AxisPin::Pin);
    // The location whose deltas the coordinates take: the instance's,
    // or for a partial one its new default.
    let default_coords: Vec<f32> = coords
        .iter()
        .enumerate()
        .map(|(i, &c)| match pins.get(i) {
            Some(AxisPin::Keep) => 0.0,
            _ => c,
        })
        .collect();
    // Coordinates can share a row; each row is resolved once.
    let deltas = StoreDeltas::new(&store, &default_coords);
    let projected: Option<(Vec<u8>, RegionRemap)> = if full {
        None
    } else {
        let how = Projection {
            pinned_only: PinnedOnly::Drop,
            merge: true,
            keep_outer_zero: false,
            keep_itemless: false,
        };
        Some(
            project_ivs_with(store_bytes, coords, pins, how)
                .map_err(|e| shift_err(e, store_off))?,
        )
    };

    let mut out = bytes.to_vec();
    // The header: version 1.0 without the store, 1.1 with the
    // projected one, whose offset is written below.
    let mut live: Vec<(usize, usize)> = alloc::vec![(0, if full { 8 } else { 12 })];
    live.extend_from_slice(&layout.ranges);
    let mut offsets = layout.offsets.clone();
    for (&at, &format) in &layout.coords {
        let len = match format {
            1 => 4,
            2 => 8,
            _ => 6,
        };
        let device = match format {
            3 => read::u16_at(bytes, at + 4, CTX)?,
            _ => 0,
        };
        if device == 0 {
            live.push((at, at + len));
            continue;
        }
        let device_at = at + usize::from(device);
        let device_len = device_size(bytes, device_at)?;
        if read::u16_at(bytes, device_at + 4, CTX)? != VARIATION_INDEX {
            // A hinting Device table stays.
            live.extend([(at, at + 6), (device_at, device_at + device_len)]);
            offsets.push((at + 4, at, device_at));
            continue;
        }
        let outer = read::u16_at(bytes, device_at, CTX)?;
        let inner = read::u16_at(bytes, device_at + 2, CTX)?;
        let delta = round_half_up(deltas.get(outer, inner));
        let coordinate = i16::from_be_bytes([bytes[at + 2], bytes[at + 3]]);
        let moved = i32::from(coordinate)
            .saturating_add(delta)
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        out[at + 2..at + 4].copy_from_slice(&moved.to_be_bytes());
        match projected
            .as_ref()
            .and_then(|(_, remap)| remap.lookup(outer, inner))
        {
            Some((new_outer, new_inner)) => {
                out[device_at..device_at + 2].copy_from_slice(&new_outer.to_be_bytes());
                out[device_at + 2..device_at + 4].copy_from_slice(&new_inner.to_be_bytes());
                live.extend([(at, at + 6), (device_at, device_at + 6)]);
                offsets.push((at + 4, at, device_at));
            }
            None => {
                // No variation left: a format 1 coordinate.
                out[at..at + 2].copy_from_slice(&1u16.to_be_bytes());
                live.push((at, at + 4));
            }
        }
    }
    if full {
        out[2..4].copy_from_slice(&0u16.to_be_bytes());
    }

    let mut rebuilt = compact(&out, live, &offsets)?;
    if let Some((store, _)) = projected {
        let at = u32::try_from(rebuilt.len())
            .map_err(|_| SubsetError::Unsupported("instance: BASE exceeds 4 GiB"))?;
        rebuilt[8..12].copy_from_slice(&at.to_be_bytes());
        rebuilt.extend_from_slice(&store);
    }
    Ok(Some(rebuilt))
}

/// The size of the Device or VariationIndex table at `at`: a
/// VariationIndex (or a Device of an unknown format) is six bytes, a
/// Device six plus its packed deltas.
fn device_size(bytes: &[u8], at: usize) -> Result<usize, Error> {
    let start = usize::from(read::u16_at(bytes, at, CTX)?);
    let end = usize::from(read::u16_at(bytes, at + 2, CTX)?);
    let bits = match read::u16_at(bytes, at + 4, CTX)? {
        1 => 2,
        2 => 4,
        3 => 8,
        _ => 0,
    };
    let count = (end + 1).saturating_sub(start);
    let size = 6 + 2 * (count * bits).div_ceil(16);
    read::slice_at(bytes, at, size, "Device table truncated")?;
    Ok(size)
}

/// Lays out the bytes of `src` that `live` covers, in order, and
/// rewrites every Offset16 `(slot, base, target)` for the structures'
/// new positions. Bytes no range covers are left out; as nothing moves
/// down, every offset shrinks or stays.
fn compact(
    src: &[u8],
    mut live: Vec<(usize, usize)>,
    offsets: &[(usize, usize, usize)],
) -> Result<Vec<u8>, SubsetError> {
    live.sort_unstable();
    // Merge overlapping and touching ranges: (start, end, new start).
    let mut merged: Vec<(usize, usize, usize)> = Vec::with_capacity(live.len());
    let mut size = 0;
    for (start, end) in live {
        let end = end.min(src.len());
        if start >= end {
            continue;
        }
        match merged.last_mut() {
            Some(last) if start <= last.1 => {
                if end > last.1 {
                    size += end - last.1;
                    last.1 = end;
                }
            }
            _ => {
                merged.push((start, end, size));
                size += end - start;
            }
        }
    }
    let new_pos = |p: usize| -> Option<usize> {
        let i = merged.partition_point(|r| r.0 <= p).checked_sub(1)?;
        let (start, end, new_start) = merged[i];
        (p < end).then_some(new_start + p - start)
    };
    let mut out = Vec::with_capacity(size);
    for &(start, end, _) in &merged {
        out.extend_from_slice(&src[start..end]);
    }
    const LOST: SubsetError = SubsetError::Unsupported("instance: BASE offset lost in rebuild");
    for &(slot, base, target) in offsets {
        let (slot, base, target) = (
            new_pos(slot).ok_or(LOST)?,
            new_pos(base).ok_or(LOST)?,
            new_pos(target).ok_or(LOST)?,
        );
        let off = target
            .checked_sub(base)
            .and_then(|d| u16::try_from(d).ok())
            .ok_or(LOST)?;
        out.get_mut(slot..slot + 2)
            .ok_or(LOST)?
            .copy_from_slice(&off.to_be_bytes());
    }
    Ok(out)
}

/// Moves the offset of a parse error found in the store, which starts
/// `by` bytes into the table, so it counts from the table's start.
fn shift(err: Error, by: usize) -> Error {
    match err {
        Error::Truncated { offset, context } => Error::Truncated {
            offset: offset.saturating_add(by),
            context,
        },
        Error::Malformed { offset, context } => Error::Malformed {
            offset: offset.saturating_add(by),
            context,
        },
        other => other,
    }
}

/// [`shift`] for a [`SubsetError`].
fn shift_err(err: SubsetError, by: usize) -> SubsetError {
    match err {
        SubsetError::Parse(e) => SubsetError::Parse(shift(e, by)),
        other => other,
    }
}

#[cfg(test)]
mod tests;
