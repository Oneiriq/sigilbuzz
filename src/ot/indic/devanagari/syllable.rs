//! Indic syllable segmentation: the syllable record and the greedy
//! parser that splits a run into consonant, vowel, standalone, symbol
//! and broken syllables.

use alloc::vec::Vec;

use super::{IndicConfig, RephMode};
use crate::unicode::indic_category::{syllabic_category, IndicSyllabicCategory};

/// Syllable classification mirroring the Indic2 syllable types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyllableKind {
    /// Consonant-based syllable: the common case.
    Consonant,
    /// Vowel-based syllable: starts with an independent vowel.
    Vowel,
    /// Standalone: a sole Bindu/Visarga/placeholder + marks.
    Standalone,
    /// Symbol or pass-through: digits, dandas, OM, ...
    Symbol,
    /// Broken: an orphan matra or virama we could not fold in.
    Broken,
}

/// One syllable's footprint in the input.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Syllable {
    pub kind: SyllableKind,
    /// Start codepoint/glyph index (inclusive).
    pub start: usize,
    /// End codepoint/glyph index (exclusive).
    pub end: usize,
    /// Codepoint-space index of the base consonant, or `None` for
    /// non-consonant syllables. Indices are relative to the syllable,
    /// i.e. `start <= base_index < end`.
    pub base_index: Option<usize>,
    /// True when the syllable begins with `ra + halant` and the
    /// leading ra is a reph candidate. The reph candidate sits at
    /// `start`; the halant sits at `start + 1`.
    pub has_reph: bool,
}

/// Breaks the codepoint run into Indic syllables.
///
/// The segmenter is a forgiving greedy parser: it starts at each
/// index, consumes the longest prefix matching a syllable pattern,
/// and emits one [`Syllable`]. Codepoints that do not begin any
/// syllable pattern emit a one-wide Broken/Symbol syllable.
pub(crate) fn segment_syllables(codepoints: &[char], config: &IndicConfig) -> Vec<Syllable> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < codepoints.len() {
        let syl = scan_one_syllable(codepoints, i, config);
        i = syl.end;
        out.push(syl);
    }
    out
}

/// Parses a single syllable starting at `start`. Always makes
/// progress: the returned syllable has `end > start`.
fn scan_one_syllable(cps: &[char], start: usize, config: &IndicConfig) -> Syllable {
    let first_isc = syllabic_category(cps[start]);
    match first_isc {
        IndicSyllabicCategory::Consonant | IndicSyllabicCategory::ConsonantPlaceholder => {
            scan_consonant_syllable(cps, start, config)
        }
        IndicSyllabicCategory::VowelIndependent | IndicSyllabicCategory::Vowel => {
            scan_vowel_syllable(cps, start)
        }
        IndicSyllabicCategory::Bindu
        | IndicSyllabicCategory::Visarga
        | IndicSyllabicCategory::Avagraha => Syllable {
            kind: SyllableKind::Standalone,
            start,
            end: start + 1,
            base_index: None,
            has_reph: false,
        },
        IndicSyllabicCategory::Number | IndicSyllabicCategory::Other => {
            // Consume any run of pass-through codepoints in one go.
            let mut end = start + 1;
            while end < cps.len() {
                let c = syllabic_category(cps[end]);
                if matches!(
                    c,
                    IndicSyllabicCategory::Number | IndicSyllabicCategory::Other
                ) {
                    end += 1;
                } else {
                    break;
                }
            }
            Syllable {
                kind: SyllableKind::Symbol,
                start,
                end,
                base_index: None,
                has_reph: false,
            }
        }
        _ => Syllable {
            kind: SyllableKind::Broken,
            start,
            end: start + 1,
            base_index: None,
            has_reph: false,
        },
    }
}

/// Consumes a consonant-based syllable. Pattern (simplified):
///
/// ```text
///   [(C N?) Virama]*  C  N?  (Matra Bindu?)*  (Virama)?
/// ```
///
/// We track the base consonant index as "last consonant not
/// followed by a virama". On exit `has_reph` is true when the
/// syllable begins with `ra + halant` and at least one more
/// consonant follows (required for reph positioning), subject to the
/// config's [`RephMode`].
fn scan_consonant_syllable(cps: &[char], start: usize, config: &IndicConfig) -> Syllable {
    let mut i = start;
    let len = cps.len();

    // Head-of-syllable reph detection. Three modes:
    //
    // * [`RephMode::Implicit`]: bare `ra + halant` at position 0
    //   is a reph candidate (Devanagari / Bengali / Gurmukhi /
    //   Gujarati / Oriya / Tamil / Kannada).
    // * [`RephMode::Explicit`]: `ra + halant + ZWJ` is required
    //   (Telugu / Sinhala). The trailing ZWJ is consumed as part of
    //   the prefix and never ends up in the output of the rphf
    //   ligature.
    // * [`RephMode::LogRepha`]: a dedicated code point (Malayalam
    //   U+0D4E DOT REPH) flags the syllable as reph-bearing
    //   regardless of ra / halant.
    let implicit_ra_halant = config.reph_mode == RephMode::Implicit
        && i + 1 < len
        && cps[i] as u32 == config.ra
        && cps[i + 1] as u32 == config.virama;
    let explicit_ra_halant_zwj = config.reph_mode == RephMode::Explicit
        && i + 2 < len
        && cps[i] as u32 == config.ra
        && cps[i + 1] as u32 == config.virama
        && cps[i + 2] == '\u{200D}';
    let logrepha_prefix = config.reph_mode == RephMode::LogRepha && cps[i] == '\u{0D4E}';
    let ra_halant_prefix = implicit_ra_halant || explicit_ra_halant_zwj || logrepha_prefix;

    // Advance past a LogRepha head so the syllable machine picks up
    // the following base consonant as the syllable's base. For
    // Explicit the ZWJ sits between the halant and the base; the
    // existing (C H)+ loop below treats ZWJ as non-consonant and
    // stops, so we walk it manually here.
    if logrepha_prefix {
        i += 1;
    } else if explicit_ra_halant_zwj {
        // Skip the ZWJ after ra+halant; the head now points at the
        // base consonant. The ra+halant pair will be swallowed by
        // the (C H)+ loop below as normal.
    }

    let mut base_index: Option<usize> = None;

    // Walk consonants and halant pairs.
    loop {
        if i >= len {
            break;
        }
        let isc = syllabic_category(cps[i]);
        match isc {
            IndicSyllabicCategory::Consonant | IndicSyllabicCategory::ConsonantPlaceholder => {
                base_index = Some(i);
                i += 1;
                // Optional nukta.
                if i < len && syllabic_category(cps[i]) == IndicSyllabicCategory::Nukta {
                    i += 1;
                }
                // Optional virama: tells us this consonant is a
                // half-form / conjunct participant, not the base.
                if i < len && syllabic_category(cps[i]) == IndicSyllabicCategory::Virama {
                    i += 1;
                    // Optional ZWJ/ZWNJ after halant: requests an
                    // explicit conjunct / half-form. Consumed here so
                    // the following consonant keeps extending the
                    // C+H loop (needed for `ra + halant + ZWJ + C`
                    // under [`RephMode::Explicit`] in particular).
                    if i < len
                        && matches!(
                            syllabic_category(cps[i]),
                            IndicSyllabicCategory::Joiner | IndicSyllabicCategory::NonJoiner
                        )
                    {
                        i += 1;
                    }
                    continue;
                }
                break;
            }
            _ => break,
        }
    }

    // Trailing matras and modifier marks.
    while i < len {
        let isc = syllabic_category(cps[i]);
        match isc {
            IndicSyllabicCategory::VowelDependent
            | IndicSyllabicCategory::Bindu
            | IndicSyllabicCategory::Visarga
            | IndicSyllabicCategory::CantillationMark
            | IndicSyllabicCategory::Nukta => {
                i += 1;
            }
            IndicSyllabicCategory::Virama => {
                // A trailing virama (explicit halant at the end of a
                // syllable) is legal. It is rendered as a visible
                // virama. Consume and stop.
                i += 1;
                break;
            }
            IndicSyllabicCategory::Joiner | IndicSyllabicCategory::NonJoiner => {
                // ZWJ/ZWNJ request the preceding consonant's
                // half-form / non-conjunct behavior.
                i += 1;
            }
            _ => break,
        }
    }

    // If we never moved past `start`, we could not form a
    // consonant syllable. Emit a one-wide Broken syllable so the
    // caller advances.
    if i == start {
        return Syllable {
            kind: SyllableKind::Broken,
            start,
            end: start + 1,
            base_index: None,
            has_reph: false,
        };
    }

    // Reph is only real when the syllable has a base consonant
    // past the prefix. Otherwise the "prefix" was the whole
    // syllable and there is no base to hang the reph off.
    //
    // The minimum base offset depends on the head pattern:
    // * Implicit: `ra + halant + C`. Base must sit at >= start+2.
    // * Explicit: `ra + halant + ZWJ + C`. Base must sit at >= start+3.
    // * LogRepha: `U+0D4E + C`. Base must sit at >= start+1.
    let min_base_offset = if logrepha_prefix {
        1
    } else if explicit_ra_halant_zwj {
        3
    } else {
        2
    };
    let has_reph = ra_halant_prefix && base_index.is_some_and(|b| b >= start + min_base_offset);

    Syllable {
        kind: SyllableKind::Consonant,
        start,
        end: i,
        base_index,
        has_reph,
    }
}

/// Consumes a vowel syllable starting with an independent vowel.
fn scan_vowel_syllable(cps: &[char], start: usize) -> Syllable {
    let len = cps.len();
    let mut i = start + 1;
    while i < len {
        let isc = syllabic_category(cps[i]);
        match isc {
            IndicSyllabicCategory::VowelDependent
            | IndicSyllabicCategory::Bindu
            | IndicSyllabicCategory::Visarga
            | IndicSyllabicCategory::Nukta
            | IndicSyllabicCategory::CantillationMark => {
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
        has_reph: false,
    }
}
