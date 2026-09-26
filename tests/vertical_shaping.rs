//! End-to-end check that vertical shaping pulls advances from
//! `vmtx` rather than `hmtx`, drives `y_advance` instead of
//! `x_advance`, and flips sign for top-to-bottom flow. The fixture
//! is a synthetic font built inline because bundling a real CJK
//! font would balloon the repo; the spec path this exercises is
//! identical to what HarfBuzz does for e.g. Noto Sans CJK.

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

/// Minimal SFNT with head / maxp / hhea / hmtx / vhea / vmtx / cmap
/// for glyphs 0..=3. Glyph 1 = 'A'; glyph 2 = 'B'; glyph 3 = 'C'.
/// Horizontal advances are 500 / 600 / 700; vertical advances are
/// 1000 / 1100 / 1200 so the two tables are distinguishable.
fn build_vertical_font() -> Vec<u8> {
    // head
    let mut head: Vec<u8> = Vec::new();
    head.extend_from_slice(&1u16.to_be_bytes()); // majorVersion
    head.extend_from_slice(&0u16.to_be_bytes());
    head.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    head.extend_from_slice(&0u32.to_be_bytes());
    head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head.extend_from_slice(&0u16.to_be_bytes());
    head.extend_from_slice(&1000u16.to_be_bytes());
    head.extend_from_slice(&[0; 8 + 8 + 8 + 2 + 2 + 2]);
    head.extend_from_slice(&0i16.to_be_bytes());
    head.extend_from_slice(&0i16.to_be_bytes());

    // maxp 0.5 (4 glyphs)
    let mut maxp = Vec::new();
    maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes());
    maxp.extend_from_slice(&4u16.to_be_bytes());

    // hhea, numberOfHMetrics = 4
    let mut hhea = Vec::new();
    hhea.extend_from_slice(&1u16.to_be_bytes());
    hhea.extend_from_slice(&0u16.to_be_bytes());
    hhea.extend_from_slice(&800i16.to_be_bytes());
    hhea.extend_from_slice(&(-200i16).to_be_bytes());
    hhea.extend_from_slice(&0i16.to_be_bytes());
    hhea.extend_from_slice(&[0; 14]);
    hhea.extend_from_slice(&[0; 8]);
    hhea.extend_from_slice(&0i16.to_be_bytes());
    hhea.extend_from_slice(&4u16.to_be_bytes());

    // hmtx: four longs
    let mut hmtx = Vec::new();
    for (adv, lsb) in &[(0u16, 0i16), (500, 0), (600, 0), (700, 0)] {
        hmtx.extend_from_slice(&adv.to_be_bytes());
        hmtx.extend_from_slice(&lsb.to_be_bytes());
    }

    // vhea, numberOfLongVerMetrics = 4
    let mut vhea = Vec::new();
    vhea.extend_from_slice(&1u16.to_be_bytes());
    vhea.extend_from_slice(&1u16.to_be_bytes()); // minor 1.1 (OpenType)
    vhea.extend_from_slice(&500i16.to_be_bytes());
    vhea.extend_from_slice(&(-500i16).to_be_bytes());
    vhea.extend_from_slice(&0i16.to_be_bytes());
    vhea.extend_from_slice(&[0; 14]);
    vhea.extend_from_slice(&[0; 8]);
    vhea.extend_from_slice(&0i16.to_be_bytes());
    vhea.extend_from_slice(&4u16.to_be_bytes());

    // vmtx: four longs, different from hmtx
    let mut vmtx = Vec::new();
    for (adv, tsb) in &[(0u16, 0i16), (1000, 0), (1100, 0), (1200, 0)] {
        vmtx.extend_from_slice(&adv.to_be_bytes());
        vmtx.extend_from_slice(&tsb.to_be_bytes());
    }

    // cmap: format 4 mapping 'A'..='C' to glyphs 1..=3 (idDelta = -64).
    let cmap_sub = build_cmap_format4(&[(b'A' as u16, b'C' as u16, -64)]);
    let cmap = build_cmap_wrapper(3, 1, &cmap_sub);

    let tables: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"cmap", cmap),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"maxp", maxp),
        (*b"vhea", vhea),
        (*b"vmtx", vmtx),
    ];
    assemble_sfnt(&tables)
}

/// Build a minimal cmap format-4 subtable for the supplied
/// contiguous `(start_code, end_code, id_delta)` ranges. Matches
/// the helper used by the internal shape-pipeline tests.
fn build_cmap_format4(segments: &[(u16, u16, i16)]) -> Vec<u8> {
    // Four segments, last is the spec-required (0xFFFF, 0xFFFF, 0, 1).
    // The caller supplies `segments.len() - 0` real segments, and we
    // append the sentinel ourselves.
    let mut segs: Vec<(u16, u16, i16)> = segments.to_vec();
    segs.push((0xFFFF, 0xFFFF, 1));
    let seg_count = segs.len() as u16;
    let seg_count_x2 = seg_count * 2;
    // Binary-search helpers (same formulas as spec § cmap Subtable Format 4).
    let search_range = 2 * (1u16 << ((seg_count as f32).log2().floor() as u16));
    let entry_selector = (search_range as f32 / 2.0).log2() as u16;
    let range_shift = seg_count_x2 - search_range;

    let mut out = Vec::new();
    out.extend_from_slice(&4u16.to_be_bytes()); // format
                                                // length filled at the end.
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // language
    out.extend_from_slice(&seg_count_x2.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&range_shift.to_be_bytes());
    for (_, end, _) in &segs {
        out.extend_from_slice(&end.to_be_bytes());
    }
    out.extend_from_slice(&0u16.to_be_bytes()); // reservedPad
    for (start, _, _) in &segs {
        out.extend_from_slice(&start.to_be_bytes());
    }
    for (_, _, delta) in &segs {
        out.extend_from_slice(&delta.to_be_bytes());
    }
    for _ in &segs {
        out.extend_from_slice(&0u16.to_be_bytes()); // idRangeOffset
    }
    // Patch length.
    let len = out.len() as u16;
    out[2..4].copy_from_slice(&len.to_be_bytes());
    out
}

/// Wraps one encoding subtable in a cmap table header.
fn build_cmap_wrapper(platform: u16, encoding: u16, subtable: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&1u16.to_be_bytes()); // numTables
    let header_len = 4 + 8; // 4 header + 8 per record
    out.extend_from_slice(&platform.to_be_bytes());
    out.extend_from_slice(&encoding.to_be_bytes());
    out.extend_from_slice(&(header_len as u32).to_be_bytes());
    out.extend_from_slice(subtable);
    out
}

fn assemble_sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let header_len = 12 + tables.len() * 16;
    let mut body_offset = header_len;
    let mut offsets = Vec::with_capacity(tables.len());
    for (_tag, body) in tables {
        offsets.push(body_offset);
        body_offset += body.len();
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    for ((tag, body), off) in tables.iter().zip(offsets.iter()) {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(*off as u32).to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    }
    for (_, body) in tables {
        out.extend_from_slice(body);
    }
    out
}

#[test]
fn horizontal_shaping_uses_hmtx_advances() {
    let data = build_vertical_font();
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    buffer.set_direction(Direction::Ltr);
    buffer.push_str("AB");

    let run = shape(&font, &buffer, &[]).unwrap();
    assert_eq!(run.len(), 2);
    assert_eq!(run.glyphs[0].x_advance, 500);
    assert_eq!(run.glyphs[1].x_advance, 600);
    // Horizontal flow: y_advance stays zero.
    assert_eq!(run.glyphs[0].y_advance, 0);
}

#[test]
fn vertical_top_to_bottom_pulls_from_vmtx_and_negates_y() {
    let data = build_vertical_font();
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    buffer.set_direction(Direction::Ttb);
    buffer.push_str("AB");

    let run = shape(&font, &buffer, &[]).unwrap();
    assert_eq!(run.len(), 2);
    // Vertical flow: y advances come from vmtx (1000/1100), negated
    // because TTB moves the pen downward.
    assert_eq!(run.glyphs[0].y_advance, -1000);
    assert_eq!(run.glyphs[1].y_advance, -1100);
    // x_advance is pinned to zero so glyphs stack on-axis.
    assert_eq!(run.glyphs[0].x_advance, 0);
    assert_eq!(run.glyphs[1].x_advance, 0);
}

#[test]
fn vertical_bottom_to_top_preserves_positive_advance() {
    let data = build_vertical_font();
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    buffer.set_direction(Direction::Btt);
    buffer.push_str("C");

    let run = shape(&font, &buffer, &[]).unwrap();
    assert_eq!(run.len(), 1);
    // BTT is a reverse vertical flow. The advance stays positive so
    // the pen walks upward.
    assert_eq!(run.glyphs[0].y_advance, 1200);
    assert_eq!(run.glyphs[0].x_advance, 0);
}

#[test]
fn vertical_without_vmtx_falls_back_to_em_square() {
    // Font without a vmtx/vhea: shaping vertically should still
    // produce a sane non-zero y_advance derived from hhea.
    // Reuse Open Sans, which has no vertical tables.
    let data: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
    let blob = Blob::new(data);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 16.0);
    let mut buffer = Buffer::new();
    buffer.set_direction(Direction::Ttb);
    buffer.push_str("A");

    let run = shape(&font, &buffer, &[]).unwrap();
    assert_eq!(run.len(), 1);
    assert_eq!(run.glyphs[0].x_advance, 0);
    // The fallback value is `ascent - descent` (always positive), then
    // negated for TTB, so we expect a negative advance here.
    assert!(run.glyphs[0].y_advance < 0);
}
