//! VARC subset round-trip integration tests.
//!
//! Builds the same synthetic 3-glyph VARC font that `tests/varc_synthetic.rs`
//! exercises in the parser crate (gid 0 .notdef, gid 1 VARC composite of
//! gid 2, gid 2 a 100×100 square), then runs the subsetter and asserts:
//!
//! - Closure expansion pulls gid 2 into the kept set when the caller asks
//!   only for gid 1.
//! - The output VARC has one coverage entry pointing at the renumbered
//!   gid 1.
//! - Each component gid in the output VARC matches the new gid map.
//! - The output font's gid 1 outline at default coords matches the
//!   source's gid 1 outline.
//! - When the kept set has only gid 2 (a base glyph not VARC-covered),
//!   the VARC table is omitted from the output entirely.

use sigilbuzz::tables::PathOp;
use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

/// Pack one TableRecord as 16 BE bytes (tag, checksum=0, offset, length).
fn record(tag: [u8; 4], offset: u32, length: u32) -> [u8; 16] {
    let mut buf = [0u8; 16];
    buf[0..4].copy_from_slice(&tag);
    buf[8..12].copy_from_slice(&offset.to_be_bytes());
    buf[12..16].copy_from_slice(&length.to_be_bytes());
    buf
}

fn build_square_glyph() -> Vec<u8> {
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

/// Builds a minimal VARC table where gid 1 is a composite of gid 2
/// translated by (50, 25).
fn build_minimal_varc() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    let cov_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // coverage
    out.extend_from_slice(&0u32.to_be_bytes()); // varStore = none
    out.extend_from_slice(&0u32.to_be_bytes()); // conditionList = none
    out.extend_from_slice(&0u32.to_be_bytes()); // axisIndicesList = none
    let gr_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // glyphRecords

    let cov_off = out.len() as u32;
    out[cov_slot..cov_slot + 4].copy_from_slice(&cov_off.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&1u16.to_be_bytes()); // glyphCount
    out.extend_from_slice(&1u16.to_be_bytes()); // gid 1

    // Component record: HAVE_TRANSLATE_X | HAVE_TRANSLATE_Y = 0x30.
    let mut rec = Vec::new();
    rec.push(0x30);
    rec.extend_from_slice(&2u16.to_be_bytes()); // gid 2
    rec.extend_from_slice(&50i16.to_be_bytes()); // tx
    rec.extend_from_slice(&25i16.to_be_bytes()); // ty

    let gr_off = out.len() as u32;
    out[gr_slot..gr_slot + 4].copy_from_slice(&gr_off.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes()); // count
    out.push(1); // off_size
    out.push(1); // offsets[0] = 1
    out.push(1u8 + rec.len() as u8); // offsets[1]
    out.extend_from_slice(&rec);
    out
}

#[allow(clippy::too_many_lines)]
fn build_synthetic_varc_font() -> Vec<u8> {
    let head = {
        let mut h = Vec::new();
        h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0u32.to_be_bytes());
        h.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h.extend_from_slice(&1024u16.to_be_bytes());
        h.extend_from_slice(&0u64.to_be_bytes());
        h.extend_from_slice(&0u64.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&200i16.to_be_bytes());
        h.extend_from_slice(&200i16.to_be_bytes());
        h.extend_from_slice(&0u16.to_be_bytes());
        h.extend_from_slice(&8u16.to_be_bytes());
        h.extend_from_slice(&2i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat = short
        h.extend_from_slice(&0i16.to_be_bytes());
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h
    };

    let maxp = {
        let mut m = Vec::new();
        m.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        m.extend_from_slice(&3u16.to_be_bytes()); // numGlyphs
        while m.len() % 4 != 0 {
            m.push(0);
        }
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
        h.extend_from_slice(&3u16.to_be_bytes()); // numberOfHMetrics
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h
    };

    let hmtx = {
        let mut m = Vec::new();
        for _ in 0..3 {
            m.extend_from_slice(&500u16.to_be_bytes());
            m.extend_from_slice(&0i16.to_be_bytes());
        }
        while m.len() % 4 != 0 {
            m.push(0);
        }
        m
    };

    let square = build_square_glyph();
    let mut glyf = Vec::new();
    let off0 = glyf.len();
    let off1 = glyf.len();
    let off2 = glyf.len();
    glyf.extend_from_slice(&square);
    while glyf.len() % 2 != 0 {
        glyf.push(0);
    }
    let off3 = glyf.len();
    while glyf.len() % 4 != 0 {
        glyf.push(0);
    }

    let loca = {
        let mut l = Vec::new();
        for off in [off0, off1, off2, off3] {
            l.extend_from_slice(&((off / 2) as u16).to_be_bytes());
        }
        while l.len() % 4 != 0 {
            l.push(0);
        }
        l
    };

    // Minimal cmap format 4 mapping nothing useful — exists so subset
    // doesn't choke trying to rebuild it. (`subset_cmap` requires a
    // cmap on the source.)
    let cmap = {
        let mut c = Vec::new();
        c.extend_from_slice(&0u16.to_be_bytes()); // version
        c.extend_from_slice(&1u16.to_be_bytes()); // numTables
        c.extend_from_slice(&0u16.to_be_bytes()); // platformID = 0 (Unicode)
        c.extend_from_slice(&3u16.to_be_bytes()); // encodingID = 3 (Unicode 2.0 BMP)
        let off_slot = c.len();
        c.extend_from_slice(&0u32.to_be_bytes()); // offset
        let sub_off = c.len() as u32;
        c[off_slot..off_slot + 4].copy_from_slice(&sub_off.to_be_bytes());
        // Subtable format 4 with 1 segment mapping just 0xFFFF -> 0.
        // length = 24, segCountX2 = 2, searchRange/entrySelector/rangeShift trivial.
        c.extend_from_slice(&4u16.to_be_bytes()); // format
        c.extend_from_slice(&24u16.to_be_bytes()); // length
        c.extend_from_slice(&0u16.to_be_bytes()); // language
        c.extend_from_slice(&2u16.to_be_bytes()); // segCountX2
        c.extend_from_slice(&2u16.to_be_bytes()); // searchRange
        c.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
        c.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
        c.extend_from_slice(&0xFFFFu16.to_be_bytes()); // endCount[0]
        c.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
        c.extend_from_slice(&0xFFFFu16.to_be_bytes()); // startCount[0]
        c.extend_from_slice(&1u16.to_be_bytes()); // idDelta[0]
        c.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset[0]
        while c.len() % 4 != 0 {
            c.push(0);
        }
        c
    };

    // post format 3.
    let post = {
        let mut p = Vec::new();
        p.extend_from_slice(&0x0003_0000u32.to_be_bytes()); // version 3.0
        p.extend_from_slice(&0u32.to_be_bytes()); // italicAngle
        p.extend_from_slice(&0i16.to_be_bytes()); // underlinePosition
        p.extend_from_slice(&0i16.to_be_bytes()); // underlineThickness
        p.extend_from_slice(&0u32.to_be_bytes()); // isFixedPitch
        p.extend_from_slice(&0u32.to_be_bytes()); // minMemType42
        p.extend_from_slice(&0u32.to_be_bytes()); // maxMemType42
        p.extend_from_slice(&0u32.to_be_bytes()); // minMemType1
        p.extend_from_slice(&0u32.to_be_bytes()); // maxMemType1
        while p.len() % 4 != 0 {
            p.push(0);
        }
        p
    };

    // name table: minimal valid empty table.
    let name = {
        let mut n = Vec::new();
        n.extend_from_slice(&0u16.to_be_bytes()); // format
        n.extend_from_slice(&0u16.to_be_bytes()); // count
        n.extend_from_slice(&6u16.to_be_bytes()); // stringOffset
        while n.len() % 4 != 0 {
            n.push(0);
        }
        n
    };

    let varc = {
        let mut v = build_minimal_varc();
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v
    };

    let payloads: Vec<([u8; 4], &[u8])> = vec![
        (*b"VARC", varc.as_slice()),
        (*b"cmap", cmap.as_slice()),
        (*b"glyf", glyf.as_slice()),
        (*b"head", head.as_slice()),
        (*b"hhea", hhea.as_slice()),
        (*b"hmtx", hmtx.as_slice()),
        (*b"loca", loca.as_slice()),
        (*b"maxp", maxp.as_slice()),
        (*b"name", name.as_slice()),
        (*b"post", post.as_slice()),
    ];

    let num_tables = payloads.len() as u16;
    let header_len = 12 + num_tables as usize * 16;

    let mut out = Vec::with_capacity(header_len);
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

#[test]
fn varc_subset_pulls_components_into_closure_and_renumbers() {
    let bytes = build_synthetic_varc_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    // Sanity: source has VARC covering gid 1 referencing gid 2.
    let src_varc = face.varc().unwrap().expect("source has VARC");
    assert!(src_varc.covers(1));
    assert!(!src_varc.covers(2));

    // Subset to keep only gid 1. Closure must pull gid 2 in.
    let input = SubsetInput {
        gids: vec![1],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: true,
    };
    let out = subset(&face, &input).expect("subset succeeds");

    // gid 0 + gid 1 + gid 2 all kept (closure pulled gid 2).
    let new_gid_map: Vec<(u16, u16)> = out.gid_map.clone();
    assert!(
        new_gid_map.iter().any(|(o, _)| *o == 1),
        "gid 1 must be in the kept set"
    );
    assert!(
        new_gid_map.iter().any(|(o, _)| *o == 2),
        "VARC closure must pull gid 2 into the kept set"
    );

    // Re-parse the output font.
    let new_blob = Blob::from_vec(out.bytes.clone());
    let new_face = Face::parse(&new_blob, 0).expect("subset font parses");

    // Output VARC: 1 covered entry, points at the renumbered gid 1.
    let new_varc = new_face
        .varc()
        .expect("varc parse")
        .expect("output font has VARC");
    let new_gid1 = new_gid_map.iter().find(|(o, _)| *o == 1).unwrap().1;
    let new_gid2 = new_gid_map.iter().find(|(o, _)| *o == 2).unwrap().1;
    assert!(
        new_varc.covers(new_gid1),
        "output VARC must cover the renumbered gid (was 1, now {new_gid1})",
    );
    assert_eq!(new_varc.glyph_record_count(), 1);

    // Component gid in the rewritten record must be the renumbered gid 2.
    let comp = new_varc.composite(new_gid1, &[]).unwrap();
    assert_eq!(comp.components.len(), 1);
    assert_eq!(
        comp.components[0].gid, new_gid2,
        "component must reference the renumbered child gid",
    );

    // Translation preserved verbatim.
    assert!((comp.components[0].transform[4] - 50.0).abs() < 1e-3);
    assert!((comp.components[0].transform[5] - 25.0).abs() < 1e-3);

    // gid 1 outline at default coords matches the source's gid 1 (square
    // shifted by (50, 25)).
    let src_outline = face.glyph_outline(1).unwrap().expect("source gid 1");
    let new_outline = new_face
        .glyph_outline(new_gid1)
        .unwrap()
        .expect("subset gid 1");
    let extract = |o: &sigilbuzz::tables::Outline| -> Vec<(f32, f32)> {
        o.ops()
            .iter()
            .filter_map(|op| match op {
                PathOp::MoveTo { x, y } | PathOp::LineTo { x, y } => Some((*x, *y)),
                _ => None,
            })
            .collect()
    };
    let src_pts = extract(&src_outline);
    let new_pts = extract(&new_outline);
    assert_eq!(
        src_pts.len(),
        new_pts.len(),
        "outline point count must match"
    );
    for ((sx, sy), (nx, ny)) in src_pts.iter().zip(new_pts.iter()) {
        assert!((sx - nx).abs() < 1e-3, "x mismatch: {sx} vs {nx}");
        assert!((sy - ny).abs() < 1e-3, "y mismatch: {sy} vs {ny}");
    }
}

#[test]
fn varc_subset_drops_table_when_no_covered_gid_kept() {
    let bytes = build_synthetic_varc_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    // Keep only gid 2 (a base glyph not VARC-covered). VARC must be
    // omitted entirely from the output.
    let input = SubsetInput {
        gids: vec![2],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: true,
    };
    let out = subset(&face, &input).expect("subset succeeds");

    let new_blob = Blob::from_vec(out.bytes.clone());
    let new_face = Face::parse(&new_blob, 0).expect("subset font parses");

    // VARC was dropped — gid 1 was not in the kept set, so coverage
    // would be empty, and the subsetter omits the whole table.
    let varc_opt = new_face.varc().expect("varc accessor");
    assert!(
        varc_opt.is_none(),
        "VARC must be omitted when no kept gid is covered",
    );
}

#[test]
fn varc_subset_is_deterministic() {
    // Subsetting the same input twice yields byte-identical output.
    let bytes = build_synthetic_varc_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let input = SubsetInput {
        gids: vec![1],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: true,
    };
    let a = subset(&face, &input).expect("subset succeeds");
    let b = subset(&face, &input).expect("subset succeeds");
    assert_eq!(a.bytes, b.bytes);
}
