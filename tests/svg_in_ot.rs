//! Integration tests for the OpenType `SVG ` (SVG-in-OT) table.
//!
//! Drives the parser against the hand-crafted fixture in
//! `tests/fixtures/svg_synthetic.ttf` (built by
//! `tests/tools/build_svg_fixture.py`).
//!
//! The fixture carries one inline SVG document covering gid 1. Tests
//! check the expose-bytes contract: sigilbuzz returns the raw XML
//! payload intact, exposes the gzip-magic bit (false here, the
//! fixture is plain ASCII), and never tries to parse the XML or
//! rasterize.

use sigilbuzz::{Blob, Face};

#[test]
fn svg_synthetic_face_exposes_table() {
    let bytes = std::fs::read("tests/fixtures/svg_synthetic.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let svg = face.svg().unwrap().expect("SVG table present");
    assert_eq!(svg.num_entries(), 1);
}

#[test]
fn svg_synthetic_document_for_gid_returns_payload() {
    let bytes = std::fs::read("tests/fixtures/svg_synthetic.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();

    let doc = face.svg_document(1).unwrap().expect("gid 1 has SVG");
    assert_eq!(doc.start_gid, 1);
    assert_eq!(doc.end_gid, 1);
    assert!(!doc.gzipped, "fixture is plain ASCII, not gzipped");

    // The fixture's payload starts with `<svg` and contains a
    // `<circle>`. We don't parse the XML, just confirm the bytes
    // round-trip intact.
    let text = core::str::from_utf8(doc.data).expect("ascii payload");
    assert!(text.starts_with("<svg"));
    assert!(text.contains("<circle"));
    assert!(text.ends_with("</svg>"));
}

#[test]
fn svg_synthetic_returns_none_for_uncovered_gid() {
    let bytes = std::fs::read("tests/fixtures/svg_synthetic.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    // gid 0 (.notdef) is outside the [1..=1] record range.
    assert!(face.svg_document(0).unwrap().is_none());
    // gid past end of font also returns None.
    assert!(face.svg_document(99).unwrap().is_none());
}

#[test]
fn face_svg_returns_none_for_outline_only_font() {
    // Open Sans has no SVG table: should yield None cleanly.
    let bytes = std::fs::read("tests/fixtures/opensans_regular.ttf").unwrap();
    let blob = Blob::new(&bytes);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.svg().unwrap().is_none());
    assert!(face.svg_document(1).unwrap().is_none());
}
