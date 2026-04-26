//! Integration tests for CBDT/CBLC and sbix bitmap-font support.
//!
//! Drives the parsers against the hand-crafted fixtures in
//! `tests/fixtures/{cbdt,sbix}_synthetic.ttf` (built deterministically
//! by `tests/tools/build_{cbdt,sbix}_fixture.py`).
//!
//! The fixtures hold one PNG-tagged glyph each. Tests check the
//! expose-bytes contract: sigilbuzz returns the raw PNG payload
//! intact, exposes strike + per-glyph metrics, and never tries to
//! decode pixels.

use sigilbuzz::tables::sbix::TAG_PNG;
use sigilbuzz::tables::GlyphBitmapMetrics;
use sigilbuzz::{Blob, Face};

const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

#[test]
fn cbdt_synthetic_face_exposes_glyph_bitmap() {
    let bytes = std::fs::read("tests/fixtures/cbdt_synthetic.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let cblc = face.cblc().unwrap().expect("CBLC present");
    assert_eq!(cblc.num_sizes(), 1);
    let size = cblc.size(0).unwrap();
    assert_eq!(size.ppem_x, 32);
    assert_eq!(size.ppem_y, 32);
    assert_eq!(size.bit_depth, 32);
    assert_eq!(size.start_glyph_index, 1);

    let cbdt = face.cbdt().unwrap().expect("CBDT present");
    let loc = cblc.locate(&size, 1).unwrap().expect("gid 1 in strike");
    let bm = cbdt.glyph_bitmap(&loc).unwrap();
    assert_eq!(bm.image_format, 17);
    match bm.metrics {
        GlyphBitmapMetrics::Small(s) => {
            assert_eq!(s.height, 10);
            assert_eq!(s.width, 10);
            assert_eq!(s.advance, 12);
        }
        GlyphBitmapMetrics::Big(_) => panic!("format 17 carries small metrics"),
    }
    assert!(bm.data.starts_with(PNG_SIGNATURE));
}

#[test]
fn cbdt_synthetic_face_glyph_bitmap_unified_accessor() {
    let bytes = std::fs::read("tests/fixtures/cbdt_synthetic.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    // Closest strike is 32; ask for 28 to exercise the picker.
    let entry = face
        .glyph_bitmap(1, 28)
        .unwrap()
        .expect("CBDT bitmap for gid 1");
    match entry {
        sigilbuzz::GlyphBitmapEntry::Cbdt {
            ppem_x,
            ppem_y,
            bitmap,
        } => {
            assert_eq!(ppem_x, 32);
            assert_eq!(ppem_y, 32);
            assert_eq!(bitmap.image_format, 17);
            assert!(bitmap.data.starts_with(PNG_SIGNATURE));
        }
        sigilbuzz::GlyphBitmapEntry::Sbix { .. } => panic!("expected CBDT"),
        sigilbuzz::GlyphBitmapEntry::Ebdt { .. } => panic!("expected CBDT"),
    }
}

#[test]
fn cbdt_synthetic_returns_none_for_uncovered_gid() {
    let bytes = std::fs::read("tests/fixtures/cbdt_synthetic.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    // gid 0 (.notdef) is outside the strike's [1..=1] range.
    assert!(face.glyph_bitmap(0, 32).unwrap().is_none());
}

#[test]
fn sbix_synthetic_face_exposes_strike_and_glyph() {
    let bytes = std::fs::read("tests/fixtures/sbix_synthetic.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let sbix = face.sbix().unwrap().expect("sbix present");
    assert_eq!(sbix.num_strikes(), 1);
    let strike = sbix.strike(0).unwrap().unwrap();
    assert_eq!(strike.ppem(), 32);
    assert_eq!(strike.ppi(), 72);

    // gid 0 (.notdef) is empty; gid 1 carries the PNG.
    assert!(strike.glyph(0).unwrap().is_none());
    let g = strike.glyph(1).unwrap().expect("gid 1 has bitmap");
    assert_eq!(g.graphic_type, TAG_PNG);
    assert_eq!(g.origin_offset_x, 0);
    assert_eq!(g.origin_offset_y, 0);
    assert!(g.data.starts_with(PNG_SIGNATURE));
}

#[test]
fn sbix_synthetic_face_glyph_bitmap_unified_accessor() {
    let bytes = std::fs::read("tests/fixtures/sbix_synthetic.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let entry = face
        .glyph_bitmap(1, 30)
        .unwrap()
        .expect("sbix bitmap for gid 1");
    match entry {
        sigilbuzz::GlyphBitmapEntry::Sbix { ppem, ppi, glyph } => {
            assert_eq!(ppem, 32);
            assert_eq!(ppi, 72);
            assert_eq!(glyph.graphic_type, TAG_PNG);
            assert!(glyph.data.starts_with(PNG_SIGNATURE));
        }
        sigilbuzz::GlyphBitmapEntry::Cbdt { .. } => panic!("expected sbix"),
        sigilbuzz::GlyphBitmapEntry::Ebdt { .. } => panic!("expected sbix"),
    }
}

#[test]
fn face_glyph_bitmap_returns_none_for_outline_only_font() {
    // Open Sans has no CBDT, no sbix — should yield None cleanly.
    let bytes = std::fs::read("tests/fixtures/opensans_regular.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.glyph_bitmap(1, 32).unwrap().is_none());
    assert!(face.cblc().unwrap().is_none());
    assert!(face.cbdt().unwrap().is_none());
    assert!(face.sbix().unwrap().is_none());
}
