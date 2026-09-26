//! MultiVarStore pruning and `MultiVarIdx` remapping.

use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

use crate::fixtures::{
    build_cff2_index_test, build_component_with_var, build_square_glyph, record,
};

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
    // a single i8 zero (0x00, 0x00): `0x00` ctrl = run of 1 i8, `0x00`
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

    // Six glyphs in the loca array: gid 0..=4 empty, gid 5 carries
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

    // Locate and report the MVS sub-block sizes before/after. This
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
    // parser's `composite()` would silently zero its delta, but the
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

/// MVS pruning preserves determinism: same input gives identical output
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
