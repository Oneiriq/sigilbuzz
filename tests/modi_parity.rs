//! Modi shaping parity tests.
//!
//! Shapes a Modi corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Modi (OFL) and asserts the output matches byte-for-byte.
//! Modi is a historical script for Marathi (17th century). Routes
//! through sigilbuzz's USE pipeline with the `modi` script tag.
//!
//! Codepoint range: U+11600..U+1165F (SMP).

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_MODI: &[u8] = include_bytes!("fonts/NotoSansModi-Regular.ttf");

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
    // U+11606 — letter ka.
    Case {
        text: "\u{11606}",
        note: "ka alone",
        compare_rustybuzz: true,
    },
    // ka + sign aa (post).
    Case {
        text: "\u{11606}\u{11630}",
        note: "kaa (ka + sign aa)",
        compare_rustybuzz: true,
    },
    // ka + sign u (below).
    Case {
        text: "\u{11606}\u{11633}",
        note: "ku (ka + sign u)",
        compare_rustybuzz: true,
    },
    // ka + sign e (above).
    Case {
        text: "\u{11606}\u{11639}",
        note: "ke (ka + sign e)",
        compare_rustybuzz: true,
    },
    // ka + virama + ka (conjunct).
    Case {
        text: "\u{11606}\u{1163F}\u{11606}",
        note: "ka + virama + ka conjunct",
        compare_rustybuzz: true,
    },
    // Modi digit.
    Case {
        text: "\u{11650}",
        note: "digit zero",
        compare_rustybuzz: true,
    },
    // Mixed Latin + Modi.
    Case {
        text: "Hi \u{11606}\u{11630}",
        note: "mixed latin + modi",
        compare_rustybuzz: true,
    },
];

#[test]
fn modi_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_MODI);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_MODI, 0).expect("parse rustybuzz face");

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
