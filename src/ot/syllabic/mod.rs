//! Machinery the syllable-based shapers share: HarfBuzz's Indic-family
//! character table, the syllable scanner, dotted-circle insertion for
//! broken clusters, and the per-glyph state that GSUB stages carry.
//!
//! HarfBuzz's Indic and Khmer shapers classify every character by
//! `hb_indic_get_categories` (`hb-ot-shaper-indic-table.cc`), split the
//! run into syllables with a Ragel machine, and then keep a category,
//! a position, a syllable serial, and a feature mask on every glyph
//! while their GSUB features run. [`GlyphInfo`] holds that state here,
//! and [`stage`] keeps it aligned with the glyphs through
//! substitutions.

pub(crate) mod machine;
pub(crate) mod stage;
#[rustfmt::skip]
mod table;

use alloc::vec::Vec;

use crate::buffer::{ClusterLevel, Glyph};

/// Character categories of HarfBuzz's Indic and Khmer syllable
/// machines (`I_Cat` in `hb-ot-shaper-indic-machine.rl`, `K_Cat` in
/// `hb-ot-shaper-khmer-machine.rl`). The two machines share the
/// numbers they have in common.
#[allow(dead_code)] // the full HarfBuzz set, some only used by the table
pub(crate) mod cat {
    /// Anything the grammars do not name.
    pub(crate) const X: u8 = 0;
    /// Consonant.
    pub(crate) const C: u8 = 1;
    /// Independent vowel.
    pub(crate) const V: u8 = 2;
    /// Nukta.
    pub(crate) const N: u8 = 3;
    /// Halant (virama), Khmer coeng.
    pub(crate) const H: u8 = 4;
    /// Zero width non-joiner.
    pub(crate) const ZWNJ: u8 = 5;
    /// Zero width joiner.
    pub(crate) const ZWJ: u8 = 6;
    /// Dependent vowel (matra).
    pub(crate) const M: u8 = 7;
    /// Syllable modifier (bindu, visarga).
    pub(crate) const SM: u8 = 8;
    /// Cantillation mark.
    pub(crate) const A: u8 = 9;
    /// Vedic sign, the same category as [`A`].
    pub(crate) const VD: u8 = 9;
    /// Consonant placeholder.
    pub(crate) const PLACEHOLDER: u8 = 10;
    /// U+25CC DOTTED CIRCLE.
    pub(crate) const DOTTEDCIRCLE: u8 = 11;
    /// Register shifter.
    pub(crate) const RS: u8 = 12;
    /// Post-base matra that may follow a bindu.
    pub(crate) const MPST: u8 = 13;
    /// Encoded repha.
    pub(crate) const REPHA: u8 = 14;
    /// The script's Ra.
    pub(crate) const RA: u8 = 15;
    /// Consonant medial.
    pub(crate) const CM: u8 = 16;
    /// Avagraha and similar symbols.
    pub(crate) const SYMBOL: u8 = 17;
    /// Consonant with stacker.
    pub(crate) const CS: u8 = 18;
    /// Khmer above-base vowel sign.
    pub(crate) const VABV: u8 = 20;
    /// Khmer below-base vowel sign.
    pub(crate) const VBLW: u8 = 21;
    /// Khmer pre-base vowel sign.
    pub(crate) const VPRE: u8 = 22;
    /// Khmer post-base vowel sign.
    pub(crate) const VPST: u8 = 23;
    /// Khmer robat and register shifters.
    pub(crate) const ROBATIC: u8 = 25;
    /// Khmer signs of the X group (nikahit and others).
    pub(crate) const XGROUP: u8 = 26;
    /// Khmer signs of the Y group (reahmuk and others).
    pub(crate) const YGROUP: u8 = 27;
    /// Syllable modifier with no position of its own.
    pub(crate) const SMPST: u8 = 57;
}

/// Visual positions in a syllable, left to right (HarfBuzz's
/// `ot_position_t`, `hb-ot-shaper-indic.hh`).
#[allow(dead_code)] // the full HarfBuzz set, some only used by the table
pub(crate) mod pos {
    /// Unset.
    pub(crate) const START: u8 = 0;
    /// A Ra that becomes a reph.
    pub(crate) const RA_TO_BECOME_REPH: u8 = 1;
    /// Pre-base matra.
    pub(crate) const PRE_M: u8 = 2;
    /// Pre-base consonant.
    pub(crate) const PRE_C: u8 = 3;
    /// Base consonant.
    pub(crate) const BASE_C: u8 = 4;
    /// Right after the base.
    pub(crate) const AFTER_MAIN: u8 = 5;
    /// Above-base consonant.
    pub(crate) const ABOVE_C: u8 = 6;
    /// Before subjoined consonants.
    pub(crate) const BEFORE_SUB: u8 = 7;
    /// Below-base consonant.
    pub(crate) const BELOW_C: u8 = 8;
    /// After subjoined consonants.
    pub(crate) const AFTER_SUB: u8 = 9;
    /// Before post-base consonants.
    pub(crate) const BEFORE_POST: u8 = 10;
    /// Post-base consonant.
    pub(crate) const POST_C: u8 = 11;
    /// After post-base consonants.
    pub(crate) const AFTER_POST: u8 = 12;
    /// Syllable modifiers and Vedic signs.
    pub(crate) const SMVD: u8 = 13;
    /// End of the syllable.
    pub(crate) const END: u8 = 14;
}

/// Packs a category and a position into one table entry.
const fn pack(category: u8, position: u8) -> u16 {
    category as u16 | (position as u16) << 8
}

/// HarfBuzz's `hb_indic_get_categories`: the syllable-machine category
/// and the initial position of `ch`.
pub(crate) fn categories(ch: char) -> (u8, u8) {
    let u = ch as u32;
    let i = table::RANGES.partition_point(|&(first, _)| first <= u);
    let entry = i
        .checked_sub(1)
        .and_then(|i| table::RANGES.get(i))
        .and_then(|&(first, entries)| entries.get((u - first) as usize))
        .copied()
        .unwrap_or(pack(cat::X, pos::END));
    ((entry & 0xFF) as u8, (entry >> 8) as u8)
}

/// The shaper state HarfBuzz keeps in each glyph's info while a
/// syllable-based shaper runs: `indic_category`, `indic_position`,
/// `syllable`, and the feature mask.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GlyphInfo {
    /// The syllable-machine category.
    pub(crate) category: u8,
    /// The visual position ([`pos`]).
    pub(crate) position: u8,
    /// The syllable serial in the high nibble and the syllable type in
    /// the low one, as HarfBuzz's `found_syllable` sets them. Serials
    /// run 1 to 15 and wrap, so neighbors always differ.
    pub(crate) syllable: u8,
    /// Which of the shaper's features apply to the glyph, one bit per
    /// feature in the shaper's own numbering.
    pub(crate) mask: u32,
    /// Whether a GSUB substitution touched the glyph (HarfBuzz's
    /// `_hb_glyph_info_substituted`).
    pub(crate) substituted: bool,
}

impl GlyphInfo {
    /// The syllable type bits.
    pub(crate) const fn syllable_type(self) -> u8 {
        self.syllable & 0x0F
    }
}

/// Marks each syllable `scan` found on its glyphs' `syllable`
/// (HarfBuzz's `found_syllable`). `info` has one entry per category
/// the scan read.
pub(crate) fn set_syllables(info: &mut [GlyphInfo], syllables: &[machine::Syllable]) {
    let mut serial: u8 = 1;
    for s in syllables {
        if let Some(run) = info.get_mut(s.start..s.end) {
            for g in run {
                g.syllable = (serial << 4) | s.kind;
            }
        }
        serial += 1;
        if serial == 16 {
            serial = 1;
        }
    }
}

/// The glyph ranges of the syllables, as runs of equal `syllable`
/// (HarfBuzz's `foreach_syllable`).
pub(crate) fn syllable_ranges(info: &[GlyphInfo]) -> Vec<core::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=info.len() {
        if i == info.len() || info[i].syllable != info[start].syllable {
            out.push(start..i);
            start = i;
        }
    }
    out
}

/// What a shaper's dotted circles look like: the syllable type that
/// gets one, the circle's category and position, and the category of a
/// repha the circle goes after, when the shaper has one.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DottedCircle {
    /// The broken syllable type.
    pub(crate) broken: u8,
    /// The circle's category.
    pub(crate) category: u8,
    /// The circle's position.
    pub(crate) position: u8,
    /// The repha category the circle goes after.
    pub(crate) repha: Option<u8>,
}

/// HarfBuzz's `hb_syllabic_insert_dotted_circles`: a dotted circle
/// (glyph `circle`) goes at the start of every syllable of the broken
/// type, after a leading run of repha glyphs when the shaper has such
/// a category. The circle takes the cluster, mask, and syllable of the
/// syllable's first glyph. Glyphs and infos grow together.
pub(crate) fn insert_dotted_circles(
    glyphs: &mut Vec<Glyph>,
    info: &mut Vec<GlyphInfo>,
    spec: DottedCircle,
    circle: u16,
) {
    if glyphs.len() != info.len() || !info.iter().any(|g| g.syllable_type() == spec.broken) {
        return;
    }
    let mut out_glyphs = Vec::with_capacity(glyphs.len() + 1);
    let mut out_info = Vec::with_capacity(info.len() + 1);
    let mut last_syllable = 0u8;
    let mut i = 0;
    while i < glyphs.len() {
        let syllable = info[i].syllable;
        if last_syllable != syllable && syllable & 0x0F == spec.broken {
            last_syllable = syllable;
            let ginfo = GlyphInfo {
                category: spec.category,
                position: spec.position,
                syllable,
                mask: info[i].mask,
                substituted: false,
            };
            let glyph = Glyph::new(u32::from(circle), glyphs[i].cluster);
            while i < glyphs.len()
                && info[i].syllable == syllable
                && Some(info[i].category) == spec.repha
            {
                out_glyphs.push(glyphs[i]);
                out_info.push(info[i]);
                i += 1;
            }
            out_glyphs.push(glyph);
            out_info.push(ginfo);
            continue;
        }
        out_glyphs.push(glyphs[i]);
        out_info.push(info[i]);
        i += 1;
    }
    *glyphs = out_glyphs;
    *info = out_info;
}

/// HarfBuzz's `merge_clusters` over `glyphs[start..end]`, which only
/// acts at the monotone cluster levels.
pub(crate) fn merge_clusters(glyphs: &mut [Glyph], start: usize, end: usize, level: ClusterLevel) {
    crate::shape::merge_clusters(glyphs, start, end, level);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_classifies_known_characters() {
        assert_eq!(categories('\u{0915}'), (cat::C, pos::BASE_C));
        assert_eq!(categories('\u{0930}'), (cat::RA, pos::BASE_C));
        assert_eq!(categories('\u{094D}'), (cat::H, pos::BELOW_C));
        assert_eq!(categories('\u{093F}'), (cat::M, pos::PRE_M));
        assert_eq!(categories('\u{17D2}'), (cat::H, pos::END));
        assert_eq!(categories('\u{17C1}'), (cat::VPRE, pos::PRE_C));
        assert_eq!(categories('\u{200C}'), (cat::ZWNJ, pos::END));
        assert_eq!(categories('\u{25CC}'), (cat::DOTTEDCIRCLE, pos::BASE_C));
        assert_eq!(categories('A'), (cat::X, pos::END));
        assert_eq!(categories('\u{10FFFF}'), (cat::X, pos::END));
    }

    #[test]
    fn syllables_get_wrapping_serials() {
        let mut info = alloc::vec![GlyphInfo::default(); 17];
        let syllables: Vec<machine::Syllable> = (0..17)
            .map(|i| machine::Syllable {
                start: i,
                end: i + 1,
                kind: 2,
            })
            .collect();
        set_syllables(&mut info, &syllables);
        assert_eq!(info[0].syllable, 0x12);
        assert_eq!(info[14].syllable, 0xF2);
        assert_eq!(info[15].syllable, 0x12);
        assert_eq!(syllable_ranges(&info).len(), 17);
    }
}
