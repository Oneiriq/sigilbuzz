//! The Khmer syllable grammar of HarfBuzz's
//! `hb-ot-shaper-khmer-machine.rl`, which HarfBuzz notes was
//! extracted from what Uniscribe allows:
//!
//! ```text
//! c = (C | Ra | V);
//! cn = c.((ZWJ|ZWNJ)?.Robatic)?;
//! joiner = (ZWJ | ZWNJ);
//! xgroup = (joiner*.Xgroup)*;
//! ygroup = Ygroup*;
//! matra_group = VPre? xgroup VBlw? xgroup (joiner?.VAbv)? xgroup VPst?;
//! syllable_tail = xgroup matra_group xgroup (H.c)? ygroup;
//! broken_cluster = Robatic? (H.cn)* (H | syllable_tail);
//! consonant_syllable = (cn|PLACEHOLDER|DOTTEDCIRCLE) broken_cluster;
//! ```

use alloc::vec;
use alloc::vec::Vec;

use super::syllable;
use crate::ot::syllabic::cat::{
    C, DOTTEDCIRCLE, H, PLACEHOLDER, RA, ROBATIC, V, VABV, VBLW, VPRE, VPST, XGROUP, YGROUP, ZWJ,
    ZWNJ,
};
use crate::ot::syllabic::machine::{alt, one, opt, seq, star, Machine, Pat, Syllable};
use crate::sync::OnceBox;

fn c() -> Pat {
    one(&[C, RA, V])
}

fn cn() -> Pat {
    seq([c(), opt(seq([opt(one(&[ZWJ, ZWNJ])), one(&[ROBATIC])]))])
}

fn joiner() -> Pat {
    one(&[ZWJ, ZWNJ])
}

fn xgroup() -> Pat {
    star(seq([star(joiner()), one(&[XGROUP])]))
}

fn matra_group() -> Pat {
    seq([
        opt(one(&[VPRE])),
        xgroup(),
        opt(one(&[VBLW])),
        xgroup(),
        opt(seq([opt(joiner()), one(&[VABV])])),
        xgroup(),
        opt(one(&[VPST])),
    ])
}

fn syllable_tail() -> Pat {
    seq([
        xgroup(),
        matra_group(),
        xgroup(),
        opt(seq([one(&[H]), c()])),
        star(one(&[YGROUP])),
    ])
}

fn broken_cluster() -> Pat {
    seq([
        opt(one(&[ROBATIC])),
        star(seq([one(&[H]), cn()])),
        alt([one(&[H]), syllable_tail()]),
    ])
}

fn consonant_syllable() -> Pat {
    seq([
        alt([cn(), one(&[PLACEHOLDER, DOTTEDCIRCLE])]),
        broken_cluster(),
    ])
}

/// Splits Khmer categories into syllables (`find_syllables_khmer`).
/// The grammar is compiled once.
pub(super) fn find_syllables(cats: &[u8]) -> Vec<Syllable> {
    static MACHINE: OnceBox<Machine> = OnceBox::new();
    let machine = MACHINE.get_or_init(|| {
        Machine::new(
            vec![
                (consonant_syllable(), syllable::CONSONANT),
                (broken_cluster(), syllable::BROKEN),
            ],
            syllable::NON_KHMER,
        )
    });
    machine.scan(cats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(cats: &[u8]) -> Vec<(usize, usize, u8)> {
        find_syllables(cats)
            .iter()
            .map(|s| (s.start, s.end, s.kind))
            .collect()
    }

    const CS: u8 = syllable::CONSONANT;
    const BR: u8 = syllable::BROKEN;
    const NK: u8 = syllable::NON_KHMER;

    #[test]
    fn consonant_syllables_take_their_marks() {
        // Ka, coeng, ro, sign e.
        assert_eq!(kinds(&[C, H, RA, VPRE]), [(0, 4, CS)]);
        // Ka, robat, sign i, nikahit.
        assert_eq!(kinds(&[C, ROBATIC, VABV, XGROUP]), [(0, 4, CS)]);
        // Ka ZWJ robat.
        assert_eq!(kinds(&[C, ZWJ, ROBATIC]), [(0, 3, CS)]);
    }

    #[test]
    fn a_joiner_before_a_coeng_breaks_the_syllable() {
        // Ka, ZWJ: the ZWJ waits for an X-group sign or a vowel above
        // and ends up alone. Coeng, ro is a broken cluster.
        assert_eq!(
            kinds(&[C, ZWJ, H, RA]),
            [(0, 1, CS), (1, 2, NK), (2, 4, BR)]
        );
        // Ka, ZWNJ, sign e.
        assert_eq!(
            kinds(&[C, ZWNJ, VPRE]),
            [(0, 1, CS), (1, 2, NK), (2, 3, BR)]
        );
    }

    #[test]
    fn joiners_before_signs_stay_in_the_syllable() {
        assert_eq!(kinds(&[C, VABV, ZWNJ, XGROUP]), [(0, 4, CS)]);
        assert_eq!(kinds(&[C, ZWJ, VABV]), [(0, 3, CS)]);
    }
}
