//! End-to-end WOFF1 round-trip on a vendored Latin face.
//!
//! Confirms that `unwrap_woff1` accepts a real-world uncompressed
//! WOFF1 envelope and that the SFNT it produces parses with the
//! sigilbuzz core and shapes a non-empty buffer.

use sigilbuzz::{shape, Buffer, Face, Font};
use sigilbuzz_woff::{unwrap_woff1, wrap_woff1};

const WOFF1: &[u8] = include_bytes!("fixtures/opensans_latin_uncompressed.woff");
const TTF: &[u8] = include_bytes!("fixtures/opensans_latin.ttf");

#[test]
fn unwrap_woff1_round_trips_against_reference_ttf() {
    let sfnt = unwrap_woff1(WOFF1).expect("WOFF1 unwraps");

    // Same number of tables and same per-table content as the TTF.
    let face_woff = Face::parse_bytes(&sfnt, 0).expect("unwrapped SFNT parses");
    let face_ttf = Face::parse_bytes(TTF, 0).expect("reference TTF parses");
    assert_eq!(face_woff.num_tables(), face_ttf.num_tables());

    for rec in face_ttf.records() {
        let a = face_ttf.table_bytes(rec.tag).expect("ttf table");
        let b = face_woff.table_bytes(rec.tag).expect("woff table");
        assert_eq!(a, b, "table {:?} differs after WOFF1 unwrap", rec.tag);
    }
}

#[test]
fn unwrapped_face_shapes_hello() {
    let sfnt = unwrap_woff1(WOFF1).expect("WOFF1 unwraps");
    let face = Face::parse_bytes(&sfnt, 0).expect("SFNT parses");
    let font = Font::new(face, 16.0);
    let mut buf = Buffer::new();
    buf.push_str("Hello");
    let shaped = shape(&font, &buf, &[]).expect("shape succeeds");
    assert_eq!(shaped.glyphs.len(), 5);
    for g in &shaped.glyphs {
        assert_ne!(g.glyph_id, 0, "every char should map to a non-notdef gid");
    }
}

#[test]
fn wrap_then_unwrap_is_lossless_for_table_bodies() {
    // Round-trip the reference TTF through the WOFF1 wrapper. We
    // don't byte-compare the resulting SFNT (the wrapper recomputes
    // search params + body offsets) but every table body must come
    // out identical.
    let woff = wrap_woff1(TTF).expect("TTF wraps");
    let unwrapped = unwrap_woff1(&woff).expect("re-unwraps");

    let face_in = Face::parse_bytes(TTF, 0).expect("TTF parses");
    let face_out = Face::parse_bytes(&unwrapped, 0).expect("re-unwrapped SFNT parses");
    assert_eq!(face_in.num_tables(), face_out.num_tables());
    for rec in face_in.records() {
        let a = face_in.table_bytes(rec.tag).unwrap();
        let b = face_out.table_bytes(rec.tag).unwrap();
        assert_eq!(a, b, "table {:?} differs after WOFF1 round-trip", rec.tag);
    }
}

#[test]
fn bad_signature_is_rejected() {
    let mut bytes = WOFF1.to_vec();
    bytes[0] = 0;
    assert!(unwrap_woff1(&bytes).is_err());
}
