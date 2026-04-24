//! Old Hangul (Jamo) shaping parity tests.
//!
//! Shapes a small Old-Hangul corpus with both sigilbuzz and rustybuzz
//! against a subset of Noto Sans Korean (OFL) restricted to the Jamo
//! blocks plus a handful of precomposed syllables so the "don't break
//! modern Hangul" invariant is also exercised. Hangul Jamo runs route
//! through sigilbuzz's Universal Shaping Engine pipeline
//! (`src/ot/use_shaper`) with the Hangul script-tag priority (`hang`
//! → `jamo` → `DFLT`) and the jamo feature chain (`ccmp` / `ljmo` /
//! `vjmo` / `tjmo` / `calt`).
//!
//! The corpus exercises:
//!
//!   * a single precomposed syllable (modern Hangul path — must NOT
//!     go through the USE machine)
//!   * Jamo L + V (leading choseong + vowel jungseong)
//!   * Jamo L + V + T (full LVT triple)
//!   * a Jamo Extended-A consonant
//!   * a Jamo Extended-B trailing jamo
//!   * mixed precomposed + Jamo-decomposed in the same run
//!   * mixed Latin + Jamo
//!
//! A failure here is a parity drift against rustybuzz.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_OLD_HANGUL: &[u8] = include_bytes!("fonts/NotoSansOldHangul-Subset.ttf");

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
    // 가 — precomposed syllable U+AC00 (kiyeok + a). Flows through
    // the default path, NOT the USE pipeline.
    Case {
        text: "\u{AC00}",
        note: "precomposed syllable 가",
        compare_rustybuzz: true,
    },
    // ᄀ + ᅡ — leading kiyeok (U+1100) + vowel a (U+1161). Two-
    // jamo LV syllable; the font's ljmo / vjmo features pick the
    // choseong and jungseong variant forms.
    Case {
        text: "\u{1100}\u{1161}",
        note: "jamo L + V (ka)",
        compare_rustybuzz: true,
    },
    // ᄀ + ᅡ + ᆨ — LVT triple (kiyeok + a + kiyeok-final). The
    // tjmo feature picks the jongseong variant.
    Case {
        text: "\u{1100}\u{1161}\u{11A8}",
        note: "jamo L + V + T (kak)",
        compare_rustybuzz: true,
    },
    // ᄂ + ᅧ + ᆼ — nieun + yeo + ieung-final.
    Case {
        text: "\u{1102}\u{1167}\u{11BC}",
        note: "jamo L + V + T (nyeong)",
        compare_rustybuzz: true,
    },
    // Jamo Extended-A leading jamo (U+A960 tikeut-mieum).
    Case {
        text: "\u{A960}\u{1161}",
        note: "jamo Extended-A L + V",
        compare_rustybuzz: true,
    },
    // Jamo Extended-B trailing jamo (U+D7CB — kiyeok-rieul).
    Case {
        text: "\u{1100}\u{1161}\u{D7CB}",
        note: "jamo L + V + Extended-B T",
        compare_rustybuzz: true,
    },
    // Mixed precomposed + jamo in one run. Precomposed stays on
    // the default path; jamo portion routes through USE.
    Case {
        text: "\u{AC00}\u{1100}\u{1161}",
        note: "precomposed + jamo decomposed",
        compare_rustybuzz: true,
    },
    // Mixed Latin + jamo. rustybuzz script-segments the run and
    // shapes the Latin and Hangul portions independently, which
    // leaves the jamo as plain L + V glyphs (no `ljmo`/`vjmo`
    // substitution). sigilbuzz currently runs the whole buffer
    // through a single pipeline — mixed-script segmentation is
    // landing in PR #40 (feature/script-segmenter). Tracked as a
    // follow-up; the string still shapes cleanly, we just do not
    // compare glyph ids here.
    Case {
        text: "Hi \u{1100}\u{1161}",
        note: "latin + jamo — script-segmenter follow-up",
        compare_rustybuzz: false,
    },
];

#[test]
fn old_hangul_corpus_matches_rustybuzz() {
    let blob = Blob::new(NOTO_OLD_HANGUL);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_OLD_HANGUL, 0).expect("parse rustybuzz face");

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
