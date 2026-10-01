//! The character categories of the Universal Shaping Engine, with the
//! numbers HarfBuzz's syllable machine gives them
//! (`hb-ot-shaper-use-machine.rl`), and the lookup over the table
//! generated from the Unicode Character Database
//! (`hb-ot-shaper-use-table.hh`).

use super::table::RANGES;

/// Other: anything the grammar does not name.
pub(crate) const O: u8 = 0;
/// Base: a consonant, an independent vowel, or a joining letter.
pub(crate) const B: u8 = 1;
/// Number base: a Brahmi joining number.
pub(crate) const N: u8 = 4;
/// Generic base: a consonant placeholder or a dash-like symbol.
pub(crate) const GB: u8 = 5;
/// Combining grapheme joiner, ZWJ, and the default-ignorable marks,
/// which the syllable machine does not see.
pub(crate) const CGJ: u8 = 6;
/// Subjoined consonant.
pub(crate) const SUB: u8 = 11;
/// Halant (virama).
pub(crate) const H: u8 = 12;
/// Number joiner.
pub(crate) const HN: u8 = 13;
/// Zero width non-joiner.
pub(crate) const ZWNJ: u8 = 14;
/// Word joiner and the other default ignorables, and unassigned code
/// points.
pub(crate) const WJ: u8 = 16;
/// Repha.
pub(crate) const R: u8 = 18;
/// Pre-base vowel sign.
pub(crate) const VPRE: u8 = 22;
/// Pre-base vowel modifier.
pub(crate) const VMPRE: u8 = 23;
/// Final consonant above the base.
pub(crate) const FABV: u8 = 24;
/// Final consonant below the base.
pub(crate) const FBLW: u8 = 25;
/// Final consonant after the base.
pub(crate) const FPST: u8 = 26;
/// Medial consonant above the base.
pub(crate) const MABV: u8 = 27;
/// Medial consonant below the base.
pub(crate) const MBLW: u8 = 28;
/// Medial consonant after the base.
pub(crate) const MPST: u8 = 29;
/// Medial consonant before the base.
pub(crate) const MPRE: u8 = 30;
/// Consonant modifier above the base.
pub(crate) const CMABV: u8 = 31;
/// Consonant modifier below the base.
pub(crate) const CMBLW: u8 = 32;
/// Vowel sign above the base.
pub(crate) const VABV: u8 = 33;
/// Vowel sign below the base.
pub(crate) const VBLW: u8 = 34;
/// Vowel sign after the base.
pub(crate) const VPST: u8 = 35;
/// Vowel modifier above the base.
pub(crate) const VMABV: u8 = 37;
/// Vowel modifier below the base.
pub(crate) const VMBLW: u8 = 38;
/// Vowel modifier after the base.
pub(crate) const VMPST: u8 = 39;
/// Symbol modifier above.
pub(crate) const SMABV: u8 = 41;
/// Symbol modifier below.
pub(crate) const SMBLW: u8 = 42;
/// Consonant with stacker.
pub(crate) const CS: u8 = 43;
/// Invisible stacker.
pub(crate) const IS: u8 = 44;
/// Final consonant modifier above.
pub(crate) const FMABV: u8 = 45;
/// Final consonant modifier below.
pub(crate) const FMBLW: u8 = 46;
/// Final consonant modifier with no position of its own.
pub(crate) const FMPST: u8 = 47;
/// Sakot (Tai Tham).
pub(crate) const SK: u8 = 48;
/// Hieroglyph.
pub(crate) const G: u8 = 49;
/// Hieroglyph joiner.
pub(crate) const J: u8 = 50;
/// Hieroglyph segment begin.
pub(crate) const SB: u8 = 51;
/// Hieroglyph segment end.
pub(crate) const SE: u8 = 52;
/// Halant or vowel modifier (Sinhala al-lakuna).
pub(crate) const HVM: u8 = 53;
/// Hieroglyph modifier.
pub(crate) const HM: u8 = 54;
/// Hieroglyph mirror.
pub(crate) const HR: u8 = 55;
/// Reordering killer.
pub(crate) const RK: u8 = 56;

/// HarfBuzz's `hb_use_get_category`: the USE category of `ch`.
pub(crate) fn category(ch: char) -> u8 {
    let u = ch as u32;
    let i = RANGES.partition_point(|&(first, _, _)| first <= u);
    i.checked_sub(1)
        .and_then(|i| RANGES.get(i))
        .filter(|&&(_, last, _)| u <= last)
        .map_or(O, |&(_, _, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_classifies_known_characters() {
        // Tirhuta ka, virama, sign i, candrabindu, and repha-forming ra.
        assert_eq!(category('\u{1148F}'), B);
        assert_eq!(category('\u{114C2}'), H);
        assert_eq!(category('\u{114B1}'), VPRE);
        assert_eq!(category('\u{114BF}'), VMABV);
        // Sinhala al-lakuna and kombuva.
        assert_eq!(category('\u{0DCA}'), HVM);
        assert_eq!(category('\u{0DD9}'), VPRE);
        assert_eq!(category('\u{1A60}'), SK);
        assert_eq!(category('\u{25CC}'), B);
        assert_eq!(category('\u{200C}'), ZWNJ);
        assert_eq!(category('\u{200D}'), CGJ);
        assert_eq!(category('\u{2060}'), WJ);
        assert_eq!(category('A'), O);
        assert_eq!(category('\u{0E01}'), O);
        assert_eq!(category('\u{10FFFF}'), O);
    }

    #[test]
    fn ranges_are_sorted_and_disjoint() {
        assert!(RANGES.windows(2).all(|w| w[0].1 < w[1].0));
        assert!(RANGES.iter().all(|&(a, b, c)| a <= b && c != O));
    }
}
