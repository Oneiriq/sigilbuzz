//! Buginese shaping parity tests.
//!
//! Shapes a Buginese (Lontara) corpus with both sigilbuzz and
//! rustybuzz against Noto Sans Buginese (OFL) and asserts the output
//! matches byte-for-byte. Buginese is a Brahmic-derived script for
//! the Bugis language of South Sulawesi. It uses the USE pipeline
//! with the `bugi` script tag.
//!
//! The corpus exercises:
//!
//!   * single base consonants
//!   * consonant + above-base vowel sign (sara i, sara u, sara ae)
//!   * consonant + pre-base vowel sign (sara e, reorders before base)
//!   * consonant + post-base vowel sign (sara o)
//!   * mixed Buginese + Latin
//!
//! A failure here is a parity drift against rustybuzz; fix in
//! `src/ot/use_shaper`.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_BUGINESE: &[u8] = include_bytes!("fonts/NotoSansBuginese-Regular.ttf");

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
    // ᨀ U+1A00: letter ka. Single base.
    Case {
        text: "\u{1A00}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ka + sara i (above).
    Case {
        text: "\u{1A00}\u{1A17}",
        note: "ki (ka + sara i)",
        compare_rustybuzz: true,
    },
    // ka + sara u (above).
    Case {
        text: "\u{1A00}\u{1A18}",
        note: "ku (ka + sara u)",
        compare_rustybuzz: true,
    },
    // ka + sara e (pre-base, reorder fires).
    Case {
        text: "\u{1A00}\u{1A19}",
        note: "ke (ka + sara e, pre-base reorder)",
        compare_rustybuzz: true,
    },
    // ka + sara o (post-base).
    Case {
        text: "\u{1A00}\u{1A1A}",
        note: "ko (ka + sara o)",
        compare_rustybuzz: true,
    },
    // ka + sara ae (above).
    Case {
        text: "\u{1A00}\u{1A1B}",
        note: "kae (ka + sara ae)",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Buginese.
    Case {
        text: "Hi \u{1A00}\u{1A17}",
        note: "mixed latin + buginese",
        compare_rustybuzz: true,
    },
];

#[test]
fn buginese_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_BUGINESE);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_BUGINESE, 0).expect("parse rustybuzz face");

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
