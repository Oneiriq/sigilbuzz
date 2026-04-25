//! End-to-end WOFF1 round-trip on a vendored Latin face.
//!
//! Confirms that `unwrap_woff1` accepts a real-world uncompressed
//! WOFF1 envelope and that the SFNT it produces parses with the
//! sigilbuzz core and shapes a non-empty buffer. The
//! `woff1-deflate`-gated tests further exercise the zlib path:
//! synthetic compressed-table fixtures decompress correctly,
//! malformed streams are rejected, and a wrap+unwrap round-trip
//! through the deflate-enabled wrapper recovers every table body
//! byte-for-byte.

use sigilbuzz::{shape, Buffer, Face, Font};
use sigilbuzz_woff::{unwrap_woff1, wrap_woff1};

#[cfg(feature = "woff1-deflate")]
use sigilbuzz_woff::{wrap_woff1_with_options, WrapWoff1Options};

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

#[cfg(feature = "woff1-deflate")]
#[test]
fn wrap_with_deflate_actually_compresses_the_envelope() {
    // The reference uncompressed WOFF1 is essentially the TTF in a
    // 44-byte header + 20-byte-per-table directory. The deflate
    // wrap should land well under both the raw TTF and the
    // uncompressed WOFF1.
    let wrapped =
        wrap_woff1_with_options(TTF, WrapWoff1Options::default()).expect("wraps with deflate");
    assert!(
        wrapped.len() < TTF.len(),
        "deflate wrap should be smaller than raw TTF: {} >= {}",
        wrapped.len(),
        TTF.len()
    );
    assert!(
        wrapped.len() < WOFF1.len(),
        "deflate wrap should be smaller than uncompressed WOFF1: {} >= {}",
        wrapped.len(),
        WOFF1.len()
    );
    eprintln!(
        "deflate wrap report: ttf={} uncompressed_woff={} deflated_woff={} \
         deflate_ratio={:.2}%",
        TTF.len(),
        WOFF1.len(),
        wrapped.len(),
        (wrapped.len() as f64 / TTF.len() as f64) * 100.0,
    );
}

#[cfg(feature = "woff1-deflate")]
#[test]
fn deflate_wrap_then_unwrap_recovers_every_table_body() {
    let wrapped = wrap_woff1_with_options(TTF, WrapWoff1Options { deflate_quality: 9 })
        .expect("wraps with deflate");
    let recovered = unwrap_woff1(&wrapped).expect("unwraps deflated WOFF1");

    let face_in = Face::parse_bytes(TTF, 0).expect("TTF parses");
    let face_out = Face::parse_bytes(&recovered, 0).expect("recovered SFNT parses");
    assert_eq!(face_in.num_tables(), face_out.num_tables());
    for rec in face_in.records() {
        let a = face_in.table_bytes(rec.tag).unwrap();
        let b = face_out.table_bytes(rec.tag).unwrap();
        assert_eq!(
            a,
            b,
            "table {:?} differs after deflate-WOFF1 round-trip",
            rec.tag
        );
    }
}

#[cfg(feature = "woff1-deflate")]
#[test]
fn deflate_wrapped_face_shapes_hello_after_unwrap() {
    let wrapped =
        wrap_woff1_with_options(TTF, WrapWoff1Options::default()).expect("wraps with deflate");
    let sfnt = unwrap_woff1(&wrapped).expect("unwraps deflated WOFF1");
    let face = Face::parse_bytes(&sfnt, 0).expect("recovered SFNT parses");
    let font = Font::new(face, 16.0);
    let mut buf = Buffer::new();
    buf.push_str("Hello");
    let shaped = shape(&font, &buf, &[]).expect("shape succeeds");
    assert_eq!(shaped.glyphs.len(), 5);
    for g in &shaped.glyphs {
        assert_ne!(g.glyph_id, 0);
    }
}

#[cfg(feature = "woff1-deflate")]
#[test]
fn deflate_quality_zero_still_round_trips() {
    // Level 0 is store-only — the deflate stream just wraps the
    // input in `BTYPE=00` blocks. A reasonable lower bound to
    // exercise.
    let wrapped = wrap_woff1_with_options(TTF, WrapWoff1Options { deflate_quality: 0 })
        .expect("wraps at quality 0");
    let recovered = unwrap_woff1(&wrapped).expect("unwraps");
    let face_in = Face::parse_bytes(TTF, 0).unwrap();
    let face_out = Face::parse_bytes(&recovered, 0).unwrap();
    for rec in face_in.records() {
        assert_eq!(
            face_in.table_bytes(rec.tag),
            face_out.table_bytes(rec.tag),
        );
    }
}

#[cfg(feature = "woff1-deflate")]
#[test]
fn unwrap_rejects_corrupted_zlib_stream_in_table_body() {
    // Wrap, then flip a byte deep inside the deflate payload of the
    // first compressed table. Decompression must fail.
    let mut wrapped =
        wrap_woff1_with_options(TTF, WrapWoff1Options::default()).expect("wraps");
    // Find the first directory entry whose compLength < origLength
    // and corrupt a byte inside its body.
    let num_tables = u16::from_be_bytes([wrapped[12], wrapped[13]]) as usize;
    let mut corrupted = false;
    for i in 0..num_tables {
        let rec = 44 + 20 * i;
        let offset = u32::from_be_bytes(wrapped[rec + 4..rec + 8].try_into().unwrap()) as usize;
        let comp = u32::from_be_bytes(wrapped[rec + 8..rec + 12].try_into().unwrap()) as usize;
        let orig = u32::from_be_bytes(wrapped[rec + 12..rec + 16].try_into().unwrap()) as usize;
        if comp < orig && comp > 8 {
            // Flip a byte well past the zlib header — guaranteed to
            // sit inside the deflate payload.
            wrapped[offset + 4] ^= 0xFF;
            corrupted = true;
            break;
        }
    }
    assert!(corrupted, "expected at least one compressed table");
    let err = unwrap_woff1(&wrapped).unwrap_err();
    eprintln!("corruption rejected with: {err}");
}

#[cfg(not(feature = "woff1-deflate"))]
#[test]
fn unwrap_rejects_compressed_tables_when_feature_disabled() {
    // Hand-build a tiny compressed-table WOFF1 by re-using the
    // deflate-enabled crate from another build wouldn't make sense
    // here; instead, take the uncompressed reference and rewrite the
    // first directory entry's compLength to be smaller than origLength.
    // The unwrapper should refuse it with `Unsupported`.
    let mut bytes = WOFF1.to_vec();
    let rec = 44; // first directory entry
    // origLength stays as-is; shrink compLength by 1.
    let orig = u32::from_be_bytes(bytes[rec + 12..rec + 16].try_into().unwrap());
    if orig > 1 {
        let smaller = orig - 1;
        bytes[rec + 8..rec + 12].copy_from_slice(&smaller.to_be_bytes());
    }
    // We don't care about decompressing — only about hitting the
    // "feature disabled" branch.
    assert!(unwrap_woff1(&bytes).is_err());
}
