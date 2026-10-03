use super::*;
use crate::buffer::char_class;
use alloc::string::String;

/// Runs the constraints of `script` over `text`, one cluster per
/// character (its index), and returns the characters and clusters.
fn run(script: Script, flags: BufferFlags, text: &str) -> Vec<(char, u32)> {
    let mut cps: Vec<char> = text.chars().collect();
    let mut glyphs: Vec<Glyph> = (0..cps.len() as u32).map(|i| Glyph::new(0, i)).collect();
    let mut mirrored = alloc::vec![false; cps.len()];
    insert_dotted_circles(Some(script), flags, &mut cps, &mut glyphs, &mut mirrored);
    assert_eq!(cps.len(), glyphs.len());
    assert_eq!(cps.len(), mirrored.len());
    cps.into_iter()
        .zip(glyphs.iter().map(|g| g.cluster))
        .collect()
}

fn chars(script: Script, text: &str) -> String {
    run(script, BufferFlags::empty(), text)
        .into_iter()
        .map(|(c, _)| c)
        .collect()
}

#[test]
fn circle_goes_before_the_last_character() {
    // DEVANAGARI LETTER A, VOWEL SIGN AA.
    assert_eq!(
        run(Script::Devanagari, BufferFlags::empty(), "\u{0905}\u{093E}"),
        [('\u{0905}', 0), ('\u{25CC}', 1), ('\u{093E}', 1)]
    );
    // RA, VIRAMA, LETTER I.
    assert_eq!(
        chars(Script::Devanagari, "\u{0930}\u{094D}\u{0907}"),
        "\u{0930}\u{094D}\u{25CC}\u{0907}"
    );
}

#[test]
fn sequences_of_other_scripts_are_left_alone() {
    // The buffer's script picks the sequences: HarfBuzz switches on the
    // script in the buffer's `props`.
    assert_eq!(
        chars(Script::Bengali, "\u{0905}\u{093E}"),
        "\u{0905}\u{093E}"
    );
    assert_eq!(chars(Script::Latin, "\u{0905}\u{093E}"), "\u{0905}\u{093E}");
    assert_eq!(chars(Script::Khmer, "\u{0905}\u{093E}"), "\u{0905}\u{093E}");
    let mut cps: Vec<char> = "\u{0905}\u{093E}".chars().collect();
    let mut glyphs = alloc::vec![Glyph::new(0, 0); 2];
    let mut mirrored = alloc::vec![false; 2];
    insert_dotted_circles(
        None,
        BufferFlags::empty(),
        &mut cps,
        &mut glyphs,
        &mut mirrored,
    );
    assert_eq!(cps.len(), 2);
}

#[test]
fn flag_turns_the_circles_off() {
    let flags = BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE;
    assert_eq!(
        run(Script::Devanagari, flags, "\u{0905}\u{093E}"),
        [('\u{0905}', 0), ('\u{093E}', 1)]
    );
}

#[test]
fn scan_resumes_after_a_match() {
    // A, AA, AA: the second AA follows the first match and gets none.
    assert_eq!(
        chars(Script::Devanagari, "\u{0905}\u{093E}\u{093E}"),
        "\u{0905}\u{25CC}\u{093E}\u{093E}"
    );
    // Gujarati CANDRA E, AA starts a sequence of its own, but A,
    // CANDRA E took the CANDRA E first.
    assert_eq!(
        chars(Script::Gujarati, "\u{0A85}\u{0AC5}\u{0ABE}"),
        "\u{0A85}\u{25CC}\u{0AC5}\u{0ABE}"
    );
    assert_eq!(
        chars(Script::Gujarati, "\u{0AC5}\u{0ABE}"),
        "\u{0AC5}\u{25CC}\u{0ABE}"
    );
    // Two sequences in a row.
    assert_eq!(
        chars(Script::Devanagari, "\u{0905}\u{093E}\u{0906}\u{0947}"),
        "\u{0905}\u{25CC}\u{093E}\u{0906}\u{25CC}\u{0947}"
    );
}

#[test]
fn sequences_must_fit_in_the_text() {
    assert_eq!(
        chars(Script::Devanagari, "\u{0930}\u{094D}"),
        "\u{0930}\u{094D}"
    );
    assert_eq!(chars(Script::Devanagari, "\u{0905}"), "\u{0905}");
    assert_eq!(chars(Script::Devanagari, ""), "");
    // A joiner between the two breaks the sequence.
    assert_eq!(
        chars(Script::Devanagari, "\u{0905}\u{200D}\u{093E}"),
        "\u{0905}\u{200D}\u{093E}"
    );
}

#[test]
fn circle_copies_the_following_character() {
    let mut cps: Vec<char> = "\u{0905}\u{0945}".chars().collect();
    let mut first = Glyph::new(0, 0);
    first.flags = crate::buffer::GlyphFlags::UNSAFE_TO_BREAK;
    let mut glyphs = alloc::vec![first, Glyph::new(0, 3)];
    glyphs[1].flags = crate::buffer::GlyphFlags::UNSAFE_TO_CONCAT;
    let mut mirrored = alloc::vec![false, true];
    let script = Some(Script::Devanagari);
    insert_dotted_circles(
        script,
        BufferFlags::empty(),
        &mut cps,
        &mut glyphs,
        &mut mirrored,
    );
    assert_eq!(cps, ['\u{0905}', '\u{25CC}', '\u{0945}']);
    let circle = glyphs[1];
    assert_eq!(circle.cluster, 3);
    assert_eq!(circle.flags, crate::buffer::GlyphFlags::UNSAFE_TO_CONCAT);
    // U+0945 DEVANAGARI VOWEL SIGN CANDRA E is a nonspacing mark.
    assert_eq!(
        circle.char_class,
        char_class::MARK | char_class::NONSPACING_MARK
    );
    assert_eq!(mirrored, [false, false, true]);
    // Before a letter (RA, VIRAMA, I) the circle has no mark props.
    let mut cps: Vec<char> = "\u{0930}\u{094D}\u{0907}".chars().collect();
    let mut glyphs = alloc::vec![Glyph::new(0, 0); 3];
    let mut mirrored = alloc::vec![false; 3];
    insert_dotted_circles(
        script,
        BufferFlags::empty(),
        &mut cps,
        &mut glyphs,
        &mut mirrored,
    );
    assert_eq!(glyphs[2].char_class, 0);
}

#[test]
fn telugu_length_mark_keeps_its_combining_class() {
    // U+0C55 TELUGU LENGTH MARK has combining class 84, which HarfBuzz
    // stores as its modified class 4. The circle takes it too.
    let mut cps: Vec<char> = "\u{0C12}\u{0C55}".chars().collect();
    let mut glyphs = alloc::vec![Glyph::new(0, 0); 2];
    let mut mirrored = alloc::vec![false; 2];
    let script = Some(Script::Telugu);
    insert_dotted_circles(
        script,
        BufferFlags::empty(),
        &mut cps,
        &mut glyphs,
        &mut mirrored,
    );
    assert_eq!(cps, ['\u{0C12}', '\u{25CC}', '\u{0C55}']);
    assert_eq!(glyphs[1].combining_class, 4);
}

#[test]
fn every_script_with_sequences_is_reachable() {
    for script in [
        Script::Devanagari,
        Script::Bengali,
        Script::Gurmukhi,
        Script::Gujarati,
        Script::Oriya,
        Script::Tamil,
        Script::Telugu,
        Script::Kannada,
        Script::Malayalam,
        Script::Sinhala,
        Script::Brahmi,
        Script::Khojki,
        Script::Tirhuta,
        Script::Modi,
        Script::Khudawadi,
        Script::Takri,
    ] {
        assert!(!sequences(script).is_empty(), "{script:?}");
    }
    assert!(sequences(Script::Khmer).is_empty());
    assert!(sequences(Script::Myanmar).is_empty());
}

#[test]
fn long_runs_stay_linear() {
    // Every pair matches, and the scan still makes one pass.
    let text: String = "\u{0905}\u{093E}".repeat(20_000);
    let out = chars(Script::Devanagari, &text);
    assert_eq!(out.chars().count(), 60_000);
}
