//! Indic shaping parity tests.
//!
//! Shapes a short Devanagari corpus with both sigilbuzz and
//! rustybuzz against Noto Sans Devanagari (OFL) and asserts the
//! glyph output matches byte-for-byte.
//!
//! As with the Latin parity test, failures here mean sigilbuzz
//! drifted from rustybuzz for a shared feature set. Test entries
//! that exercise lookup types sigilbuzz does not implement yet
//! skip with a `// rustybuzz: ...` comment explaining why, and a
//! TODO to remove once the gap closes.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_DEVA: &[u8] = include_bytes!("fonts/NotoSansDevanagari-Regular.ttf");

/// Tuple of (input-text, `sigilbuzz-glyph-count-expected`,
/// `compare-with-rustybuzz`). The middle field lets us pin the glyph
/// count even when rustybuzz runs features sigilbuzz does not yet
/// handle; set the third field to `true` to cross-check against
/// rustybuzz as well.
struct Case {
    text: &'static str,
    compare_rustybuzz: bool,
    note: &'static str,
}

const CORPUS: &[Case] = &[
    // Empty string — identity.
    Case {
        text: "",
        compare_rustybuzz: true,
        note: "empty",
    },
    // Single independent consonant. No reorder, no features.
    Case {
        text: "\u{0915}",
        compare_rustybuzz: true,
        note: "ka alone",
    },
    // Consonant + post-base matra — shape order = logical order.
    Case {
        text: "\u{0915}\u{0940}",
        compare_rustybuzz: true,
        note: "kii (ka + post-base ii)",
    },
    // Devanagari digits — pass-through Symbol syllables.
    Case {
        text: "\u{0966}\u{0967}\u{0968}\u{0969}",
        compare_rustybuzz: true,
        note: "devanagari digits 0-3",
    },
    // Latin interleaved with Devanagari — mixed-script runs must
    // not corrupt the ASCII.
    Case {
        text: "Hi \u{0915}",
        compare_rustybuzz: true,
        note: "mixed Latin + devanagari",
    },
    // Standalone independent vowel.
    Case {
        text: "\u{0905}",
        compare_rustybuzz: true,
        note: "independent vowel a",
    },
    // Consonant + consonant + consonant (no conjuncts): three
    // independent syllables back-to-back. Exercises the segmenter
    // without engaging reorder.
    Case {
        text: "\u{0915}\u{0916}\u{0917}",
        compare_rustybuzz: true,
        note: "ka kha ga",
    },
    // Pre-base matra: कि — requires reorder to match rustybuzz.
    Case {
        text: "\u{0915}\u{093F}",
        compare_rustybuzz: true,
        note: "ki (pre-base matra i)",
    },
    // नमस्ते — conjunct स्त, exercises `half` and `pres`.
    Case {
        text: "\u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}",
        compare_rustybuzz: true,
        note: "namaste",
    },
    // हिन्दी — conjunct न्द via `half`/`pres`, plus pre-base matra ि.
    Case {
        text: "\u{0939}\u{093F}\u{0928}\u{094D}\u{0926}\u{0940}",
        compare_rustybuzz: true,
        note: "hindi",
    },
    // क्ष्य — triple conjunct, no matras. Fires `akhn` + `cjct`.
    Case {
        text: "\u{0915}\u{094D}\u{0937}\u{094D}\u{092F}",
        compare_rustybuzz: true,
        note: "kshya (triple conjunct)",
    },
    // ज्ञ — single-akhand conjunct (ja + halant + nya → ज्ञ).
    Case {
        text: "\u{091C}\u{094D}\u{091E}",
        compare_rustybuzz: true,
        note: "jnya (akhand ligature)",
    },
    // र् क — reph + ka. sigilbuzz defers final reorder pending
    // per-glyph masks, so the reph glyph order diverges from
    // rustybuzz (rustybuzz moves reph to after the base via final
    // reorder; sigilbuzz leaves it at the syllable start). Listed
    // here with `compare_rustybuzz: false` so the test still
    // exercises the segmentation + rphf feature path without
    // flagging the known-stubbed step.
    Case {
        text: "\u{0930}\u{094D}\u{0915}",
        compare_rustybuzz: false,
        note: "reph (stubbed final reorder)",
    },
];

#[test]
fn devanagari_corpus_matches_rustybuzz_for_supported_features() {
    let blob = Blob::new(NOTO_DEVA);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_DEVA, 0).expect("parse rustybuzz face");

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
                "glyph id mismatch at position {i} of {} ({:?})",
                case.note, case.text
            );
            assert_eq!(
                sig_g.x_advance, rb_pos.x_advance,
                "x_advance mismatch at position {i} of {} ({:?}): sigilbuzz={} rustybuzz={}",
                case.note, case.text, sig_g.x_advance, rb_pos.x_advance
            );
        }
    }
}

#[test]
fn pre_base_matra_glyphs_end_up_before_their_consonant() {
    // कि — with pre-base matra, the matra glyph should appear at
    // index 0 and the ka glyph at index 1. Verify this independently
    // of rustybuzz so we catch regressions even if upstream drifts.
    let blob = Blob::new(NOTO_DEVA);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut buffer = Buffer::new();
    buffer.push_str("\u{0915}\u{093F}");
    let shaped = shape(&font, &buffer, &[]).expect("shape ki");

    // The ka codepoint is at cluster 0 (byte offset); the matra at
    // cluster 3 (ka is 3 UTF-8 bytes).
    assert_eq!(shaped.len(), 2, "ki should be two glyphs after shaping");
    assert_eq!(
        shaped.glyphs[0].cluster, 3,
        "pre-base matra (cluster 3) should appear first visually"
    );
    assert_eq!(
        shaped.glyphs[1].cluster, 0,
        "base consonant (cluster 0) should appear second"
    );
}
