//! Universal Shaping Engine (USE) per-codepoint categorization.
//!
//! The USE is Microsoft's generalized complex-script shaper. It covers
//! Khmer, Myanmar, Tai Tham, Buginese, Cham, New Tai Lue and several
//! others that do not fit the Arabic / Indic2 moulds. Each codepoint
//! it sees is classified along two axes:
//!
//! - [`UseCategory`]: the role the codepoint plays inside a syllable
//!   (base, halant, vowel, final mark, ...). Drives the syllable state
//!   machine and the feature masking.
//! - [`UsePosition`]: where a mark visually sits relative to its base
//!   (pre-base, above-base, below-base, post-base). Drives the reorder
//!   pass and picks the correct positional feature bucket (`abvf`,
//!   `blwf`, `pstf`, `pref`).
//!
//! The tables cover Khmer, Myanmar, Thai, Lao, the Jamo subset of
//! Hangul, N'Ko, Buginese, Tai Tham, Balinese, Sundanese, Lepcha,
//! Limbu, Cham, and the Brahmi-family historical scripts. They live
//! here rather than inside a script-specific module because the same
//! state machine consumes them for every USE script.
//!
//! # Sources
//!
//! The classification mirrors the columns in the MS-published
//! `IndicSyllabicCategory.txt` / `IndicPositionalCategory.txt` files
//! combined with the USE-specific overrides listed in the MS USE docs.
//! Each script slice was cross-checked against `rustybuzz`'s
//! `ot_shaper_use_table.rs` (reference implementation) so the state
//! machine consumes identical categories.

mod categories;
mod positions;

pub use categories::use_category;
pub use positions::use_position;

/// USE per-codepoint category. The mnemonic names mirror the labels
/// from the MS USE documentation so OpenType spec readers can map
/// straight across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseCategory {
    /// Base consonant / independent letter: anchors a syllable.
    B,
    /// Independent vowel.
    IV,
    /// Number: digits and number signs.
    N,
    /// Generic base: default for letters that do not participate in
    /// the syllable cluster (punctuation, signs that stand alone).
    GB,
    /// Repha: reordering ra-equivalent. Myanmar uses this for the
    /// `kinzi` cluster (ra + asat + virama preceding the base).
    R,
    /// Symbol.
    S,
    /// Halant / virama: deletes the inherent vowel of the preceding
    /// base and glues it to the following consonant as a subscript.
    /// Khmer uses U+17D2 COENG, Myanmar U+1039, Hangul none.
    H,
    /// Pre-base vowel sign: renders before the base visually.
    VPre,
    /// Above-base vowel sign.
    VAbv,
    /// Below-base vowel sign.
    VBlw,
    /// Post-base vowel sign: renders after the base visually.
    VPst,
    /// Modifying mark: tone marks, registers, robat, bindu-likes.
    M,
    /// Final mark: syllable-final modifiers (visarga, anusvara).
    FM,
    /// Consonant modifier / medial: Myanmar medial ya/ra/wa/ha,
    /// Myanmar asat (U+103A when it is not acting as a kinzi virama).
    CM,
    /// Variation selector.
    VS,
    /// Zero-width non-joiner.
    ZWNJ,
    /// Zero-width joiner.
    ZWJ,
    /// Whitespace / cluster boundary.
    WS,
    /// Anything else: treated as a cluster-break / pass-through.
    O,
}

/// Where a mark sits relative to its base. Matches the IPC partition
/// used by the Indic shaper but with the USE-specific pre/below/post
/// split laid out explicitly so the reorder pass can branch cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsePosition {
    /// No positional role: the default for bases, whitespace, marks
    /// that attach at the overall glyph box.
    NotApplicable,
    /// Before the base visually: pre-base matras.
    PreBase,
    /// Above the base: above-base vowel signs and tone marks.
    AboveBase,
    /// Below the base: below-base vowel signs and subscript marks.
    BelowBase,
    /// After the base visually: post-base matras and final marks.
    PostBase,
}

/// Returns `true` if the codepoint is a Hangul Leading Jamo
/// (Choseong). The USE segmenter uses this to anchor a Hangul syllable
/// (L is the required opening of `L V? T?`).
#[must_use]
pub const fn is_hangul_l(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp, 0x1100..=0x115F | 0xA960..=0xA97C)
}

/// Returns `true` if the codepoint is a Hangul Vowel Jamo (Jungseong).
#[must_use]
pub const fn is_hangul_v(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp, 0x1160..=0x11A7 | 0xD7B0..=0xD7C6)
}

/// Returns `true` if the codepoint is a Hangul Trailing Jamo
/// (Jongseong).
#[must_use]
pub const fn is_hangul_t(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp, 0x11A8..=0x11FF | 0xD7CB..=0xD7FB)
}

#[cfg(test)]
mod tests;
