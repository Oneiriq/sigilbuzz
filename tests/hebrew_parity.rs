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
//! - **Plain consonants**: shalom, toda, boker tov. No marks;
//!   proves cmap + RTL iteration yield the same glyph ids.
//! - **Niqqud (vowel points)**: bereshit, shalom with kamatz /
//!   holam / sheva, hallelu-Yah with mapiq-he. Validates the GPOS
//!   `mark` feature anchoring niqqud below/above the base consonant.
//! - **Cantillation (te'amim)**: the opening of Genesis 1:1 with
//!   tipeha, munach, etnahta. Validates GPOS `mkmk` stacking of
//!   cantillation marks on top of niqqud.
//! - **Final-form consonants**: words ending in kaf-sofit, mem-sofit,
//!   nun-sofit, pe-sofit, tzadi-sofit. Those encode as distinct
//!   codepoints in Hebrew (unlike Arabic, where joining selects
//!   the form) so correct behavior is pure cmap; the test pins
//!   that nothing in the pipeline breaks the straight-through path.
//! - **Mixed Hebrew + Latin**: proves script-segment transitions
//!   do not corrupt either side.
//!
//! # Direction
//!
//! Both engines shape with the direction set to RTL and both return
//! visual order (leftmost glyph first), so the outputs are compared
//! position by position with no reversal. Mark offsets use the RTL
//! attachment convention in both engines.
//!
//! Byte-identical glyph id, advance, and x/y offset agreement is the
//! bar.

use rustybuzz::Direction as RbDirection;
use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Font};

const NOTO_HEBREW: &[u8] = include_bytes!("fonts/NotoSansHebrew-Regular.ttf");

/// One corpus entry. `compare_rustybuzz == false` reserves the slot
/// for an input that shapes correctly on sigilbuzz in isolation but
/// diverges from rustybuzz on some subtable we have not yet tracked
/// down; it still round-trips through the shaper to guard against
/// panics.
///
/// `mixed_script_segments` lists per-segment `(text, script)` pairs
/// for mixed-script inputs. `assert_parity_on` compares sigilbuzz's
/// output against rustybuzz shapes of each segment, matching how a
/// correct client calls HarfBuzz for mixed runs. Pure-script cases
/// use a single whole-buffer rustybuzz call via an empty slice.
struct Case {
    text: &'static str,
    compare_rustybuzz: bool,
    note: &'static str,
    /// Empty for single-script runs; one entry per script segment
    /// otherwise, in logical order. Each segment's slice of `text` is
    /// shaped RTL under the named script on the rustybuzz side, so the
    /// segments' visual outputs, concatenated last segment first,
    /// match sigilbuzz's per-segment dispatch of one RTL buffer.
    mixed_script_segments: &'static [MixedSeg],
}

struct MixedSeg {
    text: &'static str,
    script: rustybuzz::Script,
}

/// One shaped glyph reduced to the fields both engines report:
/// `(glyph_id, x_advance, y_advance, x_offset, y_offset)`.
type Pos = (u32, i32, i32, i32, i32);

fn sigilbuzz_positions(text: &str, direction: Direction) -> Vec<Pos> {
    let blob = Blob::new(NOTO_HEBREW);
    let face = Face::parse(&blob, 0).expect("parse sigilbuzz face");
    let font = Font::new(face, 1000.0);
    let mut buffer = Buffer::new();
    buffer.set_direction(direction);
    buffer.push_str(text);
    let run = shape(&font, &buffer, &[]).expect("sigilbuzz shape");
    run.glyphs
        .iter()
        .map(|g| (g.glyph_id, g.x_advance, g.y_advance, g.x_offset, g.y_offset))
        .collect()
}

fn rustybuzz_positions(text: &str, direction: RbDirection, script: rustybuzz::Script) -> Vec<Pos> {
    let rb_face = rustybuzz::Face::from_slice(NOTO_HEBREW, 0).expect("parse rustybuzz face");
    let mut rb_buf = rustybuzz::UnicodeBuffer::new();
    rb_buf.push_str(text);
    rb_buf.set_direction(direction);
    rb_buf.set_script(script);
    let out = rustybuzz::shape(&rb_face, &[], rb_buf);
    out.glyph_infos()
        .iter()
        .zip(out.glyph_positions())
        .map(|(i, p)| (i.glyph_id, p.x_advance, p.y_advance, p.x_offset, p.y_offset))
        .collect()
}

/// Pure Hebrew and mixed-script inputs. Every entry cross-checks
/// against rustybuzz unless a `compare_rustybuzz: false` flag says
/// otherwise.
///
/// - `\u{05E9}\u{05DC}\u{05D5}\u{05DD}`: shalom (peace). Four
///   plain consonants, no marks.
/// - `\u{05EA}\u{05D5}\u{05D3}\u{05D4}`: toda (thanks). Same
///   shape family, different letter set.
/// - `\u{05D1}\u{05D5}\u{05E7}\u{05E8} \u{05D8}\u{05D5}\u{05D1}`:
///   boker tov (good morning). Space-broken two-word run.
/// - `\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05C1}\u{05B4}\u{05D9}\u{05EA}`
///   is bereshit (in the beginning). Full niqqud: dagesh (U+05BC),
///   sheva (U+05B0), tsere (U+05B5), shin-dot (U+05C1), hiriq (U+05B4).
///   Exercises mark-to-base in bulk.
/// - `\u{05E9}\u{05C1}\u{05B8}\u{05DC}\u{05D5}\u{05B9}\u{05DD}`:
///   shalom with niqqud (shin-dot, kamatz, holam). Mark-to-base
///   over three separate bases.
/// - `\u{05D4}\u{05B7}\u{05DC}\u{05B0}\u{05DC}\u{05D5}\u{05BC}\u{05D9}\u{05B8}\u{05D4}\u{05BC}`
///   is halleluyah with dagesh + niqqud stacks. Multiple mark-to-base
///   and some mark-to-mark because the mapiq-he final needs the
///   dagesh anchored on top of the base.
/// - `\u{05D1}\u{05BC}\u{05B0}\u{05E8}\u{05B5}\u{05D0}\u{05E9}\u{05C1}\u{05B4}\u{0596}\u{05D9}\u{05EA} \u{05D1}\u{05BC}\u{05B8}\u{05E8}\u{05B8}\u{05A3}\u{05D0}`
///   is bereshit bara (Genesis 1:1 opening) with cantillation
///   (tipeha U+0596 and munach U+05A3). Exercises GPOS mkmk
///   stacking te'amim on top of niqqud.
/// - Final-form words: `\u{05DC}\u{05D9}\u{05DA}` (to you, kaf-sofit),
///   `\u{05D9}\u{05D5}\u{05DD}` (day, mem-sofit),
///   `\u{05D1}\u{05DF}` (son, nun-sofit),
///   `\u{05E7}\u{05E6}\u{05E3}` (end, pe-sofit),
///   `\u{05E7}\u{05E5}` (summer, tzadi-sofit).
/// - `Hi \u{05E9}\u{05DC}\u{05D5}\u{05DD}`: mixed Latin+Hebrew.
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
    // rustybuzz (RTL, Latin then Hebrew) and concatenates the visual
    // outputs last segment first, because rustybuzz by itself does
    // not auto-segment a pre-existing buffer: the client is expected
    // to segment upstream. sigilbuzz does that upstream step inside
    // `shape()`, so matching rustybuzz's per-segment call chain
    // proves the segmenter routes each half correctly, and that the
    // final reversal covers the whole run.
    Case {
        text: "Hi \u{05E9}\u{05DC}\u{05D5}\u{05DD}",
        compare_rustybuzz: true,
        note: "Latin + Hebrew (script segmenter)",
        mixed_script_segments: &[
            MixedSeg {
                text: "Hi ",
                script: rustybuzz::script::LATIN,
            },
            MixedSeg {
                text: "\u{05E9}\u{05DC}\u{05D5}\u{05DD}",
                script: rustybuzz::script::HEBREW,
            },
        ],
    },
    // ASCII + currency + digits + Hebrew: pins that a COMMON span
    // (the shekel sign U+20AA qualifies as COMMON) attaches to the
    // Latin prefix rather than carving its own segment.
    Case {
        text: "Price: \u{20AA}100 \u{05E9}\u{05DC}\u{05D5}\u{05DD}",
        compare_rustybuzz: true,
        note: "Latin + common + Hebrew",
        mixed_script_segments: &[
            MixedSeg {
                text: "Price: \u{20AA}100 ",
                script: rustybuzz::script::LATIN,
            },
            MixedSeg {
                text: "\u{05E9}\u{05DC}\u{05D5}\u{05DD}",
                script: rustybuzz::script::HEBREW,
            },
        ],
    },
];

/// Shape `case.text` with both engines in RTL and assert byte-identical
/// visual output: glyph ids, advances, and offsets. For mixed-script
/// cases we shape each declared segment independently on the
/// rustybuzz side and concatenate the visual outputs last segment
/// first. That mirrors how sigilbuzz's `shape()` dispatches per
/// segment, and matches the "correct client" call pattern HarfBuzz
/// documents.
fn assert_parity_on(case: &Case) {
    let sig = sigilbuzz_positions(case.text, Direction::Rtl);
    let rb: Vec<Pos> = if case.mixed_script_segments.is_empty() {
        let mut rb_buf = rustybuzz::UnicodeBuffer::new();
        rb_buf.push_str(case.text);
        rb_buf.set_direction(RbDirection::RightToLeft);
        let rb_face = rustybuzz::Face::from_slice(NOTO_HEBREW, 0).expect("parse rustybuzz face");
        let out = rustybuzz::shape(&rb_face, &[], rb_buf);
        out.glyph_infos()
            .iter()
            .zip(out.glyph_positions())
            .map(|(i, p)| (i.glyph_id, p.x_advance, p.y_advance, p.x_offset, p.y_offset))
            .collect()
    } else {
        case.mixed_script_segments
            .iter()
            .rev()
            .flat_map(|seg| rustybuzz_positions(seg.text, RbDirection::RightToLeft, seg.script))
            .collect()
    };

    let text = case.text;
    let note = case.note;
    assert_eq!(
        sig.len(),
        rb.len(),
        "glyph count diverged for {note} ({text:?}): sigilbuzz={} rustybuzz={}",
        sig.len(),
        rb.len()
    );
    for (i, (s, r)) in sig.iter().zip(&rb).enumerate() {
        assert_eq!(
            s, r,
            "(gid, x_adv, y_adv, x_off, y_off) mismatch at visual position {i} of \
             {note} ({text:?}): sigilbuzz={s:?} rustybuzz={r:?}"
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
fn kamatz_on_bet_offsets_match_rustybuzz_in_both_directions() {
    // bet (U+05D1) + kamatz (U+05B8). Noto Sans Hebrew anchors the
    // kamatz below the bet through GPOS mark-to-base.
    let text = "\u{05D1}\u{05B8}";

    // RTL: visual order puts the mark first. HarfBuzz's backward
    // attachment convention adds the advances after the base (only the
    // mark's own, zero) instead of subtracting the base advance.
    let rtl = sigilbuzz_positions(text, Direction::Rtl);
    let rb_rtl = rustybuzz_positions(text, RbDirection::RightToLeft, rustybuzz::script::HEBREW);
    assert_eq!(rtl, rb_rtl, "RTL bet + kamatz");
    assert_eq!(rtl.len(), 2, "no ligation");
    let (mark_rtl, base_rtl) = (rtl[0], rtl[1]);
    assert_eq!(mark_rtl.1, 0, "marks do not advance the pen");
    assert!(base_rtl.1 > 0);
    // Noto Sans Hebrew draws the kamatz below the baseline already, so
    // the anchor only moves it horizontally under the bet.
    assert_ne!(mark_rtl.3, 0, "mark-to-base must move the kamatz");

    // An explicit LTR shape of the same text keeps logical order and
    // uses the forward convention (subtract the base advance). There
    // is no rustybuzz comparison here: for a script whose native
    // direction is RTL, HarfBuzz reverses the graphemes and shapes RTL
    // when asked for LTR, which sigilbuzz does not do.
    let ltr = sigilbuzz_positions(text, Direction::Ltr);
    let (base_ltr, mark_ltr) = (ltr[0], ltr[1]);
    assert_eq!(base_ltr.0, base_rtl.0);
    assert_eq!(mark_ltr.0, mark_rtl.0);

    // Both land the mark on the same spot relative to the bet: the
    // two x offsets differ by exactly the bet's advance.
    assert_eq!(mark_rtl.3 - mark_ltr.3, base_ltr.1);
    assert_eq!(mark_rtl.4, mark_ltr.4);
}

#[test]
fn stacked_niqqud_offsets_match_rustybuzz_in_rtl() {
    // Several marks on one base (shin + shin-dot + kamatz, bet +
    // dagesh + sheva): each mark hangs from the base, so the RTL
    // compensation adds the advances of every mark between the base
    // and itself (all zero after late zeroing).
    for text in [
        "\u{05E9}\u{05C1}\u{05B8}",
        "\u{05D1}\u{05BC}\u{05B0}",
        "\u{05D4}\u{05BC}\u{05B8}\u{05D4}\u{05B7}",
    ] {
        let sig = sigilbuzz_positions(text, Direction::Rtl);
        let rb = rustybuzz_positions(text, RbDirection::RightToLeft, rustybuzz::script::HEBREW);
        assert_eq!(sig, rb, "RTL {text:?}");
        assert!(
            sig.iter().any(|p| p.3 != 0 || p.4 != 0),
            "{text:?}: at least one mark must be offset"
        );
    }
}

#[test]
fn rtl_output_is_the_reverse_of_the_logical_glyph_sequence() {
    // Same glyphs and advances as an LTR shape, just reversed, for a
    // run without marks (marks change offsets between conventions).
    let text = "\u{05E9}\u{05DC}\u{05D5}\u{05DD}";
    let ltr = sigilbuzz_positions(text, Direction::Ltr);
    let mut rtl = sigilbuzz_positions(text, Direction::Rtl);
    rtl.reverse();
    assert_eq!(ltr, rtl);
}

#[test]
fn final_form_consonants_survive_cmap_untouched() {
    // Hebrew final forms (kaf-sofit U+05DA etc.) are distinct
    // codepoints with their own cmap entries. The shaper should not
    // rewrite them via any GSUB pass. They go straight through.
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
    // not Hebrew is in the buffer. The Latin half hits DFLT via
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
