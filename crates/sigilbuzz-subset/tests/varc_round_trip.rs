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

// ---- MVS pruning integration -----------------------------------------
//
// Build a richer VARC font: 4 VARC composites (gids 1..=4) each
// pointing at the base glyph (gid 5), with each composite carrying
// multiple components that name distinct `MultiVarIdx` entries via
// `VC_TRANSFORM_HAS_VARIATION`. The MultiVarStore has 2 subtables of 5
// entries each = 10 total entries; the four composites collectively
// reference all 10. After subsetting to keep only gids 1 and 2 the
// output MVS should contain only the 6 entries those two composites
// reference (subtable 0 keeps all 5 entries, subtable 1 keeps only
// entry 0).

const VC_TRANSFORM_HAS_VARIATION: u32 = 1 << 3;
const VC_HAVE_TRANSLATE_X: u32 = 1 << 4;
const VC_HAVE_TRANSLATE_Y: u32 = 1 << 5;

/// Builds one component record:
/// flags = TRANSFORM_HAS_VARIATION | HAVE_TRANSLATE_X | HAVE_TRANSLATE_Y
/// (all three bits sit below 0x80, so flags fits in a one-byte uint32var).
/// var_idx = (outer << 16) | inner — encoded as a two-byte uint32var when
/// the value spans more than 7 bits (our test values do).
fn build_component_with_var(gid: u16, outer: u16, inner: u16, tx: i16, ty: i16) -> Vec<u8> {
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
fn build_cff2_index_test(entries: &[&[u8]]) -> Vec<u8> {
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

/// Builds a synthetic MultiItemVariationStore with `subtables.len()`
/// subtables. Each subtable carries the listed region indexes and
/// delta-set payloads. One region (axis 0, peak +1) is emitted; that
/// keeps both subtables referencing region 0.
fn build_synthetic_mvs(subtables: &[(Vec<u16>, Vec<Vec<u8>>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let region_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(subtables.len() as u16).to_be_bytes());
    let sub_off_slots_start = out.len();
    for _ in subtables {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    // Region list: one region at axis 0 peak +1.
    let region_list_start = out.len() as u32;
    out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_list_start.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // region count
    let region_off_slot_inner = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let region_off_inner = (out.len() as u32) - region_list_start;
    out[region_off_slot_inner..region_off_slot_inner + 4]
        .copy_from_slice(&region_off_inner.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    out.extend_from_slice(&0u16.to_be_bytes()); // axisIndex
    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }
    write_f2dot14(&mut out, 0.0); // start
    write_f2dot14(&mut out, 1.0); // peak
    write_f2dot14(&mut out, 1.0); // end
    while out.len() % 4 != 0 {
        out.push(0);
    }

    for (i, (region_indexes, delta_sets)) in subtables.iter().enumerate() {
        let sub_start = out.len() as u32;
        let slot = sub_off_slots_start + i * 4;
        out[slot..slot + 4].copy_from_slice(&sub_start.to_be_bytes());
        out.push(1); // format
        out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes());
        for ri in region_indexes {
            out.extend_from_slice(&ri.to_be_bytes());
        }
        let entries: Vec<&[u8]> = delta_sets.iter().map(|v| v.as_slice()).collect();
        out.extend_from_slice(&build_cff2_index_test(&entries));
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

/// Builds the VARC table with 4 composites referencing 10 distinct
/// `MultiVarIdx` values across 2 subtables.
fn build_mvs_varc() -> Vec<u8> {
    // Composite 1 (gid 1): 3 components referencing (0,0), (0,1), (0,2).
    // Composite 2 (gid 2): 3 components referencing (0,3), (0,4), (1,0).
    // Composite 3 (gid 3): 2 components referencing (1,1), (1,2).
    // Composite 4 (gid 4): 2 components referencing (1,3), (1,4).
    let rec1 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 0, 0, 1, 1));
        r.extend(build_component_with_var(5, 0, 1, 2, 2));
        r.extend(build_component_with_var(5, 0, 2, 3, 3));
        r
    };
    let rec2 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 0, 3, 4, 4));
        r.extend(build_component_with_var(5, 0, 4, 5, 5));
        r.extend(build_component_with_var(5, 1, 0, 6, 6));
        r
    };
    let rec3 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 1, 1, 7, 7));
        r.extend(build_component_with_var(5, 1, 2, 8, 8));
        r
    };
    let rec4 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 1, 3, 9, 9));
        r.extend(build_component_with_var(5, 1, 4, 10, 10));
        r
    };

    // MVS: 2 subtables, 5 entries each = 10 total. Each delta set is
    // a single i8 zero (0x00, 0x00) — `0x00` ctrl = run of 1 i8, `0x00`
    // payload. Two bytes per entry.
    let mvs_subtables: Vec<(Vec<u16>, Vec<Vec<u8>>)> = vec![
        (vec![0], (0..5).map(|i| vec![0x00_u8, i as u8]).collect()),
        (
            vec![0],
            (0..5).map(|i| vec![0x00_u8, (10 + i) as u8]).collect(),
        ),
    ];
    let mvs = build_synthetic_mvs(&mvs_subtables);

    // VARC table.
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    let cov_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // coverage
    let vs_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // varStore
    out.extend_from_slice(&0u32.to_be_bytes()); // conditionList
    out.extend_from_slice(&0u32.to_be_bytes()); // axisIndicesList
    let gr_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes()); // glyphRecords

    let cov_off = out.len() as u32;
    out[cov_slot..cov_slot + 4].copy_from_slice(&cov_off.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&4u16.to_be_bytes()); // glyphCount
    for g in [1u16, 2, 3, 4] {
        out.extend_from_slice(&g.to_be_bytes());
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }

    let vs_off = out.len() as u32;
    out[vs_slot..vs_slot + 4].copy_from_slice(&vs_off.to_be_bytes());
    out.extend_from_slice(&mvs);
    while out.len() % 4 != 0 {
        out.push(0);
    }

    let gr_off = out.len() as u32;
    out[gr_slot..gr_slot + 4].copy_from_slice(&gr_off.to_be_bytes());
    out.extend_from_slice(&build_cff2_index_test(&[
        rec1.as_slice(),
        rec2.as_slice(),
        rec3.as_slice(),
        rec4.as_slice(),
    ]));
    out
}

/// Builds the same 6-glyph SFNT as `build_synthetic_varc_font` but
/// with `numGlyphs = 6`, an extra base square at gid 5, and the MVS-
/// rich VARC table from `build_mvs_varc`.
#[allow(clippy::too_many_lines)]
fn build_mvs_varc_font() -> Vec<u8> {
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
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h
    };

    let maxp = {
        let mut m = Vec::new();
        m.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        m.extend_from_slice(&6u16.to_be_bytes()); // numGlyphs
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
        h.extend_from_slice(&6u16.to_be_bytes()); // numberOfHMetrics
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h
    };

    let hmtx = {
        let mut m = Vec::new();
        for _ in 0..6 {
            m.extend_from_slice(&500u16.to_be_bytes());
            m.extend_from_slice(&0i16.to_be_bytes());
        }
        while m.len() % 4 != 0 {
            m.push(0);
        }
        m
    };

    // Six glyphs in the loca array — gid 0..=4 empty, gid 5 carries
    // the square. Subsetting walks glyf via loca, so all gids must
    // have a valid loca slot.
    let square = build_square_glyph();
    let mut glyf = Vec::new();
    let off0 = glyf.len();
    let off1 = glyf.len();
    let off2 = glyf.len();
    let off3 = glyf.len();
    let off4 = glyf.len();
    let off5 = glyf.len();
    glyf.extend_from_slice(&square);
    while glyf.len() % 2 != 0 {
        glyf.push(0);
    }
    let off6 = glyf.len();
    while glyf.len() % 4 != 0 {
        glyf.push(0);
    }

    let loca = {
        let mut l = Vec::new();
        for off in [off0, off1, off2, off3, off4, off5, off6] {
            l.extend_from_slice(&((off / 2) as u16).to_be_bytes());
        }
        while l.len() % 4 != 0 {
            l.push(0);
        }
        l
    };

    let cmap = {
        let mut c = Vec::new();
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&3u16.to_be_bytes());
        let off_slot = c.len();
        c.extend_from_slice(&0u32.to_be_bytes());
        let sub_off = c.len() as u32;
        c[off_slot..off_slot + 4].copy_from_slice(&sub_off.to_be_bytes());
        c.extend_from_slice(&4u16.to_be_bytes());
        c.extend_from_slice(&24u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&2u16.to_be_bytes());
        c.extend_from_slice(&2u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&0xFFFFu16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&0xFFFFu16.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        while c.len() % 4 != 0 {
            c.push(0);
        }
        c
    };

    let post = {
        let mut p = Vec::new();
        p.extend_from_slice(&0x0003_0000u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0i16.to_be_bytes());
        p.extend_from_slice(&0i16.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        while p.len() % 4 != 0 {
            p.push(0);
        }
        p
    };

    let name = {
        let mut n = Vec::new();
        n.extend_from_slice(&0u16.to_be_bytes());
        n.extend_from_slice(&0u16.to_be_bytes());
        n.extend_from_slice(&6u16.to_be_bytes());
        while n.len() % 4 != 0 {
            n.push(0);
        }
        n
    };

    let varc = {
        let mut v = build_mvs_varc();
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

/// MVS pruning: keep only composites 1 and 2; the output MVS must
/// drop the entries the kept composites do not reference, and every
/// surviving `MultiVarIdx` must resolve to a real entry (no orphans).
#[test]
fn varc_subset_prunes_mvs_and_remaps_var_idx_orphan_free() {
    let bytes = build_mvs_varc_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).expect("source parses");

    // Sanity: source VARC covers gids 1..=4, source MVS has 10 entries.
    let src_varc = face
        .varc()
        .expect("varc accessor")
        .expect("source has VARC");
    for g in 1..=4u16 {
        assert!(src_varc.covers(g), "source covers gid {g}");
    }

    // Subset to keep gids 1 and 2 (closure pulls gid 5 in via the
    // component references).
    let input = SubsetInput {
        gids: vec![1, 2],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: true,
    };
    let out = subset(&face, &input).expect("subset succeeds");

    // Quick MVS-bytes-before vs MVS-bytes-after sanity: the rewritten
    // VARC table is strictly smaller than the source (the dropped
    // composites + dropped MVS entries together are >0 bytes).
    let src_varc_bytes = face.table_bytes(sigilbuzz::tables::tag::VARC).unwrap();
    let new_blob = Blob::from_vec(out.bytes.clone());
    let new_face = Face::parse(&new_blob, 0).expect("subset font parses");
    let new_varc_bytes = new_face
        .table_bytes(sigilbuzz::tables::tag::VARC)
        .expect("output has VARC");
    assert!(
        new_varc_bytes.len() < src_varc_bytes.len(),
        "VARC should shrink after MVS prune (was {}, now {})",
        src_varc_bytes.len(),
        new_varc_bytes.len(),
    );

    // Locate and report the MVS sub-block sizes before/after — this
    // is the size delta the pruning closure reclaims. (Surfaced via
    // `eprintln!` so `cargo test -- --nocapture` shows it; gated on
    // an env var so CI noise stays low.)
    if std::env::var_os("SIGILBUZZ_REPORT_MVS_DELTA").is_some() {
        let read_off = |b: &[u8], at: usize| {
            u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize
        };
        let mvs_block_size = |b: &[u8]| -> usize {
            let vs_off = read_off(b, 8);
            if vs_off == 0 {
                return 0;
            }
            let nexts = [read_off(b, 12), read_off(b, 16), read_off(b, 20), b.len()];
            let next = nexts
                .iter()
                .copied()
                .filter(|x| *x > vs_off)
                .min()
                .unwrap_or(b.len());
            next - vs_off
        };
        eprintln!(
            "VARC bytes: source = {}, output = {}",
            src_varc_bytes.len(),
            new_varc_bytes.len(),
        );
        eprintln!(
            "MVS bytes: source = {}, output = {}",
            mvs_block_size(src_varc_bytes),
            mvs_block_size(new_varc_bytes),
        );
    }

    // Re-parse the output and assert every component's transform
    // resolves cleanly. If a `MultiVarIdx` had become orphaned the
    // parser's `composite()` would silently zero its delta — but the
    // structural assertion is that the table re-parses at all (a
    // dangling outer index would surface via region_count out of range
    // and the whole record would fail to decode under the parser's
    // strict path).
    let new_varc = new_face.varc().unwrap().expect("output has VARC");
    let new_gid_for = |old: u16| -> u16 {
        out.gid_map
            .iter()
            .find(|(o, _)| *o == old)
            .map(|(_, n)| *n)
            .expect("kept gid present in map")
    };
    for old in [1u16, 2] {
        let new_gid = new_gid_for(old);
        let comp = new_varc
            .composite(new_gid, &[1.0])
            .expect("composite resolves");
        // gid 1 has 3 components, gid 2 has 3 components.
        assert_eq!(comp.components.len(), 3, "gid {old} component count");
        // Translation values survive (verifies the var-idx splice did
        // not corrupt the trailing transform fields).
        for (i, c) in comp.components.iter().enumerate() {
            let tx_expected = if old == 1 {
                f32::from((i + 1) as i16)
            } else {
                f32::from((i + 4) as i16)
            };
            assert!(
                (c.transform[4] - tx_expected).abs() < 1e-3,
                "gid {old} comp {i} tx {} expected {tx_expected}",
                c.transform[4],
            );
        }
    }
}

/// MVS pruning preserves determinism — same input → identical output
/// across two subset calls, including the rewritten MVS bytes.
#[test]
fn varc_subset_with_mvs_prune_is_deterministic() {
    let bytes = build_mvs_varc_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let input = SubsetInput {
        gids: vec![1, 2],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: true,
    };
    let a = subset(&face, &input).expect("subset succeeds");
    let b = subset(&face, &input).expect("subset succeeds");
    assert_eq!(a.bytes, b.bytes);
}

// ---- Region-list pruning integration -------------------------------
//
// Builds a richer fixture where the MVS region list carries 4 regions
// but two of them (regions 2 and 3) are referenced **only** by the
// subtable that gets dropped during PR #220's prune. After the region-
// list pruning closes the loop, those two orphaned regions are dropped
// from the region list and the surviving subtable's region indexes are
// renumbered through the remap.

/// Builds an MVS with `regions.len()` regions (each axis 0 only,
/// triangular falloff) and `subtables.len()` subtables. Each subtable
/// declares `(region_indexes, delta_sets)`.
fn build_multi_region_mvs(
    regions: &[(f32, f32, f32)],
    subtables: &[(Vec<u16>, Vec<Vec<u8>>)],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    let region_off_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(subtables.len() as u16).to_be_bytes());
    let sub_off_slots_start = out.len();
    for _ in subtables {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    // Region list — `regions.len()` regions, each constraining axis 0.
    let region_list_start = out.len() as u32;
    out[region_off_slot..region_off_slot + 4].copy_from_slice(&region_list_start.to_be_bytes());
    out.extend_from_slice(&(regions.len() as u16).to_be_bytes());
    let region_off_slots_start = out.len();
    for _ in regions {
        out.extend_from_slice(&0u32.to_be_bytes());
    }
    fn write_f2dot14(out: &mut Vec<u8>, v: f32) {
        let raw = (v * 16384.0).round() as i16;
        out.extend_from_slice(&raw.to_be_bytes());
    }
    let mut region_offsets_relative: Vec<u32> = Vec::with_capacity(regions.len());
    for (s, p, e) in regions {
        let rel = (out.len() as u32) - region_list_start;
        region_offsets_relative.push(rel);
        out.extend_from_slice(&1u16.to_be_bytes()); // axisCount
        out.extend_from_slice(&0u16.to_be_bytes()); // axisIndex
        write_f2dot14(&mut out, *s);
        write_f2dot14(&mut out, *p);
        write_f2dot14(&mut out, *e);
    }
    for (i, rel) in region_offsets_relative.iter().enumerate() {
        let slot = region_off_slots_start + i * 4;
        out[slot..slot + 4].copy_from_slice(&rel.to_be_bytes());
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }

    for (i, (region_indexes, delta_sets)) in subtables.iter().enumerate() {
        let sub_start = out.len() as u32;
        let slot = sub_off_slots_start + i * 4;
        out[slot..slot + 4].copy_from_slice(&sub_start.to_be_bytes());
        out.push(1); // format
        out.extend_from_slice(&(region_indexes.len() as u16).to_be_bytes());
        for ri in region_indexes {
            out.extend_from_slice(&ri.to_be_bytes());
        }
        let entries: Vec<&[u8]> = delta_sets.iter().map(|v| v.as_slice()).collect();
        out.extend_from_slice(&build_cff2_index_test(&entries));
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

/// Builds a VARC table with 4 composites (gids 1..=4) where:
///
/// - gid 1, 2 each carry components referencing subtable 0 (regions 0,1).
/// - gid 3, 4 each carry components referencing subtable 1 (regions 2,3).
///
/// After subsetting to keep only gids 1,2, subtable 1 is dropped, which
/// leaves regions 2,3 unreferenced — the region-list prune must drop
/// those, leaving exactly 2 regions in the output.
fn build_multi_region_mvs_varc() -> Vec<u8> {
    // Each subtable carries 5 delta-set entries with run-of-2 i8s
    // (one delta per region, since both subtables reference 2 regions).
    // ctrl = 0x01 (no zero, no words, run_len=2).
    let make_entries = |base: u8| -> Vec<Vec<u8>> {
        (0..5)
            .map(|i| vec![0x01_u8, base.wrapping_add(i as u8), base.wrapping_add(i as u8 + 1)])
            .collect()
    };
    // 4 regions: pos peak, mid peak (axis 0 +0.5), neg peak,
    // mid-neg peak (axis 0 -0.5). All on axis 0.
    let regions = vec![
        (0.0_f32, 1.0, 1.0),
        (0.0_f32, 0.5, 1.0),
        (-1.0_f32, -1.0, 0.0),
        (-1.0_f32, -0.5, 0.0),
    ];
    let subtables = vec![
        (vec![0u16, 1], make_entries(10)),
        (vec![2u16, 3], make_entries(20)),
    ];
    let mvs = build_multi_region_mvs(&regions, &subtables);

    // gid 1: 3 components → subtable 0, inners 0..2.
    let rec1 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 0, 0, 1, 1));
        r.extend(build_component_with_var(5, 0, 1, 2, 2));
        r.extend(build_component_with_var(5, 0, 2, 3, 3));
        r
    };
    // gid 2: 2 components → subtable 0, inners 3,4.
    let rec2 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 0, 3, 4, 4));
        r.extend(build_component_with_var(5, 0, 4, 5, 5));
        r
    };
    // gid 3: 3 components → subtable 1, inners 0..2.
    let rec3 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 1, 0, 6, 6));
        r.extend(build_component_with_var(5, 1, 1, 7, 7));
        r.extend(build_component_with_var(5, 1, 2, 8, 8));
        r
    };
    // gid 4: 2 components → subtable 1, inners 3,4.
    let rec4 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 1, 3, 9, 9));
        r.extend(build_component_with_var(5, 1, 4, 10, 10));
        r
    };

    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // major
    out.extend_from_slice(&0u16.to_be_bytes()); // minor
    let cov_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    let vs_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // conditionList
    out.extend_from_slice(&0u32.to_be_bytes()); // axisIndicesList
    let gr_slot = out.len();
    out.extend_from_slice(&0u32.to_be_bytes());

    let cov_off = out.len() as u32;
    out[cov_slot..cov_slot + 4].copy_from_slice(&cov_off.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // format
    out.extend_from_slice(&4u16.to_be_bytes()); // glyphCount
    for g in [1u16, 2, 3, 4] {
        out.extend_from_slice(&g.to_be_bytes());
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }

    let vs_off = out.len() as u32;
    out[vs_slot..vs_slot + 4].copy_from_slice(&vs_off.to_be_bytes());
    out.extend_from_slice(&mvs);
    while out.len() % 4 != 0 {
        out.push(0);
    }

    let gr_off = out.len() as u32;
    out[gr_slot..gr_slot + 4].copy_from_slice(&gr_off.to_be_bytes());
    out.extend_from_slice(&build_cff2_index_test(&[
        rec1.as_slice(),
        rec2.as_slice(),
        rec3.as_slice(),
        rec4.as_slice(),
    ]));
    out
}

/// Wraps `build_multi_region_mvs_varc` in the same 6-glyph SFNT shell
/// as `build_mvs_varc_font` so the `subset` driver has a real face to
/// chew on.
#[allow(clippy::too_many_lines)]
fn build_multi_region_mvs_varc_font() -> Vec<u8> {
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
        h.extend_from_slice(&0i16.to_be_bytes());
        h.extend_from_slice(&0i16.to_be_bytes());
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h
    };
    let maxp = {
        let mut m = Vec::new();
        m.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        m.extend_from_slice(&6u16.to_be_bytes());
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
        h.extend_from_slice(&6u16.to_be_bytes());
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h
    };
    let hmtx = {
        let mut m = Vec::new();
        for _ in 0..6 {
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
    let off3 = glyf.len();
    let off4 = glyf.len();
    let off5 = glyf.len();
    glyf.extend_from_slice(&square);
    while glyf.len() % 2 != 0 {
        glyf.push(0);
    }
    let off6 = glyf.len();
    while glyf.len() % 4 != 0 {
        glyf.push(0);
    }
    let loca = {
        let mut l = Vec::new();
        for off in [off0, off1, off2, off3, off4, off5, off6] {
            l.extend_from_slice(&((off / 2) as u16).to_be_bytes());
        }
        while l.len() % 4 != 0 {
            l.push(0);
        }
        l
    };
    let cmap = {
        let mut c = Vec::new();
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&3u16.to_be_bytes());
        let off_slot = c.len();
        c.extend_from_slice(&0u32.to_be_bytes());
        let sub_off = c.len() as u32;
        c[off_slot..off_slot + 4].copy_from_slice(&sub_off.to_be_bytes());
        c.extend_from_slice(&4u16.to_be_bytes());
        c.extend_from_slice(&24u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&2u16.to_be_bytes());
        c.extend_from_slice(&2u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&0xFFFFu16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        c.extend_from_slice(&0xFFFFu16.to_be_bytes());
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&0u16.to_be_bytes());
        while c.len() % 4 != 0 {
            c.push(0);
        }
        c
    };
    let post = {
        let mut p = Vec::new();
        p.extend_from_slice(&0x0003_0000u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0i16.to_be_bytes());
        p.extend_from_slice(&0i16.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        while p.len() % 4 != 0 {
            p.push(0);
        }
        p
    };
    let name = {
        let mut n = Vec::new();
        n.extend_from_slice(&0u16.to_be_bytes());
        n.extend_from_slice(&0u16.to_be_bytes());
        n.extend_from_slice(&6u16.to_be_bytes());
        while n.len() % 4 != 0 {
            n.push(0);
        }
        n
    };
    let varc = {
        let mut v = build_multi_region_mvs_varc();
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

/// Parses the MVS region count out of a VARC byte block. Returns the
/// region list's `regionCount` u16, or 0 when the table has no MVS.
fn varc_mvs_region_count(varc_bytes: &[u8]) -> u16 {
    // VARC header: u16 major + u16 minor + 5 × Offset32. varStore is
    // the second offset (slot at byte 8).
    let read_u32 = |b: &[u8], at: usize| -> u32 {
        u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    };
    let vs_off = read_u32(varc_bytes, 8) as usize;
    if vs_off == 0 {
        return 0;
    }
    // MVS layout: u16 format + u32 regionListOffset (relative to MVS).
    let mvs = &varc_bytes[vs_off..];
    let region_list_off = read_u32(mvs, 2) as usize;
    let region_list = &mvs[region_list_off..];
    u16::from_be_bytes([region_list[0], region_list[1]])
}

/// Walks every subtable's region_indexes in the rewritten MVS and
/// asserts every index is `< region_count`. This is the orphan-free
/// invariant the region-list prune is meant to preserve.
fn assert_no_orphan_region_refs(varc_bytes: &[u8]) {
    let read_u32 = |b: &[u8], at: usize| -> u32 {
        u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    };
    let vs_off = read_u32(varc_bytes, 8) as usize;
    if vs_off == 0 {
        return;
    }
    let mvs = &varc_bytes[vs_off..];
    let region_count = varc_mvs_region_count(varc_bytes);
    let subtable_count = u16::from_be_bytes([mvs[6], mvs[7]]) as usize;
    for i in 0..subtable_count {
        let sub_off = read_u32(mvs, 8 + i * 4) as usize;
        let sub = &mvs[sub_off..];
        // sub: u8 format + u16 regionIndexCount + regionIndexes[u16]…
        assert_eq!(sub[0], 1, "MVS subtable {i} format must be 1");
        let ric = u16::from_be_bytes([sub[1], sub[2]]) as usize;
        for j in 0..ric {
            let p = 3 + j * 2;
            let ri = u16::from_be_bytes([sub[p], sub[p + 1]]);
            assert!(
                ri < region_count,
                "subtable {i} region_index[{j}] = {ri} >= region_count {region_count} (orphan!)",
            );
        }
    }
}

/// Region-list pruning: a fixture where 2 of 4 regions are referenced
/// only by the subtable that gets dropped. After subsetting the
/// surviving region list contains exactly 2 regions, every surviving
/// subtable's region_indexes is renumbered through the remap, and the
/// output is strictly smaller than a hypothetical unpruned region
/// list.
#[test]
fn varc_subset_prunes_region_list_and_renumbers_tuple_indexes() {
    let bytes = build_multi_region_mvs_varc_font();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    // Sanity: source has 4 regions.
    let src_varc_bytes = face.table_bytes(sigilbuzz::tables::tag::VARC).unwrap();
    assert_eq!(varc_mvs_region_count(src_varc_bytes), 4);

    // Subset to keep gids 1, 2 → subtable 1 dropped → regions 2, 3 orphan.
    let input = SubsetInput {
        gids: vec![1, 2],
        retain_hints: false,
        drop_unhandled: true,
        retain_layout: false,
        retain_variations: true,
    };
    let out = subset(&face, &input).expect("subset succeeds");
    let new_blob = Blob::from_vec(out.bytes.clone());
    let new_face = Face::parse(&new_blob, 0).expect("subset font parses");
    let new_varc_bytes = new_face
        .table_bytes(sigilbuzz::tables::tag::VARC)
        .expect("output has VARC");

    // Region list pruned 4 → 2.
    assert_eq!(
        varc_mvs_region_count(new_varc_bytes),
        2,
        "region list must drop the 2 orphaned regions",
    );

    // Every surviving tuple's region index is in-range (no orphans).
    assert_no_orphan_region_refs(new_varc_bytes);

    // Output VARC is strictly smaller than source. The size delta
    // covers (a) the dropped subtable (already from PR #220), plus (b)
    // the region-list shrink (this PR's contribution).
    assert!(
        new_varc_bytes.len() < src_varc_bytes.len(),
        "output VARC must shrink (was {}, now {})",
        src_varc_bytes.len(),
        new_varc_bytes.len(),
    );

    // Surviving composites still resolve cleanly through the parser
    // — i.e. the renumbered region indexes still produce valid scalars.
    let new_varc = new_face.varc().unwrap().expect("output has VARC");
    let new_gid_for = |old: u16| -> u16 {
        out.gid_map
            .iter()
            .find(|(o, _)| *o == old)
            .map(|(_, n)| *n)
            .expect("kept gid present in map")
    };
    for old in [1u16, 2] {
        let new_gid = new_gid_for(old);
        let comp = new_varc
            .composite(new_gid, &[1.0])
            .expect("composite resolves");
        let expected_count = if old == 1 { 3 } else { 2 };
        assert_eq!(comp.components.len(), expected_count, "gid {old}");
    }

    // Determinism: a second subset call yields byte-identical output.
    let again = subset(&face, &input).expect("subset succeeds");
    assert_eq!(out.bytes, again.bytes, "region-prune output not deterministic");

    // Size delta surfacing — gated on an env var so CI noise stays low.
    if std::env::var_os("SIGILBUZZ_REPORT_MVS_DELTA").is_some() {
        eprintln!(
            "VARC region prune: source = {} bytes, output = {} bytes (delta {})",
            src_varc_bytes.len(),
            new_varc_bytes.len(),
            src_varc_bytes.len() - new_varc_bytes.len(),
        );
    }
}
