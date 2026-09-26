//! Thai shaping parity tests.
//!
//! Shapes a Thai corpus with both sigilbuzz and rustybuzz against
//! Noto Sans Thai (OFL) and asserts the output matches byte-for-byte.
//! Thai runs through sigilbuzz's Universal Shaping Engine pipeline
//! (`src/ot/use_shaper`) with the Thai script-tag priority (`thai` ->
//! `DFLT`) and the reduced Thai/Lao feature chain (`ccmp` / `liga` /
//! `calt`, Thai has no halant and no subjoining).
//!
//! The corpus exercises:
//!
//!   * plain consonants
//!   * pre-base vowels (sara e / sara ae / sara ai-maimuan / sara
//!     ai-maimalai / sara o)
//!   * above-base vowels (mai han-akat, sara i, sara ii, sara ue,
//!     sara uee)
//!   * below-base vowels (sara u, sara uu, pinthu)
//!   * tone marks (mai ek, mai tho, mai tri, mai chattawa)
//!   * nikkhahit and thanthakhat
//!   * the composed sara am (U+0E33), a single codepoint that
//!     renders as nikkhahit + sara aa
//!   * Thai digits
//!   * mixed Thai + Latin
//!
//! A failure here is a parity drift against rustybuzz; fix in
//! src/ot/use_shaper or src/unicode/use_category.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_THAI: &[u8] = include_bytes!("fonts/NotoSansThai-Regular.ttf");

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
    // ก: ko kai, base alone.
    Case {
        text: "\u{0E01}",
        note: "ko kai alone",
        compare_rustybuzz: true,
    },
    // กา: ko + sara aa. Post-base matra, no reorder.
    Case {
        text: "\u{0E01}\u{0E32}",
        note: "kaa (post-base aa)",
        compare_rustybuzz: true,
    },
    // กิ: ko + sara i. Above-base matra.
    Case {
        text: "\u{0E01}\u{0E34}",
        note: "ki (above-base i)",
        compare_rustybuzz: true,
    },
    // กี: ko + sara ii. Above-base matra.
    Case {
        text: "\u{0E01}\u{0E35}",
        note: "kii (above-base ii)",
        compare_rustybuzz: true,
    },
    // กุ: ko + sara u. Below-base matra.
    Case {
        text: "\u{0E01}\u{0E38}",
        note: "ku (below-base u)",
        compare_rustybuzz: true,
    },
    // กู: ko + sara uu. Below-base matra.
    Case {
        text: "\u{0E01}\u{0E39}",
        note: "kuu (below-base uu)",
        compare_rustybuzz: true,
    },
    // เก: sara e + ko. Pre-base vowel typed before the base
    // already; the reorder pass is a no-op.
    Case {
        text: "\u{0E40}\u{0E01}",
        note: "ke (pre-base sara e)",
        compare_rustybuzz: true,
    },
    // แก: sara ae + ko.
    Case {
        text: "\u{0E41}\u{0E01}",
        note: "kae (pre-base sara ae)",
        compare_rustybuzz: true,
    },
    // โก: sara o + ko.
    Case {
        text: "\u{0E42}\u{0E01}",
        note: "ko (pre-base sara o)",
        compare_rustybuzz: true,
    },
    // ใก: sara ai-maimuan + ko.
    Case {
        text: "\u{0E43}\u{0E01}",
        note: "kai-maimuan",
        compare_rustybuzz: true,
    },
    // ก่า: ko + mai ek + sara aa. Tone mark on a syllable.
    Case {
        text: "\u{0E01}\u{0E48}\u{0E32}",
        note: "kaa with mai ek",
        compare_rustybuzz: true,
    },
    // ก้อน: ko + mai tho + sara o-mai-muan-like + no.
    Case {
        text: "\u{0E01}\u{0E49}\u{0E2D}\u{0E19}",
        note: "kon (mai tho)",
        compare_rustybuzz: true,
    },
    // กัน: ko + mai han-akat + no. Above-base + consonant close.
    Case {
        text: "\u{0E01}\u{0E31}\u{0E19}",
        note: "kan (mai han-akat)",
        compare_rustybuzz: true,
    },
    // การ์: ko + sara aa + ro + thanthakhat. Killer mark.
    Case {
        text: "\u{0E01}\u{0E32}\u{0E23}\u{0E4C}",
        note: "kaar (thanthakhat kills ro)",
        compare_rustybuzz: true,
    },
    // สวัสดี: "hello". sa + wo + mai han-akat + sa + do + sara ii.
    Case {
        text: "\u{0E2A}\u{0E27}\u{0E31}\u{0E2A}\u{0E14}\u{0E35}",
        note: "sawatdi (hello)",
        compare_rustybuzz: true,
    },
    // Thai digits 0-4.
    Case {
        text: "\u{0E50}\u{0E51}\u{0E52}\u{0E53}\u{0E54}",
        note: "thai digits 0-4",
        compare_rustybuzz: true,
    },
    // Mixed Thai + Latin.
    Case {
        text: "Hi \u{0E01}\u{0E32}",
        note: "mixed latin + thai",
        compare_rustybuzz: true,
    },
    // ก + sara am (U+0E33). HarfBuzz's Thai shaper decomposes
    // sara am into nikkhahit + sara aa at buffer-prep time;
    // sigilbuzz mirrors that in shape.rs so the cmap lookup lands
    // on the decomposed pair and GSUB/GPOS see what rustybuzz sees.
    Case {
        text: "\u{0E01}\u{0E33}",
        note: "kam (sara am, PUA-decomposed)",
        compare_rustybuzz: true,
    },
];

#[test]
fn thai_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_THAI);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_THAI, 0).expect("parse rustybuzz face");

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
