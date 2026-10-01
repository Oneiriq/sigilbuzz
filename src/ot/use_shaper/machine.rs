//! The syllable grammar of HarfBuzz's `hb-ot-shaper-use-machine.rl`,
//! with the input filtering of its `find_syllables_use`:
//!
//! ```text
//! h = H | HVM | IS | Sk;
//! consonant_modifiers = CMAbv* CMBlw* ((h B | SUB) CMAbv* CMBlw*)*;
//! medial_consonants = MPre? MAbv? MBlw? MPst?;
//! dependent_vowels = VPre* VAbv* VBlw* VPst* | H;
//! vowel_modifiers = HVM? VMPre* VMAbv* VMBlw* VMPst*;
//! final_consonants = FAbv* FBlw* FPst*;
//! final_modifiers = FMAbv* FMBlw* | FMPst?;
//! complex_syllable_start = (R | CS)? (B | GB);
//! complex_syllable_middle = consonant_modifiers medial_consonants
//!     dependent_vowels vowel_modifiers (Sk B)*;
//! complex_syllable_tail = complex_syllable_middle final_consonants
//!     final_modifiers;
//! number_joiner_terminated_cluster_tail = (HN N)* HN;
//! numeral_cluster_tail = (HN N)+;
//! symbol_cluster_tail = SMAbv+ SMBlw* | SMBlw+;
//! virama_terminated_cluster_tail = consonant_modifiers (IS | RK);
//! sakot_terminated_cluster_tail = complex_syllable_middle Sk;
//! tail = complex_syllable_tail | sakot_terminated_cluster_tail
//!     | symbol_cluster_tail | virama_terminated_cluster_tail;
//! broken_cluster = R? (tail | number_joiner_terminated_cluster_tail
//!     | numeral_cluster_tail);
//! hieroglyph_cluster = SB* G HR? HM? SE* (J SB* (G HR? HM? SE*)?)*;
//! ```
//!
//! Each cluster rule may end in one ZWNJ. The machine does not see
//! `CGJ` characters, or a ZWNJ whose next visible character is a
//! mark. Those belong to the syllable before them.

use alloc::vec;
use alloc::vec::Vec;

use super::category::{
    B, CGJ, CMABV, CMBLW, CS, FABV, FBLW, FMABV, FMBLW, FMPST, FPST, G, GB, H, HM, HN, HR, HVM, IS,
    J, MABV, MBLW, MPRE, MPST, N, O, R, RK, SB, SE, SK, SMABV, SMBLW, SUB, VABV, VBLW, VMABV,
    VMBLW, VMPRE, VMPST, VPRE, VPST, ZWNJ,
};
use crate::ot::syllabic::machine::{alt, one, opt, seq, star, Machine, Pat};
use crate::ot::syllabic::GlyphInfo;

/// USE syllable types (`use_syllable_type_t`).
pub(crate) mod syllable {
    /// A cluster that ends in an invisible stacker or a reordering
    /// killer.
    pub(crate) const VIRAMA_TERMINATED: u8 = 0;
    /// A cluster that ends in a sakot.
    pub(crate) const SAKOT_TERMINATED: u8 = 1;
    /// A standard cluster.
    pub(crate) const STANDARD: u8 = 2;
    /// A number cluster that ends in a number joiner.
    pub(crate) const NUMBER_JOINER_TERMINATED: u8 = 3;
    /// A number cluster.
    pub(crate) const NUMERAL: u8 = 4;
    /// A symbol cluster.
    pub(crate) const SYMBOL: u8 = 5;
    /// A hieroglyph cluster.
    pub(crate) const HIEROGLYPH: u8 = 6;
    /// A broken cluster.
    pub(crate) const BROKEN: u8 = 7;
    /// A character outside any cluster.
    pub(crate) const NON_CLUSTER: u8 = 8;
}

fn h() -> Pat {
    one(&[H, HVM, IS, SK])
}

fn consonant_modifiers() -> Pat {
    let modifiers = || seq([star(one(&[CMABV])), star(one(&[CMBLW]))]);
    seq([
        modifiers(),
        star(seq([
            alt([seq([h(), one(&[B])]), one(&[SUB])]),
            modifiers(),
        ])),
    ])
}

fn medial_consonants() -> Pat {
    seq([
        opt(one(&[MPRE])),
        opt(one(&[MABV])),
        opt(one(&[MBLW])),
        opt(one(&[MPST])),
    ])
}

fn dependent_vowels() -> Pat {
    alt([
        seq([
            star(one(&[VPRE])),
            star(one(&[VABV])),
            star(one(&[VBLW])),
            star(one(&[VPST])),
        ]),
        one(&[H]),
    ])
}

fn vowel_modifiers() -> Pat {
    seq([
        opt(one(&[HVM])),
        star(one(&[VMPRE])),
        star(one(&[VMABV])),
        star(one(&[VMBLW])),
        star(one(&[VMPST])),
    ])
}

fn final_consonants() -> Pat {
    seq([star(one(&[FABV])), star(one(&[FBLW])), star(one(&[FPST]))])
}

fn final_modifiers() -> Pat {
    alt([
        seq([star(one(&[FMABV])), star(one(&[FMBLW]))]),
        opt(one(&[FMPST])),
    ])
}

fn complex_syllable_start() -> Pat {
    seq([opt(one(&[R, CS])), one(&[B, GB])])
}

fn complex_syllable_middle() -> Pat {
    seq([
        consonant_modifiers(),
        medial_consonants(),
        dependent_vowels(),
        vowel_modifiers(),
        star(seq([one(&[SK]), one(&[B])])),
    ])
}

fn complex_syllable_tail() -> Pat {
    seq([
        complex_syllable_middle(),
        final_consonants(),
        final_modifiers(),
    ])
}

fn number_joiner_terminated_cluster_tail() -> Pat {
    seq([star(seq([one(&[HN]), one(&[N])])), one(&[HN])])
}

fn numeral_cluster_tail() -> Pat {
    let pair = || seq([one(&[HN]), one(&[N])]);
    seq([pair(), star(pair())])
}

fn symbol_cluster_tail() -> Pat {
    alt([
        seq([one(&[SMABV]), star(one(&[SMABV])), star(one(&[SMBLW]))]),
        seq([one(&[SMBLW]), star(one(&[SMBLW]))]),
    ])
}

fn virama_terminated_cluster_tail() -> Pat {
    seq([consonant_modifiers(), one(&[IS, RK])])
}

fn sakot_terminated_cluster_tail() -> Pat {
    seq([complex_syllable_middle(), one(&[SK])])
}

fn tail() -> Pat {
    alt([
        complex_syllable_tail(),
        sakot_terminated_cluster_tail(),
        symbol_cluster_tail(),
        virama_terminated_cluster_tail(),
    ])
}

fn broken_cluster() -> Pat {
    seq([
        opt(one(&[R])),
        alt([
            tail(),
            number_joiner_terminated_cluster_tail(),
            numeral_cluster_tail(),
        ]),
    ])
}

fn hieroglyph_cluster() -> Pat {
    let glyph = || {
        seq([
            one(&[G]),
            opt(one(&[HR])),
            opt(one(&[HM])),
            star(one(&[SE])),
        ])
    };
    seq([
        star(one(&[SB])),
        glyph(),
        star(seq([one(&[J]), star(one(&[SB])), opt(glyph())])),
    ])
}

/// `rule ZWNJ?`.
fn with_zwnj(rule: Pat) -> Pat {
    seq([rule, opt(one(&[ZWNJ]))])
}

/// The machine's rules, in priority order.
fn machine() -> Machine {
    use syllable::{
        BROKEN, HIEROGLYPH, NON_CLUSTER, NUMBER_JOINER_TERMINATED, NUMERAL, SAKOT_TERMINATED,
        STANDARD, SYMBOL, VIRAMA_TERMINATED,
    };
    let start = complex_syllable_start;
    Machine::new(
        vec![
            (
                with_zwnj(seq([start(), virama_terminated_cluster_tail()])),
                VIRAMA_TERMINATED,
            ),
            (
                with_zwnj(seq([start(), sakot_terminated_cluster_tail()])),
                SAKOT_TERMINATED,
            ),
            (with_zwnj(seq([start(), complex_syllable_tail()])), STANDARD),
            (
                with_zwnj(seq([one(&[N]), number_joiner_terminated_cluster_tail()])),
                NUMBER_JOINER_TERMINATED,
            ),
            (
                with_zwnj(seq([one(&[N]), opt(numeral_cluster_tail())])),
                NUMERAL,
            ),
            (with_zwnj(seq([one(&[O, GB, SB]), opt(tail())])), SYMBOL),
            (with_zwnj(hieroglyph_cluster()), HIEROGLYPH),
            (one(&[FMPST]), NON_CLUSTER),
            (with_zwnj(broken_cluster()), BROKEN),
        ],
        NON_CLUSTER,
    )
}

/// HarfBuzz's `_hb_glyph_info_is_unicode_mark`: a character of
/// General_Category Mn, Mc, or Me.
fn is_mark(ch: char) -> bool {
    use crate::unicode::general_category::{general_category_class, GeneralCategoryClass};
    general_category_class(ch) == Some(GeneralCategoryClass::Mark)
}

/// HarfBuzz's `find_syllables_use`: sets the syllable of every entry
/// of `info`, whose categories are set, from the characters
/// `codepoints` (one per entry). The machine reads the categories
/// without the `CGJ` characters and without a ZWNJ whose next
/// character other than a `CGJ` is a mark. A syllable takes those
/// characters up to the next syllable. Characters before the first
/// syllable keep syllable 0.
pub(crate) fn find_syllables(codepoints: &[char], info: &mut [GlyphInfo]) {
    let n = info.len().min(codepoints.len());
    // The next character from each index on that is not a `CGJ`.
    let mut next_visible = vec![n; n + 1];
    for i in (0..n).rev() {
        next_visible[i] = if info[i].category == CGJ {
            next_visible[i + 1]
        } else {
            i
        };
    }
    let kept: Vec<usize> = (0..n)
        .filter(|&i| {
            let c = info[i].category;
            if c == CGJ {
                return false;
            }
            if c != ZWNJ {
                return true;
            }
            let next = next_visible[i + 1];
            !codepoints.get(next).is_some_and(|&ch| is_mark(ch))
        })
        .collect();
    let cats: Vec<u8> = kept.iter().map(|&i| info[i].category).collect();
    let mut serial: u8 = 1;
    for s in machine().scan(&cats) {
        let start = kept.get(s.start).copied().unwrap_or(n);
        let end = kept.get(s.end).copied().unwrap_or(n);
        if let Some(run) = info.get_mut(start..end) {
            for g in run {
                g.syllable = (serial << 4) | s.kind;
            }
        }
        serial = if serial == 15 { 1 } else { serial + 1 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syllable::{BROKEN, NON_CLUSTER, NUMERAL, STANDARD, SYMBOL};

    /// `(start, end, type)` of each syllable of the characters.
    fn syllables(text: &[char]) -> Vec<(usize, usize, u8)> {
        let mut info: Vec<GlyphInfo> = text
            .iter()
            .map(|&c| GlyphInfo {
                category: super::super::category::category(c),
                ..GlyphInfo::default()
            })
            .collect();
        find_syllables(text, &mut info);
        let mut out = Vec::new();
        for range in crate::ot::syllabic::syllable_ranges(&info) {
            out.push((range.start, range.end, info[range.start].syllable_type()));
        }
        out
    }

    #[test]
    fn standard_clusters_take_their_marks() {
        // Tirhuta ra, virama, ka, sign i, candrabindu.
        let text = [
            '\u{114A9}',
            '\u{114C2}',
            '\u{1148F}',
            '\u{114B1}',
            '\u{114BF}',
        ];
        assert_eq!(syllables(&text), [(0, 5, STANDARD)]);
        // Ka, virama: the halant ends a standard cluster.
        assert_eq!(syllables(&['\u{1148F}', '\u{114C2}']), [(0, 2, STANDARD)]);
    }

    #[test]
    fn a_lone_sign_is_a_broken_cluster() {
        assert_eq!(syllables(&['\u{114B1}', '\u{114BF}']), [(0, 2, BROKEN)]);
        // A lone final consonant is a broken cluster, and a final
        // modifier with no position of its own stands alone.
        assert_eq!(syllables(&['\u{1B03}']), [(0, 1, BROKEN)]);
        assert_eq!(syllables(&['\u{00B2}']), [(0, 1, NON_CLUSTER)]);
    }

    #[test]
    fn joiners_follow_the_filter() {
        // Ka, ZWNJ, ka: the ZWNJ ends the first cluster.
        let text = ['\u{1148F}', '\u{200C}', '\u{1148F}'];
        assert_eq!(syllables(&text), [(0, 2, STANDARD), (2, 3, STANDARD)]);
        // Ka, ZWNJ, sign i: the machine does not see the ZWNJ, which
        // joins the cluster before it.
        let text = ['\u{1148F}', '\u{200C}', '\u{114B1}'];
        assert_eq!(syllables(&text), [(0, 3, STANDARD)]);
        // Ka, ZWJ, virama, ka: the ZWJ is a CGJ, invisible to the
        // machine.
        let text = ['\u{1148F}', '\u{200D}', '\u{114C2}', '\u{1148F}'];
        assert_eq!(syllables(&text), [(0, 4, STANDARD)]);
        // A leading CGJ is in no syllable: its syllable byte stays 0.
        let mut info = vec![GlyphInfo::default(); 2];
        info[0].category = CGJ;
        info[1].category = B;
        find_syllables(&['\u{034F}', '\u{1148F}'], &mut info);
        assert_eq!((info[0].syllable, info[1].syllable), (0, 0x10 | STANDARD));
        // A trailing one joins the syllable before it.
        let text = ['\u{1148F}', '\u{034F}'];
        assert_eq!(syllables(&text), [(0, 2, STANDARD)]);
    }

    #[test]
    fn numbers_symbols_and_others() {
        // Brahmi number one, number joiner, number two.
        let text = ['\u{11052}', '\u{1107F}', '\u{11053}'];
        assert_eq!(syllables(&text), [(0, 3, NUMERAL)]);
        // A space and a Latin letter are symbol clusters of O.
        assert_eq!(syllables(&[' ', 'a']), [(0, 1, SYMBOL), (1, 2, SYMBOL)]);
        // A word joiner is outside any cluster.
        assert_eq!(syllables(&['\u{2060}']), [(0, 1, NON_CLUSTER)]);
    }

    #[test]
    fn a_long_run_of_signs_scans_in_linear_time() {
        let mut text = vec!['\u{1148F}'];
        text.extend(core::iter::repeat('\u{114BF}').take(100_000));
        text.extend(core::iter::repeat('\u{200C}').take(100_000));
        let out = syllables(&text);
        assert_eq!(out[0], (0, 100_002, STANDARD));
        assert_eq!(out.len(), 1 + 99_999);
    }
}
