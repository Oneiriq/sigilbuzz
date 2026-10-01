//! The Indic syllable grammar of HarfBuzz's
//! `hb-ot-shaper-indic-machine.rl`:
//!
//! ```text
//! c = (C | Ra);
//! n = ((ZWNJ?.RS)? (N.N?)?);
//! z = ZWJ|ZWNJ;
//! reph = (Ra H | Repha);
//! sm = SM | SMPst;
//! cn = c.ZWJ?.n?;
//! symbol = Symbol.N?;
//! matra_group = z*.(M | sm? MPst).N?.H?;
//! syllable_tail = (z?.sm.sm?.ZWNJ?)? (A | VD)*;
//! halant_group = (z?.H.(ZWJ.N?)?);
//! final_halant_group = halant_group | H.ZWNJ;
//! medial_group = CM?;
//! halant_or_matra_group = (final_halant_group | matra_group*);
//! complex_syllable_tail = (halant_group.cn)* medial_group
//!                         halant_or_matra_group syllable_tail;
//! consonant_syllable = (Repha|CS)? cn complex_syllable_tail;
//! vowel_syllable = reph? V.n? (ZWJ | complex_syllable_tail);
//! standalone_cluster = ((Repha|CS)? PLACEHOLDER | reph? DOTTEDCIRCLE).n?
//!                      complex_syllable_tail;
//! symbol_cluster = symbol syllable_tail;
//! broken_cluster = reph? n? complex_syllable_tail;
//! ```

use alloc::vec;
use alloc::vec::Vec;

use crate::ot::syllabic::cat::{
    A, C, CM, CS, DOTTEDCIRCLE, H, M, MPST, N, PLACEHOLDER, RA, REPHA, RS, SM, SMPST, SYMBOL, V,
    VD, ZWJ, ZWNJ,
};
use crate::ot::syllabic::machine::{alt, one, opt, seq, star, Machine, Pat, Syllable};

/// Indic syllable types (`indic_syllable_type_t`).
pub(crate) mod syllable {
    /// A consonant syllable.
    pub(crate) const CONSONANT: u8 = 0;
    /// A vowel syllable.
    pub(crate) const VOWEL: u8 = 1;
    /// A standalone cluster.
    pub(crate) const STANDALONE: u8 = 2;
    /// A symbol cluster.
    pub(crate) const SYMBOL: u8 = 3;
    /// A broken cluster.
    pub(crate) const BROKEN: u8 = 4;
    /// Anything else.
    pub(crate) const NON_INDIC: u8 = 5;
}

fn c() -> Pat {
    one(&[C, RA])
}

fn n() -> Pat {
    seq([
        opt(seq([opt(one(&[ZWNJ])), one(&[RS])])),
        opt(seq([one(&[N]), opt(one(&[N]))])),
    ])
}

fn z() -> Pat {
    one(&[ZWJ, ZWNJ])
}

fn reph() -> Pat {
    alt([seq([one(&[RA]), one(&[H])]), one(&[REPHA])])
}

fn sm() -> Pat {
    one(&[SM, SMPST])
}

fn cn() -> Pat {
    seq([c(), opt(one(&[ZWJ])), opt(n())])
}

fn matra_group() -> Pat {
    seq([
        star(z()),
        alt([one(&[M]), seq([opt(sm()), one(&[MPST])])]),
        opt(one(&[N])),
        opt(one(&[H])),
    ])
}

fn syllable_tail() -> Pat {
    seq([
        opt(seq([opt(z()), sm(), opt(sm()), opt(one(&[ZWNJ]))])),
        star(one(&[A, VD])),
    ])
}

fn halant_group() -> Pat {
    seq([opt(z()), one(&[H]), opt(seq([one(&[ZWJ]), opt(one(&[N]))]))])
}

fn final_halant_group() -> Pat {
    alt([halant_group(), seq([one(&[H]), one(&[ZWNJ])])])
}

fn complex_syllable_tail() -> Pat {
    seq([
        star(seq([halant_group(), cn()])),
        opt(one(&[CM])),
        alt([final_halant_group(), star(matra_group())]),
        syllable_tail(),
    ])
}

fn consonant_syllable() -> Pat {
    seq([opt(one(&[REPHA, CS])), cn(), complex_syllable_tail()])
}

fn vowel_syllable() -> Pat {
    seq([
        opt(reph()),
        one(&[V]),
        opt(n()),
        alt([one(&[ZWJ]), complex_syllable_tail()]),
    ])
}

fn standalone_cluster() -> Pat {
    seq([
        alt([
            seq([opt(one(&[REPHA, CS])), one(&[PLACEHOLDER])]),
            seq([opt(reph()), one(&[DOTTEDCIRCLE])]),
        ]),
        opt(n()),
        complex_syllable_tail(),
    ])
}

fn symbol_cluster() -> Pat {
    seq([one(&[SYMBOL]), opt(one(&[N])), syllable_tail()])
}

fn broken_cluster() -> Pat {
    seq([opt(reph()), opt(n()), complex_syllable_tail()])
}

/// The compiled Indic grammar.
pub(super) fn machine() -> Machine {
    Machine::new(
        vec![
            (consonant_syllable(), syllable::CONSONANT),
            (vowel_syllable(), syllable::VOWEL),
            (standalone_cluster(), syllable::STANDALONE),
            (symbol_cluster(), syllable::SYMBOL),
            (one(&[SMPST]), syllable::NON_INDIC),
            (broken_cluster(), syllable::BROKEN),
        ],
        syllable::NON_INDIC,
    )
}

/// Splits Indic categories into syllables (`find_syllables_indic`).
pub(super) fn find_syllables(cats: &[u8]) -> Vec<Syllable> {
    machine().scan(cats)
}

#[cfg(test)]
mod tests {
    use super::syllable::{BROKEN, CONSONANT, NON_INDIC, STANDALONE, VOWEL};
    use super::*;

    fn kinds(cats: &[u8]) -> Vec<(usize, usize, u8)> {
        find_syllables(cats)
            .iter()
            .map(|s| (s.start, s.end, s.kind))
            .collect()
    }

    #[test]
    fn conjuncts_and_matras_form_one_syllable() {
        // Ra, halant, ka, halant, ta, matra, bindu.
        assert_eq!(kinds(&[RA, H, C, H, C, M, SM]), [(0, 7, CONSONANT)]);
        // Ka, halant, ZWJ, ta: the ZWJ asks for a half form.
        assert_eq!(kinds(&[C, H, ZWJ, C]), [(0, 4, CONSONANT)]);
    }

    #[test]
    fn halant_zwnj_ends_a_syllable() {
        // Ka, halant, ZWNJ, then ta, matra i.
        assert_eq!(
            kinds(&[C, H, ZWNJ, C, M]),
            [(0, 3, CONSONANT), (3, 5, CONSONANT)]
        );
    }

    #[test]
    fn vowels_standalones_and_broken_clusters() {
        assert_eq!(kinds(&[V, SM]), [(0, 2, VOWEL)]);
        assert_eq!(kinds(&[PLACEHOLDER, M]), [(0, 2, STANDALONE)]);
        assert_eq!(kinds(&[M, SM]), [(0, 2, BROKEN)]);
        assert_eq!(kinds(&[SMPST]), [(0, 1, NON_INDIC)]);
        assert_eq!(kinds(&[0]), [(0, 1, NON_INDIC)]);
    }
}
