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
//!     the strings are kept logical-only).
//!
//! Both engines shape RTL and return visual order, so glyph ids,
//! advances, and offsets are compared position by position with no
//! reversal.
//!
//! A failure here is a parity drift against rustybuzz; fix in
//! `src/ot/use_shaper` or `src/unicode/use_category`.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

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
        // N'Ko is RTL: both engines get the direction explicitly and
        // both return visual order.
        buffer.set_direction(Direction::Rtl);
        buffer.push_str(case.text);
        let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

        if !case.compare_rustybuzz {
            continue;
        }

        let mut rb_buf = rustybuzz::UnicodeBuffer::new();
        rb_buf.push_str(case.text);
        rb_buf.set_direction(RbDirection::RightToLeft);
        let rb_out = rustybuzz::shape(&rb_face, &[], rb_buf);
        let rb: Vec<(u32, i32, i32, i32)> = rb_out
            .glyph_infos()
            .iter()
            .zip(rb_out.glyph_positions())
            .map(|(g, p)| (g.glyph_id, p.x_advance, p.x_offset, p.y_offset))
            .collect();
        let sig: Vec<(u32, i32, i32, i32)> = sig
            .glyphs
            .iter()
            .map(|g| (g.glyph_id, g.x_advance, g.x_offset, g.y_offset))
            .collect();

        assert_eq!(
            sig.len(),
            rb.len(),
            "glyph count diverged for {} ({:?}): sigilbuzz={} rustybuzz={}",
            case.note,
            case.text,
            sig.len(),
            rb.len()
        );

        for (i, (s, r)) in sig.iter().zip(&rb).enumerate() {
            assert_eq!(
                s, r,
                "(gid, x_adv, x_off, y_off) mismatch at visual position {i} of {} ({:?})",
                case.note, case.text
            );
        }
    }
}
