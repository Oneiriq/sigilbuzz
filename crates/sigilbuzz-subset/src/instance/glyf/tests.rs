//! Unit tests for the glyf bake: composite offsets, phantom metrics,
//! and the simple glyph codec.

use super::*;
use alloc::vec;

/// A composite glyph body: a zero header, then one record per
/// `(flags, glyph, arg1, arg2)`, the arguments as words when the
/// flags say so and as bytes otherwise, with `MORE_COMPONENTS` set on
/// every record but the last.
fn composite(records: &[(u16, u16, i16, i16)]) -> Vec<u8> {
    let mut body = vec![0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0];
    for (i, &(flags, glyph, a, b)) in records.iter().enumerate() {
        let more = if i + 1 < records.len() {
            COMP_MORE_COMPONENTS
        } else {
            0
        };
        body.extend_from_slice(&(flags | more).to_be_bytes());
        body.extend_from_slice(&glyph.to_be_bytes());
        if flags & COMP_ARG_1_AND_2_ARE_WORDS != 0 {
            body.extend_from_slice(&a.to_be_bytes());
            body.extend_from_slice(&b.to_be_bytes());
        } else {
            body.push(a as u8);
            body.push(b as u8);
        }
    }
    body
}

const XY: u16 = COMP_ARGS_ARE_XY_VALUES;

#[test]
fn component_offsets_move_by_their_deltas_and_round_halves_up() {
    let body = composite(&[(XY, 1, 10, -20), (XY, 2, -5, 7)]);
    let records = read_component_records(&body).unwrap();
    assert_eq!(
        records
            .iter()
            .map(CompRecord::gvar_point)
            .collect::<Vec<_>>(),
        vec![(10, -20), (-5, 7)]
    );
    let out = rewrite_components(&body, &records, &[(2.5, -0.5), (-2.5, 0.4)]);
    let moved = read_component_records(&out).unwrap();
    // 12.5 rounds to 13, -20.5 to -20, -7.5 to -7, 7.4 to 7.
    assert_eq!(
        moved.iter().map(CompRecord::gvar_point).collect::<Vec<_>>(),
        vec![(13, -20), (-7, 7)]
    );
    assert_eq!(out.len(), body.len(), "byte arguments still fit");
}

#[test]
fn byte_arguments_widen_to_words_when_the_offset_outgrows_them() {
    let body = composite(&[(XY, 1, 120, 0), (XY, 2, 3, 4)]);
    let records = read_component_records(&body).unwrap();
    let out = rewrite_components(&body, &records, &[(20.0, -200.0), (0.0, 0.0)]);
    let moved = read_component_records(&out).unwrap();
    assert_eq!(moved[0].gvar_point(), (140, -200));
    assert_ne!(moved[0].flags & COMP_ARG_1_AND_2_ARE_WORDS, 0);
    assert_eq!(moved[0].flags & COMP_MORE_COMPONENTS, COMP_MORE_COMPONENTS);
    assert_eq!(moved[1].gvar_point(), (3, 4));
    assert_eq!(out.len(), body.len() + 2);
}

#[test]
fn anchored_components_and_trailing_instructions_are_copied() {
    // An anchored component (points 3 and 4), then one with a scale
    // and words, then the instructions of the last record.
    let mut body = composite(&[(0, 1, 3, 4)]);
    body[10..12].copy_from_slice(&COMP_MORE_COMPONENTS.to_be_bytes());
    let flags = XY | COMP_ARG_1_AND_2_ARE_WORDS | COMP_WE_HAVE_A_SCALE | 0x0100;
    body.extend_from_slice(&flags.to_be_bytes());
    body.extend_from_slice(&2u16.to_be_bytes());
    body.extend_from_slice(&300i16.to_be_bytes());
    body.extend_from_slice(&(-300i16).to_be_bytes());
    body.extend_from_slice(&0x2000u16.to_be_bytes()); // scale 0.5
    body.extend_from_slice(&[0, 2, 0xB0, 0x01]); // two instruction bytes
    let records = read_component_records(&body).unwrap();
    assert_eq!(records[0].gvar_point(), (0, 0), "anchored: no offset");
    let out = rewrite_components(&body, &records, &[(50.0, 50.0), (1.0, -1.0)]);
    let moved = read_component_records(&out).unwrap();
    assert_eq!(&out[10..16], &body[10..16], "anchored record unchanged");
    assert_eq!(moved[1].gvar_point(), (301, -301));
    assert_eq!(&out[out.len() - 6..], &[0x20, 0x00, 0, 2, 0xB0, 0x01]);
}

#[test]
fn metrics_come_from_the_phantom_points_and_the_new_bounds() {
    // Phantom points: left origin -12.4, advance origin 600.6, top 880,
    // bottom -120.5.
    let pp = [(-12.4, 0.0), (600.6, 0.0), (0.0, 880.0), (0.0, -120.5)];
    let m = metrics_from(&pp, Some([30, -10, 500, 700]));
    assert_eq!(m.advance, 613); // 613.0
    assert_eq!(m.lsb, 42); // 30 - (-12.4) = 42.4
    assert_eq!(m.v_advance, 1001); // 1000.5 rounds up
    assert_eq!(m.tsb, 180);
    // An empty glyph measures its bearings from zero.
    let m = metrics_from(&pp, None);
    assert_eq!((m.lsb, m.tsb, m.bounds), (12, 880, None));
    // A negative advance clamps to zero.
    let crossed = [(10.0, 0.0), (4.0, 0.0), (0.0, 0.0), (0.0, 5.0)];
    let m = metrics_from(&crossed, None);
    assert_eq!((m.advance, m.v_advance), (0, 0));
}

#[test]
fn simple_glyphs_keep_on_curve_and_overlap_bits_and_hints() {
    // Two points, the first flagged on curve and OVERLAP_SIMPLE, with a
    // one-byte instruction stream.
    let mut body = Vec::new();
    body.extend_from_slice(&1i16.to_be_bytes());
    body.extend_from_slice(&[0; 8]);
    body.extend_from_slice(&1u16.to_be_bytes()); // endPtsOfContours
    body.extend_from_slice(&1u16.to_be_bytes()); // instructionLength
    body.push(0xB0);
    body.push(0x41 | 0x02 | 0x04 | 0x10 | 0x20); // on curve, overlap, +x +y bytes
    body.push(0x00); // off curve, x and y words
                     // The x stream (+10, then +300), then the y stream (+20, then -40).
    body.push(10);
    body.extend_from_slice(&300i16.to_be_bytes());
    body.push(20);
    body.extend_from_slice(&(-40i16).to_be_bytes());
    let glyph = SimpleGlyph::decode(&body).unwrap();
    assert_eq!(glyph.points(), vec![(10, 20), (310, -20)]);
    let (baked, bounds) = encode_baked_simple(&glyph, &[(0.5, -0.5), (-0.5, 0.25)]);
    assert_eq!(bounds, Some([11, -20, 310, 20]));
    let again = SimpleGlyph::decode(&baked).unwrap();
    // 10.5 and 309.5 round up, 19.5 too, and -19.75 to -20.
    assert_eq!(again.points(), vec![(11, 20), (310, -20)]);
    assert_eq!(again.flags[0] & 0xC1, 0x41);
    assert_eq!(again.flags[1] & 0xC1, 0x00);
    // The instructions ride through, as HarfBuzz keeps them.
    assert_eq!(&baked[12..15], &[0, 1, 0xB0]);
    assert_eq!(again.instructions, vec![0xB0]);
}

#[test]
fn round_half_up_rounds_toward_positive_infinity_on_ties() {
    use crate::util::round_half_up;
    assert_eq!(round_half_up(2.5), 3);
    assert_eq!(round_half_up(-2.5), -2);
    assert_eq!(round_half_up(-2.6), -3);
    assert_eq!(round_half_up(-0.4), 0);
    assert_eq!(round_half_up(f32::NAN), 0);
    assert_eq!(round_half_up(1.0e12), i32::MAX);
    assert_eq!(round_half_up(-1.0e12), i32::MIN);
}

// ---------------------------------------------------------------------------
// Whole bakes of hand-built fonts.
// ---------------------------------------------------------------------------

fn be16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// One component record: `flags` (`MORE_COMPONENTS` added unless
/// `last`), `glyph`, word arguments, then `transform` as F2DOT14s.
fn record(flags: u16, glyph: u16, a: i16, b: i16, transform: &[f32], last: bool) -> Vec<u8> {
    let mut r = Vec::new();
    let more = if last { 0 } else { COMP_MORE_COMPONENTS };
    be16(&mut r, flags | COMP_ARG_1_AND_2_ARE_WORDS | more);
    be16(&mut r, glyph);
    r.extend_from_slice(&a.to_be_bytes());
    r.extend_from_slice(&b.to_be_bytes());
    for &v in transform {
        r.extend_from_slice(&((v * 16384.0) as i16).to_be_bytes());
    }
    r
}

/// A composite glyph body of `records`.
fn composite_of(records: &[Vec<u8>]) -> Vec<u8> {
    let mut body = vec![0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0];
    for r in records {
        body.extend_from_slice(r);
    }
    body
}

/// A one-axis (`wght`) TrueType font of `glyphs`, glyph 1 a triangle
/// (0, 0), (10, 0), (10, 10) that wght 900 moves 3 right and its top
/// 3 up.
fn one_axis_font(glyphs: &[Vec<u8>]) -> Vec<u8> {
    one_axis_font_with(glyphs, 1)
}

/// [`one_axis_font`] with `tuples` copies of glyph 1's tuple.
fn one_axis_font_with(glyphs: &[Vec<u8>], tuples: u16) -> Vec<u8> {
    let n = glyphs.len() as u16;
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for g in glyphs {
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
        glyf.extend_from_slice(g);
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
    }
    loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
    // gvar: one tuple for glyph 1, every point, x then y as words.
    let mut tuple = vec![0x40 | 6];
    for v in [3i16, 3, 3, 0, 0, 0, 0] {
        tuple.extend_from_slice(&v.to_be_bytes());
    }
    tuple.push(0x40 | 6);
    for v in [0i16, 0, 3, 0, 0, 0, 0] {
        tuple.extend_from_slice(&v.to_be_bytes());
    }
    let mut data = Vec::new();
    be16(&mut data, tuples); // no shared points
    be16(&mut data, 4 + 6 * tuples); // data offset
    for _ in 0..tuples {
        be16(&mut data, tuple.len() as u16);
        be16(&mut data, 0x8000); // embedded peak
        be16(&mut data, 0x4000); // wght 1
    }
    for _ in 0..tuples {
        data.extend_from_slice(&tuple);
    }
    let mut gvar = Vec::new();
    for v in [1u16, 0, 1, 0] {
        be16(&mut gvar, v);
    }
    let offsets_end = 20 + 4 * (u32::from(n) + 1);
    gvar.extend_from_slice(&offsets_end.to_be_bytes());
    be16(&mut gvar, n);
    be16(&mut gvar, 1); // long offsets
    gvar.extend_from_slice(&offsets_end.to_be_bytes());
    for gid in 0..=u32::from(n) {
        let at = if gid >= 2 { data.len() as u32 } else { 0 };
        gvar.extend_from_slice(&at.to_be_bytes());
    }
    gvar.extend_from_slice(&data);
    let mut fvar = Vec::new();
    for v in [1u16, 0, 16, 2, 1, 20, 0, 0] {
        be16(&mut fvar, v);
    }
    fvar.extend_from_slice(b"wght");
    for v in [100i32, 400, 900] {
        fvar.extend_from_slice(&(v << 16).to_be_bytes());
    }
    be16(&mut fvar, 0);
    be16(&mut fvar, 256);
    let mut head = vec![0u8; 54];
    head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    head[50..52].copy_from_slice(&1u16.to_be_bytes());
    let mut hhea = vec![0u8; 36];
    hhea[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    hhea[34..36].copy_from_slice(&n.to_be_bytes());
    let mut maxp = 0x0000_5000u32.to_be_bytes().to_vec();
    be16(&mut maxp, n);
    let mut hmtx = Vec::new();
    for _ in 0..n {
        be16(&mut hmtx, 500);
        be16(&mut hmtx, 0);
    }
    crate::sfnt::build(
        0x0001_0000,
        &[
            (tag::HEAD, head),
            (tag::HHEA, hhea),
            (tag::MAXP, maxp),
            (tag::HMTX, hmtx),
            (tag::LOCA, loca),
            (tag::GLYF, glyf),
            (tag::FVAR, fvar),
            (tag::GVAR, gvar),
        ],
    )
}

/// The triangle at glyph 1, with an `instructions` byte stream.
fn triangle(instructions: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [1u16, 0, 0, 10, 10, 2] {
        be16(&mut b, v);
    }
    be16(&mut b, instructions.len() as u16);
    b.extend_from_slice(instructions);
    b.extend_from_slice(&[0x01, 0x01, 0x01]); // on curve, word deltas
    for v in [0i16, 10, 0, 0, 0, 10] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b
}

/// The header box of glyph `gid` in a bake.
fn baked_box(bake: &GlyfLocaBake, gid: u16) -> [i16; 4] {
    let b = bake.body(gid);
    let at = |i: usize| i16::from_be_bytes([b[i], b[i + 1]]);
    [at(2), at(4), at(6), at(8)]
}

/// Bakes `font` at wght 900 and returns the bake and its warnings.
fn bake_at_900(font: &[u8]) -> (GlyfLocaBake, Vec<crate::SubsetWarning>) {
    let face = Face::parse_bytes(font, 0).unwrap();
    let n = face.maxp().unwrap().num_glyphs;
    let warnings = Warnings::default();
    let bake = bake_glyf_loca(&face, &[1.0], n, &warnings).unwrap();
    (bake, warnings.into_sorted())
}

#[test]
fn a_huge_component_tree_bakes_each_glyph_once() {
    // Glyph 2 + i draws the glyph before it twice, 5 units apart, for
    // 24 levels, then 50 glyphs each draw the last level twice: a draw
    // of one of those visits 2^25 triangles. Each extent is worked out
    // once, and nothing is drawn through the core walk.
    const LEVELS: u16 = 24;
    const TOPS: u16 = 50;
    let mut glyphs = vec![Vec::new(), triangle(&[])];
    for i in 0..LEVELS + TOPS {
        let child = 1 + i.min(LEVELS);
        glyphs.push(composite_of(&[
            record(XY, child, 0, 0, &[], false),
            record(XY, child, 5, 0, &[], true),
        ]));
    }
    let (bake, warnings) = bake_at_900(&one_axis_font(&glyphs));
    assert!(warnings.is_empty(), "{warnings:?}");
    let (computed, drawn, _) = bake.extent_work;
    // The triangle, every level, and every top glyph, once each.
    assert_eq!(computed, u64::from(1 + LEVELS + TOPS));
    assert_eq!(drawn, 0);
    // The moved triangle spans (3, 0) to (13, 13); each level adds 5.
    let top = 2 + LEVELS;
    assert_eq!(
        baked_box(&bake, top),
        [3, 0, 13 + 5 * (LEVELS as i16 + 1), 13]
    );
}

#[test]
fn skewed_and_matched_composites_draw_within_one_budget() {
    // Glyph 2 skews the triangle: drawn through the core walk, exactly.
    // Glyphs 3 to 26 build a tree of 2^24 triangles; glyph 27 skews it,
    // too costly to draw, so it takes the box around the skewed box;
    // glyph 28 places it by matching points, so it keeps its source box.
    let skew = [1.0, 0.0, 0.5, 1.0]; // x' = x + 0.5 y
    let mut glyphs = vec![
        Vec::new(),
        triangle(&[]),
        composite_of(&[record(XY | COMP_WE_HAVE_A_TWO_BY_TWO, 1, 0, 0, &skew, true)]),
    ];
    for i in 0..24u16 {
        let child = if i == 0 { 1 } else { 2 + i };
        glyphs.push(composite_of(&[
            record(XY, child, 0, 0, &[], false),
            record(XY, child, 5, 0, &[], true),
        ]));
    }
    let big = 26;
    glyphs.push(composite_of(&[record(
        XY | COMP_WE_HAVE_A_TWO_BY_TWO,
        big,
        0,
        0,
        &skew,
        true,
    )]));
    let mut matched = composite_of(&[
        record(XY, 1, 0, 0, &[], false),
        record(0, big, 0, 0, &[], true),
    ]);
    matched[2..10].copy_from_slice(&[0, 1, 0, 2, 0, 3, 0, 4]);
    glyphs.push(matched);
    let (bake, warnings) = bake_at_900(&one_axis_font(&glyphs));
    let (_, drawn, _) = bake.extent_work;
    assert_eq!(
        drawn, 1,
        "only the small skewed composite is drawn: {warnings:?}"
    );
    // The moved triangle (3, 0), (13, 0), (13, 13) skewed: x + y / 2.
    assert_eq!(baked_box(&bake, 2), [3, 0, 20, 13]);
    // The tree spans (3, 0) to (133, 13); its skewed box's corners.
    assert_eq!(baked_box(&bake, 27), [3, 0, 140, 13]);
    assert_eq!(baked_box(&bake, 28), [1, 2, 3, 4]);
    let contexts: Vec<_> = warnings.iter().map(|w| w.context).collect();
    assert_eq!(
        contexts,
        [
            "glyf composite too costly to draw for its bounds",
            "glyf composite too costly to draw for its bounds",
        ],
        "{warnings:?}"
    );
}

#[test]
fn instructions_ride_through_and_a_glyph_without_contours_is_empty() {
    // Glyph 1 has instructions; glyph 2 is a bare 10-byte header with
    // no contours, which HarfBuzz reads as an empty glyph.
    let mut header_only = vec![0, 0];
    for v in [7i16, 0, 7, 0] {
        header_only.extend_from_slice(&v.to_be_bytes());
    }
    let glyphs = vec![Vec::new(), triangle(&[0xB0, 0x01]), header_only];
    let (bake, warnings) = bake_at_900(&one_axis_font(&glyphs));
    assert!(warnings.is_empty(), "{warnings:?}");
    let glyph = SimpleGlyph::decode(bake.body(1)).unwrap();
    assert_eq!(glyph.instructions, vec![0xB0, 0x01]);
    assert_eq!(glyph.points(), vec![(3, 0), (13, 0), (13, 13)]);
    assert!(bake.body(2).is_empty());
    let metrics = bake.metrics.unwrap();
    // The empty glyph's phantom points start from its header's xMin:
    // its left side bearing origin is 7 - 0, so its bearing is -7.
    assert_eq!(
        (metrics[2].advance, metrics[2].lsb, metrics[2].bounds),
        (500, -7, None)
    );
}

#[test]
fn every_glyph_of_a_deep_shared_chain_is_walked_once() {
    // Glyphs 2 to 67 form a chain 66 composites deep, each also drawing
    // the triangle 20 times; glyphs 68 to 167 each draw the top of the
    // chain. The chain's two top links nest more than 64 levels and
    // keep their source boxes, and so does every glyph drawing them;
    // each glyph is still worked out once.
    const DEPTH: u16 = 66;
    const TOPS: u16 = 100;
    let mut glyphs = vec![Vec::new(), triangle(&[])];
    for k in 0..DEPTH {
        let mut records: Vec<Vec<u8>> = (0..20).map(|_| record(XY, 1, 1, 0, &[], false)).collect();
        let next = if k + 1 < DEPTH { 3 + k } else { 1 };
        records.push(record(XY, next, 0, 0, &[], true));
        glyphs.push(composite_of(&records));
    }
    for _ in 0..TOPS {
        glyphs.push(composite_of(&[record(XY, 2, 0, 0, &[], true)]));
    }
    let (bake, warnings) = bake_at_900(&one_axis_font(&glyphs));
    let (computed, drawn, _) = bake.extent_work;
    assert_eq!(computed, u64::from(1 + DEPTH + TOPS));
    assert_eq!(drawn, 0);
    let deep: Vec<_> = warnings
        .iter()
        .filter(|w| w.context == "glyf composite recursion exceeded cap")
        .collect();
    assert_eq!(deep.len(), usize::from(2 + TOPS), "{warnings:?}");
    // The third link nests 64 levels and its box is worked out; the
    // second keeps its source box.
    assert_eq!(baked_box(&bake, 4), [3, 0, 14, 13]);
    assert_eq!(baked_box(&bake, 3), [0, 0, 0, 0]);
}

#[test]
fn the_depth_cap_does_not_depend_on_glyph_order() {
    // A chain of 70 composites over the triangle, stored leaf first and
    // then root first: either way the 6 links nesting more than 64
    // levels keep their source boxes, and the rest are worked out.
    const LINKS: u16 = 70;
    for root_first in [false, true] {
        let mut glyphs = vec![Vec::new(), triangle(&[])];
        // The link at height `h` (1 for the one over the triangle).
        let gid_of = |h: u16| if root_first { 2 + LINKS - h } else { 1 + h };
        let mut links = vec![Vec::new(); usize::from(LINKS)];
        for h in 1..=LINKS {
            let child = if h == 1 { 1 } else { gid_of(h - 1) };
            links[usize::from(gid_of(h) - 2)] = composite_of(&[record(XY, child, 1, 0, &[], true)]);
        }
        glyphs.extend(links);
        let (bake, warnings) = bake_at_900(&one_axis_font(&glyphs));
        assert_eq!(warnings.len(), 6, "root first {root_first}: {warnings:?}");
        assert_eq!(baked_box(&bake, gid_of(64)), [3 + 64, 0, 13 + 64, 13]);
        assert_eq!(baked_box(&bake, gid_of(65)), [0, 0, 0, 0]);
        assert_eq!(bake.extent_work.0, u64::from(1 + LINKS));
    }
}

#[test]
fn draws_are_charged_for_the_tuples_they_decode() {
    // The triangle has 300 tuples; glyph 2 draws it 100 times by offset
    // and glyphs 3 to 52 each skew glyph 2. A draw of one of those
    // decodes the triangle's tuples 100 times: 240,503 units of the
    // 1,048,576 budget, so 4 are drawn and the other 46 take the box
    // around their components' boxes.
    let skew = [1.0, 0.0, 0.5, 1.0];
    let mut glyphs = vec![Vec::new(), triangle(&[])];
    let records: Vec<Vec<u8>> = (0..100)
        .map(|i| record(XY, 1, i, 0, &[], i == 99))
        .collect();
    glyphs.push(composite_of(&records));
    for _ in 0..50 {
        glyphs.push(composite_of(&[record(
            XY | COMP_WE_HAVE_A_TWO_BY_TWO,
            2,
            0,
            0,
            &skew,
            true,
        )]));
    }
    let (bake, warnings) = bake_at_900(&one_axis_font_with(&glyphs, 300));
    let (computed, drawn, spent) = bake.extent_work;
    assert_eq!((computed, drawn), (52, 4));
    assert_eq!(spent, 4 * 240_503);
    assert_eq!(warnings.len(), 46, "{warnings:?}");
}
