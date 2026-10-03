//! Variable-font integration: Rubik Variable (`wght` axis) exercises
//! the full `fvar`, `avar`, `Font::with_coords`, `shape` pipeline, and
//! checks that the resulting per-glyph advances match HarfBuzz 14.5.0
//! at the same axis coordinate, and rustybuzz at the default instance.
//!
//! `tests/fixtures/rubik_variable_shaping.expected` holds HarfBuzz's
//! output; `tests/tools/variable_shaping_expected.py` regenerates it.
//! rustybuzz maps the unrounded `fvar` coordinate through `avar` and
//! rounds deltas half away from zero, so its varied advances can be a
//! unit off HarfBuzz's.

use rustybuzz::{Face as RbFace, UnicodeBuffer};
use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");
const EXPECTED: &str = include_str!("fixtures/rubik_variable_shaping.expected");

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

fn rustybuzz_default_advances(bytes: &[u8], text: &str) -> Vec<i32> {
    let face = RbFace::from_slice(bytes, 0).unwrap();
    let mut buf = UnicodeBuffer::new();
    buf.push_str(text);
    let out = rustybuzz::shape(&face, &[], buf);
    out.glyph_positions().iter().map(|p| p.x_advance).collect()
}

/// `(glyph_id, x_advance, y_advance, x_offset, y_offset)`.
type Pos = (u32, i32, i32, i32, i32);

/// HarfBuzz's glyphs for `text` at `wght`, from the expected file.
fn harfbuzz_positions(wght: f32, text: &str) -> Vec<Pos> {
    let cps: Vec<String> = text.chars().map(|c| format!("{:04X}", c as u32)).collect();
    let key = format!("advances {wght} ltr {}", cps.join(","));
    let line = EXPECTED
        .lines()
        .find(|l| {
            l.strip_prefix(&key)
                .is_some_and(|rest| rest.starts_with(' '))
        })
        .unwrap_or_else(|| panic!("no HarfBuzz record for {key}"));
    line[key.len()..]
        .split_whitespace()
        .map(|g| {
            let v: Vec<i64> = g.split(',').map(|n| n.parse().unwrap()).collect();
            (
                v[0] as u32,
                v[1] as i32,
                v[2] as i32,
                v[3] as i32,
                v[4] as i32,
            )
        })
        .collect()
}

fn sigilbuzz_positions(coords: &[f32], text: &str) -> Vec<Pos> {
    let blob = Blob::new(RUBIK);
    let face = Face::parse(&blob, 0).unwrap();
    let font = Font::new(face, 1000.0).with_coords(coords);
    let mut buf = Buffer::new();
    buf.push_str(text);
    let shaped = shape(&font, &buf, &[]).unwrap();
    shaped
        .glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.y_advance, g.x_offset, g.y_offset))
        .collect()
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
fn advances_match_harfbuzz_across_wght_axis() {
    // The coordinates go through `fvar` and `avar` the way HarfBuzz
    // takes them: rounded to 16.16 before `avar` and to F2DOT14 after.
    // At 700 the F2DOT14 rounding moves some advances by a unit, and
    // at 493.75 both roundings matter.
    let blob = Blob::new(RUBIK);
    let face = Face::parse(&blob, 0).unwrap();
    let texts = [
        "A",
        "Hello",
        "o",
        "Hello variable world",
        "AV To Yo",
        "\u{0413}\u{043E}",
    ];
    for &wght in &[350.0f32, 493.75, 500.0, 613.0, 700.0, 777.7, 900.0] {
        let coords = normalize_wght(&face, wght);
        for text in texts {
            assert_eq!(
                sigilbuzz_positions(&coords, text),
                harfbuzz_positions(wght, text),
                "wght={wght} text={text:?}"
            );
        }
    }
}
#[test]
fn default_instance_matches_rustybuzz_without_coords() {
    // When Font::with_coords is not called, sigilbuzz must produce
    // the same advances as rustybuzz with no variations set. Rubik
    // keeps its class kerning under the `latn` and `cyrl` scripts,
    // not DFLT, so the kerned pairs check that Latin and Cyrillic
    // text use those script tables, as in HarfBuzz.
    let corpus = ["A", "Hello", "o", "AV", "To", "Yo", "\u{0413}\u{043E}"];
    for text in &corpus {
        let sig = sigilbuzz_advances(RUBIK, &[], text);
        let rb = rustybuzz_default_advances(RUBIK, text);
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
