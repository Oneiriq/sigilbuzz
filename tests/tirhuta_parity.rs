//! Tirhuta shaping parity tests.
//!
//! Shapes a Tirhuta corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Tirhuta (OFL) and asserts the output matches byte-for-byte.
//! Tirhuta is a historical script for Maithili / Sanskrit. Routes
//! through sigilbuzz's USE pipeline with the `tirh` script tag.
//!
//! Codepoint range: U+11480..U+114DF (SMP). Notable: sign-e (U+114B9)
//! and sign-o (U+114BC) are pre-base vowel signs — the USE pre-base
//! reorder pass moves them to before the base consonant.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_TIRHUTA: &[u8] = include_bytes!("fonts/NotoSansTirhuta-Regular.ttf");

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
    // U+1148A — letter ka.
    Case {
        text: "\u{1148A}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ka + sign aa (post).
    Case {
        text: "\u{1148A}\u{114B0}",
        note: "kaa (ka + sign aa)",
        compare_rustybuzz: true,
    },
    // ka + sign u (below).
    Case {
        text: "\u{1148A}\u{114B3}",
        note: "ku (ka + sign u)",
        compare_rustybuzz: true,
    },
    // ka + sign e (pre-base reorder).
    Case {
        text: "\u{1148A}\u{114B9}",
        note: "ke (ka + sign e, pre-base reorder)",
        compare_rustybuzz: true,
    },
    // ka + virama + ka (conjunct).
    Case {
        text: "\u{1148A}\u{114C2}\u{1148A}",
        note: "ka + virama + ka conjunct",
        compare_rustybuzz: true,
    },
    // Tirhuta digit.
    Case {
        text: "\u{114D0}",
        note: "digit zero",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Tirhuta.
    Case {
        text: "Hi \u{1148A}\u{114B0}",
        note: "mixed latin + tirhuta",
        compare_rustybuzz: true,
    },
];

#[test]
fn tirhuta_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_TIRHUTA);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_TIRHUTA, 0).expect("parse rustybuzz face");

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
