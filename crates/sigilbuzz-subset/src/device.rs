//! Device and VariationIndex tables under GPOS value records and
//! anchors.
//!
//! A `ValueRecord` field or a format 3 `Anchor` can point at a Device
//! table (hinting deltas per ppem) or a VariationIndex table (an
//! `(outer, inner)` row in the `GDEF` ItemVariationStore). The offset
//! is relative to the table that holds the record, so when the
//! subsetter rebuilds that table it has to copy each Device table and
//! point the offset at the copy. The tables hold no glyph ids, and the
//! `GDEF` rewrite keeps the ItemVariationStore rows where they were,
//! so the copies travel unchanged.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::layout::{patch_offset16, read_u16};

/// ValueFormat bits of the four device offset fields, in field order.
const DEVICE_BITS: [u16; 4] = [0x0010, 0x0020, 0x0040, 0x0080];

/// ValueFormat bits that each add one 16-bit field to a record.
const DEFINED_BITS: u16 = 0x00FF;

/// `deltaFormat` of a VariationIndex table.
const VARIATION_INDEX: u16 = 0x8000;

/// Number of bytes a `ValueRecord` with the given format occupies.
pub(crate) fn value_record_size(format: u16) -> usize {
    (format & DEFINED_BITS).count_ones() as usize * 2
}

/// Returns the Device or VariationIndex table at `offset` in `parent`.
/// Returns `None` for a null offset, an unknown `deltaFormat`, or a
/// table that runs past `parent`.
pub(crate) fn device_at(parent: &[u8], offset: usize) -> Option<&[u8]> {
    if offset == 0 {
        return None;
    }
    let table = parent.get(offset..)?;
    let start_size = usize::from(read_u16(table, 0)?);
    let end_size = usize::from(read_u16(table, 2)?);
    let len = match read_u16(table, 4)? {
        // Hinting formats pack 2, 4, or 8 bits per ppem.
        format @ 1..=3 if start_size <= end_size => {
            2 * (4 + ((end_size - start_size) >> (4 - usize::from(format))))
        }
        VARIATION_INDEX => 6,
        _ => return None,
    };
    table.get(..len)
}

/// Value records copied out of a source table, plus the Device tables
/// their offset fields point at.
#[derive(Debug, Clone)]
pub(crate) struct Values<'a> {
    /// The records, back to back. Device offset fields still hold
    /// their source values until [`DeviceWriter::finish`] patches
    /// them.
    bytes: Vec<u8>,
    /// Each non-null device field: its position in `bytes` and the
    /// table it points at.
    devices: Vec<(usize, &'a [u8])>,
}

impl<'a> Values<'a> {
    /// Copies the value records in `records`, which repeat the layout
    /// `formats` describes (one format per record) until the slice
    /// ends. Device offsets resolve against `parent`, the table that
    /// holds the records. An offset that does not resolve to a Device
    /// or VariationIndex table is written as null.
    pub(crate) fn read(parent: &'a [u8], records: &[u8], formats: &[u16]) -> Self {
        let mut bytes = records.to_vec();
        let mut devices = Vec::new();
        let stride: usize = formats.iter().map(|&f| value_record_size(f)).sum();
        let mut group = 0;
        while stride != 0 && group + stride <= bytes.len() {
            let mut record = group;
            for &format in formats {
                for bit in DEVICE_BITS.iter().filter(|&&bit| format & bit != 0) {
                    let fields_before = (format & (bit - 1) & DEFINED_BITS).count_ones() as usize;
                    let slot = record + 2 * fields_before;
                    let offset = read_u16(&bytes, slot).map_or(0, usize::from);
                    match device_at(parent, offset) {
                        Some(table) => devices.push((slot, table)),
                        None => bytes[slot..slot + 2].fill(0),
                    }
                }
                record += value_record_size(format);
            }
            group += stride;
        }
        Self { bytes, devices }
    }
}

/// Collects the Device tables of the records written into one rebuilt
/// table. [`DeviceWriter::finish`] appends them after everything else
/// and points each offset at its copy. Records that share a source
/// Device table share one copy.
#[derive(Default)]
pub(crate) struct DeviceWriter<'a> {
    /// `(offset field position in the output, source table)`.
    pending: Vec<(usize, &'a [u8])>,
}

impl<'a> DeviceWriter<'a> {
    /// Appends `values` to `out` and queues its Device tables.
    pub(crate) fn push(&mut self, out: &mut Vec<u8>, values: &Values<'a>) {
        let at = out.len();
        out.extend_from_slice(&values.bytes);
        self.pending.extend(
            values
                .devices
                .iter()
                .map(|&(slot, table)| (at + slot, table)),
        );
    }

    /// Appends the queued Device tables to `out` and patches their
    /// offsets, which are relative to the start of `out`. Returns
    /// `None` when an offset does not fit in 16 bits.
    pub(crate) fn finish(self, out: &mut Vec<u8>) -> Option<()> {
        // Keyed by the source table's address, so two records that
        // point at the same source table get the same copy.
        let mut copies: BTreeMap<usize, usize> = BTreeMap::new();
        for (slot, table) in self.pending {
            let key = table.as_ptr() as usize;
            let pos = match copies.get(&key) {
                Some(&pos) => pos,
                None => {
                    let pos = out.len();
                    out.extend_from_slice(table);
                    copies.insert(key, pos);
                    pos
                }
            };
            patch_offset16(out, slot, pos)?;
        }
        Some(())
    }
}

/// Copies the Anchor table that starts at `anchor`. A format 3 anchor
/// carries its Device tables right after its 10 fixed bytes, with the
/// offsets patched to point at them. Returns `None` for an unknown
/// format or a truncated table.
pub(crate) fn copy_anchor(anchor: &[u8]) -> Option<Vec<u8>> {
    let len = match read_u16(anchor, 0)? {
        1 => 6,
        2 => 8,
        3 => 10,
        _ => return None,
    };
    let mut out = anchor.get(..len)?.to_vec();
    if len == 10 {
        let mut writer = DeviceWriter::default();
        for slot in [6, 8] {
            let offset = read_u16(&out, slot).map_or(0, usize::from);
            match device_at(anchor, offset) {
                Some(table) => writer.pending.push((slot, table)),
                None => out[slot..slot + 2].fill(0),
            }
        }
        writer.finish(&mut out)?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn be(words: &[u16]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_be_bytes()).collect()
    }

    #[test]
    fn device_sizes_follow_the_delta_format() {
        // VariationIndex: outer, inner, 0x8000.
        let var_index = be(&[1, 2, 0x8000]);
        assert_eq!(device_at(&var_index, 0), None, "null offset");
        let mut parent = vec![0, 0];
        parent.extend_from_slice(&var_index);
        assert_eq!(device_at(&parent, 2), Some(&var_index[..]));

        // Format 1, ppem 9..=17: nine 2-bit values fit two words.
        let mut hinting = vec![0, 0];
        hinting.extend_from_slice(&be(&[9, 17, 1, 0xAAAA, 0x8000]));
        assert_eq!(device_at(&hinting, 2).map(<[u8]>::len), Some(10));

        // Format 3, ppem 10..=12: three 8-bit values fit two words.
        let mut wide = vec![0, 0];
        wide.extend_from_slice(&be(&[10, 12, 3, 0x0102, 0x0300]));
        assert_eq!(device_at(&wide, 2).map(<[u8]>::len), Some(10));
    }

    #[test]
    fn unknown_or_truncated_devices_are_rejected() {
        let mut parent = vec![0, 0];
        parent.extend_from_slice(&be(&[0, 0, 0x4000]));
        assert_eq!(device_at(&parent, 2), None, "unknown deltaFormat");
        let mut truncated = vec![0, 0];
        truncated.extend_from_slice(&be(&[9, 40, 3, 0]));
        assert_eq!(device_at(&truncated, 2), None, "runs past the parent");
        assert_eq!(device_at(&parent, 100), None, "offset past the parent");
    }

    #[test]
    fn values_carry_their_device_tables_to_the_new_parent() {
        // Parent: one ValueRecord (xAdvance + xAdvDevice) at 0, then a
        // VariationIndex table at 4.
        let format = 0x0004 | 0x0040;
        let parent = be(&[0xFFF6, 4, 0, 3, 0x8000]);
        let values = Values::read(&parent, &parent[..4], &[format]);

        // New parent: a 2-byte header, the record, then the device.
        let mut out = vec![0xAB, 0xCD];
        let mut writer = DeviceWriter::default();
        writer.push(&mut out, &values);
        writer.push(&mut out, &values);
        writer.finish(&mut out).unwrap();

        // Both records point at one shared copy right after them.
        assert_eq!(out, be(&[0xABCD, 0xFFF6, 10, 0xFFF6, 10, 0, 3, 0x8000]));
    }

    #[test]
    fn unresolvable_device_offsets_become_null() {
        let format = 0x0004 | 0x0040;
        let parent = be(&[5, 200]);
        let values = Values::read(&parent, &parent, &[format]);
        let mut out = Vec::new();
        let mut writer = DeviceWriter::default();
        writer.push(&mut out, &values);
        writer.finish(&mut out).unwrap();
        assert_eq!(out, be(&[5, 0]));
    }

    #[test]
    fn format3_anchor_brings_its_devices() {
        // Anchor format 3 at 0 with x/yDevice at 10 and 16.
        let anchor = be(&[3, 100, 200, 10, 16, 0, 1, 0x8000, 0, 2, 0x8000]);
        let copy = copy_anchor(&anchor).unwrap();
        assert_eq!(copy, anchor);

        // Format 1 anchors copy their 6 bytes only.
        let plain = be(&[1, 7, 8, 0xFFFF]);
        assert_eq!(copy_anchor(&plain).unwrap(), be(&[1, 7, 8]));
        assert_eq!(copy_anchor(&be(&[9, 0, 0])), None);
    }
}
