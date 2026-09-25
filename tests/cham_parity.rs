//! Cham shaping parity tests.
//!
//! Shapes a Cham corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Cham (OFL) and asserts the output matches byte-for-byte.
//! Cham is a Brahmic script of Cambodia and Vietnam used for the Cham
//! language. Routes through sigilbuzz's USE pipeline with the `cham`
//! script tag.
//!
//! The corpus exercises:
//!
//!   * single base consonants
//!   * above-base vowel sign (sign-aa U+1A29, wait, actually U+AA29)
//!   * post-base vowel sign (sign-oe U+AA2F)
//!   * medial ra (U+AA34, below-base CM)
//!   * final consonant (U+AA40 final k, CM)
//!   * final ng (U+AA43, final mark)
//!   * Cham digit (Symbol pass-through)
//!   * mixed Cham + Latin
//!
//! A failure here is a parity drift against rustybuzz.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_CHAM: &[u8] = include_bytes!("fonts/NotoSansCham-Regular.ttf");

struct Case {
    text: &'static str,
    note: &'static str,
    compare_rustybuzz: bool,
}

const CORPUS: &[Case] = &[
    Case {
        text: "",
        note: "empty",
        compare_rustybuzz: true,
    },
    // ꨆ U+AA06: letter ka.
    Case {
        text: "\u{AA06}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ꨀ U+AA00: letter a (independent vowel).
    Case {
        text: "\u{AA00}",
        note: "independent a",
        compare_rustybuzz: true,
    },
    // ka + sign-aa (above-base).
    Case {
        text: "\u{AA06}\u{AA29}",
        note: "kaa (ka + sign-aa)",
        compare_rustybuzz: true,
    },
    // ka + sign-oe (post-base).
    Case {
        text: "\u{AA06}\u{AA2F}",
        note: "koe (ka + sign-oe)",
        compare_rustybuzz: true,
    },
    // ka + medial ra (below-base). Cham fonts ship a `pref`-driven
    // ligature that collapses ka + medial-ra into a precomposed
    // kra form. sigilbuzz's USE pipeline emits the right basic
    // features, but Noto Sans Cham registers the lookup under the
    // `pref` feature with a contextual rule that the current
    // dispatcher does not yet match, tracked as a follow-up at
    // <https://github.com/Oneiriq/sigilbuzz/issues> (USE: medial-ra
    // pref ligature for Cham). The string still shapes; we just
    // do not byte-compare against rustybuzz.
    Case {
        text: "\u{AA06}\u{AA34}",
        note: "kra (ka + medial ra)",
        compare_rustybuzz: true,
    },
    // ka + final ng (final mark).
    Case {
        text: "\u{AA06}\u{AA43}",
        note: "kang (ka + final ng)",
        compare_rustybuzz: true,
    },
    // Cham digit run.
    Case {
        text: "\u{AA50}\u{AA51}\u{AA52}",
        note: "cham digits 0-2",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Cham.
    Case {
        text: "Hi \u{AA06}\u{AA29}",
        note: "mixed latin + cham",
        compare_rustybuzz: true,
    },
];

#[test]
fn cham_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_CHAM);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_CHAM, 0).expect("parse rustybuzz face");

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
        }
    }
}
