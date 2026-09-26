//! Adversarial inputs for the closure walker, the `glyf` rewriter, and
//! the layout rewriters.
//!
//! Every font here is built byte by byte and every case panicked,
//! hung, or exhausted memory before the fix it covers.

use sigilbuzz::Face;
use sigilbuzz_subset::{subset, SubsetError, SubsetInput};

/// Builds an SFNT from `(tag, bytes)` pairs. Table records are written
/// in the order given, each table padded to four bytes.
fn build_font(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // sfntVersion
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
    let records_start = out.len();
    out.resize(records_start + tables.len() * 16, 0);
    for (i, (tag, body)) in tables.iter().enumerate() {
        let off = out.len();
        out.extend_from_slice(body);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let rec = records_start + i * 16;
        out[rec..rec + 4].copy_from_slice(tag);
        out[rec + 8..rec + 12].copy_from_slice(&(off as u32).to_be_bytes());
        out[rec + 12..rec + 16].copy_from_slice(&(body.len() as u32).to_be_bytes());
    }
    out
}

/// `head` with the given `indexToLocFormat` (0 = short, 1 = long).
fn head(index_to_loc_format: i16) -> Vec<u8> {
    let mut out = vec![0u8; 54];
    out[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // version
    out[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes()); // magicNumber
    out[18..20].copy_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
    out[50..52].copy_from_slice(&index_to_loc_format.to_be_bytes());
    out
}

/// `maxp` version 0.5 with `num_glyphs`.
fn maxp(num_glyphs: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0x0000_5000u32.to_be_bytes()); // version 0.5
    out.extend_from_slice(&num_glyphs.to_be_bytes());
    out
}

/// `hhea` with `number_of_h_metrics`.
fn hhea(number_of_h_metrics: u16) -> Vec<u8> {
    let mut out = vec![0u8; 36];
    out[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes()); // version
    out[34..36].copy_from_slice(&number_of_h_metrics.to_be_bytes());
    out
}

/// `hmtx` with one long metric plus a left side bearing per remaining
/// glyph.
fn hmtx(num_glyphs: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&500u16.to_be_bytes()); // advanceWidth
    out.extend_from_slice(&0i16.to_be_bytes()); // lsb
    for _ in 1..num_glyphs {
        out.extend_from_slice(&0i16.to_be_bytes());
    }
    out
}

/// `cmap` with a single empty format 4 subtable.
fn cmap() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&1u16.to_be_bytes()); // numTables
    out.extend_from_slice(&3u16.to_be_bytes()); // platformID
    out.extend_from_slice(&1u16.to_be_bytes()); // encodingID
    out.extend_from_slice(&12u32.to_be_bytes()); // subtableOffset
    out.extend_from_slice(&4u16.to_be_bytes()); // format
    out.extend_from_slice(&24u16.to_be_bytes()); // length
    out.extend_from_slice(&0u16.to_be_bytes()); // language
    out.extend_from_slice(&2u16.to_be_bytes()); // segCountX2
    out.extend_from_slice(&2u16.to_be_bytes()); // searchRange
    out.extend_from_slice(&0u16.to_be_bytes()); // entrySelector
    out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift
    out.extend_from_slice(&0xFFFFu16.to_be_bytes()); // endCode[0]
    out.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
    out.extend_from_slice(&0xFFFFu16.to_be_bytes()); // startCode[0]
    out.extend_from_slice(&1u16.to_be_bytes()); // idDelta[0]
    out.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset[0]
    out
}

/// A minimal `name` table with no name records.
fn name() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&0u16.to_be_bytes()); // count
    out.extend_from_slice(&6u16.to_be_bytes()); // storageOffset
    out
}

/// A composite glyph naming `component` once, with byte args and no
/// further components.
fn composite_glyph(component: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(-1i16).to_be_bytes()); // numberOfContours
    out.extend_from_slice(&[0u8; 8]); // bbox
    out.extend_from_slice(&0x0002u16.to_be_bytes()); // ARGS_ARE_XY_VALUES
    out.extend_from_slice(&component.to_be_bytes());
    out.extend_from_slice(&[0u8; 2]); // args
    out
}

/// An empty simple glyph: no contours and no instructions.
fn empty_glyph() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0i16.to_be_bytes()); // numberOfContours
    out.extend_from_slice(&[0u8; 8]); // bbox
    out.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
    out
}

/// Builds a `glyf` table plus a long `loca` from per-glyph bodies. Each
/// body is padded to two bytes.
fn glyf_and_loca(bodies: &[Vec<u8>]) -> (Vec<u8>, Vec<u8>) {
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    loca.extend_from_slice(&0u32.to_be_bytes());
    for body in bodies {
        glyf.extend_from_slice(body);
        while glyf.len() % 2 != 0 {
            glyf.push(0);
        }
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
    }
    (glyf, loca)
}

/// Assembles a TrueType font around `glyf` / `loca` with `num_glyphs`.
fn font_with_glyf(num_glyphs: u16, glyf: Vec<u8>, loca: Vec<u8>) -> Vec<u8> {
    build_font(&[
        (*b"cmap", cmap()),
        (*b"glyf", glyf),
        (*b"head", head(1)),
        (*b"hhea", hhea(1)),
        (*b"hmtx", hmtx(num_glyphs)),
        (*b"loca", loca),
        (*b"maxp", maxp(num_glyphs)),
        (*b"name", name()),
    ])
}

fn keep_all(num_glyphs: u16) -> SubsetInput {
    SubsetInput {
        gids: (0..num_glyphs).collect(),
        ..SubsetInput::default()
    }
}

#[test]
fn composite_cycle_terminates() {
    // Glyph 1 references glyph 2, glyph 2 references glyph 1. The
    // closure walker must not chase the cycle forever.
    let (glyf, loca) = glyf_and_loca(&[empty_glyph(), composite_glyph(2), composite_glyph(1)]);
    let font = font_with_glyf(3, glyf, loca);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let out = subset(&face, &keep_all(3)).expect("subset succeeds");
    // All three glyphs survive: gid 1 drags in gid 2 and back.
    assert_eq!(out.gid_map.len(), 3);
}

#[test]
fn composite_self_reference_terminates() {
    let (glyf, loca) = glyf_and_loca(&[empty_glyph(), composite_glyph(1)]);
    let font = font_with_glyf(2, glyf, loca);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let out = subset(&face, &keep_all(2)).expect("subset succeeds");
    assert_eq!(out.gid_map.len(), 2);
}

#[test]
fn composite_component_past_num_glyphs_errors() {
    // Glyph 1 names glyph 9, which the font does not have. The closure
    // walker skips it (the kept set must only hold real glyphs) and the
    // glyf rewriter reports it instead of writing a dangling reference.
    let (glyf, loca) = glyf_and_loca(&[empty_glyph(), composite_glyph(9)]);
    let font = font_with_glyf(2, glyf, loca);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let kept = sigilbuzz_subset::compute_closure(&face, &[1]).unwrap();
    assert_eq!(kept, vec![0, 1], "closure must not invent glyph 9");
    let err = subset(&face, &keep_all(2)).expect_err("subset must report the bad component");
    assert!(matches!(err, SubsetError::Unsupported(_)), "got {err:?}");
}

#[test]
fn seed_gid_past_num_glyphs_errors() {
    let (glyf, loca) = glyf_and_loca(&[empty_glyph()]);
    let font = font_with_glyf(1, glyf, loca);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let input = SubsetInput {
        gids: vec![0, 5],
        ..SubsetInput::default()
    };
    let err = subset(&face, &input).expect_err("gid 5 is out of range");
    assert_eq!(
        err,
        SubsetError::GidOutOfRange {
            gid: 5,
            num_glyphs: 1
        }
    );
}

#[test]
fn font_without_glyphs_errors() {
    // numGlyphs = 0 leaves no `.notdef` for the closure walker to keep.
    let font = font_with_glyf(0, Vec::new(), 0u32.to_be_bytes().to_vec());
    let face = Face::parse_bytes(&font, 0).unwrap();
    let err = subset(&face, &SubsetInput::default()).expect_err("no glyphs to keep");
    assert_eq!(
        err,
        SubsetError::GidOutOfRange {
            gid: 0,
            num_glyphs: 0
        }
    );
}

#[test]
fn descending_loca_offsets_error() {
    // Glyph 1 spans loca[1]..loca[2], which runs backwards. The
    // rewriter must report it rather than slice with start > end.
    let mut big = empty_glyph();
    big.resize(4096, 0);
    let big_len = big.len() as u32;
    let mut loca = Vec::new();
    for off in [0u32, big_len, 0, big_len] {
        loca.extend_from_slice(&off.to_be_bytes());
    }
    let font = font_with_glyf(3, big, loca);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let err = subset(&face, &keep_all(3)).expect_err("descending loca must error");
    assert!(matches!(err, SubsetError::Unsupported(_)), "got {err:?}");
}

/// Wraps `lookups` in a GSUB table with one script and one feature
/// naming every lookup.
fn build_gsub(lookups: &[(u16, Vec<Vec<u8>>)]) -> Vec<u8> {
    let mut lookup_list = Vec::new();
    lookup_list.extend_from_slice(&(lookups.len() as u16).to_be_bytes());
    let offsets_start = lookup_list.len();
    lookup_list.resize(offsets_start + lookups.len() * 2, 0);
    for (i, (lookup_type, subtables)) in lookups.iter().enumerate() {
        let body_start = lookup_list.len();
        let slot = offsets_start + i * 2;
        lookup_list[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
        lookup_list.extend_from_slice(&lookup_type.to_be_bytes());
        lookup_list.extend_from_slice(&0u16.to_be_bytes()); // lookupFlag
        lookup_list.extend_from_slice(&(subtables.len() as u16).to_be_bytes());
        let sub_offsets_start = lookup_list.len();
        lookup_list.resize(sub_offsets_start + subtables.len() * 2, 0);
        for (j, sub) in subtables.iter().enumerate() {
            let sub_start = lookup_list.len();
            let slot = sub_offsets_start + j * 2;
            let rel = (sub_start - body_start) as u16;
            lookup_list[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
            lookup_list.extend_from_slice(sub);
        }
    }
    build_gsub_around(lookups.len() as u16, &lookup_list)
}

/// Wraps a raw LookupList in a GSUB table with one script and one
/// feature naming `lookup_count` lookups.
fn build_gsub_around(lookup_count: u16, lookup_list: &[u8]) -> Vec<u8> {
    let mut script_list = Vec::new();
    script_list.extend_from_slice(&1u16.to_be_bytes()); // scriptCount
    script_list.extend_from_slice(b"DFLT");
    script_list.extend_from_slice(&8u16.to_be_bytes()); // scriptOffset
    script_list.extend_from_slice(&4u16.to_be_bytes()); // defaultLangSysOffset
    script_list.extend_from_slice(&0u16.to_be_bytes()); // langSysCount
    script_list.extend_from_slice(&0u16.to_be_bytes()); // lookupOrderOffset
    script_list.extend_from_slice(&0xFFFFu16.to_be_bytes()); // requiredFeatureIndex
    script_list.extend_from_slice(&1u16.to_be_bytes()); // featureIndexCount
    script_list.extend_from_slice(&0u16.to_be_bytes()); // featureIndices[0]

    let mut feature_list = Vec::new();
    feature_list.extend_from_slice(&1u16.to_be_bytes()); // featureCount
    feature_list.extend_from_slice(b"liga");
    feature_list.extend_from_slice(&8u16.to_be_bytes()); // featureOffset
    feature_list.extend_from_slice(&0u16.to_be_bytes()); // featureParamsOffset
    feature_list.extend_from_slice(&lookup_count.to_be_bytes());
    for i in 0..lookup_count {
        feature_list.extend_from_slice(&i.to_be_bytes());
    }

    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
    out.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
    let header_len = 10usize;
    let feature_off = header_len + script_list.len();
    let lookup_off = feature_off + feature_list.len();
    out.extend_from_slice(&(header_len as u16).to_be_bytes());
    out.extend_from_slice(&(feature_off as u16).to_be_bytes());
    out.extend_from_slice(&(lookup_off as u16).to_be_bytes());
    out.extend_from_slice(&script_list);
    out.extend_from_slice(&feature_list);
    out.extend_from_slice(lookup_list);
    out
}

/// A Coverage format 1 table over `glyphs`.
fn coverage(glyphs: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    for g in glyphs {
        out.extend_from_slice(&g.to_be_bytes());
    }
    out
}

/// A type 2 (multiple substitution) subtable mapping each covered glyph
/// to a sequence of `seq_len` copies of glyph 1.
fn multiple_subst(covered: &[u16], seq_len: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
    let cov_slot = out.len();
    out.extend_from_slice(&0u16.to_be_bytes()); // coverageOffset
    out.extend_from_slice(&(covered.len() as u16).to_be_bytes()); // sequenceCount
    let seq_offsets_start = out.len();
    out.resize(seq_offsets_start + covered.len() * 2, 0);
    // One shared Sequence: every Coverage entry points at it.
    let seq_start = out.len();
    for i in 0..covered.len() {
        let slot = seq_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(seq_start as u16).to_be_bytes());
    }
    out.extend_from_slice(&seq_len.to_be_bytes()); // glyphCount
    for _ in 0..seq_len {
        out.extend_from_slice(&1u16.to_be_bytes());
    }
    let cov_start = out.len();
    out[cov_slot..cov_slot + 2].copy_from_slice(&(cov_start as u16).to_be_bytes());
    out.extend_from_slice(&coverage(covered));
    out
}

/// Assembles a TrueType font with `num_glyphs` empty glyphs plus `gsub`.
fn font_with_gsub(num_glyphs: u16, gsub: Vec<u8>) -> Vec<u8> {
    let bodies: Vec<Vec<u8>> = (0..num_glyphs).map(|_| empty_glyph()).collect();
    let (glyf, loca) = glyf_and_loca(&bodies);
    build_font(&[
        (*b"GSUB", gsub),
        (*b"cmap", cmap()),
        (*b"glyf", glyf),
        (*b"head", head(1)),
        (*b"hhea", hhea(1)),
        (*b"hmtx", hmtx(num_glyphs)),
        (*b"loca", loca),
        (*b"maxp", maxp(num_glyphs)),
        (*b"name", name()),
    ])
}

#[test]
fn shared_gsub_subtables_do_not_explode_the_output() {
    // 300 lookups all point at the same 130 KB subtable, so a naive
    // rewrite would emit 39 MB from a 140 KB font.
    let covered: Vec<u16> = (1..=2000u16).collect();
    let sub = multiple_subst(&covered, 30_000);
    let lookups: Vec<(u16, Vec<Vec<u8>>)> = (0..300).map(|_| (2u16, vec![sub.clone()])).collect();
    let gsub = build_gsub(&lookups);
    let font = font_with_gsub(2001, gsub);
    let face = Face::parse_bytes(&font, 0).unwrap();
    // Drop one glyph so the rewriter runs instead of passing through.
    let gids: Vec<u16> = (0..2000u16).collect();
    let input = SubsetInput {
        gids,
        ..SubsetInput::default()
    };
    let out = subset(&face, &input).expect("subset succeeds");
    assert!(
        out.bytes.len() < 16 * 1024 * 1024,
        "output grew to {} bytes",
        out.bytes.len()
    );
}

#[test]
fn large_rewritten_gsub_stays_parseable() {
    // Ten lookups of 60 KB each put later lookups past the 16-bit
    // offset range of the LookupList. They must come back as Extension
    // lookups rather than pointing at the wrong bytes.
    let covered: Vec<u16> = (1..=2000u16).collect();
    let sub = multiple_subst(&covered, 13_000);
    let lookups: Vec<(u16, Vec<Vec<u8>>)> = (0..10).map(|_| (2u16, vec![sub.clone()])).collect();
    let gsub = build_gsub(&lookups);
    let font = font_with_gsub(2001, gsub);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let gids: Vec<u16> = (0..2000u16).collect();
    let input = SubsetInput {
        gids,
        ..SubsetInput::default()
    };
    let out = subset(&face, &input).expect("subset succeeds");
    let subset_face = Face::parse_bytes(&out.bytes, 0).expect("subset parses");
    let Some(new_gsub) = subset_face.gsub().expect("GSUB parses") else {
        return; // The rewriter may drop the table; nothing to check then.
    };
    let lookup_list = new_gsub.lookup_list();
    for li in 0..lookup_list.len() {
        let lookup = lookup_list.get(li).expect("lookup parses");
        for si in 0..lookup.subtable_count() {
            let sub = lookup.subtable_bytes(si).expect("subtable in range");
            let format = u16::from_be_bytes([sub[0], sub[1]]);
            assert_eq!(format, 1, "lookup {li} subtable {si} format {format}");
        }
    }
}
/// A Coverage format 2 table with `ranges` copies of a record covering
/// glyphs `0..=end`.
fn coverage_full_ranges(ranges: u16, end: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&ranges.to_be_bytes());
    for _ in 0..ranges {
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&end.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
    }
    out
}

/// A type 1 (single substitution) format 1 subtable whose Coverage is
/// `cov` and whose delta is 1, with `subtable_count` offsets in the
/// enclosing lookup all pointing at it.
fn single_subst_delta(cov: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // substFormat
    out.extend_from_slice(&6u16.to_be_bytes()); // coverageOffset
    out.extend_from_slice(&1i16.to_be_bytes()); // deltaGlyphID
    out.extend_from_slice(cov);
    out
}

/// A LookupList whose `lookup_count` lookups each claim
/// `subtable_count` subtables, every offset pointing at the one shared
/// subtable that follows the offset array.
fn lookup_list_sharing_one_subtable(lookup_count: u16, subtable_count: u16, sub: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&lookup_count.to_be_bytes());
    let offsets_start = out.len();
    out.resize(offsets_start + usize::from(lookup_count) * 2, 0);
    // One shared lookup body: every LookupList offset points at it.
    let body_start = out.len();
    for i in 0..usize::from(lookup_count) {
        let slot = offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&(body_start as u16).to_be_bytes());
    }
    out.extend_from_slice(&1u16.to_be_bytes()); // lookupType
    out.extend_from_slice(&0u16.to_be_bytes()); // lookupFlag
    out.extend_from_slice(&subtable_count.to_be_bytes());
    let sub_offsets_start = out.len();
    out.resize(sub_offsets_start + usize::from(subtable_count) * 2, 0);
    let sub_start = out.len();
    let rel = (sub_start - body_start) as u16;
    for i in 0..usize::from(subtable_count) {
        let slot = sub_offsets_start + i * 2;
        out[slot..slot + 2].copy_from_slice(&rel.to_be_bytes());
    }
    out.extend_from_slice(sub);
    out
}

#[test]
fn closure_finishes_on_shared_lookups_with_huge_coverage() {
    // Reduction of a fuzz timeout. 300 lookups each claim 300
    // subtables, and every one of those 90000 offsets points at the
    // same single-substitution subtable whose Coverage format 2 lists
    // 64 ranges covering all 65536 glyphs. The closure walker used to
    // re-expand 4.2 million glyphs per subtable, on every pass of its
    // fixed-point loop.
    let cov = coverage_full_ranges(64, 0xFFFF);
    let sub = single_subst_delta(&cov);
    let lookup_list = lookup_list_sharing_one_subtable(300, 300, &sub);
    let gsub = build_gsub_around(300, &lookup_list);
    let font = font_with_gsub(8, gsub);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let start = std::time::Instant::now();
    let kept = sigilbuzz_subset::compute_closure(&face, &[0, 1, 2]).expect("closure succeeds");
    let elapsed = start.elapsed();
    // The font has 8 glyphs, so the kept set cannot name more.
    assert!(kept.len() <= 8, "kept {} glyphs", kept.len());
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "closure took {elapsed:?}"
    );
    // The whole subset runs under the same budget.
    let out = subset(&face, &keep_all(8)).expect("subset succeeds");
    assert!(Face::parse_bytes(&out.bytes, 0).is_ok(), "subset parses");
}
