//! Tai Tham (Lanna) shaping parity tests.
//!
//! Shapes a Tai Tham corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Tai Tham (OFL) and asserts the output matches byte-for-
//! byte. Tai Tham is a Brahmic script used historically for Northern
//! Thai (Lanna), Tai Lue, Khün and Lao Tham. Routes through
//! sigilbuzz's USE pipeline with the `lana` script tag.
//!
//! The corpus exercises:
//!
//!   * single base consonants
//!   * sakot (U+1A60): Tai Tham's halant
//!   * above-base vowel signs (sign-i U+1A65, sign-ii U+1A66)
//!   * below-base vowel sign (sign-u U+1A69)
//!   * post-base vowel sign (sign-aa U+1A63)
//!   * Tai Tham digit (Symbol pass-through)
//!   * mixed Tai Tham + Latin
//!
//! A failure here is a parity drift against rustybuzz.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_LANA: &[u8] = include_bytes!("fonts/NotoSansTaiTham-Regular.ttf");

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
    // ᨠ U+1A20: high ka.
    Case {
        text: "\u{1A20}",
        note: "high ka alone",
        compare_rustybuzz: true,
    },
    // ka + sakot + ka (subscript).
    Case {
        text: "\u{1A20}\u{1A60}\u{1A20}",
        note: "ka + sakot + ka (subscript)",
        compare_rustybuzz: true,
    },
    // ka + sign-aa (post-base).
    Case {
        text: "\u{1A20}\u{1A63}",
        note: "kaa (ka + sign aa)",
        compare_rustybuzz: true,
    },
    // ka + sign-i (above-base).
    Case {
        text: "\u{1A20}\u{1A65}",
        note: "ki (ka + sign i)",
        compare_rustybuzz: true,
    },
    // ka + sign-u (below-base).
    Case {
        text: "\u{1A20}\u{1A69}",
        note: "ku (ka + sign u)",
        compare_rustybuzz: true,
    },
    // Tai Tham hora digit run.
    Case {
        text: "\u{1A80}\u{1A81}\u{1A82}",
        note: "tai tham digits 0-2",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Tai Tham.
    Case {
        text: "Hi \u{1A20}\u{1A65}",
        note: "mixed latin + tai tham",
        compare_rustybuzz: true,
    },
];

#[test]
fn tai_tham_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_LANA);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_LANA, 0).expect("parse rustybuzz face");

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
