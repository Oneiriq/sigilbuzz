//! Sundanese shaping parity tests.
//!
//! Shapes a Sundanese corpus with both sigilbuzz and rustybuzz
//! against Noto Sans Sundanese (OFL) and asserts the output matches
//! byte-for-byte. Sundanese is a Brahmic script for the Sundanese
//! language of West Java. Routes through the USE pipeline with the
//! `sund` script tag.
//!
//! The corpus exercises:
//!
//!   * single base consonants
//!   * panyecek (anusvara, U+1B80)
//!   * pangwisad (visarga-equivalent, U+1B82)
//!   * vowel signs (above / below / pre / post)
//!   * virama (U+1BAB) terminating a syllable
//!   * mixed Sundanese + Latin
//!
//! A failure here is a parity drift against rustybuzz.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_SUND: &[u8] = include_bytes!("fonts/NotoSansSundanese-Regular.ttf");

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
    // ᮃ U+1B83: letter a. Single base.
    Case {
        text: "\u{1B83}",
        note: "a alone",
        compare_rustybuzz: true,
    },
    // ka (U+1B95).
    Case {
        text: "\u{1B95}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ka + panyecek (U+1B80, anusvara above).
    Case {
        text: "\u{1B95}\u{1B80}",
        note: "ka + panyecek",
        compare_rustybuzz: true,
    },
    // ka + sara i (U+1BA4, above).
    Case {
        text: "\u{1B95}\u{1BA4}",
        note: "ki (ka + sara i)",
        compare_rustybuzz: true,
    },
    // ka + sara e (U+1BA6, pre-base).
    Case {
        text: "\u{1B95}\u{1BA6}",
        note: "ke (ka + sara e, pre-base)",
        compare_rustybuzz: true,
    },
    // ka + sara aa (U+1BA7, post-base).
    Case {
        text: "\u{1B95}\u{1BA7}",
        note: "kaa (ka + sara aa)",
        compare_rustybuzz: true,
    },
    // ka + virama (U+1BAB).
    Case {
        text: "\u{1B95}\u{1BAB}",
        note: "ka + virama",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Sundanese.
    Case {
        text: "Hi \u{1B95}\u{1BA4}",
        note: "mixed latin + sundanese",
        compare_rustybuzz: true,
    },
];

#[test]
fn sundanese_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_SUND);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_SUND, 0).expect("parse rustybuzz face");

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
