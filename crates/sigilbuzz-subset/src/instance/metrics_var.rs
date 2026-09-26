//! The HVAR, VVAR and MVAR rebuilds of a partial instance: each store
//! projected, and every DeltaSetIndexMap and value record renumbered
//! to match.

use alloc::vec::Vec;

use super::ivs::{offset32, project_ivs, shifted, RegionRemap};
use super::AxisPin;
use crate::read;
use crate::SubsetError;

// ---------------------------------------------------------------------------
// DeltaSetIndexMap rewrite (used by HVAR / VVAR partial bake)
// ---------------------------------------------------------------------------

/// Re-emits a `DeltaSetIndexMap` with every entry's outer index
/// rewritten through `remap`. Entries whose outer subtable collapsed
/// land at `(new_subtable_count, 0)`, guaranteed out-of-range, so
/// IVS evaluation returns zero (the desired "no variation for this
/// row" semantics).
///
/// The output keeps the source's format (0 / 1). The packed
/// `(outer, inner)` may overflow the source's bit allocation, so the
/// entryFormat is recomputed to the smallest one that holds every
/// remapped entry.
///
/// `start` is the offset into `data` where the map begins. A map that
/// runs past `data` is a parse error measured from the start of `data`;
/// its size is checked, so a crafted mapCount cannot wrap a 32-bit
/// `usize`.
pub(super) fn rewrite_delta_set_index_map(
    data: &[u8],
    start: usize,
    remap: &RegionRemap,
    new_subtable_count: u16,
) -> Result<Vec<u8>, sigilbuzz::Error> {
    const CTX: &str = "DeltaSetIndexMap truncated";
    let header = read::slice_at(data, start, 2, CTX)?;
    let (format, entry_format) = (header[0], header[1]);
    let (map_count, entries_at): (u32, usize) = match format {
        0 => (u32::from(read::u16_at(data, start + 2, CTX)?), start + 4),
        1 => (read::u32_at(data, start + 2, CTX)?, start + 6),
        _ => {
            return Err(sigilbuzz::Error::Malformed {
                offset: start,
                context: "unsupported DeltaSetIndexMap format",
            })
        }
    };

    let entry_bytes = ((entry_format >> 4) & 0x03) as usize + 1;
    let inner_bits = (entry_format & 0x0F) as u32 + 1;
    let inner_mask: u32 = (1u32 << inner_bits) - 1;

    if map_count == 0 {
        // Nothing to rewrite: return a clone of the unchanged map
        // header so the caller's offset surgery still works.
        return Ok(data[start..entries_at].to_vec());
    }

    let count = usize::try_from(map_count).map_err(|_| sigilbuzz::Error::Truncated {
        offset: entries_at,
        context: CTX,
    })?;
    let entries = read::array_at(data, entries_at, count, entry_bytes, CTX)?;

    // Decode every entry, remap, then decide the new entryFormat.
    let mut new_entries: Vec<(u16, u16)> = Vec::with_capacity(count);
    for entry in entries.chunks_exact(entry_bytes) {
        let raw = entry.iter().fold(0u32, |raw, &b| (raw << 8) | u32::from(b));
        let inner = (raw & inner_mask) as u16;
        let outer = (raw >> inner_bits) as u16;
        let (new_outer, new_inner) = match remap.lookup(outer, inner) {
            Some(v) => v,
            None => (new_subtable_count, 0),
        };
        new_entries.push((new_outer, new_inner));
    }

    // Compute new entryFormat. Use the smallest entryFormat that
    // covers every (outer, inner) we'll write. inner_bits = ceil(log2)
    // of (max_inner + 1), clamped to [1, 16]; total bits = inner_bits
    // + outer_bits, clamped to multiples of 8 for entry_bytes.
    let max_outer = new_entries.iter().map(|(o, _)| *o).max().unwrap_or(0);
    let max_inner = new_entries.iter().map(|(_, i)| *i).max().unwrap_or(0);
    let new_inner_bits: u32 = if max_inner == 0 {
        1
    } else {
        16 - max_inner.leading_zeros()
    };
    let new_outer_bits: u32 = if max_outer == 0 {
        0
    } else {
        16 - max_outer.leading_zeros()
    };
    let total_bits = new_inner_bits + new_outer_bits;
    let new_entry_bytes: u32 = total_bits.div_ceil(8);
    let new_entry_bytes = new_entry_bytes.clamp(1, 4);
    let new_entry_format =
        (((new_entry_bytes - 1) as u8) << 4) | ((new_inner_bits - 1) as u8 & 0x0F);
    let new_inner_mask: u32 = (1u32 << new_inner_bits) - 1;

    // Re-emit, keeping the source's mapCount field width.
    let mut out = Vec::with_capacity(6 + count * (new_entry_bytes as usize));
    out.push(format);
    out.push(new_entry_format);
    out.extend_from_slice(&data[start + 2..entries_at]);
    for (outer, inner) in new_entries {
        let packed: u32 =
            (u32::from(outer) << new_inner_bits) | (u32::from(inner) & new_inner_mask);
        let bytes = packed.to_be_bytes();
        // Take the low `new_entry_bytes` bytes (big-endian).
        out.extend_from_slice(&bytes[(4 - new_entry_bytes as usize)..]);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// HVAR / VVAR partial bake
// ---------------------------------------------------------------------------

/// Re-emits a HVAR or VVAR table: `header_len` bytes of header (the
/// version, the Offset32 to the store at byte 4, then one Offset32 per
/// DeltaSetIndexMap), the store partial-projected through `pins` /
/// `coords`, and every map rewritten through the remap.
///
/// # Errors
///
/// A parse error measured from the start of the table when it is
/// malformed (the caller drops the table and reports it), and
/// [`SubsetError::Unsupported`] when the rebuilt table outgrows its
/// Offset32s.
fn bake_metrics_var_partial(
    table: &[u8],
    header_len: usize,
    coords: &[f32],
    pins: &[AxisPin],
    too_big: &'static str,
) -> Result<Vec<u8>, SubsetError> {
    const CTX: &str = "metrics variations header truncated";
    let header = read::slice_at(table, 0, header_len, CTX)?;
    if u16::from_be_bytes([header[0], header[1]]) != 1 {
        return Err(sigilbuzz::Error::Malformed {
            offset: 0,
            context: "unsupported metrics variations major version",
        }
        .into());
    }
    let ivs_off = read::offset32_at(table, 4, 0, "metrics variations store offset past the end")?;
    let (new_ivs, remap) =
        project_ivs(&table[ivs_off..], coords, pins).map_err(|e| shifted(e, ivs_off))?;
    // The subtable count of the store just emitted.
    let new_subtable_count = u16::from_be_bytes([new_ivs[6], new_ivs[7]]);

    // Rewrite each non-zero map.
    let mut new_maps: Vec<Option<Vec<u8>>> = Vec::new();
    for slot in (8..header_len).step_by(4) {
        let off = read::u32_at(table, slot, CTX)?;
        new_maps.push(if off == 0 {
            None
        } else {
            let start = read::offset32_at(table, slot, 0, "DeltaSetIndexMap offset past the end")?;
            Some(rewrite_delta_set_index_map(
                table,
                start,
                &remap,
                new_subtable_count,
            )?)
        });
    }

    // Layout the output: header + store + maps, Offset32s from the
    // start of the table.
    let mut out = Vec::with_capacity(table.len());
    out.extend_from_slice(&header[..4]); // major + minor
    out.extend_from_slice(&offset32(header_len, too_big)?.to_be_bytes());
    out.resize(header_len, 0);
    out.extend_from_slice(&new_ivs);
    for (i, map) in new_maps.iter().enumerate() {
        if let Some(map) = map {
            let slot = 8 + i * 4;
            let at = offset32(out.len(), too_big)?;
            out[slot..slot + 4].copy_from_slice(&at.to_be_bytes());
            out.extend_from_slice(map);
        }
    }
    Ok(out)
}

/// Re-emits HVAR with its embedded IVS partial-projected through
/// `pins` / `coords`, every DeltaSetIndexMap rewritten through the
/// remap, and the table header offsets adjusted to match. HVAR's
/// header is 20 bytes: the version, then Offset32s to the store and to
/// the advance, LSB and RSB maps.
///
/// A malformed HVAR is a parse error; the caller drops the table (no
/// advance variation, safe but slightly degraded) and reports it.
pub(super) fn bake_hvar_partial(
    hvar_bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Result<Vec<u8>, SubsetError> {
    bake_metrics_var_partial(
        hvar_bytes,
        20,
        coords,
        pins,
        "partial instancing: HVAR exceeds 4 GiB",
    )
}

/// Re-emits VVAR with its embedded IVS partial-projected and every
/// DeltaSetIndexMap rewritten. VVAR's header is 24 bytes (4 ver + 5
/// x o32: ivs / advance-height / tsb / bsb / vorg). The vorg map
/// shares the IVS rows with the others; we rewrite it through the
/// same remap.
pub(super) fn bake_vvar_partial(
    vvar_bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Result<Vec<u8>, SubsetError> {
    bake_metrics_var_partial(
        vvar_bytes,
        24,
        coords,
        pins,
        "partial instancing: VVAR exceeds 4 GiB",
    )
}

// ---------------------------------------------------------------------------
// MVAR partial bake
// ---------------------------------------------------------------------------

/// Re-emits MVAR with its embedded IVS partial-projected. MVAR
/// references rows by direct (outer, inner) in each value record,
/// no DeltaSetIndexMap. Rows pointing at collapsed subtables get
/// rewritten to `(new_subtable_count, 0)` (out-of-range; resolves to
/// zero delta).
///
/// The MVAR header has `valueRecordSize >= 8`; we preserve the
/// source's record_size and only patch the first 8 bytes of each
/// record (tag + outer + inner).
///
/// # Errors
///
/// A parse error measured from the start of MVAR when it is malformed
/// (the caller drops the table and reports it), and
/// [`SubsetError::Unsupported`] when the value records outgrow the
/// Offset16 that has to reach the store behind them.
pub(super) fn bake_mvar_partial(
    mvar_bytes: &[u8],
    coords: &[f32],
    pins: &[AxisPin],
) -> Result<Vec<u8>, SubsetError> {
    const CTX: &str = "MVAR truncated";
    let header = read::slice_at(mvar_bytes, 0, 12, CTX)?;
    if u16::from_be_bytes([header[0], header[1]]) != 1 {
        return Err(sigilbuzz::Error::Malformed {
            offset: 0,
            context: "unsupported MVAR major version",
        }
        .into());
    }
    let record_size = usize::from(u16::from_be_bytes([header[6], header[7]]));
    let record_count = usize::from(u16::from_be_bytes([header[8], header[9]]));
    let store_off = usize::from(u16::from_be_bytes([header[10], header[11]]));

    if record_count > 0 && record_size < 8 {
        return Err(sigilbuzz::Error::Malformed {
            offset: 6,
            context: "MVAR valueRecordSize is below 8",
        }
        .into());
    }
    if store_off == 0 {
        // No store: pass through unchanged.
        return Ok(mvar_bytes.to_vec());
    }
    let Some(store) = mvar_bytes.get(store_off..) else {
        return Err(sigilbuzz::Error::Malformed {
            offset: 10,
            context: "MVAR store offset past the end",
        }
        .into());
    };
    let (new_ivs, remap) = project_ivs(store, coords, pins).map_err(|e| shifted(e, store_off))?;
    let new_subtable_count = u16::from_be_bytes([new_ivs[6], new_ivs[7]]);

    // Layout: 12-byte header + records + IVS. Preserve record_size.
    let records = read::array_at(mvar_bytes, 12, record_count, record_size, CTX)?;
    let new_store_off = u16::try_from(12 + records.len()).map_err(|_| {
        SubsetError::Unsupported("partial instancing: MVAR value records exceed 64 KiB")
    })?;
    let mut out = Vec::with_capacity(mvar_bytes.len());
    out.extend_from_slice(&header[..10]);
    out.extend_from_slice(&new_store_off.to_be_bytes());

    // Records.
    for record in records.chunks_exact(record_size.max(1)).take(record_count) {
        let outer = u16::from_be_bytes([record[4], record[5]]);
        let inner = u16::from_be_bytes([record[6], record[7]]);
        let (new_outer, new_inner) = match remap.lookup(outer, inner) {
            Some(v) => v,
            None => (new_subtable_count, 0),
        };
        out.extend_from_slice(&record[..4]); // tag
        out.extend_from_slice(&new_outer.to_be_bytes());
        out.extend_from_slice(&new_inner.to_be_bytes());
        // Trailing bytes per record_size (record_size >= 8).
        out.extend_from_slice(&record[8..]);
    }
    out.extend_from_slice(&new_ivs);
    Ok(out)
}
