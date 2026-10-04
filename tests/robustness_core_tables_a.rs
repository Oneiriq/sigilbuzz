//! Regression tests for hostile-input bugs in the `ankr` through
//! `gvar` table parsers. Every fixture is built by hand. Each test
//! either panicked, hung, or tried to allocate gigabytes before the
//! matching fix.

use sigilbuzz::tables::base::Base;
use sigilbuzz::tables::cbdt::Cbdt;
use sigilbuzz::tables::cblc::{CbdtLocation, Cblc};
use sigilbuzz::tables::cff::Cff;
use sigilbuzz::tables::cff2::Cff2;
use sigilbuzz::tables::gvar::Gvar;
use sigilbuzz::tables::head::IndexToLocFormat;
use sigilbuzz::tables::{Glyf, Loca, Outline};
use sigilbuzz::Error;

// ---------------------------------------------------------------------
// CFF and CFF2 INDEX
// ---------------------------------------------------------------------

#[test]
fn cff_index_with_zero_last_offset_is_malformed() {
    // Header, then a Name INDEX with count 2, offSize 1, and offsets
    // [1, 1, 0]. The zero final offset made the data-length math
    // underflow (a fuzzer find).
    let cff = [1, 0, 4, 1, 0, 2, 1, 1, 1, 0];
    let err = Cff::parse(&cff).unwrap_err();
    assert!(matches!(err, Error::Malformed { .. }), "{err:?}");
}

#[test]
fn cff2_index_count_past_data_fails_before_allocating() {
    // CFF2 header with an empty Top DICT. The Global Subr INDEX that
    // follows claims 0xFFFF_FFFF entries with 4-byte offsets, and the
    // parser used to reserve 16 GiB for the offset array before
    // reading any of it (a fuzzer find).
    let global = [2, 0, 5, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 4];
    let err = Cff2::parse(&global).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }), "{err:?}");

    // Same count on the CharStrings INDEX. The Top DICT is
    // `29 <i32 15> 17` and the Global Subr INDEX is empty.
    let mut charstrings = vec![2, 0, 5, 0, 6, 29, 0, 0, 0, 15, 17, 0, 0, 0, 0];
    charstrings.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 4]);
    let err = Cff2::parse(&charstrings).unwrap_err();
    assert!(matches!(err, Error::Truncated { .. }), "{err:?}");
}

/// Encodes a DICT integer operand in the 5-byte form.
fn dict_int(out: &mut Vec<u8>, v: usize) {
    out.push(29);
    out.extend_from_slice(&(v as i32).to_be_bytes());
}

/// Builds a CID-keyed CFF with `n_glyphs` empty glyphs, one empty Font
/// DICT, and an FDSelect format 3 with one range per entry of `firsts`,
/// all in FD 0, ended by `sentinel`.
fn cid_cff_with_fd_ranges(n_glyphs: usize, firsts: &[u16], sentinel: u16) -> Vec<u8> {
    let top_dict_len = 20;
    let prefix = 4 + 6 + (2 + 1 + 2 + top_dict_len) + 2 + 2;
    let cs_off = prefix;
    let fda_off = cs_off + 3 + (n_glyphs + 1);
    let fds_off = fda_off + 5;

    let mut cff = vec![1, 0, 4, 4];
    cff.extend_from_slice(&[0, 1, 1, 1, 2, b'a']); // Name INDEX
    cff.extend_from_slice(&[0, 1, 1, 1, 1 + top_dict_len as u8]); // Top DICT INDEX
    dict_int(&mut cff, cs_off);
    cff.push(17); // CharStrings
    dict_int(&mut cff, fda_off);
    cff.extend_from_slice(&[12, 36]); // FDArray
    dict_int(&mut cff, fds_off);
    cff.extend_from_slice(&[12, 37]); // FDSelect
    cff.extend_from_slice(&[0, 0]); // String INDEX
    cff.extend_from_slice(&[0, 0]); // Global Subr INDEX
    assert_eq!(cff.len(), cs_off);

    // CharStrings INDEX: `n_glyphs` empty entries.
    cff.extend_from_slice(&(n_glyphs as u16).to_be_bytes());
    cff.push(1);
    cff.extend(core::iter::repeat(1).take(n_glyphs + 1));
    assert_eq!(cff.len(), fda_off);

    // FDArray INDEX: one empty Font DICT.
    cff.extend_from_slice(&[0, 1, 1, 1, 1]);
    assert_eq!(cff.len(), fds_off);

    // FDSelect format 3.
    cff.push(3);
    cff.extend_from_slice(&(firsts.len() as u16).to_be_bytes());
    for first in firsts {
        cff.extend_from_slice(&first.to_be_bytes());
        cff.push(0);
    }
    cff.extend_from_slice(&sentinel.to_be_bytes());
    cff
}

#[test]
fn cff_fd_select_with_unsorted_ranges_is_not_expanded() {
    // A CID-keyed CFF with 65,535 empty glyphs and an FDSelect format 3
    // whose 65,535 ranges alternate between first glyph 0 and first
    // glyph 65,535. Half of them span every glyph, so filling range
    // by range took about two billion writes. FDSelect is no longer
    // expanded, and range 0 already spans every glyph, so the lookups
    // for the first and the last glyph both stop at range 0.
    const N: usize = 65_535;
    let firsts: Vec<u16> = (0..N)
        .map(|i| if i % 2 == 0 { 0 } else { N as u16 })
        .collect();
    let cff = cid_cff_with_fd_ranges(N, &firsts, N as u16);
    let parsed = Cff::parse(&cff).unwrap();
    assert_eq!(usize::from(parsed.num_glyphs()), N);
    for gid in [0, N as u16 - 1] {
        let mut out = Outline::new();
        assert!(parsed.outline(gid, &mut out).unwrap());
    }
}

#[test]
fn cff_fd_select_unsorted_lookup_walks_to_the_last_range() {
    // First glyphs alternate between 1 and 0, so every range but one
    // is empty or covers only glyph 0, and only the last range, which
    // runs to the sentinel, covers the last glyph. Opening the table
    // works that out in one pass over the 65,535 ranges.
    const N: usize = 65_535;
    let firsts: Vec<u16> = (0..N).map(|i| if i % 2 == 0 { 1 } else { 0 }).collect();
    let cff = cid_cff_with_fd_ranges(N, &firsts, N as u16);
    let parsed = Cff::parse(&cff).unwrap();
    let mut out = Outline::new();
    assert!(parsed.outline(N as u16 - 1, &mut out).unwrap());
}

#[test]
fn cff_fd_select_with_one_glyph_ranges_draws_every_glyph_without_quadratic_cost() {
    // 65,535 sorted ranges of one glyph each. A lookup used to scan the
    // ranges from the front, so drawing every glyph from one held
    // `Cff` read about two billion range records. Sorted ranges are
    // now binary-searched.
    const N: usize = 65_535;
    let firsts: Vec<u16> = (0..N as u16).collect();
    let cff = cid_cff_with_fd_ranges(N, &firsts, N as u16);
    let parsed = Cff::parse(&cff).unwrap();
    for gid in 0..N as u16 {
        assert!(parsed.outline(gid, &mut Outline::new()).unwrap());
    }
}

#[test]
fn cff_fd_select_with_unsorted_one_glyph_ranges_draws_every_glyph_without_quadratic_cost() {
    // 65,535 one-glyph ranges in order except that ranges 1 and 2 trade
    // places, so the binary search over the records does not apply. A
    // lookup used to scan from the front, through every range below
    // the glyph, so drawing every glyph from one held `Cff` read about
    // two billion range records. Opening the table now works out the
    // span each range keeps, once, and lookups binary-search those.
    const N: usize = 65_535;
    let mut firsts: Vec<u16> = (0..N as u16).collect();
    firsts.swap(1, 2);
    let cff = cid_cff_with_fd_ranges(N, &firsts, N as u16);
    let parsed = Cff::parse(&cff).unwrap();
    for gid in 0..N as u16 {
        assert!(parsed.outline(gid, &mut Outline::new()).unwrap());
    }
}

// ---------------------------------------------------------------------
// CBLC / CBDT
// ---------------------------------------------------------------------

/// Builds a CBLC table with one strike and one IndexSubTableArray entry
/// covering `first..=last`. The index sub-table starts with
/// `index_format`, image format 17, and `image_data_offset`, followed
/// by `body`.
fn cblc_with_subtable(
    first: u16,
    last: u16,
    index_format: u16,
    image_data_offset: u32,
    body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&3u16.to_be_bytes()); // majorVersion
    out.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
    out.extend_from_slice(&1u32.to_be_bytes()); // numSizes
                                                // BitmapSize record (48 bytes).
    out.extend_from_slice(&56u32.to_be_bytes()); // indexSubTableArrayOffset
    out.extend_from_slice(&0u32.to_be_bytes()); // indexTablesSize
    out.extend_from_slice(&1u32.to_be_bytes()); // numberOfIndexSubTables
    out.extend_from_slice(&0u32.to_be_bytes()); // colorRef
    out.extend_from_slice(&[0; 24]); // hori + vert line metrics
    out.extend_from_slice(&first.to_be_bytes()); // startGlyphIndex
    out.extend_from_slice(&last.to_be_bytes()); // endGlyphIndex
    out.extend_from_slice(&[32, 32, 32, 1]); // ppemX, ppemY, bitDepth, flags
    assert_eq!(out.len(), 56);
    // IndexSubTableArray entry.
    out.extend_from_slice(&first.to_be_bytes());
    out.extend_from_slice(&last.to_be_bytes());
    out.extend_from_slice(&8u32.to_be_bytes()); // additionalOffsetToIndexSubTable
                                                // IndexSubTable header and body.
    out.extend_from_slice(&index_format.to_be_bytes());
    out.extend_from_slice(&17u16.to_be_bytes());
    out.extend_from_slice(&image_data_offset.to_be_bytes());
    out.extend_from_slice(body);
    out
}

fn locate(table: &[u8], glyph_id: u16) -> Result<Option<CbdtLocation>, Error> {
    let cblc = Cblc::parse(table).unwrap();
    let size = cblc.size(0).unwrap();
    cblc.locate(&size, glyph_id)
}

#[test]
fn cblc_full_u16_glyph_range_does_not_overflow() {
    // Glyphs 0..=0xFFFF are 65,536 glyphs. Counting them in a u16
    // overflowed before any lookup happened.
    let mut body = 10u32.to_be_bytes().to_vec(); // imageSize
    body.extend_from_slice(&[0; 8]); // BigGlyphMetrics
    let table = cblc_with_subtable(0, 0xFFFF, 2, 0, &body);
    let loc = locate(&table, 5).unwrap().unwrap();
    assert_eq!(loc.offset, 50);
    assert_eq!(loc.length, 10);
}

#[test]
fn cblc_image_offset_overflow_is_malformed() {
    let base = 0xFFFF_FFF0;

    // Format 1: u32 offsets [0x100, 0x200].
    let mut body = 0x100u32.to_be_bytes().to_vec();
    body.extend_from_slice(&0x200u32.to_be_bytes());
    let table = cblc_with_subtable(0, 0, 1, base, &body);
    assert!(matches!(locate(&table, 0), Err(Error::Malformed { .. })));

    // Format 3: u16 offsets [0x100, 0x200].
    let mut body = 0x100u16.to_be_bytes().to_vec();
    body.extend_from_slice(&0x200u16.to_be_bytes());
    let table = cblc_with_subtable(0, 0, 3, base, &body);
    assert!(matches!(locate(&table, 0), Err(Error::Malformed { .. })));

    // Format 4: numGlyphs 1, pairs (0, 0x100) and the (0, 0x200) end.
    let mut body = 1u32.to_be_bytes().to_vec();
    body.extend_from_slice(&[0, 0, 0x01, 0x00, 0, 0, 0x02, 0x00]);
    let table = cblc_with_subtable(0, 0, 4, base, &body);
    assert!(matches!(locate(&table, 0), Err(Error::Malformed { .. })));
}

#[test]
fn cblc_constant_metric_offset_overflow_is_malformed() {
    let image_size = 0x8000_0000u32;

    // Format 2: glyph 2 sits at 2 * imageSize, past u32::MAX.
    let mut body = image_size.to_be_bytes().to_vec();
    body.extend_from_slice(&[0; 8]);
    let table = cblc_with_subtable(0, 10, 2, 0, &body);
    assert!(matches!(locate(&table, 2), Err(Error::Malformed { .. })));

    // Format 5: glyph 9 is the third entry, so the same product.
    let mut body = image_size.to_be_bytes().to_vec();
    body.extend_from_slice(&[0; 8]);
    body.extend_from_slice(&3u32.to_be_bytes()); // numGlyphs
    for gid in [7u16, 8, 9] {
        body.extend_from_slice(&gid.to_be_bytes());
    }
    let table = cblc_with_subtable(7, 9, 5, 0, &body);
    assert!(matches!(locate(&table, 9), Err(Error::Malformed { .. })));
}

#[test]
fn cbdt_payload_length_near_u32_max_is_truncated() {
    // Format 19 record whose dataLen is u32::MAX. The end offset is
    // now checked, which matters on 32-bit targets where the sum
    // used to wrap.
    let table = [0, 3, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF];
    let cbdt = Cbdt::parse(&table).unwrap();
    let loc = CbdtLocation {
        offset: 4,
        length: 4,
        image_format: 19,
        metrics: None,
    };
    assert!(matches!(
        cbdt.glyph_bitmap(&loc),
        Err(Error::Truncated { .. })
    ));
}

// ---------------------------------------------------------------------
// BASE
// ---------------------------------------------------------------------

#[test]
fn base_offsets_past_u16_range_are_not_truncated() {
    // The horizontal axis sits at 0xFFF0 and its tag list 0x20 bytes
    // later, at 0x1_0010. Storing that sum in a u16 read the tag list
    // at 0x0010 instead, where this fixture keeps a decoy.
    let mut data = vec![0u8; 0x1_0010];
    data[0..2].copy_from_slice(&1u16.to_be_bytes()); // majorVersion
    data[4..6].copy_from_slice(&0xFFF0u16.to_be_bytes()); // horizAxisOffset
    data[0x10..0x12].copy_from_slice(&1u16.to_be_bytes()); // decoy count
    data[0x12..0x16].copy_from_slice(b"xxxx"); // decoy tag
    data[0xFFF0..0xFFF2].copy_from_slice(&0x20u16.to_be_bytes()); // baseTagListOffset
    data.extend_from_slice(&2u16.to_be_bytes());
    data.extend_from_slice(b"ideo");
    data.extend_from_slice(b"romn");

    let base = Base::parse(&data).unwrap();
    let axis = base.horizontal_axis().unwrap();
    assert_eq!(axis.baseline_tags(), vec![*b"ideo", *b"romn"]);
}

// ---------------------------------------------------------------------
// glyf
// ---------------------------------------------------------------------

/// A composite glyph with two components, both `child` at (0, 0).
fn doubling_composite(child: u16) -> Vec<u8> {
    let mut g = Vec::new();
    g.extend_from_slice(&(-1i16).to_be_bytes());
    g.extend_from_slice(&[0; 8]); // bbox
                                  // ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES | MORE_COMPONENTS.
    g.extend_from_slice(&0x0023u16.to_be_bytes());
    g.extend_from_slice(&child.to_be_bytes());
    g.extend_from_slice(&[0; 4]);
    // ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES.
    g.extend_from_slice(&0x0003u16.to_be_bytes());
    g.extend_from_slice(&child.to_be_bytes());
    g.extend_from_slice(&[0; 4]);
    g
}

/// A simple glyph with one contour of `points` on-curve points, all
/// at (0, 0). Flags use REPEAT, and the SAME bits mean no coordinate
/// bytes follow.
fn simple_glyph(points: u16) -> Vec<u8> {
    let mut g = Vec::new();
    g.extend_from_slice(&1i16.to_be_bytes());
    g.extend_from_slice(&[0; 8]); // bbox
    g.extend_from_slice(&(points - 1).to_be_bytes()); // endPtsOfContours
    g.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
    let mut left = usize::from(points);
    while left > 0 {
        let run = left.min(256);
        // ON_CURVE | REPEAT | X_SAME | Y_SAME, then the extra count.
        g.push(0x39);
        g.push((run - 1) as u8);
        left -= run;
    }
    g
}

/// Glyphs `0..depth` each double the next one, and glyph `depth` is a
/// simple glyph with `leaf_points` points. Returns (glyf, loca).
fn doubling_chain(depth: u16, leaf_points: u16) -> (Vec<u8>, Vec<u8>) {
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for gid in 0..depth {
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
        glyf.extend_from_slice(&doubling_composite(gid + 1));
    }
    loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
    glyf.extend_from_slice(&simple_glyph(leaf_points));
    loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
    (glyf, loca)
}

#[test]
fn glyf_composite_fan_out_stops_at_glyph_limit() {
    // 40 levels of doubling reach 2^40 leaf visits while the depth
    // stays under the cap of 64. The walk used to run for hours.
    let (glyf_bytes, loca_bytes) = doubling_chain(40, 1);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Long, 41).unwrap();
    let glyf = Glyf::new(&glyf_bytes);
    let mut out = Outline::new();
    let err = glyf.outline(&loca, 0, None, None, &mut out).unwrap_err();
    assert!(matches!(err, Error::Malformed { .. }), "{err:?}");
}

#[test]
fn glyf_composite_fan_out_stops_at_point_limit() {
    // 20 levels of doubling over a 65,535-point leaf would lay down
    // about 68 billion points. The point limit stops it after a few
    // leaves.
    let (glyf_bytes, loca_bytes) = doubling_chain(20, u16::MAX);
    let loca = Loca::parse(&loca_bytes, IndexToLocFormat::Long, 21).unwrap();
    let glyf = Glyf::new(&glyf_bytes);
    let mut out = Outline::new();
    let err = glyf.outline(&loca, 0, None, None, &mut out).unwrap_err();
    assert!(matches!(err, Error::Malformed { .. }), "{err:?}");

    // A single leaf of the same size is still well within the limit.
    let mut single = Outline::new();
    assert!(glyf.outline(&loca, 20, None, None, &mut single).unwrap());
}

// ---------------------------------------------------------------------
// gvar
// ---------------------------------------------------------------------

#[test]
fn gvar_all_points_deltas_accumulate_in_linear_time() {
    // One glyph with two all-points tuples over 65,535 points. Summing
    // each point with a linear search took about four billion
    // comparisons.
    const POINTS: u16 = u16::MAX;
    // 64 zero deltas per control byte, for x and then for y.
    let runs = usize::from(POINTS).div_ceil(64);
    let tuple_data_len = 2 * runs;

    let mut glyph_data = Vec::new();
    glyph_data.extend_from_slice(&2u16.to_be_bytes()); // tupleVariationCount
    glyph_data.extend_from_slice(&16u16.to_be_bytes()); // dataOffset
    for _ in 0..2 {
        glyph_data.extend_from_slice(&(tuple_data_len as u16).to_be_bytes());
        glyph_data.extend_from_slice(&0x8000u16.to_be_bytes()); // embedded peak
        glyph_data.extend_from_slice(&0x4000u16.to_be_bytes()); // peak 1.0
    }
    for _ in 0..2 {
        glyph_data.extend(core::iter::repeat(0xBF).take(tuple_data_len));
    }

    let mut gvar = Vec::new();
    gvar.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
    gvar.extend_from_slice(&0u16.to_be_bytes()); // minorVersion
    gvar.extend_from_slice(&1u16.to_be_bytes()); // axisCount
    gvar.extend_from_slice(&0u16.to_be_bytes()); // sharedTupleCount
    gvar.extend_from_slice(&0u32.to_be_bytes()); // sharedTuplesOffset
    gvar.extend_from_slice(&1u16.to_be_bytes()); // glyphCount
    gvar.extend_from_slice(&1u16.to_be_bytes()); // flags: long offsets
    gvar.extend_from_slice(&28u32.to_be_bytes()); // glyphVariationDataArrayOffset
    gvar.extend_from_slice(&0u32.to_be_bytes());
    gvar.extend_from_slice(&(glyph_data.len() as u32).to_be_bytes());
    assert_eq!(gvar.len(), 28);
    gvar.extend_from_slice(&glyph_data);

    let parsed = Gvar::parse(&gvar).unwrap();
    let deltas = parsed.glyph_deltas(0, &[1.0], POINTS);
    assert_eq!(deltas.len(), usize::from(POINTS));
    assert_eq!(deltas[1234].point, 1234);
}
