//! The shapers' normalization hooks: their `decompose` and `compose`
//! overrides and their `reorder_marks` functions (HarfBuzz 14.5.0,
//! `hb-ot-shaper-{indic,khmer,use,hebrew,arabic}.cc`).

use super::{NormChar, MAX_COMBINING_MARKS};
use crate::buffer::ClusterLevel;
use crate::shape::cluster::merge_clusters;
use crate::shape::shaper::Shaper;
use crate::unicode::general_category::{general_category_class, GeneralCategoryClass};
use crate::unicode::normalize;

/// One decomposition step of `ab` under `shaper`: the shaper's
/// override, else the canonical decomposition.
pub(super) fn decompose(shaper: Shaper, ab: char) -> Option<(char, Option<char>)> {
    match shaper {
        // `decompose_indic`: keep these whole.
        Shaper::Indic
            if matches!(
                ab,
                '\u{0931}' // DEVANAGARI LETTER RRA
                    | '\u{09DC}' // BENGALI LETTER RRA
                    | '\u{09DD}' // BENGALI LETTER RHA
                    | '\u{0B94}' // TAMIL LETTER AU
            ) =>
        {
            None
        }
        // `decompose_khmer`: split matras without a Unicode
        // decomposition get their pre-base part, U+17C1 KHMER VOWEL
        // SIGN E, in front of themselves.
        Shaper::Khmer if matches!(ab, '\u{17BE}'..='\u{17C0}' | '\u{17C4}' | '\u{17C5}') => {
            Some(('\u{17C1}', Some(ab)))
        }
        _ => normalize::decompose(ab),
    }
}

/// The composite of `a` and `b` under `shaper`: the shaper's override,
/// else the primary composite. `has_gpos_mark` says whether the font
/// has GPOS mark positioning for the run.
pub(super) fn compose(shaper: Shaper, a: char, b: char, has_gpos_mark: bool) -> Option<char> {
    match shaper {
        // `compose_indic`, `compose_khmer`, `compose_use`: never
        // recompose split matras.
        Shaper::Indic | Shaper::Khmer | Shaper::Use if is_mark(a) => None,
        // A composition exclusion the Indic shaper recomposes anyway.
        Shaper::Indic if (a, b) == ('\u{09AF}', '\u{09BC}') => Some('\u{09DF}'),
        Shaper::Hebrew => normalize::compose(a, b).or_else(|| {
            if has_gpos_mark {
                None
            } else {
                hebrew_presentation_form(a, b)
            }
        }),
        _ => normalize::compose(a, b),
    }
}

/// The shaper's `reorder_marks` hook, run on a run of marks that was
/// just sorted by combining class.
pub(super) fn reorder_marks(
    shaper: Shaper,
    chars: &mut [NormChar],
    start: usize,
    end: usize,
    level: ClusterLevel,
) {
    match shaper {
        Shaper::Arabic => reorder_marks_arabic(chars, start, end, level),
        Shaper::Hebrew => reorder_marks_hebrew(chars, start, end, level),
        _ => {}
    }
}

fn is_mark(ch: char) -> bool {
    general_category_class(ch) == Some(GeneralCategoryClass::Mark)
}

/// `compose_hebrew`'s fallback for fonts without GPOS mark positioning:
/// the Hebrew presentation forms normalization excludes but old fonts
/// draw.
fn hebrew_presentation_form(a: char, b: char) -> Option<char> {
    /// Letters U+05D0..U+05EA with dagesh; `\0` where none is encoded.
    const DAGESH_FORMS: [char; 27] = [
        '\u{FB30}', // ALEF
        '\u{FB31}', // BET
        '\u{FB32}', // GIMEL
        '\u{FB33}', // DALET
        '\u{FB34}', // HE
        '\u{FB35}', // VAV
        '\u{FB36}', // ZAYIN
        '\0',       // HET
        '\u{FB38}', // TET
        '\u{FB39}', // YOD
        '\u{FB3A}', // FINAL KAF
        '\u{FB3B}', // KAF
        '\u{FB3C}', // LAMED
        '\0',       // FINAL MEM
        '\u{FB3E}', // MEM
        '\0',       // FINAL NUN
        '\u{FB40}', // NUN
        '\u{FB41}', // SAMEKH
        '\0',       // AYIN
        '\u{FB43}', // FINAL PE
        '\u{FB44}', // PE
        '\0',       // FINAL TSADI
        '\u{FB46}', // TSADI
        '\u{FB47}', // QOF
        '\u{FB48}', // RESH
        '\u{FB49}', // SHIN
        '\u{FB4A}', // TAV
    ];
    let composite = match (b, a) {
        ('\u{05B4}', '\u{05D9}') => '\u{FB1D}', // HIRIQ: YOD
        ('\u{05B7}', '\u{05F2}') => '\u{FB1F}', // PATAH: YIDDISH DOUBLE YOD
        ('\u{05B7}', '\u{05D0}') => '\u{FB2E}', // PATAH: ALEF
        ('\u{05B8}', '\u{05D0}') => '\u{FB2F}', // QAMATS: ALEF
        ('\u{05B9}', '\u{05D5}') => '\u{FB4B}', // HOLAM: VAV
        ('\u{05BC}', '\u{05D0}'..='\u{05EA}') => DAGESH_FORMS[(u32::from(a) - 0x05D0) as usize],
        ('\u{05BC}', '\u{FB2A}') => '\u{FB2C}', // DAGESH: SHIN WITH SHIN DOT
        ('\u{05BC}', '\u{FB2B}') => '\u{FB2D}', // DAGESH: SHIN WITH SIN DOT
        ('\u{05BF}', '\u{05D1}') => '\u{FB4C}', // RAFE: BET
        ('\u{05BF}', '\u{05DB}') => '\u{FB4D}', // RAFE: KAF
        ('\u{05BF}', '\u{05E4}') => '\u{FB4E}', // RAFE: PE
        ('\u{05C1}', '\u{05E9}') => '\u{FB2A}', // SHIN DOT: SHIN
        ('\u{05C1}', '\u{FB49}') => '\u{FB2C}', // SHIN DOT: SHIN WITH DAGESH
        ('\u{05C2}', '\u{05E9}') => '\u{FB2B}', // SIN DOT: SHIN
        ('\u{05C2}', '\u{FB49}') => '\u{FB2D}', // SIN DOT: SHIN WITH DAGESH
        _ => return None,
    };
    (composite != '\0').then_some(composite)
}

/// Arabic modifier combining marks (Unicode UAX #53): they move to the
/// front of their class so they render next to the letter.
const MODIFIER_COMBINING_MARKS: [char; 14] = [
    '\u{0654}', // ARABIC HAMZA ABOVE
    '\u{0655}', // ARABIC HAMZA BELOW
    '\u{0658}', // ARABIC MARK NOON GHUNNA
    '\u{06DC}', // ARABIC SMALL HIGH SEEN
    '\u{06E3}', // ARABIC SMALL LOW SEEN
    '\u{06E7}', // ARABIC SMALL HIGH YEH
    '\u{06E8}', // ARABIC SMALL HIGH NOON
    '\u{08CA}', // ARABIC SMALL HIGH FARSI YEH
    '\u{08CB}', // ARABIC SMALL HIGH YEH BARREE WITH TWO DOTS BELOW
    '\u{08CD}', // ARABIC SMALL HIGH ZAH
    '\u{08CE}', // ARABIC LARGE ROUND DOT ABOVE
    '\u{08CF}', // ARABIC LARGE ROUND DOT BELOW
    '\u{08D3}', // ARABIC SMALL LOW WAW
    '\u{08F3}', // ARABIC SMALL HIGH WAW
];

/// Modified combining classes the moved modifier marks get: the Hebrew
/// meteg and point varika classes, smaller than every Arabic class so
/// the run stays sorted, and folded back to below and above by the
/// fallback positioner.
const CCC22_METEG: u8 = 25;
const CCC26_VARIKA: u8 = 26;

/// `reorder_marks_arabic`: within `chars[start..end]`, the modifier
/// combining marks at the head of the below (220) and above (230)
/// classes move to the front of the run.
fn reorder_marks_arabic(chars: &mut [NormChar], mut start: usize, end: usize, level: ClusterLevel) {
    let mut i = start;
    for class in [220u8, 230] {
        while i < end && chars[i].mcc < class {
            i += 1;
        }
        if i == end {
            break;
        }
        if chars[i].mcc > class {
            continue;
        }
        let mut j = i;
        while j < end && chars[j].mcc == class && MODIFIER_COMBINING_MARKS.contains(&chars[j].ch) {
            j += 1;
        }
        if i == j {
            continue;
        }
        debug_assert!(j - i <= MAX_COMBINING_MARKS);
        // Shift the modifier marks in front of the rest.
        merge_clusters(chars, start, j, level);
        chars[start..j].rotate_right(j - i);
        let new_start = start + j - i;
        let new_class = if class == 220 {
            CCC22_METEG
        } else {
            CCC26_VARIKA
        };
        for c in &mut chars[start..new_start] {
            c.mcc = new_class;
        }
        start = new_start;
        i = j;
    }
}

/// Modified combining classes of patah, qamats, sheva, hiriq, and meteg.
const CCC17_PATAH: u8 = 20;
const CCC18_QAMATS: u8 = 21;
const CCC10_SHEVA: u8 = 22;
const CCC14_HIRIQ: u8 = 23;
/// Canonical class 220, below.
const BELOW: u8 = 220;

/// `reorder_marks_hebrew`: a patah or qamats followed by a sheva or
/// hiriq and then a meteg or below mark swaps the last two, once.
fn reorder_marks_hebrew(chars: &mut [NormChar], start: usize, end: usize, level: ClusterLevel) {
    for i in start + 2..end {
        let c0 = chars[i - 2].mcc;
        let c1 = chars[i - 1].mcc;
        let c2 = chars[i].mcc;
        if matches!(c0, CCC17_PATAH | CCC18_QAMATS)
            && matches!(c1, CCC10_SHEVA | CCC14_HIRIQ)
            && matches!(c2, CCC22_METEG | BELOW)
        {
            merge_clusters(chars, i - 1, i + 1, level);
            chars.swap(i - 1, i);
            break;
        }
    }
}
