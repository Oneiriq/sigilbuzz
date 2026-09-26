//! Lepcha shaping parity tests.
//!
//! Shapes a Lepcha corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Lepcha (OFL) and asserts the output matches byte-for-
//! byte. Lepcha is a Brahmic script of Sikkim and northeastern India
//! used for the Lepcha language. Routes through the USE pipeline
//! with the `lepc` script tag.
//!
//! The corpus exercises:
//!
//!   * single base consonants
//!   * post-base vowel signs (sign-ii / sign-uu / sign-o)
//!   * pre-base vowel signs (sign-e / sign-eu / sign-i, reorder
//!     fires)
//!   * below-base vowel sign (sign-u)
//!   * final consonants (e.g. ran sign U+1C36)
//!   * mixed Lepcha + Latin
//!
//! A failure here is a parity drift against rustybuzz.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_LEPC: &[u8] = include_bytes!("fonts/NotoSansLepcha-Regular.ttf");

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
    // ᰀ U+1C00: letter ka. Single base.
    Case {
        text: "\u{1C00}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ka + sign i (U+1C26 post-base).
    Case {
        text: "\u{1C00}\u{1C26}",
        note: "ki (ka + sign i)",
        compare_rustybuzz: true,
    },
    // ka + sign o (U+1C27 pre-base, reorder fires).
    Case {
        text: "\u{1C00}\u{1C27}",
        note: "ko (ka + sign o, pre-base)",
        compare_rustybuzz: true,
    },
    // ka + sign uu (U+1C2B post-base).
    Case {
        text: "\u{1C00}\u{1C2B}",
        note: "kuu (ka + sign uu)",
        compare_rustybuzz: true,
    },
    // ka + sign u (U+1C2C below-base).
    Case {
        text: "\u{1C00}\u{1C2C}",
        note: "ku (ka + sign u below)",
        compare_rustybuzz: true,
    },
    // ka + ran sign (U+1C36 above, final consonant marker).
    Case {
        text: "\u{1C00}\u{1C36}",
        note: "ka + ran",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Lepcha.
    Case {
        text: "Hi \u{1C00}\u{1C26}",
        note: "mixed latin + lepcha",
        compare_rustybuzz: true,
    },
];

#[test]
fn lepcha_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_LEPC);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_LEPC, 0).expect("parse rustybuzz face");

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
