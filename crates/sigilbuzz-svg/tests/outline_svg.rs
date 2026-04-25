//! Integration tests for outline-only SVG emission.
//!
//! These tests load Open Sans (TrueType, ASCII glyphs) from the
//! shaper's bundled fixtures via a relative include, drive a few
//! glyphs through [`glyph_to_svg`], and assert structural invariants
//! of the output: well-formed `<svg>` framing, a single `d="..."`
//! attribute, deterministic re-emission. We deliberately avoid an
//! XML parser dependency on the *output* path — the production crate
//! does no XML parsing at all, and these tests stay matched to the
//! production policy by checking byte-level structure rather than
//! parsing the result.

use sigilbuzz::Face;
use sigilbuzz_svg::{glyph_to_svg, path_bbox, path_data};

const OPEN_SANS: &[u8] = include_bytes!("../../../tests/fixtures/opensans_regular.ttf");

fn face() -> Face<'static> {
    Face::parse_bytes(OPEN_SANS, 0).expect("opensans face parses")
}

fn gid_for(face: &Face<'_>, ch: char) -> u16 {
    face.cmap()
        .expect("cmap")
        .glyph_id(ch)
        .unwrap_or_else(|| panic!("glyph for {ch:?}"))
}

#[test]
fn ascii_glyph_round_trips_to_svg() {
    let face = face();
    for ch in ['A', 'g', 'O'] {
        let gid = gid_for(&face, ch);
        let svg = glyph_to_svg(&face, gid).unwrap_or_else(|| panic!("svg for {ch}"));
        assert!(svg.starts_with("<svg "), "missing <svg prefix for {ch}: {svg}");
        assert!(svg.ends_with("</svg>"), "missing </svg> tail for {ch}");
        // Exactly one path element with a d="..." attribute.
        let path_count = svg.matches("<path ").count();
        assert_eq!(path_count, 1, "expected exactly one <path> for {ch}, got {path_count}");
        let d_count = svg.matches(" d=\"").count();
        assert_eq!(d_count, 1, "expected exactly one d=\"\" for {ch}");
        // Path data starts with a MoveTo command.
        let after_d = svg.split(" d=\"").nth(1).unwrap();
        assert!(
            after_d.starts_with('M'),
            "expected path data to start with M for {ch}, got: {}",
            &after_d[..after_d.find('"').unwrap_or(after_d.len())]
        );
        // Quotes balance: every " has a partner.
        let quote_count = svg.matches('"').count();
        assert_eq!(quote_count % 2, 0, "unbalanced quotes for {ch}: {svg}");
        // Angle brackets balance.
        let open = svg.matches('<').count();
        let close = svg.matches('>').count();
        assert_eq!(open, close, "unbalanced <> for {ch}");
    }
}

#[test]
fn whitespace_glyph_returns_none() {
    let face = face();
    let gid = gid_for(&face, ' ');
    // Open Sans ' ' (gid=2 typically) has no outline.
    assert!(
        glyph_to_svg(&face, gid).is_none(),
        "whitespace glyph should yield no SVG"
    );
}

#[test]
fn output_is_byte_deterministic() {
    let face = face();
    let gid = gid_for(&face, 'A');
    let a = glyph_to_svg(&face, gid).expect("A");
    let b = glyph_to_svg(&face, gid).expect("A");
    assert_eq!(a, b, "same input must yield byte-identical SVG");
}

#[test]
fn viewbox_contains_glyph_bbox() {
    let face = face();
    let gid = gid_for(&face, 'A');
    let outline = face.glyph_outline(gid).unwrap().unwrap();
    let (mnx, mny, mxx, mxy) = path_bbox(outline.ops()).unwrap();
    let svg = glyph_to_svg(&face, gid).unwrap();
    // Pull the viewBox attribute and parse its four numbers.
    let vb = svg
        .split("viewBox=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("viewBox attr");
    let parts: Vec<f32> = vb.split_whitespace().map(|s| s.parse().unwrap()).collect();
    assert_eq!(parts.len(), 4);
    let (vx, vy, vw, vh) = (parts[0], parts[1], parts[2], parts[3]);
    assert!(vx <= mnx, "viewBox x={vx} should be <= bbox min_x={mnx}");
    assert!(vy <= mny, "viewBox y={vy} should be <= bbox min_y={mny}");
    assert!(vx + vw >= mxx, "viewBox should cover bbox max_x");
    assert!(vy + vh >= mxy, "viewBox should cover bbox max_y");
}

#[test]
fn path_data_string_well_formed() {
    let face = face();
    let gid = gid_for(&face, 'O');
    let outline = face.glyph_outline(gid).unwrap().unwrap();
    let d = path_data(outline.ops());
    assert!(d.starts_with('M'), "path data must start with M");
    // 'O' is a closed glyph with two contours (outer, inner). The
    // shaper emits one Close per contour.
    let z_count = d.matches('Z').count();
    assert!(
        z_count >= 1,
        "closed glyph 'O' should contain at least one Z, got: {d}"
    );
}
