//! Variable-font integration: Rubik Variable (`wght` axis) exercises
//! the full fvar -> avar -> Font::with_coords -> shape() pipeline, and
//! checks that the resulting per-glyph advances match rustybuzz with
//! the same axis coordinate.

use rustybuzz::ttf_parser::Tag;
use rustybuzz::{Face as RbFace, UnicodeBuffer, Variation};
use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");

fn normalize_wght(face: &Face<'_>, user_value: f32) -> Vec<f32> {
    let fvar = face.fvar().unwrap().expect("rubik has fvar");
    let avar = face.avar().unwrap();
    let normalized = fvar.normalize_coords(&[user_value]);
    match avar {
        Some(a) => a.remap_all(&normalized),
        None => normalized,
    }
}

fn sigilbuzz_advances(bytes: &[u8], coords: &[f32], text: &str) -> Vec<i32> {
    let blob = Blob::new(bytes);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0).with_coords(coords);
    let mut buf = Buffer::new();
    buf.push_str(text);
    let shaped = shape(&font, &buf, &[]).unwrap();
    shaped.glyphs.iter().map(|g| g.x_advance).collect()
}

fn rustybuzz_advances(bytes: &[u8], user_wght: f32, text: &str) -> Vec<i32> {
    let mut face = RbFace::from_slice(bytes, 0).unwrap();
    if user_wght > 0.0 {
        face.set_variations(&[Variation {
            tag: Tag::from_bytes(b"wght"),
            value: user_wght,
        }]);
    }
    let mut buf = UnicodeBuffer::new();
    buf.push_str(text);
    let out = rustybuzz::shape(&face, &[], buf);
    out.glyph_positions().iter().map(|p| p.x_advance).collect()
}

#[test]
fn font_coords_slice_is_preserved() {
    let blob = Blob::new(RUBIK);
    let face = Face::parse(&blob, 0).unwrap();
    let coords = normalize_wght(&face, 700.0);
    let font = Font::new(face, 1000.0).with_coords(&coords);
    assert_eq!(font.coords().len(), 1);
    // Rubik's wght axis spans (min=300, default=300, max=900) and
    // the fvar normalization gives (700-300)/(900-300) ~= 0.667 for
    // the above-default case. avar may then remap that value; both
    // rubik and most variable fonts keep it strictly positive.
    assert!(font.coords()[0] > 0.0);
    assert!(font.coords()[0] <= 1.0);
}

#[test]
fn heavy_weight_differs_from_default_weight_at_least_once() {
    // Proof-of-fire: without HVAR wiring, both of these advance lists
    // are identical. The wiring test below then compares us to
    // rustybuzz. This test just makes sure the integration fixture
    // actually exercises HVAR.
    let blob = Blob::new(RUBIK);
    let face = Face::parse(&blob, 0).unwrap();
    let default = sigilbuzz_advances(RUBIK, &[], "Hello variable world");
    let heavy_coords = normalize_wght(&face, 900.0);
    let heavy = sigilbuzz_advances(RUBIK, &heavy_coords, "Hello variable world");
    assert_eq!(default.len(), heavy.len());
    let any_diff = default.iter().zip(heavy.iter()).any(|(a, b)| a != b);
    assert!(
        any_diff,
        "expected at least one advance to differ between weight=300 and weight=900"
    );
}

#[test]
fn advance_deltas_match_rustybuzz_across_wght_axis() {
    // Compare (heavy - default) advance deltas between sigilbuzz and
    // rustybuzz. This isolates the HVAR contribution from any
    // pre-existing GPOS differences between the two shapers, which
    // is all the variable-font wiring is responsible for.
    let blob = Blob::new(RUBIK);
    let face = Face::parse(&blob, 0).unwrap();

    // Corpus chosen to avoid GPOS pair adjustments that rustybuzz
    // varies with weight via feature-variations. That layer is
    // orthogonal to HVAR advance deltas. With un-kerning-varying
    // glyphs, all measured divergence between the two shapers is
    // HVAR drift alone.
    for &wght in &[500.0f32, 700.0, 900.0] {
        let coords = normalize_wght(&face, wght);
        for text in &["A", "Hello", "o"] {
            let sig_default = sigilbuzz_advances(RUBIK, &[], text);
            let sig_heavy = sigilbuzz_advances(RUBIK, &coords, text);
            let rb_default = rustybuzz_advances(RUBIK, 0.0, text);
            let rb_heavy = rustybuzz_advances(RUBIK, wght, text);
            assert_eq!(sig_default.len(), sig_heavy.len());
            assert_eq!(rb_default.len(), rb_heavy.len());
            assert_eq!(sig_default.len(), rb_default.len());
            for i in 0..sig_default.len() {
                let sig_delta = sig_heavy[i] - sig_default[i];
                let rb_delta = rb_heavy[i] - rb_default[i];
                // Tolerance of 1 design unit: sigilbuzz uses f32
                // math, rustybuzz fixed-point f2dot14. Drift stays at
                // or under 1 unit on this corpus.
                assert!(
                    (sig_delta - rb_delta).abs() <= 1,
                    "HVAR delta diverged at wght={wght} text={text:?} pos={i}: \
                     sigilbuzz_delta={sig_delta} rustybuzz_delta={rb_delta}"
                );
            }
        }
    }
}

#[test]
fn default_instance_matches_rustybuzz_without_coords() {
    // When Font::with_coords is not called, sigilbuzz must produce
    // the same advances as rustybuzz with no variations set. Corpus
    // avoids kerning pairs rubik renders differently in
    // the two engines (pre-existing shaper divergence that the
    // parity test in rustybuzz_parity.rs is the right home for).
    let corpus = ["A", "Hello", "o"];
    for text in &corpus {
        let sig = sigilbuzz_advances(RUBIK, &[], text);
        let rb = rustybuzz_advances(RUBIK, 0.0, text);
        assert_eq!(sig, rb, "default-instance advance mismatch for {text:?}");
    }
}

#[test]
fn rubik_mvar_resolves_underline_offset_delta() {
    // Rubik VF ships MVAR with a single `undo` record (underline
    // position). Parsing the table at heavy weight should produce
    // a non-zero delta. The heavier instance positions the
    // underline differently. We assert "non-default differs from
    // default" rather than a fixed number to stay future-proof
    // against re-mastering of the fixture.
    let blob = Blob::new(RUBIK);
    let face = Face::parse(&blob, 0).unwrap();
    let mvar = face.mvar().unwrap().expect("rubik ships MVAR");
    assert!(!mvar.is_empty());
    let entries: Vec<_> = mvar.entries().collect();
    assert!(
        entries.iter().any(|(t, _)| t == b"undo"),
        "rubik MVAR should carry the `undo` (underline offset) record, got {entries:?}"
    );

    // Resolving `undo` at coord 0 gives the default-instance delta
    // (zero) and at heavy weight gives a non-zero value.
    let zero = mvar.metric_delta(*b"undo", &[0.0]).unwrap();
    assert!(zero.abs() < 1e-3, "default coord should yield zero delta");

    let heavy_coords = normalize_wght(&face, 900.0);
    let heavy = mvar
        .metric_delta(*b"undo", &heavy_coords)
        .expect("undo record present");
    assert!(
        heavy.abs() > 0.0,
        "heavy weight should shift underline offset, got {heavy}"
    );

    // Unrecognized tags resolve to None.
    assert!(mvar.metric_delta(*b"xxxx", &heavy_coords).is_none());
}

#[test]
fn source_sans_3_vf_carries_no_mvar_or_vvar() {
    // Source Sans 3 VF (the OTF subset vendored at
    // tests/fonts/SourceSans3VF-Latin-Subset.otf) ships HVAR but
    // omits MVAR and VVAR, typical of horizontal-only Latin
    // variable fonts. The accessors must return Ok(None) for both,
    // never an error: Face::table_bytes' MissingTable path is
    // mapped to None by the optional accessor convention.
    const SS3: &[u8] = include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf");
    let blob = Blob::new(SS3);
    let face = Face::parse(&blob, 0).unwrap();
    assert!(face.mvar().unwrap().is_none());
    assert!(face.vvar().unwrap().is_none());
    // Sanity: HVAR is present, so the optional-table machinery is
    // working. This rules out a parse error masking as None.
    assert!(face.hvar().unwrap().is_some());
}

#[test]
fn glyph_bounds_at_coords_shifts_bbox() {
    // For a weight-varying font, 'A' at weight=900 should have a
    // wider bbox (larger x_max - x_min) than at weight=300 because
    // heavier strokes extend outward. Exact delta depends on the
    // font; we assert inequality rather than a specific value.
    let blob = Blob::new(RUBIK);
    let face = Face::parse(&blob, 0).unwrap();
    let cmap = face.cmap().unwrap();
    let gid = cmap.glyph_id('A').unwrap();

    let default_bounds = face
        .glyph_bounds_at_coords(gid, &[])
        .unwrap()
        .expect("A has bounds");

    let heavy_coords = normalize_wght(&face, 900.0);
    let heavy_bounds = face
        .glyph_bounds_at_coords(gid, &heavy_coords)
        .unwrap()
        .expect("A has bounds at wght=900");

    let default_w = i32::from(default_bounds.x_max) - i32::from(default_bounds.x_min);
    let heavy_w = i32::from(heavy_bounds.x_max) - i32::from(heavy_bounds.x_min);
    // The heavy weight should at minimum differ from the default:
    // whether wider or merely shifted depends on the font. The key
    // invariant: gvar deltas should actually change something.
    let differs = default_bounds != heavy_bounds;
    assert!(
        differs,
        "expected gvar to shift A's bbox between default and heavy weight (default={default_bounds:?}, heavy={heavy_bounds:?}, default_w={default_w}, heavy_w={heavy_w})"
    );
}
