//! Byte-level builders shared by the VARC round-trip tests: SFNT table
//! records, the base square glyph, and MultiVarIdx component records.

/// Pack one TableRecord as 16 BE bytes (tag, checksum=0, offset, length).
pub(crate) fn record(tag: [u8; 4], offset: u32, length: u32) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(&tag);
    buf[8..12].copy_from_slice(&offset.to_be_bytes());
    buf[12..16].copy_from_slice(&length.to_be_bytes());
    buf
}

pub(crate) fn build_square_glyph() -> Vec<u8> {
    let mut g = Vec::new();
    g.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
    g.extend_from_slice(&0i16.to_be_bytes()); // xMin
    g.extend_from_slice(&0i16.to_be_bytes()); // yMin
    g.extend_from_slice(&100i16.to_be_bytes()); // xMax
    g.extend_from_slice(&100i16.to_be_bytes()); // yMax
    g.extend_from_slice(&3u16.to_be_bytes()); // endPts[0] = 3
    g.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
    g.extend_from_slice(&[0x01u8; 4]); // ON_CURVE flags
    for d in [0i16, 100, 0, -100] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    for d in [0i16, 0, 100, 0] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    g
}

const VC_TRANSFORM_HAS_VARIATION: u32 = 1 << 3;
const VC_HAVE_TRANSLATE_X: u32 = 1 << 4;
const VC_HAVE_TRANSLATE_Y: u32 = 1 << 5;

/// Builds one component record:
/// flags = TRANSFORM_HAS_VARIATION | HAVE_TRANSLATE_X | HAVE_TRANSLATE_Y
/// (all three bits sit below 0x80, so flags fits in a one-byte uint32var).
/// var_idx = (outer << 16) | inner, encoded as a two-byte uint32var when
/// the value spans more than 7 bits (our test values do).
pub(crate) fn build_component_with_var(
    gid: u16,
    outer: u16,
    inner: u16,
    tx: i16,
    ty: i16,
) -> Vec<u8> {
    let flags = (VC_TRANSFORM_HAS_VARIATION | VC_HAVE_TRANSLATE_X | VC_HAVE_TRANSLATE_Y) as u8;
    assert!(flags < 0x80, "flags must fit in one-byte uint32var");
    let mut rec = Vec::new();
    rec.push(flags);
    rec.extend_from_slice(&gid.to_be_bytes());
    let var_idx = (u32::from(outer) << 16) | u32::from(inner);
    rec.extend_from_slice(&encode_uint32var_test(var_idx));
    rec.extend_from_slice(&tx.to_be_bytes());
    rec.extend_from_slice(&ty.to_be_bytes());
    rec
}

/// Mirror of the in-tree `encode_uint32var` so the test fixture can
/// produce VARC component records the parser will accept.
fn encode_uint32var_test(v: u32) -> Vec<u8> {
    if v <= 0x7F {
        vec![v as u8]
    } else if v <= 0x3FFF {
        vec![((v >> 8) & 0x3F) as u8 | 0x80, (v & 0xFF) as u8]
    } else if v <= 0x001F_FFFF {
        vec![
            ((v >> 16) & 0x1F) as u8 | 0xC0,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        ]
    } else if v <= 0x0FFF_FFFF {
        vec![
            ((v >> 24) & 0x0F) as u8 | 0xE0,
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
        ]
    } else {
        let mut o = vec![0xF0_u8];
        o.extend_from_slice(&v.to_be_bytes());
        o
    }
}

/// Builds a CFF2 INDEX with 1-byte offsets over `entries`.
pub(crate) fn build_cff2_index_test(entries: &[&[u8]]) -> Vec<u8> {
    let count = entries.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&count.to_be_bytes());
    if entries.is_empty() {
        return out;
    }
    out.push(1);
    let mut cursor: u32 = 1;
    out.push(cursor as u8);
    for e in entries {
        cursor += e.len() as u32;
        out.push(cursor as u8);
    }
    for e in entries {
        out.extend_from_slice(e);
    }
    out
}
