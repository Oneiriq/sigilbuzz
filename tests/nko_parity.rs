//! N'Ko shaping parity tests.
//!
//! Shapes an N'Ko corpus with both sigilbuzz and rustybuzz against
//! Noto Sans NKo (OFL) and asserts the output matches byte-for-byte.
//! N'Ko is a right-to-left alphabetic script for Manding languages
//! (Bambara / Maninka / Dyula). Routes through sigilbuzz's USE
//! pipeline with the `nko ` script tag.
//!
//! The corpus exercises:
//!
//!   * single letters
//!   * letter + tone mark (high tone, low tone, rising, descending)
//!   * the dantayalan low-tone mark (U+07FD)
//!   * N'Ko digits (Symbol pass-through)
//!   * mixed N'Ko + Latin (BiDi reordering not exercised here:
//!     the parity test compares raw glyph order, so the strings
//!     are kept logical-only).
//!
//! A failure here is a parity drift against rustybuzz; fix in
//! `src/ot/use_shaper` or `src/unicode/use_category`.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_NKO: &[u8] = include_bytes!("fonts/NotoSansNKo-Regular.ttf");

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
    // ߒ U+07D2: N'Ko letter ta. Single base.
    Case {
        text: "\u{07D2}",
        note: "ta alone",
        compare_rustybuzz: true,
    },
    // N'Ko digits 0-4 (U+07C0..U+07C4). Symbol pass-through:
    // sigilbuzz emits one syllable per digit so each keeps its
    // own cluster id, matching rustybuzz.
    Case {
        text: "\u{07C0}\u{07C1}\u{07C2}\u{07C3}\u{07C4}",
        note: "nko digits 0-4",
        compare_rustybuzz: true,
    },
    // The remaining cases below exercise tone-mark + base
    // combinations. The Noto Sans NKo font registers `ccmp` /
    // `init` / `medi` / `fina` lookups that compose the tone with
    // a positional variant of the base. sigilbuzz's USE shaper
    // does not yet drive joining-form masking for non-Arabic
    // scripts, so the precomposition diverges. Tracked as the
    // follow-up at <https://github.com/Oneiriq/sigilbuzz/issues>
    // (USE: joining-form masking for N'Ko / Phags-pa). The cases
    // still shape. We just do not byte-compare the result.
    Case {
        text: "\u{07D2}\u{07EB}",
        note: "ta + high tone (joining-form)",
        compare_rustybuzz: true,
    },
    Case {
        text: "\u{07D2}\u{07EC}",
        note: "ta + low tone (joining-form)",
        compare_rustybuzz: true,
    },
    Case {
        text: "\u{07D2}\u{07ED}",
        note: "ta + rising tone (joining-form)",
        compare_rustybuzz: true,
    },
    Case {
        text: "\u{07D2}\u{07EE}",
        note: "ta + descending tone (joining-form)",
        compare_rustybuzz: true,
    },
    Case {
        text: "\u{07D2}\u{07FD}",
        note: "ta + dantayalan (joining-form)",
        compare_rustybuzz: true,
    },
    Case {
        text: "\u{07D2}\u{07DE}\u{07CF}",
        note: "nko (n'ko, joining-form)",
        compare_rustybuzz: true,
    },
];

#[test]
fn nko_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_NKO);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_NKO, 0).expect("parse rustybuzz face");

    for case in CORPUS {
        let mut buffer = Buffer::new();
        buffer.push_str(case.text);
        let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

        if !case.compare_rustybuzz {
            continue;
        }

        let mut rb_buf = rustybuzz::UnicodeBuffer::new();
        rb_buf.push_str(case.text);
        // N'Ko is RTL. Set direction explicitly so rustybuzz emits
        // visual order; reverse it before zipping against sigilbuzz's
        // logical-order output.
        rb_buf.set_direction(RbDirection::RightToLeft);
        let rb_out = rustybuzz::shape(&rb_face, &[], rb_buf);
        let rb_gids: Vec<u32> = rb_out
            .glyph_infos()
            .iter()
            .rev()
            .map(|g| g.glyph_id)
            .collect();
        let rb_xadvs: Vec<i32> = rb_out
            .glyph_positions()
            .iter()
            .rev()
            .map(|p| p.x_advance)
            .collect();

        assert_eq!(
            sig.len(),
            rb_gids.len(),
            "glyph count diverged for {} ({:?}): sigilbuzz={} rustybuzz={}",
            case.note,
            case.text,
            sig.len(),
            rb_gids.len()
        );

        for (i, (sig_g, (rb_gid, rb_xadv))) in sig
            .glyphs
            .iter()
            .zip(rb_gids.iter().zip(rb_xadvs.iter()))
            .enumerate()
        {
            assert_eq!(
                sig_g.glyph_id, *rb_gid,
                "glyph id mismatch at position {i} of {} ({:?}): sigilbuzz={} rustybuzz={}",
                case.note, case.text, sig_g.glyph_id, rb_gid
            );
            assert_eq!(
                sig_g.x_advance, *rb_xadv,
                "x_advance mismatch at position {i} of {} ({:?}): sigilbuzz={} rustybuzz={}",
                case.note, case.text, sig_g.x_advance, rb_xadv
            );
        }
    }
}
