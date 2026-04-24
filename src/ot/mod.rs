//! OpenType Layout — GSUB / GPOS / GDEF machinery.
//!
//! Nothing meaningful here yet beyond placeholder tag constants. This
//! is the module that will grow a full script-to-feature-list table,
//! GSUB lookup evaluation, and GPOS positioning adjustments.
//!
//! See `docs/ot-roadmap.md` (to be written) for sequencing.

#![allow(missing_docs)]

pub mod arabic;
pub mod indic;
pub mod use_shaper;

/// Common OpenType feature tags. These are byte-literal constants so
/// consumers can compare against them without string handling.
pub mod feature {
    pub const LIGA: [u8; 4] = *b"liga"; // Standard ligatures
    pub const DLIG: [u8; 4] = *b"dlig"; // Discretionary ligatures
    pub const KERN: [u8; 4] = *b"kern"; // Kerning
    pub const CALT: [u8; 4] = *b"calt"; // Contextual alternates
    pub const ISOL: [u8; 4] = *b"isol"; // Isolated form (Arabic)
    pub const INIT: [u8; 4] = *b"init"; // Initial form (Arabic)
    pub const MEDI: [u8; 4] = *b"medi"; // Medial form (Arabic)
    pub const FINA: [u8; 4] = *b"fina"; // Final form (Arabic)
    pub const RLIG: [u8; 4] = *b"rlig"; // Required ligatures (Arabic)
    pub const SMCP: [u8; 4] = *b"smcp"; // Small capitals
    pub const C2SC: [u8; 4] = *b"c2sc"; // Petite capitals from capitals
    pub const ONUM: [u8; 4] = *b"onum"; // Old-style figures
    pub const LNUM: [u8; 4] = *b"lnum"; // Lining figures
    pub const TNUM: [u8; 4] = *b"tnum"; // Tabular figures
    pub const PNUM: [u8; 4] = *b"pnum"; // Proportional figures
    pub const FRAC: [u8; 4] = *b"frac"; // Diagonal fractions
    pub const SWSH: [u8; 4] = *b"swsh"; // Swash
    pub const SS01: [u8; 4] = *b"ss01"; // Stylistic set 1
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
