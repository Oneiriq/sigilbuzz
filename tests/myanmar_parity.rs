//! Myanmar shaping parity tests.
//!
//! Shapes a Myanmar corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Myanmar (OFL) and asserts the output matches byte-for-
//! byte. Myanmar runs through sigilbuzz's Myanmar pass
//! (`src/ot/myanmar`) with the Myanmar script tags (`mym2`, `mymr`,
//! then `DFLT`) and the Myanmar basic features (`rphf`, `pref`,
//! `blwf`, and `pstf`).
//!
//! The corpus exercises:
//!
//!   * plain consonants (one syllable, no reorder)
//!   * post-base vowel signs (aa)
//!   * pre-base vowel sign (sign e, the only pre-base matra in
//!     Myanmar, U+1031)
//!   * medial consonants (medial ya, ra, wa, ha)
//!   * virama-linked subjoining consonant
//!   * independent vowels
//!   * final marks (anusvara, dot below, visarga)
//!   * Myanmar digits (Symbol pass-through)
//!   * mixed Myanmar + Latin runs
//!
//! A failure here is a parity drift against rustybuzz; fix in
//! src/ot/myanmar.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_MYANMAR: &[u8] = include_bytes!("fonts/NotoSansMyanmar-Regular.ttf");

struct Case {
    text: &'static str,
    note: &'static str,
    /// When a known rustybuzz / sigilbuzz divergence is tracked as a
    /// follow-up issue, set this to `false` so the test still shapes
    /// the string (to catch panics and crashes) but does not compare
    /// glyph ids against rustybuzz.
    compare_rustybuzz: bool,
}

const CORPUS: &[Case] = &[
    // Empty run: identity.
    Case {
        text: "",
        note: "empty",
        compare_rustybuzz: true,
    },
    // က: ka alone.
    Case {
        text: "\u{1000}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ကာ: ka + sign aa. Post-base matra, no reorder.
    Case {
        text: "\u{1000}\u{102C}",
        note: "kaa (post-base aa)",
        compare_rustybuzz: true,
    },
    // ကေ: ka + sign e. Pre-base matra: USE reorder moves sign-e
    // before ka visually.
    Case {
        text: "\u{1000}\u{1031}",
        note: "ke (pre-base sign-e)",
        compare_rustybuzz: true,
    },
    // ကိ: ka + sign i. Above-base matra.
    Case {
        text: "\u{1000}\u{102D}",
        note: "ki (above-base i)",
        compare_rustybuzz: true,
    },
    // ကု: ka + sign u. Below-base matra.
    Case {
        text: "\u{1000}\u{102F}",
        note: "ku (below-base u)",
        compare_rustybuzz: true,
    },
    // ကံ: ka + anusvara. Final mark.
    Case {
        text: "\u{1000}\u{1036}",
        note: "kang (ka + anusvara)",
        compare_rustybuzz: true,
    },
    // ကျ: ka + medial ya (U+103B). Consonant modifier.
    Case {
        text: "\u{1000}\u{103B}",
        note: "ka + medial ya",
        compare_rustybuzz: true,
    },
    // ကြ: ka + medial ra (U+103C). Pre-base medial in Myanmar.
    Case {
        text: "\u{1000}\u{103C}",
        note: "ka + medial ra",
        compare_rustybuzz: true,
    },
    // ကွ: ka + medial wa (U+103D).
    Case {
        text: "\u{1000}\u{103D}",
        note: "ka + medial wa",
        compare_rustybuzz: true,
    },
    // ကှ: ka + medial ha (U+103E).
    Case {
        text: "\u{1000}\u{103E}",
        note: "ka + medial ha",
        compare_rustybuzz: true,
    },
    // က္က: ka + virama + ka (stacked subscript).
    Case {
        text: "\u{1000}\u{1039}\u{1000}",
        note: "ka + virama + ka (subscript)",
        compare_rustybuzz: true,
    },
    // အ: independent vowel a.
    Case {
        text: "\u{1021}",
        note: "independent vowel a",
        compare_rustybuzz: true,
    },
    // ဣ: independent vowel i (U+1023).
    Case {
        text: "\u{1023}",
        note: "independent vowel i",
        compare_rustybuzz: true,
    },
    // မင်္ဂလာပါ: "Hello" (mingalaba). Exercises the kinzi prefix
    // (`nga + asat + virama`) which sigilbuzz's Myanmar reorder
    // moves to POS_AFTER_MAIN, immediately after the base
    // consonant. Once the triple sits after the base, `rphf`
    // collapses it to the font's kinzi glyph in the reph slot,
    // matching rustybuzz glyph-for-glyph.
    Case {
        text: "\u{1019}\u{1004}\u{103A}\u{1039}\u{1002}\u{101C}\u{102C}\u{1015}\u{102B}",
        note: "mingalaba (hello): kinzi reorder",
        compare_rustybuzz: true,
    },
    // Myanmar digits 0-4.
    Case {
        text: "\u{1040}\u{1041}\u{1042}\u{1043}\u{1044}",
        note: "myanmar digits 0-4",
        compare_rustybuzz: true,
    },
    // Mixed Myanmar + Latin.
    Case {
        text: "Hi \u{1000}",
        note: "mixed latin + myanmar",
        compare_rustybuzz: true,
    },
    // Myanmar Extended-A: shan letter kha (U+AA60).
    Case {
        text: "\u{AA60}",
        note: "shan letter kha (Extended-A)",
        compare_rustybuzz: true,
    },
    // Myanmar Extended-B: shan digit 0 (U+A9F0).
    Case {
        text: "\u{A9F0}",
        note: "shan digit 0 (Extended-B)",
        compare_rustybuzz: true,
    },
];

#[test]
fn myanmar_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_MYANMAR);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_MYANMAR, 0).expect("parse rustybuzz face");

    for case in CORPUS {
        let mut buffer = Buffer::new();
        buffer.push_str(case.text);
        let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

        if !case.compare_rustybuzz {
            continue;
        }

        let mut rb_buf = rustybuzz::UnicodeBuffer::new();
        rb_buf.push_str(case.text);
        let rb_out = rustybuzz::shape(&rb_face, &[], rb_buf);
        let rb_infos = rb_out.glyph_infos();
        let rb_positions = rb_out.glyph_positions();

        assert_eq!(
            sig.len(),
            rb_infos.len(),
            "glyph count diverged for {} ({:?}): sigilbuzz={} rustybuzz={}",
            case.note,
            case.text,
            sig.len(),
            rb_infos.len()
        );

        for (i, (sig_g, (rb_info, rb_pos))) in sig
            .glyphs
            .iter()
            .zip(rb_infos.iter().zip(rb_positions.iter()))
            .enumerate()
        {
            assert_eq!(
                sig_g.glyph_id, rb_info.glyph_id,
                "glyph id mismatch at position {i} of {} ({:?}): sigilbuzz={} rustybuzz={}",
                case.note, case.text, sig_g.glyph_id, rb_info.glyph_id
            );
            assert_eq!(
                sig_g.x_advance, rb_pos.x_advance,
                "x_advance mismatch at position {i} of {} ({:?}): sigilbuzz={} rustybuzz={}",
                case.note, case.text, sig_g.x_advance, rb_pos.x_advance
            );
            assert_eq!(
                (sig_g.y_advance, sig_g.x_offset, sig_g.y_offset),
                (rb_pos.y_advance, rb_pos.x_offset, rb_pos.y_offset),
                "(y_advance, x_offset, y_offset) mismatch at position {i} of {} ({:?})",
                case.note,
                case.text
            );
        }
    }
}
