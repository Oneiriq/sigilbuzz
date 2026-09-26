//! Balinese shaping parity tests.
//!
//! Shapes a Balinese corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Balinese (OFL) and asserts the output matches byte-for-
//! byte. Balinese is a Brahmic script for Balinese / Sasak / Old
//! Javanese. Routes through sigilbuzz's USE pipeline with the `bali`
//! script tag.
//!
//! The corpus exercises:
//!
//!   * single base consonants
//!   * tedung post-base vowel (U+1B35)
//!   * above-base vowel signs (sign-i U+1B36, sign-u U+1B38)
//!   * pre-base vowel sign (sign-e U+1B3E, reorder fires)
//!   * adeg adeg halant (U+1B44) terminating a syllable
//!   * mixed Balinese + Latin
//!
//! A failure here is a parity drift against rustybuzz.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_BALI: &[u8] = include_bytes!("fonts/NotoSansBalinese-Regular.ttf");

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
    // ᬓ U+1B13: letter ka. Single base.
    Case {
        text: "\u{1B13}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ka + tedung (U+1B35, post-base aa).
    Case {
        text: "\u{1B13}\u{1B35}",
        note: "kaa (ka + tedung)",
        compare_rustybuzz: true,
    },
    // ka + sign-i (U+1B36, above-base).
    Case {
        text: "\u{1B13}\u{1B36}",
        note: "ki (ka + sign-i)",
        compare_rustybuzz: true,
    },
    // ka + sign-u (U+1B38, below-base).
    Case {
        text: "\u{1B13}\u{1B38}",
        note: "ku (ka + sign-u)",
        compare_rustybuzz: true,
    },
    // ka + sign-e (U+1B3E, pre-base, reorder fires).
    Case {
        text: "\u{1B13}\u{1B3E}",
        note: "ke (ka + sign-e, pre-base)",
        compare_rustybuzz: true,
    },
    // ka + adeg adeg (U+1B44, halant).
    Case {
        text: "\u{1B13}\u{1B44}",
        note: "ka + adeg adeg",
        compare_rustybuzz: true,
    },
    // ᬑ U+1B11: independent vowel o.
    Case {
        text: "\u{1B11}",
        note: "independent o",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Balinese.
    Case {
        text: "Hi \u{1B13}\u{1B36}",
        note: "mixed latin + balinese",
        compare_rustybuzz: true,
    },
];

#[test]
fn balinese_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_BALI);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_BALI, 0).expect("parse rustybuzz face");

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
