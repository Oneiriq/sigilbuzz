//! A `VARC` table sigilbuzz cannot parse counts as absent.
//!
//! HarfBuzz 14.5.0 drops a `VARC` its sanitizer rejects and draws and
//! shapes the font from `glyf` or CFF. After 14.5.0, HarfBuzz and the
//! VARC spec moved the MultiItemVariationStore to a new layout (a 32-bit
//! region count, and an Offset32 to each MultiVarData's delta-set
//! INDEX), so fonts built to the updated spec carry a store this parser
//! rejects. Before, every glyph of such a font failed to draw, and
//! shaping failed (fallback mark positioning) or fell back to the
//! ascender (vertical origins), where HarfBuzz 14.5.0 ignores the table.
//!
//! The tests splice a `VARC` table with a new-layout store into a CFF
//! font and a `glyf` font, and check that every glyph draws, and every
//! run shapes, exactly as the font without `VARC`.

use sigilbuzz::tables::{Outline, OutlineSink, PathOp};
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const CFF_FONT: &[u8] = include_bytes!("fonts/SourceCodePro-Latin-Subset.otf");
const GLYF_FONT: &[u8] = include_bytes!("fixtures/varc_parity.ttf");

/// A `VARC` table covering `covered`, each glyph one component naming
/// glyph 1, over a MultiItemVariationStore in the layout HarfBuzz
/// adopted after 14.5.0: `u32` region count, and an Offset32 from each
/// MultiVarData to its delta-set INDEX.
fn new_layout_varc(covered: &[u16]) -> Vec<u8> {
    let mut coverage = vec![0, 1];
    coverage.extend_from_slice(&(covered.len() as u16).to_be_bytes());
    for g in covered {
        coverage.extend_from_slice(&g.to_be_bytes());
    }

    let mut store = Vec::new();
    store.extend_from_slice(&1u16.to_be_bytes()); // format
    store.extend_from_slice(&12u32.to_be_bytes()); // region list
    store.extend_from_slice(&1u16.to_be_bytes()); // one MultiVarData
    store.extend_from_slice(&30u32.to_be_bytes()); // at 30
                                                   // SparseVarRegionList at 12: u32 count, Offset32 per region.
    store.extend_from_slice(&1u32.to_be_bytes());
    store.extend_from_slice(&8u32.to_be_bytes());
    // The region at 20: axis 0, peak +1.
    store.extend_from_slice(&1u16.to_be_bytes());
    store.extend_from_slice(&0u16.to_be_bytes());
    store.extend_from_slice(&0i16.to_be_bytes());
    store.extend_from_slice(&0x4000i16.to_be_bytes());
    store.extend_from_slice(&0x4000i16.to_be_bytes());
    assert_eq!(store.len(), 30);
    // MultiVarData at 30: format, region indexes, Offset32 to the
    // delta-set INDEX right after it.
    store.push(1);
    store.extend_from_slice(&1u16.to_be_bytes());
    store.extend_from_slice(&0u16.to_be_bytes());
    store.extend_from_slice(&9u32.to_be_bytes());
    // One delta set: one i8, 100.
    store.extend_from_slice(&1u32.to_be_bytes());
    store.extend_from_slice(&[1, 1, 3, 0x00, 100]);

    // One record per covered glyph: TRANSFORM_HAS_VARIATION and
    // HAVE_TRANSLATE_X, glyph 1, variation index 0, translate 0.
    let record = [0x18, 0x00, 0x01, 0x00, 0x00, 0x00];
    let mut records = (covered.len() as u32).to_be_bytes().to_vec();
    records.push(1); // offSize
    for i in 0..=covered.len() {
        records.push((1 + i * record.len()) as u8);
    }
    for _ in covered {
        records.extend_from_slice(&record);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&[0, 1, 0, 0]); // version 1.0
    let mut at = 24u32;
    for part in [&coverage, &store] {
        out.extend_from_slice(&at.to_be_bytes());
        at += part.len() as u32;
    }
    out.extend_from_slice(&0u32.to_be_bytes()); // no conditions
    out.extend_from_slice(&0u32.to_be_bytes()); // no axis indices
    out.extend_from_slice(&at.to_be_bytes()); // glyph records
    out.extend_from_slice(&coverage);
    out.extend_from_slice(&store);
    out.extend_from_slice(&records);
    out
}

/// `font` with its tables rewritten into a new directory, `VARC`
/// replaced by `varc`, or dropped when `varc` is `None`.
fn with_varc(font: &[u8], varc: Option<&[u8]>) -> Vec<u8> {
    let face = Face::parse_bytes(font, 0).unwrap();
    let mut tables: Vec<([u8; 4], &[u8])> = face
        .records()
        .iter()
        .filter(|r| &r.tag != b"VARC")
        .map(|r| (r.tag, face.table_bytes(r.tag).unwrap()))
        .collect();
    if let Some(varc) = varc {
        tables.push((*b"VARC", varc));
    }
    tables.sort_by_key(|t| t.0);
    let header = 12 + 16 * tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&font[..4]);
    out.extend_from_slice(&(tables.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    let mut body = Vec::new();
    for (tag, data) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&((header + body.len()) as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
    }
    out.extend_from_slice(&body);
    out
}

#[derive(Default)]
struct Recorder(Outline);

impl OutlineSink for Recorder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(PathOp::MoveTo { x, y });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(PathOp::LineTo { x, y });
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.push(PathOp::QuadTo { cx, cy, x, y });
    }
    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.0.push(PathOp::CubicTo {
            c1x,
            c1y,
            c2x,
            c2y,
            x,
            y,
        });
    }
    fn close(&mut self) {
        self.0.push(PathOp::Close);
    }
}

/// Every glyph of `with` draws, through every outline path, exactly as
/// the same glyph of `without` does.
fn assert_outlines_match(with: &Face<'_>, without: &Face<'_>, coords: &[f32]) {
    let n = without.maxp().unwrap().num_glyphs;
    let (a, b) = (with.glyph_outlines(coords), without.glyph_outlines(coords));
    for gid in 0..n {
        let want = without.glyph_outline_at_coords(gid, coords).unwrap();
        assert_eq!(
            with.glyph_outline_at_coords(gid, coords).unwrap(),
            want,
            "glyph {gid}"
        );
        assert_eq!(
            a.outline(gid).unwrap(),
            b.outline(gid).unwrap(),
            "glyph {gid}"
        );
        assert_eq!(a.outline(gid).unwrap(), want, "glyph {gid}");
        let mut sink = Recorder::default();
        let drew = a.draw(gid, &mut sink).unwrap();
        assert_eq!(drew.then_some(sink.0), want, "glyph {gid}");
        if coords.is_empty() {
            assert_eq!(with.glyph_outline(gid).unwrap(), want, "glyph {gid}");
        }
    }
}

#[test]
fn a_cff_font_with_an_unparsable_varc_draws_and_shapes_without_it() {
    let varc = new_layout_varc(&[1, 2, 3]);
    let with = with_varc(CFF_FONT, Some(&varc));
    let without = with_varc(CFF_FONT, None);
    let (with_blob, without_blob) = (Blob::new(&with), Blob::new(&without));
    let with = Face::parse(&with_blob, 0).unwrap();
    let without = Face::parse(&without_blob, 0).unwrap();
    // The table is there, and this parser rejects its store.
    assert!(with.varc().is_err());
    assert_outlines_match(&with, &without, &[]);

    // "á" decomposes to a and U+0301, which the font lacks: fallback
    // mark positioning reads glyph extents. A vertical run without
    // VORG or vmtx centers the extents for the origins.
    for (text, direction) in [("\u{e1}b", Direction::Ltr), ("abc", Direction::Ttb)] {
        let run = |face: &Face<'_>| {
            let font = Font::new(face.clone(), 1000.0);
            let mut buffer = Buffer::new();
            buffer.set_direction(direction);
            buffer.push_str(text);
            shape(&font, &buffer, &[]).unwrap().glyphs
        };
        assert_eq!(run(&with), run(&without), "{text:?} {direction:?}");
    }
}

#[test]
fn a_glyf_font_with_an_unparsable_varc_draws_without_it() {
    // varc_parity.ttf with its VARC table swapped for one in the new
    // layout: glyphs 3 and up draw their (empty) glyf outlines.
    let varc = new_layout_varc(&[3, 4, 5]);
    let with = with_varc(GLYF_FONT, Some(&varc));
    let without = with_varc(GLYF_FONT, None);
    let (with_blob, without_blob) = (Blob::new(&with), Blob::new(&without));
    let with = Face::parse(&with_blob, 0).unwrap();
    let without = Face::parse(&without_blob, 0).unwrap();
    assert!(with.varc().is_err());
    for coords in [&[][..], &[0.5, -0.25, 0.75][..]] {
        assert_outlines_match(&with, &without, coords);
    }
}
