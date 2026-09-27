//! GSUB applies ligature and multiple substitutions in time linear in
//! the length of the run, through HarfBuzz's output-buffer model.
//!
//! Editing the glyph vector in place moved the whole tail on every
//! substitution. A run of 40,000 ligatures then took tens of seconds
//! in a debug build (8 seconds for 20,000, and four times that for
//! twice as many). Each test here shapes such a run and compares the
//! time with shaping a run of the same length that substitutes
//! nothing, with room to spare for slow machines.

use std::time::{Duration, Instant};

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const OPEN_SANS: &[u8] = include_bytes!("fixtures/opensans_regular.ttf");
const RUBIK: &[u8] = include_bytes!("fixtures/rubik_vf.ttf");

/// Number of repeats of each trigger.
const N: usize = 40_000;

/// Shapes `text` with `font`, returning the glyph count and the time.
fn timed(font: &[u8], text: &str) -> (usize, Duration) {
    let blob = Blob::new(font);
    let face = Face::parse(&blob, 0).expect("face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    let start = Instant::now();
    let glyphs = shape(&font, &buffer, &[]).expect("shape").len();
    (glyphs, start.elapsed())
}

/// Asserts the substituting run shaped within twenty times the plain
/// run's time plus two seconds.
fn assert_linear(label: &str, substituted: Duration, plain: Duration) {
    let budget = plain * 20 + Duration::from_secs(2);
    assert!(
        substituted < budget,
        "{label}: {substituted:?} for {N} substitutions, budget {budget:?}"
    );
}

#[test]
fn a_long_run_of_ligatures_shapes_in_linear_time() {
    // Open Sans ligates "fi" in `liga`.
    let (glyphs, substituted) = timed(OPEN_SANS, &"fi".repeat(N));
    assert_eq!(glyphs, N, "every pair ligates");
    let (_, plain) = timed(OPEN_SANS, &"ab".repeat(N));
    assert_linear("Open Sans fi", substituted, plain);
}

#[test]
fn a_long_run_of_multiple_substitutions_shapes_in_linear_time() {
    // Rubik's `ccmp` splits U+FB01 into f and i, and `liga` ligates
    // them again: one multiple substitution and one ligature each.
    let (glyphs, substituted) = timed(RUBIK, &"\u{FB01}".repeat(N));
    assert_eq!(glyphs, N);
    let (_, plain) = timed(RUBIK, &"a".repeat(N));
    assert_linear("Rubik U+FB01", substituted, plain);
}
