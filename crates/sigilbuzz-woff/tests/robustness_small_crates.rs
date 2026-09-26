//! Regression tests for hostile WOFF1, WOFF2, and SFNT input.
//!
//! Every test here feeds hand-built bytes through the public API. Each
//! input used to panic, hang, or allocate far more memory than the
//! input justified. The contract now is a clean `Ok` or `Err`.

use sigilbuzz_woff::{unwrap_woff1, wrap_woff1};
#[cfg(feature = "woff2")]
use sigilbuzz_woff::{unwrap_woff2, wrap_woff2};

const WOFF1_HEADER_LEN: usize = 44;
const WOFF1_ENTRY_LEN: usize = 20;

/// Builds a WOFF1 header with the given table count. Every other
/// header field is zero.
fn woff1_header(num_tables: u16) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"wOFF");
    v.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // flavor
    v.extend_from_slice(&0u32.to_be_bytes()); // length
    v.extend_from_slice(&num_tables.to_be_bytes());
    v.extend_from_slice(&0u16.to_be_bytes()); // reserved
    v.extend_from_slice(&[0u8; 28]); // totalSfntSize and the header tail
    assert_eq!(v.len(), WOFF1_HEADER_LEN);
    v
}

fn woff1_entry(v: &mut Vec<u8>, tag: &[u8; 4], offset: u32, comp: u32, orig: u32) {
    v.extend_from_slice(tag);
    v.extend_from_slice(&offset.to_be_bytes());
    v.extend_from_slice(&comp.to_be_bytes());
    v.extend_from_slice(&orig.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes()); // origChecksum
}

/// Builds an SFNT whose directory has `num_tables` records that all
/// point at the same `table_len` bytes starting at offset 0. The file
/// is zero-padded so that every record is in bounds.
fn sfnt_with_shared_tables(num_tables: u16, table_len: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    v.extend_from_slice(&num_tables.to_be_bytes());
    v.extend_from_slice(&[0u8; 6]);
    for i in 0..num_tables {
        v.extend_from_slice(&u32::from(i).to_be_bytes()); // tag
        v.extend_from_slice(&0u32.to_be_bytes()); // checksum
        v.extend_from_slice(&0u32.to_be_bytes()); // offset
        v.extend_from_slice(&table_len.to_be_bytes());
    }
    v.resize(v.len().max(table_len as usize), 0);
    v
}

/// Minimized fuzzer crash input. It is a TrueType SFNT whose `loca`
/// tag was mutated to `0oca`, so it carries `glyf` without `loca`.
/// `wrap_woff2` used to hit an `expect` on it.
const FUZZ_GLYF_WITHOUT_LOCA: [u8; 52] = [
    0x00, 0x01, 0x00, 0x00, // sfntVersion
    0x00, 0x02, 0x00, 0x20, 0x00, 0x01, 0x00, 0x00, // numTables, search params
    b'g', b'l', b'y', b'f', 0x00, 0x00, 0x00, 0x00, // tag, checksum
    0x00, 0x00, 0x00, 0x2C, 0x00, 0x00, 0x00, 0x04, // offset 44, length 4
    b'0', b'o', b'c', b'a', 0x00, 0x00, 0x00, 0x00, // tag, checksum
    0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x00, 0x04, // offset 48, length 4
    0x00, 0x00, 0x00, 0x00, // glyf body
    0x00, 0x00, 0x00, 0x00, // 0oca body
];

#[test]
fn fuzz_glyf_without_loca_survives_woff1_round_trips() {
    // The fuzz harness also runs these WOFF1 paths on every input.
    assert!(unwrap_woff1(&FUZZ_GLYF_WITHOUT_LOCA).is_err());
    let woff = wrap_woff1(&FUZZ_GLYF_WITHOUT_LOCA).expect("wraps");
    let sfnt = unwrap_woff1(&woff).expect("unwraps");
    assert_eq!(&sfnt[12 + 32..], &FUZZ_GLYF_WITHOUT_LOCA[44..]);
}

#[test]
fn woff1_unwrap_accepts_the_maximum_table_count() {
    // 65535 empty tables. The SFNT search-parameter helper used to
    // overflow u16 arithmetic here, which panics in debug builds and
    // spins forever in release builds.
    let n = u16::MAX;
    let mut woff = woff1_header(n);
    for i in 0..u32::from(n) {
        woff1_entry(&mut woff, &i.to_be_bytes(), 0, 0, 0);
    }
    let sfnt = unwrap_woff1(&woff).expect("empty tables unwrap");
    assert_eq!(sfnt.len(), 12 + 16 * usize::from(n));
}

#[test]
fn woff1_unwrap_rejects_overlapping_tables() {
    // Two tables share the same four body bytes. The WOFF1 spec
    // requires a reader to reject overlapping table data. Accepting it
    // lets a small file expand into one copy of the shared body per
    // directory entry.
    let mut woff = woff1_header(2);
    let body_offset = (WOFF1_HEADER_LEN + 2 * WOFF1_ENTRY_LEN) as u32;
    woff1_entry(&mut woff, b"aaaa", body_offset, 4, 4);
    woff1_entry(&mut woff, b"bbbb", body_offset, 4, 4);
    woff.extend_from_slice(&[1, 2, 3, 4]);
    assert!(unwrap_woff1(&woff).is_err());
}

#[test]
fn woff1_unwrap_rejects_table_inside_directory() {
    // The table body claims to live inside the header and directory.
    let mut woff = woff1_header(1);
    woff1_entry(&mut woff, b"aaaa", 0, 8, 8);
    assert!(unwrap_woff1(&woff).is_err());
}

#[test]
fn woff1_unwrap_accepts_adjacent_tables() {
    // Back-to-back bodies with no gap must still unwrap.
    let mut woff = woff1_header(2);
    let first = (WOFF1_HEADER_LEN + 2 * WOFF1_ENTRY_LEN) as u32;
    woff1_entry(&mut woff, b"aaaa", first, 4, 4);
    woff1_entry(&mut woff, b"bbbb", first + 4, 4, 4);
    woff.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    let sfnt = unwrap_woff1(&woff).expect("adjacent tables unwrap");
    assert_eq!(&sfnt[12 + 32..], &[1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn woff1_unwrap_rejects_declared_size_past_u32() {
    // Two "compressed" tables that each claim to inflate to 4 GiB.
    // The unwrapper used to reserve the full declared size up front.
    let mut woff = woff1_header(2);
    let first = (WOFF1_HEADER_LEN + 2 * WOFF1_ENTRY_LEN) as u32;
    woff1_entry(&mut woff, b"aaaa", first, 4, u32::MAX);
    woff1_entry(&mut woff, b"bbbb", first + 4, 4, u32::MAX);
    woff.extend_from_slice(&[0u8; 8]);
    assert!(unwrap_woff1(&woff).is_err());
}

#[test]
fn woff1_unwrap_rejects_huge_declared_table_with_tiny_body() {
    // One table claims a 4 GiB inflated size from a 4-byte body.
    let mut woff = woff1_header(1);
    let first = (WOFF1_HEADER_LEN + WOFF1_ENTRY_LEN) as u32;
    woff1_entry(&mut woff, b"aaaa", first, 4, u32::MAX - 64);
    woff.extend_from_slice(&[0x78, 0x9C, 0x03, 0x00]);
    assert!(unwrap_woff1(&woff).is_err());
}

#[test]
fn woff1_wrap_rejects_tables_whose_sum_passes_u32() {
    // 4097 directory records that all cover the same 1 MiB. The sum
    // of table lengths does not fit the u32 fields of a WOFF1 file.
    // The wrapper used to compress every copy and then truncate the
    // offsets it wrote.
    let sfnt = sfnt_with_shared_tables(4097, 1 << 20);
    assert!(wrap_woff1(&sfnt).is_err());
}

#[cfg(feature = "woff2")]
mod woff2 {
    use super::{sfnt_with_shared_tables, unwrap_woff2, wrap_woff2, FUZZ_GLYF_WITHOUT_LOCA};

    #[test]
    fn fuzz_glyf_without_loca_is_rejected() {
        assert!(wrap_woff2(&FUZZ_GLYF_WITHOUT_LOCA).is_err());
        assert!(unwrap_woff2(&FUZZ_GLYF_WITHOUT_LOCA).is_err());
    }

    /// Builds a WOFF2 file from raw directory bytes and a compressed
    /// payload. Every other header field is zero.
    fn woff2_file(num_tables: u16, directory: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"wOF2");
        v.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // flavor
        v.extend_from_slice(&0u32.to_be_bytes()); // length
        v.extend_from_slice(&num_tables.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes()); // reserved
        v.extend_from_slice(&0u32.to_be_bytes()); // totalSfntSize
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(&[0u8; 24]);
        v.extend_from_slice(directory);
        v.extend_from_slice(payload);
        v
    }

    /// A complete Brotli stream that decodes to zero bytes.
    const EMPTY_BROTLI: [u8; 1] = [0x06];

    #[test]
    fn unwrap_accepts_the_maximum_table_count() {
        // 65535 empty `cmap` entries: flag byte 0, origLength 0.
        let n = u16::MAX;
        let directory = [0u8, 0u8].repeat(usize::from(n));
        let woff = woff2_file(n, &directory, &EMPTY_BROTLI);
        let sfnt = unwrap_woff2(&woff).expect("empty tables unwrap");
        assert_eq!(sfnt.len(), 12 + 16 * usize::from(n));
    }

    #[test]
    fn unwrap_rejects_implausible_declared_size() {
        // One `name` table (known tag 5) that claims 4 GiB from a
        // one-byte payload. The unwrapper used to reserve the full
        // declared size before decoding anything.
        let mut directory = vec![5u8];
        // UIntBase128 of 0xFFFF_FFF0.
        directory.extend_from_slice(&[0x8F, 0xFF, 0xFF, 0xFF, 0x70]);
        let woff = woff2_file(1, &directory, &EMPTY_BROTLI);
        assert!(unwrap_woff2(&woff).is_err());
    }

    #[test]
    fn wrap_rejects_glyf_without_loca() {
        // A `glyf` table with no `loca` used to hit an `expect` in the
        // directory builder.
        let mut sfnt = Vec::new();
        sfnt.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        sfnt.extend_from_slice(&1u16.to_be_bytes());
        sfnt.extend_from_slice(&[0u8; 6]);
        sfnt.extend_from_slice(b"glyf");
        sfnt.extend_from_slice(&0u32.to_be_bytes());
        sfnt.extend_from_slice(&28u32.to_be_bytes());
        sfnt.extend_from_slice(&4u32.to_be_bytes());
        sfnt.extend_from_slice(&[0u8; 4]);
        assert!(wrap_woff2(&sfnt).is_err());
    }

    #[test]
    fn wrap_rejects_loca_without_glyf() {
        // A lone `loca` used to produce a file that no reader could
        // unwrap, because the directory promised a transformed table
        // that was never written.
        let mut sfnt = Vec::new();
        sfnt.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        sfnt.extend_from_slice(&1u16.to_be_bytes());
        sfnt.extend_from_slice(&[0u8; 6]);
        sfnt.extend_from_slice(b"loca");
        sfnt.extend_from_slice(&0u32.to_be_bytes());
        sfnt.extend_from_slice(&28u32.to_be_bytes());
        sfnt.extend_from_slice(&4u32.to_be_bytes());
        sfnt.extend_from_slice(&[0u8; 4]);
        assert!(wrap_woff2(&sfnt).is_err());
    }

    #[test]
    fn wrap_rejects_tables_whose_sum_passes_u32() {
        // 4097 records that all cover the same 1 MiB. The wrapper used
        // to copy every record before it checked the total size.
        let sfnt = sfnt_with_shared_tables(4097, 1 << 20);
        assert!(wrap_woff2(&sfnt).is_err());
    }
}
