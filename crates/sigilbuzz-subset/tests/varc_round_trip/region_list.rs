//! MultiVarStore region-list pruning and region index renumbering.

use sigilbuzz::{Blob, Face};
use sigilbuzz_subset::{subset, SubsetInput};

use crate::fixtures::{
    build_cff2_index_test, build_component_with_var, build_square_glyph, record,
};

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
    // Region list: `regions.len()` regions, each constraining axis 0.
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
/// leaves regions 2,3 unreferenced. The region-list prune must drop
/// those, leaving exactly 2 regions in the output.
fn build_multi_region_mvs_varc() -> Vec<u8> {
    // Each subtable carries 5 delta-set entries with run-of-2 i8s
    // (one delta per region, since both subtables reference 2 regions).
    // ctrl = 0x01 (no zero, no words, run_len=2).
    let make_entries = |base: u8| -> Vec<Vec<u8>> {
        (0..5)
            .map(|i| {
                vec![
                    0x01_u8,
                    base.wrapping_add(i as u8),
                    base.wrapping_add(i as u8 + 1),
                ]
            })
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

    // gid 1: 3 components -> subtable 0, inners 0..2.
    let rec1 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 0, 0, 1, 1));
        r.extend(build_component_with_var(5, 0, 1, 2, 2));
        r.extend(build_component_with_var(5, 0, 2, 3, 3));
        r
    };
    // gid 2: 2 components -> subtable 0, inners 3,4.
    let rec2 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 0, 3, 4, 4));
        r.extend(build_component_with_var(5, 0, 4, 5, 5));
        r
    };
    // gid 3: 3 components -> subtable 1, inners 0..2.
    let rec3 = {
        let mut r = Vec::new();
        r.extend(build_component_with_var(5, 1, 0, 6, 6));
        r.extend(build_component_with_var(5, 1, 1, 7, 7));
        r.extend(build_component_with_var(5, 1, 2, 8, 8));
        r
    };
    // gid 4: 2 components -> subtable 1, inners 3,4.
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
    // VARC header: u16 major + u16 minor + 5 x Offset32. varStore is
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
        // sub: u8 format + u16 regionIndexCount + regionIndexes[u16]...
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

    // Subset to keep gids 1, 2 -> subtable 1 dropped -> regions 2, 3 orphan.
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

    // Region list pruned 4 -> 2.
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
    // (i.e. the renumbered region indexes still produce valid scalars).
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
    assert_eq!(
        out.bytes, again.bytes,
        "region-prune output not deterministic"
    );

    // Size delta surfacing, gated on an env var so CI noise stays low.
    if std::env::var_os("SIGILBUZZ_REPORT_MVS_DELTA").is_some() {
        eprintln!(
            "VARC region prune: source = {} bytes, output = {} bytes (delta {})",
            src_varc_bytes.len(),
            new_varc_bytes.len(),
            src_varc_bytes.len() - new_varc_bytes.len(),
        );
    }
}
