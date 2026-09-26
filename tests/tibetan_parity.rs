//! Tibetan shaping parity tests.
//!
//! Shapes a small Tibetan corpus with both sigilbuzz and rustybuzz
//! against Noto Serif Tibetan (OFL) and asserts the output matches.
//! Tibetan runs through sigilbuzz's feature-loop-only Tibetan shaper
//! in `src/ot/tibetan.rs`: no reordering, just `abvs`/`blws` driven
//! from the script-tag priority `[tibt, DFLT]`.
//!
//! The corpus exercises:
//!
//!   * empty run
//!   * single base consonant
//!   * base + above-base vowel sign (`abvs`)
//!   * base + below-base vowel sign (`blws`)
//!   * base + subjoined consonant (stack)
//!   * base + multiple subjoined consonants (deep stack)
//!   * mixed Tibetan + Latin
//!
//! A failure here is a parity drift against rustybuzz; fix in
//! `src/ot/tibetan.rs` or the script_of arm in
//! `src/unicode/mod.rs`.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_TIBETAN: &[u8] = include_bytes!("fonts/NotoSerifTibetan-Regular.ttf");

struct Case {
    text: &'static str,
    note: &'static str,
}

const CORPUS: &[Case] = &[
    // Empty: neither engine should emit glyphs.
    Case {
        text: "",
        note: "empty",
    },
    // ཀ: KA (U+0F40), simplest consonant alone. No reorder, no
    // subjoined, no vowel.
    Case {
        text: "\u{0F40}",
        note: "KA alone",
    },
    // ཨ: A (U+0F68). Another base consonant; exercises the cmap
    // path for a different glyph.
    Case {
        text: "\u{0F68}",
        note: "A alone",
    },
    // ཀི: KA + sign I (U+0F72). Above-base vowel; `abvs` should
    // pick the contextual form.
    Case {
        text: "\u{0F40}\u{0F72}",
        note: "KA + sign I (above-base)",
    },
    // ཀུ: KA + sign U (U+0F74). Below-base vowel; `blws` may apply
    // depending on font.
    Case {
        text: "\u{0F40}\u{0F74}",
        note: "KA + sign U (below-base)",
    },
    // ཀྱ: KA (U+0F40) + subjoined YA (U+0FB1). Below-base
    // subjoined consonant, the canonical Tibetan stack.
    Case {
        text: "\u{0F40}\u{0FB1}",
        note: "KA + subjoined YA",
    },
    // ཀྲ: KA + subjoined RA (U+0FB2). Another stack.
    Case {
        text: "\u{0F40}\u{0FB2}",
        note: "KA + subjoined RA",
    },
    // སྐྲ: SA + subjoined KA + subjoined RA. Three-deep stack that
    // exercises multiple `blws` lookups in sequence.
    Case {
        text: "\u{0F66}\u{0F90}\u{0FB2}",
        note: "SA + subjoined KA + subjoined RA",
    },
    // Mixed Tibetan + Latin: segmenter must split correctly so
    // Tibetan features only see the Tibetan segment.
    Case {
        text: "Hi \u{0F40}",
        note: "mixed latin + Tibetan",
    },
];

#[test]
fn tibetan_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_TIBETAN);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_TIBETAN, 0).expect("parse rustybuzz face");

    for case in CORPUS {
        let mut buffer = Buffer::new();
        buffer.push_str(case.text);
        let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

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

#[test]
fn tibetan_segment_dispatches_under_tibt_tag() {
    // Sanity: a Tibetan-only run must produce non-zero glyph ids
    // (i.e. cmap finds them) and at least one glyph per codepoint
    // when there are no decompositions in play. Subjoined consonants
    // typically render as single glyphs already, so the count for a
    // simple base+subjoined pair should be 2 unless the font's
    // `abvs`/`blws` collapses them.
    let blob = Blob::new(NOTO_TIBETAN);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut buffer = Buffer::new();
    buffer.push_str("\u{0F40}");
    let shaped = shape(&font, &buffer, &[]).expect("shape KA");
    assert_eq!(shaped.len(), 1, "KA alone is one glyph");
    assert_ne!(
        shaped.glyphs[0].glyph_id, 0,
        "KA must have a non-notdef glyph id in Noto Serif Tibetan"
    );
}
