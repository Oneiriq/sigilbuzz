//! Test support: rebuild a TrueType font with some tables swapped.
//!
//! Integration tests use it to graft hand-built layout or variation
//! tables onto a real font, so the subsetter and instancer can be
//! checked against a reference shaper on structures no vendored font
//! carries.

// Every test crate that includes this module uses only some helpers.
#![allow(dead_code)]

/// Rebuilds the SFNT `font` with each `(tag, Some(bytes))` in `edits`
/// added or replacing the table of that tag, and each `(tag, None)`
/// removed. The directory comes out sorted by tag with 4-byte aligned
/// tables and correct table checksums.
pub fn edit_tables(font: &[u8], edits: &[([u8; 4], Option<Vec<u8>>)]) -> Vec<u8> {
    let read_u16 = |pos: usize| u16::from_be_bytes([font[pos], font[pos + 1]]);
    let read_u32 = |pos: usize| {
        u32::from_be_bytes([font[pos], font[pos + 1], font[pos + 2], font[pos + 3]]) as usize
    };
    let mut tables: Vec<([u8; 4], Vec<u8>)> = (0..usize::from(read_u16(4)))
        .map(|i| {
            let rec = 12 + i * 16;
            let tag: [u8; 4] = font[rec..rec + 4].try_into().unwrap();
            let (off, len) = (read_u32(rec + 8), read_u32(rec + 12));
            (tag, font[off..off + len].to_vec())
        })
        .collect();
    for (tag, bytes) in edits {
        tables.retain(|(t, _)| t != tag);
        if let Some(bytes) = bytes {
            tables.push((*tag, bytes.clone()));
        }
    }
    tables.sort_by_key(|(tag, _)| *tag);

    let count = tables.len();
    let mut out = font[0..4].to_vec();
    let search = if count == 0 {
        0
    } else {
        1usize << (usize::BITS - 1 - count.leading_zeros())
    };
    for v in [
        count,
        search * 16,
        search.trailing_zeros() as usize,
        count * 16 - search * 16,
    ] {
        out.extend_from_slice(&(v as u16).to_be_bytes());
    }
    let mut body_at = 12 + count * 16;
    let mut bodies = Vec::new();
    for (tag, bytes) in &tables {
        let checksum = bytes
            .chunks(4)
            .map(|c| {
                let mut word = [0u8; 4];
                word[..c.len()].copy_from_slice(c);
                u32::from_be_bytes(word)
            })
            .fold(0u32, u32::wrapping_add);
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum.to_be_bytes());
        out.extend_from_slice(&(body_at as u32).to_be_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        bodies.extend_from_slice(bytes);
        while bodies.len() % 4 != 0 {
            bodies.push(0);
        }
        body_at = 12 + count * 16 + bodies.len();
    }
    out.extend_from_slice(&bodies);
    out
}

/// A big-endian u16 writer for hand-built tables.
pub fn be16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// A big-endian u32 writer for hand-built tables.
pub fn be32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}
