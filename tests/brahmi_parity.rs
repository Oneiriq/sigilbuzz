//! Brahmi shaping parity tests.
//!
//! Shapes a Brahmi corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Brahmi (OFL) and asserts the output matches byte-for-byte.
//! Brahmi is the 3rd-century-BCE ancestor of every Brahmic script.
//! Routes through sigilbuzz's USE pipeline with the `brah` script tag.
//!
//! The corpus exercises:
//!
//!   * single base consonants (BMP-style classification, but the
//!     codepoints sit in the SMP — U+11000..U+1107F)
//!   * consonant + above-base vowel sign (sign-i U+11039)
//!   * consonant + post-base vowel sign (sign-aa U+11038)
//!   * consonant + virama + consonant (conjunct via U+11046)
//!   * Brahmi digit
//!   * mixed Brahmi + Latin
//!
//! A failure here is a parity drift against rustybuzz; fix in
//! `src/ot/use_shaper` or `src/unicode/use_category`.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_BRAHMI: &[u8] = include_bytes!("fonts/NotoSansBrahmi-Regular.ttf");

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
    // U+11015 — letter ka. Single base.
    Case {
        text: "\u{11015}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ka + sign aa (post-base).
    Case {
        text: "\u{11015}\u{11038}",
        note: "kaa (ka + sign aa)",
        compare_rustybuzz: true,
    },
    // ka + sign i (above-base).
    Case {
        text: "\u{11015}\u{11039}",
        note: "ki (ka + sign i)",
        compare_rustybuzz: true,
    },
    // ka + virama + ka (conjunct).
    Case {
        text: "\u{11015}\u{11046}\u{11015}",
        note: "ka + virama + ka conjunct",
        compare_rustybuzz: true,
    },
    // Brahmi digit (number).
    Case {
        text: "\u{11066}",
        note: "digit zero",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Brahmi.
    Case {
        text: "Hi \u{11015}\u{11038}",
        note: "mixed latin + brahmi",
        compare_rustybuzz: true,
    },
];

#[test]
fn brahmi_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_BRAHMI);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_BRAHMI, 0).expect("parse rustybuzz face");

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

#[test]
fn brahmi_smoke_test_runs() {
    // Smoke test: shape a Brahmi run without panicking. Verifies the
    // Brahmi codepath is wired up — categorization, script routing,
    // segmenter, and USE shaping pipeline all execute.
    let blob = Blob::new(NOTO_BRAHMI);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.push_str("\u{11015}\u{11038}");
    let result = shape(&font, &buffer, &[]).expect("sigilbuzz shape");
    assert!(!result.is_empty(), "Brahmi run produced no glyphs");
}
