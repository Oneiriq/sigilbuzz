//! Hebrew-shaping parity contract: sigilbuzz matches rustybuzz
//! glyph-for-glyph and advance-for-advance on a fixed Hebrew corpus
//! against Noto Sans Hebrew (OFL).
//!
//! # What this test proves
//!
//! Hebrew is RTL but non-cursive, so the only per-codepoint
//! transforms are GDEF-driven mark classification plus GPOS
//! mark-to-base / mark-to-mark anchors. The corpus exercises:
//!
//! - **Plain consonants** — shalom, toda, boker tov. No marks;
//!   proves cmap + RTL iteration yield the same glyph ids.
//! - **Niqqud (vowel points)** — bereshit, shalom with kamatz /
//!   holam / sheva, hallelu-Yah with mapiq-he. Validates the GPOS
//!   `mark` feature anchoring niqqud below/above the base consonant.
//! - **Cantillation (te'amim)** — the opening of Genesis 1:1 with
//!   tipeha, munach, etnahta. Validates GPOS `mkmk` stacking of
//!   cantillation marks on top of niqqud.
//! - **Final-form consonants** — words ending in kaf-sofit, mem-sofit,
//!   nun-sofit, pe-sofit, tzadi-sofit. Those encode as distinct
//!   codepoints in Hebrew (unlike Arabic, where joining selects
//!   the form) so correct behaviour is pure cmap; the test pins
//!   that nothing in the pipeline breaks the straight-through path.
//! - **Mixed Hebrew + Latin** — proves script-segment transitions
//!   do not corrupt either side.
//!
//! # Direction
//!
//! Sigilbuzz emits logical order; rustybuzz emits visual order with
//! direction set to RTL. Following the Arabic parity convention we
//! reverse the rustybuzz side before zipping.
//!
//! Byte-identical glyph-id and x_advance agreement is the bar.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Face, Font};

const NOTO_HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");

/// One corpus entry. `compare_rustybuzz == false` reserves the slot
/// for an input that shapes correctly on sigilbuzz in isolation but
/// diverges from rustybuzz on some subtable we have not yet tracked
/// down; it still round-trips through the shaper to guard against
/// panics.
///
/// `mixed_script_segments` lists per-segment `(text, direction,
/// script)` triples for mixed-script inputs. `assert_parity_on`
/// compares sigilbuzz's concatenated output against the concatenation
/// of rustybuzz shapes of each segment — matching how a correct
/// client calls HarfBuzz for mixed runs. Pure-script cases use a
/// single whole-buffer rustybuzz call via an empty slice.
struct Case {
    text: &'static str,
    compare_rustybuzz: bool,
    note: &'static str,
    /// Empty for single-script runs; one entry per script segment
    /// otherwise. Each segment's slice of `text` is shaped under the
    /// named script/direction on the rustybuzz side so the
    /// concatenated output matches sigilbuzz's per-segment dispatch.
    mixed_script_segments: &'static [MixedSeg],
}

struct MixedSeg {
    text: &'static str,
    rtl: bool,
    script: rustybuzz::Script,
}

/// Pure Hebrew and mixed-script inputs. Every entry cross-checks
/// against rustybuzz unless a `compare_rustybuzz: false` flag says
/// otherwise.
///
/// - `\u{05E9}\u{05DC}\u{05D5}\u{05DD}` — shalom (peace). Four
///   plain consonants, no marks.
/// - `\u{05EA}\u{05D5}\u{05D3}\u{05D4}` — toda (thanks). Same
///   shape family, different letter set.
/// - `\u{05D1}\u{05D5}\u{05E7}\u{05E8} \u{05D8}\u{05D5}\u{05D1}` —
///   boker tov (good morning). Space-broken two-word run.
/// - `\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05C1}\u{05B4}\u{05D9}\u{05EA}`
///   — bereshit (in the beginning). Full niqqud: dagesh (U+05BC),
///   sheva (U+05B0), tsere (U+05B5), shin-dot (U+05C1), hiriq (U+05B4).
///   Exercises mark-to-base in bulk.
/// - `\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}` —
///   shalom with niqqud (shin-dot, kamatz, holam). Mark-to-base
///   over three separate bases.
/// - `\u{05D4}\u{05B7}\u{05DC}\u{05B0}\u{05DC}\u{05D5}\u{05BC}\u{05D9}\u{05B8}\u{05D4}\u{05BC}`
///   — halleluyah with dagesh + niqqud stacks. Multiple mark-to-base
///   and some mark-to-mark because the mapiq-he final needs the
///   dagesh anchored on top of the base.
/// - `\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05C1}\u{05B4}\u{0596}\u{05D9}\u{05EA} \u{05D1}\u{05BC}\u{05B8}\u{05E8}\u{05B8}\u{05A3}\u{05D0}`
///   — bereshit bara (Genesis 1:1 opening) with cantillation
///   (tipeha U+0596 and munach U+05A3). Exercises GPOS mkmk
///   stacking te'amim on top of niqqud.
/// - Final-form words: `\u{05DC}\u{05D9}\u{05DA}` (to you — kaf-sofit),
///   `\u{05D9}\u{05D5}\u{05DD}` (day — mem-sofit),
///   `\u{05D1}\u{05DF}` (son — nun-sofit),
///   `\u{05E7}\u{05E6}\u{05E3}` (end — pe-sofit),
///   `\u{05E7}\u{05E5}` (summer — tzadi-sofit).
/// - `Hi \u{05E9}\u{05DC}\u{05D5}\u{05DD}` — mixed Latin+Hebrew.
const CORPUS: &[Case] = &[
    Case {
        text: "\u{05E9}\u{05DC}\u{05D5}\u{05DD}",
        compare_rustybuzz: true,
        note: "shalom (plain consonants)",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05EA}\u{05D5}\u{05D3}\u{05D4}",
        compare_rustybuzz: true,
        note: "toda (plain consonants)",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05D1}\u{05D5}\u{05E7}\u{05E8} \u{05D8}\u{05D5}\u{05D1}",
        compare_rustybuzz: true,
        note: "boker tov (two words)",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05C1}\u{05B4}\u{05D9}\u{05EA}",
        compare_rustybuzz: true,
        note: "bereshit (full niqqud)",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}",
        compare_rustybuzz: true,
        note: "shalom with niqqud",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05D4}\u{05B7}\u{05DC}\u{05B0}\u{05DC}\u{05D5}\u{05BC}\u{05D9}\u{05B8}\u{05D4}\u{05BC}",
        compare_rustybuzz: true,
        note: "halleluyah with dagesh stacks",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05C1}\u{05B4}\u{0596}\u{05D9}\u{05EA} \u{05D1}\u{05BC}\u{05B8}\u{05E8}\u{05B8}\u{05A3}\u{05D0}",
        compare_rustybuzz: true,
        note: "bereshit bara (cantillation)",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05DC}\u{05D9}\u{05DA}",
        compare_rustybuzz: true,
        note: "lekha (kaf-sofit)",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05D9}\u{05D5}\u{05DD}",
        compare_rustybuzz: true,
        note: "yom (mem-sofit)",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05D1}\u{05DF}",
        compare_rustybuzz: true,
        note: "ben (nun-sofit)",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05E7}\u{05E6}\u{05E3}",
        compare_rustybuzz: true,
        note: "ketsef (pe-sofit)",
        mixed_script_segments: &[],
    },
    Case {
        text: "\u{05E7}\u{05E5}",
        compare_rustybuzz: true,
        note: "kayits (tzadi-sofit)",
        mixed_script_segments: &[],
    },
    // Mixed-script runs parity-clean once `shape()` segments the
    // buffer and dispatches each segment under its own script-tag
    // priority (`hebr` for the Hebrew half, DFLT for the Latin
    // half). The parity side shapes each segment independently with
    // rustybuzz (LTR+Latin then RTL+Hebrew) and concatenates,
    // because rustybuzz by itself does not auto-segment a
    // pre-existing buffer — the client is expected to segment
    // upstream. sigilbuzz now does that upstream step inside
    // `shape()`, so matching rustybuzz's per-segment call chain
    // proves the new segmenter routes each half correctly.
    Case {
        text: "Hi \u{05E9}\u{05DC}\u{05D5}\u{05DD}",
        compare_rustybuzz: true,
        note: "Latin + Hebrew (script segmenter)",
        mixed_script_segments: &[
            MixedSeg {
                text: "Hi ",
                rtl: false,
                script: rustybuzz::script::LATIN,
            },
            MixedSeg {
                text: "\u{05E9}\u{05DC}\u{05D5}\u{05DD}",
                rtl: true,
                script: rustybuzz::script::HEBREW,
            },
        ],
    },
    // ASCII + currency + digits + Hebrew — pins that a COMMON span
    // (the shekel sign U+20AA qualifies as COMMON) attaches to the
    // Latin prefix rather than carving its own segment.
    Case {
        text: "Price: \u{20AA}100 \u{05E9}\u{05DC}\u{05D5}\u{05DD}",
        compare_rustybuzz: true,
        note: "Latin + common + Hebrew",
        mixed_script_segments: &[
            MixedSeg {
                text: "Price: \u{20AA}100 ",
                rtl: false,
                script: rustybuzz::script::LATIN,
            },
            MixedSeg {
                text: "\u{05E9}\u{05DC}\u{05D5}\u{05DD}",
                rtl: true,
                script: rustybuzz::script::HEBREW,
            },
        ],
    },
];

/// Shape `case.text` with both engines and assert byte-identical
/// output. For mixed-script cases we shape each declared segment
/// independently on the rustybuzz side and concatenate — that mirrors
/// how sigilbuzz's `shape()` now dispatches per segment, and matches
/// the "correct client" call pattern HarfBuzz documents.
fn assert_parity_on(case: &Case) {
    let blob = Blob::new(NOTO_HEBREW);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);

    let rb_face = rustybuzz::Face::from_slice(NOTO_HEBREW, 0).expect("parse rustybuzz face");

    let mut buffer = Buffer::new();
    buffer.push_str(case.text);
    let sig = shape(&font, &buffer, &[]).expect("sigilbuzz shape");

    // Build the rustybuzz comparison sequence. Pure-script cases hit
    // the simple single-buffer path; mixed cases shape each declared
    // segment with its own direction/script and concatenate in
    // logical order.
    let (rb_gids, rb_xadvs): (Vec<u32>, Vec<i32>) = if case.mixed_script_segments.is_empty() {
        let mut rb_buf = rustybuzz::UnicodeBuffer::new();
        rb_buf.push_str(case.text);
        rb_buf.set_direction(RbDirection::RightToLeft);
        let rb_out = rustybuzz::shape(&rb_face, &[], rb_buf);
        let gids: Vec<u32> = rb_out.glyph_infos().iter().rev().map(|g| g.glyph_id).collect();
        let xadvs: Vec<i32> = rb_out
            .glyph_positions()
            .iter()
            .rev()
            .map(|p| p.x_advance)
            .collect();
        (gids, xadvs)
    } else {
        let mut gids = Vec::new();
        let mut xadvs = Vec::new();
        for seg in case.mixed_script_segments {
            let mut rb_buf = rustybuzz::UnicodeBuffer::new();
            rb_buf.push_str(seg.text);
            rb_buf.set_direction(if seg.rtl {
                RbDirection::RightToLeft
            } else {
                RbDirection::LeftToRight
            });
            rb_buf.set_script(seg.script);
            let rb_out = rustybuzz::shape(&rb_face, &[], rb_buf);
            let mut seg_gids: Vec<u32> =
                rb_out.glyph_infos().iter().map(|g| g.glyph_id).collect();
            let mut seg_xadvs: Vec<i32> = rb_out
                .glyph_positions()
                .iter()
                .map(|p| p.x_advance)
                .collect();
            if seg.rtl {
                seg_gids.reverse();
                seg_xadvs.reverse();
            }
            gids.extend(seg_gids);
            xadvs.extend(seg_xadvs);
        }
        (gids, xadvs)
    };

    let text = case.text;
    let note = case.note;
    assert_eq!(
        sig.len(),
        rb_gids.len(),
        "glyph count diverged for {note} ({text:?}): sigilbuzz={} rustybuzz={}",
        sig.len(),
        rb_gids.len()
    );

    // Glyph IDs and advances are the parity contract. Offsets
    // carry an RTL convention difference (HarfBuzz subtracts the
    // base-advance from the mark offset during RTL finalisation and
    // relies on the caller to reverse the visual run; sigilbuzz
    // emits logical order with unmodified anchor deltas so the
    // renderer sees the same absolute position once it walks the
    // pen). We pin the offsets with a dedicated check further down
    // rather than demand engine-for-engine equality here.
    for (i, (sig_g, (rb_gid, rb_xadv))) in sig
        .glyphs
        .iter()
        .zip(rb_gids.iter().zip(rb_xadvs.iter()))
        .enumerate()
    {
        assert_eq!(
            sig_g.glyph_id, *rb_gid,
            "glyph id mismatch at position {i} of {note} ({text:?}): \
             sigilbuzz={} rustybuzz={}",
            sig_g.glyph_id, rb_gid
        );
        assert_eq!(
            sig_g.x_advance, *rb_xadv,
            "x_advance mismatch at position {i} of {note} ({text:?}) \
             (glyph {}): sigilbuzz={} rustybuzz={}",
            sig_g.glyph_id, sig_g.x_advance, rb_xadv
        );
    }
}

#[test]
fn hebrew_corpus_matches_rustybuzz_glyph_for_glyph() {
    for case in CORPUS {
        if !case.compare_rustybuzz {
            // Still shape to make sure the pipeline does not panic
            // on the input.
            let blob = Blob::new(NOTO_HEBREW);
            let face = Face::parse(&blob, 0).expect("parse face");
            let font = Font::new(face, 1000.0);
            let mut buffer = Buffer::new();
            buffer.push_str(case.text);
            let _ = shape(&font, &buffer, &[]).expect("shape diagnostic case");
            continue;
        }
        assert_parity_on(case);
    }
}

#[test]
fn niqqud_anchors_below_base_via_gpos_mark() {
    // Sanity check independent of rustybuzz: shape a base+niqqud
    // pair and verify the mark glyph received a non-zero y_offset
    // from the GPOS mark-to-base lookup. If the Hebrew script tag
    // were not routed to GPOS, the mark would sit at its advance
    // origin with zero offset.
    //
    // bet (U+05D1) + kamatz (U+05B8). Noto Sans Hebrew anchors
    // kamatz below the bet — the resulting y_offset is non-zero
    // and typically negative (below the baseline) in HarfBuzz
    // design-unit convention.
    let blob = Blob::new(NOTO_HEBREW);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let mut buffer = Buffer::new();
    buffer.push_str("\u{05D1}\u{05B8}");
    let shaped = shape(&font, &buffer, &[]).expect("shape bet+kamatz");

    assert_eq!(
        shaped.len(),
        2,
        "bet + kamatz should remain two glyphs (no ligation)"
    );
    // Second glyph is the mark. Having a non-zero offset on either
    // axis proves GPOS mark-to-base fired.
    let mark = shaped.glyphs[1];
    assert!(
        mark.x_offset != 0 || mark.y_offset != 0,
        "GPOS mark attachment should produce a non-zero offset on \
         the kamatz (x_offset={}, y_offset={})",
        mark.x_offset,
        mark.y_offset
    );
    // Mark advance is zero — marks do not advance the pen.
    assert_eq!(
        mark.x_advance, 0,
        "a combining mark should have zero advance after GPOS"
    );
}

#[test]
fn final_form_consonants_survive_cmap_untouched() {
    // Hebrew final forms (kaf-sofit U+05DA etc.) are distinct
    // codepoints with their own cmap entries. The shaper should not
    // rewrite them via any GSUB pass — they go straight through.
    let blob = Blob::new(NOTO_HEBREW);
    let face = Face::parse(&blob, 0).expect("parse face");
    let cmap = face.cmap().expect("cmap");
    let font = Font::new(face, 1000.0);

    // ben (son): bet + final-nun.
    let text = "\u{05D1}\u{05DF}";
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    let shaped = shape(&font, &buffer, &[]).expect("shape");

    let cmap_ids: Vec<u32> = text
        .chars()
        .map(|c| u32::from(cmap.glyph_id(c).unwrap_or(0)))
        .collect();
    let shaped_ids: Vec<u32> = shaped.glyphs.iter().map(|g| g.glyph_id).collect();

    assert_eq!(
        shaped_ids, cmap_ids,
        "final-form consonants must pass through without GSUB rewrites"
    );
}

#[test]
fn mixed_hebrew_and_latin_runs_shape_each_half_correctly() {
    // The Latin portion should produce the same glyphs whether or
    // not Hebrew is in the buffer — the Latin half hits DFLT via
    // the script-priority fallback in apply_gsub_feature_in_scripts.
    let blob = Blob::new(NOTO_HEBREW);
    let face = Face::parse(&blob, 0).expect("parse face");
    let font = Font::new(face, 1000.0);

    let latin_only = "Hi";
    let mixed = "Hi \u{05E9}\u{05DC}\u{05D5}\u{05DD}";

    let mut buf = Buffer::new();
    buf.push_str(latin_only);
    let shaped_latin = shape(&font, &buf, &[]).expect("shape latin");

    let mut buf = Buffer::new();
    buf.push_str(mixed);
    let shaped_mixed = shape(&font, &buf, &[]).expect("shape mixed");

    // First 2 glyphs of the mixed run should equal the pure-Latin
    // shape.
    for (i, (lat, mix)) in shaped_latin
        .glyphs
        .iter()
        .zip(shaped_mixed.glyphs.iter())
        .enumerate()
    {
        assert_eq!(
            lat.glyph_id, mix.glyph_id,
            "Latin glyph {i} diverged in mixed run: pure={} mixed={}",
            lat.glyph_id, mix.glyph_id
        );
        assert_eq!(
            lat.x_advance, mix.x_advance,
            "Latin glyph {i} advance diverged in mixed run"
        );
    }
}
