//! Unicode canonical normalization data: one-step canonical
//! decomposition, primary composition, and the canonical combining
//! class, plus HarfBuzz's modified combining classes.
//!
//! These are the character questions HarfBuzz's normalizer
//! (`_hb_ot_shape_normalize` in `hb-ot-shape-normalize.cc`) asks its
//! `hb_unicode_funcs_t`: `decompose`, `compose`, and
//! `modified_combining_class`. The normalizer itself needs the font's
//! character map, so it lives with the shaper; this module only knows
//! about characters.
//!
//! The tables in the child modules are generated from the Unicode
//! 17.0.0 `UnicodeData.txt`, `DerivedCombiningClass.txt`, and
//! `CompositionExclusions.txt` (snapshots in `tests/tools/ucd/`;
//! regenerate with `cargo test --test unicode_table_gen -- --ignored`).
//! Hangul syllables compose and decompose algorithmically (Unicode
//! 17.0.0, section 3.12), as in HarfBuzz's `hb-ucd.cc`.

#[rustfmt::skip]
mod combining_class_table;
#[rustfmt::skip]
mod compose_table;
#[rustfmt::skip]
mod decompose_table;

use combining_class_table::COMBINING_CLASSES;
use compose_table::COMPOSITIONS;
use decompose_table::DECOMPOSITIONS;

const HANGUL_SBASE: u32 = 0xAC00;
const HANGUL_LBASE: u32 = 0x1100;
const HANGUL_VBASE: u32 = 0x1161;
const HANGUL_TBASE: u32 = 0x11A7;
const HANGUL_LCOUNT: u32 = 19;
const HANGUL_VCOUNT: u32 = 21;
const HANGUL_TCOUNT: u32 = 28;
const HANGUL_NCOUNT: u32 = HANGUL_VCOUNT * HANGUL_TCOUNT;
const HANGUL_SCOUNT: u32 = HANGUL_LCOUNT * HANGUL_NCOUNT;

/// One step of canonical decomposition: the first character of `ch`'s
/// canonical `Decomposition_Mapping` and, unless the mapping is a
/// singleton, the second. `None` when `ch` has no canonical
/// decomposition.
///
/// A mapping may itself decompose further (U+1E08 LATIN CAPITAL LETTER C
/// WITH CEDILLA AND ACUTE maps to U+00C7 + U+0301); callers that want
/// the full decomposition apply this again to the first character. A
/// Hangul syllable splits into its LV syllable and trailing jamo, or
/// into its leading and vowel jamo, like HarfBuzz's `hb_ucd_decompose`.
///
/// # Examples
///
/// ```
/// use sigilbuzz::unicode::normalize::decompose;
///
/// assert_eq!(decompose('\u{00E9}'), Some(('e', Some('\u{0301}'))));
/// // ANGSTROM SIGN is a singleton.
/// assert_eq!(decompose('\u{212B}'), Some(('\u{00C5}', None)));
/// assert_eq!(decompose('\u{AC01}'), Some(('\u{AC00}', Some('\u{11A8}'))));
/// assert_eq!(decompose('e'), None);
/// ```
#[must_use]
pub fn decompose(ch: char) -> Option<(char, Option<char>)> {
    if let Some(pair) = decompose_hangul(ch) {
        return Some(pair);
    }
    let cp = u32::from(ch);
    let i = DECOMPOSITIONS.binary_search_by_key(&cp, |e| e.0).ok()?;
    let (_, first, second) = DECOMPOSITIONS[i];
    let first = char::from_u32(first)?;
    let second = if second == 0 {
        None
    } else {
        Some(char::from_u32(second)?)
    };
    Some((first, second))
}

/// The primary composite of `a` followed by `b`, when there is one:
/// the character whose canonical decomposition is `a b` and that is
/// not a full composition exclusion. Hangul jamo and LV syllables
/// compose algorithmically.
///
/// # Examples
///
/// ```
/// use sigilbuzz::unicode::normalize::compose;
///
/// assert_eq!(compose('e', '\u{0301}'), Some('\u{00E9}'));
/// assert_eq!(compose('\u{1100}', '\u{1161}'), Some('\u{AC00}'));
/// // DEVANAGARI LETTER QA is a composition exclusion.
/// assert_eq!(compose('\u{0915}', '\u{093C}'), None);
/// ```
#[must_use]
pub fn compose(a: char, b: char) -> Option<char> {
    if let Some(c) = compose_hangul(a, b) {
        return Some(c);
    }
    let key = (u32::from(a), u32::from(b));
    let i = COMPOSITIONS
        .binary_search_by_key(&key, |e| (e.0, e.1))
        .ok()?;
    char::from_u32(COMPOSITIONS[i].2)
}

/// Greedy left-to-right pairwise composition of `input`, the pass
/// [`crate::Buffer::set_normalize_nfc`] runs: each character composes
/// with the one emitted before it when [`compose`] has a composite.
#[must_use]
pub fn compose_str(input: &str) -> alloc::string::String {
    let mut out: alloc::vec::Vec<char> = alloc::vec::Vec::with_capacity(input.len());
    for ch in input.chars() {
        if let Some(last) = out.last_mut() {
            if let Some(composed) = compose(*last, ch) {
                *last = composed;
                continue;
            }
        }
        out.push(ch);
    }
    out.into_iter().collect()
}

/// The `Canonical_Combining_Class` of `ch`: zero for starters, the
/// class number (1 to 240) for combining marks that reorder.
///
/// # Examples
///
/// ```
/// use sigilbuzz::unicode::normalize::combining_class;
///
/// assert_eq!(combining_class('a'), 0);
/// assert_eq!(combining_class('\u{0301}'), 230);
/// assert_eq!(combining_class('\u{0327}'), 202);
/// ```
#[must_use]
pub fn combining_class(ch: char) -> u8 {
    let cp = u32::from(ch);
    COMBINING_CLASSES
        .binary_search_by(|&(start, end, _)| {
            if end < cp {
                core::cmp::Ordering::Less
            } else if start > cp {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .map_or(0, |i| COMBINING_CLASSES[i].2)
}

/// HarfBuzz's modified combining class of `ch`
/// (`hb_unicode_funcs_t::modified_combining_class` in `hb-unicode.hh`,
/// HarfBuzz 14.5.0): the canonical class, renumbered so that sorting
/// by it puts marks in the order fonts expect.
///
/// - Hebrew points (classes 10 to 26) follow the SBL Hebrew order.
/// - Arabic shadda (33) sorts before the other harakat (27 to 35).
/// - The Telugu length marks (84, 91) become 4 and 5 so they do not
///   reorder around the virama.
/// - Thai sara u and uu (103) become 3, before phinthu (9).
/// - Tibetan sign i (130) and sign u (132) swap.
/// - U+1A60 TAI THAM SIGN SAKOT and U+0FC6 TIBETAN SYMBOL PADMA GDAN
///   get 254 (after every other mark); U+0F39 TIBETAN MARK TSA -PHRU
///   gets 127 (before U+0F74).
///
/// # Examples
///
/// ```
/// use sigilbuzz::unicode::normalize::modified_combining_class;
///
/// // ARABIC SHADDA sorts before ARABIC FATHA.
/// assert!(modified_combining_class('\u{0651}') < modified_combining_class('\u{064E}'));
/// assert_eq!(modified_combining_class('\u{0301}'), 230);
/// ```
#[must_use]
pub fn modified_combining_class(ch: char) -> u8 {
    match ch {
        '\u{1A60}' | '\u{0FC6}' => return 254,
        '\u{0F39}' => return 127,
        _ => {}
    }
    modify_class(combining_class(ch))
}

/// HarfBuzz's `_hb_modified_combining_class` table (`hb-unicode.cc`).
const fn modify_class(class: u8) -> u8 {
    match class {
        // Hebrew, in the SBL Hebrew manual's order.
        10 => 22, // sheva
        11 => 15, // hataf segol
        12 => 16, // hataf patah
        13 => 17, // hataf qamats
        14 => 23, // hiriq
        15 => 18, // tsere
        16 => 19, // segol
        17 => 20, // patah
        18 => 21, // qamats and qamats qatan
        19 => 14, // holam and holam haser for vav
        20 => 24, // qubuts
        21 => 12, // dagesh
        22 => 25, // meteg
        23 => 13, // rafe
        24 => 10, // shin dot
        25 => 11, // sin dot
        // Arabic: shadda first.
        27 => 28, // fathatan
        28 => 29, // dammatan
        29 => 30, // kasratan
        30 => 31, // fatha
        31 => 32, // damma
        32 => 33, // kasra
        33 => 27, // shadda
        // Telugu length marks.
        84 => 4,
        91 => 5,
        // Thai sara u and uu, before phinthu.
        103 => 3,
        // Tibetan: sign u before sign i.
        130 => 132,
        132 => 131,
        other => other,
    }
}

fn decompose_hangul(ch: char) -> Option<(char, Option<char>)> {
    let s_index = u32::from(ch).wrapping_sub(HANGUL_SBASE);
    if s_index >= HANGUL_SCOUNT {
        return None;
    }
    let (a, b) = if s_index % HANGUL_TCOUNT == 0 {
        // LV syllable: leading plus vowel jamo.
        (
            HANGUL_LBASE + s_index / HANGUL_NCOUNT,
            HANGUL_VBASE + (s_index % HANGUL_NCOUNT) / HANGUL_TCOUNT,
        )
    } else {
        // LVT syllable: LV syllable plus trailing jamo.
        (
            HANGUL_SBASE + (s_index / HANGUL_TCOUNT) * HANGUL_TCOUNT,
            HANGUL_TBASE + s_index % HANGUL_TCOUNT,
        )
    };
    Some((char::from_u32(a)?, Some(char::from_u32(b)?)))
}

fn compose_hangul(a: char, b: char) -> Option<char> {
    let (a, b) = (u32::from(a), u32::from(b));
    if (HANGUL_LBASE..HANGUL_LBASE + HANGUL_LCOUNT).contains(&a)
        && (HANGUL_VBASE..HANGUL_VBASE + HANGUL_VCOUNT).contains(&b)
    {
        let lv =
            HANGUL_SBASE + (a - HANGUL_LBASE) * HANGUL_NCOUNT + (b - HANGUL_VBASE) * HANGUL_TCOUNT;
        return char::from_u32(lv);
    }
    let s_index = a.wrapping_sub(HANGUL_SBASE);
    if s_index < HANGUL_SCOUNT
        && s_index % HANGUL_TCOUNT == 0
        && (HANGUL_TBASE + 1..HANGUL_TBASE + HANGUL_TCOUNT).contains(&b)
    {
        return char::from_u32(a + (b - HANGUL_TBASE));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted() {
        assert!(DECOMPOSITIONS.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(COMPOSITIONS
            .windows(2)
            .all(|w| (w[0].0, w[0].1) < (w[1].0, w[1].1)));
        assert!(COMBINING_CLASSES.windows(2).all(|w| w[0].1 < w[1].0));
    }

    #[test]
    fn every_composite_decomposes_back() {
        for &(a, b, c) in COMPOSITIONS {
            let c = char::from_u32(c).unwrap();
            let pair = (char::from_u32(a).unwrap(), char::from_u32(b));
            assert_eq!(decompose(c), Some(pair), "{c:?}");
        }
    }

    #[test]
    fn latin_greek_and_cyrillic_compose() {
        assert_eq!(compose('e', '\u{0301}'), Some('\u{00E9}'));
        assert_eq!(compose('A', '\u{0308}'), Some('\u{00C4}'));
        assert_eq!(compose('\u{03B1}', '\u{0301}'), Some('\u{03AC}'));
        assert_eq!(compose('\u{0435}', '\u{0308}'), Some('\u{0451}'));
        // Vietnamese: o with circumflex, then acute on the result.
        assert_eq!(compose('o', '\u{0302}'), Some('\u{00F4}'));
        assert_eq!(compose('\u{00F4}', '\u{0301}'), Some('\u{1ED1}'));
    }

    #[test]
    fn exclusions_and_non_starters_do_not_compose() {
        assert_eq!(compose('x', '\u{0301}'), None);
        // COMBINING GREEK DIALYTIKA TONOS decomposes to a non-starter.
        assert_eq!(compose('\u{0308}', '\u{0301}'), None);
        // HEBREW LETTER SHIN WITH SHIN DOT is a composition exclusion.
        assert_eq!(compose('\u{05E9}', '\u{05C1}'), None);
        assert_eq!(decompose('\u{FB2A}'), Some(('\u{05E9}', Some('\u{05C1}'))));
    }

    #[test]
    fn hangul_composes_and_decomposes_algorithmically() {
        assert_eq!(compose('\u{1100}', '\u{1161}'), Some('\u{AC00}'));
        assert_eq!(compose('\u{AC00}', '\u{11A8}'), Some('\u{AC01}'));
        // An LVT syllable takes no further trailing jamo.
        assert_eq!(compose('\u{AC01}', '\u{11A8}'), None);
        // U+11A7 is not a trailing consonant.
        assert_eq!(compose('\u{AC00}', '\u{11A7}'), None);
        assert_eq!(decompose('\u{AC00}'), Some(('\u{1100}', Some('\u{1161}'))));
        assert_eq!(decompose('\u{D7A3}'), Some(('\u{D788}', Some('\u{11C2}'))));
        assert_eq!(decompose('\u{D7A4}'), None);
    }

    #[test]
    fn combining_classes_come_from_the_ucd() {
        assert_eq!(combining_class('\u{0300}'), 230);
        assert_eq!(combining_class('\u{0316}'), 220);
        assert_eq!(combining_class('\u{094D}'), 9);
        assert_eq!(combining_class('\u{05B0}'), 10);
        assert_eq!(combining_class('\u{0E38}'), 103);
        assert_eq!(combining_class('\u{1D16E}'), 216);
        assert_eq!(combining_class('\u{0915}'), 0);
    }

    #[test]
    fn modified_classes_follow_harfbuzz() {
        // Hebrew: shin dot (24) before dagesh (21) before sheva (10).
        assert_eq!(modified_combining_class('\u{05C1}'), 10);
        assert_eq!(modified_combining_class('\u{05BC}'), 12);
        assert_eq!(modified_combining_class('\u{05B0}'), 22);
        // Arabic: shadda before fatha.
        assert_eq!(modified_combining_class('\u{0651}'), 27);
        assert_eq!(modified_combining_class('\u{064E}'), 31);
        // Telugu length mark, Thai sara u, Tibetan signs.
        assert_eq!(modified_combining_class('\u{0C55}'), 4);
        assert_eq!(modified_combining_class('\u{0C56}'), 5);
        assert_eq!(modified_combining_class('\u{0E38}'), 3);
        assert_eq!(modified_combining_class('\u{0F72}'), 132);
        assert_eq!(modified_combining_class('\u{0F74}'), 131);
        assert_eq!(modified_combining_class('\u{1A60}'), 254);
        assert_eq!(modified_combining_class('\u{0FC6}'), 254);
        assert_eq!(modified_combining_class('\u{0F39}'), 127);
        assert_eq!(modified_combining_class('a'), 0);
    }
}
