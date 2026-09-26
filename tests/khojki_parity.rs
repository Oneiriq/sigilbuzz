//! Khojki shaping parity tests.
//!
//! Shapes a Khojki corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Khojki (OFL) and asserts the output matches byte-for-byte.
//! Khojki is a historical script for Sindhi / Khoja Ismaili community.
//! Routes through sigilbuzz's USE pipeline with the `khoj` script tag.
//!
//! Codepoint range: U+11200..U+1124F (SMP).

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_KHOJKI: &[u8] = include_bytes!("fonts/NotoSansKhojki-Regular.ttf");

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
    // U+11208: letter ka. Single base.
    Case {
        text: "\u{11208}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ka + sign aa (post).
    Case {
        text: "\u{11208}\u{1122C}",
        note: "kaa (ka + sign aa)",
        compare_rustybuzz: true,
    },
    // ka + sign u (below).
    Case {
        text: "\u{11208}\u{1122F}",
        note: "ku (ka + sign u)",
        compare_rustybuzz: true,
    },
    // ka + sign e (above).
    Case {
        text: "\u{11208}\u{11230}",
        note: "ke (ka + sign e)",
        compare_rustybuzz: true,
    },
    // ka + virama + ka (conjunct).
    Case {
        text: "\u{11208}\u{11235}\u{11208}",
        note: "ka + virama + ka conjunct",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Khojki.
    Case {
        text: "Hi \u{11208}\u{1122C}",
        note: "mixed latin + khojki",
        compare_rustybuzz: true,
    },
];

#[test]
fn khojki_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_KHOJKI);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_KHOJKI, 0).expect("parse rustybuzz face");

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
