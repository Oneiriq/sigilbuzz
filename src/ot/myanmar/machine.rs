//! The Myanmar syllable grammar of HarfBuzz's
//! `hb-ot-shaper-myanmar-machine.rl`:
//!
//! ```text
//! j = ZWJ|ZWNJ;
//! k = (Ra As H);
//! sm = SM | SMPst;
//! c = C|Ra;
//!
//! medial_group = MY? As? MR? ((MW MH? ML? | MH ML? | ML) As?)?;
//! main_vowel_group = (VPre.VS?)* VAbv* VBlw* A* (DB As?)?;
//! post_vowel_group = VPst MH? ML? As* VAbv* A* (DB As?)?;
//! tone_group = sm | PT A* DB? As?;
//!
//! complex_syllable_tail = As* medial_group main_vowel_group
//!                         post_vowel_group* tone_group* j?;
//! syllable_tail = (H (c|IV).VS?)* (H | complex_syllable_tail);
//!
//! consonant_syllable = (k|CS)? (c|IV|GB|DOTTEDCIRCLE).VS? syllable_tail;
//! broken_cluster = k? VS? syllable_tail;
//! ```
//!
//! A joiner or an `SMPst` sign on its own is a non-Myanmar cluster.
//! The machine's `IV`, `DB`, and `GB` are the shared categories `V`,
//! `N`, and `PLACEHOLDER`.

use alloc::vec;
use alloc::vec::Vec;

use super::syllable;
use crate::ot::syllabic::cat::{
    A, AS, C, CS, DOTTEDCIRCLE, H, MH, ML, MR, MW, MY, N, PLACEHOLDER, PT, RA, SM, SMPST, V, VABV,
    VBLW, VPRE, VPST, VS, ZWJ, ZWNJ,
};
use crate::ot::syllabic::machine::{alt, one, opt, seq, star, Machine, Pat, Syllable};

fn j() -> Pat {
    one(&[ZWJ, ZWNJ])
}

fn k() -> Pat {
    seq([one(&[RA]), one(&[AS]), one(&[H])])
}

/// `(DB As?)?`.
fn dot_below() -> Pat {
    opt(seq([one(&[N]), opt(one(&[AS]))]))
}

fn medial_group() -> Pat {
    seq([
        opt(one(&[MY])),
        opt(one(&[AS])),
        opt(one(&[MR])),
        opt(seq([
            alt([
                seq([one(&[MW]), opt(one(&[MH])), opt(one(&[ML]))]),
                seq([one(&[MH]), opt(one(&[ML]))]),
                one(&[ML]),
            ]),
            opt(one(&[AS])),
        ])),
    ])
}

fn main_vowel_group() -> Pat {
    seq([
        star(seq([one(&[VPRE]), opt(one(&[VS]))])),
        star(one(&[VABV])),
        star(one(&[VBLW])),
        star(one(&[A])),
        dot_below(),
    ])
}

fn post_vowel_group() -> Pat {
    seq([
        one(&[VPST]),
        opt(one(&[MH])),
        opt(one(&[ML])),
        star(one(&[AS])),
        star(one(&[VABV])),
        star(one(&[A])),
        dot_below(),
    ])
}

fn tone_group() -> Pat {
    alt([
        one(&[SM, SMPST]),
        seq([one(&[PT]), star(one(&[A])), opt(one(&[N])), opt(one(&[AS]))]),
    ])
}

fn complex_syllable_tail() -> Pat {
    seq([
        star(one(&[AS])),
        medial_group(),
        main_vowel_group(),
        star(post_vowel_group()),
        star(tone_group()),
        opt(j()),
    ])
}

fn syllable_tail() -> Pat {
    seq([
        star(seq([one(&[H]), one(&[C, RA, V]), opt(one(&[VS]))])),
        alt([one(&[H]), complex_syllable_tail()]),
    ])
}

fn consonant_syllable() -> Pat {
    seq([
        opt(alt([k(), one(&[CS])])),
        one(&[C, RA, V, PLACEHOLDER, DOTTEDCIRCLE]),
        opt(one(&[VS])),
        syllable_tail(),
    ])
}

fn broken_cluster() -> Pat {
    seq([opt(k()), opt(one(&[VS])), syllable_tail()])
}

/// Splits Myanmar categories into syllables (`find_syllables_myanmar`).
pub(super) fn find_syllables(cats: &[u8]) -> Vec<Syllable> {
    let machine = Machine::new(
        vec![
            (consonant_syllable(), syllable::CONSONANT),
            (one(&[ZWJ, ZWNJ, SMPST]), syllable::NON_MYANMAR),
            (broken_cluster(), syllable::BROKEN),
        ],
        syllable::NON_MYANMAR,
    );
    machine.scan(cats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ot::syllabic::cat::X;

    fn kinds(cats: &[u8]) -> Vec<(usize, usize, u8)> {
        find_syllables(cats)
            .iter()
            .map(|s| (s.start, s.end, s.kind))
            .collect()
    }

    const CS_: u8 = syllable::CONSONANT;
    const BR: u8 = syllable::BROKEN;
    const NM: u8 = syllable::NON_MYANMAR;

    #[test]
    fn consonant_syllables_take_kinzi_stacks_medials_and_vowels() {
        // Kinzi, ka, sign e, sign aa, asat.
        assert_eq!(kinds(&[RA, AS, H, C, VPRE, VPST, AS]), [(0, 7, CS_)]);
        // Ka, virama, kha, medial ya, medial wa, medial ha.
        assert_eq!(kinds(&[C, H, C, MY, MW, MH]), [(0, 6, CS_)]);
        // Ka, sign i, sign u, anusvara, dot below, visarga.
        assert_eq!(kinds(&[C, VABV, VBLW, A, N, SM]), [(0, 6, CS_)]);
        // A trailing ZWJ stays in the syllable.
        assert_eq!(kinds(&[C, VPST, ZWJ]), [(0, 3, CS_)]);
        // Medial ra after medial wa does not fit: it starts a broken
        // cluster.
        assert_eq!(kinds(&[C, MW, MR]), [(0, 2, CS_), (2, 3, BR)]);
    }

    #[test]
    fn joiners_and_orphan_marks() {
        // A joiner on its own is a non-Myanmar cluster, even though the
        // broken-cluster rule matches it too.
        assert_eq!(kinds(&[ZWJ]), [(0, 1, NM)]);
        assert_eq!(kinds(&[SMPST]), [(0, 1, NM)]);
        // A run of signs with no base is one broken cluster.
        assert_eq!(kinds(&[VPRE, VPST, AS]), [(0, 3, BR)]);
        // A lone kinzi is a broken cluster, which is longer than the
        // consonant syllable of nga and asat.
        assert_eq!(kinds(&[RA, AS, H]), [(0, 3, BR)]);
        // Other characters stand alone.
        assert_eq!(kinds(&[X, C]), [(0, 1, NM), (1, 2, CS_)]);
    }

    #[test]
    fn variation_selectors_follow_bases_and_pre_base_vowels() {
        assert_eq!(kinds(&[C, VS, VPRE, VS]), [(0, 4, CS_)]);
        assert_eq!(kinds(&[PLACEHOLDER, VS]), [(0, 2, CS_)]);
        assert_eq!(kinds(&[C, VS, VS]), [(0, 2, CS_), (2, 3, BR)]);
    }
}
