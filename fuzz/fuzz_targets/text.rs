//! Runs arbitrary text through line breaking, word boundaries, line wrapping,
//! and hyphenation, and parses arbitrary text as a hyphenation pattern file.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sigilbuzz::Glyph;
use sigilbuzz_hyphen::{hyphenate, Language, Patterns};
use sigilbuzz_text_layout::{line_break_opportunities, word_breaks, wrap_lines, WrapOptions};

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else { return };
    let text = String::from_utf8_lossy(rest);

    let _ = line_break_opportunities(&text).count();
    let _ = word_breaks(&text).count();

    // One fake glyph per char, with clusters pointing at char starts, the
    // way the shaper reports them. The last few glyphs get odd clusters.
    let mut glyphs: Vec<Glyph> = text
        .char_indices()
        .map(|(i, _)| Glyph {
            glyph_id: 1,
            cluster: u32::try_from(i).unwrap_or(u32::MAX),
            x_advance: i32::from(mode % 7) * 100 - 50,
            y_advance: 0,
            x_offset: 0,
            y_offset: 0,
            unicode_props: 0,
            indic_position: 0,
            char_class: 0,
            combining_class: 0,
            syllable: 0,
            flags: sigilbuzz::GlyphFlags::empty(),
        })
        .collect();
    if mode & 0x80 != 0 {
        if let Some(last) = glyphs.last_mut() {
            last.cluster = u32::MAX;
        }
    }
    let options = WrapOptions {
        max_width: match mode % 4 {
            0 => 0.0,
            1 => f32::NAN,
            2 => -10.0,
            _ => f32::from(mode) * 10.0,
        },
        break_at_word_boundaries: mode & 1 == 0,
    };
    let _ = wrap_lines(&glyphs, &text, options);

    if let Some(patterns) = Patterns::for_language(Language::EnglishUs) {
        for word in text.split_whitespace().take(32) {
            let _ = hyphenate(word, patterns);
        }
    }
    if let Ok(custom) = Patterns::parse(&text) {
        for word in ["hyphenation", "a", "", "\u{00e9}t\u{00e9}", &text] {
            let _ = hyphenate(word, &custom);
        }
    }
});
