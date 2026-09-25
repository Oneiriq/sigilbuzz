//! Regression tests for hostile fonts that used to crash, hang, or
//! allocate without bound in the render crate.
//!
//! Every font here is built from hand-made bytes. Each test notes what
//! the input did before the fix.

use sigilbuzz::Face;
use sigilbuzz_render::{Rasterizer, RenderError};

// ---------------------------------------------------------------------------
// SFNT builders
// ---------------------------------------------------------------------------

/// Builds an SFNT from `(tag, body)` pairs. The directory is sorted by
/// tag and every body is padded to four bytes.
fn sfnt(mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|(tag, _)| *tag);
    let header_len = 12 + 16 * tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    let mut offset = header_len as u32;
    for (tag, body) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        offset += body.len().next_multiple_of(4) as u32;
    }
    for (_, body) in &tables {
        out.extend_from_slice(body);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    out
}

fn maxp(num_glyphs: u16) -> Vec<u8> {
    let mut m = 0x0000_5000u32.to_be_bytes().to_vec();
    m.extend_from_slice(&num_glyphs.to_be_bytes());
    m
}

fn head(upem: u16) -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    h.extend_from_slice(&0u32.to_be_bytes());
    h.extend_from_slice(&0u32.to_be_bytes());
    h.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    h.extend_from_slice(&0u16.to_be_bytes());
    h.extend_from_slice(&upem.to_be_bytes());
    h.extend_from_slice(&[0; 16]); // created, modified
    h.extend_from_slice(&[0; 8]); // bbox
    h.extend_from_slice(&0u16.to_be_bytes()); // macStyle
    h.extend_from_slice(&8u16.to_be_bytes()); // lowestRecPPEM
    h.extend_from_slice(&2i16.to_be_bytes()); // fontDirectionHint
    h.extend_from_slice(&0i16.to_be_bytes()); // indexToLocFormat: short
    h.extend_from_slice(&0i16.to_be_bytes()); // glyphDataFormat
    h
}

fn hhea(num_glyphs: u16) -> Vec<u8> {
    let mut h = 0x0001_0000u32.to_be_bytes().to_vec();
    for v in [800i16, -200, 0] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    h.extend_from_slice(&500u16.to_be_bytes());
    for v in [0i16, 0, 500, 1, 0, 0, 0, 0, 0, 0, 0] {
        h.extend_from_slice(&v.to_be_bytes());
    }
    h.extend_from_slice(&num_glyphs.to_be_bytes());
    h
}

/// One-contour square glyph from `(0, 0)` to `(side, side)`.
fn square_glyph(side: i16) -> Vec<u8> {
    let mut g = Vec::new();
    for v in [1i16, 0, 0, side, side] {
        g.extend_from_slice(&v.to_be_bytes());
    }
    g.extend_from_slice(&3u16.to_be_bytes()); // endPts[0]
    g.extend_from_slice(&0u16.to_be_bytes()); // no instructions
    g.extend_from_slice(&[0x01; 4]); // on-curve, long deltas
    for d in [0, side, 0, -side] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    for d in [0, 0, side, 0] {
        g.extend_from_slice(&d.to_be_bytes());
    }
    g
}

/// Outline tables for two glyphs: gid 0 empty, gid 1 a 200-unit
/// square, at 1024 units per em.
fn outline_tables() -> Vec<([u8; 4], Vec<u8>)> {
    let square = square_glyph(200);
    let mut loca = Vec::new();
    for off in [0usize, 0, square.len()] {
        loca.extend_from_slice(&((off / 2) as u16).to_be_bytes());
    }
    let mut hmtx = Vec::new();
    for _ in 0..2 {
        hmtx.extend_from_slice(&500u16.to_be_bytes());
        hmtx.extend_from_slice(&0i16.to_be_bytes());
    }
    vec![
        (*b"head", head(1024)),
        (*b"maxp", maxp(2)),
        (*b"hhea", hhea(2)),
        (*b"hmtx", hmtx),
        (*b"loca", loca),
        (*b"glyf", square),
    ]
}

/// CPAL v0 with one palette holding one opaque red entry.
fn cpal() -> Vec<u8> {
    let mut c = Vec::new();
    for v in [0u16, 1, 1, 1] {
        c.extend_from_slice(&v.to_be_bytes());
    }
    c.extend_from_slice(&14u32.to_be_bytes()); // colorRecordsArrayOffset
    c.extend_from_slice(&0u16.to_be_bytes()); // colorRecordIndices[0]
    c.extend_from_slice(&[0, 0, 255, 255]); // BGRA
    c
}

/// COLRv0 table: gid 1 drawn as one layer of its own outline.
fn colr_v0() -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&0u16.to_be_bytes()); // version
    c.extend_from_slice(&1u16.to_be_bytes()); // numBaseGlyphRecords
    c.extend_from_slice(&14u32.to_be_bytes()); // baseGlyphRecordsOffset
    c.extend_from_slice(&20u32.to_be_bytes()); // layerRecordsOffset
    c.extend_from_slice(&1u16.to_be_bytes()); // numLayerRecords
    for v in [1u16, 0, 1] {
        c.extend_from_slice(&v.to_be_bytes()); // gid, firstLayer, numLayers
    }
    for v in [1u16, 0] {
        c.extend_from_slice(&v.to_be_bytes()); // layer gid, palette index
    }
    c
}

/// COLRv1 table: gid 1 is `PaintGlyph(gid 1, PaintSolid(entry 0))`.
fn colr_v1() -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&1u16.to_be_bytes()); // version
    c.extend_from_slice(&0u16.to_be_bytes()); // numBaseGlyphRecords
    c.extend_from_slice(&30u32.to_be_bytes()); // baseGlyphRecordsOffset
    c.extend_from_slice(&30u32.to_be_bytes()); // layerRecordsOffset
    c.extend_from_slice(&0u16.to_be_bytes()); // numLayerRecords
    c.extend_from_slice(&30u32.to_be_bytes()); // baseGlyphListOffset
    c.extend_from_slice(&[0; 12]); // layerList, clipList, varStore
    c.extend_from_slice(&1u32.to_be_bytes()); // BaseGlyphList count
    c.extend_from_slice(&1u16.to_be_bytes()); // gid
    c.extend_from_slice(&10u32.to_be_bytes()); // paint offset
    c.extend_from_slice(&[10, 0, 0, 6]); // PaintGlyph, child at +6
    c.extend_from_slice(&1u16.to_be_bytes()); // outline gid
    c.push(2); // PaintSolid
    c.extend_from_slice(&0u16.to_be_bytes()); // palette entry
    c.extend_from_slice(&0x4000u16.to_be_bytes()); // alpha 1.0
    c
}

fn colr_font(colr: Vec<u8>) -> Vec<u8> {
    let mut tables = outline_tables();
    tables.push((*b"COLR", colr));
    tables.push((*b"CPAL", cpal()));
    sfnt(tables)
}

// ---------------------------------------------------------------------------
// Outline and color glyph size caps
// ---------------------------------------------------------------------------

/// Pixel size at which the 200-unit square spans about 175000 pixels
/// per side: inside the rasterizer's coordinate range, but a 30 GB
/// coverage buffer.
const HUGE_SIZE: f32 = 900_000.0;

#[test]
fn huge_outline_glyph_is_bad_size_not_a_giant_allocation() {
    // Used to allocate a ~175000 x 175000 alpha mask and abort.
    let font = sfnt(outline_tables());
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    assert_eq!(
        rast.rasterize_glyph(&face, 1, HUGE_SIZE, &[]).unwrap_err(),
        RenderError::BadSize(HUGE_SIZE)
    );
    // Ordinary sizes are unaffected.
    let pix = rast.rasterize_glyph(&face, 1, 100.0, &[]).unwrap();
    assert_eq!(pix.get(10, 10), 255);
}

#[test]
fn huge_colrv0_glyph_is_bad_size_not_a_giant_allocation() {
    // Used to rasterize the layer at full size and abort on the
    // allocation.
    let font = colr_font(colr_v0());
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    assert_eq!(
        rast.rasterize_colrv0_glyph(&face, 1, 0, HUGE_SIZE, &[])
            .unwrap_err(),
        RenderError::BadSize(HUGE_SIZE)
    );
    let pix = rast
        .rasterize_colrv0_glyph(&face, 1, 0, 100.0, &[])
        .unwrap();
    assert_eq!(pix.get(10, 10), [255, 0, 0, 255]);
}

#[test]
fn huge_colrv1_glyph_is_bad_size_not_a_giant_allocation() {
    // Used to rasterize the leaf at full size and abort on the
    // allocation.
    let font = colr_font(colr_v1());
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    assert_eq!(
        rast.rasterize_colrv1_glyph(&face, 1, 0, HUGE_SIZE, &[])
            .unwrap_err(),
        RenderError::BadSize(HUGE_SIZE)
    );
    let pix = rast
        .rasterize_colrv1_glyph(&face, 1, 0, 100.0, &[])
        .unwrap();
    assert_eq!(pix.get(10, 10), [255, 0, 0, 255]);
}

// ---------------------------------------------------------------------------
// EBDT composite fan-out
// ---------------------------------------------------------------------------

/// EBDT format 1 entry: small metrics plus a byte-aligned mask.
fn ebdt_mono(width: u8, height: u8, mask: &[u8]) -> Vec<u8> {
    let mut e = vec![height, width, 0, height, width];
    e.extend_from_slice(mask);
    e
}

/// EBDT format 8 entry: small metrics, a pad byte, and components.
fn ebdt_composite(width: u8, height: u8, components: &[u16]) -> Vec<u8> {
    let mut e = vec![height, width, 0, height, width, 0];
    e.extend_from_slice(&(components.len() as u16).to_be_bytes());
    for gid in components {
        e.extend_from_slice(&gid.to_be_bytes());
        e.extend_from_slice(&[0, 0]); // x, y offset
    }
    e
}

/// EBLC with one 16 ppem strike. Glyph `first + i` uses `entries[i]`,
/// with one index subtable (format 1) per glyph so each can carry its
/// own image format.
fn eblc(first: u16, entries: &[(u16, usize)]) -> Vec<u8> {
    let n = entries.len() as u32;
    let mut b = Vec::new();
    b.extend_from_slice(&2u16.to_be_bytes());
    b.extend_from_slice(&0u16.to_be_bytes());
    b.extend_from_slice(&1u32.to_be_bytes()); // numSizes
    b.extend_from_slice(&56u32.to_be_bytes()); // indexSubTableArrayOffset
    b.extend_from_slice(&(n * 8 + n * 16).to_be_bytes()); // indexTablesSize
    b.extend_from_slice(&n.to_be_bytes()); // numberOfIndexSubTables
    b.extend_from_slice(&0u32.to_be_bytes()); // colorRef
    b.extend_from_slice(&[0; 24]); // line metrics
    b.extend_from_slice(&first.to_be_bytes());
    b.extend_from_slice(&(first + entries.len() as u16 - 1).to_be_bytes());
    b.extend_from_slice(&[16, 16, 1, 1]); // ppemX, ppemY, bitDepth, flags
    for i in 0..entries.len() as u32 {
        let gid = first + i as u16;
        b.extend_from_slice(&gid.to_be_bytes());
        b.extend_from_slice(&gid.to_be_bytes());
        b.extend_from_slice(&(n * 8 + i * 16).to_be_bytes());
    }
    let mut offset = 0u32;
    for &(format, len) in entries {
        b.extend_from_slice(&1u16.to_be_bytes()); // index format 1
        b.extend_from_slice(&format.to_be_bytes());
        b.extend_from_slice(&4u32.to_be_bytes()); // past the EBDT header
        b.extend_from_slice(&offset.to_be_bytes());
        offset += len as u32;
        b.extend_from_slice(&offset.to_be_bytes());
    }
    b
}

#[test]
fn ebdt_composite_fan_out_stops_at_the_component_budget() {
    // gid 1..=4 are composites that each list the next gid 200 times,
    // and gid 5 is a plain mask. The depth cap allows all four levels,
    // so rendering gid 1 used to expand 200^4 = 1.6e9 components.
    let fan: Vec<u16> = (0..200).collect();
    let mut entries = Vec::new();
    for gid in 2..=5u16 {
        let components: Vec<u16> = fan.iter().map(|_| gid).collect();
        entries.push(ebdt_composite(8, 8, &components));
    }
    entries.push(ebdt_mono(8, 8, &[0xFF; 8]));
    let mut ebdt = vec![0, 2, 0, 0];
    for e in &entries {
        ebdt.extend_from_slice(e);
    }
    let formats: Vec<(u16, usize)> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (if i < 4 { 8 } else { 1 }, e.len()))
        .collect();
    let font = sfnt(vec![
        (*b"maxp", maxp(6)),
        (*b"EBLC", eblc(1, &formats)),
        (*b"EBDT", ebdt),
    ]);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let rast = Rasterizer::new();
    assert_eq!(
        rast.rasterize_bitmap_glyph(&face, 1, 16.0, &[])
            .unwrap_err(),
        RenderError::BitmapDecodeFailed("composite component budget exceeded")
    );
    // A single level of the same chain still renders.
    let pix = rast.rasterize_bitmap_glyph(&face, 4, 16.0, &[]).unwrap();
    assert_eq!(pix.get(0, 0), [0, 0, 0, 255]);
}

// ---------------------------------------------------------------------------
// SVG-in-OT
// ---------------------------------------------------------------------------

/// Font whose `SVG ` table maps gid 1 to `doc`.
fn svg_font(doc: &str) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&0u16.to_be_bytes()); // version
    t.extend_from_slice(&10u32.to_be_bytes()); // documentListOffset
    t.extend_from_slice(&0u32.to_be_bytes()); // reserved
    t.extend_from_slice(&1u16.to_be_bytes()); // numEntries
    t.extend_from_slice(&1u16.to_be_bytes()); // startGlyphID
    t.extend_from_slice(&1u16.to_be_bytes()); // endGlyphID
    t.extend_from_slice(&14u32.to_be_bytes()); // offset from list start
    t.extend_from_slice(&(doc.len() as u32).to_be_bytes());
    t.extend_from_slice(doc.as_bytes());
    sfnt(vec![(*b"maxp", maxp(2)), (*b"SVG ", t)])
}

#[test]
fn svg_shape_far_larger_than_the_canvas_rasterizes_only_the_canvas() {
    // The rect spans about 640000 pixels per side at this size. Its
    // full coverage mask used to be allocated (hundreds of gigabytes)
    // even though only the 64x64 canvas is visible.
    let doc = concat!(
        r#"<svg viewBox="0 0 100 100">"#,
        r#"<rect x="-50000" y="-50000" width="100000" height="100000" fill="red"/>"#,
        "</svg>"
    );
    let font = svg_font(doc);
    let face = Face::parse_bytes(&font, 0).unwrap();
    let pix = Rasterizer::new()
        .rasterize_svg_glyph(&face, 1, 64.0, &[])
        .unwrap();
    assert_eq!((pix.width, pix.height), (64, 64));
    assert!(pix.data.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
}

/// Appends `levels` groups named `{prefix}0..`, each holding `inner`
/// nested `<g>` elements around a `<use>` of the next group. The last
/// `<use>` points at `{prefix}{levels}`, which the caller defines.
fn use_chain(doc: &mut String, prefix: &str, levels: usize, inner: usize) {
    use std::fmt::Write;
    for level in 0..levels {
        write!(doc, r#"<g id="{prefix}{level}">"#).unwrap();
        doc.push_str(&"<g>".repeat(inner));
        write!(doc, r##"<use href="#{prefix}{}"/>"##, level + 1).unwrap();
        doc.push_str(&"</g>".repeat(inner));
        doc.push_str("</g>");
    }
}

/// Renders gid 1 of an SVG font on a thread with a 1 MiB stack, the
/// size of the main thread on Windows.
fn render_on_small_stack(doc: String) -> Result<[u8; 4], RenderError> {
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(move || {
            let font = svg_font(&doc);
            let face = Face::parse_bytes(&font, 0).unwrap();
            Rasterizer::new()
                .rasterize_svg_glyph(&face, 1, 10.0, &[])
                .map(|p| p.get(5, 5))
        })
        .unwrap()
        .join()
        .expect("no stack overflow")
}

#[test]
fn svg_use_expansion_past_the_nesting_cap_is_an_error_not_a_stack_overflow() {
    // `<use>` restarts the group depth count, so sixteen `<use>`
    // levels through 32-deep groups used to recurse over 500 walk
    // frames and overflow a 1 MiB stack in debug builds.
    let mut doc = String::from(r#"<svg viewBox="0 0 10 10"><defs>"#);
    use_chain(&mut doc, "u", 16, 31);
    doc.push_str(
        r##"<rect id="u16" width="10" height="10" fill="red"/></defs><use href="#u0"/></svg>"##,
    );
    assert_eq!(
        render_on_small_stack(doc),
        Err(RenderError::Parse("svg nesting"))
    );
}

#[test]
fn svg_deepest_xml_nesting_fits_a_small_stack() {
    // The XML reader accepts 256 levels of elements and the walk stops
    // at 32. Parsing, indexing, and dropping the tree all recurse.
    let doc = format!(
        r#"<svg viewBox="0 0 10 10">{}<rect width="10" height="10"/>{}</svg>"#,
        "<g>".repeat(255),
        "</g>".repeat(255)
    );
    assert_eq!(
        render_on_small_stack(doc),
        Err(RenderError::Parse("svg nesting"))
    );
}

#[test]
fn svg_deepest_allowed_nesting_fits_a_small_stack() {
    // The deepest walk the caps allow: 62 levels of nesting to the
    // masked rect, whose mask body nests another 61 levels.
    let mut doc = String::from(r#"<svg viewBox="0 0 10 10"><defs>"#);
    use_chain(&mut doc, "u", 6, 8);
    doc.push_str(r##"<rect id="u6" width="10" height="10" fill="red" mask="url(#m)"/>"##);
    use_chain(&mut doc, "v", 6, 8);
    doc.push_str(r##"<rect id="v6" width="10" height="10" fill="white"/>"##);
    doc.push_str(r##"<mask id="m"><use href="#v0"/></mask></defs><use href="#u0"/></svg>"##);
    assert_eq!(render_on_small_stack(doc), Ok([255, 0, 0, 255]));
}

#[test]
fn svg_path_number_after_closepath_is_a_parse_error() {
    // `Z` followed by a bare number used to loop forever, pushing a
    // Close op on every turn until memory ran out.
    let font = svg_font(r#"<svg viewBox="0 0 10 10"><path d="M0 0 L5 0 L5 5 Z 1"/></svg>"#);
    let face = Face::parse_bytes(&font, 0).unwrap();
    assert_eq!(
        Rasterizer::new()
            .rasterize_svg_glyph(&face, 1, 10.0, &[])
            .unwrap_err(),
        RenderError::Parse("svg path d")
    );
}
