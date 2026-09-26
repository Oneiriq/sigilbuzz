//! Script-specific OpenType shapers.
//!
//! Each submodule drives the GSUB features one script family needs,
//! in the order that family's shaping model prescribes. The generic
//! lookup machinery lives in [`crate::tables::layout`] and
//! [`crate::shape`]. The submodules here add syllable segmentation,
//! reordering, and joining-form selection on top of it.
//!
//! - [`arabic`]: the cursive joining state machine.
//! - [`indic`]: the Indic2 reordering shaper.
//! - [`mongolian`]: Mongolian joining with free variation selectors.
//! - [`tibetan`]: the Tibetan feature chain.
//! - [`use_shaper`]: the Universal Shaping Engine and its clients.

pub mod arabic;
pub mod indic;
pub mod mongolian;
pub mod tibetan;
pub mod use_shaper;

/// Common OpenType feature tags. These are byte-literal constants so
/// consumers can compare against them without string handling.
pub mod feature {
    /// Standard ligatures.
    pub const LIGA: [u8; 4] = *b"liga";
    /// Discretionary ligatures.
    pub const DLIG: [u8; 4] = *b"dlig";
    /// Kerning.
    pub const KERN: [u8; 4] = *b"kern";
    /// Contextual alternates.
    pub const CALT: [u8; 4] = *b"calt";
    /// Isolated form (Arabic).
    pub const ISOL: [u8; 4] = *b"isol";
    /// Initial form (Arabic).
    pub const INIT: [u8; 4] = *b"init";
    /// Medial form (Arabic).
    pub const MEDI: [u8; 4] = *b"medi";
    /// Final form (Arabic).
    pub const FINA: [u8; 4] = *b"fina";
    /// Required ligatures (Arabic).
    pub const RLIG: [u8; 4] = *b"rlig";
    /// Small capitals.
    pub const SMCP: [u8; 4] = *b"smcp";
    /// Small capitals from capitals.
    pub const C2SC: [u8; 4] = *b"c2sc";
    /// Old-style figures.
    pub const ONUM: [u8; 4] = *b"onum";
    /// Lining figures.
    pub const LNUM: [u8; 4] = *b"lnum";
    /// Tabular figures.
    pub const TNUM: [u8; 4] = *b"tnum";
    /// Proportional figures.
    pub const PNUM: [u8; 4] = *b"pnum";
    /// Diagonal fractions.
    pub const FRAC: [u8; 4] = *b"frac";
    /// Swash.
    pub const SWSH: [u8; 4] = *b"swsh";
    /// Stylistic set 1.
    pub const SS01: [u8; 4] = *b"ss01";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_tags_are_four_byte_ascii() {
        assert_eq!(feature::LIGA, *b"liga");
        assert_eq!(feature::KERN, *b"kern");
        assert_eq!(feature::SS01, *b"ss01");
    }
}
