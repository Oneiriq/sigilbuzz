//! Verifies bounding-box extraction against a real font. Uses Open
//! Sans Regular (already shipping as a fixture) and compares
//! sigilbuzz's numbers against ttf-parser via the rustybuzz
//! re-export so we're not inventing reference values.

use sigilbuzz::{Blob, Face};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");

#[test]
fn ascii_glyph_bounds_are_sane() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let cmap = face.cmap().unwrap();

    for ch in ['A', 'g', 'x', '0'] {
        let gid = cmap.glyph_id(ch).unwrap();
        let bounds = face
            .glyph_bounds(gid)
            .unwrap_or_else(|e| panic!("bounds for {ch:?}: {e}"))
            .unwrap_or_else(|| panic!("no bounds for {ch:?}"));
        assert!(bounds.x_min < bounds.x_max, "{ch:?}: empty x range");
        assert!(bounds.y_min < bounds.y_max, "{ch:?}: empty y range");
        assert!(bounds.num_contours > 0, "{ch:?}: expected simple glyph");
    }
}

#[test]
fn space_glyph_reports_no_outline() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let space = cmap.glyph_id(' ');
    // Some fonts don't map U+0020 (they route it through GPOS pair
    // adjustment or a default-glyph fallback); when they do, the
    // glyph has no outline and should report no bounds.
    if let Some(gid) = space {
        let bounds = face.glyph_bounds(gid).unwrap();
        assert!(
            bounds.is_none(),
            "space glyph unexpectedly has an outline: {bounds:?}"
        );
    }
}

#[test]
fn sigilbuzz_bounds_agree_with_ttf_parser() {
    let blob = Blob::new(OPEN_SANS);
    let face = Face::parse(&blob, 0).unwrap();
    let cmap = face.cmap().unwrap();

    let ttfp =
        rustybuzz::ttf_parser::Face::parse(OPEN_SANS, 0).expect("ttf-parser reads the same bytes");

    for ch in ['A', 'Q', 'g', 'x', '0', 'i'] {
        let gid = cmap.glyph_id(ch).unwrap();
        let sigil = face.glyph_bounds(gid).unwrap().unwrap();
        let ttfp_bbox = ttfp
            .glyph_bounding_box(rustybuzz::ttf_parser::GlyphId(gid))
            .unwrap_or_else(|| panic!("ttf-parser has no bounds for {ch:?}"));

        assert_eq!(
            sigil.x_min, ttfp_bbox.x_min,
            "x_min diverged for {ch:?}: sigil={} ttf-parser={}",
            sigil.x_min, ttfp_bbox.x_min
        );
        assert_eq!(
            sigil.y_min, ttfp_bbox.y_min,
            "y_min diverged for {ch:?}: sigil={} ttf-parser={}",
            sigil.y_min, ttfp_bbox.y_min
        );
        assert_eq!(
            sigil.x_max, ttfp_bbox.x_max,
            "x_max diverged for {ch:?}: sigil={} ttf-parser={}",
            sigil.x_max, ttfp_bbox.x_max
        );
        assert_eq!(
            sigil.y_max, ttfp_bbox.y_max,
            "y_max diverged for {ch:?}: sigil={} ttf-parser={}",
            sigil.y_max, ttfp_bbox.y_max
        );
    }
}
