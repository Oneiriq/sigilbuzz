//! Lao shaping parity tests.
//!
//! Shapes a Lao corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Lao (OFL) and asserts the output matches byte-for-byte.
//! Lao runs through sigilbuzz's Universal Shaping Engine pipeline
//! (`src/ot/use_shaper`) with the Lao script-tag priority (`lao ` ->
//! `DFLT`) and the same reduced Thai/Lao feature chain (`ccmp` /
//! `liga` / `calt`: Lao has no halant and no subjoining).
//!
//! The corpus exercises:
//!
//!   * plain consonants
//!   * pre-base vowels (sara e / sara ae / sara ai / sara o / ...)
//!   * above-base vowels (mai kan, sara i, sara ii, sara y, sara yy)
//!   * below-base vowels (sara u, sara uu)
//!   * tone marks (mai ek, mai tho)
//!   * nikkhahit and niggahita
//!   * Lao digits
//!   * mixed Lao + Latin
//!
//! A failure here is a parity drift against rustybuzz; fix in
//! src/ot/use_shaper or src/unicode/use_category.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_LAO: &[u8] = include_bytes!("fonts/NotoSansLao-Regular.ttf");

struct Case {
    text: &'static str,
    note: &'static str,
    /// When a known rustybuzz / sigilbuzz divergence is tracked as a
    /// follow-up issue, set this to `false` so the test still shapes
    /// the string (to catch panics and crashes) but does not compare
    /// glyph ids against rustybuzz.
    compare_rustybuzz: bool,
}

const CORPUS: &[Case] = &[
    Case {
        text: "",
        note: "empty",
        compare_rustybuzz: true,
    },
    // ກ: ko, base alone.
    Case {
        text: "\u{0E81}",
        note: "ko alone",
        compare_rustybuzz: true,
    },
    // ກາ: ko + sara aa. Post-base matra, no reorder.
    Case {
        text: "\u{0E81}\u{0EB2}",
        note: "kaa (post-base aa)",
        compare_rustybuzz: true,
    },
    // ກິ: ko + sara i. Above-base matra.
    Case {
        text: "\u{0E81}\u{0EB4}",
        note: "ki (above-base i)",
        compare_rustybuzz: true,
    },
    // ກີ: ko + sara ii.
    Case {
        text: "\u{0E81}\u{0EB5}",
        note: "kii (above-base ii)",
        compare_rustybuzz: true,
    },
    // ກຸ: ko + sara u. Below-base.
    Case {
        text: "\u{0E81}\u{0EB8}",
        note: "ku (below-base u)",
        compare_rustybuzz: true,
    },
    // ກູ: ko + sara uu.
    Case {
        text: "\u{0E81}\u{0EB9}",
        note: "kuu (below-base uu)",
        compare_rustybuzz: true,
    },
    // ເກ: sara e + ko. Pre-base vowel typed before base already.
    Case {
        text: "\u{0EC0}\u{0E81}",
        note: "ke (pre-base sara e)",
        compare_rustybuzz: true,
    },
    // ແກ: sara ae + ko.
    Case {
        text: "\u{0EC1}\u{0E81}",
        note: "kae (pre-base sara ae)",
        compare_rustybuzz: true,
    },
    // ໂກ: sara o + ko.
    Case {
        text: "\u{0EC2}\u{0E81}",
        note: "ko (pre-base sara o)",
        compare_rustybuzz: true,
    },
    // ໃກ: sara ai + ko.
    Case {
        text: "\u{0EC3}\u{0E81}",
        note: "kai",
        compare_rustybuzz: true,
    },
    // ກັນ: ko + mai kan + no. Above-base vowel + consonant close.
    Case {
        text: "\u{0E81}\u{0EB1}\u{0E99}",
        note: "kan (mai kan)",
        compare_rustybuzz: true,
    },
    // ກ່າ: ko + mai ek + sara aa.
    Case {
        text: "\u{0E81}\u{0EC8}\u{0EB2}",
        note: "kaa with mai ek",
        compare_rustybuzz: true,
    },
    // ກ້າ: ko + mai tho + sara aa.
    Case {
        text: "\u{0E81}\u{0EC9}\u{0EB2}",
        note: "kaa with mai tho",
        compare_rustybuzz: true,
    },
    // ສະບາຍດີ: "hello". sa + sara a + ba + sara aa + ny + do + sara ii.
    Case {
        text: "\u{0EAA}\u{0EB0}\u{0E9A}\u{0EB2}\u{0E8D}\u{0E94}\u{0EB5}",
        note: "sabaidi (hello)",
        compare_rustybuzz: true,
    },
    // Lao digits 0-4.
    Case {
        text: "\u{0ED0}\u{0ED1}\u{0ED2}\u{0ED3}\u{0ED4}",
        note: "lao digits 0-4",
        compare_rustybuzz: true,
    },
    // Mixed Lao + Latin.
    Case {
        text: "Hi \u{0E81}\u{0EB2}",
        note: "mixed latin + lao",
        compare_rustybuzz: true,
    },
    // ກໍ: ko + niggahita (U+0ECD). Nikkhahit-equivalent mark.
    Case {
        text: "\u{0E81}\u{0ECD}",
        note: "ko + niggahita",
        compare_rustybuzz: true,
    },
    // ກຳ: ko + lao am (U+0EB3). HarfBuzz decomposes this to
    // niggahita (U+0ECD) + sara aa (U+0EB2) at buffer-prep;
    // sigilbuzz mirrors that in shape.rs so GSUB/GPOS see the
    // decomposed pair rustybuzz sees.
    Case {
        text: "\u{0E81}\u{0EB3}",
        note: "kam (lao am, PUA-decomposed)",
        compare_rustybuzz: true,
    },
];

#[test]
fn lao_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_LAO);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_LAO, 0).expect("parse rustybuzz face");

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
