//! WOFF2 unwrap test: Brotli + glyf/loca inverse transform.
//!
//! The vendored `opensans_latin.woff2` is the same subset that's also
//! shipped as a plain TTF (`opensans_latin.ttf`); after Brotli decode
//! and the inverse `glyf` transform the *contour shapes* must match
//! the reference TTF byte-for-byte. We don't compare the entire glyf
//! table verbatim because the WOFF2 transform doesn't preserve flag
//! compression. Instead we walk every glyph through `ttf-parser`'s
//! outline visitor and compare the emitted `MoveTo` / `LineTo` /
//! `CurveTo` / `Close` callbacks.
//!
//! All tests in this file require the `woff2` cargo feature: the
//! `unwrap_woff2` / `wrap_woff2` symbols still exist with the feature
//! disabled, but they are stubs that return `WoffError::Woff2Disabled`,
//! which would fail every `expect`/`unwrap` here. Cfg-gating the whole
//! file keeps `cargo test --no-default-features` clean for WOFF1-only
//! consumers.

#![cfg(feature = "woff2")]

use sigilbuzz::{shape, Buffer, Face, Font};
use sigilbuzz_woff::{unwrap_woff2, wrap_woff2};

const WOFF2: &[u8] = include_bytes!("fixtures/opensans_latin.woff2");
const TTF: &[u8] = include_bytes!("fixtures/opensans_latin.ttf");

#[test]
fn unwrap_woff2_yields_parseable_sfnt() {
    let sfnt = unwrap_woff2(WOFF2).expect("WOFF2 unwraps");
    let face = Face::parse_bytes(&sfnt, 0).expect("SFNT parses");
    assert!(
        face.num_tables() >= 7,
        "should have all the core SFNT tables"
    );
    // glyf + loca must both come back.
    assert!(face.record(*b"glyf").is_some(), "glyf reconstructed");
    assert!(face.record(*b"loca").is_some(), "loca reconstructed");
}

#[test]
fn shaping_succeeds_on_unwrapped_woff2() {
    let sfnt = unwrap_woff2(WOFF2).expect("WOFF2 unwraps");
    let face = Face::parse_bytes(&sfnt, 0).expect("SFNT parses");
    let font = Font::new(face, 16.0);
    let mut buf = Buffer::new();
    buf.push_str("Hello");
    let shaped = shape(&font, &buf, &[]).expect("shape succeeds");
    assert_eq!(shaped.glyphs.len(), 5);
    for g in &shaped.glyphs {
        assert_ne!(g.glyph_id, 0);
    }
}

#[test]
fn glyph_outlines_match_reference_ttf() {
    use ttf_parser::{Face as TtfFace, OutlineBuilder};

    #[derive(Default, PartialEq, Eq, Debug)]
    struct Path(Vec<String>);
    impl OutlineBuilder for Path {
        fn move_to(&mut self, x: f32, y: f32) {
            self.0.push(format!("M {x} {y}"));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.0.push(format!("L {x} {y}"));
        }
        fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
            self.0.push(format!("Q {x1} {y1} {x} {y}"));
        }
        fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
            self.0.push(format!("C {x1} {y1} {x2} {y2} {x} {y}"));
        }
        fn close(&mut self) {
            self.0.push("Z".to_string());
        }
    }

    let sfnt = unwrap_woff2(WOFF2).expect("WOFF2 unwraps");
    let face_woff = TtfFace::parse(&sfnt, 0).expect("ttf-parser likes our SFNT");
    let face_ttf = TtfFace::parse(TTF, 0).expect("ttf-parser likes the reference");

    let n = face_ttf.number_of_glyphs();
    assert_eq!(n, face_woff.number_of_glyphs());

    let mut compared = 0;
    let mut nonempty = 0;
    for gid in 0..n {
        let mut a = Path::default();
        let mut b = Path::default();
        let r1 = face_ttf.outline_glyph(ttf_parser::GlyphId(gid), &mut a);
        let r2 = face_woff.outline_glyph(ttf_parser::GlyphId(gid), &mut b);
        assert_eq!(
            r1.is_some(),
            r2.is_some(),
            "glyph {gid} bbox presence differs"
        );
        assert_eq!(a, b, "glyph {gid} outline differs");
        compared += 1;
        if !a.0.is_empty() {
            nonempty += 1;
        }
    }
    assert!(compared > 0);
    assert!(nonempty > 10, "expected non-empty outlines, got {nonempty}");
}

#[test]
fn bad_signature_is_rejected() {
    let mut bytes = WOFF2.to_vec();
    bytes[0] = 0;
    assert!(unwrap_woff2(&bytes).is_err());
}

#[test]
fn wrap_then_unwrap_recovers_glyph_outlines() {
    // Forward direction: wrap the reference TTF, then unwrap and
    // walk every glyph through ttf-parser. The contour shapes must
    // match the reference TTF byte-for-byte (we don't compare the
    // entire glyf verbatim because the WOFF2 transform doesn't
    // preserve flag-byte compression, same caveat as the existing
    // unwrap-only test).
    use ttf_parser::{Face as TtfFace, OutlineBuilder};

    #[derive(Default, PartialEq, Eq, Debug)]
    struct Path(Vec<String>);
    impl OutlineBuilder for Path {
        fn move_to(&mut self, x: f32, y: f32) {
            self.0.push(format!("M {x} {y}"));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.0.push(format!("L {x} {y}"));
        }
        fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
            self.0.push(format!("Q {x1} {y1} {x} {y}"));
        }
        fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
            self.0.push(format!("C {x1} {y1} {x2} {y2} {x} {y}"));
        }
        fn close(&mut self) {
            self.0.push("Z".to_string());
        }
    }

    let wrapped = wrap_woff2(TTF).expect("wraps");
    // Compression must improve on the raw SFNT; otherwise the format
    // serves no purpose. The brief mandates `wrapped.len() < TTF.len()`.
    assert!(
        wrapped.len() < TTF.len(),
        "wrap should compress: {} >= {}",
        wrapped.len(),
        TTF.len()
    );

    let recovered = unwrap_woff2(&wrapped).expect("unwraps");
    let face_recovered = TtfFace::parse(&recovered, 0).expect("recovered SFNT parses");
    let face_ref = TtfFace::parse(TTF, 0).expect("reference SFNT parses");

    assert_eq!(
        face_ref.number_of_glyphs(),
        face_recovered.number_of_glyphs()
    );

    let n = face_ref.number_of_glyphs();
    for gid in 0..n {
        let mut a = Path::default();
        let mut b = Path::default();
        let r1 = face_ref.outline_glyph(ttf_parser::GlyphId(gid), &mut a);
        let r2 = face_recovered.outline_glyph(ttf_parser::GlyphId(gid), &mut b);
        assert_eq!(
            r1.is_some(),
            r2.is_some(),
            "glyph {gid} bbox presence differs after wrap+unwrap"
        );
        assert_eq!(a, b, "glyph {gid} outline differs after wrap+unwrap");
    }
}

#[test]
fn wrap_then_unwrap_yields_shapeable_face() {
    let wrapped = wrap_woff2(TTF).expect("wraps");
    let sfnt = unwrap_woff2(&wrapped).expect("unwraps");
    let face = Face::parse_bytes(&sfnt, 0).expect("parses");
    let font = Font::new(face, 16.0);
    let mut buf = Buffer::new();
    buf.push_str("Hello");
    let shaped = shape(&font, &buf, &[]).expect("shape succeeds");
    assert_eq!(shaped.glyphs.len(), 5);
    for g in &shaped.glyphs {
        assert_ne!(g.glyph_id, 0);
    }
}

#[test]
fn wrap_rejects_non_sfnt_input() {
    let bogus = [0u8; 16];
    assert!(wrap_woff2(&bogus).is_err());
}

#[test]
fn wrap_then_unwrap_byte_equivalence_report() {
    // Diagnostic: outline-equivalence is the main
    // assertion (see `wrap_then_unwrap_recovers_glyph_outlines`).
    // Byte-exact round-trip is *not* expected because the WOFF2
    // forward transform doesn't preserve simple-glyph flag-byte
    // compression: the unwrapper rebuilds flags without
    // REPEAT_FLAG runs, and therefore the recovered glyf is
    // typically *larger* than the input (see Google's reference
    // woff2 which behaves the same way). All other tables and
    // checksums match.
    let wrapped = wrap_woff2(TTF).expect("wraps");
    let recovered = unwrap_woff2(&wrapped).expect("unwraps");
    eprintln!(
        "byte-equivalence report: ttf={} recovered={} byte_equal={}",
        TTF.len(),
        recovered.len(),
        recovered == TTF,
    );
}

#[test]
fn wrap_compression_report() {
    // Not strictly an assertion-only test. Useful when comparing
    // against the bundled reference woff2. Run with
    // `cargo test --test woff2 wrap_compression_report -- --nocapture`.
    let wrapped = wrap_woff2(TTF).expect("wraps");
    let ttf_size = TTF.len();
    let wrapped_size = wrapped.len();
    let ref_w2_size = WOFF2.len();
    eprintln!(
        "wrap report: ttf={ttf_size} wrapped={wrapped_size} \
         ratio={:.2}%  reference_woff2={ref_w2_size} \
         reference_ratio={:.2}%",
        (wrapped_size as f64 / ttf_size as f64) * 100.0,
        (ref_w2_size as f64 / ttf_size as f64) * 100.0,
    );
    assert!(wrapped_size < ttf_size);
}
