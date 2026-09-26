//! SFNT directory builder with checksum maintenance.
//!
//! Per the OpenType spec, each table in the directory carries a
//! 32-bit checksum: sum of every u32 in the table's byte run, padded
//! to a 4-byte boundary with zeros. The font as a whole has its own
//! `head.checkSumAdjustment` field, which is computed last:
//!
//! ```text
//!   head.checkSumAdjustment = 0xB1B0AFBA - sum_of(every u32 in the
//!                                                    final font file,
//!                                                    treating
//!                                                    head.checkSumAdjustment
//!                                                    as zero during the sum)
//! ```
//!
//! Tables are written in ascending tag order and the directory entries
//! match. Output is byte-deterministic for a given (sfnt_version,
//! tables) input.

use alloc::vec::Vec;

const CHECKSUM_MAGIC: u32 = 0xB1B0_AFBA;
const HEAD_TAG: [u8; 4] = *b"head";

/// Builds a complete SFNT byte buffer from `tables`. Tables are
/// passed in any order; the directory output is sorted by tag.
pub fn build(sfnt_version: u32, tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut sorted: Vec<&([u8; 4], Vec<u8>)> = tables.iter().collect();
    sorted.sort_by_key(|(t, _)| *t);

    // Callers pass at most one table per source directory entry, and a
    // directory holds at most `u16::MAX` entries.
    let num_tables = sorted.len() as u16;
    // Spec searchRange / entrySelector / rangeShift derivation. The
    // values are informational only (every modern SFNT consumer
    // ignores them), but emit them correctly anyway. The power of two
    // is tracked in `usize` so doubling past 32768 cannot overflow.
    let mut entry_selector: u16 = 0;
    let mut sr_pow: usize = 1;
    while sr_pow * 2 <= usize::from(num_tables) {
        sr_pow *= 2;
        entry_selector += 1;
    }
    let search_range = u16::try_from(sr_pow * 16).unwrap_or(u16::MAX);
    let range_shift = num_tables.saturating_mul(16).saturating_sub(search_range);

    let header_len = 12 + sorted.len() * 16;
    // Each table is padded to a 4-byte boundary.
    let mut total = header_len;
    for (_, body) in &sorted {
        total += round_up_4(body.len());
    }
    let mut buf = Vec::with_capacity(total);

    // Header.
    buf.extend_from_slice(&sfnt_version.to_be_bytes());
    buf.extend_from_slice(&num_tables.to_be_bytes());
    buf.extend_from_slice(&search_range.to_be_bytes());
    buf.extend_from_slice(&entry_selector.to_be_bytes());
    buf.extend_from_slice(&range_shift.to_be_bytes());

    // Reserve directory space; we patch checksums + offsets after
    // we know each table's position. Directory layout: tag(4),
    // checksum(4), offset(4), length(4) = 16 bytes.
    let directory_off = buf.len();
    for _ in &sorted {
        buf.extend_from_slice(&[0u8; 16]);
    }

    // Track per-table (offset, length, checksum) as we lay them
    // down. head's checksum is computed against its bytes-with-
    // checkSumAdjustment-zeroed slice; we set the field to 0
    // before writing and patch the final value at the end.
    let mut metas: Vec<(usize, usize, u32)> = Vec::with_capacity(sorted.len());

    for (tag, body) in &sorted {
        let off = buf.len();
        // Pad-to-4 length is what goes in the file, but `length`
        // in the directory is the unpadded byte count (per spec).
        let padded_len = round_up_4(body.len());
        let mut padded = body.clone();
        if *tag == HEAD_TAG && padded.len() >= 12 {
            // Zero out checkSumAdjustment for the table's own checksum.
            padded[8..12].copy_from_slice(&0u32.to_be_bytes());
        }
        while padded.len() < padded_len {
            padded.push(0);
        }
        let cs = checksum(&padded);
        buf.extend_from_slice(&padded);
        metas.push((off, body.len(), cs));
    }

    // Patch directory.
    for (i, ((tag, _), (off, len, cs))) in sorted.iter().zip(metas.iter()).enumerate() {
        let dir = directory_off + i * 16;
        buf[dir..dir + 4].copy_from_slice(tag);
        buf[dir + 4..dir + 8].copy_from_slice(&cs.to_be_bytes());
        buf[dir + 8..dir + 12].copy_from_slice(&(*off as u32).to_be_bytes());
        buf[dir + 12..dir + 16].copy_from_slice(&(*len as u32).to_be_bytes());
    }

    // Locate head and patch checkSumAdjustment.
    let head_pos = sorted
        .iter()
        .zip(metas.iter())
        .find(|((t, _), _)| *t == HEAD_TAG)
        .map(|(_, (off, _, _))| *off);

    if let Some(head_off) = head_pos {
        let csa_off = head_off + 8;
        // First, zero out the field (we already wrote zero into the
        // table body for the per-table checksum, but the file-wide
        // sum below assumes csa is currently zero. Confirm by
        // overwriting).
        if csa_off + 4 <= buf.len() {
            buf[csa_off..csa_off + 4].copy_from_slice(&0u32.to_be_bytes());
            let file_sum = checksum(&buf);
            let csa = CHECKSUM_MAGIC.wrapping_sub(file_sum);
            buf[csa_off..csa_off + 4].copy_from_slice(&csa.to_be_bytes());
        }
    }

    buf
}

fn round_up_4(n: usize) -> usize {
    (n + 3) & !3
}

/// Sum of every u32 in `data`, big-endian, zero-padded if the length
/// is not a multiple of four.
fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 4 <= data.len() {
        let w = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        sum = sum.wrapping_add(w);
        i += 4;
    }
    if i < data.len() {
        let mut tail = [0u8; 4];
        for (j, b) in data[i..].iter().enumerate() {
            tail[j] = *b;
        }
        sum = sum.wrapping_add(u32::from_be_bytes(tail));
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_up_4_works() {
        assert_eq!(round_up_4(0), 0);
        assert_eq!(round_up_4(1), 4);
        assert_eq!(round_up_4(4), 4);
        assert_eq!(round_up_4(5), 8);
    }

    #[test]
    fn checksum_handles_padding() {
        // Five bytes: [0x01,0x02,0x03,0x04,0x05] ->
        //   word0 = 0x01020304
        //   word1 (padded) = 0x05000000
        //   sum = 0x06020304
        assert_eq!(
            checksum(&[0x01, 0x02, 0x03, 0x04, 0x05]),
            0x0102_0304u32.wrapping_add(0x0500_0000u32),
        );
    }

    #[test]
    fn build_emits_sorted_directory() {
        // Two trivial tables; directory should be tag-sorted.
        let tables: alloc::vec::Vec<([u8; 4], alloc::vec::Vec<u8>)> = alloc::vec![
            (*b"name", alloc::vec![1, 2, 3, 4]),
            (*b"head", alloc::vec![0u8; 54]),
        ];
        let bytes = build(0x0001_0000, &tables);
        // numTables word.
        assert_eq!(&bytes[4..6], &2u16.to_be_bytes());
        // First record tag should be "head" since it sorts before "name".
        assert_eq!(&bytes[12..16], b"head");
        assert_eq!(&bytes[28..32], b"name");
    }

    #[test]
    fn build_handles_more_than_32768_tables() {
        // A source directory can list up to 65535 tables. Doubling the
        // searchRange power of two past 32768 used to overflow u16,
        // which panicked in debug builds and looped forever in release.
        let tables: alloc::vec::Vec<([u8; 4], alloc::vec::Vec<u8>)> = (0..40_000u32)
            .map(|i| (i.to_be_bytes(), alloc::vec::Vec::new()))
            .collect();
        let bytes = build(0x0001_0000, &tables);
        assert_eq!(&bytes[4..6], &40_000u16.to_be_bytes());
        // searchRange saturates, entrySelector = floor(log2(40000)).
        assert_eq!(&bytes[6..8], &u16::MAX.to_be_bytes());
        assert_eq!(&bytes[8..10], &15u16.to_be_bytes());
    }
}
