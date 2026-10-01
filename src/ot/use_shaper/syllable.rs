//! USE syllable segmentation: the syllable record and the greedy
//! parser that splits a run into consonant, vowel, symbol and broken
//! syllables.

use alloc::vec::Vec;

use crate::unicode::use_category::{use_category, UseCategory};

/// Classification of one USE syllable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyllableKind {
    /// Consonant-based syllable, the common case.
    Consonant,
    /// Vowel syllable: starts with an independent vowel (IV).
    Vowel,
    /// A single symbol / number / generic-base pass-through. The
    /// state machine should not reorder these.
    Symbol,
    /// A broken syllable: codepoint we could not fit into any
    /// grammar production. Emitted as a one-wide unit so the
    /// segmenter always advances.
    Broken,
}

/// One USE syllable's footprint in the codepoint run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Syllable {
    pub kind: SyllableKind,
    /// Start codepoint/glyph index (inclusive).
    pub start: usize,
    /// End codepoint/glyph index (exclusive).
    pub end: usize,
    /// Codepoint-space index of the base consonant inside the
    /// syllable, or `None` for non-consonant syllables.
    pub base_index: Option<usize>,
    /// Codepoint-space index of a Myanmar kinzi prefix: the triple
    /// `Nga (U+1004) + Asat (U+103A) + Virama (U+1039)` at the start
    /// of a consonant syllable. When present, those three glyphs move
    /// to immediately after the base before the rphf feature fires,
    /// so the collapsed kinzi glyph sits in the reph slot (after
    /// the base consonant in logical order). Matches rustybuzz's
    /// `initial_reordering_consonant_syllable` POS_AFTER_MAIN path.
    pub kinzi_index: Option<usize>,
}

/// Splits the codepoint run into USE syllables using a simple greedy
/// parser over [`UseCategory`]. Each iteration either matches one
/// of the known grammar productions or emits a one-wide Broken
/// syllable so the outer loop terminates.
pub(crate) fn segment_syllables(codepoints: &[char]) -> Vec<Syllable> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < codepoints.len() {
        let syl = scan_one_syllable(codepoints, i);
        i = syl.end;
        out.push(syl);
    }
    out
}

/// Parses a single syllable starting at `start`, which must be below
/// `cps.len()`. Always makes progress and stays in bounds:
/// `start < end <= cps.len()` on return.
fn scan_one_syllable(cps: &[char], start: usize) -> Syllable {
    let first = use_category(cps[start]);

    match first {
        UseCategory::B => scan_consonant_syllable(cps, start),
        UseCategory::IV => scan_vowel_syllable(cps, start),
        // A generic base that marks attach to (U+25CC DOTTED CIRCLE,
        // typed or inserted for a broken syllable) anchors a syllable
        // like a consonant does, as in HarfBuzz's USE grammar.
        UseCategory::GB
            if cps
                .get(start + 1)
                .is_some_and(|&c| !matches!(use_category(c), UseCategory::B | UseCategory::GB)) =>
        {
            let syl = scan_consonant_syllable(cps, start);
            if syl.end > start + 1 {
                syl
            } else {
                Syllable {
                    kind: SyllableKind::Symbol,
                    start,
                    end: start + 1,
                    base_index: None,
                    kinzi_index: None,
                }
            }
        }
        UseCategory::GB | UseCategory::N | UseCategory::S => {
            // One-wide Symbol syllable. Runs of digits or generic
            // bases are kept as separate syllables so each keeps
            // its own cluster id after the merge pass, matching
            // rustybuzz, where e.g. the three Khmer digits ០១២
            // emit clusters 0/3/6 rather than a single merged 0.
            Syllable {
                kind: SyllableKind::Symbol,
                start,
                end: start + 1,
                base_index: None,
                kinzi_index: None,
            }
        }
        UseCategory::R if start + 1 < cps.len() => {
            // Repha prefix, followed by a consonant syllable. The
            // Myanmar kinzi case is handled inline in
            // `scan_consonant_syllable` because kinzi's codepoints
            // (Nga / Asat / Virama) are categorized as B/H/H, not R.
            // No codepoint maps to R today. A repha at the very end
            // of the run falls through to the one-wide Broken arm so
            // the syllable never runs past `cps.len()`.
            let syl = scan_consonant_syllable(cps, start + 1);
            Syllable {
                kind: syl.kind,
                start,
                end: syl.end,
                base_index: syl.base_index,
                kinzi_index: syl.kinzi_index,
            }
        }
        UseCategory::ZWJ | UseCategory::ZWNJ | UseCategory::WS | UseCategory::O => Syllable {
            kind: SyllableKind::Symbol,
            start,
            end: start + 1,
            base_index: None,
            kinzi_index: None,
        },
        _ => Syllable {
            kind: SyllableKind::Broken,
            start,
            end: start + 1,
            base_index: None,
            kinzi_index: None,
        },
    }
}

/// Matches a consonant syllable:
///
/// ```text
///   B (H B)* (VPre | VAbv | VBlw | VPst)* M* FM* VS?
/// ```
fn scan_consonant_syllable(cps: &[char], start: usize) -> Syllable {
    let len = cps.len();
    let mut i = start;

    // Myanmar kinzi prefix: `Nga (U+1004) + Asat (U+103A) + Virama
    // (U+1039)` at the syllable head. Consume the triple up front
    // and remember its position so `initial_reorder` can move it
    // to POS_AFTER_MAIN once we know the base consonant index.
    // Matches the first branch of rustybuzz's
    // `initial_reordering_consonant_syllable` (the `Ra + As + H`
    // check). Leaves the outer grammar intact: after the kinzi
    // triple we still require a leading base consonant.
    let kinzi_index: Option<usize> = if i + 3 <= len
        && cps[i] == '\u{1004}'
        && cps[i + 1] == '\u{103A}'
        && cps[i + 2] == '\u{1039}'
        && i + 3 < len
        && matches!(use_category(cps[i + 3]), UseCategory::B | UseCategory::GB)
    {
        let kz = i;
        i += 3;
        Some(kz)
    } else {
        None
    };

    // Required leading base.
    let mut base_index: Option<usize> =
        if i < len && matches!(use_category(cps[i]), UseCategory::B | UseCategory::GB) {
            i += 1;
            Some(i - 1)
        } else {
            // Degenerate case: caller routed us here with a non-B
            // first codepoint. Emit Broken so the outer loop advances.
            return Syllable {
                kind: SyllableKind::Broken,
                start,
                end: start + 1,
                base_index: None,
                kinzi_index: None,
            };
        };

    // Zero or more halant-consonant pairs (conjunct stacks). The last
    // base is the visible consonant. Earlier bases become subscripts
    // through the `blwf` and `pstf` GSUB features.
    while i + 1 < len
        && use_category(cps[i]) == UseCategory::H
        && matches!(use_category(cps[i + 1]), UseCategory::B | UseCategory::GB)
    {
        base_index = Some(i + 1);
        i += 2;
    }

    // Trailing vowel signs and marks. Order the grammar is lenient
    // about: we accept any interleaving of V* / M* / FM* because
    // the reorder pass handles positions explicitly.
    while i < len {
        match use_category(cps[i]) {
            UseCategory::VPre
            | UseCategory::VAbv
            | UseCategory::VBlw
            | UseCategory::VPst
            | UseCategory::M
            | UseCategory::FM
            | UseCategory::CM
            | UseCategory::VS
            | UseCategory::ZWJ
            | UseCategory::ZWNJ => {
                i += 1;
            }
            // A trailing halant without a following base ends the
            // syllable. Consume and stop.
            UseCategory::H => {
                i += 1;
                break;
            }
            _ => break,
        }
    }

    Syllable {
        kind: SyllableKind::Consonant,
        start,
        end: i,
        base_index,
        kinzi_index,
    }
}

/// Matches a vowel-led syllable: independent vowel + optional
/// trailing marks.
fn scan_vowel_syllable(cps: &[char], start: usize) -> Syllable {
    let len = cps.len();
    let mut i = start + 1;
    while i < len {
        match use_category(cps[i]) {
            UseCategory::VAbv
            | UseCategory::VBlw
            | UseCategory::VPst
            | UseCategory::M
            | UseCategory::FM
            | UseCategory::CM
            | UseCategory::VS => {
                i += 1;
            }
            _ => break,
        }
    }
    Syllable {
        kind: SyllableKind::Vowel,
        start,
        end: i,
        base_index: Some(start),
        kinzi_index: None,
    }
}
