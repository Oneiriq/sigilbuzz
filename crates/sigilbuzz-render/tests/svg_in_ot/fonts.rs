//! Synthetic SFNT builders carrying an `SVG ` table, shared by the
//! SVG-in-OT tests.

pub(crate) fn record(tag: [u8; 4], offset: u32, length: u32) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(&tag);
    buf[8..12].copy_from_slice(&offset.to_be_bytes());
    buf[12..16].copy_from_slice(&length.to_be_bytes());
    buf
}

pub(crate) fn align4(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

/// Builds a minimum-viable SFNT carrying head, maxp, hhea, hmtx, loca,
/// glyf (with one empty glyph), and an `SVG ` table whose record 0
/// covers gid 1 with the supplied SVG payload.
pub(crate) fn build_svg_font(svg_payload: &[u8]) -> Vec<u8> {
    let head = {
        let mut h = Vec::new();
        h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h.extend_from_slice(&1024u16.to_be_bytes()); // upem
        h.extend_from_slice(&0u64.to_be_bytes());
        h.extend_from_slice(&0u64.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&500i16.to_be_bytes());
        h.extend_from_slice(&500i16.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h.extend_from_slice(&8u16.to_be_bytes());
        h.extend_from_slice(&2i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat = short
        h.extend_from_slice(&0i16.to_be_bytes());
        align4(&mut h);
        h
    };
    let maxp = {
        let mut m = Vec::new();
        m.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        m.extend_from_slice(&2u16.to_be_bytes()); // 2 glyphs (gid 0, gid 1)
        align4(&mut m);
        m
    };
    let hhea = {
        let mut h = Vec::new();
        h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        h.extend_from_slice(&800i16.to_be_bytes());
        h.extend_from_slice(&(-200i16).to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&500u16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&500i16.to_be_bytes());
        h.extend_from_slice(&1i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        for _ in 0..4 {
            h.extend_from_slice(&0i16.to_be_bytes());
        }
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&2u16.to_be_bytes());
        align4(&mut h);
        h
    };
    let hmtx = {
        let mut m = Vec::new();
        for _ in 0..2 {
            m.extend_from_slice(&500u16.to_be_bytes());
            m.extend_from_slice(&0i16.to_be_bytes());
        }
        align4(&mut m);
        m
    };
    // Empty glyf: both gids point at offset 0, length 0.
    let mut glyf = Vec::new();
    align4(&mut glyf);
    let loca = {
        let mut l = Vec::new();
        for _ in 0..3 {
            l.extend_from_slice(&0u16.to_be_bytes());
        }
        align4(&mut l);
        l
    };

    // SVG ` table.
    let svg = build_svg_table(&[(1, 1, svg_payload)]);

    let payloads: Vec<([u8; 4], &[u8])> = vec![
        (*b"SVG ", svg.as_slice()),
        (*b"glyf", glyf.as_slice()),
        (*b"head", head.as_slice()),
        (*b"hhea", hhea.as_slice()),
        (*b"hmtx", hmtx.as_slice()),
        (*b"loca", loca.as_slice()),
        (*b"maxp", maxp.as_slice()),
    ];

    let num_tables = payloads.len() as u16;
    let header_len = 12 + num_tables as usize * 16;
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&num_tables.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());

    let mut cursor = header_len as u32;
    let mut directory: Vec<[u8; 16]> = Vec::with_capacity(payloads.len());
    for (tag, body) in &payloads {
        directory.push(record(*tag, cursor, body.len() as u32));
        cursor += body.len() as u32;
    }
    for d in &directory {
        out.extend_from_slice(d);
    }
    for (_, body) in &payloads {
        out.extend_from_slice(body);
    }
    out
}

/// Builds the byte image of an `SVG ` table holding the supplied
/// `(start_gid, end_gid, payload)` records. Mirrors the helper used in
/// the core `tests/svg_in_ot.rs` fixture so the layout is identical.
pub(crate) fn build_svg_table(records: &[(u16, u16, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&10u32.to_be_bytes()); // documentListOffset = 10
    out.extend_from_slice(&0u32.to_be_bytes()); // reserved

    // Document List Index at offset 10.
    let list_start = out.len();
    assert_eq!(list_start, 10);
    out.extend_from_slice(&(records.len() as u16).to_be_bytes());

    let records_pos = out.len();
    out.resize(records_pos + records.len() * 12, 0);

    let mut entries = Vec::with_capacity(records.len());
    for (s, e, payload) in records {
        let doc_off = (out.len() - list_start) as u32;
        let doc_len = payload.len() as u32;
        out.extend_from_slice(payload);
        entries.push((*s, *e, doc_off, doc_len));
    }

    for (i, (s, e, off, len)) in entries.iter().enumerate() {
        let dst = records_pos + i * 12;
        out[dst..dst + 2].copy_from_slice(&s.to_be_bytes());
        out[dst + 2..dst + 4].copy_from_slice(&e.to_be_bytes());
        out[dst + 4..dst + 8].copy_from_slice(&off.to_be_bytes());
        out[dst + 8..dst + 12].copy_from_slice(&len.to_be_bytes());
    }

    out
}
