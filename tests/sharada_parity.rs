//! Sharada shaping parity tests.
//!
//! Shapes a Sharada corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Sharada (OFL) and asserts the output matches byte-for-byte.
//! Sharada is a historical Kashmiri / Sanskrit script (8th century);
//! still used liturgically in Kashmiri Hindu communities. Routes
//! through sigilbuzz's USE pipeline with the `shrd` script tag.
//!
//! The corpus exercises:
//!
//!   * single base consonants (SMP — U+11180..U+111DF)
//!   * consonant + above-base vowel sign (sign-i U+111B4)
//!   * consonant + below-base vowel sign (sign-u U+111B6)
//!   * consonant + post-base vowel sign (sign-aa U+111B3)
//!   * consonant + virama + consonant (conjunct via U+111C0)
//!   * Sharada digit
//!   * mixed Sharada + Latin

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_SHARADA: &[u8] = include_bytes!("fonts/NotoSansSharada-Regular.ttf");

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
    // U+11192 — letter ka. Single base.
    Case {
        text: "\u{11192}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ka + sign aa.
    Case {
        text: "\u{11192}\u{111B3}",
        note: "kaa (ka + sign aa)",
        compare_rustybuzz: true,
    },
    // ka + sign i (renders pre-base in Sharada — the USE reorder
    // moves sign-i to before the base consonant).
    Case {
        text: "\u{11192}\u{111B4}",
        note: "ki (ka + sign i, pre-base reorder)",
        compare_rustybuzz: true,
    },
    // ka + sign u (below).
    Case {
        text: "\u{11192}\u{111B6}",
        note: "ku (ka + sign u)",
        compare_rustybuzz: true,
    },
    // ka + virama + ka (conjunct).
    Case {
        text: "\u{11192}\u{111C0}\u{11192}",
        note: "ka + virama + ka conjunct",
        compare_rustybuzz: true,
    },
    // Sharada digit.
    Case {
        text: "\u{111D0}",
        note: "digit zero",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Sharada.
    Case {
        text: "Hi \u{11192}\u{111B3}",
        note: "mixed latin + sharada",
        compare_rustybuzz: true,
    },
];

#[test]
fn sharada_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_SHARADA);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_SHARADA, 0).expect("parse rustybuzz face");

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
